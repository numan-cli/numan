//! Real-Nu active-plugin lifecycle matrix marker (Issue #22).
//!
//! ## CI coverage map (honest)
//!
//! | Scenario | Where it runs today |
//! |---|---|
//! | Deactivate → remove → gc | Stage 1 official-registry acceptance (Windows x86_64) |
//! | Remove while active (incl. `--force`) | Unit tests in `cmd::remove` + Stage 1 post-list assertions |
//! | Active update (deactivate→upgrade→reactivate) | Unit fakes in `cmd::update`; real-Nu suite in `plugin_active_update_real_nu` |
//! | Unregister / reactivate failure journals | Unit fakes; real-Nu approximations in `plugin_active_update_real_nu` |
//! | Ownership (path + lockfile identity) | Deactivate/update fake hooks assert absolute binary path |
//!
//! ## Default-on gate
//!
//! Active update is default-on after the real-Nu suite passed on
//! Linux/macOS/Windows in the green [workflow run 30427823246](https://github.com/tonythethompson/numan/actions/runs/30427823246).
//! `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=0` remains the emergency kill switch.
//!
//! Run the matrix (Nu 0.113.x required):
//!   cargo build
//!   cargo test --test plugin_active_update_real_nu -- --ignored --nocapture --test-threads=1
//!
//! This ignored test is a smoke marker only (Nu present + print status).

use std::process::Command;

#[test]
#[ignore = "requires real Nu on PATH; Stage 1 + plugin_active_update_real_nu are the authoritative gates"]
fn real_nu_active_plugin_lifecycle_matrix_marker() {
    let nu_ok = Command::new("nu")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !nu_ok {
        eprintln!("skip: no `nu` on PATH: cannot exercise real plugin lifecycle marker");
        return;
    }

    let version = String::from_utf8_lossy(
        &Command::new("nu")
            .arg("--version")
            .output()
            .expect("nu --version")
            .stdout,
    )
    .trim()
    .to_string();
    eprintln!("real-Nu lifecycle marker: nu {version}");
    eprintln!(
        "matrix status: Stage 1 deactivate→remove (Windows x86_64); \
         active update real-Nu suite: tests/plugin_active_update_real_nu.rs \
         (workflow_dispatch; skipped on default PR ignored job)"
    );
}
