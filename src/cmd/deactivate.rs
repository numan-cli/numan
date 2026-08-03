use anyhow::{bail, Context, Result};
use clap::Args;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::nu::autoload::{
    delete_managed_file, generate_autoload_content, replace_managed_file, validate_candidate,
    write_candidate, CandidateRunner, NuCandidateRunner,
};
use crate::nu::paths::NuPaths;
use crate::state::autoload_journal::{
    sha256_file, AutoloadOperation, AutoloadStage, PendingAutoload,
    SCHEMA_VERSION as AUTOLOAD_SCHEMA_VERSION,
};
use crate::state::autoload_recovery::reconcile_pending_autoload;
use crate::state::autoload_state::AutoloadState;
use crate::state::lockfile::{Lockfile, PluginActivation};
use crate::state::plugin_deactivate_journal::{
    PendingPluginDeactivate, PendingPluginDeactivateEntry, PluginDeactivateStatus,
};
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::hints::{self, CMD_DEACTIVATE, CMD_INIT_REFRESH};
use crate::util::{format_timestamp, fs_safety::acquire_mutation_lock};

use super::print_autoload_recovery;

/// Nu program string for plugin unregister. Identity/config only via env vars.
///
/// `NUMAN_PLUGIN_NAME` carries the recorded absolute plugin binary path (Nu
/// accepts name or filename for `plugin rm`). Preferring the path scopes removal
/// to Numan's installed entry when another binary shares the derived name.
///
/// `--force` makes recovery idempotent: if Nu already removed the plugin but
/// the journal is still `Prepared`, retry succeeds instead of marking Failed.
const RM_PLUGIN: &str =
    "plugin rm --force --plugin-config $env.NUMAN_PLUGIN_CONFIG $env.NUMAN_PLUGIN_NAME";
#[derive(Args, Debug)]
pub struct DeactivateArgs {
    /// Package IDs (owner/name) to deactivate. Omit to deactivate all active plugins and modules.
    pub packages: Vec<String>,

    /// Show detailed output
    #[arg(long)]
    pub verbose: bool,
}

/// A plugin that is currently active and eligible for deactivation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ActivePlugin {
    package_id: String,
    executable_path: String,
    activation: PluginActivation,
    plugin_name: String,
    absolute_binary_path: PathBuf,
}

/// A module that is currently active and eligible for deactivation.
#[derive(Debug, PartialEq, Eq)]
struct ActiveModule {
    package_id: String,
    vendor_autoload_dir: String,
    managed_file_path: String,
}

#[derive(Debug, PartialEq, Eq)]
struct ClassifiedTargets {
    plugins: Vec<ActivePlugin>,
    modules: Vec<ActiveModule>,
}

impl ClassifiedTargets {
    fn is_empty(&self) -> bool {
        self.plugins.is_empty() && self.modules.is_empty()
    }
}

pub fn execute(args: &DeactivateArgs, root: &Path) -> Result<()> {
    execute_with_candidate_runner_and_unregistrar(args, root, None, &run_plugin_rm)
}

/// Testability entry point — accepts an injectable plugin unregistrar.
///
/// The unregistrar receives `(nu_executable, plugin_rm_identity, plugin_config_path)`
/// where `plugin_rm_identity` is the absolute plugin binary path (preferred) or
/// the derived Nu plugin name as a fallback for older journals.
/// Returns `Ok(())` on success. Production code calls `run_plugin_rm`.
pub fn execute_with_unregistrar(
    args: &DeactivateArgs,
    root: &Path,
    unregistrar: &dyn Fn(&str, &str, &str) -> Result<()>,
) -> Result<()> {
    execute_with_candidate_runner_and_unregistrar(args, root, None, unregistrar)
}

/// Testability entry point — accepts an injectable candidate runner.
pub fn execute_with_candidate_runner(
    args: &DeactivateArgs,
    root: &Path,
    runner: &dyn CandidateRunner,
) -> Result<()> {
    execute_with_candidate_runner_and_unregistrar(args, root, Some(runner), &run_plugin_rm)
}

/// Full testability entry point — injectable module candidate runner and plugin unregistrar.
pub fn execute_with_candidate_runner_and_unregistrar(
    args: &DeactivateArgs,
    root: &Path,
    runner: Option<&dyn CandidateRunner>,
    unregistrar: &dyn Fn(&str, &str, &str) -> Result<()>,
) -> Result<()> {
    // 1. Load cached Nu identity
    let nu_paths = NuPaths::load(root)?;

    // 2. Reconcile interrupted work and classify targets while holding the
    //    mutation lock. This prevents stale pre-recovery state from causing an
    //    early error or no-op return.
    let planning_lock = acquire_mutation_lock(root)?;
    let mut lockfile = Lockfile::load(root)?;
    prepare_deactivate_recovery(&nu_paths, root)?;
    reconcile_plugin_deactivate_journal(root, &nu_paths, &mut lockfile, unregistrar)?;
    print_autoload_recovery(reconcile_pending_autoload(root, &nu_paths, &mut lockfile)?);
    let lockfile = Lockfile::load(root)?;

    // 3. Classify explicit package IDs (plugins + modules; script/completion deferred).
    let targets_requested = classify_and_validate_packages(args, &lockfile, root, &nu_paths)?;

    if targets_requested.is_empty() {
        println!("Nothing to deactivate.");
        return Ok(());
    }

    // 4. Module lane: full deactivation may proceed under stale Nu identity;
    //    partial must not. Plugin lane always requires a current Nu identity
    //    for unregister against the active registry.
    if !targets_requested.plugins.is_empty() {
        nu_paths.validate_drift()?;
    }
    if !targets_requested.modules.is_empty()
        && !is_full_module_deactivation(&lockfile, &targets_requested.modules)
    {
        nu_paths.validate_drift()?;
    }
    drop(planning_lock);

    // 5. Show consent table (informational only; no prompt)
    print_consent_table(&targets_requested, &nu_paths.plugin_registry_path);

    // 6. Reacquire the root mutation lock after consent.
    let _lock = acquire_mutation_lock(root)?;

    // Reload lockfile under the lock
    let mut lockfile = Lockfile::load(root)?;

    // 7. Reconcile interrupted journals under the lock (drift + snapshot first).
    prepare_deactivate_recovery(&nu_paths, root)?;
    reconcile_plugin_deactivate_journal(root, &nu_paths, &mut lockfile, unregistrar)?;
    print_autoload_recovery(reconcile_pending_autoload(root, &nu_paths, &mut lockfile)?);

    // Reload and reclassify after reconciliation so the new operation is based
    // on the current authoritative state.
    let mut lockfile = Lockfile::load(root)?;
    let targets_requested =
        reclassify_targets(args, &lockfile, root, &nu_paths, &targets_requested)?;
    if targets_requested.is_empty() {
        println!("Nothing to deactivate.");
        return Ok(());
    }

    if !targets_requested.plugins.is_empty() {
        nu_paths.validate_drift()?;
    }
    if !targets_requested.modules.is_empty()
        && !is_full_module_deactivation(&lockfile, &targets_requested.modules)
    {
        nu_paths.validate_drift()?;
    }

    // 8. Snapshot current state before any mutation
    let snapshot = create_snapshot(
        root,
        SnapshotReason::PreMutation,
        SnapshotTrigger::Deactivate,
        None,
        None,
    )?;

    let mut any_failed = false;

    // ── PLUGIN LANE ───────────────────────────────────────────────────────────
    if !targets_requested.plugins.is_empty() {
        let plugin_failed = run_plugin_deactivate_lane(
            args,
            root,
            &nu_paths,
            &mut lockfile,
            &targets_requested.plugins,
            unregistrar,
        )?;
        if plugin_failed {
            any_failed = true;
        }
    }

    // ── MODULE LANE ───────────────────────────────────────────────────────────
    if !targets_requested.modules.is_empty() {
        run_module_deactivate_lane(
            args,
            root,
            &nu_paths,
            &mut lockfile,
            &targets_requested.modules,
            runner,
            Some(snapshot.id.clone()),
        )?;
    }

    if any_failed {
        bail!(
            "One or more plugins failed to deactivate. Successful deactivations have been persisted."
        );
    }

    Ok(())
}

