//! Journaled migration transaction for the legacy single-binary → versioned
//! Nushell install layout.
//!
//! Previously, `migrate_legacy_install` recovered from a half-applied state by
//! opportunistically pruning any empty `<version>/` subdirectory it found at
//! the top of each attempt. That worked but had no on-disk audit: a half-state
//! was indistinguishable from "no journal ever existed". This module promotes
//! that recovery into a journaled transaction, mirroring the discipline of
//! [`state/autoload_journal`](crate::state::autoload_journal) and
//! [`state/journal`](crate::state::journal).
//!
//! ## Stages
//!
//! Mirrors `state/autoload_journal.rs` stage semantics:
//!
//! - [`MigrationStage::Prepared`] — written BEFORE `create_dir_all(<version>/)`;
//!   the only filesystem side effect is the (potential) empty subdir.
//! - [`MigrationStage::Renamed`] — written AFTER the legacy binary has been
//!   renamed into `<version>/<bin>`. `nu_state/active-version.json` is not
//!   yet written.
//! - [`MigrationStage::Active`] — written AFTER `write_active_version` succeeds.
//!   The journal is cleared on this transition (definitively complete).
//!
//! ## Recovery
//!
//! [`reconcile`] runs at the top of `migrate_legacy_install_with_detector`
//! (self-heal) and is invoked by `numan doctor --fix` (Auto-tier repair). It
//! removes the empty subdir in the `Prepared` case, completes the
//! `write_active_version` step in the `Renamed` case, and defensively clears
//! the journal in any `Active`-or-better state.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::nu::version_manager::{
    read_active_version, version_install_dir, versioned_nu_dir, write_active_version,
};
use crate::util::atomic::write_json_atomic;
use crate::util::fs_safety::assert_not_symlink;

/// Schema version for `migration-journal.json`.
pub const SCHEMA_VERSION: u32 = 1;

/// In-flight stage of the legacy-Nu migration transaction.
///
/// `Prepared` — journal written; `create_dir_all(<version>/)` may or may not
/// have run; legacy binary may or may not still exist (depends on whether the
/// `create_dir_all` itself crashed, or anything in between).
/// `Renamed`  — legacy binary moved into `<version>/<bin>`; `write_active_version`
/// has not yet succeeded.
/// `Active`   — all filesystem effects are committed; the journal is cleared
/// on entry to this stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStage {
    /// Journal written; `create_dir_all(<version>/)` is the next filesystem effect.
    Prepared,
    /// Legacy binary moved; `write_active_version` is the next filesystem effect.
    Renamed,
    /// All filesystem effects committed; journal clear is the next (and last) step.
    Active,
}

/// Crash-recovery journal for the single-binary → versioned-Nu migration.
///
/// Written to `<root>/state/migration-journal.json`. Cleared on successful
/// completion. Presence on disk after a `numan use`/`migrate_legacy_install`
/// invocation indicates an interrupted migration. [`reconcile`] is called by
/// `numan use`, the top of `migrate_legacy_install_with_detector`, and
/// `numan doctor --fix` to clean up half-states the next time the system boots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingMigration {
    /// Schema version for forward-compatibility.
    pub schema_version: u32,
    /// The Nu version this journal pertains to (already normalized).
    pub version: String,
    /// Current in-flight stage.
    pub stage: MigrationStage,
}

impl std::fmt::Display for MigrationStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            MigrationStage::Prepared => "prepared",
            MigrationStage::Renamed => "renamed",
            MigrationStage::Active => "active",
        };
        f.write_str(s)
    }
}

/// Reject a path component that contains traversal separators, control
/// characters, NULs, or absolute-path indicators. The migration journal's
/// `version` field is used as a directory name under
/// `<root>/tools/nushell/<version>/`. A tampered or corrupted journal with
/// `..` segments, leading slashes, or backslashes would let `reconcile()`
/// (or anything that builds on top of the journal) escape the managed
/// tree. Applied in both `save()` (block tampered writes) and `reconcile()`
/// (refuse to act on a tampered disk state).
pub fn is_safe_version_component(v: &str) -> bool {
    if v.is_empty() || v.len() > 64 {
        return false;
    }
    if v == "." || v == ".." {
        return false;
    }
    if v.starts_with('/') || v.starts_with('\\') {
        return false;
    }
    for ch in v.chars() {
        if ch.is_control() || ch == '/' || ch == '\\' || ch == ':' || ch == '\0' {
            return false;
        }
    }
    true
}

