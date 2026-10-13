#!/usr/bin/env bash
# Omniphony for one user on Linux: the player (mpv-omniphony), Studio with its
# renderer, and the engine library, from one release. No root, no config file
# written. Design: docs/installer.md in mgth/Omniphony.
#
#   ./install.sh             install or upgrade
#   ./install.sh --service   also run the renderer as a systemd user service
#
# What goes where ($XDG_DATA_HOME defaults to ~/.local/share, $XDG_CONFIG_HOME
# to ~/.config):
#   ~/.local/lib/omniphony/                 the player AppImage, Studio's tree,
#                                           installed.txt and uninstall.sh
#   ~/.local/bin/                           mpv-omniphony, orender,
#                                           omniphony-studio-egui (links)
#   $XDG_DATA_HOME/omniphony/lib/           the engine library, where the player,
#                                           Studio and orender look first; the
#                                           decoder bridge goes there too
#   $XDG_DATA_HOME/applications/, icons/    menu entries
#   $XDG_CONFIG_HOME/systemd/user/          the unit, with --service
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
payload="$here/payload"
version="$(cat "$here/VERSION")"

service=0
for arg in "$@"; do
    case "$arg" in
        --service) service=1 ;;
        -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "install.sh: unknown option $arg (see --help)" >&2; exit 2 ;;
    esac
done

if [ "$(id -u)" -eq 0 ]; then
    echo "install.sh: run it as the user who will play films, not as root:" >&2
    echo "the engine, the config and the audio session are per user." >&2
    exit 1
fi
if [ "$(uname -m)" != x86_64 ]; then
    echo "install.sh: these binaries are x86_64, this machine is $(uname -m)." >&2
    exit 1
fi

data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
root="$HOME/.local/lib/omniphony"
bin="$HOME/.local/bin"
lib="$data_home/omniphony/lib"
apps="$data_home/applications"
icons="$data_home/icons/hicolor"
unit_dir="$config_home/systemd/user"
unit="$unit_dir/omniphony-renderer.service"
manifest="$root/installed.txt"

# The player AppImage needs glibc 2.38, Studio and orender 2.35.
glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}' || true)"
if [ -n "$glibc" ] && [ "$(printf '%s\n' 2.38 "$glibc" | sort -V | head -n1)" != 2.38 ]; then
    echo "warning: glibc $glibc is older than the 2.38 the player needs;" >&2
    echo "         it will not start on this system (docs/install/linux.md)." >&2
fi

echo "Installing Omniphony $version for $(id -un)"

# What an earlier run wrote, to remove what this release no longer has.
previous=()
if [ -f "$manifest" ]; then
    while IFS= read -r line; do previous+=("$line"); done < "$manifest"
fi
written=()
record() { written+=("$1"); }

# Copy a file into place through a rename, so a running process keeps the old
# one and nothing ever reads a half-written file.
put() {
    local src="$1" dst="$2" mode="$3"
    mkdir -p "$(dirname "$dst")"
    cp "$src" "$dst.omniphony-new"
    chmod "$mode" "$dst.omniphony-new"
    mv -f "$dst.omniphony-new" "$dst"
    record "$dst"
}

systemd_reachable() { systemctl --user show-environment >/dev/null 2>&1; }

# The renderer the unit runs: the real file, as Studio's "Install service"
# names it, not the link in ~/.local/bin.
orender="$root/studio/orender"
# As the packaged unit has it for a plain path. A path with a space or another
# character systemd reads specially goes between double quotes: systemd
# refuses a backslash-escaped space in the executable's path. A `%` is doubled,
# quoted or not it would be read as a specifier. This is the rule of Studio's
# systemd_quote_word() (omniphony-studio-egui/core/src/host/commands/orender.rs)
# for the executable, character for character: keep the two alike, or an
# upgrade stops recognising the unit Studio wrote for this orender.
case "$orender" in
    *[!A-Za-z0-9_./@:+-]*)
        quoted="${orender//\\/\\\\}"
        quoted="${quoted//\"/\\\"}"
        quoted="${quoted//$'\n'/\\n}"
        quoted="${quoted//\%/%%}"
        exec_start="ExecStart=\"$quoted\"" ;;
    *)
        exec_start="ExecStart=$orender" ;;
