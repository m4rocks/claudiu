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
Write-Host "Packing m4code $Version"

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$publish = Join-Path $root "dist\publish"
Remove-Item $publish -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $publish | Out-Null
Copy-Item target\release\m4code.exe $publish

vpk pack `
  --packId m4code `
  --packVersion $Version `
  --packDir $publish `
  --mainExe m4code.exe `
  --packTitle "m4code" `
  --packAuthors "m4code" `
  --icon assets\m4code.ico `
  --outputDir dist\releases
if ($LASTEXITCODE -ne 0) { throw "vpk pack failed" }

Write-Host "Done. Installer: dist\releases\m4code-win-Setup.exe"
