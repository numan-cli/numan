use crate::core::official_registry::RegistrySignature;
use crate::core::registry::{RegistryManager, VerifiedRegistry};
use crate::core::trust::TrustStore;
use crate::util::fs_safety::acquire_mutation_lock;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::io::Write;
use std::path::Path;

#[derive(Subcommand)]
pub enum RegistryCommands {
    /// List configured registries
    List,
    /// Fetch latest index from all registries
    Sync,
    /// Add a custom registry
    Add {
        /// Registry name
        name: String,
        /// Registry index URL
        url: String,
        /// Ed25519 public key (base64)
        #[arg(long)]
        key: String,
    },
    /// Remove a registry
    Remove {
        /// Registry name
        name: String,
    },
    /// List all packages in registry
    Packages,
}

pub fn execute(cmd: RegistryCommands, root: &Path) -> Result<()> {
    match cmd {
        RegistryCommands::List => list_registries(root, &mut std::io::stdout()),
        RegistryCommands::Sync => sync_registries(root),
        RegistryCommands::Add { name, url, key } => add_registry(root, &name, &url, &key),
        RegistryCommands::Remove { name } => remove_registry(root, &name),
        RegistryCommands::Packages => list_packages(root, &mut std::io::stdout()),
    }
}

fn list_registries(root: &Path, out: &mut dyn Write) -> Result<()> {
    let config = crate::config::Config::load(root)?;
    if config.registries.is_empty() {
        writeln!(out, "No registries configured.")?;
        return Ok(());
    }

    writeln!(out, "Configured registries:\n")?;
    for (name, reg) in &config.registries {
        let status = if reg.enabled { "enabled" } else { "disabled" };
        writeln!(out, "  {name}  [{status}]")?;
        writeln!(out, "    url: {}", reg.url)?;
    }

    Ok(())
}

fn sync_registries(root: &Path) -> Result<()> {
    let _lock = acquire_mutation_lock(root)?;
    let config = crate::config::Config::load(root)?;
    let mgr = RegistryManager::new(root)?;

    for (name, reg) in &config.registries {
        if !reg.enabled {
            continue;
        }

        println!("Syncing '{name}' from {}...", reg.url);

        let fetch_result: Result<VerifiedRegistry> = (|| {
            let index_response = reqwest::blocking::get(&reg.url)
                .and_then(|r| r.error_for_status())
                .map_err(|e| anyhow::anyhow!("Failed to fetch registry '{name}': {e}"))?;
            let sig_response = reqwest::blocking::get(format!("{}.sig", reg.url))
                .and_then(|r| r.error_for_status())
                .map_err(|e| anyhow::anyhow!("Failed to fetch signature for '{name}': {e}"))?;

            let index_content = index_response.text()?;
            let sig_content = sig_response.text()?;
            let signature: RegistrySignature = serde_json::from_str(&sig_content)
                .with_context(|| format!("Registry '{name}' signature file is malformed"))?;
            mgr.replace_index(name, &index_content, &signature)
        })();

        let verified = match fetch_result {
            Ok(v) => v,
            Err(e) => {
                // Network fetch or signature validation failed. If a cached
                // verified index exists, use it and warn; otherwise error.
                let cached = mgr.load_verified(name);
                if let Ok(cached) = cached {
                    eprintln!(
                        "Warning: Could not refresh registry '{name}' ({e}); using cached index from {}.",
                        cached.index.updated_at
                    );
                    cached
                } else if let Ok(lkg) = mgr.load_last_known_good(name) {
                    eprintln!(
                        "Warning: Could not refresh registry '{name}' ({e}); using last-known-good index from {}.",
                        lkg.index.updated_at
                    );
                    lkg
                } else {
                    bail!("Failed to sync registry '{name}' and no cached or last-known-good index is available: {e}");
                }
            }
        };

        println!(
            "  Synced '{name}' successfully (key_id: {}, index_sha256: {}).",
            verified.key_id,
            &verified.index_sha256[..8.min(verified.index_sha256.len())]
        );
    }

    Ok(())
}

