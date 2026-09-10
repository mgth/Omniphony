#!/usr/bin/env bash
# Phase 0 gate measurement for the egui spike.
#
# Runs the release binary with a synthetic feed that stops after FEED_SECS,
# samples process CPU (utime+stime from /proc) and RSS during the feed and
# again once the window has gone quiet, and keeps the stats lines the app
# prints. Optionally takes a screenshot while the feed runs.
#
#   scripts/measure.sh [objects] [rate_hz] [feed_secs]
#
# Output: measure-<timestamp>.log next to this script and a summary on stdout.
set -euo pipefail

OBJECTS="${1:-64}"
RATE="${2:-100}"
FEED_SECS="${3:-20}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE="$(cd "$HERE/.." && pwd)"
BIN="$CRATE/target/release/omniphony-studio-egui"
STAMP="$(date +%Y%m%d-%H%M%S)"
LOG="$HERE/measure-$STAMP.log"
SHOT="$HERE/measure-$STAMP.png"
CLK="$(getconf CLK_TCK)"

[ -x "$BIN" ] || { echo "build first: cargo build --release" >&2; exit 1; }

cpu_ticks() { awk '{print $14+$15}' "/proc/$1/stat" 2>/dev/null || echo 0; }
rss_mb()    { awk '/VmRSS/{printf "%d", $2/1024}' "/proc/$1/status" 2>/dev/null || echo 0; }

cd "$CRATE"
"$BIN" --synthetic "$OBJECTS" --rate "$RATE" --synthetic-stop-after "$FEED_SECS" \
       --stats-interval 1 >"$LOG" 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT

sleep 5   # window up, feed running
T0=$(cpu_ticks $PID); R0=$(rss_mb $PID)
sleep 10
T1=$(cpu_ticks $PID); R1=$(rss_mb $PID)
LOAD_CPU=$(( (T1 - T0) * 100 / CLK / 10 ))

if command -v spectacle >/dev/null 2>&1; then
  spectacle -b -n -a -o "$SHOT" >/dev/null 2>&1 || true
fi

# Wait until the feed has stopped, plus a settling margin.
REMAIN=$(( FEED_SECS - 15 + 4 ))
[ "$REMAIN" -gt 0 ] && sleep "$REMAIN"
T2=$(cpu_ticks $PID); R2=$(rss_mb $PID)
sleep 10
T3=$(cpu_ticks $PID); R3=$(rss_mb $PID)
IDLE_TICKS=$(( T3 - T2 ))
THREADS=$(awk '/Threads/{print $2}' "/proc/$PID/status")

kill $PID 2>/dev/null || true
wait $PID 2>/dev/null || true
trap - EXIT

FPS_LINES=$(grep -c '^stats ' "$LOG" || true)
FPS_UNDER_LOAD=$(grep '^stats ' "$LOG" | sed -n '6,15p' | sed -E 's/.*fps=([0-9.]+).*/\1/' | awk '{s+=$1; n++} END {if (n) printf "%.1f", s/n; else print "n/a"}')
FRAME_MS=$(grep '^stats ' "$LOG" | sed -n '6,15p' | sed -E 's/.*frame_ms=([0-9.]+).*/\1/' | awk '{s+=$1; n++} END {if (n) printf "%.2f", s/n; else print "n/a"}')
PKT=$(grep '^stats ' "$LOG" | sed -n '6,15p' | sed -E 's/.*osc_pkt_s=([0-9.]+).*/\1/' | awk '{s+=$1; n++} END {if (n) printf "%.0f", s/n; else print "n/a"}')

{
  echo "objects=$OBJECTS rate_hz=$RATE feed_secs=$FEED_SECS"
  echo "under load (t=5..15s): fps=$FPS_UNDER_LOAD frame_ms=$FRAME_MS osc_pkt_s=$PKT process_cpu=${LOAD_CPU}% rss=${R0}..${R1}MB"
  echo "idle (feed stopped, 10 s window): cpu_ticks=$IDLE_TICKS (=$(( IDLE_TICKS * 100 / CLK / 10 ))% of one core) rss=${R2}..${R3}MB threads=$THREADS"
  echo "stats lines: $FPS_LINES  log: $LOG"
  [ -f "$SHOT" ] && echo "screenshot: $SHOT"
} | tee -a "$LOG"
