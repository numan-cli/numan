use anyhow::{bail, Context, Result};
use std::collections::HashSet;

use crate::core::nu_version::NuVersion;
use crate::core::package::{Package, PackageType, VersionEntry};
use crate::core::platform::Platform;
use crate::util::hints;

pub struct Resolver<'a> {
    pub platform: &'a Platform,
    pub nu_version: &'a NuVersion,
}

/// Why a version (or package) is not installable on this Nu + platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incompatibility {
    /// Current Nu is newer than the version's allowed range.
    NuTooNew { constraint: String },
    /// Current Nu is older than the version's minimum.
    NuTooOld { constraint: String },
    /// Nu does not satisfy the constraint for another reason.
    NuUnsatisfied { constraint: String },
    /// Binary artifact has no entry for this platform triple.
    MissingTarget { triple: String },
}

impl Incompatibility {
    pub fn short_label(&self) -> String {
        match self {
            Self::NuTooNew { constraint }
            | Self::NuTooOld { constraint }
            | Self::NuUnsatisfied { constraint } => {
                format!("needs Nu {constraint}")
            }
            Self::MissingTarget { triple } => format!("no artifact for {triple}"),
        }
    }
}

/// Package-level diagnosis when no version is compatible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageIncompatibility {
    pub issue: Incompatibility,
    /// Suggested managed Nu pin (`setup nu <VERSION>`), when one can be derived.
    pub suggested_pin: Option<String>,
    pub available_versions: Vec<String>,
}

impl<'a> Resolver<'a> {
    pub fn new(platform: &'a Platform, nu_version: &'a NuVersion) -> Self {
        Self {
            platform,
            nu_version,
        }
    }

