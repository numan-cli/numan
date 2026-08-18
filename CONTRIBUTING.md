# Contributing to Numan

Thank you for your interest in contributing to Numan. This guide covers the workflow, quality gates, and architecture rules you need to follow. For deeper project structure and module layout, see [AGENTS.md](AGENTS.md).

## Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Nushell](https://www.nushell.sh/) on `PATH` — required only for activation commands and real-Nu acceptance tests

## Getting started

```bash
git clone https://github.com/numan-cli/numan.git
cd numan
cargo build
cargo test
```

Run a single test module while iterating:

```bash
cargo test core::resolve
cargo test cmd::activate
```

Format and lint before opening a PR (CI enforces both):

```bash
cargo fmt
cargo clippy -- -D warnings
```

Real-Nu acceptance tests are marked `#[ignore]` in the test suite. Run them locally when your change touches activation or nupm import:

```bash
cargo test -- --ignored
```

CI runs the full suite, clippy, `rustfmt --check`, and real-Nu acceptance on Ubuntu, Windows, and macOS.

## Development workflow

1. **Branch from `master`** using `feature/<short-description>` or `fix/<short-description>`.
2. **Keep changes focused** — one logical change per pull request; avoid unrelated refactors.
3. **Add or update tests** for behavior changes, including failure paths where relevant.
4. **Update docs** when you change structure, conventions, or user-visible behavior (`AGENTS.md`, `docs/`, or command help as appropriate).
5. **Open a pull request** against `master` with a clear description and test plan.

### Commit messages

Use imperative mood and keep the subject under 72 characters:

```
fix(activate): reject stale module autoload when Nu version drifts
feat(nupm): add manifest batch import
docs: clarify lockfile v2 fields in README
```

Squash merges are used for feature branches.

## Pull request checklist

Before requesting review, confirm:

- [ ] `cargo test` passes
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo fmt` applied (or `cargo fmt --check` clean)
- [ ] New mutating code paths acquire the mutation lock and snapshot the lockfile before writes
- [ ] Error paths return `anyhow::Result` with context; library code does not panic
- [ ] Function parameters use `&Path`, not `&PathBuf` (clippy-enforced)
- [ ] Unit tests use injectable seams (`FakeCandidateRunner`, registrars) — do not spawn real `nu` in unit tests
- [ ] PR description explains *why* and includes a test plan

Reviewers follow [`REVIEW.md`](REVIEW.md) for severity labels and architecture invariants.

## Architecture rules

These boundaries are non-negotiable. Violations are **P0** or **P1** findings in review.

| Rule | Summary |
|------|---------|
| **Install is inert** | `numan install` writes only to `$NUMAN_ROOT`. It must not invoke Nu or register plugins/autoloads. |
| **Activate is separate** | Only `activate` / `deactivate` modify Nu integration state. |
| **Lockfile is ground truth** | Derived projections (e.g. autoload state) are not authoritative. |
| **Immutable payloads** | Installs land under versioned, content-addressed paths; never overwrite in place. |
| **Mutation lock** | Mutating commands (`install`, `remove`, `update`, `gc`, `nupm import`, etc.) call `acquire_mutation_lock(root)`. |
| **Atomic JSON writes** | Lockfile, journals, and state files use `write_json_atomic`. |
| **Managed file ownership** | Never overwrite foreign autoload files; respect `OWNERSHIP_MARKER`. |
| **Safe Nu invocation** | Plugin paths via environment variables only; no runtime interpolation in Nu program strings. |
| **nupm boundary** | Read-only toward `NUPM_HOME`; no `build.nu` execution; no bidirectional sync. |

See [docs/nupm-compatibility.md](docs/nupm-compatibility.md) for the nupm interoperability contract and `tests/fixtures/nupm/` for parser/classifier fixtures.

## Code style

- **Edition**: Rust 2021
- **Errors**: `anyhow::Result` + `.context(...)` in application code; `thiserror` for library types callers match on
- **CLI**: `clap` derive macros
- **Serialization**: `serde` + `serde_json` / `toml`
- Match existing naming, module layout, and documentation level in the file you edit

## Reporting issues

Open a [GitHub issue](https://github.com/numan-cli/numan/issues) with:

- Numan version (`numan --version` or commit SHA)
- OS and architecture
- Steps to reproduce
- Expected vs actual behavior
- Relevant logs or lockfile excerpts (redact secrets)

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
