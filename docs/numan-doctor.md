# `numan doctor` specification

**Status:** Implemented (Phase 7.2)  
**Authority:** This document defines behavior for `numan doctor` before implementation.

## Purpose

`numan doctor` diagnoses the health of a Numan root and applies **safe automated repairs** by default — the same pattern as `brew doctor`, `npm doctor`, and similar tooling.

Default mode repairs. Use `--scan` for report-only output (safe for CI and scripting). Repairs delegate to existing commands (`init`, `activate`, `registry sync`) rather than inventing new mutation paths.

It answers: *“Is this Numan root consistent, safe to mutate, and aligned with the current Nu environment?”* and *“Fix what you can.”*

## Non-goals

- No `install`, `remove`, `update`, or `gc`
- No nupm import, nupm mutation, or `build.nu` execution
- No overwriting foreign managed files (`autoload.managed_foreign` stays manual)
- No blind completion of in-flight lifecycle journals (too risky — report + guide re-run)
- No re-download of missing payloads (report `payload.missing`; user runs `install` again)

## Invocation

```text
numan doctor [--scan] [--json] [--nupm-home PATH]
```

| Flag | Behavior |
|------|----------|
| `--scan` | Report findings without applying repairs (dry-run mode) |
| `--json` | Emit a single JSON object (schema versioned); no ANSI styling. Includes a `repairs` array when repairs ran; omitted under `--scan` |
| `--nupm-home PATH` | Override nupm home for the optional coexistence section (same resolution order as `numan nupm status`) |

Global `--root` applies as for all commands.

**Default (no flags):** diagnose and apply available repairs, then print findings.
Confirm-tier repairs are applied automatically in default mode (no TTY gate,
no `not_confirmed` outcome). Nested `setup nu use` for
`nu.binary.found_off_path` still requires explicit consent (`--yes` / TTY)
and is never auto-approved by doctor.
**`--scan`:** diagnose and print findings without mutating state.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | No **error**-severity findings (warnings and info allowed) |
| `1` | One or more **error**-severity findings |
| `2` | Cannot run meaningful checks (e.g. not initialized, unreadable root) |

## Severity model

Each finding has:

- `id` — stable machine identifier (e.g. `nu_paths.missing`)
- `severity` — `ok` \| `info` \| `warn` \| `error`
- `message` — human-readable summary
- `fix` — optional suggested command for manual issues (e.g. `numan init` or `numan registry add …`)
- `repair` — `none` \| `auto` \| `confirm` \| `manual` (whether default repair mode can act; see below)

**Rules:**

- `error` — blocks safe mutation until resolved (drift, stale journal with wrong identity, missing payload for active package)
- `warn` — operational risk or incomplete setup (no registries, nupm drift, pending journal from interrupted run)
- `info` — contextual only (nupm home not configured, no packages installed)
- `ok` — check passed (included in `--json`; omitted in default human output unless `--verbose` is added later)

## Repair policy

In default (repair) mode, doctor acquires `acquire_mutation_lock(root)` for its own
filesystem repairs (layout dirs, `registry.none` config writes), then **releases**
that guard before nested `init` / `setup` / `registry sync` / `activate` /
`deactivate` commands. Those nested commands acquire the mutation lock themselves.
Doctor does **not** hold one lock for the entire repair pass end-to-end.
A PreMutation snapshot (`SnapshotTrigger::Doctor`) is taken after the lock is
acquired and before layout/config writes. If snapshot creation fails (for
example a malformed lockfile or missing payload revision), doctor records
`snapshot.pre_mutation` as failed, continues with independent `layout.*`,
`nu.active_version.invalid` cleanup, and `registry.none` repairs, and skips
nested mutations that need a PreMutation baseline (`nu_paths.missing` → `init`,
plus `setup` / `registry sync` / `activate` / `deactivate`) with reason
`snapshot_unavailable`.

Repair steps run in this **order** (each step re-validates only what it changed):

