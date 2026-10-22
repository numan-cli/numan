mod support;

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;

use support::acceptance::filesystem::{
    capture_state, classify_package_dirs, discover_journals, inventory_root, path_is_contained,
};
use support::acceptance::model::{
    ChildEnvironment, CommandSpec, RunSummary, Stage1Config, EVIDENCE_SCHEMA_VERSION,
};
use support::acceptance::process::{run_command, streaming_sha256};
use support::acceptance::runner::AcceptanceRun;

#[test]
#[ignore = "helper process invoked by process-runner tests"]
fn acceptance_process_helper() {
    match std::env::var("NUMAN_ACCEPTANCE_HELPER_ACTION").as_deref() {
        Ok("capture") => {
            print!("stdout-marker");
            eprint!("stderr-marker");
        }
        Ok("nonzero") => std::process::exit(23),
        Ok("timeout") => std::thread::sleep(Duration::from_secs(30)),
        other => panic!("unknown helper action: {other:?}"),
    }
}

fn helper_command(action: &str, timeout: Duration) -> (CommandSpec, ChildEnvironment) {
    let mut variables = BTreeMap::new();
    variables.insert(
        "NUMAN_ACCEPTANCE_HELPER_ACTION".to_string(),
        action.to_string(),
    );
    let environment = ChildEnvironment::new_for_test(variables);
    let spec = CommandSpec::new(
        "helper",
        std::env::current_exe().unwrap(),
        vec![
            "acceptance_process_helper".to_string(),
            "--ignored".to_string(),
            "--exact".to_string(),
            "--nocapture".to_string(),
        ],
        timeout,
    );
    (spec, environment)
}

#[test]
fn process_runner_captures_separate_streams_and_nonzero_exit() {
    let (capture, environment) = helper_command("capture", Duration::from_secs(10));
    let outcome = run_command(&capture, &environment).unwrap();
    assert_eq!(outcome.exit_code, Some(0));
    assert!(String::from_utf8_lossy(&outcome.stdout).contains("stdout-marker"));
    assert!(String::from_utf8_lossy(&outcome.stderr).contains("stderr-marker"));

    let (nonzero, environment) = helper_command("nonzero", Duration::from_secs(10));
    let outcome = run_command(&nonzero, &environment).unwrap();
    assert_eq!(outcome.exit_code, Some(23));
    assert!(!outcome.timed_out);
}

#[test]
fn streaming_sha256_and_command_serialization_are_stable() {
    let (hash, bytes) = streaming_sha256(Cursor::new(b"stage-1-evidence")).unwrap();
    assert_eq!(bytes, 16);
    assert_eq!(
        hash,
        "cf2af44ff4918d15ebce0b9a3db875e92e0e783767028c9a5ad4bb0f30a3b947"
    );

    let spec = CommandSpec::new(
        "search",
        PathBuf::from("numan.exe"),
        vec!["--root".into(), "isolated".into(), "search".into()],
        Duration::from_secs(60),
    );
    let json = serde_json::to_value(&spec).unwrap();
    assert_eq!(json["step"], "search");
    assert_eq!(json["timeout_ms"], 60_000);
    assert_eq!(json["arguments"][0], "--root");
}

#[test]
fn process_runner_times_out_and_kills_the_child() {
    let (spec, environment) = helper_command("timeout", Duration::from_millis(100));
    let outcome = run_command(&spec, &environment).unwrap();
    assert!(outcome.timed_out);
    assert_ne!(outcome.exit_code, Some(0));
    assert!(outcome.duration_ms < 10_000);
}

#[test]
fn inventory_is_recursive_hashed_and_stably_sorted() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("z/sub")).unwrap();
    std::fs::write(temp.path().join("z/sub/file.txt"), b"payload").unwrap();
    std::fs::write(temp.path().join("a.txt"), b"first").unwrap();

    let first = inventory_root(temp.path()).unwrap();
    let second = inventory_root(temp.path()).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        vec!["a.txt", "z", "z/sub", "z/sub/file.txt"]
    );
    assert!(first[0].sha256.is_some());
}

