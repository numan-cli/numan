//! Active Nu version management for side-by-side installs.
//!
//! Numan supports multiple Nu versions installed under `<root>/tools/nushell/<version>/`.
//! The "active" version is tracked in `<root>/nu_state/active-version.json` and determines
//! which Nu binary is used for plugin registration, module autoload, and other operations.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::state::migration_journal::{
    self as migration_journal, MigrationStage, PendingMigration, SCHEMA_VERSION,
};
use crate::util::atomic::write_json_atomic;

/// Active version marker file location.
fn active_version_path(root: &Path) -> PathBuf {
    root.join("nu_state").join("active-version.json")
}

/// Active version marker contents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveVersion {
    /// The currently active Nu version (e.g., "0.113.1").
    pub version: String,
    /// Canonical path to the binary when the active Nu is off-tree
    /// (`<root>/tools/nushell/`) rather than installed under
    /// `<root>/tools/nushell/<version>/nu`. Recorded when
    /// `numan setup nu use <path>` switches to a user-supplied Nu so
    /// subsequent `numan use list` and `find_nu_executable_with_root`
    /// can resolve the user's chosen version even though it lives
    /// outside the versioned layout. `None` for on-tree selections
    /// (the on-tree binary path is derived from `version`).
    ///
    /// Schema note: the field is `skip_serializing_if = "Option::is_none"`,
    /// so on-tree markers continue to serialize as just `{"version": "x.y.z"}`
    /// and pre-existing markers deserialize unchanged with `binary_path = None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
}

/// Read the active Nu version from the marker file.
///
/// Returns `None` only when the marker file is absent. A present but malformed
/// marker is propagated as a contextual error so callers can distinguish
/// "no selection" from "broken selection" — `numan doctor` relies on the
/// distinction to report dangling active-version state.
pub fn read_active_version(root: &Path) -> Result<Option<ActiveVersion>> {
    let path = active_version_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read active version from '{}'", path.display()))?;
    let active: ActiveVersion = serde_json::from_str(&content)
        .with_context(|| format!("Malformed active-version.json at '{}'", path.display()))?;
    Ok(Some(active))
}

/// Write the active Nu version to the marker file.
pub fn write_active_version(root: &Path, version: &str) -> Result<()> {
    let normalized = normalize_version(version)?;
    write_active_marker(
        root,
        &ActiveVersion {
            version: normalized,
            binary_path: None,
        },
    )
}

/// Write the active Nu version with the resolved off-tree binary path.
///
/// Used when `numan setup nu use <path>` switches to a user-supplied Nu
/// outside the versioned layout. The marker then carries both the detected
/// version (so `list_installed_versions` and `find_nu_executable_with_root`
/// can surface it) and the resolved binary path (so subsequent lookups can
/// fall through to the off-tree location when the on-tree version-binary is
/// absent or fails validation).
pub fn write_active_version_with_binary(
    root: &Path,
    version: &str,
    binary_path: &Path,
) -> Result<()> {
    let normalized = normalize_version(version)?;
    write_active_marker(
        root,
        &ActiveVersion {
            version: normalized,
            binary_path: Some(binary_path.to_string_lossy().into_owned()),
        },
    )
}

fn write_active_marker(root: &Path, active: &ActiveVersion) -> Result<()> {
    let path = active_version_path(root);
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create nu_state directory '{}'", parent.display())
        })?;
    }
    write_json_atomic(&path, active)
        .with_context(|| format!("Failed to write active version to '{}'", path.display()))?;
    Ok(())
}

/// Remove the active-version marker if it exists.
///
/// Returns `Ok(true)` when a marker was removed, `Ok(false)` when no marker
/// existed. Other I/O errors are propagated with context. Callers that
/// destructively remove the versioned Nu tree should call this first so the
/// marker cannot dangle at a missing binary.
pub fn clear_active_version(root: &Path) -> Result<bool> {
    let path = active_version_path(root);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| {
            format!(
                "Failed to clear active-version marker at '{}'",
                path.display()
            )
        }),
    }
}

/// Directory where versioned Nu installs live.
pub fn versioned_nu_dir(root: &Path) -> PathBuf {
    root.join("tools").join("nushell")
}

