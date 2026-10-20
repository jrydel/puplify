#!/bin/sh
# Installs the latest Puplify release into /Applications on macOS (Apple Silicon).
#
#   curl -fsSL https://puplify.com/install.sh | sh
#
# Files fetched with curl are not quarantined by macOS, so the app opens
# without the "damaged or from an unidentified developer" prompt.
set -eu

APP="Puplify.app"
DEST="/Applications"
URL="https://puplify.com/download/mac"

fail() {
	printf 'puplify: %s\n' "$1" >&2
	exit 1
}

[ "$(uname -s)" = "Darwin" ] || fail "this installer is for macOS. Other platforms: https://puplify.com/#download"
[ "$(uname -m)" = "arm64" ] || fail "Puplify for macOS needs an Apple Silicon Mac. On Intel, build from source: https://github.com/jrydel/puplify"

tmp=$(mktemp -d)
mnt="$tmp/mnt"
cleanup() {
	hdiutil detach "$mnt" -quiet >/dev/null 2>&1 || true
	rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

printf 'Downloading Puplify…\n'
curl -fL --progress-bar -o "$tmp/Puplify.dmg" "$URL" || fail "download failed"

mkdir -p "$mnt"
hdiutil attach "$tmp/Puplify.dmg" -nobrowse -readonly -quiet -mountpoint "$mnt" || fail "could not open the disk image"
[ -d "$mnt/$APP" ] || fail "$APP not found in the disk image"

if pgrep -xq Puplify; then
	printf 'Quitting the running Puplify…\n'
	osascript -e 'quit app "Puplify"' >/dev/null 2>&1 || true
	sleep 2
fi

target="$DEST/$APP"
if [ -w "$DEST" ]; then
	rm -rf "$target"
	ditto "$mnt/$APP" "$target"
else
	printf 'Copying to %s needs your password.\n' "$DEST"
	sudo rm -rf "$target"
	sudo ditto "$mnt/$APP" "$target"
fi

printf 'Installed %s. Opening Puplify…\n' "$target"
open "$target"
