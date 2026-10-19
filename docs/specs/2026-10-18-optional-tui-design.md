# Optional Numan TUI: Package Workbench

Date: 2026-10-18
Status: Design specification; implementation pending

## Decision

Open the Ratatui/Crossterm TUI when a user runs bare `numan` in an interactive terminal. Also support `numan tui` as an explicit entry point. Use the selected Package Workbench concept: a package list beside contextual details, with a command launcher available from any screen. Keep ordinary CLI subcommands and their output contracts unchanged.

The primary outcome is to help users understand what is installed, what is active for their current Nushell, and what an operation will change before running it. The TUI is a presentation layer over Numan operations, not another package manager or source of state.

This document defines the full screen map and implementation boundaries. It does not claim the TUI, typed operation interfaces, or acceptance tests exist yet.

## Scope and packaging

- Make Ratatui and Crossterm optional dependencies behind a Cargo feature named `tui`, excluded from default features. Official end-user release artifacts should enable it so bare `numan` opens the TUI after a normal binary installation. Ordinary builds remain usable without terminal UI dependencies.
- With the feature disabled, bare `numan` prints help with a TUI-unavailable note and exits successfully. Retain a lightweight explicit `tui` command that explains the missing build capability and returns a nonzero status. Neither path initializes the root or terminal, downloads dependencies, or rebuilds the executable.
- Require interactive stdin and stdout for the TUI. Bare `numan` in a redirected or non-interactive session prints ordinary help and exits successfully. Explicit `numan tui` rejects that session with a nonzero status before enabling raw mode; point users to ordinary CLI commands.
- `numan --help` and `numan --version` retain their normal behavior and never enter the TUI. A supplied subcommand always dispatches to that command. No entry path invokes shell profile changes.
- Honor `--root` and existing root resolution. Show the resolved root in context details. Opening the TUI must not create an absent root; offer explicit initialization instead.
- Preserve Rust 1.88 support. Ratatui 0.30.2 declares Rust 1.88.0 and defaults to Crossterm 0.29. Validate the resolved dependency graph and feature-enabled build on the MSRV before pinning the implementation.

At inspection time, the checkout already had uncommitted Ratatui 0.30.2 and Crossterm 0.29.0 dependency additions in `Cargo.toml` and `Cargo.lock`. They were not changed for this specification. Converting them to optional dependencies is future implementation work.

### Launch dispatch

| Invocation | TUI-enabled interactive build | Non-interactive session | Build without TUI support |
| --- | --- | --- | --- |
| `numan` | Open Packages / Installed | Print help; success | Print help and capability note; success |
| `numan --root <path>` | Open TUI for that root | Print help; success | Print help and capability note; success |
| `numan tui` | Open Packages / Installed | Explain interactive-terminal requirement; failure | Explain missing build feature; failure |
| `numan --help` / `numan --version` | Standard help/version | Standard help/version | Standard help/version |
| `numan <existing-command>` | Run existing command | Run existing command | Run existing command |

Make the parsed subcommand optional and resolve this dispatch before the current `main.rs` root-directory creation. Both TUI entry points share one launcher and terminal capability check. When both TUI support and interactivity are absent, the explicit entry reports the missing build feature first. Never infer a TUI launch from an invalid command or argument; preserve normal parse errors.

## Information architecture

Use six destinations. Packages opens by default. Avoid a dashboard of counts or a landing screen that delays package work.

| Destination | List or primary content | Contextual details and actions |
| --- | --- | --- |
| Packages | Installed / Discover / Updates views | Package metadata, versions, origin, compatibility, activation identity; review package actions |
| Nushell | Installed managed versions and current executable | Switch, install, remove, adopt PATH or existing binary; loader tools and configuration |
| Registries | Configured registries and cached package catalogs | URL, key identity, verification evidence, cache age; sync, add, remove |
| Health | Findings grouped by severity | Evidence, repair tier, effect and command; explicit scan or repair |
| Snapshots | Stored snapshots | Inspect manifest and retained payloads; review rollback or deletion |
| Integrations | nupm candidates, completions, initialization | Inspect/import/diff, completion preview/install, initialize/refresh paths |

