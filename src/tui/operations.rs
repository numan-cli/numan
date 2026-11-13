//! Workflow-specific adapters. Reviews are advisory; execution revalidates under lock.
use anyhow::{bail, Context, Result};
use clap::Parser;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::cmd::{activate, deactivate, doctor, plugin_lifecycle, remove, update};
use crate::config::Config;
use crate::core::{
    nu_version::NuVersion,
    package::{CompletionKind, Package, PackageType},
    platform::Platform,
    registry::RegistryManager,
    resolve::Resolver,
};
use crate::install::transaction::{self, InstallOptions};
use crate::nu::paths::NuPaths;
use crate::state::{
    lockfile::{Lockfile, LockfileEntry, BUNDLED_NU_ORIGIN},
    snapshot::{SnapshotManifest, SnapshotTrigger},
};
use crate::util::fs_safety::acquire_mutation_lock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Activate,
    Deactivate,
    Update,
    BatchUpdate,
    Remove,
    Scan,
}

#[derive(Debug, Clone)]
pub struct CatalogItem {
    pub registry: String,
    pub package: Package,
    pub index_digest: String,
    pub signing_key: String,
}

#[derive(Debug, Clone)]
pub struct RegistryInfo {
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub cached_packages: usize,
    pub updated_at: Option<String>,
    pub signing_key: Option<String>,
    pub index_digest: Option<String>,
    pub error: Option<String>,
}

pub struct Data {
    pub installed: Lockfile,
    pub nu: Option<NuPaths>,
    pub nu_valid: bool,
    pub catalog: Vec<CatalogItem>,
    pub warnings: Vec<String>,
    pub default_registry: String,
    pub registries: Vec<RegistryInfo>,
    pub snapshots: Vec<SnapshotManifest>,
    pub managed_autoload_file: Option<String>,
}

/// Load only local files. Missing/corrupt data is visible, never initialized or synced.
pub fn load(root: &Path) -> Result<Data> {
    let installed = Lockfile::load(root)?;
    let mut warnings = Vec::new();
    let nu = match NuPaths::load(root) {
        Ok(nu) => Some(nu),
        Err(error) => {
            warnings.push(format!("Nu context unavailable: {error}"));
            None
        }
    };
    let nu_valid = match &nu {
        Some(nu) => match nu.validate_drift() {
            Ok(()) => true,
            Err(error) => {
                warnings.push(format!("STALE Nu context: {error:#}"));
                false
            }
        },
        None => false,
    };
    let config = Config::load(root)?;
    let manager = RegistryManager::new(root)?;
    let mut registries = Vec::new();
    let mut catalog = Vec::new();
    if config.registries.is_empty() {
        let name = config.general.default_registry.clone();
        let url = if name == crate::core::official_registry::OFFICIAL_REGISTRY.name {
            crate::core::official_registry::OFFICIAL_REGISTRY
                .production_url
                .to_string()
        } else {
            String::new()
        };
        match manager.load_verified(&name) {
            Ok(verified) => {
                let key = verified
                    .signing_key_fingerprint
                    .unwrap_or_else(|| "UNSIGNED development override".into());
                registries.push(RegistryInfo {
                    name: name.clone(),
                    url,
                    enabled: true,
                    cached_packages: verified.index.packages.len(),
                    updated_at: Some(verified.index.updated_at.clone()),
                    signing_key: Some(key.clone()),
                    index_digest: Some(verified.index_sha256.clone()),
                    error: None,
                });
                for package in verified.index.packages {
                    catalog.push(CatalogItem {
                        registry: name.clone(),
                        package,
                        index_digest: verified.index_sha256.clone(),
                        signing_key: key.clone(),
                    });
                }
            }
            Err(error) => {
                let msg = format!("{error:#}");
                warnings.push(format!("Cached registry {name}: {msg}"));
                registries.push(RegistryInfo {
                    name,
                    url,
                    enabled: true,
                    cached_packages: 0,
                    updated_at: None,
                    signing_key: None,
                    index_digest: None,
                    error: Some(msg),
                });
            }
        }
    } else {
        let mut names: Vec<String> = config.registries.keys().cloned().collect();
        names.sort();
        for name in names {
            let reg_cfg = &config.registries[&name];
            if !reg_cfg.enabled {
                registries.push(RegistryInfo {
                    name,
                    url: reg_cfg.url.clone(),
                    enabled: false,
                    cached_packages: 0,
                    updated_at: None,
                    signing_key: None,
                    index_digest: None,
                    error: None,
                });
                continue;
            }
            match manager.load_verified(&name) {
                Ok(verified) => {
                    let key = verified
                        .signing_key_fingerprint
                        .unwrap_or_else(|| "UNSIGNED development override".into());
                    registries.push(RegistryInfo {
                        name: name.clone(),
                        url: reg_cfg.url.clone(),
                        enabled: true,
                        cached_packages: verified.index.packages.len(),
                        updated_at: Some(verified.index.updated_at.clone()),
                        signing_key: Some(key.clone()),
                        index_digest: Some(verified.index_sha256.clone()),
                        error: None,
                    });
                    for package in verified.index.packages {
                        catalog.push(CatalogItem {
                            registry: name.clone(),
                            package,
                            index_digest: verified.index_sha256.clone(),
                            signing_key: key.clone(),
                        });
                    }
                }
                Err(error) => {
                    let msg = format!("{error:#}");
                    warnings.push(format!("Cached registry {name}: {msg}"));
                    registries.push(RegistryInfo {
                        name,
                        url: reg_cfg.url.clone(),
                        enabled: true,
                        cached_packages: 0,
                        updated_at: None,
                        signing_key: None,
                        index_digest: None,
                        error: Some(msg),
                    });
                }
            }
        }
    }
    catalog.sort_by_key(|item| (item.package.id.to_string(), item.registry.clone()));
    let mut snapshots = match crate::state::snapshot::list_snapshots(root) {
        Ok(list) => list,
        Err(error) => {
            warnings.push(format!("Snapshots: {error:#}"));
            Vec::new()
        }
    };
    snapshots.reverse();
    let managed_autoload_file = nu
        .as_ref()
        .and_then(|paths| crate::cmd::activate::resolve_managed_file_path(paths).ok());
    Ok(Data {
        installed,
        nu,
        nu_valid,
        catalog,
        warnings,
        default_registry: config.general.default_registry,
        registries,
        snapshots,
        managed_autoload_file,
    })
}