    pub fn resolve<'b>(&self, package: &'b Package) -> Result<&'b VersionEntry> {
        if let Some(entry) = self.latest_compatible(package) {
            return Ok(entry);
        }

        let diagnosis = self.diagnose_package(package);
        bail!("{}", self.format_resolve_error(package, &diagnosis));
    }

    /// Latest compatible version, if any (semver descending).
    pub fn latest_compatible<'b>(&self, package: &'b Package) -> Option<&'b VersionEntry> {
        let mut candidates: Vec<&VersionEntry> = package
            .versions
            .iter()
            .filter(|v| self.is_compatible(v))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by(|a, b| b.version.cmp(&a.version));
        Some(candidates[0])
    }

    pub fn has_compatible_version(&self, package: &Package) -> bool {
        package.versions.iter().any(|v| self.is_compatible(v))
    }

    pub fn is_compatible(&self, version: &VersionEntry) -> bool {
        self.classify_version(version).is_none()
    }

    /// `None` when the version is compatible with this resolver.
    pub fn classify_version(&self, version: &VersionEntry) -> Option<Incompatibility> {
        if !self.nu_version.matches_constraint(&version.nu_version) {
            return Some(classify_nu_mismatch(self.nu_version, &version.nu_version));
        }

        if version.artifact.kind == "binary"
            && !version.artifact.targets.contains_key(&self.platform.triple)
        {
            return Some(Incompatibility::MissingTarget {
                triple: self.platform.triple.clone(),
            });
        }

        None
    }

    /// Diagnose why no version of `package` is compatible.
    pub fn diagnose_package(&self, package: &Package) -> PackageIncompatibility {
        let available_versions: Vec<String> = package
            .versions
            .iter()
            .map(|v| v.version.to_string())
            .collect();

        if package.versions.is_empty() {
            return PackageIncompatibility {
                issue: Incompatibility::MissingTarget {
                    triple: self.platform.triple.clone(),
                },
                suggested_pin: None,
                available_versions,
            };
        }

        // Prefer diagnosis of the newest version (what users usually try).
        let mut sorted: Vec<&VersionEntry> = package.versions.iter().collect();
        sorted.sort_by(|a, b| b.version.cmp(&a.version));

        let mut nu_only: Option<(&VersionEntry, Incompatibility)> = None;
        let mut missing_target: Option<Incompatibility> = None;

        for entry in &sorted {
            match self.classify_version(entry) {
                None => {
                    // Should not happen when diagnose is called after empty candidates.
                }
                Some(Incompatibility::MissingTarget { .. }) => {
                    if missing_target.is_none() {
                        missing_target = self.classify_version(entry);
                    }
                }
                Some(issue) if nu_only.is_none() => {
                    nu_only = Some((entry, issue));
                }
                Some(_) => {}
            }
        }

        // If every version fails Nu first (or newest fails Nu), prefer Nu diagnosis.
        if let Some((entry, issue)) = nu_only {
            let all_nu = package.versions.iter().all(|v| {
                matches!(
                    self.classify_version(v),
                    Some(
                        Incompatibility::NuTooNew { .. }
                            | Incompatibility::NuTooOld { .. }
                            | Incompatibility::NuUnsatisfied { .. }
                    )
                )
            });
            if all_nu || missing_target.is_none() {
                return PackageIncompatibility {
                    suggested_pin: suggest_managed_nu_pin(entry),
                    issue,
                    available_versions,
                };
            }
        }

        if let Some(issue) = missing_target {
            return PackageIncompatibility {
                issue,
                suggested_pin: None,
                available_versions,
            };
        }

        // Fallback: newest version's classification.
        let newest = sorted[0];
        let issue = self
            .classify_version(newest)
            .unwrap_or(Incompatibility::NuUnsatisfied {
                constraint: newest.nu_version.clone(),
            });
        PackageIncompatibility {
            suggested_pin: suggest_managed_nu_pin(newest),
            issue,
            available_versions,
        }
    }

    fn format_resolve_error(
        &self,
        package: &Package,
        diagnosis: &PackageIncompatibility,
    ) -> String {
        let available = if diagnosis.available_versions.is_empty() {
            "(none)".to_string()
        } else {
            diagnosis.available_versions.join(", ")
        };

        match &diagnosis.issue {
            Incompatibility::NuTooNew { constraint } => {
                let mut msg = format!(
                    "No compatible version found for '{}'.
       Your Nu version: {} ({}) is too new for the indexed builds.
       Required Nu: {constraint}
       Available package versions: {available}",
                    package.id, self.nu_version.version, self.platform.triple
                );
                append_nu_pin_options(&mut msg, package, diagnosis);
                msg
            }
            Incompatibility::NuTooOld { constraint } => {
                let mut msg = format!(
                    "No compatible version found for '{}'.
       Your Nu version: {} ({}) is too old for the indexed builds.
       Required Nu: {constraint}
       Available package versions: {available}",
                    package.id, self.nu_version.version, self.platform.triple
                );
                append_nu_pin_options(&mut msg, package, diagnosis);
                msg
            }
            Incompatibility::NuUnsatisfied { constraint } => {
                let mut msg = format!(
                    "No compatible version found for '{}'.
       Your Nu version: {} ({}) does not satisfy '{constraint}'.
       Available package versions: {available}",
                    package.id, self.nu_version.version, self.platform.triple
                );
                append_nu_pin_options(&mut msg, package, diagnosis);
                msg
            }
            Incompatibility::MissingTarget { triple } => {
                if matches!(package.package_type, PackageType::Plugin) {
                    format!(
                        "No compatible version found for '{}'.
       Your platform: {triple}
       Available package versions: {available}
       No binary artifact is indexed for this platform.
       Options:
         - Check for a module package that supports your platform
         - {}",
                        package.id,
                        hints::run(hints::CMD_REGISTRY_SYNC)
                    )
                } else {
                    format!(
                        "No compatible version found for '{}' on {triple}.",
                        package.id
                    )
                }
            }
        }
    }

    /// Resolve an exact version with compatibility validation.
    /// Returns an error if the version exists but is not compatible.
    pub fn resolve_exact<'b>(
        &self,
        package: &'b Package,
        target_version: &semver::Version,
    ) -> Result<&'b VersionEntry> {
        let entry = package
            .versions
            .iter()
            .find(|v| v.version == *target_version)
            .with_context(|| {
                let available: Vec<String> = package
                    .versions
                    .iter()
                    .map(|v| v.version.to_string())
                    .collect();
                format!(
                    "Version {target_version} not available for '{}'. Available: {}",
                    package.id,
                    available.join(", ")
                )
            })?;

        if let Some(issue) = self.classify_version(entry) {
            match issue {
                Incompatibility::NuTooNew { constraint }
                | Incompatibility::NuTooOld { constraint }
                | Incompatibility::NuUnsatisfied { constraint } => {
                    let mut msg = format!(
                        "Version {target_version} of '{}' is not compatible with your system: \
                         Nu version {} does not satisfy constraint '{constraint}'",
                        package.id, self.nu_version.version
                    );
                    append_nu_mismatch_remediation(
                        &mut msg,
                        package,
                        suggest_managed_nu_pin(entry).as_deref(),
                        NuMismatchRemediation::ExactVersion,
                    );
                    bail!("{msg}");
                }
                Incompatibility::MissingTarget { triple } => {
                    bail!(
                        "Version {target_version} of '{}' is not compatible with your system: \
                         No binary for target '{triple}'",
                        package.id
                    );
                }
            }
        }

        Ok(entry)
    }
}

