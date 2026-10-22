# Numan TUI: CLI Parity Design

Date: 2026-10-22
Status: Implemented. Phase 0 (Clap-derived catalog, Commands section, guided forms, editable re-validated preview), Phase 1 (captured read-only execution), and Phases 2-4 (generic reviewed execution for mutating, prompt-isolated, and external-system commands via execute_cli) are in the tree. Remaining exceptions are listed under Parity exceptions.
Supersedes, on approval: the CLI-only inventory in
`docs/specs/2026-10-18-optional-tui-design.md` § "Native commitment and CLI-only inventory"

## Selected decisions

| Decision | Choice |
|---|---|
| Delivery for this step | Design first, then implemented in Phases 0-4 (see Status). |
| Execution model | Execute inside the TUI worker. Print-and-exit hand-off is no longer the end state. |
| Argument input | Guided form per command, plus an editable result that is re-validated before execution. |

## Goal

Every leaf command reachable from `numan --help` is findable, fillable, reviewable, and
executable from the TUI, with the same state effects and safety gates as the CLI, or is
listed in § "Parity exceptions" with a named blocker. Parity must be structural, not
hand-maintained: adding a CLI command without a TUI descriptor must fail a test.

## Success criteria

1. A `Commands` section lists all leaf commands, arrow-key selectable, filterable, grouped
   by CLI family, showing risk tier and availability for every entry.
2. Selecting a command opens a form whose fields cover every positional, option, and flag
   of that command. Values pre-fill from TUI context where one exists (selected package,
   selected registry, resolved root).
3. The form renders a canonical command preview. The preview is editable; any edit is
   tokenized, re-parsed, and re-validated before execution is offered.
4. Execution runs in the existing single worker, under the existing mutation lock,
   snapshot, journal, and trust rules. No command spawns a shell.
5. Read-only browsing, form opening, and editing cause no network request and no write.
6. A completeness test walks the Clap command tree and fails if any leaf lacks a
   descriptor, a form schema, or an execution classification.

## Non-goals

- No arbitrary shell input, pipelines, redirection, or environment expansion anywhere.
- No batch/multi-command queue. One command at a time, as today.
- No cancellation promise. The current "cancellation unavailable after submission"
  contract in `src/tui/app.rs:636` stays until a workflow implements safe cancellation.
- No change to CLI argument names, output contracts, or exit codes.
- No new state format. The lockfile remains ground truth.

## Current state (evidence)

- CLI surface: 22 top-level commands in `src/cli.rs:23`. Leaf groups: `hook` (3),
  `registry` (5), `snapshot` (4), `setup` (2 plus 3 `setup nu` actions), `nupm` (4),
  `overlay` (3), plus 16 singletons including `tui`.
- TUI native actions today: `Install`, `Activate`, `Update`, `Remove`, `Scan` in
  `src/tui/operations.rs:21`, plus registry sync via `Shift+R` in `src/tui/app.rs:678`.
- Everything else is a print-and-exit hand-off from `launcher_items()`
  (`src/tui/app.rs:272`): 4 selected-package and 16 global entries, 6 of which are only
  `--help` placeholders. Hand-offs never execute (`src/tui/app.rs:441`,
  `src/cmd/tui.rs:47`).
- Absent entirely, not even as hand-offs: `hook enable|disable|list`,
  `overlay use|hide|list`, `migrate`, `use <version>`, concrete
  `registry add|remove|packages`, `snapshot inspect|delete|rollback`,
  `nupm status|inspect|import`, `activate --list`, all concrete `setup`/`completions`
  invocations.
- The only text inputs are filter and launcher query (`src/tui/app.rs:84`). There is no
  way to type a package ID, path, snapshot ID, registry key, or Nu version. This is the
  root cause of most of the gap, and the form system below is the fix.
