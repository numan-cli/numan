//! `numan use` — switch the active managed Nu version.
//!
//! Supports side-by-side Nu version management:
//! - `numan use <version>` — switch to a specific installed version
//! - `numan use latest` — switch to the newest installed version
//! - `numan use list` — show all installed versions with active marker

use anyhow::{bail, Context, Result};

use clap::Args;
use std::path::Path;

use crate::nu::paths::NuPaths;
use crate::nu::version_manager;
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::fs_safety::setup_subcommand_lock;
use crate::util::hints::CMD_INIT_REFRESH;

#[derive(Args, Debug)]
pub struct UseArgs {
    /// Nu version to switch to (e.g. 0.113.1), "latest", or "list"
    #[arg(required = true)]
    pub version: String,
}

pub fn execute(args: &UseArgs, root: &Path) -> Result<()> {
    // Listing is read-only: `list_installed_versions` already surfaces a flat
    // legacy install via the VERSION marker without migrating on disk.
    if args.version == "list" {
        return execute_list(root);
    }

    // Hold the mutation lock for the entire operation to prevent races
    // between concurrent `numan setup nu` and `numan use` invocations.
    setup_subcommand_lock(root, "Nu version switch", || {
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
    })
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
                            refresh_cached_nu_paths_after_switch(root)?;
                            return Ok(());
                        }
                    }
                }
            }
            version_manager::write_active_version(root, &version)?;
            println!("Switched to Nu {} (latest installed).", version);
            refresh_cached_nu_paths_after_switch(root)?;
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
    refresh_cached_nu_paths_after_switch(root)?;
    Ok(())
}

/// Keep `nu_state/paths.json` aligned with the newly selected active Nu.
///
/// `activate` loads the cached paths and only checks that the cached binary
/// still hashes — side-by-side installs leave the previous binary intact, so
/// a stale cache would silently keep activating against the old Nu. Re-probe
/// when possible; if probing fails, delete the cache so callers fail closed
/// with an `init --refresh` hint instead of using the wrong Nu.
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
            "`numan use list` must not create a PreMutation snapshot"
        );
        // Active selection unchanged.
        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
    }

    #[test]
    fn test_use_list_discovers_legacy_via_version_marker_without_mutation() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let tools = version_manager::versioned_nu_dir(root);
        std::fs::create_dir_all(&tools).unwrap();
        let bin = if cfg!(windows) { "nu.exe" } else { "nu" };
        std::fs::write(tools.join(bin), b"legacy").unwrap();
        std::fs::write(tools.join("VERSION"), "0.113.1\n").unwrap();

        execute(
            &UseArgs {
                version: "list".to_string(),
            },
            root,
        )
        .unwrap();

        assert!(
            list_snapshots(root).unwrap().is_empty(),
            "list must not create a PreMutation snapshot"
        );
        assert!(
            tools.join(bin).is_file(),
            "list must leave the flat legacy binary in place"
        );
        assert!(
            !version_manager::version_binary(root, "0.113.1").is_file(),
            "list must not migrate into tools/nushell/<version>/"
        );
        assert!(
            version_manager::list_installed_versions(root)
                .unwrap()
                .iter()
                .any(|v| v == "0.113.1"),
            "VERSION marker must still surface the legacy install to list"
        );
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
    fn test_use_switch_clears_stale_paths_cache_when_probe_fails() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.112.0");
        create_fake_version(root, "0.113.1");
        version_manager::write_active_version(root, "0.112.0").unwrap();

        // Seed a paths.json that still points at the previous Nu. Fake binaries
        // cannot be probed, so the refresh path must fail closed and delete it.
        let stale = NuPaths {
            nu_executable: version_manager::version_binary(root, "0.112.0")
                .to_string_lossy()
                .into_owned(),
            nu_version: "0.112.0".to_string(),
            plugin_registry_path: root.join("plugins.msgpackz").to_string_lossy().into_owned(),
            nu_executable_hash: "deadbeef".to_string(),
            platform: "test".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![],
            vendor_autoload_dir: None,
        };
        stale.save(root).unwrap();
        assert!(root.join("nu_state/paths.json").is_file());

        execute(
            &UseArgs {
                version: "0.113.1".to_string(),
            },
            root,
        )
        .unwrap();

        assert_eq!(
            version_manager::read_active_version(root)
                .unwrap()
                .unwrap()
                .version,
            "0.113.1"
        );
        assert!(
            !root.join("nu_state/paths.json").exists(),
            "stale paths.json must be cleared so activate cannot keep using old Nu"
        );
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
