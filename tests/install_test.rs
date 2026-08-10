use ed25519_dalek::{Signer, SigningKey};
use numan_cli::core::nu_version::NuVersion;
use numan_cli::core::official_registry::RegistrySignature;
use numan_cli::core::package::{
    Artifact, Package, PackageType, RegistryIndex, ScopedId, TargetArtifact, VersionEntry,
};
use numan_cli::core::platform::{Arch, Env, Os, Platform};
use numan_cli::install::transaction::{self, InstallOptions};
use numan_cli::state::lockfile::Lockfile;
use numan_cli::state::snapshot::{list_snapshots, SnapshotTrigger};
use rand_core::OsRng;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::process::Command;
use tempfile::TempDir;

/// Helper: generate a test keypair and store the trusted key.
fn setup_trusted_key(root: &std::path::Path, registry_name: &str) -> SigningKey {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let public_key_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        verifying_key.to_bytes(),
    );

    let mut trust = numan_cli::core::trust::TrustStore {
        keys: HashMap::new(),
    };
    trust.add_key(registry_name, &public_key_b64).unwrap();
    trust.save(root).unwrap();

    signing_key
}

/// Helper: create and sign a registry index.
fn create_signed_registry(
    root: &std::path::Path,
    registry_name: &str,
    packages: Vec<Package>,
    signing_key: &SigningKey,
) -> (String, String) {
    let index = RegistryIndex {
        schema_version: 1,
        updated_at: "2026-06-27T00:00:00Z".to_string(),
        registry_revision: Some("abc123".to_string()),
        trust: None,
        packages,
    };

    let content = serde_json::to_string_pretty(&index).unwrap();
    let canonical_bytes = numan_cli::core::official_registry::canonical_json_bytes(
        &serde_json::from_str(&content).unwrap(),
    )
    .unwrap();
    let index_sha256 = numan_cli::core::integrity::compute_sha256(&canonical_bytes);

    let reg_dir = root.join("registry").join(registry_name);
    std::fs::create_dir_all(&reg_dir).unwrap();
    std::fs::write(reg_dir.join("index.json"), &content).unwrap();

    // Sign the canonical JSON bytes and write a structured envelope.
    let signature = signing_key.sign(&canonical_bytes);
    let sig_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        signature.to_bytes(),
    );
    let envelope = RegistrySignature::new(registry_name, &sig_b64);
    std::fs::write(
        reg_dir.join("index.json.sig"),
        serde_json::to_string_pretty(&envelope).unwrap(),
    )
    .unwrap();

    (index_sha256, index.registry_revision.unwrap())
}

