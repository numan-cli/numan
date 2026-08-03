use anyhow::{bail, Context, Result};
use clap::Args;
use std::path::Path;

use crate::config::{Config, RegistryConfig};
use crate::core::official_registry::OFFICIAL_REGISTRY;
use crate::nu::autoload::{validate_candidate, CandidateRunner, NuCandidateRunner};
use crate::nu::paths::NuPaths;
use crate::state::autoload_state::AutoloadState;
use crate::state::lockfile::Lockfile;
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::format_timestamp;
use crate::util::fs_safety::acquire_mutation_lock;
use crate::util::fs_safety::assert_managed_file_owned;
use crate::util::hints::{
    self, CMD_ACTIVATE, CMD_INIT_REFRESH, CMD_REGISTRY_ADD, CMD_REGISTRY_SYNC, CMD_TRY,
};

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Re-probe Nu and refresh cached paths and activation identity records
    #[arg(long)]
    pub refresh: bool,
}

pub fn execute(args: &InitArgs, root: &Path) -> Result<()> {
    execute_with_runner(
        args,
        root,
        || NuPaths::detect_with_root(root),
        nu_runner_factory,
    )
}

fn nu_runner_factory(exe: &str) -> Box<dyn CandidateRunner> {
    Box::new(NuCandidateRunner::new(exe))
}

pub fn execute_with_runner<F>(
    args: &InitArgs,
    root: &Path,
    detect: F,
    runner_factory: fn(&str) -> Box<dyn CandidateRunner>,
) -> Result<()>
where
    F: Fn() -> Result<NuPaths>,
{
    let paths_exist = root.join("nu_state/paths.json").exists();

    if paths_exist && !args.refresh {
        bail!(
            "Numan is already initialized at '{}'. \
             {}",
            root.display(),
            hints::run(CMD_INIT_REFRESH)
        );
    }

    if args.refresh {
        return execute_refresh(root, detect, runner_factory);
    }

    execute_first_init(root, detect)
}

fn execute_first_init<F>(root: &Path, detect: F) -> Result<()>
where
    F: Fn() -> Result<NuPaths>,
{
    let paths = detect()?;
    ensure_layout_dirs(root)?;
    paths.save(root)?;

    let config_path = root.join("config.toml");
    let config_created = !config_path.exists();
    let mut config = if config_created {
        let config = Config::default();
        config.save(root)?;
        println!("Created default config at '{}'.", config_path.display());
        config
    } else {
        Config::load(root)?
    };

    let official_added = ensure_official_registry_config(root, &mut config)?;
    if official_added {
        println!(
            "Configured official registry '{}' at {}.",
            OFFICIAL_REGISTRY.name, OFFICIAL_REGISTRY.production_url
        );
    }

    print_summary(root, &paths, false);
    warn_missing_vendor_target(&paths);

    if config_created || config.registries.is_empty() || official_added {
        print_onboarding_next_steps(official_registry_configured(&config));
    }

    Ok(())
}

/// When the built-in trust root is production-ready, seed the official registry
/// in config so first-time users can `registry sync` without manual `registry add`.
pub(crate) fn ensure_official_registry_config(root: &Path, config: &mut Config) -> Result<bool> {
    if OFFICIAL_REGISTRY.is_placeholder_key() {
        return Ok(false);
    }
    if config.registries.contains_key(OFFICIAL_REGISTRY.name) {
        return Ok(false);
    }

    config.registries.insert(
        OFFICIAL_REGISTRY.name.to_string(),
        RegistryConfig {
            url: OFFICIAL_REGISTRY.production_url.to_string(),
            sync_interval: "24h".to_string(),
            enabled: true,
            trust_key: None,
        },
    );
    config.save(root)?;
    Ok(true)
}

fn official_registry_configured(config: &Config) -> bool {
    config.registries.contains_key(OFFICIAL_REGISTRY.name)
}

