# Active-plugin update real-Nu acceptance

Manual, fixture-registry integration tests for Issue #22 active-plugin **update**
orchestration. They spawn the built `numan` binary under an isolated home and
prove happy-path upgrade plus gate/fault approximations against a **local signed
dual-version registry** (one real plugin artifact republished as `1.0.0` and
`2.0.0`).

Official Stage 1 (`docs/acceptance/official-registry-stage1.md`) remains the
production-registry gate for deactivate → remove → gc on Windows x86_64. It does
not exercise update: the official index has no multi-version plugins today.

## Lifecycle covered

| Test | Intent |
|---|---|
| `real_nu_active_update_happy_path` | Default-on → deactivate → upgrade → reactivate; journals cleared |
| `real_nu_active_update_refuses_when_kill_switch_is_set` | Explicit kill switch; activation unchanged |
| `real_nu_active_update_refuses_stale_nupaths` | Fail-closed identity mismatch |
| `real_nu_active_update_refuses_missing_nupaths` | Fail-closed without cached Nu paths |
| `real_nu_active_update_resume_lockfile_updated_reactivates` | `needs_reactivate` resume despite the kill switch |
| `real_nu_active_update_unregister_failure_leaves_journals` | Nu shim fails `plugin rm` |
| `real_nu_active_update_reactivate_failure_leaves_recovery` | Nu shim fails `plugin add` after upgrade |

## Prerequisites

- Nushell **0.113.x** on `PATH` (hard fail on mismatch; not a silent skip).
- Network access once to download the subject artifact from the official registry
  (cached under `target/acceptance/artifact-cache/`).
- A built `numan` binary (`cargo build` / `cargo build --locked`).

## Invocation

```bash
cargo build --locked
cargo test --locked --test plugin_active_update_real_nu -- --ignored --nocapture --test-threads=1
```

The **Active Plugin Update Acceptance** workflow
(`.github/workflows/active-plugin-update-acceptance.yml`) is `workflow_dispatch`
only, matrixed across Ubuntu / Windows / macOS with Nu 0.113.1, and uploads
evidence on failure.

Default PR `real-nu-acceptance` **skips** this suite (same class as Stage 1).

## Overrides

| Variable | Default | Meaning |
|---|---|---|
| `NUMAN_ACCEPTANCE_PACKAGE` | `cptpiepmatz/nu_plugin_highlight` | Official package used as the real artifact source (must have a host-triple target). |
| `NUMAN_ACCEPTANCE_OUTPUT` | `target/acceptance/active-plugin-update-real-nu` | Evidence parent directory. |
| `NUMAN_ACCEPTANCE_ARTIFACT_CACHE` | `target/acceptance/artifact-cache` | Downloaded archive cache; original format suffix is preserved. |

## Isolation

Each run creates a unique directory with `home/`, `evidence/`, and
`fixture-registry/`. Child processes use `env_clear()` plus an isolated home
(same model as Stage 1). A Nu shim is prepended to `PATH` so init caches shim
identity while forwarding to the real Nu binary; Windows uses a test-only native
forwarder because Rust safely rejects batch launchers for Nu programs. Fault tests set
`NUMAN_TEST_FAIL_PLUGIN_RM=1` or `NUMAN_TEST_FAIL_PLUGIN_ADD=1`.

The fixture registry is planted into the Numan root after `init` (signed index +
trust key). Artifact URLs are absolute local paths so installs do not re-hit the
network.

## Non-goals

- Production official multi-version update e2e (blocked until an official v2 exists).
- True mid-crash races between Nu success and lockfile write (covered by unit
  journal-reconcile tests).

See also: [docs/active-plugin-gate.md](../active-plugin-gate.md).

The default-on evidence bar was met by the green Ubuntu, Windows, and macOS
[workflow run 30427823246](https://github.com/tonythethompson/numan/actions/runs/30427823246).
