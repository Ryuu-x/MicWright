# Generates ui-winui/Assets/AppIcon.ico: a hand-drawn microphone on an accent tile.
# Drawn from shapes, not a font, so the icon carries no third-party licence.
# PNG-compressed ICO entries (supported since Windows Vista).
param([string]$Out = "$PSScriptRoot\..\ui-winui\Assets\AppIcon.ico")

Add-Type -AssemblyName System.Drawing
$sizes = 16, 24, 32, 48, 64, 128, 256
$pngs = foreach ($s in $sizes) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $r = [Math]::Max(2, $s * 0.22)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $path.AddArc(0, 0, $d, $d, 180, 90); $path.AddArc($s - $d - 1, 0, $d, $d, 270, 90)
    $path.AddArc($s - $d - 1, $s - $d - 1, $d, $d, 0, 90); $path.AddArc(0, $s - $d - 1, $d, $d, 90, 90)
    $path.CloseFigure()
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.Point 0, 0), (New-Object System.Drawing.Point $s, $s),
        [System.Drawing.Color]::FromArgb(255, 139, 124, 246), [System.Drawing.Color]::FromArgb(255, 93, 79, 196))
    $g.FillPath($brush, $path)
    $c = $s / 2
    # Capsule
    $w = $s * 0.26; $top = $s * 0.17; $h = $s * 0.42
    $body = New-Object System.Drawing.Drawing2D.GraphicsPath
    $body.AddArc([float]($c - $w / 2), [float]$top, [float]$w, [float]$w, 180, 180)
    $body.AddArc([float]($c - $w / 2), [float]($top + $h - $w), [float]$w, [float]$w, 0, 180)
    $body.CloseFigure()
    $g.FillPath([System.Drawing.Brushes]::White, $body)
    # Cradle, stem and base
    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::White), ([float][Math]::Max(1.2, $s * 0.075))
    $pen.StartCap = 'Round'; $pen.EndCap = 'Round'
    $r2 = $s * 0.23
    $g.DrawArc($pen, [float]($c - $r2), [float]($s * 0.47 - $r2), [float]($r2 * 2), [float]($r2 * 2), 0, 180)
    $g.DrawLine($pen, [float]$c, [float]($s * 0.70), [float]$c, [float]($s * 0.82))
    $g.DrawLine($pen, [float]($c - $s * 0.13), [float]($s * 0.82), [float]($c + $s * 0.13), [float]($s * 0.82))
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    , $ms.ToArray()
}

New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
$fs = [System.IO.File]::Create($Out)
$w = New-Object System.IO.BinaryWriter $fs
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]
    $w.Write([byte]($s % 256)); $w.Write([byte]($s % 256)); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$pngs[$i].Length); $w.Write([uint32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $w.Write($p) }
$w.Close()
"Wrote $Out ($((Get-Item $Out).Length) bytes)"
