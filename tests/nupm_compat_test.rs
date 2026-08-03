//! Phase 6.1 integration tests (T13–T15).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use numan_cli::cmd::activate::{execute_with_candidate_runner, ActivateArgs};
use numan_cli::cmd::nupm::{self, DiffArgs, NupmArgs, NupmCommands, StatusArgs};
use numan_cli::core::integrity;
use numan_cli::core::package::{ModuleImportMode, ScopedId};
use numan_cli::nu::autoload::FakeCandidateRunner;
use numan_cli::nu::paths::NuPaths;
use numan_cli::nupm_compat::compare_import;
use numan_cli::nupm_compat::drift::DriftStatus;
use numan_cli::nupm_compat::import_manifest_with_runner;
use numan_cli::nupm_compat::import_module_with_runner;
use numan_cli::nupm_compat::schema::{METADATA_FILENAME, NUPM_IMPORT_ORIGIN};
use numan_cli::state::lifecycle_journal::{LifecycleOp, LifecycleStage, PendingLifecycle};
use numan_cli::state::lockfile::Lockfile;
use numan_cli::state::nupm_import::NupmImportsFile;
use numan_cli::util::fs_safety::is_symlink_or_reparse;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nupm")
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestEntry {
    rel_path: String,
    is_symlink: bool,
    is_dir: bool,
    sha256: Option<String>,
}

fn fixture_manifest(root: &Path) -> BTreeMap<String, ManifestEntry> {
    let mut out = BTreeMap::new();
    walk_manifest(root, root, &mut out);
    out
}

fn walk_manifest(base: &Path, dir: &Path, out: &mut BTreeMap<String, ManifestEntry>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = path
            .strip_prefix(base)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let meta = fs::symlink_metadata(&path).ok();
        let is_symlink = is_symlink_or_reparse(&path).unwrap_or(false);
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let sha256 = if meta.as_ref().is_some_and(|m| m.is_file()) && !is_symlink {
            let bytes = fs::read(&path).unwrap_or_default();
            Some(hex::encode(Sha256::digest(bytes)))
        } else {
            None
        };
        out.insert(
            rel.clone(),
            ManifestEntry {
                rel_path: rel,
                is_symlink,
                is_dir,
                sha256,
            },
        );
        if is_dir && !is_symlink {
            walk_manifest(base, &path, out);
        }
    }
}

#[test]
fn t13_nupm_home_layout_installed_only() {
    let home = fixtures_root().join("nupm-home-layout");
    let scan = numan_cli::nupm_compat::scan_nupm_home(&home).unwrap();
    assert_eq!(scan.installed_only.len(), 1);
    assert!(scan
        .source_roots
        .iter()
        .all(|r| r.compatibility != numan_cli::nupm_compat::NupmCompatibility::ImportableModule));
}

#[test]
fn t14_inspect_all_without_home_errors_status_ok() {
    let prev = std::env::var_os("NUPM_HOME");
    std::env::remove_var("NUPM_HOME");

    let root = TempDir::new().unwrap();
    let mut buf = Vec::new();
    let status_args = NupmArgs {
        command: NupmCommands::Status(StatusArgs { nupm_home: None }),
    };
    nupm::execute(&status_args, root.path(), &mut buf).unwrap();
    assert!(String::from_utf8(buf).unwrap().contains("not configured"));

    let inspect_args = NupmArgs {
        command: NupmCommands::Inspect(nupm::InspectArgs {
            all: true,
            path: None,
            nupm_home: None,
            exit_on_ineligible: false,
        }),
    };
    let mut buf2 = Vec::new();
    assert!(nupm::execute(&inspect_args, root.path(), &mut buf2).is_err());

    if let Some(p) = prev {
        std::env::set_var("NUPM_HOME", p);
    }
}

#[test]
fn status_fails_on_corrupt_lockfile() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("lockfile"), b"{not json").unwrap();
    let args = NupmArgs {
        command: NupmCommands::Status(StatusArgs { nupm_home: None }),
    };
    let mut buf = Vec::new();
    assert!(nupm::execute(&args, root.path(), &mut buf).is_err());
}

