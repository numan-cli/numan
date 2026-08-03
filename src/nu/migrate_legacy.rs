//! Legacy single-binary Nu migrate, journaled, and self-healing.
//!
//! This module owns the one-time transition from a flat
//! `<root>/tools/nushell/nu` legacy install to the versioned layout
//! `<root>/tools/nushell/<version>/nu`. The transition is a journaled
//! transaction (`Prepared` -> `Renamed` -> `Active`) tracked at
//! `state/migration-journal.json`. `reconcile` self-heals any half-
//! applied journal at the top of each [`migrate_legacy_install`] call;
//! `numan doctor --fix` reconciles pending journals without invoking
//! `numan use`.
//!
//! Extracted from `nu::version_manager` for the
//! `pr-migrate-legacy-installs` PR.

use std::path::{Path, PathBuf};
use thiserror::Error;

use super::version_manager::{
    legacy_managed_binary_with_bin, normalize_version, nu_binary_name, version_binary,
    version_install_dir, versioned_nu_dir, write_active_version, VersionManagerError,
};
use crate::state::migration_journal::{
    self as migration_journal, MigrationJournalError, MigrationStage, PendingMigration,
    SCHEMA_VERSION,
};

/// Errors from legacy single-binary → versioned Nu migration.
#[derive(Debug, Error)]
pub enum MigrateLegacyError {
    #[error(
        "Refusing to migrate: managed Nushell directory '{}' is a symlink or reparse point. \
         Run `numan setup nu` from a clean root before migrating.",
        path.display()
    )]
    SymlinkedManagedDir { path: PathBuf },

    #[error("Failed to check managed Nushell directory '{}': {message}", path.display())]
    SymlinkCheck { path: PathBuf, message: String },

    #[error(transparent)]
    Journal(#[from] MigrationJournalError),

    /// Renamed-stage journal whose versioned binary is gone: manual recovery only.
    #[error(
        "Legacy Nu migration is stuck at Renamed for version '{version}' \
         (journal: '{}'). The versioned binary is missing, so automatic \
         recovery cannot continue. Discard the journal file to unblock, or \
         reinstall with `numan setup nu {version}` and re-run `numan use` / \
         `numan doctor`.",
        path.display()
    )]
    RenamedBinaryManualRecovery { path: PathBuf, version: String },

    #[error(transparent)]
    VersionManager(#[from] VersionManagerError),

    #[error("{message}")]
    Detector { message: String },

    #[error("Failed to determine version of legacy Nu binary at '{}': {message}", path.display())]
    DetectFailed { path: PathBuf, message: String },

    #[error("Failed to read VERSION metadata at '{}'", path.display())]
    ReadVersionFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("VERSION metadata at '{}' did not contain a parseable Nu version", path.display())]
    UnparseableVersionFile { path: PathBuf },

    #[error("Failed to execute legacy Nu binary at '{}'", path.display())]
    ExecuteLegacy {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Legacy Nu binary at '{}' exited with status {status}", path.display())]
    LegacyExitStatus {
        path: PathBuf,
        status: std::process::ExitStatus,
    },

    #[error("Failed to parse Nu version from output: '{output}'")]
    ParseVersionOutput { output: String },

    #[error("Failed to read versioned Nu directory '{}'", path.display())]
    ReadVersionedDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "Failed to write migration journal (Prepared) before 'create_dir_all' for '{version}'"
    )]
    JournalPrepared {
        version: String,
        #[source]
        source: MigrationJournalError,
    },

    #[error(
        "Failed to create version directory '{}' (migration journal at Prepared: '{}')",
        path.display(),
        journal_path.display()
    )]
    CreateVersionDir {
        path: PathBuf,
        journal_path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Post-create hook blocked migration for '{}': {message}", path.display())]
    PostCreateHook { path: PathBuf, message: String },

    #[error(
        "Failed to move '{}' to '{}' (migration journal at Prepared: a future reconcile will clean up '<{version}>/')",
        from.display(),
        to.display()
    )]
    Rename {
        from: PathBuf,
        to: PathBuf,
        version: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "Failed to advance migration journal to 'Renamed' (legacy binary already moved to '{}')",
        binary.display()
    )]
    JournalRenamed {
        binary: PathBuf,
        #[source]
        source: MigrationJournalError,
    },

    #[error(
        "Failed to persist active Nu version marker for '{version}' (legacy binary already moved to '{}'; migration journal is at Renamed stage)",
        binary.display()
    )]
    PersistActive {
        version: String,
        binary: PathBuf,
        #[source]
        source: VersionManagerError,
    },

    #[error(
        "Failed to advance migration journal to 'Active' for '{version}' (active version is set; clearing journal)"
    )]
    JournalActive {
        version: String,
        #[source]
        source: MigrationJournalError,
    },
}

