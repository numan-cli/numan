use anyhow::{Context, Result};
use clap::Parser;
use std::path::Path;

use crate::cmd::nu_pin_offer;
use crate::core::nu_version::NuVersion;
use crate::core::package::ScopedId;
use crate::core::platform::Platform;
use crate::core::registry::RegistryManager;
use crate::core::resolve::Resolver;
use crate::install::transaction;
use crate::util::fs_safety::acquire_mutation_lock;

/// Install a package
#[derive(Parser)]
pub struct InstallArgs {
    /// Package to install (owner/name or owner/name@version)
    package: String,

    /// Force reinstall even if already installed
    #[arg(long)]
    force: bool,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

pub fn execute(args: &InstallArgs, root: &Path) -> Result<()> {
    let platform = Platform::detect();
    let mut nu_version = detect_nu(root);

    match install_once(args, root, &platform, &nu_version) {
        Ok(()) => Ok(()),
        Err(first_err) => {
            let diagnosis = match diagnose_target(root, &args.package, &platform, &nu_version) {
                Ok(Some(d)) => d,
                Ok(None) | Err(_) => return Err(first_err),
            };
            if !nu_pin_offer::is_nu_mismatch(&diagnosis) || diagnosis.suggested_pin.is_none() {
                return Err(first_err);
            }

            eprintln!("Error: {first_err:#}");
            let accepted =
                nu_pin_offer::offer_managed_nu_pin(root, &nu_version.version, &diagnosis)?;
            if !accepted {
                return Err(first_err);
            }

            nu_version = detect_nu(root);
            install_once(args, root, &platform, &nu_version)
        }
    }
}

fn install_once(
    args: &InstallArgs,
    root: &Path,
    platform: &Platform,
    nu_version: &NuVersion,
) -> Result<()> {
    let _lock = acquire_mutation_lock(root)?;
    let root_buf = root.to_path_buf();

    let options = transaction::InstallOptions {
        root: &root_buf,
        platform,
        nu_version,
        force: args.force,
        verbose: args.verbose,
        registry_name: None,
        snapshot_trigger: crate::state::snapshot::SnapshotTrigger::Install,
    };

    let version = if args.package.contains('@') {
        Some(args.package.split('@').nth(1).unwrap_or(""))
    } else {
        None
    };

    let package_id = args.package.split('@').next().unwrap_or(&args.package);

    transaction::install_package(package_id, version, &options)?;
    Ok(())
}

fn diagnose_target(
    root: &Path,
    package_spec: &str,
    platform: &Platform,
    nu_version: &NuVersion,
) -> Result<Option<crate::core::resolve::PackageIncompatibility>> {
    let package_id = package_spec.split('@').next().unwrap_or(package_spec);
    let id = ScopedId::parse(package_id)?;
    let registry = RegistryManager::new(root)?;
    let loaded = registry
        .load_verified(&registry.default_registry_name())
        .context("Failed to load registry for compatibility diagnosis")?;
    let Some(pkg) = loaded
        .index
        .packages
        .iter()
        .find(|p| p.id.to_string() == id.to_string())
    else {
        return Ok(None);
    };

    let resolver = Resolver::new(platform, nu_version);
    if let Some(ver_str) = package_spec.split('@').nth(1) {
        if let Ok(target) = ver_str.parse::<semver::Version>() {
            if let Some(entry) = pkg.versions.iter().find(|v| v.version == target) {
                if let Some(issue) = resolver.classify_version(entry) {
                    return Ok(Some(crate::core::resolve::PackageIncompatibility {
                        issue,
                        suggested_pin: crate::core::resolve::suggest_managed_nu_pin(entry),
                        available_versions: pkg
                            .versions
                            .iter()
                            .map(|v| v.version.to_string())
                            .collect(),
                    }));
                }
                return Ok(None);
            }
        }
    }

    if resolver.has_compatible_version(pkg) {
        return Ok(None);
    }

    Ok(Some(resolver.diagnose_package(pkg)))
}

fn detect_nu(root: &Path) -> NuVersion {
    NuVersion::from_paths_or_detect(root).unwrap_or_else(|e| {
        eprintln!("Warning: Could not detect Nu version: {e}");
        NuVersion {
            version: "unknown".to_string(),
            major: 0,
            minor: 0,
            patch: 0,
        }
    })
}