- Handlers that read stdin or gate on TTY are not worker-safe today: `remove`
  (`src/cmd/remove.rs:49`), `snapshot` delete/rollback (`src/cmd/snapshot.rs:264`,
  `:284`), `setup` (`src/cmd/setup.rs:409`), `migrate` (`src/cmd/migrate.rs:119`), and
  the Nu pin offer (`src/cmd/nu_pin_offer.rs:29`).
- Output capture already exists and is thread-scoped: `util::output::with_sink`
  (`src/util/output.rs:20`), fed into the transcript by the worker in
  `src/tui/mod.rs:28`. Adapters must route through it, never through process stdio.

## UX: the Commands section

### Placement

Add `Commands` as a fourth destination beside Installed, Discover, and Updates
(`View` in `src/tui/app.rs:7`). Navigation (`1`..`4` is not used today; keep arrow/Tab
focus movement and add the destination to the nav list in `src/tui/ui.rs:92`). The
existing `Ctrl+P` launcher stays and deep-links into the same catalog; it does not get a
second, divergent command list.

### List behavior

- One row per leaf command: canonical path (`registry add`), one-line summary from the
  Clap `about` text, risk tier badge, availability (`Ready`, `Needs context: select a
  package`, `Blocked: pending recovery journal`, `Phase 3 — not yet executable`).
- `/` filters by path and summary, reusing the filter overlay. Filtering never executes.
- Rows are derived from the catalog (§ "Catalog"), never hard-coded in `ui.rs`.
- Non-executable rows (later-phase or parity-exception) remain listed and selectable so
  the section is a complete map of the CLI from Phase 0. Selecting one opens its form in
  preview-only mode with the blocker stated. Hiding them would recreate today's gap.

### Form behavior

Each field maps to one Clap argument:

| Field kind | Used for | Input behavior |
|---|---|---|
| `PackageId` | package positionals | Picker from installed/catalog rows plus free type, validated by `ScopedId::parse` |
| `Version` | `@version`, Nu versions | Free type with candidate list where cached data exists |
| `Path` | `--project-root`, `--nupm-home`, `setup nu use <path>`, `nupm` paths | Free type, no existence assumption; existence shown as a hint, checked at review |
| `SnapshotId` | snapshot positionals | Picker from `list_snapshots` plus free type |
| `RegistryName`/`Url`/`Key` | `registry add` | Free type; key field masks nothing but validates base64 at review |
| `Enum` | completion shell, `use list|latest|<version>` | Arrow-cycle, no free type except where the CLI also accepts free values |
| `Bool` | `--force`, `--yes`, `--dry-run`, `--check`, `--self`, `--scan`, `--json`, `--all`, `--no-activate`, loader flags | Toggle, default always the CLI default; `--yes` is never pre-set or persisted |
| `Text` | search query | Free type |

Form rules:

- Required fields block review until valid. This mirrors Clap, so the same command
  cannot be valid in one surface and invalid in the other.
- Optional arguments left unset are omitted from argv, so CLI defaults apply unchanged.
- Context pre-fill is a starting value, not a constraint: a `PackageId` pre-filled from
  the selected row remains editable to any other ID.
- Opening, editing, and abandoning a form writes nothing and performs no network I/O.
  Candidate lists come from already-loaded local data; if none is loaded, the picker is
  empty and free type still works.

### Editable result and review

Below the form, a preview shows the canonical invocation as an argv vector rendered
readably, e.g. `numan --root <root> snapshot rollback <id> --yes`.

- The preview is editable as text, but it is never executed as text. On any edit:
  1. Tokenize with a non-shell tokenizer (whitespace and quotes only; `; | & $ ( )`,
     redirection, and newlines are literal characters that then fail validation, or are
     rejected outright — implementation picks reject-outright).
  2. Re-parse with `Cli::try_parse_from`. A parse error returns to the form with the
     Clap error shown verbatim; execution is not offered.
  3. Re-derive the descriptor from the parsed command path. If the edit changed the
     command, the form rebuilds to that command's fields.
  4. Invalidate any prior review/effects summary and state fingerprint. Execution
     requires a fresh review of the edited request.
