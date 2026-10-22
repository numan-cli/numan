//! Integration tests for `numan init` and `numan init --refresh`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use numan_cli::cmd::init::{execute_with_runner, InitArgs};
use numan_cli::config::Config;
use numan_cli::core::integrity;
use numan_cli::core::official_registry::OFFICIAL_REGISTRY;
use numan_cli::core::package::ModuleImportMode;
use numan_cli::nu::autoload::{CandidateRunner, FakeCandidateRunner};
use numan_cli::nu::paths::NuPaths;
use numan_cli::state::autoload_state::AutoloadState;
use numan_cli::state::lockfile::{Lockfile, LockfileEntry, ModuleActivation};
use numan_cli::util::fs_safety::OWNERSHIP_MARKER;
use tempfile::TempDir;

struct InitRefreshEnv {
    dir: TempDir,
    nu_v1: PathBuf,
    nu_v2: PathBuf,
    vendor_dir: PathBuf,
    managed_file: PathBuf,
    hash_v1: String,
    hash_v2: String,
}

impl InitRefreshEnv {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let nu_v1 = root.join("nu_v1");
        let nu_v2 = root.join("nu_v2");
        std::fs::write(&nu_v1, b"nu-binary-v1").unwrap();
        std::fs::write(&nu_v2, b"nu-binary-v2").unwrap();

        let vendor_dir = root.join("data").join("vendor").join("autoload");
        std::fs::create_dir_all(&vendor_dir).unwrap();
        let managed_file = vendor_dir.join("numan.nu");

