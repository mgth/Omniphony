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

# The service first, if this installer wrote its unit: stopped and disabled
# before its binary goes.
for path in "${paths[@]}"; do
    if [ "$path" = "$unit" ] && systemctl --user show-environment >/dev/null 2>&1; then
        systemctl --user disable --now omniphony-renderer 2>/dev/null || true
    fi
done

for path in "${paths[@]}"; do
    case "$path" in
        */) rm -rf "${path:?}" ;;
        "$HOME/.local/bin"/*)
            # A command link the user has since replaced with a file of their
            # own is theirs.
            [ ! -L "$path" ] || rm -f "$path" ;;
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
