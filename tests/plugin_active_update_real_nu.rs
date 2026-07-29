//! Real-Nu active-plugin update acceptance matrix (Issue #22).
//!
//! Uses a local signed dual-version fixture registry so `numan update` can
//! discover an upgrade without an official multi-version plugin.
//!
//! ## Prerequisites
//!
//! - Nushell **0.113.x** on PATH (hard fail, not skip)
//! - Network access to fetch one official plugin artifact (cached under
//!   `target/acceptance/artifact-cache/`)
//! - Built `numan` binary (`cargo build`)
//!
//! ## Run
//!
//! ```text
//! cargo build
//! cargo test --locked --test plugin_active_update_real_nu -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Default PR CI skips this suite (same class as Stage 1). Manual workflow:
//! `.github/workflows/active-plugin-update-acceptance.yml`.
//!
//! See `docs/acceptance/active-plugin-update-real-nu.md`.

mod support;

use std::time::Duration;

use anyhow::{bail, Context, Result};
use numan_cli::state::journal::PendingActivation;
use numan_cli::state::lifecycle_journal::{LifecycleOp, LifecycleStage, PendingLifecycle};
use numan_cli::state::plugin_deactivate_journal::PendingPluginDeactivate;
use support::active_update::{
    resolve_numan_binary, ActiveUpdateConfig, ActiveUpdateRun, ENV_FAIL_PLUGIN_ADD,
    ENV_FAIL_PLUGIN_RM, ENV_MUTATION, FROM_VERSION, TO_VERSION,
};

fn new_run() -> Result<ActiveUpdateRun> {
    let config = ActiveUpdateConfig::from_env()?;
    let numan = resolve_numan_binary()?;
    ActiveUpdateRun::bootstrap(config, numan)
}

fn assert_contains(haystack: &str, needle: &str) -> Result<()> {
    if !haystack.contains(needle) {
        bail!("expected output to contain {needle:?}, got:\n{haystack}");
    }
    Ok(())
}

#[test]
#[ignore = "real-Nu active-plugin update matrix; requires Nu 0.113.x + network artifact fetch"]
fn real_nu_active_update_happy_path() -> Result<()> {
    let mut run = new_run()?;
    run.set_env(ENV_MUTATION, "1");
    run.prepare_active_v1()?;

    run.require_ok(&["update", &run.package_id], Duration::from_secs(300))?;

    let lockfile = run.lockfile()?;
    let entry = lockfile
        .packages
        .get(&run.package_id)
        .context("missing package after update")?;
    assert_eq!(entry.version, TO_VERSION);
    assert!(
        entry.activation.is_some(),
        "expected reactivation after orchestrated update"
    );
    assert!(PendingLifecycle::load(&run.root)?.is_none());
    assert!(PendingActivation::load(&run.root)?.is_none());
    assert!(PendingPluginDeactivate::load(&run.root)?.is_none());
    Ok(())
}

