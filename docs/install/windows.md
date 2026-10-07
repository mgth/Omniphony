# Windows: from nothing to a film playing

This page takes you from an empty machine to a film whose sound is rendered by
Omniphony, through **mpv-omniphony** (the player) and the **harletty bridge**
(the decoder). Studio, the control app, is optional and comes last.

Other systems: [Linux](linux.md) · [macOS](macos.md).

## What you download

| Piece | Asset | From |
| --- | --- | --- |
| Player, with the engine inside | `mpv-omniphony-v0.6.0-windows-x86_64.zip` | [mpv-v0.6.0](https://github.com/mgth/Omniphony/releases/tag/mpv-v0.6.0) |
| Decoder bridge | `harletty-bridge-v0.8.0-windows-x86_64.zip` | [harletty v0.8.0](https://github.com/harletty/harletty-bridge/releases/tag/v0.8.0) |
| Studio (optional) | `omniphony-studio-egui-v0.6.0-windows-x86_64.zip` | [v0.6.0](https://github.com/mgth/Omniphony/releases/tag/v0.6.0) |

The engine and the bridge must come from matching releases: the 0.6.0 engine
loads only a 0.8.x bridge, and refuses older and newer ones. When a newer
release is out, take the versions its release notes pair together.

All commands below are for **PowerShell**.

## 1. The player

Extract the zip into a folder of its own, for example `C:\omniphony`. Right-click
the zip, choose *Extract All…*, and give that folder. Or:

```powershell
Expand-Archive "$HOME\Downloads\mpv-omniphony-v0.6.0-windows-x86_64.zip" C:\omniphony
```

The folder now holds `mpv.exe`, `mpv.com`, `orender.dll` (the engine) and the
DLLs mpv needs.

**Check:** play one second of a test tone, without sound or picture:

```powershell
cd C:\omniphony
.\mpv --no-config --ao=null --vo=null --length=1 av://lavfi:sine
```

The first line names the engine and where it came from:

```text
orender: loaded liborender ABI 0.8 0.6.0 … from C:\omniphony\orender.dll (next to mpv)
```

The text in the last brackets says which copy was loaded. `(next to mpv)` is
the one from the zip. `(studio install)` means a copy deployed by the Tauri
Studio installer in `%LOCALAPPDATA%\omniphony\lib\` was found first. That copy
wins over the one next to mpv, so it must be of the same release as the bridge.

From a terminal, `.\mpv` runs `mpv.com`, the console version: it shows the
messages this page relies on. A double-clicked `mpv.exe` shows none of them.

## 2. The decoder bridge

Extract it **into the same folder**. The engine looks for a `*_bridge.dll` next
to the player, so no configuration is needed:

```powershell
Expand-Archive "$HOME\Downloads\harletty-bridge-v0.8.0-windows-x86_64.zip" C:\omniphony
```

That adds `harletty_bridge.dll` next to `mpv.exe`.

If `C:\ProgramData\omniphony\config.yaml` already exists (Studio writes it when you press Save) and
sets `render.bridge_path`, that path is used instead, and it must point at this
file. Remove the line, or change it to `C:\omniphony\harletty_bridge.dll`.

The check for this step is the first playback, in step 4.

## 3. Headphones or speakers

The configuration lives in `C:\ProgramData\omniphony\config.yaml`, shared by every
account and by the engine service.

**Headphones.** If the file does not exist yet, create it with:

```powershell
New-Item -ItemType Directory -Force "$env:ProgramData\omniphony" | Out-Null
Set-Content "$env:ProgramData\omniphony\config.yaml" "render:`n  binaural:`n    output_mode: binaural"
```

If it exists, open it in Notepad and put `binaural:` and `output_mode: binaural`
under its `render:` section, indented like this:

```yaml
render:
  binaural:
    output_mode: binaural
```

**Speakers.** Without a config, the engine renders to a 7.1.4 layout (12
channels). For another room, choose the layout in Studio ([step 5](#5-optional-studio))
and save. Then give Windows as many channels: *Settings → System → Sound →* your
output device *→ Speaker setup* (or *Configure* in the classic Sound control
panel), and pick your speaker count, e.g. *7.1 Surround*. mpv folds the render down
to what the device takes.

The mode is read when the player starts. After changing it, restart mpv.

## 4. Play a film

```powershell
cd C:\omniphony
.\mpv --ad=orender "D:\Films\film.mkv"
```

`--ad=orender` is required: without it mpv decodes the sound itself. The engine
takes TrueHD, E-AC-3, AC-3 and DTS tracks; any other track plays as in plain mpv.

**Check**, in the terminal, within a few seconds of the start:

```text
[… INFO  orender_engine::engine] bridge loaded + configured in 0.02s
[… INFO  orender_engine::engine] engine ready in 0.35s (bridge load + VBAP table + renderer build)
AO: [wasapi] 48000Hz … 2ch float
```

- `bridge loaded` and `engine ready` come from the engine: the bridge is
  loaded and the renderer is built.
- `AO:` comes from mpv: the output device is open. The channel count is `2ch`
  for headphones and your device's count for speakers.

To make `--ad=orender` the default, add `ad=orender` to
`%APPDATA%\mpv\mpv.conf`. Studio's *Activate in mpv config* switch does exactly
that.

## 5. Optional: Studio

Studio shows the objects in the room and changes the render live. It talks to
the engine inside mpv over OSC.

Extract `omniphony-studio-egui-v0.6.0-windows-x86_64.zip` into `C:\omniphony`.
It creates the folder `omniphony-studio-egui-v0.6.0-windows-x86_64` holding
`omniphony-studio-egui.exe`.

1. Start the player first, with OSC on:
   `.\mpv --ad=orender --ad-orender-osc "D:\Films\film.mkv"`
   (or set `osc: true` under `render:` in the config).
   From the next release, the flag is only needed with a config file that
   leaves OSC off: with no config file the engine turns OSC on by itself, and
   the config Studio then writes when you press Save keeps it on. (A config
   that says `osc: false`, or one saved before that release without
   `osc: true`, leaves it off.)
2. Then start
   `C:\omniphony\omniphony-studio-egui-v0.6.0-windows-x86_64\omniphony-studio-egui.exe`
   (double-click it, or from another terminal). It connects by itself. Allow it
   through the Windows firewall if asked.

Start them in this order. If Studio finds no renderer for six seconds, it starts
its own (*Auto-start local renderer*, in the connection settings), and that one
holds the port the player needs. When you only use Studio with the player,
switch that option off.

## When it does not work

Run mpv from PowerShell as above: every message below appears there. Read the
**first** error. Later ones are often consequences of it.

### 1. `liborender unavailable (…) — decoding natively`

The player found no usable engine. The film plays, but without Omniphony. This
message is followed by `orender_create failed — … Set render.bridge_path`,
which is misleading here: the bridge is not the problem, the engine is.

- Run the step 1 check with `-v` to see each place the engine was looked for,
  and why a copy was rejected:
  `.\mpv -v --no-config --ao=null --vo=null --length=1 av://lavfi:sine 2>&1 | Select-String orender`
- `The specified module could not be found` in that output, for an
  `orender.dll` that is there: the engine and the bridge are built with
  Microsoft's compiler and need the
  [Microsoft Visual C++ Redistributable (x64)](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist).
  Install it and try again.
- `orender: rejecting '…': ABI major …`: a stale engine, usually the one an
  older Tauri Studio left in `%LOCALAPPDATA%\omniphony\lib\`. Update that Studio,
  or delete the file.

### 2. `orender_create failed — decoding natively`, after the engine loaded

The engine is there but could not load the bridge. The reason is on the line
just before it, starting with `orender_create failed:`:

- `No bridge plugin found` with the folders searched: `harletty_bridge.dll` is
  not next to `mpv.exe`. Extract it there (step 2).
- `render.bridge_path '…' (from config) does not exist or is not a file`: the
  config names a path that is wrong. Fix it or remove it (step 2).
- `Failed to load bridge plugin from …` followed by a long
  `Compared <this>: --- Type Layout ---` dump that names two `bridge_api`
  versions: the bridge and the engine come from releases that do not match.
  With the 0.6.0 engine, use bridge 0.8.x. If the engine line of step 1 says
  `(studio install)`, the mismatch may be that engine's, not the player's.

### 3. The film plays, but nothing from Omniphony appears

No `engine ready` line, and no error:

- `--ad=orender` is missing.
- The track is not TrueHD, E-AC-3, AC-3 or DTS. Switch tracks with `#` in mpv.
- `--audio-spdif` is set (in the command or in `mpv.conf`): passthrough to the
  receiver takes precedence over the engine.

### 4. The engine or Studio is blocked as an unknown app

The builds are not code-signed yet. If SmartScreen stops `mpv.exe` or Studio,
choose *More info → Run anyway*. If your antivirus quarantines `orender.dll` or
`harletty_bridge.dll`, restore it and add the folder as an exception.

### Headphones play only the front, or speakers only two channels

Look at the `AO:` line. Many channels on headphones means the binaural mode was
not active at start: check `output_mode: binaural` (step 3) and restart mpv.
`2ch` on speakers means the Windows output device is set to stereo: change its
speaker setup (step 3).

---

Something else, or none of this helps: open an issue on
[Omniphony](https://github.com/mgth/Omniphony/issues), with the terminal output
of `.\mpv -v --ad=orender film.mkv`.
