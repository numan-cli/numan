use crate::util::confirm::{confirm_or_bail, require_tty_or_yes};
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use std::path::{Path, PathBuf};

use crate::core::platform::Platform;
use crate::nu::bootstrap::{self, NuSetupOptions};
use crate::nu::paths::{
    find_nu_executable_with_root, find_nu_on_path, probe_nu_config_path, validate_nushell_binary,
};
use crate::nu::version_manager;
use crate::util::atomic::write_bytes_atomic;
use crate::util::fs_safety::{
    assert_managed_file_owned, assert_not_symlink, setup_subcommand_lock,
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
        /// Opt into the destructive two-step flow (delete the managed
        /// Nushell install at NUMAN_ROOT, then adopt the user-supplied
        /// binary as the active Nu). Required when a managed install
        /// exists; without it, the call refuses with a hint.
        #[arg(long)]
        force: bool,
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
    pub fn use_existing(path: PathBuf, yes: bool, force: bool) -> Self {
        Self {
            action: Some(NuAction::Use { path, force }),
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
    // PR69 WCr: every destructive setup entry acquires the root mutation
    // lock through `setup_subcommand_lock`. The helper audit-logs the
    // entry so safe-batch automation can grep one consistent `(audit)`
    // prefix across the destructive setup surface. Direct callers of
    // `execute_nu_impl` (e.g. `cmd::doctor::apply_repairs`) get the same
    // guarantee because `execute_nu_impl` itself acquires the lock.
    let what = setup_action_lock_label(args);
    setup_subcommand_lock(root, what, || execute_nu_impl_locked(args, root))
}

/// Short, audit-friendly label describing which destructive setup
/// subcommand is requesting the mutation lock.
fn setup_action_lock_label(args: &NuSetupArgs) -> &'static str {
    // Legacy hidden flags take precedence in `execute_nu_impl` and route
    // to the same leaf helpers as the explicit subcommands. We need the
    // lock label to reflect the *effective* action, not the literal
    // `args.action` field, so peek at the legacy flags first.
    if args.remove {
        return "managed Nushell removal";
    }
    if args.use_path {
        return "PATH Nu registration";
    }
    if args.use_existing.is_some() {
        return "off-path Nu registration";
    }
    match &args.action {
        Some(NuAction::Remove) => "managed Nushell removal",
        Some(NuAction::Path) => "PATH Nu registration",
        Some(NuAction::Use { .. }) => "off-path Nu registration",
        None => "Nushell install",
    }
}

/// Setup Nu under the root mutation lock. Public callers go through
/// [`execute_nu`] (which audit-labels the lock); direct callers of this
/// function are responsible for emitting their own audit prefix (e.g.
/// `cmd::doctor::apply_repairs` already logs `(doctor)` repair records).
pub(crate) fn execute_nu_impl(args: &NuSetupArgs, root: &Path) -> Result<()> {
    let what = setup_action_lock_label(args);
    setup_subcommand_lock(root, what, || execute_nu_impl_locked(args, root))
}

fn execute_nu_impl_locked(args: &NuSetupArgs, root: &Path) -> Result<()> {
    // COMPAT: remove in v0.3.0 — translate hidden legacy flags to subcommands
    if args.remove {
        eprintln!("warning: --remove is deprecated, use 'numan setup nu remove' instead");
        return remove_managed_nu(root, args.yes);
    }
    if args.use_path {
        eprintln!("warning: --use-path is deprecated, use 'numan setup nu path' instead");
        return execute_use_path(args.yes, root, ExecuteUseOpts::default());
    }
    if let Some(existing) = &args.use_existing {
        eprintln!("warning: --use-existing is deprecated, use 'numan setup nu use <path>' instead");
        if args.skip_path {
            bail!(
                "numan setup nu use cannot be combined with --skip-path. \
                 Off-PATH registration must persist the binary directory to PATH."
            );
        }
        return execute_use_existing(existing, args.yes, root, false, ExecuteUseOpts::default());
    }

    match &args.action {
        Some(NuAction::Remove) => remove_managed_nu(root, args.yes),
        Some(NuAction::Path) => execute_use_path(args.yes, root, ExecuteUseOpts::default()),
        Some(NuAction::Use { path, force }) => {
            if args.skip_path {
                bail!(
                    "numan setup nu use cannot be combined with --skip-path. \
                     Off-PATH registration must persist the binary directory to PATH."
                );
            }
            execute_use_existing(path, args.yes, root, *force, ExecuteUseOpts::default())
        }
        None => {
            // Default: install (latest or pinned version)
            let options = NuSetupOptions {
                yes: args.yes,
                force: args.force,
                skip_path: args.skip_path,
                version: args.version.clone(),
                caller_consented_destructive: false,
            };
            let platform = Platform::detect();
            bootstrap::execute_nu_setup(root, &platform, &options)?;
            Ok(())
        }
    }
}

/// Test seam for [`execute_use_existing`] / [`execute_use_path`].
///
/// Carry closures that override the production validators so unit tests
/// can mock the binary probe and the destructive-step confirm prompt.
/// Both fields default to `None`, which falls back to the production
/// behavior (`validate_nushell_binary` and
/// `crate::util::confirm::confirm_or_bail`).
#[derive(Default, Copy, Clone)]
pub(crate) struct ExecuteUseOpts<'a> {
    /// Override [`validate_nushell_binary`]. Pass `Some(&stub)` from a
    /// unit test to skip the real-Nu probe.
    #[allow(clippy::type_complexity)]
    pub(crate) validate: Option<&'a dyn Fn(&Path) -> Result<()>>,
    /// Override [`confirm_or_bail`]. Mirrors `confirm_or_bail`'s
    /// signature minus the `yes` flag, which is already bound by the
    /// caller. Tests can capture the prompt text (e.g. to assert it
    /// contains "no undo") and decline by returning `Err`.
    #[allow(clippy::type_complexity)]
    pub(crate) confirm: Option<&'a dyn Fn(&str, &str) -> Result<()>>,
}