#[test]
#[ignore = "real-Nu active-plugin update matrix; requires Nu 0.113.x + network artifact fetch"]
fn real_nu_active_update_refuses_when_opt_in_is_unset() -> Result<()> {
    let mut run = new_run()?;
    run.remove_env(ENV_MUTATION);
    run.prepare_active_v1()?;

    let outcome = run.run_numan(&["update", &run.package_id], Duration::from_secs(120))?;
    assert_ne!(outcome.exit_code, Some(0), "update must refuse while gated");
    let err = format!(
        "{}\n{}",
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert_contains(&err, "NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION")?;

    let entry = run
        .lockfile()?
        .packages
        .get(&run.package_id)
        .cloned()
        .context("missing package")?;
    assert_eq!(entry.version, FROM_VERSION);
    assert!(entry.activation.is_some(), "activation must remain");
    Ok(())
}

#[test]
#[ignore = "real-Nu active-plugin update matrix; requires Nu 0.113.x + network artifact fetch"]
fn real_nu_active_update_refuses_stale_nupaths() -> Result<()> {
    let mut run = new_run()?;
    run.set_env(ENV_MUTATION, "1");
    run.prepare_active_v1()?;
    run.mutate_nu_hash("deadbeef_stale_hash_for_acceptance")?;

    let outcome = run.run_numan(&["update", &run.package_id], Duration::from_secs(120))?;
    assert_ne!(
        outcome.exit_code,
        Some(0),
        "update must refuse stale Nu identity"
    );
    let err = format!(
        "{}\n{}",
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert_contains(&err, "does not match")?;

    let entry = run
        .lockfile()?
        .packages
        .get(&run.package_id)
        .cloned()
        .context("missing package")?;
    assert_eq!(entry.version, FROM_VERSION);
    assert!(entry.activation.is_some(), "activation must be preserved");
    Ok(())
}

#[test]
#[ignore = "real-Nu active-plugin update matrix; requires Nu 0.113.x + network artifact fetch"]
fn real_nu_active_update_refuses_missing_nupaths() -> Result<()> {
    let mut run = new_run()?;
    run.set_env(ENV_MUTATION, "1");
    run.prepare_active_v1()?;
    run.delete_nu_paths()?;

    let outcome = run.run_numan(&["update", &run.package_id], Duration::from_secs(120))?;
    assert_ne!(
        outcome.exit_code,
        Some(0),
        "update must refuse without NuPaths"
    );
    let err = format!(
        "{}\n{}",
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert!(
        err.contains("not cached") || err.contains("Nu paths") || err.contains("init --refresh"),
        "unexpected refusal text:\n{err}"
    );

    let entry = run
        .lockfile()?
        .packages
        .get(&run.package_id)
        .cloned()
        .context("missing package")?;
    assert!(entry.activation.is_some());
    Ok(())
}

#[test]
#[ignore = "real-Nu active-plugin update matrix; requires Nu 0.113.x + network artifact fetch"]
fn real_nu_active_update_resume_lockfile_updated_reactivates() -> Result<()> {
    let mut run = new_run()?;
    // Resume completes already-journaled work without starting new orchestration.
    run.remove_env(ENV_MUTATION);
    run.prepare_active_v1()?;

    // Deactivate + install v2 without reactivate, then seed needs_reactivate journal.
    run.require_ok(
        &["deactivate", &run.package_id, "--yes"],
        Duration::from_secs(120),
    )?;
    let install_spec = format!("{}@{TO_VERSION}", run.package_id);
    run.require_ok(&["install", &install_spec], Duration::from_secs(300))?;

    let lockfile = run.lockfile()?;
    let entry = lockfile
        .packages
        .get(&run.package_id)
        .context("missing package")?;
    assert_eq!(entry.version, TO_VERSION);
    assert!(entry.activation.is_none());

    PendingLifecycle {
        op: LifecycleOp::Update,
        package_id: run.package_id.clone(),
        stage: LifecycleStage::LockfileUpdated,
        orphan_payload_path: Some(format!(
            "packages/plugins/{}/{FROM_VERSION}-orphan",
            run.package_id
        )),
        from_version: Some(FROM_VERSION.to_string()),
        to_version: Some(TO_VERSION.to_string()),
        nupm_source_path: None,
        nupm_metadata_sha256: None,
        staging_dir: None,
        promoted_payload_path: None,
        batch_package_ids: Vec::new(),
        batch_staging_dirs: Vec::new(),
        target_snapshot_id: None,
        pre_rollback_snapshot_id: None,
        needs_reactivate: true,
    }
    .save(&run.root)?;

    run.require_ok(&["update"], Duration::from_secs(180))?;

    let entry = run
        .lockfile()?
        .packages
        .get(&run.package_id)
        .cloned()
        .context("missing package after resume")?;
    assert_eq!(entry.version, TO_VERSION);
    assert!(
        entry.activation.is_some(),
        "resume must reactivate after LockfileUpdated"
    );
    assert!(PendingLifecycle::load(&run.root)?.is_none());
    Ok(())
}

#[test]
#[ignore = "real-Nu active-plugin update matrix; requires Nu 0.113.x + network artifact fetch"]
fn real_nu_active_update_unregister_failure_leaves_journals() -> Result<()> {
    let mut run = new_run()?;
    run.set_env(ENV_MUTATION, "1");
    run.prepare_active_v1()?;
    run.set_env(ENV_FAIL_PLUGIN_RM, "1");

    let outcome = run.run_numan(&["update", &run.package_id], Duration::from_secs(180))?;
    assert_ne!(
        outcome.exit_code,
        Some(0),
        "update must fail when plugin rm fails"
    );

    let entry = run
        .lockfile()?
        .packages
        .get(&run.package_id)
        .cloned()
        .context("missing package")?;
    assert_eq!(entry.version, FROM_VERSION);
    assert!(
        entry.activation.is_some(),
        "activation must remain when unregister fails"
    );
    assert!(
        PendingLifecycle::load(&run.root)?.is_some()
            || PendingPluginDeactivate::load(&run.root)?.is_some(),
        "expected lifecycle and/or deactivate journal after unregister failure"
    );
    Ok(())
}

#[test]
#[ignore = "real-Nu active-plugin update matrix; requires Nu 0.113.x + network artifact fetch"]
fn real_nu_active_update_reactivate_failure_leaves_recovery() -> Result<()> {
    let mut run = new_run()?;
    run.set_env(ENV_MUTATION, "1");
    run.prepare_active_v1()?;
    // Allow deactivate (plugin rm) but fail reactivate (plugin add).
    run.remove_env(ENV_FAIL_PLUGIN_RM);
    run.set_env(ENV_FAIL_PLUGIN_ADD, "1");

    let outcome = run.run_numan(&["update", &run.package_id], Duration::from_secs(300))?;
    assert_ne!(
        outcome.exit_code,
        Some(0),
        "update must fail when reactivate fails"
    );
    let err = format!(
        "{}\n{}",
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert!(
        err.contains("activate") || err.contains("reactivat"),
        "expected reactivate recovery guidance, got:\n{err}"
    );

    let entry = run
        .lockfile()?
        .packages
        .get(&run.package_id)
        .cloned()
        .context("missing package")?;
    assert_eq!(
        entry.version, TO_VERSION,
        "upgrade must have completed before reactivate failure"
    );
    assert!(
        entry.activation.is_none(),
        "package must remain inactive after reactivate failure"
    );
    let lifecycle = PendingLifecycle::load(&run.root)?.context("expected lifecycle journal")?;
    assert_eq!(lifecycle.stage, LifecycleStage::LockfileUpdated);
    assert!(lifecycle.needs_reactivate);
    assert!(PendingActivation::load(&run.root)?.is_some());
    Ok(())
}
