//! Active Nu version management for side-by-side installs.
//!
//! Numan supports multiple Nu versions installed under `<root>/tools/nushell/<version>/`.
//! The "active" version is tracked in `<root>/nu_state/active-version.json` and determines
//! which Nu binary is used for plugin registration, module autoload, and other operations.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::util::atomic::write_json_atomic;

/// Errors from managed Nu version-marker and install-layout APIs.
///
/// This module is part of the library surface (`pub mod nu`). Callers that need
/// to inspect failures should match on these variants; application handlers may
/// still lift them into `anyhow` with `?` / `.context(...)`.
#[derive(Debug, Error)]
pub enum VersionManagerError {
    #[error("Failed to read active version from '{path}'")]
    ReadMarker {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Malformed active-version.json at '{path}'")]
    MalformedMarker {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("Failed to create nu_state directory '{path}'")]
    CreateStateDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Failed to write active version to '{path}': {message}")]
    WriteMarker { path: String, message: String },
    #[error("Failed to clear active-version marker at '{path}'")]
    ClearMarker {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Invalid Nu version '{version}'; expected X.Y.Z")]
    InvalidVersion { version: String },
    #[error(
        "Refusing to persist active-version marker with `..` in binary path '{path}' \
         (path traversal would let a tampered marker escape the managed tree)."
    )]
    PathTraversal { path: String },
    #[error("Failed to read Nu versions directory '{path}'")]
    ReadVersionsDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Failed to read legacy Nu VERSION file '{path}'")]
    ReadLegacyVersion {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "Active Nu version '{version}' is set but neither the on-tree binary at '{on_tree}' \
         nor the recorded off-tree path '{off_tree}' is present. \
         Run 'numan setup nu' to install the selected version or \
         'numan use <version>' / 'numan use latest' to choose a different one."
    )]
    DanglingActiveWithOffTree {
        version: String,
        on_tree: String,
        off_tree: String,
    },
    #[error(
        "Active Nu version '{version}' is set but the on-tree binary at '{on_tree}' is missing. \
         Run 'numan setup nu' to install the selected version or \
         'numan use <version>' / 'numan use latest' to choose a different one."
    )]
    DanglingActive { version: String, on_tree: String },
}

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
pub fn read_active_version(root: &Path) -> Result<Option<ActiveVersion>, VersionManagerError> {
    let path = active_version_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let content =
        std::fs::read_to_string(&path).map_err(|source| VersionManagerError::ReadMarker {
            path: path.display().to_string(),
            source,
        })?;
    let active: ActiveVersion =
        serde_json::from_str(&content).map_err(|source| VersionManagerError::MalformedMarker {
            path: path.display().to_string(),
            source,
        })?;
    Ok(Some(active))
}

/// Write the active Nu version to the marker file.
pub fn write_active_version(root: &Path, version: &str) -> Result<(), VersionManagerError> {
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
) -> Result<(), VersionManagerError> {
    let normalized = normalize_version(version)?;

    // Refuse any `..` segment: the marker is later read by
    // `find_nu_executable_with_root` and a tampered relative path could
    // be used to anchor an open() outside the user's expected Nu
    // binary location.
    //
    // Absolute paths are intentionally accepted: `numan setup nu use
    // <external-path>` records the user's canonical external Nu at its
    // absolute path. off-tree Nu binaries are a first-class case (the
    // `offtree_*` test fixtures below prove it).
    for component in binary_path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(VersionManagerError::PathTraversal {
                path: binary_path.display().to_string(),
            });
        }
    }

    write_active_marker(
        root,
        &ActiveVersion {
            version: normalized,
            binary_path: Some(binary_path.to_string_lossy().into_owned()),
        },
    )
}

