# ADR 0001: Ecosystem Trust, Upstream Contribution, and Fork Stewardship Policy

## Status

Accepted.

## Context

Nushell's ecosystem has fragmented discovery, uneven maintenance, weak
provenance conventions, and no single authoritative package registry. Users
encounter packages through Git repositories, community lists (e.g.
`awesome-nu`), crates.io, `nupm` installations, copied scripts, or abandoned
repositories. Because Nu plugin registration can execute plugin binaries,
package discovery and activation are supply-chain decisions, not merely
convenience features.

Numan must not solve this by silently becoming the owner of every broken
package it touches. Its role is to make provenance, compatibility,
integrity, maintenance status, and risk visible and enforceable — not to
absorb unbounded maintenance burden.

This ADR governs **all** package sources Numan can discover or install
from — the official registry, custom registries, and `nupm`-discovered
packages — not just `tonythethompson/numan-registry`. It is a client-side
(`tonythethompson/numan`) policy document.

### Relationship to existing work

This separates invariants and mechanisms that already exist from follow-up
requirements this ADR introduces:

- **Already implemented, unchanged by this ADR:**
  - Install is inert; only `activate` touches Nu (`AGENTS.md` Architecture Rule 1).
  - Ed25519 trust-store and registry verification primitives exist in
    `src/core/trust.rs` and `src/core/registry.rs`
    (`RegistryManager::verify_and_load`, `RegistryManager::load_verified`).
  - Artifact SHA-256 is mandatory for plugin artifacts (`AGENTS.md` Architecture Rule 6).
  - Lockfile snapshots before mutation; activation uses journal recovery
    (`AGENTS.md` Architecture Rule 7, Phase 3).
- **Not yet implemented — follow-up requirement from this ADR:**
  atomic verified registry sync that downloads index+signature to temporary
  paths, verifies/parses the downloaded bytes, and preserves the active cache
  on failure. Today `numan registry sync` is implemented in
  `src/cmd/registry.rs` and does not provide that promotion/rollback behavior;
  the "Registry Trust Requirements" section below records the target behavior.
- **A distinct, narrower layer this ADR does not replace:**
  `docs/nupm-compatibility.md` owns the *mechanics* of importing one
  specific source format (`nupm.nuon` parsing, `NupmOutcome`,
  `NupmReasonCode`, import provenance in `state/nupm-imports.json`). That
  document answers "can this nupm-format package be imported at all." This
  ADR answers an orthogonal question that applies regardless of source
  format: "what is Numan's trust and maintenance relationship with this
  package's origin." A package can simultaneously be `importable_now` in
  nupm-compatibility terms and `unreviewed discovery result` in this ADR's
  terms — the two taxonomies compose, they don't collapse into one.
- **Also not yet implemented — genuinely new work:**
  the package status taxonomy, the metadata model fields (`fork_of`,
  `patches`, `maintenance_status`, etc.), and the `inspect` / `audit` /
  `provenance` / `compatibility` UX surface. `src/cmd/info.rs` is the
  closest existing analog and is the natural extension point; there is no
  `audit`, `provenance`, or `compat` command today.

### A concrete instance of the policy this ADR describes

