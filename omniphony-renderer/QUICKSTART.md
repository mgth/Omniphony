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

Rust 1.89 or newer (the workspace's `rust-version`). The same commands on
every platform, from `omniphony-renderer/`:

```bash
cargo build --release                        # orender, the CLI
cargo build --release -p reference_bridge    # the WAV bridge the demo uses
cargo build --release -p orender_ffi         # liborender, the engine library mpv loads
```

(`cargo build --release --workspace` builds all three and the rest of the
workspace.) Nothing needs a feature flag: the platform's realtime output
backend (PipeWire on Linux, ASIO on Windows with a WASAPI fallback, CoreAudio on macOS) and the
native VBAP backend (pure Rust, no external library) are in the default build.
The `pipewire` and `asio` features Cargo still accepts are empty aliases kept
for old scripts.

### Linux

```bash
# Debian / Ubuntu
sudo apt install build-essential pkg-config clang libclang-dev libpipewire-0.3-dev
# Arch
sudo pacman -S base-devel clang pipewire
```

PipeWire 0.3.65 or newer is needed at build time (`pipewire-rs` 0.9); Ubuntu
24.04's own package is fine (the arm64 CI job uses it). Ubuntu 22.04 ships
0.3.48: add the `pipewire-debian/pipewire-upstream` PPA, as the x64 CI job does
(`.github/workflows/ci.yml`).

### Windows

- Visual Studio 2022 (or the Build Tools) with the C++ desktop workload, and the
  `x86_64-pc-windows-msvc` Rust toolchain.
- LLVM/Clang, for the bindings the ASIO backend generates.
- The Steinberg ASIO SDK, which `cpal` compiles in. It is available under GPLv3,
  the licence this project carries, from <https://github.com/audiosdk/asio>;
  point `CPAL_ASIO_DIR` at the checkout before building:

```powershell
git clone https://github.com/audiosdk/asio C:\dev\asio_sdk
$env:CPAL_ASIO_DIR = "C:\dev\asio_sdk"
cargo build --release
```

CI pins the SDK commit in `.github/actions/setup-asio-sdk/action.yml`.

### macOS

The Xcode command line tools (`xcode-select --install`) are all the build
needs; the CoreAudio backend uses the system frameworks. Apple Silicon is the
platform CI builds and tests (`macos-14`).

### Optional: SAF-backed VBAP

`saf_vbap` adds a second VBAP implementation from
[`Spatial_Audio_Framework` (SAF)](https://github.com/leomccormack/Spatial_Audio_Framework)
(not the separate [`SPARTA`](https://leomccormack.github.io/sparta-site/)
plug-in suite). The default native backend does not need it. It links SAF and
OpenBLAS/LAPACKE, which you build yourself; this repository does not bundle
or redistribute either, so check SAF's licence terms for your build.

```bash
export SAF_ROOT="/path/to/Spatial_Audio_Framework"   # with build/framework/libsaf.a
cargo build --release --features saf_vbap
```

On Linux install `libopenblas-dev` and `liblapacke-dev`; on Windows follow
[BUILDING_WINDOWS.md](BUILDING_WINDOWS.md).

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

Windows / ASIO (`list-asio-devices` prints the exact device names; FlexASIO
or ASIO4ALL work when the hardware has no ASIO driver of its own). With no ASIO
driver at all, `--output-backend asio` falls back to WASAPI shared mode and says
so in the log; `list-asio-devices` then names the WASAPI devices. WASAPI shared
mode plays as many channels as the device's Windows speaker setup, and a layout
wider than that is refused with an error rather than played with channels
missing:

```powershell
.\target\release\orender.exe list-asio-devices
.\target\release\orender.exe assets\demo\spatial-demo.wav `
  --bridge-path target\release\reference_bridge.dll `
  --output-backend asio --output-device "Your ASIO Device"
```

macOS / CoreAudio (`list-coreaudio-devices` prints the device names):

```bash
./target/release/orender list-coreaudio-devices
./target/release/orender assets/demo/spatial-demo.wav \
  --bridge-path target/release/libreference_bridge.dylib \
  --output-backend coreaudio
```

`--output-backend device` picks the platform's realtime backend, whichever it
is.

## 9. Configuration file

Default config path:

- Linux and macOS: `~/.config/omniphony/config.yaml`
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
- [BUILDING_WINDOWS.md](BUILDING_WINDOWS.md)
- [BINAURAL.md](BINAURAL.md)
- [OSC_PROTOCOL.md](OSC_PROTOCOL.md)
- [BRIDGE_API.md](BRIDGE_API.md)
- [../layouts/README.md](../layouts/README.md)
