# Script package activation (issue #14)

## Contract

- A script package declares entry points in its registry activation spec:
  `activation: {kind: "nu-script", entry_points: [{name, path}, ...]}`.
  `name` is the stable public command name; `path` is a package-relative
  `.nu` target. At least one entry point is required.
- Command names use a small grammar (ASCII letter or `_`, then letters,
  digits, `_`, `-`) because they are emitted unquoted into the generated
  autoload file. Duplicate names or duplicate paths within a package are
  rejected, as are collisions with entry points of any other active
  script package (rejection, never shadowing).
- Install persists the declaration in the lockfile (`script_entry_points`);
  install never executes package code. Activation validates every target
  with the same containment/regular-file/`.nu` rules as module entries.
- Activation renders one wrapper per entry point into the Numan-managed
  autoload file (`numan.nu`), alongside module `use` statements:

  ```nu
  # numan:script <owner/name>
  export def --wrapped <name> [...rest: string] {
      ^"<nu>" "<script>" ...$rest
  }
  ```

  The wrapper runs the script as a bare external call under the
  activating Nu executable, so rest arguments, stdout, stderr, and
  exit status forward as for an external program (Nu propagates a
  non-zero external exit itself; capturing the output was rejected
  because Nu aborts the caller on a failed captured external before
  anything can be forwarded). Known limitation, verified against Nu
  0.116.1: pipeline input is not forwarded — the wrapper does not pipe
  `$in` into the child, so a script that reads stdin inherits the
  caller's stdin rather than the pipeline value. It never sources the
  script, so script code runs only when the user invokes the command.
- The managed file goes through the existing candidate-validation,
  journal, drift, and recovery machinery; the lockfile ground truth is a
  per-package `script_activation` record with the same Nu-identity fields
  as `module_activation`.
- Deactivation removes exactly the package's wrappers and clears its
  record. Update/remove follow the existing autoload reconciliation.
- `numan info` displays declared script entry points before activation.
- `numan init --refresh` re-resolves active script targets and regenerates
  the managed file when the Nu executable path changes, so wrappers do not
  keep invoking a stale binary.

## Non-goals

- Arbitrary (non-Nushell) scripts or binaries.
- Auto-executing scripts on install or activate.
- Completion packages (issue #15) — same generated-file discipline, later.
- Rejecting declared names that shadow Nushell built-ins or exports of
  modules imported with `import: all`. Public names are registry-curated,
  and cross-package collision checks cover script packages only; built-in
  shadowing is a package-curation concern rather than an activation check.
