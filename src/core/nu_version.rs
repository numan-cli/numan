use anyhow::{bail, Result};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct NuVersion {
    pub version: String,
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl NuVersion {
    pub fn detect() -> Result<Self> {
        Self::from_binary(Path::new("nu"))
    }

    /// Run `path --version` and parse the Nu version string.
    pub fn from_binary(path: &Path) -> Result<Self> {
        let output = Command::new(path)
            .arg("--version")
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to run '{} --version': {e}", path.display()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("'{} --version' failed: {stderr}", path.display());
        }

        let stdout = String::from_utf8(output.stdout)?;
        let version_str = stdout.trim();

        Self::parse(version_str)
    }

    pub fn parse(version_str: &str) -> Result<Self> {
        // Nu versions are like "0.113.1" or "0.113.1 (hash)"
        let version_part = version_str.split_whitespace().next().unwrap_or(version_str);
        let parts: Vec<&str> = version_part.split('.').collect();

        if parts.len() != 3 {
            bail!("Invalid Nu version format: '{version_str}' (expected X.Y.Z)");
        }

        let major = parts[0]
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("Invalid major version: '{}'", parts[0]))?;
        let minor = parts[1]
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("Invalid minor version: '{}'", parts[1]))?;
        let patch = parts[2]
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("Invalid patch version: '{}'", parts[2]))?;

        Ok(Self {
            version: version_part.to_string(),
            major,
            minor,
            patch,
        })
    }

    /// Load Nu version from cached paths (if present) or detect from PATH.
    /// This is the canonical helper for commands that need Nu version detection.
    pub fn from_paths_or_detect(root: &Path) -> Result<Self> {
        use crate::nu::paths::NuPaths;

        if let Ok(paths) = NuPaths::load(root) {
            if let Ok(nu) = Self::parse(&paths.nu_version) {
                return Ok(nu);
            }
        }
        Self::detect()
    }

    pub fn matches_constraint(&self, constraint: &str) -> bool {
        // Simple constraint matching:
        // ">=0.113.0 <0.114.0" — range
        // ">=0.100.0" — minimum
        // "*" — any
        // "=0.113.x" — exact minor (legacy format)
        if constraint == "*" {
            return true;
        }

        let parts: Vec<&str> = constraint.split_whitespace().collect();
        for part in parts {
            if let Some(ver) = part.strip_prefix(">=") {
                match parse_version(ver) {
                    Ok(min) if version_gte(self, &min) => {}
                    _ => return false,
                }
            } else if let Some(ver) = part.strip_prefix("<=") {
                match parse_version(ver) {
                    Ok(max) if version_lte(self, &max) => {}
                    _ => return false,
                }
            } else if let Some(ver) = part.strip_prefix('>') {
                match parse_version(ver) {
                    Ok(min) if version_gt(self, &min) => {}
                    _ => return false,
                }
            } else if let Some(ver) = part.strip_prefix('<') {
                match parse_version(ver) {
                    Ok(max) if version_lt(self, &max) => {}
                    _ => return false,
                }
            } else if let Some(ver) = part.strip_prefix('=') {
                if let Some(minor_str) = ver.strip_prefix("0.") {
                    // "=0.113.x" format — exact minor
                    match minor_str.trim_end_matches(".x").parse::<u64>() {
                        Ok(minor) if self.minor == minor => {}
                        _ => return false,
                    }
                } else {
                    match parse_version(ver) {
                        Ok(exact) if version_eq(self, &exact) => {}
                        _ => return false,
                    }
                }
            }
        }
        true
    }
}

fn parse_version(v: &str) -> Result<(u64, u64, u64)> {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3 {
        bail!("Invalid version: '{v}'");
    }
    Ok((parts[0].parse()?, parts[1].parse()?, parts[2].parse()?))
}

fn version_gte(a: &NuVersion, b: &(u64, u64, u64)) -> bool {
    (a.major, a.minor, a.patch) >= *b
}

fn version_gt(a: &NuVersion, b: &(u64, u64, u64)) -> bool {
    (a.major, a.minor, a.patch) > *b
}

fn version_lte(a: &NuVersion, b: &(u64, u64, u64)) -> bool {
    (a.major, a.minor, a.patch) <= *b
}

