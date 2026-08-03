# Numan — Nushell Package Manager

## Overview
Numan is a cross-platform, production-grade Nushell package manager CLI written in Rust. It handles plugins, modules, scripts, and completions with verified artifacts, compatibility resolution, lockfiles, rollback, and interoperability with nupm.

## Build & Test
```bash
# Build
cargo build

# Run
cargo run -- search <query>
cargo run -- info <owner/name>
cargo run -- list
cargo run -- nupm status --nupm-home <path>
cargo run -- nupm inspect <package-path>

# Test (419 tests)
cargo test

# Test single module
cargo test core::platform
cargo test core::package
cargo test core::resolve
cargo test state::lockfile
cargo test cmd::activate

# Lint / format (CI enforces -D warnings and fmt --check)
cargo clippy -- -D warnings
cargo fmt
```

CI runs `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`, and a real-Nu acceptance job (`cargo test -- --ignored` with Nu 0.113 on PATH) on Ubuntu, Windows, and macOS.

## Project Structure
```
src/
  main.rs              — CLI entry point (clap-based)
  config.rs            — Config load/save, root resolution
  core/
    platform.rs        — OS/arch/target detection
    package.rs         — ScopedId, Package, VersionEntry, Artifact types
    nu_version.rs      — Nu version detection and constraint matching
    registry.rs        — Registry index load/search/verify
    official_registry.rs — Built-in official trust root + signature verification
    trust.rs           — Ed25519 trust store and signature verification
    integrity.rs       — SHA256 compute and verify
    resolve.rs         — Version resolution with strict plugin constraints
  cli.rs               — Root `Cli` / `Commands` (clap derive; used by main + completions)
  cmd/
    search.rs          — Search subcommand (`--all` shows incompatible packages)
    info.rs            — Info subcommand (per-version compatibility markers)
    list.rs            — List subcommand
    registry.rs        — Registry management subcommands
    activate.rs        — Plugin + module activation (Phase 3 & 4); public entry: execute_with_candidate_runner
    init.rs            — `numan init [--refresh]`: Nu probe, paths cache, auto-configures official registry
    doctor.rs          — `numan doctor [--scan] [--json]`: repairs by default; `--scan` report-only (Phase 7.2; spec: docs/numan-doctor.md)
    snapshot.rs        — `numan snapshot list|inspect|delete|rollback` (Phase 5.3)
    deactivate.rs      — Plugin + module deactivation: journaled plugin unregister (`execute_with_unregistrar`); module full/partial (Phase 4 / Issue #22 PR2)
    plugin_lifecycle.rs — Activate/deactivate-owned lifecycle boundary exposed to opt-in update orchestration (Issue #22 PR3)
    update.rs          — `numan update [--check] [pkg]`: upgrades; active plugins orchestrate deactivate→upgrade→activate only with exact env opt-in (Phase 5 / Issue #22 PR3)
    remove.rs          — `numan remove [--force] <pkg>`: remove from lockfile + delete payload (Phase 5); `--force` bypasses module activation only (active plugins always gated until deactivate, Issue #22)
    gc.rs              — `numan gc [--dry-run]`: delete orphaned payload directories (Phase 5)
    nupm.rs            — `numan nupm status|inspect|import|diff`: nupm discovery + import + drift (Phase 6.1–6.3)
    completions.rs     — `numan completions <shell>`: bash/fish/zsh/powershell/nushell scripts (Phase 7.3)
    setup.rs           — `numan setup nu [VERSION]|remove|path|use <path>` + `setup loader`: Nushell bootstrap + nushell-loader install
    try_cmd.rs         — `numan try [--no-activate]`: curated starter install + activate for current Nu
    use_cmd.rs         — `numan use <version>|latest|list`: activates a previously installed managed Nu version (no auto-download); writes the active-version marker after a PreMutation snapshot under the root mutation lock
    nu_pin_offer.rs    — Shared TTY offer to `setup nu <version>` + `init --refresh` on Nu mismatch
  install/
    download.rs        — HTTP download with progress
    extract.rs         — tar/zip/xz archive extraction
    transaction.rs     — Full install flow (resolve→download→verify→extract→lockfile)
  state/
    lockfile.rs        — Lockfile v2: PluginActivation, ModuleActivation, revision_id, payload_sha256, compute_revision_id()
    journal.rs         — Plugin pending-activation journal for crash recovery
    plugin_deactivate_journal.rs — Plugin pending-deactivate journal (`pending-plugin-deactivate.json`) for crash recovery (Issue #22 PR2)
    active_plugin_mutation.rs — Fail-closed exact-`1` opt-in for active update orchestration (Issue #22 PR3)
    autoload_journal.rs — Module autoload journal (PendingAutoload, Prepared→Replaced stages) for crash recovery (Phase 4)
    autoload_recovery.rs — Command-independent PendingAutoload reconciliation into lockfile + derived autoload state
    autoload_state.rs  — Derived autoload-state projection (NOT authoritative; lockfile is ground truth) (Phase 4)
    lifecycle_journal.rs — pending-lifecycle.json for update/remove/nupm_import crash recovery (Phase 5–6)
    snapshot.rs        — Immutable activation snapshots (`create_snapshot`, `list_snapshots`, etc.)
    rollback.rs        — Journaled restore of Numan-owned state to a snapshot
    nupm_import.rs     — nupm-import provenance (`state/nupm-imports.json`, Phase 6.2)
  nu/
    bootstrap.rs        — download/install official Nushell release under tools/nushell
    paths.rs           — Nu path cache (detect, load, save, validate_drift)
    autoload.rs        — render_use_statement, generate_autoload_content, FakeCandidateRunner, managed-file ops (Phase 4)
  util/
    atomic.rs          — write_json_atomic helper (tempfile+persist)
    fs_safety.rs       — OWNERSHIP_MARKER, acquire_mutation_lock (advisory fd_lock mutex), assert_managed_file_owned (Phase 4)
    hints.rs           — Canonical `fix` hint strings aligned with docs/numan-doctor.md (Phase 7.3)
  nupm_compat/         — nupm discovery, import, drift (Phase 6.1–6.3); contract: docs/nupm-compatibility.md (compat-schema-v1)
    drift.rs           — compare_import, count_drifted_imports, DriftStatus (Phase 6.3)
    import.rs          — safe payload copy, lifecycle-journaled import transaction
    schema.rs          — COMPAT_SCHEMA_VERSION, parser caps, pinned nupm revision
    metadata.rs        — compat-schema-v1 metadata parser (ParsedMetadata, BehaviorFlags)
    classify.rs        — four-step classifier (NupmCompatibility)
    discovery.rs       — NupmHomeResolution, scan_nupm_home, inspect_path
    walk.rs            — bounded safe path walks (symlink_metadata)
    report.rs          — NupmStatusReport, NupmInspectionReport formatters
docs/
  nupm-compatibility.md — versioned nupm interoperability contract (authority for Phase 6)
  PACKAGING.md          — winget release checklist
  RELEASING.md          — version bump, tag, CI gates
  snapshots-and-rollback.md — snapshot CLI scope and rollback guarantees
tests/
  fixtures/nupm/       — supported/rejected fixture corpus for parser/classifier tests
  init_test.rs          — `numan init` / `init --refresh` (vendor drift, managed-file revalidation)
  completions_test.rs  — shell completion script generation (Phase 7.3)
  doctor_test.rs       — `numan doctor` default repairs, `--scan` report-only, journal checks (Phase 7.2)
  nupm_compat_test.rs  — Phase 6 integration tests (T13–T25, import/drift/manifest/activation/platform)
  nupm_real_nu_test.rs — Phase 6.4 real-Nu #[ignore] acceptance tests (run with `cargo test -- --ignored`)
  plugin_lifecycle_real_nu.rs — Issue #22 smoke marker (points at Stage 1 + active-update suite)
  plugin_active_update_real_nu.rs — Issue #22 active-update real-Nu matrix (fixture dual-version registry; Nu 0.113.x; skipped on default PR ignored job)
  official_registry_stage1.rs — Windows production-registry Stage 1 (workflow_dispatch)
  setup_test.rs        — `numan setup loader` install and config.nu snippet detection
  setup_nu_test.rs     — `numan setup nu` managed binary discovery and injected installer
```