#[test]
fn state_capture_discovers_known_and_additional_journals() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path();
    let root = run_dir.join("root");
    let plugin_registry = run_dir.join("home/nushell/plugin.msgpackz");
    std::fs::create_dir_all(root.join("nu_state")).unwrap();
    std::fs::create_dir_all(root.join("state/nested")).unwrap();
    std::fs::create_dir_all(plugin_registry.parent().unwrap()).unwrap();
    std::fs::write(root.join("lockfile"), r#"{"version":2,"packages":{}}"#).unwrap();
    std::fs::write(
        root.join("nu_state/paths.json"),
        serde_json::to_vec(&serde_json::json!({
            "plugin_registry_path": plugin_registry,
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(root.join("state/pending-custom.json"), b"{}").unwrap();
    std::fs::write(root.join("state/nested/recovery-journal.json"), b"{}").unwrap();
    std::fs::write(&plugin_registry, b"registry-before").unwrap();

    let inventory = inventory_root(&root).unwrap();
    let state = capture_state(&root, run_dir, &inventory);
    let journals = discover_journals(&root).unwrap();
    assert_eq!(journals.len(), 2);
    assert!(state
        .journals
        .contains(&"state/pending-custom.json".to_string()));
    assert!(state
        .journals
        .contains(&"state/nested/recovery-journal.json".to_string()));
    assert!(state.files.iter().any(|file| {
        file.path.ends_with("home/nushell/plugin.msgpackz") && file.sha256.is_some()
    }));
}

#[test]
fn containment_handles_existing_and_nonexisting_paths() {
    let temp = tempfile::tempdir().unwrap();
    let boundary = temp.path().join("boundary");
    std::fs::create_dir_all(boundary.join("existing")).unwrap();
    assert!(path_is_contained(&boundary.join("existing"), &boundary).unwrap());
    assert!(path_is_contained(&boundary.join("future/deep/file"), &boundary).unwrap());
    assert!(!path_is_contained(&temp.path().join("outside"), &boundary).unwrap());
}

#[test]
fn payload_classification_labels_current_snapshot_journal_and_orphan() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let current = "packages/plugins/owner/current/1.0.0-current";
    let journal = "packages/plugins/owner/journal/1.0.0-journal";
    let orphan = "packages/plugins/owner/orphan/1.0.0-orphan";
    for path in [current, journal, orphan] {
        std::fs::create_dir_all(root.join(path)).unwrap();
    }
    std::fs::write(
        root.join("lockfile"),
        serde_json::to_vec(&serde_json::json!({
            "version": 2,
            "packages": {"owner/current": {"payload_path": current}}
        }))
        .unwrap(),
    )
    .unwrap();
    let snapshot = root.join("state/snapshots/snapshot-one");
    std::fs::create_dir_all(&snapshot).unwrap();
    std::fs::write(
        snapshot.join("lockfile.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 2,
            "packages": {"owner/current": {"payload_path": current}}
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("state/pending-lifecycle.json"),
        serde_json::to_vec(&serde_json::json!({
            "package_id": "owner/journal",
            "orphan_payload_path": journal
        }))
        .unwrap(),
    )
    .unwrap();

    let classified = classify_package_dirs(root).unwrap();
    let current = classified
        .iter()
        .find(|entry| entry.path.ends_with("-current"))
        .unwrap();
    assert_eq!(current.references.len(), 2);
    let journal = classified
        .iter()
        .find(|entry| entry.path.ends_with("-journal"))
        .unwrap();
    assert_eq!(
        journal.references[0].source,
        "journal:state/pending-lifecycle.json"
    );
    let orphan = classified
        .iter()
        .find(|entry| entry.path.ends_with("-orphan"))
        .unwrap();
    assert!(orphan.orphan);
}

#[test]
fn isolated_environment_and_failure_summary_are_durable() {
    let temp = tempfile::tempdir().unwrap();
    let config = Stage1Config {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        output_base: temp.path().to_path_buf(),
        package_id: "owner/plugin".to_string(),
        query: "plugin".to_string(),
    };
    let mut run = AcceptanceRun::new(config, std::env::current_exe().unwrap()).unwrap();
    assert!(!run.root.exists());
    assert!(!run
        .environment
        .variables
        .contains_key("NUMAN_ALLOW_UNSIGNED"));
    assert!(!run.environment.variables.contains_key("NUMAN_ROOT"));
    assert!(!run.environment.variables.contains_key("NUPM_HOME"));
    let serialized_environment = serde_json::to_string(&run.environment).unwrap();
    assert!(!serialized_environment.contains(";"));

    let summary = RunSummary {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        run_id: run.run_id.clone(),
        status: "failed".to_string(),
        package_id: "owner/plugin".to_string(),
        query: "plugin".to_string(),
        resolved_version: None,
        registry_key_id: None,
        registry_index_sha256: None,
        signing_key_fingerprint: None,
        executable_sha256: None,
        doctor_errors: None,
        doctor_warnings: None,
        steps: Vec::new(),
        remaining_payloads: Vec::new(),
        evidence_directory: run.evidence.to_string_lossy().into_owned(),
    };
    run.finalize(&summary).unwrap();
    assert!(run.evidence.join("run.json").exists());
    assert!(run.evidence.join("summary.json").exists());
    assert!(run.evidence.join("summary.md").exists());
    assert!(std::fs::read_to_string(run.evidence.join("summary.md"))
        .unwrap()
        .contains("Status: **failed**"));
}

#[test]
#[ignore = "live production-registry acceptance; requires Windows x64 and Nu 0.113.x"]
fn stage1_official_registry() {
    let config = Stage1Config::from_env().expect("invalid Stage 1 acceptance configuration");
    let mut run = AcceptanceRun::new(config, PathBuf::from(env!("CARGO_BIN_EXE_numan")))
        .expect("failed to create isolated Stage 1 acceptance run");
    match run.execute() {
        Ok(summary) => {
            // The evidence path identifies the acceptance run; it is not a
            // secret and is needed to inspect the generated evidence.
            // codeql[rust/cleartext-logging]
            println!(
                "Stage 1 passed: package={} version={} evidence={}",
                summary.package_id,
                summary.resolved_version.as_deref().unwrap_or("unresolved"),
                summary.evidence_directory
            );
        }
        Err(failure) => panic!("{failure}"),
    }
}