- This preserves the user's chosen "form + editable result" without opening a shell
  injection surface. Editing is an argument editor, not a command line.

### Review and execution

- Read-only commands execute directly after form validation (no review step), show
  output in a scrollable result overlay, and append to the transcript (`l`).
- Mutating commands reuse the existing review contract: typed effects, fingerprint
  revalidation, confirm defaults to Cancel (`Overlay::Review`, `src/tui/app.rs:90`).
  Current `prepare`/`validate`/`execute` in `src/tui/operations.rs:234` is the model;
  it generalizes per family rather than being replaced.
- Pending recovery journals block mutating execution with the existing message
  (`src/tui/operations.rs:175`) until the recovery workflow itself is executable
  (Phase 3); browsing and read-only commands stay available.
- After any execution, authoritative state reloads (`Command::Reload`) and the result
  is read back, as `execute` already does (`src/tui/operations.rs:487`). A success
  message alone is never the evidence.

## Catalog: parity by construction

```rust
pub struct CommandSpec {
    pub path: &'static [&'static str],        // e.g. ["registry", "add"]
    pub summary: &'static str,                // must equal Clap about text
    pub tier: Tier,                           // ReadOnly | RootMutation | Destructive | Network | External | ShellEmitting
    pub fields: &'static [FieldSpec],         // one per positional/option/flag
    pub executor: Executor,                   // adapter id, or Blocked(reason, phase)
}
```

- One `&[CommandSpec]` registry in a new `src/tui/catalog.rs`. Descriptors are data;
  execution adapters register separately by id so a spec cannot claim to execute
  something no adapter implements.
- Completeness test: build `Cli::command()`, walk every leaf, assert exactly one spec
  per leaf, assert `summary` equals the Clap about string, assert every Clap argument
  has a `FieldSpec` (name, requiredness, value kind) and every `FieldSpec` maps to a
  real argument. Adding a CLI command, renaming a flag, or adding an option then fails
  CI until the TUI is updated. This is what makes "parity" durable instead of a sweep.
- Argument→field mapping is mechanical from the Clap arg definition (value name,
  required, possible values) with hand-written overrides only for picker sources
  (package, snapshot, registry name) and masked/sensitive display.
- Hidden compatibility flags (`setup nu --remove/--use-path/--use-existing`,
  `src/cmd/setup.rs:89`) and aliases (`loader --install-missing`,
  `completions nushell|nu`) get field entries marked hidden: executable via an edited
  preview, not shown as form fields. Aliases are not separate catalog rows.

## Execution architecture

Rejected: spawning the `numan` binary (or a shell) as a subprocess from the worker.
It would reintroduce stdin prompts on a thread with no TTY, lose the thread-scoped
output sink, double-acquire or bypass lock discipline, return exit codes instead of
typed results, and make the editable preview a shell surface. The existing reviewed
operations path (`prepare` → review → `execute` → read-back) is the pattern to extend.

Per family, an adapter in `src/tui/operations.rs` (or a sibling module) that:

1. Takes parsed, typed args (the same `*Args` structs the CLI uses, built via
   `try_parse_from` on the validated argv, as update/remove already do at
   `src/tui/operations.rs:473` and `:481`).
2. Replaces every stdin/TTY prompt with the form + review. Where a handler confirms
   via `confirm_or_bail`, the TUI review is that confirmation; the adapter calls the
   handler's existing `*_reviewed` / `yes`-carrying entry point, never a raw prompt.
   Handlers lacking such a seam get one added in the CLI layer first — the seam is
   shared, so CLI behavior gains a testable path rather than a TUI fork.
3. Runs under `output::with_sink` on the single worker, holds
   `acquire_mutation_lock` exactly where the CLI does, and returns a typed summary
   the App renders. No adapter writes to process stdio.
