use anyhow::{bail, Context, Result};
use clap::Args;
use std::path::{Component, Path, PathBuf};

use crate::nu::autoload::{
    generate_autoload_content, replace_managed_file, resolve_entry, validate_candidate,
    write_candidate, CandidateRunner, NuCandidateRunner,
};
use crate::nu::paths::NuPaths;
use crate::state::autoload_journal::{sha256_file, SCHEMA_VERSION as AUTOLOAD_SCHEMA_VERSION};
use crate::state::autoload_journal::{AutoloadOperation, AutoloadStage, PendingAutoload};
use crate::state::autoload_recovery::reconcile_pending_autoload;
use crate::state::autoload_state::AutoloadState;
use crate::state::journal::{PendingActivation, PendingActivationEntry, PendingStatus};
use crate::state::lockfile::{Lockfile, ModuleActivation, PluginActivation};
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::format_timestamp;

use super::print_autoload_recovery;
use crate::util::fs_safety::acquire_mutation_lock;
use crate::util::hints::{self, CMD_ACTIVATE, CMD_INIT_REFRESH};

#[derive(Args, Debug)]
pub struct ActivateArgs {
    /// Package IDs (owner/name) to activate. Omit to activate all installed inactive packages.
    pub packages: Vec<String>,

    /// Show detailed output
    #[arg(long)]
    pub verbose: bool,

    /// List all installed packages and their activation status (read-only)
    #[arg(long, conflicts_with = "packages")]
    pub list: bool,

    /// Check activation integrity for packages (read-only, no mutation)
    #[arg(long, conflicts_with = "list")]
    pub check: bool,
}

/// Resolved plugin target ready for registration.
#[derive(Debug)]
struct PluginTarget {
    package_id: String,
    payload_path: String,
    executable_path: String,
    /// Canonicalized, root-anchored absolute path to the plugin binary.
    absolute_binary_path: PathBuf,
}

/// Resolved module target ready for autoload generation.
#[derive(Debug)]
struct ModuleTarget {
    package_id: String,
    payload_path: String,
    entry_relative: String,
    import_mode: crate::core::package::ModuleImportMode,
}

/// `plugin add` Nu program — paths come only from environment variables.
/// Never interpolated into the command string.
const ADD_PLUGIN: &str =
    "plugin add --plugin-config $env.NUMAN_PLUGIN_CONFIG $env.NUMAN_PLUGIN_BINARY";

pub fn execute(args: &ActivateArgs, root: &Path) -> Result<()> {
    execute_with_registrar_and_runner(args, root, &run_plugin_add, None)
}

/// Testability entry point — accepts an injectable plugin registrar.
///
/// The registrar receives `(nu_executable, plugin_binary_path, plugin_config_path)`
/// and returns `Ok(())` on success. Production code calls `run_plugin_add`.
/// Tests inject a fake registrar to exercise all flow paths without invoking Nu.
pub fn execute_with_registrar(
    args: &ActivateArgs,
    root: &Path,
    registrar: &dyn Fn(&str, &str, &str) -> Result<()>,
) -> Result<()> {
    execute_with_registrar_and_runner(args, root, registrar, None)
}

/// Full testability entry point — accepts both an injectable plugin registrar
/// and an injectable module candidate runner.
///
/// Production code uses `run_plugin_add` and `NuCandidateRunner`. Tests can
/// inject `FakeCandidateRunner` and a fake registrar to exercise all flow paths
/// without invoking Nu.
pub fn execute_with_candidate_runner(
    args: &ActivateArgs,
    root: &Path,
    registrar: &dyn Fn(&str, &str, &str) -> Result<()>,
    runner: &dyn CandidateRunner,
) -> Result<()> {
    execute_with_registrar_and_runner(args, root, registrar, Some(runner))
}

fn execute_with_registrar_and_runner(
    args: &ActivateArgs,
    root: &Path,
    registrar: &dyn Fn(&str, &str, &str) -> Result<()>,
    runner: Option<&dyn CandidateRunner>,
) -> Result<()> {
    // 1. Load cached Nu identity — never re-discover here
    let nu_paths = NuPaths::load(root)?;

    // 2. Validate drift — refuse if nu binary changed since init
    nu_paths.validate_drift()?;

    // 3. Load lockfile
    let lockfile = Lockfile::load(root)?;

    // Handle read-only subcommands first (no mutation, no lock needed)
    if args.list {
        return execute_list(&lockfile, &nu_paths);
    }

    if args.check {
        return execute_check(args, &lockfile, &nu_paths, root);
    }

    // 4. Preflight: validate any pending journal identity before touching targets.
    //    This is read-only — bails if a stale-identity journal exists so the user
    //    is warned before the consent table is printed.
    check_journals_for_stale_identity(root, &nu_paths)?;

    // 5. Reconcile interrupted work and resolve consent targets while holding
    //    the mutation lock. This prevents a journal from being skipped by an
    //    early no-op return.
    let planning_lock = acquire_mutation_lock(root)?;
    let mut lockfile = Lockfile::load(root)?;
    reconcile_plugin_journal(root, &nu_paths, &mut lockfile)?;
    print_autoload_recovery(reconcile_pending_autoload(root, &nu_paths, &mut lockfile)?);
    lockfile = Lockfile::load(root)?;
    let plugin_targets = resolve_plugin_targets(args, &lockfile, &nu_paths, root)?;
    let module_targets = resolve_module_targets(args, &lockfile, &nu_paths)?;

    // Check for deferred-feature errors (script/completion types)
    // These are already caught in resolve functions.

    if plugin_targets.is_empty() && module_targets.is_empty() {
        // Already-active (or inactive-but-requested) packages still need profile sync.
        sync_user_activate_profile(root, &nu_paths, args, &lockfile)?;
        println!("Nothing to activate.");
        return Ok(());
    }

    // 7. Resolve module autoload paths for the consent table
    let managed_file_path = if !module_targets.is_empty() {
        Some(resolve_managed_file_path(&nu_paths)?)
    } else {
        None
    };
    drop(planning_lock);

    // 8. Consent table (informational only; no prompt)
    print_grouped_consent_table(
        &plugin_targets,
        &module_targets,
        managed_file_path.as_deref(),
        &nu_paths.plugin_registry_path,
    );

    // 9. Reacquire the root mutation lock after consent.
    let _lock = acquire_mutation_lock(root)?;

    // Reload lockfile under the lock to get the latest state
    let mut lockfile = Lockfile::load(root)?;

    // 10. Reconcile any interrupted journals under the lock so recovery
    //     mutations are serialized with all other writes.
    reconcile_plugin_journal(root, &nu_paths, &mut lockfile)?;
    print_autoload_recovery(reconcile_pending_autoload(root, &nu_paths, &mut lockfile)?);
    // Reload after reconciliation — recovery may have mutated the lockfile.
    lockfile = Lockfile::load(root)?;

    // Re-resolve targets from the post-reconciliation lockfile so packages
    // that were just reconciled (already activated) are correctly skipped.
    let plugin_targets = resolve_plugin_targets(args, &lockfile, &nu_paths, root)?;
    let module_targets = resolve_module_targets(args, &lockfile, &nu_paths)?;

    if plugin_targets.is_empty() && module_targets.is_empty() {
        sync_user_activate_profile(root, &nu_paths, args, &lockfile)?;
        println!("Nothing to activate.");
        return Ok(());
    }

    let managed_file_path = if !module_targets.is_empty() {
        Some(resolve_managed_file_path(&nu_paths)?)
    } else {
        None
    };

    // 11. Snapshot current state once before first mutation
    let snapshot = create_snapshot(
        root,
        SnapshotReason::PreMutation,
        SnapshotTrigger::Activate,
        None,
        None,
    )?;

    let mut any_failed = false;

    // ── PLUGIN LANE ───────────────────────────────────────────────────────────
    if !plugin_targets.is_empty() {
        let plugin_failed = run_plugin_lane(
            args,
            root,
            &nu_paths,
            &mut lockfile,
            &plugin_targets,
            registrar,
        )?;
        if plugin_failed {
            any_failed = true;
        }
    }

    // ── MODULE LANE ───────────────────────────────────────────────────────────
    if !module_targets.is_empty() {
        let managed_file_path = managed_file_path.unwrap();
        let real_runner;
        let runner_ref: &dyn CandidateRunner = if let Some(r) = runner {
            r
        } else {
            real_runner = NuCandidateRunner::new(&nu_paths.nu_executable);
            &real_runner
        };

        let module_failed = run_module_lane(
            root,
            &nu_paths,
            &mut lockfile,
            &module_targets,
            &managed_file_path,
            runner_ref,
            Some(snapshot.id.clone()),
        )?;
        if module_failed {
            any_failed = true;
        }
    }

    // Reload lockfile so profile sync sees post-activation state for "activate all".
    let lockfile = Lockfile::load(root)?;
    sync_user_activate_profile(root, &nu_paths, args, &lockfile)?;

    if any_failed {
        bail!(
            "One or more packages failed to activate. Successful activations have been persisted."
        );
    }

    Ok(())
}