pub(crate) fn write_active_marker(
    root: &Path,
    active: &ActiveVersion,
) -> Result<(), VersionManagerError> {
    let path = active_version_path(root);
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| VersionManagerError::CreateStateDir {
            path: parent.display().to_string(),
            source,
        })?;
    }
    write_json_atomic(&path, active).map_err(|e| VersionManagerError::WriteMarker {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(())
}

/// Remove the active-version marker if it exists.
///
/// Returns `Ok(true)` when a marker was removed, `Ok(false)` when no marker
/// existed. Other I/O errors are propagated with context. Callers that
/// destructively remove the versioned Nu tree should call this first so the
/// marker cannot dangle at a missing binary.
pub fn clear_active_version(root: &Path) -> Result<bool, VersionManagerError> {
    let path = active_version_path(root);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(VersionManagerError::ClearMarker {
            path: path.display().to_string(),
            source,
        }),
    }
}

/// Directory where versioned Nu installs live.
pub fn versioned_nu_dir(root: &Path) -> PathBuf {
    root.join("tools").join("nushell")
}

/// Parse and normalize a Nu version, rejecting path-like values.
pub fn normalize_version(version: &str) -> Result<String, VersionManagerError> {
    let version = version.strip_prefix('v').unwrap_or(version);
    if version.is_empty()
        || version.contains('/')
        || version.contains('\\')
        || version.contains("..")
    {
        return Err(VersionManagerError::InvalidVersion {
            version: version.to_string(),
        });
    }
    let parsed =
        semver::Version::parse(version).map_err(|_| VersionManagerError::InvalidVersion {
            version: version.to_string(),
        })?;
    Ok(parsed.to_string())
}

/// Directory for a specific Nu version install.
pub fn version_install_dir(root: &Path, version: &str) -> PathBuf {
    versioned_nu_dir(root).join(version)
}