The header shows destination, resolved Nu version and executable, and whether that identity is known, missing, or stale. Opening a different version's details does not switch the active Nu. A context drawer exposes full executable path/hash, plugin registry, module autoload target, platform, and root.

The footer shows only shortcuts valid for the focused pane. Long-running work has one compact status line with an expandable transcript. It must not replace package state with a transient success toast.

### Packages: the default workbench

At wide sizes, use approximately 60% of the content width for the list and 40% for details. Reserve two header rows and two footer/status rows; allocate the remaining rows to content.

Installed columns: package ID, version, type, and activation state. Keep origin in details unless there is room for an extra column. Discover searches the cached catalog and exposes an explicit option to include incompatible results. Updates performs a requested compatibility check; it never installs on view entry.

Details show the selected version, source/registry, platform and Nu constraints, package origin, and applicable actions. Expand provenance to inspect recorded signing key, index digest, artifact digest, and payload revision. Distinguish recorded verification from a fresh integrity check.

Use separate fields for installation, activation, and compatibility:

- Installation: not installed / installed, including version and provenance.
- Activation: inactive / active for current Nu / recorded for another Nu / unknown / unsupported for this package type.
- Compatibility: compatible by resolver / incompatible with reason / not checked. Compatibility is not proof that the package executes correctly.

Plugin activation uses executable hash, Nu version, and registry path. Module activation uses its own four-part identity, including the vendor autoload directory and managed file path. Do not reuse the current plugin-only display predicate in `cmd/list.rs` for modules.

Default action on an installed inactive package is Review activation. Default action on an available package is Review installation. `try` is a separate action named Install and activate, with both effects stated. After inert installation, show Installed, inactive and offer activation as a separate next step.

### Command launcher

`Ctrl+P` opens a searchable list of typed actions and destinations. It filters known actions, not arbitrary shell input. Results include a destination, package context when required, and availability reason. Selecting a mutating action opens its review screen; it never executes directly.

The launcher complements navigation and exposes infrequent features without adding permanent panels. It can explain the equivalent CLI command. Copying is optional and must not require native clipboard support; the command remains selectable in an ordinary transcript after exit when requested.

### Other destinations

Nushell defaults to inspecting installed versions. Switching previews the existing per-minor activation transition; it does not imply packages were reinstalled. Loader status is distinct from configuring tools. In particular, existing `--detect` configures discovered tools, so label it Detect and configure, not Scan. Adding a custom initialization command, installing tools, clearing caches, and appending a source line to `config.nu` are separate reviewed actions.

Registries distinguish fresh remote data, verified cached data, last-known-good fallback, and failure. A successful command that fell back to cache must not appear as Freshly synced. Adding a registry displays its URL and supplied public-key fingerprint for review. Removing a registry describes configuration/trust effects without suggesting it uninstalls packages.

Health opens with no automatic repair. A requested local scan uses structured findings and disables optional network probes. Remote checks require an explicit action. Preserve finding IDs, severity, repair tier, and evidence. Do not promise individually selectable repairs while the handler only supports a broader repair pass: show the actual scope, or keep the action unavailable until a scoped service exists.

Snapshots are recovery of Numan-managed state, not whole-machine backups. Inspect before rollback, explain missing payloads and Nu identity constraints, and never claim deleted package payloads can always be restored. Deletion can reduce future rollback choices; GC preserves payloads referenced by retained snapshots.

Integrations keeps `NUPM_HOME` read-only. Import copies supported content into Numan ownership; it never runs `build.nu` or implies bidirectional sync. Show unsupported profiles and drift classifications. Completion preview is separate from installation into a shell-specific path. Initialization and path refresh are explicit operations.

## Current command coverage

The equivalent commands below describe existing CLI operations, not a requirement to spawn shell commands. Advanced flags belong in the action form or details, not in the primary navigation.

