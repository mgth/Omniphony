# mpv-omniphony — usage

**mpv-omniphony** is the [mpv](https://mpv.io/) media-player frontend for the
[Omniphony](https://github.com/mgth/Omniphony) spatial audio engine. It adds an
opt-in spatial audio decoder (`--ad=orender`) that renders objects through
`liborender` (VBAP spatial rendering) instead of letting FFmpeg downmix.
Non-spatial audio keeps playing via mpv's normal `ad_lavc` decoder.

> **Installing:** the step-by-step pages take you from nothing to a film
> playing — [Linux](install/linux.md) · [Windows](install/windows.md) ·
> [macOS](install/macos.md).
>
> **Downloads:** prebuilt player builds are on the
> [Omniphony releases page](https://github.com/mgth/Omniphony/releases) (`mpv-v*`).
> Source, build instructions and packaging live in the
> [mpv-omniphony repository](https://github.com/mgth/mpv-omniphony).

![mpv-omniphony — mpv playing a spatial mix, supervised by Omniphony Studio](https://github.com/mgth/mpv-omniphony/raw/main/mpv-omniphony.png)

*Left: mpv's stats overlay shows `ad_orender` picked up the stream and the
renderer is feeding the platform's audio output. Right: Omniphony Studio attached
over OSC, showing per-object positions in the room and live meters.*

## How it fits together

1. mpv demuxes raw access units from the container.
2. `ad_orender` feeds them to `liborender` (`orender_process`), which loads the
   decoder bridge plugin, decodes to PCM + object metadata, and VBAP-renders to
   N-channel interleaved float (`AF_FORMAT_FLOAT` — so mpv's normal resampler /
   audio filter chain still applies, unlike spdif passthrough).
3. It's opt-in: the decoder is only selected when `orender` is in the `--ad` list,
   so default playback is untouched. The first packet resolves spatial-vs-plain
   (`orender_is_spatial`); for non-spatial streams the bed is still VBAP-rendered
   to the layout (automatic fallback to `ad_lavc` is a future refinement — use
   plain `--ad=` to bypass orender entirely).
4. The output channel map comes from `orender_channel_layout` (per-speaker labels
   → `mp_chmap`).

## Requirements

- **The engine library (`liborender`) — loaded at runtime, not linked.** mpv
  finds it on its own in most setups: the release zips bundle a fallback copy
  next to `mpv(.exe)`, and installing/updating **Omniphony Studio** deploys the
  current engine to a per-user location that takes precedence — so updating
  Studio updates the engine mpv uses, with no new mpv build. The full search
  order:
  1. `--ad-orender-library=<path>` or `$ORENDER_LIBRARY` — explicit choice; if
     set and unusable, spatial audio is disabled with a clear error (no
     fallback);
  2. the Studio-deployed per-user engine:
     `$XDG_DATA_HOME/omniphony/lib/liborender.so.0`, by default
     `~/.local/share/omniphony/lib/liborender.so.0` (Linux),
     `~/Library/Application Support/omniphony/lib/liborender.dylib` (macOS),
     `%LOCALAPPDATA%\omniphony\lib\orender.dll` (Windows);
  3. the fallback copy bundled next to `mpv(.exe)` in the release zips;
  4. the system library path (e.g. the `liborender` distro package).

  Each candidate is checked with an ABI version handshake: an incompatible
  library is rejected (one clear log line) and the search falls through to the
  next location. If nothing usable is found, mpv still plays everything through
  its native decoders — spatial rendering is simply unavailable.
- **The decoder bridge**
  ([harletty-bridge](https://github.com/harletty/harletty-bridge/releases) — it
  is not bundled with the player). Without configuration the engine takes the
  first `*_bridge.{so,dll,dylib}` next to the mpv executable, then in
  `$ORENDER_BRIDGE_DIR`, then in the per-user engine folder of point 2 above
  (`<local data>/omniphony/lib/`), then in `/usr/lib/orender` (Unix). Studio's
  own renderer searches the same folders from its `orender`, so a bridge in the
  per-user engine folder serves both; Studio also hands its renderer the folder
  of a bridge named by `ad-orender-bridge-path` in `mpv.conf` (an absolute path,
  default profile). `render.bridge_path`
  in the config, or `--ad-orender-bridge-path`, names one file instead (no
  globs, and no fallback when it is wrong).
- The **shared omniphony config** (the same one the `orender` CLI and Studio
  use): `~/.config/omniphony/config.yaml` on Linux and macOS,
  `%ProgramData%\omniphony\config.yaml` on Windows, or
  `$OMNIPHONY_CONFIG_DIR/config.yaml` when that is set. It carries the speaker
  layout, the output mode (speakers or binaural) and optionally
  `render.bridge_path`. Without it the engine runs on its defaults: a 7.1.4
  speaker render, OSC off.

### macOS prebuilt releases (Apple Silicon)

The release ships one macOS arm64 archive, `…-macos-arm64.zip`, holding a
self-contained `mpv-omniphony.app` (every dylib bundled, no Homebrew ffmpeg
needed); command-line use runs `mpv-omniphony.app/Contents/MacOS/mpv`. It is
ad-hoc signed (not notarized), so Gatekeeper blocks the first launch. Clear the
download quarantine once:

```sh
xattr -dr com.apple.quarantine /path/to/mpv-omniphony.app
```

(or right-click → Open the first time).

## Play

```sh
mpv --ad=orender film.spatial.mkv          # opt-in; default playback is untouched
```

With no options, everything (bridge path, speaker layout, OSC) comes from the
shared omniphony config. Per-invocation overrides:

| Option | Overrides |
| --- | --- |
| `--ad-orender-library=<path>` | the liborender to load (else the search order above) |
| `--ad-orender-config=<path>` | the render config YAML (else the shared default) |
| `--ad-orender-bridge-path=<path>` | `render.bridge_path` (the decoder bridge `.so`) |
| `--ad-orender-osc` | force OSC on (else follows `render.osc` in the config; on when there is no config file) |
| `--ad-orender-osc-port=<n>` | outgoing/monitoring port |
| `--ad-orender-osc-rx-port=<n>` | incoming control port (studio registers here; default 9000) |
| `--ad-orender-osc-bind=<addr>` | listener bind address |
| `--ad-orender-osc-monitor-target=<host>` | monitoring host |

Empty/zero values fall back to the config then the built-in defaults.
**OSC + studio:** with no config file, OSC is on: the renderer listens on 9000
(the rendezvous studio registers to, or `OMNIPHONY_OSC_PORT` when set) and
studio connects on its own. A config file that exists decides:
`render.osc: true` turns OSC on, and `render.osc: false` or no `osc` key
leaves it off unless you pass `--ad-orender-osc`. The config Studio creates (its first Save,
or a setting it keeps at once) records `osc: true`, so creating the file does
not turn OSC off. Note the shared config means the standalone CLI would also
enable OSC; without a config file the CLI keeps it off (it reports in its
terminal, and Studio starts it with `--osc`).

When a Studio-launched standby renderer already holds the port, the player's
engine asks it to step aside: the standby releases the port and its audio
output, the player's engine takes the port, and the standby resumes when the
player exits. Only a client on this machine can make a renderer step aside.
A second player cannot take the port from the first (an embedded engine never
yields) and plays without OSC; a player whose bridge fails to load reports it
on the port only if it is free, never evicting a standby.

## Supervision with Omniphony Studio

[Omniphony Studio](https://github.com/mgth/Omniphony) is the 3D
visualization / live-control UI for the renderer. It speaks OSC to whichever
host runs `liborender` — the standalone `orender` CLI, or the embedded host
inside this mpv build. Studio detects the embedded variant via the renderer's
capabilities handshake and hides the panels that don't apply (audio-output
device, adaptive resampler, latency target), keeping spatial controls and
metering enabled.

### Get Studio

Prebuilt bundles ship on the Omniphony repo's releases page:

[Omniphony Studio Latest Release](https://github.com/mgth/Omniphony/releases/latest)

- **Linux** — `Omniphony.Studio_<ver>_amd64.deb`,
  `Omniphony.Studio_<ver>_amd64.AppImage`, or
  `Omniphony.Studio-<ver>-1.x86_64.rpm`.
- **Windows** — `Omniphony.Studio_<ver>_x64-setup.exe` (NSIS) or
  `Omniphony.Studio_<ver>_x64_en-US.msi`.

The Linux .deb installs an `omniphony-studio` binary; the Windows installers add
a Start menu entry.

### Connect Studio to mpv

1. Start mpv with OSC on. With no config file it already is
   (`mpv --ad=orender film.mkv`); otherwise one of the two — they're
   equivalent:
   - add `render.osc: true` to `~/.config/omniphony/config.yaml`, or
   - launch with `mpv --ad=orender --ad-orender-osc film.mkv`.
2. Launch Studio. It registers with the renderer on the rendezvous port
   (default 9000) and starts receiving the live state.

Studio also works against the standalone CLI the same way — the same shared
config drives both. You can flip between embedded (mpv) and standalone sessions
without re-configuring Studio.

### Live overlay (optional)

Studio can also draw a pseudo-3D front-view diagram of the live audio objects
**directly on top of the mpv video**, mirroring the 3D view's mapping so the two
stay readable side-by-side.

What gets drawn:

- **Active objects**: filled circles at `(X, Z)`; radius from RMS level,
  per-object colour from Studio's palette (FNV-1a hash of the object id, with the
  speaker-tag override applied — same logic as the 3D view).
- **Wireframe cube**: the spatial unit cube projected with the same pseudo-3D
  depth ratio as the objects. The `Y = -1` face is omitted because it traces the
  screen border anyway; the four diagonals carry the depth structure.
- **Per-object depth axis**: a coloured line at the object's `(X, Z)` spanning
  the full `Y ∈ [-1, +1]` range, with a perpendicular tick at `Y = 0` (screen
  midpoint of the line). Marks where on the front/back axis the object sits.
- **Trails**: line or diffuse mode, mirroring Studio's *Trails* panel. Diffuse
  mode uses screen-distance-adaptive subdivision so fast-moving objects keep a
  near-continuous trail regardless of the OSC sample rate.
- **Teleport break**: a configurable threshold in Studio (*Trails → Teleport
  threshold*, default `0.5` in normalised XYZ units) drops the segment connecting
  two trail points further apart than that threshold, in both the 3D view and the
  overlay. Useful when objects jump rather than glide.
- **Object count**: a small header in the top-right corner.

Pseudo-3D depth mapping: `Y = -1` (listener's rear / wrap-around) fills the whole
screen; `Y = +1` (screen plane) fits inside the 2.35:1 cinema band. `(X = 0,
Z = 0.5)` stays at screen centre across the whole `Y` range so the projection
looks coherent in fullscreen, letterboxed and windowed mpv.

**Nothing to install**: the overlay client is built into mpv itself (part of the
`ad_orender` patch set, compiled whenever the `orender` feature is enabled). It
pulls the finished ASS scene and the heatmap bitmap straight from liborender
inside the mpv process — no Lua script, no LuaJIT requirement, no IPC socket, no
Studio dependency. The overlay starts enabled and simply stays blank until
spatial content is decoded.

Studio still configures the overlay (trails, A/B tags, heatmap parameters) over
OSC, into the renderer, whenever it is connected to the same liborender instance.

> **Upgrading from the old Lua overlay?** Earlier versions shipped an
> `omniphony-overlay.lua` you copied into your mpv `scripts/` directory. It is no
> longer needed — you can delete it. On the released builds (PUC Lua) it
> self-disables anyway (no LuaJIT FFI), so a leftover copy is harmless; on a
> LuaJIT build it would just redundantly drive the same overlay. Removing it keeps
> things tidy.

#### Controlling the overlay from mpv

The overlay client grabs **no keys by default** (mpv convention: you own your
`input.conf`). It exposes named, keyless bindings — map your own keys with
`script-binding omniphony_overlay/<name>` (underscore: mpv client names are always
alphanumeric), or drive them from any client with
`script-message omniphony-overlay <name>`:

| Binding / message     | Action                                           |
| --------------------- | ------------------------------------------------ |
| `toggle`              | master overlay on/off                            |
| `labels`              | object-name labels on/off                        |
| `objects`             | objects (markers + trails + depth lines) on/off  |
| `trails`              | motion trails on/off                             |
| `heatmap`             | energy heatmap field on/off                      |
| `heatmap-colormap`    | cycle the heatmap gradient                       |
| `heatmap-bands-inc` / `heatmap-bands-dec` | more / fewer heatmap depth planes |

Each toggle flips the control inside liborender and reports the new state in the
OSD, so it stays in sync with Studio's OSC changes. See
[`runtime/input.conf.example`](https://github.com/mgth/mpv-omniphony/blob/main/runtime/input.conf.example)
for a ready-to-copy set of bindings.

---

**Source & build:** [mpv-omniphony](https://github.com/mgth/mpv-omniphony) ·
**Engine:** [Omniphony](https://github.com/mgth/Omniphony)

If Omniphony is useful to you, you can support development with a donation — it
helps maintain and improve the suite.

[![Sponsor on GitHub](https://img.shields.io/badge/Sponsor-mgth-ea4aaa?logo=githubsponsors&logoColor=white)](https://github.com/sponsors/mgth)
[![ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/G5X022D1RW)
[![Donate using Liberapay](https://liberapay.com/assets/widgets/donate.svg)](https://liberapay.com/mgth/donate)
