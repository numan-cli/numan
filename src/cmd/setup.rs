use crate::util::confirm::{confirm_or_bail, require_tty_or_yes};
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use std::path::{Path, PathBuf};

use crate::core::platform::Platform;
use crate::nu::bootstrap::{self, NuSetupOptions};
use crate::nu::paths::{
    find_nu_executable_with_root, find_nu_on_path, probe_nu_config_path, validate_nushell_binary,
};
use crate::nu::version_manager;
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::atomic::write_bytes_atomic;
use crate::util::fs_safety::{
    assert_managed_file_owned, assert_not_symlink, setup_subcommand_lock,
};

/// Snapshot established Numan state before a `setup nu` mutation.
fn snapshot_before_setup_mutation(root: &Path, trigger: SnapshotTrigger) -> Result<()> {
    create_snapshot(root, SnapshotReason::PreMutation, trigger, None, None)
        .context("Failed to create pre-mutation snapshot for `numan setup nu`")?;
    Ok(())
}

/// True when the managed tree holds a real install (legacy binary or at least
/// one versioned package). An empty `tools/nushell/` shell left after a
/// partial migration / deleted binary must not trip the `--force` gate for
/// `setup nu path|use` (including `doctor --fix` off-PATH repair).
fn managed_tree_has_install(root: &Path) -> bool {
    let managed_dir = bootstrap::managed_nu_dir(root);
    if !managed_dir.is_dir() {
        return false;
    }
    let legacy = bootstrap::managed_nu_binary(root);
    if legacy.is_file() {
        return true;
    }
    match version_manager::list_installed_versions(root) {
        Ok(versions) => !versions.is_empty(),
        // Unreadable tree: treat as present so we stay fail-closed on wipe.
        Err(_) => true,
    }
}

const VENDOR_LOADER: &str = include_str!("../../assets/nushell-loader/loader.nu");

const CONFIG_SOURCE_LINE: &str = "source ($nu.config-path | path dirname | path join 'loader.nu')";

const CONFIG_SNIPPET: &str = r#"
# Cached third-party init files (numan setup loader)
source ($nu.config-path | path dirname | path join 'loader.nu')
"#;

#[derive(Debug, Subcommand)]
pub enum SetupCommands {
    /// Download and install the official Nushell release under the Numan root
    Nu(NuSetupArgs),
    /// Setup nushell-loader integration and manage external shell CLI tools
    Loader(LoaderArgs),
}

#[derive(Debug, Args)]
pub struct NuSetupArgs {
    /// Action to perform (default: install)
    #[command(subcommand)]
    pub action: Option<NuAction>,

    /// Nushell version to install (e.g. 0.113.1); omit for latest
    #[arg(value_name = "VERSION")]
    pub version: Option<String>,

    /// Re-download and replace an existing managed Nushell install
    #[arg(long)]
    pub force: bool,

    /// Skip updating the user PATH (Numan still uses the managed binary)
    #[arg(long)]
    pub skip_path: bool,

    /// Skip confirmation prompts
    #[arg(long)]
    pub yes: bool,

    // COMPAT: remove in v0.3.0 — hidden backward-compat flags
    #[arg(long, hide = true)]
    pub remove: bool,
    #[arg(long, hide = true)]
    pub use_path: bool,
    #[arg(long, hide = true, value_name = "PATH")]
    pub use_existing: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum NuAction {
    /// Remove the managed Nushell install and fall back to PATH Nu
    Remove,
    /// Use the Nushell already on PATH (remove managed install, no download)
    Path {
        /// Opt into deleting an existing managed Nushell tree before adopting
        /// PATH Nu. Required when a managed install exists; without it, the
        /// call refuses with a hint (same gate as `setup nu use --force`).
        #[arg(long)]
        force: bool,
    },
    /// Use a specific existing Nushell binary
    Use {
        /// Path to the Nu binary
        path: PathBuf,
        /// Opt into the destructive two-step flow (delete the managed
        /// Nushell install at NUMAN_ROOT, then adopt the user-supplied
        /// binary as the active Nu). Required when a managed install
        /// exists; without it, the call refuses with a hint.
        #[arg(long)]
        force: bool,
    },
}

impl NuSetupArgs {
    /// Construct args for installing a managed Nu (latest or pinned).
    pub fn install(version: Option<String>, force: bool, skip_path: bool, yes: bool) -> Self {
        Self {
            action: None,
            version,
            force,
            skip_path,
            yes,
            remove: false,
            use_path: false,
            use_existing: None,
        }
    }

    /// Construct args for switching to the PATH Nu.
    pub fn use_path(yes: bool) -> Self {
        Self::use_path_with_force(yes, false)
    }

    /// Construct args for switching to the PATH Nu, optionally forcing
    /// replacement of an existing managed tree.
    pub fn use_path_with_force(yes: bool, force: bool) -> Self {
        Self {
            action: Some(NuAction::Path { force }),
            version: None,
            force: false,
            skip_path: false,
            yes,
            remove: false,
            use_path: false,
            use_existing: None,
        }
    }

    /// Construct args for registering a specific existing Nu binary.
    pub fn use_existing(path: PathBuf, yes: bool, force: bool) -> Self {
        Self {
            action: Some(NuAction::Use { path, force }),
            version: None,
            force: false,
            skip_path: false,
            yes,
            remove: false,
            use_path: false,
            use_existing: None,
        }
    }

    /// Test helper: register an existing Nu with explicit `--skip-path` / `--yes`.
    pub fn use_existing_for_test(path: PathBuf, skip_path: bool, yes: bool) -> Self {
        Self {
            action: Some(NuAction::Use { path, force: false }),
            version: None,
            force: false,
            skip_path,
            yes,
            remove: false,
            use_path: false,
            use_existing: None,
        }
    }

    /// Construct args for removing the managed Nu.
    pub fn remove(yes: bool) -> Self {
        Self {
            action: Some(NuAction::Remove),
            version: None,
            force: false,
            skip_path: false,
            yes,
            remove: false,
            use_path: false,
            use_existing: None,
        }
    }
}

#[derive(Debug, Args, Clone, Default)]
pub struct LoaderArgs {
    /// Overwrite an existing loader.nu without prompting
    #[arg(long)]
    pub force: bool,

    /// Append the loader source line to config.nu when it is not already present
    #[arg(long)]
    pub configure: bool,

    /// Skip confirmation prompts
    #[arg(long)]
    pub yes: bool,

    /// Display current status of loader, configured tools, and cache files
    #[arg(long)]
    pub status: bool,

    /// Scan PATH for known CLI tools (starship, zoxide, carapace, etc.) and configure them
    #[arg(long)]
    pub detect: bool,

    /// Add a tool preset (e.g. starship, zoxide) or custom name=command pair
    #[arg(long, value_name = "TOOL")]
    pub add: Option<String>,

    /// Remove a tool from loader configuration and delete its cached autoload file
    #[arg(long, value_name = "TOOL")]
    pub remove: Option<String>,

    /// Invalidate and remove cached tool init files
    #[arg(long)]
    pub clean: bool,

    /// Automatically download and install missing tools from GitHub
    #[arg(long, alias = "install-missing")]
    pub install: bool,
}

pub fn execute(cmd: SetupCommands, root: &Path) -> Result<()> {
    match cmd {
        SetupCommands::Nu(args) => execute_nu(&args, root),
        SetupCommands::Loader(args) => execute_loader(&args, root),
    }
}

pub fn execute_nu(args: &NuSetupArgs, root: &Path) -> Result<()> {
    execute_nu_impl(args, root)
}

/// Doctor repair entry point — same as [`execute_nu`].
pub fn execute_nu_repair(args: &NuSetupArgs, root: &Path) -> Result<()> {
    execute_nu_impl(args, root)
}

/// Short, audit-friendly label describing which destructive setup
/// subcommand is requesting the mutation lock.
fn setup_action_lock_label(args: &NuSetupArgs) -> &'static str {
    // Legacy hidden flags take precedence in `execute_nu_impl` and route
    // to the same leaf helpers as the explicit subcommands. We need the
    // lock label to reflect the *effective* action, not the literal
    // `args.action` field, so peek at the legacy flags first.
    if args.remove {
        return "managed Nushell removal";
    }
    if args.use_path {
        return "PATH Nu registration";
    }
    if args.use_existing.is_some() {
        return "off-path Nu registration";
    }
    match &args.action {
        Some(NuAction::Remove) => "managed Nushell removal",
        Some(NuAction::Path { .. }) => "PATH Nu registration",
        Some(NuAction::Use { .. }) => "off-path Nu registration",
        None => "Nushell install",
    }
}