pub fn has_any_activation(entry: &LockfileEntry) -> bool {
    entry.activation.is_some()
        || entry.module_activation.is_some()
        || entry.script_activation.is_some()
        || entry.completion_activation.is_some()
}

pub fn is_entry_activatable(entry: &LockfileEntry) -> bool {
    if !entry.locked_dependencies.is_empty() {
        return false;
    }
    match entry.package_type.as_str() {
        "plugin" | "module" => true,
        "script" => entry.is_script_activatable(),
        "completion" => entry.is_completion_activatable(),
        _ => false,
    }
}

pub fn activation(entry: &LockfileEntry, nu: Option<&NuPaths>) -> &'static str {
    if !matches!(
        entry.package_type.as_str(),
        "plugin" | "module" | "script" | "completion"
    ) {
        return "unsupported";
    }
    let Some(nu) = nu else {
        return "unknown";
    };
    let active = if entry.package_type == "plugin" {
        entry.is_active_for(
            &nu.nu_executable_hash,
            &nu.nu_version,
            &nu.plugin_registry_path,
        )
    } else {
        nu.vendor_autoload_dir.as_ref().is_some_and(|dir| {
            crate::cmd::activate::resolve_managed_file_path(nu).is_ok_and(|path| {
                entry.is_module_active_for(&nu.nu_executable_hash, &nu.nu_version, dir, &path)
                    || entry.is_script_active_for(
                        &nu.nu_executable_hash,
                        &nu.nu_version,
                        dir,
                        &path,
                    )
                    || entry.is_completion_active_for(
                        &nu.nu_executable_hash,
                        &nu.nu_version,
                        dir,
                        &path,
                    )
            })
        })
    };
    if active {
        "active"
    } else if has_any_activation(entry) {
        "other Nu"
    } else {
        "inactive"
    }
}

/// Why a package type never gets an Activate step, for display before the
/// user commits to an install they may expect to activate afterwards.
pub fn install_only_note(package_type: &PackageType) -> Option<&'static str> {
    match package_type {
        PackageType::Script => Some(
            "Install-only: this script declares no entry points, so it cannot be activated. \
             Install only downloads the payload; run or source it from its payload path afterwards.",
        ),
        PackageType::Completion => Some(
            "Install-only: this completion package declares no targets, so it cannot be \
             activated. Install only downloads the payload; source it from your shell \
             configuration afterwards.",
        ),
        PackageType::Plugin | PackageType::Module => None,
    }
}

/// Install-only note for a concrete catalog package: script packages
/// that declare `nu-script` entry points can be activated (issue #14),
/// and completion packages that declare `nu-completion` targets can be
/// activated (issue #15), so the note applies only when no version
/// declares the relevant activation surface.
pub fn install_only_note_for_package(
    package: &crate::core::package::Package,
) -> Option<&'static str> {
    if package.package_type == PackageType::Script {
        let declared = package.versions.iter().any(|v| {
            v.activation
                .as_ref()
                .map(|spec| spec.kind == "nu-script" && !spec.entry_points.is_empty())
                .unwrap_or(false)
        });
        if declared {
            return None;
        }
    }
    if package.package_type == PackageType::Completion {
        let declared = package.versions.iter().any(|v| {
            v.activation
                .as_ref()
                .map(|spec| spec.kind == "nu-completion" && !spec.targets.is_empty())
                .unwrap_or(false)
        });
        if declared {
            return None;
        }
    }
    install_only_note(&package.package_type)
}

pub fn candidate(item: &CatalogItem, nu: Option<&NuPaths>) -> Option<String> {
    let nu = NuVersion::parse(&nu?.nu_version).ok()?;
    let platform = Platform::detect();
    Resolver::new(&platform, &nu)
        .latest_compatible(&item.package)
        .map(|v| v.version.to_string())
}

pub fn registry_for(entry: &LockfileEntry, default: &str) -> String {
    entry
        .origin
        .as_deref()
        .or(entry.registry_url.as_deref())
        .and_then(|value| value.strip_prefix("registry:"))
        .unwrap_or(default)
        .to_string()
}

#[derive(Debug, Clone)]
pub struct Review {
    pub action: Action,
    pub id: String,
    pub registry: String,
    pub version: String,
    pub effects: Vec<String>,
    pub targets: Vec<(String, String, String)>,
    fingerprint: String,
}

const JOURNALS: &[&str] = &[
    "pending-activation.json",
    "pending-plugin-deactivate.json",
    "pending-autoload.json",
    "pending-lifecycle.json",
    "migration-journal.json",
];

fn ensure_no_pending(root: &Path) -> Result<()> {
    for name in JOURNALS {
        if root.join("state").join(name).exists() {
            bail!("Pending recovery ({name}); use the CLI recovery workflow before another TUI mutation.");
        }
    }
    Ok(())
}

fn hash_review_path(path: &Path, hash: &mut Sha256) -> Result<()> {
    hash.update(path.to_string_lossy().as_bytes());
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            hash.update(b"missing");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        bail!("Refusing linked review input: {}", path.display());
    }
    if path.is_dir() {
        let mut children: Vec<PathBuf> = std::fs::read_dir(path)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        children.sort();
        for child in children {
            hash_review_path(&child, hash)?;
        }
    } else {
        let bytes = std::fs::read(path)?;
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(())
}

