# Packaging (winget)

Third-party install manifests live under `packaging/`. They pin GitHub Release binaries and are submitted to winget-pkgs automatically on each published release.

## Release packaging checklist

After a GitHub Release is published (see [RELEASING.md](RELEASING.md)):

1. Download `SHA256SUMS` from the release assets.
2. The [`Publish to WinGet`](../.github/workflows/winget.yml) workflow generates and submits the update PR using the published Windows `.zip` asset. It requires the repository's `WINGET_TOKEN` secret and the existing `tonythethompson/winget-pkgs` fork.
3. **winget** — the generated PR contains `packaging/winget/manifests/t/tonythethompson/numan/<version>/` with three manifests (schema **1.12.0**):
   - `tonythethompson.numan.yaml` (version)
   - `tonythethompson.numan.installer.yaml`
   - `tonythethompson.numan.locale.en-US.yaml`
   - Set `PackageIdentifier` to `tonythethompson.numan` (all-lowercase; matches open community PR)
   - Set `InstallerSha256` to uppercase hex from `SHA256SUMS`
   - Update nested `RelativeFilePath` if the archive folder name changed
   - **One version per PR** — the workflow submits only the new version; WinGetSvc validation rejects duplicate publisher paths.

## Install channels

| Channel | Command |
|---------|---------|
| GitHub Release | Download archive from [Releases](https://github.com/tonythethompson/numan/releases) |
| crates.io | `cargo install numan-cli` |
| From git | `cargo install --git https://github.com/tonythethompson/numan` |
| winget (local manifest) | `winget install --manifest packaging/winget/manifests/t/tonythethompson/numan/<version>` |
| winget (community) | `winget install tonythethompson.numan` (after the automated winget-pkgs PR merges) |

## Archive layout

Release archives extract to `numan-<version>-<target>/` containing the `numan` (or `numan.exe`) binary. Winget nested installers assume this layout.

## Installation coverage

| Installation channel | Linux x86_64 | macOS Apple Silicon | macOS Intel | Windows x86_64 | Windows ARM64 |
|-----------------------|---------------|---------------------|-------------|----------------|---------------|
| GitHub release archive | yes | yes | yes | yes | no |
| `cargo install numan-cli` | source build | source build | source build | source build | not validated |
| `cargo install --git` | source build | source build | source build | source build | not validated |
| winget | — | — | — | yes | — |

Cargo installs compile Numan locally for the host target. Windows ARM64 is not currently an official release target: the release workflow does not publish a Windows ARM64 archive, and CI does not validate an ARM64 runner or cross-target build.

Scoop is not packaged yet; see [Phase7Plan.md](plans/Phase7Plan.md).