/// Setup Nu under the root mutation lock. All public callers go through this
/// function; it holds the lock for the full operation.
pub(crate) fn execute_nu_impl(args: &NuSetupArgs, root: &Path) -> Result<()> {
    let what = setup_action_lock_label(args);
    setup_subcommand_lock(root, what, || execute_nu_impl_locked(args, root))
}

fn reject_skip_path_for_off_path_registration(skip_path: bool) -> Result<()> {
    if skip_path {
        bail!(
            "numan setup nu use cannot be combined with --skip-path. \
             Off-PATH registration must persist the binary directory to PATH."
        );
    }
    Ok(())
}

fn execute_nu_impl_locked(args: &NuSetupArgs, root: &Path) -> Result<()> {
    // COMPAT: remove in v0.3.0 — translate hidden legacy flags to subcommands
    if args.remove {
        eprintln!("warning: --remove is deprecated, use 'numan setup nu remove' instead");
        return remove_managed_nu(root, args.yes);
    }
    if args.use_path {
        // Do not pass install-scoped `args.force` into the managed-tree
        // destructive gate. Legacy flags always refuse when a managed tree
        // exists; use the explicit subcommand with `--force` instead.
        eprintln!(
            "warning: --use-path is deprecated, use 'numan setup nu path' instead \
             (and 'numan setup nu path --force' if a managed tree must be replaced; \
             install --force does not authorize managed-tree deletion on this flag)"
        );
        return execute_use_path(args.yes, root, false, ExecuteUseOpts::default());
    }
    if let Some(existing) = &args.use_existing {
        eprintln!(
            "warning: --use-existing is deprecated, use 'numan setup nu use <path>' instead \
             (and 'numan setup nu use <path> --force' if a managed tree must be replaced; \
             install --force does not authorize managed-tree deletion on this flag)"
        );
        reject_skip_path_for_off_path_registration(args.skip_path)?;
        return execute_use_existing(existing, args.yes, root, false, ExecuteUseOpts::default());
    }

    match &args.action {
        Some(NuAction::Remove) => remove_managed_nu(root, args.yes),
        Some(NuAction::Path { force }) => {
            execute_use_path(args.yes, root, *force, ExecuteUseOpts::default())
        }
        Some(NuAction::Use { path, force }) => {
            reject_skip_path_for_off_path_registration(args.skip_path)?;
            execute_use_existing(path, args.yes, root, *force, ExecuteUseOpts::default())
        }
        None => {
            // Default: install (latest or pinned version)
            let options = NuSetupOptions {
                yes: args.yes,
                force: args.force,
                skip_path: args.skip_path,
                version: args.version.clone(),
                caller_consented_destructive: false,
                is_tty: None,
            };
            let platform = Platform::detect();
            bootstrap::execute_nu_setup(root, &platform, &options)?;
            Ok(())
        }
    }
}

/// Test seam for [`execute_use_existing`] / [`execute_use_path`].
///
/// Carry closures that override the production validators so unit tests
/// can mock the binary probe and the destructive-step confirm prompt.
/// Both fields default to `None`, which falls back to the production
/// behavior (`validate_nushell_binary` and
/// `crate::util::confirm::confirm_or_bail`).
#[derive(Default, Copy, Clone)]
pub(crate) struct ExecuteUseOpts<'a> {
    /// Override [`validate_nushell_binary`]. Pass `Some(&stub)` from a
    /// unit test to skip the real-Nu probe.
    #[allow(clippy::type_complexity)]
    pub(crate) validate: Option<&'a dyn Fn(&Path) -> Result<()>>,
    /// Override [`confirm_or_bail`]. Mirrors `confirm_or_bail`'s
    /// signature minus the `yes` flag, which is already bound by the
    /// caller. Tests can capture the prompt text (e.g. to assert it
    /// contains "no undo") and decline by returning `Err`.
    #[allow(clippy::type_complexity)]
    pub(crate) confirm: Option<&'a dyn Fn(&str, &str) -> Result<()>>,
}

/// Validate a user-supplied Nushell binary before destructive operations
/// proceed. The `validate_fn` seam lets unit tests bypass the real Nu
/// probe; production callers should pass `None` to fall through to
/// `validate_nushell_binary` (which runs `nu -c <probe>`).
#[allow(clippy::type_complexity)]
fn validate_user_supplied_nu(
    path: &Path,
    validate_fn: Option<&dyn Fn(&Path) -> Result<()>>,
) -> Result<()> {
    let resolved = path
        .canonicalize()
        .with_context(|| format!("Failed to resolve Nushell binary '{}'", path.display()))?;
    match validate_fn {
        Some(validator) => validator(&resolved),
        None => validate_nushell_binary(&resolved),
    }
    .with_context(|| format!("'{}' is not a runnable Nushell binary", path.display()))?;
    Ok(())
}

fn execute_use_path(yes: bool, root: &Path, force: bool, opts: ExecuteUseOpts<'_>) -> Result<()> {
    // PR #69 WCt: the non-TTY guard must run before any destructive step.
    // `register_existing_nu` mutates the user's PATH and `remove_managed_nu_if_present`
    // deletes the entire managed tree, so refusing the operation on a pipe without
    // `--yes` is non-negotiable.
    if opts.confirm.is_none() {
        require_tty_or_yes(yes, "PATH Nu registration")?;
    }

    let path_nu = find_nu_on_path()?;
    println!("Found Nu on PATH: {path_nu}");

    // Validate before any destructive removal: a broken/unrunnable PATH
    // Nu must not leave us without a managed install.
    validate_user_supplied_nu(Path::new(&path_nu), opts.validate)?;

    // Resolve and normalize the version *before* any destructive step, so a
    // detection failure never leaves the managed tree deleted / PATH mutated.
    let normalized_version = {
        let detected = crate::core::nu_version::NuVersion::from_binary(Path::new(&path_nu))
            .with_context(|| format!("Failed to determine Nu version for '{path_nu}'"))?;
        version_manager::normalize_version(&detected.version)?
    };

    let managed_dir = bootstrap::managed_nu_dir(root);
    // Empty shell dirs / deleted-binary partial state are not "present".
    let managed_dir_was_present = managed_tree_has_install(root);
    if managed_dir_was_present && !force {
        bail!(
            "Refusing `numan setup nu path` while a managed Nushell install at '{}' exists.\n\n\
             The destructive two-step flow (delete the managed tree + adopt PATH Nu) would \
             discard every installed version and the active-version marker. Re-run with \
             `--force` to opt into it, or run `numan setup nu remove` first to stage the \
             removal out-of-band so this subcommand can register PATH Nu without \
             touching managed state.\n\n\
             Both flows are reversible only by `numan setup nu <version>`.",
            managed_dir.display(),
        );
    }
    if managed_dir_was_present {
        let resolved_path_nu = Path::new(&path_nu)
            .canonicalize()
            .with_context(|| format!("Failed to resolve PATH Nu '{}'", path_nu))?;
        let resolved_managed_dir = managed_dir.canonicalize().with_context(|| {
            format!(
                "Failed to resolve managed Nushell directory '{}'",
                managed_dir.display()
            )
        })?;
        if resolved_path_nu.starts_with(&resolved_managed_dir) {
            bail!(
                "PATH Nu resolves to the managed install; install a separate Nu or use `setup nu remove`."
            );
        }

        // Consolidate the destructive-removal confirm + the
        // register_existing_nu PATH-add prompt into one. Without this
        // merge, a user who declines the PATH prompt would already have
        // lost their managed install; with it, the user sees one prompt
        // covering both.
        let nu_parent = resolved_path_nu
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path_nu.clone());
        let prompt = format!(
            "Switching to PATH Nu will remove the managed Nushell install at '{}' \
             (this clears every installed version and the active-version marker; \
             there is no undo) and will add '{}' to your user PATH. Continue?",
            managed_dir.display(),
            nu_parent,
        );
        let cancel_msg = "Switch to PATH Nu cancelled; managed install kept intact.";
        // Gate on `!yes` so `confirm_or_bail`'s yes-skip contract holds
        // when callers inject a confirm seam for telemetry/audit.
        if !yes {
            match opts.confirm {
                Some(confirm_fn) => confirm_fn(&prompt, cancel_msg)?,
                None => confirm_or_bail(&prompt, false, cancel_msg)?,
            }
        }
    }

    // Snapshot true pre-operation state before preflight creates nu_state /
    // probe files or any destructive managed-tree removal.
    snapshot_before_setup_mutation(root, SnapshotTrigger::Update)?;
    preflight_active_marker_writable(root)?;
    remove_managed_nu_if_present(root)?;
    let options = NuSetupOptions {
        yes,
        force: false,
        skip_path: false,
        version: None,
        // Hoist consent so register_existing_nu's inner PATH prompt is
        // suppressed -- only valid because the merged prompt above
        // collected consent for both the delete AND the PATH add.
        caller_consented_destructive: managed_dir_was_present,
        is_tty: None,
    };
    let registered = bootstrap::register_existing_nu(Path::new(&path_nu), &options)?;
    // Persist the registered binary as the active version marker so
    // `numan use list` reports it as the selection.
    version_manager::write_active_version_with_binary(root, &normalized_version, &registered)?;
    Ok(())
}