fn sync_user_activate_profile(
    root: &Path,
    nu_paths: &NuPaths,
    args: &ActivateArgs,
    lockfile: &Lockfile,
) -> Result<()> {
    let ids = if args.packages.is_empty() {
        let active = crate::cmd::activation_switch::collect_currently_active(lockfile, nu_paths);
        let mut all = active.plugins;
        all.extend(active.modules);
        all
    } else {
        args.packages.clone()
    };
    crate::cmd::activation_switch::sync_profile_after_user_activate(
        root,
        &nu_paths.nu_version,
        &ids,
        lockfile,
    )
}

// ── Plugin lane ────────────────────────────────────────────────────────────────

/// Run the plugin registration lane. Returns `true` if any plugin failed.
fn run_plugin_lane(
    args: &ActivateArgs,
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
    targets: &[PluginTarget],
    registrar: &dyn Fn(&str, &str, &str) -> Result<()>,
) -> Result<bool> {
    // Write pending journal — all entries start at `prepared`
    let mut journal = PendingActivation {
        nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
        nu_version: nu_paths.nu_version.clone(),
        plugin_registry_path: nu_paths.plugin_registry_path.clone(),
        created_at: format_timestamp(),
        entries: targets
            .iter()
            .map(|t| PendingActivationEntry {
                package_id: t.package_id.clone(),
                payload_path: t.payload_path.clone(),
                executable_path: t.executable_path.clone(),
                absolute_binary_path: t.absolute_binary_path.to_string_lossy().into_owned(),
                status: PendingStatus::Prepared,
                error: None,
            })
            .collect(),
    };
    journal.save(root)?;

    let mut any_failed = false;
    for (i, target) in targets.iter().enumerate() {
        if args.verbose {
            println!(
                "  Registering {} ({})",
                target.package_id,
                target.absolute_binary_path.display()
            );
        }

        let binary_str = target.absolute_binary_path.to_string_lossy();
        match registrar(
            &nu_paths.nu_executable,
            &binary_str,
            &nu_paths.plugin_registry_path,
        ) {
            Ok(()) => {
                // Advance journal entry to `registered`
                journal.entries[i].status = PendingStatus::Registered;
                journal.save(root)?;

                // Persist activation record to lockfile atomically
                if let Some(pkg) = lockfile.packages.get_mut(&target.package_id) {
                    pkg.activation = Some(PluginActivation {
                        plugin_registry_path: nu_paths.plugin_registry_path.clone(),
                        nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
                        nu_version: nu_paths.nu_version.clone(),
                        activated_at: format_timestamp(),
                    });
                }
                lockfile.save(root)?;

                println!(
                    "{} Registered {} — takes effect in future Nu sessions (not the current shell)",
                    console::style("✓").green(),
                    target.package_id,
                );
            }
            Err(e) => {
                journal.entries[i].status = PendingStatus::Failed;
                journal.entries[i].error = Some(e.to_string());
                journal.save(root)?;

                eprintln!(
                    "{} Failed to register {}: {}",
                    console::style("✗").red(),
                    target.package_id,
                    e
                );
                any_failed = true;
            }
        }
    }

    PendingActivation::delete(root)?;
    Ok(any_failed)
}

// ── Module lane ────────────────────────────────────────────────────────────────

