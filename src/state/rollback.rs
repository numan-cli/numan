//! Journaled rollback of Numan-owned state to an immutable snapshot.
//!
//! Rollback restores exactly the state captured in a snapshot: the lockfile,
//! the managed module-autoload file (`numan.nu`), the autoload-state
//! projection, the nupm-import provenance sidecar, and the Nu path cache
//! (`nu_state/paths.json`) when the snapshot captured it. It never re-solves
//! against a registry, never substitutes compatible versions, never moves Nu
//! binaries or rewrites shell PATH, and never rewrites files Numan does not
//! own.
//!
//! The operation is journaled through `PendingLifecycle` with dedicated
//! rollback stages. Every commit step is an atomic file write, and re-running
//! the rollback against the same snapshot is idempotent, so recovery from an
//! interruption at any stage is: run the same rollback again.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::nu::autoload::{validate_candidate, write_candidate, CandidateRunner};
use crate::nu::paths::NuPaths;
use crate::state::autoload_state::AutoloadState;
use crate::state::lifecycle_journal::{LifecycleOp, LifecycleStage, PendingLifecycle};
use crate::state::snapshot::{
    create_snapshot, load_snapshot, verify_payloads, ManagedAutoloadProjection, Snapshot,
    SnapshotPaths, SnapshotReason, SnapshotRelation, SnapshotSidecar, SnapshotTrigger,
};

/// Outcome summary of a completed rollback.
#[derive(Debug)]
pub struct RollbackReport {
    /// Snapshot that was restored.
    pub target_snapshot_id: String,
    /// Snapshot of the pre-rollback state, for undoing the rollback itself.
    pub pre_rollback_snapshot_id: String,
    /// Number of packages in the restored lockfile.
    pub packages_restored: usize,
    /// Human-readable description of what happened to the managed autoload file.
    pub autoload_action: String,
}