fn execute_use_existing(
    path: &Path,
    yes: bool,
    root: &Path,
    force: bool,
    opts: ExecuteUseOpts<'_>,
) -> Result<()> {
    // PR #69 WCt: refuse the operation on a non-TTY session without
    // `--yes` *before* any PATH mutation or managed-tree removal.
    if opts.confirm.is_none() {
        require_tty_or_yes(yes, "off-path Nu registration")?;
    }

    // Validate before any destructive removal: an invalid binary must
    // not leave us without a managed install.
    validate_user_supplied_nu(path, opts.validate)?;

    // Consolidate the destructive-removal confirm + the
    // register_existing_nu PATH-add prompt into one (mirrors
    // `execute_use_path`'s gate). With a real managed install in place, the
    // `--force` flag is required to *enter* this path at all — the
    // merged warn-and-confirm below is the second stage of the
    // destructive two-step opt-in. Empty shell dirs / deleted-binary
    // partial state do not count (doctor --fix off-PATH repair).
    let managed_dir = bootstrap::managed_nu_dir(root);
    let managed_dir_was_present = managed_tree_has_install(root);
    if managed_dir_was_present && !force {
        bail!(
            "Refusing `numan setup nu use` while a managed Nushell install at '{}' exists.\n\n\
             The destructive two-step flow (delete the managed tree + adopt '{}') would \
             discard every installed version and the active-version marker. Re-run with \
             `--force` to opt into it, or run `numan setup nu remove` first to stage the \
             removal out-of-band so this subcommand can register the off-path Nu without \
             touching managed state.\n\n\
             Both flows are reversible only by `numan setup nu <version>`.",
            managed_dir.display(),
            path.display(),
        );
    }
    if managed_dir_was_present {
        let resolved_path = std::fs::canonicalize(path)
            .with_context(|| format!("Failed to resolve Nushell binary '{}'", path.display()))?;
        let nu_parent = resolved_path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let prompt = format!(
            "Switching to '{}' will remove the managed Nushell install at '{}' \
             (this clears every installed version and the active-version marker; \
             there is no undo) and will add '{}' to your user PATH. Continue?",
            path.display(),
            managed_dir.display(),
            nu_parent,
        );
        let cancel_msg = "Switch to existing Nushell cancelled; managed install kept intact.";
        if !yes {
            match opts.confirm {
                Some(confirm_fn) => confirm_fn(&prompt, cancel_msg)?,
                None => confirm_or_bail(&prompt, false, cancel_msg)?,
            }
        }
    }

    // Resolve and normalize the version *before* any destructive step, so a
    // detection failure never leaves the managed tree deleted / PATH mutated.
    let normalized_version = {
        let detected = crate::core::nu_version::NuVersion::from_binary(path)
            .with_context(|| format!("Failed to determine Nu version for '{}'", path.display()))?;
        version_manager::normalize_version(&detected.version)?
    };

    // Snapshot true pre-operation state before preflight creates nu_state /
    // probe files or any destructive managed-tree removal.
    snapshot_before_setup_mutation(root, SnapshotTrigger::Update)?;
    preflight_active_marker_writable(root)?;
    remove_managed_nu_if_present(root)?;
    let options = NuSetupOptions {
        yes,
        force: false,
        skip_path: false,
        version: None,
        // Hoist consent so register_existing_nu's inner PATH prompt is
        // suppressed -- only valid because the merged prompt above
        // collected consent for both the delete AND the PATH add.
        caller_consented_destructive: managed_dir_was_present,
        is_tty: None,
    };
    let registered = bootstrap::register_existing_nu(path, &options)?;
    // Persist the registered binary as the active version marker so
    // `numan use list` reports it as the selection.
    version_manager::write_active_version_with_binary(root, &normalized_version, &registered)?;
    Ok(())
}

