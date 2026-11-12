#!/bin/bash
# Renders the Avery artwork (macOS only). Needs a Chromium-based browser (Microsoft Edge, Google Chrome,
# Chromium or Brave; or set AVERY_BROWSER). Sources are assets/avery-app-icon.svg, assets/avery-icon.svg (both from tools/gen-logo.js) and assets/dmg-background.html.
#
# Writes:
#   share/avery/Avery.icns         app icon for macOS 26+ (full-bleed square, the system applies its own mask)
#   share/avery/AveryClassic.icns  app icon for older macOS and for the DMG volume (squircle with margin)
#   assets/logo.png                512 px icon for the README
#   assets/dmg/background.tiff    Retina DMG window background (640 x 448 pt, 1x + 2x)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
[ "$(uname -s)" = "Darwin" ] || { echo "render-assets: nur auf macOS" >&2; exit 1; }

browser="${AVERY_BROWSER:-}"
if [ -z "$browser" ]; then
  for candidate in \
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge" \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium" \
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"; do
    if [ -x "$candidate" ]; then browser="$candidate"; break; fi
  done
fi
[ -x "$browser" ] || { echo "render-assets: kein Chromium-Browser gefunden (AVERY_BROWSER setzen)" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# render URL OUT.png WIDTH HEIGHT SCALE
render() {
  "$browser" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor="$5" \
    --default-background-color=00000000 --window-size="$3,$4" --screenshot="$2" "$1" >/dev/null 2>&1 || true
  local got_w got_h
  got_w="$(sips -g pixelWidth "$2" 2>/dev/null | awk '/pixelWidth/ {print $2}')"
  got_h="$(sips -g pixelHeight "$2" 2>/dev/null | awk '/pixelHeight/ {print $2}')"
  if [ "$got_w" != "$(( $3 * $5 ))" ] || [ "$got_h" != "$(( $4 * $5 ))" ]; then
    echo "render-assets: $2 hat ${got_w:-?}x${got_h:-?} statt $(( $3 * $5 ))x$(( $4 * $5 ))" >&2
    exit 1
  fi
}

# Every icon size is rendered from the vector source, not scaled down from one big bitmap.
icon_png() {
  local svg="$1" size="$2" out="$3"
  cat > "$tmp/icon-$size.html" <<HTML
<!doctype html><html><head><style>html,body{margin:0;background:transparent}img{display:block}</style></head>
<body><img src="file://$svg" width="$size" height="$size"></body></html>
HTML
  render "file://$tmp/icon-$size.html" "$out" "$size" "$size" 1
}

# build_icns SVG OUT.icns
build_icns() {
  local svg="$1" out="$2" work size
  work="$(mktemp -d "$tmp/iconset.XXXXXX")"
  mkdir "$work/Avery.iconset"
  for size in 16 32 64 128 256 512 1024; do icon_png "$svg" "$size" "$work/$size.png"; done
  cp "$work/16.png"   "$work/Avery.iconset/icon_16x16.png"
  cp "$work/32.png"   "$work/Avery.iconset/icon_16x16@2x.png"
  cp "$work/32.png"   "$work/Avery.iconset/icon_32x32.png"
  cp "$work/64.png"   "$work/Avery.iconset/icon_32x32@2x.png"
  cp "$work/128.png"  "$work/Avery.iconset/icon_128x128.png"
  cp "$work/256.png"  "$work/Avery.iconset/icon_128x128@2x.png"
  cp "$work/256.png"  "$work/Avery.iconset/icon_256x256.png"
  cp "$work/512.png"  "$work/Avery.iconset/icon_256x256@2x.png"
  cp "$work/512.png"  "$work/Avery.iconset/icon_512x512.png"
  cp "$work/1024.png" "$work/Avery.iconset/icon_512x512@2x.png"
  iconutil -c icns "$work/Avery.iconset" -o "$out"
  LAST_512="$work/512.png"
}

build_icns "$root/assets/avery-app-icon.svg" "$root/share/avery/Avery.icns"
build_icns "$root/assets/avery-icon.svg" "$root/share/avery/AveryClassic.icns"
cp "$LAST_512" "$root/assets/logo.png"

mkdir -p "$root/assets/dmg"
render "file://$root/assets/dmg-background.html" "$tmp/bg.png" 640 448 1
render "file://$root/assets/dmg-background.html" "$tmp/bg@2x.png" 640 448 2
sips -s dpiWidth 72 -s dpiHeight 72 "$tmp/bg.png" >/dev/null
sips -s dpiWidth 144 -s dpiHeight 144 "$tmp/bg@2x.png" >/dev/null
tiffutil -cathidpicheck "$tmp/bg.png" "$tmp/bg@2x.png" -out "$root/assets/dmg/background.tiff" >/dev/null 2>&1

echo "geschrieben:"
echo "  share/avery/Avery.icns"
echo "  share/avery/AveryClassic.icns"
echo "  assets/logo.png"
echo "  assets/dmg/background.tiff"