/// Helper: create a test ZIP with plugin binary.
fn create_plugin_zip(dir: &std::path::Path, exe_name: &str, content: &[u8]) -> (String, String) {
    let zip_path = dir.join("plugin.zip");
    let file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file(exe_name, zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(content).unwrap();
    zip.finish().unwrap();

    let bytes = std::fs::read(&zip_path).unwrap();
    let sha = numan_cli::core::integrity::compute_sha256(&bytes);
    (zip_path.to_string_lossy().to_string(), sha)
}

#[test]
fn integration_full_install_from_signed_registry() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Setup: trusted key
    let signing_key = setup_trusted_key(&root, "test");

    // Setup: plugin artifact
    let artifacts_dir = root.join("artifacts");
    std::fs::create_dir_all(&artifacts_dir).unwrap();
    let (zip_url, zip_sha) =
        create_plugin_zip(&artifacts_dir, "nu_plugin_test.exe", b"plugin binary");

    // Setup: registry with the package
    let package = Package {
        id: ScopedId::new("test", "plugin"),
        description: "Test plugin for integration test".to_string(),
        repo: "https://github.com/test/plugin".to_string(),
        package_type: PackageType::Plugin,
        tags: vec!["test".to_string()],
        versions: vec![VersionEntry {
            version: semver::Version::new(1, 0, 0),
            nu_version: "*".to_string(),
            verified_with: vec![],
            artifact: Artifact {
                kind: "binary".to_string(),
                url: None,
                sha256: None,
                targets: {
                    let mut m = HashMap::new();
                    m.insert(
                        "x86_64-pc-windows-msvc".to_string(),
                        TargetArtifact {
                            url: zip_url.clone(),
                            sha256: zip_sha.clone(),
                            executable_path: "nu_plugin_test.exe".to_string(),
                        },
                    );
                    m
                },
                archive_root: None,
                include: None,
                entry: None,
            },
            source: None,
            dependencies: BTreeMap::new(),
            activation: None,
            provenance: None,
        }],
    };

    let (_index_sha, _revision) =
        create_signed_registry(&root, "test", vec![package], &signing_key);

    // Setup: config
    std::fs::write(
        root.join("config.toml"),
        "[general]\ndefault_registry = \"test\"\n",
    )
    .unwrap();

    // Act: install the package
    let platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
        env: Env::Msvc,
        triple: "x86_64-pc-windows-msvc".to_string(),
    };
    let nu_version = NuVersion::parse("0.113.1").unwrap();

    let options = InstallOptions {
        root: &root,
        platform: &platform,
        nu_version: &nu_version,
        force: false,
        verbose: false,
        registry_name: None,
        snapshot_trigger: SnapshotTrigger::Install,
    };

    let result = transaction::install_package("test/plugin", None, &options).unwrap();

    // Assert: installed
    assert!(result.installed);
    assert!(!result.already_existed);
    assert_eq!(result.version, "1.0.0");

    // Assert: file exists at immutable path
    let pkg_dir = root.join(&result.path);
    assert!(pkg_dir.exists());
    assert!(pkg_dir.join("nu_plugin_test.exe").exists());

    // Assert: lockfile entry
    let lockfile = Lockfile::load(&root).unwrap();
    let entry = lockfile.packages.get("test/plugin").unwrap();
    assert_eq!(entry.version, "1.0.0");
    assert_eq!(entry.package_type, "plugin");
    assert!(entry.payload_path.contains("packages/plugins/test/plugin/"));
    assert!(entry.index_sha256.is_some());
    assert!(entry.signing_key_fingerprint.is_some());
    assert!(entry.activation.is_none());

    // Assert: second install says "already installed"
    let result2 = transaction::install_package("test/plugin", None, &options).unwrap();
    assert!(!result2.installed);
    assert!(result2.already_existed);
}

#[test]
fn integration_install_rejects_unsigned_registry() {
    const CHILD_MARKER: &str = "NUMAN_UNSIGNED_REJECTION_TEST_CHILD";

    if std::env::var_os(CHILD_MARKER).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("integration_install_rejects_unsigned_registry")
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env_remove("NUMAN_ALLOW_UNSIGNED")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Unsigned-registry child test failed.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Create unsigned registry (no .sig file)
    let reg_dir = root.join("registry/test");
    std::fs::create_dir_all(&reg_dir).unwrap();
    let index = RegistryIndex {
        schema_version: 1,
        updated_at: "2026-06-27T00:00:00Z".to_string(),
        registry_revision: None,
        trust: None,
        packages: vec![],
    };
    std::fs::write(
        reg_dir.join("index.json"),
        serde_json::to_string_pretty(&index).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("config.toml"),
        "[general]\ndefault_registry = \"test\"\n",
    )
    .unwrap();

    let platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
        env: Env::Msvc,
        triple: "x86_64-pc-windows-msvc".to_string(),
    };
    let nu_version = NuVersion::parse("0.113.1").unwrap();
    let options = InstallOptions {
        root: &root,
        platform: &platform,
        nu_version: &nu_version,
        force: false,
        verbose: false,
        registry_name: None,
        snapshot_trigger: SnapshotTrigger::Install,
    };

    // Should fail: no signature
    let result = transaction::install_package("test/plugin", None, &options);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("no signature"),
        "Expected unsigned registry error, got: {err}"
    );
}

