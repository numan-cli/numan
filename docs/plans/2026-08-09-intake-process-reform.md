# ADR: Intake Process Reform — Breaking Through Catalog Coverage Blockers

**Date:** 2026-08-09
**Status:** Proposed
**Scope:** Cross-repo (`numan`, `numan-plugins`, `numan-registry`)
**Motivation:** Gap Analysis — August 2026 research campaign (7 cycles, strong evidence; internal research document, not committed to repo)

---

## Context

A systematic gap analysis of the numan catalog vs. the Nushell extension
ecosystem found that numan covers **~95% of what's currently promotable** for
Nu 0.114, but only **34% of the total tracked ecosystem** (22/65 awesome-nu
plugins). The headline gap is real but overwhelmingly caused by upstream
constraints, not numan's prioritization.

However, several of these "upstream constraints" are amplified — or outright
created — by rigidity in our own intake process. This ADR identifies where we
tie our own hands, proposes specific reforms, and sequences them by unlock
potential.

### The intake pipeline today

```
Upstream repo (must have a tagged release)
    │
    ▼
numan-plugins/manifest.json (must be Rust, must target specific Nu minor)
    │ CI cross-compiles to 5 targets (all must succeed)
    ▼
gen_spec.py → spec.json (all expected targets must be present)
    │
    ▼
numan-registry/scripts/add-package.py (downloads, SHA-256, format check)
    │
    ▼
lifecycle-prove.py (full install→activate→doctor→deactivate→remove)
    │
    ▼
Production signing + publication
```

Each stage has legitimate safety rationale. But the cumulative effect is a
pipeline that can only accept: **Rust plugins, with tagged releases, targeting
one Nu minor, building on all 5 platforms, passing full lifecycle proof**.
This excludes the majority of the ecosystem by design.

---

## Decision Drivers

1. **North star:** "Make it easy for someone who's never used Nushell to install
   extensions." Catalog depth directly serves this.
2. **Trust is non-negotiable.** SHA-256 integrity, Ed25519 signatures, and
   provenance must remain. We relax *process gates*, not *cryptographic
   guarantees*.
3. **Marginal cost per package should decrease.** Each reform should make the
   *next* package cheaper to promote, not just unblock one.
4. **User risk must be communicated.** Where we relax a gate, users see the
   difference (provisional tier, partial platform, commit-pinned).

---

## Proposals

### P1. Commit-Snapshot Intake (unlocks ~24 tag-less plugins)

**Problem:** 24 plugins on awesome-nu have zero tagged releases. The manifest
requires a `tag` field verified against `source_commit` via `git ls-remote`.
No tag = cannot enter the pipeline at any stage.

**Observation:** We already pin by immutable commit SHA (`source_commit`). The
tag is a human-readable label and a signal of author intent — it is not a
cryptographic requirement.

**Proposal:**

- Add an optional `intake_mode` field to manifest entries: `"tagged"` (default)
  or `"commit-snapshot"`.
- When `intake_mode: "commit-snapshot"`:
  - `tag` becomes optional (may be omitted or `null`).
  - `source_commit` remains mandatory and is the sole provenance anchor.
  - Version is derived: `0.0.0-snapshot.20260809.<short-sha>` (semver-prerelease
    with ISO date prefix ensures monotonic sorting regardless of SHA ordering,
    clearly communicates "not author-versioned").
  - The date component (`YYYYMMDD`) guarantees that newer snapshots always
    sort above older ones under SemVer prerelease rules, which the resolver
    (`src/core/resolve.rs`) and update command (`src/cmd/update.rs`) depend on
    for strict version ordering.
  - Registry entry carries `"provenance": "commit-snapshot"` metadata.
  - `numan info` displays a note: "This package is built from a commit snapshot,
    not a tagged release."
- Validation: `validate_manifest.py` skips the `git ls-remote` tag→SHA check
  when `intake_mode` is `"commit-snapshot"`.