impl PendingMigration {
    /// Path to the journal: `<root>/state/migration-journal.json`.
    /// Public for callers that need to reference it for error context.
    pub(crate) fn journal_path(root: &Path) -> PathBuf {
        root.join("state").join("migration-journal.json")
    }

    /// Load the journal from disk; `None` when absent.
    pub fn load(root: &Path) -> Result<Option<Self>> {
        let path = Self::journal_path(root);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read migration journal at '{}'", path.display()))?;
        let journal: Self =
            serde_json::from_str(&content).context("Failed to parse migration-journal.json")?;
        // copilot PR69 VwSra: hard-fail on unknown schema_version so a future
        // variant cannot be silently misinterpreted as the current one.
        // The original "coerce to SCHEMA_VERSION" wording was aspirational
        // and never actually performed; treat it as an error so doctor finds
        // it instead of silently downgrading.
        if journal.schema_version != SCHEMA_VERSION {
            anyhow::bail!(
                "Migration journal at '{}' uses schema_version {} but this build expects {}.                  Upgrade Numan or remove the stale journal.",
                path.display(),
                journal.schema_version,
                SCHEMA_VERSION,
            );
        }
        Ok(Some(journal))
    }

    /// Atomically write the journal to disk.
    pub fn save(&self, root: &Path) -> Result<()> {
        if !is_safe_version_component(&self.version) {
            bail!(
                "Refusing to write migration journal with unsafe version component '{}'. \
                 A version with traversal segments, separators, or control characters \
                 would let later reconciliation escape the managed tree. \
                 This is either a tampered parent or a logic bug; refuse to advance.",
                self.version
            );
        }
        write_json_atomic(&Self::journal_path(root), self)
    }

    /// Delete (clear) the journal. Idempotent: does not error when the file is
    /// absent.
    pub fn delete(root: &Path) -> Result<()> {
        let path = Self::journal_path(root);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| {
                format!("Failed to remove migration journal at '{}'", path.display())
            }),
        }
    }
}

/// File-system truth check: does the canonical Nu binary for `<version>`
/// exist in the versioned layout?
///
/// Takes precedence over the journal stage when there is disagreement —
/// recovery actions are gated by what's actually on disk.
fn versioned_binary_present(root: &Path, version: &str) -> bool {
    let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };
    version_install_dir(root, version).join(bin_name).is_file()
}

