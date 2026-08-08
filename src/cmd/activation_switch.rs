//! `numan use` activation profile leave / restore (plugins and modules).
//!
//! Calls lower-level lifecycle primitives that do **not** mutate the activation
//! profile. Leave merges are union-only; restore never edits the saved target
//! profile.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::core::nu_version::NuVersion;
use crate::core::platform::Platform;
use crate::core::registry::RegistryManager;
use crate::core::resolve::Resolver;
use crate::nu::autoload::{CandidateRunner, NuCandidateRunner};
use crate::nu::paths::NuPaths;
use crate::nu::version_manager;
use crate::state::activation_profile::{self, ActivationProfile, MinorActivationSet, ProfileKind};
use crate::state::lockfile::Lockfile;
use crate::util::hints::CMD_INIT_REFRESH;

use super::plugin_lifecycle::{activate_one_plugin, deactivate_one_plugin};

/// Summary printed after a switch or same-target reconcile.
#[derive(Debug, Default, Clone)]
pub struct SwitchActivationReport {
    pub left_plugins: Vec<String>,
    pub left_modules: Vec<String>,
    pub restored_plugins: Vec<String>,
    pub restored_modules: Vec<String>,
    pub skipped_missing: Vec<String>,
    pub skipped_incompatible: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl SwitchActivationReport {
    pub fn print_summary(&self) {
        if !self.left_plugins.is_empty() || !self.left_modules.is_empty() {
            println!(
                "Deactivated for Nu switch: {} plugin(s), {} module(s).",
                self.left_plugins.len(),
                self.left_modules.len()
            );
        }
        if !self.restored_plugins.is_empty() || !self.restored_modules.is_empty() {
            println!(
                "Restored for active Nu: {} plugin(s), {} module(s).",
                self.restored_plugins.len(),
                self.restored_modules.len()
            );
        }
        for id in &self.skipped_missing {
            eprintln!("warning: skipped restore of '{id}' (not installed).");
        }
        for id in &self.skipped_incompatible {
            eprintln!(
                "warning: skipped restore of '{id}' (incompatible with current Nu/platform)."
            );
        }
        for (id, err) in &self.failed {
            eprintln!(
                "{} Failed to restore {id}: {err}",
                console::style("✗").red()
            );
        }
    }

    pub fn has_lifecycle_failure(&self) -> bool {
        !self.failed.is_empty()
    }
}

/// Injectable Nu registrar / unregistrar / module runner for tests.
pub struct SwitchHooks<'a> {
    pub registrar: &'a dyn Fn(&str, &str, &str) -> Result<()>,
    pub unregistrar: &'a dyn Fn(&str, &str, &str) -> Result<()>,
    pub runner: Option<&'a dyn CandidateRunner>,
    /// Optional replacement for post-switch `paths.json` refresh (tests).
    pub path_refresh: Option<&'a PathRefreshHook>,
}

/// Post-switch Nu paths refresh callback (tests inject a fake probe).
pub type PathRefreshHook = dyn Fn(&Path) -> Result<()>;

/// Collect packages currently Numan-active for the given `NuPaths`.
pub fn collect_currently_active(lockfile: &Lockfile, nu_paths: &NuPaths) -> MinorActivationSet {
    let vendor_dir = nu_paths.vendor_autoload_dir.as_deref().unwrap_or("");
    let managed_path = if vendor_dir.is_empty() {
        String::new()
    } else {
        format!("{vendor_dir}/numan.nu")
    };

    let mut set = MinorActivationSet::default();
    for (id, entry) in &lockfile.packages {
        match entry.package_type.as_str() {
            "plugin"
                if entry.is_active_for(
                    &nu_paths.nu_executable_hash,
                    &nu_paths.nu_version,
                    &nu_paths.plugin_registry_path,
                ) =>
            {
                set.plugins.push(id.clone());
            }
            "module"
                if entry.is_module_active_for(
                    &nu_paths.nu_executable_hash,
                    &nu_paths.nu_version,
                    vendor_dir,
                    &managed_path,
                ) =>
            {
                set.modules.push(id.clone());
            }
            _ => {}
        }
    }
    set.plugins.sort();
    set.modules.sort();
    set
}

/// Same-target `numan use`: restore/reconcile desired profile only.
pub fn reconcile_target_profile(
    root: &Path,
    target_minor: &str,
    hooks: &SwitchHooks<'_>,
) -> Result<SwitchActivationReport> {
    let mut report = SwitchActivationReport::default();
    let profile = ActivationProfile::load_or_default(root)?;
    let desired = profile.set_for_minor(target_minor);
    if desired.is_empty() {
        return Ok(report);
    }

    let Some(nu_paths) = try_load_paths(root)? else {
        bail!(
            "Cannot reconcile activation profile for Nu {target_minor}: \
             cached Nu paths are missing. Run `numan init --refresh` first."
        );
    };
    nu_paths.validate_drift()?;

    restore_desired(root, &nu_paths, &desired, hooks, &mut report)?;
    Ok(report)
}

