# Next Steps Implementation Plan (code-centered)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Source audit:** [`2026-07-19-six-month-strategy-audit.md`](2026-07-19-six-month-strategy-audit.md)

**Goal:** Turn the six-month strategy into ordered, code-touching work across numan, numan-registry, and numan-plugins without greenfielding features that already exist.

**Architecture:** Prefer upgrading existing surfaces (`search`, `info`, `resolve`, `try_cmd`, `setup nu --version`, `add-package.py`, `manifest.json`) and closing the provenance/handoff gaps. Catalog depth stays plugins→registry→client. Client schema for `source` / `verified_with` already exists; fill the pipeline and display.

**Tech Stack:** Rust CLI (numan), Python intake scripts (numan-registry), GitHub Actions + `manifest.json` (numan-plugins), Ed25519 signed `index.json`.

## Global Constraints

- Install stays inert; only `activate` touches Nu.
- Registry signatures mandatory; plugin artifact SHA256 mandatory.
- Plugin ABI is Nu-minor-scoped; never silent Nu auto-switch.
- Prefer `numan setup nu --version` (not inventing `--pin`).
- Never call packages "approved" or "security-audited."
- Do not start Phase 5.2 source builds, side-by-side Nu profiles, or Lane 3 forks.
- Intake Stages 4–6 wait until Stage 1 acceptance + Stage 2 lint are boring.
- Cross-repo order for new plugins: **numan-plugins → numan-registry → numan**.

## File map (who owns what)

| Area | Primary files |
| --- | --- |
| Search display | `numan/src/cmd/search.rs`, tests near module / `tests/` |
| Info + provenance display | `numan/src/cmd/info.rs`, `numan/src/core/package.rs` (`SourceInfo`, `VersionEntry`) |
| Compat / pin messaging | `numan/src/core/resolve.rs`, `numan/src/util/hints.rs` |
| Managed Nu | `numan/src/cmd/setup.rs`, `numan/src/nu/bootstrap.rs`, `tests/setup_nu_test.rs` |
| Starter path | `numan/src/cmd/try_cmd.rs`, related tests |
| Doctor Nu/plugin status | `numan/src/cmd/doctor.rs`, `tests/doctor_test.rs` |
| ADR status labels | `numan/src/cmd/info.rs` first; optional thin helper under `numan/src/core/` |
| Source passthrough | `numan-registry/scripts/add-package.py` (`build_version_entry`) |
| Schema / index | `numan-registry/schemas/`, `numan-registry/registry/index.json`, `numan-registry/specs/` |
| Plugin matrix | `numan-plugins/manifest.json`, `numan-plugins/docs/backlog.json`, build workflow under `.github/workflows/` |
| Intake tracking | `numan-registry/docs/intake-state.json`, `scripts/sync-intake-candidates.py` |

---

## Workstream A — Provenance in the signed index (Bet 3, highest leverage / small)

### Task A1: Pass `source` through intake

**Repo:** numan-registry (+ minimal numan-plugins `gen_spec.py` emit)

- [x] Read `scripts/add-package.py` `build_version_entry()` and a sample `specs/*.json` that includes or should include `source` / git / rev / cargo_name.
- [x] Write a failing unit-style check (`scripts/test_build_version_entry_source.py`) that a spec with `source: {git, rev, cargo_name}` produces those fields in the version entry.
- [x] Change `build_version_entry()` / `copy_source_field()` to copy `source` when present (mirror how `verified_with` / `activation` are handled).
- [x] Emit `source` from `numan-plugins/scripts/gen_spec.py` (`build_spec`) so new builds do not omit provenance.
- [x] Backfill `cptpiepmatz/nu_plugin_highlight` in `specs/` and `registry/index.json` with `source` (surgical edit; hashes unchanged).
- [ ] **Follow-up (ops):** production resign + publish `index.json` after review (not done in the coding pass).
- [x] Verify client deserializes: `parse_version_entry_with_source` in `src/core/package.rs`.
- [ ] Commit in numan-registry / numan-plugins (when ready to ship).

### Task A2: Show provenance on `numan info`

**Repo:** numan

