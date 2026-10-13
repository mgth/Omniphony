#!/usr/bin/env bash
# The Linux installer end to end, in a temporary HOME, on stand-in release
# files: compose (build.sh), install, install again as an upgrade, the service
# unit, uninstall. Run by CI (ci.yml, lint) and by hand:
#
#   packaging/installer/linux/test.sh
#
# With real release files, installer.yml runs the same install in containers.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
has() { [ -e "$1" ] || fail "missing $1"; }
hasnt() { [ ! -e "$1" ] && [ ! -L "$1" ] || fail "still there: $1"; }

# A Studio archive and a player with the shape of the published ones. The
# stand-in orender answers `check-bridges` as the real one does: it fails
# without a bridge next to the engine, and names the one it finds.
make_assets() {
    local tag="$1" extra="${2:-}"
    local dir="$work/assets-$tag/omniphony-studio-egui-$tag-linux-x86_64"
    mkdir -p "$dir/engine" "$dir/layouts/legacy" "$dir/assets"
    printf '#!/bin/sh\necho "studio %s"\n' "$tag" > "$dir/omniphony-studio-egui"
    cat > "$dir/orender" <<'ORENDER'
#!/bin/sh
lib="${XDG_DATA_HOME:-$HOME/.local/share}/omniphony/lib"
case "$1" in
    --version)
        echo "omniphony-renderer stand-in"
        echo "liborender ABI: 0.12"
        echo "bridge_api: 0.6.x" ;;
    check-bridges)
        echo "host: bridge_api 0.6.x"
        for bridge in "$lib"/*bridge*.so; do
            [ -e "$bridge" ] || { echo "Error: no decoder bridge found" >&2; exit 1; }
            echo "loaded: $bridge (test)"
        done ;;
    *) echo "orender stand-in" ;;
esac
ORENDER
    chmod 755 "$dir/omniphony-studio-egui" "$dir/orender"
    echo "engine $tag" > "$dir/engine/liborender.so.0"
    echo layout > "$dir/layouts/7.1.4.yaml"
    echo model > "$dir/assets/la_dame_de_brassempouy_centered.glb"
    echo licence > "$dir/LICENSE"
    [ -z "$extra" ] || echo extra > "$dir/$extra"
    tar -czf "$work/assets-$tag/studio.tar.gz" -C "$work/assets-$tag" "$(basename "$dir")"
    printf '#!/bin/sh\necho "player %s"\n' "$tag" > "$work/assets-$tag/player.AppImage"
}

bundle() {
    local tag="$1"
    local archive
    archive="$("$here/build.sh" "$tag" "$work/assets-$tag/studio.tar.gz" \
        "$work/assets-$tag/player.AppImage" "$work/out")"
    mkdir -p "$work/unpacked"
    tar -xzf "$archive" -C "$work/unpacked"
    echo "$work/unpacked/omniphony-$tag-linux-x86_64"
}

export HOME="$work/home"
unset XDG_DATA_HOME XDG_CONFIG_HOME
mkdir -p "$HOME"

# A stand-in systemctl ahead of the real one: the test must never reach the
# session of whoever runs it, and what the scripts ask of systemd is checked
# from its log. The renderer service counts as running once the file exists.
mkdir "$work/bin"
cat > "$work/bin/systemctl" <<SYSTEMCTL
#!/bin/sh
echo "\$*" >> "$work/systemctl.log"
case "\$*" in
    *is-active*) [ -e "$work/service-active" ] ;;
esac
SYSTEMCTL
chmod 755 "$work/bin/systemctl"
PATH="$work/bin:$PATH"
: > "$work/systemctl.log"
asked() { grep -qx -- "$1" "$work/systemctl.log" || { cat "$work/systemctl.log"; fail "systemctl was not asked: $1"; }; }
root="$HOME/.local/lib/omniphony"
lib="$HOME/.local/share/omniphony/lib"
unit="$HOME/.config/systemd/user/omniphony-renderer.service"

echo "== compose and install 1.0"
make_assets v1.0 only-in-1.0
first="$(bundle v1.0)"
"$first/install.sh" > "$work/install-1.log" 2>&1 || { cat "$work/install-1.log"; fail "install 1.0"; }
has "$root/studio/orender"
has "$root/studio/only-in-1.0"
has "$root/mpv-omniphony.AppImage"
has "$root/uninstall.sh"
has "$HOME/.local/share/applications/mpv-omniphony.desktop"
has "$HOME/.local/share/icons/hicolor/128x128/apps/omniphony-studio-egui.png"
[ "$(cat "$lib/liborender.so.0")" = "engine v1.0" ] || fail "engine not in the per-user folder"
[ "$("$HOME/.local/bin/mpv-omniphony")" = "player v1.0" ] || fail "player link"
[ "$("$HOME/.local/bin/omniphony-studio-egui")" = "studio v1.0" ] || fail "Studio link"
grep -q "^Exec=\"$HOME/.local/bin/omniphony-studio-egui\"$" \
    "$HOME/.local/share/applications/omniphony-studio-egui.desktop" || fail "Studio menu entry"
hasnt "$unit"
hasnt "$HOME/.config/omniphony"
grep -q "No usable decoder bridge" "$work/install-1.log" || fail "a missing bridge must be said"

echo "== a bridge next to the engine, a config, a command of the user's own"
echo bridge > "$lib/libtest_bridge.so"
mkdir -p "$HOME/.config/omniphony"
echo "render: {}" > "$HOME/.config/omniphony/config.yaml"
rm "$HOME/.local/bin/orender"
echo mine > "$HOME/.local/bin/orender"
# ... and a link of the user's own, to a build of theirs.
echo "#!/bin/sh" > "$HOME/my-studio"
ln -sfn "$HOME/my-studio" "$HOME/.local/bin/omniphony-studio-egui"

echo "== upgrade to 2.0, with the service"
make_assets v2.0
second="$(bundle v2.0)"
"$second/install.sh" --service > "$work/install-2.log" 2>&1 || { cat "$work/install-2.log"; fail "install 2.0"; }
[ "$(cat "$lib/liborender.so.0")" = "engine v2.0" ] || fail "engine not upgraded"
[ "$("$HOME/.local/bin/mpv-omniphony")" = "player v2.0" ] || fail "player not upgraded"
hasnt "$root/studio/only-in-1.0"
hasnt "$root/studio.omniphony-old"
[ "$(cat "$HOME/.local/bin/orender")" = mine ] || fail "the user's own orender was replaced"
[ "$(readlink "$HOME/.local/bin/omniphony-studio-egui")" = "$HOME/my-studio" ] || fail "the user's own link was replaced"
grep -q "loaded: $lib/libtest_bridge.so" "$work/install-2.log" || fail "the bridge check did not run"
has "$unit"
asked "--user enable --now omniphony-renderer"
grep -qx "ExecStart=$root/studio/orender render /tmp/orender.pipe --continuous --enable-vbap --osc --osc-host 127.0.0.1 --osc-port 9000 --osc-rx-port 9000 --osc-yield" "$unit" \
    || { cat "$unit"; fail "unit ExecStart"; }
# Apart from the path, the unit Studio writes and the packages ship.
diff <(grep -v '^#' "$here/../../systemd/omniphony-renderer.service" | sed '/./,$!d' \
        | sed "s|^ExecStart=/usr/bin/orender |ExecStart=$root/studio/orender |") "$unit" \
    || fail "the unit differs from the packaged one"
if command -v systemd-analyze >/dev/null 2>&1; then
    cp "$unit" "$work/omniphony-renderer.service"
    mkdir -p "$work/run"
    XDG_RUNTIME_DIR="$work/run" systemd-analyze --user verify "$work/omniphony-renderer.service" 2>&1 \
        | tee "$work/verify.log"
    ! grep -qi "omniphony-renderer.service:" "$work/verify.log" || fail "systemd-analyze rejects the unit"
fi

echo "== upgrade again without --service: the unit stays, a running service is restarted"
: > "$work/systemctl.log"
touch "$work/service-active"
# Settings Studio's "Install service" would have written survive an upgrade.
sed -i 's/--osc-port 9000/--osc-port 9100/' "$unit"
"$second/install.sh" > /dev/null 2>&1 || fail "second install of 2.0"
has "$unit"
grep -q -- "--osc-port 9100" "$unit" || fail "the upgrade rewrote a unit that already ran this orender"
asked "--user restart omniphony-renderer"
! grep -q "enable" "$work/systemctl.log" || fail "the service was enabled without --service"
rm "$work/service-active"

echo "== a home directory with a space in its name"
(
    export HOME="$work/home dir"
    mkdir -p "$HOME"
    : > "$work/systemctl.log"
    "$second/install.sh" --service > /dev/null 2>&1 || fail "install with a space in HOME"
    asked "--user enable --now omniphony-renderer"
    spaced="$HOME/.config/systemd/user/omniphony-renderer.service"
    grep -qF "ExecStart=\"$work/home dir/.local/lib/omniphony/studio/orender\" render " "$spaced" \
        || fail "unit path not quoted"
    # Still the packaged unit apart from the path, which is written as Studio
    # writes it (systemd_quote_word in the Studio core, and its tests).
    diff <(grep -v '^#' "$here/../../systemd/omniphony-renderer.service" | sed '/./,$!d' \
            | sed "s|^ExecStart=/usr/bin/orender |ExecStart=\"$HOME/.local/lib/omniphony/studio/orender\" |") "$spaced" \
        || fail "the unit of a path with a space differs from the packaged one"
    grep -qF "Exec=\"$work/home dir/.local/bin/mpv-omniphony\" --ad=orender" \
        "$HOME/.local/share/applications/mpv-omniphony.desktop" || fail "menu entry path not quoted"
    if command -v systemd-analyze >/dev/null 2>&1; then
        # systemd refuses a backslash-escaped space in the executable's path.
        mkdir -p "$work/run2"
        cp "$spaced" "$work/spaced.service"
        verify="$(XDG_RUNTIME_DIR="$work/run2" systemd-analyze --user verify "$work/spaced.service" 2>&1 || true)"
        ! grep -q "spaced.service" <<<"$verify" || { echo "$verify"; fail "systemd-analyze rejects the unit of a path with a space"; }
    fi
    "$HOME/.local/lib/omniphony/uninstall.sh" > /dev/null || fail "uninstall with a space in HOME"
)

echo "== the unit Studio writes for a path with a space and a percent sign is recognised"
(
    export HOME="$work/100% home"
    mkdir -p "$HOME/.config/systemd/user"
    studio_unit="$HOME/.config/systemd/user/omniphony-renderer.service"
    # Studio's "Install service" for this orender, with a port of the user's.
    studio_exec="ExecStart=\"$work/100%% home/.local/lib/omniphony/studio/orender\" render /tmp/orender.pipe --osc-port 9100"
    printf '[Service]\n%s\n' "$studio_exec" > "$studio_unit"
    "$second/install.sh" --service > /dev/null 2>&1 || fail "install over Studio's unit"
    grep -qxF "$studio_exec" "$studio_unit" || { cat "$studio_unit"; fail "Studio's unit was rewritten"; }
    # The installer writes the same line when the unit is its own.
    rm "$studio_unit"
    "$second/install.sh" --service > /dev/null 2>&1 || fail "install with a percent sign in HOME"
    grep -qF "ExecStart=\"$work/100%% home/.local/lib/omniphony/studio/orender\" render " "$studio_unit" \
        || { cat "$studio_unit"; fail "percent sign not doubled"; }
    if command -v systemd-analyze >/dev/null 2>&1; then
        mkdir -p "$work/run3"
        cp "$studio_unit" "$work/percent.service"
        verify="$(XDG_RUNTIME_DIR="$work/run3" systemd-analyze --user verify "$work/percent.service" 2>&1 || true)"
        ! grep -q "percent.service" <<<"$verify" || { echo "$verify"; fail "systemd-analyze rejects the unit of a path with a percent sign"; }
    fi
    "$HOME/.local/lib/omniphony/uninstall.sh" > /dev/null || fail "uninstall with a percent sign in HOME"
    # One Studio wrote, never recorded by the installer, goes with the uninstall.
    "$second/install.sh" > /dev/null 2>&1 || fail "install without the service"
    printf '[Service]\n%s\n' "$studio_exec" > "$studio_unit"
    : > "$work/systemctl.log"
    "$HOME/.local/lib/omniphony/uninstall.sh" > /dev/null || fail "uninstall with Studio's unit"
    asked "--user disable --now omniphony-renderer"
    hasnt "$studio_unit"
)

echo "== uninstall"
: > "$work/systemctl.log"
"$root/uninstall.sh" > "$work/uninstall.log" 2>&1 || { cat "$work/uninstall.log"; fail "uninstall"; }
asked "--user disable --now omniphony-renderer"
hasnt "$root"
hasnt "$HOME/.local/bin/mpv-omniphony"
hasnt "$lib/liborender.so.0"
hasnt "$unit"
hasnt "$HOME/.local/share/applications/mpv-omniphony.desktop"
hasnt "$HOME/.local/share/icons/hicolor/128x128/apps/omniphony-studio-egui.png"
has "$HOME/.config/omniphony/config.yaml"
has "$lib/libtest_bridge.so"
[ "$(cat "$HOME/.local/bin/orender")" = mine ] || fail "the user's own orender was removed"
[ "$(readlink "$HOME/.local/bin/omniphony-studio-egui")" = "$HOME/my-studio" ] || fail "the user's own link was removed"

echo "== a unit removed since is not written again; one Studio wrote goes with the uninstall"
"$second/install.sh" --service > /dev/null 2>&1 || fail "install with the service"
has "$unit"
rm "$unit"
"$second/install.sh" > /dev/null 2>&1 || fail "upgrade after the unit was removed"
hasnt "$unit"
# Studio's "Install service": the same unit, never recorded by the installer.
printf '[Service]\nExecStart=%s render /tmp/orender.pipe --osc-port 9100\n' "$root/studio/orender" > "$unit"
: > "$work/systemctl.log"
"$root/uninstall.sh" > /dev/null 2>&1 || fail "uninstall with Studio's unit"
asked "--user disable --now omniphony-renderer"
hasnt "$unit"

echo "== an orender older than check-bridges is never asked to run it"
make_assets v0.9
cat > "$work/assets-v0.9/omniphony-studio-egui-v0.9-linux-x86_64/orender" <<'OLD'
#!/bin/sh
case "$1" in
    --version) echo "omniphony-renderer old" ;;
    *) echo "rendering $1" ; exit 1 ;;
esac
OLD
tar -czf "$work/assets-v0.9/studio.tar.gz" -C "$work/assets-v0.9" omniphony-studio-egui-v0.9-linux-x86_64
old="$(bundle v0.9)"
"$old/install.sh" > "$work/install-old.log" 2>&1 || { cat "$work/install-old.log"; fail "install with an old orender"; }
! grep -q "rendering" "$work/install-old.log" || fail "check-bridges was passed to an orender without it"
grep -q "This orender cannot check" "$work/install-old.log" || fail "the fallback bridge report is missing"
"$root/uninstall.sh" > /dev/null

echo "== composition: a Studio archive without its engine is refused"
rm "$work/assets-v2.0/omniphony-studio-egui-v2.0-linux-x86_64/engine/liborender.so.0"
tar -czf "$work/assets-v2.0/studio.tar.gz" -C "$work/assets-v2.0" omniphony-studio-egui-v2.0-linux-x86_64
if "$here/build.sh" v2.0 "$work/assets-v2.0/studio.tar.gz" "$work/assets-v2.0/player.AppImage" \
    "$work/out2" > /dev/null 2>&1; then
    fail "build.sh accepted an archive without the engine"
fi

echo "OK"