/// Cross-minor leave: fail-closed paths gate, union leave profile, teardown
/// modules then plugins. Does not write the active-version marker.
pub fn leave_current_nu(
    root: &Path,
    leaving_minor: &str,
    hooks: &SwitchHooks<'_>,
) -> Result<SwitchActivationReport> {
    let mut report = SwitchActivationReport::default();
    let paths_present = root.join("nu_state").join("paths.json").is_file();
    // A missing lockfile already yields an empty lockfile. Any error here means
    // unreadable or malformed state, which must stop the switch.
    let lockfile = Lockfile::load(root).with_context(|| {
        "Cannot switch Nu version: the lockfile could not be read. \
         Repair or restore it, then retry `numan use`."
    })?;

    if !paths_present {
        // Without NuPaths we cannot match is_active_for. Fail closed when any
        // activation record exists at all (plugins with activation or modules
        // with module_activation).
        let any_activation = lockfile
            .packages
            .values()
            .any(|e| e.activation.is_some() || e.module_activation.is_some());
        if any_activation {
            bail!(
                "Cannot switch Nu version: packages have activation records but \
                 cached Nu paths are missing, so Numan cannot safely deactivate them.\n\
                 Run `numan init --refresh`, then retry `numan use`."
            );
        }
        // No actives, no paths: proceed without leave merge of live evidence.
        return Ok(report);
    }

    let currently_active = {
        let nu_paths = NuPaths::load(root)?;
        collect_currently_active(&lockfile, &nu_paths)
    };

    // Union leave evidence even when nothing is live (no-op for empty).
    let mut profile = ActivationProfile::load_or_default(root)?;
    profile.merge_leave(leaving_minor, &currently_active);
    profile.save(root)?;

    if currently_active.is_empty() {
        return Ok(report);
    }

    let nu_paths = NuPaths::load(root)?;
    // Prefer not tearing down under drift when registry ops would hit wrong Nu.
    nu_paths.validate_drift().with_context(|| {
        "Nu binary drifted before version switch; run `numan init --refresh` then retry"
    })?;

    // Teardown: modules first (batch), then plugins.
    if !currently_active.modules.is_empty() {
        crate::cmd::deactivate::deactivate_modules_unlocked(
            root,
            &currently_active.modules,
            hooks.runner,
        )
        .with_context(|| "Failed to deactivate modules before Nu switch")?;
        report.left_modules = currently_active.modules.clone();
    }
    for id in &currently_active.plugins {
        deactivate_one_plugin(root, id, hooks.unregistrar)
            .with_context(|| format!("Failed to deactivate plugin '{id}' before Nu switch"))?;
        report.left_plugins.push(id.clone());
    }

    Ok(report)
}

/// Restore desired profile for the current (post-switch) NuPaths.
pub fn restore_after_switch(
    root: &Path,
    target_minor: &str,
    hooks: &SwitchHooks<'_>,
) -> Result<SwitchActivationReport> {
    let mut report = SwitchActivationReport::default();
    let profile = ActivationProfile::load_or_default(root)?;
    let desired = profile.set_for_minor(target_minor);
    if desired.is_empty() {
        return Ok(report);
    }

    let Some(nu_paths) = try_load_paths(root)? else {
        eprintln!(
            "warning: cannot restore activation profile for Nu {target_minor}: \
             cached Nu paths are missing. Run `numan init --refresh`, then \
             `numan use {target_minor}` again to reconcile."
        );
        return Ok(report);
    };
    if let Err(e) = nu_paths.validate_drift() {
        eprintln!(
            "warning: cannot restore activation profile ({e:#}). \
             Run `numan init --refresh`, then `numan use` again to reconcile."
        );
        return Ok(report);
    }

    restore_desired(root, &nu_paths, &desired, hooks, &mut report)?;
    Ok(report)
}

fn try_load_paths(root: &Path) -> Result<Option<NuPaths>> {
    if !root.join("nu_state").join("paths.json").is_file() {
        return Ok(None);
    }
    // Present but unreadable or malformed: surface the error.
    NuPaths::load(root).map(Some)
}

fn restore_desired(
    root: &Path,
    nu_paths: &NuPaths,
    desired: &MinorActivationSet,
    hooks: &SwitchHooks<'_>,
    report: &mut SwitchActivationReport,
) -> Result<()> {
    let lockfile = Lockfile::load(root)?;
    let registry = RegistryManager::new(root).ok();
    let platform = Platform::detect();
    let nu_ver = NuVersion::parse(&nu_paths.nu_version)?;
    let resolver = Resolver::new(&platform, &nu_ver);

    // Restore plugins first, then modules.
    for id in &desired.plugins {
        match restore_one_plugin(
            root,
            &lockfile,
            nu_paths,
            registry.as_ref(),
            &resolver,
            id,
            hooks,
        ) {
            RestoreOutcome::Restored => report.restored_plugins.push(id.clone()),
            RestoreOutcome::AlreadyActive => {}
            RestoreOutcome::Missing => report.skipped_missing.push(id.clone()),
            RestoreOutcome::Incompatible => report.skipped_incompatible.push(id.clone()),
            RestoreOutcome::Failed(err) => report.failed.push((id.clone(), err)),
        }
    }

    let module_ids: Vec<String> = desired
        .modules
        .iter()
        .filter_map(|id| {
            match classify_restore_target(&lockfile, registry.as_ref(), &resolver, id, "module") {
                RestoreClass::Missing => {
                    report.skipped_missing.push(id.clone());
                    None
                }
                RestoreClass::Incompatible => {
                    report.skipped_incompatible.push(id.clone());
                    None
                }
                RestoreClass::Ok => Some(id.clone()),
            }
        })
        .collect();

    if !module_ids.is_empty() {
        let runner_owned;
        let runner: &dyn CandidateRunner = if let Some(r) = hooks.runner {
            r
        } else {
            runner_owned = NuCandidateRunner::new(&nu_paths.nu_executable);
            &runner_owned
        };
        match crate::cmd::activate::activate_modules_unlocked(root, &module_ids, runner) {
            Ok(failed) if failed => {
                for id in &module_ids {
                    report
                        .failed
                        .push((id.clone(), "module activation lane reported failure".into()));
                }
            }
            Ok(_) => {
                for id in module_ids {
                    report.restored_modules.push(id);
                }
            }
            Err(e) => {
                for id in module_ids {
                    report.failed.push((id, e.to_string()));
                }
            }
        }
    }

    Ok(())
}

