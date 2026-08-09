use anyhow::{bail, Context, Result};
use clap::Parser;
use std::path::Path;

use crate::cmd::activate::{self, ActivateArgs};
use crate::cmd::nu_pin_offer;
use crate::core::nu_version::NuVersion;
use crate::core::package::{Package, PackageType};
use crate::core::platform::{Os, Platform};
use crate::core::registry::RegistryManager;
use crate::core::resolve::Resolver;
use crate::install::transaction;
use crate::util::fs_safety::acquire_mutation_lock;
use crate::util::hints::{self, CMD_REGISTRY_SYNC};

/// Install and activate a starter package that fits your current Nu.
///
/// Tries curated starters (e.g., idanarye/nu_plugin_skim for Nu 0.114) matched to
/// your Nu version and platform. If no compatible starter exists, suggests installing
/// a matching managed Nu version or searching for another package with `numan search`.
#[derive(Parser, Debug)]
pub struct TryArgs {
    /// Install only; do not activate
    #[arg(long)]
    pub no_activate: bool,
}

#[derive(Debug, Clone)]
struct StarterSpec {
    id: &'static str,
    /// Optional Nu major.minor the starter targets (None = any).
    nu_minor: Option<(u64, u64)>,
    /// When set, only match this OS.
    os: Option<Os>,
}

/// Curated starters preferred before falling back to the first compatible registry package.
const STARTERS: &[StarterSpec] = &[
    // Primary: interactive fuzzy finder — a "wow" demo that shows Nu's structured data
    StarterSpec {
        id: "idanarye/nu_plugin_skim",
        nu_minor: Some((0, 114)),
        os: None,
    },
    // Interactive TUI data explorer — visually impressive for newcomers
    StarterSpec {
        id: "tonythethompson/nu_plugin_explore",
        nu_minor: Some((0, 114)),
        os: None,
    },
    // Syntax highlighting — immediate visual payoff, works on any data
    StarterSpec {
        id: "cptpiepmatz/nu_plugin_highlight",
        nu_minor: Some((0, 114)),
        os: None,
    },
    // Test framework — practical for all users
    StarterSpec {
        id: "vyadh/nutest",
        nu_minor: Some((0, 114)),
        os: None,
    },
    // Universal fallbacks (any Nu version)
    StarterSpec {
        id: "cptpiepmatz/nu_plugin_highlight",
        nu_minor: None,
        os: None,
    },
    StarterSpec {
        id: "vyadh/nutest",
        nu_minor: None,
        os: None,
    },
];

pub fn execute(args: &TryArgs, root: &Path) -> Result<()> {
    let platform = Platform::detect();
    let mut nu = detect_nu(root)?;

    let registry = RegistryManager::new(root)?;
    let registry_name = registry.default_registry_name();
    let loaded = registry.load_verified(&registry_name).with_context(|| {
        format!(
            "No usable registry index. {}",
            hints::run(CMD_REGISTRY_SYNC)
        )
    })?;

    let packages = &loaded.index.packages;
    if packages.is_empty() {
        bail!(
            "Registry '{}' has no packages. {}",
            registry_name,
            hints::run(CMD_REGISTRY_SYNC)
        );
    }

    let selection = {
        let resolver = Resolver::new(&platform, &nu);
        select_starter(packages, &resolver, &platform, &nu)
    };

    let package_id = match selection {
        StarterSelection::Compatible(id) => id,
        StarterSelection::NeedsPin { id, diagnosis } => {
            println!("Starter '{id}' needs a different Nu than {}.", nu.version);
            let accepted = nu_pin_offer::offer_managed_nu_pin(root, &nu.version, &diagnosis)?;
            if !accepted {
                bail!(
                    "{}",
                    format_no_compatible_starter(
                        &nu.version,
                        &platform.triple,
                        diagnosis.suggested_pin.as_deref(),
                    )
                );
            }
            nu = detect_nu(root)?;
            let resolver = Resolver::new(&platform, &nu);
            if !packages
                .iter()
                .find(|p| p.id.to_string() == id)
                .map(|p| resolver.has_compatible_version(p))
                .unwrap_or(false)
            {
                bail!(
                    "Starter '{id}' is still incompatible after installing managed Nu {}.",
                    nu.version
                );
            }
            id
        }
        StarterSelection::None { suggested_pin } => {
            bail!(
                "{}",
                format_no_compatible_starter(
                    &nu.version,
                    &platform.triple,
                    suggested_pin.as_deref()
                )
            );
        }
    };

    println!("Trying '{package_id}' for Nu {}…", nu.version);

    {
        let _lock = acquire_mutation_lock(root)?;
        let root_buf = root.to_path_buf();
        let options = transaction::InstallOptions {
            root: &root_buf,
            platform: &platform,
            nu_version: &nu,
            force: false,
            verbose: false,
            registry_name: None,
            snapshot_trigger: crate::state::snapshot::SnapshotTrigger::Install,
        };
        transaction::install_package(&package_id, None, &options)?;
    }

    if args.no_activate {
        println!("Installed '{package_id}' (not activated). Run `numan activate {package_id}`.");
        return Ok(());
    }

    activate::execute(
        &ActivateArgs {
            packages: vec![package_id.clone()],

            verbose: false,
            list: false,
            check: false,
        },
        root,
    )?;

    print_usage_hint(&package_id, packages);
    Ok(())
}

