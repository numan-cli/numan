use anyhow::Result;
use clap::Subcommand;
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
        SnapshotCommands::List => list(root),
        SnapshotCommands::Inspect { id } => inspect(root, &id),
        SnapshotCommands::Delete { id, yes } => delete(root, &id, yes),
        SnapshotCommands::Rollback { id, yes } => rollback(root, &id, yes),
    }
}

fn list(root: &Path) -> Result<()> {
    let snapshots = list_snapshots(root)?;
    if snapshots.is_empty() {
        println!("No snapshots.");
        return Ok(());
    }

    println!("Snapshots ({}):\n", snapshots.len());
    for s in &snapshots {
        let related = s
            .related_snapshot_id
            .as_deref()
            .map(|r| format!(" (of {r})"))
            .unwrap_or_default();
        println!(
            "  {}  {:?}  {:?}{}  {} package(s)  created {}",
            s.id,
            s.reason,
            s.trigger,
            related,
            s.payload_revisions.len(),
            s.created_at
        );
    }
    Ok(())
}

fn inspect(root: &Path, id: &str) -> Result<()> {
    let snapshot = load_snapshot(root, id)?;
    let m = &snapshot.manifest;

    println!("Snapshot {}", m.id);
    println!("  created:  {}", m.created_at);
    println!("  reason:   {:?}", m.reason);
    println!("  trigger:  {:?}", m.trigger);
    if let Some(related) = &m.related_snapshot_id {
        println!("  related:  {:?} of {}", m.relation, related);
    }
    println!("  root:     {}", m.numan_root);
    println!("  platform: {}", m.platform);
    if let Some(nu) = &m.nu_identity {
        println!(
            "  nu:       {} (executable sha256 {})",
            nu.nu_version,
            short_hash(&nu.nu_executable_sha256)
        );
    }

    println!("\nGenerated-file digests:");
    println!(
        "  lockfile: {}",
        short_hash(&m.sidecar_digests.lockfile_sha256)
    );
    if let Some(h) = &m.sidecar_digests.autoload_sha256 {
        println!("  autoload: {}", short_hash(h));
    }
    if let Some(h) = &m.sidecar_digests.imports_sha256 {
        println!("  imports:  {}", short_hash(h));
    }

    println!(
        "\nPayload provenance ({} package(s)):",
        m.payload_revisions.len()
    );
    for (pkg, rev) in &m.payload_revisions {
        println!("  {}  revision {}", pkg, short_hash(rev));
    }

    match &snapshot.autoload.projection {
        ManagedAutoloadProjection::Present {
            managed_file_path,
            active_module_ids,
            ..
        } => {
            println!(
                "\nModule autoload: {} active module(s) via '{}'",
                active_module_ids.len(),
                managed_file_path
            );
            for id in active_module_ids {
                println!("  {id}");
            }
        }
        ManagedAutoloadProjection::Absent { managed_file_path } => {
            println!("\nModule autoload: none active (managed file '{managed_file_path}' absent)");
        }
        ManagedAutoloadProjection::NotConfigured => {
            println!("\nModule autoload: not configured at snapshot time");
        }
    }

    if let Some(nu) = &m.nu_identity {
        let plugin_count = count_active_plugins(&snapshot.lockfile, nu);
        println!("Active plugins (matching snapshot Nu identity): {plugin_count}");
    }
    let _ = count_active_modules(&snapshot.autoload); // exercised above via active_module_ids

    if let Some(imports) = &snapshot.imports {
        println!(
            "\nnupm import provenance ({} record(s)):",
            imports.imports.len()
        );
        for (pkg, rec) in &imports.imports {
            println!(
                "  {}  from {} (trust: {})",
                pkg, rec.nupm_source_path, rec.trust_level
            );
        }
    }

    println!("\nAffected packages if rolled back (compared to current lockfile):");
    let current = Lockfile::load(root)?;
    let mut any_change = false;
    for (pkg, snap_entry) in &snapshot.lockfile.packages {
        match current.packages.get(pkg) {
            None => {
                println!("  + {pkg}  would be restored (v{})", snap_entry.version);
                any_change = true;
            }
            Some(cur_entry) if cur_entry.version != snap_entry.version => {
                println!(
                    "  ~ {pkg}  v{} -> v{}",
                    cur_entry.version, snap_entry.version
                );
                any_change = true;
            }
            Some(_) => {}
        }
    }
    for pkg in current.packages.keys() {
        if !snapshot.lockfile.packages.contains_key(pkg) {
            println!("  - {pkg}  would be removed (installed after this snapshot)");
            any_change = true;
        }
    }
    if !any_change {
        println!("  (none — current state already matches this snapshot)");
    }

    let payload_errors = verify_payloads(root, &snapshot.lockfile, &m.payload_revisions)?;
    if payload_errors.is_empty() {
        println!("\nAll referenced payloads verified present and unmodified.");
    } else {
        println!("\nPayload problems (rollback would refuse):");
        for e in &payload_errors {
            println!("  {e}");
        }
    }

    Ok(())
}

fn delete(root: &Path, id: &str, yes: bool) -> Result<()> {
    let _lock = acquire_mutation_lock(root)?;
    if !yes {
        confirm(&format!("Delete snapshot '{id}'? This cannot be undone."))?;
    }
    delete_snapshot(root, id)?;
    println!("{} Deleted snapshot {}", console::style("✓").green(), id);
    Ok(())
}

fn rollback(root: &Path, id: &str, yes: bool) -> Result<()> {
    let _lock = acquire_mutation_lock(root)?;

    if !yes {
        confirm(&format!(
            "Roll back Numan-managed state to snapshot '{id}'? \
             A snapshot of the current state will be taken first."
        ))?;
    }

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

fn confirm(prompt: &str) -> Result<()> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        anyhow::bail!("Interactive confirmation required for non-TTY sessions. Pass --yes to proceed without prompting.");
    }
    crate::util::confirm::confirm_or_bail(prompt, false, "Cancelled.")
}

fn short_hash(h: &str) -> String {
    h.chars().take(12).collect()
}
