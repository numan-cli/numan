use crate::cmd::activate::{self, ActivateArgs};
use crate::core::nu_version::NuVersion;
use crate::core::package::{Package, PackageType};
use crate::core::platform::Platform;
use crate::core::registry::RegistryManager;
use crate::core::resolve::{self, Incompatibility, Resolver};
use crate::install::transaction::{self, InstallResult};
use crate::state::lockfile::Lockfile;
use crate::util::fs_safety::acquire_mutation_lock;
use crate::util::hints::{self, CMD_REGISTRY_SYNC};
use anyhow::{bail, Context, Result};
use clap::Parser;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Try a package against the current Nu environment and explain compatibility.
#[derive(Parser, Debug)]
pub struct TryArgs {
    /// Package to try (owner/name or owner/name@version)
    pub package: Option<String>,

    /// Install only; do not activate
    #[arg(long)]
    pub no_activate: bool,
}

fn split_package_spec(package_spec: &str) -> (&str, Option<&str>) {
    if let Some((id, ver)) = package_spec.rsplit_once('@') {
        (id, Some(ver))
    } else {
        (package_spec, None)
    }
}

fn list_installed_nu_versions(root: &Path) -> Vec<String> {
    crate::nu::version_manager::list_installed_versions(root).unwrap_or_default()
}

fn collect_candidate_nu_versions(package: &Package, installed: &[String]) -> Vec<NuVersion> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();

    for v in installed {
        if let Ok(parsed) = NuVersion::parse(v) {
            if seen.insert((parsed.major, parsed.minor, parsed.patch)) {
                candidates.push(parsed);
            }
        }
    }

    for entry in &package.versions {
        for vw in &entry.verified_with {
            if let Ok(parsed) = NuVersion::parse(vw) {
                if seen.insert((parsed.major, parsed.minor, parsed.patch)) {
                    candidates.push(parsed);
                }
            }
        }

        if let Some(pin) = resolve::suggest_managed_nu_pin(entry) {
            if let Ok(parsed) = NuVersion::parse(&pin) {
                if seen.insert((parsed.major, parsed.minor, parsed.patch)) {
                    candidates.push(parsed);
                }
            }
        }

        for candidate in resolve::candidate_nu_versions_from_constraint(&entry.nu_version) {
            if let Ok(parsed) = NuVersion::parse(&candidate) {
                if seen.insert((parsed.major, parsed.minor, parsed.patch)) {
                    candidates.push(parsed);
                }
            }
        }
    }

    candidates
}

fn is_platform_only_issue(package: &Package, resolver: &Resolver<'_>) -> bool {
    if package.versions.is_empty() {
        return false;
    }
    package.versions.iter().all(|v| {
        matches!(
            resolver.classify_version(v),
            Some(Incompatibility::MissingTarget { .. })
        )
    })
}

fn report_incompatible(
    package: &Package,
    platform: &Platform,
    current_nu: &NuVersion,
    package_id: &str,
    current_issue: &Incompatibility,
    installed: &[String],
) -> Result<()> {
    let candidates = collect_candidate_nu_versions(package, installed);
    let compatible = resolve::compatible_nu_versions(package, platform, &candidates);
    let installed_set: HashSet<String> = installed.iter().cloned().collect();

    if compatible.is_empty() {
        if matches!(current_issue, Incompatibility::MissingTarget { .. })
            || is_platform_only_issue(package, &Resolver::new(platform, current_nu))
        {
            bail!(
                "{} is not available for your platform ({}).",
                package.id,
                platform.triple
            );
        }
        bail!(
            "{} is not compatible with Nu {}.\nNo compatible managed Nu version is known.",
            package.id,
            current_nu.version
        );
    }

    let mut msg = String::new();
    match current_issue {
        Incompatibility::MissingTarget { .. } => {
            msg.push_str(&format!(
                "{} is not available for your platform on Nu {}.",
                package.id, current_nu.version
            ));
        }
        Incompatibility::NuTooNew { .. } => {
            msg.push_str(&format!(
                "{} is not compatible with Nu {}. The package is too old for this Nu.",
                package.id, current_nu.version
            ));
        }
        Incompatibility::NuTooOld { .. } => {
            msg.push_str(&format!(
                "{} is not compatible with Nu {}. The package needs a newer Nu.",
                package.id, current_nu.version
            ));
        }
        _ => {
            msg.push_str(&format!(
                "{} is not compatible with Nu {}.",
                package.id, current_nu.version
            ));
        }
    }

    msg.push_str("\n\nCompatible managed versions:");
    for v in &compatible {
        let marker = if installed_set.contains(&v.version) {
            " (installed)"
        } else {
            ""
        };
        msg.push_str(&format!("\n  {}{}", v.version, marker));
    }

    if let Some(rec) = resolve::select_recommended_nu(current_nu, &compatible, &installed_set) {
        let rec_score = (rec.major, rec.minor, rec.patch);
        let current_score = (current_nu.major, current_nu.minor, current_nu.patch);
        let has_older = compatible
            .iter()
            .any(|v| (v.major, v.minor, v.patch) < current_score);
        let has_newer = compatible
            .iter()
            .any(|v| (v.major, v.minor, v.patch) > current_score);
        let label = if has_older && has_newer {
            "Nearest"
        } else if rec_score < current_score {
            "Newest"
        } else if rec_score > current_score {
            "Earliest"
        } else {
            "Nearest"
        };
        msg.push_str(&format!("\n\n{label} compatible version: {}", rec.version));
        msg.push_str(&format!(
            "\n\nTry:\n  numan use {}\n  numan try {}",
            rec.version, package_id
        ));
    }

    bail!("{}", msg)
}

