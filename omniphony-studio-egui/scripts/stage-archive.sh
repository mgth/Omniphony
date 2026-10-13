#!/usr/bin/env bash
# The plain archive of the native Studio for the platform this runs on: the
# Studio, orender next to it, the engine library it deploys for the player,
# the layouts and the 3D asset. No installation: unpack and run. Used by
# release.yml and integration-build.yml, after scripts/package.sh (which
# stages the engine library under its loader name), and what the combined
# installer is composed from (packaging/installer/).
#
#   stage-archive.sh LABEL OUT_DIR
#
# LABEL is the release tag, or `integration`. Writes
# OUT_DIR/omniphony-studio-egui-<label>-<platform>.tar.gz (.zip on Windows and
# macOS) and prints its path, relative to the repository root.
set -euo pipefail

label="${1:?usage: stage-archive.sh LABEL OUT_DIR}"
# A manual run on a branch passes a ref name with slashes.
label="${label//\//-}"
out="${2:?usage: stage-archive.sh LABEL OUT_DIR}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

case "$(uname -s)" in
  Linux)                platform=linux-x86_64;   exe= ;;
  Darwin)               platform=macos-arm64;    exe= ;;
  MINGW*|MSYS*|CYGWIN*) platform=windows-x86_64; exe=.exe ;;
  *) echo "stage-archive.sh: unsupported OS $(uname -s)" >&2; exit 1 ;;
esac

name="omniphony-studio-egui-$label-$platform"
dir="$out/$name"
mkdir -p "$dir/layouts/legacy" "$dir/assets" "$dir/engine"
cp "omniphony-studio-egui/target/release/omniphony-studio-egui$exe" "$dir/"
# The Studio looks for a plain `orender` next to itself.
cp "omniphony-renderer/target/release/orender$exe" "$dir/"
# The engine library the Studio deploys for the player on startup, staged
# under its loader name by package.sh.
cp omniphony-studio-egui/target/release/engine/* "$dir/engine/"
cp layouts/*.yaml "$dir/layouts/"
cp layouts/legacy/*.yaml "$dir/layouts/legacy/"
cp omniphony-studio-egui/assets/la_dame_de_brassempouy_centered.glb "$dir/assets/"
cp LICENSE "$dir/"
ls -laR "$dir" >&2

cd "$out"
case "$platform" in
  linux-*)   archive="$name.tar.gz"; tar czf "$archive" "$name" ;;
  windows-*) archive="$name.zip";    7z a -tzip "$archive" "$name" > /dev/null ;;
  macos-*)   archive="$name.zip";    zip -qr "$archive" "$name" ;;
esac
echo "$out/$archive"