fn append_nu_pin_options(msg: &mut String, package: &Package, diagnosis: &PackageIncompatibility) {
    append_nu_mismatch_remediation(
        msg,
        package,
        diagnosis.suggested_pin.as_deref(),
        NuMismatchRemediation::Resolve,
    );
}

#[derive(Clone, Copy)]
enum NuMismatchRemediation {
    /// Latest-compatible resolution failed for the package as a whole.
    Resolve,
    /// A specific requested version exists but is incompatible.
    ExactVersion,
}

/// Shared Nu-mismatch remediation for resolve / resolve_exact.
///
/// Suggests `setup nu <PIN>` for an ABI-compatible managed Nu and follows with
/// `numan init --refresh && numan install <pkg>`. Notes that activations are
/// per-Nu (re-run `numan activate` after switching). Does NOT promise PATH is
/// untouched: `setup nu` may update PATH unless the caller passes
/// `--skip-path`. Never suggests `registry sync` as an ABI fix.
fn append_nu_mismatch_remediation(
    msg: &mut String,
    package: &Package,
    pin: Option<&str>,
    remediation: NuMismatchRemediation,
) {
    if matches!(package.package_type, PackageType::Plugin) {
        msg.push_str(
            "\n       Plugins only work with the Nu minor they were built for; \
             this install would not load in your current shell.",
        );
    }
    msg.push_str("\n       Options:");
    if let Some(pin) = pin {
        msg.push_str(&format!(
            "\n         - Install a matching managed Nu: numan setup nu {pin}\n           (installs under your Numan root; your existing Nu is not replaced)"
        ));
        msg.push_str(&format!(
            "\n         - Then: {}",
            hints::run_then(
                hints::CMD_INIT_REFRESH,
                &hints::install_pkg(&package.id.to_string())
            )
        ));
        msg.push_str(
            "\n         - Note: activations are per-Nu; re-run `numan activate` after switching",
        );
    } else {
        msg.push_str(&format!(
            "\n         - Install a Nu that satisfies the package constraint: {}",
            hints::CMD_SETUP_NU
        ));
        msg.push_str(
            "\n           (managed install under your Numan root; your existing Nu is not replaced)",
        );
    }
    msg.push_str(
        "\n         - Or pick a different package that supports your current Nu (`numan search <query>`)",
    );
    match remediation {
        NuMismatchRemediation::Resolve => {
            msg.push_str(&format!(
                "\n         - To see why this package is incompatible: `numan search {} --all`",
                package.id
            ));
        }
        NuMismatchRemediation::ExactVersion => {
            msg.push_str(&format!(
                "\n         - To see version compatibility details: `numan info {}`",
                package.id
            ));
        }
    }
    msg.push_str("\n       Nothing was installed. No changes were made.");
}

/// Classify a Nu constraint mismatch relative to the current Nu.
pub fn classify_nu_mismatch(nu: &NuVersion, constraint: &str) -> Incompatibility {
    let (min, max_exclusive) = parse_constraint_bounds(constraint);
    let current = (nu.major, nu.minor, nu.patch);

    if let Some(min) = min {
        if current < min {
            return Incompatibility::NuTooOld {
                constraint: constraint.to_string(),
            };
        }
    }
    if let Some(max) = max_exclusive {
        if current >= max {
            return Incompatibility::NuTooNew {
                constraint: constraint.to_string(),
            };
        }
    }

    Incompatibility::NuUnsatisfied {
        constraint: constraint.to_string(),
    }
}