fn add_registry(root: &Path, name: &str, url: &str, key_b64: &str) -> Result<()> {
    let mut config = crate::config::Config::load(root)?;

    if config.registries.contains_key(name) {
        bail!("Registry '{name}' already exists. Remove it first.");
    }

    // Add key to trust store
    let mut trust = TrustStore::load(root)?;
    let fingerprint = trust.add_key(name, key_b64)?;
    trust.save(root)?;

    // Add registry to config
    config.registries.insert(
        name.to_string(),
        crate::config::RegistryConfig {
            url: url.to_string(),
            sync_interval: "24h".to_string(),
            enabled: true,
            trust_key: Some(key_b64.to_string()),
        },
    );
    config.save(root)?;

    println!("Added registry '{name}'.");
    println!("  URL: {url}");
    println!("  Fingerprint: {fingerprint}");
    println!("\nRun 'numan registry sync' to fetch the index.");

    Ok(())
}

fn remove_registry(root: &Path, name: &str) -> Result<()> {
    let mut config = crate::config::Config::load(root)?;

    if !config.registries.contains_key(name) {
        bail!("Registry '{name}' not found.");
    }

    config.registries.remove(name);
    config.save(root)?;

    // Remove cached index
    let index_dir = root.join(format!("registry/{name}"));
    if index_dir.exists() {
        std::fs::remove_dir_all(index_dir)?;
    }

    println!("Removed registry '{name}'.");
    Ok(())
}

fn list_packages(root: &Path, out: &mut dyn Write) -> Result<()> {
    let config = crate::config::Config::load(root)?;
    let mgr = RegistryManager::new(root)?;

    let default_reg = &config.general.default_registry;
    let index = mgr.load_index(default_reg)?;

    writeln!(
        out,
        "Packages in '{default_reg}' ({}):\n",
        index.packages.len()
    )?;

    let desc_width = package_description_width();
    for (i, pkg) in index.packages.iter().enumerate() {
        if i > 0 {
            writeln!(out)?;
        }
        let latest = pkg
            .versions
            .last()
            .map(|v| v.version.to_string())
            .unwrap_or_else(|| "n/a".to_string());
        let id = format!("{}/{}", pkg.id.owner, pkg.id.name);
        writeln!(
            out,
            "  {}  {}  [{}]",
            console::style(id).cyan().bold(),
            console::style(format!("v{latest}")).dim(),
            console::style(pkg.package_type.to_string()).dim(),
        )?;
        if !pkg.description.trim().is_empty() {
            for line in wrap_words(pkg.description.trim(), desc_width) {
                writeln!(out, "    {}", console::style(line).dim())?;
            }
        }
    }

    Ok(())
}

/// Convert terminal column count to usable description width.
fn terminal_cols_to_description_width(terminal_cols: usize) -> usize {
    let available = terminal_cols.saturating_sub(4);
    if available >= 40 {
        available.max(40)
    } else {
        available
    }
}

/// Usable width for indented package descriptions (leave room for `    ` prefix).
fn package_description_width() -> usize {
    let cols = console::Term::stdout()
        .size_checked()
        .map(|(_, cols)| cols as usize)
        .unwrap_or(80);
    terminal_cols_to_description_width(cols)
}

