# Linux: from nothing to a film playing

This page takes you from an empty machine to a film whose sound is rendered by
Omniphony, through **mpv-omniphony** (the player) and the **harletty bridge**
(the decoder). Studio, the control app, is optional and comes last.

Other systems: [Windows](windows.md) · [macOS](macos.md).

## What you download

| Piece | Asset | From |
| --- | --- | --- |
| Player, with the engine inside | `mpv-omniphony-v0.6.0-linux-x86_64.zip` | [mpv-v0.6.0](https://github.com/mgth/Omniphony/releases/tag/mpv-v0.6.0) |
| Decoder bridge | `harletty-bridge-v0.8.0-linux-x86_64.zip` | [harletty v0.8.0](https://github.com/harletty/harletty-bridge/releases/tag/v0.8.0) |
| Studio (optional) | `omniphony-studio-egui-v0.6.0-linux-x86_64.tar.gz` | [v0.6.0](https://github.com/mgth/Omniphony/releases/tag/v0.6.0) |

The engine and the bridge must come from matching releases: the 0.6.0 engine
loads only a 0.8.x bridge, and refuses older and newer ones. When a newer
release is out, take the versions its release notes pair together.

### Which distribution each prebuilt runs on

| Asset | Built on | Runs on |
| --- | --- | --- |
| `mpv-omniphony-…-linux-x86_64.zip` | Ubuntu 24.04 | **Ubuntu 24.04 and its derivatives only.** It links the system's FFmpeg 6.1 (`libavcodec.so.60`) and libplacebo (`libplacebo.so.338`). |
| `harletty-bridge-…-linux-x86_64.zip` | Ubuntu 24.04 | Any x86_64 distribution with glibc 2.39 or newer. It needs nothing else. |
| `omniphony-studio-egui-…-linux-x86_64.tar.gz` | Ubuntu 22.04 | Any x86_64 distribution with glibc 2.35 or newer and PipeWire. |
| `Omniphony.Studio_…_amd64.AppImage` (Tauri Studio) | Ubuntu 22.04 | Any x86_64 distribution with glibc 2.35 or newer. |

**On Arch and its derivatives**, skip the downloads: the whole stack is on the
AUR, built on your machine. `paru -S mpv-omniphony harletty-bridge` installs the
player, the engine and the bridge (into `/usr/lib/orender/`, where the engine
looks without any configuration). Continue at [step 3](#3-headphones-or-speakers).

**On Fedora, openSUSE, Debian 12 and other distributions**, the 0.6.0 player
zip does not start (see [failure 1](#1-error-while-loading-shared-libraries)).
The next player release adds an AppImage that runs on any desktop with glibc
2.38 or newer, and this page will switch to it. Until then, build the player
from [mpv-omniphony](https://github.com/mgth/mpv-omniphony); the bridge and
Studio prebuilts above work as they are.

## 1. The player

```bash
mkdir -p ~/omniphony && cd ~/omniphony
unzip ~/Downloads/mpv-omniphony-v0.6.0-linux-x86_64.zip
```

The folder now holds `mpv`, `liborender.so.0` (the engine), `orender.h` and a
README. On Ubuntu 24.04 the player uses the system's libraries; installing the
distribution's own mpv once pulls them all in:

```bash
sudo apt install mpv
```

**Check:** play one second of a test tone, without sound or picture:

```bash
./mpv --no-config --ao=null --vo=null --length=1 av://lavfi:sine
```

The first line names the engine and where it came from:

```text
orender: loaded liborender ABI 0.8 0.6.0 … from /home/you/omniphony/liborender.so.0 (next to mpv)
```

The text in the last brackets says which copy was loaded. `(next to mpv)` is
the one from the zip. `(studio install)` means a copy deployed by the Tauri
Studio in `~/.local/share/omniphony/lib/` was found first. That copy wins over
the one next to mpv, so it must be of the same release as the bridge.

## 2. The decoder bridge

Unzip it **into the same folder**. The engine looks for a `*_bridge.so` next to
the player, so no configuration is needed:

```bash
cd ~/omniphony
unzip ~/Downloads/harletty-bridge-v0.8.0-linux-x86_64.zip   # adds libharletty_bridge.so
```

If your `~/.config/omniphony/config.yaml` already exists (Studio writes it when you press Save) and
sets `render.bridge_path`, that path is used instead, and it must point at this
file. Remove the line, or change it to `/home/you/omniphony/libharletty_bridge.so`
(an absolute path).

The check for this step is the first playback, in step 4.

## 3. Headphones or speakers

**Headphones.** Write this to `~/.config/omniphony/config.yaml` (or add the
three lines to the file Studio already wrote):

```bash
mkdir -p ~/.config/omniphony
cat >> ~/.config/omniphony/config.yaml <<'EOF'
render:
  binaural:
    output_mode: binaural
EOF
```

Only append this if the file has no `render:` section yet. Otherwise, put
`binaural:` and `output_mode: binaural` under the `render:` already there.

**Speakers.** Without a config, the engine renders to a 7.1.4 layout (12
channels). For another room, choose the layout in Studio ([step 5](#5-optional-studio))
and save. Then give your system output as many channels: in your desktop's sound
settings, pick the output device's profile with your speaker count (e.g.
*Surround 7.1*). Otherwise PipeWire folds the render down to what the device
takes.

The mode is read when the player starts. After changing it, restart mpv.

## 4. Play a film

```bash
cd ~/omniphony
./mpv --ad=orender /path/to/film.mkv
```

`--ad=orender` is required: without it mpv decodes the sound itself. The engine
takes TrueHD, E-AC-3, AC-3 and DTS tracks; any other track plays as in plain mpv.

**Check**, in the terminal, within a few seconds of the start:

```text
[… INFO  orender_engine::engine] bridge loaded + configured in 0.02s
[… INFO  orender_engine::engine] engine ready in 0.35s (bridge load + VBAP table + renderer build)
AO: [pipewire] 48000Hz unknown2 (empty) 2ch float
```

- `bridge loaded` and `engine ready` come from the engine: the bridge is
  loaded and the renderer is built.
- `AO:` comes from mpv: the output device is open. The channel count is `2ch`
  for headphones and the layout's count (`12ch` for 7.1.4) for speakers.

To make `--ad=orender` the default, add `ad=orender` to `~/.config/mpv/mpv.conf`.
Studio's *Activate in mpv config* switch does exactly that.

## 5. Optional: Studio

Studio shows the objects in the room and changes the render live. It talks to
the engine inside mpv over OSC.

```bash
mkdir -p ~/omniphony/studio && cd ~/omniphony/studio
tar xzf ~/Downloads/omniphony-studio-egui-v0.6.0-linux-x86_64.tar.gz --strip-components=1
```

1. Start the player first, with OSC on:
   `~/omniphony/mpv --ad=orender --ad-orender-osc film.mkv`
   (or set `osc: true` under `render:` in the config). With the AUR packages
   the player is on your `PATH`: `mpv --ad=orender --ad-orender-osc film.mkv`.
2. Then, from another terminal, start `~/omniphony/studio/omniphony-studio-egui`
   (with the AUR, `paru -S omniphony-studio-egui`, then `omniphony-studio-egui`).
   It connects by itself.

Start them in this order. If Studio finds no renderer for six seconds, it starts
its own (*Auto-start local renderer*, in the connection settings), and that one
holds the port the player needs. When you only use Studio with the player,
switch that option off.

## When it does not work

Run mpv from a terminal: every message below appears there. Read the **first**
error. Later ones are often consequences of it.

### 1. `error while loading shared libraries`

```text
./mpv: error while loading shared libraries: libavcodec.so.60: cannot open shared object file
```

The player zip only runs on Ubuntu 24.04. On Ubuntu 24.04, run
`sudo apt install mpv` and try again. On any other distribution, see
[Which distribution each prebuilt runs on](#which-distribution-each-prebuilt-runs-on).

### 2. `liborender unavailable (…) — decoding natively`

The player found no usable engine. The film plays, but without Omniphony. This
message is followed by `orender_create failed — … Set render.bridge_path`,
which is misleading here: the bridge is not the problem, the engine is.

- Run the step 1 check with `-v` to see each place the engine was looked for,
  and why a copy was rejected (`orender: rejecting '…': ABI major …`):
  `./mpv -v --no-config --ao=null --vo=null --length=1 av://lavfi:sine 2>&1 | grep orender`
- Check that `liborender.so.0` is still next to `mpv`.
- A stale engine in `~/.local/share/omniphony/lib/` (left by an older Tauri
  Studio) is tried first. Update that Studio, or delete the file.

### 3. `orender_create failed — decoding natively`, after the engine loaded

The engine is there but could not load the bridge. The reason is on the line
just before it, starting with `orender_create failed:`:

- `No bridge plugin found` with the folders searched:
  `libharletty_bridge.so` is not next to `mpv`. Unzip it there (step 2).
- `render.bridge_path '…' (from config) does not exist or is not a file`:
  the config names a path that is wrong. Fix it or remove it (step 2).
- `Failed to load bridge plugin from …` followed by a long
  `Compared <this>: --- Type Layout ---` dump that names two `bridge_api`
  versions: the bridge and the engine come from releases that do not match.
  With the 0.6.0 engine, use bridge 0.8.x. If the engine line of step 1 says
  `(studio install)`, the mismatch may be that engine's, not the player's.

### 4. The film plays, but nothing from Omniphony appears

No `engine ready` line, and no error:

- `--ad=orender` is missing.
- The track is not TrueHD, E-AC-3, AC-3 or DTS. Switch tracks with `#` in mpv.
- `--audio-spdif` is set (in the command or in `mpv.conf`): passthrough to the
  receiver takes precedence over the engine.

### Headphones play only the front, or speakers only two channels

Look at the `AO:` line. `12ch` on headphones means the binaural mode was not
active at start: check `output_mode: binaural` (step 3) and restart mpv. `2ch`
on speakers means the output device is in stereo: choose a profile with more
channels in the sound settings.

---

Something else, or none of this helps: open an issue on
[Omniphony](https://github.com/mgth/Omniphony/issues), with the terminal output
of `./mpv -v --ad=orender film.mkv`.
