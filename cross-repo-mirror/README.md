# Cross-repo mirror for `check-roadmap-drift`

This directory holds **drop-in artifacts** that complete the cross-repo
roadmap-drift guardrail started in PR 67 of `numan`. Each subdirectory
mirrors both the local-roadmap file and the GitHub Actions workflow
that runs `scripts/check-roadmap-drift.py` against the consolidated
roadmap hosted in `numan@master`.

## What ships

```
cross-repo-mirror/
├── README.md                                         ← you are here
├── numan-plugins/
│   ├── docs/roadmap.md                               ← the local pointer
│   └── .github/workflows/roadmap-drift.yml           ← the CI job snippet
├── numan-registry/
│   ├── docs/roadmap.md                               ← the local pointer
│   └── .github/workflows/roadmap-drift.yml           ← the CI job snippet
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
straight from `numan@master/docs/plans/consolidated-multi-repo-roadmap.md`
into a temp file at the start of the job, then runs the exact same
`scripts/check-roadmap-drift.py` against that fetched copy. There is
never a stale copy in a sibling repo: the source of truth is the
published URL.

## Installation

In each sibling repo:

```bash
mkdir -p scripts
curl -sSfL \
    https://raw.githubusercontent.com/tonythethompson/numan/master/scripts/check-roadmap-drift.py \
    -o scripts/check-roadmap-drift.py
chmod +x scripts/check-roadmap-drift.py

# Then drop the contents of the relevant subdirectory into the sibling repo:
#   cross-repo-mirror/numan-plugins/docs/roadmap.md       → <sibling>/docs/roadmap.md
#   cross-repo-mirror/numan-plugins/.github/workflows/roadmap-drift.yml
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

1. Update `scripts/check-roadmap-drift.py` in `numan` and run its tests
   (the negative-test proves it still bites).
2. Update each sibling's local CI workflow to mirror the change — most
   changes are confined to the script itself; the workflow is just a
   thin courier that `curl`s the consolidated roadmap and runs the script
   with `CONSOLIDATED_ROADMAP=<path>`.
3. Bump the pinned `commit` SHA the workflow fetches (currently
   `tonythethompson/numan@master`; the URL can be pinned to a tag for
   reproducibility once a roadmap release cadence exists).