/// Reconcile any in-flight migration journal.
///
/// Recovery actions:
/// - [`MigrationStage::Prepared`] — best-effort remove the associated empty
///   `<version>/` subdir, then delete the journal.
/// - [`MigrationStage::Renamed`] — confirm the versioned binary exists on
///   disk (the journal's claim is verified); if so, write the active-version
///   unless one is already set, and delete the journal plus any stray legacy
///   binary at `tools/nushell/<bin>`. Missing binary → escalate with a
///   contextual error so `numan doctor` can surface the corruption.
/// - [`MigrationStage::Active`] — every filesystem effect is committed;
///   defensively delete the journal (it should have been cleared on
///   transition into this stage).
///
/// Returns `Ok(Some(journal))` if any cleanup occurred (the journal is gone
/// from disk; `Some` only signals "we cleaned one up"). Caller can surface
/// this as a repair record or log message.
///
/// Used by `migrate_legacy_install_with_detector` (self-heal), `numan use`
/// (boot reconciliation), and `numan doctor --fix` (Auto-tier repair).
pub fn reconcile(root: &Path) -> Result<Option<PendingMigration>> {
    let Some(journal) = PendingMigration::load(root)? else {
        return Ok(None);
    };

    // Refuse to act on a tampered or corrupted journal whose version
    // contains path-traversal segments. `version_install_dir(<root>, v)`
    // appends v as a directory name; if v is `../etc` we would otherwise
    // scrub a directory outside `<root>/tools/nushell`.
    if !is_safe_version_component(&journal.version) {
        bail!(
            "Migration journal at '{}' has unsafe version component '{}'. \
             Refusing to reconcile to avoid escaping the managed tree. \
             Run `numan doctor --fix` to discard the journal.",
            PendingMigration::journal_path(root).display(),
            journal.version
        );
    }

    match journal.stage {
        MigrationStage::Prepared => {
            // The rename can complete before the journal advances to Renamed.
            // Trust the filesystem in that crash window and finish recovery.
            if versioned_binary_present(root, &journal.version) {
                if read_active_version(root)?.is_none() {
                    write_active_version(root, &journal.version).with_context(|| {
                        format!(
                            "Migration recovery: failed to write active version '{}'",
                            journal.version
                        )
                    })?;
                }
                let managed_dir = versioned_nu_dir(root);
                assert_not_symlink(&managed_dir, "managed Nushell directory")?;
                let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };
                let legacy_binary = managed_dir.join(bin_name);
                if legacy_binary.is_file() {
                    std::fs::remove_file(&legacy_binary).with_context(|| {
                        format!(
                            "Migration recovery: failed to remove stray legacy binary '{}'",
                            legacy_binary.display()
                        )
                    })?;
                }
            } else {
                // PR69 WCq: surface remove_dir failures instead of ignoring
                // them. A half-migrated state whose empty version directory
                // cannot be removed (permissions, foreign file inside) must
                // retain the journal so a follow-up reconcile can succeed
                // once the user resolves the underlying issue. Discarding
                // both the orphan dir AND the journal would silently lose
                // the recoverable crash window.
                let managed_dir = versioned_nu_dir(root);
                assert_not_symlink(&managed_dir, "managed Nushell directory")?;
                let version_dir = version_install_dir(root, &journal.version);
                if version_dir.is_dir() {
                    if let Err(e) = std::fs::remove_dir(&version_dir) {
                        bail!(
                            "Migration journal at '{}' has '{}' as Prepared-but-orphan, \
                             but the empty version directory '{}' could not be removed: {}. \
                             Journal retained so a follow-up `numan use` (or \
                             `numan doctor --fix`) can recover once permissions or \
                             the directory contents are resolved.",
                            PendingMigration::journal_path(root).display(),
                            journal.version,
                            version_dir.display(),
                            e
                        );
                    }
                }
            }
            PendingMigration::delete(root)?;
            Ok(Some(journal))
        }
        MigrationStage::Renamed => {
            // File-system truth: the versioned binary should exist.
            if !versioned_binary_present(root, &journal.version) {
                anyhow::bail!(
                    "Migration journal at '{}' is staged 'Renamed' but the versioned binary is missing.\n\
                     Run `numan setup nu {}` to repair.",
                    PendingMigration::journal_path(root).display(),
                    journal.version,
                );
            }
            // Complete the transaction — write active-version if no selection
            // exists. (If a user has already chosen a different active
            // version, that user-controlled choice takes precedence.)
            if read_active_version(root)?.is_none() {
                write_active_version(root, &journal.version).with_context(|| {
                    format!(
                        "Migration recovery: failed to write active version '{}'",
                        journal.version
                    )
                })?;
            }
            // Defensive cleanup: a stray legacy binary at `tools/nushell/<bin>`
            // would otherwise confuse `list_installed_versions` into
            // re-running migration. Remove it.
            let managed_dir = versioned_nu_dir(root);
            assert_not_symlink(&managed_dir, "managed Nushell directory")?;
            let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };
            let legacy_binary = managed_dir.join(bin_name);
            if legacy_binary.is_file() {
                std::fs::remove_file(&legacy_binary).with_context(|| {
                    format!(
                        "Migration recovery: failed to remove stray legacy binary '{}'",
                        legacy_binary.display()
                    )
                })?;
            }
            PendingMigration::delete(root)?;
            Ok(Some(journal))
        }
        MigrationStage::Active => {
            // Migration fully committed; defensively clear the journal.
            // This branch should not be reachable on the happy path
            // (the success path sets `Active` then immediately deletes),
            // but it covers the case where a previous run wrote `Active`
            // before the `delete` failed (very rare), or where an older
            // version left an `Active` entry behind. Treat as no-op
            // cleanup so the journal does not linger indefinitely.
            PendingMigration::delete(root)?;
            Ok(Some(journal))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn bin_name() -> &'static str {
        if cfg!(windows) {
            "nu.exe"
        } else {
            "nu"
        }
    }

    fn write_journal(root: &Path, version: &str, stage: MigrationStage) {
        std::fs::create_dir_all(root.join("state")).unwrap();
        PendingMigration {
            schema_version: SCHEMA_VERSION,
            version: version.to_string(),
            stage,
        }
        .save(root)
        .unwrap();
    }

    // ── save / load / delete ────────────────────────────────────────────────

    #[test]
    fn roundtrip_save_load() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let j = PendingMigration {
            schema_version: SCHEMA_VERSION,
            version: "0.113.1".to_string(),
            stage: MigrationStage::Prepared,
        };
        j.save(root).unwrap();
        let loaded = PendingMigration::load(root).unwrap().unwrap();
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.version, "0.113.1");
        assert_eq!(loaded.stage, MigrationStage::Prepared);
    }

    #[test]
    fn load_returns_none_when_absent() {
        let tmp = TempDir::new().unwrap();
        assert!(PendingMigration::load(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn delete_removes_file() {
        let tmp = TempDir::new().unwrap();
        write_journal(tmp.path(), "0.113.1", MigrationStage::Prepared);
        assert!(PendingMigration::load(tmp.path()).unwrap().is_some());
        PendingMigration::delete(tmp.path()).unwrap();
        assert!(PendingMigration::load(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn delete_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        PendingMigration::delete(tmp.path()).unwrap();
    }

    // ── stage serde roundtrip ───────────────────────────────────────────────

    #[test]
    fn stage_serde_roundtrip() {
        for stage in [
            MigrationStage::Prepared,
            MigrationStage::Renamed,
            MigrationStage::Active,
        ] {
            let s = serde_json::to_string(&stage).unwrap();
            let parsed: MigrationStage = serde_json::from_str(&s).unwrap();
            assert_eq!(parsed, stage);
        }
    }

    // ── reconcile ───────────────────────────────────────────────────────────

    #[test]
    fn reconcile_no_journal_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(reconcile(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn reconcile_prepared_removes_empty_subdir_and_clears_journal() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Empty versioned subdir from a crashed `create_dir_all(<version>/)`.
        let version_dir = version_install_dir(root, "0.113.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        assert!(version_dir.is_dir());
        write_journal(root, "0.113.1", MigrationStage::Prepared);

        let recovered = reconcile(root).unwrap().unwrap();
        assert_eq!(recovered.stage, MigrationStage::Prepared);
        assert_eq!(recovered.version, "0.113.1");
        assert!(!version_dir.exists(), "empty subdir must be removed");
        assert!(
            PendingMigration::load(root).unwrap().is_none(),
            "journal must be cleared on reconcile"
        );
    }

    #[test]
    fn reconcile_prepared_is_safe_when_no_orphan_subdir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // No <version>/ subdir at all (would be the case if the crash hit
        // BEFORE create_dir_all ran). Reconcile must still clear the journal
        // without error.
        write_journal(root, "0.113.1", MigrationStage::Prepared);

        let recovered = reconcile(root).unwrap().unwrap();
        assert_eq!(recovered.stage, MigrationStage::Prepared);
        assert!(PendingMigration::load(root).unwrap().is_none());
    }

    /// PR69 WCq regression: a `Prepared`-stage journal whose orphan
    /// version directory cannot be removed (here: a stray file inside
    /// makes `remove_dir` fail with ENOTEMPTY) must NOT silently clear
    /// the journal. The Err path keeps the journal intact so a follow-up
    /// reconcile can recover once the user resolves the underlying issue.
    #[test]
    fn reconcile_prepared_retains_journal_when_remove_dir_fails() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Half-migrated state: journal at Prepared + a populated version
        // subdir (the directory cannot be removed because it has content).
        let version_dir = version_install_dir(root, "0.113.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        // Plant a stray file so `remove_dir(version_dir)` returns ENOTEMPTY
        // on Unix and STATUS_DIRECTORY_NOT_EMPTY on Windows.
        std::fs::write(version_dir.join("stray.dat"), b"foreign").unwrap();
        write_journal(root, "0.113.1", MigrationStage::Prepared);

        let err = reconcile(root).expect_err("reconcile must Err when remove_dir fails");
        // The error message must name the version + the directory + the
        // underlying I/O kind so safe-batch automation can decide whether
        // to refuse or auto-recover.
        let msg = err.to_string();
        assert!(msg.contains("Prepared-but-orphan"), "msg: {msg}");
        assert!(msg.contains("0.113.1"), "msg: {msg}");
        assert!(
            msg.contains(&version_dir.display().to_string()),
            "msg must include version_dir: {msg}"
        );

        // The journal must still be present — discarded-journal state
        // would lose the recoverable crash window.
        let loaded = PendingMigration::load(root)
            .unwrap()
            .expect("journal must survive a failed reconcile");
        assert_eq!(loaded.stage, MigrationStage::Prepared);
        assert_eq!(loaded.version, "0.113.1");

        // And the orphan subdir is still on disk so the user can resolve it.
        assert!(version_dir.is_dir(), "version_dir must remain on disk");
        assert!(version_dir.join("stray.dat").exists());
    }

    #[test]
    fn reconcile_renamed_writes_active_version_and_clears_journal() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Versioned binary present (rename completed before the crash).
        let version_dir = version_install_dir(root, "0.113.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join(bin_name()), b"binary").unwrap();

        write_journal(root, "0.113.1", MigrationStage::Renamed);

        let recovered = reconcile(root).unwrap().unwrap();
        assert_eq!(recovered.stage, MigrationStage::Renamed);

        let active = read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
        assert!(PendingMigration::load(root).unwrap().is_none());
    }

    #[test]
    fn reconcile_renamed_skips_active_write_when_already_set() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let version_dir = version_install_dir(root, "0.113.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join(bin_name()), b"binary").unwrap();

        // User has since switched to a different active version — preserve
        // that user-controlled choice.
        write_active_version(root, "0.114.0").unwrap();
        write_journal(root, "0.113.1", MigrationStage::Renamed);

        reconcile(root).unwrap();

        let active = read_active_version(root).unwrap().unwrap();
        assert_eq!(
            active.version, "0.114.0",
            "user's active choice must not be overwritten by recovery"
        );
    }

    #[test]
    fn reconcile_renamed_clears_stray_legacy_binary() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let version_dir = version_install_dir(root, "0.113.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join(bin_name()), b"binary").unwrap();

        // Stray legacy binary reappeared at the original path (e.g., user
        // manually copied it back during debugging). Reconcile removes it to
        // keep the system consistent.
        std::fs::create_dir_all(versioned_nu_dir(root)).unwrap();
        std::fs::write(versioned_nu_dir(root).join(bin_name()), b"stray").unwrap();

        write_journal(root, "0.113.1", MigrationStage::Renamed);

        reconcile(root).unwrap();

        assert!(
            !versioned_nu_dir(root).join(bin_name()).is_file(),
            "stray legacy binary must be removed"
        );
        assert!(version_dir.join(bin_name()).is_file());
    }

    #[test]
    fn reconcile_renamed_escalates_when_binary_missing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Renamed journal says binary should be at <version>/<bin>, but the
        // directory is empty (corrupt state — file got manually removed).
        std::fs::create_dir_all(version_install_dir(root, "0.113.1")).unwrap();
        write_journal(root, "0.113.1", MigrationStage::Renamed);

        let err = reconcile(root).unwrap_err().to_string();
        assert!(
            err.contains("Renamed"),
            "err must name the journal stage: {err}"
        );
        assert!(err.contains("0.113.1"), "err must name the version: {err}");
    }

    #[test]
    fn reconcile_active_defensively_clears_journal() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // A stale Active-stage journal that was missed on a previous success
        // path (defensive: should never happen). Reconcile clears it without
        // touching filesystem state.
        write_journal(root, "0.113.1", MigrationStage::Active);

        let recovered = reconcile(root).unwrap().unwrap();
        assert_eq!(recovered.stage, MigrationStage::Active);
        assert!(PendingMigration::load(root).unwrap().is_none());
    }

    #[test]
    fn reconcile_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let version_dir = version_install_dir(root, "0.113.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join(bin_name()), b"binary").unwrap();
        write_journal(root, "0.113.1", MigrationStage::Renamed);

        assert!(reconcile(root).unwrap().is_some());
        // Second call: no journal left; returns None.
        assert!(reconcile(root).unwrap().is_none());
    }
}
