#!/usr/bin/env bash
# Install the native messaging host the extension streams IAMF to (Linux,
# Chrome and Chromium, current user).
#
#   host/install-host.sh --orender PATH [--config PATH] [--args "EXTRA ARGS"]
#
#   --orender   the orender binary to run, with a bridge that decodes IAMF
#               (harletty's libharletty_iamf_bridge.so, or a combined 0.8.x
#               bridge built with its `iamf` feature) named in its config's
#               render.bridge_paths
#   --config    config.yaml for that orender. Use an isolated copy: the host's
#               orender runs alongside the live one, so it must not take the
#               OSC port or the live input pipe (the host already passes
#               --no-osc).
#   --args      extra orender arguments, e.g. "--output-device NAME"
#
# Writes a launcher that fixes the command (nothing the page sends can change
# what runs) and the host manifest allowing only this extension's id.
set -euo pipefail

EXTENSION_ID=jkoonfghgdmdknbimiahfjfpclfmaflj
HOST_NAME=fr.mgth.omniphony.iamf
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

orender="" config="" args=""
while [ $# -gt 0 ]; do
  case "$1" in
    --orender) orender="$(realpath "${2:?}")"; shift 2;;
    --config)  config="$(realpath "${2:?}")"; shift 2;;
    --args)    args="${2:?}"; shift 2;;
    *) echo "unknown option $1" >&2; exit 2;;
  esac
done
if [ -z "$orender" ] || [ ! -x "$orender" ]; then
  echo "--orender must name an executable orender" >&2
  exit 2
fi

data="${XDG_DATA_HOME:-$HOME/.local/share}/omniphony"
mkdir -p "$data"
launcher="$data/omniphony-iamf-host"
cat > "$launcher" <<EOF
#!/bin/sh
# Written by $here/install-host.sh
export ORENDER='$orender'
export ORENDER_CONFIG='$config'
export ORENDER_ARGS='$args'
exec python3 '$here/omniphony_iamf_host.py' "\$@"
EOF
chmod +x "$launcher"

manifest=$(cat <<EOF
{
  "name": "$HOST_NAME",
  "description": "Omniphony: play IAMF streamed by the browser extension through orender",
  "path": "$launcher",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://$EXTENSION_ID/"]
}
EOF
)
for dir in "$HOME/.config/google-chrome/NativeMessagingHosts" "$HOME/.config/chromium/NativeMessagingHosts"; do
  if [ -d "$(dirname "$dir")" ]; then
    mkdir -p "$dir"
    printf '%s\n' "$manifest" > "$dir/$HOST_NAME.json"
    echo "installed $dir/$HOST_NAME.json"
  fi
done
echo "launcher: $launcher"
echo "logs: ${XDG_STATE_HOME:-$HOME/.local/state}/omniphony/iamf-host.log, iamf-orender.log"