## Key Conventions
- **Crate name**: `numan-cli`, **binary name**: `numan`
- **Product name**: Numan (capital N in prose, lowercase `numan` for CLI)
- **Edition**: Rust 2021
- **Error handling**: `anyhow` for application errors, `thiserror` for library errors; user-facing fix hints via `util::hints` (match `docs/numan-doctor.md`)
- **Serialization**: `serde` + `serde_json` (JSON) + `toml` (config)
- **CLI**: `clap` with derive macros
- **Platform detection**: `#[cfg(target_env)]` from binary's build target, not `std::env::consts`
- **Trust**: Ed25519 signatures over registry indexes; built-in production trust root for `official` (`src/core/official_registry.rs`); custom registries use `--key <base64-public-key>` via `registry add`
- **Immutability**: install path shape is `<root>/packages/<type>/<owner>/<name>/<version>-<sha8prefix>/` — never overwrite
- **Activate testability**: `execute_with_registrar(args, root, registrar)` for plugins; `execute_with_candidate_runner(args, root, registrar, runner)` for modules — inject fakes in tests, never spawn a real Nu binary in unit tests
- **Deactivate testability**: `execute_with_unregistrar(args, root, unregistrar)` for plugins; `execute_with_candidate_runner_and_unregistrar` when both lanes need fakes — Nu program string is `RM_PLUGIN` with name/config via env only
- **Active-plugin update testability**: `update::execute_with_hooks` injects a `plugin_lifecycle::PluginLifecycle`; Nu registrar callbacks remain owned by the activate/deactivate lifecycle boundary; exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` opt-in, default off
- **Module autoload testability**: `FakeCandidateRunner::success()` / `::failure(msg)` from `nu/autoload.rs` — use as test seam for candidate validation without real Nu
- **Module autoload identity**: Nu executable hash + Nu version + vendor autoload dir + managed file path — all four must match for a module to be considered active
- **Autoload state is NOT authoritative**: `autoload-state.json` is a fast-check projection; the lockfile `module_activation` records are ground truth
- **Managed file ownership**: `OWNERSHIP_MARKER` header identifies Numan-managed files; `assert_managed_file_owned` blocks overwrite of foreign files
- **Mutation serialization**: `acquire_mutation_lock(root)` returns `MutationLock` RAII guard; second acquire on same root fails immediately (non-blocking)
- **Nu invocation**: paths/names only via env vars (`NUMAN_PLUGIN_BINARY`, `NUMAN_PLUGIN_CONFIG`, `NUMAN_PLUGIN_NAME`); the Nu program string is a compile-time constant with no runtime interpolation
- **Activation scope**: `PluginActivation` struct stores `(nu_executable_sha256, nu_version, plugin_registry_path)`; a plugin is "active" only when all three match the current `NuPaths` — bare `bool` would go stale after `numan init --refresh`
- **Journal**: `state/pending-activation.json` written as all-`prepared` before first registration; each entry advances to `registered` atomically before lockfile update; reconciled on next `activate` run if process is interrupted
- **Plugin deactivate journal**: `state/pending-plugin-deactivate.json` (`Prepared` → `Unregistered` → clear lockfile `activation`); reconciled on next `deactivate`; doctor warns `journal.plugin_deactivate_pending`
- **Active version marker**: `nu_state/active-version.json` (`{ "version": "X.Y.Z" }`, optionally `{ "version": "X.Y.Z", "binary_path": "/abs/path/to/nu" }` for off-tree selections). Sole authority for which `tools/nushell/<v>/` is selected. Written by `numan setup nu` and `numan use <version>|latest`. The optional `binary_path` records the resolved off-tree binary when `numan setup nu use <path>` swaps to a user-supplied Nu so subsequent `numan use list` and `find_nu_executable_with_root` can resolve the chosen version even when no on-tree install exists (the field uses `#[serde(default, skip_serializing_if = "Option::is_none")]` so the on-disk shape stays `{ "version": ... }` for on-tree selections and pre-existing markers still load).
- **Atomic writes**: all JSON state files (lockfile, journal, nu_state/paths.json) use `write_json_atomic` (tempfile in same dir + persist) — no partial-write corruption
- **Function signatures**: use `&Path` not `&PathBuf` in function parameters (clippy::ptr_arg is CI-enforced)