/// Suggest a concrete Nu version to install for this package version.
pub fn suggest_managed_nu_pin(entry: &VersionEntry) -> Option<String> {
    // Prefer verified_with entries that satisfy the constraint.
    let mut verified: Vec<NuVersion> = entry
        .verified_with
        .iter()
        .filter_map(|v| NuVersion::parse(v).ok())
        .filter(|v| v.matches_constraint(&entry.nu_version))
        .collect();
    verified.sort_by(|a, b| {
        (a.major, a.minor, a.patch)
            .cmp(&(b.major, b.minor, b.patch))
            .reverse()
    });
    if let Some(best) = verified.first() {
        return Some(best.version.clone());
    }

    // Derive from exclusive upper bound: `<0.114.0` + `>=0.113.0` → `0.113.1` (or `.0`).
    let (min, max_exclusive) = parse_constraint_bounds(&entry.nu_version);
    if let Some((maj, min_minor, _)) = max_exclusive {
        if min_minor > 0 {
            let candidate_minor = min_minor - 1;
            let pin = format!("{maj}.{candidate_minor}.1");
            if let Ok(parsed) = NuVersion::parse(&pin) {
                if parsed.matches_constraint(&entry.nu_version) {
                    return Some(pin);
                }
            }
            let pin0 = format!("{maj}.{candidate_minor}.0");
            if let Ok(parsed) = NuVersion::parse(&pin0) {
                if parsed.matches_constraint(&entry.nu_version) {
                    return Some(pin0);
                }
            }
        }
    }

    if let Some((maj, min_minor, min_patch)) = min {
        let pin = format!("{maj}.{min_minor}.{min_patch}");
        if let Ok(parsed) = NuVersion::parse(&pin) {
            if parsed.matches_constraint(&entry.nu_version) {
                return Some(pin);
            }
        }
    }

    None
}

type VersionTriple = (u64, u64, u64);

fn parse_constraint_bounds(constraint: &str) -> (Option<VersionTriple>, Option<VersionTriple>) {
    let mut min = None;
    let mut max_exclusive = None;
    for part in constraint.split_whitespace() {
        if let Some(ver) = part.strip_prefix(">=") {
            if let Ok(v) = parse_triple(ver) {
                min = Some(v);
            }
        } else if let Some(ver) = part.strip_prefix('<') {
            if !ver.starts_with('=') {
                if let Ok(v) = parse_triple(ver) {
                    max_exclusive = Some(v);
                }
            }
        }
    }
    (min, max_exclusive)
}

fn parse_triple(v: &str) -> Result<VersionTriple> {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3 {
        bail!("Invalid version: '{v}'");
    }
    Ok((parts[0].parse()?, parts[1].parse()?, parts[2].parse()?))
}

/// Return candidate Nu versions derived from a package version's `nu_version` constraint.
///
/// This is metadata-driven: it includes the lower bound and a patch just below
/// an exclusive upper bound. The returned strings are not guaranteed to satisfy
/// the constraint; callers must validate them. Verified-with versions are added
/// separately by the caller.
pub fn candidate_nu_versions_from_constraint(constraint: &str) -> Vec<String> {
    let mut out = Vec::new();

    // Exact-minor legacy form: =0.113.x -> try 0.113.1 and 0.113.0.
    if let Some(ver) = constraint.strip_prefix('=') {
        if let Some(ver) = ver.strip_prefix("0.") {
            if let Some(ver) = ver.strip_suffix(".x") {
                if let Ok(minor) = ver.parse::<u64>() {
                    out.push(format!("0.{minor}.1"));
                    out.push(format!("0.{minor}.0"));
                }
            }
        }
    }

    let (min, max_exclusive) = parse_constraint_bounds(constraint);
    if let Some((maj, min_minor, patch)) = min {
        out.push(format!("{maj}.{min_minor}.{patch}"));
    }
    if let Some((maj, max_minor, patch)) = max_exclusive {
        if max_minor == 0 && patch == 0 {
            // <0.0.0: nothing useful to derive.
        } else if patch == 0 {
            // <0.X.0: the previous minor's newest common patch.
            let candidate_minor = max_minor - 1;
            out.push(format!("{maj}.{candidate_minor}.1"));
            out.push(format!("{maj}.{candidate_minor}.0"));
        } else {
            out.push(format!("{maj}.{max_minor}.{}", patch - 1));
        }
    }

    out
}

/// Return the Nu versions from `candidates` where `package` has at least one
/// compatible version entry for the current platform.
pub fn compatible_nu_versions(
    package: &Package,
    platform: &Platform,
    candidates: &[NuVersion],
) -> Vec<NuVersion> {
    let mut compatible = Vec::new();
    let mut seen = HashSet::new();
    for candidate in candidates {
        if !seen.insert((candidate.major, candidate.minor, candidate.patch)) {
            continue;
        }
        let resolver = Resolver::new(platform, candidate);
        if resolver.has_compatible_version(package) {
            compatible.push(candidate.clone());
        }
    }
    compatible.sort_by_key(|v| (v.major, v.minor, v.patch));
    compatible
}

