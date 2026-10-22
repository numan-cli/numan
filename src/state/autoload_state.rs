use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::state::lockfile::Lockfile;
use crate::util::atomic::write_json_atomic;

/// Schema version for `autoload-state.json`.
pub const SCHEMA_VERSION: u32 = 1;

/// Derived state projection written to `$NUMAN_ROOT/nu_state/autoload-state.json`.
///
/// This file is *not* authoritative. The lockfile module activation records are
/// the ground truth. `AutoloadState` is a fast-check projection derived from
/// the lockfile after a successful managed-file replacement. It enables drift
/// detection without re-reading the entire lockfile.
///
/// Rules:
/// - Written only after the managed file is successfully replaced or removed.
/// - `active_module_ids` is a deterministic, sorted projection from the lockfile.
/// - A disagreement between this file and the lockfile blocks mutation until
///   recovery completes.
/// - When no modules remain active, this file is deleted rather than zeroed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoloadState {
    pub schema_version: u32,
    /// Absolute path to the selected vendor-autoload directory.
    pub vendor_autoload_dir: String,
    /// Absolute path to the Numan-managed autoload file (`numan.nu`).
    pub managed_file_path: String,
    /// SHA-256 of the Nu executable at the time this state was written.
    pub nu_executable_sha256: String,
    /// Version string of the Nu executable at the time this state was written.
    pub nu_version: String,
    /// SHA-256 of the generated managed file.
    pub generated_file_sha256: String,
    /// Sorted list of currently active module scoped IDs, e.g. `["owner/bar",
    /// "owner/foo"]`. Deterministic; do not rely on insertion order.
    pub active_module_ids: Vec<String>,
    /// Timestamp when this state was written.
    pub generated_at: String,
}

impl AutoloadState {
    fn state_path(root: &Path) -> PathBuf {
        root.join("nu_state/autoload-state.json")
    }

    /// Load the autoload state from disk, returning `None` if the file is absent.
    pub fn load(root: &Path) -> Result<Option<Self>> {
        let path = Self::state_path(root);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read autoload-state at '{}'", path.display()))?;
        let state: Self =
            serde_json::from_str(&content).context("Failed to parse autoload-state.json")?;
        Ok(Some(state))
    }

    /// Atomically write the autoload state to disk.
    pub fn save(&self, root: &Path) -> Result<()> {
        write_json_atomic(&Self::state_path(root), self)
    }