| Existing command family | TUI location and entry point |
| --- | --- |
| `search`, `search --all`, `info`, `registry packages` | Packages / Discover, compatibility filter, package/version details; Registries / Catalog |
| `list`, `activate --list`, `activate --check` | Packages / Installed; activation details and requested integrity check |
| `install` including pinned versions and reinstall | Package / Review installation; explicit version and reinstall controls |
| `activate`, `deactivate` | Package / Review activation or deactivation; explicit all-target scope when no IDs are supplied |
| `try`, `try --no-activate` | Package / Install and activate or Install only; incompatibility guidance with a separate Nu setup/switch action |
| `update --check`, package or all-package `update` | Packages / Updates; inspect compatible candidates, then review execution scope |
| `update --self --check`, `update --self` | Launcher / Numan version; check first; follow install-source guidance or exit TUI before binary replacement |
| `remove` and its force option | Package / Review removal; explain payload deletion and active-package restrictions |
| `gc --dry-run`, `gc` | Launcher / Storage cleanup; show candidates and retained references before deletion |
| `use list`, `use latest`, `use <version>` | Nushell / Versions; latest means latest installed, not an implicit download |
| `setup nu [version]`, force, minimal, skip-path | Nushell / Install; review version, bundled plugins, replacement and PATH effects |
| `setup nu remove`, `setup nu path`, `setup nu use <path>` | Nushell / Manage installation; expose destructive managed-tree replacement and force gates explicitly |
| `setup loader`, status, detect, add, remove, clean, install/install-missing, force, configure | Nushell / Loader; separate inspection, configuration, download, and cache actions |
| `registry list`, sync, add, remove | Registries / Details and reviewed actions |
| `doctor --scan`, `doctor --json`, repair via `doctor` | Health / Scan and report; reviewed repair; JSON is a report representation, not another destination |
| `snapshot list`, inspect, rollback, delete | Snapshots / List, details, reviewed actions |
| `nupm status`, inspect path/all, import path/manifest, diff | Integrations / nupm; target and `--nupm-home` forms; explicit import IDs and batch scope |
| `init`, `init --refresh` | Integrations / Initialize or refresh resolved Nu paths |
| `completions <shell> --print`, completion install | Integrations / Completions; preview and explicit destination-path review |
| `--root`, help, version, verbose output | Global context, help overlay, About, operation transcript |

Hidden backward-compatibility aliases need no duplicate UI controls. `--yes` is not a persistent preference: a TUI confirmation authorizes exactly one reviewed request. Structured input must preserve supported options without treating a command preview as executable source.

## Keyboard, resize, and visual behavior

| Key | Behavior |
| --- | --- |
| `Ctrl+P` | Open command launcher |
| `Tab` / `Shift+Tab` | Move focus between navigation, list, details, and action controls |
| Arrows; `j` / `k` outside text input | Move selection or scroll the focused pane |
| `/` | Filter the current list; Discover labels registry search distinctly |
| `Enter` | Open details or activate the focused review control; never mutate from a package row |
| `Esc` | Close overlay, cancel an unsubmitted form, or return to list |
| `a`, `d`, `i`, `u`, `x` outside text input | Open context-specific activation, deactivation, installation, update, or removal review |
| `r` | Reload local view data; network sync has a separate named action |
| `?` | Show context-sensitive help |
| `q` | Quit when idle and outside text input; otherwise follow active-operation policy |

At 120 columns and above, show navigation plus list/detail panes. At 80–119 columns, collapse navigation to a destination selector and retain two panes only when both remain readable. Below 80 columns, show a single list or detail screen with Back navigation. Below 60 columns or 16 rows, show a resize message and Quit, preserving any operation status. These thresholds are design defaults to validate with actual terminal rendering.

Use the terminal's default background, cyan focus, readable neutral text, and restrained amber/red for warnings/errors. State labels and a selection marker carry meaning without color. Support monochrome, ordinary Unicode and ASCII border fallback, wrapped diagnostics, and scrolling for long IDs/paths. No emoji or required Nerd Fonts. Mouse support is optional; every operation must work from the keyboard. Keep animations minimal and do not require truecolor.