/// Injectable version-detection seam for [`migrate_legacy_install`].
///
/// Tests inject a closure that returns a fixed version (or an error) so
/// migration can be exercised without spawning a real `nu` process.
pub type LegacyVersionDetector = dyn Fn(&Path) -> Result<String, MigrateLegacyError>;

/// Injectable post-create seam for [`migrate_legacy_install_with_detector`].
///
/// Fired AFTER `create_dir_all(<version>/)` succeeds and BEFORE the rename of
/// the legacy binary into `<version>/<bin>`. Tests inject a closure returning
/// an `Err` to simulate the original bug where `rename` cannot cross a device
/// or filesystem boundary (or any other rename-pre failure), leaving the
/// empty versioned subdir behind on disk. A second migration attempt with no
/// hook should clean up that artifact and succeed — the recovery test
/// `migrate_legacy_recovers_from_post_create_hook_failure` proves that.
pub type LegacyPostCreateHook = dyn Fn(&Path) -> Result<(), MigrateLegacyError>;

/// Production detector: prefer the VERSION metadata file written by
/// `install_from_archive`. Fall back to probing `nu --version` when no
/// metadata is present (e.g. a manually placed legacy binary).
///
/// We avoid an unbounded `Command::output` probe when metadata exists; the
/// probe is best-effort and depends on the Nu process terminating quickly.
pub fn detect_legacy_version(binary: &Path) -> Result<String, MigrateLegacyError> {
    if let Some(parent) = binary.parent() {
        let version_file = parent.join("VERSION");
        if version_file.is_file() {
            let content = std::fs::read_to_string(&version_file).map_err(|source| {
                MigrateLegacyError::ReadVersionFile {
                    path: version_file.clone(),
                    source,
                }
            })?;
            let version = parse_nu_version_from_output(&content)
                .map_err(|_| MigrateLegacyError::UnparseableVersionFile { path: version_file })?;
            return Ok(version);
        }
    }

    let output = std::process::Command::new(binary)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|source| MigrateLegacyError::ExecuteLegacy {
            path: binary.to_path_buf(),
            source,
        })?;

    if !output.status.success() {
        return Err(MigrateLegacyError::LegacyExitStatus {
            path: binary.to_path_buf(),
            status: output.status,
        });
    }

    parse_nu_version_from_output(&String::from_utf8_lossy(&output.stdout))
}

/// Migrate a legacy single-binary install to the versioned structure.
///
/// If `<root>/tools/nushell/nu` exists but no versioned directories exist,
/// detect its version and move it to `<root>/tools/nushell/<version>/nu`.
///
/// Returns `Ok(true)` if migration occurred, `Ok(false)` otherwise.
pub fn migrate_legacy_install(root: &Path) -> Result<bool, MigrateLegacyError> {
    migrate_legacy_install_with_detector(root, &detect_legacy_version, None)
}

