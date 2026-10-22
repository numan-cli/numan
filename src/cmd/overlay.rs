//! Caller-shell overlay transactions. The CLI reserves payloads; only a shell
//! observation can confirm activation. No Rust subprocess mutates Nu scope.

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::core::package::ModuleImportMode;
use crate::nu::autoload::resolve_entry;
use crate::state::lockfile::Lockfile;
use crate::state::overlay_state::{OverlayRecord, OverlayScope, OverlayState, OverlayStatus};
use crate::state::snapshot::{create_snapshot, SnapshotReason, SnapshotTrigger};
use crate::util::format_timestamp;
use crate::util::fs_safety::acquire_mutation_lock;

#[derive(Subcommand, Debug)]
pub enum OverlayCommands {
    /// Print guidance for loading an overlay in the caller shell.
    Use(OverlayUseArgs),
    /// Print guidance for hiding an overlay in the caller shell.
    Hide(OverlayHideArgs),
    /// List last-observed overlays, or this shell's overlays through the Nu wrapper.
    List(SessionArgs),
    /// Reconcile observations from the Nu wrapper, or prune dead sessions.
    Reconcile(SessionArgs),
    #[command(hide = true)]
    Prepare(PrepareArgs),
    #[command(hide = true)]
    PrepareHide(PrepareHideArgs),
    #[command(hide = true)]
    Commit(TransactionArgs),
    #[command(hide = true)]
    Abort(TransactionArgs),
}

