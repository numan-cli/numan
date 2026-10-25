//! Command-independent recovery for interrupted module-autoload transactions.

use anyhow::{anyhow, bail, Result};
use std::collections::BTreeSet;
use std::path::Path;

use crate::core::package::ModuleImportMode;
use crate::nu::paths::NuPaths;
use crate::state::autoload_journal::{
    sha256_file, AutoloadOperation, AutoloadStage, PendingAutoload, RecoveryAction,
};
use crate::state::autoload_state::AutoloadState;
use crate::state::lockfile::{Lockfile, LockfileEntry, ModuleActivation};
use crate::util::format_timestamp;

/// Result of checking for and reconciling a pending module-autoload journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoloadRecoveryOutcome {
    /// No pending journal exists.
    NoJournal,
    /// A prepared transaction made no external change and was safely abandoned.
    PreparedCleared,
    /// A replaced transaction was completed into the lockfile and derived state.
    ReplacedCompleted,
}

/// Reconcile a pending module-autoload journal into authoritative and derived state.
///
/// Callers must hold the Numan root mutation lock for the duration of this call.
pub fn reconcile_pending_autoload(
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
) -> Result<AutoloadRecoveryOutcome> {
    let Some(journal) = PendingAutoload::load(root)? else {
        return Ok(AutoloadRecoveryOutcome::NoJournal);
    };

    if !journal.matches_nu_identity(&nu_paths.nu_executable_hash, &nu_paths.nu_version) {
        bail!(
            "A pending module-autoload journal exists from a different Nu identity.\n\
             Run 'numan init --refresh' to clear stale state, then retry.\n\
             Journal: {}",
            root.join("state/pending-autoload.json").display()
        );
    }

    match journal.stage {
        AutoloadStage::Prepared => match journal.recover_prepared()? {
            RecoveryAction::AbandonedSafely => {
                PendingAutoload::delete(root)?;
                Ok(AutoloadRecoveryOutcome::PreparedCleared)
            }
            RecoveryAction::DriftDetected { reason } => bail!(
                "Numan managed-file drift detected during module journal recovery.\n\
                 {reason}\n\
                 Resolve the drift manually before proceeding."
            ),
            RecoveryAction::CanComplete => {
                bail!("Prepared autoload recovery returned an invalid completion action")
            }
        },
        AutoloadStage::Replaced => match journal.recover_replaced()? {
            RecoveryAction::CanComplete => {
                apply_replaced_transition(root, nu_paths, lockfile, &journal)?;
                PendingAutoload::delete(root)?;
                Ok(AutoloadRecoveryOutcome::ReplacedCompleted)
            }
            RecoveryAction::DriftDetected { reason } => bail!(
                "Cannot complete module-autoload journal recovery — drift detected.\n\
                 {reason}\n\
                 Preserve the journal and investigate manually."
            ),
            RecoveryAction::AbandonedSafely => {
                bail!("Replaced autoload recovery returned an invalid abandon action")
            }
        },
    }
}

