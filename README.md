# Numan

[![CI](https://github.com/tonythethompson/numan/actions/workflows/ci.yml/badge.svg)](https://github.com/tonythethompson/numan/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
<img alt="Crates.io Version" src="https://img.shields.io/crates/v/numan-cli"> 
![WinGet Package Version](https://img.shields.io/winget/v/tonythethompson.numan)
<img alt="Crates.io Version" src="https://img.shields.io/crates/d/numan-cli"> 



**Numan** is a cross-platform package manager for [Nushell](https://www.nushell.sh/). It installs registry plugins, modules, scripts, and completion payloads from signed registries, pins immutable artifacts in a lockfile, and activates plugins and modules with Nu only when you ask — keeping installs inert until you run `numan activate`.

Built in Rust for Linux, macOS, and Windows.

---

## About

Nushell’s ecosystem has grown around community packages, but managing them across machines, Nu versions, and platforms is still painful. **nupm** covers local installs well; registry-based workflows need verified artifacts, reproducible lockfiles, and safe activation that survives Nu upgrades.

Numan fills that gap:

| Concern | How Numan handles it |
|--------|----------------------|
| **Trust** | Ed25519 signatures over registry indexes; built-in production trust root for `official`; SHA256 verification of plugin binaries |
| **Reproducibility** | Lockfile v2 pins version, payload hash, and install origin |
| **Platform safety** | Artifacts resolved for the compile-time OS/arch/libc triple |
| **Nu version matching** | Resolver respects per-package Nu constraints |
| **Activation isolation** | `install` never touches Nu; only `activate` registers plugins or writes autoloads |
| **Crash recovery** | Journals for activation, autoload, lifecycle, and nupm import operations |
| **nupm coexistence** | Read-only discovery, one-way import, and drift detection for existing nupm installs |

Numan is **early-stage**. Core install, activate, update, remove, gc, registry, doctor, snapshots, nupm interoperability, and shell completions are implemented and covered by unit and hermetic integration tests, plus real-Nu acceptance on Linux, macOS, and Windows in CI. Pre-built release binaries are published via GitHub Releases.

---

## Features

- **Registry-backed installs** — search, inspect versions, and install `owner/name` or `owner/name@version`
- **Official registry** — production trust root built in; `numan init` configures `official` automatically; `numan registry sync` verifies signed indexes
- **Package types** — plugins and modules support activation; scripts and completion packages are install-only while their activation contracts are deferred
- **Verified artifacts** — mandatory SHA256 for plugin binaries; signed registry indexes
- **Scoped activation** — plugins active only when Nu executable hash, Nu version, and plugin registry path match
- **Module autoloads** — managed vendor autoload files with ownership markers and candidate validation
- **Lifecycle management** — `update`, `remove`, and `gc` with pending-lifecycle journal recovery
- **nupm interoperability** — `numan nupm status|inspect|import|diff` for migration from [nupm](https://github.com/nushell/nupm)
- **Health checks** — `numan doctor [--fix]` diagnoses root state and applies safe repairs
- **Shell completions** — bash, fish, zsh, PowerShell, and Nushell via `numan completions`

---

## Registry package support

| Registry package type | Install, verify, and lock | `numan activate` | Support tier |
|-----------------------|----------------------------|------------------|--------------|
| Plugin | Yes | Yes, through Nu's plugin registry | Supported |
| Module | Yes | Yes, through a Numan-managed vendor autoload file | Supported |
| Script | Yes | No | Install-only; activation is deferred |
| Completion package | Yes | No | Install-only; activation is deferred |

Install-only packages remain inert: Numan downloads, verifies, locks, lists,
removes, and garbage-collects their payloads, but does not execute them or
modify Nu configuration for them. This is separate from Numan's own shell
completion generator: `numan completions <shell>` is supported for bash, fish,
zsh, PowerShell, and Nushell (`nu`).

---

## Installation

### From source

Requires [Rust](https://rustup.rs/) **1.88+** (stable recommended) and a Nushell binary on `PATH` for activation commands.

```bash
git clone https://github.com/tonythethompson/numan.git
cd numan
cargo install --path .
```

The binary is named `numan`.

### Pre-built releases

Download the latest archive for your platform from [GitHub Releases](https://github.com/tonythethompson/numan/releases). Each release ships:

| Platform | Archive | Binary |
|----------|---------|--------|
| Linux (x86_64) | `numan-<version>-x86_64-unknown-linux-gnu.tar.gz` | `numan` |
| Windows (x86_64) | `numan-<version>-x86_64-pc-windows-msvc.zip` | `numan.exe` |
| macOS (Apple Silicon) | `numan-<version>-aarch64-apple-darwin.tar.gz` | `numan` |
| macOS (Intel) | `numan-<version>-x86_64-apple-darwin.tar.gz` | `numan` |

**Linux / macOS**

```bash
tar -xzf numan-<version>-<target>.tar.gz
install -m 755 numan-<version>-<target>/numan ~/.local/bin/numan
```

**Windows (PowerShell)**

```powershell
Expand-Archive numan-<version>-x86_64-pc-windows-msvc.zip -DestinationPath .
# Add the extracted folder to your PATH, or copy numan.exe into a directory already on PATH
```

Verify downloads with the `SHA256SUMS` file attached to each release.

### From git (latest `master`)

```bash
cargo install --git https://github.com/tonythethompson/numan
```

Tracks the default branch. For a reproducible install, choose a published tag from [GitHub Releases](https://github.com/tonythethompson/numan/releases) and pass `--tag vX.Y.Z`.

### winget (Windows)

```powershell
winget install tonythethompson.numan
```

See [packaging/winget/README.md](packaging/winget/README.md) and [docs/PACKAGING.md](docs/PACKAGING.md).

### crates.io

```bash
cargo install numan-cli
```

Requires [Rust](https://rustup.rs/) (stable). The installed binary is named `numan`.

**Requirements:** a [Nushell](https://www.nushell.sh/) binary on `PATH` for `numan init`, `numan activate`, and related commands.

### Shell completions

`numan completions <shell>` prints the script on stdout and a copy-ready install command on stderr.

```bash
# Bash
numan completions bash > ~/.local/share/bash-completion/completions/numan

# Zsh
numan completions zsh > ~/.zfunc/_numan

# Fish
numan completions fish > ~/.config/fish/completions/numan.fish

# PowerShell (append to $PROFILE; do not use Out-File — that overwrites the profile)
numan completions powershell | Add-Content -Encoding utf8 $PROFILE

# Nushell (vendor autoload; `nu` is accepted as an alias for `nushell`)
mkdir ($nu.data-dir | path join vendor/autoload)
numan completions nushell | save -f ($nu.data-dir | path join vendor/autoload/numan-completions.nu)
```

PowerShell completions are safe to place after other statements in `$PROFILE`. Prefer writing to a dedicated file and dot-sourcing if you want easier updates:

```powershell
New-Item -ItemType Directory -Force -Path "$HOME\.numan" | Out-Null
numan completions powershell | Out-File -Encoding utf8 "$HOME\.numan\completions.ps1"
Add-Content -Path $PROFILE -Value '. $HOME\.numan\completions.ps1'
```


---

## Quick start

Copy-paste path from install through first activation:

```bash
# Install (pick one)
cargo install numan-cli
# or: download a release archive from GitHub Releases and add numan to PATH

numan init
numan registry sync
numan try                 # install + activate a starter that fits your Nu (e.g., skim for Nu 0.114)
numan doctor
```

Or pick a package yourself (`numan search` hides incompatible hits by default; use `--all` to see them):

```bash
numan search nutest
numan info vyadh/nutest
numan install vyadh/nutest
numan activate vyadh/nutest --yes
```

Install is **inert** — nothing is registered with Nu until you run `numan activate` (or `numan try`, which activates after install). If a package needs a different Nu minor, Numan explains the mismatch and can offer `numan setup nu --version <x.y.z>` (activations are per-Nu; re-activate after switching). When no compatible starter exists, `numan try` suggests installing a matching managed Nu version or searching for another package with `numan search`.

After Nu upgrades, refresh cached paths and activation identity:

```bash
numan init --refresh
```

Optional: install shell completions (`numan completions bash`, etc.) — see [Installation](#installation).

### Step-by-step

#### 1. Initialize

Probe your local Nu installation and create Numan state under the default root (or `--root`):

```bash
numan init
```

`numan init` configures the official registry automatically and prints a numbered checklist when setup is incomplete.

#### 2. Sync the registry

```bash
numan registry sync
```

#### 3. Prove it works, or search and install

```bash
numan try                     # curated starter for your Nu + platform (e.g., skim for Nu 0.114)
# or:
numan search nutest           # hides incompatible hits; use --all to show them
numan info vyadh/nutest
numan install vyadh/nutest
numan list
```

#### 4. Activate with Nu

```bash
numan activate                    # activate all inactive packages
numan activate owner/package-name # activate specific packages
numan activate --list             # show activation status
numan activate --check            # verify activation integrity (read-only)
```

`numan try` already activates unless you pass `--no-activate`.

For modules:

```bash
numan deactivate owner/module-name
```

#### 5. Maintain installs

```bash
numan update --check              # see available upgrades
numan update                      # apply upgrades
numan remove owner/package-name
numan gc --dry-run                # preview orphaned payload dirs
numan gc                          # delete unreferenced payloads
```

Numan snapshots activation state before `update`, `remove`, `activate`, and `deactivate`, so a bad change can be undone:

```bash
numan snapshot list
numan snapshot inspect <id>       # affected packages, digests, payload check
numan snapshot rollback <id>      # restore exactly that state
```

See [docs/snapshots-and-rollback.md](docs/snapshots-and-rollback.md) for scope, retention, and safety guarantees.

#### 6. Verify health

```bash
numan doctor                      # report-only diagnosis
numan doctor --fix --yes          # apply safe automated repairs
```

---

## Data layout

By default, Numan stores state under a platform-specific root (override with `NUMAN_ROOT` or `--root`):

| Platform | Default root |
|----------|--------------|
| Linux | `~/.local/share/numan` |
| macOS | `~/Library/Application Support/numan` |
| Windows | `%LOCALAPPDATA%\numan` |

Important paths under the root:

```text
numan/
├── config.toml          # registries, defaults
├── lockfile             # pinned installs (authoritative)
├── packages/            # immutable versioned payloads
├── registries/          # synced index caches
├── state/               # journals, nupm import provenance
└── nu_state/            # cached Nu paths for activation checks
```

Payload paths are immutable: `packages/<type>/<owner>/<name>/<version>-<hash>/`.

---

## Command reference

Global flag: `--root <path>` — override the Numan root directory (all commands).

| Command | Description |
|---------|-------------|
| `numan init [--refresh]` | Probe Nu and cache paths for activation |
| `numan try [--yes] [--no-activate]` | Install and activate a curated starter package for your Nu + platform (prefers Nu 0.114 starters; suggests managed Nu pin or search if no compatible starter) |
| `numan search <query>` | Search registry by name, description, or tags |
| `numan info <owner/name>` | Show package metadata and available versions |
| `numan install <owner/name[@version]>` | Download, verify, extract, and lock |
| `numan list` | List installed packages and activation status |
| `numan activate [pkg...]` | Register plugins / write module autoloads (scripts and completion packages are deferred) |
| `numan deactivate [pkg...]` | Remove module autoload entries |
| `numan update [--check] [pkg]` | Upgrade installed packages |
| `numan remove [--force] <pkg>` | Remove from lockfile and delete payload |
| `numan gc [--dry-run]` | Delete orphaned package directories |
| `numan snapshot list` | List all committed activation snapshots |
| `numan snapshot inspect <id>` | Show snapshot contents and rollback diff (read-only) |
| `numan snapshot delete <id> [--yes]` | Delete a snapshot |
| `numan snapshot rollback <id> [--yes]` | Restore exactly a stored snapshot |
| `numan registry list\|sync\|add\|remove\|packages` | Registry management |
| `numan setup nu [--version <x.y.z>]` | Download and install official Nushell under Numan root (optionally pinned) |
| `numan nupm status` | Summarize nupm home and import eligibility |
| `numan nupm inspect [--all] [path]` | Classify nupm packages at a path |
| `numan nupm import [--as owner/name] [path]` | One-way import into Numan |
| `numan nupm import --manifest file.toml` | Batch import from manifest |
| `numan nupm diff <owner/name>` | Compare imported payload vs nupm source |
| `numan completions <shell>` | Generate bash, fish, zsh, powershell, or nushell completions |
| `numan doctor [--fix] [--yes] [--json]` | Diagnose root health; optional safe repairs |

### Common flags (by command)

| Command | Flags |
|---------|-------|
| `install` | `--force` reinstall; `-v` / `--verbose` |
| `activate` | `--yes` skip prompt; `--verbose`; `--list` status only; `--check` integrity only |
| `deactivate` | `--yes` skip prompt; `--verbose` |
| `update` | `--check` report only; `-v` / `--verbose` |
| `remove` | `--force` remove despite active activation |
| `gc` | `--dry-run` preview only |
| `registry add` | `--key <base64-public-key>` (required for custom registries; official is auto-configured on `init`) |
| `nupm status` | `--nupm-home <path>` |
| `nupm inspect` | `--all` scan home; `--nupm-home <path>`; `--exit-on-ineligible` fail on ineligible |
| `nupm import` | `--as owner/name` (single import); `--manifest <file>` (batch); `--nupm-home <path>`; `--yes` skip consent |
| `doctor` | `--fix` apply safe repairs; `--yes` skip confirm tier; `--json` machine output; `--nupm-home <path>` |

Run `numan <command> --help` for full flag documentation.

---

## nupm migration

Numan can discover and import compatible packages from an existing [nupm](https://github.com/nushell/nupm) installation without modifying nupm state.

```bash
# Point at nupm home (or rely on $NUPM_HOME)
numan nupm status --nupm-home ~/.config/nupm
numan nupm inspect --all --nupm-home ~/.config/nupm

# Import a supported module package
numan nupm import /path/to/package --as myorg/my-module --yes

# Check drift after the source changes
numan nupm diff myorg/my-module
```

**Compatibility matrix:** which nupm package shapes Numan can import is defined in [docs/nupm-compatibility.md](docs/nupm-compatibility.md) (compat-schema-v1). Run `numan nupm inspect` to classify packages before import.

---

## Design principles

1. **Install is inert** — installs write only to `$NUMAN_ROOT`; Nu is never invoked.
2. **Activate is explicit** — the only command that registers plugins or manages autoload files.
3. **Lockfile is ground truth** — derived state (e.g. autoload projections) is not authoritative.
4. **Immutable payloads** — versions are content-addressed; updates leave old dirs until `gc`.
5. **Mutation serialization** — advisory locks prevent concurrent destructive operations.
6. **Safe Nu invocation** — plugin paths are passed via environment variables, not interpolated into shell strings.

See [AGENTS.md](AGENTS.md) for architecture details aimed at contributors and agents.

---

## Development

```bash
cargo build
cargo test                    # unit + hermetic integration tests
cargo clippy -- -D warnings   # lint (CI-enforced)
cargo fmt                     # format

# Real-Nu acceptance tests (requires Nu 0.113+ on PATH)
cargo test -- --ignored
```

CI runs tests, clippy, `rustfmt --check`, and real-Nu acceptance on Ubuntu, Windows, and macOS.

### Contributing

1. Branch from `master` (`feature/...` or `fix/...`).
2. Add or update tests for behavior changes.
3. Ensure `cargo test` and `cargo clippy -- -D warnings` pass.
4. Open a pull request with a clear description and test plan.

PR reviewers should follow [`REVIEW.md`](REVIEW.md).

---

## Roadmap

**Releases:** see the [latest GitHub Release](https://github.com/tonythethompson/numan/releases/latest) — feature-complete core on **0.1.x** while dogfooding the official registry.

For the cross-repository plan snapshot (as of 2026-07-29) across `numan`,
`numan-registry`, and `numan-plugins`, see
[docs/plans/2026-07-29-remaining-roadmap.md](docs/plans/2026-07-29-remaining-roadmap.md).

| Phase | Scope | Status |
|-------|--------|--------|
| **1–2** | Types, platform, lockfile, signed registry, install transaction | ✅ |
| **3–4** | Plugin + module activation, journals, managed autoloads | ✅ |
| **5** | `update` / `remove` / `gc`, lockfile v2, [snapshots + rollback](docs/snapshots-and-rollback.md) | ✅ (source builds deferred) |
| **6** | [nupm](docs/nupm-compatibility.md) status, inspect, import, drift | ✅ |
| **7** | Doctor, completions, onboarding, CI hardening, [winget packaging](docs/PACKAGING.md) | ✅ — [plan](docs/plans/Phase7Plan.md) |
| **Post-7.6** | Production [official registry](https://tonythethompson.github.io/numan-registry/) cutover; `numan init` and `numan doctor --fix` auto-configure `official` | ✅ (v0.1.4) |

### Next (toward 1.0)

| Item | Tracking |
|------|----------|
| Community **winget** install (`winget install tonythethompson.numan`) | ✅ [winget-pkgs PR #400470](https://github.com/microsoft/winget-pkgs/pull/400470); updates automated |
| Curated **official registry** packages + trust/bootstrap policy | 🔄 [#18](https://github.com/tonythethompson/numan/issues/18), [intake roadmap](docs/registry-intake-roadmap.md) stage 1 |
| Cross-platform **fresh-install** dogfooding | 🔄 `init` → `registry sync` → `search` → `install` → `activate` → `doctor` on Linux, macOS, Windows |

**1.0** when the rows above are done and there are no open P0/P1 issues on the core install/activate/update/remove lifecycle.

### Later

| Item | Tracking |
|------|----------|
| Source builds (clone/build with consent) | [#20](https://github.com/tonythethompson/numan/issues/20) / Phase 5.2 |
| Plugin lifecycle gate before mutation | [#22](https://github.com/tonythethompson/numan/issues/22) / Phase 5.5 |
| Registry intake automation (lint, discovery, validation reports) | [docs/registry-intake-roadmap.md](docs/registry-intake-roadmap.md) stages 2–4 |
| Scoop manifest | Deferred (low demand) |

<details>
<summary>Phase 7 detail (complete)</summary>

| Slice | Status |
|-------|--------|
| 7.1 Distribution baseline (releases, crates.io, `numan init`) | ✅ |
| 7.2 `numan doctor` | ✅ |
| 7.3 Completions + error UX | ✅ |
| 7.4 Onboarding quick start | ✅ |
| 7.5 CI / release hardening | ✅ |
| 7.6 winget manifests (in-repo) | ✅ |

</details>

---

## License

MIT — see [LICENSE](LICENSE).

---

## Related projects

- [Nushell](https://www.nushell.sh/) — the shell Numan packages for
- [nupm](https://github.com/nushell/nupm) — Nushell’s built-in package manager; Numan interoperates via import and drift detection
