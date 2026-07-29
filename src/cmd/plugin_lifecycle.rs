//! Shared single-plugin activate/deactivate helpers (Issue #22 PR3).
//!
//! Owns the activate/deactivate integration boundary used by opt-in `update`
//! orchestration while the caller already holds the mutation lock. Callers use
//! [`PluginLifecycle`] and never receive or invoke Nu registrar callbacks.
//! These helpers intentionally do **not** acquire `mutation.lock`, print consent
//! tables, or create snapshots (callers own those boundaries).
//!
//! Full CLI flows remain in [`super::activate`] and [`super::deactivate`].

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::cmd::activate::validate_payload_path;
use crate::cmd::deactivate::plugin_name_from_executable_path;
use crate::nu::paths::NuPaths;
use crate::state::journal::{PendingActivation, PendingActivationEntry, PendingStatus};
use crate::state::lockfile::{Lockfile, PluginActivation};
use crate::state::plugin_deactivate_journal::{
    PendingPluginDeactivate, PendingPluginDeactivateEntry, PluginDeactivateStatus,
};
use crate::util::format_timestamp;
use crate::util::hints::{self, CMD_ACTIVATE, CMD_DEACTIVATE, CMD_INIT_REFRESH};

/// Activate/deactivate boundary exposed to lifecycle coordinators such as
/// `update`. Nu-specific registrar callbacks remain private to this module.
pub trait PluginLifecycle {
    fn deactivate(&self, root: &Path, pkg_id: &str) -> Result<()>;
    fn activate(&self, root: &Path, pkg_id: &str) -> Result<()>;
}

/// Production implementation backed by the activate/deactivate commands'
/// Nu integration seams.
pub struct CommandPluginLifecycle;

impl PluginLifecycle for CommandPluginLifecycle {
    fn deactivate(&self, root: &Path, pkg_id: &str) -> Result<()> {
        deactivate_one_plugin(root, pkg_id, &crate::cmd::deactivate::run_plugin_rm)
    }

    fn activate(&self, root: &Path, pkg_id: &str) -> Result<()> {
        activate_one_plugin(root, pkg_id, &crate::cmd::activate::run_plugin_add)
    }
}

