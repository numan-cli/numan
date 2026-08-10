use crate::core::nu_version::NuVersion;
use crate::core::package::Package;
use crate::core::platform::Platform;
use crate::core::registry::RegistryManager;
use crate::core::resolve::Resolver;
use anyhow::Result;
use std::path::Path;

pub fn execute(id: &str, root: &Path) -> Result<()> {
    let pkg = RegistryManager::new(root)?
        .find_package(id)?
        .ok_or_else(|| anyhow::anyhow!("Package '{id}' not found."))?;

    let platform = Platform::detect();
    let nu = current_nu(root);
    print!("{}", format_info(&pkg, &platform, nu.as_ref()));
    Ok(())
}

/// Format registry package info for display (testable without I/O).
///
/// Registry packages looked up via `numan info` are labeled
/// `verified upstream artifact` (ADR 0001 class 2). This does not claim
/// a security audit of upstream source.
pub fn format_info(pkg: &Package, platform: &Platform, nu: Option<&NuVersion>) -> String {
    let mut out = String::new();
    out.push_str(&format!("Package:    {}/{}\n", pkg.id.owner, pkg.id.name));
    out.push_str(&format!("Type:       {}\n", pkg.package_type));
    match pkg.package_type {
        crate::core::package::PackageType::Script
        | crate::core::package::PackageType::Completion => {
            out.push_str(
                "Activation: install-only (activation deferred; install does not wire Nu)\n",
            );
        }
        _ => {}
    }
    out.push_str("Status:     verified upstream artifact\n");
    out.push_str(&format!("Description: {}\n", pkg.description));
    out.push_str(&format!("Repository: {}\n", pkg.repo));
    if !pkg.tags.is_empty() {
        out.push_str(&format!("Tags:       {}\n", pkg.tags.join(", ")));
    }
    if let Some(n) = nu {
        out.push_str(&format!(
            "Your Nu:    {} ({})\n",
            n.version, platform.triple
        ));
    }

    out.push_str("\nVersions:\n");
    let resolver = nu.map(|n| Resolver::new(platform, n));
    for ver in &pkg.versions {
        let status = match resolver.as_ref() {
            Some(r) => match r.classify_version(ver) {
                None => "compatible".to_string(),
                Some(issue) => issue.short_label(),
            },
            None => {
                if ver.artifact.kind == "binary"
                    && !ver.artifact.targets.contains_key(&platform.triple)
                {
                    format!("no artifact for {}", platform.triple)
                } else {
                    "nu unknown".to_string()
                }
            }
        };

        out.push_str(&format!(
            "  v{}  [nu {}]  ({status})\n",
            ver.version, ver.nu_version
        ));

        if !ver.verified_with.is_empty() {
            out.push_str(&format!(
                "    tested with: {}\n",
                ver.verified_with.join(", ")
            ));
        }

        if ver.provenance.as_deref() == Some("commit-snapshot") {
            out.push_str("    note: built from a commit snapshot, not a tagged release\n");
        }

        if let Some(ref source) = ver.source {
            out.push_str(&format!("    source git:  {}\n", source.git));
            out.push_str(&format!("    source rev:  {}\n", source.rev));
            out.push_str(&format!("    cargo_name:  {}\n", source.cargo_name));
            if let Some(ref lock) = source.cargo_lock_sha256 {
                out.push_str(&format!("    cargo_lock:  {lock}\n"));
            }
        }

        for target in ver.artifact.targets.keys() {
            out.push_str(&format!("    platform: {target}\n"));
        }
    }

    out.push_str("\nNote: verified means provenance, integrity, and install/activate\n");
    out.push_str("checks were recorded. Numan has not security-audited the upstream source.\n");
    out
}