fn fingerprint_non_lockfile(root: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    for name in [
        "config.toml",
        "nu_state",
        "registry",
        "state/activation-profile.json",
    ] {
        hash_review_path(&root.join(name), &mut hash)?;
    }
    for name in [
        "NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION",
        "NUMAN_ALLOW_UNSIGNED",
    ] {
        hash.update(name.as_bytes());
        hash.update(std::env::var(name).unwrap_or_default().as_bytes());
    }
    Ok(hex::encode(hash.finalize()))
}

/// Excludes the lock file itself, downloads and snapshots. These are not review inputs.
fn fingerprint(root: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    hash_review_path(&root.join("lockfile"), &mut hash)?;
    hash.update(fingerprint_non_lockfile(root)?.as_bytes());
    Ok(hex::encode(hash.finalize()))
}

pub fn prepare(
    root: &Path,
    action: Action,
    id: &str,
    registry: &str,
    pinned: Option<&str>,
) -> Result<Review> {
    if action == Action::BatchUpdate {
        return prepare_batch_update(root);
    }
    if !root.is_dir() {
        bail!("Root does not exist. Run numan init outside the TUI first.");
    }
    ensure_no_pending(root)?;
    let before = fingerprint(root)?;
    let data = load(root)?;
    let nu = data.nu.as_ref();
    if action != Action::Remove {
        nu.context("Initialize Nu context before modifying packages.")?
            .validate_drift()?;
    }
    crate::core::package::ScopedId::parse(id)?;
    let entry = data.installed.packages.get(id);
    let mut effects = vec![format!("Root: {}", root.display())];
    if let Some(nu) = nu {
        effects.push(format!("Nu: {} ({})", nu.nu_version, nu.nu_executable));
    }
    let version = match action {
        Action::Install | Action::Update => {
            if action == Action::Install && entry.is_some() {
                bail!("Already installed; use Updates. Reinstall is CLI only.");
            }
            if action == Action::Update && entry.is_none() {
                bail!("Package is not installed.");
            }
            let item = data
                .catalog
                .iter()
                .find(|item| item.registry == registry && item.package.id.to_string() == id)
                .context("Package is unavailable in the verified cached registry.")?;
            let platform = Platform::detect();
            let nu_version = NuVersion::parse(
                &nu.context("Initialize Nu context before modifying packages.")?
                    .nu_version,
            )?;
            let resolver = Resolver::new(&platform, &nu_version);
            let selected = if let Some(version) = pinned {
                resolver.resolve_exact(&item.package, &version.parse()?)?
            } else {
                resolver.resolve(&item.package)?
            };
            let version = selected.version.to_string();
            let (url, digest) = if selected.artifact.kind == "binary" {
                let artifact = selected
                    .artifact
                    .targets
                    .get(&platform.triple)
                    .context("No artifact for this platform.")?;
                (artifact.url.as_str(), artifact.sha256.as_str())
            } else {
                (
                    selected
                        .artifact
                        .url
                        .as_deref()
                        .context("This archive needs explicit target inspection via CLI.")?,
                    selected
                        .artifact
                        .sha256
                        .as_deref()
                        .context("Artifact SHA256 is missing.")?,
                )
            };
            if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
                bail!("Invalid mandatory artifact SHA256.");
            }
            effects.push(format!("Download: {url}\nExpected SHA256: {digest}"));
            if let Some(entry) = entry {
                if registry_for(entry, registry) != registry {
                    bail!("Update registry changed; reload the installed package.");
                }
                if selected.version <= entry.version.parse::<semver::Version>()? {
                    bail!("No newer compatible version in the cached registry.");
                }
                if has_any_activation(entry) {
                    if entry.package_type != "plugin"
                        || std::env::var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION").as_deref()
                            != Ok("1")
                    {
                        bail!("Active package update is gated. Deactivate first; active plugin mutation requires NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1.");
                    }
                    effects.push("Active plugin: deactivate, replace, reactivate through existing lifecycle gate.".into());
                }
                if entry.origin.as_deref() == Some(BUNDLED_NU_ORIGIN) {
                    bail!("Bundled Nu plugins are managed with Nushell, not this action.");
                }
            }
            effects.push(format!(
                "{} {id}@{version} from {}",
                if action == Action::Install {
                    "Install"
                } else {
                    "Update"
                },
                item.registry
            ));
            effects.push(format!("Registry key: {}", item.signing_key));
            effects.push(format!("Index SHA256: {}", item.index_digest));
            effects.push(format!(
                "Artifact: {} (mandatory SHA256 verification)",
                selected.artifact.kind
            ));
            if selected.is_provisional() {
                effects.push(format!(
                    "NOT lifecycle tested: {}",
                    selected.deferral_reason_display()
                ));
            }
            effects.push(
                "Downloads and writes package payload/state. Install does not activate.".into(),
            );
            if let Some(note) = install_only_note_for_package(&item.package) {
                effects.push(note.into());
            }
            version
        }
        Action::Activate => {
            let entry = entry.context("Package is not installed.")?;
            if !matches!(
                entry.package_type.as_str(),
                "plugin" | "module" | "script" | "completion"
            ) {
                bail!("Activation is unsupported for this package type.");
            }
            if entry.package_type == "script" && !entry.is_script_activatable() {
                bail!("Script package has no declared entry points and cannot be activated.");
            }
            if entry.package_type == "completion" && !entry.is_completion_activatable() {
                bail!("Completion package has no declared targets and cannot be activated.");
            }
            let nu = nu.context("Initialize Nu context before modifying packages.")?;
            if activation(entry, Some(nu)) == "active" {
                bail!("Already active for the current Nu.");
            }
            if !entry.locked_dependencies.is_empty() {
                bail!("Dependencies require CLI inspection before activation.");
            }
            if entry.package_type == "plugin" {
                effects.push(format!("Register plugin in {}", nu.plugin_registry_path));
            } else {
                let target = nu.vendor_autoload_dir.as_ref().context(
                    "No safe module autoload target. Refresh initialization outside the TUI.",
                )?;
                effects.push(format!(
                    "Replace managed module projection: {}",
                    Path::new(target).join("numan.nu").display()
                ));
            }
            if entry.package_type == "completion" {
                if let Some(targets) = entry.completion_targets.as_ref() {
                    for target in targets {
                        effects.push(format!(
                            "Completion: {} -> {} [{}]",
                            target.command, target.path, target.kind
                        ));
                        if target.kind == CompletionKind::CommandBacked {
                            effects.push(
                                "note: this completion provider can execute code during completion (only when Nushell requests a completion; never at install or activation)"
                                    .into(),
                            );
                        }
                    }
                }
            }
            effects.push(
                "Invokes the selected Nu; affects future Nu sessions, not the running shell."
                    .into(),
            );
            entry.version.clone()
        }
        Action::Deactivate => {
            let entry = entry.context("Package is not installed.")?;
            let nu = nu.context("Initialize Nu context before modifying packages.")?;
            if entry.package_type == "plugin" {
                if activation(entry, Some(nu)) != "active" {
                    bail!(
                        "Plugin '{id}' is not active for the current Nu (deactivate requires active plugin state for current Nu)."
                    );
                }
                effects.push(format!(
                    "Unregister plugin from {}",
                    nu.plugin_registry_path
                ));
            } else if matches!(
                entry.package_type.as_str(),
                "module" | "script" | "completion"
            ) {
                if !has_any_activation(entry) {
                    bail!("Package '{id}' is not currently active.");
                }
                effects.push(format!(
                    "Update managed autoload projection: {}",
                    data.managed_autoload_file.as_deref().unwrap_or("numan.nu")
                ));
            } else {
                bail!("Deactivation is unsupported for this package type.");
            }
            effects
                .push("Keeps installed payload on disk; enables safe removal afterwards.".into());
            effects.push(
                "Invokes the selected Nu / autoload regeneration; affects future Nu sessions."
                    .into(),
            );
            entry.version.clone()
        }
        Action::Remove => {
            let entry = entry.context("Package is not installed.")?;
            if has_any_activation(entry) {
                bail!("Deactivate this package before removal. Force is CLI only.");
            }
            if entry.origin.as_deref() == Some(BUNDLED_NU_ORIGIN) {
                bail!("Bundled plugins must be managed with Nushell.");
            }
            effects.push(format!(
                "DELETE payload {} and lockfile entry {id}",
                root.join(entry.payload_path()).display()
            ));
            effects.push("Snapshots do not guarantee restoration of deleted payloads.".into());
            entry.version.clone()
        }
        Action::Scan => bail!("Doctor scan does not need a mutation review."),
        Action::BatchUpdate => unreachable!("handled at top of prepare"),
    };
    effects.push(
        "Existing lock, snapshot and journal rules apply; cancellation unavailable after submission."
            .into(),
    );
    if fingerprint(root)? != before {
        bail!("State changed while preparing review. Reload and review again.");
    }
    Ok(Review {
        action,
        id: id.into(),
        registry: registry.into(),
        version,
        effects,
        targets: Vec::new(),
        fingerprint: before,
    })
}

