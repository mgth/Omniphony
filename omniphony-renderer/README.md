# omniphony-renderer

![omniphony-renderer preview](omniphony-renderer.png)

`omniphony-renderer` is the realtime decode and spatial rendering engine of the Omniphony suite.

The main executable is `orender`.

It loads a bridge plugin at runtime, decodes the input stream, and can then:

- stream decoded audio to realtime backends
- output through `pipewire` on Linux or `asio` on Windows (WASAPI shared mode
  when no ASIO driver is installed)
- emit OSC metadata and metering under the `/omniphony/...` namespace
- render objects to speaker feeds with VBAP

The repository also contains the supporting runtime stack:

- `renderer`: VBAP engine, speaker layouts, OSC output, runtime config
- `audio_output`: PipeWire, ASIO (with its WASAPI fallback) and CoreAudio backends
- `spdif`: IEC61937 / S/PDIF parsing helpers
- `bridge_api`: ABI-stable interface for external bridge plugins
- `reference_bridge`: a reference WAV/PCM bridge that powers the bundled demo
- `sys`: platform integration, including Windows service support
- `diag`: diagnostic-metric registry published to the Studio diag plot
- `live_log`: process logger with a runtime-adjustable level and a buffer of recent records streamed over OSC

## Status

`omniphony-renderer` is still an engineering build. The CLI, rendering path, config system and platform backends are usable, but the project should still be treated as alpha.

## Build

```bash
cargo build --release
```

builds `orender` with the platform's realtime backend (PipeWire, ASIO or
CoreAudio) and the native VBAP backend; no feature flag is needed. The system
packages each platform needs, the engine library, the reference bridge and the
optional SAF-backed VBAP are in [QUICKSTART.md](QUICKSTART.md#2-build).

## Runtime Model

`omniphony-renderer` does not hardcode a single container or codec frontend in the binary itself. Decoding is delegated to a bridge plugin loaded at runtime.

Bridge lookup order:

1. `--bridge-path <FILE>`
2. `render.bridge_path` in the config file
3. else auto-discovery:
   1. `$ORENDER_BRIDGE_FILE`, when it names an existing file: that exact
      bridge (Studio sets it to the bridge `mpv.conf` names for
      mpv-omniphony). A value naming no file is logged and skipped;
   2. else the first `*_bridge.so`, `*_bridge.dll` or `*_bridge.dylib`
      (alphabetical within a folder) in, in this order (see [BRIDGE_API.md](BRIDGE_API.md#loading-model)):
      1. the folder of the host executable (`orender`, or the player that loads
         liborender, e.g. mpv-omniphony);
      2. `$ORENDER_BRIDGE_DIR`;
      3. the per-user engine folder, where Studio deploys liborender and
         mpv-omniphony's loader looks for it: `$XDG_DATA_HOME/omniphony/lib`
         (default `~/.local/share/omniphony/lib`) on Linux,
         `~/Library/Application Support/omniphony/lib` on macOS,
         `%LOCALAPPDATA%\omniphony\lib` on Windows;
      4. the system plugin folder, `/usr/lib/orender` on Unix (where the AUR's
         `harletty-bridge` installs it; packagers override it with
         `ORENDER_BRIDGE_DIR` at build time). None on Windows.

A path named in 1 or 2 must exist: it is never replaced by a discovered one.
When nothing is named and nothing is found, `orender` still starts, without a
decoder: PCM and channel input work, and the published
`/omniphony/state/render/bridge_error` contains `no decoder bridge found`,
which Studio shows as a warning rather than an error.

The repo ships a **reference bridge** (`reference_bridge/`, built as
`libreference_bridge.so`) that reads a plain multichannel WAV. It powers the
one-command demo — `./scripts/demo.sh`, see [QUICKSTART.md](QUICKSTART.md) — and is
the smallest example for writing your own bridge ([BRIDGE_API.md](BRIDGE_API.md)).

## Commands

`orender` currently exposes these commands:

- default command: render an input stream to a realtime backend
- `generate-vbap`: generate a binary VBAP table from a speaker layout
- `list-asio-devices`: list the realtime output devices on Windows builds — the
  ASIO ones, or the WASAPI ones when output falls back to WASAPI

Inspect the exact CLI supported by your build with:

```bash
orender --help
```

## Typical Usage

The quickest check is `./scripts/demo.sh` (see [QUICKSTART.md](QUICKSTART.md)).
The examples below use the bundled demo clip + the reference bridge so each one is
runnable as-is; swap in your own input and bridge the same way.

```bash
# Decode from stdin
cat assets/demo/spatial-demo.wav | orender - --bridge-path target/release/libreference_bridge.so

# Linux realtime output via PipeWire
orender assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.so \
  --output-backend pipewire

# Enable VBAP rendering and OSC output
orender assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.so \
  --enable-vbap \
  --speaker-layout ../layouts/7.1.4.yaml \
  --osc \
  --osc-host 127.0.0.1 \
  --osc-port 9000

# Select a non-VBAP render backend and its parameters (CLI parity with Studio/OSC)
orender assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.so \
  --enable-vbap \
  --render-backend hybrid \
  --hybrid-external-backend vbap --hybrid-internal-backend barycenter \
  --hybrid-metric chebyshev \
  --distance-model-metric chebyshev \
  --size-to-spread-mode mean
```

The render backend (`--render-backend vbap|barycenter|experimental_distance|hybrid`),
its per-backend parameters (`--barycenter-localize`, `--hybrid-*`,
`--experimental-distance-*`), the distance metrics (`--distance-model-metric`,
`--distance-diffuse-metric`), `--size-to-spread-mode` and the adaptive-resampling
PI tuning (`--adaptive-resampling-*`) are all exposable on the CLI as well as via
OSC/Studio. See `docs/option-surface-parity.md` for the full per-surface
parity matrix. Run `orender render --help` for the complete flag list.

## Binaural Headphone Output

Besides the speaker/VBAP path, the renderer has an independent binaural stage
for headphones: objects and beds are rendered straight to stereo through a
measured HRTF (embedded KEMAR, or a SOFA file), with ITD, shoebox early
reflections for externalization, and live head tracking over OSC (e.g. the
Sensors2OSC Android app — use its *Game Rotation Vector* sensor). Enable it
with `render.binaural.output_mode: binaural` in the config, then drive
everything live from the Studio panel or OSC.

See [BINAURAL.md](BINAURAL.md) for setup, tuning tips and the full control
surface.

## Configuration

Global and render settings are loaded from a YAML config file.

Default path:

- Linux and macOS: `~/.config/omniphony/config.yaml`
- Windows: `%ProgramData%\\omniphony\\config.yaml` (machine-wide, so the user-mode renderer and a service share one file)

You can point to another file with `--config`, and persist the current effective settings with `--save-config`.

## Repository Pointers

- [BINAURAL.md](BINAURAL.md): binaural headphone output, head tracking, tuning
- [OSC_PROTOCOL.md](OSC_PROTOCOL.md): OSC session handshake and streams
- [../docs/osc-control-contract.md](../docs/osc-control-contract.md): every OSC control and state address
- [QUICKSTART.md](QUICKSTART.md): build (per platform), demo and local bring-up
- [../layouts/README.md](../layouts/README.md): speaker layout format
- [BRIDGE_API.md](BRIDGE_API.md): runtime bridge ABI