#[test]
fn integration_install_rejects_tampered_signature() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let _signing_key = setup_trusted_key(&root, "test");

    // Create index
    let index = RegistryIndex {
        schema_version: 1,
        updated_at: "2026-06-27T00:00:00Z".to_string(),
        registry_revision: Some("abc123".to_string()),
        trust: None,
        packages: vec![],
    };
    let content = serde_json::to_string_pretty(&index).unwrap();
    let canonical_bytes = numan_cli::core::official_registry::canonical_json_bytes(
        &serde_json::from_str(&content).unwrap(),
    )
    .unwrap();

    let reg_dir = root.join("registry/test");
    std::fs::create_dir_all(&reg_dir).unwrap();
    std::fs::write(reg_dir.join("index.json"), &content).unwrap();

    // Sign with a DIFFERENT key
    let wrong_key = SigningKey::generate(&mut OsRng);
    let signature = wrong_key.sign(&canonical_bytes);
    let sig_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        signature.to_bytes(),
    );
    let envelope = RegistrySignature::new("test", &sig_b64);
    std::fs::write(
        reg_dir.join("index.json.sig"),
        serde_json::to_string_pretty(&envelope).unwrap(),
    )
    .unwrap();

    std::fs::write(
        root.join("config.toml"),
        "[general]\ndefault_registry = \"test\"\n",
    )
    .unwrap();

    let platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
        env: Env::Msvc,
        triple: "x86_64-pc-windows-msvc".to_string(),
    };
    let nu_version = NuVersion::parse("0.113.1").unwrap();
    let options = InstallOptions {
        root: &root,
        platform: &platform,
        nu_version: &nu_version,
        force: false,
        verbose: false,
        registry_name: None,
        snapshot_trigger: SnapshotTrigger::Install,
    };

    let result = transaction::install_package("test/plugin", None, &options);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("signature verification failed"),
        "Expected tampered sig error, got: {err}"
    );
}

#[test]
fn integration_resolve_exact_rejects_incompatible() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let signing_key = setup_trusted_key(&root, "test");

    let package = Package {
        id: ScopedId::new("test", "plugin"),
        description: "Test plugin".to_string(),
        repo: "https://github.com/test/plugin".to_string(),
        package_type: PackageType::Plugin,
        tags: vec![],
        versions: vec![VersionEntry {
            version: semver::Version::new(1, 0, 0),
            nu_version: ">=0.113.0 <0.114.0".to_string(),
            verified_with: vec![],
            artifact: Artifact {
                kind: "binary".to_string(),
                url: None,
                sha256: None,
                targets: {
                    let mut m = HashMap::new();
                    m.insert(
                        "x86_64-pc-windows-msvc".to_string(),
                        TargetArtifact {
                            url: "https://example.com/pkg.zip".to_string(),
                            sha256: "abc123".to_string(),
                            executable_path: "nu_plugin_test.exe".to_string(),
                        },
                    );
                    m
                },
                archive_root: None,
                include: None,
                entry: None,
            },
            source: None,
            dependencies: BTreeMap::new(),
            activation: None,
            provenance: None,
        }],
    };

    create_signed_registry(&root, "test", vec![package], &signing_key);
    std::fs::write(
        root.join("config.toml"),
        "[general]\ndefault_registry = \"test\"\n",
    )
    .unwrap();

    // Use Nu 0.112.x — incompatible with v1.0.0 which requires >=0.113.0
    let platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
        env: Env::Msvc,
        triple: "x86_64-pc-windows-msvc".to_string(),
    };
    let nu_version = NuVersion::parse("0.112.5").unwrap();
    let options = InstallOptions {
        root: &root,
        platform: &platform,
        nu_version: &nu_version,
        force: false,
        verbose: false,
        registry_name: None,
        snapshot_trigger: SnapshotTrigger::Install,
    };

    let result = transaction::install_package("test/plugin", Some("1.0.0"), &options);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not compatible"),
        "Expected compatibility error, got: {err}"
    );
}