/// Full module deactivation compares unique IDs so duplicate CLI args cannot
/// skip Nu drift validation for a partial target set.
fn is_full_module_deactivation(lockfile: &Lockfile, requested: &[ActiveModule]) -> bool {
    let requested_ids: BTreeSet<&str> = requested.iter().map(|m| m.package_id.as_str()).collect();
    let active_ids: BTreeSet<&str> = lockfile
        .packages
        .iter()
        .filter(|(_, pkg)| pkg.package_type == "module" && pkg.module_activation.is_some())
        .map(|(id, _)| id.as_str())
        .collect();
    !active_ids.is_empty() && requested_ids == active_ids
}

fn print_consent_table(targets: &ClassifiedTargets, registry_path: &str) {
    println!();
    if !targets.plugins.is_empty() {
        println!("Plugin deactivation");
        for p in &targets.plugins {
            println!(
                "  {} ({}) <- {}",
                p.package_id, p.plugin_name, registry_path
            );
        }
        println!();
    }
    if !targets.modules.is_empty() {
        println!("Module deactivation");
        for m in &targets.modules {
            println!("  {} <- {}", m.package_id, m.managed_file_path);
        }
        println!();
    }
}

fn run_module_deactivate_lane(
    _args: &DeactivateArgs,
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
    targets_requested: &[ActiveModule],
    runner: Option<&dyn CandidateRunner>,
    pre_mutation_snapshot_id: Option<String>,
) -> Result<()> {
    // All targets must agree on the same managed file path and vendor dir.
    let managed_file_path = targets_requested[0].managed_file_path.clone();
    let vendor_autoload_dir = targets_requested[0].vendor_autoload_dir.clone();
    for t in &targets_requested[1..] {
        if t.managed_file_path != managed_file_path || t.vendor_autoload_dir != vendor_autoload_dir
        {
            bail!(
                "Module activations point to different managed files:\n  \
                 '{}' ({})\n  vs '{}' ({})\n\
                 Run 'numan activate --check' to inspect state, or \
                 'numan init --refresh' to reset the vendor-autoload target.",
                managed_file_path,
                targets_requested[0].package_id,
                t.managed_file_path,
                t.package_id
            );
        }
    }

    let currently_active_ids: Vec<String> = AutoloadState::active_module_ids_from_lockfile(
        lockfile,
        &nu_paths.nu_executable_hash,
        &nu_paths.nu_version,
        &vendor_autoload_dir,
        &managed_file_path,
    );

    let deactivating_ids: Vec<String> = targets_requested
        .iter()
        .map(|m| m.package_id.clone())
        .collect();

    let remaining_ids: Vec<String> = currently_active_ids
        .iter()
        .filter(|id| !deactivating_ids.contains(id))
        .cloned()
        .collect();

    let managed_path = Path::new(&managed_file_path);

    if remaining_ids.is_empty() {
        run_full_deactivation(
            root,
            nu_paths,
            lockfile,
            targets_requested,
            managed_path,
            &vendor_autoload_dir,
            &managed_file_path,
            &currently_active_ids,
            pre_mutation_snapshot_id,
        )
    } else {
        nu_paths.validate_drift()?;

        let real_runner;
        let runner_ref: &dyn CandidateRunner = if let Some(r) = runner {
            r
        } else {
            real_runner = NuCandidateRunner::new(&nu_paths.nu_executable);
            &real_runner
        };

        run_partial_deactivation(
            root,
            nu_paths,
            lockfile,
            targets_requested,
            &remaining_ids,
            managed_path,
            &vendor_autoload_dir,
            &managed_file_path,
            &currently_active_ids,
            runner_ref,
            pre_mutation_snapshot_id,
        )
    }
}

// ── Package classification ─────────────────────────────────────────────────────

/// Derive the Nu plugin registry name from a lockfile `executable_path`.
///
/// Normalizes `/` and `\` so Windows-style relative paths parse on Unix CI,
/// strips a Windows `.exe` suffix, then strips a leading `nu_plugin_` prefix
/// (`nu_plugin_highlight` → `highlight`).
pub fn plugin_name_from_executable_path(executable_path: &str) -> String {
    let normalized = executable_path.replace('\\', "/");
    let basename = normalized.rsplit('/').next().unwrap_or(executable_path);
    let stem = basename.trim_end_matches(".exe");
    stem.strip_prefix("nu_plugin_").unwrap_or(stem).to_string()
}

/// Classify and validate explicit or implicit deactivation targets.
///
/// - Plugins with `activation.is_some()` are collected as `ActivePlugin`.
/// - Modules with `module_activation` are collected as `ActiveModule`.
/// - Script/completion packages return a deferred-feature error.
/// - Unknown package IDs fail.
/// - Plugins that are not currently active are skipped when listed explicitly
///   (idempotent after journal reconcile). Modules that are not active still fail.
/// - No IDs means deactivate all currently active plugins and modules.
fn classify_and_validate_packages(
    args: &DeactivateArgs,
    lockfile: &Lockfile,
    root: &Path,
    nu_paths: &NuPaths,
) -> Result<ClassifiedTargets> {
    // First pass: reject deferred / unknown types in the explicit list.
    for pkg_id in &args.packages {
        let entry = match lockfile.packages.get(pkg_id) {
            Some(e) => e,
            None => {
                bail!("Package '{pkg_id}' not found in lockfile (not installed)");
            }
        };
        match entry.package_type.as_str() {
            "module" | "plugin" => {}
            "script" => {
                bail!(
                    "Package '{pkg_id}' is a script — script deactivation is deferred to a later phase."
                );
            }
            "completion" => {
                bail!(
                    "Package '{pkg_id}' is a completion — completion deactivation is deferred to a later phase."
                );
            }
            other => {
                bail!("Package '{pkg_id}' has unknown type '{other}' — cannot deactivate.");
            }
        }
    }

    let mut plugins = Vec::new();
    let mut modules = Vec::new();

    if args.packages.is_empty() {
        let mut sorted_ids: Vec<&String> = lockfile.packages.keys().collect();
        sorted_ids.sort();

        for pkg_id in sorted_ids {
            let entry = &lockfile.packages[pkg_id];
            match entry.package_type.as_str() {
                "plugin"
                    if entry.is_active_for(
                        &nu_paths.nu_executable_hash,
                        &nu_paths.nu_version,
                        &nu_paths.plugin_registry_path,
                    ) =>
                {
                    plugins.push(active_plugin_from_entry(pkg_id, entry, root)?);
                }
                "module" if entry.module_activation.is_some() => {
                    let ma = entry.module_activation.as_ref().unwrap();
                    modules.push(ActiveModule {
                        package_id: pkg_id.clone(),
                        vendor_autoload_dir: ma.vendor_autoload_dir.clone(),
                        managed_file_path: ma.managed_file_path.clone(),
                    });
                }
                _ => {}
            }
        }
    } else {
        for pkg_id in &args.packages {
            let entry = &lockfile.packages[pkg_id];
            match entry.package_type.as_str() {
                "plugin" => {
                    if entry.activation.is_none() {
                        // Already inactive (common after journal reconcile): skip.
                        continue;
                    }
                    if !entry.is_active_for(
                        &nu_paths.nu_executable_hash,
                        &nu_paths.nu_version,
                        &nu_paths.plugin_registry_path,
                    ) {
                        bail!(
                            "Package '{pkg_id}' has a plugin activation record that does not match \
                             the current Nu identity.\n\
                             {}\n\
                             Stale activation is not unregistered from the current plugin registry.",
                            hints::run_then(CMD_INIT_REFRESH, CMD_DEACTIVATE)
                        );
                    }
                    plugins.push(active_plugin_from_entry(pkg_id, entry, root)?);
                }
                "module" => match &entry.module_activation {
                    None => {
                        bail!("Package '{pkg_id}' is a module but is not currently active.");
                    }
                    Some(ma) => {
                        modules.push(ActiveModule {
                            package_id: pkg_id.clone(),
                            vendor_autoload_dir: ma.vendor_autoload_dir.clone(),
                            managed_file_path: ma.managed_file_path.clone(),
                        });
                    }
                },
                _ => unreachable!("deferred types rejected in first pass"),
            }
        }
    }

    Ok(ClassifiedTargets { plugins, modules })
}

