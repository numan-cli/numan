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

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::version_manager::{
    legacy_managed_binary_with_bin, normalize_version, nu_binary_name, version_binary,
    version_install_dir, versioned_nu_dir, write_active_version,
};
use crate::state::migration_journal::{
    self as migration_journal, MigrationStage, PendingMigration, SCHEMA_VERSION,
};

/// Injectable version-detection seam for [`migrate_legacy_install`].
///
/// Tests inject a closure that returns a fixed version (or an error) so
/// migration can be exercised without spawning a real `nu` process.
pub type LegacyVersionDetector = dyn Fn(&Path) -> Result<String>;

/// Injectable post-create seam for [`migrate_legacy_install_with_detector`].
///
/// Fired AFTER `create_dir_all(<version>/)` succeeds and BEFORE the rename of
/// the legacy binary into `<version>/<bin>`. Tests inject a closure returning
/// an `Err` to simulate the original bug where `rename` cannot cross a device
/// or filesystem boundary (or any other rename-pre failure), leaving the
/// empty versioned subdir behind on disk. A second migration attempt with no
/// hook should clean up that artifact and succeed — the recovery test
/// `migrate_legacy_recovers_from_post_create_hook_failure` proves that.
pub type LegacyPostCreateHook = dyn Fn(&Path) -> Result<()>;

