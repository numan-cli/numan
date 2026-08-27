# Numan — Nushell Package Manager

Numan is a cross-platform, production-grade Nushell package manager CLI written in Rust (`numan-cli`).

## Core Build & Test Commands

```bash
cargo build                             # Build binary
cargo test                              # Run standard unit & integration test suite
cargo clippy -- -D warnings             # CI clippy gate (lib + bins only)
cargo fmt --check                       # CI code formatting check
cargo test -- --ignored --skip acceptance_process_helper --skip stage1_official_registry --skip real_nu_active_update_ # Real-Nu tests (requires Nu on PATH)
```
> For complete test matrix commands, environment prerequisites, MSRV 1.88 notes, and VM gotchas, see [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md).

## Non-Negotiable Invariants & Conventions

1. **Inert Install**: Downloading & extracting packages only writes to `$NUMAN_ROOT/packages/<type>/<owner>/<name>/<version>-<sha8prefix>/`. Never touch Nushell configuration during install.
2. **Activate-Owned Nu Integration**: Only `activate` / `deactivate` manage Nushell plugin registration. `numan update` orchestrates active plugin mutation only when `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` is set.
3. **State Integrity**: Lockfile (`lockfile.json`) is the ground truth. Always acquire `acquire_mutation_lock(root)` before mutating state, write JSON via `write_json_atomic`, and capture `create_snapshot()` prior to state mutations.
4. **Trust & Verification**: Mandatory SHA256 verification for binary artifacts. Index signatures use Ed25519 (`NUMAN_ALLOW_UNSIGNED=1` for dev only).
5. **Rust API Conventions**: Use `&Path` instead of `&PathBuf` in function parameters (clippy enforced). Return `anyhow::Result` for application errors and `thiserror` for library error enums.

## High-Level Layout

- `src/core/`: Platform detection, package models, version constraints, Ed25519 registry trust & SHA256 integrity.
- `src/cmd/`: CLI subcommands (search, info, list, activate, deactivate, update, remove, doctor, snapshot, use, setup, nupm, try).
- `src/state/`: Lockfile v2, crash-recovery journals, activation profiles, mutation locking, snapshots & rollback.
- `src/nu/`: Managed Nushell bootstrap, path resolution, version manager, module autoload generator.
- `src/install/`: HTTP download, archive extraction, atomic install transactions.
- `src/nupm_compat/`: nupm compatibility audit, import, and drift classification.

## Reference Documentation

- **Development & Environment Gotchas**: [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md)
- **Architecture & State Machines**: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- **PR Review Guidance & Standards**: [`REVIEW.md`](REVIEW.md)
- **Doctor Diagnostics & Repair**: [`docs/numan-doctor.md`](docs/numan-doctor.md)
- **Active Plugin Gate**: [`docs/active-plugin-gate.md`](docs/active-plugin-gate.md)
- **Snapshots & Rollback**: [`docs/snapshots-and-rollback.md`](docs/snapshots-and-rollback.md)
- **nupm Compatibility Matrix**: [`docs/nupm-compatibility.md`](docs/nupm-compatibility.md)
