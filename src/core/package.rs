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
    /// Optional activation metadata (module, overlay, hook, script, or
    /// completion packages). `None` for plugins and install-only packages.
    #[serde(default)]
    pub activation: Option<RegistryActivationSpec>,
    /// Optional provenance marker, e.g. `"commit-snapshot"` for versions
    /// built from a pinned commit with no upstream tag.
    #[serde(default)]
    pub provenance: Option<String>,
    /// Evidence tier for this version. `None` means "proven" (legacy
    /// entries and any entry with full lifecycle evidence).
    #[serde(default)]
    pub evidence_tier: Option<EvidenceTier>,
    /// Required alongside `evidence_tier: "provisional"`: why
    /// lifecycle-prove was deferred for this version.
    #[serde(default)]
    pub deferral_reason: Option<String>,
}

/// Closed set of evidence tiers; unrecognized values fail deserialization
/// instead of silently being treated as proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceTier {
    Proven,
    Provisional,
}

impl VersionEntry {
    /// Whether this version is provisional (structural validation only, no
    /// full lifecycle evidence). Absent `evidence_tier` means proven.
    pub fn is_provisional(&self) -> bool {
        self.evidence_tier == Some(EvidenceTier::Provisional)
    }

    /// Sanitized, non-empty deferral reason for display, or the fallback
    /// text when the reason is missing, blank, or contains control
    /// characters that would corrupt terminal output.
    pub fn deferral_reason_display(&self) -> String {
        let cleaned: String = self
            .deferral_reason
            .as_deref()
            .unwrap_or("")
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            "reason not recorded".to_string()
        } else {
            trimmed.to_string()
        }
    }
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
    /// Cargo package name. Only set for Rust plugin sources; non-plugin
    /// (archive) sources carry `git`/`rev` provenance without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_name: Option<String>,
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

/// Registry metadata describing how a module, overlay, or hook package should be activated.
///
/// Stored under `activation` in a `VersionEntry`. Recognized kinds:
/// - `"nu-module"`: persistent module activation (Phase 4).
/// - `"nu-overlay"`: session/project overlay activation (Phase 5, issue #17).
/// - `"nu-hook"`: hook profile activation (Phase P1, issue #16).
///
/// Each activation kind is handled only by its explicit capability command.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryActivationSpec {
    /// Activation kind. `"nu-module"`, `"nu-overlay"`, `"nu-hook"`,
    /// `"nu-script"`, or `"nu-completion"`.
    pub kind: String,
    /// Import mode for the module. Defaults to `ModuleImportMode::Module`.
    /// Ignored for `nu-overlay` and `nu-hook`.
    #[serde(default)]
    pub import: ModuleImportMode,
    /// Package-relative entry point for an overlay (e.g. `"overlay.nu"`).
    /// Expected when `kind = "nu-overlay"` (validated at activation time in P2); ignored otherwise.
    #[serde(default)]
    pub entry: Option<String>,
    /// Optional overlay name. Defaults to the package name when absent.
    #[serde(default)]
    pub overlay_name: Option<String>,
    /// Optional declared exported surface for conflict checks.
    #[serde(default)]
    pub exports: Option<Vec<String>>,
    /// Hook bindings. Used when `kind = "nu-hook"`.
    #[serde(default)]
    pub bindings: Option<Vec<HookBinding>>,
    /// Declared script entry points. Only meaningful for `"nu-script"`.
    #[serde(default)]
    pub entry_points: Vec<ScriptEntryPoint>,
    /// Declared completion targets. Only meaningful for `"nu-completion"`.
    #[serde(default)]
    pub targets: Vec<CompletionTarget>,
}

/// One public command contributed by a script package (issue #14).
///
/// `name` is the stable public command name users type in a fresh Nu
/// session after activation; `path` is the package-relative `.nu` target
/// that runs when the command is invoked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptEntryPoint {
    pub name: String,
    pub path: String,
}

/// Whether `name` is a valid public script command name.
///
/// Names are emitted unquoted into the generated autoload file, so the
/// grammar is deliberately small: an ASCII letter or `_`, followed by
/// letters, digits, `_`, or `-`.
pub fn is_valid_script_command_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Nushell parser keywords that cannot be used as `def` command names.
///
/// Emitting `export def --wrapped <keyword>` makes Nu reject the whole
/// managed file, so these are refused at install time rather than failing
/// activation (or every future session).
fn is_nu_reserved_keyword(name: &str) -> bool {
    matches!(
        name,
        "alias"
            | "and"
            | "break"
            | "catch"
            | "const"
            | "continue"
            | "def"
            | "def-env"
            | "else"
            | "export"
            | "export-env"
            | "extern"
            | "false"
            | "for"
            | "hide"
            | "if"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "module"
            | "mut"
            | "not"
            | "null"
            | "or"
            | "overlay"
            | "return"
            | "source"
            | "source-env"
            | "true"
            | "try"
            | "use"
            | "while"
            | "xor"
    )
}

