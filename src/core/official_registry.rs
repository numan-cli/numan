use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::core::integrity::compute_sha256;
use crate::core::package::RegistryIndex;

/// Built-in trust root for the official Numan registry.
///
/// The official key, URL, and key ID are compiled into the binary. User config
/// may disable the official registry or add custom registries, but the official
/// root itself is immutable and authoritative.
pub struct OfficialRegistry {
    pub name: &'static str,
    pub production_url: &'static str,
    pub key_id: &'static str,
    pub public_key_b64: &'static str,
}

/// Placeholder values for the official registry.
///
/// These are intentionally not a real production trust root. The real URL and
/// public key are committed only after the registry is independently live and
/// the maintainer has manually provisioned the production signing key.
/// Released builds that enable the default registry must override these with
/// the production values.
pub const OFFICIAL_REGISTRY: OfficialRegistry = OfficialRegistry {
    name: "official",
    production_url: "https://numan-cli.github.io/numan-registry/index.json",
    key_id: "official-2026-07-01",
    public_key_b64: "1F0STZT/Fk4OiP/7Hqs3/MurixBKoe7GYVoCto2/mCc=",
};

impl OfficialRegistry {
    /// Returns true when the built-in key has not been replaced with a real
    /// production key yet.
    pub fn is_placeholder_key(&self) -> bool {
        self.key_id == "official-placeholder" || self.public_key_b64 == "PLACEHOLDER"
    }
}

/// Build the built-in trust root for the official registry.
///
/// Returns the placeholder root if the production key has not been committed
/// yet. Verification against a placeholder root will fail with a clear error.
pub fn official_built_in_root() -> RegistryTrustRoot {
    let mut root = RegistryTrustRoot::new(OFFICIAL_REGISTRY.name);
    // The placeholder key is intentionally not added as a valid key, so any
    // attempt to verify a signature fails early. Once the production key is
    // committed, this function is updated to add the real key.
    if !OFFICIAL_REGISTRY.is_placeholder_key() {
        let _ = root.add_key(OFFICIAL_REGISTRY.key_id, OFFICIAL_REGISTRY.public_key_b64);
    }
    root
}

/// A trusted Ed25519 key for a registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedKey {
    pub key_id: String,
    pub public_key_b64: String,
}

/// A set of trusted keys for a single registry.
///
/// The official registry is bootstrapped from `OfficialRegistry` compiled into
/// the binary. Custom registries are bootstrapped from the user-managed trust
/// store. In both cases, successor keys can be introduced only when an index
/// signed by an already-trusted key explicitly carries their key records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryTrustRoot {
    pub registry_name: String,
    pub keys: HashMap<String, TrustedKey>,
}

impl RegistryTrustRoot {
    pub fn new(registry_name: &str) -> Self {
        Self {
            registry_name: registry_name.to_string(),
            keys: HashMap::new(),
        }
    }

    pub fn add_key(&mut self, key_id: &str, public_key_b64: &str) -> Result<()> {
        let bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, public_key_b64)
                .with_context(|| format!("Invalid base64 public key for key_id '{key_id}'"))?;
        if bytes.len() != 32 {
            bail!(
                "Ed25519 public key for key_id '{key_id}' must be 32 bytes, got {}",
                bytes.len()
            );
        }
        self.keys.insert(
            key_id.to_string(),
            TrustedKey {
                key_id: key_id.to_string(),
                public_key_b64: public_key_b64.to_string(),
            },
        );
        Ok(())
    }

    pub fn verifying_key(&self, key_id: &str) -> Result<VerifyingKey> {
        let key = self.keys.get(key_id).context(format!(
            "No trusted key with key_id '{key_id}' for registry '{}'",
            self.registry_name
        ))?;
        let bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &key.public_key_b64,
        )
        .with_context(|| format!("Invalid base64 for key_id '{key_id}'"))?;
        let mut array = [0u8; 32];
        array.copy_from_slice(&bytes);
        Ok(VerifyingKey::from_bytes(&array)?)
    }

    pub fn verify_signature(&self, key_id: &str, data: &[u8], signature_b64: &str) -> Result<bool> {
        let verifying_key = self.verifying_key(key_id)?;
        let sig_bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, signature_b64)
                .context("Invalid base64 signature")?;
        if sig_bytes.len() != 64 {
            bail!(
                "Ed25519 signature must be 64 bytes, got {}",
                sig_bytes.len()
            );
        }
        let mut sig_array = [0u8; 64];
        sig_array.copy_from_slice(&sig_bytes);
        let signature = Signature::from_bytes(&sig_array);
        Ok(verifying_key.verify(data, &signature).is_ok())
    }
}

