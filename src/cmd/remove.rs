use anyhow::{bail, Context, Result};
use clap::Parser;
use std::io::IsTerminal;
use std::path::Path;

use crate::state::lifecycle_journal::{LifecycleOp, LifecycleStage, PendingLifecycle};
use crate::state::lockfile::{Lockfile, LockfileEntry, BUNDLED_NU_ORIGIN};
use crate::state::nupm_import::NupmImportsFile;
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::fs_safety::acquire_mutation_lock;
use crate::util::hints;

/// Remove an installed package
#[derive(Parser)]
pub struct RemoveArgs {
    /// Package to remove (owner/name)
    package: String,

    /// Skip interactive confirmation (required in non-interactive sessions)
    #[arg(long)]
    yes: bool,

    /// Remove even if the package has an active *module* activation record (does not bypass active plugin activation; see Issue #22)
    #[arg(long)]
    force: bool,
}

pub fn execute(args: &RemoveArgs, root: &Path) -> Result<()> {
    execute_with_tty(args, root, std::io::stdin().is_terminal())
}

/// Same as [`execute`] with an injectable terminal-status seam for tests.
fn execute_with_tty(args: &RemoveArgs, root: &Path, is_tty: bool) -> Result<()> {
    // Destructive: permanently deletes the package payload and lockfile entry.
    // Refuse unattended (non-TTY) sessions without explicit --yes so safe-batch
    // automation has to opt in; interactive TTY sessions still confirm below.
    crate::util::confirm::require_tty_or_yes_with_seam(args.yes, "package removal", is_tty)?;
    crate::util::confirm::confirm_or_bail(
        &format!(
            "Remove package '{}' ? This deletes its payload.",
            args.package
        ),
        args.yes,
        "Cancelled.",
    )?;

    // Validate before taking the mutation lock so a typo'd package id fails
    // fast, and so an idle interactive prompt does not block other destructive
    // ops on the same root (mirrors snapshot delete/rollback ordering).
    let lockfile = Lockfile::load(root)?;

    let entry = match lockfile.packages.get(&args.package) {
        Some(e) => e.clone(),
        None => bail!("Package '{}' is not installed.", args.package),
    };

    ensure_plugin_not_active(&entry, &args.package)?;
    ensure_not_bundled_plugin(&entry, &args.package)?;
    if !args.force && entry.module_activation.is_some() {
        bail!(
            "Package '{}' is currently active as a module. \
             Run `numan deactivate {}` first or use --force.",
            args.package,
            args.package
        );
    }

    // Interactive confirmation after validation so a typo'd package id fails
    // fast, and so `--yes` truly means "skip confirmation" rather than only
    // the non-TTY gate.
    crate::util::confirm::confirm_or_bail(
        &format!(
            "Remove package '{}' (payload will be deleted permanently)?",
            args.package
        ),
        args.yes,
        "Package removal cancelled.",
    )?;

    let _lock = acquire_mutation_lock(root)?;

    // Reload under the lock so the confirm-time view cannot race a concurrent
    // install/activate that landed while the user was at the prompt.
    let mut lockfile = Lockfile::load(root)?;
    let entry = match lockfile.packages.get(&args.package) {
        Some(e) => e.clone(),
        None => bail!(
            "Package '{}' is no longer installed (removed while confirmation was pending).",
            args.package
        ),
    };
    ensure_plugin_not_active(&entry, &args.package)?;
    ensure_not_bundled_plugin(&entry, &args.package)?;
    if !args.force && entry.module_activation.is_some() {
        bail!(
            "Package '{}' is currently active as a module. \
             Run `numan deactivate {}` first or use --force.",
            args.package,
            args.package
        );
    }

    let payload_path = entry.payload_path().to_string();
    let payload_dir = root.join(&payload_path);

    // Snapshot current state before any mutation or journal write so the
    // pre-remove activation graph is recoverable via `numan snapshot rollback`.
    create_snapshot(
        root,
        SnapshotReason::PreMutation,
        SnapshotTrigger::Remove,
        None,
        None,
    )?;

    // Write lifecycle journal before any mutation so a crash is detectable.
    let journal = PendingLifecycle {
        op: LifecycleOp::Remove,
        package_id: args.package.clone(),
        stage: LifecycleStage::Prepared,
        orphan_payload_path: Some(payload_path.clone()),
        from_version: None,
        to_version: None,
        nupm_source_path: None,
        nupm_metadata_sha256: None,
        staging_dir: None,
        promoted_payload_path: None,
        batch_package_ids: Vec::new(),
        batch_staging_dirs: Vec::new(),
        target_snapshot_id: None,
        pre_rollback_snapshot_id: None,
        needs_reactivate: false,
    };
    journal.save(root)?;

    // Clear desire before any destructive change so a profile-write failure
    // aborts the remove while the lockfile and payload are still intact and
    // the user can retry. A leftover profile entry would make later
    // `numan use` restore attempts target a removed package.
    crate::state::activation_profile::remove_from_all_minors(root, &args.package).with_context(
        || {
            format!(
                "Failed to clear activation profile entries for '{}'",
                args.package
            )
        },
    )?;

    // Remove from lockfile (atomic write).
    lockfile.packages.remove(&args.package);
    lockfile.save(root)?;

    let mut imports = NupmImportsFile::load(root)?;
    if imports.remove(&args.package) {
        imports.save(root)?;
    }

    // Advance journal so a crash here is recoverable: lockfile is already
    // updated; the payload dir is the only thing left to clean.
    PendingLifecycle {
        stage: LifecycleStage::LockfileUpdated,
        ..journal
    }
    .save(root)?;

    // Delete payload directory. A failure here is non-fatal: `numan gc` will
    // clean up the orphaned directory on the next run.
    if payload_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&payload_dir)
            .with_context(|| format!("Failed to delete {}", payload_dir.display()))
        {
            eprintln!("Warning: {e}");
            eprintln!("Run `numan gc` to finish cleanup.");
        }
    }

    PendingLifecycle::clear(root)?;
    println!("{} Removed {}", console::style("✓").green(), args.package);

    Ok(())
}

