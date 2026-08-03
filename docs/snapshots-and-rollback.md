# Immutable activation snapshots and rollback

**Status:** Implemented (Phase 5.3)
**Related:** [ADR 0001](adr/0001-ecosystem-trust-upstream-contribution-fork-stewardship.md), `numan gc`, `numan activate`/`deactivate`

## Purpose

A snapshot captures the complete Numan-owned activation graph at a point in
time: the lockfile, the managed module-autoload file (`numan.nu`) content and
its derived active-module-ID projection, nupm-import provenance, and the Nu
path cache (`nu_state/paths.json`). Rollback restores that exact graph. This
answers *"how do I get back to a known-good state after a bad update,
interrupted change, or incompatible package revision?"* without re-solving
against a registry or guessing at a substitute version.

## Scope — what a snapshot captures

- The full lockfile (every installed package's version, source, and
  provenance metadata).
- The Numan-managed autoload file's exact content and SHA-256, when Nu's
  vendor-autoload target was configured at snapshot time.
- The autoload-state projection (`nu_state/autoload-state.json`), if present.
- The nupm-import provenance sidecar (`state/nupm-imports.json`), if present.
- The Nu path cache (`nu_state/paths.json`): executable path, hash, version,
  vendor-autoload dirs, and plugin registry path. New snapshots always record
  either a full cache or an explicit `Absent` marker when the root was
  uninitialized. Legacy snapshots that predate this sidecar leave the live
  cache untouched on rollback.
- A computed revision hash for every payload directory referenced by the
  lockfile, so a rollback can detect a payload that has gone missing or been
  modified since the snapshot was taken.
- The Nu identity (executable SHA-256, version) recorded at snapshot time
  when paths were available. This remains a convenience field for display and
  the managed-autoload Nu-identity precondition; the paths sidecar is
  authoritative for cache restore.
- The platform triple recorded at snapshot time.

## Scope — what a snapshot does **not** capture

- Package payloads themselves. Payloads are immutable once installed
  (`<root>/packages/<type>/<owner>/<name>/<version>-<sha8>/`) and are never
  duplicated into the snapshot; the snapshot only records their expected
  revision hash and lets rollback verify it against the existing directory.
- Plugin registration state inside Nu's own `plugin.msgpackz` (Numan does not
  touch Nu's plugin registry directly — see `numan activate`'s design).
- Nu binaries themselves, shell `PATH`, or which Nu process is currently
  running. Restoring `paths.json` rewrites Numan's cache only.
- Any file outside Numan's ownership, including user shell configuration,
  `env.nu`/`config.nu`, or unmanaged autoload entries.
- Full filesystem state. This is not a system snapshot/checkpoint feature.

Absolute paths inside the paths and autoload sidecars are root-specific (same
limitation as managed autoload content today).

## Storage cost

Snapshots are cheap: they store JSON metadata and small sidecar files
(lockfile, autoload content, imports, paths), not payload copies. Storage under
`<root>/state/snapshots/<uuid-v7>/` grows roughly linearly with the number of
snapshot-triggering operations (`install`, `update`, `remove`, `activate`,
`deactivate`, `init --refresh`, nupm imports, and rollback itself), not with
payload size.

A snapshot does, however, keep any payload directory it references alive:
`numan gc` treats every committed snapshot's lockfile as a live root, so an
old package version superseded by `update` is **not** deleted by GC while a
snapshot still points at it. Retaining many snapshots across many updates
means retaining the corresponding old payload directories too.

## Retention

There is no automatic snapshot expiry. Snapshots accumulate until explicitly
removed with `numan snapshot delete <id>`. `numan snapshot delete` refuses to
remove a snapshot that is still referenced by an in-flight rollback journal,
and `numan gc` will never delete a payload directory that a remaining
snapshot still references — deleting the snapshot first is what allows GC to
reclaim the payload on the next run.

There is no `numan snapshot prune` or count/age-based retention policy yet;
operators who want bounded storage should delete snapshots they no longer
need to roll back to.

## Commands

