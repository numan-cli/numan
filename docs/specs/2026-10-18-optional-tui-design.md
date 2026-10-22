# Optional Numan TUI: Package Workbench

Date: 2026-10-18
Status: Design specification; implementation pending

## Decision

Open the Ratatui/Crossterm TUI when a user runs bare `numan` in an interactive terminal. Also support `numan tui` as an explicit entry point. Use the selected Package Workbench concept: a package list beside contextual details, with a command launcher available from any screen. Keep ordinary CLI subcommands and their output contracts unchanged.

The primary outcome is to help users understand what is installed, what is active for their current Nushell, and what an operation will change before running it. The TUI is a presentation layer over Numan operations, not another package manager or source of state.

This document defines a deliberately limited native surface and its implementation boundaries. The command inventory records explicit handoffs, not promised future native coverage. It does not claim the TUI, typed operation interfaces, or acceptance tests exist yet.

## Scope and packaging

- Make Ratatui and Crossterm optional dependencies behind a Cargo feature named `tui`, excluded from default features. Once the native slice passes acceptance, official end-user release artifacts should enable it so bare `numan` opens the TUI after a normal binary installation. Do not enable unfinished TUI release builds during the dependency-only step. Ordinary builds remain usable without terminal UI dependencies.
- With the feature disabled, bare `numan` prints help with a TUI-unavailable note and exits successfully. Retain a lightweight explicit `tui` command that explains the missing build capability and returns a nonzero status. Neither path initializes the root or terminal, downloads dependencies, or rebuilds the executable.
- Require interactive stdin and stdout for the TUI. Bare `numan` in a redirected or non-interactive session prints ordinary help and exits successfully. Explicit `numan tui` rejects that session with a nonzero status before enabling raw mode; point users to ordinary CLI commands.
- `numan --help` and `numan --version` retain their normal behavior and never enter the TUI. A supplied subcommand always dispatches to that command. No entry path invokes shell profile changes.
- Honor `--root` and existing root resolution. Show the resolved root in context details. Opening the TUI must not create an absent root; offer explicit initialization instead.
- Preserve Rust 1.88 support. Ratatui 0.30.2 declares Rust 1.88.0 and defaults to Crossterm 0.29. Validate the resolved dependency graph and feature-enabled build on the MSRV before pinning the implementation.

The packaging prerequisite makes Ratatui and Crossterm optional under `tui`, with an empty default feature set. CI checks the active default normal/build dependency tree for Ratatui and Crossterm and checks the enabled feature with Clippy and Rust 1.88. The lockfile can retain optional packages without compiling them into default builds. This prerequisite does not implement the TUI or launch dispatch.

`NUMAN_NO_TUI=1` disables both implicit and explicit TUI entry before terminal initialization. Bare `numan` then prints help and succeeds; explicit `numan tui` explains the environment override and fails. Other values do not disable the TUI. Existing subcommands are unaffected. Check this override before feature/TTY checks so it remains a dependable escape hatch.

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

The initial navigation has only Packages and Updates. `Ctrl+P` opens the launcher. There are no Nushell, Registries, Snapshots, Health, or Integrations destinations in this delivery. A requested doctor scan opens a report overlay, not a separate Health section. Dedicated administration screens require a later product decision.

| Surface | Purpose | Initial native scope |
| --- | --- | --- |
| Packages / Installed | Understand installed versions and activation | Read details; review single-package activation or removal |
| Packages / Discover | Find packages from cached registry data | Search/filter, inspect versions, review single-package installation |
| Updates | Inspect compatible candidates for installed packages | Requested check, then review a single-package update |
| Launcher | Find supported native actions or obtain CLI commands | Native actions are labeled Native; all other entries are labeled CLI only |
| Doctor report overlay | Understand local health findings | Explicit scan-only report; no repair controls |

