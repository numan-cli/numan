//! `numan use` — switch the active managed Nu version.
//!
//! Supports side-by-side Nu version management:
//! - `numan use <version>` — switch to a specific installed version
//! - `numan use latest` — switch to the newest installed version
//! - `numan use list` — show all installed versions with active marker
//!
//! Cross-minor switches auto-deactivate Numan-active plugins/modules (leave
//! profile is a union never shrunk by `use`) and restore the target minor's
//! desired activation set after the marker write. Same-target `use` is
//! restore-only reconcile.

use anyhow::{bail, Context, Result};

use clap::Args;
use std::path::Path;

use crate::cmd::activation_switch::{self, SwitchHooks};
use crate::nu::autoload::CandidateRunner;
use crate::nu::version_manager;
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::fs_safety::setup_subcommand_lock;

#[derive(Args, Debug)]
pub struct UseArgs {
    /// Nu version to switch to (e.g. 0.113.1), "latest", or "list"
    #[arg(required = true)]
    pub version: String,
}

pub fn execute(args: &UseArgs, root: &Path) -> Result<()> {
    execute_with_hooks(
        args,
        root,
        &crate::cmd::activate::run_plugin_add,
        &crate::cmd::deactivate::run_plugin_rm,
        None,
    )
}

/// Testability entry: injectable plugin registrar, unregistrar, module runner,
/// and optional paths refresh (for same-root tests without a real Nu binary).
pub fn execute_with_hooks(
    args: &UseArgs,
    root: &Path,
    registrar: &dyn Fn(&str, &str, &str) -> Result<()>,
    unregistrar: &dyn Fn(&str, &str, &str) -> Result<()>,
    runner: Option<&dyn CandidateRunner>,
) -> Result<()> {
    execute_with_hooks_and_refresh(args, root, registrar, unregistrar, runner, None)
}

