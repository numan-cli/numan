# Cross-repo mirror for `check-roadmap-drift`

This directory holds **drop-in artifacts** that complete the cross-repo
roadmap-drift guardrail started in PR 67 of `numan`. Each subdirectory
mirrors both the local-roadmap file and the GitHub Actions workflow
that runs `scripts/check-roadmap-drift.py` against the consolidated
roadmap hosted in `numan@<contract-tag>` (pinned by SHA; see
[`docs/contracts/roadmap-v1.md`](../docs/contracts/roadmap-v1.md)).

## What ships

```text
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
into `docs/plans/consolidated-multi-repo-roadmap.md` in the working tree
at the start of the job, then runs the exact same
`scripts/check-roadmap-drift.py` against that fetched copy. (Only the
canonical `numan` `.github/workflows/ci.yml` fetch uses
`/tmp/consolidated-roadmap.md` for a pin-diff against the local copy.)
The contract SHA is recorded as
`CONTRACT_SHA: 2829230a7b34108f53ace6bb929867596d6055c0`
in each workflow so a force-push to the contract tag can never
silently change the guardrail. Bumping the contract is a coordinated
operation across all three repos — see `scripts/bump-contract.sh` in
the `numan` repo.

## Installation

From a checkout of `tonythethompson/numan` (this repo), copy the
sibling artifacts into each sibling working tree. Do not run the `cp`
lines from inside the sibling repo alone — the `cross-repo-mirror/`
tree lives only in `numan`.

```bash
# In the numan checkout:
NUMAN_ROOT=$(pwd)   # path to tonythethompson/numan
CONTRACT_SHA=2829230a7b34108f53ace6bb929867596d6055c0
SIBLING=numan-plugins   # or numan-registry
SIBLING_ROOT=/path/to/$SIBLING

mkdir -p "$SIBLING_ROOT/scripts" "$SIBLING_ROOT/docs" "$SIBLING_ROOT/.github/workflows"

# Fetch the pinned contract version of the drift script directly from
# the contract SHA so the sibling repo never holds a stale copy.
curl -sSfL \
    "https://raw.githubusercontent.com/tonythethompson/numan/${CONTRACT_SHA}/scripts/check-roadmap-drift.py" \
    -o "$SIBLING_ROOT/scripts/check-roadmap-drift.py"
chmod +x "$SIBLING_ROOT/scripts/check-roadmap-drift.py"

cp "$NUMAN_ROOT/cross-repo-mirror/$SIBLING/docs/roadmap.md" \
   "$SIBLING_ROOT/docs/roadmap.md"
cp "$NUMAN_ROOT/cross-repo-mirror/$SIBLING/.github/workflows/roadmap-drift.yml" \
   "$SIBLING_ROOT/.github/workflows/roadmap-drift.yml"

# Stage the pinned consolidated roadmap so the offline smoke test
# matches sibling CI (which curls the same path from CONTRACT_SHA).
mkdir -p "$SIBLING_ROOT/docs/plans"
curl -sSfL \
    "https://raw.githubusercontent.com/tonythethompson/numan/${CONTRACT_SHA}/docs/plans/consolidated-multi-repo-roadmap.md" \
    -o "$SIBLING_ROOT/docs/plans/consolidated-multi-repo-roadmap.md"

# Smoke-test locally before pushing (from the sibling working tree):
cd "$SIBLING_ROOT"
CONSOLIDATED_ROADMAP=docs/plans/consolidated-multi-repo-roadmap.md \
  LOCAL_ROADMAP=docs/roadmap.md \
  python scripts/check-roadmap-drift.py
# expect: 0 errors
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