/// Run the module autoload lane. Returns `true` if the module lane failed.
///
/// Implements the 13-step module-autoload transaction protocol from Phase4Plan §10.1.
fn run_module_lane(
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
    targets: &[ModuleTarget],
    managed_file_path: &str,
    runner: &dyn CandidateRunner,
    pre_mutation_snapshot_id: Option<String>,
) -> Result<bool> {
    let vendor_autoload_dir = nu_paths
        .vendor_autoload_dir
        .as_deref()
        .expect("vendor_autoload_dir must be set before module lane runs");

    let managed_path = Path::new(managed_file_path);

    // Resolve all module entries
    let mut entries = Vec::new();
    for target in targets {
        let entry_relative = &target.entry_relative;
        let resolved = resolve_entry(
            root,
            &target.payload_path,
            entry_relative,
            target.import_mode.clone(),
            &target.package_id,
        )
        .with_context(|| format!("Failed to resolve module entry for '{}'", target.package_id))?;
        entries.push(resolved);
    }

    // Collect already-active module entries (for modules not in this activation run)
    // so the generated file includes ALL active modules
    let currently_active_ids: Vec<String> = AutoloadState::active_module_ids_from_lockfile(
        lockfile,
        &nu_paths.nu_executable_hash,
        &nu_paths.nu_version,
        vendor_autoload_dir,
        managed_file_path,
    );

    // Add already-active modules not in this batch to the entry list
    for (pkg_id, entry) in &lockfile.packages {
        if entry.package_type != "module" {
            continue;
        }
        // Skip if already being activated in this batch
        if targets.iter().any(|t| &t.package_id == pkg_id) {
            continue;
        }
        // Include if currently active
        if currently_active_ids.contains(pkg_id) {
            if let Some(ma) = &entry.module_activation {
                let existing_entry = crate::nu::autoload::ResolvedEntry {
                    absolute_path: PathBuf::from(&ma.entry_path),
                    import_mode: ma.import_mode.clone(),
                    scoped_id: pkg_id.clone(),
                };
                entries.push(existing_entry);
            }
        }
    }

    // Step 4: Generate candidate content
    let content =
        generate_autoload_content(&entries).context("Failed to generate autoload content")?;

    // Step 5: Write candidate file (same directory as managed file)
    let candidate_path =
        write_candidate(managed_path, &content).context("Failed to write candidate file")?;

    // Compute candidate SHA-256 for journal
    let candidate_sha = sha256_file(&candidate_path)?;

    // Record previous managed file state for journal
    let previous_file_exists = managed_path.exists();
    let previous_file_sha256 = if previous_file_exists {
        let live_sha = sha256_file(managed_path)?;
        // Compare against autoload-state.json to detect edits that left the
        // ownership header intact but changed the file body (drift).
        if let Some(state) = AutoloadState::load(root)? {
            if state.generated_file_sha256 != live_sha {
                bail!(
                    "Numan managed-file drift detected.\n\n\
                     numan.nu was changed outside of Numan (SHA-256 mismatch with \
                     autoload-state.json). Numan will not overwrite it automatically.\n\
                     Expected: {}\n\
                     Found:    {}",
                    state.generated_file_sha256,
                    live_sha
                );
            }
        }
        Some(live_sha)
    } else {
        None
    };

    let previous_active_ids = currently_active_ids.clone();
    let mut desired_active_ids: Vec<String> = targets
        .iter()
        .map(|t| t.package_id.clone())
        .chain(currently_active_ids.iter().cloned())
        .collect();
    desired_active_ids.sort();
    desired_active_ids.dedup();

    let targeted_ids: Vec<String> = targets.iter().map(|t| t.package_id.clone()).collect();

    // Step 5 (Nu validation): Execute candidate with the candidate runner
    let scoped_ids: Vec<&str> = targets.iter().map(|t| t.package_id.as_str()).collect();
    if let Err(e) = validate_candidate(&candidate_path, runner, &scoped_ids) {
        // Candidate was removed by validate_candidate on failure
        eprintln!(
            "{} Module candidate validation failed: {}",
            console::style("✗").red(),
            e
        );
        return Ok(true); // lane failed
    }

    // Step 6: Snapshot is already done at the top level before lane entry

    // Step 7: Atomically write journal at Prepared
    let journal = PendingAutoload {
        schema_version: AUTOLOAD_SCHEMA_VERSION,
        operation: AutoloadOperation::Activate,
        stage: AutoloadStage::Prepared,
        nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
        nu_version: nu_paths.nu_version.clone(),
        vendor_autoload_dir: vendor_autoload_dir.to_string(),
        managed_file_path: managed_file_path.to_string(),
        previous_file_exists,
        previous_file_sha256: previous_file_sha256.clone(),
        desired_file_exists: true,
        candidate_sha256: Some(candidate_sha.clone()),
        previous_active_module_ids: previous_active_ids,
        desired_active_module_ids: desired_active_ids.clone(),
        targeted_module_ids: targeted_ids,
        created_at: format_timestamp(),
        pre_mutation_snapshot_id,
    };
    journal.save(root)?;

    // Step 8: Atomically replace numan.nu with the validated candidate
    if let Err(e) = replace_managed_file(managed_path, &candidate_path) {
        // Candidate still exists for inspection — do not clear journal
        eprintln!(
            "{} Failed to replace managed autoload file: {}",
            console::style("✗").red(),
            e
        );
        return Ok(true); // lane failed
    }

    // Step 9: Update journal to Replaced
    let mut journal = journal;
    journal.stage = AutoloadStage::Replaced;
    journal.save(root)?;

    // Step 10: Atomically write lockfile module activation records
    let ts = format_timestamp();
    for target in targets {
        if let Some(pkg) = lockfile.packages.get_mut(&target.package_id) {
            // Find the resolved entry's absolute path
            if let Some(resolved) = entries.iter().find(|e| e.scoped_id == target.package_id) {
                pkg.module_activation = Some(ModuleActivation {
                    entry_path: resolved.absolute_path.to_string_lossy().into_owned(),
                    import_mode: target.import_mode.clone(),
                    vendor_autoload_dir: vendor_autoload_dir.to_string(),
                    managed_file_path: managed_file_path.to_string(),
                    nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
                    nu_version: nu_paths.nu_version.clone(),
                    activated_at: ts.clone(),
                });
            }
        }
    }

    if let Err(e) = lockfile.save(root) {
        // Lockfile save failed after replacement — preserve the Replaced journal
        eprintln!(
            "{} Failed to persist lockfile after module activation: {}\n\
             Journal preserved at state/pending-autoload.json for recovery.",
            console::style("✗").red(),
            e
        );
        return Ok(true); // lane failed, journal preserved
    }

    // Step 11: Atomically write derived autoload-state.json
    let file_sha = sha256_file(managed_path)?;
    let autoload_state = AutoloadState::new(
        vendor_autoload_dir.to_string(),
        managed_file_path.to_string(),
        nu_paths.nu_executable_hash.clone(),
        nu_paths.nu_version.clone(),
        file_sha,
        desired_active_ids.clone(),
        format_timestamp(),
    );
    if let Err(e) = autoload_state.save(root) {
        // Autoload-state save failed — preserve the Replaced journal
        eprintln!(
            "{} Failed to write autoload-state after module activation: {}\n\
             Journal preserved at state/pending-autoload.json for recovery.",
            console::style("✗").red(),
            e
        );
        return Ok(true); // lane failed, journal preserved
    }

    // Step 12: Clear the journal
    PendingAutoload::delete(root)?;

    // Step 13: (Lock release happens via RAII when _lock is dropped in caller)

    for target in targets {
        println!(
            "{} Module {} added to autoload — takes effect in future Nu sessions",
            console::style("✓").green(),
            target.package_id,
        );
    }

    Ok(false) // lane succeeded
}