pub fn execute(args: &TryArgs, root: &Path) -> Result<()> {
    let package_spec = args.package.as_deref().filter(|s| !s.is_empty()).ok_or_else(|| {
        anyhow::anyhow!(
            "error: a package is required\n\nUsage:\n  numan try <package>\n\nExample:\n  numan try idanarye/nu_plugin_skim"
        )
    })?;

    let (package_id, version) = split_package_spec(package_spec);

    let platform = Platform::detect();
    let nu = detect_nu(root)?;

    let registry = RegistryManager::new(root)?;
    let registry_name = registry.default_registry_name();
    let loaded = registry.load_verified(&registry_name).with_context(|| {
        format!(
            "No usable registry index. {}",
            hints::run(CMD_REGISTRY_SYNC)
        )
    })?;

    let package = loaded
        .index
        .packages
        .iter()
        .find(|p| p.id.to_string() == package_id)
        .with_context(|| {
            format!(
                "Package '{package_id}' not found in registry. {}",
                hints::run(CMD_REGISTRY_SYNC)
            )
        })?;

    try_package(
        args,
        root,
        package,
        package_spec,
        package_id,
        version,
        &platform,
        &nu,
        transaction::install_package,
        activate::execute,
    )
}

#[allow(clippy::too_many_arguments)]
fn try_package<I, A>(
    args: &TryArgs,
    root: &Path,
    package: &Package,
    package_spec: &str,
    package_id: &str,
    version: Option<&str>,
    platform: &Platform,
    nu: &NuVersion,
    install: I,
    activate: A,
) -> Result<()>
where
    I: for<'b> Fn(&str, Option<&str>, &transaction::InstallOptions<'b>) -> Result<InstallResult>,
    A: Fn(&ActivateArgs, &Path) -> Result<()>,
{
    let resolver = Resolver::new(platform, nu);

    let target_entry = if let Some(ver_str) = version {
        let target_version: semver::Version = ver_str
            .parse()
            .with_context(|| format!("Invalid version: '{ver_str}'"))?;
        package
            .versions
            .iter()
            .find(|v| v.version == target_version)
            .with_context(|| {
                format!(
                    "Version {target_version} not available for '{package_id}'. Available: {}",
                    package
                        .versions
                        .iter()
                        .map(|v| v.version.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?
    } else if let Some(entry) = resolver.latest_compatible(package) {
        entry
    } else {
        let current_issue = resolver.diagnose_package(package).issue;
        let installed = list_installed_nu_versions(root);
        return report_incompatible(
            package,
            platform,
            nu,
            package_id,
            &current_issue,
            &installed,
        );
    };

    if let Some(issue) = resolver.classify_version(target_entry) {
        let installed = list_installed_nu_versions(root);
        return report_incompatible(package, platform, nu, package_id, &issue, &installed);
    }

    println!("Trying '{package_spec}' for Nu {}...", nu.version);

    let _lock = acquire_mutation_lock(root)?;
    let root_buf = root.to_path_buf();
    let options = transaction::InstallOptions {
        root: &root_buf,
        platform,
        nu_version: nu,
        force: false,
        verbose: false,
        registry_name: None,
        snapshot_trigger: crate::state::snapshot::SnapshotTrigger::Install,
    };

    let result = install(package_id, version, &options)
        .with_context(|| format!("Failed to install '{package_id}'"))?;

    if result.already_existed {
        println!("'{package_id}' is already installed.");
    } else if !result.installed {
        bail!("Install reported no changes for '{package_id}'");
    }

    let install_only = matches!(
        package.package_type,
        PackageType::Script | PackageType::Completion
    );

    if args.no_activate || install_only {
        if install_only {
            print_install_only_hint(root, package_id)?;
        } else {
            println!(
                "Installed '{package_id}' (not activated). Run `numan activate {package_id}`."
            );
        }
        return Ok(());
    }

    if let Err(e) = activate(
        &ActivateArgs {
            packages: vec![package_id.to_string()],
            verbose: false,
            list: false,
            check: false,
        },
        root,
    ) {
        eprintln!("Installed '{package_id}' but activation failed: {e:#}");
        bail!("Activation failed after install.");
    }

    println!("Installed and activated '{package_id}'.");
    Ok(())
}

/// Nu `overlay use` hint with the same path-literal escaping as
/// [`crate::nu::autoload::render_use_statement`].
fn format_overlay_use_hint(path: &Path) -> Result<String> {
    let path_str = path
        .to_str()
        .with_context(|| format!("Installed path '{}' is not valid UTF-8", path.display()))?;
    let escaped = path_str.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("overlay use \"{escaped}\""))
}

fn print_install_only_hint(root: &Path, package_id: &str) -> Result<()> {
    let lockfile = Lockfile::load(root)
        .with_context(|| format!("Failed to load lockfile for installed package '{package_id}'"))?;
    let installed = lockfile.packages.get(package_id).with_context(|| {
        format!("Installed '{package_id}' but no lockfile record was found; refuse usage hint")
    })?;
    println!("Installed '{package_id}' (install-only; activation deferred).");
    match installed.entry.as_deref() {
        Some(entry_name) => {
            let full: PathBuf = root.join(&installed.payload_path).join(entry_name);
            println!("In Nu:  {}", format_overlay_use_hint(&full)?);
        }
        None => {
            let full: PathBuf = root.join(&installed.payload_path);
            println!("Installed under {}", full.display());
        }
    }
    Ok(())
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
    use crate::core::platform::{Arch, Env, Os};
    use crate::core::resolve::Incompatibility;
    use clap::Parser;
    use std::cell::Cell;
    use std::collections::{BTreeMap, HashMap};
    use std::path::PathBuf;

    fn pkg(id: &str, constraint: &str, package_type: PackageType) -> Package {
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
        let is_plugin = package_type == PackageType::Plugin;
        Package {
            id: ScopedId::new(owner, name),
            description: "d".to_string(),
            repo: "https://example.com".to_string(),
            package_type,
            tags: vec![],
            versions: vec![VersionEntry {
                version: semver::Version::new(1, 0, 0),
                nu_version: constraint.to_string(),
                verified_with: vec!["0.113.1".to_string()],
                artifact: Artifact {
                    kind: if is_plugin {
                        "binary".to_string()
                    } else {
                        "archive".to_string()
                    },
                    url: if is_plugin {
                        None
                    } else {
                        Some("https://example.com/m.zip".to_string())
                    },
                    sha256: if is_plugin {
                        None
                    } else {
                        Some("cc".to_string())
                    },
                    targets: if is_plugin { targets } else { HashMap::new() },
                    archive_root: None,
                    include: None,
                    entry: if is_plugin {
                        None
                    } else {
                        Some("entry.nu".to_string())
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
            arch: Arch::X86_64,
            env: Env::Msvc,
            triple: "x86_64-pc-windows-msvc".to_string(),
        }
    }

    fn linux_platform() -> Platform {
        Platform {
            os: Os::Linux,
            arch: Arch::X86_64,
            env: Env::Gnu,
            triple: "x86_64-unknown-linux-gnu".to_string(),
        }
    }

    fn install_result(
        package_id: &str,
        version: &str,
        path: &std::path::Path,
    ) -> transaction::InstallResult {
        transaction::InstallResult {
            installed: true,
            package: package_id.to_string(),
            version: version.to_string(),
            path: path.to_path_buf(),
            already_existed: false,
        }
    }

    #[test]
    fn try_cli_rejects_obsolete_yes_flag() {
        assert!(
            Cli::try_parse_from(["numan", "try", "--yes"]).is_err(),
            "numan try --yes must be rejected"
        );
        let cli = Cli::try_parse_from(["numan", "try", "foo/bar", "--no-activate"]).unwrap();
        match cli.command {
            Commands::Try(args) => {
                assert!(args.no_activate);
                assert_eq!(args.package.as_deref(), Some("foo/bar"));
            }
            _ => panic!("expected Try command"),
        }
    }

    #[test]
    fn try_requires_package_argument() {
        let root = tempfile::tempdir().unwrap();
        let args = TryArgs {
            package: None,
            no_activate: false,
        };
        let err = execute(&args, root.path()).unwrap_err().to_string();
        assert!(err.contains("a package is required"), "{err}");
    }

    #[test]
    fn try_package_too_new_recommends_older_nu() {
        let root = tempfile::tempdir().unwrap();
        let package = pkg("test/plugin", ">=0.113.0 <0.114.0", PackageType::Plugin);
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let args = TryArgs {
            package: Some("test/plugin".to_string()),
            no_activate: false,
        };
        let err = try_package(
            &args,
            root.path(),
            &package,
            "test/plugin",
            "test/plugin",
            None,
            &platform,
            &nu,
            |_, _, _| panic!("install must not be called"),
            |_, _| panic!("activate must not be called"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not compatible with Nu 0.114.1"), "{err}");
        assert!(err.contains("Compatible managed versions:"), "{err}");
        assert!(err.contains("0.113.1"), "{err}");
        assert!(err.contains("Newest compatible version: 0.113.1"), "{err}");
        assert!(err.contains("numan use 0.113.1"), "{err}");
    }

    #[test]
    fn try_package_too_old_recommends_newer_nu() {
        let root = tempfile::tempdir().unwrap();
        let package = pkg("test/plugin", ">=0.113.0 <0.114.0", PackageType::Plugin);
        let platform = windows_platform();
        let nu = NuVersion::parse("0.112.0").unwrap();
        let args = TryArgs {
            package: Some("test/plugin".to_string()),
            no_activate: false,
        };
        let err = try_package(
            &args,
            root.path(),
            &package,
            "test/plugin",
            "test/plugin",
            None,
            &platform,
            &nu,
            |_, _, _| panic!("install must not be called"),
            |_, _| panic!("activate must not be called"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("needs a newer Nu"), "{err}");
        assert!(err.contains("Compatible managed versions:"), "{err}");
        assert!(
            err.contains("Earliest compatible version: 0.113.0"),
            "{err}"
        );
    }

    #[test]
    fn try_package_both_sides_recommends_nearest() {
        let root = tempfile::tempdir().unwrap();
        let mut package = pkg("test/plugin", ">=0.114.0 <0.115.0", PackageType::Plugin);
        package.versions.push({
            let mut older = package.versions[0].clone();
            older.version = semver::Version::new(0, 1, 0);
            older.nu_version = ">=0.112.0 <0.113.0".to_string();
            older.verified_with = vec!["0.112.0".to_string()];
            older
        });
        package.versions.sort_by(|a, b| b.version.cmp(&a.version));

        let platform = windows_platform();
        let nu = NuVersion::parse("0.113.5").unwrap();
        let args = TryArgs {
            package: Some("test/plugin".to_string()),
            no_activate: false,
        };
        let err = try_package(
            &args,
            root.path(),
            &package,
            "test/plugin",
            "test/plugin",
            None,
            &platform,
            &nu,
            |_, _, _| panic!("install must not be called"),
            |_, _| panic!("activate must not be called"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Compatible managed versions:"), "{err}");
        assert!(err.contains("0.112.0"), "{err}");
        assert!(err.contains("0.114.0"), "{err}");
        assert!(err.contains("Nearest compatible version: 0.114.0"), "{err}");
    }

    #[test]
    fn try_package_missing_target_reports_platform() {
        let root = tempfile::tempdir().unwrap();
        let package = pkg("test/plugin", ">=0.113.0 <0.114.0", PackageType::Plugin);
        let platform = linux_platform();
        let nu = NuVersion::parse("0.113.1").unwrap();
        let args = TryArgs {
            package: Some("test/plugin".to_string()),
            no_activate: false,
        };
        let err = try_package(
            &args,
            root.path(),
            &package,
            "test/plugin",
            "test/plugin",
            None,
            &platform,
            &nu,
            |_, _, _| panic!("install must not be called"),
            |_, _| panic!("activate must not be called"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not available for your platform"), "{err}");
        assert!(err.contains("x86_64-unknown-linux-gnu"), "{err}");
    }

    #[test]
    fn try_package_no_known_compatible_nu() {
        let root = tempfile::tempdir().unwrap();
        let package = pkg("test/plugin", ">999.0.0", PackageType::Plugin);
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let args = TryArgs {
            package: Some("test/plugin".to_string()),
            no_activate: false,
        };
        let err = try_package(
            &args,
            root.path(),
            &package,
            "test/plugin",
            "test/plugin",
            None,
            &platform,
            &nu,
            |_, _, _| panic!("install must not be called"),
            |_, _| panic!("activate must not be called"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("No compatible managed Nu version is known"),
            "{err}"
        );
    }

    #[test]
    fn try_package_installs_and_activates() {
        let root = tempfile::tempdir().unwrap();
        let package = pkg("test/plugin", "*", PackageType::Plugin);
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let args = TryArgs {
            package: Some("test/plugin".to_string()),
            no_activate: false,
        };

        let installed = Cell::new(false);
        let activated = Cell::new(false);
        let result = try_package(
            &args,
            root.path(),
            &package,
            "test/plugin",
            "test/plugin",
            None,
            &platform,
            &nu,
            |id, ver, _| {
                installed.set(true);
                assert_eq!(id, "test/plugin");
                assert_eq!(ver, None);
                Ok(install_result("test/plugin", "1.0.0", root.path()))
            },
            |args, _| {
                activated.set(true);
                assert_eq!(args.packages, vec!["test/plugin".to_string()]);
                Ok(())
            },
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(installed.get());
        assert!(activated.get());
    }

    #[test]
    fn try_package_no_activate_skips_activation() {
        let root = tempfile::tempdir().unwrap();
        let package = pkg("test/plugin", "*", PackageType::Plugin);
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let args = TryArgs {
            package: Some("test/plugin".to_string()),
            no_activate: true,
        };

        let installed = Cell::new(false);
        let result = try_package(
            &args,
            root.path(),
            &package,
            "test/plugin",
            "test/plugin",
            None,
            &platform,
            &nu,
            |id, _, _| {
                installed.set(true);
                assert_eq!(id, "test/plugin");
                Ok(install_result("test/plugin", "1.0.0", root.path()))
            },
            |_, _| panic!("activate must not be called"),
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(installed.get());
    }

    #[test]
    fn try_package_already_installed_still_activates() {
        let root = tempfile::tempdir().unwrap();
        let package = pkg("test/plugin", "*", PackageType::Plugin);
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let args = TryArgs {
            package: Some("test/plugin".to_string()),
            no_activate: false,
        };

        let activated = Cell::new(false);
        let result = try_package(
            &args,
            root.path(),
            &package,
            "test/plugin",
            "test/plugin",
            None,
            &platform,
            &nu,
            |_, _, _| {
                Ok(transaction::InstallResult {
                    installed: false,
                    already_existed: true,
                    package: "test/plugin".to_string(),
                    version: "1.0.0".to_string(),
                    path: root.path().to_path_buf(),
                })
            },
            |_, _| {
                activated.set(true);
                Ok(())
            },
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(activated.get());
    }

    #[test]
    fn try_package_activation_failure_is_distinct() {
        let root = tempfile::tempdir().unwrap();
        let package = pkg("test/plugin", "*", PackageType::Plugin);
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let args = TryArgs {
            package: Some("test/plugin".to_string()),
            no_activate: false,
        };

        let installed = Cell::new(false);
        let err = try_package(
            &args,
            root.path(),
            &package,
            "test/plugin",
            "test/plugin",
            None,
            &platform,
            &nu,
            |_, _, _| {
                installed.set(true);
                Ok(install_result("test/plugin", "1.0.0", root.path()))
            },
            |_, _| Err(anyhow::anyhow!("plugin add failed")),
        )
        .unwrap_err()
        .to_string();
        assert!(installed.get(), "install must have run");
        assert!(err.contains("Activation failed after install"), "{err}");
    }

    #[test]
    fn report_incompatible_marks_installed_versions() {
        let package = pkg("test/plugin", ">=0.113.0 <0.114.0", PackageType::Plugin);
        let platform = windows_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let installed = vec!["0.113.1".to_string()];
        let err = report_incompatible(
            &package,
            &platform,
            &nu,
            "test/plugin",
            &Incompatibility::NuTooNew {
                constraint: ">=0.113.0 <0.114.0".to_string(),
            },
            &installed,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("0.113.1 (installed)"), "{err}");
        assert!(err.contains("Newest compatible version: 0.113.1"), "{err}");
    }

    #[test]
    fn collect_candidate_nu_versions_dedups_and_includes_installed() {
        let package = pkg("test/plugin", ">=0.113.0 <0.114.0", PackageType::Plugin);
        let installed = vec!["0.113.1".to_string(), "0.113.1".to_string()];
        let candidates = collect_candidate_nu_versions(&package, &installed);
        let versions: Vec<String> = candidates.into_iter().map(|v| v.version).collect();
        assert_eq!(versions, vec!["0.113.1".to_string(), "0.113.0".to_string()]);
    }

    fn script_lock_entry(
        payload_path: &str,
        entry: Option<&str>,
    ) -> crate::state::lockfile::LockfileEntry {
        crate::state::lockfile::LockfileEntry {
            version: "0.1.0".to_string(),
            package_type: "script".to_string(),
            source: "registry".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: None,
            archive_root: None,
            include: None,
            entry: entry.map(str::to_string),
            installed_at: "now".to_string(),
            nu_version_at_install: None,
            activation: None,
            registry_url: None,
            registry_revision: None,
            index_sha256: None,
            signing_key_fingerprint: None,
            git_url: None,
            git_rev: None,
            cargo_name: None,
            cargo_lock_sha256: None,
            built_sha256: None,
            payload_path: payload_path.to_string(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: BTreeMap::new(),
        }
    }

    #[test]
    fn format_overlay_use_hint_quotes_and_escapes_path() {
        let path = PathBuf::from(r#"C:\Numan Root\pkg\wttr.nu"#);
        let hint = format_overlay_use_hint(&path).unwrap();
        assert_eq!(hint, r#"overlay use "C:\\Numan Root\\pkg\\wttr.nu""#);
    }

    #[test]
    fn print_install_only_hint_uses_lockfile_entry_and_quotes_path() {
        let root = tempfile::tempdir().unwrap();
        let mut lock = Lockfile::empty();
        lock.packages.insert(
            "SuaveIV/nu_script_wttr".to_string(),
            script_lock_entry(
                "packages/scripts/SuaveIV/nu_script_wttr/0.1.0-deadbeef",
                Some("wttr.nu"),
            ),
        );
        lock.save(root.path()).unwrap();

        let err = print_install_only_hint(root.path(), "missing/pkg").unwrap_err();
        assert!(
            err.to_string().contains("no lockfile record"),
            "missing record must fail: {err:#}"
        );

        print_install_only_hint(root.path(), "SuaveIV/nu_script_wttr").unwrap();
        let full = root
            .path()
            .join("packages/scripts/SuaveIV/nu_script_wttr/0.1.0-deadbeef/wttr.nu");
        let expected = format_overlay_use_hint(&full).unwrap();
        assert!(expected.starts_with("overlay use \""));
        assert!(expected.contains("wttr.nu"));
    }

    #[test]
    fn print_install_only_hint_rejects_malformed_lockfile() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lockfile"), "{not-json").unwrap();
        let err = print_install_only_hint(root.path(), "any/pkg").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Failed to load lockfile") || msg.contains("expected"),
            "malformed lockfile must surface: {msg}"
        );
    }
}
