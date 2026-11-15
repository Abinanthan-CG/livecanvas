<#
.SYNOPSIS
    Builds livecanvas.exe and then livecanvas-setup.exe (which embeds it).

.DESCRIPTION
    The installer embeds the application binary, so the application has to be
    built first. This script runs the two builds in order and collects the
    results in .\dist. No administrator rights are needed.

.EXAMPLE
    .\build.ps1
    .\build.ps1 -Target aarch64-pc-windows-msvc
#>
param(
    [ValidateSet('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc')]
    [string]$Target = 'x86_64-pc-windows-msvc'
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

function Invoke-Cargo {
    cargo @args
    if ($LASTEXITCODE -ne 0) { throw "cargo $($args -join ' ') failed with exit code $LASTEXITCODE" }
}

$release = Join-Path $PSScriptRoot "target\$Target\release"

Write-Host "==> Running unit tests" -ForegroundColor Cyan
Invoke-Cargo test -p livecanvas-core

Write-Host "==> Building livecanvas.exe ($Target)" -ForegroundColor Cyan
Invoke-Cargo build --release --target $Target -p livecanvas

Write-Host "==> Building livecanvas-setup.exe ($Target)" -ForegroundColor Cyan
$env:LIVECANVAS_PAYLOAD = Join-Path $release 'livecanvas.exe'
try {
    Invoke-Cargo build --release --target $Target -p livecanvas-setup
}
finally {
    Remove-Item Env:LIVECANVAS_PAYLOAD -ErrorAction SilentlyContinue
}

$dist = Join-Path $PSScriptRoot 'dist'
New-Item -ItemType Directory -Force -Path $dist | Out-Null
Copy-Item (Join-Path $release 'livecanvas.exe') $dist -Force
Copy-Item (Join-Path $release 'livecanvas-setup.exe') $dist -Force

Write-Host "==> Done" -ForegroundColor Green
Get-ChildItem $dist | Format-Table Name, @{ Label = 'Size (KB)'; Expression = { [math]::Round($_.Length / 1KB) } }
