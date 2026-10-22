//! Caller-shell overlay transactions and payload reservations.
//!
//! Tracks pending, active, and degraded overlays and their scope. Ephemeral:
//! session-scoped overlays are recorded for status/drift but cannot be
//! auto-restored in a new shell.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::util::atomic::write_json_atomic;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OverlayState {
    /// Overlay transactions, including unconfirmed payload reservations.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OverlayStatus {
    #[default]
    Pending,
    Active,
    /// Execution failed or ownership no longer agrees with the shell. Keep
    /// the payload pinned until the shell exits and reconciliation prunes it.
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub process_started: Option<u64>,
    #[serde(default)]
    pub txn: Option<String>,
    #[serde(default)]
    pub status: OverlayStatus,
    #[serde(default)]
    pub payload_path: Option<String>,
}

impl OverlayState {
    /// Called under the root mutation lock. All transaction states pin payloads.
    pub fn ensure_package_mutable(root: &Path, package_id: &str) -> Result<()> {
        if Self::load(root)?
            .overlays
            .iter()
            .any(|r| r.package_id == package_id)
        {
            bail!("Package '{package_id}' is pinned by an overlay session. Hide it in that shell or close the shell and run `numan overlay reconcile` first.");
        }
        Ok(())
    }

    pub fn referenced_payloads(root: &Path) -> Result<Vec<PathBuf>> {
        let canonical_root = root.canonicalize()?;
        let mut paths = Vec::new();
        for record in Self::load(root)?.overlays {
            let entry = PathBuf::from(&record.entry_path);
            let entry = entry
                .canonicalize()
                .context("Cannot resolve reserved overlay entry; refusing garbage collection")?;
            let path = if let Some(payload) = record.payload_path {
                PathBuf::from(payload)
            } else {
                let relative = entry
                    .strip_prefix(&canonical_root)
                    .context("Overlay entry is outside the Numan root")?;
                canonical_root.join(relative.components().take(5).collect::<PathBuf>())
            };
            if path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                bail!("Overlay payload contains parent traversal");
            }
            let path = path
                .canonicalize()
                .context("Cannot resolve reserved overlay payload; refusing garbage collection")?;
            let packages = canonical_root.join("packages");
            if !path.starts_with(&packages)
                || path.strip_prefix(&packages)?.components().count() != 4
                || !entry.starts_with(&path)
            {
                bail!("Overlay payload is outside Numan's package storage");
            }
            paths.push(path);
        }
        Ok(paths)
    }

    fn validate(&self) -> Result<()> {
        for overlay in &self.overlays {
            if overlay.scope == OverlayScope::Project && overlay.project_root.is_none() {
                bail!(
                    "Project-scoped overlay '{}' requires project_root",
                    overlay.overlay_name
                );
            }
        }
        Ok(())
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("nu_state").join("overlays.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let state: Self = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        state
            .validate()
            .with_context(|| format!("Invalid overlay state in {}", path.display()))?;
        Ok(state)
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        self.validate()?;
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
    fn unresolved_reserved_entry_aborts_payload_enumeration() {
        let dir = tempdir().unwrap();
        write_json_atomic(
            &dir.path().join("nu_state/overlays.json"),
            &serde_json::json!({"overlays": [{
                "package_id": "owner/tool", "overlay_name": "tool",
                "entry_path": dir.path().join("packages/modules/owner/tool/1/mod.nu"),
                "scope": "session", "activated_at": "now"
            }]}),
        )
        .unwrap();
        assert!(OverlayState::referenced_payloads(dir.path())
            .unwrap_err()
            .to_string()
            .contains("Cannot resolve reserved overlay entry"));
    }

    #[test]
    fn roundtrip() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        let mut state = OverlayState::default();
        for (scope, project_root) in [
            (OverlayScope::Session, None),
            (OverlayScope::Project, Some("/tmp/project")),
        ] {
            state.overlays.push(OverlayRecord {
                package_id: "owner/name".to_string(),
                overlay_name: "name".to_string(),
                entry_path: "/tmp/entry.nu".to_string(),
                scope,
                project_root: project_root.map(String::from),
                activated_at: "2026-01-01T00:00:00Z".to_string(),
                session_id: None,
                pid: None,
                process_started: None,
                txn: None,
                status: OverlayStatus::Pending,
                payload_path: None,
            });
        }
        state.save(root).unwrap();
        let loaded = OverlayState::load(root).unwrap();
        assert_eq!(loaded.overlays.len(), state.overlays.len());
        for (actual, expected) in loaded.overlays.iter().zip(&state.overlays) {
            assert_eq!(actual.package_id, expected.package_id);
            assert_eq!(actual.scope, expected.scope);
            assert_eq!(actual.project_root, expected.project_root);
        }
    }

    #[test]
    fn load_rejects_project_without_root() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nu_state/overlays.json");
        for project_root in [None, Some(serde_json::Value::Null)] {
            let mut record = serde_json::json!({
                "package_id": "owner/name",
                "overlay_name": "name",
                "entry_path": "/tmp/entry.nu",
                "scope": "project",
                "activated_at": "2026-01-01T00:00:00Z"
            });
            if let Some(project_root) = project_root {
                record["project_root"] = project_root;
            }
            write_json_atomic(&path, &serde_json::json!({ "overlays": [record] })).unwrap();

            let err = OverlayState::load(dir.path()).unwrap_err();
            assert!(format!("{err:#}").contains("requires project_root"));
        }
    }

    #[test]
    fn save_rejects_project_without_root_and_preserves_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nu_state/overlays.json");
        let mut state = OverlayState::default();
        state.save(dir.path()).unwrap();
        let original = std::fs::read(&path).unwrap();
        state.overlays.push(OverlayRecord {
            package_id: "owner/name".to_string(),
            overlay_name: "name".to_string(),
            entry_path: "/tmp/entry.nu".to_string(),
            scope: OverlayScope::Project,
            project_root: None,
            activated_at: "2026-01-01T00:00:00Z".to_string(),
            session_id: None,
            pid: None,
            process_started: None,
            txn: None,
            status: OverlayStatus::Pending,
            payload_path: None,
        });

        let err = state.save(dir.path()).unwrap_err();
        assert!(err.to_string().contains("requires project_root"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
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
