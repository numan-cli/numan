use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::core::package::ScopedId;
use crate::nupm_compat::{
    compare_import, count_drifted_imports, format_drift_report, format_inspection_report,
    format_status_report, import_manifest_with_runner, inspect_path, resolve_nupm_home,
    scan_nupm_home, NupmCandidateReport, NupmCompatibility, NupmHomeResolution,
    NupmInspectionReport, NupmStatusReport,
};
use crate::state::lockfile::Lockfile;
use crate::state::nupm_import::NupmImportsFile;
use crate::util::hints::{self, CMD_INIT, CMD_INIT_REFRESH};

#[derive(clap::Parser)]
pub struct NupmArgs {
    #[command(subcommand)]
    pub command: NupmCommands,
}

#[derive(clap::Subcommand)]
pub enum NupmCommands {
    Status(StatusArgs),
    Inspect(InspectArgs),
    Import(ImportArgs),
    Diff(DiffArgs),
}

#[derive(clap::Parser)]
pub struct StatusArgs {
    #[arg(long)]
    pub nupm_home: Option<std::path::PathBuf>,
}

#[derive(clap::Parser)]
pub struct InspectArgs {
    /// Inspect all discoverable candidates under nupm home
    #[arg(long, group = "inspect_target")]
    pub all: bool,

    /// Package source path (mutually exclusive with --all)
    #[arg(value_name = "PATH", group = "inspect_target", value_hint = clap::ValueHint::DirPath)]
    pub path: Option<std::path::PathBuf>,

    #[arg(long)]
    pub nupm_home: Option<std::path::PathBuf>,

    /// Exit with code 1 if any inspected package is not import-eligible
    #[arg(long)]
    pub exit_on_ineligible: bool,
}

#[derive(clap::Parser)]
pub struct ImportArgs {
    /// Package source root or path inside it (single import)
    #[arg(value_name = "PATH", group = "import_target", value_hint = clap::ValueHint::DirPath)]
    pub path: Option<std::path::PathBuf>,

    /// TOML manifest of imports (batch)
    #[arg(long, group = "import_target")]
    pub manifest: Option<std::path::PathBuf>,

    #[arg(long)]
    pub nupm_home: Option<std::path::PathBuf>,

    /// Target Numan scoped ID (required for single import)
    #[arg(long, value_name = "OWNER/NAME")]
    pub r#as: Option<String>,

    /// Skip confirmation prompts
    #[arg(long)]
    pub yes: bool,
}

#[derive(clap::Parser)]
pub struct DiffArgs {
    /// Numan scoped ID (owner/name) of a nupm import
    pub package_id: String,
}

pub fn execute(args: &NupmArgs, numan_root: &Path, out: &mut dyn Write) -> Result<()> {
    match &args.command {
        NupmCommands::Status(a) => run_status(a, numan_root, out),
        NupmCommands::Inspect(a) => run_inspect(a, out),
        NupmCommands::Import(a) => run_import(a, numan_root, out),
        NupmCommands::Diff(a) => run_diff(a, numan_root, out),
    }
}

fn run_diff(args: &DiffArgs, numan_root: &Path, out: &mut dyn Write) -> Result<()> {
    let _ = ScopedId::parse(&args.package_id)?;
    let report = compare_import(numan_root, &args.package_id)?;
    if matches!(
        report.status,
        crate::nupm_compat::DriftStatus::CannotCompare { .. }
    ) {
        format_drift_report(&report, out)?;
        bail!(
            "Cannot compare drift for '{}'. {}",
            args.package_id,
            hints::run(&hints::nupm_diff_pkg(&args.package_id))
        );
    }
    format_drift_report(&report, out)?;
    Ok(())
}

fn run_import(args: &ImportArgs, numan_root: &Path, out: &mut dyn Write) -> Result<()> {
    if let Some(manifest) = &args.manifest {
        if args.path.is_some() {
            bail!("Cannot use PATH with --manifest");
        }
        let runner = default_runner(numan_root)?;
        let result = import_manifest_with_runner(
            numan_root,
            manifest,
            args.nupm_home.as_deref(),
            args.yes,
            &runner,
        )?;
        writeln!(
            out,
            "Imported {} package(s) from manifest",
            result.imports.len()
        )?;
        for item in result.imports {
            write_import_result(out, &item)?;
        }
        return Ok(());
    }

    let path = args
        .path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("import requires PATH or --manifest"))?;
    let target_str = args.r#as.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "single import requires --as owner/name. {}",
            hints::run("numan nupm inspect <path>")
        )
    })?;
    let target = ScopedId::parse(target_str)?;
    let runner = default_runner(numan_root)?;
    let result = import_module_with_runner_cli(numan_root, path, &target, args.yes, &runner)?;
    write_import_result(out, &result)?;
    Ok(())
}