    /// Delete the autoload state file.
    ///
    /// Called when the final module is deactivated and no active modules remain.
    /// Idempotent: does not error if the file is already absent.
    pub fn delete(root: &Path) -> Result<()> {
        let path = Self::state_path(root);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| {
                format!("Failed to remove autoload-state at '{}'", path.display())
            })?;
        }
        Ok(())
    }

    /// Build the sorted, deduplicated active-module-ID list from the lockfile.
    ///
    /// A module ID is included in the projection when the lockfile entry has a
    /// `module_activation` record whose Nu identity and vendor target match the
    /// given parameters.
    pub fn active_module_ids_from_lockfile(
        lockfile: &Lockfile,
        nu_executable_sha256: &str,
        nu_version: &str,
        vendor_autoload_dir: &str,
        managed_file_path: &str,
    ) -> Vec<String> {
        let mut ids: Vec<String> = lockfile
            .packages
            .iter()
            .filter(|(_, entry)| {
                entry.is_module_active_for(
                    nu_executable_sha256,
                    nu_version,
                    vendor_autoload_dir,
                    managed_file_path,
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Validate that this projection agrees with the current lockfile state.
    ///
    /// Returns an error describing the disagreement if any of the following hold:
    /// - `schema_version` is not recognised.
    /// - `active_module_ids` does not exactly match the lockfile projection for
    ///   the same Nu identity and vendor target.
    ///
    /// A mismatch blocks mutation: callers must complete recovery before
    /// proceeding with activation or deactivation.
    pub fn validate_against_lockfile(&self, lockfile: &Lockfile) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            bail!(
                "Unrecognised autoload-state schema version: {} (expected {})",
                self.schema_version,
                SCHEMA_VERSION,
            );
        }

        let expected = Self::active_module_ids_from_lockfile(
            lockfile,
            &self.nu_executable_sha256,
            &self.nu_version,
            &self.vendor_autoload_dir,
            &self.managed_file_path,
        );

        if self.active_module_ids != expected {
            bail!(
                "Autoload-state projection mismatch.\n\
                 autoload-state.json reports active modules: {:?}\n\
                 Lockfile reports active modules:            {:?}\n\
                 Run `numan activate --check` to diagnose, or complete the \
                 pending recovery before mutating.",
                self.active_module_ids,
                expected,
            );
        }

        Ok(())
    }

    /// Validate that `active_module_ids` in `self` matches those in `other`.
    ///
    /// Used during recovery to confirm that a previously-written state still
    /// matches an on-disk state without re-reading the lockfile.
    pub fn module_ids_match(&self, other: &AutoloadState) -> bool {
        self.active_module_ids == other.active_module_ids
    }
}

/// Helpers for constructing an `AutoloadState` from external inputs without
/// requiring a live `NuPaths` object in tests.
impl AutoloadState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vendor_autoload_dir: String,
        managed_file_path: String,
        nu_executable_sha256: String,
        nu_version: String,
        generated_file_sha256: String,
        active_module_ids: Vec<String>,
        generated_at: String,
    ) -> Self {
        let mut ids = active_module_ids;
        ids.sort();
        ids.dedup();
        Self {
            schema_version: SCHEMA_VERSION,
            vendor_autoload_dir,
            managed_file_path,
            nu_executable_sha256,
            nu_version,
            generated_file_sha256,
            active_module_ids: ids,
            generated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::package::ModuleImportMode;
    use crate::state::lockfile::{Lockfile, LockfileEntry, ModuleActivation};
    use std::collections::BTreeMap;

    fn make_state(ids: Vec<&str>) -> AutoloadState {
        AutoloadState::new(
            "/nu/vendor/autoload".to_string(),
            "/nu/vendor/autoload/numan.nu".to_string(),
            "exe-hash".to_string(),
            "0.113.1".to_string(),
            "file-sha256".to_string(),
            ids.into_iter().map(String::from).collect(),
            "0000000000000001".to_string(),
        )
    }

    fn make_lockfile_with_active_modules(ids: &[&str]) -> Lockfile {
        let mut packages = BTreeMap::new();
        for id in ids {
            packages.insert(
                id.to_string(),
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
                    installed_at: "0".to_string(),
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
                    payload_path: format!("packages/modules/{}/1.0.0-abc", id),
                    revision_id: None,
                    payload_sha256: None,
                    executable_sha256: None,
                    selection_reason: None,
                    origin: None,
                    module_activation: Some(ModuleActivation {
                        entry_path: format!("/root/packages/modules/{}/1.0.0-abc/mod.nu", id),
                        import_mode: ModuleImportMode::Module,
                        vendor_autoload_dir: "/nu/vendor/autoload".to_string(),
                        managed_file_path: "/nu/vendor/autoload/numan.nu".to_string(),
                        nu_executable_sha256: "exe-hash".to_string(),
                        nu_version: "0.113.1".to_string(),
                        activated_at: "0".to_string(),
                    }),
                    module_import_mode: Some(ModuleImportMode::Module),
                    overlay: None,
                    locked_dependencies: BTreeMap::new(),
                },
            );
        }
        Lockfile {
            version: 2,
            generated_at: "ts".to_string(),
            nu_version: "0.113.1".to_string(),
            platform: "x86_64-linux-gnu".to_string(),
            packages,
        }
    }

    // ── save / load / delete ─────────────────────────────────────────────────

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(vec!["owner/bar", "owner/foo"]);
        state.save(dir.path()).unwrap();
        let loaded = AutoloadState::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.active_module_ids, vec!["owner/bar", "owner/foo"]);
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.nu_executable_sha256, "exe-hash");
    }

    #[test]
    fn load_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(AutoloadState::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn delete_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(vec!["owner/foo"]);
        state.save(dir.path()).unwrap();
        assert!(AutoloadState::state_path(dir.path()).exists());
        AutoloadState::delete(dir.path()).unwrap();
        assert!(!AutoloadState::state_path(dir.path()).exists());
    }

    #[test]
    fn delete_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        // Must not error when file is absent.
        AutoloadState::delete(dir.path()).unwrap();
    }

    // ── new() sorts and deduplicates ids ────────────────────────────────────

    #[test]
    fn new_sorts_and_deduplicates_ids() {
        let state = AutoloadState::new(
            "/vendor".to_string(),
            "/vendor/numan.nu".to_string(),
            "h".to_string(),
            "0.113.1".to_string(),
            "sha".to_string(),
            vec![
                "owner/zeta".to_string(),
                "owner/alpha".to_string(),
                "owner/alpha".to_string(),
            ],
            "ts".to_string(),
        );
        assert_eq!(state.active_module_ids, vec!["owner/alpha", "owner/zeta"]);
    }

    // ── active_module_ids_from_lockfile ──────────────────────────────────────

    #[test]
    fn projection_from_lockfile_sorted() {
        let lock = make_lockfile_with_active_modules(&["owner/zeta", "owner/alpha"]);
        let ids = AutoloadState::active_module_ids_from_lockfile(
            &lock,
            "exe-hash",
            "0.113.1",
            "/nu/vendor/autoload",
            "/nu/vendor/autoload/numan.nu",
        );
        assert_eq!(ids, vec!["owner/alpha", "owner/zeta"]);
    }

    #[test]
    fn projection_excludes_wrong_nu_identity() {
        let lock = make_lockfile_with_active_modules(&["owner/foo"]);
        let ids = AutoloadState::active_module_ids_from_lockfile(
            &lock,
            "wrong-hash",
            "0.113.1",
            "/nu/vendor/autoload",
            "/nu/vendor/autoload/numan.nu",
        );
        assert!(ids.is_empty());
    }

    #[test]
    fn projection_excludes_wrong_vendor_dir() {
        let lock = make_lockfile_with_active_modules(&["owner/foo"]);
        let ids = AutoloadState::active_module_ids_from_lockfile(
            &lock,
            "exe-hash",
            "0.113.1",
            "/other/vendor/autoload",
            "/nu/vendor/autoload/numan.nu",
        );
        assert!(ids.is_empty());
    }

    // ── validate_against_lockfile ────────────────────────────────────────────

    #[test]
    fn validate_succeeds_when_projection_matches() {
        let lock = make_lockfile_with_active_modules(&["owner/bar", "owner/foo"]);
        let state = make_state(vec!["owner/bar", "owner/foo"]);
        state.validate_against_lockfile(&lock).unwrap();
    }

    #[test]
    fn validate_fails_on_mismatch() {
        let lock = make_lockfile_with_active_modules(&["owner/foo"]);
        // State claims "owner/bar" is active too — lockfile disagrees.
        let state = make_state(vec!["owner/bar", "owner/foo"]);
        let err = state.validate_against_lockfile(&lock).unwrap_err();
        assert!(err.to_string().contains("projection mismatch"));
    }

    #[test]
    fn validate_fails_on_wrong_schema_version() {
        let lock = make_lockfile_with_active_modules(&[]);
        let mut state = make_state(vec![]);
        state.schema_version = 99;
        let err = state.validate_against_lockfile(&lock).unwrap_err();
        assert!(err.to_string().contains("schema version"));
    }

    // ── module_ids_match ─────────────────────────────────────────────────────

    #[test]
    fn module_ids_match_true_for_identical() {
        let a = make_state(vec!["owner/foo"]);
        let b = make_state(vec!["owner/foo"]);
        assert!(a.module_ids_match(&b));
    }

    #[test]
    fn module_ids_match_false_for_different() {
        let a = make_state(vec!["owner/foo"]);
        let b = make_state(vec!["owner/bar"]);
        assert!(!a.module_ids_match(&b));
    }
}
