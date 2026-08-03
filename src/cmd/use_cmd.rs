//! `numan use` — switch the active managed Nu version.
//!
//! Supports side-by-side Nu version management:
//! - `numan use <version>` — switch to a specific installed version
//! - `numan use latest` — switch to the newest installed version
//! - `numan use list` — show all installed versions with active marker

use anyhow::{bail, Context, Result};

use clap::Args;
use std::path::Path;

use crate::nu::version_manager;
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::fs_safety::acquire_mutation_lock;

#[derive(Args, Debug)]
pub struct UseArgs {
    /// Nu version to switch to (e.g. 0.113.1), "latest", or "list"
    #[arg(required = true)]
    pub version: String,
}

pub fn execute(args: &UseArgs, root: &Path) -> Result<()> {
    // Listing reconciles the legacy single-binary layout first so a flat
    // `tools/nushell/nu` install is not invisible. Migration is a no-op when
    // already versioned, and preserves any existing active selection.
    if args.version == "list" {
        let _lock = acquire_mutation_lock(root)?;
        crate::nu::migrate_legacy::migrate_legacy_install(root)
            .with_context(|| "Failed to migrate legacy Nu installation before list")?;
        return execute_list(root);
    }

    // Hold the mutation lock for the entire operation to prevent races
    // between concurrent `numan setup nu` and `numan use` invocations.
    let _lock = acquire_mutation_lock(root)?;

    // Snapshot established state before any mutation. This covers both the
    // legacy-migration step (rename + active-version write) and the version
    // switch below.
    create_snapshot(
        root,
        SnapshotReason::PreMutation,
        SnapshotTrigger::Update,
        None,
        None,
    )
    .with_context(|| "Failed to create pre-mutation snapshot for `numan use`")?;

    crate::nu::migrate_legacy::migrate_legacy_install(root)
        .with_context(|| "Failed to migrate legacy Nu installation")?;

    match args.version.as_str() {
        "latest" => execute_latest(root),
        version => execute_switch(root, version),
    }
}

/// List all installed Nu versions, marking the active one.
fn execute_list(root: &Path) -> Result<()> {
    let versions = version_manager::list_installed_versions(root)?;
    let active = version_manager::read_active_version(root)?;

    if versions.is_empty() {
        println!("No Nu versions installed.");
        println!("Run 'numan setup nu' or 'numan setup nu <version>' to install.");
        return Ok(());
    }

    println!("Installed Nu versions:");
    for version in versions {
        let is_active = active.as_ref().is_some_and(|a| a.version == version);
        let marker = if is_active { " (active)" } else { "" };
        println!("  {}{}", version, marker);
    }

    Ok(())
}

/// Switch to the latest (newest) installed Nu version.
fn execute_latest(root: &Path) -> Result<()> {
    let latest = version_manager::latest_installed_version(root)?;
    match latest {
        Some(version) => {
            // Preserve off-tree `binary_path` only when it still resolves to a
            // real file. An empty or stale path must not be rewritten; fall
            // through to a clean on-tree active marker instead.
            if let Some(existing) = version_manager::read_active_version(root)? {
                if existing.version == version {
                    if let Some(off_tree) = existing.binary_path.as_ref() {
                        let off_tree_path = std::path::Path::new(off_tree);
                        if !off_tree.is_empty() && off_tree_path.is_file() {
                            version_manager::write_active_version_with_binary(
                                root,
                                &version,
                                off_tree_path,
                            )?;
                            println!(
                                "Switched to Nu {} (latest installed; off-tree path preserved).",
                                version
                            );
                            return Ok(());
                        }
                    }
                }
            }
            version_manager::write_active_version(root, &version)?;
            println!("Switched to Nu {} (latest installed).", version);
            Ok(())
        }
        None => {
            bail!(
                "No Nu versions installed.\n\
                 Run 'numan setup nu' or 'numan setup nu <version>' first."
            )
        }
    }
}

