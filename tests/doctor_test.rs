//! Integration tests for `numan doctor`.

use numan_cli::cmd::deactivate::DeactivateArgs;
use numan_cli::cmd::doctor::{
    execute_with_options, run_checks_with_options, DoctorArgs, DoctorOptions, Severity,
};
use numan_cli::cmd::init::{execute_with_runner, InitArgs};
use numan_cli::cmd::setup::{self, NuAction};
use numan_cli::core::integrity;
use numan_cli::nu::autoload::FakeCandidateRunner;
use numan_cli::nu::bootstrap::managed_nu_binary;
use numan_cli::nu::paths::NuPaths;
use numan_cli::state::journal::{PendingActivation, PendingActivationEntry, PendingStatus};
use numan_cli::state::plugin_deactivate_journal::{
    PendingPluginDeactivate, PendingPluginDeactivateEntry, PluginDeactivateStatus,
};
use numan_cli::state::snapshot::{list_snapshots, SnapshotTrigger};
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
    root: &Path,
) -> anyhow::Result<()> {
    let expected = TEST_OFF_PATH.lock().unwrap().clone();
    // The doctor passes the off-path binary via NuSetupArgs::use_existing(),
    // which sets action = Some(NuAction::Use { path, force }) and leaves use_existing unset.
    let Some(NuAction::Use { path, .. }) = &args.action else {
        panic!("expected NuAction::Use, got {:?}", args.action);
    };
    assert_eq!(Some(path.as_path()), expected.as_ref().map(|p| p.as_path()));
    assert!(
        args.use_existing.is_none(),
        "doctor must not use the deprecated flag"
    );
    // Doctor must not auto-approve consented wipe of a managed install.
    assert!(
        !args.yes,
        "doctor found_off_path repair must not pass --yes"
    );
    *TEST_NU_SETUP_CALLED.lock().unwrap() = true;

    // Seed a managed install just before the production use path. Seeding earlier
    // would make Nu "available" and suppress `nu.binary.found_off_path`.
    let managed = managed_nu_binary(root);
    std::fs::create_dir_all(managed.parent().unwrap())?;
    std::fs::write(&managed, b"managed-nu")?;
    // Use a missing binary so execute_nu fails before PATH persistence or wipe.
    let missing = root.join("missing-off-path-nu");
    let err = setup::execute_nu(
        &setup::NuSetupArgs::use_existing(missing, args.yes, false),
        root,
    )
    .expect_err("expected resolve failure for missing off-PATH binary");
    assert!(
        managed.exists(),
        "doctor off-PATH repair must not delete managed tools/nushell without consent: {err}"
    );
    Err(err)
}

/// Valid fake Nu for consent-gate tests (Unix). Must look runnable to
/// `validate_nushell_binary` so the failure is the wipe/PATH consent gate.
#[cfg(unix)]
fn write_valid_off_path_nu(tmp: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = tmp.join("valid-off-path-nu");
    let script: &[u8] = b"#!/bin/sh\n\
case $1 in\n\
  -c|--version) printf '{ \"version\":\"0.113.1\", \"plugin_path\":\"/tmp\", \"data_dir\":\"/tmp\", \"vendor_autoload_dirs\":[\"/tmp/vendor/autoload\"] }\n' ;;\n\
esac\n";
    std::fs::write(&bin, script).expect("write valid off-path nu");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
        .expect("chmod valid off-path nu");
    bin
}

#[cfg(unix)]
#[test]
fn execute_nu_use_existing_refuses_without_consent_and_keeps_managed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root).unwrap();

    let managed = managed_nu_binary(root);
    std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
    std::fs::write(&managed, b"managed-nu").unwrap();

    let off_path = write_valid_off_path_nu(dir.path());
    let err = setup::execute_nu(
        &setup::NuSetupArgs::use_existing(off_path, false, false),
        root,
    )
    .expect_err("expected consent-gate refusal with yes=false");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("non-interactive") || msg.contains("--yes"),
        "expected require_tty_or_yes consent refusal, got: {msg}"
    );
    assert!(
        managed.exists(),
        "managed install must remain when consent gate refuses off-PATH registration"
    );
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
        scan: true,
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
        scan: false,
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
    let snapshots = list_snapshots(root).unwrap();
    assert!(
        snapshots
            .iter()
            .any(|s| s.trigger == SnapshotTrigger::Doctor),
        "default doctor repairs must create a PreMutation Doctor snapshot"
    );
}

