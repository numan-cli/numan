/// Integration tests for `numan activate`.
///
/// Uses `execute_with_registrar` as the testability seam so no real Nu binary
/// is required. All file-system state (NuPaths, lockfile, plugin binaries) is
/// created in a `tempfile::TempDir` for each test.
///
/// ## Manual acceptance test (real Nu — run before freezing Phase 3)
///
/// These automated tests exercise the control flow but cannot truthfully prove
/// Nu plugin registration. Before shipping Phase 3:
///
/// 1. Install a known plugin via `numan install owner/plugin`.
/// 2. Run `numan init` (once implemented) to populate NuPaths.
/// 3. Run `numan activate` — verify exit 0 and success output.
/// 4. Start a new Nu session and verify the plugin's commands are available
///    (e.g. `help | where name =~ "nu_plugin_"`).
/// 5. Run `numan activate` again — verify the plugin is reported as already
///    active (idempotent, exit 0).
use anyhow::bail;
use numan_cli::cmd::activate::{execute_with_registrar, validate_payload_path, ActivateArgs};
use numan_cli::core::integrity;
use numan_cli::nu::paths::NuPaths;
use numan_cli::state::journal::{PendingActivation, PendingActivationEntry, PendingStatus};
use numan_cli::state::lockfile::{Lockfile, LockfileEntry, PluginActivation};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

struct TestEnv {
    dir: TempDir,
    nu_hash: String,
    /// The fake nu binary file (must exist for validate_drift).
    nu_exe: PathBuf,
    /// Plugin registry dir (parent must exist for validate_drift).
    registry_path: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let nu_exe = dir.path().join("fake_nu");
        std::fs::write(&nu_exe, b"fake nu binary").unwrap();
        let nu_hash = integrity::compute_sha256(b"fake nu binary");

        let registry_dir = dir.path().join("nushell");
        std::fs::create_dir_all(&registry_dir).unwrap();
        let registry_path = registry_dir.join("plugins.msgpackz");

