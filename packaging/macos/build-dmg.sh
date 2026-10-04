#!/usr/bin/env bash
# Build a drag-to-Applications DMG. The GitHub macOS runner is Apple Silicon,
# so the disk image is named with the machine architecture.
set -euo pipefail

version="${1:?version}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
arch="$(uname -m)"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

app="$stage/BioViewer.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$root/dist"
sed "s/@VERSION@/${version}/g" "$(dirname "$0")/Info.plist" > "$app/Contents/Info.plist"
cp "$root/target/release/bioviewer" "$app/Contents/MacOS/bioviewer"
chmod +x "$app/Contents/MacOS/bioviewer"
codesign --force --sign - "$app"

dmg_root="$stage/dmg"
mkdir -p "$dmg_root"
cp -R "$app" "$dmg_root/"
ln -s /Applications "$dmg_root/Applications"
hdiutil create \
  -volname "BioViewer" \
  -srcfolder "$dmg_root" \
  -ov \
  -format UDZO \
  "$root/dist/BioViewer-${version}-macos-${arch}.dmg"