## Architecture Rules
1. **Install is always inert** — no Nu integration, only writes to `$NUMAN_ROOT`
2. **Nu integration is activate/deactivate-owned** — only the activate/deactivate lifecycle boundary invokes plugin register/unregister; an explicitly opted-in `update` may coordinate that boundary but must not own or invoke Nu callbacks directly
3. **Source builds require consent** — prompt before clone/build, separate consent scope
4. **Lockfile pins immutable paths** — cached artifacts retained while referenced
5. **Registry trust** — Ed25519 signatures over exact `index.json` bytes; bypass requires `NUMAN_ALLOW_UNSIGNED=1` (dev only)
6. **Artifact SHA256 is mandatory for plugins** — the install transaction bails if `sha256` is missing from a binary artifact
7. **State snapshots before mutation** — `create_snapshot()` before `install`/`update`/`remove`/`activate`/`deactivate`/nupm-import mutations; `numan gc` treats every snapshot's referenced payloads as live roots
8. **Platform triple** — comes from `#[cfg(target_env)]` at compile time, not `std::env::consts` (see `core/platform.rs`; `LIBC` is a compile-time const)

## Development Workflow
1. Create feature branch from `master`
2. Implement with tests
3. `cargo test` — all 419 tests must pass
4. Update AGENTS.md if structure/conventions change
5. Open PR with description