4. For commands whose output is currently human text (`search`, `info`, `list`,
   registry/snapshot/nupm listings), prefer the existing `execute_to(..., out: &mut
   dyn Write)` seams where they exist (`search`, `list`, `registry`, `snapshot`,
   `nupm` already have them) and capture into the result overlay. Adding such a seam
   is in scope; parsing human output back into state is not.

New worker variant: `Command::ExecuteCli(Box<CliRequest>)` beside the existing
`Sync`/`Prepare`/`Execute`/`Scan` in `src/tui/mod.rs:17`, so busy handling, the
no-cancellation message, and the post-run `Reload` are reused unchanged.

## Phasing

Each phase is independently shippable. A phase ends with its commands' rows flipping
from preview-only to executable; no phase hides rows.

| Phase | Scope | Gate |
|---|---|---|
| 0 — Catalog & Commands tab | `catalog.rs`, completeness test, Commands view, forms, editable preview with re-parse. Nothing new executes. Hand-offs keep working. | Completeness test green; browse/form/edit inertness test (no writes, no network) |
| 1 — Read-only execution | `search`, `info`, `list`, `registry list|packages`, `snapshot list|inspect`, `nupm status|inspect|diff`, `activate --list|--check`, `doctor` (scan/json display), `use list`, `completions --print` (rendered, not installed), `overlay list`, `hook list` | Output captured to overlay; no lock taken; no writes on any of them |
| 2 — Reviewed mutations already modeled | Generalize Install/Activate/Update/Remove to catalog requests; add `deactivate` (single and all), registry `sync` (already native), single-package `update --check` as the Updates view action, `try` (install+activate as two reviews, never one) | Existing operations tests pass against the new request path; stale-review and lock-contention tests extended per command |
| 3 — Prompt-isolated mutations | `hook enable|disable`, `overlay use|hide`, `snapshot delete|rollback`, `gc` (incl. dry-run), `registry add|remove`, `nupm import`, `migrate`, `init [--refresh]`, `use <version>|latest`, all-packages `update` and `activate`, `install --force`, `remove --force` | Each handler has a typed confirm seam; journal-block rule revisited per command (recovery commands must not block themselves) |
| 4 — External-system commands | `setup nu` (install/remove/path/use), `setup loader` (all flags), `completions` install, `doctor` repair mode, `update --self --check` | Effects review lists every target outside `$NUMAN_ROOT` (PATH, `config.nu`, profile, tool bin dir); per-command isolated-root tests |

## Parity exceptions (require a decision before their phase)

These cannot reach literal parity by executing in the worker, and must not silently
revert to hand-off:

1. **`update --self` (actual update).** It replaces the running binary
   (`replace_binary`, `src/cmd/self_update.rs:409`), including a Windows path that
   renames the live executable. Executing it from the TUI worker replaces the binary
   mid-session. Options: (a) `--check` in-TUI, update scheduled as a post-exit step
   the user confirms in the review; (b) in-TUI execution with forced exit-on-success.
   Resolved during implementation: the update executes in the TUI after review and the result advises restarting numan, since the running process keeps the old binary image. This is option (b) without a forced exit; the restart happens on the user's next launch.
2. **`overlay use|hide`.** The Numan-side record executes in-TUI, but the effect —
   `overlay use "<path>"` in the caller's Nushell — cannot be applied to a shell the
   TUI does not own (the CLI has the same limit; it prints the statement,
   `src/cmd/overlay.rs:236`). Parity is defined as: same record mutation executed,
   emitted statement shown in the result overlay for the user to run. This matches CLI
   semantics exactly; it is a shell-session boundary, not a TUI shortfall.
3. **`completions` install.** Writes into shell config locations outside the root.
   Executable in Phase 4, but the effects review must name the exact target file
   (the CLI resolves it in `src/cmd/completions.rs`), and `--print` (Phase 1) covers
   the read-only half.