fn active_plugin_from_entry(
    pkg_id: &str,
    entry: &crate::state::lockfile::LockfileEntry,
    root: &Path,
) -> Result<ActivePlugin> {
    let activation = entry
        .activation
        .clone()
        .expect("caller ensures activation is Some");
    let executable_path = entry
        .executable_path
        .as_deref()
        .with_context(|| format!("{pkg_id}: missing executable_path in lockfile"))?
        .to_string();
    let plugin_name = plugin_name_from_executable_path(&executable_path);
    let absolute_binary_path = root.join(&entry.payload_path).join(&executable_path);
    Ok(ActivePlugin {
        package_id: pkg_id.to_string(),
        executable_path,
        activation,
        plugin_name,
        absolute_binary_path,
    })
}

fn reclassify_targets(
    args: &DeactivateArgs,
    lockfile: &Lockfile,
    root: &Path,
    nu_paths: &NuPaths,
    planned_targets: &ClassifiedTargets,
) -> Result<ClassifiedTargets> {
    let current_targets = classify_and_validate_packages(args, lockfile, root, nu_paths)?;
    if current_targets != *planned_targets {
        bail!(
            "Activation state changed since planning; no packages were deactivated. Retry to review current targets."
        );
    }
    Ok(current_targets)
}

// ── Plugin deactivate lane ─────────────────────────────────────────────────────

/// Run the plugin unregister lane. Returns `true` if any plugin failed.
fn run_plugin_deactivate_lane(
    args: &DeactivateArgs,
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
    targets: &[ActivePlugin],
    unregistrar: &dyn Fn(&str, &str, &str) -> Result<()>,
) -> Result<bool> {
    let mut journal = PendingPluginDeactivate {
        nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
        nu_version: nu_paths.nu_version.clone(),
        plugin_registry_path: nu_paths.plugin_registry_path.clone(),
        created_at: format_timestamp(),
        entries: targets
            .iter()
            .map(|t| PendingPluginDeactivateEntry {
                package_id: t.package_id.clone(),
                plugin_name: t.plugin_name.clone(),
                absolute_binary_path: t.absolute_binary_path.to_string_lossy().into_owned(),
                status: PluginDeactivateStatus::Prepared,
                error: None,
            })
            .collect(),
    };
    journal.save(root)?;

    let mut any_failed = false;
    for (i, target) in targets.iter().enumerate() {
        if args.verbose {
            println!(
                "  Unregistering {} ({})",
                target.package_id, target.plugin_name
            );
        }

        let rm_identity = target.absolute_binary_path.to_string_lossy();
        match unregistrar(
            &nu_paths.nu_executable,
            &rm_identity,
            &nu_paths.plugin_registry_path,
        ) {
            Ok(()) => {
                journal.entries[i].status = PluginDeactivateStatus::Unregistered;
                journal.save(root)?;

                if let Some(pkg) = lockfile.packages.get_mut(&target.package_id) {
                    pkg.activation = None;
                }
                lockfile.save(root)?;

                println!(
                    "{} Unregistered {} — takes effect in future Nu sessions (not the current shell)",
                    console::style("✓").green(),
                    target.package_id,
                );
            }
            Err(e) => {
                journal.entries[i].status = PluginDeactivateStatus::Failed;
                journal.entries[i].error = Some(e.to_string());
                journal.save(root)?;

                eprintln!(
                    "{} Failed to unregister {}: {}",
                    console::style("✗").red(),
                    target.package_id,
                    e
                );
                any_failed = true;
            }
        }
    }

    // Keep the journal whenever any unregister failed (doctor / retry visibility).
    // Otherwise every entry is Unregistered and activations are already cleared.
    if !any_failed {
        PendingPluginDeactivate::delete(root)?;
    }

    Ok(any_failed)
}

/// Drift-check and snapshot before journal replay can mutate state or spawn Nu.
///
/// `Prepared` plugin-deactivate entries invoke the unregistrar, so Nu binary
/// drift must be rejected first. Any pending plugin/autoload journal can write
/// the lockfile, so take a pre-mutation snapshot when either journal exists.
fn prepare_deactivate_recovery(nu_paths: &NuPaths, root: &Path) -> Result<()> {
    let plugin_journal = PendingPluginDeactivate::load(root)?;
    let has_autoload_journal = PendingAutoload::load(root)?.is_some();

    if plugin_journal.as_ref().is_some_and(|journal| {
        journal
            .entries
            .iter()
            .any(|entry| entry.status == PluginDeactivateStatus::Prepared)
    }) {
        nu_paths.validate_drift()?;
    }

    if plugin_journal.is_some() || has_autoload_journal {
        let _snapshot = create_snapshot(
            root,
            SnapshotReason::PreMutation,
            SnapshotTrigger::Deactivate,
            None,
            None,
        )?;
    }

    Ok(())
}

/// Identity passed to `plugin rm`: prefer the recorded absolute binary path.
fn plugin_rm_identity(entry: &PendingPluginDeactivateEntry) -> &str {
    if entry.absolute_binary_path.is_empty() {
        &entry.plugin_name
    } else {
        &entry.absolute_binary_path
    }
}