/// Ensure `nu_state/` is creatable/writable before destructive PATH/off-path
/// registration. A later active-marker write failure after managed-tree
/// deletion + PATH mutation leaves Nu selection incomplete; refuse early.
fn preflight_active_marker_writable(root: &Path) -> Result<()> {
    let nu_state = root.join("nu_state");
    std::fs::create_dir_all(&nu_state).with_context(|| {
        format!(
            "Failed to create nu_state directory '{}' before PATH/off-path Nu registration; \
             refusing destructive switch while the active-version marker cannot be written",
            nu_state.display()
        )
    })?;
    let probe = nu_state.join(".numan-active-marker-write-probe");
    std::fs::write(&probe, b"ok").with_context(|| {
        format!(
            "Failed to write probe file in '{}' before PATH/off-path Nu registration; \
             refusing destructive switch while the active-version marker cannot be written",
            nu_state.display()
        )
    })?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Remove the managed Nushell install, prompting unless `--yes`.
fn remove_managed_nu(root: &Path, yes: bool) -> Result<()> {
    // PR #69 WCt: refuse the operation on a non-TTY session without
    // `--yes` *before* any marker write or directory deletion.
    require_tty_or_yes(yes, "managed Nushell removal")?;

    let managed_dir = bootstrap::managed_nu_dir(root);
    if !managed_dir.is_dir() {
        // No managed tree to delete. Only clear the active-version marker if
        // the currently recorded binary is absent (dangling marker) or lives
        // inside the now-absent managed tree. Preserve valid off-tree
        // selections (e.g. from `numan setup nu use <path>`).
        //
        // Unreadable/malformed markers must not be silently deleted here —
        // `numan doctor` owns that repair (`nu.active_version.invalid`, auto
        // tier with `.corrupt` backup). Surface the condition and leave the
        // file for doctor rather than pre-empting the finding.
        let should_clear = match version_manager::read_active_version(root) {
            Ok(None) => false, // no marker at all
            Ok(Some(active)) => {
                // Clear if the recorded binary is missing or within the managed tree.
                let has_valid_off_tree = active
                    .binary_path
                    .as_ref()
                    .map(|p| std::path::Path::new(p).is_file())
                    .unwrap_or(false);
                !has_valid_off_tree
            }
            Err(e) => {
                return Err(e).context(format!(
                    "Active-version marker is unreadable while no managed Nushell install \
                     exists at '{}'; run `numan doctor --fix` to repair (keeps a .corrupt \
                     backup) instead of discarding diagnostic state",
                    managed_dir.display()
                ));
            }
        };
        if should_clear {
            version_manager::clear_active_version(root).with_context(|| {
                format!(
                    "Failed to clear stale active-version marker (no managed Nushell tree at '{}')",
                    managed_dir.display()
                )
            })?;
        }
        println!(
            "No managed Nushell install found at '{}'.",
            managed_dir.display()
        );
        return Ok(());
    }

    // Fail closed in non-interactive sessions without `--yes`. Removal wipes
    // the managed tree; `confirm_or_bail`'s non-TTY auto-confirm must not
    // silently proceed.
    require_tty_or_yes(yes, "managed Nushell removal")?;
    confirm_or_bail(
        &format!(
            "Remove managed Nushell at '{}'? Numan will fall back to PATH Nu.",
            managed_dir.display()
        ),
        yes,
        "Managed Nushell removal cancelled.",
    )?;

    snapshot_before_setup_mutation(root, SnapshotTrigger::Remove)?;

    // Symlink refusal and delete must succeed before clearing the marker so a
    // rejected managed tree leaves the active selection intact.
    assert_not_symlink(&managed_dir, "managed Nushell directory")?;
    std::fs::remove_dir_all(&managed_dir).with_context(|| {
        format!(
            "Failed to remove managed Nushell directory '{}'",
            managed_dir.display()
        )
    })?;
    version_manager::clear_active_version(root).with_context(|| {
        format!(
            "Failed to clear active-version marker after removing managed Nu at '{}'",
            managed_dir.display()
        )
    })?;
    println!(
        "Removed managed Nushell at '{}'. Run 'numan init --refresh' to re-detect Nu.",
        managed_dir.display()
    );
    Ok(())
}

/// Silently remove the managed Nu directory if it exists (used by
/// `setup nu path` / `setup nu use <path>` when replacing a managed tree).
fn remove_managed_nu_if_present(root: &Path) -> Result<()> {
    let managed_dir = bootstrap::managed_nu_dir(root);
    // Clear the marker immediately before deleting the managed tree (confirm was
    // already handled by the caller). Skip when there is nothing to remove so a
    // no-op path cannot wipe a still-valid off-tree selection. Propagate clear
    // failures so deletion does not proceed with a stale marker.
    if managed_dir.is_dir() {
        // Symlink refusal and delete must succeed before clearing the marker so a
        // rejected managed tree leaves the active selection intact.
        assert_not_symlink(&managed_dir, "managed Nushell directory")?;
        std::fs::remove_dir_all(&managed_dir).with_context(|| {
            format!(
                "Failed to remove managed Nushell directory '{}'",
                managed_dir.display()
            )
        })?;
        version_manager::clear_active_version(root).with_context(|| {
            format!(
                "Failed to clear active-version marker after removing managed Nu at '{}'",
                managed_dir.display()
            )
        })?;
        println!(
            "Removed managed Nushell at '{}' (replaced by registered off-path Nu).",
            managed_dir.display()
        );
    }
    Ok(())
}

pub fn execute_loader(args: &LoaderArgs, root: &Path) -> Result<()> {
    // Public entry holds the root mutation lock (same boundary as setup nu /
    // numan use). The probe helper below stays unlocked so unit tests can
    // inject a fake config path without contending on the advisory lock.
    setup_subcommand_lock(root, "nushell-loader install", || {
        execute_loader_with_probe_and_root(args, Some(root), || {
            let nu_exe = find_nu_executable_with_root(root)?;
            probe_nu_config_path(&nu_exe)
        })
    })
}

/// Install loader.nu using an injected config-path probe.
///
/// Unlocked test seam — production callers must go through [`execute_loader`],
/// which acquires [`setup_subcommand_lock`].
pub fn execute_loader_with_probe<F>(args: &LoaderArgs, probe: F) -> Result<()>
where
    F: FnOnce() -> Result<PathBuf>,
{
    execute_loader_with_probe_and_root(args, None, probe)
}

/// Unlocked test seam supporting root injection.
pub fn execute_loader_with_probe_and_root<F>(
    args: &LoaderArgs,
    root: Option<&Path>,
    probe: F,
) -> Result<()>
where
    F: FnOnce() -> Result<PathBuf>,
{
    let config_path = probe()?;
    let config_dir = config_path
        .parent()
        .context("Nu config path has no parent directory")?;
    let loader_path = config_dir.join("loader.nu");
    let loader_config_path = config_dir.join("loader-config.nu");

    std::fs::create_dir_all(config_dir).with_context(|| {
        format!(
            "Failed to create Nu config directory '{}'",
            config_dir.display()
        )
    })?;

    if args.status {
        return execute_loader_status(&loader_path, &loader_config_path, &config_path, root);
    }

    if args.clean {
        return execute_loader_clean(&loader_config_path, root);
    }

    if let Some(tool_name) = &args.remove {
        return execute_loader_remove(&loader_config_path, tool_name, root);
    }

    // Install/update loader engine file
    install_loader_file(&loader_path, args)?;

    // Ensure loader-config.nu exists
    if !loader_config_path.exists() {
        write_loader_config(&loader_config_path, &[])?;
    }

    let should_install = args.install;

    if let Some(tool_spec) = &args.add {
        execute_loader_add(
            &loader_config_path,
            tool_spec,
            root,
            should_install,
            args.yes,
        )?;
    }

    if args.detect {
        execute_loader_detect(&loader_config_path, root, should_install)?;
    }

    // Configure config.nu if requested
    if args.configure {
        configure_config_nu(&config_path, args)?;
    } else if args.add.is_none() && !args.detect {
        print_manual_snippet(&config_path);
    }

    if args.add.is_none() && !args.detect {
        print_next_steps(&loader_path, &loader_config_path, args.configure);
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderConfigEntry {
    pub name: String,
    pub command: String,
}

pub fn read_loader_config(config_path: &Path) -> Result<Vec<LoaderConfigEntry>> {
    if !config_path.is_file() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read '{}'", config_path.display()))?;
    Ok(parse_loader_config(&content))
}

/// Validate a tool name for use in loader config and autoload file paths.
fn validate_tool_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("Tool name '{name}' must be 1-64 characters.");
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_alphanumeric() {
        bail!("Tool name '{name}' must start with an ASCII letter or digit.");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("Tool name '{name}' may only contain ASCII letters, digits, '-' and '_'.");
    }
    Ok(())
}

/// Reserved tool name that must not be overwritten by loader add/remove.
const RESERVED_LOADER_NAMES: &[&str] = &["numan"];

pub fn parse_loader_config(content: &str) -> Vec<LoaderConfigEntry> {
    let mut entries = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        if let Some(entry) = parse_loader_record_line(trimmed) {
            entries.push(entry);
        }
    }
    entries
}

fn parse_loader_record_line(line: &str) -> Option<LoaderConfigEntry> {
    let name_idx = line.find("name:")?;
    let after_name = &line[name_idx + 5..].trim_start();
    let name_quote = after_name.chars().next()?;
    if name_quote != '\'' && name_quote != '"' {
        return None;
    }

    let mut name = String::new();
    let mut rest = &after_name[name_quote.len_utf8()..];
    loop {
        match rest.chars().next()? {
            '\\' if name_quote == '"' => {
                rest = &rest[1..];
                match rest.chars().next()? {
                    '\\' => {
                        name.push('\\');
                        rest = &rest[1..];
                    }
                    '"' => {
                        name.push('"');
                        rest = &rest[1..];
                    }
                    c => {
                        name.push('\\');
                        name.push(c);
                        rest = &rest[c.len_utf8()..];
                    }
                }
            }
            c if c == name_quote => {
                rest = &rest[name_quote.len_utf8()..];
                break;
            }
            c => {
                name.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }

    let cmd_idx = rest.find("command:")?;
    let after_cmd = &rest[cmd_idx + 8..].trim_start();
    let cmd_quote = after_cmd.chars().next()?;
    if cmd_quote != '\'' && cmd_quote != '"' {
        return None;
    }

    let mut command = String::new();
    let mut cmd_rest = &after_cmd[cmd_quote.len_utf8()..];
    loop {
        match cmd_rest.chars().next()? {
            '\\' if cmd_quote == '"' => {
                cmd_rest = &cmd_rest[1..];
                match cmd_rest.chars().next()? {
                    '\\' => {
                        command.push('\\');
                        cmd_rest = &cmd_rest[1..];
                    }
                    '"' => {
                        command.push('"');
                        cmd_rest = &cmd_rest[1..];
                    }
                    c => {
                        command.push('\\');
                        command.push(c);
                        cmd_rest = &cmd_rest[c.len_utf8()..];
                    }
                }
            }
            c if c == cmd_quote => {
                break;
            }
            c => {
                command.push(c);
                cmd_rest = &cmd_rest[c.len_utf8()..];
            }
        }
    }

    Some(LoaderConfigEntry { name, command })
}

pub fn render_loader_config(entries: &[LoaderConfigEntry]) -> String {
    let mut out = String::from(
        "# Generated by Numan. Tool configurations for nushell-loader.\n# Manage via `numan setup loader --add <tool>` or `numan setup loader --remove <tool>`.\n\n[\n",
    );
    for e in entries {
        let escaped_cmd = e.command.replace('\\', "\\\\").replace('"', "\\\"");
        out.push_str(&format!(
            "  {{ name: '{}', command: \"{}\" }}\n",
            e.name, escaped_cmd
        ));
    }
    out.push_str("]\n");
    out
}

pub fn write_loader_config(config_path: &Path, entries: &[LoaderConfigEntry]) -> Result<()> {
    let rendered = render_loader_config(entries);
    write_bytes_atomic(config_path, rendered.as_bytes())
        .with_context(|| format!("Failed to write '{}'", config_path.display()))?;
    Ok(())
}

fn resolve_vendor_autoload_dir(root: Option<&Path>) -> Option<PathBuf> {
    let r = root?;
    let paths = crate::nu::paths::NuPaths::load(r).ok()?;
    paths.vendor_autoload_dir.map(PathBuf::from)
}

fn execute_loader_status(
    loader_path: &Path,
    loader_config_path: &Path,
    config_path: &Path,
    root: Option<&Path>,
) -> Result<()> {
    println!("Nushell Loader Status:");
    println!("  Engine script: {}", loader_path.display());
    if loader_path.is_file() {
        println!("    Status: installed");
    } else {
        println!("    Status: not installed (run 'numan setup loader')");
    }

    println!("  Config file: {}", loader_config_path.display());
    let entries = read_loader_config(loader_config_path)
        .with_context(|| format!("Failed to parse '{}'", loader_config_path.display()))?;
    println!("  Configured tools ({}):", entries.len());

    let autoload_dir = resolve_vendor_autoload_dir(root);

    for e in &entries {
        let bin_found = crate::cmd::setup_tools::find_binary_on_path(&e.name, root);
        let bin_status = match bin_found {
            Some(p) => format!("found at {}", p.display()),
            None => "missing from PATH".to_string(),
        };

        let cache_status = if let Some(ref ad) = autoload_dir {
            let cache_file = ad.join(format!("{}.nu", e.name));
            if cache_file.is_file() {
                "cached in vendor/autoload"
            } else {
                "not yet cached (will generate on startup)"
            }
        } else {
            "unknown vendor autoload dir"
        };

        println!("    • {}:", e.name);
        println!("        Command: {}", e.command);
        println!("        Binary:  {}", bin_status);
        println!("        Cache:   {}", cache_status);
    }

    if config_path.is_file() {
        let content = std::fs::read_to_string(config_path).unwrap_or_default();
        let sourced = config_already_sources_loader(&content);
        println!(
            "  Sourced in config.nu: {}",
            if sourced {
                "yes"
            } else {
                "no (run 'numan setup loader --configure')"
            }
        );
    }

    Ok(())
}

fn execute_loader_clean(loader_config_path: &Path, root: Option<&Path>) -> Result<()> {
    let entries = read_loader_config(loader_config_path)
        .with_context(|| format!("Failed to parse '{}'", loader_config_path.display()))?;
    let Some(autoload_dir) = resolve_vendor_autoload_dir(root) else {
        println!("Could not determine vendor/autoload directory to clean.");
        return Ok(());
    };

    if !autoload_dir.is_dir() {
        println!(
            "Vendor autoload directory '{}' does not exist.",
            autoload_dir.display()
        );
        return Ok(());
    }

    let mut removed = 0;
    for e in &entries {
        let target = autoload_dir.join(format!("{}.nu", e.name));
        if target.is_file() {
            match std::fs::remove_file(&target) {
                Ok(()) => {
                    println!("Removed cache '{}'", target.display());
                    removed += 1;
                }
                Err(err) => {
                    eprintln!(
                        "Warning: failed to remove cache '{}': {err:#}",
                        target.display()
                    );
                }
            }
        }
    }

    println!("Cleaned {} cached loader file(s).", removed);
    Ok(())
}

fn execute_loader_remove(
    loader_config_path: &Path,
    tool_name: &str,
    root: Option<&Path>,
) -> Result<()> {
    validate_tool_name(tool_name).context("Invalid tool name in --remove")?;
    if RESERVED_LOADER_NAMES.contains(&tool_name) {
        bail!("Tool name '{tool_name}' is reserved and cannot be removed via the loader.");
    }

    let mut entries = read_loader_config(loader_config_path)?;
    let initial_len = entries.len();
    entries.retain(|e| e.name != tool_name);

    if entries.len() == initial_len {
        println!("Tool '{}' is not registered in loader config.", tool_name);
        return Ok(());
    }

    write_loader_config(loader_config_path, &entries)?;
    println!(
        "Removed '{}' from '{}'.",
        tool_name,
        loader_config_path.display()
    );

    if let Some(autoload_dir) = resolve_vendor_autoload_dir(root) {
        let target = autoload_dir.join(format!("{tool_name}.nu"));
        if target.is_file() {
            match std::fs::remove_file(&target) {
                Ok(()) => {
                    println!("Removed cached autoload file '{}'.", target.display());
                }
                Err(err) => {
                    eprintln!(
                        "Warning: failed to remove cached autoload file '{}': {err:#}",
                        target.display()
                    );
                }
            }
        }
    }

    Ok(())
}

fn execute_loader_add(
    loader_config_path: &Path,
    tool_spec: &str,
    root: Option<&Path>,
    should_install: bool,
    yes: bool,
) -> Result<()> {
    let (name, command) = if let Some(preset) = crate::cmd::setup_tools::find_preset(tool_spec) {
        (preset.name.to_string(), preset.init_command.to_string())
    } else if let Some((n, cmd)) = tool_spec.split_once('=') {
        (n.trim().to_string(), cmd.trim().to_string())
    } else {
        bail!(
            "Unknown tool preset '{}'. Use a known preset (starship, zoxide, carapace, atuin, mise, direnv, oh-my-posh) or specify 'name=command'.",
            tool_spec
        );
    };

    validate_tool_name(&name).context("Invalid tool name in --add")?;
    if RESERVED_LOADER_NAMES.contains(&name.as_str()) {
        bail!(
            "Tool name '{}' is reserved and cannot be added via the loader.",
            name
        );
    }

    let bin_found = crate::cmd::setup_tools::find_binary_on_path(&name, root);
    if bin_found.is_none() {
        if let Some(preset) = crate::cmd::setup_tools::find_preset(&name) {
            if should_install {
                if let Some(r) = root {
                    crate::cmd::setup_tools::download_and_install_tool(
                        preset,
                        r,
                        &Platform::detect(),
                    )?;
                } else {
                    println!("Cannot install tool without a Numan root directory.");
                }
            } else if !yes {
                println!(
                    "Notice: '{}' is not currently on your PATH. Pass '--install' to download it automatically.",
                    name
                );
            }
        } else {
            println!("Notice: '{}' binary was not found on PATH.", name);
        }
    }

    let mut entries = read_loader_config(loader_config_path)?;
    if let Some(existing) = entries.iter_mut().find(|e| e.name == name) {
        existing.command = command.clone();
        println!("Updated '{}' command in loader config.", name);
    } else {
        entries.push(LoaderConfigEntry {
            name: name.clone(),
            command: command.clone(),
        });
        println!(
            "Added '{}' (command: \"{}\") to loader config.",
            name, command
        );
    }

    write_loader_config(loader_config_path, &entries)?;
    Ok(())
}

fn execute_loader_detect(
    loader_config_path: &Path,
    root: Option<&Path>,
    should_install: bool,
) -> Result<()> {
    let mut entries = read_loader_config(loader_config_path)?;
    let mut added_count = 0;

    for preset in crate::cmd::setup_tools::KNOWN_TOOLS {
        let is_configured = entries.iter().any(|e| e.name == preset.name);
        let bin_found = crate::cmd::setup_tools::find_binary_on_path(preset.binary_name, root);

        if bin_found.is_some() {
            if !is_configured {
                entries.push(LoaderConfigEntry {
                    name: preset.name.to_string(),
                    command: preset.init_command.to_string(),
                });
                println!(
                    "Detected '{}' on PATH -> added to loader config (command: \"{}\").",
                    preset.display_name, preset.init_command
                );
                added_count += 1;
            }
        } else if should_install && !is_configured {
            if let Some(r) = root {
                println!("Installing missing preset '{}'…", preset.display_name);
                match crate::cmd::setup_tools::download_and_install_tool(
                    preset,
                    r,
                    &Platform::detect(),
                ) {
                    Ok(_) => {
                        entries.push(LoaderConfigEntry {
                            name: preset.name.to_string(),
                            command: preset.init_command.to_string(),
                        });
                        added_count += 1;
                    }
                    Err(err) => {
                        eprintln!(
                            "Warning: failed to install '{}': {err:#}",
                            preset.display_name
                        );
                    }
                }
            } else {
                eprintln!(
                    "Warning: cannot install '{}' without a Numan root directory.",
                    preset.display_name
                );
            }
        }
    }

    if added_count > 0 {
        write_loader_config(loader_config_path, &entries)?;
        println!(
            "Updated '{}' with {} tool(s).",
            loader_config_path.display(),
            added_count
        );
    } else {
        println!("No new tools detected to add.");
    }

    Ok(())
}

fn install_loader_file(loader_path: &Path, args: &LoaderArgs) -> Result<()> {
    if loader_path.exists() && !args.force {
        if !loader_path.is_file() {
            bail!(
                "Refusing to overwrite non-file at '{}'.",
                loader_path.display()
            );
        }

        let existing = std::fs::read_to_string(loader_path).with_context(|| {
            format!(
                "Failed to read existing loader at '{}'",
                loader_path.display()
            )
        })?;
        if existing == VENDOR_LOADER {
            println!(
                "Loader already installed at '{}' (unchanged).",
                loader_path.display()
            );
            return Ok(());
        }

        assert_managed_file_owned(loader_path)?;

        if !args.force {
            confirm_or_bail(
                &format!(
                    "loader.nu already exists at '{}'. Overwrite with the vendored copy?",
                    loader_path.display()
                ),
                args.yes,
                "Loader install cancelled.",
            )?;
        }
    }

    assert_not_symlink(loader_path, "loader.nu")?;

    write_bytes_atomic(loader_path, VENDOR_LOADER.as_bytes()).with_context(|| {
        format!(
            "Failed to write loader script to '{}'",
            loader_path.display()
        )
    })?;

    println!("Installed nushell-loader to '{}'.", loader_path.display());
    Ok(())
}

fn configure_config_nu(config_path: &Path, args: &LoaderArgs) -> Result<()> {
    assert_not_symlink(config_path, "config.nu")?;
    if config_path.exists() && !config_path.is_file() {
        bail!(
            "Refusing to modify non-file config at '{}'.",
            config_path.display()
        );
    }

    if config_path.exists() {
        let content = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read '{}'", config_path.display()))?;
        if config_already_sources_loader(&content) {
            println!(
                "'{}' already sources loader.nu (unchanged).",
                config_path.display()
            );
            return Ok(());
        }

        if !crate::util::confirm::confirm_or_auto(
            &format!("Append loader source line to '{}'?", config_path.display()),
            args.yes,
        )? {
            print_manual_snippet(config_path);
            return Ok(());
        }

        let updated = format!("{}{CONFIG_SNIPPET}", content.trim_end());
        write_bytes_atomic(config_path, updated.as_bytes())
            .with_context(|| format!("Failed to update '{}'", config_path.display()))?;
        println!(
            "Appended loader source line to '{}'.",
            config_path.display()
        );
        return Ok(());
    }

    write_bytes_atomic(
        config_path,
        format!("{CONFIG_SNIPPET}\n").trim_start().as_bytes(),
    )
    .with_context(|| format!("Failed to create '{}'", config_path.display()))?;
    println!(
        "Created '{}' with loader source line.",
        config_path.display()
    );
    Ok(())
}

fn print_manual_snippet(config_path: &Path) {
    println!();
    println!("Add this at the end of '{}':", config_path.display());
    println!("{CONFIG_SNIPPET}");
}

fn print_next_steps(_loader_path: &Path, loader_config_path: &Path, configured: bool) {
    println!();
    println!("Next steps:");
    println!(
        "  1. Configure tools with 'numan setup loader --detect' or 'numan setup loader --add <tool>'.\n     (Configurations are stored in '{}')",
        loader_config_path.display()
    );
    if !configured {
        println!("  2. Source loader.nu from config.nu (see snippet above).");
        println!("  3. Restart Nu. First startup generates caches; later startups are faster.");
    } else {
        println!("  2. Restart Nu. First startup generates caches; later startups are faster.");
    }
    println!();
    println!(
        "Numan module autoloads use the same vendor/autoload directory via numan.nu \
         and are unaffected by loader caches."
    );
    println!("Upstream: https://github.com/aidnem/nushell-loader");
}

pub fn config_already_sources_loader(content: &str) -> bool {
    content.contains(CONFIG_SOURCE_LINE)
        || content.contains("path join 'loader.nu'")
        || content.contains("path join \"loader.nu\"")
        || content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("source ") && trimmed.to_ascii_lowercase().contains("loader.nu")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn config_detection_finds_exact_source_line() {
        let content = format!("export-env {{}}\n{CONFIG_SOURCE_LINE}\n");
        assert!(config_already_sources_loader(&content));
    }

    #[test]
    fn config_detection_finds_literal_loader_source() {
        let content = "source ~/.config/nushell/loader.nu\n";
        assert!(config_already_sources_loader(content));
    }

    #[test]
    fn config_detection_false_when_absent() {
        assert!(!config_already_sources_loader("use std/log\n"));
    }

    #[test]
    fn install_loader_writes_vendored_copy() {
        let dir = TempDir::new().unwrap();
        let loader_path = dir.path().join("loader.nu");
        let args = LoaderArgs {
            force: false,
            configure: false,
            yes: true,
            ..Default::default()
        };

        install_loader_file(&loader_path, &args).unwrap();
        let written = std::fs::read_to_string(&loader_path).unwrap();
        assert_eq!(written, VENDOR_LOADER);
    }

    #[test]
    fn install_loader_skips_when_unchanged() {
        let dir = TempDir::new().unwrap();
        let loader_path = dir.path().join("loader.nu");
        write_bytes_atomic(&loader_path, VENDOR_LOADER.as_bytes()).unwrap();

        let args = LoaderArgs {
            force: false,
            configure: false,
            yes: true,
            ..Default::default()
        };
        install_loader_file(&loader_path, &args).unwrap();
        assert_eq!(
            std::fs::read(&loader_path).unwrap(),
            VENDOR_LOADER.as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn configure_rejects_symlinked_config() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.real.nu");
        std::fs::write(&target, "export-env {}\n").unwrap();
        let config_path = dir.path().join("config.nu");
        symlink(&target, &config_path).unwrap();

        let args = LoaderArgs {
            force: false,
            configure: true,
            yes: true,
            ..Default::default()
        };
        let err = configure_config_nu(&config_path, &args).unwrap_err();
        assert!(err.to_string().contains("symlink"));
    }
    #[test]
    fn configure_appends_snippet_to_existing_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.nu");
        std::fs::write(&config_path, "export-env {}\n").unwrap();

        let args = LoaderArgs {
            force: false,
            configure: true,
            yes: true,
            ..Default::default()
        };
        configure_config_nu(&config_path, &args).unwrap();

        let updated = std::fs::read_to_string(&config_path).unwrap();
        assert!(config_already_sources_loader(&updated));
        assert!(updated.starts_with("export-env {}\n"));
    }

    #[test]
    fn execute_loader_with_probe_installs_next_to_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.nu");
        std::fs::write(&config_path, "export-env {}\n").unwrap();

        let args = LoaderArgs {
            force: false,
            configure: true,
            yes: true,
            ..Default::default()
        };

        execute_loader_with_probe(&args, || Ok(config_path.clone())).unwrap();
        assert!(dir.path().join("loader.nu").is_file());
        let config = std::fs::read_to_string(&config_path).unwrap();
        assert!(config_already_sources_loader(&config));
    }

    #[test]
    fn loader_config_parsing_and_rendering_roundtrips() {
        let entries = vec![
            LoaderConfigEntry {
                name: "starship".to_string(),
                command: "starship init nu".to_string(),
            },
            LoaderConfigEntry {
                name: "zoxide".to_string(),
                command: "zoxide init nushell".to_string(),
            },
            LoaderConfigEntry {
                name: "mytool".to_string(),
                command: r#"echo "hello world""#.to_string(),
            },
            LoaderConfigEntry {
                name: "escaped".to_string(),
                command: r#"run "C:\path\to\app""#.to_string(),
            },
        ];

        let rendered = render_loader_config(&entries);
        let parsed = parse_loader_config(&rendered);
        assert_eq!(entries, parsed);
    }

    #[test]
    fn remove_managed_nu_removes_directory() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let managed_dir = bootstrap::managed_nu_dir(&root);
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("nu.exe"), b"fake").unwrap();

        remove_managed_nu(&root, true).unwrap();
        assert!(!managed_dir.exists());
    }

    #[test]
    fn remove_managed_nu_noop_when_absent() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        // Should succeed without error when nothing is installed.
        remove_managed_nu(&root, true).unwrap();
    }

    #[test]
    fn remove_managed_nu_errors_on_malformed_marker_without_clearing() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let marker = root.join("nu_state").join("active-version.json");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"{ not valid json").unwrap();

        let err = remove_managed_nu(&root, true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unreadable") || msg.contains("Malformed"),
            "expected unreadable/malformed marker diagnostic, got: {msg}"
        );
        assert!(
            msg.contains("doctor") || msg.contains("corrupt"),
            "expected doctor repair hint, got: {msg}"
        );
        assert!(
            marker.is_file(),
            "malformed marker must remain for doctor; remove must not pre-empt the finding"
        );
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            b"{ not valid json",
            "marker bytes must be preserved for doctor .corrupt backup"
        );
    }

    #[test]
    fn remove_managed_nu_clears_dangling_marker_when_tree_absent() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        // Readable marker pointing at a missing on-tree selection — clear it.
        version_manager::write_active_version(&root, "0.99.0").unwrap();
        let marker = root.join("nu_state").join("active-version.json");
        assert!(marker.is_file());

        remove_managed_nu(&root, true).unwrap();
        assert!(
            !marker.exists(),
            "dangling on-tree marker must be cleared when managed tree is absent"
        );
    }

    #[test]
    fn remove_managed_nu_preserves_valid_off_tree_marker() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let off_tree = dir.path().join("external-nu");
        std::fs::write(&off_tree, b"fake-nu").unwrap();
        version_manager::write_active_version_with_binary(&root, "0.99.0", &off_tree).unwrap();
        let marker = root.join("nu_state").join("active-version.json");
        assert!(marker.is_file());

        remove_managed_nu(&root, true).unwrap();
        let active = version_manager::read_active_version(&root)
            .unwrap()
            .expect("valid off-tree selection must be preserved");
        assert_eq!(active.version, "0.99.0");
        assert_eq!(
            active.binary_path.as_deref(),
            Some(off_tree.to_str().unwrap())
        );
    }

    #[test]
    fn remove_managed_nu_if_present_clears_directory() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let managed_dir = bootstrap::managed_nu_dir(&root);
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("nu.exe"), b"fake").unwrap();

        remove_managed_nu_if_present(&root).unwrap();
        assert!(!managed_dir.exists());
    }

    #[test]
    fn remove_managed_nu_if_present_preserves_off_tree_marker_when_absent() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let off_tree = dir.path().join("external-nu");
        std::fs::write(&off_tree, b"fake").unwrap();
        version_manager::write_active_version_with_binary(&root, "0.113.1", &off_tree).unwrap();

        remove_managed_nu_if_present(&root).unwrap();

        let active = version_manager::read_active_version(&root)
            .unwrap()
            .expect("off-tree selection must survive a no-op managed removal");
        assert_eq!(active.version, "0.113.1");
        assert_eq!(
            active.binary_path.as_deref(),
            Some(off_tree.to_string_lossy().as_ref())
        );
    }

    /// Symlink refusal must not clear the active marker (marker is cleared only
    /// after successful delete).
    #[test]
    fn remove_managed_nu_symlink_refusal_preserves_active_marker() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let real_managed = dir.path().join("real-nushell");
        std::fs::create_dir_all(&real_managed).unwrap();
        let bin = if cfg!(windows) { "nu.exe" } else { "nu" };
        std::fs::write(real_managed.join(bin), b"fake").unwrap();

        let tools = root.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        let managed_link = tools.join("nushell");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_managed, &managed_link).unwrap();
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_dir(&real_managed, &managed_link).is_err() {
                return;
            }
        }

        version_manager::write_active_version(&root, "0.113.1").unwrap();

        let err = remove_managed_nu_if_present(&root).expect_err("symlink must refuse");
        let msg = err.to_string();
        assert!(
            msg.contains("symlink") || msg.contains("reparse"),
            "expected symlink/reparse refusal, got: {msg}"
        );
        assert!(
            version_manager::read_active_version(&root)
                .unwrap()
                .is_some(),
            "active marker must survive symlink refusal"
        );
        assert!(
            managed_link.exists(),
            "symlinked managed tree must remain after refusal"
        );
    }

    #[test]
    fn execute_use_existing_invalid_binary_preserves_managed_installation() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let managed_dir = bootstrap::managed_nu_dir(&root);
        std::fs::create_dir_all(&managed_dir).unwrap();
        let marker = managed_dir.join("keep-me");
        std::fs::write(&marker, b"managed").unwrap();

        let missing = dir.path().join("no-such-nu");
        let err = execute_use_existing(&missing, true, &root, false, ExecuteUseOpts::default())
            .unwrap_err();
        assert!(
            err.to_string().contains("Failed to resolve") || err.to_string().contains("no-such-nu"),
            "expected resolve failure, got: {err}"
        );
        assert!(
            marker.exists(),
            "managed Nu must remain intact when off-PATH binary fails validation"
        );
    }

    #[test]
    fn preflight_active_marker_writable_creates_nu_state() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        preflight_active_marker_writable(&root).unwrap();
        assert!(
            root.join("nu_state").is_dir(),
            "preflight must create nu_state so marker write can succeed"
        );
        assert!(
            !root
                .join("nu_state")
                .join(".numan-active-marker-write-probe")
                .exists(),
            "probe file must be cleaned up"
        );
    }

    /// Production PATH/off-path flows call snapshot before preflight so a
    /// preflight failure still leaves a PreMutation snapshot of the true
    /// pre-operation root (without the nu_state/probe side effects).
    #[test]
    fn snapshot_occurs_before_preflight_nu_state_creation() {
        use crate::state::snapshot::list_snapshots;

        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        std::fs::create_dir_all(&root).unwrap();
        // Block preflight's create_dir_all(nu_state) by planting a file.
        std::fs::write(root.join("nu_state"), b"blocked").unwrap();

        snapshot_before_setup_mutation(&root, SnapshotTrigger::Update).unwrap();
        assert!(
            !list_snapshots(&root).unwrap().is_empty(),
            "snapshot must be recorded before preflight runs"
        );

        let err = preflight_active_marker_writable(&root)
            .expect_err("preflight must fail when nu_state is a blocking file");
        let msg = err.to_string();
        assert!(
            msg.contains("nu_state") || msg.contains("Failed"),
            "preflight error should mention nu_state, got: {msg}"
        );
        assert!(
            root.join("nu_state").is_file(),
            "blocked nu_state file must remain (preflight must not replace it)"
        );
        assert!(
            !list_snapshots(&root).unwrap().is_empty(),
            "snapshot from before preflight must remain after preflight failure"
        );
    }

    #[test]
    fn remove_managed_nu_if_present_noop_when_absent() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        remove_managed_nu_if_present(&root).unwrap();
    }

    #[test]
    fn execute_nu_impl_remove_flag_delegates() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let managed_dir = bootstrap::managed_nu_dir(&root);
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("nu.exe"), b"fake").unwrap();

        let args = NuSetupArgs {
            action: None,
            version: None,
            force: false,
            skip_path: false,
            yes: true,
            remove: true,
            use_path: false,
            use_existing: None,
        };
        execute_nu_impl(&args, &root).unwrap();
        assert!(!managed_dir.exists());
    }

    // --- direct unit coverage for the new confirm gate in
    //     `execute_use_existing` ---

    /// Write a fake nu script that returns parseable output for both
    /// the Nu probe (`-c <script>`) and `--version`. Unix only because
    /// Windows binary probes work differently and are tested in
    /// setup_nu_test.rs with a real Nu binary.
    ///
    /// Note: the probe JSON string is built without escaping (no
    /// embedded double-quote chars inside the shell single-quoted
    /// string), so we can store it as a clean raw byte literal.
    #[cfg(unix)]
    fn write_fake_nu(tmp: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = tmp.join("fake-nu");
        let script: &[u8] = b"#!/bin/sh\n\
case $1 in\n\
  --version) printf '0.113.1\n' ;;\n\
  -c) printf '{ \"version\":\"0.113.1\", \"plugin_path\":\"/tmp\", \"data_dir\":\"/tmp\", \"vendor_autoload_dirs\":[\"/tmp/vendor/autoload\"] }\n' ;;\n\
esac\n";
        std::fs::write(&bin, script).expect("write fake-nu script");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake-nu");
        bin
    }

    #[cfg(unix)]
    #[test]
    fn execute_use_existing_passes_with_yes_and_drops_managed() {
        use crate::util::test_paths::PathRestoreGuard;
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        std::fs::create_dir_all(&root).unwrap();

        // Stage a managed binary so the destructive-removal branch fires.
        let managed = root.join("tools").join("nushell").join("nu");
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::write(&managed, b"placeholder managed nu").unwrap();

        let fake_nu = write_fake_nu(dir.path());

        let _path_guard = PathRestoreGuard::new();
        execute_use_existing(&fake_nu, true, &root, true, ExecuteUseOpts::default()).unwrap();

        assert!(
            !managed.is_file(),
            "managed binary at {} must be removed",
            managed.display()
        );
        assert!(
            !root.join("tools/nushell").exists(),
            "tools/nushell tree must be cleaned up"
        );
    }

    #[cfg(unix)]
    #[test]
    fn execute_use_existing_no_prompts_above_when_no_managed_install() {
        use crate::util::test_paths::PathRestoreGuard;
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        std::fs::create_dir_all(&root).unwrap();

        let fake_nu = write_fake_nu(dir.path());

        // No managed tree, so the destructive-removal confirm prompt is
        // gated off (`managed_dir_was_present == false`). End-to-end
        // success via register_existing_nu is the assertion.
        let _path_guard = PathRestoreGuard::new();
        execute_use_existing(&fake_nu, true, &root, false, ExecuteUseOpts::default()).unwrap();
        assert!(
            !root.join("tools/nushell").exists(),
            "no managed tree initially -> nothing to remove"
        );
    }

    /// Registering an off-tree binary persists the `binary_path` field in the
    /// active marker, makes `active_nu_binary` return the off-tree path, and
    /// keeps the version visible in `numan use list`.
    #[cfg(unix)]
    #[test]
    fn execute_use_existing_sets_binary_path_and_off_tree_is_listed() {
        use crate::util::test_paths::PathRestoreGuard;
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        std::fs::create_dir_all(&root).unwrap();

        let fake_nu = write_fake_nu(dir.path());

        let _path_guard = PathRestoreGuard::new();
        execute_use_existing(&fake_nu, true, &root, false, ExecuteUseOpts::default()).unwrap();

        // The active marker must record the off-tree binary path.
        let active = version_manager::read_active_version(&root)
            .unwrap()
            .expect("active marker must be written");
        assert!(
            active.binary_path.is_some(),
            "binary_path must be set for an off-tree registration"
        );
        let stored_path = std::path::Path::new(active.binary_path.as_ref().unwrap());
        assert_eq!(
            stored_path.canonicalize().unwrap(),
            fake_nu.canonicalize().unwrap(),
            "binary_path must point to the registered off-tree binary"
        );

        // `active_nu_binary` must resolve to the off-tree binary.
        let resolved = version_manager::active_nu_binary(&root)
            .unwrap()
            .expect("active_nu_binary must return Some for a valid off-tree marker");
        assert_eq!(
            resolved.canonicalize().unwrap(),
            fake_nu.canonicalize().unwrap(),
            "active_nu_binary must resolve to the off-tree binary"
        );

        // `list_installed_versions` must include the registered version so
        // `numan use list` shows the off-tree binary's version.
        let versions = version_manager::list_installed_versions(&root).unwrap();
        assert!(
            versions.contains(&active.version),
            "off-tree version '{}' must appear in list_installed_versions; got: {versions:?}",
            active.version
        );
    }

    #[cfg(unix)]
    #[test]
    fn execute_use_existing_decline_keeps_managed_intact_and_prompt_says_no_undo() {
        use crate::util::test_paths::PathRestoreGuard;
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        std::fs::create_dir_all(&root).unwrap();

        let managed = root.join("tools").join("nushell").join("nu");
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::write(&managed, b"placeholder managed nu").unwrap();

        let fake_nu = write_fake_nu(dir.path());

        // Mock the production confirm seam. The closure captures the
        // prompt text so the test can assert its literal content, then
        // declines so the destructive removal is short-circuited.
        let captured_prompt = std::sync::Mutex::new(String::new());
        let mock_confirm = |prompt: &str, _cancel_msg: &str| -> anyhow::Result<()> {
            captured_prompt.lock().unwrap().push_str(prompt);
            Err(anyhow::anyhow!("declined in test (mock confirm)"))
        };
        let opts = ExecuteUseOpts {
            validate: None,
            confirm: Some(&mock_confirm),
        };
        let _path_guard = PathRestoreGuard::new();
        let result = execute_use_existing(&fake_nu, false, &root, true, opts);
        let err_msg = match result {
            Ok(()) => panic!("expected Err from declined confirm"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            err_msg.contains("declined in test"),
            "expected declined-by-mock error, got: {err_msg}"
        );

        let prompt = captured_prompt.lock().unwrap().clone();
        assert!(
            prompt.contains("no undo"),
            "merged prompt must contain the literal 'no undo'; got:\n{prompt}"
        );

        assert!(
            managed.is_file(),
            "managed binary at {} must remain intact after decline",
            managed.display()
        );
        assert!(
            root.join("tools/nushell").is_dir(),
            "managed tree must remain intact after decline"
        );
    }
}
