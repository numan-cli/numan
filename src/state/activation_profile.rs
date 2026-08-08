//! Desired per-Nu-minor activation profile (`nu_state/activation-profile.json`).
//!
//! The profile records which plugins/modules Numan should restore when
//! switching to a given Nu minor via `numan use`. It is **desired state**:
//! - `numan use` may **union** evidence into a leaving minor; it never shrinks.
//! - Only successful user `deactivate` / `remove` may remove entries.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::util::atomic::write_json_atomic;

pub const ACTIVATION_PROFILE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    Plugin,
    Module,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MinorActivationSet {
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub modules: Vec<String>,
}

impl MinorActivationSet {
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty() && self.modules.is_empty()
    }

    fn list_mut(&mut self, kind: ProfileKind) -> &mut Vec<String> {
        match kind {
            ProfileKind::Plugin => &mut self.plugins,
            ProfileKind::Module => &mut self.modules,
        }
    }

    fn ensure_contains(&mut self, kind: ProfileKind, id: &str) -> bool {
        let list = self.list_mut(kind);
        if list.iter().any(|e| e == id) {
            return false;
        }
        list.push(id.to_string());
        list.sort();
        true
    }

    fn ensure_absent(&mut self, kind: ProfileKind, id: &str) -> bool {
        let list = self.list_mut(kind);
        let before = list.len();
        list.retain(|e| e != id);
        before != list.len()
    }

    fn union_with(&mut self, other: &MinorActivationSet) {
        for id in &other.plugins {
            self.ensure_contains(ProfileKind::Plugin, id);
        }
        for id in &other.modules {
            self.ensure_contains(ProfileKind::Module, id);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivationProfile {
    pub schema_version: u32,
    #[serde(default)]
    pub by_nu_minor: BTreeMap<String, MinorActivationSet>,
}

impl ActivationProfile {
    pub fn new() -> Self {
        Self {
            schema_version: ACTIVATION_PROFILE_SCHEMA_VERSION,
            by_nu_minor: BTreeMap::new(),
        }
    }

    pub fn profile_path(root: &Path) -> std::path::PathBuf {
        root.join("nu_state").join("activation-profile.json")
    }

    pub fn load(root: &Path) -> Result<Option<Self>> {
        let path = Self::profile_path(root);
        if !path.is_file() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read activation profile '{}'", path.display()))?;
        let profile: Self = serde_json::from_str(&raw).with_context(|| {
            format!(
                "Malformed activation profile at '{}' (delete it or repair JSON)",
                path.display()
            )
        })?;
        if profile.schema_version != ACTIVATION_PROFILE_SCHEMA_VERSION {
            anyhow::bail!(
                "Unsupported activation-profile schema_version {} at '{}' (expected {})",
                profile.schema_version,
                path.display(),
                ACTIVATION_PROFILE_SCHEMA_VERSION
            );
        }
        Ok(Some(profile))
    }

    pub fn load_or_default(root: &Path) -> Result<Self> {
        Ok(Self::load(root)?.unwrap_or_default())
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = Self::profile_path(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create directory for activation profile '{}'",
                    parent.display()
                )
            })?;
        }
        write_json_atomic(&path, self)
            .with_context(|| format!("Failed to write activation profile '{}'", path.display()))?;
        Ok(())
    }

    pub fn set_for_minor(&self, minor: &str) -> MinorActivationSet {
        self.by_nu_minor.get(minor).cloned().unwrap_or_default()
    }

    /// Leave merge: `existing ∪ currently_active` (never shrink).
    pub fn merge_leave(&mut self, minor: &str, currently_active: &MinorActivationSet) {
        let entry = self.by_nu_minor.entry(minor.to_string()).or_default();
        entry.union_with(currently_active);
    }

    pub fn ensure_contains(&mut self, minor: &str, kind: ProfileKind, id: &str) -> bool {
        self.by_nu_minor
            .entry(minor.to_string())
            .or_default()
            .ensure_contains(kind, id)
    }

    pub fn ensure_absent(&mut self, minor: &str, kind: ProfileKind, id: &str) -> bool {
        let Some(entry) = self.by_nu_minor.get_mut(minor) else {
            return false;
        };
        let changed = entry.ensure_absent(kind, id);
        if entry.is_empty() {
            self.by_nu_minor.remove(minor);
        }
        changed
    }

    /// Remove a package id from every minor (both plugins and modules lists).
    pub fn remove_from_all_minors(&mut self, id: &str) -> bool {
        let mut changed = false;
        let minors: Vec<String> = self.by_nu_minor.keys().cloned().collect();
        for minor in minors {
            changed |= self.ensure_absent(&minor, ProfileKind::Plugin, id);
            changed |= self.ensure_absent(&minor, ProfileKind::Module, id);
        }
        changed
    }
}

impl Default for ActivationProfile {
    fn default() -> Self {
        Self::new()
    }
}

