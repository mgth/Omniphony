#!/usr/bin/env python3
"""Native messaging host: plays the IAMF stream the extension sends through orender.

Chrome starts this process when the extension connects and talks to it over
stdin/stdout (native messaging: a 32-bit native-endian length, then UTF-8
JSON). The host runs its own orender, reading the raw IAMF stream on its
standard input, and writes what the extension sends into it:

  {"type": "start"}                 start orender (no-op when running)
  {"type": "config", "data": b64}   the descriptor OBUs of an IA sequence
  {"type": "data", "data": b64}     temporal units, in presentation order
  {"type": "flush"}                 a seek: restart orender, re-send the config
  {"type": "stop"}                  stop orender

and answers with {"type": "host", "state": "running" | "stopped" | "error", ...}.

The orender command is fixed by the environment the installer wrote into the
launcher (ORENDER, ORENDER_CONFIG, ORENDER_ARGS): nothing a page sends can
change what runs. Only standard library.
"""

import base64
import json
import os
import shlex
import struct
import subprocess
import sys
import time
from pathlib import Path

ORENDER = os.environ.get("ORENDER", "orender")
ORENDER_CONFIG = os.environ.get("ORENDER_CONFIG", "")
# Extra arguments, e.g. `--output-backend file --output-file …` for tests.
ORENDER_ARGS = shlex.split(os.environ.get("ORENDER_ARGS", ""))
LOG_DIR = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state")) / "omniphony"


def log(message: str) -> None:
    # stdout is the protocol channel: everything else goes to the log file.
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    with open(LOG_DIR / "iamf-host.log", "a", encoding="utf-8") as f:
        f.write(f"{time.strftime('%Y-%m-%dT%H:%M:%S')} {message}\n")


def read_message():
    header = sys.stdin.buffer.read(4)
    if len(header) < 4:
        return None
    (length,) = struct.unpack("=I", header)
    return json.loads(sys.stdin.buffer.read(length))


def send(message: dict) -> None:
    body = json.dumps(message).encode()
    try:
        sys.stdout.buffer.write(struct.pack("=I", len(body)) + body)
        sys.stdout.buffer.flush()
    except (BrokenPipeError, OSError):
        # Chrome already closed the channel (shutting down): nobody to tell.
        pass


class Player:
    def __init__(self):
        self.process = None
        # Descriptors of the current sequence: the first bytes a (re)started
        # orender must read, or its bridge cannot recognise the stream.
        self.config = None
        self.bytes_written = 0

    def command(self):
        cmd = [ORENDER, "render", "--no-osc", "--no-continuous", "--enable-vbap"]
        if ORENDER_CONFIG:
            cmd += ["--config", ORENDER_CONFIG]
        return cmd + ORENDER_ARGS + ["-"]

    def start(self):
        if self.process and self.process.poll() is None:
            return
        cmd = self.command()
        log(f"start: {shlex.join(cmd)}")
        stderr = open(LOG_DIR / "iamf-orender.log", "ab")
        self.process = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=stderr
        )
        self.bytes_written = 0
        if self.config:
            self.write(self.config)
        self.report()

    def stop(self):
        if not self.process:
            return
        log(f"stop: pid {self.process.pid}")
        try:
            self.process.stdin.close()
        except OSError:
            pass
        self.process.terminate()
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.process = None
        self.report()

    def write(self, data: bytes):
        if not self.process:
            return
        try:
            self.process.stdin.write(data)
            self.process.stdin.flush()
            self.bytes_written += len(data)
        except (BrokenPipeError, OSError) as err:
            code = self.process.poll()
            log(f"orender stopped reading ({err}), exit {code}")
            self.process = None
            send({"type": "host", "state": "error", "error": f"orender exited ({code}), see iamf-orender.log"})

    def report(self):
        running = bool(self.process and self.process.poll() is None)
        send(
            {
                "type": "host",
                "state": "running" if running else "stopped",
                "pid": self.process.pid if running else None,
                "bytes": self.bytes_written,
            }
        )


def main():
    player = Player()
    log(f"host up, orender={ORENDER} config={ORENDER_CONFIG or '(default)'}")
    try:
        while (message := read_message()) is not None:
            kind = message.get("type")
            if kind == "start":
                player.start()
            elif kind == "config":
                player.config = base64.b64decode(message["data"])
                player.write(player.config)
            elif kind == "data":
                # Units before any descriptors would be read as an unknown
                # stream: they wait for the sequence instead.
                if player.config:
                    player.write(base64.b64decode(message["data"]))
            elif kind == "flush":
                player.stop()
                player.start()
            elif kind == "stop":
                player.stop()
            elif kind == "status":
                player.report()
    finally:
        # The extension disconnected (tab closed, live mode off, browser
        # quit): nothing may keep playing.
        player.stop()
        log("host down")


if __name__ == "__main__":
    main()