#[test]
fn doctor_repairs_malformed_active_version_marker() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("nu_state")).unwrap();
    std::fs::write(root.join("nu_state/active-version.json"), b"{not-json").unwrap();
    let nu_exe = managed_nu_binary(root);
    std::fs::create_dir_all(nu_exe.parent().unwrap()).unwrap();
    std::fs::write(&nu_exe, b"nu").unwrap();

    let scan = run_checks_with_options(
        &DoctorArgs {
            scan: true,
            json: true,
            nupm_home: None,
        },
        root,
        &test_doctor_options(),
    )
    .unwrap();
    let finding = scan
        .findings
        .iter()
        .find(|f| f.id == "nu.active_version.malformed")
        .expect("expected nu.active_version.malformed");
    assert_eq!(finding.severity, Severity::Error);

    let code = execute_with_options(
        &DoctorArgs {
            scan: false,
            json: true,
            nupm_home: None,
        },
        root,
        DoctorOptions {
            init_repair: Some(fake_init),
            ..test_doctor_options()
        },
    )
    .unwrap();
    assert_eq!(
        code, 0,
        "clearing the marker should leave no remaining errors"
    );
    assert!(
        !root.join("nu_state/active-version.json").exists(),
        "doctor must clear the malformed active-version marker"
    );
    let after = run_checks_with_options(
        &DoctorArgs {
            scan: true,
            json: true,
            nupm_home: None,
        },
        root,
        &test_doctor_options(),
    )
    .unwrap();
    let repaired = after
        .findings
        .iter()
        .find(|f| f.id == "nu.active_version.malformed")
        .expect("finding remains as ok after repair");
    assert_eq!(repaired.severity, Severity::Ok);
}

#[test]
fn doctor_repairs_layout_when_lockfile_malformed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("lockfile"), b"{not-valid-json").unwrap();
    let nu_exe = managed_nu_binary(root);
    std::fs::create_dir_all(nu_exe.parent().unwrap()).unwrap();
    std::fs::write(&nu_exe, b"nu").unwrap();
    // Seed a cached index so default repair mode does not attempt registry sync.
    let index_path = root.join("registry/official/index.json");
    std::fs::create_dir_all(index_path.parent().unwrap()).unwrap();
    std::fs::write(
        &index_path,
        r#"{"schema_version":1,"packages":[],"generated_at":"test"}"#,
    )
    .unwrap();

    // Capture the JSON repair report via subprocess stdout (same pattern as
    // doctor_json_default_stdout_is_valid_json). execute_with_options returns
    // only an exit code, so the repairs contract must be asserted from --json.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_numan"))
        .args([
            "--root",
            root.to_str().expect("temp root is utf-8"),
            "doctor",
            "--json",
        ])
        .env("NUMAN_ALLOW_UNSIGNED", "1")
        .output()
        .expect("run numan doctor --json with malformed lockfile");

    assert_eq!(
        output.status.code(),
        Some(1),
        "malformed lockfile leaves error-severity findings; expected exit 1\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json stdout must be valid JSON after malformed lockfile: {e}\nstdout={stdout}\nstderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let repairs = value
        .get("repairs")
        .and_then(|r| r.as_array())
        .unwrap_or_else(|| panic!("doctor --json must include repairs array: {value}"));

    let snapshot = repairs
        .iter()
        .find(|r| r.get("id").and_then(|id| id.as_str()) == Some("snapshot.pre_mutation"))
        .unwrap_or_else(|| panic!("expected snapshot.pre_mutation repair record: {repairs:?}"));
    assert_eq!(
        snapshot.get("status").and_then(|s| s.as_str()),
        Some("failed"),
        "snapshot.pre_mutation must be failed: {snapshot}"
    );

    let skipped_snapshot_deps: Vec<_> = repairs
        .iter()
        .filter(|r| {
            r.get("status").and_then(|s| s.as_str()) == Some("skipped")
                && r.get("reason").and_then(|s| s.as_str()) == Some("snapshot_unavailable")
        })
        .collect();
    assert!(
        !skipped_snapshot_deps.is_empty(),
        "snapshot-dependent repairs must be skipped with snapshot_unavailable: {repairs:?}"
    );

    assert!(
        root.join("state").is_dir(),
        "layout.state must still be created when PreMutation snapshot fails"
    );
    assert!(
        root.join("packages").is_dir(),
        "layout.packages must still be created when PreMutation snapshot fails"
    );
    assert!(
        root.join("registries").is_dir(),
        "layout.registries must still be created when PreMutation snapshot fails"
    );
    assert!(
        list_snapshots(root)
            .unwrap()
            .iter()
            .all(|s| s.trigger != SnapshotTrigger::Doctor),
        "malformed lockfile must not publish a Doctor PreMutation snapshot"
    );
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
        scan: true,
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
        scan: true,
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
        scan: true,
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
    assert!(finding.fix.as_deref().unwrap().contains("setup nu use"));

    let missing = report
        .findings
        .iter()
        .find(|f| f.id == "nu.binary.missing_on_path")
        .expect("expected nu.binary.missing_on_path");
    assert_eq!(missing.severity, Severity::Ok);
}

