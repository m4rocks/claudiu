# Builds a release binary and packs a Velopack installer + update feed into dist/releases.
#   scripts\package.ps1                # version from Cargo.toml
#   scripts\package.ps1 -Version 0.2.0
# Requires: Rust, the .NET SDK, and the Velopack CLI (`dotnet tool install -g vpk`).
param([string]$Version = "")

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot
Set-Location $root

if (-not $Version) {
  $Version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
}
Write-Host "Packing Claudiu $Version"

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$publish = Join-Path $root "dist\publish"
Remove-Item $publish -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $publish | Out-Null
Copy-Item target\release\claudiu.exe $publish

vpk pack `
  --packId Claudiu `
  --packVersion $Version `
  --packDir $publish `
  --mainExe claudiu.exe `
  --packTitle "Claudiu" `
  --packAuthors "Claudiu" `
  --icon assets\claudiu.ico `
  --outputDir dist\releases
if ($LASTEXITCODE -ne 0) { throw "vpk pack failed" }

Write-Host "Done. Installer: dist\releases\Claudiu-win-Setup.exe"