        Self {
            dir,
            nu_hash,
            nu_exe,
            registry_path,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn write_nu_paths(&self) {
        let paths = NuPaths {
            nu_executable: self.nu_exe.to_string_lossy().into_owned(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: self.registry_path.to_string_lossy().into_owned(),
            nu_executable_hash: self.nu_hash.clone(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![],
            vendor_autoload_dir: None,
        };
        paths.save(&self.root()).unwrap();
    }

    /// Create a fake plugin binary in the expected install location and return
    /// the payload_path (relative to root).
    fn create_plugin_binary(&self, owner: &str, name: &str, version: &str) -> String {
        let payload_path = format!("packages/plugins/{owner}/{name}/{version}-abc12345");
        let install_dir = self.root().join(&payload_path);
        std::fs::create_dir_all(&install_dir).unwrap();
        let bin_name = format!("nu_plugin_{name}");
        std::fs::write(install_dir.join(&bin_name), b"fake plugin binary").unwrap();
        payload_path
    }

    fn make_plugin_entry(
        &self,
        owner: &str,
        name: &str,
        version: &str,
        activation: Option<PluginActivation>,
    ) -> LockfileEntry {
        let payload_path = format!("packages/plugins/{owner}/{name}/{version}-abc12345");
        LockfileEntry {
            version: version.to_string(),
            package_type: "plugin".to_string(),
            source: "binary".to_string(),
            target: Some("x86_64-pc-windows-msvc".to_string()),
            artifact_url: None,
            artifact_sha256: Some("abc".to_string()),
            executable_path: Some(format!("nu_plugin_{name}")),
            archive_root: None,
            include: None,
            entry: None,
            installed_at: "0000000000000001".to_string(),
            nu_version_at_install: Some("0.113.1".to_string()),
            activation,
            registry_url: None,
            registry_revision: None,
            index_sha256: None,
            signing_key_fingerprint: None,
            git_url: None,
            git_rev: None,
            cargo_name: None,
            cargo_lock_sha256: None,
            built_sha256: None,
            payload_path,
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: std::collections::BTreeMap::new(),
        }
    }

    fn write_lockfile(&self, lockfile: &Lockfile) {
        lockfile.save(&self.root()).unwrap();
    }

    fn no_args(&self) -> ActivateArgs {
        ActivateArgs {
            packages: vec![],
            verbose: false,
            list: false,
            check: false,
        }
    }

    fn args_for(&self, packages: &[&str]) -> ActivateArgs {
        ActivateArgs {
            packages: packages.iter().map(|s| s.to_string()).collect(),
            verbose: false,
            list: false,
            check: false,
        }
    }
}

fn ok_registrar(_nu: &str, _binary: &str, _config: &str) -> anyhow::Result<()> {
    Ok(())
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[test]
fn test_activate_empty_lockfile() {
    let env = TestEnv::new();
    env.write_nu_paths();
    env.write_lockfile(&Lockfile::empty());

    let result = execute_with_registrar(&env.no_args(), &env.root(), &ok_registrar);
    assert!(
        result.is_ok(),
        "Empty lockfile should succeed (nothing to do): {:?}",
        result
    );
}

#[test]
fn test_activate_skips_already_activated() {
    let env = TestEnv::new();
    env.write_nu_paths();
    env.create_plugin_binary("owner", "myplugin", "1.0.0");

    let activation = Some(PluginActivation {
        plugin_registry_path: env.registry_path.to_string_lossy().into_owned(),
        nu_executable_sha256: env.nu_hash.clone(),
        nu_version: "0.113.1".to_string(),
        activated_at: "0".to_string(),
    });

    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(
        "owner/myplugin".to_string(),
        env.make_plugin_entry("owner", "myplugin", "1.0.0", activation),
    );
    env.write_lockfile(&lockfile);

    // Should report "already active" and exit 0 without calling registrar
    let called = Arc::new(AtomicUsize::new(0));
    let counter = called.clone();
    let result = execute_with_registrar(
        &env.args_for(&["owner/myplugin"]),
        &env.root(),
        &|_nu, _bin, _cfg| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(result.is_ok());
    assert_eq!(
        called.load(Ordering::SeqCst),
        0,
        "Registrar must not be called for already-active plugin"
    );
}

#[test]
fn test_activate_drift_refusal() {
    let env = TestEnv::new();
    env.write_nu_paths();

    // Tamper with the nu binary — hash no longer matches
    std::fs::write(&env.nu_exe, b"modified nu binary").unwrap();

    let result = execute_with_registrar(&env.no_args(), &env.root(), &ok_registrar);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("changed") || msg.contains("mismatch") || msg.contains("refresh"),
        "Expected drift error, got: {msg}"
    );
}

#[test]
fn test_activate_unknown_package_error() {
    let env = TestEnv::new();
    env.write_nu_paths();
    env.write_lockfile(&Lockfile::empty());

    let result = execute_with_registrar(
        &env.args_for(&["owner/nonexistent"]),
        &env.root(),
        &ok_registrar,
    );
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not found") || msg.contains("nonexistent"),
        "Expected not-found error, got: {msg}"
    );
}

#[test]
fn test_activate_non_plugin_error() {
    let env = TestEnv::new();
    env.write_nu_paths();

    let mut lockfile = Lockfile::empty();
    let mut module_entry = env.make_plugin_entry("owner", "mymodule", "1.0.0", None);
    module_entry.package_type = "module".to_string();
    lockfile
        .packages
        .insert("owner/mymodule".to_string(), module_entry);
    env.write_lockfile(&lockfile);

    let result = execute_with_registrar(
        &env.args_for(&["owner/mymodule"]),
        &env.root(),
        &ok_registrar,
    );
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("module") || msg.contains("plugin"),
        "Expected type error, got: {msg}"
    );
}

#[test]
fn test_activate_path_with_spaces() {
    let dir = TempDir::new().unwrap();
    let nu_exe = dir.path().join("fake nu");
    std::fs::write(&nu_exe, b"fake nu binary").unwrap();
    let nu_hash = integrity::compute_sha256(b"fake nu binary");

    let registry_dir = dir.path().join("nushell dir");
    std::fs::create_dir_all(&registry_dir).unwrap();
    let registry_path = registry_dir.join("plugins.msgpackz");

    let paths = NuPaths {
        nu_executable: nu_exe.to_string_lossy().into_owned(),
        nu_version: "0.113.1".to_string(),
        plugin_registry_path: registry_path.to_string_lossy().into_owned(),
        nu_executable_hash: nu_hash.clone(),
        platform: "x86_64-pc-windows-msvc".to_string(),
        data_dir: None,
        vendor_autoload_dirs: vec![],
        vendor_autoload_dir: None,
    };
    let root = dir.path().to_path_buf();
    paths.save(&root).unwrap();

    // Create plugin binary in a path with spaces
    let payload = "packages/plugins/owner/my plugin/1.0.0-abc";
    std::fs::create_dir_all(dir.path().join(payload)).unwrap();
    std::fs::write(dir.path().join(payload).join("nu_plugin_plugin"), b"fake").unwrap();

    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(
        "owner/my plugin".to_string(),
        LockfileEntry {
            version: "1.0.0".to_string(),
            package_type: "plugin".to_string(),
            source: "binary".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: Some("nu_plugin_plugin".to_string()),
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
            payload_path: payload.to_string(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: std::collections::BTreeMap::new(),
        },
    );
    lockfile.save(&root).unwrap();

    // Verify the registrar receives the binary path via env var (not shell-interpolated)
    let received_binary = Arc::new(std::sync::Mutex::new(String::new()));
    let received_config = Arc::new(std::sync::Mutex::new(String::new()));
    let bin_capture = received_binary.clone();
    let cfg_capture = received_config.clone();

    let args = ActivateArgs {
        packages: vec![],
        verbose: false,
        list: false,
        check: false,
    };
    let result = execute_with_registrar(&args, &root, &|_nu, bin, cfg| {
        *bin_capture.lock().unwrap() = bin.to_string();
        *cfg_capture.lock().unwrap() = cfg.to_string();
        Ok(())
    });

    assert!(
        result.is_ok(),
        "Path with spaces should succeed: {:?}",
        result
    );
    let bin = received_binary.lock().unwrap().clone();
    let cfg = received_config.lock().unwrap().clone();
    // Paths with spaces come through as-is (not shell-quoted), because they go via env var
    assert!(
        bin.contains("nu_plugin_plugin"),
        "Binary path should contain plugin name, got: {bin}"
    );
    assert!(
        cfg.contains("plugins.msgpackz"),
        "Config path should match registry, got: {cfg}"
    );
}

#[test]
fn test_activate_partial_failure_nonzero() {
    let env = TestEnv::new();
    env.write_nu_paths();
    env.create_plugin_binary("owner", "plugin1", "1.0.0");
    env.create_plugin_binary("owner", "plugin2", "1.0.0");

    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(
        "owner/plugin1".to_string(),
        env.make_plugin_entry("owner", "plugin1", "1.0.0", None),
    );
    lockfile.packages.insert(
        "owner/plugin2".to_string(),
        env.make_plugin_entry("owner", "plugin2", "1.0.0", None),
    );
    env.write_lockfile(&lockfile);

    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = call_count.clone();

    let result = execute_with_registrar(&env.no_args(), &env.root(), &|_nu, _binary, _cfg| {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        // First call succeeds, second fails
        if n == 0 {
            Ok(())
        } else {
            bail!("Simulated failure for second plugin")
        }
    });

    // Must return error (nonzero) when any plugin fails
    assert!(result.is_err(), "Expected error due to partial failure");

    // Successful plugin must be persisted in lockfile
    let reloaded = Lockfile::load(&env.root()).unwrap();
    let activated_count = reloaded
        .packages
        .values()
        .filter(|e| e.activation.is_some())
        .count();
    assert_eq!(
        activated_count, 1,
        "One plugin should be persisted as activated"
    );
}

#[test]
fn test_activate_idempotent() {
    let env = TestEnv::new();
    env.write_nu_paths();
    env.create_plugin_binary("owner", "idempotent", "1.0.0");

    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(
        "owner/idempotent".to_string(),
        env.make_plugin_entry("owner", "idempotent", "1.0.0", None),
    );
    env.write_lockfile(&lockfile);

    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = call_count.clone();
    let registrar = |_nu: &str, _bin: &str, _cfg: &str| -> anyhow::Result<()> {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };

    // First activation
    execute_with_registrar(&env.no_args(), &env.root(), &registrar).unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    // Second activation — plugin is now active; no-op
    execute_with_registrar(&env.no_args(), &env.root(), &registrar).unwrap();
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "Registrar must not be called again for already-active plugin"
    );
}

#[test]
fn test_activate_journal_after_prepared_interruption() {
    let env = TestEnv::new();
    env.write_nu_paths();
    env.create_plugin_binary("owner", "interrupted", "1.0.0");

    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(
        "owner/interrupted".to_string(),
        env.make_plugin_entry("owner", "interrupted", "1.0.0", None),
    );
    env.write_lockfile(&lockfile);

    // Simulate a crash between journal write and plugin add: write a `prepared` journal manually
    let journal = PendingActivation {
        nu_executable_sha256: env.nu_hash.clone(),
        nu_version: "0.113.1".to_string(),
        plugin_registry_path: env.registry_path.to_string_lossy().into_owned(),
        created_at: "0000000000000001".to_string(),
        entries: vec![PendingActivationEntry {
            package_id: "owner/interrupted".to_string(),
            payload_path: "packages/plugins/owner/interrupted/1.0.0-abc12345".to_string(),
            executable_path: "nu_plugin_interrupted".to_string(),
            absolute_binary_path: env
                .root()
                .join("packages/plugins/owner/interrupted/1.0.0-abc12345/nu_plugin_interrupted")
                .to_string_lossy()
                .into_owned(),
            status: PendingStatus::Prepared,
            error: None,
        }],
    };
    journal.save(&env.root()).unwrap();

    // Activate should reconcile and re-attempt the prepared plugin
    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = call_count.clone();
    let result = execute_with_registrar(&env.no_args(), &env.root(), &|_nu, _bin, _cfg| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    assert!(
        result.is_ok(),
        "Interrupted `prepared` entry should be retried: {:?}",
        result
    );
    // Journal should be cleaned up
    assert!(
        PendingActivation::load(&env.root()).unwrap().is_none(),
        "Journal must be removed after completion"
    );
}

#[test]
fn test_activate_journal_registered_interruption_reconciles() {
    let env = TestEnv::new();
    env.write_nu_paths();
    env.create_plugin_binary("owner", "registered", "1.0.0");

    // Lockfile does NOT yet have the activation record (simulates crash after plugin add
    // but before lockfile.save)
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(
        "owner/registered".to_string(),
        env.make_plugin_entry("owner", "registered", "1.0.0", None),
    );
    env.write_lockfile(&lockfile);

    // Write a `registered` journal entry — simulates crash after plugin add
    let journal = PendingActivation {
        nu_executable_sha256: env.nu_hash.clone(),
        nu_version: "0.113.1".to_string(),
        plugin_registry_path: env.registry_path.to_string_lossy().into_owned(),
        created_at: "0000000000000001".to_string(),
        entries: vec![PendingActivationEntry {
            package_id: "owner/registered".to_string(),
            payload_path: "packages/plugins/owner/registered/1.0.0-abc12345".to_string(),
            executable_path: "nu_plugin_registered".to_string(),
            absolute_binary_path: env
                .root()
                .join("packages/plugins/owner/registered/1.0.0-abc12345/nu_plugin_registered")
                .to_string_lossy()
                .into_owned(),
            status: PendingStatus::Registered,
            error: None,
        }],
    };
    journal.save(&env.root()).unwrap();

    // Activation should reconcile the `registered` entry without calling plugin add again
    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = call_count.clone();
    let result = execute_with_registrar(&env.no_args(), &env.root(), &|_nu, _bin, _cfg| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert!(
        result.is_ok(),
        "Registered-entry reconciliation should succeed: {:?}",
        result
    );

    // Lockfile should now have the activation record
    let reloaded = Lockfile::load(&env.root()).unwrap();
    let entry = reloaded.packages.get("owner/registered").unwrap();
    assert!(
        entry.activation.is_some(),
        "Reconciled plugin must have activation record"
    );

    // plugin add must not have been called (already registered)
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        0,
        "Registrar must not be called for `registered` entries"
    );

    // Journal must be removed
    assert!(PendingActivation::load(&env.root()).unwrap().is_none());
}

#[test]
fn test_activate_stale_journal_requires_refresh() {
    let env = TestEnv::new();
    env.write_nu_paths();

    // Journal with DIFFERENT Nu hash — stale
    let journal = PendingActivation {
        nu_executable_sha256: "different_hash_entirely".to_string(),
        nu_version: "0.112.0".to_string(),
        plugin_registry_path: env.registry_path.to_string_lossy().into_owned(),
        created_at: "0000000000000001".to_string(),
        entries: vec![],
    };
    journal.save(&env.root()).unwrap();
    env.write_lockfile(&Lockfile::empty());

    let result = execute_with_registrar(&env.no_args(), &env.root(), &ok_registrar);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("refresh") || msg.contains("identity") || msg.contains("different"),
        "Expected stale-journal error, got: {msg}"
    );
}

#[test]
fn test_activate_auto_confirms_on_no_tty() {
    // With the shared confirm utility, non-TTY sessions auto-confirm instead of
    // bailing. Without --yes and no TTY, activate should proceed. The TTY
    // branches of `confirm_or_auto` are covered by the unconditional unit tests
    // in `src/util/confirm.rs`; this end-to-end path now runs everywhere.

    let env = TestEnv::new();
    env.write_nu_paths();
    env.create_plugin_binary("owner", "guarded", "1.0.0");

    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(
        "owner/guarded".to_string(),
        env.make_plugin_entry("owner", "guarded", "1.0.0", None),
    );
    env.write_lockfile(&lockfile);

    let args = ActivateArgs {
        packages: vec![],
        yes: false,
        verbose: false,
        list: false,
        check: false,
    };
    let result = execute_with_registrar(&args, &env.root(), &ok_registrar);
    assert!(
        result.is_ok(),
        "non-TTY should auto-confirm, got error: {:?}",
        result.err()
    );
}

// ─── unit tests for validate_payload_path ───────────────────────────────────

#[test]
fn validate_rejects_absolute_payload() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let err = validate_payload_path(&root, "/absolute/path", "nu_plugin_x").unwrap_err();
    assert!(err.to_string().contains("absolute"));
}

#[test]
fn validate_rejects_traversal_in_payload() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let err = validate_payload_path(&root, "packages/../../../etc", "nu_plugin_x").unwrap_err();
    assert!(err.to_string().contains("..") || err.to_string().contains("traversal"));
}