#[test]
fn integration_snapshot_before_install() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let signing_key = setup_trusted_key(&root, "test");

    let artifacts_dir = root.join("artifacts");
    std::fs::create_dir_all(&artifacts_dir).unwrap();
    let (zip_url, zip_sha) =
        create_plugin_zip(&artifacts_dir, "nu_plugin_test.exe", b"plugin binary");

    let package = Package {
        id: ScopedId::new("test", "plugin"),
        description: "Test plugin".to_string(),
        repo: "https://github.com/test/plugin".to_string(),
        package_type: PackageType::Plugin,
        tags: vec![],
        versions: vec![VersionEntry {
            version: semver::Version::new(1, 0, 0),
            nu_version: "*".to_string(),
            verified_with: vec![],
            artifact: Artifact {
                kind: "binary".to_string(),
                url: None,
                sha256: None,
                targets: {
                    let mut m = HashMap::new();
                    m.insert(
                        "x86_64-pc-windows-msvc".to_string(),
                        TargetArtifact {
                            url: zip_url,
                            sha256: zip_sha,
                            executable_path: "nu_plugin_test.exe".to_string(),
                        },
                    );
                    m
                },
                archive_root: None,
                include: None,
                entry: None,
            },
            source: None,
            dependencies: BTreeMap::new(),
            activation: None,
            provenance: None,
        }],
    };

    create_signed_registry(&root, "test", vec![package.clone()], &signing_key);
    std::fs::write(
        root.join("config.toml"),
        "[general]\ndefault_registry = \"test\"\n",
    )
    .unwrap();

    let platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
        env: Env::Msvc,
        triple: "x86_64-pc-windows-msvc".to_string(),
    };
    let nu_version = NuVersion::parse("0.113.1").unwrap();
    let options = InstallOptions {
        root: &root,
        platform: &platform,
        nu_version: &nu_version,
        force: false,
        verbose: false,
        registry_name: None,
        snapshot_trigger: SnapshotTrigger::Install,
    };

    // First install — no snapshot (lockfile was empty)
    transaction::install_package("test/plugin", None, &options).unwrap();
    assert!(
        !root.join("state/snapshots").exists(),
        "Should not snapshot from empty lockfile"
    );

    // Create a second package to trigger snapshot
    let (zip_url2, zip_sha2) =
        create_plugin_zip(&artifacts_dir, "nu_plugin_other.exe", b"other binary");
    let package2 = Package {
        id: ScopedId::new("test", "other"),
        description: "Another plugin".to_string(),
        repo: "https://github.com/test/other".to_string(),
        package_type: PackageType::Plugin,
        tags: vec![],
        versions: vec![VersionEntry {
            version: semver::Version::new(1, 0, 0),
            nu_version: "*".to_string(),
            verified_with: vec![],
            artifact: Artifact {
                kind: "binary".to_string(),
                url: None,
                sha256: None,
                targets: {
                    let mut m = HashMap::new();
                    m.insert(
                        "x86_64-pc-windows-msvc".to_string(),
                        TargetArtifact {
                            url: zip_url2,
                            sha256: zip_sha2,
                            executable_path: "nu_plugin_other.exe".to_string(),
                        },
                    );
                    m
                },
                archive_root: None,
                include: None,
                entry: None,
            },
            source: None,
            dependencies: BTreeMap::new(),
            activation: None,
            provenance: None,
        }],
    };

    // Re-create registry with both packages
    create_signed_registry(&root, "test", vec![package, package2], &signing_key);

    // Second install — should snapshot before mutation
    transaction::install_package("test/other", None, &options).unwrap();
    assert!(
        root.join("state/snapshots").exists(),
        "Should have snapshots directory after second install"
    );
    let snapshots = list_snapshots(&root).unwrap();
    assert_eq!(snapshots.len(), 1, "Should have exactly one snapshot");
}