```text
numan snapshot list
numan snapshot inspect <id>
numan snapshot delete <id> [--yes]
numan snapshot rollback <id> [--yes]
```

- `list` — every committed snapshot with its reason, trigger, package count,
  and creation time.
- `inspect` — full manifest detail: generated-file digests, per-package
  payload provenance, active module/plugin counts, nupm-import provenance,
  a diff against the current lockfile (what would change on rollback), and
  a payload-verification report (what rollback would refuse on, if anything).
  Always read-only.
- `delete` — removes a snapshot directory. Refuses on symlink/reparse targets
  and on snapshots referenced by an in-flight journal.
- `rollback` — restores Numan-owned state to exactly the snapshot. Takes a
  `pre_rollback` snapshot of the current state first, so a rollback can
  itself be undone by rolling back to that snapshot.

## Rollback preconditions and safety

Rollback refuses rather than approximates when:

- **Payloads don't match exactly.** Every payload directory referenced by the
  snapshot's lockfile must exist and hash to the exact recorded revision.
  A missing or drifted payload produces a remediation list naming each
  affected package; rollback never substitutes a different artifact or
  re-resolves a compatible version.
- **The snapshot is for a different Numan root.** Autoload content embeds
  absolute paths, so a snapshot cannot be replayed against a different root.
- **Nu identity has changed** (for snapshots that captured a managed autoload
  file). Restoring content generated for a different Nu binary would produce
  an activation graph that was never validated against the Nu now in use;
  rollback asks the user to re-activate under the current Nu instead. This
  check uses the **live** path cache before paths restore runs, so
  refresh-then-rollback with a Present managed autoload still refuses when the
  post-refresh Nu identity differs. Paths restore alone (empty lockfile / no
  Present autoload) does not require this match and works with no active
  packages.
- **A different lifecycle operation is mid-flight.** An interrupted
  `update`/`remove`/nupm-import journal must be resolved first. An
  interrupted rollback *to the same snapshot* is safe to resume — rollback
  re-runs from the start and idempotently converges on the same result.
- **The managed autoload file isn't Numan-owned.** Every write to
  `numan.nu` — including during rollback — is gated on the ownership marker
  Numan writes into every generated file. If a user has replaced it with
  hand-written content, rollback refuses rather than overwriting it, and the
  user's file is left untouched.

Before committing anything, a candidate autoload file is staged and
syntax-validated with the current Nu binary (`nu -n <candidate>`), matching
the same validation Numan uses for `activate`/`deactivate`. Commits are
journaled through dedicated `PendingLifecycle` rollback stages so a crash
mid-rollback is recoverable by re-running `numan snapshot rollback <id>`.

Restore order (each step is a journaled commit): lockfile → managed autoload
file → autoload-state projection → nupm-imports sidecar → Nu path cache.
Paths restore runs even when the lockfile is empty and there are no
activations; that is the `init --refresh` then rollback gap this design
closes. Legacy snapshots without a paths sidecar skip the final step.

## Limitations

- No cross-root rollback and no cross-Nu-identity rollback (see above).
- No partial rollback (e.g. "restore only this one package's version") —
  rollback always restores the entire captured graph.
- No automatic snapshot retention/pruning policy.
- Deleting a snapshot is permanent; there is no snapshot-of-a-snapshot
  history beyond the single `pre_rollback` snapshot rollback creates.

## Difference from registry-version selection

`numan update`/`numan install <pkg>@<version>` select a version by **solving
against the registry**: they pick a version that satisfies the requested
constraint and the current Nu compatibility range, and may pull down a
version that was never previously installed on this machine.

`numan snapshot rollback` does the opposite: it restores **exactly** a
previously-recorded state — the same package revisions, the same artifact
digests, the same generated autoload content — with no registry lookup and
no compatibility re-solving. If the payload for that exact revision is no
longer present, rollback fails with a precise remediation message rather than
falling back to registry resolution. Use `update`/`install` to move forward
to a different version; use `snapshot rollback` to return to a state Numan
has already verified and recorded.
