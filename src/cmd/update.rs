use anyhow::{bail, Context, Result};
use clap::Parser;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::cmd::plugin_lifecycle::{CommandPluginLifecycle, PluginLifecycle};
use crate::core::nu_version::NuVersion;
use crate::core::package::RegistryIndex;
use crate::core::platform::Platform;
use crate::core::registry::RegistryManager;
use crate::core::resolve::Resolver;
use crate::install::transaction::{self, InstallOptions, InstallResult};
use crate::nu::paths::NuPaths;
use crate::state::active_plugin_mutation;
use crate::state::lifecycle_journal::{
    check_stale_journal, LifecycleOp, LifecycleStage, PendingLifecycle,
};
use crate::state::lockfile::{Lockfile, LockfileEntry};
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::fs_safety::acquire_mutation_lock;
use crate::util::hints;

/// Update installed packages to their latest compatible versions
#[derive(Parser)]
pub struct UpdateArgs {
    /// Package to update (owner/name). Omit to check/update all packages.
    package: Option<String>,

    /// Report available updates without installing
    #[arg(long)]
    check: bool,

    /// Update the numan binary itself from GitHub Releases (or print the
    /// package-manager upgrade command for Homebrew / winget / cargo installs)
    #[arg(long = "self")]
    self_update: bool,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

struct PendingUpdate {
    package_id: String,
    from_version: String,
    to_version: String,
    orphan_path: String,
    registry_name: String,
}

/// How an active lockfile entry should be handled during update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveUpdatePlan {
    /// No plugin/module activation; plain upgrade.
    Plain,
    /// Plugin activation present and mutation flag enabled: deactivate → install → activate.
    OrchestrateActivePlugin,
}

/// Injectable install seam for update tests (production uses `transaction::install_package`).
pub type UpdateInstallFn = dyn Fn(&str, Option<&str>, &InstallOptions<'_>) -> Result<InstallResult>;

/// Injectable seams for update orchestration (tests inject fakes).
pub struct UpdateHooks<'a> {
    pub lifecycle: &'a dyn PluginLifecycle,
    /// When `None`, uses [`transaction::install_package`].
    pub install: Option<&'a UpdateInstallFn>,
}

pub fn execute(args: &UpdateArgs, root: &PathBuf) -> Result<()> {
    let lifecycle = CommandPluginLifecycle;
    execute_with_hooks(
        args,
        root,
        &UpdateHooks {
            lifecycle: &lifecycle,
            install: None,
        },
    )
}