esac
# Only a line that starts with it is the command the unit runs: the same text
# after a `#` is a comment the user kept.
runs_this_orender() {
    [ -f "$unit" ] && exec_start="$exec_start" awk '
        index($0, ENVIRON["exec_start"] " ") == 1 { found = 1 }
        END { exit !found }' "$unit"
}

# A renderer service that runs this orender is restarted once the files are
# replaced, whoever installed the unit (this script or Studio).
restart_service=0
if runs_this_orender && systemd_reachable \
    && systemctl --user is-active --quiet omniphony-renderer; then
    restart_service=1
fi

# ── Studio, with orender and the engine it deploys ──────────────────────────
mkdir -p "$root"
rm -rf "$root/studio.omniphony-new"
cp -R "$payload/studio" "$root/studio.omniphony-new"
if [ -d "$root/studio" ]; then
    rm -rf "$root/studio.omniphony-old"
    mv "$root/studio" "$root/studio.omniphony-old"
fi
mv "$root/studio.omniphony-new" "$root/studio"
rm -rf "$root/studio.omniphony-old"
record "$root/studio/"

# ── The player ───────────────────────────────────────────────────────────────
put "$payload/mpv-omniphony.AppImage" "$root/mpv-omniphony.AppImage" 755

# ── The engine library, in the folder every host searches before its own copy.
# The same bytes Studio deploys there on its first start.
for engine in "$payload/studio/engine/"*; do
    put "$engine" "$lib/$(basename "$engine")" 644
done

