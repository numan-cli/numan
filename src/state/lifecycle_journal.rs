use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::util::atomic::write_json_atomic;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleOp {
    Update,
    Remove,
    NupmImport,
    NupmImportManifest,
    Rollback,
}

/// Stage of the in-flight lifecycle operation at the time the process last
/// checkpointed. Used for crash detection on the next invocation.
///
/// ## Active-plugin update (`LifecycleOp::Update`)
///
/// When orchestrating an active-plugin update, stages reuse this enum:
/// - [`Prepared`]: journal written; deactivate may be in progress (also see
///   `pending-plugin-deactivate.json`).
/// - [`LockfileUpdated`]: install upgraded the lockfile/payload; reactivate may
///   be in progress (also see `pending-activation.json`). Cleared only after
///   successful reactivate (or plain upgrade with no activation).
/// - When [`PendingLifecycle::needs_reactivate`] is true at this stage, the next
///   `numan update` must resume activation (or refuse) before treating the
///   package as up to date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStage {
    /// Journal written; no mutations have been applied yet.
    Prepared,
    /// Lockfile has been updated; payload directory deletion may be pending.
    LockfileUpdated,
    /// nupm import: staging directory populated but not yet promoted.
    PayloadsStaged,
    /// nupm import: payload promoted to immutable storage; lockfile not yet committed.
    PayloadsPromoted,
    /// nupm import: lockfile and provenance written.
    SelectionCommitted,
    /// Rollback: journal written; no mutations applied yet.
    RollbackPrepared,
    /// Rollback: pre-rollback snapshot of current state created.
    CurrentStateSnapshotted,
    /// Rollback: candidate lockfile/autoload/imports staged.
    CandidateStaged,
    /// Rollback: candidate autoload validated with Nu.
    CandidateValidated,
    /// Rollback: lockfile committed.
    LockfileCommitted,
    /// Rollback: managed autoload file committed.
    AutoloadCommitted,
    /// Rollback: autoload-state projection committed.
    AutoloadStateCommitted,
    /// Rollback: nupm-imports sidecar committed.
    ImportsCommitted,
    /// Rollback: Nu path cache (`nu_state/paths.json`) committed or cleared.
    PathsCommitted,
    /// Rollback: all owned state committed; journal clear pending.
    Completed,
}

/// Crash-recovery journal written atomically before any destructive lifecycle
/// mutation (update, remove, nupm import).
///
/// Written to `<root>/state/pending-lifecycle.json`. Cleared on successful
/// completion. A journal found on the next run indicates an interrupted
/// operation. `numan gc` can clean up any orphaned payload directories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingLifecycle {
    pub op: LifecycleOp,
    pub package_id: String,
    pub stage: LifecycleStage,
    /// Payload directory (relative to root) that is orphaned or being removed.
    #[serde(default)]
    pub orphan_payload_path: Option<String>,
    /// For `update`: the version we upgraded from.
    #[serde(default)]
    pub from_version: Option<String>,
    /// For `update`: the version we upgraded to.
    #[serde(default)]
    pub to_version: Option<String>,
    #[serde(default)]
    pub nupm_source_path: Option<String>,
    #[serde(default)]
    pub nupm_metadata_sha256: Option<String>,
    #[serde(default)]
    pub staging_dir: Option<String>,
    #[serde(default)]
    pub promoted_payload_path: Option<String>,
    /// For manifest import: all package IDs in the batch.
    #[serde(default)]
    pub batch_package_ids: Vec<String>,
    /// For manifest import: staging dirs (relative) for crash recovery.
    #[serde(default)]
    pub batch_staging_dirs: Vec<String>,
    /// For rollback: the snapshot being restored.
    #[serde(default)]
    pub target_snapshot_id: Option<String>,
    /// For rollback: the snapshot of current state taken before rollback.
    #[serde(default)]
    pub pre_rollback_snapshot_id: Option<String>,
    /// Active-plugin update: after [`LifecycleStage::LockfileUpdated`], the
    /// upgraded plugin still needs `activate` before the journal is cleared.
    /// Plain (inactive) updates leave this false so a crash mid-clear does not
    /// spuriously activate packages.
    #[serde(default)]
    pub needs_reactivate: bool,
}

