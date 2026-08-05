#Requires -Version 5.1
<#
.SYNOPSIS
  Install numan via cargo after checking for conflicting installs from other channels.
#>
[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs = @("install", "--path", ".")
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repoRoot "Cargo.toml"

Write-Host "Checking for conflicting numan installs..."
& cargo run --quiet --bin numan-install-guard --manifest-path $manifest -- cargo
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Host "Running: cargo $($CargoArgs -join ' ')"
& cargo @CargoArgs --manifest-path $manifest
exit $LASTEXITCODE