#[test]
fn t15_no_mutation_under_nupm_home_fixture() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("nupm-home");
    copy_dir_all(&fixtures_root().join("nupm-home-layout"), &home).unwrap();

    let before = fixture_manifest(&home);

    let root = TempDir::new().unwrap();
    let mut buf = Vec::new();
    let args = NupmArgs {
        command: NupmCommands::Status(StatusArgs {
            nupm_home: Some(home.clone()),
        }),
    };
    nupm::execute(&args, root.path(), &mut buf).unwrap();

    let mut buf2 = Vec::new();
    let inspect_args = NupmArgs {
        command: NupmCommands::Inspect(nupm::InspectArgs {
            all: true,
            path: None,
            nupm_home: Some(home.clone()),
            exit_on_ineligible: false,
        }),
    };
    nupm::execute(&inspect_args, root.path(), &mut buf2).unwrap();

    let path = fixtures_root().join("supported/minimal-module");
    let mut buf3 = Vec::new();
    let single = NupmArgs {
        command: NupmCommands::Inspect(nupm::InspectArgs {
            all: false,
            path: Some(path),
            nupm_home: None,
            exit_on_ineligible: false,
        }),
    };
    nupm::execute(&single, root.path(), &mut buf3).unwrap();

    let after = fixture_manifest(&home);
    assert_eq!(before, after);
}

#[test]
fn inspect_supported_minimal_module() {
    let root = TempDir::new().unwrap();
    let path = fixtures_root().join("supported/minimal-module");
    let mut buf = Vec::new();
    let args = NupmArgs {
        command: NupmCommands::Inspect(nupm::InspectArgs {
            all: false,
            path: Some(path),
            nupm_home: None,
            exit_on_ineligible: false,
        }),
    };
    nupm::execute(&args, root.path(), &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("ImportableModule"));
    assert!(out.contains("Eligible:     yes"));
    assert!(out.contains("Import:       numan nupm import"));
}

#[test]
fn t16_import_minimal_module_writes_state() {
    let root = TempDir::new().unwrap();
    let source = fixtures_root().join("supported/minimal-module");
    let result = import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/minimal").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap();
    assert!(root
        .path()
        .join(&result.payload_path)
        .join("mod.nu")
        .is_file());
    let lockfile = Lockfile::load(root.path()).unwrap();
    let entry = lockfile.packages.get("test/minimal").unwrap();
    assert_eq!(entry.origin.as_deref(), Some(NUPM_IMPORT_ORIGIN));
    assert!(NupmImportsFile::load(root.path())
        .unwrap()
        .imports
        .contains_key("test/minimal"));
}

#[test]
fn t17_import_does_not_mutate_nupm_source_fixture() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("pkg");
    copy_dir_all(&fixtures_root().join("supported/minimal-module"), &source).unwrap();
    let before = fixture_manifest(&source);

    let root = TempDir::new().unwrap();
    import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/minimal").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap();

    assert_eq!(before, fixture_manifest(&source));
}

