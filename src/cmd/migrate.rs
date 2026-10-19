use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::core::package::ScopedId;
use crate::nu::autoload::NuCandidateRunner;
use crate::nu::paths::NuPaths;
use crate::nupm_compat::{
    classify::NupmCompatibility, discovery::resolve_nupm_home, discovery::scan_nupm_home,
    NupmHomeResolution, SourceRootEntry,
};
use crate::state::lockfile::Lockfile;
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::fs_safety::acquire_mutation_lock;

#[derive(clap::Parser)]
pub struct MigrateArgs {
    #[arg(long)]
    pub nupm_home: Option<std::path::PathBuf>,

    /// Skip confirmation prompts
    #[arg(long)]
    pub yes: bool,

    /// Print what would be imported/activated without mutating state
    #[arg(long)]
    pub dry_run: bool,
}

pub fn execute(args: &MigrateArgs, numan_root: &Path, out: &mut dyn Write) -> Result<()> {
    let home = match resolve_nupm_home(args.nupm_home.as_deref())? {
        NupmHomeResolution::Found(p) => p,
        NupmHomeResolution::NotConfigured => {
            bail!(
                "nupm home is not configured.\n\n\
                 Pass --nupm-home <path> or set NUPM_HOME.\n\
                 Numan will not guess nupm's installation location."
            );
        }
    };

    let scan = scan_nupm_home(&home)?;
    let _lockfile = Lockfile::load(numan_root)?;

    // Partition source roots.
    let mut importable: Vec<(SourceRootEntry, String)> = Vec::new();
    let mut skipped: Vec<(SourceRootEntry, &'static str)> = Vec::new();

    for entry in &scan.source_roots {
        match entry.compatibility {
            NupmCompatibility::ImportableModule => {
                let name = entry
                    .metadata
                    .as_ref()
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                let scoped = format!("nupm/{}", name);
                // Validate scoped id parses.
                ScopedId::parse(&scoped)?;
                importable.push((entry.clone(), scoped));
            }
            other => skipped.push((entry.clone(), compat_reason(other))),
        }
    }

    writeln!(out, "nupm home: {}", home.display())?;
    writeln!(
        out,
        "Discovered {} source root(s).",
        scan.source_roots.len()
    )?;
    writeln!(out)?;

    if !skipped.is_empty() {
        writeln!(out, "Non-importable packages ({}):", skipped.len())?;
        for (entry, reason) in &skipped {
            let name = entry
                .metadata
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or("?");
            writeln!(out, "  {} — {}", name, reason)?;
            writeln!(
                out,
                "      compatibility: {:?}  source: {}",
                entry.compatibility,
                entry.source_path.display()
            )?;
        }
        writeln!(out)?;
    }

    if importable.is_empty() {
        writeln!(out, "Nothing to import.")?;
        return Ok(());
    }

    writeln!(out, "Importable packages ({}):", importable.len())?;
    for (_, scoped) in &importable {
        writeln!(out, "  {}", scoped)?;
    }
    writeln!(out)?;

    if args.dry_run {
        writeln!(out, "[dry-run] No state was mutated.")?;
        return Ok(());
    }

    // Nu paths + runner.
    let nu_paths = NuPaths::load(numan_root)
        .with_context(|| "Nu paths are not configured. Run `numan init` first.".to_string())?;
    let runner = NuCandidateRunner::new(&nu_paths.nu_executable);

    // Confirm the whole migration once up front. The per-import consent gate in
    // `nupm_compat` only prints the consent block and aborts without `--yes`,
    // so asking here — then passing consent granted to each import — is what
    // makes `--yes` behave as "skip the prompt".
    crate::util::confirm::confirm_or_bail(
        &format!(
            "Import {} package(s) from nupm and activate them?",
            importable.len()
        ),
        args.yes,
        "Migration cancelled.",
    )?;

    // Snapshot before mutation.
    // Acquire mutation lock before snapshot + mutation.
    let _lock = acquire_mutation_lock(numan_root)?;
    let _snap = create_snapshot(
        numan_root,
        SnapshotReason::PreMutation,
        SnapshotTrigger::Migrate,
        None,
        None,
    )
    .context("Failed to create pre-migration snapshot")?;

    // Import each, reusing the outer mutation lock so the nested non-blocking
    // acquire does not self-deadlock.
    let mut imported_ids: Vec<String> = Vec::new();
    let mut import_failures = 0usize;
    for (entry, scoped) in &importable {
        let target = ScopedId::parse(scoped)
            .expect("scoped ids are validated before the mutation lock is acquired");
        match crate::nupm_compat::import_module_with_runner_under_lock(
            numan_root,
            &entry.source_path,
            &target,
            true,
            &runner,
            &_lock,
        ) {
            Ok(res) => {
                writeln!(
                    out,
                    "Imported {}@{} -> {}",
                    res.package_id, res.version, res.payload_path
                )?;
                imported_ids.push(res.package_id);
            }
            Err(e) => {
                writeln!(out, "Failed to import {}: {:#}", scoped, e)?;
                import_failures += 1;
            }
        }
    }

    // Activate imported, reusing the same outer mutation lock.
    let mut activation_failed = false;
    if !imported_ids.is_empty() {
        let activate_args = crate::cmd::activate::ActivateArgs {
            packages: imported_ids.clone(),
            verbose: false,
            list: false,
            check: false,
        };
        if let Err(e) = crate::cmd::activate::execute_under_lock(&activate_args, numan_root, &_lock)
        {
            writeln!(out, "Activation failed: {:#}", e)?;
            activation_failed = true;
        } else {
            writeln!(out, "Activated {} package(s).", imported_ids.len())?;
        }
    }

    if import_failures == 0 && !activation_failed {
        writeln!(out, "\nMigration complete.")?;
        Ok(())
    } else {
        writeln!(
            out,
            "\nMigration finished with errors: {} import failure(s), activation {}.",
            import_failures,
            if activation_failed { "failed" } else { "ok" },
        )?;
        bail!("migration completed with errors");
    }
}

fn compat_reason(c: NupmCompatibility) -> &'static str {
    match c {
        NupmCompatibility::ImportableModule => "importable",
        NupmCompatibility::DeferredScript => {
            "manual migration required (script activation deferred)"
        }
        NupmCompatibility::UnsupportedCustomBuild => "remove build.nu / custom build step",
        NupmCompatibility::UnsupportedDependencies => "remove external dependencies",
        NupmCompatibility::InvalidMetadata => "fix nupm.nuon metadata",
        NupmCompatibility::UnsafeFilesystemLayout => "fix filesystem layout / symlinks",
        NupmCompatibility::UnknownType => "unsupported package type",
    }
}
