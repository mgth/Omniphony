#!/usr/bin/env bash
# runmpv.sh MODE SECS OUTDIR MEDIA [sink args...] -- [extra mpv args...]
# Starts s1sink, then a headless mpv (no video, no window, no config) into it,
# polls audio-pts over IPC, snapshots links, collects everything.
set -u
cd "$(dirname "$0")"
mode=$1; secs=$2; out=$3; media=$4; shift 4
sargs=(); margs=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do sargs+=("$1"); shift; done
[ $# -gt 0 ] && shift
margs=("$@")
mkdir -p "$out"
tag=$mode-$(basename "$media" | tr . _)
sock=".mpv-$tag.sock"  # relative: AF_UNIX paths are limited to 108 bytes
rm -f "$sock"
MPV=${MPV:-/usr/bin/mpv}
timeout -s INT $((secs + 25)) ./s1sink -m "$mode" -d $((secs + 8)) -o "$out/sink-$tag.csv" "${sargs[@]}" \
	>"$out/sink-$tag.txt" 2>"$out/sink-$tag.err" &
spid=$!
sleep 2
PIPEWIRE_PROPS='{ node.dont-fallback = true node.dont-reconnect = true }' \
	timeout -s INT $((secs + 10)) "$MPV" --no-config --no-video --vo=null --no-terminal \
	--ao=pipewire --audio-device="pipewire/rwspike-s1-$mode" \
	--input-ipc-server="$sock" --log-file="$out/mpv-$tag.log" --msg-level=all=v \
	--length=$secs "${margs[@]}" "$media" &
mpid=$!
sleep 4
{ echo "--- links"; pw-link -l | grep -B2 -A2 rwspike-s1; } >"$out/links-$tag.txt" 2>&1
python3 mpvpoll.py "$sock" $((secs - 6)) "$out/mpvpoll-$tag.csv" >"$out/mpvpoll-$tag.txt" 2>&1
wait $mpid
wait $spid
rm -f "$sock"
echo "done $tag"
