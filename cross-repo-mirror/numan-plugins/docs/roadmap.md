# Repo-local roadmap for numan-plugins

This repo owns CI-built plugin binaries for upstreams without compliant
release assets. The roadmap that covers the entire three-repo plan —
catalog intake, signing, plugin backfills, client compat, lifecycle
evidence, and the active-plugin gate — lives in the consolidated
cross-repo plan:

[**`numan/docs/plans/consolidated-multi-repo-roadmap.md`**](https://github.com/numan-cli/numan/blob/14f8e8c0934049c0030626b4a0d917255e988105/docs/plans/consolidated-multi-repo-roadmap.md)

The cross-repo drill is enforced by
[`scripts/check-roadmap-drift.py`](https://github.com/numan-cli/numan/blob/14f8e8c0934049c0030626b4a0d917255e988105/scripts/check-roadmap-drift.py),
which CI runs at `.github/workflows/roadmap-drift.yml` and which fails
this PR if the local roadmap drifts from the consolidated truth.

## Repo-local detail

Use this page for **operational** detail that belongs only to
`numan-plugins`:

- Workflow manifests under `.github/workflows/` (`build-plugins.yml`,
  `release.yml`, `windows-recheck.yml`).
- Per-upstream build matrix decisions (`docs/upstream-build-decisions.md`).
- Backlog triage notes (`docs/backlog.json` schema + review log).

Promote any cross-repo claim back into the consolidated roadmap via a
PR against `numan/docs/plans/consolidated-multi-repo-roadmap.md`
alongside the matching code change, so the three repos stay aligned.
