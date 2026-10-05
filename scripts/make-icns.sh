#!/bin/sh
# macOS only: build assets/claudiu.icns from assets/claudiu.png (sips + iconutil ship with macOS).
set -e
set="$(mktemp -d)/claudiu.iconset"
mkdir -p "$set"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/claudiu.png --out "$set/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) assets/claudiu.png --out "$set/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$set" -o "${1:-dist/claudiu.icns}"