/// Validate a user-supplied Nushell binary before destructive operations
/// proceed. The `validate_fn` seam lets unit tests bypass the real Nu
/// probe; production callers should pass `None` to fall through to
/// `validate_nushell_binary` (which runs `nu -c <probe>`).
#[allow(clippy::type_complexity)]
fn validate_user_supplied_nu(
    path: &Path,
    validate_fn: Option<&dyn Fn(&Path) -> Result<()>>,
) -> Result<()> {
    let resolved = path
        .canonicalize()
        .with_context(|| format!("Failed to resolve Nushell binary '{}'", path.display()))?;
    match validate_fn {
        Some(validator) => validator(&resolved),
        None => validate_nushell_binary(&resolved),
    }
    .with_context(|| format!("'{}' is not a runnable Nushell binary", path.display()))?;
    Ok(())
}

fn execute_use_path(yes: bool, root: &Path, opts: ExecuteUseOpts<'_>) -> Result<()> {
    // PR #69 WCt: the non-TTY guard must run before any destructive step.
    // `register_existing_nu` mutates the user's PATH and `remove_managed_nu_if_present`
    // deletes the entire managed tree, so refusing the operation on a pipe without
    // `--yes` is non-negotiable.
    require_tty_or_yes(yes, "PATH Nu registration")?;

    let path_nu = find_nu_on_path()?;
    println!("Found Nu on PATH: {path_nu}");

    // Validate before any destructive removal: a broken/unrunnable PATH
    // Nu must not leave us without a managed install.
    validate_user_supplied_nu(Path::new(&path_nu), opts.validate)?;

    let managed_dir = bootstrap::managed_nu_dir(root);
    let managed_dir_was_present = managed_dir.is_dir();
    if managed_dir_was_present {
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

        // Consolidate the destructive-removal confirm + the
        // register_existing_nu PATH-add prompt into one. Without this
        // merge, a user who declines the PATH prompt would already have
        // lost their managed install; with it, the user sees one prompt
        // covering both.
        let nu_parent = resolved_path_nu
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path_nu.clone());
        let prompt = format!(
            "Switching to PATH Nu will remove the managed Nushell install at '{}' \
             (this clears every installed version and the active-version marker; \
             there is no undo) and will add '{}' to your user PATH. Continue?",
            managed_dir.display(),
            nu_parent,
        );
        let cancel_msg = "Switch to PATH Nu cancelled; managed install kept intact.";
        // Gate on `!yes` so `confirm_or_bail`'s yes-skip contract holds
        // when callers inject a confirm seam for telemetry/audit.
        if !yes {
            match opts.confirm {
                Some(confirm_fn) => confirm_fn(&prompt, cancel_msg)?,
                None => confirm_or_bail(&prompt, false, cancel_msg)?,
            }
        }
    }

    remove_managed_nu_if_present(root)?;
    let options = NuSetupOptions {
        yes,
        force: false,
        skip_path: false,
        version: None,
        // Hoist consent so register_existing_nu's inner PATH prompt is
        // suppressed -- only valid because the merged prompt above
        // collected consent for both the delete AND the PATH add.
        caller_consented_destructive: managed_dir_was_present,
    };
    let registered = bootstrap::register_existing_nu(Path::new(&path_nu), &options)?;
    // chatgpt PR69 S08: persist the registered binary as the active version
    // marker so `numan use list` reports it as the selection.
    let external_label = crate::core::nu_version::NuVersion::from_binary(&registered)
        .with_context(|| {
            format!(
                "Failed to determine Nu version for '{}'",
                registered.display()
            )
        })?
        .version;
    version_manager::write_active_version_with_binary(
        root,
        &version_manager::normalize_version(&external_label)?,
        &registered,
    )?;
    Ok(())
}