4. **`doctor` repair prompts and `completions` install output.** Doctor repair runs through the captured `execute_to_with_options` seam, so its report renders in the TUI. Repair sub-flows that demand an interactive prompt still refuse under output capture instead of prompting. `completions` install writes its target file under the reviewed executor; its copy-ready hints go to stderr on the CLI path and are not part of the captured result text.

## Full leaf inventory

Executor key: R = Phase 1 read-only, M2/M3/M4 = mutation phase, X = exception above.
`—` means no positional; flags abbreviated to the non-obvious ones.

| Command | Args / flags | Form fields | Tier | Exec |
|---|---|---|---|---|
| `tui` | — | none (the TUI itself; row opens this section) | ReadOnly | R |
| `search <query> [--all]` | query | Text | ReadOnly | R |
| `info <id>` | id | PackageId | ReadOnly | R |
| `install <pkg> [--force] [-v]` | `owner/name[@ver]` | PackageId+Version, Bool | RootMutation+Network | M2 (force: M3) |
| `update [pkg] [--check] [--self] [-v]` | optional pkg | PackageId?, Bool×3 | RootMutation+Network | M2 single, M3 all, X self |
| `remove <pkg> [--yes] [--force]` | pkg | PackageId, Bool×2 | Destructive | M2 (force: M3) |
| `gc [--dry-run]` | — | Bool | Destructive | M3; dry-run moves to R only after the purity audit in Risks |
| `activate [pkgs…] [--list] [--check] [-v]` | 0..n pkgs | PackageId multi | RootMutation | M2 single, M3 all; list/check R |
| `deactivate [pkgs…] [-v]` | 0..n pkgs | PackageId multi | RootMutation | M2 |
| `hook enable\|disable [pkgs…]`, `hook list` | 0..n pkgs | PackageId multi | RootMutation | M3; list R |
| `list` | — | none | ReadOnly | R |
| `init [--refresh]` | — | Bool | RootMutation | M3 |
| `registry list\|sync` | — | none | ReadOnly / Network | R / M2 (sync already native) |
| `registry add <name> <url> --key <k>` | 3 args | RegistryName, Url, Key | RootMutation+Network | M3 |
| `registry remove <name>` | name | RegistryName | Destructive | M3 |
| `registry packages` | — | none | ReadOnly | R |
| `snapshot list\|inspect <id>` | id | SnapshotId | ReadOnly | R |
| `snapshot delete\|rollback <id> [--yes]` | id | SnapshotId, Bool | Destructive | M3 |
| `nupm status [--nupm-home]` | path | Path | ReadOnly | R |
| `nupm inspect [--all\|<path>] [--nupm-home] [--exit-on-ineligible]` | path | Path, Bool×2 | ReadOnly | R |
| `nupm import [<path>\|--manifest] [--as] [--yes] [--nupm-home]` | path | Path×2, PackageId, Bool | RootMutation | M3 |
| `nupm diff <pkg>` | pkg | PackageId | ReadOnly | R (already hand-off; becomes executable) |
| `migrate [--nupm-home] [--yes] [--dry-run]` | path | Path, Bool×2 | RootMutation | M3 (dry-run is read-only but ships in M3 with the family) |
| `completions <shell> [--print]` | enum | Enum, Bool | External | print R; install M4 |
| `doctor [--scan] [--json] [--nupm-home]` | path | Bool×2, Path | ReadOnly / External-repair | scan R; repair M4 |
| `setup nu [version] [--force] [--skip-path] [--yes] [--minimal]`, `setup nu remove\|path\|use <path>` | version/path | Version, Path, Bool×4 | External+Network | M4 |
| `setup loader [--status] [--detect] [--add] [--remove] [--clean] [--install] [--configure] [--force] [--yes]` | tool | Text(tool), Bool×8 | External+Network | M4 (status is read-only but ships in M4 with the family) |
| `try [pkg] [--no-activate]` | optional pkg | PackageId+Version, Bool | RootMutation+Network | M2 |
| `use <version\|latest\|list>` | version | Version/Enum | RootMutation | M3 (list R) |
| `overlay use <pkg> [--project-root]`, `overlay hide <target>`, `overlay list` | pkg/path | PackageId, Path, Text | RootMutation+ShellEmitting | M3 (list R), X semantics |
| `--root <path>` (global) | path | shown as context, not a field | — | always applied |

