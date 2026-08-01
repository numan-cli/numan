use crate::util::confirm::confirm_or_bail;
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use std::path::{Path, PathBuf};

use crate::core::platform::Platform;
use crate::nu::bootstrap::{self, NuSetupOptions};
use crate::nu::paths::{find_nu_executable_with_root, find_nu_on_path, probe_nu_config_path};
use crate::util::atomic::write_bytes_atomic;
use crate::util::fs_safety::{
    acquire_mutation_lock, assert_managed_file_owned, assert_not_symlink,
};

const VENDOR_LOADER: &str = include_str!("../../assets/nushell-loader/loader.nu");

const CONFIG_SOURCE_LINE: &str = "source ($nu.config-path | path dirname | path join 'loader.nu')";

const CONFIG_SNIPPET: &str = r#"
# Cached third-party init files (numan setup loader)
source ($nu.config-path | path dirname | path join 'loader.nu')
"#;

#[derive(Debug, Subcommand)]
pub enum SetupCommands {
    /// Download and install the official Nushell release under the Numan root
    Nu(NuSetupArgs),
    /// Install the vendored nushell-loader script and print a config.nu snippet
    Loader(LoaderArgs),
}

#[derive(Debug, Args)]
pub struct NuSetupArgs {
    /// Action to perform (default: install)
    #[command(subcommand)]
    pub action: Option<NuAction>,

    /// Nushell version to install (e.g. 0.113.1); omit for latest
    #[arg(value_name = "VERSION")]
    pub version: Option<String>,

    /// Re-download and replace an existing managed Nushell install
    #[arg(long)]
    pub force: bool,

    /// Skip updating the user PATH (Numan still uses the managed binary)
    #[arg(long)]
    pub skip_path: bool,

    /// Skip confirmation prompts
    #[arg(long)]
    pub yes: bool,

    // COMPAT: remove in v0.3.0 — hidden backward-compat flags
    #[arg(long, hide = true)]
    pub remove: bool,
    #[arg(long, hide = true)]
    pub use_path: bool,
    #[arg(long, hide = true, value_name = "PATH")]
    pub use_existing: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum NuAction {
    /// Remove the managed Nushell install and fall back to PATH Nu
    Remove,
    /// Use the Nushell already on PATH (remove managed install, no download)
    Path,
    /// Use a specific existing Nushell binary
    Use {
        /// Path to the Nu binary
        path: PathBuf,
    },
}

impl NuSetupArgs {
    /// Construct args for installing a managed Nu (latest or pinned).
    pub fn install(version: Option<String>, force: bool, skip_path: bool, yes: bool) -> Self {
        Self {
            action: None,
            version,
            force,
            skip_path,
            yes,
            remove: false,
            use_path: false,
            use_existing: None,
        }
    }

    /// Construct args for switching to the PATH Nu.
    pub fn use_path(yes: bool) -> Self {
        Self {
            action: Some(NuAction::Path),
            version: None,
            force: false,
            skip_path: false,
            yes,
            remove: false,
            use_path: false,
            use_existing: None,
        }
    }

    /// Construct args for registering a specific existing Nu binary.
    pub fn use_existing(path: PathBuf, yes: bool) -> Self {
        Self {
            action: Some(NuAction::Use { path }),
            version: None,
            force: false,
            skip_path: false,
            yes,
            remove: false,
            use_path: false,
            use_existing: None,
        }
    }

    /// Construct args for removing the managed Nu.
    pub fn remove(yes: bool) -> Self {
        Self {
            action: Some(NuAction::Remove),
            version: None,
            force: false,
            skip_path: false,
            yes,
            remove: false,
            use_path: false,
            use_existing: None,
        }
    }
}

#[derive(Debug, Args)]
pub struct LoaderArgs {
    /// Overwrite an existing loader.nu without prompting
    #[arg(long)]
    pub force: bool,

    /// Append the loader source line to config.nu when it is not already present
    #[arg(long)]
    pub configure: bool,