#[derive(Args, Debug)]
pub struct OverlayUseArgs {
    pub package: String,
    /// Record the project of origin; does not enable directory discovery.
    #[arg(long)]
    pub project_root: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct OverlayHideArgs {
    pub target: String,
}

#[derive(Args, Debug, Default)]
pub struct SessionArgs {
    #[arg(long, requires = "pid")]
    pub session: Option<String>,
    #[arg(long, requires = "session")]
    pub pid: Option<u32>,
    /// JSON observations: all active overlay names and their Numan transaction markers.
    #[arg(long, default_value = "[]")]
    pub live: String,
}

#[derive(Args, Debug)]
pub struct PrepareArgs {
    #[command(flatten)]
    pub activation: OverlayUseArgs,
    #[command(flatten)]
    pub shell: SessionArgs,
}

#[derive(Args, Debug)]
pub struct PrepareHideArgs {
    pub target: String,
    #[command(flatten)]
    pub shell: SessionArgs,
}

#[derive(Args, Debug)]
pub struct TransactionArgs {
    pub txn: String,
    #[command(flatten)]
    pub shell: SessionArgs,
}

#[derive(Debug, Deserialize)]
struct LiveOverlay {
    name: String,
    txn: Option<String>,
    #[serde(default = "default_active")]
    active: bool,
}

fn default_active() -> bool {
    true
}

struct Observation {
    session: String,
    pid: u32,
    started: u64,
    live: Vec<LiveOverlay>,
}

#[derive(Debug, Serialize)]
struct Prepared {
    txn: String,
    name: String,
    code: String,
    already_active: bool,
}

pub fn execute(command: OverlayCommands, root: &Path) -> Result<()> {
    match command {
        OverlayCommands::Use(args) => {
            let (_, path, name) = resolve_package(root, &args)?;
            eprintln!("Source numan_init.nu (run `numan init --refresh`) and use the Nu-side `numan overlay use` command. The external CLI has not recorded activation.");
            println!(
                "overlay use {} as {}",
                nu_literal(&path)?,
                nu_literal(&name)?
            );
        }
        OverlayCommands::Hide(args) => {
            eprintln!("Use the Nu-side `numan overlay hide` command to confirm this shell's state. No activation record was changed.");
            println!(
                "overlay hide {} --keep-env [PWD]",
                nu_literal(&args.target)?
            );
        }
        OverlayCommands::Prepare(args) => print_json(&prepare(root, &args)?)?,
        OverlayCommands::PrepareHide(args) => print_json(&prepare_hide(root, &args)?)?,
        OverlayCommands::Reconcile(args) => print_json(&reconcile(root, &args, None, false)?)?,
        OverlayCommands::Commit(args) => {
            print_json(&reconcile(root, &args.shell, Some(&args.txn), false)?)?
        }
        OverlayCommands::Abort(args) => {
            print_json(&reconcile(root, &args.shell, Some(&args.txn), true)?)?
        }
        OverlayCommands::List(args) => {
            if args.session.is_some() {
                print_json(&reconcile(root, &args, None, false)?)?;
            } else {
                list_overlays_to(root, &mut std::io::stdout())?;
            }
        }
    }
    Ok(())
}

pub(crate) fn list_overlays_to(root: &Path, out: &mut dyn std::io::Write) -> Result<()> {
    let state = OverlayState::load(root)?;
    if state.overlays.is_empty() {
        writeln!(out, "No overlays recorded.")?;
    }
    for r in state.overlays {
        writeln!(
            out,
            "{} ({}) [{:?}; session {}; last observed]",
            r.overlay_name,
            r.package_id,
            r.status,
            r.session_id.as_deref().unwrap_or("legacy/unconfirmed")
        )?;
    }
    Ok(())
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn process_start(pid: u32) -> Option<u64> {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system
        .process(pid)
        .map(|p| p.start_time())
        .filter(|t| *t != 0)
}

fn observation(args: &SessionArgs) -> Result<Observation> {
    let session = args
        .session
        .as_ref()
        .context("This operation requires the Nu-side session wrapper")?;
    uuid::Uuid::parse_str(session).context("Invalid shell session UUID")?;
    let pid = args.pid.context("Missing shell PID")?;
    let started = process_start(pid)
        .context("Cannot inspect the shell process; refusing to change overlay records")?;
    let live: Vec<LiveOverlay> =
        serde_json::from_str(&args.live).context("Invalid live overlay observations")?;
    let mut names = std::collections::BTreeSet::new();
    for overlay in &live {
        if !names.insert(&overlay.name) {
            bail!("Duplicate live overlay name");
        }
    }
    Ok(Observation {
        session: session.clone(),
        pid,
        started,
        live,
    })
}

fn prune_dead(state: &mut OverlayState) {
    let pids: Vec<Pid> = state
        .overlays
        .iter()
        .filter_map(|r| r.pid.map(Pid::from_u32))
        .collect();
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::nothing(),
    );
    state.overlays.retain(|r| {
        // Legacy records have no confirmed shell and must never be promoted.
        r.pid
            .and_then(|pid| system.process(Pid::from_u32(pid)))
            .is_some_and(|process| {
                process.start_time() == 0
                    || r.process_started
                        .is_none_or(|started| started == process.start_time())
            })
    });
}

fn apply_observation(state: &mut OverlayState, obs: &Observation) -> Result<()> {
    if state.overlays.iter().any(|r| {
        r.session_id.as_deref() == Some(&obs.session)
            && (r.pid != Some(obs.pid) || r.process_started != Some(obs.started))
    }) {
        bail!("Shell session identity is already bound to a different process");
    }
    state.overlays.retain_mut(|r| {
        if r.session_id.as_deref() != Some(&obs.session) {
            return true;
        }
        if r.pid != Some(obs.pid) || r.process_started != Some(obs.started) {
            return false;
        }
        let live = obs.live.iter().find(|live| live.name == r.overlay_name);
        if live.is_some_and(|live| live.active && live.txn.is_some() && live.txn == r.txn) {
            r.status = OverlayStatus::Active;
            true
        } else if r.status == OverlayStatus::Active && live.is_none() {
            false
        } else {
            // Parse/evaluation failure can leave callable commands even when
            // overlay list says inactive. Keep the reservation until shell exit.
            r.status = OverlayStatus::Degraded;
            true
        }
    });
    Ok(())
}

fn save_changed(
    root: &Path,
    before: &OverlayState,
    after: &OverlayState,
    trigger: SnapshotTrigger,
) -> Result<()> {
    if before != after {
        create_snapshot(root, SnapshotReason::PreMutation, trigger, None, None)?;
        after.save(root)?;
    }
    Ok(())
}

fn reconcile(
    root: &Path,
    args: &SessionArgs,
    txn: Option<&str>,
    abort: bool,
) -> Result<Vec<OverlayRecord>> {
    let _lock = acquire_mutation_lock(root)?;
    let before = OverlayState::load(root)?;
    let mut state = before.clone();
    prune_dead(&mut state);
    let obs = if args.session.is_some() {
        Some(observation(args)?)
    } else {
        None
    };
    if txn.is_some() && obs.is_none() {
        bail!("Transaction requires a shell session");
    }
    if let Some(obs) = &obs {
        if let Some(txn) = txn {
            let record = state
                .overlays
                .iter()
                .find(|r| {
                    r.txn.as_deref() == Some(txn) && r.session_id.as_deref() == Some(&obs.session)
                })
                .context("Unknown overlay transaction for this session")?;
            if abort {
                if record.status != OverlayStatus::Pending
                    || obs.live.iter().any(|l| l.txn.as_deref() == Some(txn))
                {
                    bail!("Only an unexecuted pending transaction can be aborted");
                }
                state.overlays.retain(|r| {
                    r.txn.as_deref() != Some(txn) || r.session_id.as_deref() != Some(&obs.session)
                });
            }
        }
        apply_observation(&mut state, obs)?;
    }
    save_changed(root, &before, &state, SnapshotTrigger::Deactivate)?;
    Ok(state
        .overlays
        .into_iter()
        .filter(|r| {
            obs.as_ref()
                .is_none_or(|o| r.session_id.as_deref() == Some(&o.session))
        })
        .collect())
}

fn resolve_package(root: &Path, args: &OverlayUseArgs) -> Result<(String, String, String)> {
    let lockfile = Lockfile::load(root)?;
    let package = lockfile
        .packages
        .get(&args.package)
        .with_context(|| format!("Package '{}' is not installed", args.package))?;
    let spec = package
        .overlay
        .as_ref()
        .context("Package has no overlay metadata")?;
    if package.package_type != "module" || package.module_activation.is_some() {
        bail!("Overlay package must be an inactive module");
    }
    if spec.name.trim().is_empty() || !package.locked_dependencies.is_empty() {
        bail!("Overlay name must be nonempty and dependencies must be resolved before activation");
    }
    let resolved = resolve_entry(
        root,
        &package.payload_path,
        package.entry.as_deref().context("Overlay entry missing")?,
        ModuleImportMode::Module,
        &args.package,
    )?;
    let payload = root.join(&package.payload_path).canonicalize()?;
    Ok((
        payload.to_string_lossy().into_owned(),
        resolved.absolute_path.to_string_lossy().into_owned(),
        spec.name.clone(),
    ))
}

fn prepare(root: &Path, args: &PrepareArgs) -> Result<Prepared> {
    let _lock = acquire_mutation_lock(root)?;
    let obs = observation(&args.shell)?;
    let before = OverlayState::load(root)?;
    let mut state = before.clone();
    prune_dead(&mut state);
    apply_observation(&mut state, &obs)?;
    let (payload_path, entry_path, name) = resolve_package(root, &args.activation)?;
    let requested_project = args
        .activation
        .project_root
        .as_ref()
        .map(|p| p.canonicalize())
        .transpose()?;
    if requested_project.as_ref().is_some_and(|p| !p.is_dir()) {
        bail!("Project root must be a directory");
    }
    let requested_project = requested_project.map(|p| p.to_string_lossy().into_owned());
    if let Some(record) = state.overlays.iter().find(|r| {
        r.session_id.as_deref() == Some(&obs.session) && r.package_id == args.activation.package
    }) {
        if record.status != OverlayStatus::Active
            || record.entry_path != entry_path
            || record.overlay_name != name
            || record.project_root != requested_project
        {
            bail!("Overlay has a pending or degraded transaction. Reconcile it or start a new shell before retrying");
        }
        let prepared = Prepared {
            txn: record.txn.clone().unwrap_or_default(),
            name,
            code: String::new(),
            already_active: true,
        };
        save_changed(root, &before, &state, SnapshotTrigger::Activate)?;
        return Ok(prepared);
    }
    if obs.live.iter().any(|l| l.name == name) {
        bail!("Overlay '{name}' already exists in this shell without matching Numan ownership");
    }
    let lockfile = Lockfile::load(root)?;
    let requested = lockfile
        .packages
        .get(&args.activation.package)
        .unwrap()
        .overlay
        .as_ref()
        .unwrap();
    for r in state
        .overlays
        .iter()
        .filter(|r| r.session_id.as_deref() == Some(&obs.session))
    {
        if r.overlay_name == name {
            bail!("Overlay name '{name}' is already reserved in this shell");
        }
        let existing = lockfile
            .packages
            .get(&r.package_id)
            .and_then(|e| e.overlay.as_ref())
            .context("Cannot validate exports of an existing overlay transaction")?;
        if requested
            .exports
            .iter()
            .any(|e| existing.exports.contains(e))
        {
            bail!("Overlay exports conflict in this shell");
        }
    }
    let txn = uuid::Uuid::new_v4().to_string();
    let marker = format!("__NUMAN_OVERLAY__{}", txn.replace('-', ""));
    let code = format!(
        "overlay use {} as {}\n$env.{marker} = {{txn: {}, name: {}, session: {}}}",
        nu_literal(&entry_path)?,
        nu_literal(&name)?,
        nu_literal(&txn)?,
        nu_literal(&name)?,
        nu_literal(&obs.session)?
    );
    state.overlays.push(OverlayRecord {
        package_id: args.activation.package.clone(),
        overlay_name: name.clone(),
        entry_path,
        scope: if requested_project.is_some() {
            OverlayScope::Project
        } else {
            OverlayScope::Session
        },
        project_root: requested_project,
        activated_at: format_timestamp(),
        session_id: Some(obs.session),
        pid: Some(obs.pid),
        process_started: Some(obs.started),
        txn: Some(txn.clone()),
        status: OverlayStatus::Pending,
        payload_path: Some(payload_path),
    });
    save_changed(root, &before, &state, SnapshotTrigger::Activate)?;
    Ok(Prepared {
        txn,
        name,
        code,
        already_active: false,
    })
}

fn prepare_hide(root: &Path, args: &PrepareHideArgs) -> Result<Prepared> {
    let _lock = acquire_mutation_lock(root)?;
    let obs = observation(&args.shell)?;
    let before = OverlayState::load(root)?;
    let mut state = before.clone();
    prune_dead(&mut state);
    apply_observation(&mut state, &obs)?;
    let record = state.overlays.iter().find(|r| {
        r.session_id.as_deref() == Some(&obs.session)
            && (r.package_id == args.target || r.overlay_name == args.target)
    });
    let Some(record) = record else {
        save_changed(root, &before, &state, SnapshotTrigger::Deactivate)?;
        return Ok(Prepared {
            txn: String::new(),
            name: args.target.clone(),
            code: String::new(),
            already_active: false,
        });
    };
    if record.status != OverlayStatus::Active {
        bail!(
            "Overlay is degraded or pending; start a new shell to clear possible partial Nu state"
        );
    }
    let response = Prepared {
        txn: record.txn.clone().unwrap(),
        name: record.overlay_name.clone(),
        code: format!(
            "overlay hide {} --keep-env [PWD]",
            nu_literal(&record.overlay_name)?
        ),
        already_active: false,
    };
    save_changed(root, &before, &state, SnapshotTrigger::Deactivate)?;
    Ok(response)
}

pub(crate) fn nu_literal(value: &str) -> Result<String> {
    if value.contains('\0') {
        bail!("Nushell literals cannot contain NUL");
    }
    for count in 1.. {
        let delimiter = "#".repeat(count);
        if !value.contains(&format!("'{delimiter}")) {
            return Ok(format!("r{delimiter}'{value}'{delimiter}"));
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("packages/modules/owner/tool/1");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(
            payload.join("overlay.nu"),
            "export def hello [] { 'hello' }",
        )
        .unwrap();
        std::fs::write(dir.path().join("lockfile"), serde_json::to_vec(&serde_json::json!({
            "version":2,"generated_at":"","nu_version":"","platform":"", "packages": {
                "owner/tool": {"version":"1.0.0","type":"module","source":"archive","installed_at":"now",
                    "payload_path":"packages/modules/owner/tool/1","entry":"overlay.nu", "overlay":{"name":"tooling","exports":["hello"]}}
            }
        })).unwrap()).unwrap();
        dir
    }

    fn session() -> SessionArgs {
        SessionArgs {
            session: Some(uuid::Uuid::new_v4().to_string()),
            pid: Some(std::process::id()),
            live: "[]".into(),
        }
    }

    fn args(shell: SessionArgs) -> PrepareArgs {
        PrepareArgs {
            activation: OverlayUseArgs {
                package: "owner/tool".into(),
                project_root: None,
            },
            shell,
        }
    }

    #[test]
    fn prepare_pins_payload_until_shell_confirms_then_native_hide_releases_it() {
        let dir = fixture();
        let mut args = args(session());
        let prepared = prepare(dir.path(), &args).unwrap();
        let state = OverlayState::load(dir.path()).unwrap();
        assert_eq!(state.overlays[0].status, OverlayStatus::Pending);
        assert!(OverlayState::ensure_package_mutable(dir.path(), "owner/tool").is_err());
        args.shell.live = serde_json::json!([{"name":"tooling","txn":prepared.txn}]).to_string();
        let records = reconcile(dir.path(), &args.shell, Some(&prepared.txn), false).unwrap();
        assert_eq!(records[0].status, OverlayStatus::Active);
        assert!(prepare(dir.path(), &args).unwrap().already_active);
        assert_eq!(OverlayState::load(dir.path()).unwrap().overlays.len(), 1);
        args.shell.live = "[]".into();
        reconcile(dir.path(), &args.shell, None, false).unwrap();
        OverlayState::ensure_package_mutable(dir.path(), "owner/tool").unwrap();
    }

    #[test]
    fn failed_execution_is_degraded_but_preflight_abort_removes_pending_record() {
        let dir = fixture();
        let args = args(session());
        let prepared = prepare(dir.path(), &args).unwrap();
        reconcile(dir.path(), &args.shell, Some(&prepared.txn), true).unwrap();
        assert!(OverlayState::load(dir.path()).unwrap().overlays.is_empty());
        prepare(dir.path(), &args).unwrap();
        let records = reconcile(dir.path(), &args.shell, None, false).unwrap();
        assert_eq!(records[0].status, OverlayStatus::Degraded);
        assert!(OverlayState::ensure_package_mutable(dir.path(), "owner/tool").is_err());
    }

    #[test]
    fn same_package_in_two_sessions_is_independent_and_foreign_name_is_refused() {
        let dir = fixture();
        let first = args(session());
        prepare(dir.path(), &first).unwrap();
        let mut second = args(session());
        second.shell.live = r#"[{"name":"tooling","txn":null}]"#.into();
        assert!(prepare(dir.path(), &second)
            .unwrap_err()
            .to_string()
            .contains("ownership"));
        second.shell.live = "[]".into();
        prepare(dir.path(), &second).unwrap();
        assert_eq!(OverlayState::load(dir.path()).unwrap().overlays.len(), 2);
    }

    #[test]
    fn external_use_and_hide_do_not_record_unconfirmed_shell_state() {
        let dir = fixture();
        execute(
            OverlayCommands::Use(OverlayUseArgs {
                package: "owner/tool".into(),
                project_root: None,
            }),
            dir.path(),
        )
        .unwrap();
        execute(
            OverlayCommands::Hide(OverlayHideArgs {
                target: "tooling".into(),
            }),
            dir.path(),
        )
        .unwrap();
        assert!(OverlayState::load(dir.path()).unwrap().overlays.is_empty());
        assert!(!dir.path().join("state/snapshots").exists());
    }

    #[test]
    fn snapshot_rollback_preserves_current_overlay_observations() {
        let dir = fixture();
        let snapshot = create_snapshot(
            dir.path(),
            SnapshotReason::PreMutation,
            SnapshotTrigger::Activate,
            None,
            None,
        )
        .unwrap();
        let args = args(session());
        let prepared = prepare(dir.path(), &args).unwrap();
        crate::state::rollback::rollback_to_snapshot(
            dir.path(),
            &snapshot.id,
            &crate::nu::autoload::FakeCandidateRunner::success(),
        )
        .unwrap();
        let state = OverlayState::load(dir.path()).unwrap();
        assert_eq!(state.overlays.len(), 1);
        assert_eq!(
            state.overlays[0].txn.as_deref(),
            Some(prepared.txn.as_str())
        );
    }

    #[test]
    fn held_mutation_lock_leaves_nothing_reserved() {
        let dir = fixture();
        let _lock = acquire_mutation_lock(dir.path()).unwrap();
        assert!(prepare(dir.path(), &args(session())).is_err());
        assert!(OverlayState::load(dir.path()).unwrap().overlays.is_empty());
    }

    #[test]
    fn surviving_marker_without_active_overlay_keeps_reservation_degraded() {
        let dir = fixture();
        let mut args = args(session());
        let prepared = prepare(dir.path(), &args).unwrap();
        args.shell.live =
            serde_json::json!([{"name":"tooling","txn":prepared.txn,"active":true}]).to_string();
        reconcile(dir.path(), &args.shell, None, false).unwrap();
        args.shell.live =
            serde_json::json!([{"name":"tooling","txn":prepared.txn,"active":false}]).to_string();
        let records = reconcile(dir.path(), &args.shell, None, false).unwrap();
        assert_eq!(records[0].status, OverlayStatus::Degraded);
        assert!(OverlayState::ensure_package_mutable(dir.path(), "owner/tool").is_err());
    }
}
