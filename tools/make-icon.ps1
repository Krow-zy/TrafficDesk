<#
.SYNOPSIS
  Draws the TrafficDesk icon and writes assets/icon.ico plus PNG exports.

.DESCRIPTION
  The mark matches the tray indicator: a dark rounded plate with a green
  upstream bar and a blue downstream bar, arrow caps added from 32px up so the
  small sizes stay legible. Every size is drawn at its own resolution instead
  of being downscaled, which keeps 16x16 crisp in Explorer and the taskbar.

  Needs nothing but .NET System.Drawing, so it runs on a plain Windows box.

.EXAMPLE
  pwsh -File tools/make-icon.ps1
#>
[CmdletBinding()]
param(
    [string]$AssetsDir = (Join-Path $PSScriptRoot '..\assets')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$AssetsDir = [System.IO.Path]::GetFullPath($AssetsDir)
New-Item -ItemType Directory -Path $AssetsDir -Force | Out-Null

# Palette shared with the UI (see src/ui.rs) so the icon and the app agree.
$PlateTop = [System.Drawing.Color]::FromArgb(255, 26, 33, 43)
$PlateBottom = [System.Drawing.Color]::FromArgb(255, 12, 16, 21)
$Edge = [System.Drawing.Color]::FromArgb(255, 44, 55, 68)
$Up = [System.Drawing.Color]::FromArgb(255, 61, 220, 132)
$Down = [System.Drawing.Color]::FromArgb(255, 77, 184, 255)

function New-RoundedPath {
    param([single]$X, [single]$Y, [single]$W, [single]$H, [single]$Radius)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $Radius * 2
    $path.AddArc($X, $Y, $d, $d, 180, 90)
    $path.AddArc($X + $W - $d, $Y, $d, $d, 270, 90)
    $path.AddArc($X + $W - $d, $Y + $H - $d, $d, $d, 0, 90)
    $path.AddArc($X, $Y + $H - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    $path
}

function New-IconBitmap {
    param([int]$Size)
    $bmp = New-Object System.Drawing.Bitmap $Size, $Size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality

    $scale = $Size / 256.0
    $inset = [single](10 * $scale)
    $side = [single]($Size - 2 * $inset)
    $radius = [single](56 * $scale)

    # Plate with a soft vertical gradient and a one-pixel rim.
    $plate = New-RoundedPath -X $inset -Y $inset -W $side -H $side -Radius $radius
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        (New-Object System.Drawing.PointF 0, $inset),
        (New-Object System.Drawing.PointF 0, ($inset + $side)),
        $PlateTop, $PlateBottom)
    $g.FillPath($brush, $plate)
    $pen = New-Object System.Drawing.Pen $Edge, ([single][Math]::Max(1.0, 3.0 * $scale))
    $g.DrawPath($pen, $plate)

    # Two bars: up on the left, down on the right, like the tray indicator.
    $barW = [single](26 * $scale)
    $barH = [single](96 * $scale)
    $gap = [single](26 * $scale)
    $centerY = [single]($Size / 2)
    $leftX = [single]($Size / 2 - $gap / 2 - $barW)
    $rightX = [single]($Size / 2 + $gap / 2)
    $top = [single]($centerY - $barH / 2)

    $upBrush = New-Object System.Drawing.SolidBrush $Up
    $downBrush = New-Object System.Drawing.SolidBrush $Down
    $upBar = New-RoundedPath -X $leftX -Y $top -W $barW -H $barH -Radius ([single]($barW / 2))
    $downBar = New-RoundedPath -X $rightX -Y $top -W $barW -H $barH -Radius ([single]($barW / 2))
    $g.FillPath($upBrush, $upBar)
    $g.FillPath($downBrush, $downBar)

    # Arrow caps, only where they still read: 32px and above.
    if ($Size -ge 32) {
        $head = [single](22 * $scale)
        $stroke = [single](11 * $scale)
        $upPen = New-Object System.Drawing.Pen $Up, $stroke
        $downPen = New-Object System.Drawing.Pen $Down, $stroke
        foreach ($p in @($upPen, $downPen)) {
            $p.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
            $p.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
            $p.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
        }
        $upCx = $leftX + $barW / 2
        $downCx = $rightX + $barW / 2
        $g.DrawLines($upPen, [System.Drawing.PointF[]]@(
            (New-Object System.Drawing.PointF (($upCx - $head / 2)), ($top - $head * 0.15)),
            (New-Object System.Drawing.PointF $upCx, ($top - $head * 0.85)),
            (New-Object System.Drawing.PointF (($upCx + $head / 2)), ($top - $head * 0.15))
        ))
        $bottom = $top + $barH
        $g.DrawLines($downPen, [System.Drawing.PointF[]]@(
            (New-Object System.Drawing.PointF (($downCx - $head / 2)), ($bottom + $head * 0.15)),
            (New-Object System.Drawing.PointF $downCx, ($bottom + $head * 0.85)),
            (New-Object System.Drawing.PointF (($downCx + $head / 2)), ($bottom + $head * 0.15))
        ))
        $upPen.Dispose(); $downPen.Dispose()
    }

    $upBrush.Dispose(); $downBrush.Dispose(); $pen.Dispose(); $brush.Dispose()
    $upBar.Dispose(); $downBar.Dispose(); $plate.Dispose()
    $g.Dispose()
    $bmp
}

# PNGs for the README and for the runtime icon (tray + window).
foreach ($export in @(@(64, 'icon-64.png'), @(256, 'icon-256.png'))) {
    $bmp = New-IconBitmap -Size $export[0]
    $bmp.Save((Join-Path $AssetsDir $export[1]), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Host "wrote assets/$($export[1])"
}

# Multi-size ICO. Entries are PNG-compressed, which Windows has understood
# since Vista and which keeps the file tiny.
$sizes = @(16, 24, 32, 48, 64, 128, 256)
$pngs = @()
foreach ($size in $sizes) {
    $bmp = New-IconBitmap -Size $size
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs += , @{ Size = $size; Bytes = $ms.ToArray() }
    $ms.Dispose(); $bmp.Dispose()
}

$icoPath = Join-Path $AssetsDir 'icon.ico'
$fs = [System.IO.File]::Create($icoPath)
$bw = New-Object System.IO.BinaryWriter $fs
$bw.Write([uint16]0)                 # reserved
$bw.Write([uint16]1)                 # type: icon
$bw.Write([uint16]$pngs.Count)
$offset = 6 + 16 * $pngs.Count
foreach ($entry in $pngs) {
    $dim = if ($entry.Size -ge 256) { 0 } else { $entry.Size }
    $bw.Write([byte]$dim)            # width, 0 means 256
    $bw.Write([byte]$dim)            # height
    $bw.Write([byte]0)               # palette size
    $bw.Write([byte]0)               # reserved
    $bw.Write([uint16]1)             # colour planes
    $bw.Write([uint16]32)            # bits per pixel
    $bw.Write([uint32]$entry.Bytes.Length)
    $bw.Write([uint32]$offset)
    $offset += $entry.Bytes.Length
}
foreach ($entry in $pngs) { $bw.Write($entry.Bytes) }
$bw.Flush(); $bw.Dispose(); $fs.Dispose()

$ico = Get-Item $icoPath
Write-Host ("wrote assets/icon.ico ({0} sizes, {1:N0} bytes)" -f $pngs.Count, $ico.Length)
