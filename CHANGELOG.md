# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Install support for `.tar.xz` / `.txz` package archives (alongside `.zip`, `.tar.gz`/`.tgz`, and `.tar`)
- `numan completions nushell` (alias `nu`) via `clap_complete_nushell`, with a vendor-autoload install hint
- Active-plugin mutation gate (Issue #22 PR1): refuse `remove`/`update` while a plugin activation record exists; doctor info `activation.plugin_mutation_gated` ([docs/active-plugin-gate.md](docs/active-plugin-gate.md))
- Journaled plugin deactivate (Issue #22 PR2): `numan deactivate` unregisters via injectable `plugin rm` seam, clears `PluginActivation` without deleting payload; pending journal `state/pending-plugin-deactivate.json`
- Active-plugin update orchestration (Issue #22 PR3): default **off**; only exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` enables deactivate→upgrade→reactivate via the activate/deactivate-owned `cmd::plugin_lifecycle` boundary

### Fixed

- PowerShell completions no longer emit top-of-script `using namespace` directives, so `numan completions powershell` can be appended to an existing `$PROFILE` without a ParserError
- README PowerShell install used `Out-File` (overwrites `$PROFILE`); docs now use `Add-Content` or a dedicated completions file

### Changed

- `numan remove --force` no longer bypasses active *plugin* activation (still bypasses active *module* activation only)
- Active-plugin gate hint directs users to `numan deactivate <pkg>` then `numan remove <pkg>`
- Active-plugin **update** is fail-closed and opt-in (default off; only exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` enables it; unset or alternative values refuse); remove remains gated until deactivate
- Official registry Stage 1 acceptance: after `list`, run `deactivate` → `remove` → `gc`
- `numan completions` prints a copy-ready install command on stderr after the script (stdout stays pipe-safe)
- winget manifests: identifier/path `tonythethompson.numan` (all-lowercase), plus `InstallationNotes` for Nu PATH / `init` / `registry sync`

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

[Unreleased]: https://github.com/tonythethompson/numan/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/tonythethompson/numan/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/tonythethompson/numan/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/tonythethompson/numan/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/tonythethompson/numan/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tonythethompson/numan/releases/tag/v0.1.0
