# Roadmap Contract v1

> **Status:** Active. The cross-repo guardrail is frozen at this version.
> **Tag:** `numan-roadmap-contract/v1`
> **Authoritative repo:** [`tonythethompson/numan`](https://github.com/tonythethompson/numan)
> **Frozen at commit:** *recorded by `bump-contract.sh` when v1 was minted*

This document is the single source of truth for what is "shipped" vs
"aspirational" across the three Numan repos — `numan`, `numan-plugins`,
and `numan-registry`. Changes to this contract must follow the bump
procedure at the bottom of this file.

## Why a contract

Before v1, every roadmap file lived wherever the author happened to
write it. Drift happened silently:

* `numan-plugins/docs/roadmap.md` rewrote a `numan use <version>` claim
  to "reserved for post-1.0" two months after the client shipped it.
* `numan-registry/README.md` listed an importer the client never
  accepted.
* `numan/docs/plans/consolidated-multi-repo-roadmap.md` claimed
  `numan use` was a stub when it already had a mutation-lock + snapshot
  pipeline (PR 67).

The pattern repeated because the three repos had **no shared
ground-truth document** — they each maintained their own roadmap
without coordination. Every roadmap was a contract, and every contract
was being silently rewritten. PR #67 alone had to do a "cross-repo
audit" pass to undo the worst of it.

v1 fixes this by:

1. **Naming a single authority.** `numan-roadmap-contract/v1` in
   [`tonythethompson/numan`](https://github.com/tonythethompson/numan)
   is the only document the sibling repos treat as truth.
2. **Pinning the authority via a versioned tag.** Sibling CI workflows
   `curl` `tonythethompson/numan@numan-roadmap-contract/v1:docs/plans/consolidated-multi-repo-roadmap.md`.
   `git fetch` resolves the tag to a SHA at fetch time, so a force-push
   to `master` doesn't quietly change the cross-repo guardrail.
3. **Bumping requires a coordinated PR set.** `scripts/bump-contract.sh`
   is the only sanctioned way to move to v2. It opens one PR per repo in
   merge order (numan first, then siblings) and refuses to continue until
   the gates pass.
4. **Programmatic sentinel rules.** `scripts/check-roadmap-drift.py`
   enforces the rules in § Sentinel Rules below as a CI job in every
   repo. Anyone can run the same check locally before pushing.

## What v1 freezes

v1 freezes three artifacts inside the `numan` repo at its tagged
commit:

| Artifact | Path | Role |
|---|---|---|
| Consolidated roadmap | `docs/plans/consolidated-multi-repo-roadmap.md` | The single truth document every sibling roadmap points at |
| Drift checker | `scripts/check-roadmap-drift.py` | Python script that fails CI when v1's rules break |
| This doc | `docs/contracts/roadmap-v1.md` | Human-readable narrative of the contract |

The consolidated roadmap carries:

* A **Repo-local roadmap rule** preamble declaring that sibling-repos'
  only job is to link back here.
* A list of every bullet that has shipped (`[x]`, `Phase N (shipped|complete)`,
  `Wave N closed`, or a code-pattern like `` `numan <verb>` ``).
* A list of every bullet that is **explicitly deferred** (post-1.0,
  not in scope, awaiting the Nu upstream, etc.).
* A short **Cross-repo facts** block — only items that can be verified
  from outside the `numan` source tree.

## Sentinel rules

`scripts/check-roadmap-drift.py` enforces these as a CI gate. Any
violation fails the drift job; CI stays green means the contract is
intact.

1. **Preamble required.** The consolidated roadmap must declare the
   repo-local-roadmap rule and name both `numan-plugins/docs/roadmap.md`
   and `numan-registry/docs/roadmap.md`.

2. **Shipped-feel bullets cannot lie about their state.** A bullet
   counts as "shipped" if it has `[x]`, `Phase N (shipped|complete|merged)`,
   `Wave N closed`, or invokes a code pattern
   (`` `numan <verb>` `` / `` `scripts/<name>.py` `` /
   `` `numan-<repo> <verb>` ``). Such bullets outside a deferral heading
   must not contain any of `stub`, `reserved`, `post-1.0 reserved`,
   `exists as a stub`, or `is a reserved`.

3. **Repo-local cross-link.** If a repo has `docs/roadmap.md`, it must
   reference the consolidated roadmap (or its GitHub URL). A missing
   local roadmap is a *warning* (so the source-of-truth repo `numan`
   doesn't fail); a present-but-disconnected one is an *error*.

## Bumping the contract

A bump is the only sanctioned way to change what the cross-repo
guardrail says. It is a coordinated operation — three PRs land together
or the bump is invalidated.

### Inputs

| Input | Default | Notes |
|---|---|---|
| `BUMP_FROM` | `numan-roadmap-contract/v1` | Tag to read the current contract from |
| `BUMP_TO` | `numan-roadmap-contract/v2` | Tag to create |
| `BUMP_REASON` | (required, prompt) | One-line description of what changed and why |

### Procedure

1. **Author the change locally.** Edit
   `docs/plans/consolidated-multi-repo-roadmap.md` (and, when needed,
   `scripts/check-roadmap-drift.py`) so bullets and sentinel rules
   reflect the new state. Sentinel rules must still pass.
2. **Run the bump script.**

   ```bash
   scripts/bump-contract.sh 2 -r "numan upgrade is shipped"
   ```

   For the inaugural v1 freeze, pass `--init -r "freeze the post-audit
   v1"` instead. After v1 exists, all changes go via
   `scripts/bump-contract.sh <N> -r "..."` for `N >= 2`.

   The script commits the contract document (and any staged freeze
   artifacts) first, re-derives `CONTENT_SHA` from that commit, creates
   the annotated `numan-roadmap-contract/vN` tag at `CONTENT_SHA`, then
   makes a **separate** commit for workflow-pin / README / blob-URL
   rewrites so the tag never self-references the pin-rewrite SHA.

3. **PR sequence (issued by the script, in order):**
   1. PR 1 — `numan`: update consolidated roadmap + bump sentinel
      docs; bump tag is created and pushed once merge lands.
   2. PR 2 — `numan-plugins`: bump ref to the new contract tag +
      any sibling-side rule updates.
   3. PR 3 — `numan-registry`: same as PR 2.
4. **Merge gate.** Each PR's CI must show the new tag fetched
   cleanly via `git ls-remote tonythethompson/numan refs/tags/<NEW>`.
   No PR is mergeable until the previous PR's CI is green.

### Refusing to bump

`scripts/bump-contract.sh` refuses and exits nonzero if any of the following
hold:

* The current local copy of the consolidated roadmap fails the
  sentinel rules (run `scripts/check-roadmap-drift.py` first). Exit
  status **3** when the drift script is missing or validation fails.
* The new contract tag already exists locally or on origin.
* The author is not authenticated with write access to `numan`,
  `numan-plugins`, and `numan-registry` (checked via `gh`).
* `BUMP_REASON` is empty.
* A workflow yml is missing `CONTRACT_TAG` or `CONTRACT_SHA`, the
  existing `CONTRACT_TAG` does not match the previous contract tag, or
  the bump rewrite cannot set `CONTRACT_TAG` to the new tag /
  `CONTRACT_SHA` to the frozen content commit.

### Reverting

If a bump turns out to be wrong after merge:

1. Open a "contract-rollback" PR on `numan` that restores the
   `docs/plans/consolidated-multi-repo-roadmap.md` to the v(N-1)
   tag's content and re-tags the contract as `numan-roadmap-contract/vN-deleted`.
2. Update the three sibling repos' workflow yml to fetch the
   `vN-deleted` tag for one CI cycle.
3. Land a fresh v(N+1) bump once the rollback is stable.

## What v1 does NOT promise

* **Roadmap content accuracy** — the script catches sentinel-rule
  violations but does not check whether the roadmap *describes the
  shipped design correctly*. That is the responsibility of the
  audit pass that bumps the contract.
* **Inter-version migration of the *rules themselves*** — changes to
  the sentinel rules require a new `scripts/check-roadmap-drift.py`
  commit in addition to the consolidated-roadmap bump. The bump
  script refuses to leave the rules drifting.

## See also

* [`docs/plans/consolidated-multi-repo-roadmap.md`](../plans/consolidated-multi-repo-roadmap.md) — the document under contract.
* [`scripts/check-roadmap-drift.py`](../../scripts/check-roadmap-drift.py) — the sentinel-rule enforcement script.
* [`scripts/bump-contract.sh`](../../scripts/bump-contract.sh) — the sanctioned bump procedure.
* [`cross-repo-mirror/`](../../cross-repo-mirror/) — drop-in artifacts for the two sibling repos.
* [.github/workflows/ci.yml](../../.github/workflows/ci.yml) — the `roadmap-drift` job in `numan`'s own CI.
