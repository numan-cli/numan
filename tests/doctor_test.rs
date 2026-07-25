//! Integration tests for `numan doctor`.

use numan_cli::cmd::deactivate::DeactivateArgs;
use numan_cli::cmd::doctor::{
    execute_with_options, run_checks_with_options, DoctorArgs, DoctorOptions, Severity,
};
use numan_cli::cmd::init::{execute_with_runner, InitArgs};
use numan_cli::core::integrity;
use numan_cli::nu::autoload::FakeCandidateRunner;
use numan_cli::nu::bootstrap::managed_nu_binary;
use numan_cli::nu::paths::NuPaths;
use numan_cli::state::journal::{PendingActivation, PendingActivationEntry, PendingStatus};
use numan_cli::state::plugin_deactivate_journal::{
    PendingPluginDeactivate, PendingPluginDeactivateEntry, PluginDeactivateStatus,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tempfile::TempDir;

static TEST_OFF_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static TEST_NU_SETUP_CALLED: Mutex<bool> = Mutex::new(false);
static TEST_DEACTIVATE_REPAIR_CALLED: Mutex<bool> = Mutex::new(false);
static TEST_DEACTIVATE_REPAIR_SHOULD_FAIL: Mutex<bool> = Mutex::new(false);
static TEST_DEACTIVATE_REPAIR_GUARD: Mutex<()> = Mutex::new(());
static TEST_PATH_GUARD: Mutex<()> = Mutex::new(());

fn discover_off_path_test() -> Option<PathBuf> {
    TEST_OFF_PATH.lock().ok()?.clone()
}

fn nu_setup_repair_test(
    args: &numan_cli::cmd::setup::NuSetupArgs,
    _root: &Path,
) -> anyhow::Result<()> {
    let expected = TEST_OFF_PATH.lock().unwrap();
    assert_eq!(
        args.use_existing.as_deref(),
        expected.as_ref().map(|p| p.as_path())
    );
    assert!(args.yes);
    *TEST_NU_SETUP_CALLED.lock().unwrap() = true;
    Ok(())
}

struct ClearedPath {
    saved: Option<String>,
}

impl ClearedPath {
    fn new() -> Self {
        let saved = std::env::var("PATH").ok();
        std::env::set_var("PATH", "");
        Self { saved }
    }
}

impl Drop for ClearedPath {
    fn drop(&mut self) {
        match &self.saved {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

fn fake_runner(_exe: &str) -> Box<dyn numan_cli::nu::autoload::CandidateRunner> {
    Box::new(FakeCandidateRunner::success())
}

fn fake_init(args: &InitArgs, root: &Path) -> anyhow::Result<()> {
    let nu_exe = managed_nu_binary(root);
    std::fs::create_dir_all(nu_exe.parent().unwrap()).unwrap();
    std::fs::write(&nu_exe, b"nu").unwrap();
    let bytes = std::fs::read(&nu_exe).unwrap();
    let paths = NuPaths {
        nu_executable: nu_exe.to_string_lossy().into_owned(),
        nu_version: "0.113.1".to_string(),
        plugin_registry_path: root.join("plugins.msgpackz").to_string_lossy().into_owned(),
        nu_executable_hash: integrity::compute_sha256(&bytes),
        platform: "test".to_string(),
        data_dir: None,
        vendor_autoload_dirs: vec![],
        vendor_autoload_dir: None,
    };
    execute_with_runner(args, root, move || Ok(paths.clone()), fake_runner)
}

#[test]
fn doctor_report_only_leaves_root_unchanged() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root).unwrap();

    let args = DoctorArgs {
        fix: false,
        yes: false,
        json: false,
        nupm_home: None,
    };
    execute_with_options(&args, root, test_doctor_options()).unwrap();
    assert!(!root.join("nu_state/paths.json").exists());
}

#[test]
fn doctor_fix_auto_creates_layout_without_network() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root).unwrap();
    std::env::set_var("NUMAN_ROOT", root);
    let nu_exe = managed_nu_binary(root);
    std::fs::create_dir_all(nu_exe.parent().unwrap()).unwrap();
    std::fs::write(&nu_exe, b"nu").unwrap();

    let args = DoctorArgs {
        fix: true,
        yes: true,
        json: false,
        nupm_home: None,
    };
    let code = execute_with_options(
        &args,
        root,
        DoctorOptions {
            init_repair: Some(fake_init),
            ..test_doctor_options()
        },
    )
    .unwrap();
    assert_eq!(code, 0);
    assert!(root.join("state").is_dir());
    assert!(root.join("nu_state/paths.json").is_file());
}