/// Format `major.minor` for profile keys (Nu ABI band).
pub fn nu_minor_key(major: u64, minor: u64) -> String {
    format!("{major}.{minor}")
}

pub fn nu_minor_key_from_version(version: &str) -> Result<String> {
    let nu = crate::core::nu_version::NuVersion::parse(version)?;
    Ok(nu_minor_key(nu.major, nu.minor))
}

/// Persist ensure_contains for the current minor.
/// Returns `Err` when `nu_version` cannot be parsed into a minor key.
pub fn ensure_contains_for_paths(
    root: &Path,
    nu_version: &str,
    kind: ProfileKind,
    id: &str,
) -> Result<()> {
    let minor = nu_minor_key_from_version(nu_version)?;
    let mut profile = ActivationProfile::load_or_default(root)?;
    if profile.ensure_contains(&minor, kind, id) {
        profile.save(root)?;
    }
    Ok(())
}

/// Persist ensure_absent for the current minor.
/// Returns `Err` when `nu_version` cannot be parsed into a minor key.
pub fn ensure_absent_for_paths(
    root: &Path,
    nu_version: &str,
    kind: ProfileKind,
    id: &str,
) -> Result<()> {
    let minor = nu_minor_key_from_version(nu_version)?;
    let mut profile = ActivationProfile::load_or_default(root)?;
    if profile.ensure_absent(&minor, kind, id) {
        profile.save(root)?;
    }
    Ok(())
}

pub fn remove_from_all_minors(root: &Path, id: &str) -> Result<()> {
    let mut profile = ActivationProfile::load_or_default(root)?;
    if profile.remove_from_all_minors(id) {
        profile.save(root)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn merge_leave_unions_and_never_shrinks() {
        let mut profile = ActivationProfile::new();
        profile.merge_leave(
            "0.114",
            &MinorActivationSet {
                plugins: vec!["a/p1".into(), "a/p2".into()],
                modules: vec![],
            },
        );
        profile.merge_leave(
            "0.114",
            &MinorActivationSet {
                plugins: vec!["a/p2".into()],
                modules: vec![],
            },
        );
        let set = profile.set_for_minor("0.114");
        assert_eq!(set.plugins, vec!["a/p1".to_string(), "a/p2".to_string()]);
    }

    #[test]
    fn merge_leave_bootstraps_when_missing() {
        let mut profile = ActivationProfile::new();
        profile.merge_leave(
            "0.113",
            &MinorActivationSet {
                plugins: vec!["b/semver".into()],
                modules: vec!["c/mod".into()],
            },
        );
        let set = profile.set_for_minor("0.113");
        assert_eq!(set.plugins, vec!["b/semver".to_string()]);
        assert_eq!(set.modules, vec!["c/mod".to_string()]);
    }

    #[test]
    fn ensure_absent_is_idempotent() {
        let mut profile = ActivationProfile::new();
        profile.ensure_contains("0.114", ProfileKind::Plugin, "a/p");
        assert!(profile.ensure_absent("0.114", ProfileKind::Plugin, "a/p"));
        assert!(!profile.ensure_absent("0.114", ProfileKind::Plugin, "a/p"));
        assert!(profile.by_nu_minor.is_empty());
    }

    #[test]
    fn remove_from_all_minors_clears_everywhere() {
        let mut profile = ActivationProfile::new();
        profile.ensure_contains("0.113", ProfileKind::Plugin, "a/pkg");
        profile.ensure_contains("0.114", ProfileKind::Module, "a/pkg");
        profile.ensure_contains("0.114", ProfileKind::Plugin, "other/p");
        assert!(profile.remove_from_all_minors("a/pkg"));
        assert!(profile.set_for_minor("0.113").is_empty());
        assert_eq!(
            profile.set_for_minor("0.114").plugins,
            vec!["other/p".to_string()]
        );
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let mut profile = ActivationProfile::new();
        profile.ensure_contains("0.114", ProfileKind::Plugin, "x/y");
        profile.save(root).unwrap();
        let loaded = ActivationProfile::load(root).unwrap().unwrap();
        assert_eq!(loaded, profile);
    }

    #[test]
    fn nu_minor_key_from_version_parses() {
        assert_eq!(nu_minor_key_from_version("0.114.1").unwrap(), "0.114");
    }

    #[test]
    fn load_rejects_unsupported_schema_version() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let path = ActivationProfile::profile_path(root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"schema_version":999,"by_nu_minor":{}}"#).unwrap();

        let err = ActivationProfile::load(root).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("schema_version"),
            "error must mention schema_version: {msg}"
        );
    }

    #[test]
    fn load_rejects_malformed_json() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let path = ActivationProfile::profile_path(root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"invalid json"#).unwrap();

        let err = ActivationProfile::load(root).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Malformed activation profile"),
            "error must contain 'Malformed activation profile': {msg}"
        );
    }
}