    /// Skip confirmation prompts
    #[arg(long)]
    pub yes: bool,
}

pub fn execute(cmd: SetupCommands, root: &Path) -> Result<()> {
    match cmd {
        SetupCommands::Nu(args) => execute_nu(&args, root),
        SetupCommands::Loader(args) => execute_loader(&args, root),
    }
}

pub fn execute_nu(args: &NuSetupArgs, root: &Path) -> Result<()> {
    let _lock = acquire_mutation_lock(root)?;
    execute_nu_impl(args, root)
}

/// Setup Nu without acquiring the mutation lock (caller must hold it).
pub(crate) fn execute_nu_impl(args: &NuSetupArgs, root: &Path) -> Result<()> {
    // COMPAT: remove in v0.3.0 — translate hidden legacy flags to subcommands
    if args.remove {
        eprintln!("warning: --remove is deprecated, use 'numan setup nu remove' instead");
        return remove_managed_nu(root, args.yes);
    }
    if args.use_path {
        eprintln!("warning: --use-path is deprecated, use 'numan setup nu path' instead");
        return execute_use_path(args.yes, root);
    }
    if let Some(existing) = &args.use_existing {
        eprintln!("warning: --use-existing is deprecated, use 'numan setup nu use <path>' instead");
        if args.skip_path {
            bail!(
                "numan setup nu use cannot be combined with --skip-path. \
                 Off-PATH registration must persist the binary directory to PATH."
            );
        }
        return execute_use_existing(existing, args.yes, root);
    }

    match &args.action {
        Some(NuAction::Remove) => remove_managed_nu(root, args.yes),
        Some(NuAction::Path) => execute_use_path(args.yes, root),
        Some(NuAction::Use { path }) => {
            if args.skip_path {
                bail!(
                    "numan setup nu use cannot be combined with --skip-path. \
                     Off-PATH registration must persist the binary directory to PATH."
                );
            }
            execute_use_existing(path, args.yes, root)
        }
        None => {
            // Default: install (latest or pinned version)
            let options = NuSetupOptions {
                yes: args.yes,
                force: args.force,
                skip_path: args.skip_path,
                version: args.version.clone(),
            };
            let platform = Platform::detect();
            bootstrap::execute_nu_setup(root, &platform, &options)?;
            Ok(())
        }
    }
}

fn execute_use_path(yes: bool, root: &Path) -> Result<()> {
    let path_nu = find_nu_on_path()?;
    println!("Found Nu on PATH: {path_nu}");

    let managed_dir = bootstrap::managed_nu_dir(root);
    if managed_dir.is_dir() {
        let resolved_path_nu = Path::new(&path_nu)
            .canonicalize()
            .with_context(|| format!("Failed to resolve PATH Nu '{}'", path_nu))?;
        let resolved_managed_dir = managed_dir.canonicalize().with_context(|| {
            format!(
                "Failed to resolve managed Nushell directory '{}'",
                managed_dir.display()
            )
        })?;
        if resolved_path_nu.starts_with(&resolved_managed_dir) {
            bail!(
                "PATH Nu resolves to the managed install; install a separate Nu or use `setup nu remove`."
            );
        }
    }

    remove_managed_nu_if_present(root)?;
    let options = NuSetupOptions {
        yes,
        force: false,
        skip_path: false,
        version: None,
    };
    bootstrap::register_existing_nu(Path::new(&path_nu), &options)?;
    Ok(())
}

fn execute_use_existing(path: &Path, yes: bool, root: &Path) -> Result<()> {
    remove_managed_nu_if_present(root)?;
    let options = NuSetupOptions {
        yes,
        force: false,
        skip_path: false,
        version: None,
    };
    bootstrap::register_existing_nu(path, &options)?;
    Ok(())
}

/// Remove the managed Nushell install, prompting unless `--yes`.
fn remove_managed_nu(root: &Path, yes: bool) -> Result<()> {
    let managed_dir = bootstrap::managed_nu_dir(root);
    if !managed_dir.is_dir() {
        println!(
            "No managed Nushell install found at '{}'.",
            managed_dir.display()
        );
        return Ok(());
    }

    confirm_or_bail(
        &format!(
            "Remove managed Nushell at '{}'? Numan will fall back to PATH Nu.",
            managed_dir.display()
        ),
        yes,
        "Managed Nushell removal cancelled.",
    )?;

    std::fs::remove_dir_all(&managed_dir).with_context(|| {
        format!(
            "Failed to remove managed Nushell directory '{}'",
            managed_dir.display()
        )
    })?;
    println!(
        "Removed managed Nushell at '{}'. Run 'numan init --refresh' to re-detect Nu.",
        managed_dir.display()
    );
    Ok(())
}

/// Silently remove the managed Nu directory if it exists (used by --use-existing).
fn remove_managed_nu_if_present(root: &Path) -> Result<()> {
    let managed_dir = bootstrap::managed_nu_dir(root);
    if managed_dir.is_dir() {
        std::fs::remove_dir_all(&managed_dir).with_context(|| {
            format!(
                "Failed to remove managed Nushell directory '{}'",
                managed_dir.display()
            )
        })?;
        println!(
            "Removed managed Nushell at '{}' (replaced by --use-existing).",
            managed_dir.display()
        );
    }
    Ok(())
}

pub fn execute_loader(args: &LoaderArgs, root: &Path) -> Result<()> {
    execute_loader_with_probe(args, || {
        let nu_exe = find_nu_executable_with_root(root)?;
        probe_nu_config_path(&nu_exe)
    })
}

pub fn execute_loader_with_probe<F>(args: &LoaderArgs, probe: F) -> Result<()>
where
    F: FnOnce() -> Result<PathBuf>,
{
    let config_path = probe()?;
    let config_dir = config_path
        .parent()
        .context("Nu config path has no parent directory")?;
    let loader_path = config_dir.join("loader.nu");

    std::fs::create_dir_all(config_dir).with_context(|| {
        format!(
            "Failed to create Nu config directory '{}'",
            config_dir.display()
        )
    })?;

    install_loader_file(&loader_path, args)?;

    if args.configure {
        configure_config_nu(&config_path, args)?;
    } else {
        print_manual_snippet(&config_path);
    }

    print_next_steps(&loader_path, args.configure);
    Ok(())
}

fn install_loader_file(loader_path: &Path, args: &LoaderArgs) -> Result<()> {
    if loader_path.exists() && !args.force {
        if !loader_path.is_file() {
            bail!(
                "Refusing to overwrite non-file at '{}'.",
                loader_path.display()
            );
        }

        let existing = std::fs::read_to_string(loader_path).with_context(|| {
            format!(
                "Failed to read existing loader at '{}'",
                loader_path.display()
            )
        })?;
        if existing == VENDOR_LOADER {
            println!(
                "Loader already installed at '{}' (unchanged).",
                loader_path.display()
            );
            return Ok(());
        }

        assert_managed_file_owned(loader_path)?;

        if !args.force {
            confirm_or_bail(
                &format!(
                    "loader.nu already exists at '{}'. Overwrite with the vendored copy?",
                    loader_path.display()
                ),
                args.yes,
                "Loader install cancelled.",
            )?;
        }
    }

    assert_not_symlink(loader_path, "loader.nu")?;

    write_bytes_atomic(loader_path, VENDOR_LOADER.as_bytes()).with_context(|| {
        format!(
            "Failed to write loader script to '{}'",
            loader_path.display()
        )
    })?;

    println!("Installed nushell-loader to '{}'.", loader_path.display());
    Ok(())
}

fn configure_config_nu(config_path: &Path, args: &LoaderArgs) -> Result<()> {
    assert_not_symlink(config_path, "config.nu")?;
    if config_path.exists() && !config_path.is_file() {
        bail!(
            "Refusing to modify non-file config at '{}'.",
            config_path.display()
        );
    }

    if config_path.exists() {
        let content = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read '{}'", config_path.display()))?;
        if config_already_sources_loader(&content) {
            println!(
                "'{}' already sources loader.nu (unchanged).",
                config_path.display()
            );
            return Ok(());
        }

        if !crate::util::confirm::confirm_or_auto(
            &format!("Append loader source line to '{}'?", config_path.display()),
            args.yes,
        )? {
            print_manual_snippet(config_path);
            return Ok(());
        }

        let updated = format!("{}{CONFIG_SNIPPET}", content.trim_end());
        write_bytes_atomic(config_path, updated.as_bytes())
            .with_context(|| format!("Failed to update '{}'", config_path.display()))?;
        println!(
            "Appended loader source line to '{}'.",
            config_path.display()
        );
        return Ok(());
    }

    write_bytes_atomic(
        config_path,
        format!("{CONFIG_SNIPPET}\n").trim_start().as_bytes(),
    )
    .with_context(|| format!("Failed to create '{}'", config_path.display()))?;
    println!(
        "Created '{}' with loader source line.",
        config_path.display()
    );
    Ok(())
}

fn print_manual_snippet(config_path: &Path) {
    println!();
    println!("Add this at the end of '{}':", config_path.display());
    println!("{CONFIG_SNIPPET}");
}

fn print_next_steps(loader_path: &Path, configured: bool) {
    println!();
    println!("Next steps:");
    println!(
        "  1. Edit '{}' and add entries to aidnem_loader_configs.",
        loader_path.display()
    );
    println!("     Example:");
    println!("       {{name: 'starship', command: \"starship init nu\"}}");
    if !configured {
        println!("  2. Source loader.nu from config.nu (see snippet above).");
        println!("  3. Restart Nu. First startup generates caches; later startups are faster.");
    } else {
        println!("  2. Restart Nu. First startup generates caches; later startups are faster.");
    }
    println!();
    println!(
        "Numan module autoloads use the same vendor/autoload directory via numan.nu \
         and are unaffected by loader caches."
    );
    println!("Upstream: https://github.com/aidnem/nushell-loader");
}

pub fn config_already_sources_loader(content: &str) -> bool {
    content.contains(CONFIG_SOURCE_LINE)
        || content.contains("path join 'loader.nu'")
        || content.contains("path join \"loader.nu\"")
        || content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("source ") && trimmed.to_ascii_lowercase().contains("loader.nu")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn config_detection_finds_exact_source_line() {
        let content = format!("export-env {{}}\n{CONFIG_SOURCE_LINE}\n");
        assert!(config_already_sources_loader(&content));
    }

    #[test]
    fn config_detection_finds_literal_loader_source() {
        let content = "source ~/.config/nushell/loader.nu\n";
        assert!(config_already_sources_loader(content));
    }

    #[test]
    fn config_detection_false_when_absent() {
        assert!(!config_already_sources_loader("use std/log\n"));
    }

    #[test]
    fn install_loader_writes_vendored_copy() {
        let dir = TempDir::new().unwrap();
        let loader_path = dir.path().join("loader.nu");
        let args = LoaderArgs {
            force: false,
            configure: false,
            yes: true,
        };

        install_loader_file(&loader_path, &args).unwrap();
        let written = std::fs::read_to_string(&loader_path).unwrap();
        assert_eq!(written, VENDOR_LOADER);
    }

    #[test]
    fn install_loader_skips_when_unchanged() {
        let dir = TempDir::new().unwrap();
        let loader_path = dir.path().join("loader.nu");
        write_bytes_atomic(&loader_path, VENDOR_LOADER.as_bytes()).unwrap();

        let args = LoaderArgs {
            force: false,
            configure: false,
            yes: true,
        };
        install_loader_file(&loader_path, &args).unwrap();
        assert_eq!(
            std::fs::read(&loader_path).unwrap(),
            VENDOR_LOADER.as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn configure_rejects_symlinked_config() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.real.nu");
        std::fs::write(&target, "export-env {}\n").unwrap();
        let config_path = dir.path().join("config.nu");
        symlink(&target, &config_path).unwrap();

        let args = LoaderArgs {
            force: false,
            configure: true,
            yes: true,
        };
        let err = configure_config_nu(&config_path, &args).unwrap_err();
        assert!(err.to_string().contains("symlink"));
    }
    #[test]
    fn configure_appends_snippet_to_existing_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.nu");
        std::fs::write(&config_path, "export-env {}\n").unwrap();

        let args = LoaderArgs {
            force: false,
            configure: true,
            yes: true,
        };
        configure_config_nu(&config_path, &args).unwrap();

        let updated = std::fs::read_to_string(&config_path).unwrap();
        assert!(config_already_sources_loader(&updated));
        assert!(updated.starts_with("export-env {}\n"));
    }

    #[test]
    fn execute_loader_with_probe_installs_next_to_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.nu");
        std::fs::write(&config_path, "export-env {}\n").unwrap();

        let args = LoaderArgs {
            force: false,
            configure: true,
            yes: true,
        };

        execute_loader_with_probe(&args, || Ok(config_path.clone())).unwrap();
        assert!(dir.path().join("loader.nu").is_file());
        let config = std::fs::read_to_string(&config_path).unwrap();
        assert!(config_already_sources_loader(&config));
    }