#[test]
fn doctor_reports_pending_plugin_journal() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fake_init(&InitArgs { refresh: false }, root).unwrap();

    let paths = NuPaths::load(root).unwrap();
    let journal = PendingActivation {
        nu_executable_sha256: paths.nu_executable_hash.clone(),
        nu_version: paths.nu_version.clone(),
        plugin_registry_path: paths.plugin_registry_path.clone(),
        created_at: "now".to_string(),
        entries: vec![PendingActivationEntry {
            package_id: "owner/plugin".to_string(),
            payload_path: "packages/plugins/owner/plugin/1.0.0-abc".to_string(),
            executable_path: "nu_plugin_test".to_string(),
            absolute_binary_path: root
                .join("packages/plugins/owner/plugin/1.0.0-abc/nu_plugin_test")
                .to_string_lossy()
                .into_owned(),
            status: PendingStatus::Prepared,
            error: None,
        }],
    };
    journal.save(root).unwrap();

    let args = DoctorArgs {
        fix: false,
        yes: false,
        json: false,
        nupm_home: None,
    };
    let report = run_checks_with_options(&args, root, &test_doctor_options()).unwrap();
    assert!(report
        .findings
        .iter()
        .any(|f| f.id == "journal.plugin_pending"));
}

#[test]
fn doctor_detects_nu_path_drift() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let nu_exe = root.join("nu");
    std::fs::write(&nu_exe, b"v1").unwrap();
    let paths = NuPaths {
        nu_executable: nu_exe.to_string_lossy().into_owned(),
        nu_version: "0.113.1".to_string(),
        plugin_registry_path: root.join("plugins.msgpackz").to_string_lossy().into_owned(),
        nu_executable_hash: integrity::compute_sha256(b"stale"),
        platform: "test".to_string(),
        data_dir: None,
        vendor_autoload_dirs: vec![],
        vendor_autoload_dir: None,
    };
    paths.save(root).unwrap();

    let args = DoctorArgs {
        fix: false,
        yes: false,
        json: false,
        nupm_home: None,
    };
    let report = run_checks_with_options(&args, root, &test_doctor_options()).unwrap();
    assert!(report
        .findings
        .iter()
        .any(|f| f.id == "nu_paths.drift" && f.severity == Severity::Error));
}

#[test]
fn doctor_reports_off_path_nu_without_download() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root).unwrap();

    let off_path = root.join("off-path-nu.exe");
    std::fs::write(&off_path, b"fake nu").unwrap();

    *TEST_OFF_PATH.lock().unwrap() = Some(off_path.clone());

    let _path_guard = TEST_PATH_GUARD.lock().unwrap();
    let _cleared_path = ClearedPath::new();
    let args = DoctorArgs {
        fix: false,
        yes: false,
        json: false,
        nupm_home: None,
    };
    let report = run_checks_with_options(
        &args,
        root,
        &DoctorOptions {
            discover_off_path: Some(discover_off_path_test),
            ..test_doctor_options()
        },
    )
    .unwrap();

    let finding = report
        .findings
        .iter()
        .find(|f| f.id == "nu.binary.found_off_path")
        .expect("expected nu.binary.found_off_path");
    assert_eq!(finding.severity, Severity::Warn);
    assert!(finding.fix.as_deref().unwrap().contains("--use-existing"));

    let missing = report
        .findings
        .iter()
        .find(|f| f.id == "nu.binary.missing_on_path")
        .expect("expected nu.binary.missing_on_path");
    assert_eq!(missing.severity, Severity::Ok);
}

