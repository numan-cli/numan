use anyhow::{bail, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// Registry owner slug for numan-maintained forks (ADR 0001).
pub const NUAN_MAINTAINED_OWNER: &str = "numan-maintained";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopedId {
    pub owner: String,
    pub name: String,
}

impl ScopedId {
    pub fn new(owner: &str, name: &str) -> Self {
        Self {
            owner: owner.to_string(),
            name: name.to_string(),
        }
    }

    pub fn parse(input: &str) -> Result<Self> {
        let parts: Vec<&str> = input.split('/').collect();
        if parts.len() == 2 {
            let owner = parts[0];
            let name = parts[1];
            if owner.is_empty() || name.is_empty() {
                bail!("Invalid scoped ID: '{input}' (owner and name must be non-empty)");
            }
            Ok(Self::new(owner, name))
        } else if parts.len() == 1 {
            bail!("Invalid scoped ID: '{input}' (must be owner/name)");
        } else {
            bail!("Invalid scoped ID: '{input}' (too many slashes)");
        }
    }

    pub fn alias(owner: &str, name: &str) -> Self {
        Self::new(owner, name)
    }
}

impl fmt::Display for ScopedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

impl FromStr for ScopedId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageType {
    Plugin,
    Module,
    Script,
    Completion,
}

impl PackageType {
    pub fn dir_name(&self) -> &str {
        match self {
            PackageType::Plugin => "plugins",
            PackageType::Module => "modules",
            PackageType::Script => "scripts",
            PackageType::Completion => "completions",
        }
    }
}

impl fmt::Display for PackageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageType::Plugin => write!(f, "plugin"),
            PackageType::Module => write!(f, "module"),
            PackageType::Script => write!(f, "script"),
            PackageType::Completion => write!(f, "completion"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub id: ScopedId,
    pub description: String,
    pub repo: String,
    #[serde(rename = "type")]
    pub package_type: PackageType,
    #[serde(default)]
    pub tags: Vec<String>,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionEntry {
    pub version: Version,
    pub nu_version: String,
    #[serde(default)]
    pub verified_with: Vec<String>,
    pub artifact: Artifact,
    #[serde(default)]
    pub source: Option<SourceInfo>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Optional activation metadata for module packages.
    /// `None` for plugins, scripts, and completions.
    #[serde(default)]
    pub activation: Option<RegistryActivationSpec>,
    /// Optional provenance marker, e.g. `"commit-snapshot"` for versions
    /// built from a pinned commit with no upstream tag.
    #[serde(default)]
    pub provenance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub kind: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub targets: std::collections::HashMap<String, TargetArtifact>,
    #[serde(default)]
    pub archive_root: Option<String>,
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub entry: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetArtifact {
    pub url: String,
    pub sha256: String,
    pub executable_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub git: String,
    pub rev: String,
    pub cargo_name: String,
    #[serde(default)]
    pub cargo_lock_sha256: Option<String>,
    /// Original upstream repo URL. Set only when this version is a
    /// numan-maintained fork (`git`/`rev` above point at the fork).
    #[serde(default)]
    pub upstream: Option<String>,
}

/// How a module's exported symbols are imported into the Nu namespace.
///
/// Corresponds to `activation.import` in the registry entry.
/// Defaults to `Module` (namespaced) when the field is omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModuleImportMode {
    /// `use <path>` — symbols are namespaced under the module name.
    #[default]
    Module,
    /// `use <path> *` — all exported symbols are imported without a prefix.
    All,
}

impl fmt::Display for ModuleImportMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModuleImportMode::Module => write!(f, "module"),
            ModuleImportMode::All => write!(f, "all"),
        }
    }
}

