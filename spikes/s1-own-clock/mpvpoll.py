#!/usr/bin/env python3
"""Poll mpv's audio-pts over IPC and report the clock mpv derives from the sink.

offset(t) = t_monotonic - audio_pts(t) is the CLOCK_MONOTONIC instant mpv believes
pts 0 is (or was) heard. On a clean source clock it is constant; its slope is the
rate error mpv sees and its residual is the noise mpv's A/V sync would chase.
Writes offsets to a CSV and prints stats. Throwaway spike code.
"""
import json, os, socket, statistics, sys, time

sock_path, duration, out_csv = sys.argv[1], float(sys.argv[2]), sys.argv[3]
deadline = time.monotonic() + 20
while True:
    try:
        s = socket.socket(socket.AF_UNIX)
        s.connect(sock_path)
        break
    except OSError:
        if time.monotonic() > deadline:
            sys.exit("no mpv socket")
        time.sleep(0.1)
f = s.makefile("rwb")
rows = []
t_end = time.monotonic() + duration
rid = 0
while time.monotonic() < t_end:
    rid += 1
    t0 = time.monotonic_ns()
    f.write((json.dumps({"command": ["get_property", "audio-pts"], "request_id": rid}) + "\n").encode())
    f.flush()
    while True:
        msg = json.loads(f.readline())
        if msg.get("request_id") == rid:
            break
    t1 = time.monotonic_ns()
    if msg.get("error") == "success" and msg.get("data") is not None:
        rows.append(((t0 + t1) / 2, (t1 - t0), float(msg["data"])))
    time.sleep(0.02)
with open(out_csv, "w") as o:
    o.write("t_ns,rtt_ns,audio_pts\n")
    for r in rows:
        o.write("%d,%d,%.9f\n" % r)
rows = [r for r in rows if r[2] > 2.0]  # skip start-up
if len(rows) < 10:
    sys.exit("too few samples: %d" % len(rows))
t = [r[0] / 1e9 for r in rows]
off = [r[0] / 1e9 - r[2] for r in rows]
n = len(t)
mt, mo = sum(t) / n, sum(off) / n
sxx = sum((x - mt) ** 2 for x in t)
b = sum((x - mt) * (y - mo) for x, y in zip(t, off)) / sxx
res = [y - (mo + b * (x - mt)) for x, y in zip(t, off)]
print("mpv audio-pts samples=%d span=%.1fs" % (n, t[-1] - t[0]))
print("  offset (t - audio_pts) first=%.6f s, slope=%+.3f ppm" % (off[0], b * 1e6))
print("  residual std=%.1f us p-p=%.1f us (IPC rtt median %.1f us)" % (
    statistics.pstdev(res) * 1e6, (max(res) - min(res)) * 1e6,
    statistics.median(r[1] for r in rows) / 1e3))
print("OFFSET0_NS=%d" % int(off[0] * 1e9))