- Trust: SHA-256 artifact hashes and Ed25519 index signatures still apply. The
  commit is immutable; reproducibility is identical to tagged builds.

**Tradeoffs:**

| Pro | Con |
|-----|-----|
| Unlocks 24 blocked plugins overnight | No author "intent to release" signal |
| Immutable provenance preserved | Version string is synthetic, not upstream |
| Low implementation cost (manifest + validate changes) | User must trust numan's commit selection |

**Mitigation for cons:** Document selection criteria (latest commit on
default branch passing CI, or latest commit touching plugin code). Prefer
tagged releases when available — snapshot is a fallback, not default.

**Affected repos:** `numan-plugins` (manifest schema, validate_manifest.py),
`numan-registry` (add-package.py provenance passthrough, schema extension),
`numan` (info display, optional provenance field rendering).

---

### P2. Non-Binary Intake Lane (unlocks modules, scripts, completions without CI)

**Problem:** The CI pipeline is entirely Cargo cross-compilation. Modules
(`.nu` files), scripts, and completions don't need compilation — they're just
archives of source files. But there's no streamlined path from "here's a Git
repo with Nu files" to "here's a registry entry."

**Current workaround:** Manual `add-package.py` invocations with hand-crafted
spec JSON. This works (Wave 3A/3B used it) but is high-friction and
undocumented as a repeatable workflow.

**Proposal:**

- Create `numan-registry/scripts/intake-archive.py`:
  - Input: Git URL + ref + entry path + package metadata (name, owner, type,
    description, tags, nu_version, activation config).
  - Behavior: clone at ref, archive the relevant subtree as `.tar.gz`, upload
    to a GitHub Release on `numan-registry` (or a dedicated assets repo),
    compute SHA-256, emit a spec compatible with `add-package.py --write`.
  - Validation: entry file must exist in archive, schema validation passes,
    activation config is coherent (the mod.nu / import mode check).
- Add a `non-binary` section to the manifest (or a separate
  `manifest-archives.json`) tracking upstream repos, refs, and entry points
  for repeatable re-intake on version bumps.
- Lifecycle-prove still runs for activatable packages (modules with activation
  config). Scripts and completions that are install-only can skip it.

**Tradeoffs:**

| Pro | Con |
|-----|-----|
| 30+ modules/scripts/completions become low-cost intake | Need a hosting solution for archive assets |
| No cross-compilation complexity | Still need version-bump tracking for upstream changes |
| Aligns with existing schema (`kind: "archive"`) | New script to maintain |

**Affected repos:** `numan-registry` (new script, possible assets hosting),
`numan-plugins` (optional: manifest extension for tracking).

---

### P3. Partial-Platform Shipping (unlocks plugins that don't build everywhere)

**Problem:** `gen_spec.py` requires all expected targets to be present. If a
plugin builds on 4/5 targets, the spec generation fails. The only escape hatch
is `exclude_targets` — but that's a per-entry manual decision made before the
build, not a graceful "ship what succeeded."

**Real-world impact:** Some plugins have Windows build failures due to
`windows-sys` version conflicts or missing C dependencies. Others lack macOS
ARM support. These are fully usable on the platforms that work.

**Proposal:**

- Add a `--partial` flag to `gen_spec.py` that emits a spec with only the
  targets that successfully built.
- Registry schema already supports this (targets is a map; partial maps are
  valid).
- `numan install` already handles "no artifact for your platform" gracefully
  (`Resolver::format_resolve_error` explains why).
- Add a `platforms` field to `numan search` output showing which platforms a
  package supports, so users see *before* attempting install.
- CI workflow change: on partial build failure, generate a partial spec +
  summary of what failed and why. Maintainer reviews and decides whether to
  ship partial or fix.

**Tradeoffs:**

| Pro | Con |
|-----|-----|
| Ships value to users on working platforms immediately | Users on unsupported platforms see "not available" |
| Reduces pressure to fix every cross-compile issue before any release | Catalog appears uneven across platforms |
| Matches reality (many upstream plugins only test on Linux) | Harder to communicate "why not my platform?" clearly |