/// Activate named modules without acquiring the mutation lock, snapshotting, or
/// syncing the activation profile. Caller must hold the root mutation lock.
///
/// Returns `true` if the module lane reported failure.
pub(crate) fn activate_modules_unlocked(
    root: &Path,
    package_ids: &[String],
    runner: &dyn CandidateRunner,
) -> Result<bool> {
    if package_ids.is_empty() {
        return Ok(false);
    }

    let nu_paths = NuPaths::load(root)?;
    nu_paths.validate_drift()?;
    let mut lockfile = Lockfile::load(root)?;

    let args = ActivateArgs {
        packages: package_ids.to_vec(),
        verbose: false,
        list: false,
        check: false,
    };
    let module_targets = resolve_module_targets(&args, &lockfile, &nu_paths)?;
    if module_targets.is_empty() {
        return Ok(false);
    }
    let managed_file_path = resolve_managed_file_path(&nu_paths)?;
    run_module_lane(
        root,
        &nu_paths,
        &mut lockfile,
        &module_targets,
        &managed_file_path,
        runner,
        None,
    )
}

// ── --list subcommand ──────────────────────────────────────────────────────────

fn execute_list(lockfile: &Lockfile, nu_paths: &NuPaths) -> Result<()> {
    let vendor_dir = nu_paths.vendor_autoload_dir.as_deref().unwrap_or("<none>");
    let managed_path = nu_paths
        .vendor_autoload_dir
        .as_deref()
        .map(|d| format!("{d}/numan.nu"))
        .unwrap_or_default();

    if lockfile.is_empty() {
        println!("No packages installed.");
        return Ok(());
    }

    // Sort packages by ID for deterministic output
    let mut packages: Vec<(&String, &crate::state::lockfile::LockfileEntry)> =
        lockfile.packages.iter().collect();
    packages.sort_by_key(|(id, _)| id.as_str());

    println!(
        "\n{:<32} {:<12} {:<20} {:<10} Target",
        "Package", "Type", "Version", "Status"
    );
    println!("{}", "-".repeat(110));

    for (id, entry) in &packages {
        let status = match entry.package_type.as_str() {
            "plugin" => {
                if entry.is_active_for(
                    &nu_paths.nu_executable_hash,
                    &nu_paths.nu_version,
                    &nu_paths.plugin_registry_path,
                ) {
                    "active"
                } else {
                    "inactive"
                }
            }
            "module" => {
                if entry.is_module_active_for(
                    &nu_paths.nu_executable_hash,
                    &nu_paths.nu_version,
                    vendor_dir,
                    &managed_path,
                ) {
                    "active"
                } else {
                    "inactive"
                }
            }
            _ => "n/a",
        };

        let target = match entry.package_type.as_str() {
            "plugin" => nu_paths.plugin_registry_path.as_str(),
            "module" => vendor_dir,
            _ => "-",
        };

        println!(
            "{:<32} {:<12} {:<20} {:<10} {}",
            id, entry.package_type, entry.version, status, target
        );

        if entry.package_type == "plugin" && entry.activation.is_some() {
            let update_permitted =
                status == "active" && crate::state::active_plugin_mutation::is_enabled();
            let update_note = hints::active_plugin_update_list_note(update_permitted);
            println!("  └─ active plugin: remove gated (deactivate first); {update_note}");
        }
    }
    println!();
    Ok(())
}

// ── --check subcommand ─────────────────────────────────────────────────────────

fn execute_check(
    args: &ActivateArgs,
    lockfile: &Lockfile,
    nu_paths: &NuPaths,
    root: &Path,
) -> Result<()> {
    let vendor_dir = match nu_paths.vendor_autoload_dir.as_deref() {
        Some(d) => d,
        None => {
            bail!(
                "No Numan-safe vendor-autoload directory is available.\n\
                 {}",
                hints::run(CMD_INIT_REFRESH)
            );
        }
    };
    let managed_path_str = format!("{vendor_dir}/numan.nu");
    let managed_path = Path::new(&managed_path_str);

    // Load autoload state for projection check
    let autoload_state = AutoloadState::load(root)?;

    // Pending journal check
    if let Some(journal) = PendingAutoload::load(root)? {
        println!(
            "{} Pending module-autoload journal detected (stage: {:?})",
            console::style("⚠").yellow(),
            journal.stage
        );
    }

    let packages_to_check: Vec<&str> = if args.packages.is_empty() {
        lockfile
            .packages
            .keys()
            .filter(|id| {
                lockfile
                    .packages
                    .get(*id)
                    .map(|e| e.package_type == "module")
                    .unwrap_or(false)
            })
            .map(|s| s.as_str())
            .collect()
    } else {
        args.packages.iter().map(|s| s.as_str()).collect()
    };

    let mut any_issues = false;

    // Check autoload-state vs lockfile projection
    if let Some(state) = &autoload_state {
        if let Err(e) = state.validate_against_lockfile(lockfile) {
            println!(
                "{} Autoload-state projection mismatch: {}",
                console::style("✗").red(),
                e
            );
            any_issues = true;
        } else {
            println!(
                "{} Autoload-state projection matches lockfile",
                console::style("✓").green()
            );
        }
    }

    // Check managed file ownership if it exists
    if managed_path.exists() {
        match crate::util::fs_safety::assert_managed_file_owned(managed_path) {
            Ok(()) => println!(
                "{} Managed file ownership marker valid",
                console::style("✓").green()
            ),
            Err(e) => {
                println!(
                    "{} Managed file ownership check failed: {}",
                    console::style("✗").red(),
                    e
                );
                any_issues = true;
            }
        }
    }

    // Check each module package
    for pkg_id in &packages_to_check {
        let entry = match lockfile.packages.get(*pkg_id) {
            Some(e) => e,
            None => {
                println!(
                    "{} {}: not found in lockfile",
                    console::style("✗").red(),
                    pkg_id
                );
                any_issues = true;
                continue;
            }
        };

        if entry.package_type != "module" {
            println!(
                "  {}: not a module package (type: {}), skipping check",
                pkg_id, entry.package_type
            );
            continue;
        }

        // Check activatable conditions
        if !entry.is_module_activatable() {
            let reason = if entry.module_import_mode.is_none() {
                "missing module_import_mode"
            } else if entry.entry.is_none() {
                "missing entry path"
            } else {
                "has locked dependencies (Phase 4 cannot resolve)"
            };
            println!(
                "{} {}: not activatable — {}",
                console::style("✗").red(),
                pkg_id,
                reason
            );
            any_issues = true;
            continue;
        }

        // Check payload and entry containment
        let entry_relative = entry.entry.as_deref().unwrap_or("");
        match crate::util::fs_safety::is_safe_relative_path(Path::new(entry_relative)) {
            true => {}
            false => {
                println!(
                    "{} {}: entry path '{}' is not a safe relative path",
                    console::style("✗").red(),
                    pkg_id,
                    entry_relative
                );
                any_issues = true;
                continue;
            }
        }

        println!("{} {}: check passed", console::style("✓").green(), pkg_id);
    }

    if any_issues {
        bail!(
            "One or more checks failed. See output above.\n{}",
            hints::run(CMD_ACTIVATE)
        );
    }

    println!("\nAll checks passed.");
    Ok(())
}