## PR review guidance

Automated and human PR reviewers should follow [`REVIEW.md`](REVIEW.md) for review checklists, severity expectations, and architecture invariants to flag. Keep that file updated when review conventions change; link here rather than duplicating review rules in this doc. Copilot apply-to instructions remain at [`.github/instructions/review.instructions.md`](.github/instructions/review.instructions.md) and must stay aligned with `REVIEW.md`.

## Dependencies
- clap (CLI), clap_complete + clap_complete_nushell (shell completions), serde/serde_json/toml (serialization), reqwest (HTTP), tar/flate2/xz2/zip (archives)
- sha2/hex (integrity), ed25519-dalek/base64 (signatures), semver (versioning)
- dirs (platform paths), git2 (source builds), tempfile (safe extraction)

## Phase Status
- [x] Phase 1: Foundation (types, platform, config, lockfile, registry, trust, CLI skeleton)
- [x] Phase 2: Install transaction (download, verify, extract, lockfile write)
- [x] Phase 3: Activate command (plugin-only; `plugin add` via env-vars; journal recovery; drift detection)
- [x] Phase 4: Module autoload (render_use_statement, candidate validation, managed-file replacement, deactivation, journal recovery, mutation lock)
- [x] Phase 5 (partial): Lockfile v2; `numan update/remove/gc`; pending-lifecycle journal; activation snapshots + rollback CLI ([docs/snapshots-and-rollback.md](docs/snapshots-and-rollback.md)); active-plugin update orchestration remains exact-`1` opt-in with a green 3-OS real-Nu fixture suite ([docs/active-plugin-gate.md](docs/active-plugin-gate.md))
- [ ] Phase 5 (deferred): Source builds (5.2)
- [x] Phase 6.0: nupm compatibility audit + fixture corpus (`docs/nupm-compatibility.md`)
- [x] Phase 6.1: read-only `numan nupm status|inspect` (no import, no nupm mutation, no Nu)
- [x] Phase 6.2: one-way `numan nupm import` (staging, provenance, lifecycle journal; no activation)
- [x] Phase 6.3: drift (`numan nupm diff`), status drift count, manifest import, re-import polish, activation tests
- [x] Phase 6.4: `--exit-on-ineligible`, parser fuzz, Unicode/symlink tests, real-Nu acceptance
- [x] Phase 6 complete: compatibility matrix (`docs/nupm-compatibility.md`); CI acceptance job for `#[ignore]` real-Nu tests
- [x] Phase 7.1: Distribution baseline — GitHub Releases, crates.io, `numan init`, real-Nu CI ([Phase7Plan.md](docs/plans/Phase7Plan.md))
- [x] Phase 7.2: `numan doctor` ([docs/numan-doctor.md](docs/numan-doctor.md))
- [x] Phase 7.3: shell completions + error UX hints + README `--help` audit ([Phase7Plan.md](docs/plans/Phase7Plan.md))
- [x] Phase 7.4: Onboarding path — init checklist, README quick start ([Phase7Plan.md](docs/plans/Phase7Plan.md))
- [x] Phase 7.5: CI hardening — MSRV, cargo deny/package, release gates ([Phase7Plan.md](docs/plans/Phase7Plan.md))
- [x] Phase 7.6: Wider distribution — winget manifests ([docs/PACKAGING.md](docs/PACKAGING.md)); macOS/Linux package-manager support deferred pending a verified formula
- [x] Post-7.6: Official registry production cutover + init auto-configures `official` (v0.1.4)
- [x] Phase 7 complete (polish, CI, distribution) — see [Phase7Plan.md](docs/plans/Phase7Plan.md); toward 1.0: winget merge, registry intake, Phase 5.2/5.5