/// Testability entry: inject deactivate/activate/install seams.
pub fn execute_with_hooks(
    args: &UpdateArgs,
    root: &PathBuf,
    hooks: &UpdateHooks<'_>,
) -> Result<()> {
    if args.self_update {
        if args.package.is_some() {
            bail!("--self cannot be combined with a package name");
        }
        // Self-update replaces the CLI binary; it does not touch package state
        // and must not take the root mutation lock.
        return crate::cmd::self_update::execute(args.check, args.verbose);
    }

    if args.check {
        warn_stale_lifecycle_journal(root)?;
    }

    let platform = Platform::detect();
    let path_nu_version = NuVersion::detect().unwrap_or_else(|e| {
        eprintln!("Warning: Could not detect Nu version: {e}");
        NuVersion {
            version: "unknown".to_string(),
            major: 0,
            minor: 0,
            patch: 0,
        }
    });
    // Prefer cached Nu identity when present so discovery and install share one
    // Nu version (including Plain updates when PATH Nu differs or is missing).
    let cached_nu_paths = NuPaths::load(root).ok();
    let resolve_nu_version =
        resolve_nu_version_for_update(cached_nu_paths.as_ref(), &path_nu_version);

    if args.check {
        let lockfile = Lockfile::load(root)?;
        if lockfile.is_empty() {
            println!("No packages installed.");
            return Ok(());
        }

        let registry = RegistryManager::new(root)?;
        let default_reg = registry.default_registry_name();
        let resolver = Resolver::new(&platform, &resolve_nu_version);
        let packages_to_check = packages_to_check(args, &lockfile)?;
        let mut index_cache = HashMap::new();
        let updates = discover_pending_updates(
            &lockfile,
            &packages_to_check,
            &default_reg,
            &registry,
            &resolver,
            args.verbose,
            &mut index_cache,
        )?;

        if updates.is_empty() {
            println!("All packages are up to date.");
            return Ok(());
        }

        println!("Updates available ({}):", updates.len());
        for update in &updates {
            println!(
                "  {}  {} → {}",
                update.package_id, update.from_version, update.to_version
            );
        }
        return Ok(());
    }

    let _lock = acquire_mutation_lock(root)?;
    // Finish interrupted active-plugin reactivation before discovery so a
    // LockfileUpdated package is not treated as already up to date.
    resume_interrupted_reactivate(root, hooks)?;
    warn_stale_lifecycle_journal(root)?;

    let lockfile = Lockfile::load(root)?;
    if lockfile.is_empty() {
        println!("No packages installed.");
        return Ok(());
    }

    let registry = RegistryManager::new(root)?;
    let default_reg = registry.default_registry_name();
    let resolver = Resolver::new(&platform, &resolve_nu_version);
    let packages_to_check = packages_to_check(args, &lockfile)?;
    let mut index_cache = HashMap::new();
    let updates = discover_pending_updates(
        &lockfile,
        &packages_to_check,
        &default_reg,
        &registry,
        &resolver,
        args.verbose,
        &mut index_cache,
    )?;

    if updates.is_empty() {
        println!("All packages are up to date.");
        return Ok(());
    }

    println!("Updating {} package(s)...", updates.len());

    let mut failures: Vec<String> = Vec::new();

    for update in &updates {
        let current = match lockfile.packages.get(update.package_id.as_str()) {
            Some(entry) => entry,
            None => continue,
        };

        let plan = match &cached_nu_paths {
            Some(paths) => match plan_active_update(current, &update.package_id, paths) {
                Ok(plan) => plan,
                Err(e) => {
                    if args.package.is_some() {
                        return Err(e);
                    }
                    eprintln!("  {e}");
                    failures.push(update.package_id.clone());
                    continue;
                }
            },
            None => {
                // Fail closed: without cached NuPaths we cannot verify activation
                // identity, so refuse active plugins/modules instead of Plain.
                if current.module_activation.is_some() {
                    let e = anyhow::anyhow!(
                        "Package '{}' is currently active as a module. \
                         Run `numan deactivate {}` first, then run update again.",
                        update.package_id,
                        update.package_id
                    );
                    if args.package.is_some() {
                        return Err(e);
                    }
                    eprintln!("  {e}");
                    failures.push(update.package_id.clone());
                    continue;
                }
                if current.activation.is_some() {
                    let e = anyhow::anyhow!(
                        "Package '{}' has a plugin activation record, but Nu paths \
                         are not cached (cannot verify identity). {}\n{}",
                        update.package_id,
                        hints::run(hints::CMD_INIT_REFRESH),
                        hints::active_plugin_update_disabled(&update.package_id)
                    );
                    if args.package.is_some() {
                        return Err(e);
                    }
                    eprintln!("  {e}");
                    failures.push(update.package_id.clone());
                    continue;
                }
                ActiveUpdatePlan::Plain
            }
        };

        let needs_reactivate = plan == ActiveUpdatePlan::OrchestrateActivePlugin;
        let journal = PendingLifecycle {
            op: LifecycleOp::Update,
            package_id: update.package_id.clone(),
            stage: LifecycleStage::Prepared,
            orphan_payload_path: Some(update.orphan_path.clone()),
            from_version: Some(update.from_version.clone()),
            to_version: Some(update.to_version.clone()),
            nupm_source_path: None,
            nupm_metadata_sha256: None,
            staging_dir: None,
            promoted_payload_path: None,
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            target_snapshot_id: None,
            pre_rollback_snapshot_id: None,
            needs_reactivate,
        };
        journal.save(root)?;

        let mut deactivated = false;
        if plan == ActiveUpdatePlan::OrchestrateActivePlugin {
            create_snapshot(
                root,
                SnapshotReason::PreMutation,
                SnapshotTrigger::Update,
                None,
                None,
            )?;
            if args.verbose {
                println!(
                    "  Deactivating {} before upgrade (active-plugin update)",
                    update.package_id
                );
            }
            if let Err(e) = hooks.lifecycle.deactivate(root, &update.package_id) {
                eprintln!("Failed to deactivate {}: {}", update.package_id, e);
                // Leave PendingLifecycle at Prepared + deactivate journal for recovery.
                failures.push(update.package_id.clone());
                // Do not continue the batch: a leftover deactivate/activate journal
                // can poison the next orchestrated update (Codex batch-abort).
                break;
            }
            deactivated = true;
        }

        // Same Nu identity as discovery (`resolve_nu_version`) for Plain and orchestrated.
        let options = InstallOptions {
            root,
            platform: &platform,
            nu_version: &resolve_nu_version,
            force: false,
            verbose: args.verbose,
            registry_name: Some(&update.registry_name),
            snapshot_trigger: SnapshotTrigger::Update,
        };

        let install_result = match hooks.install {
            Some(install) => install(&update.package_id, Some(&update.to_version), &options),
            None => {
                transaction::install_package(&update.package_id, Some(&update.to_version), &options)
            }
        };

        match install_result {
            Ok(_) => {
                PendingLifecycle {
                    stage: LifecycleStage::LockfileUpdated,
                    ..journal.clone()
                }
                .save(root)?;

                if plan == ActiveUpdatePlan::OrchestrateActivePlugin {
                    if args.verbose {
                        println!(
                            "  Reactivating {} after upgrade (active-plugin update)",
                            update.package_id
                        );
                    }
                    if let Err(e) = hooks.lifecycle.activate(root, &update.package_id) {
                        eprintln!(
                            "Failed to reactivate {} after upgrade: {}",
                            update.package_id, e
                        );
                        eprintln!(
                            "Package was upgraded but is inactive. Run `numan activate {}`.",
                            update.package_id
                        );
                        // Leave LockfileUpdated journal + activate journal for recovery.
                        failures.push(update.package_id.clone());
                        // Abort remaining updates so a Failed activate journal cannot
                        // let the next plugin deactivate/upgrade then fail reactivate.
                        break;
                    }
                }

                PendingLifecycle::clear(root)?;
                println!(
                    "{} {}  {} → {}",
                    console::style("✓").green(),
                    update.package_id,
                    update.from_version,
                    update.to_version
                );
            }
            Err(e) => {
                eprintln!("Failed to update {}: {}", update.package_id, e);
                if deactivated {
                    eprintln!(
                        "Attempting to restore previous activation for {}...",
                        update.package_id
                    );
                    if let Err(restore_err) = hooks.lifecycle.activate(root, &update.package_id) {
                        eprintln!(
                            "Failed to restore activation for {}: {}",
                            update.package_id, restore_err
                        );
                        eprintln!(
                            "Package may be inactive. Run `numan activate {}` or `numan gc`.",
                            update.package_id
                        );
                        // Leave journals for recovery.
                    } else {
                        eprintln!(
                            "Restored previous activation for {}. Run `numan gc` to clean up.",
                            update.package_id
                        );
                    }
                } else {
                    eprintln!("Run `numan gc` to clean up orphaned packages.");
                }
                failures.push(update.package_id.clone());
                if deactivated {
                    // Journals may remain; stop the batch rather than risk the next
                    // orchestrated update against a half-finished singleton journal.
                    break;
                }
            }
        }
    }

    if !failures.is_empty() {
        bail!(
            "Failed to update {} package(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }

    Ok(())
}

fn warn_stale_lifecycle_journal(root: &Path) -> Result<()> {
    let Some(journal) = check_stale_journal(root)? else {
        return Ok(());
    };

    if journal.needs_reactivate && journal.stage == LifecycleStage::LockfileUpdated {
        eprintln!(
            "Warning: A previous update of '{}' finished installing but did not \
             reactivate the plugin.",
            journal.package_id
        );
        eprintln!(
            "Run `numan update` to resume reactivation, or `numan activate {}`.",
            journal.package_id
        );
        return Ok(());
    }

    let op = match journal.op {
        LifecycleOp::Update => "update",
        LifecycleOp::Remove => "remove",
        LifecycleOp::NupmImport => "nupm import",
        LifecycleOp::NupmImportManifest => "nupm manifest import",
        LifecycleOp::Rollback => "rollback",
    };
    eprintln!(
        "Warning: A previous '{}' operation on '{}' was interrupted.",
        op, journal.package_id
    );
    eprintln!("Run `numan gc` to clean up any orphaned packages.");
    Ok(())
}

/// Resume activation after an interrupted orchestrated update
/// (`LockfileUpdated` + `needs_reactivate`).
///
/// Without this, discovery treats the package as up to date and never
/// reactivates it (Codex: resume LockfileUpdated journals).
fn resume_interrupted_reactivate(root: &Path, hooks: &UpdateHooks<'_>) -> Result<()> {
    let Some(journal) = PendingLifecycle::load(root)? else {
        return Ok(());
    };
    if !matches!(journal.op, LifecycleOp::Update)
        || journal.stage != LifecycleStage::LockfileUpdated
        || !journal.needs_reactivate
    {
        return Ok(());
    }

    let lockfile = Lockfile::load(root)?;
    let Some(entry) = lockfile.packages.get(&journal.package_id) else {
        // Package removed after the interrupt; leave journal for gc warning.
        return Ok(());
    };
    if entry.package_type != "plugin" {
        return Ok(());
    }

    if entry.activation.is_some() {
        // User (or doctor) already reactivated; clear the lifecycle journal.
        PendingLifecycle::clear(root)?;
        return Ok(());
    }

    println!(
        "Resuming reactivation of {} after interrupted update...",
        journal.package_id
    );
    hooks
        .lifecycle
        .activate(root, &journal.package_id)
        .with_context(|| {
            format!(
                "Failed to resume reactivation of '{}'. {}",
                journal.package_id,
                hints::run(&format!("numan activate {}", journal.package_id))
            )
        })?;
    PendingLifecycle::clear(root)?;
    println!(
        "{} {} reactivated",
        console::style("✓").green(),
        journal.package_id
    );
    Ok(())
}

fn packages_to_check(args: &UpdateArgs, lockfile: &Lockfile) -> Result<Vec<String>> {
    if let Some(ref id) = args.package {
        if !lockfile.packages.contains_key(id.as_str()) {
            bail!("Package '{}' is not installed.", id);
        }
        Ok(vec![id.clone()])
    } else {
        Ok(lockfile.packages.keys().cloned().collect())
    }
}

fn discover_pending_updates(
    lockfile: &Lockfile,
    packages_to_check: &[String],
    default_reg: &str,
    registry: &RegistryManager,
    resolver: &Resolver,
    verbose: bool,
    index_cache: &mut HashMap<String, RegistryIndex>,
) -> Result<Vec<PendingUpdate>> {
    let mut updates = Vec::new();

    for pkg_id in packages_to_check {
        let current = match lockfile.packages.get(pkg_id.as_str()) {
            Some(e) => e,
            None => continue,
        };

        let registry_name = registry_name_for_entry(current, default_reg);
        let index = match index_cache.get(&registry_name) {
            Some(idx) => idx,
            None => {
                let loaded = registry.load_verified(&registry_name)?;
                index_cache.insert(registry_name.clone(), loaded.index);
                index_cache.get(&registry_name).expect("just inserted")
            }
        };

        let pkg = match index.packages.iter().find(|p| p.id.to_string() == *pkg_id) {
            Some(p) => p,
            None => {
                if verbose {
                    println!(
                        "  {} not found in registry '{}' (skipping)",
                        pkg_id, registry_name
                    );
                }
                continue;
            }
        };

        let resolved = match resolver.resolve(pkg) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let latest = resolved.version.to_string();
        if is_upgrade_available(&current.version, &resolved.version) {
            updates.push(PendingUpdate {
                package_id: pkg_id.clone(),
                from_version: current.version.clone(),
                to_version: latest,
                orphan_path: current.payload_path().to_string(),
                registry_name,
            });
        }
    }

    Ok(updates)
}

/// Parse `registry:official` from the lockfile entry's recorded origin.
fn registry_name_for_entry(entry: &LockfileEntry, default: &str) -> String {
    entry
        .origin
        .as_deref()
        .or(entry.registry_url.as_deref())
        .and_then(|value| value.strip_prefix("registry:"))
        .unwrap_or(default)
        .to_string()
}

/// Returns true when `resolved` is strictly newer than the installed version.
fn is_upgrade_available(current: &str, resolved: &semver::Version) -> bool {
    match current.parse::<semver::Version>() {
        Ok(current_ver) => resolved > &current_ver,
        Err(_) => false,
    }
}

/// Nu version for both candidate discovery and install.
///
/// Prefer the cached `NuPaths` version when present so Plain updates cannot
/// discover against cache and then fail (or pick a different artifact) against PATH.
fn resolve_nu_version_for_update(
    cached_nu_paths: Option<&NuPaths>,
    path_nu_version: &NuVersion,
) -> NuVersion {
    cached_nu_paths
        .and_then(|paths| NuVersion::parse(&paths.nu_version).ok())
        .unwrap_or_else(|| path_nu_version.clone())
}

/// Decide whether an active plugin/module may be updated.
///
/// - Active **module**: always refuse (deactivate first).
/// - Active **plugin** matching current Nu identity with mutation enabled: orchestrate.
/// - Active **plugin** matching identity without exact opt-in: refuse.
/// - Active **plugin** with stale/mismatched Nu identity: refuse (preserve activation record).
fn plan_active_update(
    entry: &LockfileEntry,
    pkg_id: &str,
    nu_paths: &NuPaths,
) -> Result<ActiveUpdatePlan> {
    if entry.module_activation.is_some() {
        bail!(
            "Package '{}' is currently active as a module. \
             Run `numan deactivate {}` first, then run update again.",
            pkg_id,
            pkg_id
        );
    }
    if entry.activation.is_some() {
        let matching = entry.is_active_for(
            &nu_paths.nu_executable_hash,
            &nu_paths.nu_version,
            &nu_paths.plugin_registry_path,
        );
        if matching {
            if !active_plugin_mutation::is_enabled() {
                bail!("{}", hints::active_plugin_update_disabled(pkg_id));
            }
            return Ok(ActiveUpdatePlan::OrchestrateActivePlugin);
        }
        // Fail closed: keep the activation record until Nu identity is aligned
        // and the plugin is deactivated against the registry that owns it.
        bail!(
            "Package '{}' has a plugin activation record that does not match \
             the current Nu identity. {}\nThen run `numan deactivate {}` before updating.",
            pkg_id,
            hints::run(hints::CMD_INIT_REFRESH),
            pkg_id
        );
    }
    Ok(ActiveUpdatePlan::Plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::plugin_lifecycle::{activate_one_plugin, deactivate_one_plugin};
    use crate::core::integrity;
    use crate::core::package::ModuleImportMode;
    use crate::state::active_plugin_mutation::EnvOverrideGuard;
    use crate::state::lifecycle_journal::PendingLifecycle;
    use crate::state::lockfile::{Lockfile, LockfileEntry, ModuleActivation, PluginActivation};
    use crate::state::plugin_deactivate_journal::PendingPluginDeactivate;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    struct SuccessfulPluginLifecycle;

    impl PluginLifecycle for SuccessfulPluginLifecycle {
        fn deactivate(&self, root: &Path, pkg_id: &str) -> Result<()> {
            deactivate_one_plugin(root, pkg_id, &|_nu, _name, _cfg| Ok(()))
        }

        fn activate(&self, root: &Path, pkg_id: &str) -> Result<()> {
            activate_one_plugin(root, pkg_id, &|_nu, _bin, _cfg| Ok(()))
        }
    }

    fn base_entry() -> LockfileEntry {
        LockfileEntry {
            version: "1.0.0".to_string(),
            package_type: "plugin".to_string(),
            source: "binary".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: None,
            installed_at: "0".to_string(),
            nu_version_at_install: None,
            activation: None,
            registry_url: None,
            registry_revision: None,
            index_sha256: None,
            signing_key_fingerprint: None,
            git_url: None,
            git_rev: None,
            cargo_name: None,
            cargo_lock_sha256: None,
            built_sha256: None,
            payload_path: String::new(),
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

    fn plugin_activation() -> PluginActivation {
        PluginActivation {
            plugin_registry_path: "/tmp/plugins.nu".to_string(),
            nu_executable_sha256: "abc".to_string(),
            nu_version: "0.95.0".to_string(),
            activated_at: "0".to_string(),
        }
    }

    fn matching_nu_paths() -> NuPaths {
        NuPaths {
            nu_executable: "/tmp/nu".to_string(),
            nu_version: "0.95.0".to_string(),
            plugin_registry_path: "/tmp/plugins.nu".to_string(),
            nu_executable_hash: "abc".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![],
            vendor_autoload_dir: None,
        }
    }

    #[test]
    fn is_upgrade_available_requires_strictly_newer_semver() {
        let v2 = semver::Version::new(2, 0, 0);
        assert!(is_upgrade_available("1.0.0", &v2));
        assert!(!is_upgrade_available("2.0.0", &v2));
        assert!(!is_upgrade_available("3.0.0", &v2));
        assert!(!is_upgrade_available("not-a-version", &v2));
    }

    #[test]
    fn registry_name_for_entry_prefers_origin() {
        let entry = LockfileEntry {
            origin: Some("registry:community".to_string()),
            registry_url: Some("registry:official".to_string()),
            ..base_entry()
        };
        assert_eq!(registry_name_for_entry(&entry, "official"), "community");
    }

    #[test]
    fn registry_name_for_entry_falls_back_to_default() {
        let entry = base_entry();
        assert_eq!(registry_name_for_entry(&entry, "official"), "official");
    }

    #[test]
    fn resolve_nu_version_for_update_prefers_cached_over_path() {
        let path = NuVersion {
            version: "0.90.0".to_string(),
            major: 0,
            minor: 90,
            patch: 0,
        };
        let cached = matching_nu_paths();
        let resolved = resolve_nu_version_for_update(Some(&cached), &path);
        assert_eq!(resolved.version, "0.95.0");
        assert_eq!(resolved.minor, 95);

        let fallback = resolve_nu_version_for_update(None, &path);
        assert_eq!(fallback.version, "0.90.0");
        assert_eq!(fallback.minor, 90);
    }

    #[test]
    fn plan_active_update_orchestrates_when_exact_opt_in_is_set() {
        let guard = EnvOverrideGuard::acquire();
        guard.set("1");
        let entry = LockfileEntry {
            activation: Some(plugin_activation()),
            ..base_entry()
        };
        assert_eq!(
            plan_active_update(&entry, "owner/pkg", &matching_nu_paths()).unwrap(),
            ActiveUpdatePlan::OrchestrateActivePlugin
        );
    }

    #[test]
    fn plan_active_update_refuses_when_opt_in_is_unset() {
        let guard = EnvOverrideGuard::acquire();
        guard.clear();
        let entry = LockfileEntry {
            activation: Some(plugin_activation()),
            ..base_entry()
        };
        let err = plan_active_update(&entry, "owner/pkg", &matching_nu_paths()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("owner/pkg"));
        assert!(msg.contains("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION"));
        assert!(
            !msg.contains("numan remove"),
            "update gate must not suggest remove"
        );
    }

    #[test]
    fn plan_active_update_refuses_when_activation_stale() {
        let guard = EnvOverrideGuard::acquire();
        guard.set("1");
        let entry = LockfileEntry {
            activation: Some(plugin_activation()),
            ..base_entry()
        };
        let mut stale_paths = matching_nu_paths();
        stale_paths.nu_executable_hash = "different".to_string();
        let err = plan_active_update(&entry, "owner/pkg", &stale_paths).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does not match"));
        assert!(msg.contains("init --refresh") || msg.contains("deactivate"));
    }

    #[test]
    fn plan_active_update_rejects_module_activation() {
        let entry = LockfileEntry {
            module_activation: Some(ModuleActivation {
                entry_path: "/tmp/mod.nu".to_string(),
                import_mode: ModuleImportMode::Module,
                vendor_autoload_dir: "/tmp/vendor".to_string(),
                managed_file_path: "/tmp/vendor/numan.nu".to_string(),
                nu_executable_sha256: "abc".to_string(),
                nu_version: "0.95.0".to_string(),
                activated_at: "0".to_string(),
            }),
            ..base_entry()
        };
        let err = plan_active_update(&entry, "owner/mod", &matching_nu_paths()).unwrap_err();
        assert!(err.to_string().contains("active as a module"));
    }

    struct ActiveUpdateEnv {
        _dir: TempDir,
        root: PathBuf,
        pkg_id: String,
    }

    impl ActiveUpdateEnv {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let root = dir.path().to_path_buf();
            std::fs::create_dir_all(root.join("state")).unwrap();
            std::fs::create_dir_all(root.join("packages")).unwrap();

            let nu_exe = root.join("fake_nu");
            std::fs::write(&nu_exe, b"fake nu binary").unwrap();
            let nu_hash = integrity::compute_sha256(b"fake nu binary");
            let registry = root.join("plugin-registry.msgpack.z");
            std::fs::write(&registry, b"reg").unwrap();

            let paths = NuPaths {
                nu_executable: nu_exe.to_string_lossy().into_owned(),
                nu_version: "0.95.0".to_string(),
                plugin_registry_path: registry.to_string_lossy().into_owned(),
                nu_executable_hash: nu_hash.clone(),
                platform: "x86_64-pc-windows-msvc".to_string(),
                data_dir: None,
                vendor_autoload_dirs: vec![],
                vendor_autoload_dir: None,
            };
            paths.save(&root).unwrap();

            let pkg_id = "owner/highlight".to_string();
            let payload = "packages/plugins/owner/highlight/1.0.0-abc";
            let payload_dir = root.join(payload);
            std::fs::create_dir_all(&payload_dir).unwrap();
            std::fs::write(payload_dir.join("nu_plugin_highlight"), b"fake").unwrap();

            let mut lockfile = Lockfile::empty();
            lockfile.packages.insert(
                pkg_id.clone(),
                LockfileEntry {
                    version: "1.0.0".to_string(),
                    package_type: "plugin".to_string(),
                    source: "binary".to_string(),
                    target: None,
                    artifact_url: None,
                    artifact_sha256: None,
                    executable_path: Some("nu_plugin_highlight".to_string()),
                    archive_root: None,
                    include: None,
                    entry: None,
                    installed_at: "0".to_string(),
                    nu_version_at_install: None,
                    activation: Some(PluginActivation {
                        plugin_registry_path: paths.plugin_registry_path.clone(),
                        nu_executable_sha256: nu_hash,
                        nu_version: paths.nu_version.clone(),
                        activated_at: "0".to_string(),
                    }),
                    registry_url: None,
                    registry_revision: None,
                    index_sha256: None,
                    signing_key_fingerprint: None,
                    git_url: None,
                    git_rev: None,
                    cargo_name: None,
                    cargo_lock_sha256: None,
                    built_sha256: None,
                    payload_path: payload.to_string(),
                    revision_id: None,
                    payload_sha256: None,
                    executable_sha256: None,
                    selection_reason: None,
                    origin: Some("registry:official".to_string()),
                    module_activation: None,
                    module_import_mode: None,
                    locked_dependencies: BTreeMap::new(),
                },
            );
            lockfile.save(&root).unwrap();

            Self {
                _dir: dir,
                root,
                pkg_id,
            }
        }

        fn root(&self) -> &PathBuf {
            &self.root
        }
    }

    /// Bypass registry discovery: one fabricated pending update, then hooks.
    fn run_orchestrated_update_with_hooks(
        env: &ActiveUpdateEnv,
        unregistrar: &dyn Fn(&str, &str, &str) -> Result<()>,
        registrar: &dyn Fn(&str, &str, &str) -> Result<()>,
        install: &dyn Fn(&str, Option<&str>, &InstallOptions<'_>) -> Result<InstallResult>,
    ) -> Result<()> {
        let _lock = acquire_mutation_lock(env.root())?;
        let lockfile = Lockfile::load(env.root())?;
        let entry = lockfile.packages.get(&env.pkg_id).unwrap();
        let nu_paths = NuPaths::load(env.root())?;
        let plan = plan_active_update(entry, &env.pkg_id, &nu_paths)?;
        assert_eq!(plan, ActiveUpdatePlan::OrchestrateActivePlugin);

        let journal = PendingLifecycle {
            op: LifecycleOp::Update,
            package_id: env.pkg_id.clone(),
            stage: LifecycleStage::Prepared,
            orphan_payload_path: Some(entry.payload_path.clone()),
            from_version: Some("1.0.0".to_string()),
            to_version: Some("2.0.0".to_string()),
            nupm_source_path: None,
            nupm_metadata_sha256: None,
            staging_dir: None,
            promoted_payload_path: None,
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            target_snapshot_id: None,
            pre_rollback_snapshot_id: None,
            needs_reactivate: true,
        };
        journal.save(env.root())?;

        create_snapshot(
            env.root(),
            SnapshotReason::PreMutation,
            SnapshotTrigger::Update,
            None,
            None,
        )?;
        deactivate_one_plugin(env.root(), &env.pkg_id, unregistrar)?;

        let platform = Platform::detect();
        let nu_version = NuVersion::parse(&nu_paths.nu_version)?;
        let options = InstallOptions {
            root: env.root(),
            platform: &platform,
            nu_version: &nu_version,
            force: false,
            verbose: false,
            registry_name: Some("official"),
            snapshot_trigger: SnapshotTrigger::Update,
        };
        install(&env.pkg_id, Some("2.0.0"), &options)?;

        PendingLifecycle {
            stage: LifecycleStage::LockfileUpdated,
            ..journal.clone()
        }
        .save(env.root())?;

        activate_one_plugin(env.root(), &env.pkg_id, registrar)?;
        PendingLifecycle::clear(env.root())?;
        Ok(())
    }

    #[test]
    fn active_plugin_update_succeeds_with_fake_hooks() {
        let guard = EnvOverrideGuard::acquire();
        guard.set("1");

        let env = ActiveUpdateEnv::new();
        let pkg_id = env.pkg_id.clone();
        let root = env.root().clone();

        let install = move |id: &str, ver: Option<&str>, opts: &InstallOptions<'_>| {
            assert_eq!(id, pkg_id);
            assert_eq!(ver, Some("2.0.0"));
            let mut lockfile = Lockfile::load(opts.root)?;
            let entry = lockfile.packages.get_mut(id).unwrap();
            assert!(
                entry.activation.is_none(),
                "deactivate must clear activation"
            );
            let new_payload = "packages/plugins/owner/highlight/2.0.0-def";
            let new_dir = opts.root.join(new_payload);
            std::fs::create_dir_all(&new_dir)?;
            std::fs::write(new_dir.join("nu_plugin_highlight"), b"fake v2")?;
            entry.version = "2.0.0".to_string();
            entry.payload_path = new_payload.to_string();
            lockfile.save(opts.root)?;
            Ok(InstallResult {
                installed: true,
                package: id.to_string(),
                version: "2.0.0".to_string(),
                path: new_dir,
                already_existed: false,
            })
        };

        run_orchestrated_update_with_hooks(
            &env,
            &|_nu, identity, _cfg| {
                let normalized = identity.replace('\\', "/");
                assert!(
                    normalized.ends_with("nu_plugin_highlight"),
                    "expected absolute plugin path, got {identity}"
                );
                Ok(())
            },
            &|_nu, bin, _cfg| {
                assert!(bin.contains("nu_plugin_highlight"));
                Ok(())
            },
            &install,
        )
        .unwrap();

        let lockfile = Lockfile::load(&root).unwrap();
        let entry = lockfile.packages.get(&env.pkg_id).unwrap();
        assert_eq!(entry.version, "2.0.0");
        assert!(entry.activation.is_some());
        assert!(PendingLifecycle::load(&root).unwrap().is_none());
    }

    #[test]
    fn install_failure_after_deactivate_attempts_restore() {
        let guard = EnvOverrideGuard::acquire();
        guard.set("1");

        let env = ActiveUpdateEnv::new();
        let _lock = acquire_mutation_lock(env.root()).unwrap();
        let lockfile = Lockfile::load(env.root()).unwrap();
        let entry = lockfile.packages.get(&env.pkg_id).unwrap();
        let nu_paths = NuPaths::load(env.root()).unwrap();
        assert_eq!(
            plan_active_update(entry, &env.pkg_id, &nu_paths).unwrap(),
            ActiveUpdatePlan::OrchestrateActivePlugin
        );

        let journal = PendingLifecycle {
            op: LifecycleOp::Update,
            package_id: env.pkg_id.clone(),
            stage: LifecycleStage::Prepared,
            orphan_payload_path: Some(entry.payload_path.clone()),
            from_version: Some("1.0.0".to_string()),
            to_version: Some("2.0.0".to_string()),
            nupm_source_path: None,
            nupm_metadata_sha256: None,
            staging_dir: None,
            promoted_payload_path: None,
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            target_snapshot_id: None,
            pre_rollback_snapshot_id: None,
            needs_reactivate: true,
        };
        journal.save(env.root()).unwrap();
        create_snapshot(
            env.root(),
            SnapshotReason::PreMutation,
            SnapshotTrigger::Update,
            None,
            None,
        )
        .unwrap();

        deactivate_one_plugin(env.root(), &env.pkg_id, &|_nu, _id, _cfg| Ok(())).unwrap();
        assert!(Lockfile::load(env.root())
            .unwrap()
            .packages
            .get(&env.pkg_id)
            .unwrap()
            .activation
            .is_none());

        // Simulate install failure after successful deactivate: restore prior activation.
        activate_one_plugin(env.root(), &env.pkg_id, &|_nu, _bin, _cfg| Ok(())).unwrap();
        assert!(Lockfile::load(env.root())
            .unwrap()
            .packages
            .get(&env.pkg_id)
            .unwrap()
            .activation
            .is_some());
        assert!(PendingPluginDeactivate::load(env.root()).unwrap().is_none());
        // Lifecycle journal remains Prepared until the outer update command clears it.
        assert_eq!(
            PendingLifecycle::load(env.root()).unwrap().unwrap().stage,
            LifecycleStage::Prepared
        );
    }

    #[test]
    fn registrar_failure_after_install_leaves_recovery_journals() {
        use crate::state::journal::PendingActivation;

        let guard = EnvOverrideGuard::acquire();
        guard.set("1");

        let env = ActiveUpdateEnv::new();
        let pkg_id = env.pkg_id.clone();

        let install = move |id: &str,
                            ver: Option<&str>,
                            opts: &InstallOptions<'_>|
              -> Result<InstallResult> {
            assert_eq!(id, pkg_id);
            assert_eq!(ver, Some("2.0.0"));
            let mut lockfile = Lockfile::load(opts.root)?;
            let entry = lockfile.packages.get_mut(id).unwrap();
            assert!(entry.activation.is_none());
            let new_payload = "packages/plugins/owner/highlight/2.0.0-def";
            let new_dir = opts.root.join(new_payload);
            std::fs::create_dir_all(&new_dir)?;
            std::fs::write(new_dir.join("nu_plugin_highlight"), b"fake v2")?;
            entry.version = "2.0.0".to_string();
            entry.payload_path = new_payload.to_string();
            lockfile.save(opts.root)?;
            Ok(InstallResult {
                installed: true,
                package: id.to_string(),
                version: "2.0.0".to_string(),
                path: new_dir,
                already_existed: false,
            })
        };

        let _lock = acquire_mutation_lock(env.root()).unwrap();
        let lockfile = Lockfile::load(env.root()).unwrap();
        let entry = lockfile.packages.get(&env.pkg_id).unwrap();
        let nu_paths = NuPaths::load(env.root()).unwrap();
        assert_eq!(
            plan_active_update(entry, &env.pkg_id, &nu_paths).unwrap(),
            ActiveUpdatePlan::OrchestrateActivePlugin
        );

        let journal = PendingLifecycle {
            op: LifecycleOp::Update,
            package_id: env.pkg_id.clone(),
            stage: LifecycleStage::Prepared,
            orphan_payload_path: Some(entry.payload_path.clone()),
            from_version: Some("1.0.0".to_string()),
            to_version: Some("2.0.0".to_string()),
            nupm_source_path: None,
            nupm_metadata_sha256: None,
            staging_dir: None,
            promoted_payload_path: None,
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            target_snapshot_id: None,
            pre_rollback_snapshot_id: None,
            needs_reactivate: true,
        };
        journal.save(env.root()).unwrap();
        create_snapshot(
            env.root(),
            SnapshotReason::PreMutation,
            SnapshotTrigger::Update,
            None,
            None,
        )
        .unwrap();
        deactivate_one_plugin(env.root(), &env.pkg_id, &|_nu, _id, _cfg| Ok(())).unwrap();

        let platform = Platform::detect();
        let nu_version = NuVersion::parse(&nu_paths.nu_version).unwrap();
        let options = InstallOptions {
            root: env.root(),
            platform: &platform,
            nu_version: &nu_version,
            force: false,
            verbose: false,
            registry_name: Some("official"),
            snapshot_trigger: SnapshotTrigger::Update,
        };
        install(&env.pkg_id, Some("2.0.0"), &options).unwrap();
        PendingLifecycle {
            stage: LifecycleStage::LockfileUpdated,
            ..journal.clone()
        }
        .save(env.root())
        .unwrap();

        let err = activate_one_plugin(env.root(), &env.pkg_id, &|_nu, _bin, _cfg| {
            bail!("injected reactivate failure")
        })
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("injected reactivate failure"),
            "unexpected error: {msg}"
        );

        let lifecycle = PendingLifecycle::load(env.root()).unwrap().unwrap();
        assert_eq!(lifecycle.stage, LifecycleStage::LockfileUpdated);
        let lockfile = Lockfile::load(env.root()).unwrap();
        assert_eq!(lockfile.packages.get(&env.pkg_id).unwrap().version, "2.0.0");
        assert!(lockfile
            .packages
            .get(&env.pkg_id)
            .unwrap()
            .activation
            .is_none());
        let pending_act = PendingActivation::load(env.root()).unwrap().unwrap();
        assert_eq!(pending_act.entries.len(), 1);
        assert!(pending_act.entries[0].error.is_some());
    }

    #[test]
    fn active_plugin_update_refuses_when_opt_in_is_unset() {
        let guard = EnvOverrideGuard::acquire();
        guard.clear();

        let env = ActiveUpdateEnv::new();
        let lockfile = Lockfile::load(env.root()).unwrap();
        let entry = lockfile.packages.get(&env.pkg_id).unwrap();
        let nu_paths = NuPaths::load(env.root()).unwrap();
        let err = plan_active_update(entry, &env.pkg_id, &nu_paths).unwrap_err();
        assert!(err
            .to_string()
            .contains("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION"));
    }

    #[test]
    fn unregistrar_failure_leaves_activation_and_journals() {
        let guard = EnvOverrideGuard::acquire();
        guard.set("1");

        let env = ActiveUpdateEnv::new();
        let _lock = acquire_mutation_lock(env.root()).unwrap();
        let lockfile = Lockfile::load(env.root()).unwrap();
        let entry = lockfile.packages.get(&env.pkg_id).unwrap();

        let journal = PendingLifecycle {
            op: LifecycleOp::Update,
            package_id: env.pkg_id.clone(),
            stage: LifecycleStage::Prepared,
            orphan_payload_path: Some(entry.payload_path.clone()),
            from_version: Some("1.0.0".to_string()),
            to_version: Some("2.0.0".to_string()),
            nupm_source_path: None,
            nupm_metadata_sha256: None,
            staging_dir: None,
            promoted_payload_path: None,
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            target_snapshot_id: None,
            pre_rollback_snapshot_id: None,
            needs_reactivate: true,
        };
        journal.save(env.root()).unwrap();

        let err = deactivate_one_plugin(env.root(), &env.pkg_id, &|_nu, _name, _cfg| {
            bail!("injected unregister failure")
        })
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("injected unregister failure"),
            "unexpected error: {msg}"
        );

        let lockfile = Lockfile::load(env.root()).unwrap();
        assert!(
            lockfile
                .packages
                .get(&env.pkg_id)
                .unwrap()
                .activation
                .is_some(),
            "activation must remain when unregister fails"
        );
        assert!(PendingLifecycle::load(env.root()).unwrap().is_some());
        assert!(PendingPluginDeactivate::load(env.root()).unwrap().is_some());
    }

    #[test]
    fn resume_lockfile_updated_reactivates_inactive_plugin() {
        use crate::state::journal::{PendingActivation, PendingActivationEntry, PendingStatus};

        let guard = EnvOverrideGuard::acquire();
        // Resume does not require the mutation env; finishing an interrupted
        // orchestrated update must not leave the plugin gated forever.
        guard.clear();

        let env = ActiveUpdateEnv::new();
        let pkg_id = env.pkg_id.clone();

        // Simulate interrupt after install: already at to_version, activation cleared.
        let mut lockfile = Lockfile::load(env.root()).unwrap();
        let entry = lockfile.packages.get_mut(&pkg_id).unwrap();
        let new_payload = "packages/plugins/owner/highlight/2.0.0-def";
        let new_dir = env.root().join(new_payload);
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(new_dir.join("nu_plugin_highlight"), b"fake v2").unwrap();
        entry.version = "2.0.0".to_string();
        entry.payload_path = new_payload.to_string();
        entry.activation = None;
        lockfile.save(env.root()).unwrap();

        let paths = NuPaths::load(env.root()).unwrap();
        PendingLifecycle {
            op: LifecycleOp::Update,
            package_id: pkg_id.clone(),
            stage: LifecycleStage::LockfileUpdated,
            orphan_payload_path: Some("packages/plugins/owner/highlight/1.0.0-abc".to_string()),
            from_version: Some("1.0.0".to_string()),
            to_version: Some("2.0.0".to_string()),
            nupm_source_path: None,
            nupm_metadata_sha256: None,
            staging_dir: None,
            promoted_payload_path: None,
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            target_snapshot_id: None,
            pre_rollback_snapshot_id: None,
            needs_reactivate: true,
        }
        .save(env.root())
        .unwrap();

        // Failed activate journal left by the interrupted reactivate attempt.
        PendingActivation {
            nu_executable_sha256: paths.nu_executable_hash.clone(),
            nu_version: paths.nu_version.clone(),
            plugin_registry_path: paths.plugin_registry_path.clone(),
            created_at: "0".to_string(),
            entries: vec![PendingActivationEntry {
                package_id: pkg_id.clone(),
                payload_path: new_payload.to_string(),
                executable_path: "nu_plugin_highlight".to_string(),
                absolute_binary_path: new_dir
                    .join("nu_plugin_highlight")
                    .to_string_lossy()
                    .into_owned(),
                status: PendingStatus::Failed,
                error: Some("injected".to_string()),
            }],
        }
        .save(env.root())
        .unwrap();

        let hooks = UpdateHooks {
            lifecycle: &SuccessfulPluginLifecycle,
            install: None,
        };
        resume_interrupted_reactivate(env.root(), &hooks).unwrap();

        let lockfile = Lockfile::load(env.root()).unwrap();
        assert!(
            lockfile.packages[&pkg_id].activation.is_some(),
            "resume must reactivate after LockfileUpdated interrupt"
        );
        assert!(PendingLifecycle::load(env.root()).unwrap().is_none());
        assert!(PendingActivation::load(env.root()).unwrap().is_none());
    }

    #[test]
    fn resume_skips_plain_lockfile_updated_without_needs_reactivate() {
        let env = ActiveUpdateEnv::new();
        let pkg_id = env.pkg_id.clone();

        let mut lockfile = Lockfile::load(env.root()).unwrap();
        lockfile.packages.get_mut(&pkg_id).unwrap().activation = None;
        lockfile.save(env.root()).unwrap();

        PendingLifecycle {
            op: LifecycleOp::Update,
            package_id: pkg_id.clone(),
            stage: LifecycleStage::LockfileUpdated,
            orphan_payload_path: Some("packages/plugins/owner/highlight/1.0.0-abc".to_string()),
            from_version: Some("1.0.0".to_string()),
            to_version: Some("2.0.0".to_string()),
            nupm_source_path: None,
            nupm_metadata_sha256: None,
            staging_dir: None,
            promoted_payload_path: None,
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            target_snapshot_id: None,
            pre_rollback_snapshot_id: None,
            needs_reactivate: false,
        }
        .save(env.root())
        .unwrap();

        let hooks = UpdateHooks {
            lifecycle: &SuccessfulPluginLifecycle,
            install: None,
        };
        resume_interrupted_reactivate(env.root(), &hooks).unwrap();

        assert!(
            Lockfile::load(env.root()).unwrap().packages[&pkg_id]
                .activation
                .is_none(),
            "plain update crash must not spuriously activate"
        );
        assert!(PendingLifecycle::load(env.root()).unwrap().is_some());
    }
}
