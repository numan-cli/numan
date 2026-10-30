/// Integration tests for first-class completion package activation (issue #15).
///
/// Uses the injectable candidate-runner seam from the script activation
/// tests so no real Nu binary is required, plus an `#[ignore]`d real-Nu
/// acceptance test. Direct completion querying is not exposed by Nu in a
/// non-interactive invocation, so the real-Nu test proves the honest
/// equivalent: a fresh session that loads the managed file has the
/// contributed `extern` defined with its parameter bound to the declared
/// completer (the signature records the completion), and after
/// deactivation the managed contribution is gone.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use numan_cli::cmd::activate::{execute_with_candidate_runner, ActivateArgs};
use numan_cli::cmd::deactivate::{
    execute_with_candidate_runner as deactivate_with_runner, DeactivateArgs,
};
use numan_cli::core::integrity;
use numan_cli::core::package::{validate_completion_targets, CompletionKind, CompletionTarget};
use numan_cli::nu::autoload::FakeCandidateRunner;
use numan_cli::nu::paths::NuPaths;
use numan_cli::state::autoload_journal::{
    AutoloadOperation, AutoloadStage, PendingAutoload, SCHEMA_VERSION as AUTOLOAD_SCHEMA_VERSION,
};
use numan_cli::state::autoload_recovery::{reconcile_pending_autoload, AutoloadRecoveryOutcome};
use numan_cli::state::autoload_state::AutoloadState;
use numan_cli::state::lockfile::{Lockfile, LockfileEntry};
use numan_cli::util::fs_safety::{acquire_mutation_lock, OWNERSHIP_MARKER};

use tempfile::TempDir;

struct CompletionTestEnv {
    dir: TempDir,
    nu_hash: String,
    nu_exe: PathBuf,
    vendor_dir: PathBuf,
}

