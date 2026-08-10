use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::core::platform::Platform;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default = "default_registry")]
    pub general: GeneralConfig,

    #[serde(default)]
    pub registries: std::collections::HashMap<String, RegistryConfig>,

    #[serde(default)]
    pub activation: ActivationConfig,

    #[serde(default)]
    pub install: InstallConfig,

    #[serde(default)]
    pub nupm_compat: NupmCompatConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_registry_name")]
    pub default_registry: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            default_registry: "official".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    pub url: String,
    #[serde(default = "default_sync_interval")]
    pub sync_interval: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub trust_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationConfig {
    #[serde(default = "default_method")]
    pub method: String,
}

impl Default for ActivationConfig {
    fn default() -> Self {
        Self {
            method: "autoload".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallConfig {
    #[serde(default = "default_true")]
    pub prefer_binary: bool,
    #[serde(default = "default_true")]
    pub source_pinned: bool,
    #[serde(default = "default_true")]
    pub cache_downloads: bool,
    #[serde(default = "default_true")]
    pub cache_builds: bool,
    #[serde(default = "default_max_cache_size")]
    pub max_cache_size: String,
}

impl Default for InstallConfig {
    fn default() -> Self {
        Self {
            prefer_binary: true,
            source_pinned: true,
            cache_downloads: true,
            cache_builds: true,
            max_cache_size: "500MB".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NupmCompatConfig {
    #[serde(default = "default_true")]
    pub scan_on_doctor: bool,
    #[serde(default = "default_true")]
    pub import_metadata: bool,
}

impl Default for NupmCompatConfig {
    fn default() -> Self {
        Self {
            scan_on_doctor: true,
            import_metadata: true,
        }
    }
}

fn default_registry() -> GeneralConfig {
    GeneralConfig::default()
}

fn default_registry_name() -> String {
    "official".to_string()
}

fn default_sync_interval() -> String {
    "24h".to_string()
}

fn default_true() -> bool {
    true
}

fn default_method() -> String {
    "autoload".to_string()
}

fn default_max_cache_size() -> String {
    "500MB".to_string()
}

impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        let config_path = root.join("config.toml");
        if !config_path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", config_path.display()))?;
        Ok(config)
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let config_path = root.join("config.toml");
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&config_path, content)?;
        Ok(())
    }

    pub fn resolve_root(platform: &Platform) -> PathBuf {
        if let Ok(env_root) = std::env::var("NUMAN_ROOT") {
            return PathBuf::from(env_root);
        }
        platform.default_root()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempdir().unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config.general.default_registry, "official");
        assert_eq!(config.activation.method, "autoload");
        assert!(config.install.prefer_binary);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let mut config = Config::default();
        config.general.default_registry = "custom".to_string();
        config.registries.insert(
            "custom".to_string(),
            RegistryConfig {
                url: "https://example.com/registry".to_string(),
                sync_interval: "12h".to_string(),
                enabled: true,
                trust_key: Some("abc123".to_string()),
            },
        );
        config.save(dir.path()).unwrap();

        let loaded = Config::load(dir.path()).unwrap();
        assert_eq!(loaded.general.default_registry, "custom");
        let registry = loaded.registries.get("custom").unwrap();
        assert_eq!(registry.url, "https://example.com/registry");
        assert_eq!(registry.sync_interval, "12h");
        assert_eq!(registry.trust_key.as_deref(), Some("abc123"));
    }

    #[test]
    fn load_malformed_toml_errors() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "not = [valid toml").unwrap();
        let result = Config::load(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn registry_config_defaults_apply() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[registries.official]\nurl = \"https://example.com\"\n",
        )
        .unwrap();
        let config = Config::load(dir.path()).unwrap();
        let registry = config.registries.get("official").unwrap();
        assert_eq!(registry.sync_interval, "24h");
        assert!(registry.enabled);
        assert!(registry.trust_key.is_none());
    }

    #[test]
    fn resolve_root_prefers_numan_root_env_var() {
        let dir = tempdir().unwrap();
        std::env::set_var("NUMAN_ROOT", dir.path());
        let platform = Platform::detect();
        let resolved = Config::resolve_root(&platform);
        std::env::remove_var("NUMAN_ROOT");
        assert_eq!(resolved, dir.path());
    }
}
