# Overlay P3 handoff

## Checkout and publication

- Branch: `codex/overlay-p3`, pushed to origin.
- PR: https://github.com/numan-cli/numan/pull/160, targeting master.
- Overlay P1/P2 PRs #147 and #150 are merged. Master through TUI parity PR #157
  was merged into this branch; do not retarget to the old overlay branches.

## Implementation

The owned `nu_state/numan_overlay.nu` defines Nu-side use/hide/list commands and
loads through regenerated `numan_init.nu`. Native statements execute at REPL scope
through one-shot cleanup/string/verify pre_prompt entries. The exact statement
passes nu-check before arming. Activation takes effect at the next prompt;
same-line use of newly exported commands is unsupported. Existing user hooks are
preserved, and repeated sourcing does not duplicate the reconciliation hook.

Rust helpers validate metadata, acquire the mutation lock, snapshot changes, and
atomically reserve immutable payloads. Records carry session UUID, PID, process
start time, transaction UUID, payload path, and pending/active/degraded status.
Only matching markers plus active overlay observations confirm activation.
Marker-only, overlay-only, and partial execution remain degraded and pinned.
Failed reconciliation retries without acknowledging its signature. Hide preserves
PWD and releases records only after both overlay and ownership marker disappear.

Session identity is record-valued and not exported to children. Process start
time detects PID reuse. Missing process inventory entries require OS confirmation
of absence before pruning: Unix signal-zero ESRCH or Windows OpenProcess invalid
parameter. Permission errors and unknown errors retain reservations. Zero start
times retain existing reservations; new observations require verifiable identity.

Reservations protect remove, update, install replacement, GC, nupm import, and
rollback. GC fails closed on invalid/unresolvable reserved paths. Rollback must
preserve reserved payloads and never restores historical overlay sidecars over
live observations. Legacy P2 records are unconfirmed and pruned on reconciliation;
restart shells that used manually emitted P2 statements before migration.

External CLI use/hide emits guidance/native statements without fabricating state.
The TUI captures last-observed listing, classifies reconciliation as a mutation,
and excludes hidden protocol helpers from command forms. Project root records
origin only; automatic project discovery is not implemented.

## Verification evidence

- Initial implementation library suite: 820 passed, 2 ignored.
- Windows Nu 0.116.1 interactive PTY acceptance: 4 passed.
- Post-merge standard suite with process-visibility hardening: passed; library
  portion 823 passed, 2 ignored, and all standard integration targets passed.
- Rust 1.88 all-target check passed after moving the build cache off a full C:
  drive. The earlier failed attempt reported disk-full, not an MSRV incompatibility.
- Current code revision `c9d1ca5` CI passed Clippy, MSRV, formatting, packaging,
  dependency/license checks, workflow validation, and Linux/macOS real-Nu
  acceptance on Nu 0.115.0 at the time this handoff was refreshed.
- Final local TUI library suite: 848 passed, 3 ignored. Windows Nu 0.115.0
  PTY acceptance: 4 passed. External hide package-ID regression plus the scoped
  overlay suite: 11 passed. Final CI status belongs in the PR description.
- The first CI revision failed an existing bootstrap PATH assertion in its TUI
  suite. Matrix fail-fast canceled the other standard platform jobs; these were
  not independent overlay failures. Follow the current-head rerun evidence.

PTY tests use isolated home/config/vendor/root directories, actual generated init
sourcing, OSC 133 prompt synchronization, and JSON observations from Nu. Coverage
includes command/env exports, duplicate use/hide, PWD preservation, foreign names,
native hide, marker-only degradation, failed reconciliation retry, preflight and
execution failure, prompt replay prevention, two independent shells, payload
protection, and dead-shell pruning.

## Review and remaining limits

Review hardening replaced unchecked metadata/transaction lookups with contextual
errors, loaded rollback's lockfile once, and rejected unresolved GC reservations.
The reported legacy promotion issue is already prevented by requiring a nonempty
matching transaction. Failed CLI reconciliation throws before acknowledging the
signature; hide-env uses ignore-errors, matching the retry contract.

Restricted-process permission errors are retained conservatively in code but have
not been exercised with an actual elevated/other-user shell. Nested child Nu
identity inheritance has not received a dedicated PTY case. Degraded execution
requires closing the affected shell and reconciling; no automatic repair of
arbitrary partial Nu state is claimed. Review the PR's latest checks before merge.

User-facing contract: `docs/overlay-sessions.md`.
