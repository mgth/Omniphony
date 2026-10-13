#!/usr/bin/env bash
# Two installers made of stand-in release files, for test.ps1: "v1.0", and
# "v2.0" whose Studio archive has one file less and another engine.
#
#   make-standins.sh OUT_DIR
#
# Needs a C compiler targeting Windows (CC, else x86_64-w64-mingw32-gcc, else
# gcc), zip or 7z, and what build.sh needs. Runs on Linux and in Git Bash.
set -euo pipefail

out="${1:?usage: make-standins.sh OUT_DIR}"
mkdir -p "$out"
out="$(realpath "$out")"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if [ -z "${CC:-}" ]; then
    if command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then CC=x86_64-w64-mingw32-gcc; else CC=gcc; fi
fi
"$CC" -O1 -static -o "$work/standin.exe" "$here/standin/orender.c"

archive() { # zip file, directory whose content goes in
    if command -v zip >/dev/null 2>&1; then
        (cd "$2" && zip -qr "$1" .)
    else
        (cd "$2" && 7z a -tzip "$1" . >/dev/null)
    fi
}

for version in 1.0 2.0; do
    studio="$work/studio-$version/omniphony-studio-egui-v$version-windows-x86_64"
    player="$work/player-$version"
    mkdir -p "$studio/engine" "$studio/layouts/legacy" "$studio/assets" "$player"
    cp "$work/standin.exe" "$studio/orender.exe"
    cp "$work/standin.exe" "$studio/omniphony-studio-egui.exe"
    echo "engine-v$version" > "$studio/engine/orender.dll"
    echo layout > "$studio/layouts/7.1.4.yaml"
    echo layout > "$studio/layouts/legacy/5.1.yaml"
    echo model > "$studio/assets/la_dame_de_brassempouy_centered.glb"
    cp "$repo/LICENSE" "$studio/"
    [ "$version" != 1.0 ] || echo old > "$studio/only-in-1.0.txt"
    cp "$work/standin.exe" "$player/mpv.exe"
    echo com > "$player/mpv.com"
    echo fallback > "$player/orender.dll"
    archive "$work/studio-$version.zip" "$work/studio-$version"
    archive "$work/player-$version.zip" "$player"
    "$here/build.sh" "v$version" "$work/studio-$version.zip" "$work/player-$version.zip" "$out"
done
