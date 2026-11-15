# Show the three BJUTNet alert popups with sample data.
# Usage: powershell -ExecutionPolicy Bypass -File tests\popup_demo.ps1 [-ExePath <BJUTNetSvc.exe>]
param([string]$ExePath = "")

$ErrorActionPreference = "Stop"

if (-not $ExePath) {
    $candidates = @(
        (Join-Path $PSScriptRoot "..\dist\BJUTNetSvc.exe"),
        (Join-Path $PSScriptRoot "..\target\release\BJUTNetSvc.exe"),
        "C:\Program Files\BJUTNet\BJUTNetSvc.exe"
    )
    $ExePath = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}

if (-not $ExePath -or -not (Test-Path $ExePath)) {
    Write-Error "BJUTNetSvc.exe not found. Pass -ExePath <path> or build first."
    exit 1
}

Write-Host "Showing 3 demo popups (flow / balance / offline) using:"
Write-Host "  $ExePath"
Write-Host "Close each popup to see the next one."
& $ExePath --demo-popups