/// Binary path for a specific Nu version.
pub fn version_binary(root: &Path, version: &str) -> PathBuf {
    version_install_dir(root, version).join(nu_binary_name())
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
pub fn active_nu_binary(root: &Path) -> Result<Option<PathBuf>, VersionManagerError> {
    let Some(active) = read_active_version(root)? else {
        return Ok(None);
    };
    let version = normalize_version(&active.version)?;

    // Prefer the on-tree version-binary when present. This covers the
    // common case where the off-tree marker was later matched by an
    // on-tree install (e.g. the user ran `setup nu <version>` to give the
    // off-tree selection a versioned home).
    let on_tree = version_binary(root, &version);
    if on_tree.is_file() {
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
        Some(off_tree) => Err(VersionManagerError::DanglingActiveWithOffTree {
            version,
            on_tree: on_tree.display().to_string(),
            off_tree: std::path::PathBuf::from(off_tree).display().to_string(),
        }),
        // pre-migration `nu_state/active-version.json` markers have no off-tree field
        None => Err(VersionManagerError::DanglingActive {
            version,
            on_tree: on_tree.display().to_string(),
        }),
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
pub fn list_installed_versions(root: &Path) -> Result<Vec<String>, VersionManagerError> {
    let dir = versioned_nu_dir(root);
    let mut versions = Vec::new();
    if dir.exists() {
        for entry in
            std::fs::read_dir(&dir).map_err(|source| VersionManagerError::ReadVersionsDir {
                path: dir.display().to_string(),
                source,
            })?
        {
            let entry = entry.map_err(|source| VersionManagerError::ReadVersionsDir {
                path: dir.display().to_string(),
                source,
            })?;
            let file_type =
                entry
                    .file_type()
                    .map_err(|source| VersionManagerError::ReadVersionsDir {
                        path: dir.display().to_string(),
                        source,
                    })?;
            if file_type.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    // Only well-formed versions are installable selections;
                    // a stray directory must not become `numan use latest`.
                    // Also skip directories whose name does not survive
                    // normalization unchanged (e.g. a `v0.113.1` prefix would
                    // map to a different string, producing a phantom entry).
                    let Ok(normalized) = normalize_version(name) else {
                        continue;
                    };
                    if normalized != name {
                        // Directory was created with a non-canonical name (e.g. `v0.113.1`);
                        // skip it so `numan use list` stays clean.
                        continue;
                    }
                    // Check if this directory contains a Nu binary.
                    if entry.path().join(nu_binary_name()).is_file() {
                        versions.push(normalized);
                    }
                }
            }
        }
    }

    // Include the legacy single-binary layout without migrating it: listing
    // must remain read-only. The installer records its version in VERSION.
    // When versioned installs already exist, omit the flat VERSION so
    // `numan use` cannot select a non-resolvable mixed-layout entry
    // (`migrate_legacy_install` returns early once any versioned binary is
    // present, so the flat install is never turned into `<version>/nu`).
    let legacy_binary = dir.join(nu_binary_name());
    let legacy_version_file = dir.join("VERSION");
    if legacy_binary.is_file() && versions.is_empty() {
        match std::fs::read_to_string(&legacy_version_file) {
            Ok(raw) => {
                if let Ok(version) = normalize_version(raw.trim()) {
                    versions.push(version);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(VersionManagerError::ReadLegacyVersion {
                    path: legacy_version_file.display().to_string(),
                    source,
                });
            }
        }
    }

    // Augment with the off-tree marker's version so the user's chosen
    // version appears in `numan use list` even when no on-tree install
    // exists. Only append when the marker names an OFF-TREE selection
    // (binary_path is Some) AND its version is not already in the on-tree
    // scan — we dedupe so a user who runs `setup nu <version>` after an
    // off-tree swap sees a clean list.
    if let Some(marker) = read_active_version(root)? {
        if let Some(off_tree) = marker.binary_path.as_ref() {
            let off_tree_path = std::path::Path::new(off_tree);
            // Normalize the marker's version before dedup/insert so a marker
            // written with a `v`-prefixed version doesn't create a duplicate
            // entry alongside the canonically-named on-tree install.
            if let Ok(normalized_marker) = normalize_version(&marker.version) {
                if off_tree_path.is_file() && !versions.iter().any(|v| v == &normalized_marker) {
                    versions.push(normalized_marker);
                }
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

/// Resolve a version to an existing binary, preferring on-tree installs.
///
/// Propagates [`read_active_version`] errors rather than treating a malformed
/// marker as "not installed".
pub fn resolve_installed_version(
    root: &Path,
    version: &str,
) -> Result<Option<PathBuf>, VersionManagerError> {
    let normalized = normalize_version(version)?;

    let on_tree = version_binary(root, &normalized);
    if on_tree.is_file() {
        return Ok(Some(on_tree));
    }

    if let Some(active) = read_active_version(root)? {
        if active.version == normalized {
            if let Some(off_tree) = active.binary_path.as_ref() {
                let off_tree_path = Path::new(off_tree);
                if off_tree_path.is_file() {
                    return Ok(Some(off_tree_path.to_path_buf()));
                }
            }
        }
    }

    Ok(None)
}

/// Check if a specific Nu version is installed.
pub fn is_version_installed(root: &Path, version: &str) -> Result<bool, VersionManagerError> {
    Ok(resolve_installed_version(root, version)?.is_some())
}

/// Get the latest installed Nu version, or `None` if no versions are installed.
pub fn latest_installed_version(root: &Path) -> Result<Option<String>, VersionManagerError> {
    let versions = list_installed_versions(root)?;
    Ok(versions.into_iter().next())
}

/// Binary name for the managed Nu binary on the current target.
/// Windows: `nu.exe`; posix: `nu`. Used by every versioned-layout
/// helper so the conditional is not scattered across callers.
pub(crate) fn nu_binary_name() -> &'static str {
    if cfg!(windows) {
        "nu.exe"
    } else {
        "nu"
    }
}

/// Legacy single-binary path that older installs wrote:
/// `<root>/tools/nushell/${bin}`.
pub(crate) fn legacy_managed_binary_with_bin(root: &Path, bin: &str) -> PathBuf {
    root.join("tools").join("nushell").join(bin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
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

        assert!(!is_version_installed(root, "0.113.1").unwrap());

        // Create the version directory with a binary.
        let binary_name = if cfg!(windows) { "nu.exe" } else { "nu" };
        let dir = version_install_dir(root, "0.113.1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(binary_name), "fake").unwrap();

        assert!(is_version_installed(root, "0.113.1").unwrap());
        assert!(!is_version_installed(root, "0.114.0").unwrap());
    }

    #[test]
    fn read_active_version_propagates_malformed_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("nu_state")).unwrap();
        std::fs::write(root.join("nu_state/active-version.json"), b"{ not json").unwrap();

        let err = read_active_version(root).unwrap_err();
        assert!(
            matches!(err, VersionManagerError::MalformedMarker { .. }),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains("Malformed active-version.json"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn normalize_version_rejects_path_like_values() {
        let err = normalize_version("../evil").unwrap_err();
        assert!(matches!(
            err,
            VersionManagerError::InvalidVersion { version } if version == "../evil"
        ));
    }

    #[test]
    fn write_active_version_with_binary_rejects_parent_dir_segments() {
        let tmp = TempDir::new().unwrap();
        let err =
            write_active_version_with_binary(tmp.path(), "0.113.1", Path::new("/opt/../escape/nu"))
                .unwrap_err();
        assert!(matches!(err, VersionManagerError::PathTraversal { .. }));
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

    /// Legacy flat binary with a VERSION file: `list_installed_versions` must
    /// return that version when no versioned installs exist.
    #[test]
    fn list_installed_versions_falls_back_to_legacy_version_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let tools = versioned_nu_dir(root);
        std::fs::create_dir_all(&tools).unwrap();
        // Write the legacy flat binary.
        std::fs::write(tools.join(nu_binary_name()), b"fake legacy nu").unwrap();
        // Write the VERSION metadata.
        std::fs::write(tools.join("VERSION"), "0.113.1").unwrap();

        let versions = list_installed_versions(root).unwrap();
        assert_eq!(versions, vec!["0.113.1"]);
    }

    /// Legacy flat binary WITHOUT a VERSION file: `list_installed_versions`
    /// must return an empty list (no version to report, not an error).
    #[test]
    fn list_installed_versions_missing_version_file_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let tools = versioned_nu_dir(root);
        std::fs::create_dir_all(&tools).unwrap();
        // Write the legacy flat binary without any VERSION file.
        std::fs::write(tools.join(nu_binary_name()), b"fake legacy nu").unwrap();

        let versions = list_installed_versions(root).unwrap();
        assert!(
            versions.is_empty(),
            "no version metadata → empty list, got: {versions:?}"
        );
    }

    /// When at least one versioned install exists, the legacy flat binary must
    /// NOT contribute an additional entry (mixed-layout must not be selectable
    /// via `numan use`).
    #[test]
    fn list_installed_versions_omits_legacy_when_versioned_exists() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let tools = versioned_nu_dir(root);
        std::fs::create_dir_all(&tools).unwrap();

        // Legacy flat binary with version file.
        std::fs::write(tools.join(nu_binary_name()), b"legacy nu").unwrap();
        std::fs::write(tools.join("VERSION"), "0.110.0").unwrap();

        // A real versioned install alongside it.
        let versioned = version_install_dir(root, "0.113.1");
        std::fs::create_dir_all(&versioned).unwrap();
        std::fs::write(versioned.join(nu_binary_name()), b"versioned nu").unwrap();

        let versions = list_installed_versions(root).unwrap();
        assert_eq!(
            versions,
            vec!["0.113.1"],
            "legacy entry must not appear when versioned install exists"
        );
    }

    /// A versioned directory whose name carries a `v`-prefix (e.g. `v0.113.1`)
    /// must be skipped because its name does not survive normalization
    /// unchanged.
    #[test]
    fn list_installed_versions_skips_v_prefixed_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let tools = versioned_nu_dir(root);

        // Create a `v`-prefixed directory — must be skipped.
        let v_dir = tools.join("v0.113.1");
        std::fs::create_dir_all(&v_dir).unwrap();
        std::fs::write(v_dir.join(nu_binary_name()), b"fake nu").unwrap();

        // Also create a canonical directory — must be included.
        let ok_dir = version_install_dir(root, "0.114.0");
        std::fs::create_dir_all(&ok_dir).unwrap();
        std::fs::write(ok_dir.join(nu_binary_name()), b"fake nu").unwrap();

        let versions = list_installed_versions(root).unwrap();
        assert_eq!(
            versions,
            vec!["0.114.0"],
            "`v`-prefixed directory must be skipped, got: {versions:?}"
        );
    }
}
