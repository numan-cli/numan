use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::core::integrity;
use crate::core::official_registry::{
    official_built_in_root, RegistrySignature, RegistryTrustRoot,
};
use crate::core::package::{Package, RegistryIndex};
use crate::core::trust::TrustStore;
use crate::util::hints::{self, CMD_REGISTRY_SYNC};

pub struct RegistryManager {
    root: PathBuf,
    trust: TrustStore,
}

/// Registry index loaded with signature policy applied.
pub struct VerifiedRegistry {
    pub index: RegistryIndex,
    pub registry_name: String,
    pub key_id: String,
    pub index_sha256: String,
    pub signing_key_fingerprint: Option<String>,
}

fn verified_signing_key_fingerprint(
    trust_root: &RegistryTrustRoot,
    key_id: &str,
) -> Result<String> {
    let key = trust_root.verifying_key(key_id)?;
    Ok(crate::core::trust::compute_fingerprint(&key.to_bytes()))
}

impl RegistryManager {
    pub fn new(root: &Path) -> Result<Self> {
        let trust = TrustStore::load(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            trust,
        })
    }

    /// Path to the last-known-good verified index for a registry.
    pub fn last_known_good_index_path(&self, registry_name: &str) -> PathBuf {
        self.root.join(format!(
            "registry/{registry_name}/index.json.last-known-good"
        ))
    }

    /// Path to the last-known-good signature for a registry.
    pub fn last_known_good_sig_path(&self, registry_name: &str) -> PathBuf {
        self.root.join(format!(
            "registry/{registry_name}/index.json.sig.last-known-good"
        ))
    }

    fn base_trust_root_for(&self, registry_name: &str) -> RegistryTrustRoot {
        if registry_name == crate::core::official_registry::OFFICIAL_REGISTRY.name {
            official_built_in_root()
        } else {
            let mut root = RegistryTrustRoot::new(registry_name);
            if let Some(key) = self.trust.keys.get(registry_name) {
                // For custom registries, the existing trust store keys the public
                // key by registry name. Treat that name as the key_id for backward
                // compatibility until the registry and CLI gain explicit key-id
                // support.
                let _ = root.add_key(registry_name, &key.public_key_b64);
            }
            root
        }
    }

    fn trust_root_for(&self, registry_name: &str) -> RegistryTrustRoot {
        let mut root = self.base_trust_root_for(registry_name);
        if let Ok(derived) = self.load_derived_keys(registry_name) {
            for key in derived.keys {
                // Ignore malformed derived keys; a bad derived key does not break
                // the base trust root.
                let _ = root.add_key(&key.key_id, &key.public_key_b64);
            }
        }
        root
    }

    /// Path to the derived-keys file for a registry.
    ///
    /// Derived keys are successor keys introduced by a signed index. They are
    /// cached locally, but they are always secondary to the base trust root.
    pub fn derived_keys_path(&self, registry_name: &str) -> PathBuf {
        self.root
            .join(format!("registry/{registry_name}/derived_keys.json"))
    }

    fn load_derived_keys(
        &self,
        registry_name: &str,
    ) -> Result<crate::core::official_registry::RegistryTrustExtension> {
        let path = self.derived_keys_path(registry_name);
        if !path.exists() {
            return Ok(crate::core::official_registry::RegistryTrustExtension::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read derived keys for '{registry_name}'"))?;
        let derived = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse derived keys for '{registry_name}'"))?;
        Ok(derived)
    }

    /// Persist successor keys derived from a signed index.
    ///
    /// The base trust root is never written here; only keys that were introduced
    /// by a signed index are cached. This cache is read on subsequent starts to
    /// build the effective trust root.
    fn persist_derived_keys(
        &self,
        registry_name: &str,
        extension: &crate::core::official_registry::RegistryTrustExtension,
    ) -> Result<()> {
        let path = self.derived_keys_path(registry_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::util::atomic::write_json_atomic(&path, extension)?;
        Ok(())
    }

    pub fn index_path(&self, registry_name: &str) -> PathBuf {
        self.root
            .join(format!("registry/{registry_name}/index.json"))
    }

    pub fn sig_path(&self, registry_name: &str) -> PathBuf {
        self.root
            .join(format!("registry/{registry_name}/index.json.sig"))
    }

    pub fn load_index(&self, registry_name: &str) -> Result<RegistryIndex> {
        let path = self.index_path(registry_name);
        let content = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "Registry '{registry_name}' not synced. {}",
                hints::run(CMD_REGISTRY_SYNC)
            )
        })?;
        let index: RegistryIndex = serde_json::from_str(&content)?;
        Ok(index)
    }

    pub fn load_index_from_str(&self, content: &str) -> Result<RegistryIndex> {
        let index: RegistryIndex = serde_json::from_str(content)?;
        Ok(index)
    }

    pub fn save_index(&self, registry_name: &str, index: &RegistryIndex) -> Result<()> {
        let path = self.index_path(registry_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(index)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    pub fn save_signature(&self, registry_name: &str, key_id: &str, sig_b64: &str) -> Result<()> {
        let path = self.sig_path(registry_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let envelope = RegistrySignature::new(key_id, sig_b64);
        let content = serde_json::to_string_pretty(&envelope)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    pub fn verify_and_load(&self, registry_name: &str) -> Result<RegistryIndex> {
        let verified = self.load_verified(registry_name)?;
        Ok(verified.index)
    }

    pub fn search(&self, query: &str) -> Result<Vec<Package>> {
        let index = self.load_index(&self.default_registry())?;
        let query_lower = query.to_lowercase();

        let results: Vec<Package> = index
            .packages
            .into_iter()
            .filter(|p| {
                p.id.to_string().to_lowercase().contains(&query_lower)
                    || p.description.to_lowercase().contains(&query_lower)
                    || p.tags
                        .iter()
                        .any(|t| t.to_lowercase().contains(&query_lower))
            })
            .collect();

        Ok(results)
    }

    pub fn find_package(&self, id: &str) -> Result<Option<Package>> {
        let index = self.load_index(&self.default_registry())?;
        Ok(index.packages.into_iter().find(|p| p.id.to_string() == id))
    }

    fn default_registry(&self) -> String {
        let config_path = self.root.join("config.toml");
        if config_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                if let Ok(config) = toml::from_str::<crate::config::Config>(&content) {
                    return config.general.default_registry;
                }
            }
        }
        "official".to_string()
    }

    /// Get the default registry name (public).
    pub fn default_registry_name(&self) -> String {
        self.default_registry()
    }

    /// Get the signing key fingerprint for a registry, if a trusted key exists.
    pub fn signing_key_fingerprint(&self, registry_name: &str) -> Option<String> {
        self.trust
            .keys
            .get(registry_name)
            .map(|k| k.fingerprint.clone())
    }

    /// Atomically replace a verified index for a registry.
    ///
    /// Before replacing the existing index, the current verified index and
    /// signature are copied to `.last-known-good` versions. The new index and
    /// signature are written to temp files and renamed into place only after
    /// signature and schema validation succeeds.
    pub fn replace_index(
        &self,
        registry_name: &str,
        index_content: &str,
        signature: &RegistrySignature,
    ) -> Result<VerifiedRegistry> {
        let trust_root = self.trust_root_for(registry_name);
        let verified = crate::core::official_registry::verify_registry_index(
            registry_name,
            &trust_root,
            index_content,
            signature,
        )?;
        let signing_key_fingerprint =
            verified_signing_key_fingerprint(&trust_root, &verified.key_id)?;

        let index_path = self.index_path(registry_name);
        let sig_path = self.sig_path(registry_name);
        if let Some(parent) = index_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Preserve last-known-good before overwriting.
        if index_path.exists() && sig_path.exists() {
            let lkg_index_path = self.last_known_good_index_path(registry_name);
            let lkg_sig_path = self.last_known_good_sig_path(registry_name);
            if let Some(parent) = lkg_index_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let prior_index = std::fs::read(&index_path)
                .with_context(|| "Failed to read prior index for last-known-good preservation")?;
            let prior_sig = std::fs::read(&sig_path).with_context(|| {
                "Failed to read prior signature for last-known-good preservation"
            })?;
            crate::util::atomic::write_bytes_atomic(&lkg_index_path, &prior_index)
                .with_context(|| "Failed to preserve last-known-good index")?;
            crate::util::atomic::write_bytes_atomic(&lkg_sig_path, &prior_sig)
                .with_context(|| "Failed to preserve last-known-good signature")?;
        }

        crate::util::atomic::write_bytes_atomic(&index_path, index_content.as_bytes())?;
        crate::util::atomic::write_bytes_atomic(
            &sig_path,
            serde_json::to_string_pretty(signature)?.as_bytes(),
        )?;

        // Persist successor keys introduced by the newly verified index.
        self.persist_derived_keys(registry_name, &verified.trust_extension)?;

        Ok(VerifiedRegistry {
            index: verified.index,
            registry_name: verified.registry_name,
            key_id: verified.key_id,
            index_sha256: verified.index_sha256,
            signing_key_fingerprint: Some(signing_key_fingerprint),
        })
    }

    /// Load the last-known-good verified index for a registry, if one exists.
    pub fn load_last_known_good(&self, registry_name: &str) -> Result<VerifiedRegistry> {
        let index_path = self.last_known_good_index_path(registry_name);
        let sig_path = self.last_known_good_sig_path(registry_name);
        if !index_path.exists() || !sig_path.exists() {
            bail!("No last-known-good index for registry '{registry_name}'");
        }
        let index_content = std::fs::read_to_string(&index_path)?;
        let sig_content = std::fs::read_to_string(&sig_path)?;
        let signature: RegistrySignature =
            serde_json::from_str(&sig_content).with_context(|| {
                format!("Last-known-good signature for '{registry_name}' is invalid")
            })?;
        let trust_root = self.trust_root_for(registry_name);
        let verified = crate::core::official_registry::verify_registry_index(
            registry_name,
            &trust_root,
            &index_content,
            &signature,
        )?;
        let signing_key_fingerprint =
            verified_signing_key_fingerprint(&trust_root, &verified.key_id)?;
        Ok(VerifiedRegistry {
            index: verified.index,
            registry_name: verified.registry_name,
            key_id: verified.key_id,
            index_sha256: verified.index_sha256,
            signing_key_fingerprint: Some(signing_key_fingerprint),
        })
    }

    /// Load a registry index, enforcing signature verification when a sig file exists.
    pub fn load_verified(&self, registry_name: &str) -> Result<VerifiedRegistry> {
        let sig_path = self.sig_path(registry_name);
        if sig_path.exists() {
            let index_content = std::fs::read_to_string(self.index_path(registry_name))?;
            let sig_content = std::fs::read_to_string(&sig_path)?;
            let signature: RegistrySignature =
                serde_json::from_str(&sig_content).with_context(|| {
                    format!("Registry '{registry_name}' signature file is not valid JSON")
                })?;

            let trust_root = self.trust_root_for(registry_name);
            let verified = crate::core::official_registry::verify_registry_index(
                registry_name,
                &trust_root,
                &index_content,
                &signature,
            )?;

            let fingerprint = verified_signing_key_fingerprint(&trust_root, &verified.key_id)?;
            Ok(VerifiedRegistry {
                index: verified.index,
                registry_name: verified.registry_name,
                key_id: verified.key_id,
                index_sha256: verified.index_sha256,
                signing_key_fingerprint: Some(fingerprint),
            })
        } else if std::env::var("NUMAN_ALLOW_UNSIGNED").unwrap_or_default() != "1" {
            bail!(
                "Registry '{}' has no signature file. \
                 Signatures are required by default. \
                 Set NUMAN_ALLOW_UNSIGNED=1 to override (development only).",
                registry_name
            );
        } else {
            let index = self.load_index(registry_name)?;
            let index_content = std::fs::read_to_string(self.index_path(registry_name))?;
            let index_sha256 = integrity::compute_sha256(index_content.as_bytes());
            Ok(VerifiedRegistry {
                index,
                registry_name: registry_name.to_string(),
                key_id: "unsigned".to_string(),
                index_sha256,
                signing_key_fingerprint: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::official_registry::OFFICIAL_REGISTRY;
    use crate::core::package::*;
    use ed25519_dalek::Signer;
    use std::collections::{BTreeMap, HashMap};

    fn test_index() -> RegistryIndex {
        RegistryIndex {
            schema_version: 1,
            updated_at: "2026-06-27T00:00:00Z".to_string(),
            registry_revision: Some("abc123".to_string()),
            trust: None,
            packages: vec![Package {
                id: ScopedId::new("test", "pkg"),
                description: "A test package".to_string(),
                repo: "https://github.com/test/pkg".to_string(),
                package_type: PackageType::Plugin,
                tags: vec!["test".to_string()],
                versions: vec![VersionEntry {
                    version: semver::Version::new(1, 0, 0),
                    nu_version: ">=0.113.0 <0.114.0".to_string(),
                    verified_with: vec![],
                    artifact: Artifact {
                        kind: "binary".to_string(),
                        url: None,
                        sha256: None,
                        targets: HashMap::new(),
                        archive_root: None,
                        include: None,
                        entry: None,
                    },
                    source: None,
                    dependencies: BTreeMap::new(),
                    activation: None,
                    provenance: None,
                    evidence_tier: None,
                    deferral_reason: None,
                }],
            }],
        }
    }

    fn setup_root() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("registry/official")).unwrap();

        let index = test_index();
        let content = serde_json::to_string_pretty(&index).unwrap();
        std::fs::write(root.join("registry/official/index.json"), content).unwrap();

        std::fs::write(
            root.join("config.toml"),
            "[general]\ndefault_registry = \"official\"\n",
        )
        .unwrap();

        tmp
    }

    #[test]
    fn verified_fingerprint_uses_built_in_official_key() {
        let trust_root = official_built_in_root();
        let key = trust_root.verifying_key(OFFICIAL_REGISTRY.key_id).unwrap();

        assert_eq!(
            verified_signing_key_fingerprint(&trust_root, OFFICIAL_REGISTRY.key_id).unwrap(),
            crate::core::trust::compute_fingerprint(&key.to_bytes())
        );
    }

    #[test]
    fn verified_fingerprint_uses_exact_successor_key_id() {
        let initial_key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        let successor_key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        let mut trust_root = RegistryTrustRoot::new("custom");
        for (key_id, key) in [
            ("initial", initial_key.verifying_key()),
            ("successor", successor_key.verifying_key()),
        ] {
            let public_key_b64 =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key.to_bytes());
            trust_root.add_key(key_id, &public_key_b64).unwrap();
        }

        assert_eq!(
            verified_signing_key_fingerprint(&trust_root, "successor").unwrap(),
            crate::core::trust::compute_fingerprint(&successor_key.verifying_key().to_bytes())
        );
        assert_ne!(
            verified_signing_key_fingerprint(&trust_root, "successor").unwrap(),
            crate::core::trust::compute_fingerprint(&initial_key.verifying_key().to_bytes())
        );
    }

    #[test]
    fn search_finds_by_id() {
        let tmp = setup_root();
        let root = tmp.path().to_path_buf();
        let mgr = RegistryManager::new(&root).unwrap();
        let results = mgr.search("pkg").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.to_string(), "test/pkg");
    }

    #[test]
    fn search_finds_by_tag() {
        let tmp = setup_root();
        let root = tmp.path().to_path_buf();
        let mgr = RegistryManager::new(&root).unwrap();
        let results = mgr.search("test").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_no_results() {
        let tmp = setup_root();
        let root = tmp.path().to_path_buf();
        let mgr = RegistryManager::new(&root).unwrap();
        let results = mgr.search("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn find_package_exact_match() {
        let tmp = setup_root();
        let root = tmp.path().to_path_buf();
        let mgr = RegistryManager::new(&root).unwrap();
        let pkg = mgr.find_package("test/pkg").unwrap();
        assert!(pkg.is_some());
        assert_eq!(pkg.unwrap().id.to_string(), "test/pkg");
    }

    #[test]
    fn find_package_not_found() {
        let tmp = setup_root();
        let root = tmp.path().to_path_buf();
        let mgr = RegistryManager::new(&root).unwrap();
        let pkg = mgr.find_package("test/nonexistent").unwrap();
        assert!(pkg.is_none());
    }

    #[test]
    fn replace_index_preserves_last_known_good() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let reg_dir = root.join("registry/custom");
        std::fs::create_dir_all(&reg_dir).unwrap();

        // Add key to trust store
        let signing_key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        let verifying_key = signing_key.verifying_key();
        let public_key_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            verifying_key.to_bytes(),
        );
        let mut trust = crate::core::trust::TrustStore {
            keys: std::collections::HashMap::new(),
        };
        trust.add_key("custom", &public_key_b64).unwrap();
        trust.save(&root).unwrap();

        let index = RegistryIndex {
            schema_version: 1,
            updated_at: "2026-06-27T00:00:00Z".to_string(),
            registry_revision: Some("first".to_string()),
            trust: None,
            packages: vec![],
        };
        let content = serde_json::to_string_pretty(&index).unwrap();
        let canonical_bytes = crate::core::official_registry::canonical_json_bytes(
            &serde_json::from_str(&content).unwrap(),
        )
        .unwrap();
        let signature = signing_key.sign(&canonical_bytes);
        let sig_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature.to_bytes(),
        );
        let envelope = crate::core::official_registry::RegistrySignature::new("custom", &sig_b64);

        let mgr = RegistryManager::new(&root).unwrap();
        mgr.replace_index("custom", &content, &envelope).unwrap();

        let first_index = std::fs::read_to_string(reg_dir.join("index.json")).unwrap();
        assert!(first_index.contains("first"));
        assert!(!reg_dir.join("index.json.last-known-good").exists());

        // Second replace
        let index2 = RegistryIndex {
            schema_version: 1,
            updated_at: "2026-06-27T01:00:00Z".to_string(),
            registry_revision: Some("second".to_string()),
            trust: None,
            packages: vec![],
        };
        let content2 = serde_json::to_string_pretty(&index2).unwrap();
        let canonical_bytes2 = crate::core::official_registry::canonical_json_bytes(
            &serde_json::from_str(&content2).unwrap(),
        )
        .unwrap();
        let signature2 = signing_key.sign(&canonical_bytes2);
        let sig_b64_2 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature2.to_bytes(),
        );
        let envelope2 =
            crate::core::official_registry::RegistrySignature::new("custom", &sig_b64_2);
        mgr.replace_index("custom", &content2, &envelope2).unwrap();

        let lkg = std::fs::read_to_string(reg_dir.join("index.json.last-known-good")).unwrap();
        assert!(lkg.contains("first"));
        let current = std::fs::read_to_string(reg_dir.join("index.json")).unwrap();
        assert!(current.contains("second"));
    }

    #[test]
    fn replace_index_persists_derived_keys_and_uses_them_for_rotation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let reg_dir = root.join("registry/custom");
        std::fs::create_dir_all(&reg_dir).unwrap();

        // Initial key
        let initial_key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        let initial_verifying_key = initial_key.verifying_key();
        let initial_public_key_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            initial_verifying_key.to_bytes(),
        );

        // Successor key
        let successor_key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        let successor_verifying_key = successor_key.verifying_key();
        let successor_public_key_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            successor_verifying_key.to_bytes(),
        );

        let mut trust = crate::core::trust::TrustStore {
            keys: std::collections::HashMap::new(),
        };
        trust.add_key("custom", &initial_public_key_b64).unwrap();
        trust.save(&root).unwrap();

        // First index introduces the successor key.
        let index = RegistryIndex {
            schema_version: 1,
            updated_at: "2026-06-27T00:00:00Z".to_string(),
            registry_revision: Some("initial".to_string()),
            trust: Some(crate::core::official_registry::RegistryTrustExtension {
                keys: vec![crate::core::official_registry::TrustedKey {
                    key_id: "successor".to_string(),
                    public_key_b64: successor_public_key_b64,
                }],
            }),
            packages: vec![],
        };
        let content = serde_json::to_string_pretty(&index).unwrap();
        let canonical_bytes = crate::core::official_registry::canonical_json_bytes(
            &serde_json::from_str(&content).unwrap(),
        )
        .unwrap();
        let signature = initial_key.sign(&canonical_bytes);
        let sig_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature.to_bytes(),
        );
        let envelope = crate::core::official_registry::RegistrySignature::new("custom", &sig_b64);

        let mgr = RegistryManager::new(&root).unwrap();
        mgr.replace_index("custom", &content, &envelope).unwrap();

        // Verify derived keys were persisted.
        let derived = mgr.load_derived_keys("custom").unwrap();
        assert_eq!(derived.keys.len(), 1);
        assert_eq!(derived.keys[0].key_id, "successor");

        // Second index is signed by the successor key and should verify.
        let index2 = RegistryIndex {
            schema_version: 1,
            updated_at: "2026-06-27T01:00:00Z".to_string(),
            registry_revision: Some("successor".to_string()),
            trust: None,
            packages: vec![],
        };
        let content2 = serde_json::to_string_pretty(&index2).unwrap();
        let canonical_bytes2 = crate::core::official_registry::canonical_json_bytes(
            &serde_json::from_str(&content2).unwrap(),
        )
        .unwrap();
        let signature2 = successor_key.sign(&canonical_bytes2);
        let sig_b64_2 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature2.to_bytes(),
        );
        let envelope2 =
            crate::core::official_registry::RegistrySignature::new("successor", &sig_b64_2);
        mgr.replace_index("custom", &content2, &envelope2).unwrap();

        let current = std::fs::read_to_string(reg_dir.join("index.json")).unwrap();
        assert!(current.contains("successor"));
    }
}
