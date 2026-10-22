# Overlay P3 handoff

## Checkout and scope

- Worktree: `C:\Users\tonyt\.codex\worktrees\overlay-p3\numan`
- Branch: `codex/overlay-p3`
- Base: `origin/master` at `3ac4150`. PRs #147 and #150 are already merged, so
  open the eventual P3 PR against master, not their historical head branches.
- User approved implementing the pasted overlay recommendation, then requested
  committing this work with a handoff because usage was running low.
- No P3 PR has been created and this branch has not been pushed.
- Do not modify the user's live Nu config or repair the unrelated stale vendor
  autoload mentioned in the original recommendation.

## Implemented

Nu wrappers for `numan overlay use`, `hide`, and `list` load from an owned
`nu_state/numan_overlay.nu`, sourced by the regenerated `numan_init.nu`. Other
Numan commands still use the external CLI. Native overlay statements execute
through cleanup/string/verify pre_prompt entries at REPL scope. The exact use
statement is parsed with nu-check before arming. Activation occurs at the next
prompt; same-line invocation of newly exported commands is unsupported.

Rust helpers reserve payloads under the mutation lock with snapshots and atomic
writes. Records carry session UUID, PID, process start time, transaction UUID,
payload path, and pending/active/degraded status. Both the ownership marker and
active overlay name must be observed before activation is confirmed. Failed
execution is degraded and remains pinned because Nu may leave callable commands.
Hide preserves PWD, and records are removed after both marker and overlay vanish.
Marker-only or overlay-only observations remain degraded. Failed reconciliation
retries without acknowledging its signature. The stable hook is idempotently
registered without discarding user hooks. Dead sessions/PID reuse are pruned.

External CLI use/hide only emit guidance/native statements and do not fabricate
activation. Per-shell export/name conflict checks refuse foreign overlays.
Project root records origin only; there is no project discovery.

Pending, active, and degraded transactions protect payloads in remove, update,
direct install, GC, nupm import, and snapshot rollback. Snapshot overlay sidecars
remain historical evidence; rollback preserves current session observations.
Legacy P2 records are unconfirmed and discarded on reconciliation. Users of the
old manually emitted statements should restart those shells before migration.

## Verification actually completed

- `cargo check --all-targets -j 1`: passed during implementation.
- `cargo clippy --lib --bins -j 1 -- -D warnings`: passed before the last small
  identity/containment hardening edits; rerun on the final commit.
- Final `cargo test --lib -j 1`: **820 passed, 0 failed, 2 ignored**.
- Final `cargo fmt --all --check` and `git diff --check`: passed.
- Actual Nu 0.116.1 PTY acceptance: **4 passed, 0 failed** with
  `cargo test -j 1 --test overlay_session_test -- --ignored --nocapture --test-threads=1`.
  Coverage includes actual regenerated init sourcing, exported commands/env,
  duplicate use/hide, changed PWD preservation, foreign name refusal, native
  hide, marker-preserving native hide, failed reconcile retry after lock release,
  parse failure after nu-check, no prompt replay, degraded payload protection,
  independent shells, and dead-shell cleanup.
- PTY harness retains its master and answers cursor-position requests. It uses
  OSC 133 completion/prompt synchronization and isolated home/config/vendor/root
  directories. Observations are written by Nu to JSON, not inferred from echo.
- Read-only auditor found the marker-only reservation gap; it was fixed and
  covered by unit and real PTY tests. No other concrete audit finding remained.

## Remaining work before publishing

1. Review the final diff and rerun Clippy. The final library suite already passed.
2. Check new dependency/MSRV compatibility on Rust 1.88 and run standard
   integration tests. Added sysinfo 0.36 (minimal system feature) and portable-pty
   0.9 as a dev dependency; local toolchain was Rust 1.97.1 on Windows.
3. Run the PTY tests on Linux/macOS and Nu 0.115.0 (the current CI Nu pin).
   Only Windows Nu 0.116.1 interactive behavior has been verified locally.
4. Audit process-enumeration failure behavior: sysinfo start-time zero is treated
   conservatively, but a process absent from its inventory is considered dead.
   Confirm behavior on supported platforms, especially restricted processes.
5. Review generated bootstrap/doctor drift behavior, helper session protocol,
   rollback guards, and migration documentation. `OverlayState`'s older module
   header still describes the original P1 model and can be updated for clarity.
6. Push this branch and create a PR against master when ready. Attach the PR to
   this chat with the Codex artifact tool. Inspect CI failures and review comments
   using evidence; do not weaken tests or hand-edit generated live files.

Primary files: `src/cmd/overlay.rs`, `src/state/overlay_state.rs`,
`src/nu/overlay.rs`, `src/nu/overlay_shell.nu`, `tests/overlay_session_test.rs`,
and `docs/overlay-sessions.md`. Mutation guards are intentionally scoped to
overlay reservations. The primary checkout and older PR worktrees were preserved.

## Continuation: review and publication

Master advanced through the TUI parity PRs #154–#157. Merged it into this branch
and preserved captured overlay listing, classified reconciliation, and excluded
hidden helper commands from TUI forms. Added a focused captured-list regression
test and corrected the state module documentation. Final pre-merge Clippy passed.
Rust 1.88 all-target checks and the standard suite are running; Windows PTY tests
will also run against the installed CI-pinned Nu 0.115.0. Publication starts as a
draft while these checks and cross-platform CI complete.

The process-inventory limitation remains explicit: unavailable process entries
are treated as dead. Zero start times are retained conservatively. Restricted
process visibility and Linux/macOS PTY behavior need platform evidence; do not
interpret Windows acceptance as proof for those environments.