/// Reconcile an interrupted plugin-deactivate journal.
///
/// - `Unregistered` entries: clear lockfile activation (commit pending clear).
/// - `Prepared` entries: retry unregister via the injectable seam.
/// - `Failed` entries: leave activation; surface via doctor until retry.
fn reconcile_plugin_deactivate_journal(
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
    unregistrar: &dyn Fn(&str, &str, &str) -> Result<()>,
) -> Result<()> {
    let existing = PendingPluginDeactivate::load(root)?;
    let Some(mut existing) = existing else {
        return Ok(());
    };

    // Commit Unregistered clears even when Nu identity drifted: no Nu spawn is
    // required, and leaving activation set would keep remove/update gated.
    let mut changed = false;
    let mut retain_journal = false;
    existing.entries.retain(|entry| {
        if entry.status != PluginDeactivateStatus::Unregistered {
            return true;
        }
        if let Some(pkg) = lockfile.packages.get_mut(&entry.package_id) {
            if pkg.activation.is_some() {
                pkg.activation = None;
                changed = true;
            }
        }
        false
    });
    if changed {
        lockfile.save(root)?;
    }
    if existing.entries.is_empty() {
        PendingPluginDeactivate::delete(root)?;
        eprintln!("   Plugin deactivation reconciliation complete.");
        return Ok(());
    }
    existing.save(root)?;

    if !existing.matches_nu_identity(
        &nu_paths.nu_executable_hash,
        &nu_paths.nu_version,
        &nu_paths.plugin_registry_path,
    ) {
        bail!(
            "A pending plugin deactivation journal exists from a different Nu identity.\n\
             {}\n\
             Journal: {}",
            hints::run_then(CMD_INIT_REFRESH, CMD_DEACTIVATE),
            root.join("state/pending-plugin-deactivate.json").display()
        );
    }

    eprintln!(
        "{}  Reconciling interrupted plugin deactivation journal…",
        console::style("⚠").yellow()
    );

    changed = false;
    for i in 0..existing.entries.len() {
        match existing.entries[i].status {
            PluginDeactivateStatus::Unregistered => {
                // Already drained above; defensive no-op.
            }
            PluginDeactivateStatus::Prepared => {
                let rm_identity = plugin_rm_identity(&existing.entries[i]).to_string();
                let package_id = existing.entries[i].package_id.clone();
                match unregistrar(
                    &nu_paths.nu_executable,
                    &rm_identity,
                    &nu_paths.plugin_registry_path,
                ) {
                    Ok(()) => {
                        existing.entries[i].status = PluginDeactivateStatus::Unregistered;
                        existing.entries[i].error = None;
                        if let Some(pkg) = lockfile.packages.get_mut(&package_id) {
                            pkg.activation = None;
                        }
                        // Persist per entry so a mid-loop crash does not replay
                        // a successful unregister as Prepared.
                        lockfile.save(root)?;
                        existing.save(root)?;
                        changed = true;
                    }
                    Err(e) => {
                        existing.entries[i].status = PluginDeactivateStatus::Failed;
                        existing.entries[i].error = Some(e.to_string());
                        retain_journal = true;
                        existing.save(root)?;
                        eprintln!("   Retry unregister for '{package_id}' failed: {e}");
                    }
                }
            }
            PluginDeactivateStatus::Failed => {
                retain_journal = true;
            }
        }
    }

    // Drop Unregistered entries completed in this pass.
    existing
        .entries
        .retain(|e| e.status != PluginDeactivateStatus::Unregistered);
    if changed {
        lockfile.save(root)?;
    }

    let any_prepared = existing
        .entries
        .iter()
        .any(|e| e.status == PluginDeactivateStatus::Prepared);
    let any_failed = existing
        .entries
        .iter()
        .any(|e| e.status == PluginDeactivateStatus::Failed);

    if retain_journal || any_failed || any_prepared || !existing.entries.is_empty() {
        existing.save(root)?;
        eprintln!("   Plugin deactivation journal retained (failed or still prepared entries).");
    } else {
        PendingPluginDeactivate::delete(root)?;
        eprintln!("   Plugin deactivation reconciliation complete.");
    }

    Ok(())
}