/// Like [`execute_with_hooks`] with an injectable post-switch paths refresh.
pub fn execute_with_hooks_and_refresh(
    args: &UseArgs,
    root: &Path,
    registrar: &dyn Fn(&str, &str, &str) -> Result<()>,
    unregistrar: &dyn Fn(&str, &str, &str) -> Result<()>,
    runner: Option<&dyn CandidateRunner>,
    path_refresh: Option<&activation_switch::PathRefreshHook>,
) -> Result<()> {
    // Listing is read-only: no lock, snapshot, or migrate. A flat legacy
    // install is still surfaced via the VERSION marker without on-disk migrate.
    if args.version == "list" {
        return execute_list(root);
    }

    let hooks = SwitchHooks {
        registrar,
        unregistrar,
        runner,
        path_refresh,
    };

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
            "latest" => execute_latest(root, &hooks),
            version => activation_switch::switch_active_nu_version(root, version, &hooks),
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
fn execute_latest(root: &Path, hooks: &SwitchHooks<'_>) -> Result<()> {
    let latest = version_manager::latest_installed_version(root)?;
    let Some(version) = latest else {
        bail!(
            "No Nu versions installed.\n\
             Run 'numan setup nu' or 'numan setup nu <version>' first."
        )
    };

    // Self-heal dangling off-tree `binary_path` before same-target reconcile so
    // a later `use list` / resolver sees a valid on-tree selection.
    if let Some(existing) = version_manager::read_active_version(root)? {
        if existing.version == version {
            if let Some(path) = existing.binary_path.as_deref() {
                if !std::path::Path::new(path).is_file() {
                    version_manager::write_active_version(root, &version)?;
                }
            }
        }
    }

    activation_switch::switch_active_nu_version(root, &version, hooks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nu::paths::NuPaths;
    use crate::state::snapshot::{list_snapshots, SnapshotReason, SnapshotTrigger};
    use crate::util::fs_safety::acquire_mutation_lock;
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
    fn test_use_latest_self_heals_dangling_off_tree_binary_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.113.1");
        // Marker names latest version but points at a missing off-tree binary.
        version_manager::write_active_version_with_binary(
            root,
            "0.113.1",
            std::path::Path::new("/nonexistent/nu"),
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
            "dangling off-tree path must be cleared; got {:?}",
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

    #[test]
    fn execute_fails_while_mutation_lock_held() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.112.0");
        create_fake_version(root, "0.113.1");
        version_manager::write_active_version(root, "0.112.0").unwrap();

        // Hold the root mutation lock — concurrent `numan use` must refuse.
        let _lock = acquire_mutation_lock(root).unwrap();

        let err = execute(
            &UseArgs {
                version: "0.113.1".to_string(),
            },
            root,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("lock")
                || msg.contains("mutex")
                || msg.contains("busy")
                || msg.contains("mutation")
                || msg.contains("progress"),
            "error must mention lock contention: {msg}"
        );
    }

    #[test]
    fn list_succeeds_while_mutation_lock_held() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_fake_version(root, "0.113.1");
        version_manager::write_active_version(root, "0.113.1").unwrap();

        // Hold the lock — `numan use list` is read-only and must NOT acquire it.
        let _lock = acquire_mutation_lock(root).unwrap();

        execute(
            &UseArgs {
                version: "list".to_string(),
            },
            root,
        )
        .unwrap();
    }

    #[test]
    fn execute_with_hooks_and_refresh_integration() {
        use crate::core::integrity;
        use crate::nu::autoload::FakeCandidateRunner;
        use crate::state::activation_profile::{ActivationProfile, ProfileKind};
        use crate::state::lockfile::{Lockfile, LockfileEntry, PluginActivation};
        use std::cell::RefCell;
        use std::collections::BTreeMap;
        use std::rc::Rc;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_fake_version(root, "0.113.1");
        create_fake_version(root, "0.114.0");
        version_manager::write_active_version(root, "0.113.1").unwrap();

        let (nu_exe_113, hash_113) = {
            let binary = version_manager::version_binary(root, "0.113.1");
            let hash = integrity::compute_sha256(b"fake");
            (binary.to_string_lossy().into_owned(), hash)
        };

        let vendor = root.join("vendor");
        std::fs::create_dir_all(&vendor).unwrap();
        let registry = root.join("plugin-registry.msgpack.z");
        std::fs::write(&registry, b"reg").unwrap();

        let paths = NuPaths {
            nu_executable: nu_exe_113.clone(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: registry.to_string_lossy().into_owned(),
            nu_executable_hash: hash_113.clone(),
            platform: "test".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![vendor.to_string_lossy().into_owned()],
            vendor_autoload_dir: Some(vendor.to_string_lossy().into_owned()),
        };
        paths.save(root).unwrap();

        let payload = "packages/plugins/o/plug/1.0.0-aaa";
        let payload_dir = root.join(payload);
        std::fs::create_dir_all(&payload_dir).unwrap();
        std::fs::write(payload_dir.join("nu_plugin_x"), b"fake").unwrap();

        let mut lockfile = Lockfile::empty();
        lockfile.packages.insert(
            "o/plug".into(),
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
                    nu_executable_sha256: hash_113.clone(),
                    nu_version: "0.113.1".to_string(),
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
        lockfile.save(root).unwrap();

        let mut profile = ActivationProfile::new();
        profile.ensure_contains("0.113", ProfileKind::Plugin, "o/plug");
        profile.ensure_contains("0.114", ProfileKind::Plugin, "o/plug");
        profile.save(root).unwrap();

        let hook_order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let o1 = Rc::clone(&hook_order);
        let o2 = Rc::clone(&hook_order);
        let o3 = Rc::clone(&hook_order);

        let unregistrar = move |_a: &str, _b: &str, _c: &str| -> Result<()> {
            let active = version_manager::read_active_version(root)
                .expect("active version readable during leave")
                .expect("active version present during leave");
            assert_eq!(
                active.version, "0.113.1",
                "unregistrar must run before active-version marker changes"
            );
            o1.borrow_mut().push("unregister");
            Ok(())
        };
        let registrar = move |_a: &str, _b: &str, _c: &str| -> Result<()> {
            o2.borrow_mut().push("register");
            Ok(())
        };
        let path_refresh = move |_root: &Path| -> Result<()> {
            o3.borrow_mut().push("path_refresh");
            Ok(())
        };
        let runner = FakeCandidateRunner::success();

        execute_with_hooks_and_refresh(
            &UseArgs {
                version: "0.114.0".to_string(),
            },
            root,
            &registrar,
            &unregistrar,
            Some(&runner),
            Some(&path_refresh),
        )
        .unwrap();

        let order = hook_order.borrow();
        assert_eq!(
            order.as_slice(),
            &["unregister", "path_refresh", "register"][..],
            "leave (unregister) then marker/path refresh then restore (register)"
        );

        let active = version_manager::read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.114.0", "switch must complete");

        assert_pre_mutation_snapshot(root);
    }
}
