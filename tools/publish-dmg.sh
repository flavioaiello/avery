#!/bin/bash
# Publishes dist/Avery-<version>.dmg for the Homebrew cask and pins it in Casks/avery.rb.
#
#   tools/publish-dmg.sh [DMG]
#
# The cask fetches the disk image with git over SSH, which also works for a private repository (a release
# download would need a token). The image is committed on its own, without parent and without other files,
# and tagged dmg-v<version>; the history of main does not grow. Homebrew clones the whole repository (no
# shallow clone) and brew update fetches all tags, so every published image ends up in every tap clone:
# about 3.5 MB per release. A separate repository for the images, or public releases, avoids that.
# A published tag is never moved: a new image needs a new version.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
die() { printf 'publish-dmg: %s\n' "$*" >&2; exit 1; }

version="$(sed -n 's/^AVERY_VERSION="\(.*\)"$/\1/p' "$root/bin/avery")"
[ -n "$version" ] || die "Version in bin/avery nicht gefunden"
dmg="${1:-$root/dist/Avery-$version.dmg}"
[ -f "$dmg" ] || die "$dmg nicht gefunden (tools/make-dmg.sh)"
name="Avery-$version.dmg"
tag="dmg-v$version"
cask="$root/Casks/avery.rb"
g() { git -C "$root" "$@"; }
export GIT_SSH_COMMAND="${GIT_SSH_COMMAND:-ssh -o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=5 -o ServerAliveCountMax=3}"

blob="$(g hash-object -w "$dmg")"
# A local tag that never reached the remote (for example after a failed push) may be redone.
if g rev-parse -q --verify "refs/tags/$tag" >/dev/null && [ -z "$(g ls-remote --tags origin "refs/tags/$tag")" ]; then
  g tag -d "$tag" >/dev/null
fi
if g rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  existing="$(g rev-parse "$tag^{commit}")"
  [ "$(g rev-parse "$existing:$name" 2>/dev/null || true)" = "$blob" ] \
    || die "Das Tag $tag gibt es schon mit einem anderen Image. Veroeffentlichte Tags werden nicht verschoben: neue Version verwenden."
  revision="$existing"
  echo "Tag $tag existiert schon mit diesem Image: $revision"
else
  tree="$(printf '100644 blob %s\t%s\n' "$blob" "$name" | g mktree)"
  revision="$(g commit-tree "$tree" -m "Avery $version disk image")"
  g tag "$tag" "$revision"
  echo "Tag $tag angelegt: $revision"
fi

g push origin "refs/tags/$tag"

ruby -e '
  path, version, revision = ARGV
  text = File.read(path)
  text.sub!(/^  version ".*"$/, %(  version "#{version}")) || abort("version nicht gefunden")
  text.sub!(/revision: "[0-9a-f]{40}"/, %(revision: "#{revision}")) || abort("revision nicht gefunden")
  File.write(path, text)
' "$cask" "$version" "$revision"
echo "Casks/avery.rb steht jetzt auf $version ($revision)."
