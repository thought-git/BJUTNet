# Capture the window of a process by name (ASCII only: PowerShell 5.1 reads
# non-BOM UTF-8 as GBK, so Chinese comments would break parsing).
param(
    [string]$ProcessName = "BJUTNet",
    [string]$OutPath = "",
    [int]$WaitMs = 2500
)

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Shot {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
}
"@

Start-Sleep -Milliseconds $WaitMs
$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { Write-Error "process not found: $ProcessName"; exit 1 }
$h = $proc.MainWindowHandle
if ($h -eq [IntPtr]::Zero) { Write-Error "no main window"; exit 1 }

[void][Win32Shot]::SetForegroundWindow($h)
Start-Sleep -Milliseconds 400

$r = New-Object Win32Shot+RECT
[void][Win32Shot]::GetWindowRect($h, [ref]$r)
$w = $r.Right - $r.Left
$hh = $r.Bottom - $r.Top
if ($w -le 0 -or $hh -le 0) { Write-Error "bad window size: $w x $hh"; exit 1 }

$bmp = New-Object System.Drawing.Bitmap($w, $hh)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size($w, $hh)))
$bmp.Save($OutPath, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose()
$bmp.Dispose()
Write-Host "saved: $OutPath ($w x $hh)"
