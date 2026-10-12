#!/usr/bin/env bash
# Build the native Studio's installers for the platform this runs on (#677):
# deb + AppImage on Linux, NSIS (.exe) + WiX (.msi) on Windows (Git Bash),
# .dmg on macOS. Used by release.yml and integration-build.yml.
#
# Expects, from the repository root:
#   omniphony-studio-egui/target/release/omniphony-studio-egui[.exe]
#   omniphony-renderer/target/release/orender[.exe] and the engine library
#   (cargo build --release --locked, then -p orender_ffi, in omniphony-renderer)
# and cargo-packager on PATH. The packager configuration is
# [package.metadata.packager] in omniphony-studio-egui/Cargo.toml.
#
# Writes the installers' paths, relative to the repository root and one per
# line, to omniphony-studio-egui/target/packages/list.txt.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
studio="$root/omniphony-studio-egui"
renderer="$root/omniphony-renderer/target/release"
release="$studio/target/release"
out="$studio/target/packages"
list="$out/list.txt"

case "$(uname -s)" in
  Linux)                os=linux ;;
  Darwin)               os=macos ;;
  MINGW*|MSYS*|CYGWIN*) os=windows ;;
  *) echo "package.sh: unsupported OS $(uname -s)" >&2; exit 1 ;;
esac

# The renderer next to the Studio, as cargo-packager takes every binary from
# the Studio's own target directory, and the engine library under the name the
# player's loader probes (common/orender_dl.c in mpv-omniphony).
exe=
case "$os" in
  linux)
    major="$(sed -n 's/^#define ORENDER_ABI_MAJOR \([0-9][0-9]*\)$/\1/p' \
      "$root/omniphony-renderer/orender_ffi/include/orender.h")"
    [ -n "$major" ] || { echo "ORENDER_ABI_MAJOR not found in orender.h" >&2; exit 1; }
    lib_src=liborender.so; lib_name="liborender.so.$major" ;;
  macos)   lib_src=liborender.dylib; lib_name=liborender.dylib ;;
  windows) lib_src=orender.dll;      lib_name=orender.dll; exe=.exe ;;
esac
cp "$renderer/orender$exe" "$release/orender$exe"
rm -rf "$release/engine"
mkdir -p "$release/engine"
cp "$renderer/$lib_src" "$release/engine/$lib_name"

rm -rf "$out"
cd "$studio"
case "$os" in
  linux)   cargo packager --release -f deb -f appimage ;;
  windows) cargo packager --release -f nsis -f wix ;;
  macos)   cargo packager --release -f app ;;
esac

if [ "$os" = macos ]; then
  app="$(find "$out" -maxdepth 1 -name '*.app' -print -quit)"
  [ -n "$app" ] || { echo "no .app produced" >&2; exit 1; }
  # Ad-hoc signature of the whole bundle, without the hardened runtime
  # (cargo-packager would add it when it signs): the hardened runtime turns on
  # library validation, which stops orender from loading a bridge signed by
  # another team (#260). A coherent seal keeps a quarantine-cleared app from
  # showing the "damaged" dialog (#201). Same choice as the Tauri bundle.
  codesign --force --deep --sign - "$app"
  codesign --verify --deep --strict "$app"
  version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
  dmg="$out/omniphony-studio-egui_${version}_aarch64.dmg"
  stage="$out/dmg-root"
  mkdir -p "$stage"
  cp -R "$app" "$stage/"
  ln -s /Applications "$stage/Applications"
  hdiutil create -volname "Omniphony Studio (native)" -srcfolder "$stage" \
    -ov -format UDZO "$dmg"
  rm -rf "$stage"
fi

# Each installer must carry the renderer and the engine library: a missing
# resource is skipped silently by cargo-packager, so check the staged trees.
check() {
  for f in "$@"; do
    [ -e "$f" ] || { echo "package.sh: $f missing from the package tree" >&2; exit 1; }
  done
}
case "$os" in
  linux)
    tree="$(find "$out/.cargo-packager" -type d -path '*/data/usr' -print -quit)"
    check "$tree/bin/orender" "$tree/lib/omniphony-studio-egui/engine/$lib_name" \
      "$tree/lib/omniphony-studio-egui/layouts/7.1.4.yaml" \
      "$tree/lib/systemd/user/omniphony-renderer.service" ;;
  macos)
    check "$app/Contents/MacOS/orender" "$app/Contents/Resources/engine/$lib_name" \
      "$app/Contents/Resources/layouts/7.1.4.yaml" ;;
esac

# Relative, so the paths mean the same to bash, upload-artifact and gh on
# every runner (Git Bash prints /d/a/... on Windows).
cd "$root"
find "${out#"$root"/}" -maxdepth 1 -type f \
  \( -name '*.deb' -o -name '*.AppImage' -o -name '*.exe' -o -name '*.msi' -o -name '*.dmg' \) \
  > "$list"
[ -s "$list" ] || { echo "package.sh: no installer produced" >&2; exit 1; }
cat "$list"