impl CompletionTestEnv {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let nu_exe = dir.path().join("fake_nu");
        std::fs::write(&nu_exe, b"fake nu binary").unwrap();
        let nu_hash = integrity::compute_sha256(b"fake nu binary");
        let vendor_dir = dir.path().join("vendor").join("autoload");
        std::fs::create_dir_all(&vendor_dir).unwrap();
        Self {
            dir,
            nu_hash,
            nu_exe,
            vendor_dir,
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn vendor_dir_str(&self) -> String {
        self.vendor_dir.to_string_lossy().into_owned()
    }

    fn managed_file_path(&self) -> PathBuf {
        self.vendor_dir.join("numan.nu")
    }

    fn write_nu_paths(&self) {
        NuPaths {
            nu_executable: self.nu_exe.to_string_lossy().into_owned(),
            nu_version: "0.116.1".to_string(),
            plugin_registry_path: self
                .root()
                .join("plugins.msgpackz")
                .to_string_lossy()
                .into_owned(),
            nu_executable_hash: self.nu_hash.clone(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            data_dir: Some(self.root().join("data").to_string_lossy().into_owned()),
            vendor_autoload_dirs: vec![self.vendor_dir_str()],
            vendor_autoload_dir: Some(self.vendor_dir_str()),
        }
        .save(self.root())
        .unwrap();
    }

    fn create_completion(
        &self,
        owner: &str,
        name: &str,
        files: &[(&str, &str)],
        targets: Vec<CompletionTarget>,
    ) -> (String, LockfileEntry) {
        let pkg_id = format!("{owner}/{name}");
        let payload_rel = format!("packages/completions/{owner}/{name}/1.0.0-aabbccdd");
        let payload_abs = self.root().join(&payload_rel);
        std::fs::create_dir_all(&payload_abs).unwrap();
        for (rel, contents) in files {
            let p = payload_abs.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, contents).unwrap();
        }
        let entry = LockfileEntry {
            hook_activation: None,
            hook_bindings: None,
            version: "1.0.0".to_string(),
            package_type: "completion".to_string(),
            source: "archive".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: None,
            installed_at: "0000000000000001".to_string(),
            nu_version_at_install: Some("0.116.1".to_string()),
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
            payload_path: payload_rel,
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            overlay: None,
            script_activation: None,
            script_entry_points: None,
            completion_activation: None,
            completion_targets: Some(targets),
            locked_dependencies: BTreeMap::new(),
        };
        (pkg_id, entry)
    }

    fn write_lockfile(&self, entries: Vec<(String, LockfileEntry)>) {
        Lockfile {
            version: 2,
            generated_at: "0000000000000001".to_string(),
            nu_version: "0.116.1".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            packages: entries.into_iter().collect(),
        }
        .save(self.root())
        .unwrap();
    }

    fn activate_args(&self, packages: &[&str]) -> ActivateArgs {
        ActivateArgs {
            packages: packages.iter().map(|s| s.to_string()).collect(),
            verbose: false,
            list: false,
            check: false,
        }
    }

    fn deactivate_args(&self, packages: &[&str]) -> DeactivateArgs {
        DeactivateArgs {
            packages: packages.iter().map(|s| s.to_string()).collect(),
            verbose: false,
        }
    }

    fn ok_registrar() -> impl Fn(&str, &str, &str) -> anyhow::Result<()> {
        |_nu, _bin, _cfg| Ok(())
    }
}

fn ct(command: &str, path: &str, kind: CompletionKind) -> CompletionTarget {
    CompletionTarget {
        command: command.to_string(),
        path: path.to_string(),
        kind,
    }
}

#[test]
fn target_validation_rejects_bad_names_and_duplicates() {
    assert!(validate_completion_targets(&[ct("git", "git.nu", CompletionKind::Static)]).is_ok());
    assert!(validate_completion_targets(&[
        ct("git", "git.nu", CompletionKind::CommandBacked),
        ct("cargo", "cargo.nu", CompletionKind::Static),
    ])
    .is_ok());
    assert!(validate_completion_targets(&[]).is_err());
    assert!(validate_completion_targets(&[ct("9bad", "x.nu", CompletionKind::Static)]).is_err());
    assert!(
        validate_completion_targets(&[ct("has space", "x.nu", CompletionKind::Static)]).is_err()
    );
    assert!(
        validate_completion_targets(&[ct("git", "../escape.nu", CompletionKind::Static)]).is_err()
    );
    assert!(validate_completion_targets(&[ct("git", "x.txt", CompletionKind::Static)]).is_err());
    assert!(validate_completion_targets(&[ct("if", "x.nu", CompletionKind::Static)]).is_err());
    assert!(validate_completion_targets(&[ct("use", "x.nu", CompletionKind::Static)]).is_err());
    // Duplicate command, different files.
    assert!(validate_completion_targets(&[
        ct("git", "a.nu", CompletionKind::Static),
        ct("git", "b.nu", CompletionKind::Static),
    ])
    .is_err());
    // Duplicate normalized path, different commands.
    assert!(validate_completion_targets(&[
        ct("git", "x.nu", CompletionKind::Static),
        ct("cargo", "./x.nu", CompletionKind::Static),
    ])
    .is_err());
}

#[test]
fn activation_writes_completion_use_lines_and_lockfile_record() {
    let env = CompletionTestEnv::new();
    env.write_nu_paths();
    let (id, entry) = env.create_completion(
        "acme",
        "dev-completions",
        &[
            ("git.nu", "# git completions\n"),
            ("cargo.nu", "# cargo completions\n"),
        ],
        vec![
            ct("git", "git.nu", CompletionKind::CommandBacked),
            ct("cargo", "cargo.nu", CompletionKind::Static),
        ],
    );
    env.write_lockfile(vec![(id.clone(), entry)]);

    let runner = FakeCandidateRunner::success();
    execute_with_candidate_runner(
        &env.activate_args(&[&id]),
        env.root(),
        &CompletionTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap();

    let content = std::fs::read_to_string(env.managed_file_path()).unwrap();
    assert!(content.starts_with(OWNERSHIP_MARKER), "{content}");
    assert!(
        content.contains("# numan:completion acme/dev-completions git"),
        "{content}"
    );
    assert!(
        content.contains("# numan:completion acme/dev-completions cargo"),
        "{content}"
    );
    // Targets render as `use "<abs path>" *`, sorted by scoped ID then command:
    // cargo's block precedes git's.
    let cargo_pos = content
        .find("# numan:completion acme/dev-completions cargo")
        .unwrap();
    let git_pos = content
        .find("# numan:completion acme/dev-completions git")
        .unwrap();
    assert!(cargo_pos < git_pos, "{content}");
    let git_line = content
        .lines()
        .skip_while(|l| !l.contains("# numan:completion acme/dev-completions git"))
        .nth(1)
        .unwrap();
    assert!(git_line.starts_with("use \""), "{git_line}");
    assert!(git_line.ends_with("\" *"), "{git_line}");
    assert!(git_line.contains("git.nu"), "{git_line}");

    let lockfile = Lockfile::load(env.root()).unwrap();
    let saved = &lockfile.packages[&id];
    let record = saved
        .completion_activation
        .as_ref()
        .expect("completion record");
    assert_eq!(record.targets.len(), 2);
    let git = record.targets.iter().find(|t| t.command == "git").unwrap();
    assert_eq!(git.kind, CompletionKind::CommandBacked);
    assert!(git.target_path.ends_with("git.nu"));
    // The production activation record stores the managed path built as
    // `format!("{vendor_dir}/numan.nu")`, so build the expected value the
    // same way; `PathBuf::join` would use a backslash on Windows.
    let managed_path_str = format!("{}/numan.nu", env.vendor_dir_str());
    assert!(saved.is_completion_active_for(
        &env.nu_hash,
        "0.116.1",
        &env.vendor_dir_str(),
        &managed_path_str,
    ));
}

#[test]
fn already_active_completion_is_skipped_on_reactivation() {
    let env = CompletionTestEnv::new();
    env.write_nu_paths();
    let (id, entry) = env.create_completion(
        "acme",
        "git-completions",
        &[("git.nu", "# git completions\n")],
        vec![ct("git", "git.nu", CompletionKind::CommandBacked)],
    );
    env.write_lockfile(vec![(id.clone(), entry)]);

    let runner = FakeCandidateRunner::success();
    execute_with_candidate_runner(
        &env.activate_args(&[&id]),
        env.root(),
        &CompletionTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap();
    let before = std::fs::read_to_string(env.managed_file_path()).unwrap();

    // Second activation: the package is already active for this Nu identity,
    // so it is skipped and the managed file is unchanged.
    let runner = FakeCandidateRunner::success();
    execute_with_candidate_runner(
        &env.activate_args(&[&id]),
        env.root(),
        &CompletionTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap();
    let after = std::fs::read_to_string(env.managed_file_path()).unwrap();
    assert_eq!(before, after);
}

#[test]
fn activation_rejects_cross_package_target_conflict() {
    let env = CompletionTestEnv::new();
    env.write_nu_paths();
    let (a, ea) = env.create_completion(
        "acme",
        "one",
        &[("a.nu", "# a\n")],
        vec![ct("git", "a.nu", CompletionKind::Static)],
    );
    let (b, eb) = env.create_completion(
        "acme",
        "two",
        &[("b.nu", "# b\n")],
        vec![ct("git", "b.nu", CompletionKind::CommandBacked)],
    );
    env.write_lockfile(vec![(a.clone(), ea), (b.clone(), eb)]);

    let runner = FakeCandidateRunner::success();
    execute_with_candidate_runner(
        &env.activate_args(&[&a]),
        env.root(),
        &CompletionTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap();

    // Target ownership is exclusive: `git` already belongs to acme/one.
    let runner = FakeCandidateRunner::success();
    let err = execute_with_candidate_runner(
        &env.activate_args(&[&b]),
        env.root(),
        &CompletionTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap_err();
    assert!(err.to_string().contains("git"), "{err}");
    assert!(err.to_string().contains("acme/one"), "{err}");

    let lockfile = Lockfile::load(env.root()).unwrap();
    assert!(lockfile.packages[&b].completion_activation.is_none());
}

#[test]
fn activation_rejects_missing_target_and_traversal_declaration() {
    // Declared target file does not exist in the payload.
    let env = CompletionTestEnv::new();
    env.write_nu_paths();
    let (id, entry) = env.create_completion(
        "acme",
        "broken",
        &[("a.nu", "# a\n")],
        vec![ct("git", "missing.nu", CompletionKind::Static)],
    );
    env.write_lockfile(vec![(id.clone(), entry)]);
    let runner = FakeCandidateRunner::success();
    let err = execute_with_candidate_runner(
        &env.activate_args(&[&id]),
        env.root(),
        &CompletionTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap_err();
    assert!(err.to_string().contains("acme/broken"), "{err}");
    assert!(!env.managed_file_path().exists());

    // Traversal declaration persisted directly in the lockfile (bypassing
    // install-time validation) must still fail at activation time.
    let env2 = CompletionTestEnv::new();
    env2.write_nu_paths();
    let (id2, entry2) = env2.create_completion(
        "acme",
        "evil",
        &[("a.nu", "# a\n")],
        vec![ct("git", "../escape.nu", CompletionKind::Static)],
    );
    env2.write_lockfile(vec![(id2.clone(), entry2)]);
    let runner = FakeCandidateRunner::success();
    let err = execute_with_candidate_runner(
        &env2.activate_args(&[&id2]),
        env2.root(),
        &CompletionTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap_err();
    assert!(err.to_string().contains("acme/evil"), "{err}");
    assert!(!env2.managed_file_path().exists());
}

#[test]
fn deactivation_removes_only_that_packages_lines() {
    let env = CompletionTestEnv::new();
    env.write_nu_paths();
    let (a, ea) = env.create_completion(
        "acme",
        "one",
        &[("a.nu", "# a\n")],
        vec![ct("git", "a.nu", CompletionKind::Static)],
    );
    let (b, eb) = env.create_completion(
        "acme",
        "two",
        &[("b.nu", "# b\n")],
        vec![ct("cargo", "b.nu", CompletionKind::CommandBacked)],
    );
    env.write_lockfile(vec![(a.clone(), ea), (b.clone(), eb)]);
    for id in [&a, &b] {
        let runner = FakeCandidateRunner::success();
        execute_with_candidate_runner(
            &env.activate_args(&[id]),
            env.root(),
            &CompletionTestEnv::ok_registrar(),
            &runner,
        )
        .unwrap();
    }
    let runner = FakeCandidateRunner::success();
    deactivate_with_runner(&env.deactivate_args(&[&a]), env.root(), &runner).unwrap();

    let content = std::fs::read_to_string(env.managed_file_path()).unwrap();
    assert!(!content.contains("acme/one"), "{content}");
    assert!(!content.contains("a.nu"), "{content}");
    assert!(
        content.contains("# numan:completion acme/two cargo"),
        "{content}"
    );
    assert!(content.contains("b.nu"), "{content}");
    let lockfile = Lockfile::load(env.root()).unwrap();
    assert!(lockfile.packages[&a].completion_activation.is_none());
    assert!(lockfile.packages[&b].completion_activation.is_some());
}

/// Real-Nu acceptance: after activation, a fresh Nu session that loads the
/// managed file receives the contributed completion (the `fakecmd` extern
/// is defined and its positional parameter is bound to the declared
/// completer in the command signature); after deactivation it no longer
/// does. User config is never touched.
///
/// Nu does not expose reedline's completion machinery to non-interactive
/// invocations, so this asserts the signature-level binding rather than
/// simulating a TAB keypress.
#[test]
#[ignore]
fn real_nu_completion_received_after_activation_and_gone_after_deactivation() {
    use numan_cli::nu::autoload::NuCandidateRunner;

    // Resolve the Nu executable to a real path once: CI runs this test with
    // `nu` only on PATH and no NU_BINARY, and bare names resolve against the
    // working directory for `std::fs` calls, not PATH.
    let nu: PathBuf = match std::env::var("NU_BINARY") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            let exe_name = format!("nu{}", std::env::consts::EXE_SUFFIX);
            std::env::var_os("PATH")
                .and_then(|paths| {
                    std::env::split_paths(&paths)
                        .map(|dir| dir.join(&exe_name))
                        .find(|candidate| candidate.is_file())
                })
                .expect("nu executable not found on PATH (set NU_BINARY to its path)")
        }
    };
    let nu_version_out = std::process::Command::new(&nu)
        .arg("--version")
        .output()
        .unwrap();
    let nu_version = String::from_utf8_lossy(&nu_version_out.stdout)
        .trim()
        .to_string();
    let nu_bytes = std::fs::read(&nu).unwrap();
    let nu_hash = integrity::compute_sha256(&nu_bytes);
    let nu_abs = std::fs::canonicalize(&nu)
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let vendor_dir = root.join("vendor").join("autoload");
    std::fs::create_dir_all(&vendor_dir).unwrap();
    let vendor_str = vendor_dir.to_string_lossy().into_owned();
    let managed_path = vendor_dir.join("numan.nu");

    NuPaths {
        nu_executable: nu_abs.clone(),
        nu_version: nu_version.clone(),
        plugin_registry_path: root.join("plugins.msgpackz").to_string_lossy().into_owned(),
        nu_executable_hash: nu_hash,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        data_dir: Some(root.join("data").to_string_lossy().into_owned()),
        vendor_autoload_dirs: vec![vendor_str.clone()],
        vendor_autoload_dir: Some(vendor_str),
    }
    .save(root)
    .unwrap();

    // A stand-in for user-authored Nu configuration: activation and
    // deactivation must never modify it.
    let user_config = root.join("config.nu");
    let user_config_content = "# my config\nalias ll = ls -l\n";
    std::fs::write(&user_config, user_config_content).unwrap();

    // Fixture completion package: a completer plus an extern that binds it.
    let payload_rel = "packages/completions/acme/fake-completions/1.0.0-aabbccdd";
    let payload_abs = root.join(payload_rel);
    std::fs::create_dir_all(&payload_abs).unwrap();
    std::fs::write(
        payload_abs.join("fakecmd-completions.nu"),
        "def \"nu-complete fakecmd branches\" [] { [\"main\", \"dev\", \"feature-x\"] }\n\
         \n\
         export extern \"fakecmd\" [\n\
         \x20   branch?: string@\"nu-complete fakecmd branches\"\n\
         \x20   --flag\n\
         ]\n",
    )
    .unwrap();

    let pkg_id = "acme/fake-completions".to_string();
    let entry = LockfileEntry {
        hook_activation: None,
        hook_bindings: None,
        version: "1.0.0".to_string(),
        package_type: "completion".to_string(),
        source: "archive".to_string(),
        target: None,
        artifact_url: None,
        artifact_sha256: None,
        executable_path: None,
        archive_root: None,
        include: None,
        entry: None,
        installed_at: "0000000000000001".to_string(),
        nu_version_at_install: Some(nu_version.clone()),
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
        payload_path: payload_rel.to_string(),
        revision_id: None,
        payload_sha256: None,
        executable_sha256: None,
        selection_reason: None,
        origin: None,
        module_activation: None,
        module_import_mode: None,
        overlay: None,
        script_activation: None,
        script_entry_points: None,
        completion_activation: None,
        completion_targets: Some(vec![ct(
            "fakecmd",
            "fakecmd-completions.nu",
            CompletionKind::CommandBacked,
        )]),
        locked_dependencies: BTreeMap::new(),
    };
    Lockfile {
        version: 2,
        generated_at: "0000000000000001".to_string(),
        nu_version: nu_version.clone(),
        platform: "x86_64-unknown-linux-gnu".to_string(),
        packages: vec![(pkg_id.clone(), entry)].into_iter().collect(),
    }
    .save(root)
    .unwrap();

    let registrar = |_nu: &str, _bin: &str, _cfg: &str| Ok(());
    let runner = NuCandidateRunner::new(&nu_abs);
    execute_with_candidate_runner(
        &ActivateArgs {
            packages: vec![pkg_id.clone()],
            verbose: false,
            list: false,
            check: false,
        },
        root,
        &registrar,
        &runner,
    )
    .unwrap();
    assert!(managed_path.exists());

    // Fresh session: prepend the managed file (as a vendor-autoload load
    // would) and assert the extern + completer binding are received.
    let managed = std::fs::read_to_string(&managed_path).unwrap();
    let session = root.join("session.nu");
    std::fs::write(
        &session,
        format!(
            "{managed}\n\
             if (\"fakecmd\" not-in (scope commands | get name)) {{ exit 11 }}\n\
             let sig = (scope commands | where name == \"fakecmd\" | first | get signatures | values | first)\n\
             if (\"nu-complete fakecmd branches\" not-in ($sig | get completion)) {{ exit 12 }}\n\
             print \"completion received\"\n"
        ),
    )
    .unwrap();
    let out = std::process::Command::new(&nu_abs)
        .arg("--no-config-file")
        .arg(&session)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fresh session did not receive the completion: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "completion received"
    );

    // Deactivate and prove the contribution is gone from a fresh session.
    let runner = NuCandidateRunner::new(&nu_abs);
    deactivate_with_runner(
        &DeactivateArgs {
            packages: vec![pkg_id.clone()],
            verbose: false,
        },
        root,
        &runner,
    )
    .unwrap();

    let mut session2_content = String::new();
    if managed_path.exists() {
        session2_content = std::fs::read_to_string(&managed_path).unwrap();
    }
    let session2 = root.join("session2.nu");
    std::fs::write(
        &session2,
        format!(
            "{session2_content}\n\
             if (\"fakecmd\" in (scope commands | get name)) {{ exit 13 }}\n\
             print \"completion gone\"\n"
        ),
    )
    .unwrap();
    let out = std::process::Command::new(&nu_abs)
        .arg("--no-config-file")
        .arg(&session2)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fresh session still receives the completion after deactivation: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "completion gone"
    );

    assert_eq!(
        std::fs::read_to_string(&user_config).unwrap(),
        user_config_content,
        "user config must be untouched"
    );
}

#[test]
fn activate_recovers_interrupted_completion_deactivation() {
    // Mirror of the module recovery case: a deactivation replaced (deleted)
    // the managed file and journaled the transition, then crashed before the
    // lockfile record was cleared. The next activation must complete the
    // interrupted deactivation (old completion's record cleared, not
    // resurrected) and activate the new package.
    let env = CompletionTestEnv::new();
    env.write_nu_paths();
    let (old_id, old_entry) = env.create_completion(
        "acme",
        "old-completions",
        &[("git.nu", "# git completions\n")],
        vec![ct("git", "git.nu", CompletionKind::Static)],
    );
    let (next_id, next_entry) = env.create_completion(
        "acme",
        "next-completions",
        &[("cargo.nu", "# cargo completions\n")],
        vec![ct("cargo", "cargo.nu", CompletionKind::Static)],
    );
    env.write_lockfile(vec![
        (old_id.clone(), old_entry),
        (next_id.clone(), next_entry),
    ]);

    let runner = FakeCandidateRunner::success();
    execute_with_candidate_runner(
        &env.activate_args(&[&old_id]),
        env.root(),
        &CompletionTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap();
    let activation = Lockfile::load(env.root()).unwrap().packages[&old_id]
        .completion_activation
        .clone()
        .unwrap();
    let previous_sha = integrity::compute_sha256(&std::fs::read(env.managed_file_path()).unwrap());
    std::fs::remove_file(env.managed_file_path()).unwrap();

    std::fs::create_dir_all(env.root().join("state")).unwrap();
    PendingAutoload {
        schema_version: AUTOLOAD_SCHEMA_VERSION,
        operation: AutoloadOperation::Deactivate,
        stage: AutoloadStage::Replaced,
        nu_executable_sha256: env.nu_hash.clone(),
        nu_version: "0.116.1".to_string(),
        vendor_autoload_dir: activation.vendor_autoload_dir,
        managed_file_path: activation.managed_file_path,
        previous_file_exists: true,
        previous_file_sha256: Some(previous_sha),
        desired_file_exists: false,
        candidate_sha256: None,
        previous_active_module_ids: vec![old_id.clone()],
        desired_active_module_ids: vec![],
        targeted_module_ids: vec![old_id.clone()],
        created_at: "0000000000000002".to_string(),
        pre_mutation_snapshot_id: None,
    }
    .save(env.root())
    .unwrap();

    execute_with_candidate_runner(
        &env.activate_args(&[&next_id]),
        env.root(),
        &CompletionTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap();

    let recovered = Lockfile::load(env.root()).unwrap();
    assert!(
        recovered.packages[&old_id].completion_activation.is_none(),
        "interrupted deactivation must clear the old completion record"
    );
    assert!(recovered.packages[&next_id].completion_activation.is_some());
    assert_eq!(
        AutoloadState::load(env.root())
            .unwrap()
            .unwrap()
            .active_module_ids,
        vec![next_id.clone()]
    );
    assert!(PendingAutoload::load(env.root()).unwrap().is_none());
    let content = std::fs::read_to_string(env.managed_file_path()).unwrap();
    assert!(content.contains("# numan:completion acme/next-completions cargo"));
    assert!(!content.contains("old-completions"));
}

#[test]
fn recovery_rebuilds_completion_record_from_replaced_activate_journal() {
    // The rebuild branch of `apply_replaced_transition` is distinct from the
    // deactivation-clearing case above: here an interrupted *activation*
    // replaced the managed file and journaled the transition, but crashed
    // before writing the completion_activation record and autoload state.
    // Recovery must rebuild the record from the persisted declaration.
    let env = CompletionTestEnv::new();
    env.write_nu_paths();
    let (id, entry) = env.create_completion(
        "acme",
        "git-completions",
        &[("git.nu", "# git completions\n")],
        vec![ct("git", "git.nu", CompletionKind::CommandBacked)],
    );
    env.write_lockfile(vec![(id.clone(), entry)]);

    let runner = FakeCandidateRunner::success();
    execute_with_candidate_runner(
        &env.activate_args(&[&id]),
        env.root(),
        &CompletionTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap();

    // Simulate the crash window: the managed file is in place, but the
    // lockfile record and derived autoload state were never written.
    let managed_content = std::fs::read(env.managed_file_path()).unwrap();
    let candidate_sha = integrity::compute_sha256(&managed_content);
    AutoloadState::delete(env.root()).unwrap();
    let mut lockfile = Lockfile::load(env.root()).unwrap();
    lockfile
        .packages
        .get_mut(&id)
        .unwrap()
        .completion_activation = None;
    lockfile.save(env.root()).unwrap();

    std::fs::create_dir_all(env.root().join("state")).unwrap();
    PendingAutoload {
        schema_version: AUTOLOAD_SCHEMA_VERSION,
        operation: AutoloadOperation::Activate,
        stage: AutoloadStage::Replaced,
        nu_executable_sha256: env.nu_hash.clone(),
        nu_version: "0.116.1".to_string(),
        vendor_autoload_dir: env.vendor_dir_str(),
        managed_file_path: format!("{}/numan.nu", env.vendor_dir_str()),
        previous_file_exists: false,
        previous_file_sha256: None,
        desired_file_exists: true,
        candidate_sha256: Some(candidate_sha),
        previous_active_module_ids: vec![],
        desired_active_module_ids: vec![id.clone()],
        targeted_module_ids: vec![id.clone()],
        created_at: "0000000000000003".to_string(),
        pre_mutation_snapshot_id: None,
    }
    .save(env.root())
    .unwrap();

    let _guard = acquire_mutation_lock(env.root()).unwrap();
    let nu_paths = NuPaths::load(env.root()).unwrap();
    let mut lockfile = Lockfile::load(env.root()).unwrap();
    assert_eq!(
        reconcile_pending_autoload(env.root(), &nu_paths, &mut lockfile).unwrap(),
        AutoloadRecoveryOutcome::ReplacedCompleted
    );

    let recovered = Lockfile::load(env.root()).unwrap();
    let record = recovered.packages[&id]
        .completion_activation
        .as_ref()
        .expect("recovery must rebuild the completion record");
    assert_eq!(record.targets.len(), 1);
    assert_eq!(record.targets[0].command, "git");
    assert_eq!(record.targets[0].kind, CompletionKind::CommandBacked);
    assert!(record.targets[0].target_path.ends_with("git.nu"));
    assert_eq!(
        AutoloadState::load(env.root())
            .unwrap()
            .unwrap()
            .active_module_ids,
        vec![id.clone()]
    );
    assert!(PendingAutoload::load(env.root()).unwrap().is_none());
}
