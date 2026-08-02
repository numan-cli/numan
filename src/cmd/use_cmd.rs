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
    match args.version.as_str() {
        // `list` is read-only: no lock, no snapshot, no migration. Taking the
        // non-blocking mutation lock here would make a pure read fail under
        // contention with a concurrent `install`/`setup`, and the snapshot
        // would be clutter.
        "list" => execute_list(root),
        // The mutating arms flip the active-version marker, so they hold the
        // lock for the whole operation (to prevent races with concurrent
        // `numan setup nu` / `numan use`) and snapshot established state first.
        "latest" => with_mutation_guard(root, execute_latest),
        version => with_mutation_guard(root, |root| execute_switch(root, version)),
    }
}

/// Acquire the mutation lock and take a `PreMutation` snapshot before running a
/// mutating `numan use` arm. Also runs journaled legacy-install migration so a
/// single-binary layout is versioned before the active-marker flip.
fn with_mutation_guard(root: &Path, op: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
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

    op(root)
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
            let installed_binary = version_manager::resolve_installed_version(root, &version)
                .with_context(|| format!("Nu {} is no longer present", version))?;
            let on_tree = version_manager::version_binary(root, &version);
            if installed_binary == on_tree {
                version_manager::write_active_version(root, &version)?;
            } else {
                version_manager::write_active_version_with_binary(
                    root,
                    &version,
                    &installed_binary,
                )?;
            }
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
    // Validate the version is installed (on-tree or off-tree).
    let installed_binary = version_manager::resolve_installed_version(root, &version)
        .with_context(|| {
            let installed = version_manager::list_installed_versions(root).unwrap_or_default();
            if installed.is_empty() {
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
            }
        })?;

    // Switch to the requested version, preserving an off-tree binary path.
    let on_tree = version_manager::version_binary(root, &version);
    if installed_binary == on_tree {
        version_manager::write_active_version(root, &version)
            .with_context(|| format!("Failed to switch to Nu {}", version))?;
    } else {
        version_manager::write_active_version_with_binary(root, &version, &installed_binary)
            .with_context(|| format!("Failed to switch to Nu {}", version))?;
    }
    println!("Switched to Nu {}.", version);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_fake_version(root: &Path, version: &str) {
        let binary_name = if cfg!(windows) { "nu.exe" } else { "nu" };
        let dir = version_manager::version_install_dir(root, version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(binary_name), "fake").unwrap();
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
    fn test_use_list_takes_no_snapshot() {
        // `numan use list` is read-only: it must not create a PreMutation
        // snapshot (which lands under `<root>/state/snapshots`), otherwise a
        // pure listing would leave clutter and take the mutation lock.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.113.1");

        let args = UseArgs {
            version: "list".to_string(),
        };
        execute(&args, root).unwrap();

        assert!(
            !root.join("state/snapshots").exists(),
            "`numan use list` must not create a snapshot"
        );
    }

    #[test]
    fn test_use_switch_takes_snapshot() {
        // A mutating switch must snapshot established state before flipping the
        // active-version marker.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.113.1");

        let args = UseArgs {
            version: "0.113.1".to_string(),
        };
        execute(&args, root).unwrap();

        assert!(
            root.join("state/snapshots").exists(),
            "a version switch must create a PreMutation snapshot"
        );
    }
}
