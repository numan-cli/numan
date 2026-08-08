use crate::core::official_registry::RegistrySignature;
use crate::core::registry::{RegistryManager, VerifiedRegistry};
use crate::core::trust::TrustStore;
use crate::util::fs_safety::acquire_mutation_lock;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
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
        RegistryCommands::List => list_registries(root),
        RegistryCommands::Sync => sync_registries(root),
        RegistryCommands::Add { name, url, key } => add_registry(root, &name, &url, &key),
        RegistryCommands::Remove { name } => remove_registry(root, &name),
        RegistryCommands::Packages => list_packages(root),
    }
}

fn list_registries(root: &Path) -> Result<()> {
    let config = crate::config::Config::load(root)?;
    if config.registries.is_empty() {
        println!("No registries configured.");
        return Ok(());
    }

    println!("Configured registries:\n");
    for (name, reg) in &config.registries {
        let status = if reg.enabled { "enabled" } else { "disabled" };
        println!("  {name}  [{status}]");
        println!("    url: {}", reg.url);
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

fn list_packages(root: &Path) -> Result<()> {
    let config = crate::config::Config::load(root)?;
    let mgr = RegistryManager::new(root)?;

    let default_reg = &config.general.default_registry;
    let index = mgr.load_index(default_reg)?;

    println!("Packages in '{default_reg}' ({}):\n", index.packages.len());

    let desc_width = package_description_width();
    for (i, pkg) in index.packages.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let latest = pkg
            .versions
            .last()
            .map(|v| v.version.to_string())
            .unwrap_or_else(|| "n/a".to_string());
        let id = format!("{}/{}", pkg.id.owner, pkg.id.name);
        println!(
            "  {}  {}  [{}]",
            console::style(id).cyan().bold(),
            console::style(format!("v{latest}")).dim(),
            console::style(pkg.package_type.to_string()).dim(),
        );
        if !pkg.description.trim().is_empty() {
            for line in wrap_words(pkg.description.trim(), desc_width) {
                println!("    {}", console::style(line).dim());
            }
        }
    }

    Ok(())
}

fn terminal_cols_to_description_width(terminal_cols: usize) -> usize {
    let available = terminal_cols.saturating_sub(4);
    if available >= 40 {
        available.max(40)
    } else {
        available
    }
}

fn package_description_width() -> usize {
    let cols = console::Term::stdout()
        .size_checked()
        .map(|(_, cols)| cols as usize)
        .unwrap_or(80);
    terminal_cols_to_description_width(cols)
}

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