/// Return the Nu versions from `candidates` where the specific `entry` is
/// compatible for the current platform.
///
/// Unlike [`compatible_nu_versions`], this scopes compatibility to a single
/// version entry rather than any entry in the package. Used when the user
/// pinned `owner/name@version` so the recommendation reflects that release's
/// constraint, not a different release that happens to be compatible.
pub fn compatible_nu_versions_for_entry(
    entry: &VersionEntry,
    platform: &Platform,
    candidates: &[NuVersion],
) -> Vec<NuVersion> {
    let mut compatible = Vec::new();
    let mut seen = HashSet::new();
    for candidate in candidates {
        if !seen.insert((candidate.major, candidate.minor, candidate.patch)) {
            continue;
        }
        let resolver = Resolver::new(platform, candidate);
        if resolver.is_compatible(entry) {
            compatible.push(candidate.clone());
        }
    }
    compatible.sort_by_key(|v| (v.major, v.minor, v.patch));
    compatible
}

fn version_score(v: &NuVersion) -> u64 {
    v.major * 1_000_000_000 + v.minor * 1_000_000 + v.patch
}

/// Pick the nearest sensible compatible Nu version.
///
/// * If all compatible versions are older than `current`, prefer the newest
///   installed older version, otherwise the newest older catalog version.
/// * If all compatible versions are newer, prefer the earliest installed newer
///   version, otherwise the earliest newer catalog version.
/// * If compatible versions exist on both sides, prefer the nearest installed
///   version, otherwise the nearest catalog version (ties go to newer).
pub fn select_recommended_nu<'a>(
    current: &NuVersion,
    compatible: &'a [NuVersion],
    installed: &HashSet<String>,
) -> Option<&'a NuVersion> {
    if compatible.is_empty() {
        return None;
    }

    let current_score = version_score(current);
    let older: Vec<&'a NuVersion> = compatible
        .iter()
        .filter(|v| version_score(v) < current_score)
        .collect();
    let newer: Vec<&'a NuVersion> = compatible
        .iter()
        .filter(|v| version_score(v) > current_score)
        .collect();

    let older_installed: Vec<&'a NuVersion> = older
        .iter()
        .filter(|v| installed.contains(&v.version))
        .copied()
        .collect();
    let newer_installed: Vec<&'a NuVersion> = newer
        .iter()
        .filter(|v| installed.contains(&v.version))
        .copied()
        .collect();

    if !older.is_empty() && newer.is_empty() {
        // Need an older version; prefer newest installed, else newest overall.
        if let Some(v) = older_installed
            .iter()
            .max_by_key(|v| version_score(v))
            .copied()
        {
            return Some(v);
        }
        return older.iter().max_by_key(|v| version_score(v)).copied();
    }

    if !newer.is_empty() && older.is_empty() {
        // Need a newer version; prefer earliest installed, else earliest overall.
        if let Some(v) = newer_installed
            .iter()
            .min_by_key(|v| version_score(v))
            .copied()
        {
            return Some(v);
        }
        return newer.iter().min_by_key(|v| version_score(v)).copied();
    }

    // Compatible on both sides or none (current not in list). Prefer nearest
    // installed, otherwise nearest catalog version; ties go to newer.
    let nearest = nearest_comparator(current_score);
    let installed_all: Vec<&'a NuVersion> = compatible
        .iter()
        .filter(|v| installed.contains(&v.version))
        .collect();
    if !installed_all.is_empty() {
        return installed_all.iter().copied().min_by(nearest);
    }

    compatible.iter().min_by(nearest)
}

