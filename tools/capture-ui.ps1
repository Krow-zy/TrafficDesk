#Requires -Version 5.1
<#
.SYNOPSIS
  Captures the docs/ screenshots for the three UI modes.

.DESCRIPTION
  Runs the release build with TRAFFICDESK_DEMO=1, so the window shows the fixed
  sample rows from src/demo.rs. Nothing is captured and no real hostname,
  address or process name ends up in the images.

  Every shot is cropped to exactly the window rectangle reported by user32, and
  the script fails if the image does not match that rectangle or looks like it
  shows something other than the dark UI (an occluding window, for instance).

.EXAMPLE
  pwsh -File tools\capture-ui.ps1
  pwsh -File tools\capture-ui.ps1 -Mode expanded -SettleSeconds 8
#>
[CmdletBinding()]
param(
    # Which modes to capture: expanded, panel, mini.
    [string[]]$Mode = @('expanded', 'panel', 'mini'),
    # Seconds to let the sample data and the graphs settle before the shot.
    [int]$SettleSeconds = 5
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$repo = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repo 'target\release\trafficdesk.exe'
$outDir = Join-Path $repo 'docs'
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) 'TrafficDesk-capture'

if (-not (Test-Path -LiteralPath $exe)) {
    throw "build first: cargo build --release ($exe is missing)"
}
if (Get-Process -Name trafficdesk -ErrorAction SilentlyContinue) {
    throw 'TrafficDesk is already running; close it so the capture starts with a fresh window'
}
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

if (-not ('TDCapture.Win32' -as [type])) {
    Add-Type -Namespace TDCapture -Name Win32 -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc callback, IntPtr lParam);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);

[StructLayout(LayoutKind.Sequential)]
public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }

// The process also owns a hidden tray window and a small "winit thread event
// target", so pick the biggest visible top-level window of the process.
public static IntPtr LargestWindow(uint pid) {
    IntPtr best = IntPtr.Zero;
    long bestArea = 0;
    EnumWindows((hWnd, _) => {
        uint owner;
        GetWindowThreadProcessId(hWnd, out owner);
        if (owner != pid || !IsWindowVisible(hWnd)) return true;
        RECT r;
        if (!GetWindowRect(hWnd, out r)) return true;
        long area = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
        if (area > bestArea) { bestArea = area; best = hWnd; }
        return true;
    }, IntPtr.Zero);
    return best;
}
'@
}
[TDCapture.Win32]::SetProcessDPIAware() | Out-Null

function Get-WindowRect([IntPtr]$handle) {
    $rect = New-Object TDCapture.Win32+RECT
    if (-not [TDCapture.Win32]::GetWindowRect($handle, [ref]$rect)) { throw "GetWindowRect failed for $handle" }
    $rect
}

# The tray window and the winit helper window exist before the real one is
# painted, so wait for a window that is at least 100 x 20 pixels.
function Wait-AppWindow([int]$processId, [int]$timeoutSeconds = 20) {
    $deadline = (Get-Date).AddSeconds($timeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
        $handle = [TDCapture.Win32]::LargestWindow([uint32]$processId)
        if ($handle -ne [IntPtr]::Zero) {
            $rect = Get-WindowRect $handle
            if (($rect.Right - $rect.Left) -ge 100 -and ($rect.Bottom - $rect.Top) -ge 20) { return $handle }
        }
    }
    throw "no usable window appeared within ${timeoutSeconds}s"
}

function Save-WindowShot([IntPtr]$handle, [string]$path) {
    [TDCapture.Win32]::SetForegroundWindow($handle) | Out-Null
    Start-Sleep -Milliseconds 400

    $rect = Get-WindowRect $handle
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    if ($width -le 0 -or $height -le 0) { throw "window has no area ($width x $height)" }

    $bitmap = New-Object System.Drawing.Bitmap $width, $height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size $width, $height))
    }
    finally {
        $graphics.Dispose()
    }
    $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()

    $saved = [System.Drawing.Bitmap]::FromFile($path)
    $savedWidth = $saved.Width
    $savedHeight = $saved.Height
    # The UI is dark, so a grid sample should be mostly dark, and a shot of a
    # populated window always mixes several palette colours.
    $colours = @{}
    $bright = 0
    $sampled = 0
    for ($y = 0; $y -lt $savedHeight; $y += 7) {
        for ($x = 0; $x -lt $savedWidth; $x += 7) {
            $colour = $saved.GetPixel($x, $y)
            $luma = 0.299 * $colour.R + 0.587 * $colour.G + 0.114 * $colour.B
            if ($luma -gt 150) { $bright++ }
            $colours[$colour.ToArgb()] = $true
            $sampled++
        }
    }
    $saved.Dispose()

    [pscustomobject]@{
        File        = Split-Path -Leaf $path
        Size        = "${savedWidth}x${savedHeight}"
        MatchesRect = ($savedWidth -eq $width -and $savedHeight -eq $height)
        Varied      = ($colours.Count -gt 10)
        DarkTheme   = ($bright / $sampled -lt 0.4)
        Bytes       = (Get-Item -LiteralPath $path).Length
    }
}

Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $scratch | Out-Null
$env:TRAFFICDESK_DEMO = '1'
$env:TRAFFICDESK_DATA_DIR = $scratch

$results = @()
try {
    foreach ($m in $Mode) {
        @"
poll_interval_ms = 1000
retention_days = 14
ignore_processes = []
mode = "$m"
"@ | Set-Content -LiteralPath (Join-Path $scratch 'config.toml') -Encoding utf8

        $process = Start-Process -FilePath $exe -PassThru
        try {
            $handle = Wait-AppWindow $process.Id
            Start-Sleep -Seconds $SettleSeconds
            $process.Refresh()
            if ($process.HasExited) { throw "the app exited early (code $($process.ExitCode))" }
            # Re-read: the window object is stable, but this keeps the crop tied
            # to the window that is actually on screen after the settle time.
            $handle = Wait-AppWindow $process.Id 5

            $name = if ($m -eq 'mini') { 'minibar' } else { $m }
            $results += Save-WindowShot $handle (Join-Path $outDir "$name.png")
        }
        finally {
            if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit(5000) | Out-Null }
        }
    }
}
finally {
    Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue
    Remove-Item Env:\TRAFFICDESK_DEMO -ErrorAction SilentlyContinue
    Remove-Item Env:\TRAFFICDESK_DATA_DIR -ErrorAction SilentlyContinue
}

$results | Format-Table -AutoSize | Out-String -Width 100 | Write-Host
foreach ($r in $results) {
    if (-not $r.MatchesRect) { throw "$($r.File) does not match its window rectangle" }
    if (-not $r.Varied) { throw "$($r.File) has almost no colours; the window was probably not on screen" }
    if (-not $r.DarkTheme) { throw "$($r.File) looks like it captured something else on top of the window" }
}
"wrote $($results.Count) screenshots to $outDir"