## Operation boundaries

### Separate presentation, query, and execution

Proposed modules, added incrementally:

- `src/tui/`: terminal guard, application state/update loop, widgets, screens, launcher, review forms, and operation event handling. No authoritative state writes or plugin registration here.
- `src/operations/`: typed query/request/result interfaces shared by the CLI and TUI. Adapt one workflow at a time; do not migrate every handler before delivering a read-only workbench.
- Existing `core`, `install`, `state`, `nu`, and `nupm_compat`: keep resolution, trust, transaction, ownership, journal, and Nu lifecycle authority.
- Existing `cmd`: retain argument parsing and human/JSON rendering. Delegate to shared operations as each workflow is integrated.

Use a state/message/update loop. A single worker executes blocking operations off the rendering/input thread and sends typed progress/results through a bounded channel. Avoid adding an async runtime solely for UI responsiveness. Tag queries with root, Nu identity, and request generation; discard stale responses after context or filter changes.

Use structured read models for package rows/details, Nu context, registry provenance, doctor findings, and snapshot summaries. Never parse human CLI output into application state. Reuse `DoctorReport` and existing nupm report types where suitable; add only missing presentation-neutral queries.

Queries must not initialize a root, sync registries, reconcile journals, switch Nu, or persist projections. Audit helpers before calling them from view loading: a read-shaped method name is insufficient. For example, current `gc --dry-run` acquires the mutation lock and can clear lifecycle journals before returning; it is not a pure query. Extract a candidate inspection path before wiring a cleanup preview. Signature-loading paths must also be checked for trust/cache persistence.

### Review, execute, re-read

1. Build a typed request from the focused item and form, including exact targets, version policy, root, and Nu context.
2. Query the applicable effects and blockers without mutation. This is advisory inspection, not a new universal CLI dry-run contract. Show unknown effects as unresolved and disable execution when required scope cannot be established.
3. Review the scope, filesystem/Nu effects, trust evidence, and applicable recovery limits. Default destructive confirmation to Cancel. Never preselect force, active-plugin mutation opt-in, or custom-command execution.
4. Submit one request. The operation boundary owns the mutation lock and existing snapshots, journals, atomic writes, integrity checks, and lifecycle calls. Do not acquire an outer UI lock around an already-locking handler.
5. Revalidate authoritative state and context under the appropriate lock before mutation. If targets or effects changed materially, return to review rather than executing stale consent. No lock is held while a person reads a dialog.
6. Render actual operation phases and results. Show percentages only when supported by measured totals. A spinner must not imply continued progress without events.
7. Reload authoritative state after success or failure. Preserve errors, partial outcomes, retained journals, and recovery guidance in the transcript. A successful install does not establish active state; a zero exit code does not establish a fresh registry fetch.

The UI must not duplicate snapshot creation or promise that every operation is rollbackable. Existing handlers differ in snapshot and lock coverage. Audit each integrated mutation and fix a missing required boundary in that workflow before exposing it. Registry/trust configuration, downloads of tools, PATH edits, completion installation, and self-update need their own effect descriptions rather than blanket package-rollback guarantees.

### Prompts and terminal ownership

Several handlers currently print to process stdout/stderr or read stdin, including snapshot actions, removal, and Nu pin offers. Passing them a worker thread does not make them TUI-safe. Extract typed interaction/results before enabling those actions in the full-screen UI; do not redirect process-wide stdout or automatically answer every prompt with yes.

Where extraction is deferred, show an exact CLI handoff and mark the action Run outside TUI. If a later implementation offers terminal handoff, it must restore raw mode/alternate screen first, run the existing executable with argument arrays and inherited streams, then reinitialize and reload. Never concatenate user input into a shell command. Self-update exits the TUI before replacement; do not keep an executing binary-replacement task inside a UI worker.

Only one mutation may be active in a session. External processes remain governed by existing filesystem locks. A busy lock produces a visible message, never lock-file deletion. Browse the last loaded data while busy with an explicit stale marker; disable additional mutation actions. Do not silently queue changes.

