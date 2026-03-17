# Quickstart

This file gives the shortest path to a working `omniphony-renderer` setup.

## 1. Build

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

`saf_vbap` enables runtime
VBAP generation via
[`Spatial_Audio_Framework` (SAF)](https://github.com/leomccormack/Spatial_Audio_Framework),
not the separate
[`SPARTA`](https://leomccormack.github.io/sparta-site/) plug-in suite.

Windows with ASIO:

```bash
cargo build --release --features asio
```

## 2. Prepare a Bridge Plugin

`orender` requires a bridge plugin.

You can provide it explicitly:

```bash
./target/release/orender input.bin \
  --bridge-path ./libformat_bridge.so
```

Or place a matching bridge next to the executable:

- `lib*_bridge.so`
- `lib*_bridge.dll`
- `lib*_bridge.dylib`

## 3. Realtime Decode

Use the default runtime path:

```bash
./target/release/orender input.bin \
  --bridge-path ./libformat_bridge.so
```

Read from stdin:

```bash
cat input.bin | ./target/release/orender - \
  --bridge-path ./libformat_bridge.so
```

## 4. Enable VBAP

Use a standard speaker layout:

```bash
./target/release/orender input.bin \
  --bridge-path ./libformat_bridge.so \
  --enable-vbap \
  --speaker-layout layouts/7.1.4.yaml
```

Or precompute a VBAP table:

```bash
./target/release/orender generate-vbap \
  --speaker-layout layouts/7.1.4.yaml \
  --output 7.1.4.vbap \
  --az-res 2 \
  --el-res 2 \
  --spread-res 0.25
```

Then reuse it:

```bash
./target/release/orender input.bin \
  --bridge-path ./libformat_bridge.so \
  --enable-vbap \
  --vbap-table ./7.1.4.vbap
```

## 5. Enable OSC

Send metadata to a local OSC client:

```bash
./target/release/orender input.bin \
  --bridge-path ./libformat_bridge.so \
  --osc \
  --osc-host 127.0.0.1 \
  --osc-port 9000
```

See [OSC_PROTOCOL.md](OSC_PROTOCOL.md) for the full message surface.

## 6. Realtime Output

Linux / PipeWire:

```bash
./target/release/orender input.bin \
  --bridge-path ./libformat_bridge.so \
  --output-backend pipewire \
  --output-device omniphony_router
```

Windows / ASIO:

```powershell
.\target\release\orender.exe list-asio-devices
.\target\release\orender.exe input.bin --output-backend asio --output-device "Your ASIO Device"
```

## 7. Configuration File

Default config path:

- Linux: `~/.config/omniphony/config.yaml`
- Windows: `%APPDATA%\omniphony\config.yaml`

Save the current effective configuration:

```bash
./target/release/orender --config ./config.yaml --save-config input.bin \
  --bridge-path ./libformat_bridge.so \
  --enable-vbap \
  --speaker-layout layouts/7.1.4.yaml \
  --osc
```

## 8. Next References

- [README.md](README.md)
- [BUILD.md](BUILD.md)
- [BUILDING_WINDOWS.md](BUILDING_WINDOWS.md)
- [OSC_PROTOCOL.md](OSC_PROTOCOL.md)
- [layouts/README.md](layouts/README.md)
