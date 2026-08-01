//! Active Nu version management for side-by-side installs.
//!
//! Numan supports multiple Nu versions installed under `<root>/tools/nushell/<version>/`.
//! The "active" version is tracked in `<root>/nu_state/active-version.json` and determines
//! which Nu binary is used for plugin registration, module autoload, and other operations.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
}

/// Read the active Nu version from the marker file.
///
/// Returns `None` if no active version is set (marker doesn't exist or is invalid).
pub fn read_active_version(root: &Path) -> Result<Option<ActiveVersion>> {
    let path = active_version_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read active version from '{}'", path.display()))?;
    let active: ActiveVersion = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse active version from '{}'", path.display()))?;
    Ok(Some(active))
}

/// Write the active Nu version to the marker file.
pub fn write_active_version(root: &Path, version: &str) -> Result<()> {
    let path = active_version_path(root);
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create nu_state directory '{}'", parent.display())
        })?;
    }
    let active = ActiveVersion {
        version: version.to_string(),
    };
    write_json_atomic(&path, &active)
        .with_context(|| format!("Failed to write active version to '{}'", path.display()))?;
    Ok(())
}

/// Directory where versioned Nu installs live.
pub fn versioned_nu_dir(root: &Path) -> PathBuf {
    root.join("tools").join("nushell")
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
/// Returns `None` if no active version is set or the active version's binary doesn't exist.
pub fn active_nu_binary(root: &Path) -> Result<Option<PathBuf>> {
    let Some(active) = read_active_version(root)? else {
        return Ok(None);
    };
    let binary = version_binary(root, &active.version);
    if binary.exists() {
        Ok(Some(binary))
    } else {
        Ok(None)
    }
}

/// List all installed Nu versions (by scanning version directories).
pub fn list_installed_versions(root: &Path) -> Result<Vec<String>> {
    let dir = versioned_nu_dir(root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut versions = Vec::new();
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
    version_binary(root, version).exists()
}

/// Get the latest installed Nu version, or `None` if no versions are installed.
pub fn latest_installed_version(root: &Path) -> Result<Option<String>> {
    let versions = list_installed_versions(root)?;
    Ok(versions.into_iter().next())
}

/// Migrate a legacy single-binary install to the versioned structure.
///
/// If `<root>/tools/nushell/nu` exists but no versioned directories exist,
/// detect its version and move it to `<root>/tools/nushell/<version>/nu`.
///
/// Returns `Ok(true)` if migration occurred, `Ok(false)` otherwise.
pub fn migrate_legacy_install(root: &Path) -> Result<bool> {
    let legacy_binary = if cfg!(windows) {
        root.join("tools").join("nushell").join("nu.exe")
    } else {
        root.join("tools").join("nushell").join("nu")
    };

    // If legacy binary doesn't exist, nothing to migrate.
    if !legacy_binary.exists() {
        return Ok(false);
    }

    // If versioned directories already exist, don't migrate (avoid conflicts).
    let versioned_dir = versioned_nu_dir(root);
    if versioned_dir.exists() {
        for entry in std::fs::read_dir(&versioned_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                // At least one versioned dir exists; skip migration.
                return Ok(false);
            }
        }
    }

    // Detect the version of the legacy binary.
    let output = std::process::Command::new(&legacy_binary)
        .arg("--version")
        .output()
        .with_context(|| {
            format!(
                "Failed to execute legacy Nu binary at '{}'",
                legacy_binary.display()
            )
        })?;

    if !output.status.success() {
        bail!(
            "Legacy Nu binary at '{}' exited with status {}",
            legacy_binary.display(),
            output.status
        );
    }

    let version_output = String::from_utf8_lossy(&output.stdout);
    // Parse version from output like "Nushell 0.113.1 (abc123)" or "0.113.1".
    let version = parse_nu_version_from_output(&version_output)?;

    // Move the legacy binary to the versioned directory.
    let version_dir = version_install_dir(root, &version);
    std::fs::create_dir_all(&version_dir).with_context(|| {
        format!(
            "Failed to create version directory '{}'",
            version_dir.display()
        )
    })?;

    let new_binary = version_binary(root, &version);
    std::fs::rename(&legacy_binary, &new_binary).with_context(|| {
        format!(
            "Failed to move '{}' to '{}'",
            legacy_binary.display(),
            new_binary.display()
        )
    })?;

    // Set as active version.
    write_active_version(root, &version)?;

    // Try to remove the now-empty legacy directory (ignore errors if not empty).
    let _ = std::fs::remove_dir(legacy_binary.parent().unwrap());

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
}