/// Switch to a specific Nu version.
fn execute_switch(root: &Path, version: &str) -> Result<()> {
    let version = version_manager::normalize_version(version)?;
    // Resolve on-tree or off-tree so versions shown by `numan use list`
    // (including off-tree marker selections) remain switchable.
    let Some(resolved) = version_manager::resolve_installed_version(root, &version)? else {
        let installed = version_manager::list_installed_versions(root)?;
        let hint = if installed.is_empty() {
            format!(
                "No Nu versions installed.\n\
                 Run 'numan setup nu {}' to install.",
                version
            )
        } else {
            format!(
                "Nu {} is not installed.\n\
                 Installed versions: {}\n\
                 Run 'numan setup nu {}' to install, or 'numan use list' to see available versions.",
                version,
                installed.join(", "),
                version
            )
        };
        bail!("{}", hint);
    };

    let on_tree = version_manager::version_binary(root, &version);
    if resolved == on_tree {
        version_manager::write_active_version(root, &version)
            .with_context(|| format!("Failed to switch to Nu {}", version))?;
    } else {
        version_manager::write_active_version_with_binary(root, &version, &resolved)
            .with_context(|| format!("Failed to switch to Nu {}", version))?;
    }
    println!("Switched to Nu {}.", version);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::snapshot::{list_snapshots, SnapshotReason, SnapshotTrigger};
    use tempfile::TempDir;

    fn create_fake_version(root: &Path, version: &str) {
        let binary_name = if cfg!(windows) { "nu.exe" } else { "nu" };
        let dir = version_manager::version_install_dir(root, version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(binary_name), "fake").unwrap();
    }

    fn assert_pre_mutation_snapshot(root: &Path) {
        let snaps = list_snapshots(root).unwrap();
        assert_eq!(
            snaps.len(),
            1,
            "version-changing `numan use` must create exactly one pre-mutation snapshot"
        );
        assert_eq!(snaps[0].reason, SnapshotReason::PreMutation);
        assert_eq!(snaps[0].trigger, SnapshotTrigger::Update);
    }

    #[test]
    fn test_use_list_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let args = UseArgs {
            version: "list".to_string(),
        };
        // Should not error, just print "no versions installed".
        execute(&args, root).unwrap();
    }

    #[test]
    fn test_use_list_with_versions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.112.0");
        create_fake_version(root, "0.113.1");
        version_manager::write_active_version(root, "0.113.1").unwrap();

        let args = UseArgs {
            version: "list".to_string(),
        };
        execute(&args, root).unwrap();
    }

    #[test]
    fn test_use_list_creates_no_snapshot() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.113.1");
        version_manager::write_active_version(root, "0.113.1").unwrap();

        execute(
            &UseArgs {
                version: "list".to_string(),
            },
            root,
        )
        .unwrap();

        assert!(
            list_snapshots(root).unwrap().is_empty(),
            "`numan use list` is read-only and must not create a snapshot"
        );
        // Active selection unchanged (no mutation / no rollback side effects).
        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
    }

    #[test]
    fn test_use_latest_no_versions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let args = UseArgs {
            version: "latest".to_string(),
        };
        let result = execute(&args, root);
        assert!(result.is_err());
    }

    #[test]
    fn test_use_latest_with_versions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.112.0");
        create_fake_version(root, "0.113.1");

        let args = UseArgs {
            version: "latest".to_string(),
        };
        execute(&args, root).unwrap();

        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
    }

    #[test]
    fn test_use_latest_drops_stale_offtree_binary_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.113.1");
        // Marker names latest but points at a missing off-tree binary.
        version_manager::write_active_version_with_binary(
            root,
            "0.113.1",
            std::path::Path::new("/nonexistent/opt-nu"),
        )
        .unwrap();

        execute(
            &UseArgs {
                version: "latest".to_string(),
            },
            root,
        )
        .unwrap();

        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
        assert!(
            active.binary_path.is_none(),
            "stale/empty off-tree path must not be preserved: {:?}",
            active.binary_path
        );
    }

    #[test]
    fn test_use_latest_preserves_live_offtree_binary_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.113.1");
        let off_tree = tmp
            .path()
            .join("opt-nu")
            .join(if cfg!(windows) { "nu.exe" } else { "nu" });
        std::fs::create_dir_all(off_tree.parent().unwrap()).unwrap();
        std::fs::write(&off_tree, b"off-tree latest").unwrap();
        version_manager::write_active_version_with_binary(root, "0.113.1", &off_tree).unwrap();

        execute(
            &UseArgs {
                version: "latest".to_string(),
            },
            root,
        )
        .unwrap();

        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
        assert_eq!(
            active.binary_path.as_deref(),
            Some(off_tree.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn test_use_latest_creates_pre_mutation_snapshot() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.112.0");
        create_fake_version(root, "0.113.1");
        version_manager::write_active_version(root, "0.112.0").unwrap();

        execute(
            &UseArgs {
                version: "latest".to_string(),
            },
            root,
        )
        .unwrap();

        assert_pre_mutation_snapshot(root);
        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
    }

    #[test]
    fn test_use_switch_not_installed() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let args = UseArgs {
            version: "0.113.1".to_string(),
        };
        let result = execute(&args, root);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        // Error message should mention that no versions are installed or the version is not installed.
        assert!(
            err_msg.contains("installed"),
            "Error message was: {}",
            err_msg
        );
    }

    #[test]
    fn test_use_switch_success() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.113.1");

        let args = UseArgs {
            version: "0.113.1".to_string(),
        };
        execute(&args, root).unwrap();

        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
    }

    #[test]
    fn test_use_switch_creates_pre_mutation_snapshot() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.112.0");
        create_fake_version(root, "0.113.1");
        version_manager::write_active_version(root, "0.112.0").unwrap();

        execute(
            &UseArgs {
                version: "0.113.1".to_string(),
            },
            root,
        )
        .unwrap();

        assert_pre_mutation_snapshot(root);
        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
    }
}