/// Parse and normalize a Nu version, rejecting path-like values.
pub fn normalize_version(version: &str) -> Result<String> {
    let version = version.strip_prefix('v').unwrap_or(version);
    if version.is_empty()
        || version.contains('/')
        || version.contains('\\')
        || version.contains("..")
    {
        bail!("Invalid Nu version '{}'; expected X.Y.Z", version)
    }
    let parsed = semver::Version::parse(version)
        .with_context(|| format!("Invalid Nu version '{}'; expected X.Y.Z", version))?;
    Ok(parsed.to_string())
}

/// Directory for a specific Nu version install.
pub fn version_install_dir(root: &Path, version: &str) -> PathBuf {
    versioned_nu_dir(root).join(version)
}

/// Binary path for a specific Nu version.
pub fn version_binary(root: &Path, version: &str) -> PathBuf {
    let binary_name = if cfg!(windows) { "nu.exe" } else { "nu" };
    version_install_dir(root, version).join(binary_name)
}

/// Binary path for the currently active Nu version.
///
/// Returns:
/// - `Ok(None)` when no active version is set (clean absence).
/// - `Err` when the active marker names a version whose binary is missing
///   on-tree AND either no off-tree path is recorded or the recorded
///   off-tree path is also missing (dangling state) — the caller is
///   expected to surface this so `numan doctor` reports the broken
///   active-version state instead of silently falling back to the legacy
///   binary.
/// - `Ok(Some(path))` for either:
///     1. The on-tree `<root>/tools/nushell/<version>/nu`, preferred.
///     2. The off-tree `binary_path` recorded by an earlier
///        `numan setup nu use <path>` swap (when the on-tree version-binary
///        is absent and the recorded binary path still resolves).
pub fn active_nu_binary(root: &Path) -> Result<Option<PathBuf>> {
    let Some(active) = read_active_version(root)? else {
        return Ok(None);
    };
    let version = normalize_version(&active.version)?;

    // Prefer the on-tree version-binary when present. This covers the
    // common case where the off-tree marker was later matched by an
    // on-tree install (e.g. the user ran `setup nu <version>` to give the
    // off-tree selection a versioned home).
    let on_tree = version_binary(root, &version);
    if on_tree.exists() {
        return Ok(Some(on_tree));
    }

    // Fall back to the recorded off-tree path when one was stored.
    if let Some(off_tree) = active.binary_path.as_ref() {
        let off_tree_path = std::path::PathBuf::from(off_tree);
        if off_tree_path.is_file() {
            return Ok(Some(off_tree_path));
        }
    }

    // Build the message conditionally: skip the off-tree clause when no
    // off-tree path is recorded. The literal "<none>" placeholder previously
    // rendered here was clunky in `numan doctor` output.
    match active.binary_path.as_ref() {
        Some(off_tree) => Err(anyhow::anyhow!(
            "Active Nu version '{}' is set but neither the on-tree binary at '{}' \
             nor the recorded off-tree path '{}' is present. \
             Run 'numan setup nu' to install the selected version or \
             'numan use <version>' / 'numan use latest' to choose a different one.",
            version,
            on_tree.display(),
            std::path::PathBuf::from(off_tree).display(),
        )),
        None => Err(anyhow::anyhow!(
            // pre-migration `nu_state/active-version.json` markers have no off-tree field
            "Active Nu version '{}' is set but the on-tree binary at '{}' is missing. \
             Run 'numan setup nu' to install the selected version or \
             'numan use <version>' / 'numan use latest' to choose a different one.",
            version,
            on_tree.display(),
        )),
    }
}