fn import_module_with_runner_cli(
    numan_root: &Path,
    path: &Path,
    target: &ScopedId,
    yes: bool,
    runner: &dyn crate::nu::autoload::CandidateRunner,
) -> Result<crate::nupm_compat::ImportResult> {
    crate::nupm_compat::import_module_with_runner(numan_root, path, target, yes, runner)
}

fn default_runner(numan_root: &Path) -> Result<crate::nu::autoload::NuCandidateRunner> {
    let nu_paths = crate::nu::paths::NuPaths::load(numan_root).with_context(|| {
        format!(
            "Nu paths are not configured. {}",
            hints::run_then(CMD_INIT, CMD_INIT_REFRESH)
        )
    })?;
    Ok(crate::nu::autoload::NuCandidateRunner::new(
        &nu_paths.nu_executable,
    ))
}

fn write_import_result(
    out: &mut dyn Write,
    result: &crate::nupm_compat::ImportResult,
) -> Result<()> {
    if result.skipped_unchanged {
        writeln!(
            out,
            "Skipped re-import of {} (source unchanged)",
            result.package_id
        )?;
        writeln!(out, "  revision_id: {}", result.revision_id)?;
        return Ok(());
    }
    writeln!(
        out,
        "Imported {}@{} to {}",
        result.package_id, result.version, result.payload_path
    )?;
    writeln!(out, "  revision_id: {}", result.revision_id)?;
    if result.reimported {
        if let Some(old_rev) = &result.old_revision_id {
            writeln!(out, "  previous revision_id: {old_rev}")?;
        }
        if let Some(old_path) = &result.old_payload_path {
            writeln!(out, "  previous payload_path: {old_path}")?;
        }
        if let Some(old_hash) = &result.old_source_payload_sha256 {
            writeln!(out, "  previous source payload sha256: {old_hash}")?;
        }
        writeln!(out, "  (re-import; prior revision retained until gc)")?;
    }
    writeln!(out, "  activation: not performed")?;
    Ok(())
}

fn run_status(args: &StatusArgs, numan_root: &Path, out: &mut dyn Write) -> Result<()> {
    let lockfile = Lockfile::load(numan_root)?;
    let numan_nupm_imports = lockfile.count_nupm_imports();
    let source_drift_count = count_drifted_imports(numan_root)?;

    match resolve_nupm_home(args.nupm_home.as_deref())? {
        NupmHomeResolution::NotConfigured => {
            let report = NupmStatusReport {
                nupm_home: None,
                home_not_configured: true,
                modules_dir_present: false,
                scripts_dir_present: false,
                import_eligible: 0,
                rejected_source: 0,
                installed_only: 0,
                script_entries: 0,
                unsafe_entries: 0,
                numan_nupm_imports,
                source_drift_count,
                name_overlap_count: 0,
            };
            format_status_report(&report, out)?;
            Ok(())
        }
        NupmHomeResolution::Found(home) => {
            let scan = scan_nupm_home(&home)?;
            let (import_eligible, rejected_source) = count_source(&scan.source_roots);
            let name_overlap_count = count_name_overlap(numan_root, &lockfile, &scan.source_roots)?;
            let report = NupmStatusReport {
                nupm_home: Some(home.clone()),
                home_not_configured: false,
                modules_dir_present: home.join("modules").is_dir(),
                scripts_dir_present: home.join("scripts").is_dir(),
                import_eligible,
                rejected_source,
                installed_only: scan.installed_only.len(),
                script_entries: scan.script_entries,
                unsafe_entries: scan.unsafe_entries,
                numan_nupm_imports,
                source_drift_count,
                name_overlap_count,
            };
            format_status_report(&report, out)?;
            Ok(())
        }
    }
}

fn count_name_overlap(
    numan_root: &Path,
    lockfile: &Lockfile,
    source_roots: &[crate::nupm_compat::SourceRootEntry],
) -> Result<usize> {
    let imports = NupmImportsFile::load(numan_root)?;
    let mut count = 0usize;
    for root in source_roots {
        if root.compatibility != NupmCompatibility::ImportableModule {
            continue;
        }
        let Some(meta) = &root.metadata else {
            continue;
        };
        for (installed_id, entry) in &lockfile.packages {
            if entry.package_type != "module" {
                continue;
            }
            let Some((_, name)) = installed_id.split_once('/') else {
                continue;
            };
            if name != meta.name {
                continue;
            }
            let same_import = imports
                .imports
                .get(installed_id.as_str())
                .is_some_and(|r| Path::new(&r.nupm_source_path) == root.source_path.as_path());
            if !same_import {
                count += 1;
                break;
            }
        }
    }
    Ok(count)
}