enum RestoreOutcome {
    Restored,
    AlreadyActive,
    Missing,
    Incompatible,
    Failed(String),
}

enum RestoreClass {
    Ok,
    Missing,
    Incompatible,
}

fn classify_restore_target(
    lockfile: &Lockfile,
    registry: Option<&RegistryManager>,
    resolver: &Resolver<'_>,
    id: &str,
    expected_type: &str,
) -> RestoreClass {
    let Some(entry) = lockfile.packages.get(id) else {
        return RestoreClass::Missing;
    };
    if entry.package_type != expected_type {
        return RestoreClass::Missing;
    }
    if let Some(reg) = registry {
        if let Ok(Some(pkg)) = reg.find_package(id) {
            if let Some(ver) = pkg
                .versions
                .iter()
                .find(|v| v.version.to_string() == entry.version)
            {
                if !resolver.is_compatible(ver) {
                    return RestoreClass::Incompatible;
                }
            } else if expected_type == "plugin" && !resolver.has_compatible_version(&pkg) {
                // Plugins require a binary target match, so an installed
                // version absent from the current index can still be
                // incompatible. Modules are pure Nu scripts with no target
                // constraint, so we trust the lockfile and restore them.
                return RestoreClass::Incompatible;
            }
        }
    }
    RestoreClass::Ok
}

fn restore_one_plugin(
    root: &Path,
    lockfile: &Lockfile,
    nu_paths: &NuPaths,
    registry: Option<&RegistryManager>,
    resolver: &Resolver<'_>,
    id: &str,
    hooks: &SwitchHooks<'_>,
) -> RestoreOutcome {
    match classify_restore_target(lockfile, registry, resolver, id, "plugin") {
        RestoreClass::Missing => return RestoreOutcome::Missing,
        RestoreClass::Incompatible => return RestoreOutcome::Incompatible,
        RestoreClass::Ok => {}
    }

    if let Some(entry) = lockfile.packages.get(id) {
        if entry.is_active_for(
            &nu_paths.nu_executable_hash,
            &nu_paths.nu_version,
            &nu_paths.plugin_registry_path,
        ) {
            return RestoreOutcome::AlreadyActive;
        }
    }

    match activate_one_plugin(root, id, hooks.registrar) {
        Ok(()) => RestoreOutcome::Restored,
        Err(e) => RestoreOutcome::Failed(e.to_string()),
    }
}

/// Orchestrate marker write + optional leave/restore for `numan use`.
///
/// Caller already holds the mutation lock and PreMutation snapshot.
pub fn switch_active_nu_version(
    root: &Path,
    target_version: &str,
    hooks: &SwitchHooks<'_>,
) -> Result<()> {
    let target_version = version_manager::normalize_version(target_version)?;
    let target_minor = activation_profile::nu_minor_key_from_version(&target_version)?;

    let Some(resolved) = version_manager::resolve_installed_version(root, &target_version)? else {
        let installed = version_manager::list_installed_versions(root)?;
        let hint = if installed.is_empty() {
            format!(
                "No Nu versions installed.\n\
                 Run 'numan setup nu {}' to install.",
                target_version
            )
        } else {
            format!(
                "Nu {} is not installed.\n\
                 Installed versions: {}\n\
                 Run 'numan setup nu {}' to install, or 'numan use list' to see available versions.",
                target_version,
                installed.join(", "),
                target_version
            )
        };
        bail!("{}", hint);
    };

    let current = version_manager::read_active_version(root)?;
    let same_target = current
        .as_ref()
        .is_some_and(|c| c.version == target_version);

    if same_target {
        let report = reconcile_target_profile(root, &target_minor, hooks)?;
        report.print_summary();
        if report.has_lifecycle_failure() {
            bail!(
                "One or more packages failed to restore for Nu {target_version}. \
                 Successful restores were kept; the activation profile was not changed."
            );
        }
        println!("Nu {} is already active.", target_version);
        return Ok(());
    }

    let leaving_minor = current
        .as_ref()
        .map(|c| activation_profile::nu_minor_key_from_version(&c.version))
        .transpose()?;

    let mut leave_report = SwitchActivationReport::default();
    if let Some(ref leaving) = leaving_minor {
        leave_report = leave_current_nu(root, leaving, hooks)?;
    }

    let on_tree = version_manager::version_binary(root, &target_version);
    if resolved == on_tree {
        version_manager::write_active_version(root, &target_version)
            .with_context(|| format!("Failed to switch to Nu {}", target_version))?;
    } else {
        version_manager::write_active_version_with_binary(root, &target_version, &resolved)
            .with_context(|| format!("Failed to switch to Nu {}", target_version))?;
    }
    println!("Switched to Nu {}.", target_version);

    // Refresh paths so restore targets the new Nu identity.
    if let Some(refresh) = hooks.path_refresh {
        refresh(root)?;
    } else {
        refresh_cached_nu_paths_after_switch(root)?;
    }

    let restore_report = restore_after_switch(root, &target_minor, hooks)?;

    leave_report.print_summary();
    restore_report.print_summary();

    if restore_report.has_lifecycle_failure() {
        bail!(
            "One or more packages failed to restore for Nu {target_version}. \
             Successful restores were kept; the activation profile was not changed. \
             Nu remains at {target_version}."
        );
    }

    Ok(())
}

