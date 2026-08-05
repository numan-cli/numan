# Packaging (Homebrew, winget)

Third-party install manifests live under `packaging/`. They pin GitHub Release
binaries. WinGet and Homebrew tap updates are submitted automatically after each
published `v*.*.*` release.

## Release packaging checklist

After a GitHub Release is published (see [RELEASING.md](RELEASING.md)):

1. Confirm platform archives and `SHA256SUMS` on the release.
2. Confirm [`Publish to WinGet`](../.github/workflows/winget.yml) ran (requires
   `WINGET_TOKEN`) and opened/updated the winget-pkgs PR.
3. Confirm [`Publish to Homebrew tap`](../.github/workflows/homebrew.yml) ran
   (requires `HOMEBREW_TAP_TOKEN`) and pushed `Formula/numan.rb` to
   [`tonythethompson/homebrew-numan`](https://github.com/tonythethompson/homebrew-numan).
4. Spot-check on a Mac or Linux Homebrew host:

   ```bash
   brew tap tonythethompson/numan
   brew update
   brew install numan
   numan --version
   ```

Manual Homebrew dry-run (no tap push):

```bash
gh release download vX.Y.Z --pattern SHA256SUMS
python3 scripts/render_homebrew_formula.py --version X.Y.Z --sha256sums SHA256SUMS --write
```

## Install channels

| Channel | Command |
|---------|---------|
| GitHub Release | Download archive from [Releases](https://github.com/tonythethompson/numan/releases) |
| crates.io | `cargo install numan-cli` |
| From git | `cargo install --git https://github.com/tonythethompson/numan` |
| Homebrew tap | `brew tap tonythethompson/numan && brew install numan` (use `scripts/install-homebrew.sh` when switching from cargo/winget) |
| winget (community) | `winget install tonythethompson.numan` (use `scripts/install-winget.ps1` when switching from cargo/Homebrew) |

## Install channel guard

Cross-channel installs (cargo vs winget vs Homebrew) prompt to uninstall the existing copy first; declining cancels the install.

| Channel | Guard behavior |
|---------|----------------|
| `cargo install numan-cli` | Automatic via `build.rs` during install |
| winget | `powershell -File scripts/install-winget.ps1` |
| Homebrew tap | `bash scripts/install-homebrew.sh` |
| CI / automation | Set `NUMAN_SKIP_INSTALL_GUARD=1` to bypass |

`numan doctor` warns when multiple channels are detected (`install.multiple_channels`).


Release archives extract to `numan-<version>-<target>/` containing the `numan`
(or `numan.exe`) binary. Homebrew and winget installers assume this layout.

## Installation coverage

| Installation channel | Linux x86_64 | macOS Apple Silicon | Windows x86_64 | Windows ARM64 |
|-----------------------|---------------|---------------------|----------------|---------------|
| GitHub release archive | yes | yes | yes | no |
| `cargo install numan-cli` | source build | source build | source build | not validated |
| Homebrew tap | yes | yes | — | — |
| winget | — | — | yes | — |

## Secrets

| Secret | Purpose |
|--------|---------|
| `WINGET_TOKEN` | Open winget-pkgs update PRs |
| `HOMEBREW_TAP_TOKEN` | Push formula updates to `tonythethompson/homebrew-numan` |

The Homebrew tap repository must stay **public**. Private visibility is why
`brew tap tonythethompson/numan` previously failed for most users.

Scoop is not packaged yet; see [Phase7Plan.md](plans/Phase7Plan.md).
