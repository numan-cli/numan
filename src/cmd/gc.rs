use anyhow::Result;
use clap::Parser;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::state::lifecycle_journal::{check_stale_journal, PendingLifecycle};
use crate::state::lockfile::Lockfile;
use crate::state::overlay_state::OverlayState;
use crate::state::snapshot::{legacy_snapshot_payload_paths, list_snapshots, load_snapshot};
use crate::util::fs_safety::acquire_mutation_lock;
use crate::util::output::{eprintln, println};

/// Garbage-collect orphaned package directories
#[derive(Parser)]
pub struct GcArgs {
    /// Report what would be removed without deleting anything
    #[arg(long)]
    dry_run: bool,
}

pub fn execute(args: &GcArgs, root: &Path) -> Result<()> {
    if let Some(journal) = check_stale_journal(root)? {
        eprintln!(
            "Note: Cleaning up after interrupted '{}' operation on '{}'.",
            match journal.op {
                crate::state::lifecycle_journal::LifecycleOp::Update => "update",
                crate::state::lifecycle_journal::LifecycleOp::Remove => "remove",
                crate::state::lifecycle_journal::LifecycleOp::NupmImport => "nupm import",
                crate::state::lifecycle_journal::LifecycleOp::NupmImportManifest => {
                    "nupm manifest import"
                }
                crate::state::lifecycle_journal::LifecycleOp::Rollback => "snapshot rollback",
            },
            journal.package_id
        );
    }

    let _lock = acquire_mutation_lock(root)?;

    let lockfile = Lockfile::load(root)?;

    // Build absolute paths of all referenced payload directories: the current
    // lockfile, every committed snapshot's lockfile, and legacy timestamp-only
    // snapshots. Snapshots are live roots — a payload still referenced by a
    // rollback target must survive GC even after it's been superseded in the
    // current lockfile.
    let mut referenced: HashSet<PathBuf> = lockfile
        .packages
        .values()
        .map(|e| root.join(e.payload_path()))
        .collect();

    for manifest in list_snapshots(root).unwrap_or_default() {
        if let Ok(snap) = load_snapshot(root, &manifest.id) {
            referenced.extend(
                snap.lockfile
                    .packages
                    .values()
                    .map(|e| root.join(e.payload_path())),
            );
        }
    }
    referenced.extend(legacy_snapshot_payload_paths(root).unwrap_or_default());
    // Session reservations remain live roots even after the lockfile changes.
    // Invalid overlay state must abort GC rather than discard its protection.
    referenced.extend(OverlayState::referenced_payloads(root)?);
    let referenced: HashSet<PathBuf> = referenced
        .into_iter()
        .map(|path| path.canonicalize().unwrap_or(path))
        .collect();

    let packages_dir = root.join("packages");
    if !packages_dir.exists() {
        println!("Nothing to collect.");
        PendingLifecycle::clear(root)?;
        return Ok(());
    }

    // Collect all on-disk version directories.
    let mut candidates: Vec<PathBuf> = Vec::new();
    collect_version_dirs(&packages_dir, &mut candidates);

    let orphans: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|p| !referenced.contains(&p.canonicalize().unwrap_or_else(|_| p.clone())))
        .collect();

    if orphans.is_empty() {
        println!("No orphaned packages found.");
        PendingLifecycle::clear(root)?;
        return Ok(());
    }

    if args.dry_run {
        println!("Would remove {} orphaned package(s):", orphans.len());
        for p in &orphans {
            println!("  {}", p.display());
        }
        return Ok(());
    }

    println!("Removing {} orphaned package(s)...", orphans.len());
    let mut removed = 0usize;
    for p in &orphans {
        match std::fs::remove_dir_all(p) {
            Ok(_) => {
                removed += 1;
                println!("  removed {}", p.display());
            }
            Err(e) => eprintln!("  warning: could not remove {}: {}", p.display(), e),
        }
    }

    println!(
        "{} Removed {} orphaned package(s).",
        console::style("✓").green(),
        removed
    );

    PendingLifecycle::clear(root)?;

    Ok(())
}