pub fn prepare_batch_update(root: &Path) -> Result<Review> {
    if !root.is_dir() {
        bail!("Root does not exist. Run numan init outside the TUI first.");
    }
    ensure_no_pending(root)?;
    let before = fingerprint(root)?;
    let data = load(root)?;
    let nu = data
        .nu
        .as_ref()
        .context("Initialize Nu context before modifying packages.")?;
    nu.validate_drift()?;
    let platform = Platform::detect();
    let nu_version = NuVersion::parse(&nu.nu_version)?;
    let resolver = Resolver::new(&platform, &nu_version);
    let opt_in = std::env::var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION").as_deref() == Ok("1");

    let mut targets = Vec::new();
    let mut effects = vec![
        format!("Root: {}", root.display()),
        format!("Nu: {} ({})", nu.nu_version, nu.nu_executable),
    ];
    let mut skipped = Vec::new();

    for (id, entry) in &data.installed.packages {
        if entry.origin.as_deref() == Some(BUNDLED_NU_ORIGIN) {
            continue;
        }
        let reg_name = registry_for(entry, &data.default_registry);
        let Some(item) = data
            .catalog
            .iter()
            .find(|item| item.registry == reg_name && item.package.id.to_string() == *id)
        else {
            skipped.push(format!("{id} (missing from cached registry '{reg_name}')"));
            continue;
        };
        let selected = match resolver.resolve(&item.package) {
            Ok(selected) => selected,
            Err(error) => {
                skipped.push(format!("{id} (no compatible version: {error})"));
                continue;
            }
        };
        let Ok(current) = entry.version.parse::<semver::Version>() else {
            skipped.push(format!(
                "{id} (invalid installed version '{}')",
                entry.version
            ));
            continue;
        };
        if selected.version <= current {
            continue;
        }
        let (url, digest) = if selected.artifact.kind == "binary" {
            let Some(artifact) = selected.artifact.targets.get(&platform.triple) else {
                skipped.push(format!("{id} (no artifact for platform)"));
                continue;
            };
            if artifact.url.is_empty() {
                skipped.push(format!("{id} (missing artifact URL)"));
                continue;
            }
            (artifact.url.as_str(), artifact.sha256.as_str())
        } else {
            let Some(url) = selected.artifact.url.as_deref() else {
                skipped.push(format!("{id} (needs CLI archive target)"));
                continue;
            };
            if url.is_empty() {
                skipped.push(format!("{id} (missing artifact URL)"));
                continue;
            }
            let Some(sha) = selected.artifact.sha256.as_deref() else {
                skipped.push(format!("{id} (missing artifact SHA256)"));
                continue;
            };
            (url, sha)
        };
        if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
            skipped.push(format!("{id} (invalid artifact SHA256)"));
            continue;
        }
        if entry.hook_activation.is_some() {
            skipped.push(format!(
                "{id} (active hooks: run `numan hook disable {id}`)"
            ));
            continue;
        }
        if let Err(error) =
            crate::state::overlay_state::OverlayState::ensure_package_mutable(root, id)
        {
            skipped.push(format!("{id} ({error})"));
            continue;
        }
        let active = has_any_activation(entry);
        if active {
            if entry.package_type != "plugin" {
                skipped.push(format!("{id} (active autoload package: deactivate first)"));
                continue;
            }
            if !opt_in {
                skipped.push(format!(
                    "{id} (active plugin: set NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1 or deactivate first)"
                ));
                continue;
            }
            if activation(entry, data.nu.as_ref()) != "active" {
                skipped.push(format!("{id} (active plugin belongs to another Nu)"));
                continue;
            }
        }
        let next_ver = selected.version.to_string();
        effects.push(format!(
            "Update {id}: {} -> {next_ver} (from {}, URL: {url}, SHA256: {digest})",
            entry.version, item.registry
        ));
        if active {
            effects.push(
                "  Active plugin: deactivate, replace, reactivate through existing lifecycle gate."
                    .into(),
            );
        }
        if selected.is_provisional() {
            effects.push(format!(
                "  NOT lifecycle tested ({id}): {}",
                selected.deferral_reason_display()
            ));
        }
        targets.push((id.clone(), entry.version.clone(), next_ver));
    }

    if targets.is_empty() {
        if skipped.is_empty() {
            bail!("All installed packages are already up to date with cached metadata.");
        }
        bail!("No eligible updates. Skipped: {}", skipped.join("; "));
    }

    if !skipped.is_empty() {
        effects.push(format!("Skipped: {}", skipped.join("; ")));
    }
    effects.push(
        "Existing lock, snapshot and journal rules apply per updated package; cancellation unavailable after submission."
            .into(),
    );
    if fingerprint(root)? != before {
        bail!("State changed while preparing batch review. Reload and review again.");
    }
    Ok(Review {
        action: Action::BatchUpdate,
        id: format!("{} package(s)", targets.len()),
        registry: data.default_registry,
        version: "batch".into(),
        effects,
        targets,
        fingerprint: before,
    })
}