#[derive(Debug)]
enum StarterSelection {
    Compatible(String),
    NeedsPin {
        id: String,
        diagnosis: crate::core::resolve::PackageIncompatibility,
    },
    None {
        /// Optional suggested Nu pin discovered among starters.
        suggested_pin: Option<String>,
    },
}

fn select_starter(
    packages: &[Package],
    resolver: &Resolver<'_>,
    platform: &Platform,
    nu: &NuVersion,
) -> StarterSelection {
    // 1. Curated starters that match OS + Nu minor and are compatible.
    for spec in STARTERS {
        if let Some(os) = spec.os {
            if platform.os != os {
                continue;
            }
        }
        if let Some((maj, minor)) = spec.nu_minor {
            if nu.major != maj || nu.minor != minor {
                continue;
            }
        }
        if let Some(pkg) = packages.iter().find(|p| p.id.to_string() == spec.id) {
            if resolver.has_compatible_version(pkg) {
                return StarterSelection::Compatible(spec.id.to_string());
            }
        }
    }

    // 2. Any curated starter that is compatible regardless of Nu minor table miss.
    for spec in STARTERS {
        if let Some(os) = spec.os {
            if platform.os != os {
                continue;
            }
        }
        if let Some(pkg) = packages.iter().find(|p| p.id.to_string() == spec.id) {
            if resolver.has_compatible_version(pkg) {
                return StarterSelection::Compatible(spec.id.to_string());
            }
        }
    }

    // 3. Curated starter with a suggested Nu pin (prefer skim / explore / highlight / nutest).
    let mut suggested_pin = None;
    for spec in STARTERS {
        if let Some(os) = spec.os {
            if platform.os != os {
                continue;
            }
        }
        if let Some(pkg) = packages.iter().find(|p| p.id.to_string() == spec.id) {
            let diagnosis = resolver.diagnose_package(pkg);
            if nu_pin_offer::is_nu_mismatch(&diagnosis) && diagnosis.suggested_pin.is_some() {
                return StarterSelection::NeedsPin {
                    id: spec.id.to_string(),
                    diagnosis,
                };
            }
            // Remember first pin discovered for None fallback.
            if suggested_pin.is_none() && nu_pin_offer::is_nu_mismatch(&diagnosis) {
                suggested_pin = diagnosis.suggested_pin;
            }
        }
    }

    StarterSelection::None { suggested_pin }
}

/// Failure copy when no starter can install against the current Nu/platform.
///
/// Never pitches `registry sync` as the Nu/ABI fix (empty-index sync stays
/// on the early bail in `execute`).
fn format_no_compatible_starter(nu: &str, triple: &str, pin: Option<&str>) -> String {
    let mut msg = format!("No compatible starter package for Nu {nu} on {triple}.");
    if let Some(pin) = pin {
        msg.push_str(&format!(
            "\nInstall a matching managed Nu: {} (your existing Nu is not replaced), then retry `numan try`.",
            hints::setup_nu_version(pin)
        ));
    }
    msg.push_str("\nOr pick a package with `numan search <query>`.");
    msg
}