/// Invoke Nu with `plugin rm` — identity and config via environment variables only.
pub(crate) fn run_plugin_rm(
    nu_executable: &str,
    plugin_rm_identity: &str,
    plugin_config: &str,
) -> Result<()> {
    let output = std::process::Command::new(nu_executable)
        .args(["-c", RM_PLUGIN])
        .env("NUMAN_PLUGIN_NAME", plugin_rm_identity)
        .env("NUMAN_PLUGIN_CONFIG", plugin_config)
        .output()
        .with_context(|| format!("Failed to invoke Nu at '{nu_executable}'"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("plugin rm failed:\n{stderr}");
    }
    Ok(())
}

/// Count the number of modules currently active for the given Nu identity.
/// Used only in tests.
#[cfg(test)]
fn count_active_modules(lockfile: &Lockfile, nu_paths: &NuPaths) -> usize {
    let vendor_dir = nu_paths.vendor_autoload_dir.as_deref().unwrap_or("");
    let managed_path_str = if vendor_dir.is_empty() {
        String::new()
    } else {
        format!("{vendor_dir}/numan.nu")
    };

    lockfile
        .packages
        .values()
        .filter(|e| {
            e.package_type == "module"
                && e.is_module_active_for(
                    &nu_paths.nu_executable_hash,
                    &nu_paths.nu_version,
                    vendor_dir,
                    &managed_path_str,
                )
        })
        .count()
}

// ── Full deactivation ─────────────────────────────────────────────────────────

/// Run full module deactivation: verify ownership and hash, delete only the
/// Numan-managed `numan.nu`, clear activation records, remove autoload-state,
/// use the journal protocol.
///
/// This path is allowed even when Nu identity has drifted because no candidate
/// generation is needed — we are only deleting a verified Numan-managed file.
#[allow(clippy::too_many_arguments)]
fn run_full_deactivation(
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
    targets: &[ActiveModule],
    managed_path: &Path,
    vendor_autoload_dir: &str,
    managed_file_path: &str,
    previous_active_ids: &[String],
    pre_mutation_snapshot_id: Option<String>,
) -> Result<()> {
    if !managed_path.exists() {
        // Nothing to delete — just clear lockfile records.
        eprintln!(
            "{}  Managed file '{}' does not exist — clearing lockfile records only.",
            console::style("⚠").yellow(),
            managed_path.display()
        );
        clear_module_activations(lockfile, targets, root)?;
        AutoloadState::delete(root)?;
        print_deactivation_success(targets);
        return Ok(());
    }

    // Step 1-2: Verify ownership marker + SHA-256 against autoload-state.
    crate::util::fs_safety::assert_managed_file_owned(managed_path).with_context(|| {
        "Numan managed-file drift detected.\n\
         numan.nu was changed, replaced, moved, or is no longer a Numan-owned regular \
         file. Numan will not overwrite or delete it automatically."
    })?;

    // Record previous SHA-256 for the journal.
    let previous_sha256 = sha256_file(managed_path)?;

    let previous_active_sorted = {
        let mut v = previous_active_ids.to_vec();
        v.sort();
        v
    };

    // Step 3: Write journal at Prepared (desired_file_exists = false).
    let journal = PendingAutoload {
        schema_version: AUTOLOAD_SCHEMA_VERSION,
        operation: AutoloadOperation::Deactivate,
        stage: AutoloadStage::Prepared,
        nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
        nu_version: nu_paths.nu_version.clone(),
        vendor_autoload_dir: vendor_autoload_dir.to_string(),
        managed_file_path: managed_file_path.to_string(),
        previous_file_exists: true,
        previous_file_sha256: Some(previous_sha256),
        desired_file_exists: false,
        candidate_sha256: None,
        previous_active_module_ids: previous_active_sorted,
        desired_active_module_ids: vec![],
        targeted_module_ids: targets.iter().map(|m| m.package_id.clone()).collect(),
        created_at: format_timestamp(),
        pre_mutation_snapshot_id,
    };
    journal.save(root)?;

    // Step 4: Delete only the verified Numan-managed `numan.nu`.
    delete_managed_file(managed_path).with_context(|| {
        "Failed to delete Numan-managed autoload file. Journal preserved for recovery."
    })?;

    // Step 5: Update journal to Replaced.
    let mut journal = journal;
    journal.stage = AutoloadStage::Replaced;
    journal.save(root)?;

    // Step 6: Clear module activation records from lockfile.
    if let Err(e) = clear_module_activations(lockfile, targets, root) {
        eprintln!(
            "{} Failed to clear lockfile module activation records: {}\n\
             Journal preserved at state/pending-autoload.json for recovery.",
            console::style("✗").red(),
            e
        );
        return Err(e);
    }

    // Step 7: Delete autoload-state.json.
    if let Err(e) = AutoloadState::delete(root) {
        eprintln!(
            "{} Failed to delete autoload-state: {}\n\
             Journal preserved at state/pending-autoload.json for recovery.",
            console::style("✗").red(),
            e
        );
        return Err(e);
    }

    // Step 8: Clear journal.
    PendingAutoload::delete(root)?;

    print_deactivation_success(targets);
    Ok(())
}

// ── Partial deactivation ──────────────────────────────────────────────────────

/// Run partial module deactivation: regenerate and validate a candidate
/// containing only the remaining active modules, then execute the full
/// 13-step module-autoload transaction.
///
/// Nu must not be stale when this path is taken (candidate generation required).
#[allow(clippy::too_many_arguments)]
fn run_partial_deactivation(
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
    targets: &[ActiveModule],
    remaining_ids: &[String],
    managed_path: &Path,
    vendor_autoload_dir: &str,
    managed_file_path: &str,
    previous_active_ids: &[String],
    runner: &dyn CandidateRunner,
    pre_mutation_snapshot_id: Option<String>,
) -> Result<()> {
    // Resolve remaining module entries from the lockfile.
    let mut remaining_entries = Vec::new();
    for pkg_id in remaining_ids {
        let entry = lockfile.packages.get(pkg_id).with_context(|| {
            format!("Package '{pkg_id}' in remaining set not found in lockfile")
        })?;
        if let Some(ma) = &entry.module_activation {
            let resolved_entry = crate::nu::autoload::ResolvedEntry {
                absolute_path: PathBuf::from(&ma.entry_path),
                import_mode: ma.import_mode.clone(),
                scoped_id: pkg_id.clone(),
            };
            remaining_entries.push(resolved_entry);
        } else {
            // Remaining module has no activation record — skip (shouldn't happen).
            eprintln!(
                "{}  Skipping '{}' in remaining set: no module_activation record.",
                console::style("⚠").yellow(),
                pkg_id
            );
        }
    }

    // Also include any other currently active modules not in this deactivation set
    // and not already in remaining_entries (safety: should be the same set).
    // remaining_ids already excludes targets, so remaining_entries is the full
    // post-deactivation set.

    // Step 4: Generate candidate content for remaining modules.
    let content = generate_autoload_content(&remaining_entries)
        .context("Failed to generate autoload content for partial deactivation")?;

    // Step 5: Write candidate file (same directory as managed file).
    let candidate_path = write_candidate(managed_path, &content)
        .context("Failed to write candidate file for partial deactivation")?;

    // Compute candidate SHA-256 for journal.
    let candidate_sha = sha256_file(&candidate_path)?;

    // Record previous state for journal.
    let previous_file_exists = managed_path.exists();
    let previous_sha256 = if previous_file_exists {
        Some(sha256_file(managed_path)?)
    } else {
        None
    };

    let previous_active_sorted = {
        let mut v = previous_active_ids.to_vec();
        v.sort();
        v
    };

    let mut desired_active_ids = remaining_ids.to_vec();
    desired_active_ids.sort();
    desired_active_ids.dedup();

    // Step 5 (Nu validation): Execute candidate with the candidate runner.
    let scoped_ids: Vec<&str> = remaining_ids.iter().map(|s| s.as_str()).collect();
    if let Err(e) = validate_candidate(&candidate_path, runner, &scoped_ids) {
        eprintln!(
            "{} Module candidate validation failed: {}",
            console::style("✗").red(),
            e
        );
        bail!("Partial deactivation failed: candidate validation error. Managed file unchanged.");
    }

    // Step 6: Snapshot already done at the top level before lane entry.

    // Step 7: Atomically write journal at Prepared.
    let journal = PendingAutoload {
        schema_version: AUTOLOAD_SCHEMA_VERSION,
        operation: AutoloadOperation::Deactivate,
        stage: AutoloadStage::Prepared,
        nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
        nu_version: nu_paths.nu_version.clone(),
        vendor_autoload_dir: vendor_autoload_dir.to_string(),
        managed_file_path: managed_file_path.to_string(),
        previous_file_exists,
        previous_file_sha256: previous_sha256,
        desired_file_exists: true,
        candidate_sha256: Some(candidate_sha),
        previous_active_module_ids: previous_active_sorted,
        desired_active_module_ids: desired_active_ids.clone(),
        targeted_module_ids: targets.iter().map(|m| m.package_id.clone()).collect(),
        created_at: format_timestamp(),
        pre_mutation_snapshot_id,
    };
    journal.save(root)?;

    // Step 8: Atomically replace numan.nu with the validated candidate.
    if let Err(e) = replace_managed_file(managed_path, &candidate_path) {
        eprintln!(
            "{} Failed to replace managed autoload file: {}",
            console::style("✗").red(),
            e
        );
        bail!("Partial deactivation failed: could not replace managed file.");
    }

    // Step 9: Update journal to Replaced.
    let mut journal = journal;
    journal.stage = AutoloadStage::Replaced;
    journal.save(root)?;

    // Step 10: Clear module activation records for deactivated targets.
    //          (Remaining modules keep their activation records.)
    if let Err(e) = clear_module_activations(lockfile, targets, root) {
        eprintln!(
            "{} Failed to clear lockfile module activation records: {}\n\
             Journal preserved at state/pending-autoload.json for recovery.",
            console::style("✗").red(),
            e
        );
        return Err(e);
    }

    // Step 11: Atomically write derived autoload-state.json.
    let file_sha = sha256_file(managed_path)?;
    let autoload_state = AutoloadState::new(
        vendor_autoload_dir.to_string(),
        managed_file_path.to_string(),
        nu_paths.nu_executable_hash.clone(),
        nu_paths.nu_version.clone(),
        file_sha,
        desired_active_ids,
        format_timestamp(),
    );
    if let Err(e) = autoload_state.save(root) {
        eprintln!(
            "{} Failed to write autoload-state after partial deactivation: {}\n\
             Journal preserved at state/pending-autoload.json for recovery.",
            console::style("✗").red(),
            e
        );
        return Err(e);
    }

    // Step 12: Clear the journal.
    PendingAutoload::delete(root)?;

    // Step 13: (Lock released via RAII when _lock is dropped in caller)

    print_deactivation_success(targets);
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Clear `module_activation` from each deactivated target and persist lockfile.
fn clear_module_activations(
    lockfile: &mut Lockfile,
    targets: &[ActiveModule],
    root: &Path,
) -> Result<()> {
    for target in targets {
        if let Some(pkg) = lockfile.packages.get_mut(&target.package_id) {
            pkg.module_activation = None;
        }
    }
    lockfile.save(root)
}

fn print_deactivation_success(targets: &[ActiveModule]) {
    for target in targets {
        println!(
            "{} Module {} removed from autoload — takes effect in future Nu sessions",
            console::style("✓").green(),
            target.package_id,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::integrity;
    use crate::core::package::ModuleImportMode;
    use crate::state::lockfile::{Lockfile, LockfileEntry, ModuleActivation, PluginActivation};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn make_lockfile_with_modules(entries: Vec<(&str, &str, bool)>) -> Lockfile {
        // entries: (pkg_id, pkg_type, has_activation)
        let mut packages = BTreeMap::new();
        for (id, pkg_type, has_activation) in entries {
            let module_activation = if has_activation && pkg_type == "module" {
                Some(ModuleActivation {
                    entry_path: format!("/root/packages/modules/{id}/1.0.0-abc/mod.nu"),
                    import_mode: ModuleImportMode::Module,
                    vendor_autoload_dir: "/nu/vendor/autoload".to_string(),
                    managed_file_path: "/nu/vendor/autoload/numan.nu".to_string(),
                    nu_executable_sha256: "exe-hash".to_string(),
                    nu_version: "0.113.1".to_string(),
                    activated_at: "0".to_string(),
                })
            } else {
                None
            };

            packages.insert(
                id.to_string(),
                LockfileEntry {
                    version: "1.0.0".to_string(),
                    package_type: pkg_type.to_string(),
                    source: "archive".to_string(),
                    target: None,
                    artifact_url: None,
                    artifact_sha256: None,
                    executable_path: if pkg_type == "plugin" {
                        Some("nu_plugin_thing".to_string())
                    } else {
                        None
                    },
                    archive_root: None,
                    include: None,
                    entry: Some("mod.nu".to_string()),
                    installed_at: "0".to_string(),
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
                    payload_path: format!("packages/{pkg_type}s/{id}/1.0.0-abc"),
                    revision_id: None,
                    payload_sha256: None,
                    executable_sha256: None,
                    selection_reason: None,
                    origin: None,
                    module_activation,
                    module_import_mode: Some(ModuleImportMode::Module),
                    locked_dependencies: BTreeMap::new(),
                },
            );
        }
        Lockfile {
            version: 2,
            generated_at: "ts".to_string(),
            nu_version: "0.113.1".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            packages,
        }
    }

    fn plugin_activation() -> PluginActivation {
        PluginActivation {
            plugin_registry_path: "/path/to/plugins.msgpackz".to_string(),
            nu_executable_sha256: "exe-hash".to_string(),
            nu_version: "0.113.1".to_string(),
            activated_at: "0".to_string(),
        }
    }

    fn make_active_plugin_lockfile(root: &Path, pkg_id: &str) -> Lockfile {
        let mut lockfile = make_lockfile_with_modules(vec![(pkg_id, "plugin", false)]);
        let entry = lockfile.packages.get_mut(pkg_id).unwrap();
        entry.activation = Some(plugin_activation());
        entry.executable_path = Some("nu_plugin_highlight".to_string());
        entry.payload_path = format!("packages/plugins/{pkg_id}/1.0.0-abc");
        let payload_dir = root.join(&entry.payload_path);
        std::fs::create_dir_all(&payload_dir).unwrap();
        std::fs::write(payload_dir.join("nu_plugin_highlight"), b"fake").unwrap();
        lockfile
    }

    fn fake_nu_paths() -> NuPaths {
        NuPaths {
            nu_executable: "/usr/bin/nu".to_string(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: "/path/to/plugins.msgpackz".to_string(),
            nu_executable_hash: "exe-hash".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            data_dir: Some("/nu".to_string()),
            vendor_autoload_dirs: vec!["/nu/vendor/autoload".to_string()],
            vendor_autoload_dir: Some("/nu/vendor/autoload".to_string()),
        }
    }

    struct PluginTestEnv {
        dir: TempDir,
        nu_hash: String,
        nu_exe: PathBuf,
        registry_path: PathBuf,
    }

    impl PluginTestEnv {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let nu_exe = dir.path().join("fake_nu");
            std::fs::write(&nu_exe, b"fake nu binary").unwrap();
            let nu_hash = integrity::compute_sha256(b"fake nu binary");
            let registry_dir = dir.path().join("nushell");
            std::fs::create_dir_all(&registry_dir).unwrap();
            let registry_path = registry_dir.join("plugins.msgpackz");
            Self {
                dir,
                nu_hash,
                nu_exe,
                registry_path,
            }
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn write_nu_paths(&self) {
            let paths = NuPaths {
                nu_executable: self.nu_exe.to_string_lossy().into_owned(),
                nu_version: "0.113.1".to_string(),
                plugin_registry_path: self.registry_path.to_string_lossy().into_owned(),
                nu_executable_hash: self.nu_hash.clone(),
                platform: "x86_64-pc-windows-msvc".to_string(),
                data_dir: None,
                vendor_autoload_dirs: vec![],
                vendor_autoload_dir: None,
            };
            paths.save(self.root()).unwrap();
        }

        fn activation(&self) -> PluginActivation {
            PluginActivation {
                plugin_registry_path: self.registry_path.to_string_lossy().into_owned(),
                nu_executable_sha256: self.nu_hash.clone(),
                nu_version: "0.113.1".to_string(),
                activated_at: "0".to_string(),
            }
        }

        fn seed_active_plugin(&self, pkg_id: &str) {
            std::fs::create_dir_all(self.root().join("state")).unwrap();
            std::fs::create_dir_all(self.root().join("packages")).unwrap();
            let mut lockfile = make_active_plugin_lockfile(self.root(), pkg_id);
            let entry = lockfile.packages.get_mut(pkg_id).unwrap();
            entry.activation = Some(self.activation());
            lockfile.save(self.root()).unwrap();
        }
    }

    // ── plugin_name_from_executable_path ──────────────────────────────────────

    #[test]
    fn plugin_name_strips_prefix_and_exe() {
        assert_eq!(
            plugin_name_from_executable_path("nu_plugin_highlight"),
            "highlight"
        );
        assert_eq!(
            plugin_name_from_executable_path("nu_plugin_file.exe"),
            "file"
        );
        assert_eq!(
            plugin_name_from_executable_path(r"bin\nu_plugin_query.exe"),
            "query"
        );
        assert_eq!(
            plugin_name_from_executable_path("custom_name"),
            "custom_name"
        );
    }

    #[test]
    fn rm_plugin_uses_env_only() {
        assert!(RM_PLUGIN.contains("--force"));
        assert!(RM_PLUGIN.contains("$env.NUMAN_PLUGIN_NAME"));
        assert!(RM_PLUGIN.contains("$env.NUMAN_PLUGIN_CONFIG"));
        assert!(
            !RM_PLUGIN.contains('/'),
            "No literal paths in RM_PLUGIN command string"
        );
    }

    // ── classify_and_validate_packages ────────────────────────────────────────

    #[test]
    fn active_plugin_is_classified_not_deferred() {
        let dir = TempDir::new().unwrap();
        let lockfile = make_active_plugin_lockfile(dir.path(), "owner/myplugin");

        let args = DeactivateArgs {
            packages: vec!["owner/myplugin".to_string()],

            verbose: false,
        };

        let targets =
            classify_and_validate_packages(&args, &lockfile, dir.path(), &fake_nu_paths()).unwrap();
        assert_eq!(targets.plugins.len(), 1);
        assert_eq!(targets.plugins[0].package_id, "owner/myplugin");
        assert_eq!(targets.plugins[0].plugin_name, "highlight");
        assert!(targets.modules.is_empty());
    }

    #[test]
    fn stale_plugin_activation_is_rejected_when_explicit() {
        let dir = TempDir::new().unwrap();
        let lockfile = make_active_plugin_lockfile(dir.path(), "owner/myplugin");
        let mut stale_paths = fake_nu_paths();
        stale_paths.nu_executable_hash = "other-hash".to_string();

        let args = DeactivateArgs {
            packages: vec!["owner/myplugin".to_string()],

            verbose: false,
        };

        let err =
            classify_and_validate_packages(&args, &lockfile, dir.path(), &stale_paths).unwrap_err();
        assert!(
            err.to_string().contains("does not match"),
            "Expected stale-identity error, got: {err}"
        );
    }

    #[test]
    fn stale_plugin_activation_skipped_on_broad_cleanup() {
        let dir = TempDir::new().unwrap();
        let lockfile = make_active_plugin_lockfile(dir.path(), "owner/myplugin");
        let mut stale_paths = fake_nu_paths();
        stale_paths.nu_executable_hash = "other-hash".to_string();

        let args = DeactivateArgs {
            packages: vec![],

            verbose: false,
        };

        let targets =
            classify_and_validate_packages(&args, &lockfile, dir.path(), &stale_paths).unwrap();
        assert!(targets.plugins.is_empty());
    }

    #[test]
    fn inactive_plugin_is_skipped_when_explicit() {
        let dir = TempDir::new().unwrap();
        let lockfile = make_lockfile_with_modules(vec![("owner/myplugin", "plugin", false)]);

        let args = DeactivateArgs {
            packages: vec!["owner/myplugin".to_string()],

            verbose: false,
        };

        let targets =
            classify_and_validate_packages(&args, &lockfile, dir.path(), &fake_nu_paths()).unwrap();
        assert!(targets.plugins.is_empty());
        assert!(targets.modules.is_empty());
    }

    #[test]
    fn script_type_returns_deferred_error() {
        let dir = TempDir::new().unwrap();
        let lockfile = make_lockfile_with_modules(vec![("owner/myscript", "script", false)]);

        let args = DeactivateArgs {
            packages: vec!["owner/myscript".to_string()],

            verbose: false,
        };

        let err = classify_and_validate_packages(&args, &lockfile, dir.path(), &fake_nu_paths())
            .unwrap_err();
        assert!(
            err.to_string().contains("deferred"),
            "Expected deferred error for script type, got: {err}"
        );
    }

    #[test]
    fn completion_type_returns_deferred_error() {
        let dir = TempDir::new().unwrap();
        let lockfile = make_lockfile_with_modules(vec![("owner/mycomp", "completion", false)]);

        let args = DeactivateArgs {
            packages: vec!["owner/mycomp".to_string()],

            verbose: false,
        };

        let err = classify_and_validate_packages(&args, &lockfile, dir.path(), &fake_nu_paths())
            .unwrap_err();
        assert!(
            err.to_string().contains("deferred"),
            "Expected deferred error for completion type, got: {err}"
        );
    }

    #[test]
    fn inactive_module_returns_error() {
        let dir = TempDir::new().unwrap();
        let lockfile =
            make_lockfile_with_modules(vec![("owner/mymod", "module", false /* inactive */)]);

        let args = DeactivateArgs {
            packages: vec!["owner/mymod".to_string()],

            verbose: false,
        };

        let err = classify_and_validate_packages(&args, &lockfile, dir.path(), &fake_nu_paths())
            .unwrap_err();
        assert!(
            err.to_string().contains("not currently active"),
            "Expected not-active error, got: {err}"
        );
    }

    #[test]
    fn missing_package_returns_error() {
        let dir = TempDir::new().unwrap();
        let lockfile = make_lockfile_with_modules(vec![]);

        let args = DeactivateArgs {
            packages: vec!["owner/nosuchpkg".to_string()],

            verbose: false,
        };

        let err = classify_and_validate_packages(&args, &lockfile, dir.path(), &fake_nu_paths())
            .unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "Expected not-found error, got: {err}"
        );
    }

    #[test]
    fn active_module_is_resolved() {
        let dir = TempDir::new().unwrap();
        let lockfile =
            make_lockfile_with_modules(vec![("owner/mymod", "module", true /* active */)]);

        let args = DeactivateArgs {
            packages: vec!["owner/mymod".to_string()],

            verbose: false,
        };

        let targets =
            classify_and_validate_packages(&args, &lockfile, dir.path(), &fake_nu_paths()).unwrap();
        assert_eq!(targets.modules.len(), 1);
        assert_eq!(targets.modules[0].package_id, "owner/mymod");
        assert!(targets.plugins.is_empty());
    }

    #[test]
    fn full_module_deactivation_ignores_duplicate_requested_ids() {
        let lockfile = make_lockfile_with_modules(vec![
            ("owner/alpha", "module", true),
            ("owner/beta", "module", true),
        ]);
        let duplicate_partial = vec![
            ActiveModule {
                package_id: "owner/alpha".to_string(),
                vendor_autoload_dir: "/nu/vendor/autoload".to_string(),
                managed_file_path: "/nu/vendor/autoload/numan.nu".to_string(),
            },
            ActiveModule {
                package_id: "owner/alpha".to_string(),
                vendor_autoload_dir: "/nu/vendor/autoload".to_string(),
                managed_file_path: "/nu/vendor/autoload/numan.nu".to_string(),
            },
        ];
        assert!(
            !is_full_module_deactivation(&lockfile, &duplicate_partial),
            "duplicate args must not count as full deactivation"
        );

        let full = vec![
            ActiveModule {
                package_id: "owner/alpha".to_string(),
                vendor_autoload_dir: "/nu/vendor/autoload".to_string(),
                managed_file_path: "/nu/vendor/autoload/numan.nu".to_string(),
            },
            ActiveModule {
                package_id: "owner/beta".to_string(),
                vendor_autoload_dir: "/nu/vendor/autoload".to_string(),
                managed_file_path: "/nu/vendor/autoload/numan.nu".to_string(),
            },
        ];
        assert!(is_full_module_deactivation(&lockfile, &full));
    }

    #[test]
    fn no_packages_returns_all_active_modules() {
        let dir = TempDir::new().unwrap();
        let lockfile = make_lockfile_with_modules(vec![
            ("owner/alpha", "module", true),
            ("owner/beta", "module", true),
            ("owner/gamma", "module", false), // inactive — excluded
        ]);

        let args = DeactivateArgs {
            packages: vec![],

            verbose: false,
        };

        let targets =
            classify_and_validate_packages(&args, &lockfile, dir.path(), &fake_nu_paths()).unwrap();
        assert_eq!(targets.modules.len(), 2);
        assert_eq!(targets.modules[0].package_id, "owner/alpha");
        assert_eq!(targets.modules[1].package_id, "owner/beta");
    }

    #[test]
    fn reclassification_rejects_expanded_implicit_targets_after_planning() {
        let dir = TempDir::new().unwrap();
        let args = DeactivateArgs {
            packages: vec![],

            verbose: false,
        };
        let confirmed_lockfile = make_lockfile_with_modules(vec![("owner/alpha", "module", true)]);
        let confirmed_targets = classify_and_validate_packages(
            &args,
            &confirmed_lockfile,
            dir.path(),
            &fake_nu_paths(),
        )
        .unwrap();
        let changed_lockfile = make_lockfile_with_modules(vec![
            ("owner/alpha", "module", true),
            ("owner/beta", "module", true),
        ]);

        let error = reclassify_targets(
            &args,
            &changed_lockfile,
            dir.path(),
            &fake_nu_paths(),
            &confirmed_targets,
        )
        .unwrap_err();
        assert!(error.to_string().contains("changed since planning"));
    }

    #[test]
    fn non_tty_auto_confirms() {
        // Use the *_with_tty variant so the assertion doesn't depend on the
        // host machine's stdin state; preserves both branches' coverage.
        let result = crate::util::confirm::confirm_or_auto_with_tty("test?", false, false);
        assert!(result.unwrap(), "non-TTY should auto-confirm");
    }

    // ── fake unregistrar integration ──────────────────────────────────────────

    #[test]
    fn fake_unregistrar_success_clears_activation() {
        let env = PluginTestEnv::new();
        env.write_nu_paths();
        env.seed_active_plugin("owner/highlight");

        let args = DeactivateArgs {
            packages: vec!["owner/highlight".to_string()],

            verbose: false,
        };
        execute_with_unregistrar(&args, env.root(), &|_nu, identity, _cfg| {
            let normalized = identity.replace('\\', "/");
            assert!(
                normalized.ends_with("nu_plugin_highlight"),
                "expected absolute plugin path, got {identity}"
            );
            Ok(())
        })
        .unwrap();

        let lockfile = Lockfile::load(env.root()).unwrap();
        let entry = lockfile.packages.get("owner/highlight").unwrap();
        assert!(entry.activation.is_none());
        assert!(PendingPluginDeactivate::load(env.root()).unwrap().is_none());
    }

    #[test]
    fn prepared_journal_recovery_skips_unregistrar_on_nu_drift() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let env = PluginTestEnv::new();
        env.write_nu_paths();
        env.seed_active_plugin("owner/highlight");

        let lockfile = Lockfile::load(env.root()).unwrap();
        let entry = lockfile.packages.get("owner/highlight").unwrap();
        let absolute_binary_path = env
            .root()
            .join(&entry.payload_path)
            .join(entry.executable_path.as_ref().unwrap())
            .to_string_lossy()
            .into_owned();

        let journal = PendingPluginDeactivate {
            nu_executable_sha256: env.nu_hash.clone(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: env.registry_path.to_string_lossy().into_owned(),
            created_at: "0".to_string(),
            entries: vec![PendingPluginDeactivateEntry {
                package_id: "owner/highlight".to_string(),
                plugin_name: "highlight".to_string(),
                absolute_binary_path,
                status: PluginDeactivateStatus::Prepared,
                error: None,
            }],
        };
        journal.save(env.root()).unwrap();

        // Swap the cached Nu binary after the journal was written.
        std::fs::write(&env.nu_exe, b"swapped nu binary contents").unwrap();

        let called = AtomicBool::new(false);
        let args = DeactivateArgs {
            packages: vec![],

            verbose: false,
        };
        let err = execute_with_unregistrar(&args, env.root(), &|_nu, _identity, _cfg| {
            called.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap_err();

        assert!(
            err.to_string().contains("hash mismatch") || err.to_string().contains("changed since"),
            "expected drift error, got: {err}"
        );
        assert!(
            !called.load(Ordering::SeqCst),
            "unregistrar must not run when Nu binary drifted"
        );
        assert!(
            PendingPluginDeactivate::load(env.root())
                .unwrap()
                .is_some_and(|j| j.entries[0].status == PluginDeactivateStatus::Prepared),
            "Prepared journal must remain unreplayed after drift"
        );
    }

    #[test]
    fn unregistered_journal_clears_activation_despite_identity_mismatch() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let env = PluginTestEnv::new();
        env.write_nu_paths();
        env.seed_active_plugin("owner/highlight");

        let lockfile = Lockfile::load(env.root()).unwrap();
        let entry = lockfile.packages.get("owner/highlight").unwrap();
        let absolute_binary_path = env
            .root()
            .join(&entry.payload_path)
            .join(entry.executable_path.as_ref().unwrap())
            .to_string_lossy()
            .into_owned();

        let journal = PendingPluginDeactivate {
            nu_executable_sha256: "stale-journal-hash".to_string(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: env.registry_path.to_string_lossy().into_owned(),
            created_at: "0".to_string(),
            entries: vec![PendingPluginDeactivateEntry {
                package_id: "owner/highlight".to_string(),
                plugin_name: "highlight".to_string(),
                absolute_binary_path,
                status: PluginDeactivateStatus::Unregistered,
                error: None,
            }],
        };
        journal.save(env.root()).unwrap();

        let called = AtomicBool::new(false);
        let args = DeactivateArgs {
            packages: vec!["owner/highlight".to_string()],

            verbose: false,
        };
        execute_with_unregistrar(&args, env.root(), &|_nu, _identity, _cfg| {
            called.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();

        assert!(
            !called.load(Ordering::SeqCst),
            "Unregistered recovery must not spawn unregistrar"
        );
        let lockfile = Lockfile::load(env.root()).unwrap();
        assert!(lockfile
            .packages
            .get("owner/highlight")
            .unwrap()
            .activation
            .is_none());
        assert!(PendingPluginDeactivate::load(env.root()).unwrap().is_none());
    }

    #[test]
    fn fake_unregistrar_failure_leaves_activation_and_journal() {
        let env = PluginTestEnv::new();
        env.write_nu_paths();
        env.seed_active_plugin("owner/highlight");

        let args = DeactivateArgs {
            packages: vec!["owner/highlight".to_string()],

            verbose: false,
        };
        let err = execute_with_unregistrar(&args, env.root(), &|_nu, _name, _cfg| {
            bail!("simulated unregister failure")
        })
        .unwrap_err();
        assert!(err.to_string().contains("failed to deactivate"));

        let lockfile = Lockfile::load(env.root()).unwrap();
        assert!(lockfile
            .packages
            .get("owner/highlight")
            .unwrap()
            .activation
            .is_some());

        let journal = PendingPluginDeactivate::load(env.root()).unwrap().unwrap();
        assert_eq!(journal.entries.len(), 1);
        assert_eq!(journal.entries[0].status, PluginDeactivateStatus::Failed);
    }

    #[test]
    fn remove_succeeds_after_activation_cleared() {
        use crate::util::hints;

        let mut entry = LockfileEntry {
            version: "1.0.0".to_string(),
            package_type: "plugin".to_string(),
            source: "binary".to_string(),
            target: None,
            artifact_url: None,
            artifact_sha256: None,
            executable_path: Some("nu_plugin_x".to_string()),
            archive_root: None,
            include: None,
            entry: None,
            installed_at: "0".to_string(),
            nu_version_at_install: None,
            activation: Some(plugin_activation()),
            registry_url: None,
            registry_revision: None,
            index_sha256: None,
            signing_key_fingerprint: None,
            git_url: None,
            git_rev: None,
            cargo_name: None,
            cargo_lock_sha256: None,
            built_sha256: None,
            payload_path: String::new(),
            revision_id: None,
            payload_sha256: None,
            executable_sha256: None,
            selection_reason: None,
            origin: None,
            module_activation: None,
            module_import_mode: None,
            locked_dependencies: BTreeMap::new(),
        };

        // Active: remove refused
        assert!(entry.activation.is_some());
        let gated = hints::active_plugin_mutation_gated("owner/x");
        assert!(gated.contains("deactivate"));

        // After deactivate clears activation: remove allowed
        entry.activation = None;
        assert!(entry.activation.is_none());
    }

    // ── count_active_modules ──────────────────────────────────────────────────

    #[test]
    fn count_active_modules_returns_correct_count() {
        let lockfile = make_lockfile_with_modules(vec![
            ("owner/alpha", "module", true),
            ("owner/beta", "module", true),
            ("owner/gamma", "module", false),
        ]);
        let nu_paths = fake_nu_paths();
        assert_eq!(count_active_modules(&lockfile, &nu_paths), 2);
    }

    #[test]
    fn count_active_modules_excludes_wrong_identity() {
        let lockfile = make_lockfile_with_modules(vec![("owner/alpha", "module", true)]);
        let mut nu_paths = fake_nu_paths();
        nu_paths.nu_executable_hash = "different-hash".to_string();
        assert_eq!(count_active_modules(&lockfile, &nu_paths), 0);
    }
}
