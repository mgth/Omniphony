#!/usr/bin/env bash
#
# One-command "clone-and-hear" demo for the Omniphony spatial renderer.
#
# Builds the reference WAV bridge and the orender CLI (if needed), then renders
# the bundled rotating 7.1.4 demo through the binaural headphone stage — no
# external player, no proprietary decoder.
#
# Runs on Linux, macOS and Windows (Git Bash / MSYS2). The audio device is the
# platform's realtime backend: PipeWire on Linux, CoreAudio on macOS, ASIO on
# Windows (the sound card's own driver, or FlexASIO / ASIO4ALL). A Windows
# machine without an ASIO driver falls back to WASAPI shared mode, which plays
# as many channels as the device's speaker setup: enough for the binaural
# mode; the speaker modes need a speaker setup as wide as the layout.
#
# Usage:
#   scripts/demo.sh                 # binaural → audio device (default)
#   scripts/demo.sh speakers        # 7.1.4 speaker render → audio device (no binaural)
#   scripts/demo.sh file            # binaural → ffplay (no audio device needed)
#
# The `file` mode pipes raw f32 stereo to ffplay:
#   orender ... --output-backend file --output-file - --output-file-format raw-f32 \
#     | ffplay -f f32le -ar 48000 -ac 2 -

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RENDERER_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"     # omniphony-renderer (cargo workspace root)
REPO_ROOT="$(cd "$RENDERER_DIR/.." && pwd)"      # repo root (holds layouts/)

case "$(uname -s)" in
  Linux)                BRIDGE_FILE=libreference_bridge.so;    EXE=;     BACKEND=pipewire ;;
  Darwin)               BRIDGE_FILE=libreference_bridge.dylib; EXE=;     BACKEND=coreaudio ;;
  MINGW*|MSYS*|CYGWIN*) BRIDGE_FILE=reference_bridge.dll;      EXE=.exe; BACKEND=asio ;;
  *) echo "[demo] unsupported platform: $(uname -s)" >&2; exit 2 ;;
esac

BRIDGE="$RENDERER_DIR/target/release/$BRIDGE_FILE"
ORENDER="$RENDERER_DIR/target/release/orender$EXE"
LAYOUT="$REPO_ROOT/layouts/7.1.4.yaml"
WAV="$RENDERER_DIR/assets/demo/spatial-demo.wav"
CONFIG="$RENDERER_DIR/assets/demo/demo.yaml"

MODE="${1:-binaural}"

# Run hermetically: isolate orender from any pre-existing config
# (~/.config/omniphony/config.yaml, %ProgramData%\omniphony\config.yaml on
# Windows). A machine already set up for live playback can otherwise inject an
# input/output mode that does not match this file-decode demo. A throwaway
# OMNIPHONY_CONFIG_DIR guarantees clean defaults on every platform.
DEMO_CONFIG_DIR="$(mktemp -d)"
trap 'rm -rf "$DEMO_CONFIG_DIR"' EXIT
if command -v cygpath >/dev/null; then
  export OMNIPHONY_CONFIG_DIR="$(cygpath -w "$DEMO_CONFIG_DIR")"   # native path for orender.exe
else
  export OMNIPHONY_CONFIG_DIR="$DEMO_CONFIG_DIR"
fi

echo "[demo] building reference bridge + orender (release) ..."
( cd "$RENDERER_DIR" && cargo build -r -p reference_bridge && cargo build -r -p omniphony-renderer )

if [[ ! -f "$WAV" ]]; then
  echo "[demo] generating demo asset ..."
  ( cd "$RENDERER_DIR" && cargo run -r -p reference_bridge --example gen_demo_wav )
fi

COMMON=(
  "$WAV"
  --bridge-path "$BRIDGE"
  --enable-vbap
  --speaker-layout "$LAYOUT"
)

case "$MODE" in
  binaural)
    echo "[demo] binaural → $BACKEND"
    "$ORENDER" "${COMMON[@]}" --config "$CONFIG" --output-backend "$BACKEND"
    ;;
  speakers)
    echo "[demo] 7.1.4 speaker render → $BACKEND"
    "$ORENDER" "${COMMON[@]}" --output-backend "$BACKEND"
    ;;
  file)
    echo "[demo] binaural → ffplay (no audio device needed)"
    "$ORENDER" "${COMMON[@]}" --config "$CONFIG" \
      --output-backend file --output-file - --output-file-format raw-f32 \
      | ffplay -hide_banner -autoexit -f f32le -ar 48000 -ac 2 -
    ;;
  *)
    echo "unknown mode: $MODE (expected: binaural | speakers | file)" >&2
    exit 2
    ;;
esac