fn print_usage_hint(package_id: &str, packages: &[Package]) {
    let pkg = packages.iter().find(|p| p.id.to_string() == package_id);
    match pkg.map(|p| &p.package_type) {
        Some(PackageType::Module) if package_id.contains("nutest") => {
            println!("In Nu, try:  use nutest; help commands | where name =~ test");
        }
        Some(PackageType::Plugin) if package_id.contains("explore") => {
            println!("In Nu, try:  open Cargo.toml | nu_plugin_explore");
            println!("             (use arrow keys to navigate, 'q' to quit)");
        }
        Some(PackageType::Plugin) if package_id.contains("highlight") => {
            println!("In Nu, try:  open Cargo.toml | nu_plugin_highlight");
        }
        Some(PackageType::Plugin) if package_id.contains("semver") => {
            println!("In Nu, try:  help commands | where name =~ semver");
        }
        Some(PackageType::Plugin) => {
            println!(
                "In Nu, try:  help commands | where name =~ {}",
                package_id.split('/').next_back().unwrap_or(package_id)
            );
        }
        Some(PackageType::Module) => {
            println!("Module '{package_id}' is active via Numan's managed autoload.");
        }
        _ => {
            println!("Installed and activated '{package_id}'.");
        }
    }
}

fn detect_nu(root: &Path) -> Result<NuVersion> {
    NuVersion::from_paths_or_detect(root)
        .context("Could not detect Nu version. Run `numan init` first.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Commands};
    use crate::core::package::*;
    use crate::core::resolve::{Incompatibility, PackageIncompatibility};
    use clap::Parser;
    use std::collections::{BTreeMap, HashMap};

    #[test]
    fn try_cli_rejects_obsolete_yes_flag() {
        assert!(
            Cli::try_parse_from(["numan", "try", "--yes"]).is_err(),
            "numan try --yes must be rejected"
        );
        let cli = Cli::try_parse_from(["numan", "try", "--no-activate"]).unwrap();
        match cli.command {
            Commands::Try(args) => assert!(args.no_activate),
            _ => panic!("expected Try command"),
        }
    }

    fn pkg(id: &str, constraint: &str, plugin: bool) -> Package {
        let (owner, name) = id.split_once('/').unwrap();
        let mut targets = HashMap::new();
        targets.insert(
            "x86_64-pc-windows-msvc".to_string(),
            TargetArtifact {
                url: "https://example.com/p.zip".to_string(),
                sha256: "aa".to_string(),
                executable_path: "p.exe".to_string(),
            },
        );
        targets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetArtifact {
                url: "https://example.com/p.tar.gz".to_string(),
                sha256: "bb".to_string(),
                executable_path: "p".to_string(),
            },
        );
        Package {
            id: ScopedId::new(owner, name),
            description: "d".to_string(),
            repo: "https://example.com".to_string(),
            package_type: if plugin {
                PackageType::Plugin
            } else {
                PackageType::Module
            },
            tags: vec![],
            versions: vec![VersionEntry {
                version: semver::Version::new(1, 0, 0),
                nu_version: constraint.to_string(),
                verified_with: vec!["0.113.1".to_string()],
                artifact: Artifact {
                    kind: if plugin {
                        "binary".to_string()
                    } else {
                        "archive".to_string()
                    },
                    url: if plugin {
                        None
                    } else {
                        Some("https://example.com/m.zip".to_string())
                    },
                    sha256: if plugin { None } else { Some("cc".to_string()) },
                    targets: if plugin { targets } else { HashMap::new() },
                    archive_root: None,
                    include: None,
                    entry: if plugin {
                        None
                    } else {
                        Some("mod.nu".to_string())
                    },
                },
                source: None,
                dependencies: BTreeMap::new(),
                activation: None,
            }],
        }
    }

    fn windows_platform() -> Platform {
        Platform {
            os: Os::Windows,
            arch: crate::core::platform::Arch::X86_64,
            env: crate::core::platform::Env::Msvc,
            triple: "x86_64-pc-windows-msvc".to_string(),
        }
    }

    #[test]
    fn select_starter_prefers_skim_on_114_when_present() {
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let packages = vec![
            pkg("idanarye/nu_plugin_skim", ">=0.114.0 <0.115.0", true),
            pkg("tonythethompson/nu_plugin_explore", ">=0.114.0 <0.115.0", true),
            pkg("cptpiepmatz/nu_plugin_highlight", ">=0.114.0 <0.115.0", true),
            pkg("vyadh/nutest", ">=0.103.0", false),
        ];
        match select_starter(&packages, &resolver, &platform, &nu) {
            StarterSelection::Compatible(id) => assert_eq!(id, "idanarye/nu_plugin_skim"),
            other => panic!("unexpected selection: {other:?}"),
        }
    }

    #[test]
    fn select_starter_prefers_explore_on_114_without_skim() {
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let packages = vec![
            pkg("tonythethompson/nu_plugin_explore", ">=0.114.0 <0.115.0", true),
            pkg("cptpiepmatz/nu_plugin_highlight", ">=0.114.0 <0.115.0", true),
            pkg("vyadh/nutest", ">=0.114.0", false),
        ];
        match select_starter(&packages, &resolver, &platform, &nu) {
            StarterSelection::Compatible(id) => assert_eq!(id, "tonythethompson/nu_plugin_explore"),
            other => panic!("unexpected selection: {other:?}"),
        }
    }

    #[test]
    fn select_starter_falls_back_to_highlight_on_any_nu() {
        let platform = windows_platform();
        let nu = NuVersion::parse("0.113.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let packages = vec![
            pkg("cptpiepmatz/nu_plugin_highlight", ">=0.113.0 <0.115.0", true),
            pkg("vyadh/nutest", ">=0.103.0", false),
        ];
        match select_starter(&packages, &resolver, &platform, &nu) {
            StarterSelection::Compatible(id) => assert_eq!(id, "cptpiepmatz/nu_plugin_highlight"),
            other => panic!("unexpected selection: {other:?}"),
        }
    }

    #[test]
    fn format_no_compatible_starter_with_pin_is_honest() {
        let msg =
            format_no_compatible_starter("0.114.1", "x86_64-pc-windows-msvc", Some("0.113.1"));
        assert!(msg.contains("0.114.1"), "{msg}");
        assert!(msg.contains("x86_64-pc-windows-msvc"), "{msg}");
        assert!(msg.contains("setup nu 0.113.1"), "{msg}");
        assert!(msg.contains("numan search <query>"), "{msg}");
        assert!(
            !msg.contains("registry sync"),
            "must not pitch registry sync as ABI fix: {msg}"
        );
    }

    #[test]
    fn format_no_compatible_starter_without_pin_points_at_search() {
        let msg = format_no_compatible_starter("0.115.0", "x86_64-unknown-linux-gnu", None);
        assert!(msg.contains("numan search <query>"), "{msg}");
        assert!(!msg.contains("setup nu --version"), "{msg}");
        assert!(!msg.contains("registry sync"), "{msg}");
    }

    #[test]
    fn offer_managed_nu_pin_non_interactive_refuses_silent_switch() {
        let diagnosis = PackageIncompatibility {
            suggested_pin: Some("0.113.1".to_string()),
            issue: Incompatibility::NuTooNew {
                constraint: ">=0.113.0 <0.114.0".to_string(),
            },
            available_versions: vec!["1.0.0".to_string()],
        };
        let root = tempfile::tempdir().unwrap();
        let accepted = nu_pin_offer::offer_managed_nu_pin_with_interaction(
            root.path(),
            "0.114.1",
            &diagnosis,
            false,
            || panic!("non-interactive path must not read stdin"),
        )
        .unwrap();
        assert!(
            !accepted,
            "non-interactive mode must not auto-install managed Nu"
        );
    }

    #[test]
    fn select_starter_none_carries_suggested_pin() {
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        // No curated starter present at all, so no pin can be suggested.
        let packages_no_pin = vec![pkg("foo/bar", ">=0.100.0 <0.101.0", true)];
        match select_starter(&packages_no_pin, &resolver, &platform, &nu) {
            StarterSelection::None { suggested_pin } => {
                assert!(
                    suggested_pin.is_none(),
                    "no pin available for unrelated package"
                );
            }
            other => panic!("expected None, got {other:?}"),
        }
    }

    #[test]
    fn select_starter_none_path_produces_expected_message() {
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        // Starter that's too old, no pin suggestion available.
        let packages = vec![pkg("old/starter", ">=0.100.0 <0.101.0", true)];

        match select_starter(&packages, &resolver, &platform, &nu) {
            StarterSelection::None { suggested_pin } => {
                let msg = format_no_compatible_starter(
                    &nu.version.to_string(),
                    &platform.triple,
                    suggested_pin.as_deref(),
                );
                assert!(msg.contains("No compatible starter"), "message: {msg}");
                assert!(msg.contains("numan search <query>"), "message: {msg}");
            }
            other => panic!("expected None, got {other:?}"),
        }
    }
}