/// A detached signature envelope for a registry index.
///
/// The signature is computed over the canonical JSON bytes of the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySignature {
    pub key_id: String,
    pub algorithm: String,
    pub signature: String,
}

impl RegistrySignature {
    pub fn new(key_id: &str, signature_b64: &str) -> Self {
        Self {
            key_id: key_id.to_string(),
            algorithm: "ed25519".to_string(),
            signature: signature_b64.to_string(),
        }
    }
}

/// Canonical JSON serialization used for signing and digesting registry
/// indexes.
///
/// Rules:
/// - Object keys are sorted lexicographically.
/// - No whitespace between tokens.
/// - Array order is preserved.
/// - Numbers are emitted as serde_json emits them.
///
/// The registry must publish the index in a form that serializes to these
/// exact bytes; clients re-canonicalize the on-disk index before verification.
pub fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>> {
    let canonical = canonicalize(value)?;
    Ok(canonical.to_string().into_bytes())
}

fn canonicalize(value: &Value) -> Result<String> {
    match value {
        Value::Object(map) => {
            let mut pairs = Vec::with_capacity(map.len());
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                let val = canonicalize(map.get(key).unwrap())?;
                let key_json = serde_json::to_string(key)?;
                pairs.push(format!("{key_json}:{val}"));
            }
            Ok(format!("{{{}}}", pairs.join(",")))
        }
        Value::Array(arr) => {
            let mut parts = Vec::with_capacity(arr.len());
            for v in arr {
                parts.push(canonicalize(v)?);
            }
            Ok(format!("[{}]", parts.join(",")))
        }
        other => Ok(other.to_string()),
    }
}

/// Successor keys declared inside a signed index.
///
/// Because the index is signed by a trusted key, these keys become trusted for
/// future indexes. A key cannot become trusted by merely signing an index; it
/// must be introduced by a signed successor declaration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct RegistryTrustExtension {
    #[serde(default)]
    pub keys: Vec<TrustedKey>,
}

/// The result of verifying a signed registry index.
#[derive(Debug, Clone)]
pub struct VerifiedIndex {
    pub index: RegistryIndex,
    pub registry_name: String,
    pub key_id: String,
    pub index_sha256: String,
    pub trust_extension: RegistryTrustExtension,
}