// ── Target resolution ──────────────────────────────────────────────────────────

/// Resolve the managed file path from NuPaths.
fn resolve_managed_file_path(nu_paths: &NuPaths) -> Result<String> {
    let vendor_dir = nu_paths.vendor_autoload_dir.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "No Numan-safe vendor-autoload directory is available.\n\
             Module activation requires the vendor-autoload target to be set.\n\
             {}",
            hints::run(CMD_INIT_REFRESH)
        )
    })?;
    Ok(format!("{vendor_dir}/numan.nu"))
}

/// Resolve all plugin targets to activate.
fn resolve_plugin_targets(
    args: &ActivateArgs,
    lockfile: &Lockfile,
    nu_paths: &NuPaths,
    root: &Path,
) -> Result<Vec<PluginTarget>> {
    if args.packages.is_empty() {
        // All installed plugins not yet active for this Nu identity
        let mut targets = Vec::new();
        for (id, entry) in &lockfile.packages {
            if entry.package_type != "plugin" {
                continue;
            }
            if entry.is_active_for(
                &nu_paths.nu_executable_hash,
                &nu_paths.nu_version,
                &nu_paths.plugin_registry_path,
            ) {
                continue;
            }
            let exe = entry
                .executable_path
                .as_deref()
                .with_context(|| format!("{id}: missing executable_path in lockfile"))?;
            let abs = validate_payload_path(root, &entry.payload_path, exe)?;
            targets.push(PluginTarget {
                package_id: id.clone(),
                payload_path: entry.payload_path.clone(),
                executable_path: exe.to_string(),
                absolute_binary_path: abs,
            });
        }
        Ok(targets)
    } else {
        let mut targets = Vec::new();
        for pkg_id in &args.packages {
            let entry = match lockfile.packages.get(pkg_id) {
                Some(e) => e,
                None => {
                    bail!(
                        "Package '{pkg_id}' not found in lockfile (not installed). {}",
                        hints::run(&hints::install_pkg(pkg_id))
                    );
                }
            };

            match entry.package_type.as_str() {
                "plugin" => {}
                "module" => {
                    // Module targets are handled in resolve_module_targets
                    continue;
                }
                "script" => {
                    bail!(
                        "Package '{pkg_id}' is a script — script activation is deferred to a later phase."
                    );
                }
                "completion" => {
                    bail!(
                        "Package '{pkg_id}' is a completion — completion activation is deferred to a later phase."
                    );
                }
                other => {
                    bail!("Package '{pkg_id}' has unknown type '{other}' — cannot activate.");
                }
            }

            if entry.is_active_for(
                &nu_paths.nu_executable_hash,
                &nu_paths.nu_version,
                &nu_paths.plugin_registry_path,
            ) {
                println!(
                    "{} {} is already active for Nu {} (skipped)",
                    console::style("✓").green(),
                    pkg_id,
                    nu_paths.nu_version,
                );
                continue;
            }

            let exe = entry
                .executable_path
                .as_deref()
                .with_context(|| format!("{pkg_id}: missing executable_path in lockfile"))?;
            let abs = validate_payload_path(root, &entry.payload_path, exe)?;
            targets.push(PluginTarget {
                package_id: pkg_id.clone(),
                payload_path: entry.payload_path.clone(),
                executable_path: exe.to_string(),
                absolute_binary_path: abs,
            });
        }
        Ok(targets)
    }
}

/// Resolve all module targets to activate.
fn resolve_module_targets(
    args: &ActivateArgs,
    lockfile: &Lockfile,
    nu_paths: &NuPaths,
) -> Result<Vec<ModuleTarget>> {
    // For module activatability we need the managed file path, but we only
    // need the vendor_autoload_dir for the is_module_active_for check.
    // If there's no vendor_autoload_dir, modules can still be resolved here;
    // the lane function will error with a clear message if needed.
    let vendor_dir = nu_paths.vendor_autoload_dir.as_deref().unwrap_or("");
    let managed_path_str = if vendor_dir.is_empty() {
        String::new()
    } else {
        format!("{vendor_dir}/numan.nu")
    };

    if args.packages.is_empty() {
        // All installed modules not yet active for this Nu identity
        let mut targets = Vec::new();
        for (id, entry) in &lockfile.packages {
            if entry.package_type != "module" {
                continue;
            }
            if entry.is_module_active_for(
                &nu_paths.nu_executable_hash,
                &nu_paths.nu_version,
                vendor_dir,
                &managed_path_str,
            ) {
                continue;
            }
            if !entry.is_module_activatable() {
                eprintln!(
                    "{} Skipping '{}': module is not activatable in Phase 4 \
                     (check entry, import mode, and dependencies)",
                    console::style("⚠").yellow(),
                    id
                );
                continue;
            }
            let entry_relative = entry
                .entry
                .as_deref()
                .with_context(|| format!("{id}: missing entry in lockfile"))?;
            let import_mode = entry
                .module_import_mode
                .clone()
                .unwrap_or(crate::core::package::ModuleImportMode::Module);
            targets.push(ModuleTarget {
                package_id: id.clone(),
                payload_path: entry.payload_path.clone(),
                entry_relative: entry_relative.to_string(),
                import_mode,
            });
        }
        Ok(targets)
    } else {
        let mut targets = Vec::new();
        for pkg_id in &args.packages {
            let entry = match lockfile.packages.get(pkg_id) {
                Some(e) => e,
                None => {
                    // Missing packages are caught by resolve_plugin_targets for plugins;
                    // modules that are missing would only appear here if the explicit
                    // list is processed. We skip non-module types here.
                    continue;
                }
            };

            if entry.package_type != "module" {
                // Non-module explicit packages are handled by resolve_plugin_targets
                // or error there. Skip here.
                continue;
            }

            if !entry.is_module_activatable() {
                bail!(
                    "Package '{pkg_id}' is a module but cannot be activated in Phase 4.\n\
                     Modules with locked dependencies, missing entry paths, or unknown \
                     import modes cannot be activated."
                );
            }

            if entry.is_module_active_for(
                &nu_paths.nu_executable_hash,
                &nu_paths.nu_version,
                vendor_dir,
                &managed_path_str,
            ) {
                println!(
                    "{} {} is already active for Nu {} (skipped)",
                    console::style("✓").green(),
                    pkg_id,
                    nu_paths.nu_version,
                );
                continue;
            }

            let entry_relative = entry
                .entry
                .as_deref()
                .with_context(|| format!("{pkg_id}: missing entry in lockfile"))?;
            let import_mode = entry
                .module_import_mode
                .clone()
                .unwrap_or(crate::core::package::ModuleImportMode::Module);
            targets.push(ModuleTarget {
                package_id: pkg_id.clone(),
                payload_path: entry.payload_path.clone(),
                entry_relative: entry_relative.to_string(),
                import_mode,
            });
        }
        Ok(targets)
    }
}