/// Validate a package's declared script entry points.
///
/// Rejects an empty declaration, invalid or duplicate command names,
/// duplicate targets, and targets that are not safe package-relative
/// `.nu` paths. File existence is checked later, at activation time,
/// against the installed payload.
pub fn validate_script_entry_points(entry_points: &[ScriptEntryPoint]) -> Result<()> {
    if entry_points.is_empty() {
        bail!("Script activation requires at least one entry point");
    }
    let mut names = std::collections::BTreeSet::new();
    let mut paths = std::collections::BTreeSet::new();
    for ep in entry_points {
        if !is_valid_script_command_name(&ep.name) {
            bail!(
                "Invalid script command name '{}': must start with a letter or '_' \
                 and contain only letters, digits, '_' or '-'",
                ep.name
            );
        }
        if is_nu_reserved_keyword(&ep.name) {
            bail!(
                "Invalid script command name '{}': reserved Nushell keyword",
                ep.name
            );
        }
        if !names.insert(&ep.name) {
            bail!("Duplicate script command name '{}'", ep.name);
        }
        let rel = std::path::Path::new(&ep.path);
        if !crate::util::fs_safety::is_safe_relative_path(rel) {
            bail!(
                "Script entry point '{}': target '{}' is not a safe package-relative path",
                ep.name,
                ep.path
            );
        }
        match rel.extension() {
            Some(ext) if ext == "nu" => {}
            _ => bail!(
                "Script entry point '{}': target '{}' does not have a '.nu' extension",
                ep.name,
                ep.path
            ),
        }
        let normalized: std::path::PathBuf = rel
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir))
            .collect();
        if !paths.insert(normalized) {
            bail!("Duplicate script entry point target '{}'", ep.path);
        }
    }
    Ok(())
}

/// Declared completion kind for a [`CompletionTarget`] (issue #15).
///
/// The kind is declared by the package, never inferred from the source:
/// `Static` means completions are literal values in the source file;
/// `CommandBacked` means the source defines completer functions that
/// Nushell executes when it requests a completion. Activation and install
/// never invoke a completer either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompletionKind {
    #[serde(rename = "static")]
    Static,
    #[serde(rename = "command-backed")]
    CommandBacked,
}

impl fmt::Display for CompletionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompletionKind::Static => write!(f, "static"),
            CompletionKind::CommandBacked => write!(f, "command-backed"),
        }
    }
}

/// One command surface contributed by a completion package (issue #15).
///
/// `command` is the external command the package augments (a single
/// command name, e.g. `git`); `path` is the package-relative `.nu` source
/// file contributing that command's completions; `kind` is the declared
/// completion kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionTarget {
    pub command: String,
    pub path: String,
    pub kind: CompletionKind,
}

/// Validate a package's declared completion targets.
///
/// Mirrors [`validate_script_entry_points`]: rejects an empty declaration,
/// invalid or duplicate command names (command names use the script
/// command-name grammar and must not be Nushell reserved keywords),
/// duplicate targets, and targets that are not safe package-relative
/// `.nu` paths. File existence is checked later, at activation time,
/// against the installed payload.
pub fn validate_completion_targets(targets: &[CompletionTarget]) -> Result<()> {
    if targets.is_empty() {
        bail!("Completion activation requires at least one target");
    }
    let mut commands = std::collections::BTreeSet::new();
    let mut paths = std::collections::BTreeSet::new();
    for target in targets {
        if !is_valid_script_command_name(&target.command) {
            bail!(
                "Invalid completion command name '{}': must start with a letter or '_' \
                 and contain only letters, digits, '_' or '-'",
                target.command
            );
        }
        if is_nu_reserved_keyword(&target.command) {
            bail!(
                "Invalid completion command name '{}': reserved Nushell keyword",
                target.command
            );
        }
        if !commands.insert(&target.command) {
            bail!("Duplicate completion command '{}'", target.command);
        }
        let rel = std::path::Path::new(&target.path);
        if !crate::util::fs_safety::is_safe_relative_path(rel) {
            bail!(
                "Completion target '{}': path '{}' is not a safe package-relative path",
                target.command,
                target.path
            );
        }
        match rel.extension() {
            Some(ext) if ext == "nu" => {}
            _ => bail!(
                "Completion target '{}': path '{}' does not have a '.nu' extension",
                target.command,
                target.path
            ),
        }
        let normalized: std::path::PathBuf = rel
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir))
            .collect();
        if !paths.insert(normalized) {
            bail!("Duplicate completion target path '{}'", target.path);
        }
    }
    Ok(())
}