/// Upper bound on how long a hung legacy Nu binary may hold the probe.
/// Migration (and doctor repair) run under the root mutation lock, so an
/// unbounded `Command::output` would stall all other Numan mutations.
const LEGACY_VERSION_DETECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Production detector: prefer the VERSION metadata file written by
/// `install_from_archive`. Fall back to probing `nu --version` when no
/// metadata is present (e.g. a manually placed legacy binary).
///
/// The probe is bounded: a hung user-supplied binary is killed after
/// [`LEGACY_VERSION_DETECT_TIMEOUT`] so the mutation lock cannot be held
/// indefinitely.
pub fn detect_legacy_version(binary: &Path) -> Result<String> {
    if let Some(parent) = binary.parent() {
        let version_file = parent.join("VERSION");
        if version_file.is_file() {
            let content = std::fs::read_to_string(&version_file).with_context(|| {
                format!(
                    "Failed to read VERSION metadata at '{}'",
                    version_file.display()
                )
            })?;
            let version = parse_nu_version_from_output(&content).with_context(|| {
                format!(
                    "VERSION metadata at '{}' did not contain a parseable Nu version",
                    version_file.display()
                )
            })?;
            return Ok(version);
        }
    }

    let mut child = std::process::Command::new(binary)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "Failed to execute legacy Nu binary at '{}'",
                binary.display()
            )
        })?;

    // Drain stdout in a background thread while the polling loop runs.
    // Without this, a verbose binary could fill the pipe buffer and deadlock
    // `try_wait` (the child blocks on write; we block on wait).
    let stdout_pipe = child.stdout.take().expect("stdout was piped");
    let reader_handle = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut r = stdout_pipe;
        let _ = r.read_to_end(&mut buf);
        buf
    });

    let deadline = std::time::Instant::now() + LEGACY_VERSION_DETECT_TIMEOUT;
    loop {
        match child
            .try_wait()
            .with_context(|| format!("Failed to poll legacy Nu binary at '{}'", binary.display()))?
        {
            Some(_) => break,
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!(
                        "Legacy Nu binary at '{}' timed out after {}s while detecting version",
                        binary.display(),
                        LEGACY_VERSION_DETECT_TIMEOUT.as_secs()
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    let status = child.wait().with_context(|| {
        format!(
            "Failed to collect exit status from legacy Nu binary at '{}'",
            binary.display()
        )
    })?;
    let stdout_bytes = reader_handle.join().unwrap_or_default();

    if !status.success() {
        bail!(
            "Legacy Nu binary at '{}' exited with status {}",
            binary.display(),
            status
        );
    }

    parse_nu_version_from_output(&String::from_utf8_lossy(&stdout_bytes))
}

/// Migrate a legacy single-binary install to the versioned structure.
///
/// If `<root>/tools/nushell/nu` exists but no versioned directories exist,
/// detect its version and move it to `<root>/tools/nushell/<version>/nu`.
///
/// Returns `Ok(true)` if migration occurred, `Ok(false)` otherwise.
///
/// # Caller requirements
/// The caller must hold the root mutation lock
/// ([`crate::util::fs_safety::acquire_mutation_lock`]) and have created a
/// pre-mutation snapshot ([`crate::state::snapshot::create_snapshot`]) before
/// calling this function.  Both invariants are enforced at the call sites in
/// `version_manager` and `numan use`; the migration itself does not re-acquire
/// the lock because the lock is already held for the entire `numan use`
/// transaction.
pub fn migrate_legacy_install(root: &Path) -> Result<bool> {
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
///
/// # Caller requirements
/// Same as [`migrate_legacy_install`]: the caller must hold the root mutation
/// lock and have created a pre-mutation snapshot.  See that function's
/// documentation for details.
pub fn migrate_legacy_install_with_detector(
    root: &Path,
    detect: &LegacyVersionDetector,
    post_create: Option<&LegacyPostCreateHook>,
) -> Result<bool> {
    // cubic PR69 UzG: refuse to scan or mutate under a symlinked managed
    // directory. A symlink under `<root>/tools/nushell` could redirect the
    // rename or filesystem-truth cleanup outside `$NUMAN_ROOT` and silently
    // rewrite unrelated user-visible state.
    let legacy_dir = versioned_nu_dir(root);
    if crate::util::fs_safety::is_symlink_or_reparse(&legacy_dir)? {
        anyhow::bail!(
            "Refusing to migrate: managed Nushell directory '{}' is a symlink or reparse point. \
             Run `numan setup nu` from a clean root before migrating.",
            legacy_dir.display(),
        );
    }

    // Self-heal any in-flight migration journal from a prior crashed attempt.
    // The `Prepared` path removes any orphan `<version>/` subdir; the
    // `Renamed` path completes the active-version write. This is what makes
    // the migration a journaled transaction: every stage is recoverable.
    migration_journal::reconcile(root)?;

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
        // Record the first directory that contains foreign (non-binary) files
        // so we can bail after the full scan rather than short-circuiting
        // immediately. This lets us still remove all empty sibling dirs before
        // refusing, giving a more complete clean-up pass.
        let mut offending_path: Option<std::path::PathBuf> = None;
        let bin_name = nu_binary_name();
        for entry in std::fs::read_dir(&versioned_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            // Only treat normalized semver directory names as migration
            // debris. Unrelated user-created subdirs under tools/nushell
            // must stay untouched.
            let dir_name = entry.file_name();
            let Some(name) = dir_name.to_str() else {
                continue;
            };
            if normalize_version(name).is_err() {
                continue;
            }
            if entry.path().join(bin_name).is_file() {
                found_installed = true;
            } else {
                // Empty version subdir — likely from an aborted previous
                // migration. Remove it so the user is not permanently stuck
                // with an empty <version>/ blocking every future attempt.
                // If the directory contains any other file, preserve it and
                // record the offending path; the bail happens after the loop
                // so we still clean up any empty siblings in the same pass.
                if let Some(inner) = std::fs::read_dir(entry.path())?.next() {
                    let _ = inner?;
                    offending_path.get_or_insert_with(|| entry.path());
                    continue;
                }
                std::fs::remove_dir(entry.path()).with_context(|| {
                    format!(
                        "Failed to remove empty version directory '{}'",
                        entry.path().display()
                    )
                })?;
            }
        }
        if found_installed {
            return Ok(false);
        }
        if let Some(foreign) = offending_path {
            bail!(
                "Refusing to migrate: version directory '{}' exists and contains \
                 files other than the Nu binary. Remove or rename it, then retry.",
                foreign.display()
            );
        }
    }

    let version = detect(&legacy_binary).with_context(|| {
        format!(
            "Failed to determine version of legacy Nu binary at '{}'",
            legacy_binary.display()
        )
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
    journal.save(root).with_context(|| {
        format!(
            "Failed to write migration journal (Prepared) before 'create_dir_all' for '{}'",
            version
        )
    })?;

    let version_dir = version_install_dir(root, &version);
    let version_journal_path = PendingMigration::journal_path(root);
    std::fs::create_dir_all(&version_dir).with_context(|| {
        format!(
            "Failed to create version directory '{}' (migration journal at Prepared: '{}')",
            version_dir.display(),
            version_journal_path.display()
        )
    })?;

    // Post-create seam (tests only). Real callers pass `None`; a failing
    // hook here simulates the original cross-device rename bug.
    if let Some(hook) = post_create {
        hook(&version_dir).with_context(|| {
            format!(
                "Post-create hook blocked migration for '{}'",
                version_dir.display()
            )
        })?;
    }

    let new_binary = version_binary(root, &version);
    std::fs::rename(&legacy_binary, &new_binary).with_context(|| {
        format!(
            "Failed to move '{}' to '{}' (migration journal at Prepared: a future reconcile will clean up '<{}>/')",
            legacy_binary.display(),
            new_binary.display(),
            version
        )
    })?;

    // Journal `Renamed` BEFORE `write_active_version`. If we crash here,
    // reconcile completes the active-version write on the next invocation.
    let journal = PendingMigration {
        schema_version: SCHEMA_VERSION,
        version: version.clone(),
        stage: MigrationStage::Renamed,
    };
    journal.save(root).with_context(|| {
        format!(
            "Failed to advance migration journal to 'Renamed' (legacy binary already moved to '{}')",
            new_binary.display()
        )
    })?;

    // Set as active version.
    write_active_version(root, &version).with_context(|| {
        format!(
            "Failed to persist active Nu version marker for '{}' (legacy binary already moved to '{}'; migration journal is at Renamed stage)",
            version,
            new_binary.display()
        )
    })?;

    // Set the journal stage to `Active` and immediately clear it — any
    // `numan use`/`numan doctor` reconcile pass that runs after this will
    // see no journal and be a no-op.
    let journal = PendingMigration {
        schema_version: SCHEMA_VERSION,
        version: version.clone(),
        stage: MigrationStage::Active,
    };
    journal.save(root).with_context(|| {
        format!(
            "Failed to advance migration journal to 'Active' for '{}' (active version is set; clearing journal)",
            version
        )
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
fn parse_nu_version_from_output(output: &str) -> Result<String> {
    let trimmed = output.trim();
    // Strip an optional leading "Nushell " so the rest matches what
    // `NuVersion::parse` already accepts (semver with optional build
    // hash / pre-release suffix; see core/nu_version.rs).
    let body = trimmed
        .strip_prefix("Nushell ")
        .unwrap_or(trimmed)
        .trim_start_matches('v');
    // cubic PR69 UzU: delegate to NuVersion::parse so build-hash
    // suffixes ("0.113.1 (abc123)") and bare semvers both work.
    if let Ok(parsed) = crate::core::nu_version::NuVersion::parse(body) {
        return Ok(parsed.version);
    }
    if semver::Version::parse(body).is_ok() {
        return Ok(body.to_string());
    }
    bail!("Failed to parse Nu version from output: '{}'", trimmed)
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

        let result =
            migrate_legacy_install_with_detector(root, &|_| bail!("simulated probe failure"), None);

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
            Some(&|_| bail!("simulated cross-device rename failure")),
        );
        assert!(
            first.is_err(),
            "first attempt must fail when the post-create hook bails"
        );
        assert!(
            legacy.exists(),
            "legacy binary must be left in place when post-create hook fails"
        );
        let leftover = tools.join("0.113.1");
        assert!(
            leftover.is_dir(),
            "empty versioned subdir must exist (the simulated half-migrated state)"
        );
        assert!(
            std::fs::read_dir(&leftover).unwrap().next().is_none(),
            "subdir must be empty so the cleanup path triggers"
        );
        assert!(
            read_active_version(root).unwrap().is_none(),
            "active marker must not be written on the failed first attempt"
        );
        // The journal must be in Prepared stage after the failed first attempt;
        // the second attempt's reconcile path will use it to clean up.
        let journal = PendingMigration::load(root)
            .unwrap()
            .expect("journal must be present after a failed Prepared-stage migration");
        assert_eq!(
            journal.stage,
            MigrationStage::Prepared,
            "journal stage must be Prepared after failed post-create hook"
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
        // Journal must be cleared after successful migration.
        assert!(
            PendingMigration::load(root).unwrap().is_none(),
            "journal must be cleared after successful recovery migration"
        );
    }

    /// `migrate_legacy_install_with_detector` must refuse when
    /// `<root>/tools/nushell` is a symlink (same guard as `reconcile`).
    #[cfg(unix)]
    #[test]
    fn migrate_legacy_refuses_symlinked_managed_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let bin_name = nu_binary_name();

        // Create a real directory that will be the symlink target.
        let real_dir = tmp.path().join("real_nushell");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(real_dir.join(bin_name), b"fake legacy nu").unwrap();

        // Create tools/nushell as a symlink pointing to real_dir.
        let tools_parent = root.join("tools");
        std::fs::create_dir_all(&tools_parent).unwrap();
        std::os::unix::fs::symlink(&real_dir, tools_parent.join("nushell")).unwrap();

        let err = migrate_legacy_install_with_detector(
            root,
            &|_| panic!("detector must not be called on a symlinked managed dir"),
            None,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("symlink") || msg.contains("reparse"),
            "must mention symlink in error, got: {msg}"
        );
    }

    #[test]
    fn migrate_legacy_refuses_nonempty_version_dir_without_binary() {
        // A normalized version directory that contains a non-binary file (no
        // `nu` / `nu.exe`) must be preserved; migration must stop rather than
        // installing the legacy binary into that nonempty directory.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let bin_name = nu_binary_name();

        let tools = root.join("tools/nushell");
        std::fs::create_dir_all(&tools).unwrap();
        let legacy = tools.join(bin_name);
        std::fs::write(&legacy, b"fake legacy nu").unwrap();

        let version_dir = tools.join("0.113.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        let foreign = version_dir.join("NOTES.txt");
        std::fs::write(&foreign, b"do not clobber").unwrap();

        let err = migrate_legacy_install_with_detector(root, &|_| Ok("0.113.1".to_string()), None)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("contains files other than the Nu binary"),
            "must refuse nonempty foreign version dir, got: {msg}"
        );
        assert!(
            legacy.exists(),
            "legacy binary must be left in place when migration is refused"
        );
        assert!(
            foreign.exists(),
            "foreign file in version dir must be preserved"
        );
        assert!(
            !version_dir.join(bin_name).exists(),
            "must not install the legacy binary into the nonempty version dir"
        );
        assert!(
            read_active_version(root).unwrap().is_none(),
            "active marker must not be written when migration is refused"
        );
    }

    #[test]
    fn migrate_legacy_does_not_clean_populated_subdir_if_post_create_fails() {
        // A populated versioned subdir is a REAL install — never an orphan
        // from an aborted migration. The cleanup loop in
        // `migrate_legacy_install` only removes subdirs that contain no
        // binary; a populated sibling must short-circuit the whole migration
        // (`Ok(false)`) and survive untouched — even when a failing
        // post-create hook is configured (which must never fire because the
        // populated subdir blocks migration at the cleanup scan first).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let bin_name = nu_binary_name();

        let tools = root.join("tools/nushell");
        std::fs::create_dir_all(&tools).unwrap();
        let legacy = tools.join(bin_name);
        std::fs::write(&legacy, b"fake legacy nu").unwrap();

        // Pre-populate the versioned subdir with REAL content — the exact
        // shape the cleanup path must never touch.
        let populated_dir = tools.join("0.113.1");
        std::fs::create_dir_all(&populated_dir).unwrap();
        let populated_bin = populated_dir.join(bin_name);
        std::fs::write(&populated_bin, b"real installed nu").unwrap();

        // Post-create hook configured to fail; it must NOT be reached
        // because the populated subdir short-circuits migration first.
        let result = migrate_legacy_install_with_detector(
            root,
            &|_| Ok("0.113.1".to_string()),
            Some(&|_| bail!("simulated post-create failure")),
        );

        let migrated = result.unwrap();
        assert!(
            !migrated,
            "migration must be declined while a populated versioned install exists"
        );
        assert!(
            populated_dir.is_dir(),
            "populated subdir must be left in place"
        );
        assert!(
            populated_bin.is_file(),
            "populated binary must be left in place"
        );
        assert_eq!(
            std::fs::read(&populated_bin).unwrap(),
            b"real installed nu",
            "populated subdir content must be untouched"
        );
        assert!(
            legacy.exists(),
            "legacy binary must be left in place when migration is declined"
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