/// Unregister one active plugin and clear its lockfile `activation` record.
///
/// Caller must hold the root mutation lock. Idempotent when already inactive.
pub fn deactivate_one_plugin(
    root: &Path,
    pkg_id: &str,
    unregistrar: &dyn Fn(&str, &str, &str) -> Result<()>,
) -> Result<()> {
    let nu_paths = NuPaths::load(root)?;
    nu_paths.validate_drift()?;
    let mut lockfile = Lockfile::load(root)?;

    let entry = lockfile
        .packages
        .get(pkg_id)
        .with_context(|| format!("Package '{pkg_id}' is not installed"))?;

    if entry.package_type != "plugin" {
        bail!(
            "Package '{pkg_id}' is not a plugin (type: {})",
            entry.package_type
        );
    }

    if entry.activation.is_none() {
        return Ok(());
    }

    let executable_path = entry
        .executable_path
        .as_deref()
        .with_context(|| format!("{pkg_id}: missing executable_path in lockfile"))?
        .to_string();
    let plugin_name = plugin_name_from_executable_path(&executable_path);
    let absolute_binary_path = root.join(&entry.payload_path).join(&executable_path);
    let was_active_for = entry.is_active_for(
        &nu_paths.nu_executable_hash,
        &nu_paths.nu_version,
        &nu_paths.plugin_registry_path,
    );

    // Singleton journal: commit Unregistered clears / refuse unfinished foreign
    // work before overwriting (mirror of activate_one_plugin).
    reconcile_or_refuse_pending_deactivate(root, &nu_paths, &mut lockfile, pkg_id)?;

    // Same-package Unregistered commit: Nu already unregistered; do not retry.
    if lockfile
        .packages
        .get(pkg_id)
        .is_none_or(|e| e.activation.is_none())
    {
        return Ok(());
    }

    let mut journal = PendingPluginDeactivate {
        nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
        nu_version: nu_paths.nu_version.clone(),
        plugin_registry_path: nu_paths.plugin_registry_path.clone(),
        created_at: format_timestamp(),
        entries: vec![PendingPluginDeactivateEntry {
            package_id: pkg_id.to_string(),
            plugin_name: plugin_name.clone(),
            absolute_binary_path: absolute_binary_path.to_string_lossy().into_owned(),
            status: PluginDeactivateStatus::Prepared,
            error: None,
        }],
    };
    journal.save(root)?;

    // Lockfile-grounded ownership preflight (no msgpackz parse available).
    if !was_active_for {
        let err = anyhow::anyhow!(
            "Plugin '{pkg_id}' activation is stale or mismatched for current Nu identity"
        );
        journal.entries[0].status = PluginDeactivateStatus::Failed;
        journal.entries[0].error = Some(err.to_string());
        journal.save(root)?;
        return Err(err);
    }
    if !absolute_binary_path.is_file() {
        let err = anyhow::anyhow!(
            "Plugin '{pkg_id}' binary is missing at {}",
            absolute_binary_path.display()
        );
        journal.entries[0].status = PluginDeactivateStatus::Failed;
        journal.entries[0].error = Some(err.to_string());
        journal.save(root)?;
        return Err(err);
    }

    let rm_identity = absolute_binary_path.to_string_lossy();
    match unregistrar(
        &nu_paths.nu_executable,
        &rm_identity,
        &nu_paths.plugin_registry_path,
    ) {
        Ok(()) => {
            journal.entries[0].status = PluginDeactivateStatus::Unregistered;
            journal.save(root)?;

            if let Some(pkg) = lockfile.packages.get_mut(pkg_id) {
                pkg.activation = None;
            }
            lockfile.save(root)?;
            PendingPluginDeactivate::delete(root)?;
            Ok(())
        }
        Err(e) => {
            journal.entries[0].status = PluginDeactivateStatus::Failed;
            journal.entries[0].error = Some(e.to_string());
            journal.save(root)?;
            Err(e).with_context(|| format!("Failed to unregister active plugin '{pkg_id}'"))
        }
    }
}

/// Register one inactive plugin and write its lockfile `activation` record.
///
/// Caller must hold the root mutation lock. Fails if the package is already
/// active for the current Nu identity.
pub fn activate_one_plugin(
    root: &Path,
    pkg_id: &str,
    registrar: &dyn Fn(&str, &str, &str) -> Result<()>,
) -> Result<()> {
    let nu_paths = NuPaths::load(root)?;
    nu_paths.validate_drift()?;
    let mut lockfile = Lockfile::load(root)?;

    let entry = lockfile
        .packages
        .get(pkg_id)
        .with_context(|| format!("Package '{pkg_id}' is not installed"))?;

    if entry.package_type != "plugin" {
        bail!(
            "Package '{pkg_id}' is not a plugin (type: {})",
            entry.package_type
        );
    }

    if entry.is_active_for(
        &nu_paths.nu_executable_hash,
        &nu_paths.nu_version,
        &nu_paths.plugin_registry_path,
    ) {
        return Ok(());
    }

    let executable_path = entry
        .executable_path
        .as_deref()
        .with_context(|| format!("{pkg_id}: missing executable_path in lockfile"))?
        .to_string();
    let payload_path = entry.payload_path.clone();
    let absolute_binary_path = validate_payload_path(root, &payload_path, &executable_path)?;
    let binary_str = absolute_binary_path.to_string_lossy().into_owned();

    // Singleton journal: commit Registered entries / refuse unfinished foreign
    // work before overwriting (Codex: do not clobber a Registered journal).
    reconcile_or_refuse_pending_activation(root, &nu_paths, &mut lockfile, pkg_id)?;

    // Same-package Registered commit: Nu already registered; do not call
    // registrar again (would risk a false failure after successful recovery).
    if lockfile.packages.get(pkg_id).is_some_and(|e| {
        e.is_active_for(
            &nu_paths.nu_executable_hash,
            &nu_paths.nu_version,
            &nu_paths.plugin_registry_path,
        )
    }) {
        return Ok(());
    }

    let mut journal = PendingActivation {
        nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
        nu_version: nu_paths.nu_version.clone(),
        plugin_registry_path: nu_paths.plugin_registry_path.clone(),
        created_at: format_timestamp(),
        entries: vec![PendingActivationEntry {
            package_id: pkg_id.to_string(),
            payload_path: payload_path.clone(),
            executable_path: executable_path.clone(),
            absolute_binary_path: binary_str.clone(),
            status: PendingStatus::Prepared,
            error: None,
        }],
    };
    journal.save(root)?;

    match registrar(
        &nu_paths.nu_executable,
        &binary_str,
        &nu_paths.plugin_registry_path,
    ) {
        Ok(()) => {
            journal.entries[0].status = PendingStatus::Registered;
            journal.save(root)?;

            if let Some(pkg) = lockfile.packages.get_mut(pkg_id) {
                pkg.activation = Some(PluginActivation {
                    plugin_registry_path: nu_paths.plugin_registry_path.clone(),
                    nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
                    nu_version: nu_paths.nu_version.clone(),
                    activated_at: format_timestamp(),
                });
            }
            lockfile.save(root)?;
            PendingActivation::delete(root)?;
            Ok(())
        }
        Err(e) => {
            journal.entries[0].status = PendingStatus::Failed;
            journal.entries[0].error = Some(e.to_string());
            journal.save(root)?;
            Err(e).with_context(|| format!("Failed to register plugin '{pkg_id}' after update"))
        }
    }
}