/// A single hook binding declared by a package (issue #16).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookBinding {
    /// Stable binding ID (unique within the package and across active hooks).
    pub id: String,
    /// Hook event name (must be in the allow-list).
    #[serde(deserialize_with = "deserialize_allowed_hook_event")]
    pub event: String,
    /// Package-relative handler `.nu` path.
    pub handler: String,
    /// Optional filter expression.
    #[serde(default)]
    pub filter: Option<String>,
}

fn deserialize_allowed_hook_event<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let event = String::deserialize(deserializer)?;
    if crate::nu::hooks::is_allowed_event(&event) {
        Ok(event)
    } else {
        Err(serde::de::Error::custom(format!(
            "unsupported Nushell hook event '{event}'"
        )))
    }
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

impl RegistryIndex {
    /// Validate that all plugin packages have a cargo_name in their source.
    ///
    /// Archive sources (modules, scripts, completions) may omit cargo_name,
    /// but plugin sources must provide it since they represent Rust crates.
    pub fn validate_plugin_sources(&self) -> Result<()> {
        for package in &self.packages {
            if matches!(package.package_type, PackageType::Plugin) {
                for version in &package.versions {
                    if let Some(source) = &version.source {
                        if source.cargo_name.is_none() {
                            bail!("Plugin package {}/{} version {} is missing required cargo_name in source", package.id.owner, package.id.name, version.version);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_binding_event_must_be_allow_list_member() {
        assert!(serde_json::from_str::<HookBinding>(
            r#"{"id":"x","event":"pre_prompt","handler":"hooks/x.nu"}"#
        )
        .is_ok());
        assert!(serde_json::from_str::<HookBinding>(
            r#"{"id":"x","event":"custom_event","handler":"hooks/x.nu"}"#
        )
        .unwrap_err()
        .to_string()
        .contains("unsupported Nushell hook event"));
    }

    #[test]
    fn completion_target_kind_serializes_kebab() {
        let t: CompletionTarget =
            serde_json::from_str(r#"{"command":"git","path":"git.nu","kind":"command-backed"}"#)
                .unwrap();
        assert_eq!(t.kind, CompletionKind::CommandBacked);
        let t: CompletionTarget =
            serde_json::from_str(r#"{"command":"git","path":"git.nu","kind":"static"}"#).unwrap();
        assert_eq!(t.kind, CompletionKind::Static);
        assert!(serde_json::from_str::<CompletionTarget>(
            r#"{"command":"git","path":"git.nu","kind":"inferred"}"#
        )
        .is_err());
    }

    #[test]
    fn validate_completion_targets_rules() {
        let ok = vec![
            CompletionTarget {
                command: "git".into(),
                path: "git.nu".into(),
                kind: CompletionKind::CommandBacked,
            },
            CompletionTarget {
                command: "cargo".into(),
                path: "cargo.nu".into(),
                kind: CompletionKind::Static,
            },
        ];
        assert!(validate_completion_targets(&ok).is_ok());
        assert!(validate_completion_targets(&[]).is_err());
        let one = |command: &str, path: &str| {
            vec![CompletionTarget {
                command: command.into(),
                path: path.into(),
                kind: CompletionKind::Static,
            }]
        };
        assert!(validate_completion_targets(&one("9git", "git.nu")).is_err());
        assert!(validate_completion_targets(&one("has space", "git.nu")).is_err());
        assert!(validate_completion_targets(&one("if", "git.nu")).is_err());
        assert!(validate_completion_targets(&one("git", "../escape.nu")).is_err());
        assert!(validate_completion_targets(&one("git", "git.txt")).is_err());
        let dup_cmd = vec![
            CompletionTarget {
                command: "git".into(),
                path: "a.nu".into(),
                kind: CompletionKind::Static,
            },
            CompletionTarget {
                command: "git".into(),
                path: "b.nu".into(),
                kind: CompletionKind::Static,
            },
        ];
        assert!(validate_completion_targets(&dup_cmd).is_err());
        let dup_path = vec![
            CompletionTarget {
                command: "git".into(),
                path: "./git.nu".into(),
                kind: CompletionKind::Static,
            },
            CompletionTarget {
                command: "cargo".into(),
                path: "git.nu".into(),
                kind: CompletionKind::Static,
            },
        ];
        assert!(validate_completion_targets(&dup_path).is_err());
    }

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
    fn version_entry_absent_evidence_tier_is_not_provisional() {
        let json = r#"{
            "version": "0.25.2",
            "nu_version": ">=0.113.0 <0.114.0",
            "artifact": { "kind": "binary", "targets": {} }
        }"#;
        let entry: VersionEntry = serde_json::from_str(json).unwrap();
        assert!(!entry.is_provisional());
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
    fn version_entry_provisional_evidence_tier() {
        let json = r#"{
            "version": "0.25.2",
            "nu_version": ">=0.113.0 <0.114.0",
            "evidence_tier": "provisional",
            "deferral_reason": "requires cloud credentials",
            "artifact": { "kind": "binary", "targets": {} }
        }"#;
        let entry: VersionEntry = serde_json::from_str(json).unwrap();
        assert!(entry.is_provisional());
        assert_eq!(
            entry.deferral_reason.as_deref(),
            Some("requires cloud credentials")
        );
    }

    #[test]
    fn deferral_reason_display_falls_back_when_missing() {
        let json = r#"{
            "version": "0.25.2",
            "nu_version": ">=0.113.0 <0.114.0",
            "evidence_tier": "provisional",
            "artifact": { "kind": "binary", "targets": {} }
        }"#;
        let entry: VersionEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.deferral_reason_display(), "reason not recorded");
    }

    #[test]
    fn deferral_reason_display_falls_back_when_blank() {
        let mut entry: VersionEntry =
            serde_json::from_str(r#"{"version":"0.25.2","nu_version":">=0.113.0 <0.114.0","artifact":{"kind":"binary","targets":{}}}"#).unwrap();
        entry.deferral_reason = Some("   ".to_string());
        assert_eq!(entry.deferral_reason_display(), "reason not recorded");
    }

    #[test]
    fn deferral_reason_display_strips_control_characters() {
        let mut entry: VersionEntry =
            serde_json::from_str(r#"{"version":"0.25.2","nu_version":">=0.113.0 <0.114.0","artifact":{"kind":"binary","targets":{}}}"#).unwrap();
        entry.deferral_reason = Some("line one\nfake success line".to_string());
        assert_eq!(
            entry.deferral_reason_display(),
            "line one fake success line"
        );
    }

    #[test]
    fn version_entry_proven_evidence_tier_is_not_provisional() {
        let json = r#"{
            "version": "0.25.2",
            "nu_version": ">=0.113.0 <0.114.0",
            "evidence_tier": "proven",
            "artifact": { "kind": "binary", "targets": {} }
        }"#;
        let entry: VersionEntry = serde_json::from_str(json).unwrap();
        assert!(!entry.is_provisional());
    }

    #[test]
    fn version_entry_rejects_unknown_evidence_tier() {
        let json = r#"{
            "version": "0.25.2",
            "nu_version": ">=0.113.0 <0.114.0",
            "evidence_tier": "provisonal",
            "artifact": { "kind": "binary", "targets": {} }
        }"#;
        assert!(serde_json::from_str::<VersionEntry>(json).is_err());
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
        assert_eq!(source.cargo_name.as_deref(), Some("nu_plugin_highlight"));
        assert!(source.cargo_lock_sha256.is_none());
    }

    #[test]
    fn parse_version_entry_with_archive_source_without_cargo_name() {
        let json = r#"{
            "version": "1.0.0",
            "nu_version": ">=0.114.0",
            "source": {
                "git": "https://github.com/owner/cool-module",
                "rev": "5a1ca2a5ceba60108a4ca6d45ec18d213abb5227"
            },
            "artifact": {
                "kind": "archive",
                "url": "https://example.com/p.tar.gz",
                "sha256": "abc123",
                "entry": "mod.nu"
            }
        }"#;
        let entry: VersionEntry = serde_json::from_str(json).unwrap();
        let source = entry.source.expect("source present");
        assert_eq!(source.git, "https://github.com/owner/cool-module");
        assert_eq!(source.rev, "5a1ca2a5ceba60108a4ca6d45ec18d213abb5227");
        assert!(source.cargo_name.is_none());
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