fn apply_replaced_transition(
    root: &Path,
    nu_paths: &NuPaths,
    lockfile: &mut Lockfile,
    journal: &PendingAutoload,
) -> Result<()> {
    if !journal.desired_file_exists && !journal.desired_active_module_ids.is_empty() {
        bail!(
            "Invalid module-autoload journal: a deleted managed file cannot declare active modules"
        );
    }

    let desired: BTreeSet<&str> = journal
        .desired_active_module_ids
        .iter()
        .map(String::as_str)
        .collect();
    let activated_at = format_timestamp();

    for package_id in &journal.desired_active_module_ids {
        let entry = lockfile.packages.get_mut(package_id).ok_or_else(|| {
            anyhow!("Cannot recover module '{package_id}': package is missing from lockfile")
        })?;
        if entry.package_type == "script" {
            // Script activation record (issue #14): rebuild from the
            // persisted declaration; targets resolve under the payload.
            if entry.script_activation.is_none() {
                let declared = entry.script_entry_points.clone().unwrap_or_default();
                let mut commands = Vec::new();
                for ep in &declared {
                    let abs = root.join(&entry.payload_path).join(&ep.path);
                    let script_path = abs
                        .canonicalize()
                        .unwrap_or(abs)
                        .to_string_lossy()
                        .into_owned();
                    commands.push(crate::state::lockfile::ScriptActivationCommand {
                        name: ep.name.clone(),
                        path: ep.path.clone(),
                        script_path,
                    });
                }
                entry.script_activation = Some(crate::state::lockfile::ScriptActivation {
                    commands,
                    vendor_autoload_dir: journal.vendor_autoload_dir.clone(),
                    managed_file_path: journal.managed_file_path.clone(),
                    nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
                    nu_version: nu_paths.nu_version.clone(),
                    activated_at: activated_at.clone(),
                });
            }
            continue;
        }
        if entry.package_type != "module" {
            bail!(
                "Cannot recover module '{}': lockfile type is '{}'",
                package_id,
                entry.package_type
            );
        }

        let entry_path = match &entry.module_activation {
            Some(existing) => existing.entry_path.clone(),
            None => reconstruct_entry_path(root, package_id, entry)?,
        };
        let import_mode = entry
            .module_import_mode
            .clone()
            .unwrap_or(ModuleImportMode::Module);
        entry.module_activation = Some(ModuleActivation {
            entry_path,
            import_mode,
            vendor_autoload_dir: journal.vendor_autoload_dir.clone(),
            managed_file_path: journal.managed_file_path.clone(),
            nu_executable_sha256: nu_paths.nu_executable_hash.clone(),
            nu_version: nu_paths.nu_version.clone(),
            activated_at: activated_at.clone(),
        });
    }

    for package_id in &journal.previous_active_module_ids {
        if desired.contains(package_id.as_str()) {
            continue;
        }
        let Some(entry) = lockfile.packages.get_mut(package_id) else {
            continue;
        };
        let should_clear = entry
            .module_activation
            .as_ref()
            .is_some_and(|activation| activation_matches_journal(activation, journal));
        if should_clear {
            entry.module_activation = None;
        }
        let should_clear_script = entry.script_activation.as_ref().is_some_and(|a| {
            a.nu_executable_sha256 == journal.nu_executable_sha256
                && a.nu_version == journal.nu_version
                && a.vendor_autoload_dir == journal.vendor_autoload_dir
                && a.managed_file_path == journal.managed_file_path
        });
        if should_clear_script {
            entry.script_activation = None;
        }
    }

    if journal.operation == AutoloadOperation::Deactivate {
        let previous: BTreeSet<&str> = journal
            .previous_active_module_ids
            .iter()
            .map(String::as_str)
            .collect();
        for package_id in &journal.targeted_module_ids {
            if previous.contains(package_id.as_str()) || desired.contains(package_id.as_str()) {
                continue;
            }
            if let Some(entry) = lockfile.packages.get_mut(package_id) {
                entry.module_activation = None;
                entry.script_activation = None;
            }
        }
    }

    lockfile.save(root)?;

    if journal.desired_file_exists {
        let managed_path = Path::new(&journal.managed_file_path);
        let state = AutoloadState::new(
            journal.vendor_autoload_dir.clone(),
            journal.managed_file_path.clone(),
            nu_paths.nu_executable_hash.clone(),
            nu_paths.nu_version.clone(),
            sha256_file(managed_path)?,
            journal.desired_active_module_ids.clone(),
            activated_at,
        );
        state.save(root)?;
    } else {
        AutoloadState::delete(root)?;
    }

    Ok(())
}

fn reconstruct_entry_path(root: &Path, package_id: &str, entry: &LockfileEntry) -> Result<String> {
    let relative_entry = entry.entry.as_deref().ok_or_else(|| {
        anyhow!("Cannot recover module '{package_id}': entry is not set in lockfile")
    })?;
    if entry.payload_path.is_empty() {
        bail!("Cannot recover module '{package_id}': payload_path is not set in lockfile");
    }
    root.join(&entry.payload_path)
        .join(relative_entry)
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow!("Cannot recover module '{package_id}': entry path is not valid UTF-8")
        })
}

fn activation_matches_journal(activation: &ModuleActivation, journal: &PendingAutoload) -> bool {
    activation.nu_executable_sha256 == journal.nu_executable_sha256
        && activation.nu_version == journal.nu_version
        && activation.vendor_autoload_dir == journal.vendor_autoload_dir
        && activation.managed_file_path == journal.managed_file_path
}