**Mitigation:** `numan info <pkg>` already shows artifact targets. Add a
`--platforms` filter to `numan search`. Registry docs note platform coverage
per package in `catalog-compat.md`.

**Affected repos:** `numan-plugins` (gen_spec.py `--partial` flag, CI workflow
to handle partial builds), `numan-registry` (no schema change needed),
`numan` (search/info UX improvement, optional).

---

### P4. Selective Fork Policy (unlocks high-demand stale plugins)

**Problem:** clipboard (12K dl, Nu 0.110), compress (7K dl, Nu 0.103), and
units (7.4K dl, Nu 0.106) have significant user demand but upstream maintainers
haven't bumped to Nu 0.114. Outreach has no guaranteed timeline.

**Precedent:** numan has already forked `nu-plugin-explore` and `nu_plugin_qr`
for similar reasons.

**Proposal:**

Formalize the fork decision framework:

1. **Eligibility criteria** (all must be true):
   - Plugin has >5K crates.io downloads (demonstrated demand).
   - Upstream is >3 Nu minors behind current target.
   - Outreach issue has been open >30 days with no response, OR upstream is
     explicitly abandoned/archived.
   - The Nu version bump is mechanical (dependency bumps + minor API changes),
     not a rewrite.

2. **Fork protocol:**
   - Fork lives under `tonythethompson/` org (or a dedicated `numan-forks/` org).
   - Fork branch named `numan/nu-0.114` (or current target).
   - Minimal diff: only dependency bumps and compile fixes. No feature work.
   - README clearly states: "This is a maintenance fork for numan packaging.
     Upstream: [link]. PRs welcome upstream."
   - If/when upstream catches up, retire the fork and switch back.

3. **Intake treatment:**
   - Manifest entry uses the fork repo with `owner` set to `"numan-maintained"`
     (a numan-owned distribution identity), not the original upstream author.
     This avoids conflating `numan install upstream-author/pkg` with a
     numan-maintained fork, consistent with the stewardship policy in
     `docs/adr/0001-ecosystem-trust-upstream-contribution-fork-stewardship.md`.
   - Registry `description` appends "(numan-maintained fork for Nu 0.114 compat)".
   - `source.git` points to the fork; a `source.upstream` field preserves
     the original repo URL for attribution and retirement tracking.
   - If/when upstream catches up, the fork entry is retired and replaced by
     an entry under the original author's identity.

**Tradeoffs:**

| Pro | Con |
|-----|-----|
| Unlocks 3 high-demand plugins (27K combined downloads) | Maintenance burden for keeping forks current |
| Precedent already exists in project history | Community perception of "taking over" |
| Minimal diff reduces ongoing cost | Fork may diverge if upstream makes breaking changes |

**Mitigation:** Strict eligibility criteria prevent fork creep. Automated
upstream-watch (CI checks upstream for new tags weekly) triggers retirement.
Clear attribution in all fork metadata.

**Affected repos:** New fork repos, `numan-plugins` (manifest entries pointing
to forks), `numan-registry` (schema extension for `source.upstream`).

---

### P5. Automated Nu-Version-Bump CI (breaks the monthly treadmill)

**Problem:** Every Nu minor release potentially invalidates the entire catalog.
Today, re-verification is manual: someone must update `nu_version` constraints,
re-run lifecycle-prove, and promote. With 44 packages heading toward 100+, this
doesn't scale.

**Proposal:**

- **Nightly/weekly CI job** in `numan-plugins` (or a new `numan-compat-ci` repo):
  - Downloads latest stable Nu release.
  - For each active manifest entry, runs lifecycle-prove against the new Nu.
  - Outputs a compatibility matrix: `{plugin, current_nu_version, tested_nu, result}`.
  - On success: auto-opens a PR widening the `nu_version` constraint and adding
    the new version to `verified_with`.
  - On failure: opens an issue with the error log for manual triage.