While evaluating `nushell/nu_scripts` as a source for a registry seed
package (`tonythethompson/numan-registry` PR #2), `modules/maths/math_functions.nu`
was found broken on current Nu (deprecated `merge` closure syntax, plus a
call to an undefined `column2` helper only ever defined in an unrelated
file). Rather than patching a local copy or forking, the fix was pushed
upstream: [nushell/nu_scripts#1265](https://github.com/nushell/nu_scripts/pull/1265).
This is Lane 1 (Upstream contribution) from this ADR, exercised before the
ADR existed — evidence the policy matches practice that already works,
not just a theoretical preference.

## Decision

Adopt the following policy.

### Core principle

Separate **verification**, **compatibility work**, and **maintenance
ownership**. Verification is evidence. Maintenance is responsibility.
Numan must never use a vague label like "approved" that could imply an
unsupported security or quality guarantee — package records use factual
status labels only.

### Package status classes

1. **Upstream tracked** — original upstream source and release/commit, no
   Numan source changes, installable only with clear provenance metadata.
2. **Numan verified upstream artifact** — provenance, integrity, license,
   Nu-version compatibility, supported platforms, and CI evidence
   recorded. Numan does not claim upstream source is security-audited and
   does not own upstream maintenance.
3. **Numan compatibility-patched** — a small, explicit, reviewable
   patchset against a pinned upstream revision, normally corresponding to
   an open upstream issue/PR. A temporary bridge, not a hidden permanent
   fork. Every downstream change is visible and reproducible.
4. **Numan maintained fork** — a distinct Numan-owned distribution, used
   only where Numan has deliberately accepted ongoing responsibility for
   compatibility, releases, security response, and support. Must never
   silently replace an installation request for the original upstream
   package.
5. **Known incompatible** — real, identified package that doesn't work
   with a specified Nu version, platform, or Numan safety requirement,
   with the reason and scope stated.
6. **Unreviewed discovery result** — surfaced but not endorsed; must never
   be presented alongside verified artifacts without obvious
   differentiation.
7. **Quarantined or unsupported** — unclear license, unavailable source,
   insufficient provenance, unsafe behavior, unacceptably stale
   maintenance, or validation is not reproducible. Numan explains why it
   is not installable through trusted flows.

### Boundary: upstream fix vs. Numan patch or fork

Default rule: **push the fix upstream first.**

Upstream owns product behavior: plugin command behavior, parsing
correctness, data handling, user-facing semantics, plugin functionality,
UX defects, project-specific build logic where upstream supports the
affected platform.

Numan owns distribution and compatibility behavior: package metadata,
registry integrity, source provenance, artifact hashes, platform
packaging, reproducible build recipes, Nu-version compatibility
declarations, activation safety, lockfile/rollback behavior, installation
layout, machine-readable compatibility evidence.

Examples:

- A plugin command mishandles JSON → submit an upstream fix.
- A healthy upstream package lacks metadata needed for safe Numan
  installation → solve it in Numan packaging metadata.
- A plugin doesn't compile against a current Nu plugin protocol →
  upstream PR first; Numan patch only when users are blocked and the
  patch is bounded.
- A package can't support a platform because upstream intentionally
  doesn't → don't assume Numan should take ownership; evaluate whether
  that platform is strategically worth maintaining.

### Decision lanes

- **Lane 1 — Upstream contribution.** Upstream is active/reachable, the
  change fits the project's purpose, the patch is likely accepted,
  waiting for review/release doesn't materially block users. Numan
  distributes the upstream release or an explicit pinned commit.
- **Lane 2 — Temporary compatibility patch.** Concrete user-facing
  breakage exists, an upstream issue/PR has been opened (unless urgency
  or security makes that impractical), the patch is narrow/testable/
  reversible, users need a working package before upstream merges, Numan
  can build and test reproducibly. Must record `upstream`,
  `upstream_revision`, `patch_status: temporary`, `patches`,
  `upstream_issue`, `exit_condition`. Never silently substituted for
  upstream — the user must be able to inspect the exact patch and choose
  the upstream artifact.
- **Lane 3 — Numan maintained fork.** Only when *all* of: (1) concrete
  meaningful breakage or unmet need; (2) upstream abandoned,
  unresponsive, unable to release, rejects the necessary direction, or
  structurally unable to support the outcome; (3) license permits
  redistribution/modification/ongoing maintenance; (4) bounded initial
  scope with a credible maintenance plan; (5) Numan can build, test,
  sign, and release for every claimed platform; (6) Numan accepts
  ownership of Nu protocol changes, dependency CVEs, CI failures, bug
  reports, and releases over time; (7) maintaining it is cheaper/safer
  than repeatedly asking users to repair it manually. Governing question:
  *"Are we willing to be responsible for this package when Nu changes,
  when its dependencies receive CVEs, and when users report breakage two
  years from now?"* If no, do not fork.
- **Lane 4 — Unsupported.** Source provenance can't be established,
  licensing is unclear, source can't be built/reviewed, artifact can't
  meet integrity/reproducibility requirements, unsafe to activate, or
  maintenance cost is disproportionate to value. Surfaced only as an
  explicitly unsupported discovery item, with reasons.

### Fork identity and provenance

A maintained or patched package must never impersonate upstream identity.
Use a separate distribution identity (e.g. `upstream: owner/plugin`,
`distribution: numan-compat/owner-plugin`, `fork_of: github.com/owner/plugin@<rev>`).
Installing `owner/plugin` must never silently install
`numan-compat/owner-plugin`.

Inspection output and machine-readable metadata must distinguish: original
source repository and revision/tag; Numan distribution identity; fork/patch
status; exact patchset; signer; artifact SHA-256; build recipe and
environment fingerprint where available; Nu compatibility range; tested
platforms; test date; known risks; maintainer and escalation path; exit
condition for temporary patches.

### Metadata model (versioned, extensible)

At minimum: `source_url`, `source_revision`, `release_tag`, `license`,
`publisher_identity`, `artifact_sha256`, `registry_signer`,
`nu_version_range`, `tested_platforms`, `last_verified_at`,
`maintenance_status`, `fork_of`, `patches`, `upstream_issue`,
`build_recipe`, `build_environment_fingerprint`, `review_notes`,
`known_risks`, `maintainers`, `support_scope`, `exit_condition`.

### User-facing UX

Users must be able to answer, before installing or activating: where did
this come from; is it upstream, patched, forked, or merely discovered; who
signed the registry entry and artifact metadata; what source revision
produced this artifact; does it work with my Nu version/platform; what did
Numan modify; is this temporary or a maintained fork; what known risks
exist; who owns future maintenance. This information must not be buried in
verbose output or external docs only — extend `numan info` (or add
`inspect` / `audit` / `provenance` / `compatibility`) to surface it
directly.

### Registry trust requirements (target behavior, partially implemented)

Registry sync must: download index+signature to temporary paths; require a
signature for trusted registries; verify against the configured Ed25519
trust key over the exact downloaded bytes; parse and validate; atomically
promote into the local cache; preserve the prior verified cache on any
failure; treat unsigned use as an explicit, narrowly-scoped
development-only escape hatch. Never write an unverified index into the
active cache and verify later.

### Governance

Every Numan-maintained fork requires a stewardship record (e.g.
`MAINTAINERS.md`) stating: why the fork exists; original upstream and
revision; why upstream contribution was insufficient; named maintainers;
supported Nu versions/platforms; CI/release evidence; known technical
debt; security update responsibility; review cadence; upstreaming or
retirement exit condition.

## Implementation Priorities

Do not begin by creating forks. Sequence:

1. This ADR and status taxonomy.
2. Versioned package metadata schema.
3. `inspect` / `audit` output exposing provenance and maintenance state.
4. Atomic verified registry sync with active-cache preservation on failure.
5. CI evidence capture for verified artifacts.
6. Temporary compatibility-patch workflow.
7. Maintained-fork workflow, only after the above exists.
8. Discovery-only and quarantined-package handling for `nupm`
   interoperability (composes with, does not replace,
   `docs/nupm-compatibility.md`).

## Consequences

**Positive:**

- A user can tell whether an artifact is upstream, patched, forked, or
  unreviewed before installation.
- Numan never silently substitutes a fork for an upstream package.
- Every Numan patch or fork traces to an upstream source revision and
  explicit downstream changes.
- Numan doesn't claim more security assurance than its evidence supports.
- The default path for ordinary bugs remains an upstream contribution, not
  a Numan fork — bounding long-term maintenance burden.
- Numan becomes a trustworthy compatibility/stewardship layer rather than
  an uncontrolled collection of copied packages.

**Negative / costs:**

- A new metadata schema and UX surface (`inspect`/`audit`/`provenance`) is
  real, unscoped implementation work — this ADR sequences it but doesn't
  reduce it.
- Two taxonomies now exist side by side (this ADR's package status classes
  vs. `nupm-compatibility.md`'s `NupmOutcome`) — future contributors need
  to understand both apply, to different questions, and don't collapse
  into one field.
- Lane 3 (maintained fork) is deliberately high-friction by design; if a
  genuinely important package needs forking, this policy will slow that
  down rather than speed it up. That's the intended trade-off, not an
  oversight.

## Acceptance Criteria

- A user can tell whether an artifact is upstream, patched, forked, or
  unreviewed before installation.
- Numan never silently substitutes a fork for an upstream package.
- Every Numan patch or fork can be traced to an upstream source revision
  and explicit downstream changes.
- Numan does not claim more security assurance than its evidence supports.
- Registry indexes are cryptographically verified before becoming active
  cache state.
- A maintained fork exists only where Numan has consciously accepted
  ongoing maintenance responsibility.
- The default path for ordinary bugs remains an upstream contribution, not
  a Numan fork.
- Numan becomes a trustworthy compatibility and stewardship layer rather
  than an uncontrolled collection of copied packages.
