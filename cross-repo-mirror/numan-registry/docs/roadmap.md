# Repo-local roadmap for numan-registry

This repo owns the signed official catalog, intake evidence, staging,
and production signing. The roadmap that covers the entire three-repo
plan — catalog intake, signing, plugin backfills, client compat,
lifecycle evidence, and the active-plugin gate — lives in the
consolidated cross-repo plan:

[**`numan/docs/plans/consolidated-multi-repo-roadmap.md`**](https://github.com/tonythethompson/numan/blob/2cd6f5a4d5e831d1248d3fe3fd924944823510a1/docs/plans/consolidated-multi-repo-roadmap.md)

The cross-repo drill is enforced by
[`scripts/check-roadmap-drift.py`](https://github.com/tonythethompson/numan/blob/2cd6f5a4d5e831d1248d3fe3fd924944823510a1/scripts/check-roadmap-drift.py),
which CI runs at `.github/workflows/roadmap-drift.yml` and which fails
this PR if the local roadmap drifts from the consolidated truth.

## Repo-local detail

Use this page for **operational** detail that belongs only to
`numan-registry`:

- Stage gate evidence under `stage-evidence/` and `scripts/lifecycle-prove.py`.
- Intake state machine and candidate promotion rules.
- Signing key ceremony and protected-branch disclosure.

Promote any cross-repo claim back into the consolidated roadmap via a
PR against `numan/docs/plans/consolidated-multi-repo-roadmap.md`
alongside the matching code change, so the three repos stay aligned.