pub(super) fn validate(root: &Path, review: &Review) -> Result<()> {
    ensure_no_pending(root)?;
    if fingerprint(root)? != review.fingerprint {
        bail!("State changed since review. Reload and confirm a new review.");
    }
    if review.action != Action::Remove {
        NuPaths::load(root)?.validate_drift()?;
    }
    Ok(())
}

pub fn execute(root: &Path, review: &Review) -> Result<String> {
    execute_with_lifecycle(
        root,
        review,
        &activate::execute_under_lock,
        &deactivate::execute_reviewed,
    )
}

pub(super) type ActivateFn<'a> =
    &'a dyn Fn(&activate::ActivateArgs, &Path, &crate::util::fs_safety::MutationLock) -> Result<()>;

pub(super) type DeactivateFn<'a> =
    &'a dyn Fn(&deactivate::DeactivateArgs, &Path, &dyn Fn() -> Result<()>) -> Result<()>;

#[allow(dead_code)]
pub(super) fn execute_with_activation(
    root: &Path,
    review: &Review,
    activate_fn: ActivateFn<'_>,
) -> Result<String> {
    execute_with_lifecycle(root, review, activate_fn, &deactivate::execute_reviewed)
}

pub(super) fn execute_with_lifecycle(
    root: &Path,
    review: &Review,
    activate_fn: ActivateFn<'_>,
    deactivate_fn: DeactivateFn<'_>,
) -> Result<String> {
    let validate = || validate(root, review);
    match review.action {
        Action::Install => {
            let _lock = acquire_mutation_lock(root)?;
            validate()?;
            let paths = NuPaths::load(root)?;
            let nu_version = NuVersion::parse(&paths.nu_version)?;
            let platform = Platform::detect();
            let root_buf = root.to_path_buf();
            let options = InstallOptions {
                root: &root_buf,
                platform: &platform,
                nu_version: &nu_version,
                force: false,
                verbose: true,
                registry_name: Some(&review.registry),
                snapshot_trigger: SnapshotTrigger::Install,
            };
            transaction::install_package(&review.id, Some(&review.version), &options)?;
        }
        Action::Activate => {
            let lock = acquire_mutation_lock(root)?;
            validate()?;
            activate_fn(
                &activate::ActivateArgs {
                    packages: vec![review.id.clone()],
                    verbose: true,
                    list: false,
                    check: false,
                },
                root,
                &lock,
            )?;
        }
        Action::Deactivate => {
            deactivate_fn(
                &deactivate::DeactivateArgs {
                    packages: vec![review.id.clone()],
                    verbose: true,
                },
                root,
                &validate,
            )?;
        }
        Action::Update => {
            let args = update::UpdateArgs::try_parse_from(["update", &review.id, "--verbose"])?;
            let hooks = update::UpdateHooks {
                lifecycle: &plugin_lifecycle::CommandPluginLifecycle,
                install: None,
            };
            update::execute_reviewed(&args, root, &hooks, &validate)?;
        }
        Action::BatchUpdate => {
            let expected_non_lockfile = fingerprint_non_lockfile(root)?;
            let hooks = update::UpdateHooks {
                lifecycle: &plugin_lifecycle::CommandPluginLifecycle,
                install: None,
            };
            for (idx, (id, from_ver, _)) in review.targets.iter().enumerate() {
                let args =
                    update::UpdateArgs::try_parse_from(["update", id, "--no-sync", "--verbose"])?;
                if idx == 0 {
                    update::execute_reviewed(&args, root, &hooks, &validate)?;
                } else {
                    let item_validate = || -> Result<()> {
                        ensure_no_pending(root)?;
                        NuPaths::load(root)?.validate_drift()?;
                        if fingerprint_non_lockfile(root)? != expected_non_lockfile {
                            bail!(
                                "State changed since review. Reload and confirm remaining updates."
                            );
                        }
                        let lockfile = Lockfile::load(root)?;
                        let actual = lockfile
                            .packages
                            .get(id)
                            .map(|entry| entry.version.as_str());
                        if actual != Some(from_ver.as_str()) {
                            bail!("Package '{id}' version changed before batch update step; reload and review remaining updates.");
                        }
                        Ok(())
                    };
                    update::execute_reviewed(&args, root, &hooks, &item_validate)?;
                }
            }
        }
        Action::Remove => {
            let args = remove::RemoveArgs::try_parse_from(["remove", &review.id, "--yes"])?;
            remove::execute_reviewed(&args, root, false, &validate)?;
        }
        Action::Scan => bail!("Use the scan query."),
    }
    // Read back the contract rather than inferring state from printed success messages.
    let data = load(root)?;
    match review.action {
        Action::Install | Action::Update => {
            let entry = data
                .installed
                .packages
                .get(&review.id)
                .context("Operation returned without an installed entry.")?;
            if entry.version != review.version {
                bail!("Installed version differs from reviewed version; inspect state before retrying.");
            }
        }
        Action::BatchUpdate => {
            for (id, _, to_ver) in &review.targets {
                let entry = data
                    .installed
                    .packages
                    .get(id)
                    .with_context(|| format!("Updated entry '{id}' disappeared."))?;
                if entry.version != *to_ver {
                    bail!(
                        "Updated version of '{id}' ({}) differs from reviewed target ({to_ver}).",
                        entry.version
                    );
                }
            }
        }
        Action::Activate => {
            let entry = data
                .installed
                .packages
                .get(&review.id)
                .context("Activated entry disappeared.")?;
            if activation(entry, data.nu.as_ref()) != "active" {
                bail!("Activation did not establish active state for current Nu.");
            }
        }
        Action::Deactivate => {
            let entry = data
                .installed
                .packages
                .get(&review.id)
                .context("Deactivated entry disappeared.")?;
            let still_active = if entry.package_type == "plugin" {
                activation(entry, data.nu.as_ref()) == "active"
            } else {
                has_any_activation(entry)
            };
            if still_active {
                bail!("Deactivation did not clear active state.");
            }
        }
        Action::Remove => {
            if data.installed.packages.contains_key(&review.id) {
                bail!("Removed entry is still present.");
            }
        }
        Action::Scan => {}
    }
    Ok(format!(
        "{:?} completed; authoritative state reloaded.",
        review.action
    ))
}

