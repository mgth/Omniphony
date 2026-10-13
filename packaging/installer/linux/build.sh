#!/usr/bin/env bash
# Compose the Linux installer of a release from the files the release already
# publishes (docs/installer.md, step 3):
#
#   build.sh TAG STUDIO_ARCHIVE PLAYER_APPIMAGE OUT_DIR
#
# TAG              the Omniphony release, e.g. v0.7.0
# STUDIO_ARCHIVE   omniphony-studio-egui-<tag>-linux-x86_64.tar.gz of that release
# PLAYER_APPIMAGE  mpv-omniphony-<player tag>-linux-x86_64.AppImage of the player
#                  release the manifest pairs with it
#
# Writes OUT_DIR/omniphony-<tag>-linux-x86_64.tar.gz and prints its path. The
# engine library is the one inside the Studio archive (engine/), the bytes
# Studio itself deploys. No decoder bridge: it is installed apart, and
# install.sh checks it.
set -euo pipefail

tag="${1:?usage: build.sh TAG STUDIO_ARCHIVE PLAYER_APPIMAGE OUT_DIR}"
studio_archive="$(realpath "${2:?missing STUDIO_ARCHIVE}")"
player="$(realpath "${3:?missing PLAYER_APPIMAGE}")"
out="${4:?missing OUT_DIR}"
mkdir -p "$out"
out="$(realpath "$out")"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
name="omniphony-$tag-linux-x86_64"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
stage="$work/$name"
mkdir -p "$stage/payload/applications"

# The Studio archive holds one top-level directory.
mkdir "$work/studio"
tar -xzf "$studio_archive" -C "$work/studio"
top=("$work/studio"/*)
[ "${#top[@]}" -eq 1 ] && [ -d "${top[0]}" ] || {
    echo "build.sh: $studio_archive: expected one top-level directory" >&2; exit 1; }
mv "${top[0]}" "$stage/payload/studio"

cp "$player" "$stage/payload/mpv-omniphony.AppImage"
cp "$repo/packaging/systemd/omniphony-renderer.service" "$stage/payload/"
cp "$here/applications/"*.desktop.in "$stage/payload/applications/"
for size in 32x32 64x64 128x128; do
    mkdir -p "$stage/payload/icons/$size"
    cp "$repo/omniphony-studio-egui/assets/icons/$size.png" \
        "$stage/payload/icons/$size/omniphony-studio-egui.png"
done
cp "$here/install.sh" "$here/uninstall.sh" "$repo/LICENSE" "$stage/"
chmod 755 "$stage/install.sh" "$stage/uninstall.sh" "$stage/payload/mpv-omniphony.AppImage"
printf '%s\n' "$tag" > "$stage/VERSION"

# Every piece at its path: a missing one would only show on a user's machine.
missing=0
for path in \
    payload/studio/omniphony-studio-egui payload/studio/orender \
    payload/studio/layouts payload/studio/assets payload/studio/LICENSE \
    payload/mpv-omniphony.AppImage payload/omniphony-renderer.service \
    payload/applications/omniphony-studio-egui.desktop.in \
    payload/applications/mpv-omniphony.desktop.in \
    payload/icons/128x128/omniphony-studio-egui.png \
    install.sh uninstall.sh LICENSE VERSION; do
    [ -e "$stage/$path" ] || { echo "build.sh: missing $path" >&2; missing=1; }
done
ls "$stage"/payload/studio/engine/liborender.so.* >/dev/null 2>&1 \
    || { echo "build.sh: missing payload/studio/engine/liborender.so.<abi>" >&2; missing=1; }
grep -q '^ExecStart=/usr/bin/orender ' "$stage/payload/omniphony-renderer.service" \
    || { echo "build.sh: the packaged unit no longer starts /usr/bin/orender: install.sh rewrites that path" >&2; missing=1; }
[ "$missing" -eq 0 ] || exit 1

tar -czf "$out/$name.tar.gz" -C "$work" "$name"
echo "$out/$name.tar.gz"
