#!/bin/bash
# Builds dist/Avery-<version>.dmg (macOS only, needs a logged-in GUI session because Finder lays out the window).
#
#   tools/make-dmg.sh [OUTPUT.dmg]
#
# The disk image holds Avery.app, a shortcut to /Applications, the window background and a volume icon.
# The app is signed ad hoc: there is no Developer ID. Homebrew's cask installs the app from this image.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
die() { printf 'make-dmg: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "nur auf macOS"
for tool in hdiutil osascript SetFile; do command -v "$tool" >/dev/null 2>&1 || die "$tool fehlt"; done

version="$(sed -n 's/^AVERY_VERSION="\(.*\)"$/\1/p' "$root/bin/avery")"
[ -n "$version" ] || die "Version in bin/avery nicht gefunden"
volume="Avery"
out="${1:-$root/dist/Avery-$version.dmg}"
background="$root/assets/dmg/background.tiff"
[ -f "$background" ] || die "$background fehlt (tools/render-assets.sh)"
[ -f "$root/share/avery/Avery.icns" ] && [ -f "$root/share/avery/AveryClassic.icns" ] || die "Icons fehlen (tools/render-assets.sh)"
[ ! -e "/Volumes/$volume" ] || die "/Volumes/$volume ist schon eingehaengt. Bitte auswerfen und erneut starten."

# Window 640 x 448 pt (title bar included); the background image has the same size.
win_left=200 win_top=120 win_width=640 win_height=448
app_x=170 app_y=222 apps_x=470 apps_y=222

tmp="$(mktemp -d)"
mount=""
cleanup() {
  if [ -n "$mount" ] && [ -d "$mount" ]; then hdiutil detach "$mount" -force >/dev/null 2>&1 || true; fi
  rm -rf "$tmp"
}
trap cleanup EXIT

stage="$tmp/stage"
mkdir -p "$stage/.background"
"$root/libexec/avery-make-app" --output "$stage/Avery.app" --command /opt/homebrew/bin/avery \
  --version "$version" --icon "$root/share/avery/Avery.icns" >/dev/null
cp "$background" "$stage/.background/background.tiff"
ln -s /Applications "$stage/Applications"

echo "Disk-Image anlegen ..."
hdiutil create -srcfolder "$stage" -volname "$volume" -fs HFS+ -format UDRW -size 24m -ov "$tmp/rw.dmg" >/dev/null
hdiutil attach "$tmp/rw.dmg" -readwrite -noverify -noautoopen >/dev/null
mount="/Volumes/$volume"
[ -d "$mount" ] || die "Das Image wurde nicht unter $mount eingehaengt."
mdutil -i off "$mount" >/dev/null 2>&1 || true
sleep 2

echo "Fenster mit Finder gestalten ..."
osascript <<APPLESCRIPT >/dev/null
tell application "Finder"
	tell disk "$volume"
		open
		delay 1
		set theWindow to container window
		set current view of theWindow to icon view
		set toolbar visible of theWindow to false
		set statusbar visible of theWindow to false
		set the bounds of theWindow to {$win_left, $win_top, $((win_left + win_width)), $((win_top + win_height))}
		set theOptions to the icon view options of theWindow
		set arrangement of theOptions to not arranged
		set icon size of theOptions to 128
		set text size of theOptions to 13
		set label position of theOptions to bottom
		set background picture of theOptions to file ".background:background.tiff"
		set position of item "Avery.app" of theWindow to {$app_x, $app_y}
		set position of item "Applications" of theWindow to {$apps_x, $apps_y}
		update without registering applications
		delay 2
		close
		delay 1
		open
		delay 2
		close
	end tell
end tell
APPLESCRIPT

waited=0
while [ ! -f "$mount/.DS_Store" ]; do
  sleep 1
  waited=$((waited + 1))
  [ "$waited" -lt 30 ] || die "Finder hat keine .DS_Store geschrieben."
done
sleep 2

# Finder's "update" removes a .VolumeIcon.icns that is already there, so the volume icon is added last.
cp "$root/share/avery/AveryClassic.icns" "$mount/.VolumeIcon.icns"
SetFile -a C "$mount"
rm -rf "$mount/.Spotlight-V100" "$mount/.Trashes" "$mount/.fseventsd"
chmod -Rf go-w "$mount" || true
sync

for attempt in 1 2 3 4 5 6; do
  if hdiutil detach "$mount" >/dev/null 2>&1; then mount=""; break; fi
  sleep 2
done
if [ -n "$mount" ]; then hdiutil detach "$mount" -force >/dev/null; mount=""; fi

echo "Komprimieren ..."
mkdir -p "$(dirname "$out")"
rm -f "$out"
hdiutil convert "$tmp/rw.dmg" -format UDZO -imagekey zlib-level=9 -o "$out" >/dev/null

echo "Fertig: $out"
printf '  Groesse: %s\n  sha256:  %s\n' "$(du -h "$out" | awk '{print $1}')" "$(shasum -a 256 "$out" | awk '{print $1}')"