fn execute_use_existing(
    path: &Path,
    yes: bool,
    root: &Path,
    force: bool,
    opts: ExecuteUseOpts<'_>,
) -> Result<()> {
    // PR #69 WCt: refuse the operation on a non-TTY session without
    // `--yes` *before* any PATH mutation or managed-tree removal.
    require_tty_or_yes(yes, "off-path Nu registration")?;

    // Validate before any destructive removal: an invalid binary must
    // not leave us without a managed install.
    validate_user_supplied_nu(path, opts.validate)?;

    // Consolidate the destructive-removal confirm + the
    // register_existing_nu PATH-add prompt into one (mirrors
    // `execute_use_path`'s gate). With a managed tree in place, the
    // `--force` flag is required to *enter* this path at all — the
    // merged warn-and-confirm below is the second stage of the
    // destructive two-step opt-in.
    let managed_dir = bootstrap::managed_nu_dir(root);
    let managed_dir_was_present = managed_dir.is_dir();
    if managed_dir_was_present && !force {
        bail!(
            "Refusing `numan setup nu use` while a managed Nushell install at '{}' exists.\n\n\
             The destructive two-step flow (delete the managed tree + adopt '{}') would \
             discard every installed version and the active-version marker. Re-run with \
             `--force` to opt into it, or run `numan setup nu remove` first to stage the \
             removal out-of-band so this subcommand can register the off-path Nu without \
             touching managed state.\n\n\
             Both flows are reversible only by `numan setup nu install <version>`.",
            managed_dir.display(),
            path.display(),
        );
    }
    if managed_dir_was_present {
        let resolved_path = std::fs::canonicalize(path)
            .with_context(|| format!("Failed to resolve Nushell binary '{}'", path.display()))?;
        let nu_parent = resolved_path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let prompt = format!(
            "Switching to '{}' will remove the managed Nushell install at '{}' \
             (this clears every installed version and the active-version marker; \
             there is no undo) and will add '{}' to your user PATH. Continue?",
            path.display(),
            managed_dir.display(),
            nu_parent,
        );
        let cancel_msg = "Switch to existing Nushell cancelled; managed install kept intact.";
        if !yes {
            match opts.confirm {
                Some(confirm_fn) => confirm_fn(&prompt, cancel_msg)?,
                None => confirm_or_bail(&prompt, false, cancel_msg)?,
            }
        }
    }

    remove_managed_nu_if_present(root)?;
    let options = NuSetupOptions {
        yes,
        force: false,
        skip_path: false,
        version: None,
        // Hoist consent so register_existing_nu's inner PATH prompt is
        // suppressed -- only valid because the merged prompt above
        // collected consent for both the delete AND the PATH add.
        caller_consented_destructive: managed_dir_was_present,
    };
    let registered = bootstrap::register_existing_nu(path, &options)?;
    // chatgpt PR69 S08: persist the registered binary as the active version
    // marker so `numan use list` reports it as the selection.
    let external_label = crate::core::nu_version::NuVersion::from_binary(&registered)
        .with_context(|| {
            format!(
                "Failed to determine Nu version for '{}'",
                registered.display()
            )
        })?
        .version;
    version_manager::write_active_version_with_binary(
        root,
        &version_manager::normalize_version(&external_label)?,
        &registered,
    )?;
    Ok(())
}

