use crate::core::nu_version::NuVersion;
use crate::core::package::{PackageType, VersionEntry};
use crate::core::platform::Platform;
use crate::core::registry::RegistryManager;
use crate::core::resolve::{Incompatibility, Resolver};
use anyhow::Result;
use clap::Parser;
use std::path::Path;

/// Search registry by name/description/tags
#[derive(Parser, Debug)]
pub struct SearchArgs {
    /// Search query
    pub query: String,

    /// Show incompatible packages too (marked in the listing)
    #[arg(long)]
    pub all: bool,
}

pub fn execute(args: &SearchArgs, root: &Path) -> Result<()> {
    let mgr = RegistryManager::new(root)?;
    let results = mgr.search(&args.query)?;

    if results.is_empty() {
        println!("No packages found matching '{}'.", args.query);
        return Ok(());
    }

    let platform = Platform::detect();
    let nu = current_nu(root);
    let resolver = nu.as_ref().map(|n| Resolver::new(&platform, n));

    let mut shown = 0usize;
    let mut hidden = 0usize;
    let mut first_hidden_id: Option<String> = None;

    println!(
        "Found {} package(s) matching '{}':",
        results.len(),
        args.query
    );
    println!("{}", format_search_header(nu.as_ref(), &platform.triple));
    println!();

    for pkg in &results {
        let compatible = resolver
            .as_ref()
            .map(|r| r.has_compatible_version(pkg))
            .unwrap_or(true);

        if !compatible && !args.all {
            if first_hidden_id.is_none() {
                first_hidden_id = Some(pkg.id.to_string());
            }
            hidden += 1;
            continue;
        }

        shown += 1;

        // Find newest version by semver (for display and classification)
        let newest_version = pkg.versions.iter().max_by(|a, b| a.version.cmp(&b.version));

        let display_entry = match resolver.as_ref() {
            Some(r) if compatible => r.latest_compatible(pkg).or(newest_version),
            _ => newest_version,
        };

        let version_label = display_entry
            .map(|v| v.version.to_string())
            .unwrap_or_else(|| "n/a".to_string());

        let issue = match resolver.as_ref() {
            Some(r) if !compatible => newest_version.and_then(|entry| r.classify_version(entry)),
            _ => None,
        };

        let verified_with = display_entry
            .map(|v| v.verified_with.as_slice())
            .unwrap_or(&[]);

        // Status when Nu is known and the row needs a verdict: incompatibles,
        // --all listings, and all non-plugin compatible rows (asymmetric label).
        // Install-only labels do not depend on Nu detection, so script/completion
        // rows stay labeled even when the resolver is absent.
        let status = if resolver.is_some() {
            format_row_status(
                &pkg.package_type,
                compatible,
                args.all,
                issue.as_ref(),
                verified_with,
            )
        } else {
            match pkg.package_type {
                PackageType::Script | PackageType::Completion => {
                    format_row_status(&pkg.package_type, true, args.all, None, verified_with)
                }
                PackageType::Plugin | PackageType::Module => String::new(),
            }
        };

        let provisional_marker = provisional_marker(display_entry);

        println!(
            "  {}/{}  v{}  [{}]{}{}
    {}",
            pkg.id.owner,
            pkg.id.name,
            version_label,
            pkg.package_type,
            status,
            provisional_marker,
            pkg.description
        );
    }

    if shown == 0 && hidden > 0 {
        println!("(no compatible packages for your Nu/platform; {hidden} match(es) hidden)");
    }

    if hidden > 0 {
        let nu_label = nu.as_ref().map(|n| n.version.as_str()).unwrap_or("unknown");
        println!(
            "\n{}",
            format_hidden_footer(
                hidden,
                nu_label,
                &platform.triple,
                first_hidden_id.as_deref(),
            )
        );
    }

    Ok(())
}

fn current_nu(root: &Path) -> Option<NuVersion> {
    NuVersion::from_paths_or_detect(root).ok()
}

/// Environment line printed under the Found header.
fn format_search_header(nu: Option<&NuVersion>, triple: &str) -> String {
    match nu {
        Some(n) => format!("checked against: Nu {} ({triple})", n.version),
        None => format!("checked against: Nu unknown ({triple})"),
    }
}