/// Registry metadata describing how a module package should be activated.
///
/// Stored under `activation` in a `VersionEntry`. Only `kind = "nu-module"`
/// is recognized in Phase 4; all other kinds are rejected at activation time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryActivationSpec {
    /// Must be `"nu-module"` for Nushell module packages.
    pub kind: String,
    /// Import mode for the module. Defaults to `ModuleImportMode::Module`.
    #[serde(default)]
    pub import: ModuleImportMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndex {
    #[serde(rename = "schema_version", alias = "version")]
    pub schema_version: u32,
    pub updated_at: String,
    #[serde(default)]
    pub registry_revision: Option<String>,
    #[serde(default)]
    pub trust: Option<crate::core::official_registry::RegistryTrustExtension>,
    pub packages: Vec<Package>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scoped_id() {
        let id = ScopedId::parse("fdncred/file").unwrap();
        assert_eq!(id.owner, "fdncred");
        assert_eq!(id.name, "file");
        assert_eq!(id.to_string(), "fdncred/file");
    }

    #[test]
    fn parse_scoped_id_no_slash() {
        assert!(ScopedId::parse("file").is_err());
    }

    #[test]
    fn parse_scoped_id_empty_owner() {
        assert!(ScopedId::parse("/file").is_err());
    }

    #[test]
    fn parse_scoped_id_empty_name() {
        assert!(ScopedId::parse("fdncred/").is_err());
    }

    #[test]
    fn parse_scoped_id_too_many_slashes() {
        assert!(ScopedId::parse("a/b/c").is_err());
    }

    #[test]
    fn scoped_id_from_str() {
        let id: ScopedId = "owner/name".parse().unwrap();
        assert_eq!(id.owner, "owner");
        assert_eq!(id.name, "name");
    }

    #[test]
    fn package_type_dir_names() {
        assert_eq!(PackageType::Plugin.dir_name(), "plugins");
        assert_eq!(PackageType::Module.dir_name(), "modules");
        assert_eq!(PackageType::Script.dir_name(), "scripts");
        assert_eq!(PackageType::Completion.dir_name(), "completions");
    }

    #[test]
    fn parse_version_entry() {
        let json = r#"{
            "version": "0.25.2",
            "nu_version": ">=0.113.0 <0.114.0",
            "verified_with": ["0.113.0", "0.113.1"],
            "artifact": {
                "kind": "binary",
                "targets": {
                    "x86_64-pc-windows-msvc": {
                        "url": "https://example.com/plugin.zip",
                        "sha256": "abc123",
                        "executable_path": "nu_plugin_file.exe"
                    }
                }
            }
        }"#;
        let entry: VersionEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.version, Version::new(0, 25, 2));
        assert_eq!(entry.artifact.kind, "binary");
        assert!(entry
            .artifact
            .targets
            .contains_key("x86_64-pc-windows-msvc"));
        assert!(entry.source.is_none());
    }

    #[test]
    fn parse_version_entry_defaults_provenance_to_none() {
        let json = r#"{
            "version": "0.25.2",
            "nu_version": ">=0.113.0 <0.114.0",
            "artifact": { "kind": "binary", "targets": {} }
        }"#;
        let entry: VersionEntry = serde_json::from_str(json).unwrap();
        assert!(entry.provenance.is_none());
    }

    #[test]
    fn parse_version_entry_with_commit_snapshot_provenance() {
        let json = r#"{
            "version": "0.0.0-snapshot.20260809.5a1ca2a",
            "nu_version": ">=0.114.0 <0.115.0",
            "provenance": "commit-snapshot",
            "artifact": { "kind": "binary", "targets": {} }
        }"#;
        let entry: VersionEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.provenance.as_deref(), Some("commit-snapshot"));
    }

    #[test]
    fn parse_version_entry_with_source() {
        let json = r#"{
            "version": "1.4.15",
            "nu_version": ">=0.113.0 <0.114.0",
            "verified_with": ["0.113.1"],
            "source": {
                "git": "https://github.com/cptpiepmatz/nu-plugin-highlight",
                "rev": "v1.4.15+0.113.1",
                "cargo_name": "nu_plugin_highlight"
            },
            "artifact": {
                "kind": "binary",
                "targets": {
                    "x86_64-unknown-linux-gnu": {
                        "url": "https://example.com/p.tar.gz",
                        "sha256": "abc123",
                        "executable_path": "nu_plugin_highlight"
                    }
                }
            }
        }"#;
        let entry: VersionEntry = serde_json::from_str(json).unwrap();
        let source = entry.source.expect("source present");
        assert_eq!(
            source.git,
            "https://github.com/cptpiepmatz/nu-plugin-highlight"
        );
        assert_eq!(source.rev, "v1.4.15+0.113.1");
        assert_eq!(source.cargo_name, "nu_plugin_highlight");
        assert!(source.cargo_lock_sha256.is_none());
    }

    #[test]
    fn parse_registry_index() {
        let json = r#"{
            "schema_version": 1,
            "updated_at": "2026-06-27T00:00:00Z",
            "registry_revision": "abc123",
            "packages": []
        }"#;
        let index: RegistryIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.schema_version, 1);
        assert!(index.packages.is_empty());
    }

    #[test]
    fn parse_registry_index_legacy_version_field() {
        let json = r#"{
            "version": 1,
            "updated_at": "2026-06-27T00:00:00Z",
            "packages": []
        }"#;
        let index: RegistryIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.schema_version, 1);
    }

    #[test]
    fn parse_registry_index_lowercase_package_type() {
        let json = r#"{
            "schema_version": 1,
            "updated_at": "2026-06-27T00:00:00Z",
            "packages": [{
                "id": { "owner": "o", "name": "n" },
                "description": "d",
                "repo": "https://example.com",
                "type": "module",
                "tags": [],
                "versions": [{
                    "version": "1.0.0",
                    "nu_version": ">=0.113.0",
                    "artifact": {
                        "kind": "archive",
                        "url": "https://example.com/pkg.zip",
                        "sha256": "abc",
                        "entry": "mod.nu"
                    }
                }]
            }]
        }"#;
        let index: RegistryIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.packages[0].package_type, PackageType::Module);
    }
}