fn print_onboarding_next_steps(official_configured: bool) {
    println!();
    println!("Next steps:");
    if official_configured {
        println!("  1. Sync the registry index:");
        println!("     {CMD_REGISTRY_SYNC}");
        println!("  2. Prove it works (installs + activates a package for your Nu):");
        println!("     {CMD_TRY}");
        println!("  3. Or search and install manually:");
        println!("     numan search <query>");
        println!("     numan install owner/name");
        println!("     {CMD_ACTIVATE}");
    } else {
        println!("  1. Add a registry:");
        println!("     {CMD_REGISTRY_ADD}");
        println!("  2. Sync the index:");
        println!("     {CMD_REGISTRY_SYNC}");
        println!("  3. Prove it works:");
        println!("     {CMD_TRY}");
        println!("  4. Or search and install manually:");
        println!("     numan search <query>");
        println!("     numan install owner/name");
        println!("     {CMD_ACTIVATE}");
    }
    println!();
    println!(
        "Run 'numan doctor' to verify setup and apply safe repairs (use 'numan doctor --scan' for report-only output)."
    );
}

fn execute_refresh<F>(
    root: &Path,
    detect: F,
    runner_factory: fn(&str) -> Box<dyn CandidateRunner>,
) -> Result<()>
where
    F: Fn() -> Result<NuPaths>,
{
    let old_paths = NuPaths::load(root)?;
    let new_paths = detect()?;

    let lockfile = Lockfile::load(root)?;
    let has_active_modules = lockfile
        .packages
        .values()
        .any(|entry| entry.module_activation.is_some());

    // Always serialize refresh writes (lockfile, autoload-state, paths), even
    // when no packages are active. Skipping the lock left a race window against
    // concurrent mutators.
    let _lock = acquire_mutation_lock(root)?;
    create_snapshot(
        root,
        SnapshotReason::PreMutation,
        SnapshotTrigger::Init,
        None,
        None,
    )
    .context("Failed to create pre-mutation snapshot for `numan init --refresh`")?;

    if has_active_modules {
        validate_refresh_for_active_modules(
            root,
            &old_paths,
            &new_paths,
            &lockfile,
            runner_factory,
        )?;
    }

    let mut lockfile = lockfile;
    refresh_activation_records(&mut lockfile, &new_paths)?;
    lockfile.nu_version = new_paths.nu_version.clone();
    lockfile.platform = new_paths.platform.clone();
    lockfile.generated_at = format_timestamp();
    lockfile.save(root)?;

    refresh_autoload_state_identity(root, &new_paths)?;

    new_paths.save(root)?;
    print_summary(root, &new_paths, true);
    warn_missing_vendor_target(&new_paths);
    Ok(())
}

fn ensure_layout_dirs(root: &Path) -> Result<()> {
    for dir in ["nu_state", "state", "packages", "registries"] {
        std::fs::create_dir_all(root.join(dir)).with_context(|| {
            format!("Failed to create directory '{}'", root.join(dir).display())
        })?;
    }
    Ok(())
}

fn validate_refresh_for_active_modules(
    root: &Path,
    old_paths: &NuPaths,
    new_paths: &NuPaths,
    lockfile: &Lockfile,
    runner_factory: fn(&str) -> Box<dyn CandidateRunner>,
) -> Result<()> {
    if !vendor_targets_compatible(old_paths, new_paths) {
        bail!(
            "Vendor-autoload target changed since last init. \
             Numan will not migrate the managed autoload file automatically.\n\
             Old: {:?}\nNew: {:?}\n\
             Deactivate active modules, fix Nu configuration, then {}.",
            old_paths.vendor_autoload_dir,
            new_paths.vendor_autoload_dir,
            hints::run(CMD_INIT_REFRESH)
        );
    }

    let vendor_dir = new_paths
        .vendor_autoload_dir
        .as_deref()
        .context("Active modules require a Numan-safe vendor-autoload directory")?;

    let managed_file = Path::new(vendor_dir).join("numan.nu");
    if !managed_file.is_file() {
        bail!(
            "Managed autoload file '{}' is missing. \
             {}",
            managed_file.display(),
            hints::run_then(CMD_ACTIVATE, CMD_INIT_REFRESH)
        );
    }

    assert_managed_file_owned(&managed_file)?;

    let active_ids: Vec<String> = lockfile
        .packages
        .iter()
        .filter(|(_, entry)| entry.module_activation.is_some())
        .map(|(id, _)| id.clone())
        .collect();

    let scoped_refs: Vec<&str> = active_ids.iter().map(String::as_str).collect();
    let runner = runner_factory(&new_paths.nu_executable);
    validate_candidate(&managed_file, runner.as_ref(), &scoped_refs)?;

    // Touch autoload-state path consistency — projection is updated after lockfile save.
    let _ = root;
    Ok(())
}