The header shows resolved Nu version and executable, and whether that identity is known, missing, or stale. A context drawer exposes full executable path/hash, plugin registry, module autoload target, platform, and root. Inspecting context never changes it.

The footer always shows a textual focus indicator such as `Focus: Package list`, as well as shortcuts valid for that focus. Long-running work has one compact status line with an expandable transcript. It must not replace authoritative state with a transient success toast.

### No network on view entry

This rule covers every surface, overlay, launcher, refresh, resize, and selection change. Entry reads local data only. No registry sync, remote release lookup, update download, trust refresh, or network doctor probe runs implicitly. Search uses cached catalogs; update resolution uses cached metadata. Missing/stale data is visible, never repaired by an automatic fetch. Future network actions must be explicitly named and invoked; CLI-only registry sync remains a handoff in the initial delivery. Lower-level helpers must be audited for indirect network requests before use in any query.

### Packages: the default workbench

At wide sizes, use approximately 60% of the content width for the list and 40% for details. Reserve two header rows and two footer/status rows; allocate the remaining rows to content.

Installed columns: package ID, version, type, and activation state. Keep origin in details unless there is room for an extra column. Discover searches the cached catalog and exposes an explicit option to include incompatible results. Updates performs an explicitly requested local compatibility check; entry never checks or installs anything.

Details show the selected version, source/registry, platform and Nu constraints, package origin, and applicable actions. Expand provenance to inspect recorded signing key, index digest, artifact digest, and payload revision. Distinguish recorded verification from a fresh integrity check.

Use separate fields for installation, activation, and compatibility:

- Installation: not installed / installed, including version and provenance.
- Activation: inactive / active for current Nu / recorded for another Nu / unknown / unsupported for this package type.
- Compatibility: compatible by resolver / incompatible with reason / not checked. Compatibility is not proof that the package executes correctly.

Plugin activation uses executable hash, Nu version, and registry path. Module activation uses its own four-part identity, including the vendor autoload directory and managed file path. Do not reuse the current plugin-only display predicate in `cmd/list.rs` for modules.

Default action on an installed inactive package is Review activation. Default action on an available package is Review installation. `try` remains CLI only. Do not expose a combined native Install and activate action in this delivery. After inert installation, show Installed, inactive and offer activation as a separate next step.

### Command launcher

`Ctrl+P` opens a searchable list of typed actions and destinations. It filters known actions, not arbitrary shell input. Results include a destination, package context when required, and availability reason. Selecting a mutating action opens its review screen; it never executes directly.

The launcher contains two visibly separate groups: Native actions and CLI only. CLI-only results open a command explanation with the exact target/root and effects, plus `Print command and exit` and Back. They never look like in-app Execute buttons, spawn an operation, or promise a future native implementation.

`Print command and exit` stores a shell-appropriate escaped command as a pending exit result. After terminal restoration, print that command followed by one newline to stdout; diagnostics go to stderr. Nothing is executed. Respect the selected Nushell/PowerShell/POSIX quoting policy and test spaces and quotes in paths. Printing requires no clipboard API. Ordinary quit prints no command; cancelling the handoff clears the pending result.

The requested doctor scan uses the existing structured report with `scan: true` and `skip_network: true`. Show finding IDs, severity, repair tiers, evidence and command guidance. Repairs remain CLI only. Audit this scan for persistent writes and expose failures rather than quietly repairing them.

## Native commitment and CLI-only inventory

The target native action set is **single-package install, activate, update, remove, and local doctor scan**, plus the reads that support them. The install/activate spike below gates the broader commitment: no native action ships until its request, prompts, effects and failure paths pass acceptance. Scope reduction after the spike must be explicit. There is no all-packages execution or arbitrary multi-action queue in this slice.