// ── Journal reconciliation ─────────────────────────────────────────────────────

/// Read-only preflight check: bail if any pending journal has a mismatched Nu
/// identity. Runs before consent table display so the user is warned immediately.
/// Does NOT mutate any state — reconciliation mutations happen inside the lock.
fn check_journals_for_stale_identity(root: &Path, nu_paths: &NuPaths) -> Result<()> {
    if let Some(j) = PendingActivation::load(root)? {
        if !j.matches_nu_identity(
            &nu_paths.nu_executable_hash,
            &nu_paths.nu_version,
            &nu_paths.plugin_registry_path,
        ) {
            bail!(
                "A pending plugin activation journal exists from a different Nu identity.\n\
                 {}\n\
                 Journal: {}",
                hints::run_then(CMD_INIT_REFRESH, CMD_ACTIVATE),
                root.join("state/pending-activation.json").display()
            );
        }
    }
    if let Some(j) = PendingAutoload::load(root)? {
        if !j.matches_nu_identity(&nu_paths.nu_executable_hash, &nu_paths.nu_version) {
            bail!(
                "A pending module-autoload journal exists from a different Nu identity.\n\
                 {}\n\
                 Journal: {}",
                hints::run_then(CMD_INIT_REFRESH, CMD_ACTIVATE),
                root.join("state/pending-autoload.json").display()
            );
        }
    }
    Ok(())
}

/// Reconcile any interrupted plugin activation journal.
fn reconcile_plugin_journal(
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
) -> Result<()> {
    let existing = PendingActivation::load(root)?;
    let Some(existing) = existing else {
        return Ok(());
    };

    if !existing.matches_nu_identity(
        &nu_paths.nu_executable_hash,
        &nu_paths.nu_version,
        &nu_paths.plugin_registry_path,
    ) {
        bail!(
            "A pending plugin activation journal exists from a different Nu identity.\n\
             {}\n\
             Journal: {}",
            hints::run_then(CMD_INIT_REFRESH, CMD_ACTIVATE),
            root.join("state/pending-activation.json").display()
        );
    }
    eprintln!(
        "{}  Reconciling interrupted plugin activation journal…",
        console::style("⚠").yellow()
    );
    for entry in &existing.entries {
        if entry.status == PendingStatus::Registered {
            if let Some(pkg) = lockfile.packages.get_mut(&entry.package_id) {
                pkg.activation = Some(PluginActivation {
                    plugin_registry_path: nu_paths.plugin_registry_path.clone(),
                    nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
                    nu_version: nu_paths.nu_version.clone(),
                    activated_at: format_timestamp(),
                });
            }
        }
    }
    lockfile.save(root)?;
    PendingActivation::delete(root)?;
    eprintln!("   Plugin reconciliation complete.");
    Ok(())
}

// ── Consent table ──────────────────────────────────────────────────────────────

/// Print the grouped consent table showing plugins and modules grouped by operation.
fn print_grouped_consent_table(
    plugins: &[PluginTarget],
    modules: &[ModuleTarget],
    managed_file: Option<&str>,
    registry_path: &str,
) {
    println!();

    if !plugins.is_empty() {
        println!("Plugin registration");
        for t in plugins {
            println!("  {} -> {}", t.package_id, registry_path,);
        }
        println!();
    }

    if !modules.is_empty() {
        let target = managed_file.unwrap_or("<none>");
        println!("Module startup autoload");
        for t in modules {
            println!("  {} -> {}", t.package_id, target);
        }
        println!();
    }
}

// ── Path validation (plugin payloads) ─────────────────────────────────────────

/// Validate that `payload_path` and `executable_path` are safe, then resolve
/// the absolute binary path and verify it exists under `root`.
///
/// All validation happens in Rust before any shell invocation. Paths are never
/// interpolated into command strings — they are passed via environment variables.
pub fn validate_payload_path(
    root: &Path,
    payload_path: &str,
    executable_path: &str,
) -> Result<PathBuf> {
    // payload_path: relative, no traversal
    let pp = Path::new(payload_path);
    if pp.is_absolute() {
        bail!("payload_path is absolute (must be relative): '{payload_path}'");
    }
    for component in pp.components() {
        if component == Component::ParentDir {
            bail!("payload_path contains '..' traversal: '{payload_path}'");
        }
    }

    // executable_path: relative, no traversal
    let ep = Path::new(executable_path);
    if ep.is_absolute() {
        bail!("executable_path is absolute (must be relative): '{executable_path}'");
    }
    for component in ep.components() {
        if component == Component::ParentDir {
            bail!("executable_path contains '..' traversal: '{executable_path}'");
        }
    }

    // Resolve and canonicalize (follows symlinks — catches escape via symlink)
    let candidate = root.join(payload_path).join(executable_path);
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("Binary not found at '{}'", candidate.display()))?;

    // Must stay under root after canonicalization
    let canonical_root = root
        .canonicalize()
        .context("Failed to canonicalize numan root")?;
    if !canonical.starts_with(&canonical_root) {
        bail!(
            "Binary path escapes numan root after canonicalization: '{}'",
            canonical.display()
        );
    }

    // Must be a regular file
    let meta = std::fs::metadata(&canonical)
        .with_context(|| format!("Failed to stat '{}'", canonical.display()))?;
    if !meta.is_file() {
        bail!(
            "Binary path is not a regular file: '{}'",
            canonical.display()
        );
    }

    // Basename must start with nu_plugin_
    let basename = canonical.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let base_stem = basename.trim_end_matches(".exe");
    if !base_stem.starts_with("nu_plugin_") {
        bail!(
            "Binary name must start with 'nu_plugin_' (got '{basename}'). \
             Only Nu plugins can be activated with this command."
        );
    }

    Ok(canonical)
}

