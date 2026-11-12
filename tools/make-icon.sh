#!/bin/bash
# Rebuilds share/avery/Avery.icns from tools/make-icon.js (macOS only).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
osascript -l JavaScript "$here/make-icon.js" "$tmp/base.png" >/dev/null
sips -z 1024 1024 "$tmp/base.png" --out "$tmp/1024.png" >/dev/null
iconset="$tmp/Avery.iconset"
mkdir "$iconset"
for spec in 16:icon_16x16 32:icon_16x16@2x 32:icon_32x32 64:icon_32x32@2x 128:icon_128x128 \
            256:icon_128x128@2x 256:icon_256x256 512:icon_256x256@2x 512:icon_512x512 1024:icon_512x512@2x; do
  size="${spec%%:*}"
  name="${spec#*:}"
  sips -z "$size" "$size" "$tmp/1024.png" --out "$iconset/$name.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$here/../share/avery/Avery.icns"
echo "geschrieben: share/avery/Avery.icns"
