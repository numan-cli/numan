/// Integration tests for first-class script package activation (issue #14).
///
/// Uses the injectable candidate-runner seam from the module autoload tests
/// so no real Nu binary is required, plus an `#[ignore]`d real-Nu acceptance
/// test proving a declared script command is callable in a fresh session.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use numan_cli::cmd::activate::{execute_with_candidate_runner, ActivateArgs};
use numan_cli::cmd::deactivate::{
    execute_with_candidate_runner as deactivate_with_runner, DeactivateArgs,
};
use numan_cli::core::integrity;
use numan_cli::core::package::{validate_script_entry_points, ScriptEntryPoint};
use numan_cli::nu::autoload::FakeCandidateRunner;
use numan_cli::nu::paths::NuPaths;
use numan_cli::state::lockfile::{Lockfile, LockfileEntry};
use numan_cli::util::fs_safety::OWNERSHIP_MARKER;

use tempfile::TempDir;

struct ScriptTestEnv {
    dir: TempDir,
    nu_hash: String,
    nu_exe: PathBuf,
    vendor_dir: PathBuf,
}

impl ScriptTestEnv {
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

    fn create_script(
        &self,
        owner: &str,
        name: &str,
        files: &[(&str, &str)],
        entry_points: Vec<ScriptEntryPoint>,
    ) -> (String, LockfileEntry) {
        let pkg_id = format!("{owner}/{name}");
        let payload_rel = format!("packages/scripts/{owner}/{name}/1.0.0-aabbccdd");
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
            package_type: "script".to_string(),
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
            script_entry_points: Some(entry_points),
            completion_activation: None,
            completion_targets: None,
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

fn ep(name: &str, path: &str) -> ScriptEntryPoint {
    ScriptEntryPoint {
        name: name.to_string(),
        path: path.to_string(),
    }
}

#[test]
fn entry_point_validation_rejects_bad_names_and_duplicates() {
    assert!(validate_script_entry_points(&[ep("wttr", "wttr.nu")]).is_ok());
    assert!(validate_script_entry_points(&[]).is_err());
    assert!(validate_script_entry_points(&[ep("9bad", "x.nu")]).is_err());
    assert!(validate_script_entry_points(&[ep("has space", "x.nu")]).is_err());
    assert!(validate_script_entry_points(&[ep("a", "x.nu"), ep("a", "y.nu")]).is_err());
    assert!(validate_script_entry_points(&[ep("a", "../escape.nu")]).is_err());
    assert!(validate_script_entry_points(&[ep("a", "x.txt")]).is_err());
    assert!(validate_script_entry_points(&[ep("if", "x.nu")]).is_err());
    assert!(validate_script_entry_points(&[ep("def", "x.nu")]).is_err());
    assert!(validate_script_entry_points(&[ep("a", "x.nu"), ep("b", "./x.nu")]).is_err());
}

#[test]
fn activation_writes_script_wrapper_and_lockfile_record() {
    let env = ScriptTestEnv::new();
    env.write_nu_paths();
    let (id, entry) = env.create_script(
        "acme",
        "weather",
        &[("wttr.nu", "def main [] { 'sunny' }\n")],
        vec![ep("wttr", "wttr.nu")],
    );
    env.write_lockfile(vec![(id.clone(), entry)]);

    let runner = FakeCandidateRunner::success();
    execute_with_candidate_runner(
        &env.activate_args(&[&id]),
        env.root(),
        &ScriptTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap();

    let content = std::fs::read_to_string(env.managed_file_path()).unwrap();
    assert!(content.starts_with(OWNERSHIP_MARKER), "{content}");
    assert!(content.contains("# numan:script acme/weather"), "{content}");
    assert!(content.contains("export def --wrapped wttr"), "{content}");
    assert!(content.contains("wttr.nu"), "{content}");

    let lockfile = Lockfile::load(env.root()).unwrap();
    let saved = &lockfile.packages[&id];
    let record = saved.script_activation.as_ref().expect("script record");
    assert_eq!(record.commands.len(), 1);
    assert_eq!(record.commands[0].name, "wttr");
}

#[test]
fn activation_rejects_missing_target_and_command_collision() {
    let env = ScriptTestEnv::new();
    env.write_nu_paths();
    let (id1, e1) = env.create_script(
        "acme",
        "one",
        &[("a.nu", "def main [] {}\n")],
        vec![ep("dup", "a.nu"), ep("gone", "missing.nu")],
    );
    env.write_lockfile(vec![(id1.clone(), e1)]);
    let runner = FakeCandidateRunner::success();
    let err = execute_with_candidate_runner(
        &env.activate_args(&[&id1]),
        env.root(),
        &ScriptTestEnv::ok_registrar(),
        &runner,
    )
    .unwrap_err();
    assert!(err.to_string().contains("acme/one"), "{err}");
    assert!(!env.managed_file_path().exists());

    // Collision across two packages claiming the same public name.
    let env2 = ScriptTestEnv::new();
    env2.write_nu_paths();
    let (a, ea) = env2.create_script(
        "acme",
        "one",
        &[("a.nu", "def main [] {}\n")],
        vec![ep("dup", "a.nu")],
    );
    let (b, eb) = env2.create_script(
        "acme",
        "two",
        &[("b.nu", "def main [] {}\n")],
        vec![ep("dup", "b.nu")],
    );
    env2.write_lockfile(vec![(a.clone(), ea), (b.clone(), eb)]);
    let runner2 = FakeCandidateRunner::success();
    execute_with_candidate_runner(
        &env2.activate_args(&[&a]),
        env2.root(),
        &ScriptTestEnv::ok_registrar(),
        &runner2,
    )
    .unwrap();
    let runner3 = FakeCandidateRunner::success();
    let err = execute_with_candidate_runner(
        &env2.activate_args(&[&b]),
        env2.root(),
        &ScriptTestEnv::ok_registrar(),
        &runner3,
    )
    .unwrap_err();
    assert!(err.to_string().contains("dup"), "{err}");
}

#[test]
fn deactivation_removes_only_that_packages_wrappers() {
    let env = ScriptTestEnv::new();
    env.write_nu_paths();
    let (a, ea) = env.create_script(
        "acme",
        "one",
        &[("a.nu", "def main [] {}\n")],
        vec![ep("cmd-one", "a.nu")],
    );
    let (b, eb) = env.create_script(
        "acme",
        "two",
        &[("b.nu", "def main [] {}\n")],
        vec![ep("cmd-two", "b.nu")],
    );
    env.write_lockfile(vec![(a.clone(), ea), (b.clone(), eb)]);
    for id in [&a, &b] {
        let runner = FakeCandidateRunner::success();
        execute_with_candidate_runner(
            &env.activate_args(&[id]),
            env.root(),
            &ScriptTestEnv::ok_registrar(),
            &runner,
        )
        .unwrap();
    }
    let runner = FakeCandidateRunner::success();
    deactivate_with_runner(&env.deactivate_args(&[&a]), env.root(), &runner).unwrap();

    let content = std::fs::read_to_string(env.managed_file_path()).unwrap();
    assert!(!content.contains("cmd-one"), "{content}");
    assert!(content.contains("cmd-two"), "{content}");
    let lockfile = Lockfile::load(env.root()).unwrap();
    assert!(lockfile.packages[&a].script_activation.is_none());
    assert!(lockfile.packages[&b].script_activation.is_some());
}

/// Real-Nu acceptance: a declared command is callable in a fresh session,
/// forwards arguments, and preserves a non-zero exit status.
#[test]
#[ignore]
fn real_nu_script_command_runs_and_forwards_exit_status() {
    let nu = std::env::var("NU_BINARY").unwrap_or_else(|_| "nu".to_string());
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("hello.nu");
    std::fs::write(
        &script,
        "def main [name: string] { print $\"hi ($name)\"; exit 3 }\n",
    )
    .unwrap();
    let wrapper = dir.path().join("wrapper.nu");
    let cmd = numan_cli::nu::autoload::ResolvedScriptCommand {
        scoped_id: "acme/hello".to_string(),
        name: "hello".to_string(),
        script_path: script.clone(),
        nu_executable: nu.clone(),
    };
    let rendered =
        numan_cli::nu::autoload::generate_managed_content(&[], std::slice::from_ref(&cmd), &[])
            .unwrap();
    std::fs::write(&wrapper, format!("{rendered}\nhello world\n")).unwrap();
    let out = std::process::Command::new(&nu)
        .arg("--no-config-file")
        .arg(&wrapper)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "hi world",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(3));
}