fn nu_setup_must_not_be_called(
    _args: &numan_cli::cmd::setup::NuSetupArgs,
    _root: &Path,
) -> anyhow::Result<()> {
    *TEST_NU_SETUP_CALLED.lock().unwrap() = true;
    panic!("doctor must not invoke setup::execute_nu_repair for missing Nu");
}

#[test]
fn doctor_default_does_not_auto_install_managed_nu() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root).unwrap();

    *TEST_NU_SETUP_CALLED.lock().unwrap() = false;
    let _path_guard = TEST_PATH_GUARD.lock().unwrap();
    let _cleared_path = ClearedPath::new();
    let args = DoctorArgs {
        scan: false,
        json: false,
        nupm_home: None,
    };
    let _code = execute_with_options(
        &args,
        root,
        DoctorOptions {
            // Network allowed: previously this would have downloaded managed Nu.
            skip_network: false,
            nu_setup_repair: Some(nu_setup_must_not_be_called),
            discover_off_path: Some(|| None),
            ..test_doctor_options()
        },
    )
    .unwrap();

    assert!(
        !*TEST_NU_SETUP_CALLED.lock().unwrap(),
        "default doctor must not call setup Nu install without explicit consent"
    );
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
        scan: false,
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
    assert!(
        managed_nu_binary(root).exists(),
        "off-PATH registration must not delete managed tools/nushell"
    );
    assert_eq!(code, 1);
}

fn fake_deactivate_repair(args: &DeactivateArgs, root: &Path) -> anyhow::Result<()> {
    assert!(!args.verbose);
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
        scan: false,
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
        scan: false,
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
        scan: false,
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
            scan: true,
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
            scan: true,
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

#[test]
fn doctor_json_default_stdout_is_valid_json() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root).unwrap();
    fake_init(&InitArgs { refresh: false }, root).unwrap();
    // Seed a cached index so default repair mode does not attempt registry sync.
    let index_path = root.join("registry/official/index.json");
    std::fs::create_dir_all(index_path.parent().unwrap()).unwrap();
    std::fs::write(
        &index_path,
        r#"{"schema_version":1,"packages":[],"generated_at":"test"}"#,
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_numan"))
        .args([
            "--root",
            root.to_str().expect("temp root is utf-8"),
            "doctor",
            "--json",
        ])
        .env("NUMAN_ALLOW_UNSIGNED", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run numan doctor --json");

    assert!(
        output.status.success(),
        "doctor --json must exit 0\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "default doctor --json stdout must be valid JSON: {e}\nstdout={stdout}\nstderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(
        value.get("repairs").is_some_and(|r| r.is_array()),
        "default doctor --json must include repairs: {value}"
    );
}

/// `doctor --json --scan` must omit the `repairs` field (single JSON object).
#[test]
fn doctor_json_scan_omits_repairs_field() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_numan"))
        .args([
            "--root",
            root.to_str().expect("temp root is utf-8"),
            "doctor",
            "--json",
            "--scan",
        ])
        .env("NUMAN_ALLOW_UNSIGNED", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run numan doctor --json --scan");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success()
            || output.status.code() == Some(1)
            || output.status.code() == Some(2),
        "doctor --json --scan unexpected status: {}\nstdout={stdout}\nstderr={stderr}",
        output.status
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json --scan stdout must be valid JSON: {e}\nstdout={stdout}\nstderr={stderr}"
        )
    });
    assert!(
        value.is_object(),
        "doctor --json --scan must emit a single JSON object: {value}"
    );
    assert!(
        value.get("repairs").is_none(),
        "doctor --json --scan must omit repairs: {value}"
    );
    assert!(
        value.get("findings").is_some_and(|f| f.is_array()),
        "doctor --json --scan must include findings: {value}"
    );
}
