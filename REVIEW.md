# Numan PR review

Use this file when reviewing pull requests (human or automated). [`AGENTS.md`](AGENTS.md) remains the source for project structure and build commands; this file focuses on **what to flag in review**.

Copilot / IDE apply-to instructions live at [`.github/instructions/review.instructions.md`](.github/instructions/review.instructions.md) and must stay aligned with this document.

## CI gates (must pass)

PR CI (`.github/workflows/ci.yml`):

- **Test** — `cargo test`
- **Clippy** — `cargo clippy -- -D warnings`
- **Format** — `cargo fmt --check`
- **MSRV (1.88)** — `cargo +1.88 check --locked --all-targets`
- **Package** — `cargo package --locked`
- **Deny** — `cargo deny` (CI: `EmbarkStudios/cargo-deny-action@v2`)
- **Real-Nu acceptance** — `cargo test -- --ignored` with Nu 0.113 on PATH (PR job skips Stage 1 / active-plugin update suites)

## Severity labels

| Label | Meaning |
|-------|---------|
| **P0** | Data loss, security boundary break, silent corruption, or trust bypass |
| **P1** | Incorrect behavior on happy path, missing error handling for common failures |
| **P2** | Test/fixture mismatch with documented contract, misleading docs, maintainability |
| **P3** | Style, naming, non-blocking suggestions |

## Architecture invariants (flag violations)

1. **Install is inert** — `numan install` must not invoke Nu or touch autoload/plugin registration.
2. **Nu integration is activate/deactivate-owned** — only the activate/deactivate lifecycle boundary invokes plugin register/unregister; an explicitly opted-in `update` may coordinate that boundary (exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1`) but must not own or invoke Nu callbacks directly.
3. **Mutation lock** — all mutating commands (`install`, `remove`, `update`, `gc`, future `nupm import`) must call `acquire_mutation_lock(root)`.
4. **Atomic JSON writes** — lockfile, journals, and state files use `write_json_atomic`; no partial writes.
5. **Journals under `state/`** — pending activation, autoload, lifecycle journals live under `$NUMAN_ROOT/state/`.
6. **Module autoload identity** — four-part match (Nu exe hash, Nu version, vendor autoload dir, managed file path); lockfile `module_activation` is ground truth.
7. **Managed file ownership** — never overwrite foreign autoload files; respect `OWNERSHIP_MARKER`.
8. **Nu invocation safety** — paths via env vars only; no runtime interpolation in Nu program strings.
9. **Test seams** — unit tests use `FakeCandidateRunner` / injectable registrars; do not spawn real `nu` in unit tests.
10. **Phase 6 nupm boundary** — read-only toward `NUPM_HOME`; no `build.nu` execution; no bidirectional sync.

## Review checklist

- [ ] Error paths return `anyhow::Result` with context; library code does not panic.
- [ ] New mutating paths acquire the mutation lock and snapshot lockfile before change.
- [ ] Function parameters use `&Path` not `&PathBuf` (clippy enforced).
- [ ] Tests cover failure modes, not only success.
- [ ] Docs/AGENTS.md updated when structure or conventions change.
- [ ] Scope matches PR description; no unrelated refactors.

## Phase-specific notes

- **Lockfile v2** — preserve `origin`, `revision_id`, `payload_sha256`, and journal recovery semantics on lifecycle changes.
- **nupm compat (Phase 6+)** — follow [`docs/nupm-compatibility.md`](docs/nupm-compatibility.md) supported/rejected profiles; fixtures under `tests/fixtures/nupm/` are the contract for parser/classifier tests.
- **Active-plugin update** — deactivate→upgrade→activate only with exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1`; fail closed otherwise. See [`docs/active-plugin-gate.md`](docs/active-plugin-gate.md).