## Alternatives considered

- **Hand-maintained list in `ui.rs`, no completeness test.** Rejected: it is exactly
  how `hook`/`overlay`/`migrate` were missed. Parity without a drift alarm decays.
- **Free-text command line executed via shell.** Rejected: shell injection, quoting,
  and the no-shell invariant. The editable preview recovers the flexibility without it.
- **Subprocess execution of the CLI.** Rejected in § "Execution architecture".
- **Big-bang parity in one change.** Rejected: prompt isolation differs per handler;
  phasing keeps every intermediate state honest (rows visible, blocked rows labeled).
- **Keep hand-off as the end state (previous spec).** Superseded by the user's
  decision; hand-off remains only as the fallback rendering for Phase 0 rows and any
  command whose adapter is not yet registered.

## Risks

- `gc --dry-run` is not a pure query today: the 2026-10-18 spec notes it takes the
  mutation lock and can clear lifecycle journals. It stays classified Destructive
  until audited; classifying it read-only to fill Phase 1 would be a false claim.
- Output seams: handlers using process `println!` directly (not `util::output`) will
  corrupt the alternate screen. Phase 1/2 per-family work must grep-audit each handler
  for direct stdio before enabling it — this is a per-command checklist item, not a
  global assumption.
- Review fingerprint scope (`src/tui/operations.rs:185`) hashes a fixed input set.
  Commands touching state outside it (`setup`, `completions`, `use`) need their
  effects inputs added before their review can claim stale-state protection.
- Two sources of truth for "what will this do": CLI prompt text vs TUI effects text.
  Effects strings are per-adapter and must be tested against the handler's actual
  mutations, not copied from help text.
- Feature gate: all of this lives behind `--features tui` (`Cargo.toml:30`,
  `src/lib.rs:15`), so default CLI builds and tests are unaffected. The catalog
  completeness test therefore only runs under `--features tui`; CI must keep that
  configuration mandatory, or the drift alarm has a hole.

## Validation plan (for the implementation phases)

- Unit: completeness test (catalog ↔ Clap tree); tokenizer/re-parse tests including
  injection strings (`;`, `|`, `$(...)`, quotes, newlines, `--root` override attempts);
  field-validation tests per kind; form→argv→`try_parse_from` round-trip per command.
- Focus/overlay: extend `focus_and_overlays_cannot_leak_action_keys`
  (`src/tui/tests.rs:393`) to the form and preview editors — letters typed in a field
  must never trigger `i/a/x/u/r/q`.
- Inertness: extend `browse_is_inert_with_missing_root_signed_cache_and_pending_journal`
  (`src/tui/tests.rs:227`) to cover Commands browsing, form open/edit/abandon, and
  Phase 1 read-only executions (file inventory unchanged).
- Per-adapter: reuse each family's existing `cmd` tests through the reviewed entry
  points (the pattern at `src/tui/tests.rs:280`); add stale-review and lock-contention
  cases per mutating family, as at `src/tui/tests.rs:310`.
- Layout: `TestBackend` at 60/80/120 cols for the Commands list, form, and preview
  (`src/tui/tests.rs:457` pattern), including long command paths and root paths.
- Gates per phase: `cargo test`, `cargo test --features tui`,
  `cargo clippy -- -D warnings`, `cargo clippy --features tui -- -D warnings`,
  `cargo fmt --check`.
- Manual: real-terminal run on Windows for one command per tier per phase, including
  a pending-journal root and a root with spaces/quotes in its path.

## Open questions

None blocking Phase 0–2. One decision is required before Phase 4: the `update --self`
option (a) vs (b) in § "Parity exceptions".

