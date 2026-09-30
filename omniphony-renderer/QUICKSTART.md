# Quickstart

The shortest path to *hearing* `omniphony-renderer` work — then to running it on
your own input.

## 1. Hear the demo (one command)

From a fresh checkout, with no audio file to find and no decoder to install:

```bash
cd omniphony-renderer
./scripts/demo.sh            # builds the engine + reference bridge, then plays the demo
```

This binaurally renders `assets/demo/spatial-demo.wav` — a source sweeping around
you with an overhead tone — straight to your headphones, with no media player and
no proprietary decoder. Other modes:

```bash
./scripts/demo.sh speakers   # 7.1.4 speaker render instead of binaural
./scripts/demo.sh file       # no audio device? pipe raw float to ffplay
```

Everything below explains what that script does and how to run the engine on your
own input. The commands assume you are in `omniphony-renderer/`.

## 2. Build

Minimal build:

```bash
cargo build --release
```

Linux with PipeWire:

```bash
cargo build --release --features pipewire
```

Linux or Windows with runtime VBAP generation:

```bash
export SAF_ROOT="/path/to/Spatial_Audio_Framework"
cargo build --release --features saf_vbap
```

`saf_vbap` enables runtime VBAP generation via
[`Spatial_Audio_Framework` (SAF)](https://github.com/leomccormack/Spatial_Audio_Framework),
not the separate [`SPARTA`](https://leomccormack.github.io/sparta-site/) plug-in suite.

Windows with ASIO:

```bash
cargo build --release --features asio
```

## 3. The bridge model

`orender` does not decode formats in the binary itself: it loads a **bridge
plugin** at runtime that turns your input into PCM + object metadata. Bridge
lookup order:

1. `--bridge-path <FILE>`
2. `render.bridge_path` in the config file
3. the first `lib*_bridge.{so,dll,dylib}` next to the executable

The repo ships a **reference bridge** (`reference_bridge/`) that reads a plain
multichannel WAV — it is what the demo uses, and the smallest example for writing
your own (see [BRIDGE_API.md](BRIDGE_API.md)). The release build produces
`target/release/libreference_bridge.so`. For other formats, point `--bridge-path`
at the matching bridge instead (e.g. a separately-packaged decoder bridge).

## 4. Decode a file

Render the demo clip explicitly (this is what `demo.sh speakers` runs):

```bash
./target/release/orender assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.so \
  --enable-vbap \
  --speaker-layout ../layouts/7.1.4.yaml
```

Read from stdin instead of a file:

```bash
cat assets/demo/spatial-demo.wav | ./target/release/orender - \
  --bridge-path target/release/libreference_bridge.so \
  --enable-vbap --speaker-layout ../layouts/7.1.4.yaml
```

Swap in your own WAV (or your own input + bridge) the same way.

## 5. Binaural headphones

Add a config that enables the binaural stage (the demo ships one,
`assets/demo/demo.yaml`):

```bash
./target/release/orender assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.so \
  --enable-vbap --speaker-layout ../layouts/7.1.4.yaml \
  --config assets/demo/demo.yaml \
  --output-backend pipewire
```

See [BINAURAL.md](BINAURAL.md) for HRTF/SOFA, externalization and live head tracking.

## 6. Precompute a VBAP table

```bash
./target/release/orender generate-vbap \
  --speaker-layout ../layouts/7.1.4.yaml \
  --output 7.1.4.vbap \
  --az-res 2 --el-res 2 --spread-res 0.25
```

Then reuse it instead of generating at startup:

```bash
./target/release/orender assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.so \
  --enable-vbap --vbap-table ./7.1.4.vbap
```

## 7. OSC (metadata + Studio)

```bash
./target/release/orender assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.so \
  --osc --osc-host 127.0.0.1 --osc-port 9000
```

See [OSC_PROTOCOL.md](OSC_PROTOCOL.md) for the session handshake and streams, and
[`docs/osc-control-contract.md`](../docs/osc-control-contract.md) for every
control and state address.

## 8. Realtime (and file) output

Linux / PipeWire:

```bash
./target/release/orender assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.so \
  --enable-vbap --speaker-layout ../layouts/7.1.4.yaml \
  --output-backend pipewire --output-device omniphony_router
```

Write to a file or pipe instead of a device (non-realtime):

```bash
./target/release/orender assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.so \
  --enable-vbap --speaker-layout ../layouts/7.1.4.yaml \
  --output-backend file --output-file out.f32 --output-file-format raw-f32
```

Windows / ASIO:

```powershell
.\target\release\orender.exe list-asio-devices
.\target\release\orender.exe assets\demo\spatial-demo.wav `
  --bridge-path target\release\reference_bridge.dll `
  --output-backend asio --output-device "Your ASIO Device"
```

## 9. Configuration file

Default config path:

- Linux: `~/.config/omniphony/config.yaml`
- Windows: `%ProgramData%\omniphony\config.yaml` (machine-wide; shared by user-mode and the service)

Save the current effective configuration:

```bash
./target/release/orender --config ./config.yaml --save-config \
  assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.so \
  --enable-vbap --speaker-layout ../layouts/7.1.4.yaml --osc
```

## Next references

- [README.md](README.md)
- [BUILD.md](BUILD.md)
- [BUILDING_WINDOWS.md](BUILDING_WINDOWS.md)
- [BINAURAL.md](BINAURAL.md)
- [OSC_PROTOCOL.md](OSC_PROTOCOL.md)
- [BRIDGE_API.md](BRIDGE_API.md)
- [../layouts/README.md](../layouts/README.md)