/// Sync profile for packages the user asked to activate (idempotent desired-state).
pub fn sync_profile_after_user_activate(
    root: &Path,
    nu_version: &str,
    package_ids: &[String],
    lockfile: &Lockfile,
) -> Result<()> {
    for id in package_ids {
        let Some(entry) = lockfile.packages.get(id) else {
            continue;
        };
        let kind = match entry.package_type.as_str() {
            "plugin" => ProfileKind::Plugin,
            "module" => ProfileKind::Module,
            _ => continue,
        };
        activation_profile::ensure_contains_for_paths(root, nu_version, kind, id)?;
    }
    Ok(())
}

/// Sync profile for packages the user asked to deactivate (idempotent desired-state).
pub fn sync_profile_after_user_deactivate(
    root: &Path,
    nu_version: &str,
    package_ids: &[String],
    lockfile: &Lockfile,
) -> Result<()> {
    for id in package_ids {
        let Some(entry) = lockfile.packages.get(id) else {
            // Already removed from lockfile — still clear profile desire.
            activation_profile::ensure_absent_for_paths(root, nu_version, ProfileKind::Plugin, id)?;
            activation_profile::ensure_absent_for_paths(root, nu_version, ProfileKind::Module, id)?;
            continue;
        };
        let kind = match entry.package_type.as_str() {
            "plugin" => ProfileKind::Plugin,
            "module" => ProfileKind::Module,
            _ => continue,
        };
        activation_profile::ensure_absent_for_paths(root, nu_version, kind, id)?;
    }
    Ok(())
}

