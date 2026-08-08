---
name: code-review
description: >-
  Review Numan pull requests against REVIEW.md severity labels, architecture
  invariants, CI gates, and phase-specific notes. Use for Copilot code review,
  PR review requests, and any pull-request or diff review in this repository.
---

# Numan code review

When reviewing a pull request or diff in this repository, follow the canonical
guide at [`REVIEW.md`](../../../REVIEW.md). Prefer that file over paraphrased
memory. [`AGENTS.md`](../../../AGENTS.md) remains the source for project
structure and build commands.

Path-specific Copilot apply-to instructions live at
[`.github/instructions/review.instructions.md`](../../instructions/review.instructions.md)
and must stay aligned with `REVIEW.md`.

## How to review

1. Read the PR description and changed files; stay within the stated scope.
2. Apply severity labels from `REVIEW.md` (P0–P3). Lead with P0/P1 findings.
3. Flag any violation of the architecture invariants listed below.
4. Check the review checklist and phase-specific notes when relevant paths
   change (lockfile, journals, nupm compat, activation lifecycle).
5. Leave actionable comments with concrete fixes. Do not approve or request
   changes as a human gate; report findings only.

## CI gates (must pass)

- `cargo test` — full suite
- `cargo clippy -- -D warnings`
- `cargo fmt --check`

## Severity labels

| Label | Meaning |
|-------|---------|
| **P0** | Data loss, security boundary break, silent corruption, or trust bypass |
| **P1** | Incorrect behavior on happy path, missing error handling for common failures |
| **P2** | Test/fixture mismatch with documented contract, misleading docs, maintainability |
| **P3** | Style, naming, non-blocking suggestions |

## Architecture invariants (flag violations)

1. **Install is inert** — `numan install` must not invoke Nu or touch autoload/plugin registration.
2. **Activate is separate** — only activation/deactivation commands modify Nu integration state.
3. **Mutation lock** — all mutating commands (`install`, `remove`, `update`, `gc`, nupm import) must call `acquire_mutation_lock(root)`.
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
- **nupm compat (Phase 6+)** — follow [`docs/nupm-compatibility.md`](../../../docs/nupm-compatibility.md) supported/rejected profiles; fixtures under `tests/fixtures/nupm/` are the contract for parser/classifier tests.
- **Active-plugin update** — deactivate→upgrade→activate only with exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1`; fail closed otherwise. See [`docs/active-plugin-gate.md`](../../../docs/active-plugin-gate.md).