| Command family | Delivery contract |
| --- | --- |
| `search`, `search --all`, `info`, `list` | Native cached queries and package details |
| Single-package `install`, pinned version | Native review/execution after spike; forced reinstall remains CLI only |
| Single-package `activate` | Native review/execution after spike; all-target activation remains CLI only |
| Single-package `update`, `update --check` | Native local check/review/execution after calibration; all-target update remains CLI only |
| Single-package `remove` | Native review/execution after calibration; force remains CLI only |
| `doctor --scan` report | Native explicit local scan, with network disabled; repair and remote probes remain CLI only |
| `deactivate`, `activate --list`, `activate --check`, `try` | CLI only; native details still display activation identity |
| `registry list/sync/add/remove/packages` | CLI only; Discover may inspect existing cached catalogs |
| `use list/latest/<version>`, all `setup nu` actions | CLI only; no native Nu installation or switch controls |
| All `setup loader` actions/options | CLI only, including detect/configure, download/install and cache cleanup |
| `snapshot list/inspect/rollback/delete`, `gc` including dry-run | CLI only; no cleanup or snapshot preview promises |
| All `nupm` actions and completion preview/install | CLI only; no nupm import or completion writes inside the TUI |
| `init`, `init --refresh` | CLI only; missing initialization links to command explanation |
| `update --self` and its check mode | CLI only; user exits TUI before binary replacement |
| `--root`, help, version, verbose output | Root context, help overlay, About and operation transcript |

These handoffs are an intentional product boundary from day one, not placeholders for eventual full-CLI coverage. Advanced flags outside the native set remain command guidance. Hidden aliases do not need duplicate entries. `--yes` is never a persistent preference: a confirmation authorizes one exact native request.

## Keyboard, resize, and visual behavior

Use explicit focus states: Navigation, PackageList, PackageDetails, UpdatesList, FilterInput, LauncherInput, ReviewForm, DoctorReport, Transcript, Help and ResizeNotice. Exactly one owns input. An open overlay captures all input; global letter handlers never run underneath it. Show focus in the footer and with a visible pane marker, including monochrome mode.

| Focus | Enabled input |
| --- | --- |
| Navigation | Up/Down select a destination; Left/Right or Tab moves pane focus; Enter focuses the package list; `q` requests quit |
| PackageList | Arrows or `j`/`k` select; `/` enters filter; Enter opens the selected package action menu with available and blocked actions; `i`/`a`/`x` open applicable install/activate/remove review; `r` reloads local data; `q` requests quit |
| PackageDetails | Arrows or `j`/`k` scroll; `i`/`a`/`x` open applicable reviews; Esc returns to list; `r` reloads local data; `q` requests quit |
| UpdatesList | Arrows or `j`/`k` select; `/` enters filter; Enter opens the selected package action menu; `u` opens selected-package review; `r` reloads local data; `q` requests quit |
| FilterInput / LauncherInput | Printable characters, including `a/d/i/u/x/r/q/?/`, are text; Enter applies/opens the selected result; Esc cancels; no letter shortcuts |
| ReviewForm | Tab/Shift+Tab select explicit controls; text fields consume printable input; Enter activates only the focused control; Esc cancels before submission; initial focus is Cancel |
| DoctorReport / Transcript / Help | Arrows/PageUp/PageDown scroll; Esc closes; no package action or quit letter shortcuts |
| ResizeNotice | Only `q`, Ctrl+C quit policy, and resize handling; no package actions |

`Ctrl+P` opens the launcher from idle Navigation/PackageList/PackageDetails/UpdatesList only. It is disabled in input fields and overlays. `?` opens help from those same idle focuses; Tab/Shift+Tab cycles visible panes there. `d` has no shortcut because native deactivation is out of scope. `u` never updates from Packages, and `r` never fetches network data. Missing/inapplicable actions are omitted from hints and do nothing. Ctrl+C follows operation-safe quit behavior globally. Keys cannot trigger a second action through event propagation.

