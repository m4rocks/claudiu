# Generates assets/claudiu.png and assets/claudiu.ico (the Clawd-style mark on the app's black).
# Windows PowerShell + System.Drawing only. Re-run after changing the pattern or colors.
Add-Type -AssemblyName System.Drawing
$root = Split-Path $PSScriptRoot
$assets = Join-Path $root "assets"
New-Item -ItemType Directory -Force $assets | Out-Null

$pattern = @(
  "..#######..",
  "..#o###o#..",
  "###o###o###",
  "###########",
  "..#######..",
  "..#.#.#.#..",
  "..#.#.#.#.."
)
$orange = [System.Drawing.Color]::FromArgb(255, 0xd9, 0x77, 0x57)
$eye = [System.Drawing.Color]::FromArgb(255, 0x1a, 0x0f, 0x0b)
$bg = [System.Drawing.Color]::FromArgb(255, 0x0c, 0x0c, 0x0d)

function New-Icon([int]$size) {
  $bmp = New-Object System.Drawing.Bitmap $size, $size
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'AntiAlias'
  $g.Clear([System.Drawing.Color]::Transparent)
  # rounded black tile
  $r = [int]($size * 0.22)
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $path.AddArc(0, 0, 2 * $r, 2 * $r, 180, 90)
  $path.AddArc($size - 2 * $r - 1, 0, 2 * $r, 2 * $r, 270, 90)
  $path.AddArc($size - 2 * $r - 1, $size - 2 * $r - 1, 2 * $r, 2 * $r, 0, 90)
  $path.AddArc(0, $size - 2 * $r - 1, 2 * $r, 2 * $r, 90, 90)
  $path.CloseFigure()
  $g.FillPath((New-Object System.Drawing.SolidBrush $bg), $path)
  # pixel mark, pixel-aligned (no smoothing so edges stay crisp)
  $g.SmoothingMode = 'None'
  $g.PixelOffsetMode = 'Half'
  $unit = [Math]::Max(1, [Math]::Floor($size * 0.70 / 11))
  $w = $unit * 11; $h = $unit * 7
  $ox = [int](($size - $w) / 2); $oy = [int](($size - $h) / 2) - [int]($unit * 0.2)
  for ($y = 0; $y -lt 7; $y++) {
    for ($x = 0; $x -lt 11; $x++) {
      $c = $pattern[$y][$x]
      if ($c -eq '.') { continue }
      $brush = New-Object System.Drawing.SolidBrush ($(if ($c -eq 'o') { $eye } else { $orange }))
      $g.FillRectangle($brush, $ox + $x * $unit, $oy + $y * $unit, $unit, $unit)
    }
  }
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
  if ($s -eq 256) { $bmp.Save((Join-Path $assets "claudiu.png"), [System.Drawing.Imaging.ImageFormat]::Png) }
  $bmp.Dispose()
}

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
[System.IO.File]::WriteAllBytes((Join-Path $assets "claudiu.ico"), $out.ToArray())
Write-Output "Wrote assets/claudiu.ico and assets/claudiu.png"