        Self {
            hash_v1: integrity::compute_sha256(b"nu-binary-v1"),
            hash_v2: integrity::compute_sha256(b"nu-binary-v2"),
            dir,
            nu_v1,
            nu_v2,
            vendor_dir,
            managed_file,
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn vendor_dir_str(&self) -> String {
        self.vendor_dir.to_string_lossy().into_owned()
    }

    fn paths(&self, nu_exe: &Path, vendor: &str) -> NuPaths {
        let bytes = std::fs::read(nu_exe).unwrap();
        NuPaths {
            nu_executable: nu_exe.to_string_lossy().into_owned(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: self
                .root()
                .join("plugins.msgpackz")
                .to_string_lossy()
                .into_owned(),
            nu_executable_hash: integrity::compute_sha256(&bytes),
            platform: "test".to_string(),
            data_dir: Some(self.root().join("data").to_string_lossy().into_owned()),
            vendor_autoload_dirs: vec![vendor.to_string()],
            vendor_autoload_dir: Some(vendor.to_string()),
        }
    }

    fn write_managed_file(&self) {
        let content = format!("{OWNERSHIP_MARKER}\n# numan managed autoload\n");
        std::fs::write(&self.managed_file, content).unwrap();
    }

    fn write_active_module_lockfile(&self, nu_hash: &str) {
        let payload_rel = "packages/modules/owner/foo/1.0.0-aabbccdd";
        let payload_abs = self.root().join(payload_rel);
        std::fs::create_dir_all(&payload_abs).unwrap();
        std::fs::write(payload_abs.join("mod.nu"), b"export def hello [] { }\n").unwrap();

        let entry_path = payload_abs.join("mod.nu").to_string_lossy().into_owned();
        let mut lockfile = Lockfile::empty();
        lockfile.packages.insert(
            "owner/foo".to_string(),
            LockfileEntry {
                hook_activation: None,
                version: "1.0.0".to_string(),
                package_type: "module".to_string(),
                source: "archive".to_string(),
                target: None,
                artifact_url: None,
                artifact_sha256: None,
                executable_path: None,
                archive_root: None,
                include: None,
                entry: Some("mod.nu".to_string()),
                installed_at: "now".to_string(),
                nu_version_at_install: Some("0.113.1".to_string()),
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
                module_activation: Some(ModuleActivation {
                    entry_path,
                    import_mode: ModuleImportMode::Module,
                    vendor_autoload_dir: self.vendor_dir_str(),
                    managed_file_path: self.managed_file.to_string_lossy().into_owned(),
                    nu_executable_sha256: nu_hash.to_string(),
                    nu_version: "0.113.1".to_string(),
                    activated_at: "now".to_string(),
                }),
                module_import_mode: Some(ModuleImportMode::Module),
                overlay: None,
                locked_dependencies: BTreeMap::new(),
            },
        );
        lockfile.save(self.root()).unwrap();
    }

    fn write_autoload_state(&self, nu_hash: &str) {
        let state = AutoloadState::new(
            self.vendor_dir_str(),
            self.managed_file.to_string_lossy().into_owned(),
            nu_hash.to_string(),
            "0.113.1".to_string(),
            integrity::compute_sha256(&std::fs::read(&self.managed_file).unwrap()),
            vec!["owner/foo".to_string()],
            "now".to_string(),
        );
        state.save(self.root()).unwrap();
    }
}

fn fake_runner_factory(_exe: &str) -> Box<dyn CandidateRunner> {
    Box::new(FakeCandidateRunner::success())
}

fn failing_runner_factory(_exe: &str) -> Box<dyn CandidateRunner> {
    Box::new(FakeCandidateRunner::failure("candidate rejected"))
}

#[test]
fn refresh_with_active_module_revalidates_and_updates_identity() {
    let env = InitRefreshEnv::new();
    let vendor = env.vendor_dir_str();
    let paths_v1 = env.paths(&env.nu_v1, &vendor);
    paths_v1.save(env.root()).unwrap();
    env.write_managed_file();
    env.write_active_module_lockfile(&env.hash_v1);
    env.write_autoload_state(&env.hash_v1);

    let paths_v2 = env.paths(&env.nu_v2, &vendor);
    let detect = {
        let paths = paths_v2.clone();
        move || Ok(paths.clone())
    };

    execute_with_runner(
        &InitArgs { refresh: true },
        env.root(),
        detect,
        fake_runner_factory,
    )
    .unwrap();

    let lockfile = Lockfile::load(env.root()).unwrap();
    let activation = lockfile.packages["owner/foo"]
        .module_activation
        .as_ref()
        .unwrap();
    assert_eq!(activation.nu_executable_sha256, env.hash_v2);

    let paths = NuPaths::load(env.root()).unwrap();
    assert_eq!(paths.nu_executable_hash, env.hash_v2);

    let state = AutoloadState::load(env.root()).unwrap().unwrap();
    assert_eq!(state.nu_executable_sha256, env.hash_v2);
}

#[test]
fn refresh_rejects_vendor_target_change_with_active_modules() {
    let env = InitRefreshEnv::new();
    let old_vendor = env.vendor_dir_str();
    let new_vendor = env.root().join("other").join("vendor").join("autoload");
    std::fs::create_dir_all(&new_vendor).unwrap();
    let new_vendor_str = new_vendor.to_string_lossy().into_owned();

    let paths_v1 = env.paths(&env.nu_v1, &old_vendor);
    paths_v1.save(env.root()).unwrap();
    env.write_managed_file();
    env.write_active_module_lockfile(&env.hash_v1);

    let paths_v2 = env.paths(&env.nu_v2, &new_vendor_str);
    let detect = {
        let paths = paths_v2.clone();
        move || Ok(paths.clone())
    };

    let err = execute_with_runner(
        &InitArgs { refresh: true },
        env.root(),
        detect,
        fake_runner_factory,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Vendor-autoload target changed"),
        "unexpected error: {msg}"
    );
}

#[test]
fn refresh_rejects_when_managed_file_candidate_validation_fails() {
    let env = InitRefreshEnv::new();
    let vendor = env.vendor_dir_str();
    let paths_v1 = env.paths(&env.nu_v1, &vendor);
    paths_v1.save(env.root()).unwrap();
    env.write_managed_file();
    env.write_active_module_lockfile(&env.hash_v1);

    let paths_v2 = env.paths(&env.nu_v2, &vendor);
    let detect = {
        let paths = paths_v2.clone();
        move || Ok(paths.clone())
    };

    let err = execute_with_runner(
        &InitArgs { refresh: true },
        env.root(),
        detect,
        failing_runner_factory,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("candidate rejected"),
        "expected validation failure, got: {err}"
    );

    let lockfile = Lockfile::load(env.root()).unwrap();
    let activation = lockfile.packages["owner/foo"]
        .module_activation
        .as_ref()
        .unwrap();
    assert_eq!(
        activation.nu_executable_sha256, env.hash_v1,
        "refresh must not commit lockfile when validation fails"
    );
}

#[test]
fn refresh_rejects_missing_managed_file_with_active_modules() {
    let env = InitRefreshEnv::new();
    let vendor = env.vendor_dir_str();
    let paths_v1 = env.paths(&env.nu_v1, &vendor);
    paths_v1.save(env.root()).unwrap();
    env.write_active_module_lockfile(&env.hash_v1);

    let paths_v2 = env.paths(&env.nu_v2, &vendor);
    let detect = {
        let paths = paths_v2.clone();
        move || Ok(paths.clone())
    };

    let err = execute_with_runner(
        &InitArgs { refresh: true },
        env.root(),
        detect,
        fake_runner_factory,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("Managed autoload file"),
        "unexpected error: {err}"
    );
}

#[test]
fn first_init_configures_official_registry_when_trust_root_is_production() {
    if OFFICIAL_REGISTRY.is_placeholder_key() {
        return;
    }

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let nu_exe = root.join("nu");
    std::fs::write(&nu_exe, b"nu-binary").unwrap();
    let vendor = root.join("vendor").join("autoload");
    std::fs::create_dir_all(&vendor).unwrap();

    let bytes = std::fs::read(&nu_exe).unwrap();
    let paths = NuPaths {
        nu_executable: nu_exe.to_string_lossy().into_owned(),
        nu_version: "0.113.1".to_string(),
        plugin_registry_path: root.join("plugins.msgpackz").to_string_lossy().into_owned(),
        nu_executable_hash: integrity::compute_sha256(&bytes),
        platform: "test".to_string(),
        data_dir: Some(root.join("data").to_string_lossy().into_owned()),
        vendor_autoload_dirs: vec![vendor.to_string_lossy().into_owned()],
        vendor_autoload_dir: Some(vendor.to_string_lossy().into_owned()),
    };
    let detect = {
        let paths = paths.clone();
        move || Ok(paths.clone())
    };

    execute_with_runner(
        &InitArgs { refresh: false },
        root,
        detect,
        fake_runner_factory,
    )
    .unwrap();

    let config = Config::load(root).unwrap();
    let official = config
        .registries
        .get(OFFICIAL_REGISTRY.name)
        .expect("official registry should be configured");
    assert_eq!(official.url, OFFICIAL_REGISTRY.production_url);
    assert!(official.enabled);
    assert!(official.trust_key.is_none());
}