/// Row suffix marking a provisional (not lifecycle-tested) displayed version.
fn provisional_marker(entry: Option<&VersionEntry>) -> &'static str {
    if entry.map(|v| v.is_provisional()).unwrap_or(false) {
        " [provisional]"
    } else {
        ""
    }
}

/// Row status suffix (leading space + brackets), or empty.
///
/// Plugins get a hard evaluated verdict. Modules use not-ABI-locked wording.
/// Scripts and completions are install-only until activation contracts land.
fn format_row_status(
    pkg_type: &PackageType,
    compatible: bool,
    args_all: bool,
    issue: Option<&Incompatibility>,
    verified_with: &[String],
) -> String {
    if !compatible {
        let label = issue
            .map(|i| i.short_label())
            .unwrap_or_else(|| "incompatible".to_string());
        return format!(" [{label}]");
    }

    match pkg_type {
        PackageType::Plugin => {
            if args_all {
                " [compatible]".to_string()
            } else {
                String::new()
            }
        }
        PackageType::Script | PackageType::Completion => {
            if verified_with.is_empty() {
                " [install-only; activation deferred]".to_string()
            } else {
                format!(
                    " [install-only; activation deferred; verified with {}]",
                    verified_with.join(", ")
                )
            }
        }
        PackageType::Module => {
            if verified_with.is_empty() {
                " [not ABI-locked]".to_string()
            } else {
                format!(
                    " [not ABI-locked; verified with {}]",
                    verified_with.join(", ")
                )
            }
        }
    }
}

