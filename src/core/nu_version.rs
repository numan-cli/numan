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
                if let Ok(min) = parse_version(ver) {
                    if !version_gte(self, &min) {
                        return false;
                    }
                }
            } else if let Some(ver) = part.strip_prefix('>') {
                if let Ok(min) = parse_version(ver) {
                    if !version_gt(self, &min) {
                        return false;
                    }
                }
            } else if let Some(ver) = part.strip_prefix("<=") {
                if let Some(ver) = ver.strip_prefix('=') {
                    // <=0.114.0
                    if let Ok(max) = parse_version(ver) {
                        if !version_lte(self, &max) {
                            return false;
                        }
                    }
                }
            } else if let Some(ver) = part.strip_prefix('<') {
                if let Ok(max) = parse_version(ver) {
                    if !version_lt(self, &max) {
                        return false;
                    }
                }
            } else if let Some(ver) = part.strip_prefix('=') {
                if let Some(ver) = ver.strip_prefix("0.") {
                    // "=0.113.x" format — exact minor
                    if let Ok(minor) = ver.trim_end_matches(".x").parse::<u64>() {
                        if self.minor != minor {
                            return false;
                        }
                    }
                } else if let Ok(exact) = parse_version(ver) {
                    if !version_eq(self, &exact) {
                        return false;
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
}