#[test]
fn validate_rejects_traversal_in_executable() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(dir.path().join("packages")).unwrap();
    let err = validate_payload_path(&root, "packages", "../../etc/shadow").unwrap_err();
    assert!(err.to_string().contains("..") || err.to_string().contains("traversal"));
}

#[test]
fn validate_rejects_wrong_name_prefix() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let payload = "packages/plugins/o/n/1.0.0-abc";
    std::fs::create_dir_all(dir.path().join(payload)).unwrap();
    std::fs::write(dir.path().join(payload).join("wrong_name"), b"fake").unwrap();
    let err = validate_payload_path(&root, payload, "wrong_name").unwrap_err();
    assert!(err.to_string().contains("nu_plugin_"));
}

#[test]
fn validate_accepts_valid_plugin() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let payload = "packages/plugins/o/n/1.0.0-abc";
    std::fs::create_dir_all(dir.path().join(payload)).unwrap();
    std::fs::write(dir.path().join(payload).join("nu_plugin_n"), b"fake").unwrap();
    let result = validate_payload_path(&root, payload, "nu_plugin_n");
    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
}

#[test]
fn validate_accepts_windows_exe_suffix() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let payload = "packages/plugins/o/n/1.0.0-abc";
    std::fs::create_dir_all(dir.path().join(payload)).unwrap();
    std::fs::write(dir.path().join(payload).join("nu_plugin_n.exe"), b"fake").unwrap();
    let result = validate_payload_path(&root, payload, "nu_plugin_n.exe");
    assert!(
        result.is_ok(),
        "Expected Ok for .exe suffix, got {:?}",
        result
    );
}