fn run_inspect(args: &InspectArgs, out: &mut dyn Write) -> Result<()> {
    if args.all {
        let home = match resolve_nupm_home(args.nupm_home.as_deref())? {
            NupmHomeResolution::Found(p) => p,
            NupmHomeResolution::NotConfigured => {
                bail!(
                    "No nupm home was supplied.\n\n\
                     Pass --nupm-home <path> or set NUPM_HOME for inspect --all.\n\
                     Numan will not guess nupm's installation location."
                );
            }
        };
        let scan = scan_nupm_home(&home)?;
        let candidates = scan
            .source_roots
            .into_iter()
            .map(|entry| NupmCandidateReport { entry })
            .collect();
        let report = NupmInspectionReport {
            candidates,
            installed_only: scan.installed_only,
        };
        format_inspection_report(&report, out)?;
        ensure_eligible_if_requested(args.exit_on_ineligible, &report)?;
        Ok(())
    } else if let Some(path) = &args.path {
        if args.nupm_home.is_some() {
            bail!("--nupm-home cannot be used with inspect <PATH>; use inspect --all --nupm-home instead");
        }
        let entry = inspect_path(path)?;
        let report = NupmInspectionReport {
            candidates: vec![NupmCandidateReport { entry }],
            installed_only: vec![],
        };
        format_inspection_report(&report, out)?;
        ensure_eligible_if_requested(args.exit_on_ineligible, &report)?;
        Ok(())
    } else {
        bail!("inspect requires either <PATH> or --all")
    }
}

fn ensure_eligible_if_requested(
    exit_on_ineligible: bool,
    report: &NupmInspectionReport,
) -> Result<()> {
    if !exit_on_ineligible {
        return Ok(());
    }
    let ineligible: Vec<String> = report
        .candidates
        .iter()
        .filter(|c| c.entry.compatibility != NupmCompatibility::ImportableModule)
        .map(|c| {
            c.entry
                .metadata
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_else(|| c.entry.source_path.display().to_string())
        })
        .collect();
    if ineligible.is_empty() {
        return Ok(());
    }
    bail!(
        "Found {} ineligible package(s): {}. Omit --exit-on-ineligible for informational output only.",
        ineligible.len(),
        ineligible.join(", ")
    );
}

