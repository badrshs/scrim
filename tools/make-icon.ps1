<#
.SYNOPSIS
    Draw Scrim's application icon.

.DESCRIPTION
    The icon is the same mark the title bar uses: a frame with a sheet drawn
    across it. A scrim is the gauze stretched over a stage opening that hides
    what is behind it until you light it, which is exactly what the app does to
    a frame of video.

    Generated rather than checked in as a binary blob, so the shape can be
    changed by editing code and re-running this.

.EXAMPLE
    powershell -File tools/make-icon.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$repo = Split-Path -Parent $PSScriptRoot
$outDir = Join-Path $repo "src-tauri\icons"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# Straight from the design tokens.
$INK = [System.Drawing.Color]::FromArgb(255, 10, 12, 14)    # --app  #0A0C0E
$TEAL = [System.Drawing.Color]::FromArgb(255, 79, 227, 193) # --accent #4FE3C1

function New-RoundedPath {
    param([float]$x, [float]$y, [float]$w, [float]$h, [float]$r)
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function New-IconBitmap {
    param([int]$size)

    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.Clear([System.Drawing.Color]::Transparent)

    $s = [float]$size

    # Dark rounded tile, so the mark reads on a light or a dark taskbar.
    $tile = New-RoundedPath 0 0 $s $s ($s * 0.22)
    $brush = New-Object System.Drawing.SolidBrush($INK)
    $g.FillPath($brush, $tile)

    # The frame.
    $inset = $s * 0.26
    $fw = $s - ($inset * 2)
    $stroke = [Math]::Max(1.0, $s * 0.075)
    $frame = New-RoundedPath $inset $inset $fw $fw ($s * 0.055)
    $pen = New-Object System.Drawing.Pen($TEAL, $stroke)
    $pen.Alignment = [System.Drawing.Drawing2D.PenAlignment]::Center
    $g.DrawPath($pen, $frame)

    # The sheet: a bar across the frame at the design's -38 degrees, clipped so
    # it stops at the frame's inner edge.
    $clip = New-RoundedPath ($inset + $stroke / 2) ($inset + $stroke / 2) `
        ($fw - $stroke) ($fw - $stroke) ($s * 0.04)
    $g.SetClip($clip)
    $state = $g.Save()
    $g.TranslateTransform($s / 2, $s / 2)
    $g.RotateTransform(-38)
    $barH = [Math]::Max(1.0, $s * 0.15)
    $g.FillRectangle((New-Object System.Drawing.SolidBrush($TEAL)), - $s, - $barH / 2, $s * 2, $barH)
    $g.Restore($state)
    $g.ResetClip()

    $g.Dispose()
    return $bmp
}

function Save-Png {
    param($bitmap, [string]$path)
    $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
}

# --- PNGs Tauri's bundler wants -------------------------------------------
$pngSizes = @{ "32x32.png" = 32; "128x128.png" = 128; "128x128@2x.png" = 256; "icon.png" = 512 }
foreach ($name in $pngSizes.Keys) {
    $bmp = New-IconBitmap $pngSizes[$name]
    Save-Png $bmp (Join-Path $outDir $name)
    $bmp.Dispose()
}

# --- multi-resolution .ico -------------------------------------------------
# Built by hand because System.Drawing cannot write a multi-image icon.
# Every frame is stored as PNG, which Windows Vista and later read natively.
$icoSizes = @(16, 24, 32, 48, 64, 128, 256)
$frames = @()
foreach ($sz in $icoSizes) {
    $bmp = New-IconBitmap $sz
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $frames += , @{ size = $sz; bytes = $ms.ToArray() }
    $ms.Dispose()
    $bmp.Dispose()
}

$icoPath = Join-Path $outDir "icon.ico"
$fs = [System.IO.File]::Create($icoPath)
$bw = New-Object System.IO.BinaryWriter($fs)

$bw.Write([UInt16]0)                 # reserved
$bw.Write([UInt16]1)                 # type: icon
$bw.Write([UInt16]$frames.Count)

# Directory entries come first, so image data starts after all of them.
$offset = 6 + (16 * $frames.Count)
foreach ($f in $frames) {
    $dim = if ($f.size -ge 256) { 0 } else { $f.size }  # 0 encodes 256
    $bw.Write([Byte]$dim)            # width
    $bw.Write([Byte]$dim)            # height
    $bw.Write([Byte]0)               # palette size
    $bw.Write([Byte]0)               # reserved
    $bw.Write([UInt16]1)             # colour planes
    $bw.Write([UInt16]32)            # bits per pixel
    $bw.Write([UInt32]$f.bytes.Length)
    $bw.Write([UInt32]$offset)
    $offset += $f.bytes.Length
}
foreach ($f in $frames) { $bw.Write($f.bytes) }

$bw.Flush(); $bw.Dispose(); $fs.Dispose()

Write-Host ("wrote {0} ({1} sizes, {2:N0} bytes)" -f $icoPath, $frames.Count, (Get-Item $icoPath).Length)
Get-ChildItem $outDir | ForEach-Object { "  {0,-18} {1,8:N0} bytes" -f $_.Name, $_.Length }