## Testing
- Unit tests inline with source modules
- Integration tests in `tests/`
- Test-first approach: write test, verify failure, implement, verify pass
- All platform-specific code tested with mock platforms

## Error Patterns
- Use `anyhow::Result` for application code
- Use `thiserror` for library types that callers match on
- Include context with `.context("what failed")` or `?`
- Never panic in library code — return errors

## Git Conventions
- Commits: imperative mood, <72 chars
- Branches: `feature/description`, `fix/description`
- No force-push to `master`
- Squash merge for features

## Cursor Cloud specific instructions

Standard build/test/lint/run commands live in "Build & Test" above and in the README `## Development` section — use those. Notes below are only non-obvious caveats for this environment.

- **Toolchain**: MSRV is `1.88` (`Cargo.toml` `rust-version`, enforced by the CI `msrv` job). Build with the `stable` toolchain (CI uses `dtolnay/rust-toolchain@stable`). The VM default toolchain is set to `stable`.
- **System dependency**: `git2` and `reqwest` pull in `openssl-sys`, which needs the system OpenSSL dev package. `pkg-config` + `libssl-dev` are installed in the VM; without them `cargo build` fails at `openssl-sys`.
- **Clippy scope**: run lint exactly as CI does — `cargo clippy -- -D warnings` (lib + bins only). Do NOT add `--all-targets`: newer stable clippy flags pre-existing test code (e.g. `needless_borrows_for_generic_args` in `src/nupm_compat/import.rs` tests), which is not a CI gate and is not code you should "fix" as part of setup.
- **Nushell for activation/acceptance**: `numan init`, `numan activate`, `numan deactivate`, and the `#[ignore]` real-Nu acceptance tests require a `nu` binary on `PATH`. Nushell `0.113` (matching the CI acceptance job) is installed at `/usr/local/bin/nu`. Run the portable ignored suite with `cargo test -- --ignored` (PR CI skips Stage 1 and `plugin_active_update_real_nu`). **Always run `nu --version` first** — many ignored tests silently `return` (and report `ok`) when no `nu` is on `PATH`, so a "passing" `--ignored` run means nothing unless `nu` is confirmed present. The active-plugin **update** matrix (`tests/plugin_active_update_real_nu.rs`) **hard-fails** unless Nu is 0.113.x; run it via `cargo test --test plugin_active_update_real_nu -- --ignored --nocapture --test-threads=1` or the `active-plugin-update-acceptance` workflow. The Nu binary lives in the VM snapshot, not the update script; if it is missing on a fresh image, reinstall it (see PR #28 for the exact steps) before trusting the acceptance suite.
- **`numan activate` needs Nu's config dir to exist**: activation resolves the plugin registry under `~/.config/nushell` and the vendor autoload dir under `~/.local/share/nushell/vendor/autoload`. On a fresh box these may not exist until Nu has run once; if `activate` errors with "Plugin registry parent directory does not exist", create the dirs (or run `nu -c 'version'`) then `numan init --refresh`.
- **Isolated runs**: pass `--root <tmpdir>` (or set `NUMAN_ROOT`) to keep experiments out of the real Numan root. `registry sync` and `install` require network access to `https://tonythethompson.github.io/numan-registry/`. The official registry currently ships two packages; only `vyadh/nutest` (module) installs on Linux — `abusch/nu_plugin_semver` is Windows-only.

## Learned User Preferences
- Prefers streamlining Nu-compat onboarding as honest search/install UX, a one-shot starter, and an offer-based managed Nu pin (never silent auto-switch of Nu).
- Prefers the command name `numan try` for the prove-it-works starter (not `setup demo` / `setup starter`).
- Product north star for Numan: make the Nushell package ecosystem more inviting for less experienced users.
- Once a plan or todos are approved, proceed without repeated permission prompts.
- Prefers strategy work saved as code-grounded audit plus next-steps plan docs (concrete paths and checkboxes), not abstract strategy alone.

## Learned Workspace Facts
- Plugin ABI is Nu-minor-scoped: mixed plugin ABIs cannot run inside one Nu process; side-by-side Nu profiles would be a separate future product shape, not a near-term substitute for compat UX.
- PATH Nu can be newer than official-registry Windows plugin Nu constraints, so `search` can look fine while `install` fails; use compat-filtered search / `numan try` / `setup nu <version>`.
- `numan setup nu <x.y.z>` pins a managed Nu release; bare `numan setup nu` installs latest. Subcommands: `remove`, `path`, `use <path>`.
- Numan product spans three repos (`numan`, `numan-registry`, `numan-plugins`); trust is cross-cutting (client verifies, registry signs); there is no separate `numan-registry.trust` product repo.
- Near-term adoption bottleneck is thin catalog depth; release handoff is numan-plugins → numan-registry → numan client.
- `numan registry sync` only refreshes the local catalog; it does not install packages (`list` stays empty until `install`).
- Supported install archives include `.zip`, `.tar.gz`/`.tgz`, `.tar.xz`/`.txz`, and plain `.tar`.
- Active-plugin **remove** stays gated while `activation` is set; run `numan deactivate <pkg>` then `numan remove <pkg>`. `remove --force` does not bypass plugin activation (module only). Active **update** orchestrates deactivate→upgrade→activate only when `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` exactly; unset or any alternative value fails closed. See [docs/active-plugin-gate.md](docs/active-plugin-gate.md).
- Prefers streamlining Nu-compat onboarding as honest search/install UX, a one-shot starter, and an offer-based managed Nu pin (never silent auto-switch of Nu).
- Prefers the command name `numan try` for the prove-it-works starter (not `setup demo` / `setup starter`).
- Product north star for Numan: make the Nushell package ecosystem more inviting for less experienced users.
- PATH Nu can be newer than official-registry Windows plugin Nu constraints, so `search` can look fine while `install` fails; use compat-filtered search / `numan try` / `setup nu <version>`.