/// Same as [`migrate_legacy_install`] but accepts an injected version-detection
/// seam so tests can exercise migration paths without spawning a real `nu`.
///
/// `post_create` is fired after `create_dir_all(<version>/)` succeeds and
/// before the rename of the legacy binary into `<version>/<bin>`. Tests pass
/// `Some(&post_create_hook)` to simulate the original cross-device rename
/// bug where the legacy binary move fails between devices, leaving the
/// empty versioned subdir on disk. Production callers pass `None`.
pub fn migrate_legacy_install_with_detector(
    root: &Path,
    detect: &LegacyVersionDetector,
    post_create: Option<&LegacyPostCreateHook>,
) -> Result<bool, MigrateLegacyError> {
    // Refuse to scan or mutate under a symlinked managed
    // directory. A symlink under `<root>/tools/nushell` could redirect the
    // rename or filesystem-truth cleanup outside `$NUMAN_ROOT` and silently
    // rewrite unrelated user-visible state.
    let legacy_dir = versioned_nu_dir(root);
    match crate::util::fs_safety::is_symlink_or_reparse(&legacy_dir) {
        Ok(true) => {
            return Err(MigrateLegacyError::SymlinkedManagedDir { path: legacy_dir });
        }
        Ok(false) => {}
        Err(e) => {
            return Err(MigrateLegacyError::SymlinkCheck {
                path: legacy_dir,
                message: e.to_string(),
            });
        }
    }

    // Self-heal any in-flight migration journal from a prior crashed attempt.
    // The `Prepared` path removes any orphan `<version>/` subdir; the
    // `Renamed` path completes the active-version write. This is what makes
    // the migration a journaled transaction: every stage is recoverable.
    //
    // `RenamedBinaryMissing` is not auto-healable: classify it as manual
    // recovery with the journal path and discard/reinstall guidance instead
    // of propagating the journal error as a blocking opaque failure. Other
    // reconcile outcomes keep their existing recovery behavior.
    match migration_journal::reconcile(root) {
        Ok(_) => {}
        Err(MigrationJournalError::RenamedBinaryMissing { path, version }) => {
            return Err(MigrateLegacyError::RenamedBinaryManualRecovery { path, version });
        }
        Err(e) => return Err(MigrateLegacyError::Journal(e)),
    }

    let legacy_binary = legacy_managed_binary_with_bin(root, nu_binary_name());

    if !legacy_binary.exists() {
        return Ok(false);
    }

    // If a real, populated versioned install exists, don't migrate (avoid
    // conflicts). An empty subdirectory left behind by a partial migration
    // failure (from before journaling was introduced) must NOT block
    // re-migration: clean it up, then fall through. The journal-driven
    // `reconcile` above handles the same scenario for newer half-states.
    let versioned_dir = versioned_nu_dir(root);
    if versioned_dir.exists() {
        let mut found_installed = false;
        let bin_name = nu_binary_name();
        let entries = std::fs::read_dir(&versioned_dir).map_err(|source| {
            MigrateLegacyError::ReadVersionedDir {
                path: versioned_dir.clone(),
                source,
            }
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| MigrateLegacyError::ReadVersionedDir {
                path: versioned_dir.clone(),
                source,
            })?;
            let file_type =
                entry
                    .file_type()
                    .map_err(|source| MigrateLegacyError::ReadVersionedDir {
                        path: versioned_dir.clone(),
                        source,
                    })?;
            if !file_type.is_dir() {
                continue;
            }
            if entry.path().join(bin_name).exists() {
                found_installed = true;
            } else {
                // Empty subdir — likely from an aborted previous migration.
                // Remove it so the user is not permanently stuck with an
                // empty <version>/ blocking every future attempt.
                let _ = std::fs::remove_dir(entry.path());
            }
        }
        if found_installed {
            return Ok(false);
        }
    }

    let version = detect(&legacy_binary).map_err(|e| MigrateLegacyError::DetectFailed {
        path: legacy_binary.clone(),
        message: e.to_string(),
    })?;
    let version = normalize_version(&version)?;

    // Journal `Prepared` BEFORE `create_dir_all(<version>/)`. Every later
    // filesystem effect is gated on this entry existing. If we crash now, the
    // next reconcile path removes the empty subdir and clears the journal.
    let journal = PendingMigration {
        schema_version: SCHEMA_VERSION,
        version: version.clone(),
        stage: MigrationStage::Prepared,
    };
    journal
        .save(root)
        .map_err(|source| MigrateLegacyError::JournalPrepared {
            version: version.clone(),
            source,
        })?;

    let version_dir = version_install_dir(root, &version);
    let version_journal_path = PendingMigration::journal_path(root);
    std::fs::create_dir_all(&version_dir).map_err(|source| {
        MigrateLegacyError::CreateVersionDir {
            path: version_dir.clone(),
            journal_path: version_journal_path,
            source,
        }
    })?;

    // Post-create seam (tests only). Real callers pass `None`; a failing
    // hook here simulates the original cross-device rename bug.
    if let Some(hook) = post_create {
        hook(&version_dir).map_err(|e| MigrateLegacyError::PostCreateHook {
            path: version_dir.clone(),
            message: e.to_string(),
        })?;
    }

    let new_binary = version_binary(root, &version);
    std::fs::rename(&legacy_binary, &new_binary).map_err(|source| MigrateLegacyError::Rename {
        from: legacy_binary.clone(),
        to: new_binary.clone(),
        version: version.clone(),
        source,
    })?;

    // Journal `Renamed` BEFORE `write_active_version`. If we crash here,
    // reconcile completes the active-version write on the next invocation.
    let journal = PendingMigration {
        schema_version: SCHEMA_VERSION,
        version: version.clone(),
        stage: MigrationStage::Renamed,
    };
    journal
        .save(root)
        .map_err(|source| MigrateLegacyError::JournalRenamed {
            binary: new_binary.clone(),
            source,
        })?;

    // Set as active version.
    write_active_version(root, &version).map_err(|source| MigrateLegacyError::PersistActive {
        version: version.clone(),
        binary: new_binary.clone(),
        source,
    })?;

    // Set the journal stage to `Active` and immediately clear it — any
    // `numan use`/`numan doctor` reconcile pass that runs after this will
    // see no journal and be a no-op.
    let journal = PendingMigration {
        schema_version: SCHEMA_VERSION,
        version: version.clone(),
        stage: MigrationStage::Active,
    };
    journal
        .save(root)
        .map_err(|source| MigrateLegacyError::JournalActive {
            version: version.clone(),
            source,
        })?;
    PendingMigration::delete(root)?;

    // Note: the legacy binary's parent (`<root>/tools/nushell/`) now contains
    // the versioned install we just moved into it, so it cannot be removed.
    // The versioned `<version>/` subtree is the authoritative location from
    // this point forward; `tools/nushell/nu` will not be recreated.

    Ok(true)
}

