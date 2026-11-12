#!/bin/bash
# Checks a built disk image: contents, signature, icon, version and the Finder window layout (macOS only).
#
#   tools/verify-dmg.sh [DMG]      default: dist/Avery-<version>.dmg
#
# Prints one line per check and exits 1 if one of them fails.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
version="$(sed -n 's/^AVERY_VERSION="\(.*\)"$/\1/p' "$root/bin/avery")"
dmg="${1:-$root/dist/Avery-$version.dmg}"
volume="Avery"
failed=0

ok()  { printf '  ok      %s\n' "$*"; }
bad() { printf '  FEHLER  %s\n' "$*"; failed=$((failed + 1)); }
check() { local label="$1"; shift; if "$@" >/dev/null 2>&1; then ok "$label"; else bad "$label"; fi; }

[ -f "$dmg" ] || { echo "verify-dmg: $dmg nicht gefunden" >&2; exit 2; }
[ ! -e "/Volumes/$volume" ] || { echo "verify-dmg: /Volumes/$volume ist schon eingehaengt" >&2; exit 2; }

echo "Pruefe $dmg"
check "Image-Pruefsumme (hdiutil verify)" hdiutil verify "$dmg"

hdiutil attach "$dmg" -readonly -noverify -noautoopen >/dev/null 2>&1 || { bad "Image laesst sich nicht einhaengen"; exit 1; }
mount="/Volumes/$volume"
trap 'hdiutil detach "$mount" -force >/dev/null 2>&1 || true' EXIT
[ -d "$mount" ] || { bad "Volume $volume fehlt"; exit 1; }

app="$mount/Avery.app"
check "Avery.app vorhanden" test -d "$app"
check "Signatur der App gueltig" codesign --verify --deep --strict "$app"
check "Version der App ist $version" test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist" 2>/dev/null)" = "$version"
check "Kennung io.github.flavioaiello.avery" test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist" 2>/dev/null)" = "io.github.flavioaiello.avery"
check "App-Icon ist das Avery-Icon" cmp -s "$app/Contents/Resources/applet.icns" "$root/share/avery/Avery.icns"
check "kein Asset-Katalog mit dem Skript-Icon" test ! -e "$app/Contents/Resources/Assets.car"
check "keine CFBundleIconName (sonst gewinnt das Skript-Icon)" test -z "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconName' "$app/Contents/Info.plist" 2>/dev/null)"
check "App ruft /opt/homebrew/bin/avery auf" bash -c "osadecompile '$app' | grep -q '/opt/homebrew/bin/avery'"
check "App ist nicht in Quarantaene" test -z "$(xattr -p com.apple.quarantine "$app" 2>/dev/null)"
check "Applications zeigt auf /Applications" test "$(readlink "$mount/Applications" 2>/dev/null)" = "/Applications"
check "Hintergrundbild vorhanden" cmp -s "$mount/.background/background.tiff" "$root/assets/dmg/background.tiff"
check "Volume-Icon vorhanden" test -f "$mount/.VolumeIcon.icns"
check "Volume-Icon ist aktiviert (Custom-Icon-Flag)" bash -c "GetFileInfo '$mount' | grep -q 'attributes: .*C'"
check "Fensterlayout gespeichert (.DS_Store)" test "$(wc -c < "$mount/.DS_Store" 2>/dev/null || echo 0)" -gt 4096

# Keep these numbers in sync with tools/make-dmg.sh.
layout="$(osascript <<APPLESCRIPT 2>/dev/null
tell application "Finder"
	tell disk "$volume"
		open
		delay 2
		set w to container window
		set o to icon view options of w
		set b to bounds of w
		set pa to position of item "Avery.app" of w
		set pb to position of item "Applications" of w
		set r to (current view of w as string) & "|" & (item 1 of b as string) & "," & (item 2 of b as string) & "," & (item 3 of b as string) & "," & (item 4 of b as string)
		set r to r & "|" & (item 1 of pa as string) & "," & (item 2 of pa as string) & "|" & (item 1 of pb as string) & "," & (item 2 of pb as string)
		set r to r & "|" & (icon size of o as string) & "|" & (text size of o as string) & "|" & (toolbar visible of w as string)
		close
		return r
	end tell
end tell
APPLESCRIPT
)"
check "Fensterlayout: Symbolansicht, 640x448, Symbole bei (170,222) und (470,222), Groesse 128" \
  test "$layout" = "icon view|200,120,840,568|170,222|470,222|128|13|false"
[ "$layout" = "icon view|200,120,840,568|170,222|470,222|128|13|false" ] || printf '          gelesen: %s\n' "$layout"

echo
if [ "$failed" -eq 0 ]; then echo "Alles in Ordnung."; else echo "$failed Pruefung(en) fehlgeschlagen."; exit 1; fi
