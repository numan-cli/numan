use anyhow::Result;
use clap::Subcommand;
use std::io::{IsTerminal, Write};
use std::path::Path;

use crate::nu::autoload::NuCandidateRunner;
use crate::nu::paths::NuPaths;
use crate::state::lockfile::Lockfile;
use crate::state::rollback::rollback_to_snapshot;
use crate::state::snapshot::{
    count_active_modules, count_active_plugins, delete_snapshot, list_snapshots, load_snapshot,
    verify_payloads, ManagedAutoloadProjection,
};
use crate::util::fs_safety::acquire_mutation_lock;

#[derive(Subcommand)]
pub enum SnapshotCommands {
    /// List all committed snapshots
    List,
    /// Show detailed contents of a snapshot before acting on it
    Inspect {
        /// Snapshot ID (UUIDv7)
        id: String,
    },
    /// Delete a snapshot
    Delete {
        /// Snapshot ID (UUIDv7)
        id: String,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Roll back Numan-managed state to exactly a stored snapshot
    Rollback {
        /// Snapshot ID (UUIDv7)
        id: String,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

pub fn execute(cmd: SnapshotCommands, root: &Path) -> Result<()> {
    match cmd {
        SnapshotCommands::List => list(root, &mut std::io::stdout()),
        SnapshotCommands::Inspect { id } => inspect(root, &id, &mut std::io::stdout()),
        SnapshotCommands::Delete { id, yes } => delete(root, &id, yes),
        SnapshotCommands::Rollback { id, yes } => rollback(root, &id, yes),
    }
}

fn list(root: &Path, out: &mut dyn Write) -> Result<()> {
    let snapshots = list_snapshots(root)?;
    if snapshots.is_empty() {
        writeln!(out, "No snapshots.")?;
        return Ok(());
    }

    writeln!(out, "Snapshots ({}):\n", snapshots.len())?;
    for s in &snapshots {
        let related = s
            .related_snapshot_id
            .as_deref()
            .map(|r| format!(" (of {r})"))
            .unwrap_or_default();
        writeln!(
            out,
            "  {}  {:?}  {:?}{}  {} package(s)  created {}",
            s.id,
            s.reason,
            s.trigger,
            related,
            s.payload_revisions.len(),
            s.created_at
        )?;
    }
    Ok(())
}

fn inspect(root: &Path, id: &str, out: &mut dyn Write) -> Result<()> {
    let snapshot = load_snapshot(root, id)?;
    let m = &snapshot.manifest;

    writeln!(out, "Snapshot {}", m.id)?;
    writeln!(out, "  created:  {}", m.created_at)?;
    writeln!(out, "  reason:   {:?}", m.reason)?;
    writeln!(out, "  trigger:  {:?}", m.trigger)?;
    if let Some(related) = &m.related_snapshot_id {
        writeln!(out, "  related:  {:?} of {}", m.relation, related)?;
    }
    writeln!(out, "  root:     {}", m.numan_root)?;
    writeln!(out, "  platform: {}", m.platform)?;
    if let Some(nu) = &m.nu_identity {
        writeln!(
            out,
            "  nu:       {} (executable sha256 {})",
            nu.nu_version,
            short_hash(&nu.nu_executable_sha256)
        )?;
    }

    writeln!(out, "\nGenerated-file digests:")?;
    writeln!(
        out,
        "  lockfile: {}",
        short_hash(&m.sidecar_digests.lockfile_sha256)
    )?;
    if let Some(h) = &m.sidecar_digests.autoload_sha256 {
        writeln!(out, "  autoload: {}", short_hash(h))?;
    }
    if let Some(h) = &m.sidecar_digests.imports_sha256 {
        writeln!(out, "  imports:  {}", short_hash(h))?;
    }
    if let Some(h) = &m.sidecar_digests.paths_sha256 {
        writeln!(out, "  paths:    {}", short_hash(h))?;
    }

    writeln!(
        out,
        "\nPayload provenance ({} package(s)):",
        m.payload_revisions.len()
    )?;
    for (pkg, rev) in &m.payload_revisions {
        writeln!(out, "  {}  revision {}", pkg, short_hash(rev))?;
    }

    match &snapshot.autoload.projection {
        ManagedAutoloadProjection::Present {
            managed_file_path,
            active_module_ids,
            ..
        } => {
            writeln!(
                out,
                "\nModule autoload: {} active module(s) via '{}'",
                active_module_ids.len(),
                managed_file_path
            )?;
            for id in active_module_ids {
                writeln!(out, "  {id}")?;
            }
        }
        ManagedAutoloadProjection::Absent { managed_file_path } => {
            writeln!(
                out,
                "\nModule autoload: none active (managed file '{managed_file_path}' absent)"
            )?;
        }
        ManagedAutoloadProjection::NotConfigured => {
            writeln!(out, "\nModule autoload: not configured at snapshot time")?;
        }
    }

    if let Some(nu) = &m.nu_identity {
        let plugin_count = count_active_plugins(&snapshot.lockfile, nu);
        writeln!(
            out,
            "Active plugins (matching snapshot Nu identity): {plugin_count}"
        )?;
    }
    let _ = count_active_modules(&snapshot.autoload); // exercised above via active_module_ids

    if let Some(imports) = &snapshot.imports {
        writeln!(
            out,
            "\nnupm import provenance ({} record(s)):",
            imports.imports.len()
        )?;
        for (pkg, rec) in &imports.imports {
            writeln!(
                out,
                "  {}  from {} (trust: {})",
                pkg, rec.nupm_source_path, rec.trust_level
            )?;
        }
    }

    match &snapshot.paths {
        Some(crate::state::snapshot::SnapshotPaths::Present(p)) => {
            writeln!(
                out,
                "\nNu path cache: {} (executable sha256 {})",
                p.nu_version,
                short_hash(&p.nu_executable_hash)
            )?;
            writeln!(out, "  executable: {}", p.nu_executable)?;
            writeln!(out, "  plugin registry: {}", p.plugin_registry_path)?;
        }
        Some(crate::state::snapshot::SnapshotPaths::Absent) => {
            writeln!(out, "\nNu path cache: absent at snapshot time")?;
        }
        None => {
            writeln!(out, "\nNu path cache: not captured (legacy snapshot)")?;
        }
    }

    writeln!(
        out,
        "\nAffected packages if rolled back (compared to current lockfile):"
    )?;
    let current = Lockfile::load(root)?;
    let mut any_change = false;
    for (pkg, snap_entry) in &snapshot.lockfile.packages {
        match current.packages.get(pkg) {
            None => {
                writeln!(
                    out,
                    "  + {pkg}  would be restored (v{})",
                    snap_entry.version
                )?;
                any_change = true;
            }
            Some(cur_entry) if cur_entry.version != snap_entry.version => {
                writeln!(
                    out,
                    "  ~ {pkg}  v{} -> v{}",
                    cur_entry.version, snap_entry.version
                )?;
                any_change = true;
            }
            Some(_) => {}
        }
    }
    for pkg in current.packages.keys() {
        if !snapshot.lockfile.packages.contains_key(pkg) {
            writeln!(
                out,
                "  - {pkg}  would be removed (installed after this snapshot)"
            )?;
            any_change = true;
        }
    }
    if !any_change {
        writeln!(
            out,
            "  (none — current state already matches this snapshot)"
        )?;
    }

    let payload_errors = verify_payloads(root, &snapshot.lockfile, &m.payload_revisions)?;
    if payload_errors.is_empty() {
        writeln!(
            out,
            "\nAll referenced payloads verified present and unmodified."
        )?;
    } else {
        writeln!(out, "\nPayload problems (rollback would refuse):")?;
        for e in &payload_errors {
            writeln!(out, "  {e}")?;
        }
    }

    Ok(())
}

fn delete(root: &Path, id: &str, yes: bool) -> Result<()> {
    delete_with_tty(root, id, yes, std::io::stdin().is_terminal())
}

fn delete_with_tty(root: &Path, id: &str, yes: bool, is_tty: bool) -> Result<()> {
    // Destructive: permanently removes the rollback history for this snapshot.
    // Refuse unattended (non-TTY) sessions without explicit --yes; interactive
    // sessions keep the confirmation prompt below.
    crate::util::confirm::require_tty_or_yes_with_seam(yes, "snapshot deletion", is_tty)?;
    crate::util::confirm::confirm_or_bail(
        &format!("Delete snapshot '{id}'? This cannot be undone."),
        yes,
        "Cancelled.",
    )?;
    let _lock = acquire_mutation_lock(root)?;
    delete_snapshot(root, id)?;
    println!("{} Deleted snapshot {}", console::style("✓").green(), id);
    Ok(())
}

fn rollback(root: &Path, id: &str, yes: bool) -> Result<()> {
    rollback_with_tty(root, id, yes, std::io::stdin().is_terminal())
}

fn rollback_with_tty(root: &Path, id: &str, yes: bool, is_tty: bool) -> Result<()> {
    // Destructive: rewrites Numan-managed state to a past snapshot. Refuse
    // unattended sessions without explicit --yes; interactive sessions keep
    // the confirmation prompt (a pre-rollback snapshot is still taken first).
    crate::util::confirm::require_tty_or_yes_with_seam(yes, "snapshot rollback", is_tty)?;
    crate::util::confirm::confirm_or_bail(
        &format!(
            "Roll back Numan-managed state to snapshot '{id}'? \
             A snapshot of the current state will be taken first."
        ),
        yes,
        "Cancelled.",
    )?;
    let _lock = acquire_mutation_lock(root)?;

    let nu_paths = NuPaths::load(root)?;
    let runner = NuCandidateRunner::new(&nu_paths.nu_executable);
    let report = rollback_to_snapshot(root, id, &runner)?;

    println!(
        "{} Rolled back to snapshot {}",
        console::style("✓").green(),
        report.target_snapshot_id
    );
    println!("  packages restored: {}", report.packages_restored);
    println!("  autoload:          {}", report.autoload_action);
    println!(
        "  pre-rollback snapshot: {} (roll back to this to undo)",
        report.pre_rollback_snapshot_id
    );

    Ok(())
}

fn short_hash(h: &str) -> String {
    h.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::lockfile::LockfileEntry;
    use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};

    const ID: &str = "00000000-0000-0000-0000-000000000000";

    fn payload_lockfile_entry() -> LockfileEntry {
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
            payload_path: "packages/modules/owner/pkg/1.0.0-abc12345".to_string(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: Default::default(),
        }
    }

    #[test]
    fn short_hash_truncates_to_twelve_chars() {
        assert_eq!(short_hash("abcdefghijklmnopqrstuvwxyz"), "abcdefghijkl");
        assert_eq!(short_hash("short"), "short");
    }

    #[test]
    fn list_prints_no_snapshots_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        list(dir.path(), &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "No snapshots.\n");
    }

    #[test]
    fn list_prints_committed_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let manifest = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        let mut out = Vec::new();
        list(root, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Snapshots (1):"));
        assert!(s.contains(&manifest.id));
        assert!(s.contains("PreMutation"));
        assert!(s.contains("Install"));
        assert!(s.contains("0 package(s)"));
    }

    #[test]
    fn inspect_prints_snapshot_details_with_payload() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("state")).unwrap();