/// Walk `packages/<type>/<owner>/<name>/<version-sha>` and collect the leaf
/// version directories. Errors at any level are silently skipped.
fn collect_version_dirs(packages_dir: &Path, result: &mut Vec<PathBuf>) {
    for type_dir in read_dir_ok(packages_dir) {
        for owner_dir in read_dir_ok(&type_dir) {
            for name_dir in read_dir_ok(&owner_dir) {
                for version_dir in read_dir_ok(&name_dir) {
                    if version_dir.is_dir() {
                        result.push(version_dir);
                    }
                }
            }
        }
    }
}

fn read_dir_ok(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::lockfile::Lockfile;
    use std::collections::BTreeMap;

    #[test]
    fn gc_preserves_overlay_reservations_without_lockfile_entries() {
        for status in ["pending", "active", "degraded"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let payload = root.join("packages/modules/owner/pkg/1.0.0-abc12345");
            std::fs::create_dir_all(&payload).unwrap();
            let entry = payload.join("mod.nu");
            std::fs::write(&entry, "export def hello [] { 'hello' }").unwrap();
            let orphan = root.join("packages/modules/owner/unused/1.0.0-def67890");
            std::fs::create_dir_all(&orphan).unwrap();
            crate::util::atomic::write_json_atomic(
                &root.join("nu_state/overlays.json"),
                &serde_json::json!({"overlays": [{
                    "package_id": "owner/pkg", "overlay_name": "pkg",
                    "entry_path": entry.canonicalize().unwrap(), "scope": "session",
                    "activated_at": "2026-10-22T00:00:00Z",
                    "session_id": "test-session", "pid": 42,
                    "txn": "test-transaction", "status": status
                }]}),
            )
            .unwrap();

            execute(&GcArgs { dry_run: false }, root).unwrap();

            assert!(entry.is_file(), "{status} overlay payload must survive GC");
            assert!(
                !orphan.exists(),
                "unreserved orphan must still be collected"
            );
        }
    }

    #[test]
    fn gc_rejects_invalid_overlay_state_before_deleting_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let orphan = root.join("packages/modules/owner/pkg/1.0.0-abc12345");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        std::fs::write(root.join("nu_state/overlays.json"), "invalid JSON").unwrap();

        assert!(execute(&GcArgs { dry_run: false }, root).is_err());
        assert!(orphan.is_dir());
    }

    #[test]
    fn gc_finds_no_orphans_when_all_referenced() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a version directory matching the lockfile entry.
        let version_dir = root
            .join("packages")
            .join("modules")
            .join("owner")
            .join("pkg")
            .join("1.0.0-abc12345");
        std::fs::create_dir_all(&version_dir).unwrap();

        let mut lockfile = Lockfile::empty();
        lockfile.packages.insert(
            "owner/pkg".to_string(),
            crate::state::lockfile::LockfileEntry {
                hook_activation: None,
                hook_bindings: None,
                version: "1.0.0".to_string(),
                package_type: "module".to_string(),
                source: "archive".to_string(),
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
                payload_path: "packages/modules/owner/pkg/1.0.0-abc12345".to_string(),
                revision_id: None,
                payload_sha256: None,
                executable_sha256: None,
                selection_reason: None,
                origin: None,
                module_activation: None,
                module_import_mode: None,
                overlay: None,
                script_activation: None,
                script_entry_points: None,
                locked_dependencies: BTreeMap::new(),
            },
        );
        lockfile.save(root).unwrap();

        let referenced: HashSet<PathBuf> = lockfile
            .packages
            .values()
            .map(|e| root.join(e.payload_path()))
            .collect();

        let mut candidates = Vec::new();
        collect_version_dirs(&root.join("packages"), &mut candidates);

        let orphans: Vec<_> = candidates
            .into_iter()
            .filter(|p| !referenced.contains(p))
            .collect();

        assert!(orphans.is_empty(), "No orphans expected: {:?}", orphans);
    }

    #[test]
    fn gc_detects_unreferenced_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create two version dirs; only one is in the lockfile.
        let current_dir = root
            .join("packages")
            .join("modules")
            .join("owner")
            .join("pkg")
            .join("1.1.0-new12345");
        let orphan_dir = root
            .join("packages")
            .join("modules")
            .join("owner")
            .join("pkg")
            .join("1.0.0-old12345");
        std::fs::create_dir_all(&current_dir).unwrap();
        std::fs::create_dir_all(&orphan_dir).unwrap();

        let mut lockfile = Lockfile::empty();
        lockfile.packages.insert(
            "owner/pkg".to_string(),
            crate::state::lockfile::LockfileEntry {
                hook_activation: None,
                hook_bindings: None,
                version: "1.1.0".to_string(),
                package_type: "module".to_string(),
                source: "archive".to_string(),
                payload_path: "packages/modules/owner/pkg/1.1.0-new12345".to_string(),
                installed_at: "0".to_string(),
                target: None,
                artifact_url: None,
                artifact_sha256: None,
                executable_path: None,
                archive_root: None,
                include: None,
                entry: None,
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
                revision_id: None,
                payload_sha256: None,
                executable_sha256: None,
                selection_reason: None,
                origin: None,
                module_activation: None,
                module_import_mode: None,
                overlay: None,
                script_activation: None,
                script_entry_points: None,
                locked_dependencies: BTreeMap::new(),
            },
        );
        lockfile.save(root).unwrap();

        let referenced: HashSet<PathBuf> = lockfile
            .packages
            .values()
            .map(|e| root.join(e.payload_path()))
            .collect();

        let mut candidates = Vec::new();
        collect_version_dirs(&root.join("packages"), &mut candidates);

        let orphans: Vec<_> = candidates
            .into_iter()
            .filter(|p| !referenced.contains(p))
            .collect();

        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0], orphan_dir);
    }

    #[test]
    fn gc_preserves_payload_referenced_only_by_a_snapshot() {
        use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let old_payload = root.join("packages/modules/owner/pkg/1.0.0-abc12345");
        std::fs::create_dir_all(&old_payload).unwrap();
        std::fs::write(old_payload.join("mod.nu"), "# v1").unwrap();

        let mut lockfile = Lockfile::empty();
        lockfile.packages.insert(
            "owner/pkg".to_string(),
            crate::state::lockfile::LockfileEntry {
                hook_activation: None,
                hook_bindings: None,
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
                payload_path: "packages/modules/owner/pkg/1.0.0-abc12345".to_string(),
                revision_id: None,
                payload_sha256: None,
                executable_sha256: None,
                selection_reason: None,
                origin: None,
                module_activation: None,
                module_import_mode: None,
                overlay: None,
                script_activation: None,
                script_entry_points: None,
                locked_dependencies: BTreeMap::new(),
            },
        );
        lockfile.save(root).unwrap();

        // Snapshot the pre-update state, then simulate an update to 2.0.0 —
        // the 1.0.0 payload is no longer referenced by the current lockfile,
        // only by the snapshot.
        create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Update,
            None,
            None,
        )
        .unwrap();

        let new_payload = root.join("packages/modules/owner/pkg/2.0.0-def67890");
        std::fs::create_dir_all(&new_payload).unwrap();
        std::fs::write(new_payload.join("mod.nu"), "# v2").unwrap();
        let mut lockfile = Lockfile::load(root).unwrap();
        let entry = lockfile.packages.get_mut("owner/pkg").unwrap();
        entry.version = "2.0.0".to_string();
        entry.payload_path = "packages/modules/owner/pkg/2.0.0-def67890".to_string();
        lockfile.save(root).unwrap();

        execute(&GcArgs { dry_run: false }, root).unwrap();

        assert!(
            old_payload.exists(),
            "payload referenced only by a snapshot must survive GC"
        );
        assert!(new_payload.exists());
    }
}