/// Commit `Unregistered` pending-deactivate entries, then clear the journal so a
/// new singleton write is safe.
///
/// Unfinished (`Prepared` / `Failed`) entries for **other** packages refuse so
/// we do not discard their recovery path. Same-package unfinished entries are
/// dropped: this helper is about to retry deactivation for `pkg_id`.
fn reconcile_or_refuse_pending_deactivate(
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
    pkg_id: &str,
) -> Result<()> {
    let Some(existing) = PendingPluginDeactivate::load(root)? else {
        return Ok(());
    };

    let identity_ok = existing.matches_nu_identity(
        &nu_paths.nu_executable_hash,
        &nu_paths.nu_version,
        &nu_paths.plugin_registry_path,
    );

    // Commit Unregistered clears even when identity drifted (no Nu spawn needed).
    let mut changed = false;
    let mut remaining: Vec<PendingPluginDeactivateEntry> = Vec::new();
    for entry in existing.entries {
        if entry.status == PluginDeactivateStatus::Unregistered {
            if let Some(pkg) = lockfile.packages.get_mut(&entry.package_id) {
                if pkg.activation.is_some() {
                    pkg.activation = None;
                    changed = true;
                }
            }
            continue;
        }
        remaining.push(entry);
    }
    if changed {
        lockfile.save(root)?;
    }

    if remaining.is_empty() {
        PendingPluginDeactivate::delete(root)?;
        return Ok(());
    }

    let unfinished_other: Vec<String> = remaining
        .iter()
        .filter(|e| {
            e.package_id != pkg_id
                && matches!(
                    e.status,
                    PluginDeactivateStatus::Prepared | PluginDeactivateStatus::Failed
                )
        })
        .map(|e| e.package_id.clone())
        .collect();

    // Persist drained state before refusing so Unregistered clears are kept.
    PendingPluginDeactivate {
        nu_executable_sha256: existing.nu_executable_sha256,
        nu_version: existing.nu_version,
        plugin_registry_path: existing.plugin_registry_path,
        created_at: existing.created_at,
        entries: remaining,
    }
    .save(root)?;

    if !identity_ok {
        bail!(
            "A pending plugin deactivation journal exists from a different Nu identity.\n\
             {}\n\
             Journal: {}",
            hints::run_then(CMD_INIT_REFRESH, CMD_DEACTIVATE),
            root.join("state/pending-plugin-deactivate.json").display()
        );
    }

    if !unfinished_other.is_empty() {
        bail!(
            "A pending plugin deactivation journal still has unfinished entries for: {}.\n\
             {}\n\
             Journal: {}",
            unfinished_other.join(", "),
            hints::run(CMD_DEACTIVATE),
            root.join("state/pending-plugin-deactivate.json").display()
        );
    }

    // Same-package Prepared/Failed only: drop journal and retry.
    PendingPluginDeactivate::delete(root)?;
    Ok(())
}

