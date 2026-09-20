#!/usr/bin/env python3
"""End-to-end check of the fixed-channel placement modes over OSC.

Starts `orender render` on a fixed-channel bitstream (an Auro-3D carrier
extract is the natural one: its family defaults to the sphere), registers
as an OSC client, and records the fixed-channel object positions the
renderer broadcasts. Then switches the Auro family's mode over OSC —
room, then manual entries, then back to inherit — and records again.
Prints, per phase, the azimuth/elevation of a few channels derived from the
normalized ADM position; the room is forced to a cube, so a normalized
position is the rendered direction.

    scripts/placement_e2e.py <orender> <bridge .so> <stream.dts> <rx port> <tx port>

    scripts/placement_e2e.py omniphony-renderer/target/debug/orender \\
        ../harletty-bridge/target/debug/libharletty_bridge.so \\
        dumps/auro3d/demo2017_30s.dts 9021 9022

Expected on an Auro 13.1 carrier: sphere puts L at -30°, Ls at -110°, Lhs
at -110°/30°; room puts L at the corner (-45° in a cube), Ls at -90° and
Lhs on the wall above it (-90°/30°); manual renders the entries sent and
falls back to the room for the rest. See docs/placement.md.

No third-party module: the OSC encoder/decoder below covers what we need.
The renderer's config and data go to a throwaway XDG home next to this
script's log, so the run never touches the real configuration.
"""
import math
import os
import select
import socket
import struct
import subprocess
import sys
import time

ORENDER = sys.argv[1]
BRIDGE = sys.argv[2]
DTS = sys.argv[3]
RX_PORT = int(sys.argv[4])      # renderer listens here
TX_PORT = int(sys.argv[5])      # renderer broadcasts here (us)


def osc_pad(b: bytes) -> bytes:
    return b + b"\0" * (4 - len(b) % 4)


def osc_encode(addr: str, args) -> bytes:
    tags = ","
    payload = b""
    for a in args:
        if isinstance(a, str):
            tags += "s"
            payload += osc_pad(a.encode())
        elif isinstance(a, float):
            tags += "f"
            payload += struct.pack(">f", a)
        elif isinstance(a, int):
            tags += "i"
            payload += struct.pack(">i", a)
    return osc_pad(addr.encode()) + osc_pad(tags.encode()) + payload


def osc_decode(data: bytes):
    """Returns (addr, [args]) for a single message, or None."""
    if data.startswith(b"#bundle"):
        return None
    i = data.index(b"\0")
    addr = data[:i].decode(errors="replace")
    p = (i + 4) & ~3
    if p >= len(data) or data[p : p + 1] != b",":
        return addr, []
    j = data.index(b"\0", p)
    tags = data[p + 1 : j].decode()
    p = (j + 4) & ~3
    args = []
    for t in tags:
        if t == "f":
            args.append(struct.unpack(">f", data[p : p + 4])[0]); p += 4
        elif t == "i":
            args.append(struct.unpack(">i", data[p : p + 4])[0]); p += 4
        elif t == "h":
            args.append(struct.unpack(">q", data[p : p + 8])[0]); p += 8
        elif t == "d":
            args.append(struct.unpack(">d", data[p : p + 8])[0]); p += 8
        elif t == "s":
            k = data.index(b"\0", p)
            args.append(data[p:k].decode(errors="replace")); p = (k + 4) & ~3
        elif t in "TFN":
            args.append(t)
        else:
            break
    return addr, args


def angles(x, y, z):
    az = math.degrees(math.atan2(x, y))
    el = math.degrees(math.atan2(z, math.hypot(x, y)))
    return az, el


RENDERER = None


def collect(sock, seconds, positions):
    """Record the latest position per fixed-channel label for `seconds`,
    keeping the registration alive with a heartbeat."""
    end = time.time() + seconds
    beat = 0.0
    while time.time() < end:
        if time.time() - beat > 2.0:
            sock.sendto(osc_encode("/omniphony/heartbeat", []), RENDERER)
            beat = time.time()
        r, _, _ = select.select([sock], [], [], 0.2)
        if not r:
            continue
        data, _ = sock.recvfrom(65535)
        m = osc_decode(data)
        if not m:
            continue
        addr, args = m
        if addr.startswith("/omniphony/object/") and (addr.endswith("/xyz") or addr.endswith("/aed")):
            if len(args) >= 9 and isinstance(args[8], str) and args[8]:
                positions[args[8]] = (addr.rsplit("/", 1)[1], args[0], args[1], args[2])
        elif addr == "/omniphony/state/renderer" and args and isinstance(args[0], str):
            positions["__snapshot__"] = args[0]


