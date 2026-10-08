#!/usr/bin/env bash
# Runs one S2 capture (silence) against the USB hardware sink and snapshots the
# Latency params of the spike's port and the sink's input port.
set -u
Q=$1; SECS=$2; TAG=${3:-}
SINK=alsa_output.usb-Generic_USB_Audio-00.HiFi_5_1__Speaker__sink
cd "$(dirname "$0")"
mkdir -p data
if pw-link -l 2>/dev/null | grep -A1 "^$SINK:playback" | grep -q "|<-"; then
  echo "ABORT: something is already linked to $SINK"; exit 1
fi
timeout -s INT $((SECS+20)) ./target/release/s2-output-timestamps "$SINK" "$Q" "$SECS" "data/q${Q}${TAG}.csv" > "data/q${Q}${TAG}.log" 2>&1 &
PID=$!
sleep 6
pw-dump 2>/dev/null | python3 -c '
import json,sys
objs=json.load(sys.stdin)
nodes={o["id"]:o["info"]["props"].get("node.name","") for o in objs if o.get("type")=="PipeWire:Interface:Node" and o.get("info")}
for o in objs:
    if o.get("type")!="PipeWire:Interface:Port" or not o.get("info"): continue
    nid=o["info"]["props"].get("node.id")
    n=nodes.get(nid,"")
    if ("rwspike-s2" in n) or (n.endswith("Speaker__sink") and not o["info"]["props"].get("port.monitor")=="true"):
        lat=o["info"].get("params",{}).get("Latency")
        print(n, o["info"]["props"].get("port.name"), json.dumps(lat))
' | sort -u > "data/q${Q}${TAG}.latency.txt"
pw-link -l 2>/dev/null | grep -A3 "^$SINK:playback" > "data/q${Q}${TAG}.links.txt"
timeout 3 pw-top -b -n 2 2>/dev/null | tail -40 | grep -E "Speaker|rwspike-s2" >> "data/q${Q}${TAG}.links.txt"
wait $PID
echo "exit=$?"
echo "leftover rwspike-s2 nodes: $(pw-cli ls Node | grep -c rwspike-s2)"