/// Commit `Registered` pending-activation entries, then clear the journal so a
/// new singleton write is safe.
///
/// Unfinished (`Prepared` / `Failed`) entries for **other** packages refuse so
/// we do not discard their recovery path. Same-package unfinished entries are
/// dropped: this helper is about to retry activation for `pkg_id`.
fn reconcile_or_refuse_pending_activation(
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
    pkg_id: &str,
) -> Result<()> {
    let Some(existing) = PendingActivation::load(root)? else {
        return Ok(());
    };

    if !existing.matches_nu_identity(
        &nu_paths.nu_executable_hash,
        &nu_paths.nu_version,
        &nu_paths.plugin_registry_path,
    ) {
        bail!(
            "A pending plugin activation journal exists from a different Nu identity.\n\
             {}\n\
             Journal: {}",
            hints::run_then(CMD_INIT_REFRESH, CMD_ACTIVATE),
            root.join("state/pending-activation.json").display()
        );
    }

    let unfinished_other: Vec<&str> = existing
        .entries
        .iter()
        .filter(|e| {
            e.package_id != pkg_id
                && matches!(e.status, PendingStatus::Prepared | PendingStatus::Failed)
        })
        .map(|e| e.package_id.as_str())
        .collect();
    if !unfinished_other.is_empty() {
        bail!(
            "A pending plugin activation journal still has unfinished entries for: {}.\n\
             {}\n\
             Journal: {}",
            unfinished_other.join(", "),
            hints::run(CMD_ACTIVATE),
            root.join("state/pending-activation.json").display()
        );
    }

    let mut committed = false;
    for entry in &existing.entries {
        if entry.status != PendingStatus::Registered {
            continue;
        }
        if let Some(pkg) = lockfile.packages.get_mut(&entry.package_id) {
            pkg.activation = Some(PluginActivation {
                plugin_registry_path: nu_paths.plugin_registry_path.clone(),
                nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
                nu_version: nu_paths.nu_version.clone(),
                activated_at: format_timestamp(),
            });
            committed = true;
        }
    }
    if committed {
        lockfile.save(root)?;
    }
    PendingActivation::delete(root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::integrity;
    use crate::state::lockfile::LockfileEntry;
    use crate::util::format_timestamp;
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, String, String) {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

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
        paths.save(root).unwrap();

        let pkg_a = "owner/a".to_string();
        let pkg_b = "owner/b".to_string();
        let mut lockfile = Lockfile::empty();
        for (id, payload) in [
            (&pkg_a, "packages/plugins/owner/a/1.0.0-abc"),
            (&pkg_b, "packages/plugins/owner/b/1.0.0-abc"),
        ] {
            let payload_dir = root.join(payload);
            std::fs::create_dir_all(&payload_dir).unwrap();
            std::fs::write(payload_dir.join("nu_plugin_x"), b"fake").unwrap();
            lockfile.packages.insert(
                id.clone(),
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
                        plugin_registry_path: paths.plugin_registry_path.clone(),
                        nu_executable_sha256: nu_hash.clone(),
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
                },
            );
        }
        lockfile.save(root).unwrap();

        (dir, pkg_a, pkg_b)
    }

    fn clear_activation(root: &Path, pkg_id: &str) {
        let mut lockfile = Lockfile::load(root).unwrap();
        lockfile.packages.get_mut(pkg_id).unwrap().activation = None;
        lockfile.save(root).unwrap();
    }

    #[test]
    fn activate_commits_registered_journal_before_overwrite() {
        let (dir, pkg_a, pkg_b) = fixture();
        let root = dir.path();
        let paths = NuPaths::load(root).unwrap();
        clear_activation(root, &pkg_a);
        clear_activation(root, &pkg_b);

        PendingActivation {
            nu_executable_sha256: paths.nu_executable_hash.clone(),
            nu_version: paths.nu_version.clone(),
            plugin_registry_path: paths.plugin_registry_path.clone(),
            created_at: format_timestamp(),
            entries: vec![PendingActivationEntry {
                package_id: pkg_a.clone(),
                payload_path: "packages/plugins/owner/a/1.0.0-abc".to_string(),
                executable_path: "nu_plugin_x".to_string(),
                absolute_binary_path: root
                    .join("packages/plugins/owner/a/1.0.0-abc/nu_plugin_x")
                    .to_string_lossy()
                    .into_owned(),
                status: PendingStatus::Registered,
                error: None,
            }],
        }
        .save(root)
        .unwrap();

        activate_one_plugin(root, &pkg_b, &|_nu, _bin, _cfg| Ok(())).unwrap();

        let lockfile = Lockfile::load(root).unwrap();
        assert!(
            lockfile.packages[&pkg_a].activation.is_some(),
            "Registered journal entry must commit activation before overwrite"
        );
        assert!(lockfile.packages[&pkg_b].activation.is_some());
        assert!(PendingActivation::load(root).unwrap().is_none());
    }

    #[test]
    fn activate_refuses_unfinished_foreign_journal_entries() {
        let (dir, pkg_a, pkg_b) = fixture();
        let root = dir.path();
        let paths = NuPaths::load(root).unwrap();
        clear_activation(root, &pkg_b);

        PendingActivation {
            nu_executable_sha256: paths.nu_executable_hash.clone(),
            nu_version: paths.nu_version.clone(),
            plugin_registry_path: paths.plugin_registry_path.clone(),
            created_at: format_timestamp(),
            entries: vec![PendingActivationEntry {
                package_id: pkg_a.clone(),
                payload_path: "packages/plugins/owner/a/1.0.0-abc".to_string(),
                executable_path: "nu_plugin_x".to_string(),
                absolute_binary_path: root
                    .join("packages/plugins/owner/a/1.0.0-abc/nu_plugin_x")
                    .to_string_lossy()
                    .into_owned(),
                status: PendingStatus::Prepared,
                error: None,
            }],
        }
        .save(root)
        .unwrap();

        let err = activate_one_plugin(root, &pkg_b, &|_nu, _bin, _cfg| Ok(())).unwrap_err();
        assert!(err.to_string().contains("unfinished entries"));
        assert!(err.to_string().contains(&pkg_a));
        assert!(PendingActivation::load(root).unwrap().is_some());
        assert!(Lockfile::load(root).unwrap().packages[&pkg_b]
            .activation
            .is_none());
    }

    #[test]
    fn deactivate_commits_unregistered_journal_before_overwrite() {
        let (dir, pkg_a, pkg_b) = fixture();
        let root = dir.path();
        let paths = NuPaths::load(root).unwrap();

        // Simulate crash after Nu unregister for pkg_a: journal Unregistered,
        // lockfile activation still present.
        PendingPluginDeactivate {
            nu_executable_sha256: paths.nu_executable_hash.clone(),
            nu_version: paths.nu_version.clone(),
            plugin_registry_path: paths.plugin_registry_path.clone(),
            created_at: format_timestamp(),
            entries: vec![PendingPluginDeactivateEntry {
                package_id: pkg_a.clone(),
                plugin_name: "nu_plugin_x".to_string(),
                absolute_binary_path: root
                    .join("packages/plugins/owner/a/1.0.0-abc/nu_plugin_x")
                    .to_string_lossy()
                    .into_owned(),
                status: PluginDeactivateStatus::Unregistered,
                error: None,
            }],
        }
        .save(root)
        .unwrap();

        deactivate_one_plugin(root, &pkg_b, &|_nu, _name, _cfg| Ok(())).unwrap();

        let lockfile = Lockfile::load(root).unwrap();
        assert!(
            lockfile.packages[&pkg_a].activation.is_none(),
            "Unregistered journal entry must clear activation before overwrite"
        );
        assert!(lockfile.packages[&pkg_b].activation.is_none());
        assert!(PendingPluginDeactivate::load(root).unwrap().is_none());
    }

    #[test]
    fn deactivate_refuses_unfinished_foreign_journal_entries() {
        let (dir, pkg_a, pkg_b) = fixture();
        let root = dir.path();
        let paths = NuPaths::load(root).unwrap();

        PendingPluginDeactivate {
            nu_executable_sha256: paths.nu_executable_hash.clone(),
            nu_version: paths.nu_version.clone(),
            plugin_registry_path: paths.plugin_registry_path.clone(),
            created_at: format_timestamp(),
            entries: vec![PendingPluginDeactivateEntry {
                package_id: pkg_a.clone(),
                plugin_name: "nu_plugin_x".to_string(),
                absolute_binary_path: root
                    .join("packages/plugins/owner/a/1.0.0-abc/nu_plugin_x")
                    .to_string_lossy()
                    .into_owned(),
                status: PluginDeactivateStatus::Prepared,
                error: None,
            }],
        }
        .save(root)
        .unwrap();

        let err = deactivate_one_plugin(root, &pkg_b, &|_nu, _name, _cfg| Ok(())).unwrap_err();
        assert!(err.to_string().contains("unfinished entries"));
        assert!(err.to_string().contains(&pkg_a));
        assert!(PendingPluginDeactivate::load(root).unwrap().is_some());
        assert!(Lockfile::load(root).unwrap().packages[&pkg_b]
            .activation
            .is_some());
    }

    #[test]
    fn activate_returns_after_committing_same_package_registered_journal() {
        let (dir, pkg_a, _pkg_b) = fixture();
        let root = dir.path();
        let paths = NuPaths::load(root).unwrap();
        clear_activation(root, &pkg_a);

        PendingActivation {
            nu_executable_sha256: paths.nu_executable_hash.clone(),
            nu_version: paths.nu_version.clone(),
            plugin_registry_path: paths.plugin_registry_path.clone(),
            created_at: format_timestamp(),
            entries: vec![PendingActivationEntry {
                package_id: pkg_a.clone(),
                payload_path: "packages/plugins/owner/a/1.0.0-abc".to_string(),
                executable_path: "nu_plugin_x".to_string(),
                absolute_binary_path: root
                    .join("packages/plugins/owner/a/1.0.0-abc/nu_plugin_x")
                    .to_string_lossy()
                    .into_owned(),
                status: PendingStatus::Registered,
                error: None,
            }],
        }
        .save(root)
        .unwrap();

        let registrar_calls = Cell::new(0usize);
        activate_one_plugin(root, &pkg_a, &|_nu, _bin, _cfg| {
            registrar_calls.set(registrar_calls.get() + 1);
            bail!("registrar must not run after Registered reconcile")
        })
        .unwrap();

        assert_eq!(
            registrar_calls.get(),
            0,
            "must not re-register after reconcile"
        );
        assert!(Lockfile::load(root).unwrap().packages[&pkg_a]
            .activation
            .is_some());
        assert!(PendingActivation::load(root).unwrap().is_none());
    }

    #[test]
    fn deactivate_returns_after_committing_same_package_unregistered_journal() {
        let (dir, pkg_a, _pkg_b) = fixture();
        let root = dir.path();
        let paths = NuPaths::load(root).unwrap();

        PendingPluginDeactivate {
            nu_executable_sha256: paths.nu_executable_hash.clone(),
            nu_version: paths.nu_version.clone(),
            plugin_registry_path: paths.plugin_registry_path.clone(),
            created_at: format_timestamp(),
            entries: vec![PendingPluginDeactivateEntry {
                package_id: pkg_a.clone(),
                plugin_name: "nu_plugin_x".to_string(),
                absolute_binary_path: root
                    .join("packages/plugins/owner/a/1.0.0-abc/nu_plugin_x")
                    .to_string_lossy()
                    .into_owned(),
                status: PluginDeactivateStatus::Unregistered,
                error: None,
            }],
        }
        .save(root)
        .unwrap();

        let unregistrar_calls = Cell::new(0usize);
        deactivate_one_plugin(root, &pkg_a, &|_nu, _name, _cfg| {
            unregistrar_calls.set(unregistrar_calls.get() + 1);
            bail!("unregistrar must not run after Unregistered reconcile")
        })
        .unwrap();

        assert_eq!(
            unregistrar_calls.get(),
            0,
            "must not re-unregister after reconcile"
        );
        assert!(Lockfile::load(root).unwrap().packages[&pkg_a]
            .activation
            .is_none());
        assert!(PendingPluginDeactivate::load(root).unwrap().is_none());
    }
}