def report(title, positions, labels):
    print(f"\n== {title}")
    for label in labels:
        p = positions.get(label)
        if not p:
            print(f"  {label:<4} (not reported)")
            continue
        kind, x, y, z = p
        if kind == "aed":
            print(f"  {label:<4} az {x:7.1f}  el {y:6.1f}  (polar wire)")
        else:
            az, el = angles(x, y, z)
            print(f"  {label:<4} az {az:7.1f}  el {el:6.1f}   xyz ({x:+.3f}, {y:+.3f}, {z:+.3f})")


def main():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", TX_PORT))
    env = dict(os.environ)
    scratch = os.path.join(os.environ.get("TMPDIR", "/tmp"), "orender-placement-e2e")
    env["XDG_DATA_HOME"] = scratch
    env["XDG_CONFIG_HOME"] = scratch
    os.makedirs(env["XDG_DATA_HOME"], exist_ok=True)
    cmd = [
        ORENDER, "render", DTS,
        "--bridge-path", BRIDGE,
        "--speaker-layout", "/home/user/dev/spatial-renderer/workflows/channel-poses/omniphony/layouts/7.1.4.yaml",
        "--room-ratio", "1,1,1",
        "--osc", "--osc-host", "127.0.0.1", "--osc-port", str(TX_PORT),
        "--osc-rx-port", str(RX_PORT),
        "--output-backend", "file", "--output-file", "/dev/null", "--enable-vbap",
        "--continuous",
    ]
    os.makedirs(scratch, exist_ok=True)
    log = open(os.path.join(scratch, "orender.log"), "w")
    proc = subprocess.Popen(cmd, env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        global RENDERER
        renderer = ("127.0.0.1", RX_PORT)
        RENDERER = renderer
        # Register as a client (the port we listen on), then keep alive.
        time.sleep(2.0)
        for _ in range(3):
            sock.sendto(osc_encode("/omniphony/register", [TX_PORT]), renderer)
            time.sleep(0.3)
        labels = ["L", "Ls", "Lb", "Lh", "Lhs", "Ch", "TC", "LFE"]

        positions = {}
        collect(sock, 8.0, positions)
        snap = positions.get("__snapshot__", "")
        fam = "auro" if '"family":"auro"' in snap.replace(" ", "") else "?"
        print(f"snapshot family: {fam}")
        report("Auro, built-in default (sphere)", positions, labels)

        sock.sendto(osc_encode("/omniphony/control/placement/mode", ["auro", "room"]), renderer)
        positions = {}
        collect(sock, 5.0, positions)
        report("Auro, mode room", positions, labels)

        layout = ('{"radius_m":1.0,"speakers":[{"name":"Ls","coord_mode":"polar","azimuth":-135.0,'
                  '"elevation":0.0,"distance":1.0},{"name":"LFE","coord_mode":"cartesian","x":0,"y":1,'
                  '"z":0,"spatialize":false,"gain_db":-6.0}]}')
        sock.sendto(osc_encode("/omniphony/control/placement/layout", ["auro", layout]), renderer)
        sock.sendto(osc_encode("/omniphony/control/placement/mode", ["auro", "manual"]), renderer)
        positions = {}
        collect(sock, 5.0, positions)
        report("Auro, mode manual (Ls entry at -135, others fall back to room)", positions, labels)

        sock.sendto(osc_encode("/omniphony/control/placement/mode", ["auro", "inherit"]), renderer)
        sock.sendto(osc_encode("/omniphony/control/placement/layout", ["auro", ""]), renderer)
        positions = {}
        collect(sock, 5.0, positions)
        report("Auro, back to inherit (built-in sphere)", positions, labels)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()


if __name__ == "__main__":
    main()
