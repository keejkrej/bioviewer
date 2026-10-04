#!/usr/bin/env bash
# Package the release binary as a .deb. Shared-library dependencies are read
# from ldd so the control file matches the binary that was just linked.
set -euo pipefail

version="${1:?version}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
binary="$root/target/release/bioviewer"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

if ldd "$binary" | grep -q "not found"; then
  ldd "$binary"
  echo "bioviewer is missing shared libraries" >&2
  exit 1
fi

mkdir -p "$stage/DEBIAN" "$stage/usr/bin" "$stage/usr/share/applications" "$root/dist"
install -m 0755 "$binary" "$stage/usr/bin/bioviewer"
install -m 0644 "$(dirname "$0")/bioviewer.desktop" "$stage/usr/share/applications/bioviewer.desktop"

# wgpu loads libvulkan.so.1 at runtime, so ldd does not list it.
# ldd prints /lib/... on a merged /usr system; package files live under /usr/lib.
ldd "$binary" | awk '/=> \// { print $3 }' | while read -r lib; do
  resolved="$(readlink -f "$lib")"
  if ! owner="$(dpkg -S "$resolved" 2>/dev/null | cut -d: -f1)"; then
    echo "no Debian package owns $lib ($resolved)" >&2
    exit 1
  fi
  printf '%s\n' "$owner"
done > "$stage/owners"
printf '%s\n' libvulkan1 >> "$stage/owners"
depends="$(sort -u "$stage/owners" | paste -sd, -)"
depends="${depends//,/, }"
if [[ -z "$depends" ]]; then
  echo "could not resolve Debian packages for shared libraries" >&2
  exit 1
fi

arch="$(dpkg --print-architecture)"
cat > "$stage/DEBIAN/control" <<EOF
Package: bioviewer
Version: ${version}
Section: science
Priority: optional
Architecture: ${arch}
Maintainer: keejkrej <ctyjackcao@outlook.com>
Depends: ${depends}
Homepage: https://github.com/keejkrej/bioviewer
Description: Microscopy viewer for Micro-Manager TIFF series
 BioViewer opens a folder of Micro-Manager TIFFs and composites fluorescence channels.
EOF

dpkg-deb --root-owner-group --build "$stage" "$root/dist/bioviewer_${version}_${arch}.deb"