# ── Commands ────────────────────────────────────────────────────────────────
link() {
    local target="$1" name="$2"
    mkdir -p "$bin"
    # Replaced only when it is this installer's own link: a file, or a link
    # to another build, is the user's.
    if [ -e "$bin/$name" ] || [ -L "$bin/$name" ]; then
        case "$(readlink "$bin/$name" 2>/dev/null || true)" in
            "$root"/*) ;;
            *)
                echo "warning: $bin/$name exists and is not this installer's link: left as it is." >&2
                echo "         This release's is $target" >&2
                return ;;
        esac
    fi
    ln -sfn "$target" "$bin/$name"
    record "$bin/$name"
}
link "$root/mpv-omniphony.AppImage" mpv-omniphony
link "$root/studio/orender" orender
link "$root/studio/omniphony-studio-egui" omniphony-studio-egui

# ── Menu entries ─────────────────────────────────────────────────────────────
desktop() {
    local src="$1" dst="$apps/$2"
    mkdir -p "$apps"
    # The Exec key takes a quoted path, with its own escapes inside.
    local escaped="$bin"
    escaped="${escaped//\\/\\\\}"
    escaped="${escaped//\"/\\\"}"
    escaped="${escaped//\$/\\\$}"
    escaped="${escaped//\`/\\\`}"
    BIN="$escaped" awk '{
        while ((i = index($0, "@BIN@")) > 0)
            $0 = substr($0, 1, i - 1) ENVIRON["BIN"] substr($0, i + 5)
        print
    }' "$src" > "$dst.omniphony-new"
    mv -f "$dst.omniphony-new" "$dst"
    record "$dst"
}
desktop "$payload/applications/omniphony-studio-egui.desktop.in" omniphony-studio-egui.desktop
desktop "$payload/applications/mpv-omniphony.desktop.in" mpv-omniphony.desktop
for icon in "$payload"/icons/*/omniphony-studio-egui.png; do
    size="$(basename "$(dirname "$icon")")"
    put "$icon" "$icons/$size/apps/omniphony-studio-egui.png" 644
done
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$apps" 2>/dev/null || true
fi

put "$here/uninstall.sh" "$root/uninstall.sh" 755

# ── The renderer as a user service ───────────────────────────────────────────
# The unit Studio's "Install service" writes, for this orender: either can
# replace the other's. Kept from an earlier --service run on an upgrade.
had_unit=0
for line in ${previous[@]+"${previous[@]}"}; do
    # Still there: a unit removed since (Studio's "Uninstall service") is not
    # written again by an upgrade.
    if [ "$line" = "$unit" ] && [ -f "$unit" ]; then had_unit=1; fi
done
if [ "$service" -eq 1 ] || [ "$had_unit" -eq 1 ]; then
    # A unit that already starts this orender is kept as it is: Studio may
    # have rewritten it with other settings (port, input pipe, log level).
    if ! runs_this_orender; then
        mkdir -p "$unit_dir"
        # The packaged unit without its leading comment, started from this
        # orender.
        grep -v '^#' "$payload/omniphony-renderer.service" | sed '/./,$!d' \
            | exec_start="$exec_start" awk '
                index($0, "ExecStart=/usr/bin/orender ") == 1 {
                    print ENVIRON["exec_start"] substr($0, length("ExecStart=/usr/bin/orender") + 1)
                    next
                }
                { print }' > "$unit.omniphony-new"
        mv -f "$unit.omniphony-new" "$unit"
    fi
    record "$unit"
fi

# ── Leftovers of the release this one replaces ───────────────────────────────
for old in ${previous[@]+"${previous[@]}"}; do
    keep=0
    for new in "${written[@]}"; do
        [ "$old" = "$new" ] && { keep=1; break; }
    done
    if [ "$keep" -eq 0 ]; then
        case "$old" in
            */) rm -rf "${old:?}" ;;
            "$bin"/*)
                # A command link the user has since replaced with a file or a
                # link of their own is theirs.
                case "$(readlink "$old" 2>/dev/null || true)" in
                    "$root"/*) rm -f "$old" ;;
                esac ;;
            *)  rm -f "$old" ;;
        esac
    fi
done
printf '%s\n' "${written[@]}" > "$manifest.omniphony-new"
mv -f "$manifest.omniphony-new" "$manifest"

# ── Start what should run ────────────────────────────────────────────────────
if runs_this_orender; then
    if systemd_reachable; then
        systemctl --user daemon-reload
        if [ "$service" -eq 1 ]; then
            systemctl --user enable --now omniphony-renderer
            echo "The renderer runs as the user service omniphony-renderer, from every login."
        fi
        if [ "$restart_service" -eq 1 ]; then
            systemctl --user restart omniphony-renderer
        fi
    elif [ "$service" -eq 1 ]; then
        echo "warning: no systemd user session reachable from here. The unit is written;" >&2
        echo "         from your desktop session run:" >&2
        echo "           systemctl --user enable --now omniphony-renderer" >&2
    fi
fi

# ── The decoder bridge: installed apart, checked here ────────────────────────
echo
echo "Installed:"
echo "  player   $bin/mpv-omniphony   (play with: mpv-omniphony --ad=orender film.mkv)"
echo "  Studio   $bin/omniphony-studio-egui"
echo "  engine   $lib/"
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "  $bin is not in your PATH: add it, or call the commands by their full path." ;;
esac
echo
echo "Decoder bridge (not part of this installer):"
# `check-bridges` came with the `bridge_api:` line of `--version`. An older
# orender would take the word for an input to render: never call it blind.
if "$orender" --version 2>/dev/null | grep -q '^bridge_api:'; then
    # Resolves and loads the bridges the way a run does, and names a bridge
    # built for another engine with both versions.
    if "$orender" check-bridges 2>&1 | sed 's/^/  /'; then
        :
    else
        echo "  No usable decoder bridge. Without one the stack starts, and films"
        echo "  play through mpv's own decoders, not through Omniphony."
        echo "  Put a bridge of the series named on the 'host:' line into"
        echo "    $lib/"
        echo "  (docs/install/linux.md, \"The decoder bridge\")."
    fi
else
    if ls "$lib"/*bridge*.so >/dev/null 2>&1; then
        echo "  found in $lib/:"
        ls "$lib"/*bridge*.so | sed 's/^/    /'
        echo "  This orender cannot check that it matches the engine; the player's log does."
    else
        echo "  None in $lib/. Put the bridge there (docs/install/linux.md)."
    fi
fi
echo
echo "To remove everything this installed: $root/uninstall.sh"