- [x] Write failing test(s) for `info` output when `VersionEntry.source` is `Some` (`format_info_*` in `src/cmd/info.rs`).
- [x] Extend `src/cmd/info.rs` to print upstream git/rev/cargo_name when present.
- [x] Add status line v1 for registry packages: `verified upstream artifact`. Keep copy per audit (no "approved").
- [ ] **Follow-up:** nupm `unreviewed` block. `info` remains registry-only; either extend `info` for nupm-origin ids or surface status on `nupm inspect` (do not invent a new subcommand).
- [x] `cargo test` for touched modules; `cargo clippy -- -D warnings`.
- [ ] Commit: `Show registry source provenance and trust status in info` (when ready to ship).

---

## Workstream B — Compat truth surface (Bet 2, upgrade existing)

### Task B1: Search header + asymmetric labels

**Repo:** numan

- [ ] Read `src/cmd/search.rs` and `Resolver::{has_compatible_version, classify_version, diagnose_package}`.
- [ ] Write failing tests for:
  - Header line including detected Nu (from `NuPaths` / PATH via existing `current_nu`).
  - Plugin incompatible row shows short hard verdict (reuse `Incompatibility::short_label`).
  - Module compatible row does **not** claim universal success; include `verified_with` when non-empty.
- [ ] Implement display changes in `search.rs` only (keep `--all` behavior; refine labels rather than new flags unless necessary).
- [ ] Align footer copy with audit: point to `numan info <id>`, not `registry sync`, for Nu-mismatch hides.
- [ ] `cargo test cmd::search` / related; clippy.
- [ ] Commit: `Make search compat labels environment-aware and type-honest`

### Task B2: Align install error copy with search

**Repo:** numan

- [ ] Read `Resolver::format_resolve_error` and `append_nu_pin_options` in `src/core/resolve.rs`.
- [ ] Write/adjust tests so Nu-too-new messaging:
  - Mentions `numan setup nu --version {pin}` (already present; keep).
  - States nothing was installed when called from install path (install transaction / cmd layer if needed).
  - Does **not** suggest `registry sync` as the ABI fix.
- [ ] Tighten wording to match audit (plugin minor match; PATH untouched). Prefer editing strings + hints in `resolve.rs` / `hints.rs`.
- [ ] Confirm `src/install/transaction.rs` surfaces resolver errors unchanged (or map once).
- [ ] Commit: `Align Nu mismatch install errors with search facts`

### Task B3: Harden `numan try` against empty/incompat catalog

**Repo:** numan

- [ ] Read `src/cmd/try_cmd.rs` (`STARTERS`, resolve/install/activate flow, `nu_pin_offer`).
- [ ] Write failing tests for: no compatible starter → clear offer (`setup nu --version` or sync) without silent Nu switch; `--yes` still refuses silent switch.
- [ ] Update starter table if registry IDs/Nu minors drifted (current starters reference `abusch/nu_plugin_semver` / `vyadh/nutest`).
- [ ] Commit: `Harden numan try failure paths for Nu mismatch`

### Task B4: Doctor reports PATH vs managed Nu

**Repo:** numan

- [ ] Read `src/cmd/doctor.rs` Nu/path checks and `tests/doctor_test.rs`.
- [ ] Add finding(s): PATH Nu version, managed Nu under root (if present), trust root id for official when configured.
- [ ] Tests for report-only output.
- [ ] Commit: `Doctor reports PATH and managed Nu versions`

---

## Workstream C — Catalog depth Batch 1 (Bet 1)

### Task C1: Re-verify priority plugin Nu pins in upstream Cargo.toml

**Repo:** numan-plugins (read + notes)

- [ ] For the priority candidates from the six-month strategy audit
  (`yybit/nu_plugin_compress`, `Neuron-Mr-White/nu_plugin_terminal_qr` /
  `FMotalleb/nu_plugin_qr_maker`, `kaathewisegit/nugins` (`nu_plugin_endecode`),
  `JosephTLyons/nu_plugin_units`, `ArmoredPony/nu_plugin_hashes`,
  `amtoine/nu_plugin_kdl`, `cptpiepmatz/nu-jupyter-kernel`
  (`crates/nu_plugin_plotters`) / `Euphrasiologist/nu_plugin_plot`,
  `devyn/nu_plugin_dbus`): record actual `nu-plugin` / `nu-protocol` versions,
  tag status, and archive shape from each upstream tag or commit.