pub fn scan(root: &Path) -> Result<String> {
    let args = doctor::DoctorArgs {
        scan: true,
        json: false,
        nupm_home: None,
    };
    let options = doctor::DoctorOptions {
        skip_network: true,
        skip_stdio_redirect: true,
        ..Default::default()
    };
    let report = doctor::run_checks_with_options(&args, root, &options)?;
    Ok(format_scan(report))
}

fn format_scan(report: doctor::DoctorReport) -> String {
    let mut lines = vec![format!(
        "Local doctor scan: {} errors, {} warnings. No repairs or network probes.",
        report.summary.errors, report.summary.warnings
    )];
    for finding in report.findings {
        lines.push(format!(
            "{:?} {}: {}",
            finding.severity, finding.id, finding.message
        ));
        if let Some(fix) = finding.fix {
            lines.push(format!("  CLI only: {fix}"));
        }
    }
    lines.join("\n")
}

/// Execute a strictly read-only CLI command and capture its stdout/stderr text.
/// Mutating, network, and repair variants refuse here; they go through the reviewed executor.
pub fn execute_read_only(root: &Path, argv: &[String]) -> Result<String> {
    use crate::cli::{Cli, Commands};
    use crate::cmd::setup::SetupCommands;
    let parsed =
        Cli::try_parse_from(std::iter::once("numan".to_string()).chain(argv.iter().cloned()))?;
    let mut out: Vec<u8> = Vec::new();
    let mut direct_json = false;
    let (res, messages) = crate::util::output::tee_messages(|| -> Result<Option<String>> {
        match parsed.command {
            Some(Commands::Search(args)) => crate::cmd::search::execute_to(&args, root, &mut out)?,
            Some(Commands::Info { id }) => crate::cmd::info::execute_to(&id, root, &mut out)?,
            Some(Commands::List) => crate::cmd::list::execute_to(root, &mut out)?,
            Some(Commands::Registry(cmd)) => {
                crate::cmd::registry::execute_to(&cmd, root, &mut out)?
            }
            Some(Commands::Snapshot(
                ref cmd @ (crate::cmd::snapshot::SnapshotCommands::List
                | crate::cmd::snapshot::SnapshotCommands::Inspect { .. }),
            )) => crate::cmd::snapshot::execute_to(cmd, root, &mut out)?,
            Some(Commands::Nupm(args)) => match &args.command {
                crate::cmd::nupm::NupmCommands::Import(_) => {
                    bail!("nupm import is not read-only; it requires the reviewed executor.")
                }
                _ => crate::cmd::nupm::execute(&args, root, &mut out)?,
            },
            Some(Commands::Hook(crate::cmd::hook::HookCommands::List)) => {
                crate::cmd::hook::list_to(root, &mut out)?
            }
            Some(Commands::Overlay(crate::cmd::overlay::OverlayCommands::List(args)))
                if args.session.is_none() =>
            {
                crate::cmd::overlay::list_overlays_to(root, &mut out)?
            }
            Some(Commands::Use(args)) if args.version == "list" => {
                crate::cmd::use_cmd::list_to(root, &mut out)?
            }
            Some(Commands::Completions(args)) if args.print => {
                return Ok(Some(crate::cmd::completions::generate_script(args.shell)?));
            }
            Some(Commands::Doctor(args)) if args.scan => {
                let doctor_args = doctor::DoctorArgs {
                    scan: true,
                    json: args.json,
                    nupm_home: args.nupm_home,
                };
                let options = doctor::DoctorOptions {
                    skip_network: true,
                    skip_stdio_redirect: true,
                    ..Default::default()
                };
                let report = doctor::run_checks_with_options(&doctor_args, root, &options)?;
                if args.json {
                    direct_json = true;
                    return Ok(Some(serde_json::to_string_pretty(&report)?));
                }
                return Ok(Some(format_scan(report)));
            }
            Some(Commands::Activate(args)) if args.list || args.check => {
                crate::cmd::activate::execute(&args, root)?;
            }
            Some(Commands::Update(args))
                if args.check && !args.sync && (!args.all || args.no_sync) =>
            {
                update::execute(&args, root)?;
            }
            Some(Commands::Gc(args)) if args.dry_run => {
                crate::cmd::gc::execute(&args, root)?;
            }
            Some(Commands::Migrate(args)) if args.dry_run => {
                crate::cmd::migrate::execute(&args, root, &mut out)?;
            }
            Some(Commands::Setup(SetupCommands::Loader(args))) if args.is_pure_status() => {
                crate::cmd::setup::execute_loader(&args, root)?;
            }
            Some(Commands::Setup(SetupCommands::Autoupdate(args)))
                if args.status && !args.enable && !args.disable && !args.run_now =>
            {
                crate::cmd::setup::execute_autoupdate(&args, root)?;
            }
            _ => bail!("This command is not read-only; it requires the reviewed executor."),
        }
        Ok(None)
    });
    if let Some(direct) = res? {
        if direct_json || messages.is_empty() {
            return Ok(direct);
        }
        return Ok(format!("{direct}\n{}", messages.join("\n")));
    }
    let mut parts = Vec::new();
    let text = String::from_utf8_lossy(&out).trim_end().to_string();
    if !text.is_empty() {
        parts.push(text);
    }
    if !messages.is_empty() {
        parts.push(messages.join("\n"));
    }
    Ok(if parts.is_empty() {
        "Command completed with no output.".into()
    } else {
        parts.join("\n")
    })
}