At 120 columns and above, show the two-destination navigation plus list/detail panes. At 80–119 columns, collapse navigation to a destination selector and retain two panes only when both remain readable. Below 80 columns, show a single list or detail screen with Back navigation. Below 60 columns or 16 rows, show a resize message and Quit, preserving any operation status. These thresholds are design defaults to validate with actual terminal rendering.

Use the terminal's default background, cyan focus, readable neutral text, and restrained amber/red for warnings/errors. State labels and a selection marker carry meaning without color. Support monochrome, ordinary Unicode and ASCII border fallback, wrapped diagnostics, and scrolling for long IDs/paths. No emoji or required Nerd Fonts. Mouse support is optional; every operation must work from the keyboard. Keep animations minimal and do not require truecolor.

## Operation boundaries

### Separate presentation, query, and execution

Proposed boundaries; do not create a general operation framework before the spike:

- `src/tui/`: terminal guard, application state/update loop, widgets, screens, launcher, review forms, and operation event handling. No authoritative state writes or plugin registration here.
- A workflow-specific adapter, initially for install and activate only, provides typed requests/results and isolated interaction. Whether it belongs in `src/operations/` is decided after the spike. Prompt isolation, progress and safe cancellation are separate work per workflow, potentially multiple PRs; they are not implied by wrapping an existing handler.
- Existing `core`, `install`, `state`, `nu`, and `nupm_compat`: keep resolution, trust, transaction, ownership, journal, and Nu lifecycle authority.
- Existing `cmd`: retain argument parsing and human/JSON rendering. Delegate to shared operations as each workflow is integrated.

Use a state/message/update loop. A single worker executes blocking operations off the rendering/input thread and sends typed progress/results through a bounded channel. Avoid adding an async runtime solely for UI responsiveness. Tag queries with root, Nu identity, and request generation; discard stale responses after context or filter changes.

Use structured read models for package rows/details, Nu context, cached registry provenance, and doctor findings. Never parse human CLI output into application state. Reuse `DoctorReport` where suitable; add only missing presentation-neutral queries.

Queries must not initialize a root, sync registries, reconcile journals, switch Nu, or persist projections. Audit helpers before calling them from view loading: a read-shaped method name is insufficient. For example, current `gc --dry-run` acquires the mutation lock and can clear lifecycle journals before returning; it is not a pure query. Keep cleanup CLI only; any future native preview requires a separate pure inspection path. Signature-loading paths must also be checked for trust/cache persistence.

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

Features outside the native commitment use the launcher CLI-only explanation and Print command and exit mechanism. There is no suspend/spawn/resume bridge in this delivery. Never concatenate user input into an executed shell command. No native self-update task runs in a UI worker.

Only one mutation may be active in a session. External processes remain governed by existing filesystem locks. A busy lock produces a visible message, never lock-file deletion. Browse the last loaded data while busy with an explicit stale marker; disable additional mutation actions. Do not silently queue changes.

### Cancellation and recovery

Before submission, Esc cancels freely. During execution, cancellation is available only at operation-defined safe boundaries. Download interruption or cancellation of external build processes must be implemented and tested before exposing Cancel. Until then, Esc only closes the transcript; Quit and Ctrl+C explain that the current operation must finish. Do not exit while an in-process worker can still mutate state.

Restore terminal state on normal exit, returned errors, and panics. Abrupt termination remains possible and relies on existing journals; never describe terminal cleanup as transaction rollback. Show pending recovery on next load, preserve journal evidence, and offer the appropriate explicit recovery workflow. Browsing must not clear a journal just to make a warning disappear.

## Delivery sequence and acceptance

