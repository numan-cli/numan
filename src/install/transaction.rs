use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::core::integrity;
use crate::core::nu_version::NuVersion;
use crate::core::package::{ModuleImportMode, PackageType, ScopedId};
use crate::core::platform::Platform;
use crate::core::registry::RegistryManager;
use crate::core::resolve::Resolver;
use crate::install::download;
use crate::install::extract::{self, ArchiveFormat, ExtractConfig};
use crate::state::lockfile::{Lockfile, LockfileEntry};
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::hints::{self, CMD_REGISTRY_SYNC};
use std::collections::BTreeMap;

pub struct InstallOptions<'a> {
    pub root: &'a PathBuf,
    pub platform: &'a Platform,
    pub nu_version: &'a NuVersion,
    pub force: bool,
    pub verbose: bool,
    /// Registry to install from. Defaults to the configured default registry.
    pub registry_name: Option<&'a str>,
    /// Lifecycle boundary recorded on the pre-mutation snapshot.
    pub snapshot_trigger: SnapshotTrigger,
}

#[derive(Debug)]
pub struct InstallResult {
    pub installed: bool,
    pub package: String,
    pub version: String,
    pub path: PathBuf,
    pub already_existed: bool,
}

/// Verified index metadata returned alongside the index.
pub struct VerifiedIndex {
    pub index: crate::core::package::RegistryIndex,
    pub registry_name: String,
    pub index_sha256: String,
    pub signing_key_fingerprint: Option<String>,
}