- **Constraint relaxation policy:**
  - If a plugin passes lifecycle-prove on Nu 0.115 without rebuild, widen to
    `>=0.114.0 <0.116.0` (no new binary needed, just constraint + evidence).
  - If it requires a rebuild (API change), trigger a new build wave.

- **Registry-side:** `add-package.py` already supports updating `verified_with`
  on existing versions. The auto-PR just invokes the existing tooling.

**Tradeoffs:**

| Pro | Con |
|-----|-----|
| Catalog stays current without manual intervention | CI compute cost (cross-platform lifecycle-prove) |
| Users get wider constraints = fewer "incompatible" errors | False positives (passes lifecycle but has subtle runtime issues) |
| Scales to 100+ packages | Requires Nu binary provisioning in CI (already solved for lifecycle-prove) |

**Affected repos:** `numan-plugins` or new repo (CI workflow),
`numan-registry` (auto-PRs to widen constraints).

---

### P6. Provisional Tier (ships faster without weakening the proven tier)

**Problem:** Lifecycle-prove is a strong guarantee but blocks packages that:
- Need credentials (BigQuery plugin, cloud-service plugins)
- Need GUI/desktop environment (desktop_notifications on headless CI)
- Have transient network dependencies
- Are simple enough that structural validation is sufficient (scripts,
  completions)

**Existing support:** `add-package.py` already has `--provisional` flag. But
there's no user-facing tier distinction or formal policy for when provisional
is acceptable.

**Proposal:**

- **Two-tier registry model:**
  - **Proven:** Full lifecycle evidence. Default tier. No UX change.
  - **Provisional:** Passed structural validation (download, SHA-256, schema
    lint, archive extraction test) but lacks lifecycle evidence.
    Reason for deferral is recorded in the entry.

- **Schema extension:** Add optional `"evidence_tier": "proven" | "provisional"`
  to version entries. Absence = `"proven"` (backward compatible).

- **CLI UX:**
  - `numan search` shows a marker for provisional packages (e.g., `[p]` or
    `(provisional)`).
  - `numan install` of a provisional package shows a one-time notice: "This
    package has not been lifecycle-tested. It passed integrity checks."
  - `numan info` shows the deferral reason.

- **Graduation policy:** Provisional packages are re-tested periodically
  (ties into P5 auto-bump CI). On passing lifecycle-prove, they graduate to
  proven automatically.

**Tradeoffs:**

| Pro | Con |
|-----|-----|
| Ships packages 2-5x faster | User might install something that doesn't activate cleanly |
| Unblocks credential-gated and GUI plugins | Two tiers add UX complexity |
| Existing --provisional flag means minimal registry-side work | Must not become the default lazy path |

**Mitigation:** Provisional requires an explicit deferral reason. The intake
checklist requires attempting lifecycle-prove first; provisional is the
fallback, not the fast path.

**Affected repos:** `numan-registry` (schema, add-package.py policy docs),
`numan` (search/info/install UX for tier display).

---

### P7. Source-Build Support in Client (long-term unlock)

**Problem:** `artifact.kind: "source"` is schema-valid but explicitly rejected
by `add-package.py` ("deferred Phase 5 item"). This means every package must
have prebuilt binaries hosted somewhere. For the long tail of plugins that
compile fine but lack prebuilts for all platforms, this is a permanent blocker.

**Proposal (deferred — design only, no immediate implementation):**

- `numan install --from-source <pkg>` builds from the pinned `source.git` +
  `source.rev` using the user's local Rust toolchain.
- Registry entry with `kind: "source"` provides: git URL, rev, cargo crate
  name, and optionally a `Cargo.lock` SHA-256 for reproducibility.
- Build happens in a temporary directory; resulting binary is placed in the
  standard install location.
- No cross-compilation; builds only for the user's current platform.
- Requires user to have Rust toolchain installed (detected via `rustc --version`).

**Why defer:** This is a significant client feature (build orchestration, error
handling, toolchain detection, build caching). The other 6 proposals unlock
more packages with less effort. But this is the endgame for full ecosystem
coverage.