| Tier | Prompt? | Finding IDs | Action |
|------|---------|-------------|--------|
| **auto** | Never | `layout.*` (missing dirs), `nu.active_version.invalid` | Independent of PreMutation success: `create_dir_all` for layout; preserve raw `active-version.json` bytes to `active-version.json.corrupt`, then clear via `clear_active_version` |
| **auto** | Never | `journal.migration_pending` | `migration_journal::reconcile` under the mutation lock after a PreMutation Doctor snapshot (skipped/Failed without aborting later repairs) |
| **auto** | Never | `nu_paths.missing` | `numan init` (skipped with `snapshot_unavailable` when PreMutation fails) |
| **auto** | Never | `registry.index_missing` | `numan registry sync` (skipped with `snapshot_unavailable` when PreMutation fails) |
| **auto** | Never | `registry.none` (production trust root only) | Add official registry via same path as `numan init` (continues even when PreMutation fails) |
| **manual** | Never auto | `nu.binary.missing_on_path` | Print fix hint (`numan setup nu`); doctor never downloads managed Nu without explicit user opt-in |
| **confirm** | Explicit consent when managed Nu exists | `nu.binary.found_off_path` | `numan setup nu use <path>` (adds existing install to PATH; doctor never passes `--yes`, so a managed wipe stays fail-closed / interactive) |
| **confirm** | Never (applied in default mode) | `nu_paths.drift`, `nu_paths.vendor_drift` | `numan init --refresh` |
| **confirm** | Never (applied in default mode) | `journal.plugin_pending`, `journal.autoload_pending`, `journal.plugin_stale`, `journal.autoload_stale`, `activation.plugin_stale`, `activation.module_stale`, `autoload.projection`, `autoload.managed_missing` | `numan activate` (empty package list — reconciles journals and re-activates stale entries; same entry point as normal activate recovery) |
| **confirm** | Never (applied in default mode) | `journal.plugin_deactivate_pending` | `numan deactivate <journal package ids>` (reconciles pending-plugin-deactivate journal only; not a full-root deactivate) |
| **confirm** | Never (applied in default mode) | `journal.plugin_deactivate_stale` | `numan init --refresh` then `numan deactivate` |
| **manual** | Never auto | `autoload.managed_foreign`, `payload.missing`, `journal.lifecycle_pending`, `journal.lifecycle_stale`, `journal.migration_invalid`, `registry.none` (placeholder trust root), `nu_paths.vendor_missing`, `nupm.*` | Print fix hint only |
| **none** | Never | `activation.plugin_mutation_gated` (`info`) | Informational only; see [docs/active-plugin-gate.md](active-plugin-gate.md) |

**Invariants during repair:**

1. Reuse `cmd::init::execute`, `cmd::activate::execute`, `cmd::deactivate::execute`, `cmd::registry::sync` — no duplicated mutation logic.
2. Install remains inert; doctor never invokes install transaction.
3. Never write under `NUPM_HOME`.
4. If any **manual**-tier error remains after repair, exit `1` even if some auto/confirm fixes succeeded.
5. Report a repair summary: `Fixed N issues; M require manual action.`
6. Mutation lock ownership is **staged**: doctor's lock covers only its direct edits;
   nested mutators reacquire after doctor drops the guard (see above).

**Journal note:** `--scan` only *reports* journals without acting. Default repair mode may reconcile plugin/autoload journals via `activate` recovery, and plugin-deactivate journals via `deactivate` recovery scoped to journal package IDs — not by editing journal files directly.

## Check catalog

Checks run in order below. Implementation should call existing validators (`NuPaths::validate_drift`, `AutoloadState::validate_against_lockfile`, etc.) rather than duplicating logic.

### 1. Root layout

| ID | Severity if failed | Condition |
|----|-------------------|-----------|
| `root.writable` | `error` | Numan root exists and is writable |
| `layout.nu_state` | `warn` | `nu_state/` present |
| `layout.state` | `warn` | `state/` present |

### 2. Initialization (`nu_state/paths.json`)

| ID | Severity | Condition |
|----|----------|-----------|
| `nu.binary.missing_on_path` | `error` | Nu not on PATH and not under `$NUMAN_ROOT/tools/nushell/` → fix: `numan setup nu` |
| `nu.binary.found_off_path` | `warn` | Nu exists in a known install root (e.g. `~/.cargo/bin`, `%LOCALAPPDATA%\Programs\nushell`) but not on PATH → fix: `numan setup nu use <path>` |
| `nu.path.version` | `info` | PATH-only Nu version (`PATH Nu: 0.114.1`), `PATH Nu: not found`, or `PATH Nu: found at '<path>' but version probe failed (<error>)` when the binary exists but `--version` fails. Does not treat managed Nu as PATH. Report-only (no automatic repair). |
| `nu.managed.version` | `info` | Managed binary under `$NUMAN_ROOT/tools/nushell/` with version, `Managed Nu: not installed`, or `Managed Nu: present at '<path>' but version probe failed (<error>)` when the binary exists but `--version` fails. Report-only (no automatic repair). |
| `nu.active_version.invalid` | `error` | `nu_state/active-version.json` is present but unreadable/invalid JSON. Lookup would otherwise soft-miss the marker and fall back to PATH. **auto:** copy raw bytes to `active-version.json.corrupt` (best-effort, recoverable `binary_path`), then clear via `clear_active_version` so resolution recovers cleanly. |
| `nu_paths.missing` | `error` | `paths.json` absent → fix: `numan init` |
| `nu_paths.drift` | `error` | `NuPaths::validate_drift()` fails → fix: `numan init --refresh` |
| `nu_paths.vendor_drift` | `error` | `validate_vendor_drift()` fails when `data_dir` cached → fix: `numan init --refresh` |
| `nu_paths.vendor_missing` | `warn` | Active module in lockfile but `vendor_autoload_dir` is `None` → fix: fix Nu install/config, then `numan init --refresh` |