1. **Packaging prerequisite:** convert dependencies to optional `tui` feature; add the CI default dependency-tree gate and enabled-feature/MSRV checks. Keep this separately reviewable. No terminal behavior changes or release-feature enablement yet.
2. **Install/activate spike:** implement one vertical journey through the selected workbench using isolated roots: inspect package, review/install, reload as Installed/inactive, then independently review/activate and reload its Nu-specific activation state. This is a calibration spike, not a production coverage promise. Isolate prompts, emit typed results and real progress events, and define unsupported cancellation honestly. Exercise a module and plugin where lifecycle paths differ. Compare the refactor/adapter cost, regression surface, and terminal acceptance before scheduling more native actions. Record findings and revise estimates/architecture rather than treating the spike as automatic approval of `src/operations/`.
3. **First native release:** after the spike, deliver Packages + Updates + launcher with the five named native operations. Each workflow needs its own reviewed request/effects contract and failure tests; update and removal may span multiple PRs. Keep every other command CLI only with explicit handoff labels. Add bare launch, escape hatch, terminal guard, responsive focus/navigation and command-on-exit behavior. Enable the release feature only after acceptance passes.
4. **Later decisions:** dedicated Nushell, Registries, Snapshots, Health and Integrations screens, native deactivation/try, batch actions, richer cancellation and clipboard integration are deferred. They are not required for this release or implied by the inventory.

Required implementation checks:

- Default-feature and `tui`-feature builds/tests/clippy, formatting, packaging, and Rust 1.88 checks with the resolved lockfile. CI must reject Ratatui (including split `ratatui-*` crates) and Crossterm in the default normal/build graph; retaining them in `Cargo.lock` is expected. Feature-enabled checks must compile them.
- Launch-dispatch tests cover every invocation in the table with both features and TTY states, `NUMAN_NO_TUI=1` precedence, other environment values, root selection, invalid arguments, statuses, and no root/config writes on help or rejected launch. Future release builds enable the feature only when ready.
- Ratatui `TestBackend` layout checks at 60/80/120 columns, short heights, monochrome, empty/error states and long IDs/paths. Focus tests send every action letter in every focus, proving text entry/overlays cannot execute actions or quit through event propagation.
- Temp-root query tests prove no persistent writes or network requests on entry, filter, selection, refresh and overlay open. Include absent roots, retained journals and cached trust state; isolate HOME/config and deny network. Doctor scan is explicit and local.
- Spike acceptance proves inert install, separate activation consent, plugin/module identity, prompt isolation, real result/progress events, lock contention, snapshot/journal behavior, stale-review rejection, partial failure and authoritative reload. Add equivalent contracts before enabling update/remove; never claim unsafe cancellation works.
- Print-command exit tests restore the terminal before stdout output, print exactly one requested command/newline with shell-correct quoting, preserve root/target, write diagnostics to stderr, clear cancelled requests and execute nothing.
- Real terminal acceptance on Windows, Linux and macOS covers resize, TTY rejection, restoration after error/panic, focus indicators and active-operation Ctrl+C. Real-Nu lifecycle acceptance uses isolated roots/configuration. Buffer tests and a successful build do not establish runtime readiness.

## Evidence and references

Local contracts inspected: `src/cli.rs`, `src/main.rs`, `src/cmd/{list,search,install,update,remove,gc,registry,snapshot,nupm,setup,use_cmd,try_cmd,init,completions,doctor,nu_pin_offer}.rs`, `src/state/lockfile.rs`, `src/core/{registry,resolve}.rs`, `src/nu/version_manager.rs`, `REVIEW.md`, and development/architecture documentation.

The live lockfile path is resolved by `Lockfile::load`/`save` as `<root>/lockfile`; do not hardcode the `lockfile.json` spelling found in higher-level documentation. Use existing path APIs and reconcile documentation separately.

- [Ratatui backend concepts and Crossterm version compatibility](https://ratatui.rs/concepts/backends/): backend responsibilities, event handling and consistent Crossterm versions.
- [Ratatui 0.30.2 workspace manifest](https://github.com/ratatui/ratatui/blob/ratatui-v0.30.2/Cargo.toml): declared Rust 1.88.0 minimum.
- [Ratatui testing](https://ratatui.rs/recipes/testing/): terminal-buffer tests complement interactive acceptance.