#[test]
fn doctor_fix_registers_off_path_nu_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root).unwrap();

    let off_path = root.join("off-path-nu.exe");
    std::fs::write(&off_path, b"fake nu").unwrap();

    *TEST_OFF_PATH.lock().unwrap() = Some(off_path.clone());
    *TEST_NU_SETUP_CALLED.lock().unwrap() = false;

    let _path_guard = TEST_PATH_GUARD.lock().unwrap();
    let _cleared_path = ClearedPath::new();
    let args = DoctorArgs {
        fix: true,
        yes: true,
        json: false,
        nupm_home: None,
    };
    let code = execute_with_options(
        &args,
        root,
        DoctorOptions {
            skip_network: true,
            nu_setup_repair: Some(nu_setup_repair_test),
            discover_off_path: Some(discover_off_path_test),
            ..test_doctor_options()
        },
    )
    .unwrap();

    assert!(*TEST_NU_SETUP_CALLED.lock().unwrap());
    assert_eq!(code, 1);
}

fn fake_deactivate_repair(args: &DeactivateArgs, root: &Path) -> anyhow::Result<()> {
    assert!(args.yes);
    assert_eq!(args.packages, vec!["owner/plugin".to_string()]);
    *TEST_DEACTIVATE_REPAIR_CALLED.lock().unwrap() = true;
    if *TEST_DEACTIVATE_REPAIR_SHOULD_FAIL.lock().unwrap() {
        anyhow::bail!("injected deactivate repair failure");
    }
    PendingPluginDeactivate::delete(root)?;
    Ok(())
}

fn write_plugin_deactivate_journal(root: &Path, paths: &NuPaths) {
    PendingPluginDeactivate {
        nu_executable_sha256: paths.nu_executable_hash.clone(),
        nu_version: paths.nu_version.clone(),
        plugin_registry_path: paths.plugin_registry_path.clone(),
        created_at: "now".to_string(),
        entries: vec![PendingPluginDeactivateEntry {
            package_id: "owner/plugin".to_string(),
            plugin_name: "plugin".to_string(),
            absolute_binary_path: root
                .join("packages/plugins/owner/plugin/1.0.0-abc/nu_plugin_plugin")
                .to_string_lossy()
                .into_owned(),
            status: PluginDeactivateStatus::Prepared,
            error: None,
        }],
    }
    .save(root)
    .unwrap();
}

#[test]
fn doctor_fix_reconciles_pending_plugin_deactivate_journal() {
    let _guard = TEST_DEACTIVATE_REPAIR_GUARD.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fake_init(&InitArgs { refresh: false }, root).unwrap();
    let paths = NuPaths::load(root).unwrap();
    write_plugin_deactivate_journal(root, &paths);

    *TEST_DEACTIVATE_REPAIR_CALLED.lock().unwrap() = false;
    *TEST_DEACTIVATE_REPAIR_SHOULD_FAIL.lock().unwrap() = false;

    let args = DoctorArgs {
        fix: true,
        yes: true,
        json: false,
        nupm_home: None,
    };
    let code = execute_with_options(
        &args,
        root,
        DoctorOptions {
            skip_network: true,
            init_repair: Some(fake_init),
            deactivate_repair: Some(fake_deactivate_repair),
            ..test_doctor_options()
        },
    )
    .unwrap();

    assert_eq!(code, 0);
    assert!(*TEST_DEACTIVATE_REPAIR_CALLED.lock().unwrap());
    assert!(PendingPluginDeactivate::load(root).unwrap().is_none());
}

#[test]
fn doctor_fix_stale_plugin_deactivate_runs_refresh_then_deactivate() {
    let _guard = TEST_DEACTIVATE_REPAIR_GUARD.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fake_init(&InitArgs { refresh: false }, root).unwrap();
    let mut paths = NuPaths::load(root).unwrap();
    // Journal identity differs from cached paths → stale finding.
    paths.nu_executable_hash = "stale-journal-hash".to_string();
    write_plugin_deactivate_journal(root, &paths);

    *TEST_DEACTIVATE_REPAIR_CALLED.lock().unwrap() = false;
    *TEST_DEACTIVATE_REPAIR_SHOULD_FAIL.lock().unwrap() = false;

    let args = DoctorArgs {
        fix: true,
        yes: true,
        json: false,
        nupm_home: None,
    };
    let code = execute_with_options(
        &args,
        root,
        DoctorOptions {
            skip_network: true,
            init_repair: Some(fake_init),
            deactivate_repair: Some(fake_deactivate_repair),
            ..test_doctor_options()
        },
    )
    .unwrap();

    assert_eq!(code, 0);
    assert!(*TEST_DEACTIVATE_REPAIR_CALLED.lock().unwrap());
    assert!(PendingPluginDeactivate::load(root).unwrap().is_none());
}

