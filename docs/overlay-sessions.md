# Overlay sessions

Run `numan init --refresh` and start a new interactive Nushell session. The
generated `numan_init.nu` sources the owned `nu_state/numan_overlay.nu`, which
defines `numan overlay use`, `hide`, and `list`. Other commands, including
`numan --version`, continue to invoke the external binary.

```nu
numan overlay use owner/tool
# Wait for the next prompt before invoking exported commands.
hello
numan overlay list
numan overlay hide tooling
```

Activation and hiding run through a one-shot string `pre_prompt` hook so Nu
evaluates its native statement at REPL scope. A cleanup closure removes the
one-shot entries before the statement executes; a parse error cannot create a
prompt replay loop. User hooks are preserved. `nu-check` parses the exact armed
statement first. Hooks are not available in scripts or `nu -c`; those callers
should use native `overlay use` and `overlay hide` statements. The external
`^numan overlay use` and `^numan overlay hide` commands emit guidance without
changing activation records.

Each shell receives a record-valued session UUID and PID. The record is not
exported to child processes; a child Nu gets its own identity from its config.
Transactions also record the process start time to distinguish PID reuse.
`--project-root` records the activation's project of origin, without enabling
directory discovery or automatic project activation.

## State and failure recovery

The Rust `prepare` helper validates installed metadata under the mutation lock,
captures a snapshot, reserves the immutable payload, and records a pending
transaction. Only an observation containing both its ownership marker and the
active overlay name can promote it to active. These observations are local
cooperation between the shell and CLI, not a security boundary against code
already running in that shell.

A cheap prompt hook compares the current observations with its last acknowledged
set. Changed observations trigger reconciliation, including native `overlay hide`.
An unsuccessful reconciliation is retried at the next prompt. Hide executes in
the shell first; the record is removed only after the marker and active overlay
are both absent. `PWD` is preserved; other environment changes follow native Nu
overlay hiding semantics, including changes made while the overlay was on top.

If statement execution fails, Nu can leave callable commands or environment
changes even when its overlay list reports no active overlay. Numan marks this
transaction degraded, reports partial state, and keeps the payload reserved.
Start a new shell, close the affected shell, then run `numan overlay reconcile`.
Dead sessions and reused PIDs are pruned. A broken module rejected by `nu-check`
is aborted before anything is armed and leaves no reservation.

Remove, update, install replacement, nupm import, and garbage collection respect
pending, active, and degraded reservations across every shell. Rollback refuses
to replace reserved package payloads. Snapshot overlay sidecars are historical
evidence; rollback does not restore them over current session observations.
Legacy P2 records have no confirmed session and are discarded during
reconciliation. Restart shells that used P2's manually emitted statements before
using this integration.

An existing overlay with the same name and no matching Numan ownership marker
is refused. Numan never merges into or hides a user's unowned overlay.

## Verification

Unit tests cover reservation transitions and mutation guards. Ignored
`overlay_session_test` acceptance tests run an isolated interactive Nu through a
PTY and synchronize on shell integration prompt markers. Run them with Nu on
PATH, or set `NUMAN_TEST_NU` to the binary:

```text
cargo test --test overlay_session_test -- --ignored --nocapture
```
