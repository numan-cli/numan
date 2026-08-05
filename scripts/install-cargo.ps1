#Requires -Version 5.1
<#
.SYNOPSIS
  Install numan via cargo after checking for conflicting installs from other channels.

.DESCRIPTION
  Plain `cargo install` already runs the guard via build.rs. Use this wrapper when you
  want an explicit pre-check before `cargo install --path .` from a git checkout.
#>
[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs = @("install", "--path", ".")
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$guardManifest = Join-Path $repoRoot "install-guard\Cargo.toml"

if (-not (Test-Path $guardManifest)) {
    Write-Error "install-guard manifest not found at $guardManifest (run from the numan repo)."
}

Write-Host "Checking for conflicting numan installs..."
& cargo run --quiet --manifest-path $guardManifest -- cargo
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Host "Running: cargo $($CargoArgs -join ' ')"
& cargo @CargoArgs
exit $LASTEXITCODE
