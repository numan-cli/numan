//! Overlay activation state (issue #17, Phase 5 — data model only).
//!
//! Tracks which overlays are currently active and their scope. Ephemeral:
//! session-scoped overlays are recorded for status/drift but cannot be
//! auto-restored in a new shell.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::util::atomic::write_json_atomic;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OverlayState {
    /// Active overlays.
    #[serde(default)]
    pub overlays: Vec<OverlayRecord>,
}

/// Scope of an overlay activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayScope {
    Session,
    Project,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayRecord {
    pub package_id: String,
    pub overlay_name: String,
    pub entry_path: String,
    /// Scope of the activation; `"session"` or `"project"`.
    pub scope: OverlayScope,
    /// Project root when scope is `Project`; `None` for session scope.
    #[serde(default)]
    pub project_root: Option<String>,
    pub activated_at: String,
}

impl OverlayState {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("nu_state").join("overlays.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = root.join("nu_state").join("overlays.json");
        write_json_atomic(&path, self)
    }

    pub fn delete(root: &Path) -> Result<()> {
        let path = root.join("nu_state").join("overlays.json");
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
        let mut state = OverlayState::default();
        state.overlays.push(OverlayRecord {
            package_id: "owner/name".to_string(),
            overlay_name: "name".to_string(),
            entry_path: "/tmp/entry.nu".to_string(),
            scope: OverlayScope::Session,
            project_root: None,
            activated_at: "2026-01-01T00:00:00Z".to_string(),
        });
        state.save(root).unwrap();
        let loaded = OverlayState::load(root).unwrap();
        assert_eq!(loaded.overlays.len(), 1);
        assert_eq!(loaded.overlays[0].package_id, "owner/name");
    }

    #[test]
    fn load_missing_returns_default() {
        let dir = tempdir().unwrap();
        let state = OverlayState::load(dir.path()).unwrap();
        assert!(state.overlays.is_empty());
    }

    #[test]
    fn scope_serde_lowercase() {
        assert_eq!(
            serde_json::to_string(&OverlayScope::Session).unwrap(),
            "\"session\""
        );
        assert_eq!(
            serde_json::to_string(&OverlayScope::Project).unwrap(),
            "\"project\""
        );
        assert!(serde_json::from_str::<OverlayScope>("\"Session\"").is_err());
        assert!(serde_json::from_str::<OverlayScope>("\"other\"").is_err());
    }
}
