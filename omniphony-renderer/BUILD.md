# Building omniphony-renderer

## Platform-Specific Build Instructions

`omniphony-renderer` now compiles its native realtime backend and SAF-backed VBAP support by default on each supported OS:

- Linux: PipeWire
- Windows: ASIO

- Linux: PipeWire + `saf_vbap`
- Windows: ASIO + `saf_vbap`

The default build is now the full native build.

### Linux

On Linux, PipeWire output and runtime VBAP generation are built by default:

```bash
# SAF_ROOT must point to the Spatial_Audio_Framework source tree
# (default: ../SPARTA/SDKs/Spatial_Audio_Framework — adjust if needed)
export SAF_ROOT="/path/to/Spatial_Audio_Framework"

# Full native Linux build: PipeWire + SAF-backed VBAP
cargo build --release
```

Prerequisites (install via package manager):
- `libopenblas-dev` and `liblapacke-dev` (dynamic libraries linked at build time)
- SAF built as `build/framework/libsaf.a` inside `SAF_ROOT`
- `libpipewire-0.3-dev` (for PipeWire audio output)

This enables:
- Runtime VBAP table generation via SAF's `saf_vbap` module
- `generate-vbap` command for creating .vbap files
- PipeWire audio streaming output
- All runtime rendering functionality

### Windows

On Windows, ASIO output and runtime VBAP generation are built by default.

> **Prerequisites:** Native Windows builds require the ASIO SDK, SAF and OpenBLAS. See **[BUILDING_WINDOWS.md](BUILDING_WINDOWS.md)** for the full setup procedure.

```bash
# Full native Windows build: ASIO + SAF-backed VBAP + Windows Service
export SAF_ROOT="C:/dev/SAF"
export VCPKG_ROOT="C:/dev/vcpkg"
export CPAL_ASIO_DIR="C:/dev/asio_sdk"
cargo build --release
```

Full build enables:
- Runtime VBAP table generation (`generate-vbap` command)
- ASIO audio output (`--output-backend asio`, `list-asio-devices`)
- Windows Service Control Manager integration

### Building Without Extra Features

You can build the full native profile directly:

```bash
cargo build --release
```

This default build:
- Can process supported bridge-provided streams
- Can load pre-generated VBAP tables
- Can generate VBAP tables at runtime
- Includes the platform realtime backend for the current OS

Quick sanity check:

```bash
cargo check
```

This validates the default native build.

## Workflow: VBAP + ASIO on Windows

Generate a VBAP table (Linux or Windows with `saf_vbap`) and use it for playback with ASIO:

```bash
# Generate a VBAP table
orender generate-vbap \
  --speaker-layout layouts/7.1.4.yaml \
  --output 7.1.4.vbap \
  --az-res 5 \
  --el-res 5 \
  --spread-res 0.05

# List available ASIO devices
orender.exe list-asio-devices

# Decode with ASIO + VBAP
orender.exe input.bin \
  --output-backend asio \
  --output-device "Your ASIO Device" \
  --enable-vbap \
  --vbap-table 7.1.4.vbap
```

## SAF / SPARTA Naming

`omniphony-renderer` runtime VBAP generation uses
[`Spatial_Audio_Framework` (SAF)](https://github.com/leomccormack/Spatial_Audio_Framework),
not the separate
[`SPARTA`](https://leomccormack.github.io/sparta-site/) plug-in suite.

The `saf_vbap` feature means:

- generate Rust bindings against SAF
- link SAF
- enable runtime VBAP table generation through SAF's `saf_vbap` module

`omniphony-renderer` does not bundle or redistribute SAF or SPARTA. You must obtain and
build SAF separately and comply with the applicable upstream SAF license terms
for your build.

## Feature Flags

| Feature | Description | Platforms |
|---------|-------------|-----------|
| `saf_vbap` | Enable runtime VBAP table generation via SAF (`saf_vbap`) | Linux, Windows |
| `asio` | Legacy compatibility alias; ASIO is built by default on Windows | Windows only |
| `pipewire` | Legacy compatibility alias; PipeWire is built by default on Linux | Linux only |

## ASIO Devices (Windows Only)

When building with the `asio` feature, you get access to ASIO audio output and the `list-asio-devices` command.

### Listing Available ASIO Devices

```powershell
orender.exe list-asio-devices
```

This will show all ASIO devices installed on your system:

```
Available ASIO devices:
  1. FlexASIO
  2. ASIO4ALL V2
  3. Focusrite USB ASIO
```

### Using a Specific ASIO Device

```powershell
orender.exe input.bin --output-backend asio --output-device "FlexASIO"
```

**Note:** The device name must match exactly as shown by `list-asio-devices`.

### Common ASIO Drivers

If you don't have any ASIO devices, install one of these:
- **FlexASIO** - Universal ASIO driver with flexible configuration
- **ASIO4ALL** - Universal ASIO driver for most audio hardware
- **Manufacturer drivers** - Check your audio interface manufacturer's website

## Troubleshooting

### "VBAP table generation not available"

This means the build could not enable SAF-backed VBAP support. Check that:
- `SAF_ROOT` points to a valid `Spatial_Audio_Framework` tree when needed
- SAF has been built and provides `build/framework/libsaf.a`
- the native dependencies required by SAF are installed

### Missing ASIO options on Windows

On Windows, ASIO support is now part of the default native build. If it is missing,
check that you are building on Windows with the ASIO SDK configured as documented in
[BUILDING_WINDOWS.md](BUILDING_WINDOWS.md).

### SAF build fails on Windows

The `saf_vbap` feature in `omniphony-renderer` depends on a separate SAF build plus OpenBLAS.
Follow the setup steps in [BUILDING_WINDOWS.md](BUILDING_WINDOWS.md).