/// Restore Numan-owned state to exactly the state captured in snapshot `id`.
///
/// Preconditions checked before any mutation:
/// - No unrelated in-flight lifecycle journal exists (an interrupted rollback
///   to the same snapshot may be resumed).
/// - The snapshot loads and all sidecar digests verify.
/// - Every payload referenced by the snapshot lockfile exists on disk with the
///   exact recorded revision hash — a missing or drifted payload refuses with a
///   precise remediation list instead of approximating.
/// - The snapshot was taken for this Numan root (autoload content embeds
///   absolute paths).
/// - If the snapshot captured a managed autoload file, the current Nu identity
///   (from the live path cache) must match the identity recorded in the
///   snapshot. Paths restore runs after this check and does not relax it:
///   restoring `paths.json` does not move Nu binaries, so an autoload graph
///   validated against a different Nu still cannot be restored.
///
/// The caller must hold the root mutation lock.
pub fn rollback_to_snapshot(
    root: &Path,
    id: &str,
    runner: &dyn CandidateRunner,
) -> Result<RollbackReport> {
    // Refuse when a different operation is in flight. A rollback journal
    // targeting the same snapshot is resumed by re-running from the start:
    // every commit step is an atomic overwrite driven only by immutable
    // snapshot content, so re-execution converges to the same state.
    if let Some(journal) = PendingLifecycle::load(root)? {
        let resumable = matches!(journal.op, LifecycleOp::Rollback)
            && journal.target_snapshot_id.as_deref() == Some(id);
        if !resumable {
            bail!(
                "A previous operation on '{}' was interrupted and left a lifecycle journal. \
                 Complete or clear it (see 'numan gc') before rolling back.",
                journal.package_id
            );
        }
    }

    let snapshot = load_snapshot(root, id)?;

    // Exact-payload precondition: never approximate a rollback.
    let payload_errors = verify_payloads(
        root,
        &snapshot.lockfile,
        &snapshot.manifest.payload_revisions,
    )?;
    if !payload_errors.is_empty() {
        bail!(
            "Cannot roll back to snapshot '{}': required immutable payloads are \
             missing or corrupted:\n  {}\n\
             Reinstall the exact package revisions listed above, then retry. \
             Numan will not substitute different artifacts during rollback.",
            id,
            payload_errors.join("\n  ")
        );
    }

    // Root identity: snapshot autoload content embeds absolute paths under the
    // root it was created for.
    let current_root = root
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize Numan root '{}'", root.display()))?;
    if current_root.to_string_lossy() != snapshot.manifest.numan_root {
        bail!(
            "Snapshot '{}' was created for Numan root '{}', but the current root is '{}'. \
             Rollback across roots is not supported; re-activate packages instead.",
            id,
            snapshot.manifest.numan_root,
            current_root.display()
        );
    }

    // Nu identity: restoring a managed autoload file generated for a different
    // Nu binary would produce an unvalidated activation graph.
    if let ManagedAutoloadProjection::Present {
        nu_executable_sha256,
        nu_version,
        ..
    } = &snapshot.autoload.projection
    {
        let nu_paths = NuPaths::load(root).context(
            "Snapshot contains a managed autoload file but the Nu path cache is unavailable. \
             Run 'numan init' first.",
        )?;
        if &nu_paths.nu_executable_hash != nu_executable_sha256 {
            bail!(
                "Snapshot '{}' captured an activation graph for Nu {} (executable hash {}), \
                 but the current Nu identity differs. Rollback would restore a graph \
                 validated against a different Nu binary. Re-activate packages under the \
                 current Nu instead.",
                id,
                nu_version,
                &nu_executable_sha256[..12.min(nu_executable_sha256.len())]
            );
        }
    }

    // Journal: prepared.
    let mut journal = PendingLifecycle {
        op: LifecycleOp::Rollback,
        package_id: format!("snapshot:{id}"),
        stage: LifecycleStage::RollbackPrepared,
        orphan_payload_path: None,
        from_version: None,
        to_version: None,
        nupm_source_path: None,
        nupm_metadata_sha256: None,
        staging_dir: None,
        promoted_payload_path: None,
        batch_package_ids: Vec::new(),
        batch_staging_dirs: Vec::new(),
        target_snapshot_id: Some(id.to_string()),
        pre_rollback_snapshot_id: None,
        needs_reactivate: false,
    };
    journal.save(root)?;

    // Snapshot the current state so the rollback itself is reversible.
    let pre_rollback = create_snapshot(
        root,
        SnapshotReason::PreRollback,
        SnapshotTrigger::Rollback,
        Some(id.to_string()),
        Some(SnapshotRelation::PreRollbackOf),
    )?;
    journal.pre_rollback_snapshot_id = Some(pre_rollback.id.clone());
    journal.stage = LifecycleStage::CurrentStateSnapshotted;
    journal.save(root)?;

    // Stage and validate the candidate autoload file before committing anything.
    let staged_candidate = match &snapshot.autoload.projection {
        ManagedAutoloadProjection::Present {
            managed_file_path,
            content,
            ..
        } => {
            let managed_path = Path::new(managed_file_path);
            if let Some(parent) = managed_path.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create vendor-autoload directory '{}'",
                        parent.display()
                    )
                })?;
            }
            let candidate = write_candidate(managed_path, content)?;
            journal.stage = LifecycleStage::CandidateStaged;
            journal.save(root)?;

            // Syntax-validate the restored content with the current Nu before
            // committing. Removes the candidate on failure.
            validate_candidate(&candidate, runner, &[&format!("snapshot:{id}")])
                .context("Restored autoload content failed Nu validation")?;
            journal.stage = LifecycleStage::CandidateValidated;
            journal.save(root)?;
            Some((candidate, managed_path.to_path_buf()))
        }
        _ => None,
    };

    // Commit 1: lockfile.
    snapshot.lockfile.save(root)?;
    journal.stage = LifecycleStage::LockfileCommitted;
    journal.save(root)?;

    // Commit 2: managed autoload file.
    let autoload_action = commit_autoload_file(&snapshot, staged_candidate)?;
    journal.stage = LifecycleStage::AutoloadCommitted;
    journal.save(root)?;

    // Commit 3: autoload-state projection.
    match &snapshot.autoload.state_sidecar {
        SnapshotSidecar::Present { value, .. } => value.save(root)?,
        SnapshotSidecar::Absent => AutoloadState::delete(root)?,
    }
    journal.stage = LifecycleStage::AutoloadStateCommitted;
    journal.save(root)?;

    // Commit 4: nupm-imports sidecar.
    match &snapshot.imports {
        Some(imports) => imports.save(root)?,
        None => {
            let imports_path = root.join("state/nupm-imports.json");
            if imports_path.exists() {
                std::fs::remove_file(&imports_path)
                    .with_context(|| format!("Failed to remove '{}'", imports_path.display()))?;
            }
        }
    }
    journal.stage = LifecycleStage::ImportsCommitted;
    journal.save(root)?;

    // Commit 5: Nu path cache. Legacy snapshots (`paths == None`) leave the
    // live cache untouched. Empty lockfiles / no activations are supported:
    // paths restore does not depend on package state.
    match &snapshot.paths {
        None => {}
        Some(SnapshotPaths::Present(paths)) => {
            std::fs::create_dir_all(root.join("nu_state")).with_context(|| {
                format!(
                    "Failed to create nu_state directory under '{}'",
                    root.display()
                )
            })?;
            paths.save(root)?;
        }
        Some(SnapshotPaths::Absent) => {
            let paths_path = root.join("nu_state/paths.json");
            if paths_path.exists() {
                std::fs::remove_file(&paths_path)
                    .with_context(|| format!("Failed to remove '{}'", paths_path.display()))?;
            }
        }
    }
    journal.stage = LifecycleStage::PathsCommitted;
    journal.save(root)?;

    journal.stage = LifecycleStage::Completed;
    journal.save(root)?;
    PendingLifecycle::clear(root)?;

    Ok(RollbackReport {
        target_snapshot_id: id.to_string(),
        pre_rollback_snapshot_id: pre_rollback.id,
        packages_restored: snapshot.lockfile.packages.len(),
        autoload_action,
    })
}