pub fn install_package(
    package_id: &str,
    version: Option<&str>,
    options: &InstallOptions,
) -> Result<InstallResult> {
    // 1. Parse package ID
    let id = ScopedId::parse(package_id)?;

    // 2. Load registry with signature verification
    let registry = RegistryManager::new(options.root)?;
    let registry_name = options
        .registry_name
        .map(str::to_string)
        .unwrap_or_else(|| registry.default_registry_name());
    let loaded = registry.load_verified(&registry_name)?;
    let verified = VerifiedIndex {
        index: loaded.index,
        registry_name: loaded.registry_name,
        index_sha256: loaded.index_sha256,
        signing_key_fingerprint: loaded.signing_key_fingerprint,
    };

    let pkg = verified
        .index
        .packages
        .iter()
        .find(|p| p.id.to_string() == id.to_string())
        .with_context(|| {
            format!(
                "Package '{}' not found in registry. {}",
                id,
                hints::run(CMD_REGISTRY_SYNC)
            )
        })?;

    // 3. Resolve version with compatibility validation
    let resolver = Resolver::new(options.platform, options.nu_version);
    let resolved = if let Some(ver_str) = version {
        let target_version: semver::Version = ver_str
            .parse()
            .with_context(|| format!("Invalid version: '{ver_str}'"))?;
        resolver.resolve_exact(pkg, &target_version)?
    } else {
        resolver.resolve(pkg)?
    };

    let version_str = resolved.version.to_string();

    // 4. Determine artifact — SHA256 is mandatory
    let (artifact_url, artifact_sha256, executable_path, archive_format) = if resolved.artifact.kind
        == "binary"
    {
        // Plugin: get platform-specific artifact
        let target = resolved
            .artifact
            .targets
            .get(&options.platform.triple)
            .with_context(|| {
                format!(
                    "No binary available for '{}' on {}. {}",
                    id,
                    options.platform.triple,
                    hints::run(CMD_REGISTRY_SYNC)
                )
            })?;

        let fmt = ArchiveFormat::from_url(&target.url)
            .with_context(|| format!("Cannot determine archive format from URL: {}", target.url))?;

        (
            target.url.clone(),
            Some(target.sha256.clone()),
            Some(target.executable_path.clone()),
            fmt,
        )
    } else {
        // Module/script/completion: use artifact.url or target
        let url = resolved
            .artifact
            .url
            .clone()
            .or_else(|| {
                resolved
                    .artifact
                    .targets
                    .values()
                    .next()
                    .map(|t| t.url.clone())
            })
            .with_context(|| format!("No artifact URL for '{}'", id))?;

        let sha = resolved.artifact.sha256.clone().with_context(|| {
            format!(
                "Artifact SHA256 is required for '{}'. Registry entry is missing sha256.",
                id
            )
        })?;

        let fmt = ArchiveFormat::from_url(&url)
            .with_context(|| format!("Cannot determine archive format from URL: {url}"))?;

        (url, Some(sha), None, fmt)
    };

    // 5. Check if already installed
    let mut lockfile = Lockfile::load(options.root)?;
    let lock_key = id.to_string();

    if !options.force {
        if let Some(entry) = lockfile.packages.get(&lock_key) {
            if entry.version == version_str {
                // Use the path stored in the lockfile entry — not a recomputed path
                let pkg_dir = options.root.join(entry.payload_path());
                if pkg_dir.exists() {
                    if options.verbose {
                        println!(
                            "{} {}@{} is already installed",
                            console::style("✓").green(),
                            id,
                            version_str
                        );
                    }
                    return Ok(InstallResult {
                        installed: false,
                        package: lock_key,
                        version: version_str,
                        path: pkg_dir,
                        already_existed: true,
                    });
                }
            }
        }
    }

    // 6. Compute immutable install path using artifact SHA256 (not URL hash)
    let pkg_type_dir = pkg.package_type.dir_name();
    let sha_prefix = artifact_sha256
        .as_deref()
        .map(|s| s[..8.min(s.len())].to_string())
        .unwrap_or_else(|| "no-sha".to_string());
    let version_dir = format!("{version_str}-{sha_prefix}");
    let install_dir = options
        .root
        .join("packages")
        .join(pkg_type_dir)
        .join(&id.owner)
        .join(&id.name)
        .join(&version_dir);

    // 7. Download to .part file, verify, then rename
    let cache_dir = options.root.join("cache/downloads");
    std::fs::create_dir_all(&cache_dir)?;

    // Always use full SHA as cache key — artifact SHA is mandatory for plugins
    let cache_key = artifact_sha256.as_deref().unwrap_or(&sha_prefix);
    let cache_file = cache_dir.join(format!("{cache_key}.bin"));
    let cache_part = cache_dir.join(format!("{cache_key}.part"));

    // Observed archive digest after integrity verification (or a fallback hash
    // when the registry entry has no expected SHA).
    let mut payload_sha256: Option<String> = None;
    let mut need_download = !cache_file.exists() || options.force;

    if !need_download {
        if let Some(ref expected_sha) = artifact_sha256 {
            match integrity::verify_and_report(&cache_file, expected_sha, &lock_key) {
                Ok(digest) => {
                    if options.verbose {
                        println!("{} Using cached download", console::style("✓").green());
                    }
                    payload_sha256 = Some(digest);
                }
                Err(_) => {
                    // One-shot self-heal: drop the bad cache entry and re-download.
                    if options.verbose {
                        println!(
                            "{} Cached download failed integrity; re-downloading",
                            console::style("!").yellow()
                        );
                    }
                    let _ = std::fs::remove_file(&cache_file);
                    if cache_part.exists() {
                        let _ = std::fs::remove_file(&cache_part);
                    }
                    need_download = true;
                }
            }
        } else if options.verbose {
            println!("{} Using cached download", console::style("✓").green());
        }
    }

    if need_download {
        if options.verbose {
            println!(
                "{} Downloading {}@{}...",
                console::style("↓").cyan(),
                id,
                version_str
            );
        }
        download::download_file(&artifact_url, &cache_part)?;
        // Verify before promoting from .part to final; reuse digest as payload hash.
        if let Some(ref expected_sha) = artifact_sha256 {
            payload_sha256 = Some(integrity::verify_and_report(
                &cache_part,
                expected_sha,
                &lock_key,
            )?);
        }
        // Atomic promote
        if cache_file.exists() {
            std::fs::remove_file(&cache_file)?;
        }
        std::fs::rename(&cache_part, &cache_file)?;
    }

    if payload_sha256.is_none() {
        let bytes = std::fs::read(&cache_file).with_context(|| {
            format!("Failed to read cached payload at {}", cache_file.display())
        })?;
        payload_sha256 = Some(integrity::compute_sha256(&bytes));
    }

    // 8. Extract to staging dir on same volume as install target
    let parent_dir = install_dir.parent().unwrap_or(options.root);
    std::fs::create_dir_all(parent_dir)?;
    let tmp_dir = tempfile::tempdir_in(parent_dir).context("Failed to create staging directory")?;

    let extract_config = ExtractConfig {
        archive_root: resolved.artifact.archive_root.clone(),
        include: resolved.artifact.include.clone(),
        entry: resolved.artifact.entry.clone(),
        ..ExtractConfig::default()
    };

    if options.verbose {
        println!(
            "{} Extracting to {}...",
            console::style("📦").cyan(),
            install_dir.display()
        );
    }

    let extract_result =
        extract::extract_archive(&cache_file, tmp_dir.path(), &extract_config, archive_format)?;

    // Validate executable exists for plugins
    if pkg.package_type == PackageType::Plugin {
        if let Some(ref exe_path) = executable_path {
            let expected_path = tmp_dir.path().join(exe_path);
            if !expected_path.exists() {
                // Try without extension (cross-platform)
                let expected_path_no_ext = tmp_dir.path().join(exe_path.trim_end_matches(".exe"));
                if !expected_path_no_ext.exists() {
                    // Check if any file starts with nu_plugin_
                    let has_plugin = extract_result.files.iter().any(|f| {
                        f.file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n.starts_with("nu_plugin_"))
                            .unwrap_or(false)
                    });

                    if !has_plugin {
                        bail!(
                            "Expected executable '{}' not found in archive for '{}'",
                            exe_path,
                            id
                        );
                    }
                }
            }
        }
    }

    // Validate entry point if specified
    if let Some(ref entry_name) = resolved.artifact.entry {
        if !extract_result.entry_found {
            bail!(
                "Expected entry point '{}' not found in archive for '{}'",
                entry_name,
                id
            );
        }
    }

    // 9. Snapshot lockfile before mutation (after download/extraction preflight succeeds).
    // Skip when the lockfile is empty — there is no prior state worth restoring.
    if !Lockfile::load(options.root)?.packages.is_empty() {
        create_snapshot(
            options.root,
            SnapshotReason::PreMutation,
            options.snapshot_trigger,
            None,
            None,
        )?;
    }

    // 10. Atomic move from staging to final location
    // If --force, don't delete — create a new path (immutable)
    if install_dir.exists() && !options.force {
        // Already exists and not force — this shouldn't happen with immutable paths
        // but handle gracefully
        std::fs::remove_dir_all(&install_dir)?;
    }
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if install_dir.exists() {
        // --force with existing dir: remove and replace
        std::fs::remove_dir_all(&install_dir)?;
    }
    std::fs::rename(tmp_dir.path(), &install_dir)?;

    // 11. Write lockfile entry with provenance
    let installed_at = format_timestamp();
    let payload_rel_path = format!(
        "packages/{}/{}/{}/{}/{}",
        pkg_type_dir, id.owner, id.name, version_dir, ""
    )
    .trim_end_matches('/')
    .to_string();

    // Compute payload manifest hash after extraction for drift detection.
    let revision_id = crate::state::lockfile::compute_revision_id(&install_dir);

    // Capture module-specific activation metadata from registry at install time
    // so that activation can proceed without re-querying the registry.
    // Only persist import mode for known "nu-module" kind — unknown/future kinds
    // must not be silently treated as Nu module autoloads.
    let module_import_mode: Option<ModuleImportMode> = resolved
        .activation
        .as_ref()
        .filter(|spec| spec.kind == "nu-module")
        .map(|spec| spec.import.clone());

    let locked_dependencies: BTreeMap<String, String> = resolved
        .dependencies
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let selection_reason = if version.is_some() {
        Some("exact match".to_string())
    } else {
        Some("highest compatible semver".to_string())
    };

    let entry = LockfileEntry {
        version: version_str.clone(),
        package_type: pkg.package_type.to_string(),
        source: resolved.artifact.kind.clone(),
        target: Some(options.platform.triple.clone()),
        artifact_url: Some(artifact_url.clone()),
        artifact_sha256: artifact_sha256.clone(),
        executable_path,
        archive_root: resolved.artifact.archive_root.clone(),
        include: resolved.artifact.include.clone(),
        entry: resolved.artifact.entry.clone(),
        installed_at,
        nu_version_at_install: Some(options.nu_version.version.clone()),
        activation: None,
        registry_url: Some(format!("registry:{}", verified.registry_name)),
        registry_revision: verified.index.registry_revision.clone(),
        index_sha256: Some(verified.index_sha256),
        signing_key_fingerprint: verified.signing_key_fingerprint,
        git_url: None,
        git_rev: None,
        cargo_name: None,
        cargo_lock_sha256: None,
        built_sha256: None,
        payload_path: payload_rel_path,
        revision_id,
        payload_sha256,
        executable_sha256: None,
        selection_reason,
        origin: Some(format!("registry:{}", verified.registry_name)),
        module_activation: None,
        module_import_mode,
        locked_dependencies,
    };

    lockfile.packages.insert(lock_key.clone(), entry);
    lockfile.generated_at = format_timestamp();
    lockfile.nu_version = options.nu_version.version.clone();
    lockfile.platform = options.platform.triple.clone();
    lockfile.save(options.root)?;

    // 12. Print success
    println!(
        "{} Installed {}@{} to {}",
        console::style("✓").green(),
        id,
        version_str,
        install_dir.display()
    );

    if resolved.is_provisional() {
        let reason = resolved.deferral_reason_display();
        println!(
            "{} This package has not been lifecycle-tested. It passed integrity checks. ({reason})",
            console::style("!").yellow()
        );
    }

    Ok(InstallResult {
        installed: true,
        package: lock_key,
        version: version_str,
        path: install_dir,
        already_existed: false,
    })
}

/// Format current timestamp
fn format_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs:016}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_works() {
        let ts = format_timestamp();
        assert_eq!(ts.len(), 16);
        assert!(ts.chars().all(|c| c.is_ascii_digit()));
    }
}