fn version_lt(a: &NuVersion, b: &(u64, u64, u64)) -> bool {
    (a.major, a.minor, a.patch) < *b
}

fn version_eq(a: &NuVersion, b: &(u64, u64, u64)) -> bool {
    (a.major, a.minor, a.patch) == *b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_string() {
        let v = NuVersion::parse("0.113.1").unwrap();
        assert_eq!(v.major, 0);
        assert_eq!(v.minor, 113);
        assert_eq!(v.patch, 1);
    }

    #[test]
    fn parse_version_with_hash() {
        let v = NuVersion::parse("0.113.1 (abc123)").unwrap();
        assert_eq!(v.minor, 113);
    }

    #[test]
    fn matches_wildcard() {
        let v = NuVersion::parse("0.113.1").unwrap();
        assert!(v.matches_constraint("*"));
    }

    #[test]
    fn matches_range() {
        let v = NuVersion::parse("0.113.1").unwrap();
        assert!(v.matches_constraint(">=0.113.0 <0.114.0"));
        assert!(!v.matches_constraint(">=0.114.0 <0.115.0"));
    }

    #[test]
    fn matches_minimum() {
        let v = NuVersion::parse("0.113.1").unwrap();
        assert!(v.matches_constraint(">=0.113.0"));
        assert!(!v.matches_constraint(">=0.114.0"));
    }

    #[test]
    fn matches_exact_minor() {
        let v = NuVersion::parse("0.113.1").unwrap();
        assert!(v.matches_constraint("=0.113.x"));
        assert!(!v.matches_constraint("=0.112.x"));
    }

    #[test]
    fn from_binary_errors_when_executable_missing() {
        let err =
            NuVersion::from_binary(Path::new("/nonexistent/numan-test-nu-binary")).unwrap_err();
        assert!(
            err.to_string().contains("--version"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_rejects_wrong_segment_count() {
        assert!(NuVersion::parse("0.113").is_err());
        assert!(NuVersion::parse("0.113.1.2").is_err());
    }

    #[test]
    fn parse_rejects_non_numeric_segments() {
        assert!(NuVersion::parse("x.113.1").is_err());
        assert!(NuVersion::parse("0.y.1").is_err());
        assert!(NuVersion::parse("0.113.z").is_err());
    }

    #[test]
    fn matches_greater_than() {
        let v = NuVersion::parse("0.113.1").unwrap();
        assert!(v.matches_constraint(">0.113.0"));
        assert!(!v.matches_constraint(">0.113.1"));
    }

    #[test]
    fn matches_less_than() {
        let v = NuVersion::parse("0.113.1").unwrap();
        assert!(v.matches_constraint("<0.114.0"));
        assert!(!v.matches_constraint("<0.113.1"));
    }

    #[test]
    fn matches_exact_full_version() {
        let v = NuVersion::parse("1.2.3").unwrap();
        assert!(v.matches_constraint("=1.2.3"));
        assert!(!v.matches_constraint("=1.2.4"));
    }

    #[test]
    fn matches_constraint_rejects_unparseable_bound() {
        let v = NuVersion::parse("0.113.1").unwrap();
        // Malformed bound fails closed rather than being silently ignored.
        assert!(!v.matches_constraint(">=not-a-version"));
    }

    #[test]
    fn matches_less_than_or_equal() {
        let v = NuVersion::parse("0.113.1").unwrap();
        assert!(v.matches_constraint("<=0.113.1"));
        assert!(v.matches_constraint("<=0.114.0"));
        assert!(!v.matches_constraint("<=0.113.0"));
    }

    #[test]
    fn from_paths_or_detect_uses_cached_version() {
        use crate::nu::paths::NuPaths;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nu_state")).unwrap();
        let paths = NuPaths {
            nu_executable: "/usr/bin/nu".to_string(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: "/tmp/plugin.msgpackz".to_string(),
            nu_executable_hash: "deadbeef".to_string(),
            platform: "x86_64-unknown-linux-gnu".to_string(),
            data_dir: None,
            vendor_autoload_dirs: Vec::new(),
            vendor_autoload_dir: None,
        };
        paths.save(dir.path()).unwrap();

        let version = NuVersion::from_paths_or_detect(dir.path()).unwrap();
        assert_eq!(version.major, 0);
        assert_eq!(version.minor, 113);
        assert_eq!(version.patch, 1);
    }
}
