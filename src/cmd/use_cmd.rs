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
    with_mutation_guard_and_migrator(root, op, crate::nu::migrate_legacy::migrate_legacy_install)
}

/// Test seam for [`with_mutation_guard`]: inject the legacy-migration step so
/// unit tests can cover success and failure without spawning Nu or touching
/// a real legacy layout. Signature matches [`migrate_legacy_install`]
/// (`Result<bool, _>`: whether a migration ran).
fn with_mutation_guard_and_migrator<E>(
    root: &Path,
    op: impl FnOnce(&Path) -> Result<()>,
    migrate: impl FnOnce(&Path) -> std::result::Result<bool, E>,
) -> Result<()>
where
    E: Into<anyhow::Error>,
{
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

    migrate(root)
        .map_err(Into::into)
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

/// Persist the active-version marker, preserving an off-tree binary path when
/// the resolved binary lives outside `tools/nushell/<version>/`.
fn select_version(root: &Path, version: &str, installed_binary: &Path) -> Result<()> {
    if installed_binary == version_manager::version_binary(root, version) {
        version_manager::write_active_version(root, version)
    } else {
        version_manager::write_active_version_with_binary(root, version, installed_binary)
    }
    .with_context(|| format!("Failed to switch to Nu {}", version))
}

/// Switch to the latest (newest) installed Nu version.
fn execute_latest(root: &Path) -> Result<()> {
    let latest = version_manager::latest_installed_version(root)?;
    match latest {
        Some(version) => {
            let installed_binary = version_manager::resolve_installed_version(root, &version)
                .with_context(|| format!("Nu {} is no longer present", version))?;
            select_version(root, &version, &installed_binary)?;
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
    let installed_binary = match version_manager::resolve_installed_version(root, &version) {
        Some(path) => path,
        None => {
            let installed = version_manager::list_installed_versions(root).with_context(|| {
                format!(
                    "Nu {} is not installed, and listing installed versions also failed",
                    version
                )
            })?;
            if installed.is_empty() {
                bail!(
                    "No Nu versions installed.\n\
                     Run 'numan setup nu {}' to install.",
                    version
                );
            }
            bail!(
                "Nu {} is not installed.\n\
                 Installed versions: {}\n\
                 Run 'numan setup nu {}' to install, or 'numan use list' to see available versions.",
                version,
                installed.join(", "),
                version
            );
        }
    };

    // Switch to the requested version, preserving an off-tree binary path.
    select_version(root, &version, &installed_binary)?;
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
    fn test_use_list_runs_no_migration() {
        // Stage a legacy single-binary layout. A read-only `list` must leave it
        // exactly as-is; only the mutating arms may migrate.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let legacy = version_manager::versioned_nu_dir(root).join(if cfg!(windows) {
            "nu.exe"
        } else {
            "nu"
        });
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "fake legacy nu").unwrap();

        execute(
            &UseArgs {
                version: "list".to_string(),
            },
            root,
        )
        .unwrap();

        assert!(
            legacy.is_file(),
            "`numan use list` must not migrate the legacy binary"
        );
        assert!(
            version_manager::read_active_version(root)
                .unwrap()
                .is_none(),
            "`numan use list` must not write the active-version marker"
        );
    }

    #[test]
    fn test_use_switch_preserves_off_tree_binary_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Off-tree Nu recorded by `numan setup nu use <path>`.
        let external = tmp.path().join("external-nu");
        std::fs::write(&external, "fake").unwrap();
        version_manager::write_active_version_with_binary(root, "0.113.1", &external).unwrap();

        execute(
            &UseArgs {
                version: "0.113.1".to_string(),
            },
            root,
        )
        .unwrap();

        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
        assert_eq!(
            active.binary_path.as_deref(),
            Some(external.to_string_lossy().as_ref()),
            "`numan use` must not downgrade an off-tree selection to an on-tree path"
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

    #[test]
    fn test_use_runs_migration_before_version_selection() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static MIGRATED: AtomicBool = AtomicBool::new(false);

        fn migrate_ok(_root: &Path) -> Result<bool> {
            MIGRATED.store(true, Ordering::SeqCst);
            Ok(true)
        }

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.113.1");
        MIGRATED.store(false, Ordering::SeqCst);

        with_mutation_guard_and_migrator(root, |root| execute_switch(root, "0.113.1"), migrate_ok)
            .unwrap();

        assert!(
            MIGRATED.load(Ordering::SeqCst),
            "legacy migration must run before version selection"
        );
        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
    }

    #[test]
    fn test_use_propagates_migration_error() {
        fn migrate_err(_root: &Path) -> Result<bool> {
            bail!("simulated migration failure")
        }

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.113.1");

        let err = with_mutation_guard_and_migrator(
            root,
            |root| execute_switch(root, "0.113.1"),
            migrate_err,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Failed to migrate legacy Nu installation"),
            "migration error must keep context, got: {msg}"
        );
        assert!(
            msg.contains("simulated migration failure"),
            "underlying migration error must surface, got: {msg}"
        );
        assert!(
            version_manager::read_active_version(root)
                .unwrap()
                .is_none(),
            "failed migration must not flip the active marker"
        );
    }
}
