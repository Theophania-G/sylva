# Generate MSIX / Microsoft Store PNG assets:
# extract the largest frame from assets/sylva.ico, scale to store sizes,
# and sample a theme background color.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\make-store-assets.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$icoPath = Join-Path $root 'assets\sylva.ico'
$outDir = Join-Path $root 'packaging\msix\assets'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

Add-Type -AssemblyName System.Drawing

# ---- 1) Extract the largest frame (PNG or BMP) ----
$bytes = [System.IO.File]::ReadAllBytes($icoPath)
$count = [BitConverter]::ToUInt16($bytes, 4)
$bestDim = 0; $bestOffset = 0; $bestSize = 0
for ($i = 0; $i -lt $count; $i++) {
    $e = 6 + $i * 16
    $w = $bytes[$e];   if ($w -eq 0) { $w = 256 }
    $h = $bytes[$e+1]; if ($h -eq 0) { $h = 256 }
    $dim = [Math]::Max($w, $h)
    if ($dim -gt $bestDim) {
        $bestDim = $dim
        $bestSize = [BitConverter]::ToUInt32($bytes, $e + 8)
        $bestOffset = [BitConverter]::ToUInt32($bytes, $e + 12)
    }
}
$frame = New-Object byte[] $bestSize
[Array]::Copy($bytes, $bestOffset, $frame, 0, $bestSize)
$isPng = ($frame[0] -eq 0x89) -and ($frame[1] -eq 0x50)
if ($isPng) {
    $src = New-Object System.Drawing.Bitmap (New-Object System.IO.MemoryStream (,$frame))
} else {
    $full = New-Object System.IO.MemoryStream (,$bytes)
    $icon = New-Object System.Drawing.Icon ($full, 256, 256)
    $src = $icon.ToBitmap()
    $full.Dispose(); $icon.Dispose()
}

# ---- 2) Sample background color (average of opaque pixels, #RRGGBB) ----
$r = 0.0; $g = 0.0; $b = 0.0; $n = 0
for ($y = 0; $y -lt $src.Height; $y += 4) {
    for ($x = 0; $x -lt $src.Width; $x += 4) {
        $p = $src.GetPixel($x, $y)
        if ($p.A -ge 128) {
            $r += $p.R; $g += $p.G; $b += $p.B; $n++
        }
    }
}
if ($n -eq 0) { $bg = '#23262E' } else {
    $br = [int]([Math]::Round($r / $n)); $bgG = [int]([Math]::Round($g / $n)); $bgB = [int]([Math]::Round($b / $n))
    $bg = '#{0:X2}{1:X2}{2:X2}' -f $br, $bgG, $bgB
}
[System.IO.File]::WriteAllText((Join-Path $outDir 'background.txt'), $bg)

# ---- 3) Generate store sizes ----
function New-Logo([string]$path, [int]$w, [int]$h, [double]$glyphFrac, [bool]$bgFill, [int]$xOff = -1, [int]$yOff = -1) {
    $bmp = New-Object System.Drawing.Bitmap ($w, $h, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    if ($bgFill) {
        $g.Clear($bg)
    }
    $glyph = [int]([Math]::Floor([Math]::Min($w, $h) * $glyphFrac))
    $x = if ($xOff -ge 0) { $xOff } else { [int]([Math]::Floor(($w - $glyph) / 2.0)) }
    $y = if ($yOff -ge 0) { $yOff } else { [int]([Math]::Floor(($h - $glyph) / 2.0)) }
    $g.DrawImage($src, $x, $y, $glyph, $glyph)
    $g.Dispose()
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

New-Logo (Join-Path $outDir 'StoreLogo.png')         50  50  0.78  $false
New-Logo (Join-Path $outDir 'Square44x44Logo.png')   44  44  0.64  $true
New-Logo (Join-Path $outDir 'Square71x71Logo.png')   71  71  0.68  $true
New-Logo (Join-Path $outDir 'Square150x150Logo.png') 150 150 0.67  $true
New-Logo (Join-Path $outDir 'Square310x310Logo.png') 310 310 0.68  $true
New-Logo (Join-Path $outDir 'Wide310x150Logo.png')   310 150 0.80  $true  15  15
New-Logo (Join-Path $outDir 'SplashScreen.png')      620 300 0.47  $true

$src.Dispose()
Write-Host "==> assets generated: $outDir"
Write-Host "==> background color: $bg"
