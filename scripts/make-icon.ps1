# Generates assets/m4code.png (512x512) and assets/m4code.ico from the `<m4/>` mark.
# Windows PowerShell + System.Drawing only. Mirrors web/public/m4code.svg.
Add-Type -AssemblyName System.Drawing
$root = Split-Path $PSScriptRoot
$assets = Join-Path $root "assets"
New-Item -ItemType Directory -Force $assets | Out-Null

$fg = [System.Drawing.Color]::FromArgb(255, 0xff, 0xff, 0xff)
$bg = [System.Drawing.Color]::FromArgb(255, 0x12, 0x12, 0x12)

function New-Icon([int]$size) {
  $bmp = New-Object System.Drawing.Bitmap $size, $size
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'AntiAlias'
  $g.Clear([System.Drawing.Color]::Transparent)
  $scale = $size / 512.0
  # rounded black tile
  $r = [float](112.0 * $scale)
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $path.AddArc(0, 0, 2 * $r, 2 * $r, 180, 90)
  $path.AddArc($size - 2 * $r, 0, 2 * $r, 2 * $r, 270, 90)
  $path.AddArc($size - 2 * $r, $size - 2 * $r, 2 * $r, 2 * $r, 0, 90)
  $path.AddArc(0, $size - 2 * $r, 2 * $r, 2 * $r, 90, 90)
  $path.CloseFigure()
  $g.FillPath((New-Object System.Drawing.SolidBrush $bg), $path)
  # `<m4/>` strokes in 512x512 coordinates
  $g.ScaleTransform($scale, $scale)
  $pen = New-Object System.Drawing.Pen $fg, 24.0
  $pen.StartCap = 'Round'; $pen.EndCap = 'Round'; $pen.LineJoin = 'Round'
  $g.DrawLines($pen, [System.Drawing.PointF[]]@([System.Drawing.PointF]::new(94, 206), [System.Drawing.PointF]::new(48, 256), [System.Drawing.PointF]::new(94, 306)))
  $g.DrawLine($pen, 122, 306, 122, 214)
  $m1 = New-Object System.Drawing.Drawing2D.GraphicsPath
  $m1.AddBezier(122, 246, 122, 222, 135, 214, 148, 214)
  $m1.AddBezier(148, 214, 164, 214, 174, 226, 174, 246)
  $m1.AddLine(174, 246, 174, 306)
  $g.DrawPath($pen, $m1)
  $m2 = New-Object System.Drawing.Drawing2D.GraphicsPath
  $m2.AddBezier(174, 246, 174, 222, 187, 214, 200, 214)
  $m2.AddBezier(200, 214, 216, 214, 226, 226, 226, 246)
  $m2.AddLine(226, 246, 226, 306)
  $g.DrawPath($pen, $m2)
  $g.DrawLines($pen, [System.Drawing.PointF[]]@([System.Drawing.PointF]::new(308, 306), [System.Drawing.PointF]::new(308, 198), [System.Drawing.PointF]::new(254, 274), [System.Drawing.PointF]::new(324, 274)))
  $g.DrawLine($pen, 380, 198, 352, 314)
  $g.DrawLines($pen, [System.Drawing.PointF[]]@([System.Drawing.PointF]::new(418, 206), [System.Drawing.PointF]::new(464, 256), [System.Drawing.PointF]::new(418, 306)))
  $g.Dispose()
  return $bmp
}

$sizes = 16, 32, 48, 64, 128, 256
$pngs = @()
foreach ($s in $sizes) {
  $bmp = New-Icon $s
  $ms = New-Object System.IO.MemoryStream
  $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  $pngs += , $ms.ToArray()
  $bmp.Dispose()
}
$big = New-Icon 512
$big.Save((Join-Path $assets "m4code.png"), [System.Drawing.Imaging.ImageFormat]::Png)
$big.Dispose()

# ICO container with PNG-compressed images
$out = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter $out
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
  $s = $sizes[$i]
  $bw.Write([byte]($(if ($s -ge 256) { 0 } else { $s })))
  $bw.Write([byte]($(if ($s -ge 256) { 0 } else { $s })))
  $bw.Write([byte]0); $bw.Write([byte]0)
  $bw.Write([uint16]1); $bw.Write([uint16]32)
  $bw.Write([uint32]$pngs[$i].Length); $bw.Write([uint32]$offset)
  $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $bw.Write($p) }
[System.IO.File]::WriteAllBytes((Join-Path $assets "m4code.ico"), $out.ToArray())
Write-Output "Wrote assets/m4code.ico and assets/m4code.png"