#[test]
fn t18_registry_collision_fails_without_lockfile_change() {
    let root = TempDir::new().unwrap();
    let mut lockfile = Lockfile::empty();
    lockfile.packages.insert(
        "test/minimal".to_string(),
        numan_cli::state::lockfile::LockfileEntry {
            version: "9.9.9".to_string(),
            package_type: "module".to_string(),
            source: "registry".to_string(),
            installed_at: "0".to_string(),
            payload_path: "packages/modules/test/minimal/9.9.9-deadbeef".to_string(),
            origin: Some("registry:official".to_string()),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: None,
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
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: Default::default(),
        },
    );
    lockfile.save(root.path()).unwrap();

    let source = fixtures_root().join("supported/minimal-module");
    assert!(import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/minimal").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .is_err());
    assert_eq!(
        Lockfile::load(root.path()).unwrap().packages["test/minimal"].version,
        "9.9.9"
    );
}

#[test]
fn t19_stale_nupm_import_journal_blocks_retry() {
    let root = TempDir::new().unwrap();
    PendingLifecycle {
        op: LifecycleOp::NupmImport,
        package_id: "test/minimal".to_string(),
        stage: LifecycleStage::PayloadsStaged,
        orphan_payload_path: None,
        from_version: None,
        to_version: None,
        nupm_source_path: Some("/tmp/pkg".to_string()),
        nupm_metadata_sha256: Some("abc".to_string()),
        staging_dir: Some("packages/modules/test/minimal/.staging".to_string()),
        promoted_payload_path: None,
        batch_package_ids: Vec::new(),
        batch_staging_dirs: Vec::new(),
        target_snapshot_id: None,
        pre_rollback_snapshot_id: None,
        needs_reactivate: false,
    }
    .save(root.path())
    .unwrap();

    let source = fixtures_root().join("supported/minimal-module");
    let err = import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/minimal").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("interrupted"));
}

#[test]
fn t20_source_change_does_not_alter_installed_revision() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("pkg");
    copy_dir_all(&fixtures_root().join("supported/minimal-module"), &source).unwrap();

    let root = TempDir::new().unwrap();
    let first = import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/minimal").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap();

    fs::write(
        source.join("minimal-module/mod.nu"),
        b"export def changed [] { 99 }",
    )
    .unwrap();

    let lockfile = Lockfile::load(root.path()).unwrap();
    assert_eq!(
        lockfile
            .packages
            .get("test/minimal")
            .unwrap()
            .revision_id
            .as_deref(),
        Some(first.revision_id.as_str())
    );
}

fn write_nu_paths(root: &Path) {
    let nu_exe = root.join("fake_nu");
    std::fs::write(&nu_exe, b"fake nu binary").unwrap();
    let nu_hash = integrity::compute_sha256(b"fake nu binary");
    let vendor_dir = root.join("vendor").join("autoload");
    std::fs::create_dir_all(&vendor_dir).unwrap();
    let vendor = vendor_dir.to_string_lossy().into_owned();
    let paths = NuPaths {
        nu_executable: nu_exe.to_string_lossy().into_owned(),
        nu_version: "0.113.1".to_string(),
        plugin_registry_path: root.join("plugins.msgpackz").to_string_lossy().into_owned(),
        nu_executable_hash: nu_hash,
        platform: "x86_64-pc-windows-msvc".to_string(),
        data_dir: Some(root.join("data").to_string_lossy().into_owned()),
        vendor_autoload_dirs: vec![vendor.clone()],
        vendor_autoload_dir: Some(vendor),
    };
    paths.save(root).unwrap();
}

#[test]
fn t21_reimport_updates_revision_after_source_change() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("pkg");
    copy_dir_all(&fixtures_root().join("supported/minimal-module"), &source).unwrap();

    let root = TempDir::new().unwrap();
    let first = import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/minimal").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap();

    fs::write(
        source.join("minimal-module/mod.nu"),
        b"export def changed [] { 99 }",
    )
    .unwrap();

    let second = import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/minimal").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap();

    assert_ne!(first.revision_id, second.revision_id);
    assert!(second.reimported);
    assert_eq!(
        Lockfile::load(root.path())
            .unwrap()
            .packages
            .get("test/minimal")
            .unwrap()
            .payload_path,
        second.payload_path
    );
}

#[test]
fn t22_manifest_import_atomic() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("nupm-home");
    copy_dir_all(&fixtures_root().join("supported/minimal-module"), &home).unwrap();

    let manifest_path = tmp.path().join("imports.toml");
    fs::write(
        &manifest_path,
        r#"[[imports]]
source = "."
as = "test/minimal"
"#,
    )
    .unwrap();

    let root = TempDir::new().unwrap();
    let result = import_manifest_with_runner(
        root.path(),
        &manifest_path,
        Some(&home),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap();
    assert_eq!(result.imports.len(), 1);
    assert!(Lockfile::load(root.path())
        .unwrap()
        .packages
        .contains_key("test/minimal"));
}

