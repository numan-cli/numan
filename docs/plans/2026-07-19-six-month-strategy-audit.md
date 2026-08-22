# Audit: Six-month Numan strategy (2026-07-19)

> **Status:** Audit of an Opus/Fable strategy pass. Preserved as decision input.
> **Not** an implementation plan. Code-centered next steps live in
> [`2026-07-19-next-steps-code.md`](2026-07-19-next-steps-code.md).
>
> **Grounding claimed by source:** ADR 0001, Phase7Plan (Phase 7 complete,
> v0.1.4, trust root `official-2026-07-01`), registry intake materials,
> numan-plugins manifest/backlog (3 active: highlight, regex, dns).

## Code reality check (do not skip)

The strategy text below is useful but several claims need correction against
the tree as of 2026-07-19:

| Strategy claim | Actual code |
| --- | --- |
| `numan setup nu --pin` | Flag is already `numan setup nu --version <VER>` (`src/cmd/setup.rs` `NuSetupArgs::version`) |
| Ship `numan try` | Already exists: `src/cmd/try_cmd.rs`, wired in `src/cli.rs` |
| Search has no compat awareness | `search` already hides incompatible by default; `--all` marks them (`src/cmd/search.rs` + `Resolver`) |
| Install fails as a surprise | `Resolver::format_resolve_error` already suggests `setup nu --version {pin}` (`src/core/resolve.rs`) |
| Client lacks `source` / `verified_with` | `VersionEntry` already has both (`src/core/package.rs`); `info` already prints `verified_with` |
| Provenance dropped at intake | Confirmed: `numan-registry/scripts/add-package.py` `build_version_entry()` does **not** pass through `source` |
| Active plugins = 3 | Confirmed in `numan-plugins/manifest.json` `active[]` (highlight, regex, dns). Live index may also include modules (e.g. nutest) and other packages; do not equate “3 plugins” with “3 registry packages” |

Implications for planning: prefer **surface upgrade + provenance fill-in +
catalog depth** over greenfield compat/try/setup work.

---

## 1. Three bets for six months, and what not to build

**Bet 1: Catalog depth via the numan-plugins pipeline (numan-plugins + numan-registry).**
The adoption constraint right now is not UX, it's that the registry has three
plugins. The build matrix, spec generation, and intake script already work;
marginal cost per plugin is low, and each new plugin is a forcing function for
intake Stages 1 and 2 (acceptance script, registry lint). Target: top-demand
backlog tier promoted in small batches, roughly 12 to 15 active plugins. This
is the bet with compounding returns because every friction found becomes
automation per the intake roadmap's own principle.

**Bet 2: The compatibility truth surface plus `numan try` (numan).**
The known trap (search looks fine, install fails on Nu-minor mismatch) is
exactly the experience that burns a less experienced user permanently. Ship:
environment-aware compat display in `search`/`info`, harden `numan try`, and
the `numan setup nu` pin offer (already `--version`). This is the north star
made concrete: honest search→install→activate for someone who does not know
what an ABI is.

**Audit note:** Much of Bet 2 is *upgrade existing code*, not invent it. See
reality check table.

**Bet 3: Provenance metadata v1 in the signed index (numan-registry, then numan `info`).**
ADR 0001 items 1 to 3: status taxonomy, versioned metadata schema, and `info`
extension. Plus the documented registry-side gap: teach `add-package.py`
`build_version_entry()` to pass through the `source` block so
`git`/`rev`/`cargo_name` land in the signed index instead of living only in
numan-plugins' manifest. Doing this while the catalog is small is cheap;
retrofitting provenance onto a hundred entries later is not. It is also the
trust differentiator versus "a list of links."

**Explicitly do not build:**

- Side-by-side Nu profiles as an ABI workaround
- On-user-machine source builds (Phase 5.2 stays deferred; numan-plugins
  central builds are the safer substitute)
- Any Lane 3 maintained fork (ADR sequences fork workflow last)
- Intake automation Stages 4 to 6 before Stages 1 and 2 are boring
- Self-serve community publishing to the official index
- A registry website

Each either violates an invariant, front-runs prerequisites, or spends effort
where the catalog bottleneck is not.

---

## 2. Presenting the plugin/module asymmetry

Users feel lied to when an earlier surface implies a promise a later surface
breaks. The fix is to make compatibility an evaluated fact against *their*
environment at every surface, and to state what was checked.

Search target shape:

```text
checked against: Nu 0.115.0 (PATH)
  cptpiepmatz/nu-plugin-highlight  1.4.15  plugin  ✗ needs Nu 0.113
  nushell/math-functions           0.2.0   module  ~ not ABI-locked (verified with 0.113)
```

Three rules:

1. Plugins get a hard evaluated verdict (constraint vs detected Nu).
2. Modules must not be oversold as "works anywhere." Prefer
   "not ABI-locked; verified with 0.113" using `verified_with`, not a green
   checkmark that implies a guarantee. Nu source can still drift (e.g.
   deprecated `merge` closure syntax in nu_scripts math helpers).
3. Explain the asymmetry once in `info` for plugins: "Plugins are compiled
   programs matched to a specific Nu version. Modules are Nu source and are
   not ABI-locked, but may still break on syntax or API drift."

`install` must never fail for a reason `search` did not already show. If
`search` said incompatible and the user installs anyway, the error repeats the
same fact and the same options. Consistency across surfaces prevents the
"lied to" feeling more than any wording.

---

## 3. ADR 0001 taxonomy: ship first vs wait, with copy

**Ship first** the distinctions that describe packages that exist today:

- Official registry packages: class 2 (`verified upstream artifact`, built by
  numan-plugins from a pinned upstream tag)
- nupm-discovered packages: class 6 (`unreviewed discovery`)
- Known incompatible: annotation from resolver verdict (not a separate empty class)

v1 labels: `verified upstream artifact`, `unreviewed (discovered via nupm)`,
plus incompatibility annotation. Surface in `search`/`info`; `doctor` reports
registry signature state and trust root id. Extend `src/cmd/info.rs` per ADR;
do not add `audit`/`provenance` subcommands yet.

**Wait:** classes 3 and 4 (compatibility-patched, maintained fork); quarantine
UX until intake automation exists; full 20-field metadata model. Ship fields
the pipeline already produces: source pin, sha256, signer, nu range, verified
platforms, license, last verified.

Trusted package copy (`numan info cptpiepmatz/nu-plugin-highlight`):

```text
cptpiepmatz/nu-plugin-highlight 1.4.15 (plugin)
  status:     verified upstream artifact
  upstream:   github.com/cptpiepmatz/nu-plugin-highlight @ v1.4.15+0.113.1
  built by:   numan-plugins CI (unmodified source)
  signed:     official registry key official-2026-07-01
  sha256:     verified at install
  requires:   Nu 0.113   platforms: linux, macOS, windows (tested 2026-07)
  note: verified means provenance, integrity, and install/activate were
  checked. Numan has not security-audited the upstream source.
```

Unreviewed discovery (nupm import path):

```text
someuser/nu-utils 0.3.0 (module, discovered via nupm)
  status:     unreviewed. Not in the official registry.
  source:     github.com/someuser/nu-utils (no pinned revision)
  integrity:  no artifact hash, no signature
  compat:     not tested by Numan
  Numan can import this, but nothing about it has been verified.
  Continue with: numan nupm import someuser/nu-utils
```

Per ADR: unreviewed items are surfaced and installable, never visually
interchangeable with verified ones, never described as vague "approved."

---

## 4. Search-fine, install-fails UX

Detect mismatch at search time (index constraint vs cached/PATH Nu and any
managed pin). Install failure is confirmation, not surprise.

Search footer target:

```text
1 result is not compatible with your current Nu.
Run 'numan info dead10ck/nu_plugin_dns' for options.
```

Install target:

```text
error: dead10ck/nu_plugin_dns 4.0.10 requires Nu 0.113.
       Your PATH Nu is 0.115.0. Plugins only work with the exact
       Nu minor they were built for, so this install would produce
       a plugin your shell cannot load.
  Options:
    1. Let Numan install a managed Nu 0.113.1 alongside your current
       one:  numan setup nu --version 0.113.1
       (installs under your Numan root; your PATH Nu is not touched)
    2. Wait for an artifact rebuilt for Nu 0.115:
       https://github.com/tonythethompson/numan-plugins/issues
    3. Nothing was installed. No changes were made.
```

Nonzero exit, no stack trace, never "try `registry sync`" as the ABI fix.
Pin path: `setup nu --version` states what it will do, asks consent, installs
managed Nu, prints how to run/undo, never edits PATH/rc without separate
explicit yes (`--skip-path` already exists). After a pin exists, search header
should show both Nus and evaluate compat against both when meaningful.
`doctor` should report both and which plugins activate against which.

---

## 5. "Add package X for Nu 0.114": ownership, order, handoffs

Order is strictly **numan-plugins → numan-registry → numan**:

1. **numan-plugins:** add X to `manifest.json` `active[]`; run build-plugins
   workflow; release `<name>-<version>` with per-target archives +
   `spec-<name>.json`.
2. **numan-registry:** drop spec into `specs/`; `add-package.py --spec …
   --write`; lifecycle-prove on clean `NUMAN_ROOT` per OS against real Nu;
   PR; staging sign; production sign + publish.
3. **numan:** normally nothing unless new schema/display fields. Users get X
   via `numan registry sync`.

Handoff failures today:

1. Spec manually ferried from CI artifact into `specs/` (stale risk)
2. Provenance dropped: `build_version_entry()` omits `source`
3. Lifecycle-prove is manual per OS (skipped under time pressure)
4. No machine check that manifest Nu compat equals registry `nu_version`
5. Mutable release assets on workflow rerun can orphan signed hashes

Fix order of payoff: automate spec handoff, pass through `source`, script
Stage 1 acceptance in CI, manifest-vs-index lint, immutable-release policy
in numan-plugins.

---

## 6. Nu core maintainers: goodwill vs threat

**Glad:** Numan absorbs install/compat pain; pushes fixes upstream (Lane 1;
nu_scripts#1265 as evidence); never silently forks; generates demand signal.

**Threatened:** looks like nupm replacement instead of interop; single-person
trust root with no governance story; mass forking instead of contributing.

**Highest-goodwill RFC (later):** plugin release convention + reusable GitHub
Action donated from numan-plugins build matrix. Keep off the immediate
critical path until intake Stages 1–2 are boring.

---

## 7. Priority list: third-party plugins with real install friction

Keep the catalog focus on what people actually have trouble installing. Do not
compete with plugins or commands that already ship with Nu (see §8).

Priority candidates (re-verify each upstream `Cargo.toml` Nu pin and archive
shape before promotion):

- `yybit/nu_plugin_compress`: archive compression (zstd/gzip/bzip2/xz)
- `Neuron-Mr-White/nu_plugin_terminal_qr` (or `FMotalleb/nu_plugin_qr_maker`)
  : terminal QR codes
- `kaathewisegit/nugins` (`nu_plugin_endecode`): extra `encode`/`decode`
  schemes (nugins monorepo, crate at `endecode/`)
- `JosephTLyons/nu_plugin_units`: common unit conversions
- `ArmoredPony/nu_plugin_hashes`: extended hash functions
- `amtoine/nu_plugin_kdl`: KDL format support
- `cptpiepmatz/nu-jupyter-kernel` (`crates/nu_plugin_plotters`): chart
  plotting; or `Euphrasiologist/nu_plugin_plot` if it remains the better
  maintained plotting option
- `devyn/nu_plugin_dbus`: Linux D-Bus interface

These are not all 0.114-ready; the goal is to remove install friction through
upstream outreach, commit-snapshot intake where there is no tag, or a
`numan-maintained` fork under ADR 0001 when the maintainer has moved on.

Gated / defer: `audio_hook` (stale Nu tag); `from_beancount`, `nuts`, `vec`
and other pre-0.112 plugins unless a Nu-bumped tag or approved fork appears.

Source of truth for backlog data: `numan-plugins/docs/backlog.json`.

---

## 8. Official / bundled plugins and native commands (not via numan)

Numan does not install, distribute, or compete with the plugins and commands
that ship with Nu or that have become experimental native commands. Those come
from the user's Nu channel, package manager, or `cargo install nushell`, not
from the Numan registry.

Core / bundled plugins that ship with Nu releases:
`nu_plugin_polars`, `nu_plugin_query`, `nu_plugin_formats`,
`nu_plugin_gstat`, `nu_plugin_inc`.

Experimental native commands that replace earlier plugins:
`clip` / `clipboard` (Nushell 0.111+; supersedes `FMotalleb/nu_plugin_clipboard`).

**Policy for `search`, `info`, `install`:**

- Label these as `core (ships with Nu)` or `native command`.
- `numan install` does not offer them.
- `numan activate` may register a binary already present beside `nu` or on
  `NU_PLUGIN_DIRS`, but it does not download or own the install.
- `numan doctor` may report present/registered/unregistered; for absent core
  plugins it points the user to their Nu channel (or `cargo install
  nu_plugin_polars`, etc.) without owning the install.
- Revisit only if a major Nu channel ships without core plugins.

---

## 9. Registry quality bar

**Hard rejects:** missing `sha256` on binary artifacts; no pinned tag/rev; no
explicit Nu constraint; missing/non-redistributable license; unresolvable
artifact URL at intake.

**Evidence requirements (warn if missing, do not silent-omit):** provenance
chain in signed index (`source` passthrough); rebuildable recipe (workflow +
manifest pin + toolchain); hashes computed at intake from uploaded assets;
per-platform lifecycle-prove (`search → info → install → activate → doctor →
list → remove → gc` on clean root against real Nu); maintenance signal.

**Load-bearing element:** lifecycle proof. Script it in CI so the bar is
enforceable.

**ADR caveat:** never summarize as "approved" or "audited." Provenance +
integrity + install behavior only.

---

## Non-goals (repeat)

Side-by-side Nu profiles; client-side source builds (5.2); Lane 3 forks;
intake Stages 4–6 early; self-serve official publishing; registry website.
