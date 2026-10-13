#!/usr/bin/env bash
# Remove what install.sh wrote for this user, and only that: the paths listed
# in installed.txt. Kept: config.yaml and its backups, Studio's own settings,
# mpv.conf, logs, and a decoder bridge put next to the engine.
set -euo pipefail

root="$HOME/.local/lib/omniphony"
manifest="$root/installed.txt"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
unit="$config_home/systemd/user/omniphony-renderer.service"

if [ ! -f "$manifest" ]; then
    echo "uninstall.sh: $manifest not found: nothing installed by install.sh here." >&2
    exit 1
fi

paths=()
while IFS= read -r line; do paths+=("$line"); done < "$manifest"

# The service first, stopped and disabled before its binary goes: the unit
# this installer wrote, or one Studio's "Install service" wrote for the
# orender being removed, which would be left starting a file that is gone.
ours=0
for path in "${paths[@]}"; do
    [ "$path" != "$unit" ] || ours=1
done
# Its ExecStart= as install.sh and Studio write it (the rule is explained
# there): quoted when the path holds a character systemd reads specially.
orender="$root/studio/orender"
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
# after a `#` is a comment, and the service is then somebody else's.
runs_this_orender() {
    [ -f "$unit" ] && exec_start="$exec_start" awk '
        index($0, ENVIRON["exec_start"] " ") == 1 { found = 1 }
        END { exit !found }' "$unit"
}
if [ "$ours" -eq 0 ] && runs_this_orender; then
    ours=1
    paths+=("$unit")
fi
if [ "$ours" -eq 1 ] && systemctl --user show-environment >/dev/null 2>&1; then
    systemctl --user disable --now omniphony-renderer 2>/dev/null || true
fi

for path in "${paths[@]}"; do
    case "$path" in
        */) rm -rf "${path:?}" ;;
        "$HOME/.local/bin"/*)
            # A command link the user has since replaced with a file or a link
            # of their own is theirs.
            case "$(readlink "$path" 2>/dev/null || true)" in
                "$root"/*) rm -f "$path" ;;
            esac ;;
        *)  rm -f "$path" ;;
    esac
done
if systemctl --user show-environment >/dev/null 2>&1; then
    systemctl --user daemon-reload || true
fi

rm -f "$manifest"
# Directories this installer made, when nothing else is left in them.
rmdir "$root" "$data_home/omniphony/lib" "$data_home/omniphony" 2>/dev/null || true

echo "Omniphony removed. Your settings are untouched:"
echo "  $config_home/omniphony/ (config.yaml), Studio's preferences, mpv.conf"
if [ -d "$data_home/omniphony/lib" ]; then
    echo "  $data_home/omniphony/lib/ still holds files this installer did not write"
    echo "  (a decoder bridge): remove them by hand if you no longer want them."
fi