fn count_source(roots: &[crate::nupm_compat::SourceRootEntry]) -> (usize, usize) {
    let mut eligible = 0usize;
    let mut rejected = 0usize;
    for r in roots {
        if r.compatibility == NupmCompatibility::ImportableModule {
            eligible += 1;
        } else {
            rejected += 1;
        }
    }
    (eligible, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn status_not_configured_exits_ok_via_report() {
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Status(StatusArgs { nupm_home: None }),
        };
        // Temporarily clear NUPM_HOME for test
        let prev = std::env::var_os("NUPM_HOME");
        std::env::remove_var("NUPM_HOME");
        let root = tempfile::tempdir().unwrap();
        execute(&args, root.path(), &mut buf).unwrap();
        if let Some(p) = prev {
            std::env::set_var("NUPM_HOME", p);
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("not configured"));
        assert!(s.contains("Source drift"));
    }

    #[test]
    fn inspect_exit_on_ineligible_rejects_script_type() {
        let root = tempfile::tempdir().unwrap();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nupm/rejected/script-type");
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Inspect(InspectArgs {
                all: false,
                path: Some(path),
                nupm_home: None,
                exit_on_ineligible: true,
            }),
        };
        assert!(execute(&args, root.path(), &mut buf).is_err());
    }

    #[test]
    fn inspect_default_allows_ineligible() {
        let root = tempfile::tempdir().unwrap();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nupm/rejected/script-type");
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Inspect(InspectArgs {
                all: false,
                path: Some(path),
                nupm_home: None,
                exit_on_ineligible: false,
            }),
        };
        execute(&args, root.path(), &mut buf).unwrap();
    }

    #[test]
    fn inspect_exit_on_ineligible_rejects_invalid_metadata_without_name() {
        let root = tempfile::tempdir().unwrap();
        let pkg = root.path().join("pkg");
        std::fs::create_dir_all(pkg.join("m")).unwrap();
        std::fs::write(pkg.join("nupm.nuon"), b"not valid nuon {{{").unwrap();
        std::fs::write(pkg.join("m/mod.nu"), b"").unwrap();

        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Inspect(InspectArgs {
                all: false,
                path: Some(pkg),
                nupm_home: None,
                exit_on_ineligible: true,
            }),
        };
        assert!(execute(&args, root.path(), &mut buf).is_err());
    }

    #[test]
    fn diff_rejects_invalid_scoped_id() {
        let root = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Diff(DiffArgs {
                package_id: "not-a-scoped-id".to_string(),
            }),
        };
        assert!(execute(&args, root.path(), &mut buf).is_err());
    }

    #[test]
    fn diff_reports_cannot_compare_when_no_import_exists() {
        let root = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Diff(DiffArgs {
                package_id: "owner/pkg".to_string(),
            }),
        };
        let err = execute(&args, root.path(), &mut buf).unwrap_err();
        assert!(err.to_string().contains("Cannot compare drift"));
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.is_empty(), "drift report should still be printed");
    }

    #[test]
    fn inspect_all_without_nupm_home_bails() {
        let root = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Inspect(InspectArgs {
                all: true,
                path: None,
                nupm_home: Some(PathBuf::from("/nonexistent/nupm-home")),
                exit_on_ineligible: false,
            }),
        };
        // Points at a nonexistent path so resolve_nupm_home falls through to
        // NotConfigured or scan_nupm_home errors; either way this is not a
        // successful scan and must not panic.
        let _ = execute(&args, root.path(), &mut buf);
    }

    #[test]
    fn inspect_rejects_nupm_home_with_path() {
        let root = tempfile::tempdir().unwrap();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nupm/rejected/script-type");
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Inspect(InspectArgs {
                all: false,
                path: Some(path),
                nupm_home: Some(PathBuf::from("/tmp/whatever")),
                exit_on_ineligible: false,
            }),
        };
        let err = execute(&args, root.path(), &mut buf).unwrap_err();
        assert!(err
            .to_string()
            .contains("--nupm-home cannot be used with inspect <PATH>"));
    }

    #[test]
    fn inspect_requires_path_or_all() {
        let root = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Inspect(InspectArgs {
                all: false,
                path: None,
                nupm_home: None,
                exit_on_ineligible: false,
            }),
        };
        let err = execute(&args, root.path(), &mut buf).unwrap_err();
        assert!(err.to_string().contains("requires either <PATH> or --all"));
    }

    #[test]
    fn import_rejects_path_with_manifest() {
        let root = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Import(ImportArgs {
                path: Some(PathBuf::from("/tmp/whatever")),
                manifest: Some(PathBuf::from("/tmp/manifest.toml")),
                nupm_home: None,
                r#as: None,
                yes: true,
            }),
        };
        let err = execute(&args, root.path(), &mut buf).unwrap_err();
        assert!(err.to_string().contains("Cannot use PATH with --manifest"));
    }

    #[test]
    fn import_requires_path_or_manifest() {
        let root = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Import(ImportArgs {
                path: None,
                manifest: None,
                nupm_home: None,
                r#as: None,
                yes: true,
            }),
        };
        let err = execute(&args, root.path(), &mut buf).unwrap_err();
        assert!(err
            .to_string()
            .contains("import requires PATH or --manifest"));
    }

    #[test]
    fn import_single_requires_as() {
        let root = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Import(ImportArgs {
                path: Some(PathBuf::from("/tmp/whatever")),
                manifest: None,
                nupm_home: None,
                r#as: None,
                yes: true,
            }),
        };
        let err = execute(&args, root.path(), &mut buf).unwrap_err();
        assert!(err.to_string().contains("single import requires --as"));
    }

    #[test]
    fn import_fails_without_configured_nu_paths() {
        let root = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Import(ImportArgs {
                path: Some(PathBuf::from("/tmp/whatever")),
                manifest: None,
                nupm_home: None,
                r#as: Some("owner/pkg".to_string()),
                yes: true,
            }),
        };
        let err = execute(&args, root.path(), &mut buf).unwrap_err();
        assert!(err.to_string().contains("Nu paths are not configured"));
    }

    fn nupm_home_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nupm/nupm-home-layout")
    }

    #[test]
    fn status_found_reports_scan_results() {
        let root = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Status(StatusArgs {
                nupm_home: Some(nupm_home_fixture()),
            }),
        };
        execute(&args, root.path(), &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.contains("not configured"));
    }

    #[test]
    fn inspect_all_reports_candidates() {
        let root = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let args = NupmArgs {
            command: NupmCommands::Inspect(InspectArgs {
                all: true,
                path: None,
                nupm_home: Some(nupm_home_fixture()),
                exit_on_ineligible: false,
            }),
        };
        execute(&args, root.path(), &mut buf).unwrap();
        assert!(!String::from_utf8(buf).unwrap().is_empty());
    }
}