### 3. Pending journals

| ID | Severity | Condition | Repair |
|----|----------|-----------|--------|
| `journal.plugin_pending` | `warn` | `state/pending-activation.json` exists | **confirm:** `activate` reconciles |
| `journal.plugin_stale` | `error` | Journal Nu identity ≠ current `NuPaths` | **confirm:** `init --refresh` then `activate` |
| `journal.plugin_deactivate_pending` | `warn` | `state/pending-plugin-deactivate.json` exists | **confirm:** `deactivate` reconciles |
| `journal.plugin_deactivate_stale` | `error` | Deactivate journal Nu identity ≠ current `NuPaths` | **confirm:** `init --refresh` then `deactivate` |
| `journal.autoload_pending` | `warn` | `state/pending-autoload.json` exists | **confirm:** `activate` reconciles |
| `journal.autoload_stale` | `error` | Journal identity mismatch | **confirm:** `init --refresh` then `activate` |
| `journal.lifecycle_pending` | `warn` | `state/pending-lifecycle.json` exists | **manual:** re-run or clear per op |
| `journal.lifecycle_stale` | `error` | Stale lifecycle journal | **manual** |
| `journal.migration_pending` | `warn` | `state/migration-journal.json` exists, parses, and is filesystem-consistent (stage `Prepared` \| recoverable `Renamed` \| `Active`) | **auto:** `migration_journal::reconcile` under the mutation lock, after a PreMutation snapshot; hint `numan doctor --fix` (or `numan setup nu <version>` to install) when the versioned binary is absent, else `numan use <version>` |
| `journal.migration_invalid` | `error` | journal unreadable/unparseable/unsupported `schema_version`, or filesystem-inconsistent (`Renamed` with missing `<version>/<bin>`, unsafe version component) | **manual:** `numan setup nu <version>` when the binary is missing, or delete the stale journal; auto-reconcile refuses these states |

### 4. Lockfile and activation identity

| ID | Severity | Condition |
|----|----------|-----------|
| `lockfile.missing` | `info` | No lockfile or empty → nothing installed |
| `lockfile.parse` | `error` | Lockfile unreadable or invalid JSON |
| `activation.plugin_mutation_gated` | `info` | Plugin has `activation.is_some()` (lockfile-only; reported even when `NuPaths` is missing). Remove stays gated until deactivate; update orchestrates only with exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` (default off). **Repair:** none (info). See [docs/active-plugin-gate.md](active-plugin-gate.md). |
| `activation.plugin_stale` | `warn` | Plugin has `activation` but `is_active_for` false for current `NuPaths` |
| `activation.module_stale` | `warn` | Module has `module_activation` but `is_module_active_for` false |
| `autoload.projection` | `error` | `AutoloadState::validate_against_lockfile` fails |
| `autoload.managed_missing` | `warn` | Active modules but managed `numan.nu` absent |
| `autoload.managed_foreign` | `error` | Managed file exists but fails `assert_managed_file_owned` |

Reuse the same checks as `numan activate --check` where applicable, but run for **all** active modules/plugins without requiring `--check` on activate.

### 5. Payload presence (lightweight)

| ID | Severity | Condition |
|----|----------|-----------|
| `payload.missing` | `error` | Lockfile entry references `payload_path` that does not exist under root |

No re-hash or revision recompute in v1 (too expensive for doctor).

### 6. Registry configuration

| ID | Severity | Condition |
|----|----------|-----------|
| `registry.none` | `warn` | `config.toml` has no registries → fix: `numan init` before first init; `numan doctor` after init (production trust root); `numan registry add …` for custom/placeholder builds |
| `registry.index_missing` | `info` | Enabled registry has no cached index under `registry/` → fix: `numan registry sync` |
| `registry.trust_root` | `info` | Enabled `official` registry: reports built-in key id (e.g. `official-2026-07-01`). Placeholder builds note that the key is not production. Report-only (no automatic repair). |
### 7. nupm coexistence (optional section)

Controlled by `config.toml` → `[nupm_compat] scan_on_doctor` (default `true`). When `false`, skip section entirely.

When enabled:

- If `NUPM_HOME` / `--nupm-home` unavailable: `info` finding `nupm.home_unconfigured` (not an error)
- Else: run read-only discovery (same as `numan nupm status` classification counts)
  - `nupm.drift` — `warn` if `source_drift_count > 0` → fix: `numan nupm diff <pkg>`
  - `nupm.overlap` — `info` if `name_overlap_count > 0`

Never write under nupm home.

## Human output format

```text
Numan doctor — <root>