/// Refuse remove when a plugin activation record is present (Issue #22 gate).
/// `--force` does not bypass this check.
fn ensure_plugin_not_active(entry: &LockfileEntry, pkg_id: &str) -> Result<()> {
    if entry.activation.is_some() {
        bail!("{}", hints::active_plugin_mutation_gated(pkg_id));
    }
    Ok(())
}

/// Refuse remove when the entry is a bundled-Nu plugin whose payload directory
/// is shared with the managed Nu install (data-loss guard; `remove_dir_all`
/// would wipe the whole `tools/nushell/<version>/` tree). `--force` does not
/// bypass this check.
fn ensure_not_bundled_plugin(entry: &LockfileEntry, pkg_id: &str) -> Result<()> {
    if entry.origin.as_deref() == Some(BUNDLED_NU_ORIGIN) {
        bail!("{}", hints::bundled_plugin_remove_gated(pkg_id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::package::ModuleImportMode;
    use crate::state::lockfile::{LockfileEntry, ModuleActivation, PluginActivation};
    use std::collections::BTreeMap;

    fn base_entry() -> LockfileEntry {
        LockfileEntry {
            version: "1.0.0".to_string(),
            package_type: "plugin".to_string(),
            source: "binary".to_string(),
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
            payload_path: String::new(),
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

    fn plugin_activation() -> PluginActivation {
        PluginActivation {
            plugin_registry_path: "/tmp/plugins.nu".to_string(),
            nu_executable_sha256: "abc".to_string(),
            nu_version: "0.95.0".to_string(),
            activated_at: "0".to_string(),
        }
    }

    fn module_activation() -> ModuleActivation {
        ModuleActivation {
            entry_path: "/tmp/mod.nu".to_string(),
            import_mode: ModuleImportMode::Module,
            vendor_autoload_dir: "/tmp/vendor".to_string(),
            managed_file_path: "/tmp/vendor/numan.nu".to_string(),
            nu_executable_sha256: "abc".to_string(),
            nu_version: "0.95.0".to_string(),
            activated_at: "0".to_string(),
        }
    }

    /// Mirrors the execute() guard order: plugin gate always, module only without --force.
    fn ensure_removable(entry: &LockfileEntry, pkg_id: &str, force: bool) -> Result<()> {
        ensure_plugin_not_active(entry, pkg_id)?;
        if !force && entry.module_activation.is_some() {
            bail!(
                "Package '{pkg_id}' is currently active as a module. \
                 Run `numan deactivate {pkg_id}` first or use --force."
            );
        }
        Ok(())
    }

    #[test]
    fn ensure_plugin_not_active_rejects_plugin_activation() {
        let entry = LockfileEntry {
            activation: Some(plugin_activation()),
            ..base_entry()
        };
        let err = ensure_plugin_not_active(&entry, "owner/pkg").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("owner/pkg"));
        assert!(msg.contains("Issue #22"));
    }

    #[test]
    fn ensure_not_bundled_plugin_refuses_bundled_origin() {
        let entry = LockfileEntry {
            origin: Some(BUNDLED_NU_ORIGIN.to_string()),
            ..base_entry()
        };
        let err = ensure_not_bundled_plugin(&entry, "nushell/polars").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nushell/polars"));
        assert!(msg.contains("bundled Nushell plugin"));
        assert!(msg.contains("numan setup nu remove"));
    }

    #[test]
    fn ensure_not_bundled_plugin_allows_registry_origin() {
        let entry = LockfileEntry {
            origin: Some("registry:official".to_string()),
            ..base_entry()
        };
        ensure_not_bundled_plugin(&entry, "owner/pkg").unwrap();
    }

    #[test]
    fn execute_refuses_bundled_plugin_without_touching_payload() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // A bundled plugin entry shares the versioned Nu payload directory with
        // the nu binary itself and every other bundled plugin.
        let version_dir = root.join("tools/nushell/0.114.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("nu"), b"fake nu binary").unwrap();
        std::fs::write(version_dir.join("nu_plugin_polars"), b"fake polars").unwrap();

        let mut lockfile = Lockfile::empty();
        let mut entry = base_entry();
        entry.version = "0.114.0".to_string();
        entry.executable_path = Some("nu_plugin_polars".to_string());
        entry.payload_path = "tools/nushell/0.114.0".to_string();
        entry.origin = Some(BUNDLED_NU_ORIGIN.to_string());
        lockfile
            .packages
            .insert("nushell/polars".to_string(), entry);
        lockfile.save(root).unwrap();

        // Even --force must not bypass the bundled guard: remove_dir_all on the
        // shared payload would destroy the entire managed Nu install.
        let err = execute_with_tty(
            &RemoveArgs {
                package: "nushell/polars".to_string(),
                yes: true,
                force: true,
            },
            root,
            false,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bundled Nushell plugin"));
        assert!(msg.contains("numan setup nu remove"));

        // The shared payload (nu binary + bundled plugin) and lockfile entry
        // must be untouched.
        assert!(version_dir.join("nu").is_file());
        assert!(version_dir.join("nu_plugin_polars").is_file());
        let reloaded = Lockfile::load(root).unwrap();
        assert!(reloaded.packages.contains_key("nushell/polars"));
    }

    #[test]
    fn refuse_active_plugin_without_force() {
        let entry = LockfileEntry {
            activation: Some(plugin_activation()),
            ..base_entry()
        };
        let err = ensure_removable(&entry, "owner/pkg", false).unwrap_err();
        assert!(err.to_string().contains("Issue #22"));
    }

    #[test]
    fn refuse_active_plugin_even_with_force() {
        let entry = LockfileEntry {
            activation: Some(plugin_activation()),
            ..base_entry()
        };
        let err = ensure_removable(&entry, "owner/pkg", true).unwrap_err();
        assert!(err.to_string().contains("Issue #22"));
        assert!(err.to_string().contains("activation record"));
        assert!(err.to_string().contains("deactivate"));
    }

    #[test]
    fn refuse_active_module_without_force() {
        let entry = LockfileEntry {
            package_type: "module".to_string(),
            module_activation: Some(module_activation()),
            ..base_entry()
        };
        let err = ensure_removable(&entry, "owner/mod", false).unwrap_err();
        assert!(err.to_string().contains("active as a module"));
    }

    #[test]
    fn allow_active_module_with_force() {
        let entry = LockfileEntry {
            package_type: "module".to_string(),
            module_activation: Some(module_activation()),
            ..base_entry()
        };
        ensure_removable(&entry, "owner/mod", true).unwrap();
    }

    #[test]
    fn execute_refuses_non_tty_without_yes() {
        // Force non-TTY via the injectable seam so the guard is deterministic
        // regardless of process stdin terminal status.
        let dir = tempfile::tempdir().unwrap();
        let err = execute_with_tty(
            &RemoveArgs {
                package: "owner/pkg".to_string(),
                yes: false,
                force: false,
            },
            dir.path(),
            false,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(
                "Refusing destructive package removal in non-interactive session without --yes."
            ),
            "guard bail must be the audit contract, got: {msg}"
        );
    }

    #[test]
    fn execute_bypasses_guard_with_explicit_yes() {
        let dir = tempfile::tempdir().unwrap();
        // --yes must get past the destructive guard regardless of TTY; the
        // downstream "not installed" bail proves the guard was the only blocker.
        // Force non-TTY so this never depends on process stdin terminal status.
        let err = execute_with_tty(
            &RemoveArgs {
                package: "owner/pkg".to_string(),
                yes: true,
                force: false,
            },
            dir.path(),
            false,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("is not installed"),
            "expected downstream bail, got: {msg}"
        );
        assert!(
            !msg.contains("Refusing destructive"),
            "--yes must bypass the guard: {msg}"
        );
    }

    #[test]
    fn execute_removes_installed_package_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("owner/pkg")).unwrap();

        let mut lockfile = Lockfile::empty();
        let mut entry = base_entry();
        entry.payload_path = "owner/pkg".to_string();
        lockfile.packages.insert("owner/pkg".to_string(), entry);
        lockfile.save(root).unwrap();

        execute_with_tty(
            &RemoveArgs {
                package: "owner/pkg".to_string(),
                yes: true,
                force: false,
            },
            root,
            false,
        )
        .unwrap();

        let reloaded = Lockfile::load(root).unwrap();
        assert!(!reloaded.packages.contains_key("owner/pkg"));
        assert!(!root.join("owner/pkg").exists());
    }
}