impl PendingLifecycle {
    fn path(root: &Path) -> std::path::PathBuf {
        root.join("state/pending-lifecycle.json")
    }

    pub fn load(root: &Path) -> Result<Option<Self>> {
        let p = Self::path(root);
        if !p.exists() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(&p)
            .with_context(|| format!("Failed to read {}", p.display()))?;
        Ok(Some(serde_json::from_str(&s)?))
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        write_json_atomic(&Self::path(root), self)
    }

    pub fn clear(root: &Path) -> Result<()> {
        let p = Self::path(root);
        if p.exists() {
            std::fs::remove_file(&p)
                .with_context(|| format!("Failed to remove {}", p.display()))?;
        }
        Ok(())
    }
}

/// Check for a stale lifecycle journal and return it if present.
///
/// Callers should warn the user and suggest `numan gc` when `Some` is returned.
pub fn check_stale_journal(root: &Path) -> Result<Option<PendingLifecycle>> {
    PendingLifecycle::load(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_journal(op: LifecycleOp) -> PendingLifecycle {
        PendingLifecycle {
            op,
            package_id: "owner/pkg".to_string(),
            stage: LifecycleStage::Prepared,
            orphan_payload_path: None,
            from_version: None,
            to_version: None,
            nupm_source_path: None,
            nupm_metadata_sha256: None,
            staging_dir: None,
            promoted_payload_path: None,
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            target_snapshot_id: None,
            pre_rollback_snapshot_id: None,
            needs_reactivate: false,
        }
    }

    #[test]
    fn roundtrip_remove_journal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let j = PendingLifecycle {
            orphan_payload_path: Some("packages/modules/owner/pkg/1.0.0-abc".to_string()),
            ..base_journal(LifecycleOp::Remove)
        };
        j.save(root).unwrap();

        let loaded = PendingLifecycle::load(root).unwrap().unwrap();
        assert_eq!(loaded.package_id, "owner/pkg");
        assert!(loaded.from_version.is_none());

        PendingLifecycle::clear(root).unwrap();
        assert!(PendingLifecycle::load(root).unwrap().is_none());
    }

    #[test]
    fn roundtrip_update_journal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let j = PendingLifecycle {
            orphan_payload_path: Some("packages/modules/owner/pkg/1.0.0-abc".to_string()),
            from_version: Some("1.0.0".to_string()),
            to_version: Some("2.0.0".to_string()),
            ..base_journal(LifecycleOp::Update)
        };
        j.save(root).unwrap();

        let loaded = PendingLifecycle::load(root).unwrap().unwrap();
        assert_eq!(loaded.from_version.as_deref(), Some("1.0.0"));
        assert_eq!(loaded.to_version.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn roundtrip_nupm_import_journal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let j = PendingLifecycle {
            stage: LifecycleStage::PayloadsStaged,
            nupm_source_path: Some("/tmp/pkg".to_string()),
            nupm_metadata_sha256: Some("abc".to_string()),
            staging_dir: Some("packages/modules/o/n/.staging".to_string()),
            batch_package_ids: Vec::new(),
            batch_staging_dirs: Vec::new(),
            ..base_journal(LifecycleOp::NupmImport)
        };
        j.save(root).unwrap();

        let loaded = PendingLifecycle::load(root).unwrap().unwrap();
        assert!(matches!(loaded.op, LifecycleOp::NupmImport));
        assert!(matches!(loaded.stage, LifecycleStage::PayloadsStaged));
        assert_eq!(
            loaded.staging_dir.as_deref(),
            Some("packages/modules/o/n/.staging")
        );
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(PendingLifecycle::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn check_stale_journal_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_stale_journal(dir.path()).unwrap().is_none());
    }
}
