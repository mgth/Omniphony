#!/usr/bin/env bash
# Compose the Windows installer of a release from the files the release
# already publishes (docs/installer.md, step 5):
#
#   build.sh TAG STUDIO_ZIP PLAYER_ZIP OUT_DIR
#
# TAG          the Omniphony release, e.g. v0.7.0
# STUDIO_ZIP   omniphony-studio-egui-<tag>-windows-x86_64.zip of that release
# PLAYER_ZIP   mpv-omniphony-<player tag>-windows-x86_64.zip of the player
#              release the manifest pairs with it
#
# Writes OUT_DIR/omniphony-<tag>-windows-x86_64-setup.exe and prints its path.
# Needs makensis (NSIS 3) and unzip. installer.yml runs it on Linux; it also
# runs in Git Bash on a Windows that has NSIS. The engine
# is the one inside the Studio archive (engine/), the bytes Studio itself
# deploys. No decoder bridge: it is installed apart, and the installer checks it.
set -euo pipefail

tag="${1:?usage: build.sh TAG STUDIO_ZIP PLAYER_ZIP OUT_DIR}"
studio_zip="$(realpath "${2:?missing STUDIO_ZIP}")"
player_zip="$(realpath "${3:?missing PLAYER_ZIP}")"
out="${4:?missing OUT_DIR}"
mkdir -p "$out"
out="$(realpath "$out")"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

# The Studio archive holds one top-level directory; the player zip is flat.
mkdir "$stage/unpack" "$stage/player"
unzip -q "$studio_zip" -d "$stage/unpack"
top=("$stage/unpack"/*)
[ "${#top[@]}" -eq 1 ] && [ -d "${top[0]}" ] || {
    echo "build.sh: $studio_zip: expected one top-level directory" >&2; exit 1; }
mv "${top[0]}" "$stage/studio"
rmdir "$stage/unpack"
unzip -q "$player_zip" -d "$stage/player"

# Every piece at its path: a missing one would only show on a user's machine.
missing=0
for path in player/mpv.exe studio/omniphony-studio-egui.exe studio/orender.exe \
    studio/engine/orender.dll studio/layouts studio/assets studio/LICENSE; do
    [ -e "$stage/$path" ] || { echo "build.sh: missing $path" >&2; missing=1; }
done
[ "$missing" -eq 0 ] || exit 1

# What the installer copies and, file by file, what the uninstaller removes:
# anything else in those folders (a decoder bridge) is left where it is.
(
    cd "$stage"
    find player studio -type d | LC_ALL=C sort | while IFS= read -r dir; do
        printf 'SetOutPath "$INSTDIR\\%s"\n' "${dir//\//\\}"
        find "$dir" -maxdepth 1 -type f | LC_ALL=C sort | while IFS= read -r file; do
            printf 'File "${STAGE}/%s"\n' "$file"
        done
    done > install-files.nsh
    {
        find player studio -type f | LC_ALL=C sort | while IFS= read -r file; do
            printf 'Delete "$INSTDIR\\%s"\n' "${file//\//\\}"
        done
        find player studio -type d | LC_ALL=C sort -r | while IFS= read -r dir; do
            printf 'RMDir "$INSTDIR\\%s"\n' "${dir//\//\\}"
        done
    } > uninstall-files.nsh
)

# makensis wants paths of its own platform.
native() { if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi; }
makensis="${MAKENSIS:-makensis}"
if ! command -v "$makensis" >/dev/null 2>&1 && [ -x "/c/Program Files (x86)/NSIS/makensis.exe" ]; then
    makensis="/c/Program Files (x86)/NSIS/makensis.exe"
fi
setup="$out/omniphony-$tag-windows-x86_64-setup.exe"
"$makensis" -V2 -DTAG="$tag" -DSTAGE="$(native "$stage")" -DOUTFILE="$(native "$setup")" \
    "$(native "$here/omniphony.nsi")" >&2
echo "$setup"
