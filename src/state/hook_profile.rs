//! Hook profile state and active bindings (issue #16).
//!
//! Tracks Numan-owned hook bindings and their scope. Ephemeral merge state is
//! regenerated, never partially merged.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::util::atomic::write_json_atomic;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookProfile {
    #[serde(default)]
    pub bindings: Vec<HookBindingRecord>,
    #[serde(default)]
    pub nu_executable: Option<String>,
    #[serde(default)]
    pub nu_executable_sha256: Option<String>,
    #[serde(default)]
    pub nu_version: Option<String>,
    #[serde(default)]
    pub vendor_autoload_dir: Option<String>,
    #[serde(default)]
    pub managed_file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "HookBindingRecordWire")]
pub struct HookBindingRecord {
    pub id: String,
    pub event: String,
    pub handler: String,
    #[serde(default)]
    pub filter: Option<String>,
    pub package_id: String,
    pub activated_at: String,
}

#[derive(Deserialize)]
struct HookBindingRecordWire {
    id: String,
    event: String,
    handler: String,
    #[serde(default)]
    filter: Option<String>,
    package_id: String,
    activated_at: String,
}

impl TryFrom<HookBindingRecordWire> for HookBindingRecord {
    type Error = String;

    fn try_from(value: HookBindingRecordWire) -> Result<Self, Self::Error> {
        if !crate::nu::hooks::is_allowed_event(&value.event) {
            return Err(format!("unsupported Nushell hook event '{}'", value.event));
        }
        Ok(Self {
            id: value.id,
            event: value.event,
            handler: value.handler,
            filter: value.filter,
            package_id: value.package_id,
            activated_at: value.activated_at,
        })
    }
}

impl HookProfile {
    pub fn profile_path(root: &Path) -> std::path::PathBuf {
        root.join("nu_state").join("hook_profile.json")
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::profile_path(root);
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = Self::profile_path(root);
        write_json_atomic(&path, self)
    }

    pub fn delete(root: &Path) -> Result<()> {
        let path = Self::profile_path(root);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove '{}'", path.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        let mut p = HookProfile::default();
        p.bindings.push(HookBindingRecord {
            id: "pkg/pre-prompt-1".to_string(),
            event: "pre_prompt".to_string(),
            handler: "hooks/pre_prompt.nu".to_string(),
            filter: None,
            package_id: "owner/name".to_string(),
            activated_at: "2026-01-01T00:00:00Z".to_string(),
        });
        p.save(root).unwrap();
        let loaded = HookProfile::load(root).unwrap();
        assert_eq!(loaded.bindings.len(), 1);
        assert_eq!(loaded.bindings[0].id, "pkg/pre-prompt-1");
    }

    #[test]
    fn load_missing_returns_default() {
        let dir = tempdir().unwrap();
        let p = HookProfile::load(dir.path()).unwrap();
        assert!(p.bindings.is_empty());
    }

    #[test]
    fn load_invalid_profile_returns_error() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nu_state")).unwrap();
        std::fs::write(dir.path().join("nu_state/hook_profile.json"), "{").unwrap();
        let error = HookProfile::load(dir.path()).unwrap_err();
        assert!(error.to_string().contains("Failed to parse"));
    }

    #[test]
    fn allowed_events_non_empty() {
        assert!(!crate::nu::hooks::ALLOWED_HOOK_EVENTS.is_empty());
        assert!(crate::nu::hooks::ALLOWED_HOOK_EVENTS.contains(&"pre_prompt"));
    }

    #[test]
    fn deserialization_rejects_unsupported_hook_events() {
        let json = r#"{"bindings":[{"id":"x","event":"display_output","handler":"h.nu","package_id":"owner/pkg","activated_at":"now"}]}"#;
        let error = serde_json::from_str::<HookProfile>(json).unwrap_err();
        assert!(error.to_string().contains("unsupported Nushell hook event"));
    }
}
