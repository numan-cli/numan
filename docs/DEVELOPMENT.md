# Numan Development & Environment Guide

This document contains detailed development instructions, environment prerequisites, testing guidelines, and environment-specific gotchas for Numan.

## Build & Test Commands

```bash
# Build binary
cargo build

# Run CLI during development
cargo run -- search <query>
cargo run -- info <owner/name>
cargo run -- list
cargo run -- nupm status --nupm-home <path>
cargo run -- nupm inspect <package-path>

# Default unit and integration test suite
cargo test

# Single module/component tests
cargo test core::platform
cargo test core::package
cargo test core::resolve
cargo test state::lockfile
cargo test cmd::activate

# Linting and Formatting (enforced by CI)
cargo clippy -- -D warnings
cargo fmt --check
# cargo fmt   # repair formatting
```

## Real-Nu Acceptance Test Suite

Real-Nu acceptance tests require a `nu` binary on `PATH`.

```bash
# Portable ignored suite (default PR CI gate with Nu on PATH)
# Excludes Stage 1 and active-plugin update matrix
cargo test -- --ignored --skip acceptance_process_helper --skip stage1_official_registry --skip real_nu_active_update_

# Active-plugin update real-Nu acceptance matrix (requires Nushell 0.113.x on PATH)
cargo test --test plugin_active_update_real_nu -- --ignored --nocapture --test-threads=1
```

## Environment & Toolchain Gotchas

- **MSRV**: `1.88` (`Cargo.toml` `rust-version`, enforced by CI `msrv` job). Build with the `stable` Rust toolchain.
- **System Dependency**: `git2` and `reqwest` pull in `openssl-sys`, requiring system OpenSSL dev libraries (`pkg-config` + `libssl-dev` on Linux/Debian).
- **Clippy Scope**: CI runs `cargo clippy -- -D warnings` (packages & libraries only). Do **not** pass `--all-targets` as newer toolchains may flag pre-existing test-only code.
- **Nushell for Acceptance**: Portable ignored acceptance tests require `nu` on `PATH`. Always verify with `nu --version`.
- **`numan activate` Directory Prerequisites**: Activation resolves the plugin registry under `~/.config/nushell` and vendor autoload dir under `~/.local/share/nushell/vendor/autoload`. Ensure parent directories exist before running `activate` on a fresh environment.
- **Isolated Testing**: Use `--root <tmpdir>` (or set `NUMAN_ROOT`) to avoid mutating your primary Numan root during manual testing.

## Git & PR Conventions

- **Commits**: Imperative mood, <72 characters.
- **Branches**: `feature/description`, `fix/description`.
- **Review Guidelines**: Refer to [`REVIEW.md`](../REVIEW.md) for PR review checklists and severity guidelines.
