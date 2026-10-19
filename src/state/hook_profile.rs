//! Hook profile state (issue #16, Phase 5 — data model only).
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookBindingRecord {
    pub id: String,
    pub event: String,
    pub handler: String,
    #[serde(default)]
    pub filter: Option<String>,
    pub package_id: String,
    pub activated_at: String,
}

impl HookProfile {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("nu_state").join("hook_profile.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        Ok(serde_json::from_str(&content).unwrap_or_default())
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = root.join("nu_state").join("hook_profile.json");
        write_json_atomic(&path, self)
    }

    pub fn delete(root: &Path) -> Result<()> {
        let path = root.join("nu_state").join("hook_profile.json");
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
    fn allowed_events_non_empty() {
        assert!(!crate::nu::hooks::ALLOWED_HOOK_EVENTS.is_empty());
        assert!(crate::nu::hooks::ALLOWED_HOOK_EVENTS.contains(&"pre_prompt"));
    }
}
