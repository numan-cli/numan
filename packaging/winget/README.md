# Windows Package Manager (winget)

Manifests follow the [winget-pkgs](https://github.com/microsoft/winget-pkgs) layout under `manifests/t/tonythethompson/numan/<version>/`.

Package path and identifier use lowercase `tonythethompson.numan` (same publisher folder as [tonythethompson.QuickShell](https://github.com/microsoft/winget-pkgs/tree/master/manifests/t/tonythethompson/QuickShell); package segment is lowercase to avoid Windows casing duplicates).

## Install from local manifests (before winget-pkgs merge)

```powershell
winget install --manifest .\packaging\winget\manifests\t\tonythethompson\numan\0.1.4
```

Run from the repository root, or pass the full path to the version directory.

## Install from winget community repository

After manifests are accepted in [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs):

```powershell
winget install tonythethompson.numan
```

## Automated updates

The [`Publish to WinGet`](../../.github/workflows/winget.yml) workflow submits one update PR after each published GitHub Release. It uses the Windows `.zip` release asset and the existing `tonythethompson/winget-pkgs` fork.

The one-time repository setup requires a classic GitHub PAT with `public_repo` scope stored as the `WINGET_TOKEN` repository secret. The workflow can also be run manually for a published release tag.

See [docs/PACKAGING.md](../../docs/PACKAGING.md) for the full release checklist.
