# Cross-repo mirror for `check-roadmap-drift`

This directory holds **drop-in artifacts** that complete the cross-repo
roadmap-drift guardrail started in PR 67 of `numan`. Each subdirectory
mirrors both the local-roadmap file and the GitHub Actions workflow
that runs `scripts/check-roadmap-drift.py` against the consolidated
roadmap hosted in `numan@<contract-tag>` (pinned by SHA; see
[`docs/contracts/roadmap-v1.md`](../contracts/roadmap-v1.md)).

## What ships

```
cross-repo-mirror/
├── README.md                                         ← you are here
├── numan-plugins/
│   ├── docs/roadmap.md                               ← the local pointer
│   └── .github/workflows/roadmap-drift.yml           ← the CI job snippet (pinned to contract v1)
├── numan-registry/
│   ├── docs/roadmap.md                               ← the local pointer
│   └── .github/workflows/roadmap-drift.yml           ← the CI job snippet (pinned to contract v1)
└── snapshot-tests/
    └── mirror_dry_run.sh                             ← local sanity check
```

## Why three repos, one source of truth

Numan's product spans three repos; trust is cross-cutting and drift is
silent. PR 67 shipped `numan use` while the consolidated roadmap still
called it post-1.0. The drift script lives in `numan/scripts/`, but a
script only one side knows about isn't a guardrail — both sibling repos
need to run it.

The CI workflow in each sibling repo `curl`s the consolidated roadmap
straight from `numan@<CONTRACT_SHA>/docs/plans/consolidated-multi-repo-roadmap.md`
into a temp file at the start of the job, then runs the exact same
`scripts/check-roadmap-drift.py` against that fetched copy. The contract
SHA is recorded as `CONTRACT_SHA: 99aa695c6ecbb059020efc689b59a8a8d6c490f8`
in each workflow so a force-push to the contract tag can never
silently change the guardrail. Bumping the contract is a coordinated
operation across all three repos — see `scripts/bump-contract.sh` in
the `numan` repo.

## Installation

In each sibling repo:

```bash
mkdir -p scripts
# Fetch the pinned contract version of the drift script directly from
# the contract SHA so the sibling repo never holds a stale copy.
curl -sSfL \
    https://raw.githubusercontent.com/tonythethompson/numan/99aa695c6ecbb059020efc689b59a8a8d6c490f8/scripts/check-roadmap-drift.py \
    -o scripts/check-roadmap-drift.py
chmod +x scripts/check-roadmap-drift.py

# Then drop the contents of the relevant subdirectory into the sibling repo:
#   cross-repo-mirror/<sibling>/docs/roadmap.md         → <sibling>/docs/roadmap.md
#   cross-repo-mirror/<sibling>/.github/workflows/roadmap-drift.yml
#           → <sibling>/.github/workflows/roadmap-drift.yml

mkdir -p docs .github/workflows
cp cross-repo-mirror/<sibling>/docs/roadmap.md docs/roadmap.md
cp cross-repo-mirror/<sibling>/.github/workflows/roadmap-drift.yml \
   .github/workflows/roadmap-drift.yml

# Smoke-test locally before pushing:
python scripts/check-roadmap-drift.py
# expect: 0 errors, optionally a warning about the consolidated roadmap
#         being fetched URL-side. The warning is informational only.
```

## Mirror contract

When `numan/scripts/check-roadmap-drift.py` evolves (new forbidden
phrases, new SHIPPED_MARKERS, stronger structural checks), the mirror
artifacts in this directory MUST stay in sync. The recommended
workflow:

1. Update `scripts/check-roadmap-drift.py` in `numan` together with the
   consolidated roadmap (or wait for a contract bump).
2. Bump the contract via `scripts/bump-contract.sh` — it opens a
   coordinated PR set that publishes the new tag and updates all three
   sibling workflow yml files in lockstep. **Never** edit this README
   pin in isolation; the bump script is the only sanctioned path.
3. The contract tag is the human-readable handle; the SHA pinned into
   each workflow is the literal fetch target. Both must agree, and
   `scripts/bump-contract.sh` runs the agreement check locally before
   each PR issuance.