        let payload = root.join("packages/modules/owner/pkg/1.0.0-abc12345");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("mod.nu"), "# module").unwrap();

        let mut lockfile = Lockfile::empty();
        lockfile
            .packages
            .insert("owner/pkg".to_string(), payload_lockfile_entry());
        lockfile.save(root).unwrap();

        let manifest = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Install,
            None,
            None,
        )
        .unwrap();

        let mut out = Vec::new();
        inspect(root, &manifest.id, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains(&format!("Snapshot {}", manifest.id)));
        assert!(s.contains("reason:   PreMutation"));
        assert!(s.contains("trigger:  Install"));
        assert!(s.contains("Payload provenance (1 package(s)):"));
        assert!(s.contains("owner/pkg"));
        assert!(s.contains("All referenced payloads verified present and unmodified."));
    }

    #[test]
    fn delete_refuses_non_tty_without_yes() {
        // Force non-TTY via the injectable seam so the guard is deterministic
        // regardless of process stdin terminal status.
        let dir = tempfile::tempdir().unwrap();
        let err = delete_with_tty(dir.path(), ID, false, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(
                "Refusing destructive snapshot deletion in non-interactive session without --yes."
            ),
            "guard bail must be the audit contract, got: {msg}"
        );
    }

    #[test]
    fn delete_bypasses_guard_with_explicit_yes() {
        let dir = tempfile::tempdir().unwrap();
        // --yes must get past the destructive guard regardless of TTY; the
        // downstream "does not exist" bail proves the guard was the only blocker.
        // Force non-TTY so this never depends on process stdin terminal status.
        let err = delete_with_tty(dir.path(), ID, true, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("does not exist") || msg.contains("not a UUIDv7"),
            "expected downstream snapshot ID/missing bail, got: {msg}"
        );
        assert!(
            !msg.contains("Refusing destructive"),
            "--yes must bypass the guard, got: {msg}"
        );
    }

    #[test]
    fn rollback_refuses_non_tty_without_yes() {
        let dir = tempfile::tempdir().unwrap();
        let err = rollback_with_tty(dir.path(), ID, false, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(
                "Refusing destructive snapshot rollback in non-interactive session without --yes."
            ),
            "guard bail must be the audit contract, got: {msg}"
        );
    }

    #[test]
    fn rollback_bypasses_guard_with_explicit_yes() {
        let dir = tempfile::tempdir().unwrap();
        // --yes gets past the guard; the downstream "not initialized" bail
        // (NuPaths::load on an empty root) proves the guard was the blocker.
        // Force non-TTY so this never depends on process stdin terminal status.
        let err = rollback_with_tty(dir.path(), ID, true, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("Refusing destructive"),
            "--yes must bypass the guard, got: {msg}"
        );
        assert!(
            msg.contains("not initialized") || msg.contains("numan init"),
            "--yes must fail at downstream init check (got: {msg})"
        );
    }
}