/// List all installed Nu versions (by scanning version directories).
///
/// The returned vec is augmented with the active version when the marker
/// records an off-tree selection so `numan use list` still shows the
/// user's chosen version even when the versioned layout is empty:
///
///   * on-tree scan picks up `<root>/tools/nushell/<v>/nu` directories.
///   * `read_active_version` is then consulted; if the marker names a
///     version whose off-tree path exists but whose on-tree version-binary
///     is absent, the marker version is appended and deduped.
///
/// The vec is sorted semver-descending (newest first), same as before.
pub fn list_installed_versions(root: &Path) -> Result<Vec<String>> {
    let dir = versioned_nu_dir(root);
    let mut versions = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("Failed to read Nu versions directory '{}'", dir.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    // Check if this directory contains a Nu binary.
                    let binary_name = if cfg!(windows) { "nu.exe" } else { "nu" };
                    if entry.path().join(binary_name).exists() {
                        versions.push(name.to_string());
                    }
                }
            }
        }
    }

    // Augment with the off-tree marker's version so the user's chosen
    // version appears in `numan use list` even when no on-tree install
    // exists. Only append when the marker names an OFF-TREE selection
    // (binary_path is Some) AND its version is not already in the on-tree
    // scan — we dedupe so a user who runs `setup nu <version>` after an
    // off-tree swap sees a clean list.
    if let Ok(Some(marker)) = read_active_version(root) {
        if let Some(off_tree) = marker.binary_path.as_ref() {
            let off_tree_path = std::path::Path::new(off_tree);
            if off_tree_path.is_file() && !versions.iter().any(|v| v == &marker.version) {
                versions.push(marker.version.clone());
            }
        }
    }

    // Sort by semver descending (newest first).
    versions.sort_by(|a, b| {
        let a_parsed = semver::Version::parse(a);
        let b_parsed = semver::Version::parse(b);
        match (a_parsed, b_parsed) {
            (Ok(a_ver), Ok(b_ver)) => b_ver.cmp(&a_ver),
            _ => b.cmp(a),
        }
    });
    Ok(versions)
}

/// Check if a specific Nu version is installed.
pub fn is_version_installed(root: &Path, version: &str) -> bool {
    normalize_version(version)
        .map(|version| version_binary(root, &version).exists())
        .unwrap_or(false)
}

/// Get the latest installed Nu version, or `None` if no versions are installed.
pub fn latest_installed_version(root: &Path) -> Result<Option<String>> {
    let versions = list_installed_versions(root)?;
    Ok(versions.into_iter().next())
}

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

/// Production detector: prefer the VERSION metadata file written by
/// `install_from_archive`. Fall back to probing `nu --version` when no
/// metadata is present (e.g. a manually placed legacy binary).
///
/// We avoid an unbounded `Command::output` probe when metadata exists; the
/// probe is best-effort and depends on the Nu process terminating quickly.
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

    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .with_context(|| {
            format!(
                "Failed to execute legacy Nu binary at '{}'",
                binary.display()
            )
        })?;

    if !output.status.success() {
        bail!(
            "Legacy Nu binary at '{}' exited with status {}",
            binary.display(),
            output.status
        );
    }

    parse_nu_version_from_output(&String::from_utf8_lossy(&output.stdout))
}