fn catalog_display(argv: &[String]) -> Option<String> {
    crate::tui::catalog::catalog()
        .iter()
        .filter(|spec| argv.starts_with(&spec.path))
        .max_by_key(|spec| spec.path.len())
        .map(|spec| spec.display.clone())
}

/// Commands whose CLI handlers prompt for confirmation. The TUI review is
/// that confirmation, so `--yes` is appended after review, never before it.
fn needs_yes(argv: &[String]) -> bool {
    matches!(
        argv.first().map(String::as_str),
        Some("remove" | "migrate" | "setup")
    ) || matches!(
        (
            argv.first().map(String::as_str),
            argv.get(1).map(String::as_str)
        ),
        (Some("snapshot"), Some("delete" | "rollback")) | (Some("nupm"), Some("import"))
    )
}

fn format_cli_result(note: &str, out: &[u8], messages: &[String]) -> String {
    let mut sections = Vec::new();
    let raw = String::from_utf8_lossy(out).trim_end().to_string();
    if !raw.is_empty() {
        sections.push(raw);
    }
    if !messages.is_empty() {
        sections.push(messages.join("\n"));
    }
    sections.push(note.to_string());
    sections.join("\n\n")
}

/// Execute any catalog command from the Commands form or palette.
///
/// Read-only commands delegate to the capturing `execute_read_only` path.
/// Mutating commands run only after review (the caller in `app.rs`), which
/// is why confirmation flags are injected here and never in the form. Each
/// handler keeps its own lock, snapshot, and journal discipline; no outer
/// mutation lock is held, and no shell or subprocess is involved.
pub fn execute_cli(root: &Path, argv: &[String]) -> Result<String> {
    use crate::cli::{Cli, Commands};
    let display = catalog_display(argv).unwrap_or_default();
    let is_read_only = super::catalog::spec(&display).is_some_and(|spec| {
        let rest = argv.get(spec.path.len()..).unwrap_or(&[]);
        let fields = super::catalog::fields_from_argv(spec, rest);
        super::catalog::execution_mode(&display, &fields) == super::catalog::ExecutionMode::ReadOnly
    });
    if is_read_only {
        return execute_read_only(root, argv);
    }

    let mut argv = argv.to_vec();
    if needs_yes(&argv) && !argv.iter().any(|arg| arg == "--yes") {
        if argv.first().map(String::as_str) == Some("setup")
            && argv.get(1).map(String::as_str) == Some("nu")
        {
            argv.insert(2, "--yes".into());
        } else {
            argv.push("--yes".into());
        }
    }

    let parsed =
        Cli::try_parse_from(std::iter::once("numan".to_string()).chain(argv.iter().cloned()))?;
    let note = format!("{display} completed; authoritative state reloaded.");
    let mut out: Vec<u8> = Vec::new();
    let (res, messages) = crate::util::output::tee_messages(|| -> Result<Option<String>> {
        match parsed.command {
            Some(Commands::Tui) => bail!("The TUI cannot open inside itself."),
            Some(Commands::Activate(args)) => {
                if args.list || args.check {
                    crate::cmd::activate::execute(&args, root)?;
                    return Ok(None);
                }
                ensure_no_pending(root)?;
                crate::cmd::activate::execute(&args, root)?;
                Ok(None)
            }
            Some(Commands::Update(args)) => {
                // Self-update replaces the running binary (unsafe on Windows and
                // surprising mid-session); only the read-only --check form runs
                // here. The real update is a terminal handoff.
                if args.self_update && !args.check {
                    bail!(
                        "Self-update replaces the running numan binary; run `numan update --self` in a terminal instead."
                    );
                }
                if args.check && !args.sync && (!args.all || args.no_sync) {
                    crate::cmd::update::execute(&args, root)?;
                    return Ok(None);
                }
                ensure_no_pending(root)?;
                crate::cmd::update::execute(&args, root)?;
                Ok(None)
            }
            Some(Commands::Install(args)) => {
                ensure_no_pending(root)?;
                crate::cmd::install::execute(&args, root)?;
                Ok(None)
            }
            Some(Commands::Remove(args)) => {
                ensure_no_pending(root)?;
                crate::cmd::remove::execute(&args, root)?;
                Ok(None)
            }
            Some(Commands::Gc(args)) => {
                if !args.dry_run {
                    ensure_no_pending(root)?;
                }
                crate::cmd::gc::execute(&args, root)?;
                Ok(None)
            }
            Some(Commands::Deactivate(args)) => {
                ensure_no_pending(root)?;
                crate::cmd::deactivate::execute_reviewed(&args, root, &|| ensure_no_pending(root))?;
                Ok(None)
            }
            Some(Commands::Hook(cmd)) => {
                ensure_no_pending(root)?;
                crate::cmd::hook::execute(cmd, root)?;
                Ok(None)
            }
            Some(Commands::Init(args)) => {
                ensure_no_pending(root)?;
                crate::cmd::init::execute(&args, root)?;
                Ok(None)
            }
            Some(Commands::Registry(cmd)) => {
                ensure_no_pending(root)?;
                crate::cmd::registry::execute(cmd, root)?;
                Ok(None)
            }
            Some(Commands::Snapshot(cmd)) => {
                if !matches!(
                    cmd,
                    crate::cmd::snapshot::SnapshotCommands::List
                        | crate::cmd::snapshot::SnapshotCommands::Inspect { .. }
                ) {
                    ensure_no_pending(root)?;
                }
                crate::cmd::snapshot::execute_to(&cmd, root, &mut out)?;
                Ok(None)
            }
            Some(Commands::Nupm(args)) => {
                ensure_no_pending(root)?;
                crate::cmd::nupm::execute(&args, root, &mut out)?;
                Ok(None)
            }
            Some(Commands::Migrate(args)) => {
                if !args.dry_run {
                    ensure_no_pending(root)?;
                }
                if let Err(err) = crate::cmd::migrate::execute(&args, root, &mut out) {
                    let text = String::from_utf8_lossy(&out).trim_end().to_string();
                    if text.is_empty() {
                        return Err(err);
                    }
                    bail!("{err:#}\n{text}");
                }
                Ok(None)
            }
            Some(Commands::Completions(args)) => {
                crate::cmd::completions::execute(&args)?;
                Ok(Some(format!(
                    "{note} Completions were written outside the Numan root."
                )))
            }
            Some(Commands::Doctor(args)) => {
                let code = crate::cmd::doctor::execute_to_with_options(
                    &args,
                    root,
                    crate::cmd::doctor::DoctorOptions {
                        skip_stdio_redirect: true,
                        ..Default::default()
                    },
                    &mut out,
                )?;
                let text = String::from_utf8_lossy(&out).trim_end().to_string();
                if code != 0 {
                    bail!("Doctor finished with exit code {code}:\n{text}");
                }
                Ok(Some(if text.is_empty() { note.clone() } else { text }))
            }
            Some(Commands::Setup(cmd)) => {
                crate::cmd::setup::execute(cmd, root)?;
                Ok(Some(format!(
                    "{note} Setup may have changed files outside the Numan root."
                )))
            }
            Some(Commands::Try(args)) => {
                ensure_no_pending(root)?;
                crate::cmd::try_cmd::execute(&args, root)?;
                Ok(None)
            }
            Some(Commands::Use(args)) => {
                ensure_no_pending(root)?;
                crate::cmd::use_cmd::execute(&args, root)?;
                Ok(None)
            }
            Some(Commands::Overlay(cmd)) => {
                ensure_no_pending(root)?;
                let needs_shell = matches!(
                    cmd,
                    crate::cmd::overlay::OverlayCommands::Use(_)
                        | crate::cmd::overlay::OverlayCommands::Hide(_)
                );
                crate::cmd::overlay::execute_to(cmd, root, &mut out)?;
                if needs_shell {
                    return Ok(Some(format!(
                        "{} To track overlay use/hide, run `numan init` (or `numan init --refresh` for an existing installation), start an interactive Nushell session, and use its Numan overlay commands.",
                        format_cli_result(&note, &out, &[])
                    )));
                }
                Ok(None)
            }
            // Read-only leaves are routed above; reaching here means the argv
            // classification changed, so delegate rather than strand the command.
            Some(Commands::Search(_)) | Some(Commands::Info { .. }) | Some(Commands::List) => {
                Ok(Some(execute_read_only(root, &argv)?))
            }
            None => bail!("No command selected."),
        }
    });
    match res? {
        Some(custom) => {
            if messages.is_empty() || custom.starts_with('{') || custom.starts_with('[') {
                Ok(custom)
            } else {
                Ok(format!("{}\n\n{custom}", messages.join("\n")))
            }
        }
        None => Ok(format_cli_result(&note, &out, &messages)),
    }
}

/// Explicit network action; never called by view loading or ordinary reload.
pub fn sync(root: &Path) -> Result<String> {
    let (res, messages) = crate::util::output::tee_messages(|| {
        crate::cmd::registry::execute(crate::cmd::registry::RegistryCommands::Sync, root)
    });
    res?;
    let summary = "Registry refresh finished. See per-registry results and any cached fallback; this does not guarantee fresh remote data.";
    if messages.is_empty() {
        Ok(summary.into())
    } else {
        Ok(format!("{}\n\n{summary}", messages.join("\n")))
    }
}