/// Comparator that orders Nu versions by distance from `current_score`,
/// tie-breaking toward the higher version score.
fn nearest_comparator(
    current_score: u64,
) -> impl Fn(&&NuVersion, &&NuVersion) -> std::cmp::Ordering {
    move |a, b| {
        let da = version_score(a).abs_diff(current_score);
        let db = version_score(b).abs_diff(current_score);
        da.cmp(&db)
            .then_with(|| version_score(b).cmp(&version_score(a)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::package::*;
    use crate::core::platform::{Arch, Env, Os};
    use std::collections::{BTreeMap, HashMap, HashSet};

    fn test_plugin() -> Package {
        let mut targets = HashMap::new();
        targets.insert(
            "x86_64-pc-windows-msvc".to_string(),
            TargetArtifact {
                url: "https://example.com/win.zip".to_string(),
                sha256: "abc".to_string(),
                executable_path: "nu_plugin_test.exe".to_string(),
            },
        );
        targets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetArtifact {
                url: "https://example.com/linux.tar.gz".to_string(),
                sha256: "def".to_string(),
                executable_path: "nu_plugin_test".to_string(),
            },
        );

        Package {
            id: ScopedId::new("test", "plugin"),
            description: "Test plugin".to_string(),
            repo: "https://github.com/test/plugin".to_string(),
            package_type: PackageType::Plugin,
            tags: vec![],
            versions: vec![
                VersionEntry {
                    version: semver::Version::new(2, 0, 0),
                    nu_version: ">=0.113.0 <0.114.0".to_string(),
                    verified_with: vec!["0.113.1".to_string()],
                    artifact: Artifact {
                        kind: "binary".to_string(),
                        url: None,
                        sha256: None,
                        targets: targets.clone(),
                        archive_root: None,
                        include: None,
                        entry: None,
                    },
                    source: None,
                    dependencies: BTreeMap::new(),
                    activation: None,
                    provenance: None,
                },
                VersionEntry {
                    version: semver::Version::new(1, 0, 0),
                    nu_version: ">=0.112.0 <0.113.0".to_string(),
                    verified_with: vec![],
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
                    provenance: None,
                },
            ],
        }
    }

    fn linux_platform() -> Platform {
        Platform {
            os: crate::core::platform::Os::Linux,
            arch: crate::core::platform::Arch::X86_64,
            env: crate::core::platform::Env::Gnu,
            triple: "x86_64-unknown-linux-gnu".to_string(),
        }
    }

    #[test]
    fn resolve_latest_compatible_plugin() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.113.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let pkg = test_plugin();
        let resolved = resolver.resolve(&pkg).unwrap();
        assert_eq!(resolved.version, semver::Version::new(2, 0, 0));
    }

    #[test]
    fn resolve_falls_back_to_older_version() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.112.5").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let pkg = test_plugin();
        let resolved = resolver.resolve(&pkg).unwrap();
        assert_eq!(resolved.version, semver::Version::new(1, 0, 0));
    }

    #[test]
    fn resolve_no_compatible_version() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.110.0").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let pkg = test_plugin();
        assert!(resolver.resolve(&pkg).is_err());
    }

    #[test]
    fn resolve_error_nu_too_new_mentions_pin() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let pkg = test_plugin();
        let err = resolver.resolve(&pkg).unwrap_err().to_string();
        assert!(err.contains("too new"), "{err}");
        assert!(err.contains("setup nu 0.113.1"), "{err}");
        assert!(
            err.contains("existing Nu is not replaced"),
            "expected existing-Nu-untouched note: {err}"
        );
        assert!(
            err.contains("Nothing was installed. No changes were made."),
            "{err}"
        );
        assert!(
            err.contains("Plugins only work with the Nu minor"),
            "expected plugin ABI note: {err}"
        );
        assert!(
            err.contains("numan search test/plugin --all"),
            "expected incompatible-package search hint: {err}"
        );
        assert!(
            !err.contains("registry sync"),
            "must not pitch registry sync as ABI fix: {err}"
        );
        assert!(!err.contains("Upgrade Nu:"), "{err}");
        assert!(!err.contains("Install an older version:"), "{err}");
    }

    #[test]
    fn resolve_exact_nu_mismatch_matches_honesty_contract() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let pkg = test_plugin();
        let target = semver::Version::new(2, 0, 0);
        let err = resolver
            .resolve_exact(&pkg, &target)
            .unwrap_err()
            .to_string();
        assert!(err.contains("setup nu 0.113.1"), "{err}");
        assert!(err.contains("existing Nu is not replaced"), "{err}");
        assert!(
            err.contains("numan search <query>"),
            "expected alternate-package guidance: {err}"
        );
        assert!(
            err.contains("numan info test/plugin"),
            "expected per-version info hint: {err}"
        );
        assert!(
            !err.contains("numan search test/plugin --all"),
            "exact-version failures should not suggest search --all: {err}"
        );
        assert!(
            err.contains("Nothing was installed. No changes were made."),
            "{err}"
        );
        assert!(!err.contains("registry sync"), "{err}");
    }

    #[test]
    fn resolve_exact_nu_mismatch_without_pin_uses_generic_setup_hint() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.114.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let mut pkg = test_plugin();
        // Open lower-bound-only `>` is not parsed for pin derivation
        // (suggest_managed_nu_pin only uses >= / <), so pin stays None.
        for v in &mut pkg.versions {
            v.nu_version = ">999.0.0".to_string();
            v.verified_with.clear();
        }
        assert!(suggest_managed_nu_pin(&pkg.versions[0]).is_none());

        let target = semver::Version::new(2, 0, 0);
        let err = resolver
            .resolve_exact(&pkg, &target)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Install a Nu that satisfies the package constraint: numan setup nu"),
            "{err}"
        );
        assert!(
            !err.contains("setup nu --version"),
            "must not invent a concrete pin: {err}"
        );
        assert!(err.contains("existing Nu is not replaced"), "{err}");
        assert!(
            err.contains("numan search <query>"),
            "expected alternate-package guidance: {err}"
        );
        assert!(
            err.contains("numan info test/plugin"),
            "expected per-version info hint: {err}"
        );
        assert!(
            !err.contains("numan search test/plugin --all"),
            "exact-version failures should not suggest search --all: {err}"
        );
        assert!(
            err.contains("Nothing was installed. No changes were made."),
            "{err}"
        );
        assert!(!err.contains("registry sync"), "{err}");
    }

    #[test]
    fn resolve_error_nu_too_old() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.110.0").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let pkg = test_plugin();
        let err = resolver.resolve(&pkg).unwrap_err().to_string();
        assert!(err.contains("too old"), "{err}");
    }

    #[test]
    fn resolve_error_missing_target() {
        let platform = Platform {
            os: crate::core::platform::Os::Macos,
            arch: crate::core::platform::Arch::Aarch64,
            env: crate::core::platform::Env::Darwin,
            triple: "aarch64-apple-darwin".to_string(),
        };
        let nu = NuVersion::parse("0.113.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let mut pkg = test_plugin();
        // Keep only windows targets so linux/mac fail on missing target.
        for v in &mut pkg.versions {
            v.artifact.targets.retain(|k, _| k.contains("windows"));
            v.nu_version = ">=0.113.0 <0.114.0".to_string();
        }
        let err = resolver.resolve(&pkg).unwrap_err().to_string();
        assert!(
            err.contains("No binary artifact") || err.contains("platform"),
            "{err}"
        );
    }

    #[test]
    fn suggest_pin_prefers_verified_with() {
        let pkg = test_plugin();
        let pin = suggest_managed_nu_pin(&pkg.versions[0]).unwrap();
        assert_eq!(pin, "0.113.1");
    }

    #[test]
    fn classify_nu_too_new_and_too_old() {
        let nu_new = NuVersion::parse("0.114.1").unwrap();
        let issue = classify_nu_mismatch(&nu_new, ">=0.113.0 <0.114.0");
        assert!(matches!(issue, Incompatibility::NuTooNew { .. }));

        let nu_old = NuVersion::parse("0.112.0").unwrap();
        let issue = classify_nu_mismatch(&nu_old, ">=0.113.0 <0.114.0");
        assert!(matches!(issue, Incompatibility::NuTooOld { .. }));
    }

    #[test]
    fn resolve_exact_compatible() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.113.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let pkg = test_plugin();
        let v = semver::Version::new(2, 0, 0);
        let resolved = resolver.resolve_exact(&pkg, &v).unwrap();
        assert_eq!(resolved.version, v);
    }

    #[test]
    fn resolve_exact_incompatible_nu() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.112.5").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let pkg = test_plugin();
        let v = semver::Version::new(2, 0, 0);
        let result = resolver.resolve_exact(&pkg, &v);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not compatible"));
    }

    #[test]
    fn resolve_exact_incompatible_target() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.112.5").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let pkg = test_plugin();
        let mut pkg_no_target = pkg.clone();
        pkg_no_target.versions[1].artifact.targets.clear();
        let v = semver::Version::new(1, 0, 0);
        let result = resolver.resolve_exact(&pkg_no_target, &v);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No binary"));
    }

    #[test]
    fn resolve_exact_not_found() {
        let platform = linux_platform();
        let nu = NuVersion::parse("0.113.1").unwrap();
        let resolver = Resolver::new(&platform, &nu);
        let pkg = test_plugin();
        let v = semver::Version::new(99, 0, 0);
        assert!(resolver.resolve_exact(&pkg, &v).is_err());
    }

    #[test]
    fn candidate_nu_versions_from_constraint_derives_bounds() {
        let mut candidates = candidate_nu_versions_from_constraint(">=0.112.0 <0.114.0");
        candidates.sort();
        assert_eq!(candidates, vec!["0.112.0", "0.113.0", "0.113.1"]);

        let exact = candidate_nu_versions_from_constraint("=0.113.x");
        assert!(exact.contains(&"0.113.0".to_string()));
        assert!(exact.contains(&"0.113.1".to_string()));

        let minimum = candidate_nu_versions_from_constraint(">=0.113.0");
        assert_eq!(minimum, vec!["0.113.0"]);

        assert!(candidate_nu_versions_from_constraint("*").is_empty());
    }

    fn windows_plugin() -> Package {
        let platform = Platform {
            os: Os::Windows,
            arch: Arch::X86_64,
            env: Env::Msvc,
            triple: "x86_64-pc-windows-msvc".to_string(),
        };
        let targets = {
            let mut m = HashMap::new();
            m.insert(
                platform.triple.clone(),
                TargetArtifact {
                    url: "https://example.com/p.zip".to_string(),
                    sha256: "aa".to_string(),
                    executable_path: "p.exe".to_string(),
                },
            );
            m
        };
        Package {
            id: ScopedId::new("test", "plugin"),
            description: "d".to_string(),
            repo: "https://example.com".to_string(),
            package_type: PackageType::Plugin,
            tags: vec![],
            versions: vec![VersionEntry {
                version: semver::Version::new(1, 0, 0),
                nu_version: ">=0.112.0 <0.114.0".to_string(),
                verified_with: vec!["0.113.1".to_string()],
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
                provenance: None,
            }],
        }
    }

    #[test]
    fn compatible_nu_versions_lists_all_matching_nus() {
        let platform = Platform {
            os: Os::Windows,
            arch: Arch::X86_64,
            env: Env::Msvc,
            triple: "x86_64-pc-windows-msvc".to_string(),
        };
        let pkg = windows_plugin();
        let candidates = vec![
            NuVersion::parse("0.112.0").unwrap(),
            NuVersion::parse("0.113.1").unwrap(),
            NuVersion::parse("0.114.1").unwrap(),
        ];
        let compatible = compatible_nu_versions(&pkg, &platform, &candidates);
        assert_eq!(compatible.len(), 2);
        assert_eq!(compatible[0].version, "0.112.0");
        assert_eq!(compatible[1].version, "0.113.1");
    }

    #[test]
    fn compatible_nu_versions_respects_platform() {
        let platform = Platform {
            os: Os::Linux,
            arch: Arch::X86_64,
            env: Env::Gnu,
            triple: "x86_64-unknown-linux-gnu".to_string(),
        };
        let pkg = windows_plugin();
        let candidates = vec![
            NuVersion::parse("0.112.0").unwrap(),
            NuVersion::parse("0.113.1").unwrap(),
        ];
        assert!(compatible_nu_versions(&pkg, &platform, &candidates).is_empty());
    }

    #[test]
    fn select_recommended_nu_prefers_newest_older() {
        let current = NuVersion::parse("0.114.1").unwrap();
        let compatible = vec![
            NuVersion::parse("0.112.0").unwrap(),
            NuVersion::parse("0.113.1").unwrap(),
        ];
        let installed = HashSet::new();
        let rec = select_recommended_nu(&current, &compatible, &installed).unwrap();
        assert_eq!(rec.version, "0.113.1");
    }

    #[test]
    fn select_recommended_nu_prefers_earliest_newer() {
        let current = NuVersion::parse("0.112.0").unwrap();
        let compatible = vec![
            NuVersion::parse("0.113.1").unwrap(),
            NuVersion::parse("0.114.0").unwrap(),
        ];
        let installed = HashSet::new();
        let rec = select_recommended_nu(&current, &compatible, &installed).unwrap();
        assert_eq!(rec.version, "0.113.1");
    }

    #[test]
    fn select_recommended_nu_prefers_installed() {
        let current = NuVersion::parse("0.114.1").unwrap();
        let compatible = vec![
            NuVersion::parse("0.112.0").unwrap(),
            NuVersion::parse("0.113.1").unwrap(),
        ];
        let mut installed = HashSet::new();
        installed.insert("0.112.0".to_string());
        let rec = select_recommended_nu(&current, &compatible, &installed).unwrap();
        assert_eq!(rec.version, "0.112.0");
    }

    #[test]
    fn select_recommended_nu_chooses_nearest_when_both_sides() {
        let current = NuVersion::parse("0.113.0").unwrap();
        let compatible = vec![
            NuVersion::parse("0.112.0").unwrap(),
            NuVersion::parse("0.114.0").unwrap(),
        ];
        let installed = HashSet::new();
        let rec = select_recommended_nu(&current, &compatible, &installed).unwrap();
        // Both are the same numeric distance; tie goes to newer.
        assert_eq!(rec.version, "0.114.0");
    }
}