/// Migrate a legacy single-binary install to the versioned structure.
///
/// If `<root>/tools/nushell/nu` exists but no versioned directories exist,
/// detect its version and move it to `<root>/tools/nushell/<version>/nu`.
///
/// Returns `Ok(true)` if migration occurred, `Ok(false)` otherwise.
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
pub fn migrate_legacy_install_with_detector(
    root: &Path,
    detect: &LegacyVersionDetector,
    post_create: Option<&LegacyPostCreateHook>,
) -> Result<bool> {
    // Self-heal any in-flight migration journal from a prior crashed attempt.
    // The `Prepared` path removes any orphan `<version>/` subdir; the
    // `Renamed` path completes the active-version write. This is what makes
    // the migration a journaled transaction: every stage is recoverable.
    migration_journal::reconcile(root)?;

    let legacy_binary = if cfg!(windows) {
        root.join("tools").join("nushell").join("nu.exe")
    } else {
        root.join("tools").join("nushell").join("nu")
    };

    // If legacy binary doesn't exist, nothing to migrate.
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
        let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };
        for entry in std::fs::read_dir(&versioned_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
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
    write_active_version(root, &version)?;

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

    // Try to remove the now-empty legacy directory (ignore errors if not empty).
    if let Some(parent) = legacy_binary.parent() {
        let _ = std::fs::remove_dir(parent);
    }

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
    // Try to extract version from "Nushell X.Y.Z" format.
    if let Some(rest) = trimmed.strip_prefix("Nushell ") {
        // Take the first whitespace-delimited token.
        if let Some(version) = rest.split_whitespace().next() {
            // Validate it looks like a semver.
            if semver::Version::parse(version).is_ok() {
                return Ok(version.to_string());
            }
        }
    }
    // Try parsing the whole thing as a semver.
    if semver::Version::parse(trimmed).is_ok() {
        return Ok(trimmed.to_string());
    }
    bail!("Failed to parse Nu version from output: '{}'", trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    #[test]
    fn test_active_version_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Initially no active version.
        assert!(read_active_version(root).unwrap().is_none());

        // Write and read back.
        write_active_version(root, "0.113.1").unwrap();
        let active = read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");

        // Overwrite.
        write_active_version(root, "0.114.0").unwrap();
        let active = read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.114.0");
    }

    #[test]
    fn test_list_installed_versions_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let versions = list_installed_versions(root).unwrap();
        assert!(versions.is_empty());
    }

    #[test]
    fn test_list_installed_versions_with_versions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create versioned directories with fake binaries.
        let binary_name = if cfg!(windows) { "nu.exe" } else { "nu" };
        for version in &["0.112.0", "0.113.1", "0.114.0"] {
            let dir = version_install_dir(root, version);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(binary_name), "fake").unwrap();
        }

        let versions = list_installed_versions(root).unwrap();
        // Should be sorted descending.
        assert_eq!(versions, vec!["0.114.0", "0.113.1", "0.112.0"]);
    }

    #[test]
    fn test_is_version_installed() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        assert!(!is_version_installed(root, "0.113.1"));

        // Create the version directory with a binary.
        let binary_name = if cfg!(windows) { "nu.exe" } else { "nu" };
        let dir = version_install_dir(root, "0.113.1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(binary_name), "fake").unwrap();

        assert!(is_version_installed(root, "0.113.1"));
        assert!(!is_version_installed(root, "0.114.0"));
    }

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
        let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };
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
        let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };
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
        let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };

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
        let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };

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
        let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };

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
        let bin_name = if cfg!(windows) { "nu.exe" } else { "nu" };
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
    fn read_active_version_propagates_malformed_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        std::fs::write(root.join("nu_state/active-version.json"), b"{ not json").unwrap();

        let err = read_active_version(root).unwrap_err();
        assert!(
            err.to_string().contains("Malformed active-version.json"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn active_nu_binary_returns_dangling_state_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Active marker points at a version whose binary directory is missing.
        write_active_version(root, "0.113.1").unwrap();

        let err = active_nu_binary(root).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("0.113.1"), "should name the version: {msg}");
        assert!(msg.contains("missing"), "should describe dangling: {msg}");
    }

    #[test]
    fn active_nu_binary_returns_none_when_no_marker() {
        let tmp = TempDir::new().unwrap();
        // No active marker at all — clean absence, not an error.
        assert!(active_nu_binary(tmp.path()).unwrap().is_none());
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

    // --- off-tree active-marker bridge tests ---

    #[test]
    fn write_active_version_with_binary_round_trips() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_active_version_with_binary(root, "0.113.1", Path::new("/opt/my-nu/nu")).unwrap();

        let active = read_active_version(root)
            .unwrap()
            .expect("marker should be present");
        assert_eq!(active.version, "0.113.1");
        assert_eq!(active.binary_path.as_deref(), Some("/opt/my-nu/nu"));
    }

    #[test]
    fn old_marker_without_binary_path_still_loads() {
        // Backward-compat: a marker written by a previous version (no
        // `binary_path` field) must deserialize cleanly with binary_path = None.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let marker_dir = root.join("nu_state");
        fs::create_dir_all(&marker_dir).unwrap();
        fs::write(
            marker_dir.join("active-version.json"),
            b"{\"version\":\"0.113.1\"}",
        )
        .unwrap();

        let active = read_active_version(root).unwrap().unwrap();
        assert_eq!(active.version, "0.113.1");
        assert!(active.binary_path.is_none());
    }

    #[test]
    fn write_active_version_on_tree_omits_binary_path() {
        // The on-tree variant must NOT serialize a `binary_path` field,
        // preserving the existing on-disk shape for callers that grep the
        // marker file.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_active_version(root, "0.113.1").unwrap();

        let content =
            fs::read_to_string(root.join("nu_state").join("active-version.json")).unwrap();
        assert!(!content.contains("binary_path"));
        assert!(content.contains("0.113.1"));
    }

    #[test]
    fn active_nu_binary_offtree_when_ontree_missing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let off_tree = tmp.path().join("opt-nu");
        fs::create_dir_all(&off_tree).unwrap();
        let off_tree_binary = off_tree.join("nu");
        fs::write(&off_tree_binary, b"placeholder").unwrap();

        write_active_version_with_binary(root, "0.113.1", &off_tree_binary).unwrap();

        // No on-tree install, but the off-tree binary exists, so the marker
        // resolves to it.
        let resolved = active_nu_binary(root).unwrap().unwrap();
        assert_eq!(
            fs::canonicalize(&resolved).unwrap(),
            fs::canonicalize(&off_tree_binary).unwrap()
        );
    }

    #[test]
    fn active_nu_binary_prefers_ontree_when_both_present() {
        // The on-tree version-binary wins over a recorded off-tree path even
        // when both exist, mirroring the consult-order in
        // `find_nu_executable_with_root`.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create the on-tree version-binary as a placeholder file.
        let on_tree = version_binary(root, "0.113.1");
        fs::create_dir_all(on_tree.parent().unwrap()).unwrap();
        fs::write(&on_tree, b"on-tree placeholder").unwrap();

        // And a different off-tree binary file.
        let off_tree = tmp.path().join("alt-bin").join("nu");
        fs::create_dir_all(off_tree.parent().unwrap()).unwrap();
        fs::write(&off_tree, b"off-tree placeholder").unwrap();

        write_active_version_with_binary(root, "0.113.1", &off_tree).unwrap();

        let resolved = active_nu_binary(root).unwrap().unwrap();
        assert_eq!(
            fs::canonicalize(&resolved).unwrap(),
            fs::canonicalize(&on_tree).unwrap()
        );
    }

    #[test]
    fn active_nu_binary_returns_err_when_both_record_paths_missing() {
        // Dangling semantics preserved: when neither the on-tree version-binary
        // nor the recorded off-tree binary exists, return Err so
        // `numan doctor` reports the broken state.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_active_version_with_binary(root, "0.113.1", Path::new("/nonexistent/nu")).unwrap();

        let err = active_nu_binary(root).unwrap_err();
        assert!(err.to_string().contains("0.113.1"));
        assert!(err.to_string().contains("nonexistent/nu"));
    }

    #[test]
    fn list_installed_versions_empty_when_nothing_installed_and_no_marker() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let versions = list_installed_versions(root).unwrap();
        assert!(versions.is_empty());
    }

    #[test]
    fn list_installed_versions_augments_offtree_when_no_ontree() {
        // The headline bridge: only the marker is present, with an off-tree
        // binary on disk. `numan use list` must show that version.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let off_tree = tmp.path().join("opt-nu").join("nu");
        fs::create_dir_all(off_tree.parent().unwrap()).unwrap();
        fs::write(&off_tree, b"placeholder").unwrap();

        write_active_version_with_binary(root, "0.113.1", &off_tree).unwrap();

        let versions = list_installed_versions(root).unwrap();
        assert_eq!(versions, vec!["0.113.1".to_string()]);
    }

    #[test]
    fn list_installed_versions_dedupes_when_both_present() {
        // Writing an on-tree install matching the marker's version must NOT
        // double-list the version.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let on_tree = version_binary(root, "0.113.1");
        fs::create_dir_all(on_tree.parent().unwrap()).unwrap();
        fs::write(&on_tree, b"on-tree placeholder").unwrap();

        let off_tree = tmp.path().join("opt-nu").join("nu");
        fs::create_dir_all(off_tree.parent().unwrap()).unwrap();
        fs::write(&off_tree, b"off-tree placeholder").unwrap();

        write_active_version_with_binary(root, "0.113.1", &off_tree).unwrap();

        let versions = list_installed_versions(root).unwrap();
        assert_eq!(versions, vec!["0.113.1".to_string()]);
    }

    #[test]
    fn list_installed_versions_does_not_augment_when_offtree_binary_missing() {
        // Failure mode: the marker names an off-tree path but the file is no
        // longer present. Bridge must NOT silently re-add an unusable version
        // to the list; doctor catches the dangling state independently.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_active_version_with_binary(root, "0.113.1", Path::new("/nonexistent/nu")).unwrap();

        let versions = list_installed_versions(root).unwrap();
        assert!(versions.is_empty());
    }
}