fn current_nu(root: &Path) -> Option<NuVersion> {
    NuVersion::from_paths_or_detect(root).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::package::{
        Artifact, PackageType, ScopedId, SourceInfo, TargetArtifact, VersionEntry,
    };
    use crate::core::platform::{Arch, Env, Os};
    use semver::Version;
    use std::collections::BTreeMap;

    fn linux_platform() -> Platform {
        Platform {
            os: Os::Linux,
            arch: Arch::X86_64,
            env: Env::Gnu,
            triple: "x86_64-unknown-linux-gnu".to_string(),
        }
    }

    fn sample_plugin(with_source: bool) -> Package {
        let mut targets = std::collections::HashMap::new();
        targets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetArtifact {
                url: "https://example.com/p.tar.gz".into(),
                sha256: "abc".into(),
                executable_path: "nu_plugin_x".into(),
            },
        );
        Package {
            id: ScopedId {
                owner: "cptpiepmatz".into(),
                name: "nu_plugin_highlight".into(),
            },
            description: "Syntax highlighting.".into(),
            repo: "https://github.com/cptpiepmatz/nu-plugin-highlight".into(),
            package_type: PackageType::Plugin,
            tags: vec!["plugin".into()],
            versions: vec![VersionEntry {
                version: Version::new(1, 4, 15),
                nu_version: ">=0.113.0 <0.114.0".into(),
                verified_with: vec!["0.113.1".into()],
                artifact: Artifact {
                    kind: "binary".into(),
                    url: None,
                    sha256: None,
                    targets,
                    archive_root: None,
                    include: None,
                    entry: None,
                },
                source: with_source.then_some(SourceInfo {
                    git: "https://github.com/cptpiepmatz/nu-plugin-highlight".into(),
                    rev: "v1.4.15+0.113.1".into(),
                    cargo_name: "nu_plugin_highlight".into(),
                    cargo_lock_sha256: None,
                }),
                dependencies: BTreeMap::new(),
                activation: None,
                provenance: None,
            }],
        }
    }

    #[test]
    fn format_info_marks_script_install_only() {
        let mut pkg = sample_plugin(false);
        pkg.package_type = PackageType::Script;
        let out = format_info(&pkg, &linux_platform(), None);
        assert!(out.contains("Activation: install-only"), "{out}");
        assert!(out.contains("Type:       script"), "{out}");
    }

    #[test]
    fn format_info_marks_completion_install_only() {
        let mut pkg = sample_plugin(false);
        pkg.package_type = PackageType::Completion;
        let out = format_info(&pkg, &linux_platform(), None);
        assert!(out.contains("Activation: install-only"), "{out}");
        assert!(out.contains("Type:       completion"), "{out}");
    }

    #[test]
    fn format_info_prints_source_when_present() {
        let pkg = sample_plugin(true);
        let out = format_info(&pkg, &linux_platform(), None);
        assert!(
            out.contains("source git:  https://github.com/cptpiepmatz/nu-plugin-highlight"),
            "{out}"
        );
        assert!(out.contains("source rev:  v1.4.15+0.113.1"), "{out}");
        assert!(out.contains("cargo_name:  nu_plugin_highlight"), "{out}");
    }

    #[test]
    fn format_info_notes_commit_snapshot_provenance() {
        let mut pkg = sample_plugin(false);
        pkg.versions[0].provenance = Some("commit-snapshot".to_string());
        let out = format_info(&pkg, &linux_platform(), None);
        assert!(
            out.contains("built from a commit snapshot, not a tagged release"),
            "{out}"
        );
    }

    #[test]
    fn format_info_omits_provenance_note_when_absent() {
        let pkg = sample_plugin(false);
        let out = format_info(&pkg, &linux_platform(), None);
        assert!(!out.contains("commit snapshot"), "{out}");
    }

    #[test]
    fn format_info_omits_source_lines_when_absent() {
        let pkg = sample_plugin(false);
        let out = format_info(&pkg, &linux_platform(), None);
        assert!(!out.contains("source git:"), "{out}");
        assert!(!out.contains("cargo_name:"), "{out}");
    }
}