/// Commit the managed autoload file to match the snapshot projection.
///
/// Only Numan-owned files are ever replaced or deleted; ownership is verified
/// by `replace_managed_file` / `delete_managed_file` before any write.
fn commit_autoload_file(
    snapshot: &Snapshot,
    staged_candidate: Option<(std::path::PathBuf, std::path::PathBuf)>,
) -> Result<String> {
    match &snapshot.autoload.projection {
        ManagedAutoloadProjection::Present { .. } => {
            let (candidate, managed_path) =
                staged_candidate.expect("candidate staged for Present projection");
            crate::nu::autoload::replace_managed_file(&managed_path, &candidate)?;
            Ok(format!("restored {}", managed_path.display()))
        }
        ManagedAutoloadProjection::Absent { managed_file_path } => {
            let managed_path = Path::new(managed_file_path);
            if managed_path.exists() {
                crate::nu::autoload::delete_managed_file(managed_path)?;
                Ok(format!("removed {}", managed_path.display()))
            } else {
                Ok("no managed autoload file (already absent)".to_string())
            }
        }
        ManagedAutoloadProjection::NotConfigured => {
            Ok("no managed autoload file (vendor autoload not configured)".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nu::autoload::FakeCandidateRunner;
    use crate::state::lockfile::Lockfile;
    use crate::state::snapshot::create_snapshot;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("state")).unwrap();
        dir
    }

    fn entry_for(payload_rel: &str) -> crate::state::lockfile::LockfileEntry {
        crate::state::lockfile::LockfileEntry {
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
            payload_path: payload_rel.to_string(),
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

    fn install_fake_package(root: &Path, id: &str, version_dir: &str, content: &str) {
        let payload_rel = format!("packages/modules/{id}/{version_dir}");
        let payload = root.join(&payload_rel);
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("mod.nu"), content).unwrap();
        let mut lockfile = Lockfile::load(root).unwrap();
        lockfile
            .packages
            .insert(id.to_string(), entry_for(&payload_rel));
        lockfile.save(root).unwrap();
    }

    #[test]
    fn remove_then_rollback_restores_lockfile() {
        let dir = make_root();
        let root = dir.path();

        install_fake_package(root, "owner/pkg", "1.0.0-abc12345", "# v1");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Remove,
            None,
            None,
        )
        .unwrap();

        // Simulate remove: drop from lockfile (payload dir intentionally kept —
        // GC treats snapshot references as live roots).
        let mut lockfile = Lockfile::load(root).unwrap();
        lockfile.packages.remove("owner/pkg");
        lockfile.save(root).unwrap();
        assert!(Lockfile::load(root).unwrap().is_empty());

        let runner = FakeCandidateRunner::success();
        let report = rollback_to_snapshot(root, &snap.id, &runner).unwrap();
        assert_eq!(report.packages_restored, 1);

        let restored = Lockfile::load(root).unwrap();
        assert!(restored.packages.contains_key("owner/pkg"));
        assert!(PendingLifecycle::load(root).unwrap().is_none());
    }

    #[test]
    fn update_then_rollback_restores_previous_version() {
        let dir = make_root();
        let root = dir.path();

        install_fake_package(root, "owner/pkg", "1.0.0-abc12345", "# v1");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Update,
            None,
            None,
        )
        .unwrap();

        // Simulate update to 2.0.0 (old payload kept on disk).
        install_fake_package(root, "owner/pkg", "2.0.0-def67890", "# v2");
        let mut lockfile = Lockfile::load(root).unwrap();
        lockfile.packages.get_mut("owner/pkg").unwrap().version = "2.0.0".to_string();
        lockfile.save(root).unwrap();

        let runner = FakeCandidateRunner::success();
        rollback_to_snapshot(root, &snap.id, &runner).unwrap();

        let restored = Lockfile::load(root).unwrap();
        let entry = restored.packages.get("owner/pkg").unwrap();
        assert_eq!(entry.version, "1.0.0");
        assert!(entry.payload_path.contains("1.0.0-abc12345"));
    }

    #[test]
    fn rollback_refuses_missing_payload() {
        let dir = make_root();
        let root = dir.path();

        install_fake_package(root, "owner/pkg", "1.0.0-abc12345", "# v1");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Remove,
            None,
            None,
        )
        .unwrap();

        // Destroy the payload the snapshot depends on.
        std::fs::remove_dir_all(root.join("packages/modules/owner/pkg/1.0.0-abc12345")).unwrap();

        let runner = FakeCandidateRunner::success();
        let err = rollback_to_snapshot(root, &snap.id, &runner).unwrap_err();
        assert!(err.to_string().contains("missing or corrupted"), "{err}");
        // No journal left behind; nothing was mutated.
        assert!(PendingLifecycle::load(root).unwrap().is_none());
    }

    #[test]
    fn rollback_refuses_drifted_payload() {
        let dir = make_root();
        let root = dir.path();

        install_fake_package(root, "owner/pkg", "1.0.0-abc12345", "# v1");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Remove,
            None,
            None,
        )
        .unwrap();

        // Corrupt the payload content — revision hash no longer matches.
        std::fs::write(
            root.join("packages/modules/owner/pkg/1.0.0-abc12345/mod.nu"),
            "# tampered",
        )
        .unwrap();

        let runner = FakeCandidateRunner::success();
        let err = rollback_to_snapshot(root, &snap.id, &runner).unwrap_err();
        assert!(err.to_string().contains("missing or corrupted"), "{err}");
    }

    #[test]
    fn rollback_refuses_unrelated_pending_journal() {
        let dir = make_root();
        let root = dir.path();

        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        let journal = PendingLifecycle {
            op: LifecycleOp::Remove,
            package_id: "owner/other".to_string(),
            stage: LifecycleStage::Prepared,
            orphan_payload_path: None,
            from_version: None,
            to_version: None,
            nupm_source_path: None,
            nupm_metadata_sha256: None,
            staging_dir: None,
            promoted_payload_path: None,
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            target_snapshot_id: None,
            pre_rollback_snapshot_id: None,
            needs_reactivate: false,
        };
        journal.save(root).unwrap();

        let runner = FakeCandidateRunner::success();
        let err = rollback_to_snapshot(root, &snap.id, &runner).unwrap_err();
        assert!(err.to_string().contains("interrupted"), "{err}");
    }

    #[test]
    fn interrupted_rollback_is_resumable() {
        let dir = make_root();
        let root = dir.path();

        install_fake_package(root, "owner/pkg", "1.0.0-abc12345", "# v1");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Remove,
            None,
            None,
        )
        .unwrap();

        let mut lockfile = Lockfile::load(root).unwrap();
        lockfile.packages.remove("owner/pkg");
        lockfile.save(root).unwrap();

        // Simulate a crash mid-rollback: journal exists at an intermediate stage
        // targeting this snapshot.
        let journal = PendingLifecycle {
            op: LifecycleOp::Rollback,
            package_id: format!("snapshot:{}", snap.id),
            stage: LifecycleStage::LockfileCommitted,
            orphan_payload_path: None,
            from_version: None,
            to_version: None,
            nupm_source_path: None,
            nupm_metadata_sha256: None,
            staging_dir: None,
            promoted_payload_path: None,
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            target_snapshot_id: Some(snap.id.clone()),
            pre_rollback_snapshot_id: None,
            needs_reactivate: false,
        };
        journal.save(root).unwrap();

        // Re-running the rollback to the same snapshot resumes and completes.
        let runner = FakeCandidateRunner::success();
        rollback_to_snapshot(root, &snap.id, &runner).unwrap();
        assert!(Lockfile::load(root)
            .unwrap()
            .packages
            .contains_key("owner/pkg"));
        assert!(PendingLifecycle::load(root).unwrap().is_none());
    }

    #[test]
    fn rollback_restores_imports_sidecar_absence() {
        let dir = make_root();
        let root = dir.path();

        // Snapshot taken with no imports sidecar.
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        // An import happens afterwards.
        let mut imports = crate::state::nupm_import::NupmImportsFile::empty();
        imports.upsert(
            "owner/pkg",
            crate::state::nupm_import::NupmImportRecord {
                trust_level: "local".to_string(),
                nupm_source_path: "/src".to_string(),
                nupm_metadata_path: "/src/nupm.nuon".to_string(),
                nupm_metadata_sha256: "a".to_string(),
                source_payload_sha256: "b".to_string(),
                imported_payload_sha256: "c".to_string(),
                observed_git_remote: None,
                observed_git_commit: None,
                imported_at: "0".to_string(),
            },
        );
        imports.save(root).unwrap();
        assert!(root.join("state/nupm-imports.json").exists());

        let runner = FakeCandidateRunner::success();
        rollback_to_snapshot(root, &snap.id, &runner).unwrap();
        assert!(!root.join("state/nupm-imports.json").exists());
    }

    #[test]
    fn rollback_creates_pre_rollback_snapshot() {
        let dir = make_root();
        let root = dir.path();

        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        let runner = FakeCandidateRunner::success();
        let report = rollback_to_snapshot(root, &snap.id, &runner).unwrap();

        let pre =
            crate::state::snapshot::load_snapshot(root, &report.pre_rollback_snapshot_id).unwrap();
        assert_eq!(pre.manifest.reason, SnapshotReason::PreRollback);
        assert_eq!(
            pre.manifest.related_snapshot_id.as_deref(),
            Some(snap.id.as_str())
        );
    }

    /// Build a minimal `NuPaths` + vendor-autoload dir + Numan-owned managed
    /// file + lockfile module activation, so `create_snapshot` captures a
    /// `Present` autoload projection. Returns the managed file path.
    fn setup_module_activation(root: &Path, nu_hash: &str, content_suffix: &str) -> PathBuf {
        let vendor_dir = root.join("nu_vendor_autoload");
        std::fs::create_dir_all(&vendor_dir).unwrap();
        let managed_path = vendor_dir.join("numan.nu");
        let vendor_dir_str = vendor_dir.to_string_lossy().to_string();
        let managed_path_str = managed_path.to_string_lossy().to_string();

        std::fs::write(
            &managed_path,
            format!(
                "{}{}",
                crate::util::fs_safety::OWNERSHIP_MARKER,
                content_suffix
            ),
        )
        .unwrap();

        let nu_paths = crate::nu::paths::NuPaths {
            nu_executable: root.join("fake-nu").to_string_lossy().to_string(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: root.join("plugin.msgpackz").to_string_lossy().to_string(),
            nu_executable_hash: nu_hash.to_string(),
            platform: "x86_64-linux-gnu".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![vendor_dir_str.clone()],
            vendor_autoload_dir: Some(vendor_dir_str.clone()),
        };
        nu_paths.save(root).unwrap();

        install_fake_package(root, "owner/mod", "1.0.0-abc12345", "# module");
        let mut lockfile = Lockfile::load(root).unwrap();
        let entry = lockfile.packages.get_mut("owner/mod").unwrap();
        entry.module_activation = Some(crate::state::lockfile::ModuleActivation {
            entry_path: root
                .join("packages/modules/owner/mod/1.0.0-abc12345/mod.nu")
                .to_string_lossy()
                .to_string(),
            import_mode: crate::core::package::ModuleImportMode::Module,
            vendor_autoload_dir: vendor_dir_str,
            managed_file_path: managed_path_str,
            nu_executable_sha256: nu_hash.to_string(),
            nu_version: "0.113.1".to_string(),
            activated_at: "0".to_string(),
        });
        entry.module_import_mode = Some(crate::core::package::ModuleImportMode::Module);
        lockfile.save(root).unwrap();

        managed_path
    }

    #[test]
    fn rollback_restores_managed_autoload_file_content() {
        let dir = make_root();
        let root = dir.path();

        let managed_path = setup_module_activation(root, "hash-1", "use \"v1\"\n");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Deactivate,
            None,
            None,
        )
        .unwrap();

        // Simulate a later mutation: managed file content changes (still
        // Numan-owned) and the module is deactivated in the lockfile.
        std::fs::write(
            &managed_path,
            format!("{}use \"v2\"\n", crate::util::fs_safety::OWNERSHIP_MARKER),
        )
        .unwrap();
        let mut lockfile = Lockfile::load(root).unwrap();
        lockfile
            .packages
            .get_mut("owner/mod")
            .unwrap()
            .module_activation = None;
        lockfile.save(root).unwrap();

        let runner = FakeCandidateRunner::success();
        let report = rollback_to_snapshot(root, &snap.id, &runner).unwrap();
        assert!(report.autoload_action.contains("restored"));

        let restored_content = std::fs::read_to_string(&managed_path).unwrap();
        assert!(restored_content.contains("v1"));
        assert!(!restored_content.contains("v2"));
    }

    #[test]
    fn rollback_refuses_to_overwrite_non_numan_owned_file() {
        let dir = make_root();
        let root = dir.path();

        let managed_path = setup_module_activation(root, "hash-1", "use \"v1\"\n");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Deactivate,
            None,
            None,
        )
        .unwrap();

        // The user replaces the managed file with their own hand-written
        // content that lacks the Numan ownership marker.
        std::fs::write(&managed_path, "# hand-edited by user\n").unwrap();

        let runner = FakeCandidateRunner::success();
        let err = rollback_to_snapshot(root, &snap.id, &runner).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("owned")
                || err.to_string().to_lowercase().contains("ownership"),
            "{err}"
        );

        // The user's file must be untouched.
        let content = std::fs::read_to_string(&managed_path).unwrap();
        assert_eq!(content, "# hand-edited by user\n");
    }

    #[test]
    fn rollback_refuses_nu_identity_mismatch_for_module_activation() {
        let dir = make_root();
        let root = dir.path();

        setup_module_activation(root, "hash-1", "use \"v1\"\n");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Deactivate,
            None,
            None,
        )
        .unwrap();

        // Nu was upgraded — the cached identity no longer matches what the
        // snapshot's autoload content was validated against.
        let mut nu_paths = crate::nu::paths::NuPaths::load(root).unwrap();
        nu_paths.nu_executable_hash = "hash-2".to_string();
        nu_paths.save(root).unwrap();

        let runner = FakeCandidateRunner::success();
        let err = rollback_to_snapshot(root, &snap.id, &runner).unwrap_err();
        assert!(err.to_string().contains("Nu identity"), "{err}");
        assert!(PendingLifecycle::load(root).unwrap().is_none());
    }

    #[test]
    fn rollback_refuses_root_mismatch() {
        let dir = make_root();
        let root = dir.path();

        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        // Rewrite the manifest with a different recorded root. Digest checks
        // only cover sidecars, not the manifest itself, so this simulates a
        // snapshot created under another root.
        let mut manifest = crate::state::snapshot::load_manifest(root, &snap.id).unwrap();
        manifest.numan_root = PathBuf::from("/somewhere/else").display().to_string();
        crate::util::atomic::write_json_atomic(
            &root.join(format!("state/snapshots/{}/snapshot.json", snap.id)),
            &manifest,
        )
        .unwrap();

        let runner = FakeCandidateRunner::success();
        let err = rollback_to_snapshot(root, &snap.id, &runner).unwrap_err();
        assert!(err.to_string().contains("current root"), "{err}");
    }

    fn save_fake_paths(root: &Path, hash: &str, version: &str) {
        let nu_paths = crate::nu::paths::NuPaths {
            nu_executable: root.join("fake-nu").to_string_lossy().to_string(),
            nu_version: version.to_string(),
            plugin_registry_path: root.join("plugin.msgpackz").to_string_lossy().to_string(),
            nu_executable_hash: hash.to_string(),
            platform: "x86_64-linux-gnu".to_string(),
            data_dir: None,
            vendor_autoload_dirs: Vec::new(),
            vendor_autoload_dir: None,
        };
        nu_paths.save(root).unwrap();
    }

    #[test]
    fn rollback_restores_paths_with_empty_lockfile() {
        let dir = make_root();
        let root = dir.path();

        // No packages; only a Nu path cache.
        save_fake_paths(root, "hash-before", "0.113.1");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        // Simulate init --refresh rewriting paths.json while the activation
        // graph stays empty.
        save_fake_paths(root, "hash-after", "0.120.0");
        let after = crate::nu::paths::NuPaths::load(root).unwrap();
        assert_eq!(after.nu_executable_hash, "hash-after");

        let runner = FakeCandidateRunner::success();
        rollback_to_snapshot(root, &snap.id, &runner).unwrap();

        let restored = crate::nu::paths::NuPaths::load(root).unwrap();
        assert_eq!(restored.nu_executable_hash, "hash-before");
        assert_eq!(restored.nu_version, "0.113.1");
        assert!(Lockfile::load(root).unwrap().is_empty());
        assert!(PendingLifecycle::load(root).unwrap().is_none());
    }

    #[test]
    fn rollback_deletes_paths_when_snapshot_had_absent() {
        let dir = make_root();
        let root = dir.path();

        // Snapshot while uninitialized (no paths.json).
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();
        assert!(!root.join("nu_state/paths.json").exists());

        // Paths appear later (e.g. numan init).
        save_fake_paths(root, "hash-new", "0.113.1");
        assert!(root.join("nu_state/paths.json").exists());

        let runner = FakeCandidateRunner::success();
        rollback_to_snapshot(root, &snap.id, &runner).unwrap();
        assert!(
            !root.join("nu_state/paths.json").exists(),
            "rollback must remove paths.json when the snapshot recorded Absent"
        );
    }

    #[test]
    fn refresh_then_rollback_restores_pre_refresh_paths() {
        // Mirrors `numan init --refresh` with no active packages: PreMutation
        // Init snapshot, rewrite paths.json, then rollback.
        let dir = make_root();
        let root = dir.path();

        save_fake_paths(root, "hash-before", "0.113.1");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Init,
            None,
            None,
        )
        .unwrap();
        assert_eq!(snap.trigger, SnapshotTrigger::Init);

        save_fake_paths(root, "hash-after", "0.120.0");
        let runner = FakeCandidateRunner::success();
        rollback_to_snapshot(root, &snap.id, &runner).unwrap();

        let restored = crate::nu::paths::NuPaths::load(root).unwrap();
        assert_eq!(restored.nu_executable_hash, "hash-before");
        assert_eq!(restored.nu_version, "0.113.1");
    }

    #[test]
    fn rollback_leaves_paths_alone_for_legacy_snapshots() {
        let dir = make_root();
        let root = dir.path();

        save_fake_paths(root, "hash-original", "0.113.1");
        let snap = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        // Strip paths capture to simulate a pre-sidecar snapshot.
        let snap_dir = root.join(format!("state/snapshots/{}", snap.id));
        let paths_sidecar = snap_dir.join("paths.json");
        if paths_sidecar.exists() {
            std::fs::remove_file(&paths_sidecar).unwrap();
        }
        let mut manifest = crate::state::snapshot::load_manifest(root, &snap.id).unwrap();
        manifest.sidecar_digests.paths_sha256 = None;
        crate::util::atomic::write_json_atomic(&snap_dir.join("snapshot.json"), &manifest).unwrap();

        // Mutate live paths; legacy rollback must not touch them.
        save_fake_paths(root, "hash-mutated", "0.120.0");

        let runner = FakeCandidateRunner::success();
        rollback_to_snapshot(root, &snap.id, &runner).unwrap();

        let current = crate::nu::paths::NuPaths::load(root).unwrap();
        assert_eq!(current.nu_executable_hash, "hash-mutated");
        assert_eq!(current.nu_version, "0.120.0");
    }
}
