# Numan Consolidated Multi-Repo Roadmap

**Authority:** This is the single cross-repo plan for remaining work toward Numan 1.0.
Repo-local roadmaps keep operational detail and should link here:

- [`numan-plugins/docs/roadmap.md`](https://github.com/tonythethompson/numan-plugins/blob/master/docs/roadmap.md)
- [`numan-registry/docs/roadmap.md`](https://github.com/tonythethompson/numan-registry/blob/main/docs/roadmap.md)
- Prior client draft: [`2026-07-29-remaining-roadmap.md`](2026-07-29-remaining-roadmap.md) (superseded by this doc)
- Intake automation endgame: [`docs/registry-intake-roadmap.md`](../registry-intake-roadmap.md)

## Repository Split

| Repo | Owns |
|------|------|
| `numan` | Client, UX, local state, Nu integration, release packaging |
| `numan-plugins` | CI-built plugin binaries for upstreams without compliant release assets |
| `numan-registry` | Signed official catalog, intake evidence, staging, production signing |

**Operating rule:** catalog depth flows `numan-plugins → numan-registry → numan`.

- Client work must not paper over a missing registry artifact.
- Registry work must not trust a plugin build until the hardened pipeline has produced immutable assets and specs.
- Plugin builds never publish registry changes.

---

## Current Baseline (2026-07-30)

### Client (`numan`)

- 0.1.x line is feature-complete for core product surface: signed registries, inert installs, plugin/module activation, update/remove/gc, snapshots, doctor, completions, nupm import/diff, crates.io, winget automation.
- Compat UX (`search` filtered by default, `try`, managed Nu pin offer) is in place; keep it honest as the catalog grows.
- Active-plugin update remains exact-`NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` opt-in.

### Registry (`numan-registry`)

- Production publication is live via the protected `Production registry` workflow.
- Source-tree `registry/index.json.sig` remains a placeholder by design.
- Intake tooling: spec scaffold, SHA256 download, schema/validate, secrets scan, preflight, Numan parser check, manifest/index Nu-constraint lint.
- Stage 1 lifecycle evidence is mandatory for activatable package promotion.
- Live candidate truth: [`intake-candidates.md`](https://github.com/tonythethompson/numan-registry/blob/main/docs/intake-candidates.md) (synced from `docs/intake-state.json`).

### Plugins (`numan-plugins`)

- Hardened build pipeline requires manual dispatch with a non-empty package list.
- Manifest entries pin human-facing tags and immutable `source_commit`.
- Publication refuses existing release tags/assets; changed bytes need a new version or explicit build revision.
- Demand-ranked source-only queue: `docs/backlog.json`.
- **Wave 1 closed:** [PR #4](https://github.com/tonythethompson/numan-plugins/pull/4) merged; assets published; [numan-registry#32](https://github.com/tonythethompson/numan-registry/pull/32) merged; staging green; lifecycle-prove OK on Linux x86_64 (Nu 0.113.1 / 0.112.2); production published 2026-07-31; client smoke OK against official registry.

---

## P0 Critical Path: Finish Catalog Wave 1 End-To-End

Do these in order. Registry intake and client smoke wait on plugins publication.

### A. `numan-plugins` — merge, build, verify

- [x] Merge PR #4 after review and green checks; pull merge commit into `master`.
- [x] Merge Windows Recheck `shell: bash` fix ([PR #8](https://github.com/tonythethompson/numan-plugins/pull/8)).
- [x] Dispatch `build-plugins` manually with only:
  `nu_plugin_port_extension,nu_plugin_image`.
- [x] Confirm workflow checks each upstream tag against recorded `source_commit`.
- [x] Confirm all expected target assets exist; no pre-existing release/asset was replaced.
- [x] Confirm generated specs preserve `source.rev` as the immutable upstream commit.
- [x] Download generated `spec-*.json` artifacts for registry intake.
- [x] Do not rebuild existing releases unless a new version or explicit build revision was chosen.
- [x] Do not publish any registry changes from this repo.
- [x] Merge release upload-by-id fix ([numan-plugins#12](https://github.com/tonythethompson/numan-plugins/pull/12)) so future waves avoid the softprops draft race.

**Wave 1 packages:**

| Package | Version | Notes |
|---------|---------|-------|
| `FMotalleb/nu_plugin_port_extension` | 0.113.1 | On `master` via PR #4 |
| `FMotalleb/nu_plugin_image` | 0.112.2 | On `master` via PR #4 |

**Handoff contract to registry (every successful build wave):**

- generated `spec-*.json` artifacts
- release URLs hosted by `numan-plugins`
- immutable upstream `source.rev` values
- upstream tags for human-facing provenance
- target list and exclusions
- Nu compatibility and `verified_with` values

### B. `numan-registry` — intake, evidence, publish

- [x] Fetch `spec-*.json` from the successful plugins build run.
- [x] Place specs under `specs/` on a focused registry branch (no unrelated catalog targets).
- [x] Run `python scripts/add-package.py --spec specs/<file>.json --write` for each package (script downloads + computes SHA256; never hand-type hashes).
- [x] Run `python scripts/sync-intake-candidates.py` if intake-state/index changes need the human doc refreshed.
- [x] Local checks:
  - `python scripts/scan_for_secrets.py`
  - `python scripts/preflight.py`
  - `python scripts/validate.py --index registry/index.json --sig registry/index.json.sig --pub keys/official.pub --skip-artifacts`
  - `cargo run --locked --manifest-path tools/numan-parser-check/Cargo.toml -- registry/index.json`
  - `python scripts/lint-manifest-index.py --index registry/index.json --manifest ../numan-plugins/manifest.json`
- [x] Open PR with specs, index diff, intake doc updates, and test evidence ([numan-registry#32](https://github.com/tonythethompson/numan-registry/pull/32)).
- [x] Run staging after merge (green on `main` merge commit `4e3ae77`).
- [x] Run `lifecycle-prove` against a real Nu matching each package constraint before production (Linux x86_64: Nu 0.113.1 for port_extension, Nu 0.112.2 for image).
- [x] Dispatch production only after validation is green and reviewer approval exists ([run 30600799679](https://github.com/tonythethompson/numan-registry/actions/runs/30600799679)).

### C. `numan` — client smoke after production sync

- [x] Fresh smoke on a clean root:
  `init → registry sync → search → info → install → activate → doctor → list → deactivate → remove → gc`
- [x] Confirm Wave 1 packages appear with honest Nu/platform filtering on the machine under test.
- [x] Confirm `numan try` still fails clearly if no compatible starter exists (never silent Nu switch). (`try` picked a compatible starter; `search nu_plugin_image` on Nu 0.113.1 hid the incompatible package and `--all` showed `[needs Nu >=0.112.0 <0.113.0]`.)

---

## P1 Catalog Growth Loop

Repeat this loop for each subsequent wave. Prefer one or two plugins at a time.

### Promotion gates (`numan-plugins`)

Move a candidate from `docs/backlog.json` → `manifest.json` `active[]` only when recorded:

- [ ] Upstream reachable (or archive state explicitly accepted)
- [ ] Tag resolves to recorded 40-char lowercase `source_commit`
- [ ] `nu-plugin` / `nu-protocol` dependency versions known
- [ ] Nu compatibility range is minor-scoped and matches those deps
- [ ] `plugin_bin` confirmed
- [ ] Windows locked build succeeds, or Windows excluded with concrete reason
- [ ] Linux/macOS expected to work, or excluded with concrete reasons
- [ ] Exact-version Nu command-discovery smoke succeeds where practical
- [ ] No existing `numan-plugins` release tag/assets for that package version
- [ ] README active list and backlog notes updated in the same PR

### Wave 2 research queue (`numan-plugins`)

Source: `docs/backlog.json`. Research before promoting:

- [x] `devyn/nu_plugin_dbus` — `PRE_0_112` (nu-plugin 0.101.0; libdbus; not Windows)
- [x] `PhotonBursted/nu_plugin_vec` — `PRE_0_112` (nu-plugin 0.105.1; pure Rust; Windows expected)
- [x] `drbrain/nu_plugin_prometheus` — promoted to `active[]` as `v0.12.0` ([numan-plugins#15](https://github.com/tonythethompson/numan-plugins/pull/15); nu-plugin 0.114.1, commit `3fed1d93…`). Windows locked green; `aarch64-unknown-linux-gnu` excluded after openssl-sys cross failure ([#16](https://github.com/tonythethompson/numan-plugins/pull/16)). Build re-dispatched; registry intake after successful release specs.
- [x] `fdncred/nu_plugin_emoji` — **PROMOTED** v0.23.0 (nu-plugin 0.114.0; pure Rust; [numan-plugins#17](https://github.com/tonythethompson/numan-plugins/pull/17), [numan-registry#36](https://github.com/tonythethompson/numan-registry/pull/36); lifecycle-prove OK Windows/Nu 0.114.1)
- [x] `fdncred/nu_plugin_json_path` — **PROMOTED** v0.24.0 (nu-plugin 0.114.0; pure Rust; [numan-plugins#17](https://github.com/tonythethompson/numan-plugins/pull/17), [numan-registry#36](https://github.com/tonythethompson/numan-registry/pull/36); lifecycle-prove OK Windows/Nu 0.114.1)
- [x] `fdncred/nu_plugin_parquet` — **PROMOTED** v0.24.0 (nu-plugin 0.114.0; pure Rust; [numan-plugins#17](https://github.com/tonythethompson/numan-plugins/pull/17), [numan-registry#36](https://github.com/tonythethompson/numan-registry/pull/36); lifecycle-prove OK Windows/Nu 0.114.1)
- [x] `yybit/nu_plugin_compress` — `PRE_0_112` (nu-plugin 0.103.0; tag 0.2.5 exists but too old)
- [x] `JosephTLyons/nu_plugin_units` — `PRE_0_112` (nu-plugin 0.106.1; tag v0.1.8 exists but too old)
- [ ] `galuszkak/nu_plugin_bigquery` — peeked: tag `v0.2.0` pins nu-plugin 0.112.2 (eligible Nu minor) but needs Google credentials for meaningful lifecycle proof
- [x] `jcornaz/nu_plugin_from_beancount` — researched 2026-07-31: `PRE_0_112` (nu-plugin 0.76)
- [x] `dam4rus/nu_plugin_nuts` — researched 2026-07-31: `PRE_0_112` (nu-plugin 0.110.0)

For each, record: supported Nu minor compatibility, native system deps, Windows buildability, simple command-discovery smoke.

### Registry catalog maintenance

- [ ] Keep `docs/intake-state.json` as editable candidate source; regenerate `docs/intake-candidates.md`.
- [ ] Keep every live entry tied to provenance: upstream URL, source revision, asset URL, hashes, Nu constraints, targets, package type.
- [ ] Preserve upstream-vs-mirror distinction; prefer upstream byte-stable archives when available.
- [ ] Track outreach in `docs/upstream-release-outreach.md`.
- [ ] Revisit blocked packages when upstreams add archives, Nu pins, or platforms (see intake-candidates "Blocked for now").

### Deferred plugin candidates (do not promote yet)

- Pre-0.112 plugins (unless Numan re-supports older Nu minors)
- Repos with no release tag (unless commit-snapshot policy is explicitly adopted)
- Bare binary uploads / unsupported archive layouts
- Plugins needing heavy native services or credentials until lifecycle proof can be automated meaningfully

---

## P1 Client Priorities (`numan`)

### Compat UX stays honest as catalog grows

- [ ] `search` filtered by detected Nu/platform by default; `--all` explains plugin ABI mismatch clearly
- [ ] `info` shows provenance, verification metadata, type, targets, Nu constraints without implying security approval
- [ ] `try` aligned with live catalog; clear failure when no compatible starter; never silent Nu switch
- [ ] Install errors explicit that nothing was installed on Nu/platform resolution failure
- [ ] Refresh doctor checks as catalog growth exposes setup failures: PATH Nu drift, managed Nu pin drift, official trust drift, stale plugin activation, pending lifecycle journals

### Active plugin update default-on decision

- [ ] Keep exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` until real-Nu active-update evidence is boring on Ubuntu, Windows, and macOS
- [ ] Keep `update` free of direct Nu registration ownership (activate/deactivate boundary owns Nu callbacks)
- [ ] Before default-on: failure-before-lifecycle, deactivate failure, upgrade rollback, activate recovery, and real-Nu matrix evidence
- [ ] Same PR updates `docs/active-plugin-gate.md`, `AGENTS.md`, README, changelog

### Install-only package types

- [ ] Scripts and completions stay install-only until activation contracts are designed and tested
- [ ] Completions: decide managed vendor autoload vs shell-specific hints vs `numan completions` adjunct
- [ ] Scripts: define execution/discovery boundaries before any Nu config mutation
- [ ] Lifecycle evidence per package type before changing README support tiers

### Source builds (Phase 5.2)

- [ ] Keep deferred while `numan-plugins` covers highest-demand source-only plugins via CI
- [ ] When revived: explicit consent, dependency disclosure, deterministic paths, failure cleanup, no hidden Nu activation
- [ ] Never mix source builds with registry catalog expansion PRs

### Distribution polish

- [ ] Monitor winget automation after each GitHub release
- [ ] Defer macOS/Linux package managers until a verified maintained formula/tap/channel exists
- [ ] Keep release docs version-agnostic where possible (README ships in crates.io + tagged archives)
- [ ] Per release dry-run: `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`, package checks, release-note extraction, archive smoke

---

## P2 Intake Automation (`numan-registry` + `numan`)

Stage 1 (lifecycle harness) is done. Remaining stages follow [`docs/registry-intake-roadmap.md`](../registry-intake-roadmap.md). Do not block Wave 1 on these.

### Stage 2: Stronger local lint

- [x] Actionable errors: missing metadata, duplicate targets, unknown triples, unsupported archive suffixes, missing activation declarations, malformed Nu constraints, source provenance mismatches
- [x] Deterministic lint output for before/after PR comparison
- [x] PR template asks for lint, parser-check, and lifecycle evidence ([numan-registry#31](https://github.com/tonythethompson/numan-registry/pull/31))

### Stage 3: Repo discovery

- [x] Read-only discovery from GitHub repo, release URL, or local checkout (`scripts/discover.py`)
- [x] Detect `nupm.nuon`, layouts, Cargo metadata, assets, license, homepage, tags, Nu deps, platform matrix
- [x] Separate discovered facts from guessed fields and maintainer decisions

### Stage 4: Candidate generation

- [x] Draft specs only (not committed registry entries) (`scripts/gen_candidate.py`)
- [x] Provenance per inferred field; unresolved decisions marked explicitly
- [x] Stable, reviewable generated JSON

### Stage 5: Validation reports

- [x] Machine + human validation evidence per candidate (`scripts/validate_candidate.py`)
- [x] Cover download, hash, archive layout, install, activation readiness, doctor, list, deactivate/remove/gc, final state
- [x] Production secrets unavailable to validation jobs

### Stage 6: Registry PR generation

- [x] PR branch from validated specs + evidence (`scripts/open_intake_pr.py`)
- [x] Summary: type, provenance, targets, lifecycle results, limitations, publish plan
- [x] Human review and protected signing remain mandatory

---

## Ongoing Safety And Pipeline Hygiene

### `numan-plugins`

- [ ] Third-party Actions pinned to reviewed commit SHAs
- [ ] Workflow permissions read-only except release publication
- [ ] macOS runner labels current and tested
- [ ] Deterministic archive tests (`.zip`, `.tar.gz`)
- [ ] Release-absence tests (existing tags/assets fail before upload)
- [ ] Strict manifest validation (duplicates, missing targets, malformed commits, tag-to-commit drift)
- [ ] Strict generated-spec validation (packaged SHA records, complete target coverage)

### `numan-registry`

- [ ] Never commit or print private key material
- [ ] Never treat source-tree placeholder signature as production evidence
- [ ] Never publish before artifacts are hash-pinned and reviewable
- [ ] Never add lifecycle-activatable packages without lifecycle evidence
- [ ] Never mix catalog expansion with workflow/signing refactors unless the catalog change depends on the safety change

---

## Unified 1.0 Gate

Ship 1.0 when **all** of the following are true:

| Area | Criterion |
|------|-----------|
| Catalog depth | Official registry has enough packages for first-use demos to feel real on Windows, macOS, and Linux |
| Lifecycle | install / activate / update / deactivate / remove / gc / snapshots / doctor have green local + CI evidence across the OS matrix |
| Intake | Routine package additions are spec-driven and reproducible; no ad hoc hand editing for routine packages |
| Evidence | Every activatable package has lifecycle evidence or a documented exception |
| Compat UX | Package compatibility failures are discoverable before install |
| Trust | Production signing is boring, protected, auditable; mirrors and outreach status are clear |
| Client sync | `numan registry sync` + search/info/install reflect the catalog accurately |
| Distribution | Release packaging and winget updates are routine |
| Risk | No open P0/P1 lifecycle, trust, or data-loss issues |

### Repo-local health checks (supporting)

**Plugins healthy when:** one or two plugins can be added without touching publication safety code; every active manifest entry traces to upstream tag + immutable commit; every release asset is immutable and hash-pinned downstream; backlog explains promote/defer/block; registry receives specs needing no manual repair.

**Registry healthy when:** same as 1.0 registry rows above, plus meaningful multi-OS coverage in the live catalog.

---

## Post-1.0 Features

Captured from product discussion; subject to change.

### Side-by-side Nu version management (`numan use`)

**Vision:** Numan manages multiple Nu versions simultaneously. The user picks the
Nu version that has the plugins they need, and switching is instant.

- `numan use <version>` — switch active Nu (errors with a hint to run `setup nu <version>` if the version is not installed; never auto-downloads)
- `numan use latest` — switch to newest installed version
- `numan use list` — show installed versions and mark the active one (no per-version plugin counts in 0.1.x; counts arrive once per-version activation ships — see Vision below)
- Storage: `<root>/tools/nushell/<version>/nu` (immutable, one dir per version)
- Active marker: JSON file at `<root>/nu_state/active-version.json` (shape `{"version": "X.Y.Z"}`, written atomically by `version_manager::write_active_version`; cleared atomically before removing the versioned tree so the marker cannot dangle at a missing binary)
- **PATH (Unix):** `numan setup nu` writes ``export PATH="$HOME/.local/bin:$PATH"`` to the user's shell profile (`ensure_local_bin_on_path`) AND creates/refreshes a ``~/.local/bin/nu`` symlink to the active managed binary via `persist_user_path_unix` (`std::os::unix::fs::symlink` on the resolved canonical binary; rejects the call if ``~/.local/bin/nu`` already points at a different managed install unless `--skip-path` is passed). Windows: appends the binary's parent directory to the user PATH via `persist_path_dir_windows` instead.
- **PATH (process-only):** `numan setup nu` also calls `prepend_process_path` for the lifetime of the current process so the freshly-installed Nu wins over PATH-Nu until the next login.
- **Active marker ownership:** `numan setup nu` calls `version_manager::write_active_version` after a successful install (`:744`). `numan use <x.y.z>` / `latest` calls it too, under the root mutation lock and after a PreMutation snapshot. `numan use` does NOT touch PATH or the symlink — only the marker.

**Per-version activation sets:**

- SHIPPED: each `PluginActivation` record carries `nu_version: String` (`src/state/lockfile.rs:44`). Compat check at activate-time continues to validate runtime Nu version against the recorded one.
- > **Vision only — not yet shipped.** Per-version activation lookup: re-derive the activation set keyed on the version-prefix whenever the active Nu changes, so locked plugins for 0.113 stay loaded while 0.114-only ones stay locked.
- > **Vision only — not yet shipped.** Switching Nu activates/deactivates plugins for that version automatically.
- > **Vision only — not yet shipped.** `numan use 0.113.1` → activates plugins compatible with 0.113, deactivates 0.114-only ones.

**Numan-level aliases (optional):**

- > **Vision only — not yet shipped.** `numan alias work 0.113.1` records `work` as an alias for `0.113.1`; `numan use work` resolves and switches.
- > **Vision only — not yet shipped.** Persisted in Numan config; survives shell changes.
- > **Vision only — not yet shipped.** Shell-level aliases (`alias nu113 = numan use 0.113.1`) remain a user option — this is the recommended mechanism until `numan alias` ships.

**Catalog implication:** > **Vision only — not yet shipped.** Backfilling older Nu versions (0.112, 0.113) becomes valuable because each version is a switchable "profile" rather than a dead end. Pre-0.112 plugins remain deferred unless product re-scopes.

**Backfill data:** `numan-plugins/docs/backlog.json` (schema v1; verified outside this repo) tracks ALL release versions per plugin with their Nu minor compatibility. The `backfill_targets` field lists Nu minors that have eligible plugin versions not yet in the registry. Source: awesome-nu + manual discovery. Entries marked `NEEDS_RESEARCH` need their version history filled in before promotion.

### Explicitly NOT in scope for post-1.0 (requires Nu upstream)

- Simultaneous multi-version plugin hosting (running 0.113 and 0.114 plugins in
  one session) — requires Nu-core plugin protocol bridging / remote plugin hosts
- Plugin ABI translation layer between Nu versions

---

## Explicitly Deferred (All Repos)

- Source builds hidden inside registry intake
- Publishing registry entries without review
- Calling packages "approved" or "audited" solely because they are in the official registry
- Broad maintained forks of upstream plugins before the catalog pipeline has exhausted ordinary CI-built upstream tags
- Pre-0.112 plugin support (unless product chooses older Nu minors again)
- macOS/Linux system package managers without a verified maintained formula

---

## Suggested Execution Order

1. **Done:** Wave 1 end-to-end (plugins build → [registry#32](https://github.com/tonythethompson/numan-registry/pull/32) → lifecycle-prove → production → client smoke). Plugins release upload-by-id fix ([#12](https://github.com/tonythethompson/numan-plugins/pull/12)) merged.
2. **Done:** `nu_plugin_prometheus@v0.12.0` + Nu 0.114 batch (`nu_plugin_emoji@0.23.0`, `nu_plugin_json_path@0.24.0`, `nu_plugin_parquet@0.24.0`) — built, registry intake ([#36](https://github.com/tonythethompson/numan-registry/pull/36)), production live, lifecycle-prove OK on Windows/Nu 0.114.1.
3. **Next:** Continue catalog growth from backlog; keep client compat UX and doctor honest against new packages.
4. **Parallel (non-blocking):** Intake-candidates / outreach maintenance; winget release monitoring.
5. **Done:** Intake Stages 3–6 ([numan-registry#37](https://github.com/tonythethompson/numan-registry/pull/37) merged 2026-07-31).
6. **Later:** Install-only activation contracts; active-update default-on decision; Phase 5.2 source builds only after intake is steady.
7. **1.0:** When the unified gate above is green.
