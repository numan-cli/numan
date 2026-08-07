# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1] - 2026-08-07

### Added

- **`numan update --self`**: upgrade the numan CLI itself. Standalone installs download the matching GitHub Release asset, verify the Ed25519 signature over `SHA256SUMS` (`SHA256SUMS.sig` + baked-in release public key), then check the archive digest and replace the binary. Homebrew / winget / cargo installs print the exact upgrade command instead of self-replacing; `--check` still queries GitHub Releases to report whether a newer version exists before printing that hint. The Release workflow now hard-fails if `NUMAN_RELEASE_SIGNING_KEY` is unset so every published release ships `SHA256SUMS.sig`.
- **`SECURITY.md`**: vulnerability reporting, scope, and cross-repo trust summary (linked from the README)

### Changed

- **`numan search` / `numan info`**: scripts and completions are labeled install-only (activation deferred) instead of module-style "not ABI-locked" wording; install-only labels still appear when Nu is unknown
- **`numan try`**: prefers activatable curated starters (plugin/module), then falls back to Nu-agnostic install-only scripts (`SuaveIV/nu_script_wttr`, `Sanceilaks/nufetch`) without calling activate; prints a quoted `overlay use` path from the installed lockfile entry

## [0.2.0] - 2026-08-05

### Added

- Homebrew tap packaging restored: `brew tap tonythethompson/numan && brew install numan`, with `Publish to Homebrew tap` updating [`homebrew-numan`](https://github.com/tonythethompson/homebrew-numan) from release `SHA256SUMS` (requires `HOMEBREW_TAP_TOKEN`)
- **Side-by-side Nu version management** via `numan use <version>|latest|list`:
  - Switches the active managed Nushell install without re-downloading (errors with `numan setup nu <version>` hint when the requested version isn't installed yet)
  - Writes `<root>/nu_state/active-version.json` so `numan use list` and downstream activation can resolve the selected Nu
  - Acquires the root mutation lock and creates a PreMutation snapshot before any state change, matching `install`/`update`/`activate`/`deactivate`
- **`numan setup nu` versioned layout**: binaries now install under `<root>/tools/nushell/<version>/nu` instead of the previous single-binary `<root>/tools/nushell/nu`. Re-installing one version no longer clobbers another. `numan doctor` reports markers whose on-tree binary is missing.
- Journaled legacy managed-Nu migration from the single-binary layout into the versioned tree, with `numan doctor` auto-tier reconciliation
- Strict version-path validation (`<root>/tools/nushell/<version>/...`): rejects components containing `/`, `\\`, `..`, or anything `semver` won't parse, so `numan setup nu ../../path` cannot escape `$NUMAN_ROOT`.
- Shared confirmation utility (`src/util/confirm.rs`): consistent prompt/auto-confirm behavior across all commands
- Snapshots capture and restore `nu_state/paths.json` when present

### Removed

- Prebuilt **macOS Intel** (`x86_64-apple-darwin`) release archives and Homebrew bottles. Apple Silicon and `cargo install numan-cli` remain supported.

### Changed (breaking)

- **`numan setup nu` CLI redesign**: action flags (`--remove`, `--use-path`, `--use-existing`) are replaced by subcommands:
  - `numan setup nu` — install latest (unchanged)
  - `numan setup nu <VERSION>` — install pinned version (was `--version <x.y.z>`)
  - `numan setup nu remove` — uninstall managed Nu (was `--remove`)
  - `numan setup nu path` — use PATH Nu (was `--use-path`)
  - `numan setup nu use <path>` — register a specific binary (was `--use-existing <path>`)
  - Hidden backward-compat flags (`--remove`, `--use-path`, `--use-existing`) still work but emit deprecation warnings. They will be removed in v0.3.0.
- **Non-TTY auto-confirm**: all confirmation prompts now auto-confirm on non-TTY (CI, scripts) instead of requiring `--yes`. A `(non-interactive: auto-confirming)` notice is printed to stderr. `--yes` remains available to skip prompts on TTY.

## [0.1.5] - 2026-07-29

### Added

- `numan setup nu` installs an official managed Nushell release, optionally pinned with `--version`; `--use-existing` repairs an off-PATH installation without downloading another copy
- `numan setup loader` installs the bundled `nushell-loader` integration
- Compatibility-aware discovery: `search` hides incompatible packages by default (`--all` reveals them), `info` explains version compatibility, and install errors report the actual Nu/platform mismatch
- `numan try` installs and activates a curated starter compatible with the current Nu and platform, with managed-Nu pin offers when no starter matches
- Doctor findings distinguish PATH and managed Nu versions and report the built-in official trust-root identity
- Install support for `.tar.xz` / `.txz` package archives (alongside `.zip`, `.tar.gz`/`.tgz`, and `.tar`)
- `numan completions nushell` (alias `nu`) via `clap_complete_nushell`, with a vendor-autoload install hint
- Active-plugin mutation gate (Issue #22 PR1): refuse `remove`/`update` while a plugin activation record exists; doctor info `activation.plugin_mutation_gated` ([docs/active-plugin-gate.md](docs/active-plugin-gate.md))
- Journaled plugin deactivate (Issue #22 PR2): `numan deactivate` unregisters via injectable `plugin rm` seam, clears `PluginActivation` without deleting payload; pending journal `state/pending-plugin-deactivate.json`
- Active-plugin update orchestration (Issue #22 PR3): default **off**; only exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` enables deactivate→upgrade→reactivate via the activate/deactivate-owned `cmd::plugin_lifecycle` boundary
- Cross-platform real-Nu acceptance coverage for active-plugin update, plus a Windows official-registry Stage 1 lifecycle harness

### Fixed

- Autoload journal recovery is command-independent and reconciles pending state before later mutations
- Managed Nu bootstrap verifies release-asset size and SHA256 metadata, validates the installed binary, cleans up failed downloads, and discovers supported off-PATH installations more reliably
- `numan try` gives actionable managed-Nu pin or package-search guidance when no curated starter matches, without silently switching Nu
- PowerShell completions no longer emit top-of-script `using namespace` directives, so `numan completions powershell` can be appended to an existing `$PROFILE` without a ParserError
- README PowerShell install used `Out-File` (overwrites `$PROFILE`); docs now use `Add-Content` or a dedicated completions file

### Changed

- `numan remove --force` no longer bypasses active *plugin* activation (still bypasses active *module* activation only)
- Active-plugin gate hint directs users to `numan deactivate <pkg>` then `numan remove <pkg>`
- Active-plugin **update** is fail-closed and opt-in (default off; only exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` enables it; unset or alternative values refuse); remove remains gated until deactivate
- Official registry Stage 1 acceptance: after `list`, run `deactivate` → `remove` → `gc`
- `numan completions` prints a copy-ready install command on stderr after the script (stdout stays pipe-safe)
- WinGet uses the all-lowercase `tonythethompson.numan` identifier and publishes update PRs automatically after each GitHub release; unsupported Homebrew packaging was removed

## [0.1.4] - 2026-07-05

### Added

- Production official registry trust root (`official-2026-07-01`) — `numan registry sync` verifies the live index without manual `--key` onboarding
- `numan init` auto-configures the official registry when the built-in trust root is production-ready

### Changed

- winget manifests: `tonythethompson.Numan` identifier, schema 1.12.0, lowercase publisher path
- README quick start: `init` → `registry sync` (no manual `registry add` for official)

### Fixed

- Deserialize lowercase registry package `type` values (`plugin`, `module`, etc.) from the official index

## [0.1.3] - 2026-07-05

### Added

- `numan snapshot list|inspect|delete|rollback` — CLI for immutable activation snapshots ([docs/snapshots-and-rollback.md](docs/snapshots-and-rollback.md))
- Registry signature verification with built-in official trust root plumbing (`src/core/official_registry.rs`)
- Detached `index.json.sig` validation on `numan registry sync`; last-known-good index fallback
- CI: MSRV (1.88), `cargo deny`, `cargo package`; CI on version tags; release gates on green CI + preflight
- Homebrew formula and winget manifests; [docs/PACKAGING.md](docs/PACKAGING.md)
- `scripts/update-official-trust-root.sh` for client trust-root updates

### Changed

- Registry index JSON: top-level `version` → `schema_version` on write; legacy `"version"` still deserializes
- `numan gc` can prune unreferenced snapshot directories
- README: install paths (git, Homebrew, winget), common flags table, snapshot docs
- [docs/RELEASING.md](docs/RELEASING.md): pre-tag checklist and CI gate documentation

## [0.1.2] - 2026-06-30

### Added

- `numan doctor [--fix] [--yes] [--json]` — health checks and safe repairs ([docs/numan-doctor.md](docs/numan-doctor.md))
- `numan completions bash|fish|zsh|powershell` — shell completion scripts
- `util::hints` — canonical fix strings aligned with doctor output across init, install, activate, and nupm import
- First-init onboarding checklist after `numan init` (registry → sync → search → install → activate → doctor)

### Changed

- README quick start: single copy-paste onboarding path; doctor and completions documented
- Error messages in init, install, activate, and nupm import now include consistent `Run 'numan …'` fix hints

## [0.1.1] - 2026-06-30

### Added

- `numan init` and `numan init --refresh` for Nu path probing and activation identity refresh
- crates.io publishing (`cargo install numan-cli`) and [docs/RELEASING.md](docs/RELEASING.md)
- [CHANGELOG.md](CHANGELOG.md) and release checklist

## [0.1.0] - 2026-06-30

### Added

- Registry-backed install, activate, update, remove, and gc
- Module autoload with managed `numan.nu` vendor file
- nupm interoperability: status, inspect, import, diff
- GitHub Release binaries for Linux, Windows, and macOS
- Real-Nu acceptance CI job

[Unreleased]: https://github.com/tonythethompson/numan/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/tonythethompson/numan/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/tonythethompson/numan/compare/v0.1.5...v0.2.0
[0.1.5]: https://github.com/tonythethompson/numan/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/tonythethompson/numan/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/tonythethompson/numan/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/tonythethompson/numan/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/tonythethompson/numan/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tonythethompson/numan/releases/tag/v0.1.0