fn vendor_targets_compatible(old_paths: &NuPaths, new_paths: &NuPaths) -> bool {
    old_paths.vendor_autoload_dir == new_paths.vendor_autoload_dir
}

fn refresh_activation_records(lockfile: &mut Lockfile, new_paths: &NuPaths) -> Result<()> {
    for entry in lockfile.packages.values_mut() {
        if let Some(ref mut activation) = entry.activation {
            activation.nu_executable_sha256 = new_paths.nu_executable_hash.clone();
            activation.nu_version = new_paths.nu_version.clone();
            activation.plugin_registry_path = new_paths.plugin_registry_path.clone();
        }
        if let Some(ref mut activation) = entry.module_activation {
            activation.nu_executable_sha256 = new_paths.nu_executable_hash.clone();
            activation.nu_version = new_paths.nu_version.clone();
        }
    }
    Ok(())
}

fn refresh_autoload_state_identity(root: &Path, new_paths: &NuPaths) -> Result<()> {
    let Some(mut state) = AutoloadState::load(root)? else {
        return Ok(());
    };

    state.nu_executable_sha256 = new_paths.nu_executable_hash.clone();
    state.nu_version = new_paths.nu_version.clone();
    state.save(root)
}

fn warn_missing_vendor_target(paths: &NuPaths) {
    if paths.vendor_autoload_dir.is_none() {
        eprintln!(
            "\nWarning: No Numan-safe vendor-autoload directory found.\n\
             Module activation requires <data-dir>/vendor/autoload to appear in $nu.vendor-autoload-dirs."
        );
    }
}