/// Invoke Nu with `plugin add` — paths via environment variables only.
pub(crate) fn run_plugin_add(
    nu_executable: &str,
    plugin_binary: &str,
    plugin_config: &str,
) -> Result<()> {
    let output = std::process::Command::new(nu_executable)
        .args(["-c", ADD_PLUGIN])
        .env("NUMAN_PLUGIN_BINARY", plugin_binary)
        .env("NUMAN_PLUGIN_CONFIG", plugin_config)
        .output()
        .with_context(|| format!("Failed to invoke Nu at '{nu_executable}'"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("plugin add failed:\n{stderr}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_payload_path_rejects_absolute_payload() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let err = validate_payload_path(&root, "/absolute/path", "nu_plugin_x").unwrap_err();
        assert!(err.to_string().contains("absolute"));
    }

    #[test]
    fn validate_payload_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let err = validate_payload_path(&root, "packages/../../../etc", "nu_plugin_x").unwrap_err();
        assert!(err.to_string().contains("..") || err.to_string().contains("traversal"));
    }

    #[test]
    fn validate_executable_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("packages")).unwrap();
        let err = validate_payload_path(&root, "packages", "../../../etc/shadow").unwrap_err();
        assert!(err.to_string().contains("..") || err.to_string().contains("traversal"));
    }

    #[test]
    fn validate_payload_path_rejects_absolute_executable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let err = validate_payload_path(
            &root,
            "packages/plugins/x/y/1.0.0-abc",
            "/absolute/nu_plugin_x",
        )
        .unwrap_err();
        assert!(err.to_string().contains("absolute"));
    }

    #[test]
    fn validate_executable_name_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let payload = "packages/plugins/owner/name/1.0.0-abc";
        std::fs::create_dir_all(root.join(payload)).unwrap();
        // File exists but name doesn't start with nu_plugin_
        std::fs::write(root.join(payload).join("wrong_name"), b"fake").unwrap();
        let err = validate_payload_path(&root, payload, "wrong_name").unwrap_err();
        assert!(err.to_string().contains("nu_plugin_"));
    }

    #[test]
    fn validate_binary_path_under_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // Binary exists but outside root
        let outside = tempfile::tempdir().unwrap();
        let ext_binary = outside.path().join("nu_plugin_evil");
        std::fs::write(&ext_binary, b"evil").unwrap();

        // Create a symlink inside root pointing outside
        #[cfg(unix)]
        {
            let payload = "packages/plugins/owner/evil/1.0.0-abc";
            std::fs::create_dir_all(root.join(payload)).unwrap();
            std::os::unix::fs::symlink(&ext_binary, root.join(payload).join("nu_plugin_evil"))
                .unwrap();
            let err = validate_payload_path(&root, payload, "nu_plugin_evil").unwrap_err();
            assert!(
                err.to_string().contains("escapes") || err.to_string().contains("root"),
                "Expected root-escape error, got: {err}"
            );
        }
        #[cfg(not(unix))]
        {
            // On Windows, symlink creation requires elevation — skip symlink test,
            // but verify the binary-not-found path
            let err = validate_payload_path(&root, "packages/plugins/x/y/1.0.0-abc", "nu_plugin_x")
                .unwrap_err();
            assert!(err.to_string().contains("not found") || err.to_string().contains("Binary"));
        }
    }

    #[test]
    fn validate_payload_path_succeeds_for_valid_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let payload = "packages/plugins/owner/test/1.0.0-abc";
        std::fs::create_dir_all(root.join(payload)).unwrap();
        std::fs::write(root.join(payload).join("nu_plugin_test"), b"fake").unwrap();
        let result = validate_payload_path(&root, payload, "nu_plugin_test");
        assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
    }

    #[test]
    fn add_plugin_command_string_contains_env_vars_only() {
        assert!(ADD_PLUGIN.contains("$env.NUMAN_PLUGIN_BINARY"));
        assert!(ADD_PLUGIN.contains("$env.NUMAN_PLUGIN_CONFIG"));
        assert!(
            !ADD_PLUGIN.contains('/'),
            "No literal paths in ADD_PLUGIN command string"
        );
    }

    // ── Module lane planning tests ────────────────────────────────────────────

    #[test]
    fn script_type_fails_with_deferred_error() {
        use crate::state::lockfile::{Lockfile, LockfileEntry};
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut packages: BTreeMap<String, LockfileEntry> = BTreeMap::new();
        packages.insert(
            "owner/myscript".to_string(),
            LockfileEntry {
                version: "1.0.0".to_string(),
                package_type: "script".to_string(),
                source: "archive".to_string(),
                target: None,
                artifact_url: None,
                artifact_sha256: None,
                executable_path: None,
                archive_root: None,
                include: None,
                entry: None,
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
                payload_path: "packages/scripts/owner/myscript/1.0.0-abc".to_string(),
                revision_id: None,
                payload_sha256: None,
                executable_sha256: None,
                selection_reason: None,
                origin: None,
                module_activation: None,
                module_import_mode: None,
                locked_dependencies: BTreeMap::new(),
            },
        );

        let lockfile = Lockfile {
            version: 1,
            generated_at: "ts".to_string(),
            nu_version: "0.113.1".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            packages,
        };

        let fake_nu_paths = crate::nu::paths::NuPaths {
            nu_executable: "/usr/bin/nu".to_string(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: "/path/to/plugins.msgpackz".to_string(),
            nu_executable_hash: "abc123".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![],
            vendor_autoload_dir: None,
        };

        let args = ActivateArgs {
            packages: vec!["owner/myscript".to_string()],

            verbose: false,
            list: false,
            check: false,
        };

        let err = resolve_plugin_targets(&args, &lockfile, &fake_nu_paths, root).unwrap_err();
        assert!(
            err.to_string().contains("script") && err.to_string().contains("deferred"),
            "Expected deferred error for script type, got: {err}"
        );
    }

    #[test]
    fn completion_type_fails_with_deferred_error() {
        use crate::state::lockfile::{Lockfile, LockfileEntry};
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut packages: BTreeMap<String, LockfileEntry> = BTreeMap::new();
        packages.insert(
            "owner/mycomp".to_string(),
            LockfileEntry {
                version: "1.0.0".to_string(),
                package_type: "completion".to_string(),
                source: "archive".to_string(),
                target: None,
                artifact_url: None,
                artifact_sha256: None,
                executable_path: None,
                archive_root: None,
                include: None,
                entry: None,
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
                payload_path: "packages/completions/owner/mycomp/1.0.0-abc".to_string(),
                revision_id: None,
                payload_sha256: None,
                executable_sha256: None,
                selection_reason: None,
                origin: None,
                module_activation: None,
                module_import_mode: None,
                locked_dependencies: BTreeMap::new(),
            },
        );

        let lockfile = Lockfile {
            version: 1,
            generated_at: "ts".to_string(),
            nu_version: "0.113.1".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            packages,
        };

        let fake_nu_paths = crate::nu::paths::NuPaths {
            nu_executable: "/usr/bin/nu".to_string(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: "/path/to/plugins.msgpackz".to_string(),
            nu_executable_hash: "abc123".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![],
            vendor_autoload_dir: None,
        };

        let args = ActivateArgs {
            packages: vec!["owner/mycomp".to_string()],

            verbose: false,
            list: false,
            check: false,
        };

        let err = resolve_plugin_targets(&args, &lockfile, &fake_nu_paths, root).unwrap_err();
        assert!(
            err.to_string().contains("completion") && err.to_string().contains("deferred"),
            "Expected deferred error for completion type, got: {err}"
        );
    }

    #[test]
    fn already_active_module_is_skipped() {
        use crate::core::package::ModuleImportMode;
        use crate::state::lockfile::{Lockfile, LockfileEntry, ModuleActivation};
        use std::collections::BTreeMap;

        let mut packages: BTreeMap<String, LockfileEntry> = BTreeMap::new();
        packages.insert(
            "owner/mymod".to_string(),
            LockfileEntry {
                version: "1.0.0".to_string(),
                package_type: "module".to_string(),
                source: "archive".to_string(),
                target: None,
                artifact_url: None,
                artifact_sha256: None,
                executable_path: None,
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
                payload_path: "packages/modules/owner/mymod/1.0.0-abc".to_string(),
                revision_id: None,
                payload_sha256: None,
                executable_sha256: None,
                selection_reason: None,
                origin: None,
                module_activation: Some(ModuleActivation {
                    entry_path: "/root/packages/modules/owner/mymod/1.0.0-abc/mod.nu".to_string(),
                    import_mode: ModuleImportMode::Module,
                    vendor_autoload_dir: "/nu/vendor/autoload".to_string(),
                    managed_file_path: "/nu/vendor/autoload/numan.nu".to_string(),
                    nu_executable_sha256: "exe-hash".to_string(),
                    nu_version: "0.113.1".to_string(),
                    activated_at: "0".to_string(),
                }),
                module_import_mode: Some(ModuleImportMode::Module),
                locked_dependencies: BTreeMap::new(),
            },
        );

        let lockfile = Lockfile {
            version: 1,
            generated_at: "ts".to_string(),
            nu_version: "0.113.1".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            packages,
        };

        let fake_nu_paths = crate::nu::paths::NuPaths {
            nu_executable: "/usr/bin/nu".to_string(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: "/path/to/plugins.msgpackz".to_string(),
            nu_executable_hash: "exe-hash".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            data_dir: Some("/nu".to_string()),
            vendor_autoload_dirs: vec!["/nu/vendor/autoload".to_string()],
            vendor_autoload_dir: Some("/nu/vendor/autoload".to_string()),
        };

        let args = ActivateArgs {
            packages: vec!["owner/mymod".to_string()],

            verbose: false,
            list: false,
            check: false,
        };

        // Already active module should result in an empty targets list (skipped)
        let targets = resolve_module_targets(&args, &lockfile, &fake_nu_paths).unwrap();
        assert!(
            targets.is_empty(),
            "Active module should be skipped, got: {} targets",
            targets.len()
        );
    }

    #[test]
    fn non_activatable_module_fails_when_explicit() {
        use crate::core::package::ModuleImportMode;
        use crate::state::lockfile::{Lockfile, LockfileEntry};
        use std::collections::BTreeMap;

        let mut packages: BTreeMap<String, LockfileEntry> = BTreeMap::new();
        let mut deps = BTreeMap::new();
        deps.insert("owner/dep".to_string(), "^1.0.0".to_string());
        packages.insert(
            "owner/depmod".to_string(),
            LockfileEntry {
                version: "1.0.0".to_string(),
                package_type: "module".to_string(),
                source: "archive".to_string(),
                target: None,
                artifact_url: None,
                artifact_sha256: None,
                executable_path: None,
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
                payload_path: "packages/modules/owner/depmod/1.0.0-abc".to_string(),
                revision_id: None,
                payload_sha256: None,
                executable_sha256: None,
                selection_reason: None,
                origin: None,
                module_activation: None,
                module_import_mode: Some(ModuleImportMode::Module),
                locked_dependencies: deps, // has dependencies — not activatable
            },
        );

        let lockfile = Lockfile {
            version: 1,
            generated_at: "ts".to_string(),
            nu_version: "0.113.1".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            packages,
        };

        let fake_nu_paths = crate::nu::paths::NuPaths {
            nu_executable: "/usr/bin/nu".to_string(),
            nu_version: "0.113.1".to_string(),
            plugin_registry_path: "/path/to/plugins.msgpackz".to_string(),
            nu_executable_hash: "exe-hash".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            data_dir: None,
            vendor_autoload_dirs: vec![],
            vendor_autoload_dir: None,
        };

        let args = ActivateArgs {
            packages: vec!["owner/depmod".to_string()],

            verbose: false,
            list: false,
            check: false,
        };

        let err = resolve_module_targets(&args, &lockfile, &fake_nu_paths).unwrap_err();
        assert!(
            err.to_string().contains("cannot be activated"),
            "Expected activation error for module with deps, got: {err}"
        );
    }

    #[test]
    fn non_tty_auto_confirms() {
        // With the shared confirm utility, non-TTY sessions auto-confirm
        // instead of bailing. Use the *_with_tty variant so the assertion
        // doesn't depend on the host machine's stdin state.
        let result = crate::util::confirm::confirm_or_auto_with_tty("test?", false, false);
        assert!(result.unwrap(), "non-TTY should auto-confirm");
    }
}
