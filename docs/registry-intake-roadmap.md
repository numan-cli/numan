# Registry Intake Roadmap

**Status:** The Stage 1 lifecycle harness is implemented; authoritative production proof remains a manual workflow. The official registry is live with a production trust root. The August 2026 Intake Process Reform (governed by [ADR 0001](adr/0001-ecosystem-trust-upstream-contribution-fork-stewardship.md) and PR #113) establishes planned intake lanes across the ecosystem:
- **P1: Commit-Snapshot Intake** (`0.0.0-snapshot.<date>.<sha>` for unreleased/tag-less source repositories).
- **P2: Non-Binary Archive Intake** (`intake-archive.py` tooling in `numan-registry` for standalone Nu scripts, modules, and custom completions).
- **P4: Fork Stewardship** (`numan-maintained` distribution identity and transparent upstream attribution).
- **P6: Provisional Evidence Tier** (`evidence_tier: provisional` with policy-mandated deferral reasons for credential-dependent packages).

Continue growing the index in small curated batches while strengthening intake automation and promotion evidence. See the [README roadmap](../README.md#roadmap) for overall release milestones.

Numan should eventually make package onboarding feel close to "point it at a repo" while preserving the registry trust model. The target is not blind publishing. The target is a repeatable intake pipeline that can discover package metadata, validate artifacts, produce a reviewable registry candidate, and explain exactly what still needs human judgment.

This document describes the endgame and the staged path to get there.

## Goal

Given a Nushell package repository or release URL, Numan should be able to produce:

- a package classification: plugin, module, script, completion, or unsupported
- discovered metadata: name, owner, version, license, homepage, Nu constraints, platform support
- candidate artifact records with URLs, hashes, archive layout, and install paths
- a validation report covering install, lockfile state, activation readiness, and health checks
- a draft registry entry suitable for review

The future workflow could look like:

```bash
numan registry intake https://github.com/owner/repo
```

or, if publishing tools are split from the user-facing CLI:

```bash
numan-publisher intake https://github.com/owner/repo
```

The exact command name is less important than the contract: intake discovers and validates, but does not silently trust or publish.

## Near-term principle

Do not manually add a large batch of packages just to grow the registry. Use the first seeded real packages as canaries, prove the full lifecycle, and turn each pain point into automation.

For every real package candidate, the baseline acceptance path should be:

```text
search -> info -> install -> activate -> doctor -> list -> remove -> gc
```

Run that on a fresh `NUMAN_ROOT`, then inspect the lockfile and state directory after the important transitions. Once this path is boring for the seeded packages, add more packages in small curated batches.

## Package matrix

Early intake work should use a deliberately small matrix instead of a random pile:

| Candidate type | Why it matters |
|----------------|----------------|
| Simple pure-Nu module | Proves module layout, autoload generation, and activation identity |
| Binary plugin | Proves artifact selection, hashes, install layout, and plugin registration |
| Platform-specific package | Proves target matching and useful ineligible reports |
| Tight Nu-version constraints | Proves resolver behavior and user-facing compatibility errors |
| Intentionally unsupported package | Proves rejection reasons are clear and stable |
| nupm-imported package | Proves migration metadata and drift reporting, when relevant |

The matrix should drive automation requirements. If a field or check does not help one of these packages become safer or easier to onboard, it can wait.

## Staged plan

### Stage 1: Repeatable manual acceptance

Implemented by the Windows harness in [`tests/official_registry_stage1.rs`](../tests/official_registry_stage1.rs), its dispatch workflow, and the documented procedure in [`docs/acceptance/official-registry-stage1.md`](acceptance/official-registry-stage1.md). The harness:

1. creates a clean temporary `NUMAN_ROOT`
2. syncs the official registry
3. runs `search`, `info`, `install`, `activate`, `doctor`, `list`, `deactivate`, `remove`, and `gc`
4. records command output and exit codes
5. inspects lockfile records, payload paths, journals, and activation state
6. produces evidence for debugging; the manual workflow uploads it

This stage remains manual because it exercises the production registry and a real Nu plugin, but the harness removes guesswork from "does this real package work?"

### Stage 2: Registry/package lint

Add a local lint command for package entries before intake becomes ambitious. It should verify:

- required metadata fields
- scoped package ID shape
- semantic versions and Nu version constraints
- artifact URL presence
- artifact target triples
- SHA256 format and uniqueness
- archive path expectations
- license and provenance fields
- package type support
- activation expectations for plugins and modules

The linter should be useful for both hand-authored registry entries and generated candidates.

### Stage 3: Repo discovery

Add a read-only discovery command that can inspect a repository URL or local path and produce a report. It should look for:

- `nupm.nuon` metadata
- `mod.nu`, scripts, completions, or plugin binaries
- GitHub Releases and downloadable artifacts
- release archive naming patterns
- README, license, homepage, and repository metadata
- Nu version constraints, when declared
- platform support, when inferable

The output should separate facts from guesses. For example:

```text
discovered:
  package_type: module
  license: MIT
  nupm_metadata: present

needs_decision:
  registry_owner: not declared
  version_source: release tag or nupm metadata
  activation_command: not inferable
```

### Stage 4: Candidate generation

Once discovery is useful, generate draft registry entries. Candidate generation should:

- avoid publishing directly
- include provenance for every inferred field
- mark unresolved decisions explicitly
- write deterministic JSON for review
- include validation status alongside the entry

Candidate output should be reviewable in a PR. A maintainer should be able to see what Numan discovered, what it guessed, what it verified, and what still needs a human answer.

### Stage 5: Validation harness

For each generated candidate, run a real acceptance harness:

- download artifacts
- compute and compare hashes
- inspect archive layout
- install into a clean root
- verify immutable payload paths
- check lockfile records
- run `numan doctor`
- for modules, validate managed autoload output
- for plugins, optionally activate against a real Nu binary in CI

Validation should produce a machine-readable report and a concise human summary.

### Stage 6: Registry PR generation

When candidate generation and validation are stable, add tooling to open a registry PR. The PR should include:

- registry entry JSON
- validation report
- provenance summary
- known limitations or unsupported variants
- release artifact hashes
- license notes

This is the point where onboarding becomes close to one command for well-formed packages.

## Trust boundary

Automation must not weaken Numan's trust story.

Intake can:

- discover metadata
- download public artifacts
- compute hashes
- inspect archives
- run isolated install and activation checks
- draft registry entries
- prepare PRs

Intake must not:

- publish a package without review
- invent missing compatibility claims
- silently accept source builds
- skip hash verification
- mutate a user's Nu environment during discovery
- treat GitHub repository metadata as proof of artifact integrity

Source builds remain a separate consent scope. If a package has no usable release artifact, intake should report that clearly instead of hiding a build step inside discovery.

## Success criteria

This roadmap is working when:

- seeded real packages pass the full lifecycle on a clean root
- adding the next package mostly means answering a small set of explicit questions
- generated candidates are deterministic and reviewable
- unsupported packages produce actionable rejection reasons
- platform and Nu-version ineligibility is visible before install
- registry maintainers trust the validation report enough to review faster

The endgame is not a giant hand-curated registry. It is a small, trustworthy registry that can grow through evidence-backed intake.