#[test]
fn t23_diff_and_status_drift_count() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("pkg");
    copy_dir_all(&fixtures_root().join("supported/minimal-module"), &source).unwrap();

    let root = TempDir::new().unwrap();
    import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/minimal").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap();

    let report = compare_import(root.path(), "test/minimal").unwrap();
    assert_eq!(report.status, DriftStatus::Unchanged);

    fs::write(
        source.join("minimal-module/mod.nu"),
        b"export def drift [] { 1 }",
    )
    .unwrap();

    let report = compare_import(root.path(), "test/minimal").unwrap();
    assert_eq!(report.status, DriftStatus::PayloadChanged);

    let mut buf = Vec::new();
    let args = NupmArgs {
        command: NupmCommands::Status(StatusArgs { nupm_home: None }),
    };
    nupm::execute(&args, root.path(), &mut buf).unwrap();
    assert!(String::from_utf8(buf)
        .unwrap()
        .contains("Source drift (imports): 1"));

    let mut diff_buf = Vec::new();
    let diff_args = NupmArgs {
        command: NupmCommands::Diff(DiffArgs {
            package_id: "test/minimal".to_string(),
        }),
    };
    nupm::execute(&diff_args, root.path(), &mut diff_buf).unwrap();
    let diff_out = String::from_utf8(diff_buf).unwrap();
    assert!(diff_out.contains("PayloadChanged"));
}

#[test]
fn t24_import_then_activate_nupm_module() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("pkg");
    copy_dir_all(&fixtures_root().join("supported/minimal-module"), &source).unwrap();

    let root = TempDir::new().unwrap();
    write_nu_paths(root.path());
    import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/minimal").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap();

    let args = ActivateArgs {
        packages: vec!["test/minimal".to_string()],
        verbose: false,
        list: false,
        check: false,
    };
    let ok_registrar = |_: &str, _: &str, _: &str| Ok(());
    execute_with_candidate_runner(
        &args,
        root.path(),
        &ok_registrar,
        &FakeCandidateRunner::success(),
    )
    .unwrap();

    let lockfile = Lockfile::load(root.path()).unwrap();
    let entry = lockfile.packages.get("test/minimal").unwrap();
    assert!(entry.module_activation.is_some());
    assert_eq!(entry.module_import_mode, Some(ModuleImportMode::Module));

    let managed = root.path().join("vendor/autoload/numan.nu");
    let content = fs::read_to_string(managed).unwrap();
    assert!(content.contains("use"));
}

#[test]
fn t24_unicode_path_inspect_and_import() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("café-pkg");
    copy_dir_all(&fixtures_root().join("supported/minimal-module"), &source).unwrap();

    let root = TempDir::new().unwrap();
    let mut buf = Vec::new();
    let args = NupmArgs {
        command: NupmCommands::Inspect(nupm::InspectArgs {
            all: false,
            path: Some(source.clone()),
            nupm_home: None,
            exit_on_ineligible: false,
        }),
    };
    nupm::execute(&args, root.path(), &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("café-pkg"));
    assert!(out.contains("ImportableModule"));

    let result = import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/cafe").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap();
    assert!(root
        .path()
        .join(&result.payload_path)
        .join("mod.nu")
        .is_file());
}

#[cfg(unix)]
#[test]
fn t25_symlink_in_module_tree_rejected_at_import() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("pkg");
    copy_dir_all(&fixtures_root().join("supported/minimal-module"), &source).unwrap();
    let module_dir = source.join("minimal-module");
    symlink(module_dir.join("mod.nu"), module_dir.join("escape.nu")).unwrap();

    let root = TempDir::new().unwrap();
    let err = import_module_with_runner(
        root.path(),
        &source,
        &ScopedId::parse("test/minimal").unwrap(),
        true,
        &FakeCandidateRunner::success(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("Unsafe filesystem layout"));
    assert!(!Lockfile::load(root.path())
        .unwrap()
        .packages
        .contains_key("test/minimal"));
}

// silence unused import warning for METADATA_FILENAME if not used
#[allow(dead_code)]
fn _metadata_name() -> &'static str {
    METADATA_FILENAME
}
