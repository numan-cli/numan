# Remaining Numan Roadmap

**Status date:** 2026-07-29

**Superseded:** Use the consolidated multi-repo plan
[`2026-07-30-consolidated-multi-repo-roadmap.md`](2026-07-30-consolidated-multi-repo-roadmap.md).
This file is retained as the prior client draft for history.

---

This was the cross-repository plan for the remaining work in the Numan product
line. It is intentionally grounded in the current repository split:

- `numan` owns the client, user experience, local state, Nu integration, and
  release packaging.
- `numan-plugins` owns CI-built plugin binaries for upstreams that do not ship
  compliant release artifacts.
- `numan-registry` owns the signed official catalog, package intake evidence,
  staging, production signing, and publication.

The operating rule remains: new plugin catalog depth flows
`numan-plugins -> numan-registry -> numan`. Client work should not paper over a
missing registry artifact, and registry work should not trust a plugin build
until the hardened plugin pipeline has produced immutable assets and specs.

## Current Baseline

- Numan client core is feature-complete for the current 0.1.x line:
  signed registries, inert installs, plugin/module activation, update/remove/gc,
  snapshots, doctor, completions, nupm import/diff, release packaging, crates.io,
  and winget automation are in place.
- The official registry is live and has moved past the original seed catalog.
  Current package truth lives in
  [`numan-registry/docs/intake-candidates.md`](https://github.com/tonythethompson/numan-registry/blob/main/docs/intake-candidates.md).
- The plugin-build pipeline has an open catalog expansion PR:
  `numan-plugins` PR #4, branch `feature/catalog-expansion-wave-1`, commit
  `88151d8`, adding `FMotalleb/nu_plugin_port_extension` and
  `FMotalleb/nu_plugin_image` plus updated macOS runner coverage.
- No PR #4 assets have been published yet. Registry intake for those plugins
  must wait until that PR is merged and the manual build workflow is dispatched
  with an explicit `only` list.

## Release 0.1.x To 1.0 Priorities

### 1. Finish catalog wave 1 through the hardened pipeline

- [ ] Merge `numan-plugins` PR #4 after review and green checks.
- [ ] Dispatch `build-plugins` manually with only:
  `nu_plugin_port_extension,nu_plugin_image`.
- [ ] Confirm every expected release asset exists and every generated spec
  preserves `source.rev` as the immutable upstream commit.
- [ ] Intake the generated specs in `numan-registry` without hand-typed hashes.
- [ ] Run registry validation, manifest/index lint, staging, and lifecycle
  evidence before production publication.
- [ ] Publish the signed registry only after the registry PR is reviewed and the
  production workflow validation job passes.
- [ ] Run a fresh client smoke:
  `init -> registry sync -> search -> info -> install -> activate -> doctor -> list -> deactivate -> remove -> gc`.

### 2. Keep client compatibility UX honest as the catalog grows

- [ ] Keep `numan search` filtered by detected Nu/platform by default and ensure
  `--all` explains plugin ABI mismatch clearly.
- [ ] Keep `numan info` showing source provenance, verification metadata, package
  type, supported targets, and Nu constraints without implying security approval.
- [ ] Keep `numan try` aligned with the live catalog. Starter packages must fail
  clearly when no compatible starter exists; they must not silently switch Nu.
- [ ] Keep install errors explicit that nothing was installed when resolution
  fails because of Nu or platform incompatibility.
- [ ] Add or refresh doctor checks when catalog growth exposes common local
  setup failures: PATH Nu drift, managed Nu pin drift, official registry trust
  drift, stale plugin activation records, and pending lifecycle journals.

### 3. Decide when active plugin update can become default-on

- [ ] Keep `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` as the only mutation opt-in
  until real-Nu active-update evidence is boring on Ubuntu, Windows, and macOS.
- [ ] Keep `update` orchestration free of direct Nu registration ownership; Nu
  integration remains owned by activate/deactivate lifecycle helpers.
- [ ] Before any default-on change, require:
  exact failure-before-lifecycle guard coverage, deactivate failure coverage,
  upgrade failure rollback coverage, activate failure recovery coverage, and
  real-Nu matrix evidence.
- [ ] Update `docs/active-plugin-gate.md`, `AGENTS.md`, README, and changelog in
  the same PR as any default semantics change.

### 4. Grow install-only package usefulness without weakening activation rules

- [ ] Keep scripts and completion packages install-only until their activation
  contracts are designed and tested.
- [ ] For completions, define whether activation means managed vendor autoload,
  shell-specific install hints, or a separate `numan completions` adjunct.
- [ ] For scripts, define execution/discovery boundaries before adding any Nu
  config mutation.
- [ ] Add lifecycle evidence per package type before changing README support
  tiers.

### 5. Revisit Phase 5.2 source builds only after catalog intake is steady

- [ ] Keep source builds deferred while `numan-plugins` can cover the highest
  demand source-only plugins through controlled CI.
- [ ] When revived, source builds need explicit user consent, dependency
  disclosure, deterministic install paths, failure cleanup, and no hidden Nu
  activation.
- [ ] Do not mix source builds with registry catalog expansion PRs.

### 6. Distribution and release polish

- [ ] Keep winget automation monitored after each GitHub release.
- [ ] Keep macOS/Linux package manager work deferred until there is a verified,
  maintained formula/tap/channel that will not advertise a broken path.
- [ ] Keep release docs version-agnostic where possible, because README ships in
  crates.io package metadata and tagged source archives.
- [ ] For each release, run the existing dry-run gates before tagging:
  `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`, package
  checks, release-note extraction, and fresh archive smoke where practical.

## 1.0 Gate

Numan is ready for 1.0 when:

- the official registry has enough packages to make first-use demos feel real on
  Windows, macOS, and Linux;
- install, activate, update, deactivate, remove, gc, snapshots, and doctor have
  green local and CI evidence across the supported OS matrix;
- registry intake no longer depends on ad hoc hand editing for routine packages;
- package compatibility failures are discoverable before install;
- release packaging and winget updates are routine;
- there are no open P0/P1 lifecycle, trust, or data-loss issues.

## Explicitly Deferred

- Silent side-by-side Nu profile switching.
- Source builds hidden inside registry intake.
- Publishing registry entries without review.
- Calling packages "approved" or "audited" solely because they are in the
  official registry.
- Broad maintained forks of upstream plugins before the catalog pipeline has
  exhausted ordinary CI-built upstream tags.
