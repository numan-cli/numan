#Requires -Version 5.1
<#
.SYNOPSIS
  Install numan via winget after checking for conflicting installs from other channels.
#>
[CmdletBinding()]
param(
    [string[]]$WingetArgs = @("install", "tonythethompson.numan")
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repoRoot "Cargo.toml"

Write-Host "Checking for conflicting numan installs..."
& cargo run --quiet --bin numan-install-guard --manifest-path $manifest -- winget
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Host "Running: winget $($WingetArgs -join ' ')"
& winget @WingetArgs
exit $LASTEXITCODE
