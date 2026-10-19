use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::core::integrity::compute_sha256;
use crate::core::package::ModuleImportMode;
use crate::nupm_compat::schema::NUPM_IMPORT_ORIGIN;
use crate::util::atomic::write_json_atomic;

/// Origin marker for plugins bundled with an official Nushell release archive
/// and discovered automatically during `numan setup nu`.
pub const BUNDLED_NU_ORIGIN: &str = "bundled:nu";

/// Per-Nu-identity activation record stored on a plugin lockfile entry.
///
/// A plugin is "currently active" only when this record's hash, version, and
/// registry path all match the loaded `NuPaths`. A bare boolean would become
/// stale after `numan init --refresh` changes the Nu binary or registry target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginActivation {
    pub plugin_registry_path: String,
    pub nu_executable_sha256: String,
    pub nu_version: String,
    pub activated_at: String,
}

/// Per-Nu-identity activation record stored on a module lockfile entry.
///
/// A module is "currently active" only when this record's Nu executable hash,
/// Nu version, vendor-autoload directory, and managed file path all match the
/// cached `NuPaths` and the autoload-state projection. This is separate from
/// the plugin activation record because module activation is tied to a
/// vendor-autoload file rather than the Nu plugin registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleActivation {
    /// Absolute path to the entry `.nu` file within the installed payload.
    pub entry_path: String,
    /// Whether this module is imported namespaced or glob-imported.
    pub import_mode: ModuleImportMode,
    /// Absolute path to the selected vendor-autoload directory.
    pub vendor_autoload_dir: String,
    /// Absolute path to the Numan-managed autoload file (`numan.nu`).
    pub managed_file_path: String,
    /// SHA-256 of the Nu executable used when this activation was recorded.
    pub nu_executable_sha256: String,
    /// Version string of the Nu executable used when this activation was recorded.
    pub nu_version: String,
    /// Timestamp when this activation record was written.
    pub activated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lockfile {
    pub version: u32,
    pub generated_at: String,
    pub nu_version: String,
    pub platform: String,
    pub packages: BTreeMap<String, LockfileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockfileEntry {
    pub version: String,
    #[serde(rename = "type")]
    pub package_type: String,
    pub source: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub artifact_url: Option<String>,
    #[serde(default)]
    pub artifact_sha256: Option<String>,
    #[serde(default)]
    pub executable_path: Option<String>,
    #[serde(default)]
    pub archive_root: Option<String>,
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub entry: Option<String>,
    pub installed_at: String,
    #[serde(default)]
    pub nu_version_at_install: Option<String>,
    /// Activation record for this plugin under a specific Nu identity.
    /// `None` means not yet activated (or activation record not yet written).
    /// Old JSON with `"activated": false` deserializes cleanly — serde ignores
    /// the unknown field; this field defaults to `None` via `#[serde(default)]`.
    #[serde(default)]
    pub activation: Option<PluginActivation>,
    #[serde(default)]
    pub registry_url: Option<String>,
    #[serde(default)]
    pub registry_revision: Option<String>,
    #[serde(default)]
    pub index_sha256: Option<String>,
    #[serde(default)]
    pub signing_key_fingerprint: Option<String>,
    // Source-built fields
    #[serde(default)]
    pub git_url: Option<String>,
    #[serde(default)]
    pub git_rev: Option<String>,
    #[serde(default)]
    pub cargo_name: Option<String>,
    #[serde(default)]
    pub cargo_lock_sha256: Option<String>,
    #[serde(default)]
    pub built_sha256: Option<String>,
    /// Relative path to the installed payload directory from the numan root.
    /// E.g., "packages/plugins/fdncred/file/0.25.2-abc12345"
    #[serde(default)]
    pub payload_path: String,

    // Phase 5 schema v2 fields
    /// Manifest hash of the installed payload directory.
    ///
    /// Computed as SHA256 of sorted "path:sha256:kind" lines for every file
    /// under the payload directory. Use `compute_revision_id()` to derive it.
    /// `None` for entries written by pre-v2 tooling (v1 lockfiles).
    #[serde(default)]
    pub revision_id: Option<String>,

    /// SHA256 of the payload archive (tarball / zip) as downloaded from the
    /// artifact URL. Distinct from `artifact_sha256` (the registry-declared
    /// digest used for integrity verification).
    /// `None` for entries that do not have a downloadable payload archive.
    #[serde(default)]
    pub payload_sha256: Option<String>,

    /// SHA256 of the plugin executable extracted from the payload archive.
    /// Only set for `type = "plugin"` entries; `None` for all other types.
    #[serde(default)]
    pub executable_sha256: Option<String>,

    /// Human-readable reason the resolver chose this version over alternatives
    /// (e.g. `"exact match"`, `"highest compatible semver"`).
    /// For informational/debugging purposes only.
    #[serde(default)]
    pub selection_reason: Option<String>,

    /// Origin of the install request that created this entry.
    /// E.g. `"registry:official"`, `"direct"`, `"source"`.
    #[serde(default)]
    pub origin: Option<String>,

    // Module-specific fields (Phase 4)
    /// Activation record for this module under a specific Nu identity and
    /// vendor-autoload target. `None` means not yet activated.
    /// This field is distinct from the plugin `activation` field — do not
    /// reinterpret or merge them.
    #[serde(default)]
    pub module_activation: Option<ModuleActivation>,

    /// Import mode persisted from the registry at install time.
    /// `None` for packages that have no `activation` spec (plugins, scripts,
    /// completions). `Some(ModuleImportMode::Module)` is the implicit default
    /// for module packages whose registry entry omits `activation.import`.
    #[serde(default)]
    pub module_import_mode: Option<ModuleImportMode>,

    /// Registry dependency map captured at install time.
    /// Activation requires this to be empty in Phase 4.
    #[serde(default)]
    pub locked_dependencies: BTreeMap<String, String>,
}

impl LockfileEntry {
    pub fn payload_path(&self) -> &str {
        &self.payload_path
    }

    /// Returns `true` if this plugin entry is active for the given Nu identity.
    pub fn is_active_for(
        &self,
        nu_executable_sha256: &str,
        nu_version: &str,
        plugin_registry_path: &str,
    ) -> bool {
        match &self.activation {
            Some(a) => {
                a.nu_executable_sha256 == nu_executable_sha256
                    && a.nu_version == nu_version
                    && a.plugin_registry_path == plugin_registry_path
            }
            None => false,
        }
    }

    /// Returns `true` if this module entry is active for the given Nu identity
    /// and vendor-autoload target. All four identity fields must match.
    pub fn is_module_active_for(
        &self,
        nu_executable_sha256: &str,
        nu_version: &str,
        vendor_autoload_dir: &str,
        managed_file_path: &str,
    ) -> bool {
        match &self.module_activation {
            Some(a) => {
                a.nu_executable_sha256 == nu_executable_sha256
                    && a.nu_version == nu_version
                    && a.vendor_autoload_dir == vendor_autoload_dir
                    && a.managed_file_path == managed_file_path
            }
            None => false,
        }
    }

    /// Returns `true` when this module can be activated in Phase 4:
    /// - `module_import_mode` is known (set at install time),
    /// - `entry` is set (required for `use` statement generation),
    /// - `locked_dependencies` is empty (Phase 4 cannot resolve cross-package deps).
    pub fn is_module_activatable(&self) -> bool {
        self.module_import_mode.is_some()
            && self.entry.is_some()
            && self.locked_dependencies.is_empty()
    }
}

impl Lockfile {
    pub fn load(root: &Path) -> Result<Self> {
        let lock_path = root.join("lockfile");
        if !lock_path.exists() {
            return Ok(Self::empty());
        }
        let content = std::fs::read_to_string(&lock_path)
            .with_context(|| format!("Failed to read {}", lock_path.display()))?;
        let lockfile: Lockfile = serde_json::from_str(&content)?;
        Ok(lockfile)
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let lock_path = root.join("lockfile");
        write_json_atomic(&lock_path, self)
    }

    pub fn empty() -> Self {
        Self {
            version: 2,
            generated_at: String::new(),
            nu_version: String::new(),
            platform: String::new(),
            packages: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// Count lockfile entries imported from nupm (`origin == NUPM_IMPORT_ORIGIN`).
    pub fn count_nupm_imports(&self) -> usize {
        self.packages
            .values()
            .filter(|e| e.origin.as_deref() == Some(NUPM_IMPORT_ORIGIN))
            .count()
    }
}

/// Compute the `revision_id` for an installed payload directory.
///
/// Walks every file under `payload_dir` recursively, builds a sorted list of
/// `"<relative-path>:<sha256>:<kind>"` lines (where `kind` is `"file"` for all
/// regular files), joins them with `"\n"`, and returns the SHA-256 hex digest of
/// that UTF-8 manifest string.
///
/// Paths are represented with forward-slash separators regardless of the host
/// OS, and are relative to `payload_dir`.  Only regular files are included;
/// directories and symlinks are skipped.
///
/// Returns `None` when `payload_dir` does not exist or cannot be read, so
/// callers may gracefully degrade rather than failing an install.
pub fn compute_revision_id(payload_dir: &Path) -> Option<String> {
    let mut lines: BTreeMap<String, String> = BTreeMap::new();

    fn walk(base: &Path, dir: &Path, lines: &mut BTreeMap<String, String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                walk(base, &path, lines);
            } else if file_type.is_file() {
                let rel = match path.strip_prefix(base) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                // Normalise to forward-slash separators for cross-platform
                // determinism.
                let rel_str = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                let content = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                let sha = compute_sha256(&content);
                let line = format!("{sha}:file");
                lines.insert(rel_str, line);
            }
        }
    }

    if !payload_dir.exists() {
        return None;
    }

    walk(payload_dir, payload_dir, &mut lines);

    // Build the manifest: sorted "path:sha256:kind" lines.
    let manifest = lines
        .into_iter()
        .map(|(path, digest_kind)| format!("{path}:{digest_kind}"))
        .collect::<Vec<_>>()
        .join("\n");

    Some(compute_sha256(manifest.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_nupm_imports_only_nupm_origin() {
        let mut lock = Lockfile::empty();
        lock.packages.insert(
            "a/b".to_string(),
            LockfileEntry {
                version: "1".to_string(),
                package_type: "module".to_string(),
                source: "nupm".to_string(),
                installed_at: String::new(),
                payload_path: "packages/modules/a/b/1".to_string(),
                origin: Some(NUPM_IMPORT_ORIGIN.to_string()),
                ..minimal_lockfile_entry_defaults()
            },
        );
        lock.packages.insert(
            "c/d".to_string(),
            LockfileEntry {
                version: "1".to_string(),
                package_type: "module".to_string(),
                source: "registry".to_string(),
                installed_at: String::new(),
                payload_path: "packages/modules/c/d/1".to_string(),
                origin: Some("registry:official".to_string()),
                ..minimal_lockfile_entry_defaults()
            },
        );
        assert_eq!(lock.count_nupm_imports(), 1);
    }

    fn minimal_lockfile_entry_defaults() -> LockfileEntry {
        LockfileEntry {
            version: String::new(),
            package_type: String::new(),
            source: String::new(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: None,
            installed_at: String::new(),
            nu_version_at_install: None,
            activation: None,
            registry_url: None,
            registry_revision: None,
            index_sha256: None,
            signing_key_fingerprint: None,
            git_url: None,
            git_rev: None,
            cargo_name: None,
            cargo_lock_sha256: None,
            built_sha256: None,
            payload_path: String::new(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: BTreeMap::new(),
        }
    }

    #[test]
    fn empty_lockfile_roundtrip() {
        let lock = Lockfile::empty();
        let json = serde_json::to_string_pretty(&lock).unwrap();
        let parsed: Lockfile = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn lockfile_roundtrip_with_entry() {
        let mut lock = Lockfile {
            version: 2,
            generated_at: "2026-06-27T12:00:00Z".to_string(),
            nu_version: "0.113.1".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            packages: BTreeMap::new(),
        };

        lock.packages.insert(
            "test/pkg".to_string(),
            LockfileEntry {
                version: "1.0.0".to_string(),
                package_type: "plugin".to_string(),
                source: "binary".to_string(),
                target: Some("x86_64-pc-windows-msvc".to_string()),
                artifact_url: Some("https://example.com/pkg.zip".to_string()),
                artifact_sha256: Some("abc123".to_string()),
                executable_path: Some("nu_plugin_test.exe".to_string()),
                archive_root: None,
                include: None,
                entry: None,
                installed_at: "2026-06-27T12:00:00Z".to_string(),
                nu_version_at_install: Some("0.113.1".to_string()),
                activation: None,
                registry_url: Some("registry:official".to_string()),
                registry_revision: Some("abc123".to_string()),
                index_sha256: Some("def456".to_string()),
                signing_key_fingerprint: Some("sha256:789".to_string()),
                git_url: None,
                git_rev: None,
                cargo_name: None,
                cargo_lock_sha256: None,
                built_sha256: None,
                payload_path: "packages/plugins/test/pkg/1.0.0-abc12345".to_string(),
                revision_id: None,
                payload_sha256: None,
                executable_sha256: None,
                selection_reason: None,
                origin: None,
                module_activation: None,
                module_import_mode: None,
                locked_dependencies: BTreeMap::new(),
            },
        );

        let json = serde_json::to_string_pretty(&lock).unwrap();
        let parsed: Lockfile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.packages.len(), 1);
        let entry = parsed.packages.get("test/pkg").unwrap();
        assert_eq!(
            entry.payload_path,
            "packages/plugins/test/pkg/1.0.0-abc12345"
        );
        assert!(entry.activation.is_none());
        assert!(entry.module_activation.is_none());
        assert!(entry.module_import_mode.is_none());
        assert!(entry.locked_dependencies.is_empty());
    }

    #[test]
    fn old_json_with_activated_bool_deserializes() {
        // JSON from before the PluginActivation migration — `"activated": false`
        // must deserialize without error; unknown fields are ignored by serde.
        let json = r#"{
            "version": 1,
            "generated_at": "",
            "nu_version": "0.113.1",
            "platform": "x86_64-pc-windows-msvc",
            "packages": {
                "test/pkg": {
                    "version": "1.0.0",
                    "type": "plugin",
                    "source": "binary",
                    "installed_at": "0000000000000000",
                    "activated": false,
                    "payload_path": "packages/plugins/test/pkg/1.0.0-abc"
                }
            }
        }"#;
        let parsed: Lockfile = serde_json::from_str(json).unwrap();
        let entry = parsed.packages.get("test/pkg").unwrap();
        assert!(entry.activation.is_none());
    }

    fn make_base_entry() -> LockfileEntry {
        LockfileEntry {
            version: "1.0.0".to_string(),
            package_type: "plugin".to_string(),
            source: "binary".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: None,
            installed_at: "0".to_string(),
            nu_version_at_install: None,
            activation: None,
            registry_url: None,
            registry_revision: None,
            index_sha256: None,
            signing_key_fingerprint: None,
            git_url: None,
            git_rev: None,
            cargo_name: None,
            cargo_lock_sha256: None,
            built_sha256: None,
            payload_path: String::new(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: BTreeMap::new(),
        }
    }

    #[test]
    fn is_active_for_matches_correctly() {
        let entry = LockfileEntry {
            activation: Some(PluginActivation {
                plugin_registry_path: "/path/to/plugins.msgpackz".to_string(),
                nu_executable_sha256: "abc123".to_string(),
                nu_version: "0.113.1".to_string(),
                activated_at: "0".to_string(),
            }),
            ..make_base_entry()
        };

        assert!(entry.is_active_for("abc123", "0.113.1", "/path/to/plugins.msgpackz"));
        assert!(!entry.is_active_for("different_hash", "0.113.1", "/path/to/plugins.msgpackz"));
        assert!(!entry.is_active_for("abc123", "0.114.0", "/path/to/plugins.msgpackz"));
        assert!(!entry.is_active_for("abc123", "0.113.1", "/other/path.msgpackz"));
    }

    #[test]
    fn is_module_active_for_matches_correctly() {
        use crate::core::package::ModuleImportMode;

        let entry = LockfileEntry {
            package_type: "module".to_string(),
            module_activation: Some(ModuleActivation {
                entry_path: "/root/packages/modules/owner/foo/1.0.0-abc/mod.nu".to_string(),
                import_mode: ModuleImportMode::Module,
                vendor_autoload_dir: "/nu/vendor/autoload".to_string(),
                managed_file_path: "/nu/vendor/autoload/numan.nu".to_string(),
                nu_executable_sha256: "exe-hash".to_string(),
                nu_version: "0.113.1".to_string(),
                activated_at: "0".to_string(),
            }),
            module_import_mode: Some(ModuleImportMode::Module),
            ..make_base_entry()
        };

        assert!(entry.is_module_active_for(
            "exe-hash",
            "0.113.1",
            "/nu/vendor/autoload",
            "/nu/vendor/autoload/numan.nu"
        ));
        assert!(!entry.is_module_active_for(
            "wrong-hash",
            "0.113.1",
            "/nu/vendor/autoload",
            "/nu/vendor/autoload/numan.nu"
        ));
        assert!(!entry.is_module_active_for(
            "exe-hash",
            "0.114.0",
            "/nu/vendor/autoload",
            "/nu/vendor/autoload/numan.nu"
        ));
        assert!(!entry.is_module_active_for(
            "exe-hash",
            "0.113.1",
            "/other/vendor/autoload",
            "/nu/vendor/autoload/numan.nu"
        ));
        assert!(!entry.is_module_active_for(
            "exe-hash",
            "0.113.1",
            "/nu/vendor/autoload",
            "/other/path/numan.nu"
        ));
    }

    #[test]
    fn is_module_activatable_requires_mode_entry_and_no_deps() {
        use crate::core::package::ModuleImportMode;

        // All three conditions met
        let ready = LockfileEntry {
            package_type: "module".to_string(),
            entry: Some("mod.nu".to_string()),
            module_import_mode: Some(ModuleImportMode::Module),
            locked_dependencies: BTreeMap::new(),
            ..make_base_entry()
        };
        assert!(ready.is_module_activatable());

        // Missing entry
        let no_entry = LockfileEntry {
            package_type: "module".to_string(),
            entry: None,
            module_import_mode: Some(ModuleImportMode::Module),
            ..make_base_entry()
        };
        assert!(!no_entry.is_module_activatable());

        // Missing import mode
        let no_mode = LockfileEntry {
            package_type: "module".to_string(),
            entry: Some("mod.nu".to_string()),
            module_import_mode: None,
            ..make_base_entry()
        };
        assert!(!no_mode.is_module_activatable());

        // Has dependencies
        let with_deps = LockfileEntry {
            package_type: "module".to_string(),
            entry: Some("mod.nu".to_string()),
            module_import_mode: Some(ModuleImportMode::All),
            locked_dependencies: {
                let mut m = BTreeMap::new();
                m.insert("owner/dep".to_string(), "^1.0.0".to_string());
                m
            },
            ..make_base_entry()
        };
        assert!(!with_deps.is_module_activatable());
    }

    #[test]
    fn module_fields_survive_lockfile_roundtrip() {
        use crate::core::package::ModuleImportMode;

        let mut lock = Lockfile {
            version: 2,
            generated_at: "ts".to_string(),
            nu_version: "0.113.1".to_string(),
            platform: "x86_64-linux-gnu".to_string(),
            packages: BTreeMap::new(),
        };

        let mut deps = BTreeMap::new();
        deps.insert("owner/lib".to_string(), "^2.0.0".to_string());

        lock.packages.insert(
            "owner/mymod".to_string(),
            LockfileEntry {
                package_type: "module".to_string(),
                source: "archive".to_string(),
                entry: Some("mod.nu".to_string()),
                module_import_mode: Some(ModuleImportMode::All),
                locked_dependencies: deps,
                module_activation: Some(ModuleActivation {
                    entry_path: "/root/packages/modules/owner/mymod/1.0.0-aaa/mod.nu".to_string(),
                    import_mode: ModuleImportMode::All,
                    vendor_autoload_dir: "/nu/vendor/autoload".to_string(),
                    managed_file_path: "/nu/vendor/autoload/numan.nu".to_string(),
                    nu_executable_sha256: "abc".to_string(),
                    nu_version: "0.113.1".to_string(),
                    activated_at: "ts".to_string(),
                }),
                payload_path: "packages/modules/owner/mymod/1.0.0-aaa".to_string(),
                ..make_base_entry()
            },
        );

        let json = serde_json::to_string_pretty(&lock).unwrap();
        let parsed: Lockfile = serde_json::from_str(&json).unwrap();
        let entry = parsed.packages.get("owner/mymod").unwrap();

        assert_eq!(entry.module_import_mode, Some(ModuleImportMode::All));
        assert_eq!(entry.locked_dependencies.len(), 1);
        assert_eq!(
            entry.locked_dependencies.get("owner/lib").unwrap(),
            "^2.0.0"
        );

        let ma = entry.module_activation.as_ref().unwrap();
        assert_eq!(ma.import_mode, ModuleImportMode::All);
        assert_eq!(ma.vendor_autoload_dir, "/nu/vendor/autoload");
        assert_eq!(ma.managed_file_path, "/nu/vendor/autoload/numan.nu");
    }

    #[test]
    fn old_json_without_module_fields_deserializes_with_defaults() {
        // Existing lockfile entries written before Phase 4 must still parse.
        // All three new fields have #[serde(default)] and default to None/empty.
        let json = r#"{
            "version": 1,
            "generated_at": "",
            "nu_version": "0.113.1",
            "platform": "x86_64-pc-windows-msvc",
            "packages": {
                "test/pkg": {
                    "version": "1.0.0",
                    "type": "plugin",
                    "source": "binary",
                    "installed_at": "0000000000000000",
                    "payload_path": "packages/plugins/test/pkg/1.0.0-abc"
                }
            }
        }"#;
        let parsed: Lockfile = serde_json::from_str(json).unwrap();
        let entry = parsed.packages.get("test/pkg").unwrap();
        assert!(entry.module_activation.is_none());
        assert!(entry.module_import_mode.is_none());
        assert!(entry.locked_dependencies.is_empty());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let lock = Lockfile {
            version: 2,
            generated_at: "ts".to_string(),
            nu_version: "0.113.1".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            packages: BTreeMap::new(),
        };
        lock.save(&root).unwrap();
        let loaded = Lockfile::load(&root).unwrap();
        assert_eq!(loaded.nu_version, "0.113.1");
    }

    /// v1 lockfile JSON (no revision_id, payload_sha256, executable_sha256,
    /// selection_reason, or origin fields) must deserialize cleanly into a v2
    /// `LockfileEntry` with all new fields defaulting to `None`.
    #[test]
    fn v1_to_v2_migration_new_fields_default_to_none() {
        let json = r#"{
            "version": 1,
            "generated_at": "0000000000000000",
            "nu_version": "0.113.1",
            "platform": "x86_64-pc-windows-msvc",
            "packages": {
                "fdncred/file": {
                    "version": "0.25.2",
                    "type": "plugin",
                    "source": "binary",
                    "target": "x86_64-pc-windows-msvc",
                    "artifact_url": "https://example.com/nu_plugin_file.zip",
                    "artifact_sha256": "abc123def456abc123def456abc123def456abc123def456abc123def456abcd",
                    "executable_path": "nu_plugin_file.exe",
                    "installed_at": "0000000000000001",
                    "payload_path": "packages/plugins/fdncred/file/0.25.2-abc123de"
                }
            }
        }"#;

        let parsed: Lockfile = serde_json::from_str(json).unwrap();
        // Schema version is preserved as-is from the JSON (v1).
        assert_eq!(parsed.version, 1);

        let entry = parsed.packages.get("fdncred/file").unwrap();

        // Core fields must parse correctly.
        assert_eq!(entry.version, "0.25.2");
        assert_eq!(entry.package_type, "plugin");
        assert_eq!(
            entry.artifact_sha256.as_deref(),
            Some("abc123def456abc123def456abc123def456abc123def456abc123def456abcd")
        );

        // All v2-only fields must default to None.
        assert!(
            entry.revision_id.is_none(),
            "revision_id should default to None"
        );
        assert!(
            entry.payload_sha256.is_none(),
            "payload_sha256 should default to None"
        );
        assert!(
            entry.executable_sha256.is_none(),
            "executable_sha256 should default to None"
        );
        assert!(
            entry.selection_reason.is_none(),
            "selection_reason should default to None"
        );
        assert!(entry.origin.is_none(), "origin should default to None");

        // Existing optional fields that were absent also default to None.
        assert!(entry.activation.is_none());
        assert!(entry.module_activation.is_none());
        assert!(entry.module_import_mode.is_none());
        assert!(entry.locked_dependencies.is_empty());
    }

    #[test]
    fn compute_revision_id_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Write a pair of files.
        std::fs::write(root.join("a.nu"), b"def foo [] { }").unwrap();
        std::fs::write(root.join("b.nu"), b"def bar [] { }").unwrap();

        let id1 = compute_revision_id(root).expect("should compute revision_id");
        let id2 = compute_revision_id(root).expect("should compute revision_id again");

        // Same directory contents → identical digest.
        assert_eq!(id1, id2);
        // SHA-256 hex is 64 characters.
        assert_eq!(id1.len(), 64);
    }

    #[test]
    fn compute_revision_id_changes_when_content_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("mod.nu"), b"version 1").unwrap();
        let id_before = compute_revision_id(root).unwrap();

        std::fs::write(root.join("mod.nu"), b"version 2").unwrap();
        let id_after = compute_revision_id(root).unwrap();

        assert_ne!(id_before, id_after);
    }

    #[test]
    fn compute_revision_id_nonexistent_dir_returns_none() {
        use std::path::PathBuf;
        let path = PathBuf::from("/nonexistent/payload/directory");
        assert!(compute_revision_id(&path).is_none());
    }

    #[test]
    fn v2_fields_roundtrip() {
        let mut lock = Lockfile {
            version: 2,
            generated_at: "ts".to_string(),
            nu_version: "0.113.1".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            packages: BTreeMap::new(),
        };
        lock.packages.insert(
            "owner/pkg".to_string(),
            LockfileEntry {
                revision_id: Some("deadbeef".repeat(8)),
                payload_sha256: Some("payload-hash".to_string()),
                executable_sha256: Some("exe-hash".to_string()),
                selection_reason: Some("highest compatible semver".to_string()),
                origin: Some("registry:official".to_string()),
                ..make_base_entry()
            },
        );

        let json = serde_json::to_string_pretty(&lock).unwrap();
        let parsed: Lockfile = serde_json::from_str(&json).unwrap();
        let entry = parsed.packages.get("owner/pkg").unwrap();

        assert_eq!(
            entry.revision_id.as_deref(),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        );
        assert_eq!(entry.payload_sha256.as_deref(), Some("payload-hash"));
        assert_eq!(entry.executable_sha256.as_deref(), Some("exe-hash"));
        assert_eq!(
            entry.selection_reason.as_deref(),
            Some("highest compatible semver")
        );
        assert_eq!(entry.origin.as_deref(), Some("registry:official"));
    }
}