#[test]
fn doctor_fix_reports_deactivate_repair_failure() {
    let _guard = TEST_DEACTIVATE_REPAIR_GUARD.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fake_init(&InitArgs { refresh: false }, root).unwrap();
    let paths = NuPaths::load(root).unwrap();
    write_plugin_deactivate_journal(root, &paths);

    *TEST_DEACTIVATE_REPAIR_CALLED.lock().unwrap() = false;
    *TEST_DEACTIVATE_REPAIR_SHOULD_FAIL.lock().unwrap() = true;

    let args = DoctorArgs {
        fix: true,
        yes: true,
        json: true,
        nupm_home: None,
    };
    let code = execute_with_options(
        &args,
        root,
        DoctorOptions {
            skip_network: true,
            init_repair: Some(fake_init),
            deactivate_repair: Some(fake_deactivate_repair),
            ..test_doctor_options()
        },
    )
    .unwrap();

    assert!(*TEST_DEACTIVATE_REPAIR_CALLED.lock().unwrap());
    assert!(PendingPluginDeactivate::load(root).unwrap().is_some());
    // Pending journal remains a warning after failed repair.
    assert_eq!(code, 0);
}

fn probe_fixed_version(_path: &Path) -> anyhow::Result<String> {
    Ok("0.99.9".to_string())
}

/// Skip network and never exec a real `nu` during doctor integration tests.
fn test_doctor_options() -> DoctorOptions {
    DoctorOptions {
        skip_network: true,
        nu_version_probe: Some(probe_fixed_version),
        ..DoctorOptions::default()
    }
}

#[test]
fn doctor_reports_path_nu_not_found_when_path_cleared() {
    let _path_guard = TEST_PATH_GUARD.lock().unwrap();
    let _cleared_path = ClearedPath::new();

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root).unwrap();

    let report = run_checks_with_options(
        &DoctorArgs {
            fix: false,
            yes: false,
            json: true,
            nupm_home: None,
        },
        root,
        &DoctorOptions {
            discover_off_path: Some(|| None),
            ..test_doctor_options()
        },
    )
    .unwrap();

    let path_finding = report
        .findings
        .iter()
        .find(|f| f.id == "nu.path.version")
        .expect("nu.path.version");
    assert_eq!(path_finding.message, "PATH Nu: not found");
    assert_eq!(path_finding.severity, Severity::Info);
}

#[test]
fn doctor_reports_managed_and_trust_root_findings() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let managed = managed_nu_binary(root);
    std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
    std::fs::write(&managed, b"nu").unwrap();

    let mut config = numan_cli::config::Config::default();
    config.registries.insert(
        "official".to_string(),
        numan_cli::config::RegistryConfig {
            url: "https://example.invalid/registry".to_string(),
            sync_interval: "24h".to_string(),
            enabled: true,
            trust_key: None,
        },
    );
    config.save(root).unwrap();

    let report = run_checks_with_options(
        &DoctorArgs {
            fix: false,
            yes: false,
            json: true,
            nupm_home: None,
        },
        root,
        &test_doctor_options(),
    )
    .unwrap();

    let managed_finding = report
        .findings
        .iter()
        .find(|f| f.id == "nu.managed.version")
        .expect("nu.managed.version");
    assert!(
        managed_finding.message.starts_with("Managed Nu: 0.99.9"),
        "unexpected: {}",
        managed_finding.message
    );

    let trust = report
        .findings
        .iter()
        .find(|f| f.id == "registry.trust_root")
        .expect("registry.trust_root");
    assert!(
        trust
            .message
            .contains(numan_cli::core::official_registry::OFFICIAL_REGISTRY.key_id),
        "unexpected: {}",
        trust.message
    );

    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("nu.path.version"));
    assert!(json.contains("nu.managed.version"));
    assert!(json.contains("registry.trust_root"));
}
