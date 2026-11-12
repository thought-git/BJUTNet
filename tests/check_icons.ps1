# Check that each exe has an embedded icon and version info.
# ASCII only (PowerShell 5.1 reads non-BOM UTF-8 as GBK).
param([string]$Dir = "")

Add-Type -AssemblyName System.Drawing
foreach ($n in @('BJUTNet', 'BJUTNetSvc', 'BJUTNetUninstall', 'BJUTNetSetup')) {
    $p = Join-Path $Dir ($n + '.exe')
    if (-not (Test-Path $p)) { Write-Output ($n + '|missing|'); continue }
    $icon = [System.Drawing.Icon]::ExtractAssociatedIcon($p)
    $ver = (Get-Item $p).VersionInfo.FileVersion
    $desc = (Get-Item $p).VersionInfo.FileDescription
    $has = if ($icon) { 'icon' } else { 'noicon' }
    Write-Output ($n + '|' + $has + '|' + $ver + '|' + $desc)
}
