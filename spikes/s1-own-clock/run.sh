#!/usr/bin/env bash
# run.sh MODE SINK_SECS PLAY_SECS OUTDIR [sink args...] -- [play args...]
# Starts s1sink, then a follower (s1play), snapshots the links, waits, collects reports.
set -u
cd "$(dirname "$0")"
mode=$1; ss=$2; ps=$3; out=$4; shift 4
sargs=(); pargs=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do sargs+=("$1"); shift; done
[ $# -gt 0 ] && shift
pargs=("$@")
mkdir -p "$out"
target=rwspike-s1-$mode
timeout -s INT $((ss + 15)) ./s1sink -m "$mode" -d "$ss" -o "$out/sink-$mode.csv" "${sargs[@]}" \
	>"$out/sink-$mode.txt" 2>"$out/sink-$mode.err" &
spid=$!
sleep 2
if [ "$ps" -gt 0 ]; then
	timeout -s INT $((ps + 15)) ./s1play -t "$target" -d "$ps" "${pargs[@]}" \
		>"$out/play-$mode.txt" 2>"$out/play-$mode.err" &
	ppid_=$!
	sleep 3
	{ echo "--- links"; pw-link -l | grep -B2 -A2 rwspike; echo "--- nodes"; pw-cli ls Node | grep -B4 rwspike | grep -E "id |node.name"; } >"$out/links-$mode.txt" 2>&1
	wait $ppid_
fi
wait $spid
echo "done $mode"
