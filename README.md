# numan

[![CI](https://github.com/numan-cli/numan/actions/workflows/ci.yml/badge.svg)](https://github.com/numan-cli/numan/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
<a href="https://github.com/numan-cli/homebrew-numan"><img alt="Homebrew Package Version" src="https://img.shields.io/badge/dynamic/regex?url=https%3A%2F%2Fraw.githubusercontent.com%2Fnuman-cli%2Fhomebrew-numan%2Fmaster%2FFormula%2Fnuman.rb&search=version%20%22(%5B%5E%22%5D%2B)%22&replace=%241&label=homebrew&logo=homebrew&color=fbb040"></a>
![WinGet Package Version](https://img.shields.io/winget/v/tonythethompson.numan)
<img alt="Crates.io Version" src="https://img.shields.io/crates/v/numan-cli">
<img alt="Crates.io Version" src="https://img.shields.io/crates/d/numan-cli">

**numan** is a cross-platform package manager for [Nushell](https://www.nushell.sh/). It installs plugins, modules, scripts, and completion packages from signed registries, verifies downloaded artifacts, and records immutable install state in a lockfile.
Packages remain inert after installation. Plugins and modules are only registered with Nushell when you explicitly run `numan activate`.

Built in Rust for Linux, macOS, and Windows.

---

## About

Nushell has a growing ecosystem of community packages, but managing them across machines, Nu versions, and operating systems remains difficult.

nupm handles local package installation well. Registry-based workflows also need artifact verification, reproducible lockfiles, platform-aware resolution, and activation that remains safe across Nushell upgrades.

numan provides those guarantees:

| Concern | How numan handles it |
| -------- | ---------------------- |
| **Trust** | Verifies registry indexes with Ed25519 signatures, includes a built-in trust root for the `official` registry, and validates plugin binaries with SHA-256 |
| **Reproducibility** | Lockfile v2 pins version, payload hash, and install origin |
| **Platform safety** | Resolves artifacts for the current compile-time OS, architecture, and libc target |
| **Nu compatibility** | Respects the Nushell version constraints declared by each package |
| **Activation isolation** | `install` never modifies Nushell state. Only `activate` registers plugins or writes managed autoload files |
| **Crash recovery** | Uses journals to recover interrupted activation, autoload, lifecycle, and nupm import operations |
| **nupm coexistence** | Provides read-only discovery, one-way import, and drift detection for existing nupm installations |

numan is **early-stage**. The core install, activate, update, remove, garbage collection, registry, doctor, snapshot, nupm interoperability, and shell completion workflows are implemented.

These workflows are covered by unit tests, hermetic integration tests, and real-Nushell acceptance tests on Linux, macOS, and Windows in CI. Prebuilt binaries are published through GitHub Releases.

---

## Features

- **Registry-backed installs**: Search packages, inspect available versions, and install `owner/name` or `owner/name@version`.
- **Official registry**: `numan init` configures the `official` registry automatically. Its production trust root is built into numan, and `numan registry sync` verifies every signed index.
- **Package types**: Plugins and modules support activation. Scripts and completion packages are currently install-only while their activation contracts are finalized.
- **Verified artifacts**: Plugin binaries require SHA-256 hashes, and registry indexes require valid Ed25519 signatures.
- **Scoped activation**: Plugins remain active only while the Nushell executable hash, Nushell version, and plugin registry path match the recorded activation state.
- **Module autoloads**: numan writes managed vendor autoload files with ownership markers and validates candidate files before promotion.
- **Lifecycle management**: Update, remove, and garbage collection operations recover safely through lifecycle journals.
- **Shell tool integration & loader**: `numan setup loader` configures cached initialization for third-party shell CLI tools (Starship, Zoxide, Carapace, Atuin, Mise, Direnv, Oh-My-Posh), detects installed tools, isolates configurations in `loader-config.nu`, and downloads missing prebuilt binaries into `$NUMAN_ROOT/tools/bin/`.
- **nupm interoperability**: Use `numan nupm status`, `inspect`, `import`, and `diff` to inspect, migrate, and detect drift in existing [nupm](https://github.com/nushell/nupm) installations.
- **Health checks**: `numan doctor` diagnoses installation health and applies safe repairs by default. Use `--scan` for report-only mode.
- **Shell completions**: Install completions for Bash, Fish, Zsh, PowerShell, and Nushell with `numan completions` (use `--print` to emit the script).

---

## Registry package support

| Registry package type | Install, verify, and lock | `numan activate` | Support tier |
| ----------------------- | ---------------------------- | ------------------ | -------------- |
| Plugin | Yes | Yes, through Nu's plugin registry | Supported |
| Module | Yes | Yes, through a numan-managed vendor autoload file | Supported |
| Script | Yes | No | Install-only; activation is deferred |
| Completion package | Yes | No | Install-only; activation is deferred |

Install-only packages remain inert: numan downloads, verifies, locks, lists,
removes, and garbage-collects their payloads, but does not execute them or
modify Nu configuration for them. This is separate from numan's own shell
completion installer: `numan completions <shell>` is supported for bash, fish,
zsh, PowerShell, and Nushell (`nu`); use `--print` to emit the script instead.

---

## Installation

### From source

Requires [Rust](https://rustup.rs/) **1.88+** (stable recommended) and a Nushell binary on `PATH` for activation commands.

```bash
git clone https://github.com/numan-cli/numan.git
cd numan
cargo install --path .
```

The binary is named `numan`.

### Pre-built releases

Download the latest archive for your platform from [GitHub Releases](https://github.com/numan-cli/numan/releases). Each release ships:

| Platform | Archive | Binary |
| ---------- | --------- | -------- |
| Linux (x86_64) | `numan-<version>-x86_64-unknown-linux-gnu.tar.gz` | `numan` |
| Linux (aarch64) | `numan-<version>-aarch64-unknown-linux-gnu.tar.gz` | `numan` |
| Windows (x86_64) | `numan-<version>-x86_64-pc-windows-msvc.zip` | `numan.exe` |
| Windows (ARM64) | `numan-<version>-aarch64-pc-windows-msvc.zip` | `numan.exe` |
| macOS (Apple Silicon) | `numan-<version>-aarch64-apple-darwin.tar.gz` | `numan` |

#### Linux / macOS

```bash
tar -xzf numan-VERSION-TARGET.tar.gz
install -m 755 numan-VERSION-TARGET/numan ~/.local/bin/numan
```

#### Windows (PowerShell)

```powershell
Expand-Archive numan-VERSION-TARGET.zip -DestinationPath .
# Add the extracted folder to your PATH, or copy numan.exe into a directory already on PATH
```

Verify downloads with the `SHA256SUMS` file attached to each release.

### From git (latest `master`)

```bash
cargo install --git https://github.com/numan-cli/numan
```

Tracks the default branch. For a reproducible install, choose a published tag from [GitHub Releases](https://github.com/numan-cli/numan/releases) and pass `--tag vX.Y.Z`.

### Homebrew (macOS / Linux)

```bash
brew tap numan-cli/numan https://github.com/numan-cli/homebrew-numan
brew install numan
```

Uses the public [`homebrew-numan`](https://github.com/numan-cli/homebrew-numan) tap. Prefer the explicit HTTPS remote so `brew update` does not depend on SSH host keys. Formula digests update automatically after each GitHub Release (see [docs/PACKAGING.md](docs/PACKAGING.md)).

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

`numan completions <shell>` installs to the canonical path and creates parent directories if needed. Use `--print` to emit the script on stdout (pipe-safe; redirect hints go to stderr).

```bash
numan completions bash
numan completions zsh
numan completions fish
numan completions nushell
numan completions powershell   # writes ~/.numan/completions.ps1; dot-source from $PROFILE once

# Advanced: print + redirect / pipe
numan completions bash --print > ~/.local/share/bash-completion/completions/numan
numan completions powershell --print | Add-Content -Encoding utf8 $PROFILE
```

PowerShell completions are safe to place after other statements in `$PROFILE`.

---

## Quick start

Install with **one** of the options below, then run the activation path.

### Homebrew (macOS / Linux)

```bash
brew tap numan-cli/numan https://github.com/numan-cli/homebrew-numan && brew install numan
```

### winget (Windows)

```powershell
winget install tonythethompson.numan
```

### crates.io (any platform with Rust)

```bash
cargo install numan-cli
```

Or download a release archive from [GitHub Releases](https://github.com/numan-cli/numan/releases) and add `numan` to `PATH`.

Then:

```bash
numan init
numan registry sync
numan try owner/package-name  # try a package for your current Nu; explains compatibility if it won't work
numan doctor
```

Or pick a package yourself (`numan search` hides incompatible hits by default; use `--all` to see them):

```bash
numan search your-query
numan info owner/package-name
numan install owner/package-name
numan activate owner/package-name
```

Install is **inert** — nothing is registered with Nu until you run `numan activate` (or `numan try`, which activates after install if the package is compatible). `numan install` fails on incompatibility; `numan try <owner/name>` attempts the package, explains which managed Nu versions it works with, and suggests `numan use <version>` followed by `numan try <owner/name>` if a switch would help.

After Nu upgrades, refresh cached paths and activation identity:

```bash
numan init --refresh
```

Optional: install shell completions (`numan completions bash`, etc.) — see [Installation](#installation).

### Step-by-step

#### 1. Initialize

Probe your local Nu installation and create numan state under the default root (or `--root`):

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
numan try owner/package-name  # try a package for your Nu; explains compatibility if it won't work
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

`numan try <owner/name>` installs and activates if compatible, and explains compatible Nu versions if not. Use `--no-activate` to install without activating.

For modules:

```bash
numan deactivate owner/module-name
```

#### 5. Maintain installs

```bash
numan update --check              # see available package upgrades
numan update                      # apply package upgrades
numan update --self --check       # standalone: report if a newer binary is available
numan update --self               # standalone: download, checksum-verify, replace binary
numan remove owner/package-name
numan gc --dry-run                # preview orphaned payload dirs
numan gc                          # delete unreferenced payloads
```

For Homebrew, winget, or `cargo install` installs, `numan update --self` prints the matching upgrade command (`brew upgrade numan`, `winget upgrade tonythethompson.numan`, or `cargo install --locked --force numan-cli`) instead of replacing the binary. With `--check`, those installs still query GitHub Releases to report whether a newer version exists, then print the upgrade command only when an update is available. Standalone apply downloads the archive plus `SHA256SUMS` and `SHA256SUMS.sig`, verifies the Ed25519 signature with a public key baked into the binary, then checks the archive digest.

numan snapshots activation state before `update`, `remove`, `activate`, and `deactivate`, so a bad change can be undone:

```bash
numan snapshot list
numan snapshot inspect SNAPSHOT-ID       # affected packages, digests, payload check
numan snapshot rollback SNAPSHOT-ID      # restore exactly that state
```

See [docs/snapshots-and-rollback.md](docs/snapshots-and-rollback.md) for scope, retention, and safety guarantees.

#### 6. Verify health

```bash
numan doctor                      # diagnose and apply safe automated repairs
numan doctor --scan               # report-only diagnosis
```

---

## Data layout

By default, numan stores state under a platform-specific root (override with `NUMAN_ROOT` or `--root`):

| Platform | Default root |
| ---------- | -------------- |
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

Global flag: `--root <path>` — override the numan root directory (all commands).

| Command | Description |
| --------- | ------------- |
| `numan init [--refresh]` | Probe Nu and cache paths for activation |
| `numan try <owner/name[@version]> [--no-activate]` | Try a package against your current Nu and platform; lists compatible managed Nu versions and recommends one if it does not work now |
| `numan search <query>` | Search registry by name, description, or tags |
| `numan info <owner/name>` | Show package metadata and available versions |
| `numan install <owner/name[@version]>` | Download, verify, extract, and lock |
| `numan list` | List installed packages and activation status |
| `numan activate [pkg...]` | Register plugins / write module autoloads (scripts and completion packages are deferred) |
| `numan deactivate [pkg...]` | Remove module autoload entries |
| `numan update [--check] [pkg]` | Upgrade installed packages |
| `numan update --self [--check]` | Upgrade the numan binary (GitHub Release self-replace, or print brew/winget/cargo command) |
| `numan remove [--force] <pkg>` | Remove from lockfile and delete payload |
| `numan gc [--dry-run]` | Delete orphaned package directories |
| `numan snapshot list` | List all committed activation snapshots |
| `numan snapshot inspect <id>` | Show snapshot contents and rollback diff (read-only) |
| `numan snapshot delete <id> [--yes]` | Delete a snapshot |
| `numan snapshot rollback <id> [--yes]` | Restore exactly a stored snapshot |
| `numan registry list\|sync\|add\|remove\|packages` | Registry management |
| `numan setup nu [VERSION]` | Download and install official Nushell under numan root (optionally pinned) |
| `numan setup nu remove` | Remove the managed Nushell install and fall back to PATH Nu |
| `numan setup nu path` | Use the Nushell already on PATH (removes managed install) |
| `numan setup nu use <path>` | Register a specific existing Nushell binary |
| `numan setup loader` | Setup nushell-loader integration and third-party shell CLI tools |
| `numan use <version>` | Switch the active managed Nu to a pinned version (no auto-install; errors with a hint to run `numan setup nu <version>` if missing). Cross-minor switches deactivate Numan-active plugins/modules for the leaving Nu and restore that minor's remembered set when you switch back. |
| `numan use latest` | Switch the active managed Nu to the latest installed version (same leave/restore behavior as `use <version>`) |
| `numan use list` | List installed managed Nu versions and mark the active one |
| `numan nupm status` | Summarize nupm home and import eligibility |
| `numan nupm inspect [--all] [path]` | Classify nupm packages at a path |
| `numan nupm import [--as owner/name] [path]` | One-way import into numan |
| `numan nupm import --manifest file.toml` | Batch import from manifest |
| `numan nupm diff <owner/name>` | Compare imported payload vs nupm source |
| `numan completions <shell>` | Install shell completions (use `--print` to emit the script) |
| `numan doctor [--scan] [--json]` | Diagnose root health and repair (use `--scan` for report-only) |

### Common flags (by command)

| Command | Flags |
| --------- | ------- |
| `install` | `--force` reinstall; `-v` / `--verbose` |
| `activate` | `--verbose`; `--list` status only; `--check` integrity only |
| `deactivate` | `--verbose` |
| `update` | `--check` report only; `--self` update the numan binary; `-v` / `--verbose` |
| `remove` | `--force` remove despite active activation |
| `gc` | `--dry-run` preview only |
| `registry add` | `--key <base64-public-key>` (required for custom registries; official is auto-configured on `init`) |
| `nupm status` | `--nupm-home <path>` |
| `nupm inspect` | `--all` scan home; `--nupm-home <path>`; `--exit-on-ineligible` fail on ineligible |
| `nupm import` | `--as owner/name` (single import); `--manifest <file>` (batch); `--nupm-home <path>`; `--yes` skip consent |
| `doctor` | `--scan` report only; `--json` machine output; `--nupm-home <path>` (repairs by default) |
| `setup nu` | `--force` re-download; `--skip-path` don't update PATH; `--yes` skip prompt |
| `setup loader` | `--status` check health/cache; `--detect` scan PATH tools; `--add <tool>` add preset/custom; `--remove <tool>` remove tool; `--clean` purge caches; `--install` / `--install-missing` download binaries; `--force` overwrite engine; `--configure` append to `config.nu` |

Run `numan <command> --help` for full flag documentation.

---

## Shell tool integration (nushell-loader)

Numan includes a high-performance loader integration based on [nushell-loader](https://github.com/aidnem/nushell-loader). It caches initialization scripts for external tools (Starship, Zoxide, Carapace, Atuin, Mise, Direnv, Oh-My-Posh) in `$nu.data-dir/vendor/autoload/`, speeding up Nushell startup.

```bash
# 1. Install loader.nu and append source snippet to config.nu
numan setup loader --configure

# 2. Detect shell tools on your PATH and register them
numan setup loader --detect

# 3. Add a tool preset (with optional binary download if missing from PATH)
numan setup loader --add starship --install
numan setup loader --add "custom=echo 'source ~/.custom.nu'"

# 4. Check status and cache files
numan setup loader --status

# 5. Clean / invalidate cached initialization scripts
numan setup loader --clean
```

Tool definitions are isolated in `loader-config.nu`, ensuring that updating the loader engine (`numan setup loader --force`) preserves your custom configuration. Binaries downloaded via `--install` or `--install-missing` are placed in `$NUMAN_ROOT/tools/bin/` and persisted to your `PATH`.

---

## nupm migration

numan can discover and import compatible packages from an existing [nupm](https://github.com/nushell/nupm) installation without modifying nupm state.

```bash
# Point at nupm home (or rely on $NUPM_HOME)
numan nupm status --nupm-home ~/.config/nupm
numan nupm inspect --all --nupm-home ~/.config/nupm

# Import a supported module package
numan nupm import /path/to/package --as myorg/my-module --yes

# Check drift after the source changes
numan nupm diff myorg/my-module
```

**Compatibility matrix:** which nupm package shapes numan can import is defined in [docs/nupm-compatibility.md](docs/nupm-compatibility.md) (compat-schema-v1). Run `numan nupm inspect` to classify packages before import.

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

# Real-Nu acceptance tests (requires Nu 0.115 on PATH; portable suite)
cargo test -- --ignored --skip acceptance_process_helper --skip stage1_official_registry --skip real_nu_active_update_
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

**Releases:** see the [latest GitHub Release](https://github.com/numan-cli/numan/releases/latest) — feature-complete core on **0.2.x** while dogfooding the official registry ([catalog × Nu matrix](https://github.com/numan-cli/numan-registry/blob/main/docs/catalog-compat.md) for live package counts and Nu bands).

For the cross-repository plan toward 1.0 across `numan`, `numan-registry`, and
`numan-plugins`, see
[docs/plans/consolidated-multi-repo-roadmap.md](docs/plans/consolidated-multi-repo-roadmap.md)
(contract-pinned). The 2026-07-29 snapshot is
[superseded](docs/plans/2026-07-29-remaining-roadmap.md).

| Phase | Scope | Status |
| ------- | -------- | -------- |
| **1–2** | Types, platform, lockfile, signed registry, install transaction | ✅ |
| **3–4** | Plugin + module activation, journals, managed autoloads | ✅ |
| **5** | `update` / `remove` / `gc`, lockfile v2, [snapshots + rollback](docs/snapshots-and-rollback.md) | ✅ (source builds deferred; active-plugin update opt-in) |
| **6** | [nupm](docs/nupm-compatibility.md) status, inspect, import, drift | ✅ |
| **7** | Doctor, completions, onboarding, CI hardening, [winget + Homebrew tap](docs/PACKAGING.md) | ✅ — [plan](docs/plans/Phase7Plan.md) |
| **Post-7.6** | Production [official registry](https://numan-cli.github.io/numan-registry/) cutover; `numan init` and `numan doctor` auto-configure `official` | ✅ (v0.1.4) |

### Next (toward 1.0)

| Item | Tracking |
| ------ | ---------- |
| Curated **official registry** depth + multi-OS first-use demos | 🔄 [#18](https://github.com/numan-cli/numan/issues/18), [catalog-compat](https://github.com/numan-cli/numan-registry/blob/main/docs/catalog-compat.md), [intake roadmap](docs/registry-intake-roadmap.md) |
| Cross-platform **fresh-install** dogfooding + lifecycle evidence | 🔄 `init` → `registry sync` → `search` → `install` → `activate` → `doctor` on Linux, macOS, Windows |
| Ship **0.2.0** (`setup nu` redesign + `numan use`) | ✅ [CHANGELOG](CHANGELOG.md) |
| Cut **0.2.1** (`update --self`, install-only honesty, `try` script fallback) | ✅ [v0.2.1](https://github.com/numan-cli/numan/releases/tag/v0.2.1) |
| Cut **0.2.2** (ARM release assets, `try` UX redesign, intake provenance/tiers, Nu 0.114 extract cap fix) | ✅ [v0.2.2](https://github.com/numan-cli/numan/releases/tag/v0.2.2) |

**1.0** when the [unified gate](docs/plans/consolidated-multi-repo-roadmap.md#unified-10-gate) is green and there are no open P0/P1 issues on the core install/activate/update/remove lifecycle.

### Later

| Item | Tracking |
| ------ | ---------- |
| Source builds (clone and build each require explicit consent, separate scopes) | [#20](https://github.com/numan-cli/numan/issues/20) / Phase 5.2 |
| Active-plugin **update** default-on (today: exact `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` opt-in) | [#22](https://github.com/numan-cli/numan/issues/22) / [active-plugin-gate.md](docs/active-plugin-gate.md) |
| Completions/scripts activation contracts | [docs/registry-intake-roadmap.md](docs/registry-intake-roadmap.md) |
| Scoop manifest | Deferred (low demand) |

<details>
<summary>Phase 7 detail (complete)</summary>

| Slice | Status |
| ------- | -------- |
| 7.1 Distribution baseline (releases, crates.io, `numan init`) | ✅ |
| 7.2 `numan doctor` | ✅ |
| 7.3 Completions + error UX | ✅ |
| 7.4 Onboarding quick start | ✅ |
| 7.5 CI / release hardening | ✅ |
| 7.6 winget manifests (in-repo) + Homebrew tap | ✅ |

</details>

---

## Security

To report a vulnerability in the Numan CLI, see [SECURITY.md](SECURITY.md).
Registry catalog and signing incidents are covered by
[numan-registry SECURITY.md](https://github.com/numan-cli/numan-registry/blob/main/SECURITY.md).

---

## License

MIT — see [LICENSE](LICENSE).

---

## Related projects

- [Nushell](https://www.nushell.sh/) — the shell numan packages for
- [nupm](https://github.com/nushell/nupm) — Nushell’s built-in package manager; numan interoperates via import and drift detection
- [numan-registry](https://github.com/numan-cli/numan-registry) — signed official catalog ([catalog × Nu matrix](https://github.com/numan-cli/numan-registry/blob/main/docs/catalog-compat.md))
- [numan-plugins](https://github.com/numan-cli/numan-plugins) — CI-built plugin binaries for source-only upstreams ([backlog](https://github.com/numan-cli/numan-plugins/blob/main/docs/backlog.json))