/// Verify a registry index against a trust root.
///
/// This validates:
/// - The signature envelope is well-formed and names a known algorithm.
/// - The `key_id` is present in the trust root.
/// - The signature is valid over the canonical JSON bytes of the index.
/// - The index declares a recognized `schema_version`.
///
/// Returns the parsed index, the key used, any successor keys declared in the
/// signed index, and `index_sha256` — the SHA-256 of the canonical bytes,
/// computed (not verified against a declared value) for callers to record.
pub fn verify_registry_index(
    registry_name: &str,
    trust_root: &RegistryTrustRoot,
    index_content: &str,
    signature: &RegistrySignature,
) -> Result<VerifiedIndex> {
    if signature.algorithm != "ed25519" {
        bail!(
            "Unsupported signature algorithm '{}' for registry '{}'",
            signature.algorithm,
            registry_name
        );
    }

    let value: Value = serde_json::from_str(index_content)
        .with_context(|| format!("Registry '{registry_name}' index is not valid JSON"))?;

    if value.get("schema_version").is_none() {
        bail!("Registry '{registry_name}' index is missing required 'schema_version' field");
    }

    let schema_version = value["schema_version"]
        .as_u64()
        .with_context(|| format!("Registry '{registry_name}' has non-integer schema_version"))?;

    if schema_version != 1 {
        bail!("Registry '{registry_name}' uses unsupported schema_version {schema_version}");
    }

    let canonical_bytes = canonical_json_bytes(&value)?;
    let index_sha256 = compute_sha256(&canonical_bytes);

    let valid = trust_root
        .verify_signature(&signature.key_id, &canonical_bytes, &signature.signature)
        .with_context(|| {
            format!(
                "Failed to verify signature for registry '{registry_name}' with key_id '{}'",
                signature.key_id
            )
        })?;

    if !valid {
        bail!(
            "Registry '{registry_name}' signature verification failed with key_id '{}'. \
             The index may have been tampered with.",
            signature.key_id
        );
    }

    let index: RegistryIndex = serde_json::from_value(value)
        .with_context(|| format!("Registry '{registry_name}' index failed schema validation"))?;

    let trust_extension: RegistryTrustExtension = index.trust.clone().unwrap_or_default();

    // Validate that any successor keys are well-formed, but do not add them to
    // the trust root here; the caller decides whether to persist them.
    for key in &trust_extension.keys {
        let mut temp = RegistryTrustRoot::new(registry_name);
        temp.add_key(&key.key_id, &key.public_key_b64)
            .with_context(|| format!("Invalid successor key '{}' in index", key.key_id))?;
    }

    Ok(VerifiedIndex {
        index,
        registry_name: registry_name.to_string(),
        key_id: signature.key_id.clone(),
        index_sha256,
        trust_extension,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand_core::OsRng;

    fn test_keypair() -> (SigningKey, String) {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let public_key_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            verifying_key.to_bytes(),
        );
        (signing_key, public_key_b64)
    }

    fn sign_index(signing_key: &SigningKey, value: &Value) -> RegistrySignature {
        let canonical_bytes = canonical_json_bytes(value).unwrap();
        let signature = signing_key.sign(&canonical_bytes);
        let signature_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature.to_bytes(),
        );
        RegistrySignature::new("test-key", &signature_b64)
    }

    fn minimal_index_value() -> Value {
        serde_json::json!({
            "schema_version": 1,
            "updated_at": "2026-07-01T00:00:00Z",
            "packages": []
        })
    }

    #[test]
    fn canonical_json_sorts_object_keys() {
        let value = serde_json::json!({"b": 1, "a": 2, "c": [3, 1, 2]});
        let canonical = canonical_json_bytes(&value).unwrap();
        assert_eq!(canonical, b"{\"a\":2,\"b\":1,\"c\":[3,1,2]}");
    }

    #[test]
    fn canonical_json_is_deterministic_for_nested_objects() {
        let value = serde_json::json!({"z": {"b": 1, "a": 2}, "a": 3});
        let canonical1 = canonical_json_bytes(&value).unwrap();
        let canonical2 = canonical_json_bytes(&value).unwrap();
        assert_eq!(canonical1, canonical2);
    }

    #[test]
    fn verify_valid_index() {
        let (signing_key, public_key_b64) = test_keypair();
        let value = minimal_index_value();
        let signature = sign_index(&signing_key, &value);

        let mut trust_root = RegistryTrustRoot::new("test");
        trust_root.add_key("test-key", &public_key_b64).unwrap();

        let index_content = value.to_string();
        let verified =
            verify_registry_index("test", &trust_root, &index_content, &signature).unwrap();
        assert_eq!(verified.index.schema_version, 1);
        assert_eq!(verified.key_id, "test-key");
        assert_eq!(verified.registry_name, "test");
        assert!(!verified.index_sha256.is_empty());
    }

    #[test]
    fn verify_rejects_missing_schema_version() {
        let (signing_key, public_key_b64) = test_keypair();
        let value = serde_json::json!({"updated_at": "2026-07-01T00:00:00Z", "packages": []});
        let signature = sign_index(&signing_key, &value);

        let mut trust_root = RegistryTrustRoot::new("test");
        trust_root.add_key("test-key", &public_key_b64).unwrap();

        let index_content = value.to_string();
        let result = verify_registry_index("test", &trust_root, &index_content, &signature);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("schema_version"));
    }

    #[test]
    fn verify_rejects_tampered_index() {
        let (signing_key, public_key_b64) = test_keypair();
        let value = minimal_index_value();
        let signature = sign_index(&signing_key, &value);

        let mut trust_root = RegistryTrustRoot::new("test");
        trust_root.add_key("test-key", &public_key_b64).unwrap();

        let mut tampered = value.clone();
        tampered["updated_at"] = Value::String("tampered".to_string());
        let index_content = tampered.to_string();
        let result = verify_registry_index("test", &trust_root, &index_content, &signature);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("tampered"));
    }

    #[test]
    fn verify_rejects_unknown_key_id() {
        let (signing_key, public_key_b64) = test_keypair();
        let value = minimal_index_value();
        let signature = sign_index(&signing_key, &value);

        let mut trust_root = RegistryTrustRoot::new("test");
        trust_root.add_key("other-key", &public_key_b64).unwrap();

        let index_content = value.to_string();
        let result = verify_registry_index("test", &trust_root, &index_content, &signature);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("test-key"));
    }

    #[test]
    fn trust_extension_validates_successor_keys() {
        let (signing_key, public_key_b64) = test_keypair();
        let (_successor_key, successor_public_key_b64) = test_keypair();
        let value = serde_json::json!({
            "schema_version": 1,
            "updated_at": "2026-07-01T00:00:00Z",
            "trust": {
                "keys": [
                    {"key_id": "successor-key", "public_key_b64": successor_public_key_b64}
                ]
            },
            "packages": []
        });
        let signature = sign_index(&signing_key, &value);

        let mut trust_root = RegistryTrustRoot::new("test");
        trust_root.add_key("test-key", &public_key_b64).unwrap();

        let index_content = value.to_string();
        let verified =
            verify_registry_index("test", &trust_root, &index_content, &signature).unwrap();
        assert_eq!(verified.trust_extension.keys.len(), 1);
        assert_eq!(verified.trust_extension.keys[0].key_id, "successor-key");
    }

    #[test]
    fn trust_extension_rejects_invalid_successor_keys() {
        let (signing_key, public_key_b64) = test_keypair();
        let value = serde_json::json!({
            "schema_version": 1,
            "updated_at": "2026-07-01T00:00:00Z",
            "trust": {
                "keys": [
                    {"key_id": "bad-key", "public_key_b64": "not-valid-base64"}
                ]
            },
            "packages": []
        });
        let signature = sign_index(&signing_key, &value);

        let mut trust_root = RegistryTrustRoot::new("test");
        trust_root.add_key("test-key", &public_key_b64).unwrap();

        let index_content = value.to_string();
        let result = verify_registry_index("test", &trust_root, &index_content, &signature);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("bad-key"));
    }

    /// Guards the post-cutover state: released builds must ship a real production key.
    #[test]
    fn official_registry_is_not_placeholder_in_production() {
        assert!(!OFFICIAL_REGISTRY.is_placeholder_key());
    }

    /// Guards against a half-applied trust-root edit: key_id and
    /// public_key_b64 must move off their placeholder values together, and
    /// once real, the public key must be well-formed. This is the invariant
    /// scripts/update-official-trust-root.sh enforces at edit time; this
    /// test enforces it at build time regardless of how the field got set.
    #[test]
    fn official_registry_key_id_and_public_key_are_consistent() {
        let key_id_is_placeholder = OFFICIAL_REGISTRY.key_id == "official-placeholder";
        let public_key_is_placeholder = OFFICIAL_REGISTRY.public_key_b64 == "PLACEHOLDER";
        assert_eq!(
            key_id_is_placeholder, public_key_is_placeholder,
            "key_id and public_key_b64 must both be placeholders or both be real values"
        );

        if !public_key_is_placeholder {
            let bytes = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                OFFICIAL_REGISTRY.public_key_b64,
            )
            .expect("OFFICIAL_REGISTRY.public_key_b64 must be valid base64");
            assert_eq!(
                bytes.len(),
                32,
                "OFFICIAL_REGISTRY.public_key_b64 must decode to 32 bytes"
            );
        }
    }
}