/// Keep `nu_state/paths.json` aligned with the newly selected active Nu.
fn refresh_cached_nu_paths_after_switch(root: &Path) -> Result<()> {
    let paths_file = root.join("nu_state").join("paths.json");
    if !paths_file.is_file() {
        return Ok(());
    }

    match NuPaths::detect_with_root(root) {
        Ok(refreshed) => {
            refreshed.save(root).with_context(|| {
                format!(
                    "Failed to write refreshed Nu paths to '{}'",
                    paths_file.display()
                )
            })?;
            println!("Refreshed cached Nu paths for the selected version.");
            Ok(())
        }
        Err(e) => {
            std::fs::remove_file(&paths_file).with_context(|| {
                format!(
                    "Failed to clear stale Nu paths at '{}' after version switch",
                    paths_file.display()
                )
            })?;
            eprintln!(
                "warning: could not re-probe Nu after switch ({e:#}). \
                 Cleared cached paths; run '{CMD_INIT_REFRESH}' before activating packages."
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::integrity;
    use crate::nu::autoload::FakeCandidateRunner;
    use crate::nu::version_manager;
    use crate::state::activation_profile::{ActivationProfile, ProfileKind};
    use crate::state::lockfile::{LockfileEntry, ModuleActivation, PluginActivation};
    use crate::util::fs_safety::acquire_mutation_lock;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use tempfile::TempDir;

    fn write_fake_nu(root: &Path, version: &str, contents: &[u8]) -> (String, String) {
        let binary_name = if cfg!(windows) { "nu.exe" } else { "nu" };
        let dir = version_manager::version_install_dir(root, version);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join(binary_name);
        std::fs::write(&bin, contents).unwrap();
        let hash = integrity::compute_sha256(contents);
        (bin.to_string_lossy().into_owned(), hash)
    }

    fn save_paths(
        root: &Path,
        nu_exe: &str,
        nu_version: &str,
        nu_hash: &str,
        vendor: &Path,
    ) -> NuPaths {
        let registry = root.join("plugin-registry.msgpack.z");
        std::fs::write(&registry, b"reg").unwrap();
        std::fs::create_dir_all(vendor).unwrap();
        let paths = NuPaths {
            nu_executable: nu_exe.to_string(),
            nu_version: nu_version.to_string(),
            plugin_registry_path: registry.to_string_lossy().into_owned(),
            nu_executable_hash: nu_hash.to_string(),
            platform: "test".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![vendor.to_string_lossy().into_owned()],
            vendor_autoload_dir: Some(vendor.to_string_lossy().into_owned()),
        };
        paths.save(root).unwrap();
        paths
    }

    fn plugin_entry(paths: &NuPaths, payload: &str, active: bool) -> LockfileEntry {
        LockfileEntry {
            version: "1.0.0".to_string(),
            package_type: "plugin".to_string(),
            source: "binary".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: Some("nu_plugin_x".to_string()),
            archive_root: None,
            include: None,
            entry: None,
            installed_at: "0".to_string(),
            nu_version_at_install: None,
            activation: active.then(|| PluginActivation {
                plugin_registry_path: paths.plugin_registry_path.clone(),
                nu_executable_sha256: paths.nu_executable_hash.clone(),
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
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: BTreeMap::new(),
        }
    }

    fn module_entry(paths: &NuPaths, payload: &str, active: bool) -> LockfileEntry {
        let vendor = paths.vendor_autoload_dir.clone().unwrap();
        let managed = format!("{vendor}/numan.nu");
        LockfileEntry {
            version: "1.0.0".to_string(),
            package_type: "module".to_string(),
            source: "archive".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: Some("mod.nu".to_string()),
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
            payload_path: payload.to_string(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: active.then(|| ModuleActivation {
                entry_path: format!("{payload}/mod.nu"),
                import_mode: crate::core::package::ModuleImportMode::Module,
                vendor_autoload_dir: vendor,
                managed_file_path: managed,
                nu_executable_sha256: paths.nu_executable_hash.clone(),
                nu_version: paths.nu_version.clone(),
                activated_at: "0".to_string(),
            }),
            module_import_mode: Some(crate::core::package::ModuleImportMode::Module),
            locked_dependencies: BTreeMap::new(),
        }
    }

    fn seed_plugin_payload(root: &Path, payload: &str) {
        let dir = root.join(payload);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nu_plugin_x"), b"fake").unwrap();
    }

    fn seed_module_payload(root: &Path, payload: &str) {
        let dir = root.join(payload);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mod.nu"), b"export def main [] {}").unwrap();
    }

    #[test]
    fn leave_merge_unions_never_shrinks_on_retry() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let (exe, hash) = write_fake_nu(root, "0.114.0", b"nu-114");
        let vendor = root.join("vendor");
        let paths = save_paths(root, &exe, "0.114.0", &hash, &vendor);
        version_manager::write_active_version(root, "0.114.0").unwrap();

        let payload_a = "packages/plugins/a/p1/1.0.0-aaa";
        let payload_b = "packages/plugins/a/p2/1.0.0-bbb";
        seed_plugin_payload(root, payload_a);
        seed_plugin_payload(root, payload_b);

        let mut lockfile = Lockfile::empty();
        lockfile
            .packages
            .insert("a/p1".into(), plugin_entry(&paths, payload_a, true));
        lockfile
            .packages
            .insert("a/p2".into(), plugin_entry(&paths, payload_b, true));
        lockfile.save(root).unwrap();

        let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let log_c = Rc::clone(&log);
        let unreg = move |_nu: &str, id: &str, _cfg: &str| -> Result<()> {
            log_c.borrow_mut().push(format!("unreg:{id}"));
            // Fail second plugin on first leave attempt.
            if log_c.borrow().len() == 2 {
                bail!("boom");
            }
            Ok(())
        };
        let reg = |_a: &str, _b: &str, _c: &str| Ok(());
        let hooks = SwitchHooks {
            registrar: &reg,
            unregistrar: &unreg,
            runner: None,
            path_refresh: None,
        };

        let _lock = acquire_mutation_lock(root).unwrap();
        let err = leave_current_nu(root, "0.114", &hooks).unwrap_err();
        let err_msg = format!("{err:#}");
        assert!(
            err_msg.contains("boom"),
            "expected nested boom error, got: {err_msg}"
        );

        let profile = ActivationProfile::load(root).unwrap().unwrap();
        assert_eq!(
            profile.set_for_minor("0.114").plugins,
            vec!["a/p1".to_string(), "a/p2".to_string()]
        );

        // Clear activation check for p1 (already torn down).
        let lockfile = Lockfile::load(root).unwrap();
        let p1_cleared = lockfile.packages.get("a/p1").unwrap().activation.is_none();
        assert!(p1_cleared, "first plugin should have been deactivated");

        // Retry leave: currently active is only p2; merge must retain p1∪p2.
        let unreg2 = |_a: &str, _b: &str, _c: &str| Ok(());
        let hooks2 = SwitchHooks {
            registrar: &reg,
            unregistrar: &unreg2,
            runner: None,
            path_refresh: None,
        };
        leave_current_nu(root, "0.114", &hooks2).unwrap();
        let profile = ActivationProfile::load(root).unwrap().unwrap();
        assert_eq!(
            profile.set_for_minor("0.114").plugins,
            vec!["a/p1".to_string(), "a/p2".to_string()]
        );
    }

    #[test]
    fn teardown_order_modules_then_plugins() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let (exe, hash) = write_fake_nu(root, "0.113.1", b"nu-113");
        let vendor = root.join("vendor");
        let paths = save_paths(root, &exe, "0.113.1", &hash, &vendor);

        let p_pay = "packages/plugins/o/plug/1.0.0-aaa";
        let m_pay = "packages/modules/o/mod/1.0.0-bbb";
        seed_plugin_payload(root, p_pay);
        seed_module_payload(root, m_pay);

        let mut lockfile = Lockfile::empty();
        lockfile
            .packages
            .insert("o/plug".into(), plugin_entry(&paths, p_pay, true));
        lockfile
            .packages
            .insert("o/mod".into(), module_entry(&paths, m_pay, true));
        lockfile.save(root).unwrap();

        // Seed managed autoload so full module deactivate can delete it.
        let managed = vendor.join("numan.nu");
        std::fs::write(
            &managed,
            format!("{}\n", crate::util::fs_safety::OWNERSHIP_MARKER),
        )
        .unwrap();

        let order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let o1 = Rc::clone(&order);
        let o2 = Rc::clone(&order);
        let unreg = move |_a: &str, _b: &str, _c: &str| -> Result<()> {
            o1.borrow_mut().push("plugin");
            Ok(())
        };
        let reg = |_a: &str, _b: &str, _c: &str| Ok(());
        let inner_runner = FakeCandidateRunner::success();

        struct TrackingRunner<'a> {
            inner: &'a FakeCandidateRunner,
            order: Rc<RefCell<Vec<&'static str>>>,
        }
        impl CandidateRunner for TrackingRunner<'_> {
            fn run(&self, candidate: &Path) -> Result<()> {
                self.order.borrow_mut().push("module");
                self.inner.run(candidate)
            }
        }

        let runner = TrackingRunner {
            inner: &inner_runner,
            order: o2,
        };
        let hooks = SwitchHooks {
            registrar: &reg,
            unregistrar: &unreg,
            runner: Some(&runner),
            path_refresh: None,
        };

        let _lock = acquire_mutation_lock(root).unwrap();
        leave_current_nu(root, "0.113", &hooks).unwrap();
        assert_eq!(
            order.borrow().as_slice(),
            &["module", "plugin"][..],
            "module lane executes before plugin unreg"
        );
        let lockfile = Lockfile::load(root).unwrap();
        assert!(lockfile
            .packages
            .get("o/mod")
            .unwrap()
            .module_activation
            .is_none());
        assert!(lockfile
            .packages
            .get("o/plug")
            .unwrap()
            .activation
            .is_none());
    }

    #[test]
    fn missing_paths_with_actives_fails_before_marker() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("state")).unwrap();
        write_fake_nu(root, "0.113.1", b"nu-113");
        write_fake_nu(root, "0.114.0", b"nu-114");
        version_manager::write_active_version(root, "0.113.1").unwrap();

        let mut lockfile = Lockfile::empty();
        lockfile.packages.insert(
            "a/p".into(),
            LockfileEntry {
                version: "1.0.0".to_string(),
                package_type: "plugin".to_string(),
                source: "binary".to_string(),
                target: None,
                artifact_url: None,
                artifact_sha256: None,
                executable_path: Some("nu_plugin_x".to_string()),
                archive_root: None,
                include: None,
                entry: None,
                installed_at: "0".to_string(),
                nu_version_at_install: None,
                activation: Some(PluginActivation {
                    plugin_registry_path: "/x".into(),
                    nu_executable_sha256: "h".into(),
                    nu_version: "0.113.1".into(),
                    activated_at: "0".into(),
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
                payload_path: "packages/plugins/a/p/1.0.0-aaa".into(),
                revision_id: None,
                payload_sha256: None,
                executable_sha256: None,
                selection_reason: None,
                origin: None,
                module_activation: None,
                module_import_mode: None,
                locked_dependencies: BTreeMap::new(),
            },
        );
        lockfile.save(root).unwrap();
        // No paths.json

        let reg = |_a: &str, _b: &str, _c: &str| Ok(());
        let unreg = |_a: &str, _b: &str, _c: &str| Ok(());
        let hooks = SwitchHooks {
            registrar: &reg,
            unregistrar: &unreg,
            runner: None,
            path_refresh: None,
        };
        let _lock = acquire_mutation_lock(root).unwrap();
        let err = switch_active_nu_version(root, "0.114.0", &hooks).unwrap_err();
        assert!(
            err.to_string().contains("cached Nu paths are missing"),
            "got: {err}"
        );
        assert_eq!(
            version_manager::read_active_version(root)
                .unwrap()
                .unwrap()
                .version,
            "0.113.1",
            "marker must not change on fail-closed leave"
        );
    }

    #[test]
    fn round_trip_cross_minor_restores_plugin() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let (exe113, hash113) = write_fake_nu(root, "0.113.1", b"nu-113-bytes");
        let (exe114, hash114) = write_fake_nu(root, "0.114.0", b"nu-114-bytes");
        let vendor = root.join("vendor");
        let paths113 = save_paths(root, &exe113, "0.113.1", &hash113, &vendor);
        version_manager::write_active_version(root, "0.113.1").unwrap();

        let payload = "packages/plugins/o/semver/1.0.0-aaa";
        seed_plugin_payload(root, payload);
        let mut lockfile = Lockfile::empty();
        lockfile
            .packages
            .insert("o/semver".into(), plugin_entry(&paths113, payload, true));
        lockfile.save(root).unwrap();

        let unreg_count = Rc::new(RefCell::new(0usize));
        let reg_count = Rc::new(RefCell::new(0usize));
        let uc = Rc::clone(&unreg_count);
        let rc = Rc::clone(&reg_count);
        let unreg = move |_a: &str, _b: &str, _c: &str| {
            *uc.borrow_mut() += 1;
            Ok(())
        };
        let reg = move |_a: &str, _b: &str, _c: &str| {
            *rc.borrow_mut() += 1;
            Ok(())
        };

        let refresh114 = move |root: &Path| -> Result<()> {
            let vendor = root.join("vendor");
            save_paths(root, &exe114, "0.114.0", &hash114, &vendor);
            Ok(())
        };
        let refresh113 = {
            let exe113 = exe113.clone();
            let hash113 = hash113.clone();
            move |root: &Path| -> Result<()> {
                let vendor = root.join("vendor");
                save_paths(root, &exe113, "0.113.1", &hash113, &vendor);
                Ok(())
            }
        };

        let hooks_to_114 = SwitchHooks {
            registrar: &reg,
            unregistrar: &unreg,
            runner: None,
            path_refresh: Some(&refresh114),
        };
        let _lock = acquire_mutation_lock(root).unwrap();
        switch_active_nu_version(root, "0.114.0", &hooks_to_114).unwrap();
        assert_eq!(*unreg_count.borrow(), 1);
        assert!(Lockfile::load(root)
            .unwrap()
            .packages
            .get("o/semver")
            .unwrap()
            .activation
            .is_none());
        let profile = ActivationProfile::load(root).unwrap().unwrap();
        assert_eq!(
            profile.set_for_minor("0.113").plugins,
            vec!["o/semver".to_string()]
        );

        // Seed empty 0.114 profile desire so restore on return is from 0.113 leave.
        let hooks_to_113 = SwitchHooks {
            registrar: &reg,
            unregistrar: &unreg,
            runner: None,
            path_refresh: Some(&refresh113),
        };
        switch_active_nu_version(root, "0.113.1", &hooks_to_113).unwrap();
        assert_eq!(*reg_count.borrow(), 1);
        assert!(Lockfile::load(root)
            .unwrap()
            .packages
            .get("o/semver")
            .unwrap()
            .is_active_for(&hash113, "0.113.1", &paths113.plugin_registry_path));
    }

    #[test]
    fn same_target_restore_only_does_not_deactivate() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let (exe, hash) = write_fake_nu(root, "0.113.1", b"nu-113");
        let vendor = root.join("vendor");
        let paths = save_paths(root, &exe, "0.113.1", &hash, &vendor);
        version_manager::write_active_version(root, "0.113.1").unwrap();

        let payload_active = "packages/plugins/o/a/1.0.0-aaa";
        let payload_desired = "packages/plugins/o/b/1.0.0-bbb";
        seed_plugin_payload(root, payload_active);
        seed_plugin_payload(root, payload_desired);

        let mut lockfile = Lockfile::empty();
        lockfile
            .packages
            .insert("o/a".into(), plugin_entry(&paths, payload_active, true));
        lockfile
            .packages
            .insert("o/b".into(), plugin_entry(&paths, payload_desired, false));
        lockfile.save(root).unwrap();

        let mut profile = ActivationProfile::new();
        profile.ensure_contains("0.113", ProfileKind::Plugin, "o/a");
        profile.ensure_contains("0.113", ProfileKind::Plugin, "o/b");
        profile.save(root).unwrap();

        let unreg_n = Rc::new(RefCell::new(0usize));
        let reg_n = Rc::new(RefCell::new(0usize));
        let uc = Rc::clone(&unreg_n);
        let rc = Rc::clone(&reg_n);
        let unreg = move |_a: &str, _b: &str, _c: &str| {
            *uc.borrow_mut() += 1;
            Ok(())
        };
        let reg = move |_a: &str, _b: &str, _c: &str| {
            *rc.borrow_mut() += 1;
            Ok(())
        };
        let hooks = SwitchHooks {
            registrar: &reg,
            unregistrar: &unreg,
            runner: None,
            path_refresh: None,
        };
        let _lock = acquire_mutation_lock(root).unwrap();
        switch_active_nu_version(root, "0.113.1", &hooks).unwrap();
        assert_eq!(*unreg_n.borrow(), 0, "same-target must not deactivate");
        assert_eq!(*reg_n.borrow(), 1, "missing desired must be restored");
        assert!(Lockfile::load(root)
            .unwrap()
            .packages
            .get("o/a")
            .unwrap()
            .activation
            .is_some());
        assert!(Lockfile::load(root)
            .unwrap()
            .packages
            .get("o/b")
            .unwrap()
            .activation
            .is_some());
    }

    #[test]
    fn remove_clears_id_from_all_minors() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut profile = ActivationProfile::new();
        profile.ensure_contains("0.113", ProfileKind::Plugin, "a/pkg");
        profile.ensure_contains("0.114", ProfileKind::Module, "a/pkg");
        profile.save(root).unwrap();
        crate::state::activation_profile::remove_from_all_minors(root, "a/pkg").unwrap();
        let profile = ActivationProfile::load_or_default(root).unwrap();
        assert!(profile.set_for_minor("0.113").is_empty());
        assert!(profile.set_for_minor("0.114").is_empty());
    }

    #[test]
    fn activate_already_active_ensures_profile_contains() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let (exe, hash) = write_fake_nu(root, "0.113.1", b"nu-113");
        let vendor = root.join("vendor");
        let paths = save_paths(root, &exe, "0.113.1", &hash, &vendor);
        let payload = "packages/plugins/o/p/1.0.0-aaa";
        seed_plugin_payload(root, payload);
        let mut lockfile = Lockfile::empty();
        lockfile
            .packages
            .insert("o/p".into(), plugin_entry(&paths, payload, true));
        lockfile.save(root).unwrap();

        let reg = |_a: &str, _b: &str, _c: &str| Ok(());
        crate::cmd::activate::execute_with_registrar(
            &crate::cmd::activate::ActivateArgs {
                packages: vec!["o/p".into()],
                verbose: false,
                list: false,
                check: false,
            },
            root,
            &reg,
        )
        .unwrap();

        let profile = ActivationProfile::load(root).unwrap().unwrap();
        assert_eq!(
            profile.set_for_minor("0.113").plugins,
            vec!["o/p".to_string()]
        );
    }

    #[test]
    fn deactivate_already_inactive_clears_profile() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let (exe, hash) = write_fake_nu(root, "0.113.1", b"nu-113");
        let vendor = root.join("vendor");
        let paths = save_paths(root, &exe, "0.113.1", &hash, &vendor);
        let payload = "packages/plugins/o/p/1.0.0-aaa";
        seed_plugin_payload(root, payload);
        let mut lockfile = Lockfile::empty();
        lockfile
            .packages
            .insert("o/p".into(), plugin_entry(&paths, payload, false));
        lockfile.save(root).unwrap();

        let mut profile = ActivationProfile::new();
        profile.ensure_contains("0.113", ProfileKind::Plugin, "o/p");
        profile.save(root).unwrap();

        let unreg = |_a: &str, _b: &str, _c: &str| Ok(());
        crate::cmd::deactivate::execute_with_unregistrar(
            &crate::cmd::deactivate::DeactivateArgs {
                packages: vec!["o/p".into()],
                verbose: false,
            },
            root,
            &unreg,
        )
        .unwrap();

        let profile = ActivationProfile::load_or_default(root).unwrap();
        assert!(
            profile.set_for_minor("0.113").is_empty(),
            "retry after deactivated lifecycle must clear profile desire"
        );
    }

    #[test]
    fn restore_order_plugins_then_modules() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let (exe, hash) = write_fake_nu(root, "0.113.1", b"nu-113");
        let vendor = root.join("vendor");
        let paths = save_paths(root, &exe, "0.113.1", &hash, &vendor);
        version_manager::write_active_version(root, "0.113.1").unwrap();

        let p_pay = "packages/plugins/o/plug/1.0.0-aaa";
        let m_pay = "packages/modules/o/mod/1.0.0-bbb";
        seed_plugin_payload(root, p_pay);
        seed_module_payload(root, m_pay);

        let mut lockfile = Lockfile::empty();
        lockfile
            .packages
            .insert("o/plug".into(), plugin_entry(&paths, p_pay, false));
        lockfile
            .packages
            .insert("o/mod".into(), module_entry(&paths, m_pay, false));
        // entry_path for inactive modules comes from payload on activate
        lockfile.save(root).unwrap();

        let mut profile = ActivationProfile::new();
        profile.ensure_contains("0.113", ProfileKind::Plugin, "o/plug");
        profile.ensure_contains("0.113", ProfileKind::Module, "o/mod");
        profile.save(root).unwrap();

        let order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let o1 = Rc::clone(&order);
        let reg = move |_a: &str, _b: &str, _c: &str| -> Result<()> {
            o1.borrow_mut().push("plugin");
            Ok(())
        };
        let unreg = |_a: &str, _b: &str, _c: &str| Ok(());
        let runner = FakeCandidateRunner::success();
        // Track module activation by wrapping order push via a custom runner
        // won't work easily. Instead: after restore, both active; call order
        // observed because plugin reg runs before module lane starts.
        let hooks = SwitchHooks {
            registrar: &reg,
            unregistrar: &unreg,
            runner: Some(&runner),
            path_refresh: None,
        };
        let _lock = acquire_mutation_lock(root).unwrap();
        let report = reconcile_target_profile(root, "0.113", &hooks).unwrap();
        assert_eq!(order.borrow().as_slice(), &["plugin"][..]);
        assert!(report.restored_plugins.contains(&"o/plug".to_string()));
        assert!(report.restored_modules.contains(&"o/mod".to_string()));
        let lf = Lockfile::load(root).unwrap();
        assert!(lf.packages.get("o/plug").unwrap().activation.is_some());
        assert!(lf
            .packages
            .get("o/mod")
            .unwrap()
            .module_activation
            .is_some());
    }
}
