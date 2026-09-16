<#
.SYNOPSIS
  Draw windows\pime\likhi\icon.ico: the Likhi mark, a Bengali letter on a rounded square.

  Run after changing anything below; the result is committed, so a normal build never needs this.

    powershell -NoProfile -ExecutionPolicy Bypass -File windows\pime\make_icon.ps1

  PowerShell rather than Python, which is the rest of this repository, for one reason: Bengali needs
  complex-script shaping. In "লি" the vowel sign is typed after the consonant and drawn before it,
  and a rasteriser that walks code points in order produces the wrong picture. TextRenderer hands
  the string to Windows' own text stack, which shapes it correctly, and it needs nothing installed.
  The previous icon avoided the problem by drawing a diagonal stroke and calling it a placeholder.
#>

param(
    [string]$Out = (Join-Path (Split-Path -Parent $PSCommandPath) 'likhi\icon.ico')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

# --------------------------------------------------------------------------------- design
# Deep green, a nod to Bangladesh without reproducing the flag, which turns to mud at 16 pixels.
$TopColor    = [Drawing.Color]::FromArgb(255, 16, 138, 95)
$BottomColor = [Drawing.Color]::FromArgb(255, 9, 95, 66)
$InkColor    = [Drawing.Color]::White
# Noto Sans Bengali if it is installed: its letterforms are more open at small sizes, which is the
# whole problem this icon has. Falls back to the face Windows always has.
$FontFamily  = 'Nirmala UI'
try {
    Add-Type -AssemblyName System.Drawing
    if ((New-Object Drawing.Text.InstalledFontCollection).Families.Name -contains 'Noto Sans Bengali') {
        $FontFamily = 'Noto Sans Bengali'
    }
} catch { }

# At 16 and 20 pixels the matra of "লি" closes up into a blob, so the smallest sizes carry the bare
# consonant. Same letter, same colour: it still reads as one mark across the set.
function Glyph([int]$size) { if ($size -le 24) { return [char]0x09B2 } else { return ([char]0x09B2).ToString() + ([char]0x09BF) } }

$Sizes = 16, 20, 24, 32, 48, 64, 128, 256

# --------------------------------------------------------------------------------- drawing

function New-RoundedPath([Drawing.RectangleF]$r, [float]$radius) {
    $p = New-Object Drawing.Drawing2D.GraphicsPath
    $d = $radius * 2
    $p.AddArc($r.X, $r.Y, $d, $d, 180, 90)
    $p.AddArc($r.Right - $d, $r.Y, $d, $d, 270, 90)
    $p.AddArc($r.Right - $d, $r.Bottom - $d, $d, $d, 0, 90)
    $p.AddArc($r.X, $r.Bottom - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function Render([int]$size) {
    $bmp = New-Object Drawing.Bitmap($size, $size, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([Drawing.Color]::Transparent)

    # A full-bleed tile at 16px; the rounding only becomes visible once there are pixels to spare.
    $inset = [Math]::Max(0.0, $size * 0.01)
    $rect = New-Object Drawing.RectangleF($inset, $inset, ($size - 2 * $inset), ($size - 2 * $inset))
    $radius = [Math]::Max(2.0, $size * 0.22)
    $path = New-RoundedPath $rect $radius
    $brush = New-Object Drawing.Drawing2D.LinearGradientBrush(
        (New-Object Drawing.PointF(0, 0)), (New-Object Drawing.PointF(0, $size)), $TopColor, $BottomColor)
    $g.FillPath($brush, $path)
    $brush.Dispose(); $path.Dispose()

    # Fit the glyph by its ink, not by its text box.
    #
    # A measured text box carries the font's full line height -- ascender and descender space that
    # a Bangla letter without them simply does not use. Fitting to that box leaves the letter
    # floating in a third of the tile, which is exactly why this icon read as small beside "ENG" in
    # the keyboard picker. So the glyph is rendered once on a large canvas, its actual inked bounds
    # are found, and it is then scaled and placed so that ink fills the tile.
    $text = Glyph $size
    $pad = if ($size -le 24) { $size * 0.08 } else { $size * 0.12 }
    $boxW = $size - 2 * $pad
    $boxH = $size - 2 * $pad

    $probeSize = 256
    $probe = New-Object Drawing.Bitmap($probeSize, $probeSize, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $pg = [Drawing.Graphics]::FromImage($probe)
    $pg.Clear([Drawing.Color]::Transparent)
    $pg.TextRenderingHint = [Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $probeFont = New-Object Drawing.Font($FontFamily, ($probeSize * 0.5), [Drawing.FontStyle]::Bold, [Drawing.GraphicsUnit]::Pixel)
    $flags = [Windows.Forms.TextFormatFlags]::NoPadding -bor [Windows.Forms.TextFormatFlags]::SingleLine
    [Windows.Forms.TextRenderer]::DrawText($pg, $text, $probeFont, (New-Object Drawing.Point(20, 20)), [Drawing.Color]::White, $flags)
    $pg.Dispose()

    $minX = $probeSize; $minY = $probeSize; $maxX = -1; $maxY = -1
    $data = $probe.LockBits((New-Object Drawing.Rectangle(0, 0, $probeSize, $probeSize)),
        [Drawing.Imaging.ImageLockMode]::ReadOnly, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $row = New-Object byte[] ($probeSize * 4)
        for ($yy = 0; $yy -lt $probeSize; $yy++) {
            [Runtime.InteropServices.Marshal]::Copy([IntPtr]($data.Scan0.ToInt64() + $yy * $data.Stride), $row, 0, $row.Length)
            for ($xx = 0; $xx -lt $probeSize; $xx++) {
                if ($row[$xx * 4 + 3] -gt 24) {
                    if ($xx -lt $minX) { $minX = $xx }
                    if ($xx -gt $maxX) { $maxX = $xx }
                    if ($yy -lt $minY) { $minY = $yy }
                    if ($yy -gt $maxY) { $maxY = $yy }
                }
            }
        }
    } finally { $probe.UnlockBits($data) }

    if ($maxX -ge $minX -and $maxY -ge $minY) {
        $inkW = $maxX - $minX + 1
        $inkH = $maxY - $minY + 1
        $scale = [Math]::Min($boxW / $inkW, $boxH / $inkH)
        $drawPx = ($probeSize * 0.5) * $scale
        $f = New-Object Drawing.Font($FontFamily, $drawPx, [Drawing.FontStyle]::Bold, [Drawing.GraphicsUnit]::Pixel)
        # Where the ink sat relative to the draw origin, scaled to this size.
        $offX = (20 - $minX) * $scale
        $offY = (20 - $minY) * $scale
        $x = ($size - $inkW * $scale) / 2.0 + $offX
        $y = ($size - $inkH * $scale) / 2.0 + $offY
        $g.TextRenderingHint = [Drawing.Text.TextRenderingHint]::AntiAliasGridFit
        [Windows.Forms.TextRenderer]::DrawText($g, $text, $f, (New-Object Drawing.Point([int][Math]::Round($x), [int][Math]::Round($y))), $InkColor, $flags)
        $f.Dispose()
    }
    $probeFont.Dispose(); $probe.Dispose()
    $g.Dispose()
    return $bmp
}

# --------------------------------------------------------------------------------- .ico container

function Get-PngBytes([Drawing.Bitmap]$bmp) {
    $ms = New-Object IO.MemoryStream
    $bmp.Save($ms, [Drawing.Imaging.ImageFormat]::Png)
    $bytes = $ms.ToArray(); $ms.Dispose()
    # Comma operator: a bare `return $bytes` streams the array one byte at a time, and the caller
    # gets an Object[] that BinaryWriter then writes as a single element.
    return , $bytes
}

function Get-DibBytes([Drawing.Bitmap]$bmp) {
    # BITMAPINFOHEADER with doubled height, bottom-up BGRA rows, then the AND mask. The mask is
    # unused because the alpha channel carries transparency, but the format still requires it.
    $w = $bmp.Width; $h = $bmp.Height
    $ms = New-Object IO.MemoryStream
    $bw = New-Object IO.BinaryWriter($ms)
    # Mask rows are padded to a 4-byte boundary. Floor, not -as [int]: PowerShell's int cast rounds,
    # and for a 32 pixel icon that produced 8 mask bytes per row where the format wants 4, which is
    # enough to make the whole file unreadable.
    $maskRow = [int][Math]::Floor(($w + 31) / 32) * 4
    $bw.Write([int]40); $bw.Write([int]$w); $bw.Write([int]($h * 2))
    $bw.Write([int16]1); $bw.Write([int16]32); $bw.Write([int]0)
    $bw.Write([int]($w * $h * 4 + $maskRow * $h))
    $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0)
    $data = $bmp.LockBits((New-Object Drawing.Rectangle(0, 0, $w, $h)),
        [Drawing.Imaging.ImageLockMode]::ReadOnly, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $row = New-Object byte[] ($w * 4)
        for ($y = $h - 1; $y -ge 0; $y--) {
            [Runtime.InteropServices.Marshal]::Copy([IntPtr]($data.Scan0.ToInt64() + $y * $data.Stride), $row, 0, $row.Length)
            $bw.Write($row)
        }
    } finally { $bmp.UnlockBits($data) }
    $bw.Write((New-Object byte[] ($maskRow * $h)))
    $bw.Flush(); $bytes = $ms.ToArray(); $bw.Dispose()
    return , $bytes
}

$entries = @()
foreach ($s in $Sizes) {
    $bmp = Render $s
    # PNG compression above 48px; a 256x256 uncompressed frame alone would be 256 KB.
    $payload = if ($s -ge 64) { Get-PngBytes $bmp } else { Get-DibBytes $bmp }
    $entries += [pscustomobject]@{ Size = $s; Data = $payload }
    $bmp.Dispose()
}

$ms = New-Object IO.MemoryStream
$bw = New-Object IO.BinaryWriter($ms)
$bw.Write([int16]0); $bw.Write([int16]1); $bw.Write([int16]$entries.Count)
$offset = 6 + 16 * $entries.Count
foreach ($e in $entries) {
    # 0 in the width/height byte means 256.
    $bw.Write([byte]($e.Size % 256)); $bw.Write([byte]($e.Size % 256))
    $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([int16]1); $bw.Write([int16]32)
    $bw.Write([int]$e.Data.Length); $bw.Write([int]$offset)
    $offset += $e.Data.Length
}
foreach ($e in $entries) { $bw.Write([byte[]]$e.Data) }
$bw.Flush()
[IO.File]::WriteAllBytes($Out, $ms.ToArray())
$bw.Dispose()

Write-Host "wrote $Out ($((Get-Item $Out).Length) bytes, sizes: $($Sizes -join ', '))"