fn print_summary(root: &Path, paths: &NuPaths, refreshed: bool) {
    let action = if refreshed {
        "Refreshed"
    } else {
        "Initialized"
    };
    println!("{action} Numan at '{}'.", root.display());
    println!("  Nu executable: {}", paths.nu_executable);
    println!("  Nu version:    {}", paths.nu_version);
    println!("  Plugin registry: {}", paths.plugin_registry_path);
    println!("  Platform:      {}", paths.platform);
    match &paths.vendor_autoload_dir {
        Some(dir) => println!("  Vendor autoload: {dir}"),
        None => println!("  Vendor autoload: (none — module activation unavailable)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::integrity;
    use crate::nu::autoload::FakeCandidateRunner;
    use crate::state::lockfile::{LockfileEntry, PluginActivation};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn fake_paths(root: &Path, nu_exe: &Path, vendor: Option<&str>) -> NuPaths {
        let bytes = std::fs::read(nu_exe).unwrap();
        NuPaths {
            nu_executable: nu_exe.to_string_lossy().into_owned(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: root.join("plugins.msgpackz").to_string_lossy().into_owned(),
            nu_executable_hash: integrity::compute_sha256(&bytes),
            platform: "test".to_string(),
            data_dir: Some(root.join("data").to_string_lossy().into_owned()),
            vendor_autoload_dirs: vendor.map(|v| vec![v.to_string()]).unwrap_or_default(),
            vendor_autoload_dir: vendor.map(|s| s.to_string()),
        }
    }

    fn make_detect(paths: NuPaths) -> impl Fn() -> Result<NuPaths> {
        move || Ok(paths.clone())
    }

    fn plugin_entry(payload_path: &str, activation: Option<PluginActivation>) -> LockfileEntry {
        LockfileEntry {
            version: "1.0.0".to_string(),
            package_type: "plugin".to_string(),
            source: "binary".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: Some("nu_plugin_test".to_string()),
            archive_root: None,
            include: None,
            entry: None,
            installed_at: "now".to_string(),
            nu_version_at_install: None,
            activation,
            registry_url: None,
            registry_revision: None,
            index_sha256: None,
            signing_key_fingerprint: None,
            git_url: None,
            git_rev: None,
            cargo_name: None,
            cargo_lock_sha256: None,
            built_sha256: None,
            payload_path: payload_path.to_string(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: BTreeMap::new(),
        }
    }

    fn fake_runner_factory(_exe: &str) -> Box<dyn CandidateRunner> {
        Box::new(FakeCandidateRunner::success())
    }

    #[test]
    fn init_creates_paths_and_default_config() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let nu_exe = root.join("nu");
        std::fs::write(&nu_exe, b"v1").unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        let vendor = root.join("data/vendor/autoload");
        std::fs::create_dir_all(&vendor).unwrap();

        let paths = fake_paths(root, &nu_exe, Some(&vendor.to_string_lossy()));
        let args = InitArgs { refresh: false };
        execute_with_runner(&args, root, make_detect(paths), fake_runner_factory).unwrap();

        assert!(root.join("nu_state/paths.json").is_file());
        assert!(root.join("config.toml").is_file());
    }

    #[test]
    fn init_refuses_second_run_without_refresh() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let nu_exe = root.join("nu");
        std::fs::write(&nu_exe, b"v1").unwrap();
        let paths = fake_paths(root, &nu_exe, None);
        let args = InitArgs { refresh: false };

        execute_with_runner(&args, root, make_detect(paths.clone()), fake_runner_factory).unwrap();
        let err = execute_with_runner(
            &InitArgs { refresh: false },
            root,
            make_detect(paths),
            fake_runner_factory,
        )
        .unwrap_err();
        assert!(err.to_string().contains("already initialized"));
    }

    #[test]
    fn refresh_updates_plugin_activation_identity() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let nu_v1 = root.join("nu_v1");
        let nu_v2 = root.join("nu_v2");
        std::fs::write(&nu_v1, b"v1").unwrap();
        std::fs::write(&nu_v2, b"v2").unwrap();

        let paths_v1 = fake_paths(root, &nu_v1, None);
        paths_v1.save(root).unwrap();

        let hash_v1 = paths_v1.nu_executable_hash.clone();
        let mut lockfile = Lockfile::empty();
        let payload_rel = "packages/plugins/owner/plugin/1.0.0-abc";
        let payload_dir = root.join(payload_rel);
        std::fs::create_dir_all(&payload_dir).unwrap();
        std::fs::write(payload_dir.join("nu_plugin_test"), b"bin").unwrap();
        lockfile.packages.insert(
            "owner/plugin".to_string(),
            plugin_entry(
                payload_rel,
                Some(PluginActivation {
                    plugin_registry_path: paths_v1.plugin_registry_path.clone(),
                    nu_executable_sha256: hash_v1,
                    nu_version: "0.113.1".to_string(),
                    activated_at: "now".to_string(),
                }),
            ),
        );
        lockfile.save(root).unwrap();

        let paths_v2 = fake_paths(root, &nu_v2, None);
        let args = InitArgs { refresh: true };
        execute_with_runner(&args, root, make_detect(paths_v2), fake_runner_factory).unwrap();

        let loaded = Lockfile::load(root).unwrap();
        let activation = loaded.packages["owner/plugin"].activation.as_ref().unwrap();
        assert_eq!(activation.nu_version, "0.113.1");
        assert_ne!(
            activation.nu_executable_sha256,
            integrity::compute_sha256(b"v1")
        );
        assert_eq!(
            activation.nu_executable_sha256,
            integrity::compute_sha256(b"v2")
        );
    }

    #[test]
    fn refresh_without_active_packages_still_locks_and_snapshots() {
        use crate::state::snapshot::list_snapshots;

        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let nu_v1 = root.join("nu_v1");
        let nu_v2 = root.join("nu_v2");
        std::fs::write(&nu_v1, b"v1").unwrap();
        std::fs::write(&nu_v2, b"v2").unwrap();

        let paths_v1 = fake_paths(root, &nu_v1, None);
        paths_v1.save(root).unwrap();
        Lockfile::empty().save(root).unwrap();

        let paths_v2 = fake_paths(root, &nu_v2, None);
        execute_with_runner(
            &InitArgs { refresh: true },
            root,
            make_detect(paths_v2.clone()),
            fake_runner_factory,
        )
        .unwrap();

        let loaded = NuPaths::load(root).unwrap();
        assert_eq!(loaded.nu_executable_hash, paths_v2.nu_executable_hash);
        let snapshots = list_snapshots(root).unwrap();
        assert!(
            snapshots.iter().any(|s| s.trigger == SnapshotTrigger::Init),
            "init --refresh must create a PreMutation Init snapshot even with no active packages"
        );
    }
}