/// Remove the managed Nushell install, prompting unless `--yes`.
fn remove_managed_nu(root: &Path, yes: bool) -> Result<()> {
    // PR #69 WCt: refuse the operation on a non-TTY session without
    // `--yes` *before* any marker write or directory deletion.
    require_tty_or_yes(yes, "managed Nushell removal")?;

    // chatgpt PR69 S09: clear the active-version marker before deleting
    // the managed tree so the marker cannot dangle at a binary we are
    // about to remove.
    version_manager::clear_active_version(root)?;
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
    // chatgpt PR69 S09: clear the active-version marker before deleting
    // the managed tree so the marker cannot dangle at a binary we are
    // about to remove.
    version_manager::clear_active_version(root)?;
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

    // --- direct unit coverage for the new confirm gate in
    //     `execute_use_existing` ---

    /// Write a fake nu script that returns parseable output for both
    /// the Nu probe (`-c <script>`) and `--version`. Unix only because
    /// Windows binary probes work differently and are tested in
    /// setup_nu_test.rs with a real Nu binary.
    ///
    /// Note: the probe JSON string is built without escaping (no
    /// embedded double-quote chars inside the shell single-quoted
    /// string), so we can store it as a clean raw byte literal.
    #[cfg(unix)]
    fn write_fake_nu(tmp: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = tmp.join("fake-nu");
        let script: &[u8] = b"#!/bin/sh\n\
case $1 in\n\
  -c|--version) printf '{ \"version\":\"0.113.1\", \"plugin_path\":\"\", \"data_dir\":\"\", \"vendor_autoload_dirs\":[] }\n' ;;\n\
esac\n";
        std::fs::write(&bin, script).expect("write fake-nu script");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake-nu");
        bin
    }

    /// Save/restore process-global PATH around a closure so `prepend_*`
    /// calls don't leak across tests.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn run_with_path_snapshot<F, T>(body: F) -> T
    where
        F: FnOnce() -> T + std::panic::UnwindSafe,
    {
        let saved = std::env::var("PATH").ok();
        let result = std::panic::catch_unwind(body);
        match saved {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        match result {
            Ok(v) => v,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    #[cfg(unix)]
    #[test]
    fn execute_use_existing_passes_with_yes_and_drops_managed() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        std::fs::create_dir_all(&root).unwrap();

        // Stage a managed binary so the destructive-removal branch fires.
        let managed = root.join("tools").join("nushell").join("nu");
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::write(&managed, b"placeholder managed nu").unwrap();

        let fake_nu = write_fake_nu(dir.path());

        run_with_path_snapshot(std::panic::AssertUnwindSafe(|| {
            execute_use_existing(&fake_nu, true, &root, true, ExecuteUseOpts::default()).unwrap();
        }));

        assert!(
            !managed.is_file(),
            "managed binary at {} must be removed",
            managed.display()
        );
        assert!(
            !root.join("tools/nushell").exists(),
            "tools/nushell tree must be cleaned up"
        );
    }

    #[cfg(unix)]
    #[test]
    fn execute_use_existing_no_prompts_above_when_no_managed_install() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        std::fs::create_dir_all(&root).unwrap();

        let fake_nu = write_fake_nu(dir.path());

        // No managed tree, so the destructive-removal confirm prompt is
        // gated off (`managed_dir_was_present == false`). End-to-end
        // success via register_existing_nu is the assertion.
        run_with_path_snapshot(std::panic::AssertUnwindSafe(|| {
            execute_use_existing(&fake_nu, true, &root, false, ExecuteUseOpts::default()).unwrap();
        }));
        assert!(
            !root.join("tools/nushell").exists(),
            "no managed tree initially -> nothing to remove"
        );
    }

    #[cfg(unix)]
    #[test]
    fn execute_use_existing_decline_keeps_managed_intact_and_prompt_says_no_undo() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        std::fs::create_dir_all(&root).unwrap();

        let managed = root.join("tools").join("nushell").join("nu");
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::write(&managed, b"placeholder managed nu").unwrap();

        let fake_nu = write_fake_nu(dir.path());

        // Mock the production confirm seam. The closure captures the
        // prompt text so the test can assert its literal content, then
        // declines so the destructive removal is short-circuited.
        let captured_prompt = std::sync::Mutex::new(String::new());
        let mock_confirm = |prompt: &str, _cancel_msg: &str| -> anyhow::Result<()> {
            captured_prompt.lock().unwrap().push_str(prompt);
            Err(anyhow::anyhow!("declined in test (mock confirm)"))
        };
        let opts = ExecuteUseOpts {
            validate: None,
            confirm: Some(&mock_confirm),
        };
        let result = run_with_path_snapshot(std::panic::AssertUnwindSafe(|| {
            execute_use_existing(&fake_nu, false, &root, true, opts)
        }));
        let err_msg = match result {
            Ok(()) => panic!("expected Err from declined confirm"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            err_msg.contains("declined in test"),
            "expected declined-by-mock error, got: {err_msg}"
        );

        let prompt = captured_prompt.lock().unwrap().clone();
        assert!(
            prompt.contains("no undo"),
            "merged prompt must contain the literal 'no undo'; got:\n{prompt}"
        );

        assert!(
            managed.is_file(),
            "managed binary at {} must remain intact after decline",
            managed.display()
        );
        assert!(
            root.join("tools/nushell").is_dir(),
            "managed tree must remain intact after decline"
        );
    }
}