    #[test]
    fn remove_managed_nu_removes_directory() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let managed_dir = bootstrap::managed_nu_dir(&root);
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("nu.exe"), b"fake").unwrap();

        remove_managed_nu(&root, true).unwrap();
        assert!(!managed_dir.exists());
    }

    #[test]
    fn remove_managed_nu_noop_when_absent() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        // Should succeed without error when nothing is installed.
        remove_managed_nu(&root, true).unwrap();
    }

    #[test]
    fn remove_managed_nu_if_present_clears_directory() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let managed_dir = bootstrap::managed_nu_dir(&root);
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("nu.exe"), b"fake").unwrap();

        remove_managed_nu_if_present(&root).unwrap();
        assert!(!managed_dir.exists());
    }

    #[test]
    fn remove_managed_nu_if_present_noop_when_absent() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        remove_managed_nu_if_present(&root).unwrap();
    }

    #[test]
    fn execute_nu_impl_remove_flag_delegates() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let managed_dir = bootstrap::managed_nu_dir(&root);
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("nu.exe"), b"fake").unwrap();

        let args = NuSetupArgs {
            action: None,
            version: None,
            force: false,
            skip_path: false,
            yes: true,
            remove: true,
            use_path: false,
            use_existing: None,
        };
        execute_nu_impl(&args, &root).unwrap();
        assert!(!managed_dir.exists());
    }
}