**Affected repos:** `numan` (new install path), `numan-registry` (allow
`kind: "source"` entries in add-package.py).

---

## Implementation Sequence

Ordered by unlock potential × implementation cost:

| Phase | Proposals | Timeline | Packages Unlocked |
|-------|-----------|----------|-------------------|
| **Phase A** | P1 (commit-snapshot) + P3 (partial-platform) | 1–2 weeks | ~24 tag-less + partial-build plugins |
| **Phase B** | P2 (archive intake) + P6 (provisional tier) | 2–3 weeks | 30+ modules/scripts/completions + credential-gated plugins |
| **Phase C** | P4 (fork policy) | 2–4 weeks | clipboard, compress, units (27K combined downloads) |
| **Phase D** | P5 (auto Nu-bump CI) | 3–5 weeks | Ongoing: prevents monthly catalog staleness |
| **Phase E** | P7 (source builds) | 2–3 months | Long tail of ecosystem |

Phases A and B are independent and can run in parallel. Phase C depends on
the fork policy being agreed, not on A or B. Phase D can start any time but
has highest value once the catalog is larger. Phase E is explicitly deferred.

---

## What This Does NOT Change

- **Cryptographic integrity:** SHA-256 hashes and Ed25519 signatures remain
  mandatory for all tiers.
- **Signed index:** Every package — proven or provisional, tagged or
  commit-snapshot — goes through the same signing workflow.
- **Immutable provenance:** `source_commit` / `source.rev` remains the anchor.
  We relax the requirement for a *human-readable label* (tag), not the
  *immutable reference* (commit SHA).
- **Archive bomb protection:** Client-side extraction limits (100 MiB, 10K
  files, no symlinks, no path traversal) stay unchanged.
- **Minimum Nu floor:** 0.112 minimum remains. Plugins below that are too far
  behind to provide user value.

---

## Success Metrics

| Metric | Current | Target (3 months) |
|--------|---------|-------------------|
| Total registry packages | 44 | 80+ |
| Promotable ecosystem coverage | ~95% of Nu 0.114 | ~95% of Nu 0.115+ (maintained automatically) |
| Raw awesome-nu coverage | 34% (22/65) | 55%+ (35/65) |
| Time from "plugin exists" to "in registry" | Days–weeks (manual) | Hours (automated lanes) |
| Platform coverage gaps per package | Binary: all or nothing | Partial shipping accepted |
| Monthly manual re-verification effort | High (all packages) | Low (auto-bump handles most) |

---

## Open Questions

1. **Hosting for non-binary archives:** Use GitHub Releases on `numan-registry`
   itself? A dedicated `numan-assets` repo? GitHub Pages with artifact storage?
2. **Fork org structure:** `tonythethompson/` (current) vs. a dedicated
   `numan-forks/` GitHub org for clearer separation?
3. **Provisional tier graduation SLA:** How long can a package stay provisional
   before it's either proven or removed?
4. **Commit-snapshot version bumps:** When upstream pushes new commits, do we
   auto-bump snapshots or wait for demand signal?
5. **Community attestation:** Could users submit lifecycle evidence for
   provisional packages to help them graduate? (Crowdsourced trust.)

---

## References

- [Gap Analysis — August 2026](# "Internal research document; 7-cycle campaign not committed to repo") (research campaign, 7 cycles)
- [`numan/docs/plans/consolidated-multi-repo-roadmap.md`](consolidated-multi-repo-roadmap.md)
- [`numan-registry/docs/roadmap.md`](https://github.com/tonythethompson/numan-registry/blob/main/docs/roadmap.md)
- [`numan-plugins/docs/backlog.json`](https://github.com/tonythethompson/numan-plugins/blob/main/docs/backlog.json)
- [`numan-registry/schemas/index-v1.json`](https://github.com/tonythethompson/numan-registry/blob/main/schemas/index-v1.json)
- [Six-month strategy audit (2026-07-19)](2026-07-19-six-month-strategy-audit.md)