/// Parse a Nu version string from `nu --version` output.
///
/// Handles formats like:
/// - "Nushell 0.113.1 (abc123)"
/// - "0.113.1"
/// - "0.113.1\n"
fn parse_nu_version_from_output(output: &str) -> Result<String, MigrateLegacyError> {
    let trimmed = output.trim();
    // Strip an optional leading "Nushell " so the rest matches what
    // `NuVersion::parse` already accepts (semver with optional build
    // hash / pre-release suffix; see core/nu_version.rs).
    let body = trimmed
        .strip_prefix("Nushell ")
        .unwrap_or(trimmed)
        .trim_start_matches('v');
    // Delegate to NuVersion::parse so build-hash
    // suffixes ("0.113.1 (abc123)") and bare semvers both work.
    if let Ok(parsed) = crate::core::nu_version::NuVersion::parse(body) {
        return Ok(parsed.version);
    }
    if semver::Version::parse(body).is_ok() {
        return Ok(body.to_string());
    }
    Err(MigrateLegacyError::ParseVersionOutput {
        output: trimmed.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nu::version_manager::read_active_version;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    #[test]
    fn test_parse_nu_version_from_output() {
        assert_eq!(
            parse_nu_version_from_output("Nushell 0.113.1 (abc123)").unwrap(),
            "0.113.1"
        );
        assert_eq!(parse_nu_version_from_output("0.113.1").unwrap(), "0.113.1");
        assert_eq!(
            parse_nu_version_from_output("0.113.1\n").unwrap(),
            "0.113.1"
        );
        assert!(parse_nu_version_from_output("invalid").is_err());
    }

    fn create_legacy_binary(root: &Path, with_version_file: Option<&str>) -> PathBuf {
        let bin_name = nu_binary_name();
        let tools = root.join("tools").join("nushell");
        std::fs::create_dir_all(&tools).unwrap();
        let binary = tools.join(bin_name);
        std::fs::write(&binary, b"fake legacy nu").unwrap();
        if let Some(version) = with_version_file {
            std::fs::write(tools.join("VERSION"), version).unwrap();
        }
        binary
    }

    #[test]
    fn migrate_legacy_skips_when_no_legacy_binary() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Detector must not be called when there's nothing to migrate; a panic
        // in the closure gives a loud failure if it is ever invoked.
        let result = migrate_legacy_install_with_detector(
            root,
            &|_| panic!("detector must not be called when legacy binary is absent"),
            None,
        )
        .unwrap();
        assert!(!result);
    }

    #[test]
    fn migrate_legacy_skips_when_versioned_dir_already_present() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let binary = create_legacy_binary(root, None);
        let bin_name = nu_binary_name();
        let existing = root.join("tools/nushell/0.114.0");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join(bin_name), b"different binary").unwrap();

        let result = migrate_legacy_install_with_detector(
            root,
            &|_| panic!("detector must not be invoked while a versioned dir exists"),
            None,
        )
        .unwrap();

        assert!(!result, "must not migrate while any versioned dir exists");
        assert!(binary.exists(), "legacy binary must be left untouched");
    }

    #[test]
    fn migrate_legacy_moves_binary_and_persists_marker() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let binary = create_legacy_binary(root, None);
        let bin_name = nu_binary_name();

        let result =
            migrate_legacy_install_with_detector(root, &|_| Ok("0.113.1".to_string()), None)
                .unwrap();

        assert!(result);
        assert!(!binary.exists(), "legacy binary should be moved");
        let moved = root.join("tools/nushell/0.113.1").join(bin_name);
        assert!(moved.is_file(), "versioned binary should exist");
        let active = read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
    }

    #[test]
    fn migrate_legacy_propagates_detector_failure() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let binary = create_legacy_binary(root, None);

        let result = migrate_legacy_install_with_detector(
            root,
            &|_| {
                Err(MigrateLegacyError::Detector {
                    message: "simulated probe failure".into(),
                })
            },
            None,
        );

        assert!(result.is_err());
        assert!(
            binary.exists(),
            "binary must be left in place when detector fails"
        );
        assert!(
            read_active_version(root).unwrap().is_none(),
            "active marker must not be written on detector failure"
        );
    }

    #[test]
    fn migrate_legacy_recovers_from_post_create_hook_failure() {
        // Reproduce the exact original bug: `create_dir_all(<version>/)`
        // succeeds, then the subsequent rename (or its real-world analogues:
        // cross-device rename, NFS rename, etc.) fails before the legacy
        // binary moves into the versioned tree. The empty versioned subdir
        // is the only artifact left behind. A second migration attempt must
        // detect and clean up that empty subdir, then succeed end-to-end.

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let bin_name = nu_binary_name();

        let tools = root.join("tools/nushell");
        std::fs::create_dir_all(&tools).unwrap();
        let legacy = tools.join(bin_name);
        std::fs::write(&legacy, b"fake legacy nu").unwrap();

        // First attempt: post-create hook fires AFTER `create_dir_all(s)`
        // and BEFORE the rename, returning an error that simulates the
        // original cross-device rename failure.
        let first = migrate_legacy_install_with_detector(
            root,
            &|_| Ok("0.113.1".to_string()),
            Some(&|_| {
                Err(MigrateLegacyError::Detector {
                    message: "simulated cross-device rename failure".into(),
                })
            }),
        );
        assert!(
            first.is_err(),
            "first attempt must fail when the post-create hook bails"
        );
        assert!(
            legacy.exists(),
            "legacy binary must be left in place when post-create hook fails"
        );
        assert!(
            tools.join("0.113.1").is_dir(),
            "empty versioned subdir must exist (the simulated half-migrated state)"
        );
        assert!(
            read_active_version(root).unwrap().is_none(),
            "active marker must not be written on the failed first attempt"
        );

        // Second attempt: no hook — the cleanup path now removes the empty
        // subdir and the rename succeeds.
        let second =
            migrate_legacy_install_with_detector(root, &|_| Ok("0.113.1".to_string()), None)
                .unwrap();
        assert!(
            second,
            "second attempt must succeed after cleaning up the empty subdir"
        );
        assert!(
            !legacy.exists(),
            "legacy binary must be moved on the recovery attempt"
        );
        let moved = tools.join("0.113.1").join(bin_name);
        assert!(
            moved.is_file(),
            "versioned binary must exist after recovery"
        );
        let active = read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
        assert!(
            PendingMigration::load(root).unwrap().is_none(),
            "migration journal must be cleared after a successful recovery"
        );
    }

    #[test]
    fn migrate_legacy_proceeds_after_partial_failure() {
        // Simulates a partial-failure state from a prior interrupted
        // migration: `create_dir_all(<version>/)` succeeded but the
        // subsequent `rename` somehow failed, leaving an empty subdir
        // inside tools/nushell/. The next migration attempt must clean up
        // the empty subdir and proceed.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let bin_name = nu_binary_name();

        // Legacy binary at the pre-migration location.
        let tools = root.join("tools/nushell");
        std::fs::create_dir_all(&tools).unwrap();
        let legacy = tools.join(bin_name);
        std::fs::write(&legacy, b"fake legacy nu").unwrap();

        // Empty subdir from the simulated failed migration.
        let empty_subdir = tools.join("0.114.0");
        std::fs::create_dir_all(&empty_subdir).unwrap();

        let result =
            migrate_legacy_install_with_detector(root, &|_| Ok("0.113.1".to_string()), None)
                .unwrap();

        assert!(
            result,
            "should migrate after cleaning up the empty versioned subdir"
        );
        assert!(
            !legacy.exists(),
            "legacy binary should be moved into the versioned tree"
        );
        assert!(
            !empty_subdir.exists(),
            "empty versioned subdir from aborted migration must be cleaned up"
        );
        let moved = tools.join("0.113.1").join(bin_name);
        assert!(
            moved.is_file(),
            "versioned binary should exist after migration"
        );
        let active = read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
        assert!(
            PendingMigration::load(root).unwrap().is_none(),
            "migration journal must be cleared after a successful recovery"
        );
    }

    #[test]
    fn migrate_legacy_cleans_empty_subdir_while_keeping_populated_one() {
        // Mixed state: one populated versioned install (0.114.0) and one empty
        // sibling (0.115.0). Migration must skip (real install present) while
        // silently removing the empty sibling.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let bin_name = nu_binary_name();
        let tools = root.join("tools/nushell");
        std::fs::create_dir_all(&tools).unwrap();
        let legacy = tools.join(bin_name);
        std::fs::write(&legacy, b"fake legacy nu").unwrap();

        let populated = tools.join("0.114.0");
        std::fs::create_dir_all(&populated).unwrap();
        std::fs::write(populated.join(bin_name), b"existing install").unwrap();

        let empty = tools.join("0.115.0");
        std::fs::create_dir_all(&empty).unwrap();

        let result =
            migrate_legacy_install_with_detector(root, &|_| Ok("0.113.1".to_string()), None)
                .unwrap();

        assert!(!result, "must not migrate when a populated install exists");
        assert!(legacy.exists(), "legacy binary must be left untouched");
        assert!(
            populated.is_dir() && populated.join(bin_name).is_file(),
            "populated install must remain intact"
        );
        assert!(
            !empty.exists(),
            "empty sibling must be cleaned up even when migration is skipped"
        );
    }

    #[test]
    fn production_detector_prefers_version_metadata_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let binary = create_legacy_binary(root, Some("0.113.1\n"));
        let detected = detect_legacy_version(&binary).unwrap();
        assert_eq!(detected, "0.113.1");
    }

    #[test]
    fn migrate_legacy_auto_heals_renamed_journal_with_binary_present() {
        // Renamed journal + versioned binary already on disk: reconcile completes
        // the active-version write and clears the journal. migrate returns
        // Ok(false) because no legacy binary remains to migrate.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let bin_name = nu_binary_name();
        let version_dir = version_install_dir(root, "0.113.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join(bin_name), b"fake versioned nu").unwrap();
        PendingMigration {
            schema_version: SCHEMA_VERSION,
            version: "0.113.1".to_string(),
            stage: MigrationStage::Renamed,
        }
        .save(root)
        .unwrap();

        let migrated = migrate_legacy_install_with_detector(
            root,
            &|_| panic!("detector must not run when Renamed reconcile auto-heals"),
            None,
        )
        .unwrap();
        assert!(
            !migrated,
            "auto-heal must not report a fresh migration when only reconcile ran"
        );
        let active = read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
        assert!(
            PendingMigration::load(root).unwrap().is_none(),
            "auto-heal must clear the Renamed journal"
        );
    }

    #[test]
    fn migrate_legacy_classifies_renamed_binary_missing_as_manual_recovery() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Renamed journal without a versioned binary: reconcile cannot auto-heal.
        std::fs::create_dir_all(version_install_dir(root, "0.113.1")).unwrap();
        PendingMigration {
            schema_version: SCHEMA_VERSION,
            version: "0.113.1".to_string(),
            stage: MigrationStage::Renamed,
        }
        .save(root)
        .unwrap();

        let err = migrate_legacy_install_with_detector(
            root,
            &|_| panic!("detector must not run when Renamed recovery is manual"),
            None,
        )
        .unwrap_err();
        let msg = err.to_string();
        match err {
            MigrateLegacyError::RenamedBinaryManualRecovery { path, version } => {
                assert_eq!(version, "0.113.1");
                assert_eq!(path, PendingMigration::journal_path(root));
            }
            other => panic!("expected RenamedBinaryManualRecovery, got {other}"),
        }
        assert!(
            msg.contains("Discard the journal file to unblock"),
            "diagnostic must include full discard guidance: {msg}"
        );
        assert!(
            msg.contains("setup nu"),
            "diagnostic must offer reinstall: {msg}"
        );
        assert!(
            PendingMigration::load(root).unwrap().is_some(),
            "manual-recovery path must retain the journal"
        );
    }

    #[test]
    fn migrate_legacy_clears_journal_on_success() {
        // End-to-end invariant: a successful migration's last act is
        // `PendingMigration::delete(root)?`. If that step ever silently fails
        // (or is removed by a future refactor), a stale journal would survive
        // and `numan use` would keep reconciling instead of treating migration
        // as a one-shot. This test pins the post-success journal-clear.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_legacy_binary(root, None);

        let result =
            migrate_legacy_install_with_detector(root, &|_| Ok("0.113.1".to_string()), None)
                .unwrap();
        assert!(result);
        assert!(
            PendingMigration::load(root).unwrap().is_none(),
            "successful migration must clear the journal"
        );
    }
}
