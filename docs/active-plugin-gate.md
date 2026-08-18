# Active plugin mutation gate (Issue #22)

## Safety invariant

While a package has a lockfile `activation` record (plugin activation), Numan must not **remove** that package until the activation record is cleared. Plugin deactivate clears the record via a journaled `plugin rm` flow. `--force` on `numan remove` does **not** bypass active plugin activation. It only bypasses active *module* activation (`module_activation`).

**Update** of an active plugin is fail-closed and opt-in. Only exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` permits deactivate → upgrade → reactivate. Missing, empty, or alternative values refuse before lifecycle operations begin. Missing Nu path cache or a stale/mismatched activation identity also refuses update so the activation record is not rewritten without a verified unregister.

## Current behavior

| Operation | Active plugin | Active module |
|---|---|---|
| `numan remove` | **Always refused** while `activation` is set (even when mutation enabled) | Refused unless `--force` |
| `numan update` | Orchestrated deactivate→upgrade→activate only with exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1`; otherwise refused | Refused (use `deactivate` first) |
| `numan deactivate` | Supported: journaled unregister + clear `activation` (payload kept) | Supported today |

After `numan deactivate <pkg>`, `numan remove <pkg>` succeeds without `--force` (inactive plugin).

### Exact opt-in

```text
NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1   # enable active update orchestration
```

Unset is disabled. The value must be exactly `1`; empty values, alternate truthy spellings, whitespace, and all other values are disabled.

`numan doctor` reports an **info** finding `activation.plugin_mutation_gated` for each package with `package_type == "plugin"` and `activation.is_some()` (even when `nu_state/paths.json` is missing). A pending deactivate journal surfaces as `journal.plugin_deactivate_pending` (warn); `--fix` runs deactivate reconcile.

Canonical hint text lives in `util::hints::active_plugin_mutation_gated` / `ACTIVE_PLUGIN_MUTATION_GATED_FIX` (and `active_plugin_update_disabled` / `active_plugin_update_list_note` for the opt-in path).

### Lifecycle journal (active update)

`state/pending-lifecycle.json` with `op: update` reuses existing stages:

- `prepared`: before/during deactivate (also `pending-plugin-deactivate.json`)
- `lockfile_updated`: after install upgrade; reactivate may still be pending (`pending-activation.json`)

Cleared only after a successful full path (including reactivate when orchestration ran).

### Reporting

- `numan activate --list`: for packages with plugin activation, prints whether remove stays gated and whether update is permitted under the current override and Nu identity.
- `numan doctor`: info finding `activation.plugin_mutation_gated` (deactivate available; remove still gated; update exact-`1` opt-in). Pending deactivate journal: `journal.plugin_deactivate_pending` (warn); `--fix` runs deactivate reconcile.

Canonical remove hint: `util::hints::active_plugin_mutation_gated`. Update disabled hint: `util::hints::active_plugin_update_disabled`.

## Shared helpers

`cmd::plugin_lifecycle` owns the activate/deactivate integration boundary and exposes `PluginLifecycle` to coordinators such as `update`. `update` can request deactivate/activate operations but does not own or invoke Nu registrar/unregistrar callbacks. Full CLI consent/lock/snapshot stay in `deactivate` / `activate`.

## Evidence matrix (Issue #22)

| Scenario | Evidence |
|---|---|
| Remove while active (incl. `--force`) | Unit tests (`cmd::remove`); Stage 1 asserts activation still present after list |
| Deactivate → remove → gc | [Stage 1 acceptance](acceptance/official-registry-stage1.md) on Windows x86_64 |
| Active update orchestration | Unit fake hooks (`cmd::update`); real-Nu happy path in [active-plugin-update-real-nu](acceptance/active-plugin-update-real-nu.md) (fixture dual-version registry; `workflow_dispatch` 3-OS job) |
| Fault injection (unregister / reactivate failure) | Unit journal/restore tests; real-Nu Nu-shim approximations in the same suite (`FAIL_PLUGIN_RM` / `FAIL_PLUGIN_ADD`) |
| Gate refusals (missing exact opt-in, stale/missing NuPaths) | Unit + real-Nu suite |
| Resume `LockfileUpdated` + `needs_reactivate` | Unit + real-Nu suite |
| Ownership / name targeting | Lockfile `is_active_for` + binary file existence + unregister via absolute payload path (no msgpackz parse) |
| Real-Nu smoke marker | `tests/plugin_lifecycle_real_nu.rs` (points at Stage 1 + update suite) |

**Stage 1 covers deactivate→remove on Windows x86_64.** Active **update** real-Nu evidence lives in the fixture suite. The exact-opt-in lifecycle path passed Ubuntu, Windows, and macOS in [acceptance run 30429081756](https://github.com/numan-cli/numan/actions/runs/30429081756) (commit `05eb89d`, after the fixture fixes in `8706bfe`). Official multi-version update e2e remains blocked until the production index publishes a second plugin version.

See also: [docs/acceptance/official-registry-stage1.md](acceptance/official-registry-stage1.md), [docs/acceptance/active-plugin-update-real-nu.md](acceptance/active-plugin-update-real-nu.md).