fn format_hidden_footer(
    hidden: usize,
    nu_label: &str,
    triple: &str,
    first_hidden_id: Option<&str>,
) -> String {
    let mut out =
        format!("{hidden} package(s) hidden (incompatible with Nu {nu_label} / {triple}).");
    if let Some(id) = first_hidden_id {
        out.push_str(&format!("\nRun 'numan info {id}' for options."));
    }
    out.push_str(" Use --all to show them.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::package::*;
    use crate::core::resolve::Resolver;
    use std::collections::{BTreeMap, HashMap};

    fn sample_pkg(nu_constraint: &str) -> Package {
        sample_pkg_typed(PackageType::Plugin, nu_constraint, vec![])
    }

    fn sample_pkg_typed(
        package_type: PackageType,
        nu_constraint: &str,
        verified_with: Vec<String>,
    ) -> Package {
        let mut targets = HashMap::new();
        targets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetArtifact {
                url: "https://example.com/p.zip".to_string(),
                sha256: "abc".to_string(),
                executable_path: "p".to_string(),
            },
        );
        Package {
            id: ScopedId::new("owner", "pkg"),
            description: "desc".to_string(),
            repo: "https://example.com".to_string(),
            package_type,
            tags: vec![],
            versions: vec![VersionEntry {
                version: semver::Version::new(1, 0, 0),
                nu_version: nu_constraint.to_string(),
                verified_with,
                artifact: Artifact {
                    kind: "binary".to_string(),
                    url: None,
                    sha256: None,
                    targets,
                    archive_root: None,
                    include: None,
                    entry: None,
                },
                source: None,
                dependencies: BTreeMap::new(),
                activation: None,
                evidence_tier: None,
                deferral_reason: None,
            }],
        }
    }

    #[test]
    fn has_compatible_respects_nu_constraint() {
        let platform = Platform {
            os: crate::core::platform::Os::Linux,
            arch: crate::core::platform::Arch::X86_64,
            env: crate::core::platform::Env::Gnu,
            triple: "x86_64-unknown-linux-gnu".to_string(),
        };
        let nu = NuVersion::parse("0.114.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        assert!(!resolver.has_compatible_version(&sample_pkg(">=0.113.0 <0.114.0")));
        assert!(resolver.has_compatible_version(&sample_pkg(">=0.114.0")));
    }

    #[test]
    fn search_header_includes_nu_and_triple() {
        let nu = NuVersion::parse("0.114.1").unwrap();
        let header = format_search_header(Some(&nu), "x86_64-pc-windows-msvc");
        assert!(header.contains("0.114.1"));
        assert!(header.contains("x86_64-pc-windows-msvc"));
        assert!(header.starts_with("checked against: Nu "));
    }

    #[test]
    fn search_header_unknown_nu() {
        let header = format_search_header(None, "x86_64-unknown-linux-gnu");
        assert_eq!(
            header,
            "checked against: Nu unknown (x86_64-unknown-linux-gnu)"
        );
    }

    #[test]
    fn plugin_incompatible_uses_short_label() {
        let issue = Incompatibility::NuTooNew {
            constraint: ">=0.113.0 <0.114.0".to_string(),
        };
        let status = format_row_status(&PackageType::Plugin, false, true, Some(&issue), &[]);
        assert_eq!(status, " [needs Nu >=0.113.0 <0.114.0]");
        assert!(!status.contains("compatible"));
    }

    #[test]
    fn module_compatible_with_verified_with_is_not_oversold() {
        let verified = vec!["0.113.1".to_string()];
        let status = format_row_status(&PackageType::Module, true, true, None, &verified);
        assert!(status.contains("not ABI-locked"));
        assert!(status.contains("verified with 0.113.1"));
        assert!(!status.contains("[compatible]"));
    }

    #[test]
    fn module_compatible_without_verified_with() {
        let status = format_row_status(&PackageType::Module, true, false, None, &[]);
        assert_eq!(status, " [not ABI-locked]");
        assert!(!status.contains("compatible"));
    }

    #[test]
    fn script_and_completion_use_install_only_labels() {
        for ty in [PackageType::Script, PackageType::Completion] {
            let status = format_row_status(&ty, true, true, None, &["0.113.1".to_string()]);
            assert!(status.contains("install-only"), "{ty}");
            assert!(status.contains("activation deferred"), "{ty}");
            assert!(!status.contains("[compatible]"), "{ty}");
            assert!(!status.contains("not ABI-locked"), "{ty}");
        }
    }

    #[test]
    fn plugin_compatible_all_keeps_hard_ok_marker() {
        let status = format_row_status(&PackageType::Plugin, true, true, None, &[]);
        assert_eq!(status, " [compatible]");
    }

    #[test]
    fn plugin_compatible_default_listing_omits_status() {
        let status = format_row_status(&PackageType::Plugin, true, false, None, &[]);
        assert!(status.is_empty());
    }

    #[test]
    fn provisional_marker_shown_for_provisional_entry() {
        let mut pkg = sample_pkg_typed(PackageType::Plugin, "*", vec![]);
        pkg.versions[0].evidence_tier = Some("provisional".to_string());
        assert_eq!(provisional_marker(Some(&pkg.versions[0])), " [provisional]");
    }

    #[test]
    fn provisional_marker_empty_for_proven_entry() {
        let pkg = sample_pkg_typed(PackageType::Plugin, "*", vec![]);
        assert_eq!(provisional_marker(Some(&pkg.versions[0])), "");
    }

    #[test]
    fn provisional_marker_empty_for_none() {
        assert_eq!(provisional_marker(None), "");
    }

    #[test]
    fn hidden_footer_points_at_info_not_sync() {
        let footer = format_hidden_footer(
            1,
            "0.114.1",
            "x86_64-unknown-linux-gnu",
            Some("dead10ck/nu_plugin_dns"),
        );
        assert!(footer.contains("1 package(s) hidden"));
        assert!(footer.contains("0.114.1"));
        assert!(footer.contains("Run 'numan info dead10ck/nu_plugin_dns' for options."));
        assert!(footer.contains("Use --all to show them."));
        assert!(!footer.contains("registry sync"));
    }

    #[test]
    fn sample_module_pkg_helper_builds() {
        let pkg = sample_pkg_typed(PackageType::Module, "*", vec!["0.113.1".to_string()]);
        assert_eq!(pkg.package_type, PackageType::Module);
        assert_eq!(pkg.versions[0].verified_with, vec!["0.113.1"]);
    }
}