### Cancellation and recovery

Before submission, Esc cancels freely. During execution, cancellation is available only at operation-defined safe boundaries. Download interruption or cancellation of external build processes must be implemented and tested before exposing Cancel. Until then, Esc only closes the transcript; Quit and Ctrl+C explain that the current operation must finish. Do not exit while an in-process worker can still mutate state.

Restore terminal state on normal exit, returned errors, and panics. Abrupt termination remains possible and relies on existing journals; never describe terminal cleanup as transaction rollback. Show pending recovery on next load, preserve journal evidence, and offer the appropriate explicit recovery workflow. Browsing must not clear a journal just to make a warning disappear.

## Delivery sequence and acceptance

1. **Read-only workbench:** optional feature, bare-command and explicit launch dispatch, terminal guard, Installed/Discover/details, launcher, local context, structured reads, empty/error states, responsive layouts. No live mutations are exposed. Navigation offers an honest CLI handoff for unintegrated features.
2. **Package lifecycle:** typed install/activate/deactivate/try/update/remove requests, reviewed scope, worker events, authoritative reload, prompt isolation, active-plugin gates and recovery tests.
3. **Administration:** integrate Nu/loader, registries, health repair, snapshots, GC, nupm and completions incrementally. Add self-update handoff. Full feature coverage is achieved only when every coverage-table row has a tested native action or an explicitly labeled CLI handoff; report native coverage separately.

Required implementation checks:

- Default-feature and `tui`-feature builds/tests/clippy, formatting, packaging, and Rust 1.88 checks with the resolved lockfile. Verify that default builds do not link terminal UI dependencies.
- Launch-dispatch tests cover every invocation in the table with both feature configurations and TTY states, global root selection, invalid arguments, exit statuses, and absence of root/config writes during help or rejected launches. Verify official release builds enable the TUI feature.
- Ratatui `TestBackend` layout checks at 60/80/120 columns, short heights, monochrome, empty state, long IDs/paths, errors and overlays; keyboard tests verify focus and that row selection never mutates.
- Temp-root query tests prove no files are created or changed on browse, including missing roots, existing journals, cached trust state, and GC preview. Isolate HOME/config and block test network access.
- Operation tests verify exact confirmation scope, stale-review rejection, lock contention, snapshots/journals, inert installation, separate activation, plugin/module identity, mutation-gate refusal, partial failure and reload. Use existing fake Nu/test seams.
- Real terminal acceptance on Windows, Linux and macOS for resize, redirected I/O rejection, panic/error restoration, key handling, long-running work and Ctrl+C behavior. `TestBackend` is not proof of terminal restoration.
- Real-Nu lifecycle acceptance against isolated roots and configuration before claiming end-to-end install/activate/update/recovery readiness. Ordinary CLI output and existing tests remain compatible.

## Evidence and references

Local contracts inspected: `src/cli.rs`, `src/main.rs`, `src/cmd/{list,search,install,update,remove,gc,registry,snapshot,nupm,setup,use_cmd,try_cmd,init,completions,doctor,nu_pin_offer}.rs`, `src/state/lockfile.rs`, `src/core/{registry,resolve}.rs`, `src/nu/version_manager.rs`, `REVIEW.md`, and development/architecture documentation.

The live lockfile path is resolved by `Lockfile::load`/`save` as `<root>/lockfile`; do not hardcode the `lockfile.json` spelling found in higher-level documentation. Use existing path APIs and reconcile documentation separately.

- [Ratatui backend concepts and Crossterm version compatibility](https://ratatui.rs/concepts/backends/): backend responsibilities, event handling and consistent Crossterm versions.
- [Ratatui 0.30.2 workspace manifest](https://github.com/ratatui/ratatui/blob/ratatui-v0.30.2/Cargo.toml): declared Rust 1.88.0 minimum.
- [Ratatui testing](https://ratatui.rs/recipes/testing/): terminal-buffer tests complement interactive acceptance.
