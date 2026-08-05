#Requires -Version 5.1
<#
.SYNOPSIS
  Install numan via winget after checking for conflicting installs from other channels.

.DESCRIPTION
  Runs the numan install guard (prompt to uninstall cargo/homebrew/release copies first).
  Cancels winget install when the user declines or uninstall fails.

  From the numan repository root:
    powershell -File scripts/install-winget.ps1

  Plain `winget install` does not run this guard; prefer this script when switching
  from cargo or another package manager.
#>
[CmdletBinding()]
param(
    [string[]]$WingetArgs = @("install", "tonythethompson.numan")
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$guardManifest = Join-Path $repoRoot "install-guard\Cargo.toml"

if (-not (Test-Path $guardManifest)) {
    Write-Error "install-guard manifest not found at $guardManifest (run from the numan repo)."
}

Write-Host "Checking for conflicting numan installs..."
& cargo run --quiet --manifest-path $guardManifest -- winget
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Host "Running: winget $($WingetArgs -join ' ')"
& winget @WingetArgs
exit $LASTEXITCODE