- [ ] Do not include core / bundled Nu plugins or the `clip` / `clipboard`
  native command in this catalog work; those ship with Nu (see Task D1).
- [ ] Drop any that fail gates (pre-0.112, NO_RELEASE, broken Windows, etc.) into deferred notes in `docs/backlog.json` or intake-state.
- [ ] Commit notes only if backlog metadata changes.

### Task C2: Promote first Batch 1 plugin end-to-end

**Repos:** numan-plugins → numan-registry (numan only if display bugs found)

- [ ] Add one verified plugin to `numan-plugins/manifest.json` `active[]` with `nu_version`, `verified_with`, tag, excludes.
- [ ] Run / dispatch build-plugins workflow; confirm release assets + `spec-*.json`.
- [ ] Copy/fetch spec into `numan-registry/specs/`; run `python scripts/add-package.py --spec … --write` (after Task A1 so `source` lands).
- [ ] Lifecycle-prove on at least one OS: clean `NUMAN_ROOT`, real Nu matching constraint:
  `search → info → install → activate → doctor → list → remove → gc`.
- [ ] Sign/publish per registry process.
- [ ] Record prove results in PR description.
- [ ] Repeat for remaining Batch 1 only after the first handoff is boring.

### Task C3: Manifest vs index Nu constraint lint (Stage 2 slice)

**Repo:** numan-registry (script)

- [ ] Add a small checker (extend `add-package.py` or new `scripts/lint-manifest-index.py`) that compares plugins' declared Nu range vs index entry when both known.
- [ ] Wire into CI or document manual gate in registry PR template.
- [ ] Commit: `Lint Nu constraints between plugin specs and index`

### Task C4: Script lifecycle-prove (Stage 1 slice)

**Repo:** numan-registry or numan-plugins (pick one owner; prefer registry docs + script that invokes installed `numan`)

- [ ] Add scripted acceptance: given package id + Nu binary, run the prove sequence against temp `--root`.
- [ ] Fail nonzero on any step; print which step failed.
- [ ] Document in registry README / intake docs.
- [ ] Commit: `Add lifecycle-prove script for registry intake`

---

## Workstream D — Core plugins detect (secondary)

### Task D1: Search/info recognition for core plugin names

**Repo:** numan

- [ ] Define static table of core plugin ids/names (`polars`, `formats`, `gstat`, `query`, `inc`) and the `clip` / `clipboard` native command in a small module (e.g. `src/core/core_plugins.rs`) or const in `search.rs`.
- [ ] Failing test: `search polars` returns a row labeled `core (ships with Nu)` and does not offer install as a registry package; `search clip` is labeled `native command (ships with Nu)`.
- [ ] `activate` path: if binary present beside Nu / on `NU_PLUGIN_DIRS`, allow register via existing activate machinery; otherwise doctor/info points to Nu channel.
- [ ] Commit: `Detect core Nu plugins in search without owning install`

---

## Suggested execution order

1. **A1 → A2** (provenance pipeline + info) — unblocks honest trust copy while catalog is small.
2. **B1 → B2** (search/install honesty) — upgrades code that already half-exists.
3. **C2 first plugin** (catalog) — proves handoff with A1 in place.
4. **B3 → B4** (try + doctor polish).
5. **C3 → C4** (make intake enforceable).
6. **C2 remaining Batch 1**.
7. **D1** when search empty-results for "polars" becomes a support issue.

## Explicitly deferred (do not schedule here)

- Side-by-side Nu profiles
- Phase 5.2 client source builds
- Lane 3 maintained forks
- Intake Stages 4–6 / self-serve publishing / registry website
- Donated upstream GitHub Action RFC (goodwill track; after C3/C4)

## Verification gates (every numan PR)

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

Real-Nu when touching activate/try/prove:

```bash
nu --version   # must be present; ignored tests no-op without it
cargo test -- --ignored
```
