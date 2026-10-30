# Completion package activation (issue #15)

## Contract

- A completion package declares its targets in its registry activation spec:
  `activation: {kind: "nu-completion", targets: [{command, path, kind}, ...]}`.
  `command` is the external command surface the package augments (a single
  command name, e.g. `git`); `path` is a package-relative `.nu` source file
  contributing that command's completions; `kind` is the declared completion
  kind, `"static"` or `"command-backed"`. At least one target is required.
- Command names use the same small grammar as script command names (ASCII
  letter or `_`, then letters, digits, `_`, `-`) because the target name is
  emitted into the generated managed file's ownership comments, and a
  declared target is an ownership claim, not free text. Duplicate commands
  or duplicate paths within a package are rejected, as are Nushell reserved
  keywords.
- `kind` is declared, never inferred. `"command-backed"` means the source
  defines completer functions that Nushell will execute when it requests a
  completion; `"static"` means completions are literal values in the source.
  Activation and install never invoke a completer either way. `numan info`
  displays every declared target and, for `"command-backed"` targets, states
  plainly that the provider can execute code during completion.
- Target ownership is exclusive across active completion packages: if a
  command target is already owned by another active completion package,
  activation of the second package is rejected. Numan never silently
  replaces or shadows another active package's completion.
- Install persists the declaration in the lockfile (`completion_targets`);
  install never executes package code. Activation validates every target
  with the same containment/regular-file/`.nu` rules as script entry points.
- Activation composes each target into the Numan-managed autoload file
  (`numan.nu`), alongside module `use` statements and script wrappers:

  ```nu
  # numan:completion <owner/name> <command>
  use "<absolute path to target file>" *
  ```

  The `use ... *` form matches how Nushell custom-completion files are
  consumed (they export `extern` definitions plus completer functions).
  Rendering is sorted by scoped package ID, then command, so the generated
  file is deterministic.
- The managed file goes through the existing candidate-validation, journal,
  drift, and recovery machinery; the lockfile ground truth is a per-package
  `completion_activation` record with the same Nu-identity fields as
  `script_activation` (Nu executable hash, Nu version, vendor autoload dir,
  managed file path, resolved targets).
- Deactivation removes exactly the package's `use` lines and clears its
  record, leaving other packages' contributions and all user-authored
  configuration untouched. Update/remove follow the existing autoload
  reconciliation used for script packages.
- `numan init --refresh` re-resolves active completion targets and
  regenerates the managed file when the Nu identity changes, matching
  script behaviour.

## Acceptance

- Fixture coverage for static and command-backed targets, multi-command
  packages, cross-package target conflicts, malformed declarations,
  traversal attempts, deactivation, interrupted activation (recovery), and
  updates, mirroring `tests/script_activation_test.rs`.
- Real-Nu acceptance (ignored by default, run explicitly) proves a clean
  interactive session receives a contributed completion after activation
  and no longer receives it after deactivation, without modifying user
  config files.
- Documentation distinguishes Numan's own shell-completion generator
  (`numan completions <shell>`) from installable Nushell completion
  packages, and the README package-support table lists completion packages
  as activatable via `numan activate`.

## Non-goals

- Managing Bash/Fish/Zsh/PowerShell completion packages for arbitrary tools.
- Executing completion providers at install, inspect, lock, update, or
  activation time. Providers run only when Nushell requests a completion.
- Merging arbitrary user-authored completion files, or detecting conflicts
  with completions a user defined outside Numan. Declared targets are
  registry-curated; the ownership check covers Numan-activated completion
  packages.
- Inferring a target's kind by parsing its source. The declaration is the
  contract; a package that mis-declares is a curation failure, surfaced by
  `numan info` showing the declared kind before activation.