/// Soft-wrap `text` on whitespace so the terminal does not split mid-word.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
            continue;
        }
        let current_width = console::measure_text_width(&current);
        let word_width = console::measure_text_width(word);
        if current_width + 1 + word_width <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key_b64() -> String {
        let signing_key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signing_key.verifying_key().to_bytes(),
        )
    }

    #[test]
    fn list_registries_prints_none_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        list_registries(dir.path(), &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "No registries configured.\n"
        );
    }

    #[test]
    fn list_registries_prints_configured_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut config = crate::config::Config::default();
        config.registries.insert(
            "custom".to_string(),
            crate::config::RegistryConfig {
                url: "https://example.com/index.json".to_string(),
                sync_interval: "24h".to_string(),
                enabled: true,
                trust_key: None,
            },
        );
        config.save(root).unwrap();
        let mut out = Vec::new();
        list_registries(root, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("custom  [enabled]"));
        assert!(s.contains("url: https://example.com/index.json"));
    }

    #[test]
    fn add_registry_persists_config_and_trust_key() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let key_b64 = test_key_b64();
        add_registry(root, "custom", "https://example.com/index.json", &key_b64).unwrap();

        let config = crate::config::Config::load(root).unwrap();
        assert!(config.registries.contains_key("custom"));
        let trust = TrustStore::load(root).unwrap();
        assert!(trust.keys.contains_key("custom"));
    }

    #[test]
    fn add_registry_rejects_duplicate_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let key_b64 = test_key_b64();
        add_registry(root, "custom", "https://example.com/index.json", &key_b64).unwrap();

        let err =
            add_registry(root, "custom", "https://example.com/other.json", &key_b64).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn remove_registry_removes_config_and_cached_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let key_b64 = test_key_b64();
        add_registry(root, "custom", "https://example.com/index.json", &key_b64).unwrap();
        std::fs::create_dir_all(root.join("registry/custom")).unwrap();

        remove_registry(root, "custom").unwrap();

        let config = crate::config::Config::load(root).unwrap();
        assert!(!config.registries.contains_key("custom"));
        assert!(!root.join("registry/custom").exists());
    }

    #[test]
    fn remove_registry_errors_when_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = remove_registry(dir.path(), "missing").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn list_packages_prints_index_contents() {
        use crate::core::package::{
            Artifact, Package, PackageType, RegistryIndex, ScopedId, VersionEntry,
        };

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("registry/official")).unwrap();

        let index = RegistryIndex {
            schema_version: 1,
            updated_at: "2026-06-27T00:00:00Z".to_string(),
            registry_revision: Some("abc123".to_string()),
            trust: None,
            packages: vec![Package {
                id: ScopedId::new("test", "pkg"),
                description: "A test package for listing".to_string(),
                repo: "https://github.com/test/pkg".to_string(),
                package_type: PackageType::Plugin,
                tags: vec!["test".to_string()],
                versions: vec![VersionEntry {
                    version: semver::Version::new(1, 0, 0),
                    nu_version: ">=0.113.0 <0.114.0".to_string(),
                    verified_with: vec![],
                    artifact: Artifact {
                        kind: "binary".to_string(),
                        url: None,
                        sha256: None,
                        targets: std::collections::HashMap::new(),
                        archive_root: None,
                        include: None,
                        entry: None,
                    },
                    source: None,
                    dependencies: std::collections::BTreeMap::new(),
                    activation: None,
                    provenance: None,
                    evidence_tier: None,
                    deferral_reason: None,
                }],
            }],
        };
        let content = serde_json::to_string_pretty(&index).unwrap();
        std::fs::write(root.join("registry/official/index.json"), content).unwrap();
        std::fs::write(
            root.join("config.toml"),
            "[general]\ndefault_registry = \"official\"\n",
        )
        .unwrap();

        let mut out = Vec::new();
        list_packages(root, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Packages in 'official' (1):"));
        assert!(s.contains("test/pkg"));
        assert!(s.contains("v1.0.0"));
        assert!(s.contains("plugin"));
        assert!(s.contains("A test package for listing"));
    }

    #[test]
    fn wrap_words_keeps_short_text_on_one_line() {
        assert_eq!(
            wrap_words("short description", 40),
            vec!["short description".to_string()]
        );
    }

    #[test]
    fn wrap_words_breaks_on_spaces_not_mid_word() {
        let lines = wrap_words(
            "A Nushell testing framework with commands for running suites",
            30,
        );
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.len() <= 30, "line too long: {line:?}");
            assert!(!line.contains("  "));
        }
        assert_eq!(
            lines.join(" "),
            "A Nushell testing framework with commands for running suites"
        );
    }

    #[test]
    fn wrap_words_handles_empty_and_whitespace() {
        assert!(wrap_words("", 40).is_empty());
        assert_eq!(
            wrap_words("   spaced   out  ", 40),
            vec!["spaced out".to_string()]
        );
    }

    #[test]
    fn terminal_cols_to_description_width_narrow_terminal() {
        assert_eq!(terminal_cols_to_description_width(30), 26);
        assert_eq!(terminal_cols_to_description_width(20), 16);
        assert_eq!(terminal_cols_to_description_width(4), 0);
        assert_eq!(terminal_cols_to_description_width(0), 0);
    }

    #[test]
    fn terminal_cols_to_description_width_applies_minimum_when_spacious() {
        assert_eq!(terminal_cols_to_description_width(44), 40);
        assert_eq!(terminal_cols_to_description_width(50), 46);
        assert_eq!(terminal_cols_to_description_width(80), 76);
    }

    #[test]
    fn wrap_words_non_ascii_display_width() {
        let text = "日本語 test 中文";
        let lines = wrap_words(text, 15);
        for line in &lines {
            let width = console::measure_text_width(line);
            assert!(
                width <= 15,
                "line display width {width} exceeds limit: {line:?}"
            );
        }
        assert_eq!(lines.join(" "), text);
    }
}