Initialization
  ✓ Nu paths cached (0.113.1)
  ✓ Nu binary hash matches

Journals
  ⚠ Pending lifecycle journal (op: nupm_import, stage: StagingPayload)
    Fix: complete or clear per docs/RELEASING.md …

Activation
  ✓ Plugin owner/foo active for current Nu
  ✗ Autoload-state projection mismatch: …
    Fix: numan activate owner/module-name

nupm coexistence
  · nupm home not configured (pass --nupm-home or set NUPM_HOME)

Summary: 1 error, 1 warning

Repairs: 3 applied, 0 skipped
```

Use `console` styling consistent with `activate --check`.

## JSON output format (v1)

```json
{
  "schema_version": 1,
  "root": "/path/to/numan",
  "summary": { "errors": 1, "warnings": 1, "infos": 0 },
  "findings": [
    {
      "id": "autoload.projection",
      "severity": "error",
      "message": "…",
      "fix": "numan activate",
      "repair": "confirm"
    }
  ],
  "repairs": [
    { "id": "registry.index_missing", "status": "applied" },
    { "id": "nu_paths.drift", "status": "applied" },
    { "id": "nu.binary.missing_on_path", "status": "skipped", "reason": "requires_explicit_setup_nu" }
  ]
}
```

`repairs` is present in default repair mode and omitted when `--scan` is set.

## Architecture

| Piece | Location |
|-------|----------|
| CLI | `src/cmd/doctor.rs` |
| Dispatch | `src/main.rs` → `Commands::Doctor` |
| Config gate | `config::NupmCompatConfig::scan_on_doctor` |
| Tests | `tests/doctor_test.rs` + inline unit tests; **no real Nu** |

Public test seam (if needed):

```rust
pub fn execute_with_options(args: &DoctorArgs, root: &Path, options: DoctorOptions) -> Result<DoctorReport>
```

## Relationship to existing commands

| Command | Role |
|---------|------|
| `numan init` / `init --refresh` | **Repair** Nu path drift (default doctor delegates here) |
| `numan setup nu` | **Manual fix** for missing Nushell (`nu.binary.missing_on_path`; doctor prints the hint and does not download) |
| `numan setup nu use <path>` | **Repair** off-PATH Nushell (`nu.binary.found_off_path`; adds parent dir to user PATH; consented wipe of managed Nu requires `--yes` / TTY; doctor does not auto-approve) |
| `numan activate` | **Repair** activation + journal reconciliation |
| `numan registry sync` | **Repair** missing index cache (auto tier) |
| `numan activate --check` | Deep **module** check only; no repair |
| `numan nupm status` | nupm-only summary; doctor embeds optional subset |
| `numan update` / `remove` / `gc` | Block on stale lifecycle journal; doctor reports, does not fix lifecycle |

## Definition of done

- [x] `numan doctor`, `numan doctor --scan`, and `numan doctor --json` implemented per check catalog
- [x] `scan_on_doctor` respected
- [x] `--scan` mode: no state mutation (test: hashes unchanged)
- [x] Default repair mode: only repair tiers in policy; uses mutation lock; PreMutation Doctor snapshot; delegates to init/activate/sync
- [x] Documented in README command table and `AGENTS.md`
- [x] Integration tests: `--scan` report-only, default auto repairs, confirm-tier applied by default, manual tier untouched

## Changelog

| Date | Change |
|------|--------|
| 2026-06-30 | Initial spec (Phase 7.2) |
| 2026-06-30 | Add `--fix` / `--yes` repair policy (auto / confirm / manual tiers) |
| 2026-08-02 | Invert defaults: repair by default; `--scan` for report-only; remove `--fix` / `--yes` |