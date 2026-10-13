# Windows: from nothing to a film playing

This page takes you from an empty machine to a film whose sound is rendered by
Omniphony, through **mpv-omniphony** (the player) and the **harletty bridge**
(the decoder). Studio, the control app, is optional and comes last.

Other systems: [Linux](linux.md) · [macOS](macos.md).

## The installer

`omniphony-v0.7.0-windows-x86_64-setup.exe`, on the
[v0.7.0](https://github.com/mgth/Omniphony/releases/tag/v0.7.0) release page,
holds the player, the engine and Studio of that release in one download, and
puts each where the others look for it. It installs for your account, asks
for no administrator rights and writes no config file.

The installer is not code-signed: SmartScreen shows *Windows protected your
PC*. Choose *More info*, then *Run anyway*
(see [failure 4](#4-the-engine-or-studio-is-blocked-as-an-unknown-app)).

| Installed | Where |
| --- | --- |
| The player | `%LOCALAPPDATA%\Programs\Omniphony\player\` |
| Studio and `orender.exe` | `%LOCALAPPDATA%\Programs\Omniphony\studio\` |
| The engine | `%LOCALAPPDATA%\omniphony\lib\`, where the player, Studio and `orender` look before their own copy |
| Start menu | *Omniphony Studio*, and *mpv-omniphony*, the player started with `--ad=orender` |
| `%ProgramData%\omniphony\` | created empty, for the config Studio saves later |

**The decoder bridge is not in the installer.** Take it as in
[step 2](#2-the-decoder-bridge) and put its three DLLs into
`%LOCALAPPDATA%\omniphony\lib\`, next to the engine: every host finds them
there without any option. The installer's last page says whether it found a
bridge the engine accepts. To ask again after adding one:

```powershell
& "$env:LOCALAPPDATA\Programs\Omniphony\studio\orender.exe" check-bridges
```

**Start the renderer when I log on** is an unchecked option of the installer.
Most setups do not need it: the player carries its own engine. When checked,
`orender` starts with your session in a minimized console window; closing
that window stops it. Studio's own *Install service* remains the way to a
Windows service.

**Upgrade** by running the installer of the newer release, with mpv and Studio
closed. **Remove** from *Settings → Apps → Installed apps → Omniphony*: it
deletes what the installer wrote and keeps `config.yaml`, Studio's
preferences, `mpv.conf` and the bridge.

Silent install: `omniphony-v0.7.0-windows-x86_64-setup.exe /S`, with
`/STARTUP` for the logon start and `/D=C:\some\folder` (last, unquoted) for
another folder.

Then go to [step 3](#3-headphones-or-speakers), using
`%LOCALAPPDATA%\Programs\Omniphony\player\mpv.exe` where the steps say
`.\mpv`. The rest of this page is the same install by hand.

## What you download

| Piece | Asset | From |
| --- | --- | --- |
| Player, with the engine inside | `mpv-omniphony-v0.7.0-windows-x86_64.zip` | [mpv-v0.7.0](https://github.com/mgth/Omniphony/releases/tag/mpv-v0.7.0) |
| Decoder bridge | `harletty-bridge-v0.9.0-windows-x86_64.zip` | [harletty v0.9.0](https://github.com/harletty/harletty-bridge/releases/tag/v0.9.0) |
| Studio (optional) | `omniphony-studio-egui_0.7.0_x64-setup.exe`, `omniphony-studio-egui_0.7.0_x64_en-US.msi` or `omniphony-studio-egui-v0.7.0-windows-x86_64.zip` | [v0.7.0](https://github.com/mgth/Omniphony/releases/tag/v0.7.0) |

The engine and the bridge must come from matching releases: the 0.7.0 engine
loads only a 0.9.x bridge, and refuses older and newer ones. When a newer
release is out, take the versions its release notes pair together.

The bridge zip holds one library per codec family:
`harletty_dolby_bridge.dll`, `harletty_dts_bridge.dll` and
`harletty_iamf_bridge.dll`. The engine loads all of them. Step 2 says what to
do when you upgrade from the single `harletty_bridge.dll` of 0.8.

None of these needs the Microsoft Visual C++ Redistributable any more: the
engine, the bridge and Studio carry that runtime inside.

All commands below are for **PowerShell**.

## 1. The player

Extract the zip into a folder of its own, for example `C:\omniphony`. Right-click
the zip, choose *Extract All…*, and give that folder. Or:

```powershell
Expand-Archive "$HOME\Downloads\mpv-omniphony-v0.7.0-windows-x86_64.zip" C:\omniphony
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
orender: loaded liborender ABI 0.12 0.7.0 … from C:\omniphony\orender.dll (next to mpv)
```

The text in the last brackets says which copy was loaded. `(next to mpv)` is
the one from the zip. `(studio install)` means a copy deployed by
Studio in `%LOCALAPPDATA%\omniphony\lib\` was found first. That copy
wins over the one next to mpv, so it must be of the same release as the bridge.

From a terminal, `.\mpv` runs `mpv.com`, the console version: it shows the
messages this page relies on. A double-clicked `mpv.exe` shows none of them.

## 2. The decoder bridge

Extract it **into the same folder**. The engine loads every `*_bridge.dll`
file it finds next to the player, so no configuration is needed:

```powershell
Expand-Archive "$HOME\Downloads\harletty-bridge-v0.9.0-windows-x86_64.zip" C:\omniphony
```

That adds three libraries next to `mpv.exe`, one per codec family:
`harletty_dolby_bridge.dll`, `harletty_dts_bridge.dll` and
`harletty_iamf_bridge.dll`.

If `C:\ProgramData\omniphony\config.yaml` already exists (Studio writes it
when you press Save) and names bridges, those paths are used instead of the
search, and they must point at these files. Remove the key, or list the
libraries under `render.bridge_paths`, in the order the engine tries them:

```yaml
render:
  bridge_paths:
    - C:\omniphony\harletty_dolby_bridge.dll
    - C:\omniphony\harletty_dts_bridge.dll
    - C:\omniphony\harletty_iamf_bridge.dll
```

**Upgrading from 0.6.0 and bridge 0.8:**

- Delete the old `harletty_bridge.dll`. The engine refuses it, says so, and
  loads the others, but it has no use left.
- A `render.bridge_path` that still names `harletty_bridge.dll` keeps working
  in 0.7.0: the engine loads the family libraries found in the same folder in
  its place, and the next Save writes them as `render.bridge_paths`. Removing
  the line, so that the engine finds them on its own, works too.

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
takes TrueHD, E-AC-3, AC-3, DTS and IAMF tracks; any other track plays as in
plain mpv.

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

Run the setup (`omniphony-studio-egui_0.7.0_x64-setup.exe`) or the MSI: Studio
is then in the Start menu. Or extract
`omniphony-studio-egui-v0.7.0-windows-x86_64.zip` into `C:\omniphony`: it
creates the folder `omniphony-studio-egui-v0.7.0-windows-x86_64` holding
`omniphony-studio-egui.exe`. None of them is code-signed
(see [failure 4](#4-the-engine-or-studio-is-blocked-as-an-unknown-app)).

1. Start the player first: `.\mpv --ad=orender "D:\Films\film.mkv"`.
   With no config file the engine turns OSC on by itself, and the config
   Studio writes when you press Save keeps it on. A config that says
   `osc: false`, or one saved by 0.6.0 without `osc: true`, leaves it off:
   add `--ad-orender-osc` to the command, or set `osc: true` under `render:`.
2. Then start Studio from the Start menu, or
   `C:\omniphony\omniphony-studio-egui-v0.7.0-windows-x86_64\omniphony-studio-egui.exe`
   (double-click it, or from another terminal). It connects by itself. Allow it
   through the Windows firewall if asked.

Start them in this order. If Studio finds no renderer for six seconds, it starts
its own (*Auto-start local renderer*, in the connection settings), and that one
holds the port the player needs. When you only use Studio with the player,
switch that option off.

Studio's own renderer uses the bridges `%APPDATA%\mpv\mpv.conf` names
(`ad-orender-bridge-path=`, with absolute paths), else it looks next to its
`orender.exe`, then in `%LOCALAPPDATA%\omniphony\lib\`. It does not know the
player's folder: bridges that only sit next to `mpv.exe`, with no `mpv.conf`
line naming them, are not found (nor is a `portable_config` folder read).
Without one it still runs, and Studio shows an orange *No decoder* banner:
films keep playing in the player. To give it the bridges too, copy the three
`harletty_*_bridge.dll` files into `%LOCALAPPDATA%\omniphony\lib\`, a folder
the player also searches, or list them in `%APPDATA%\mpv\mpv.conf`,
separated by `;` (the player reads the same list):

```text
ad-orender-bridge-path=C:\omniphony\harletty_dolby_bridge.dll;C:\omniphony\harletty_dts_bridge.dll;C:\omniphony\harletty_iamf_bridge.dll
```

A line that still names `harletty_bridge.dll` keeps working in 0.7.0, even
once the file is deleted: the engine loads the family libraries of the same
folder in its place.

A renderer that Studio starts itself plays through ASIO when an ASIO driver is
installed (your interface's own, or FlexASIO / ASIO4ALL). Without one, it falls
back to WASAPI, the Windows mixer, and Studio's *Audio output* section reads
`host: WASAPI (fallback: no ASIO driver)`. WASAPI plays as many channels as the
device's speaker setup ([step 3](#3-headphones-or-speakers)): a speaker layout
wider than that is refused, with the reason in that section. Widen the speaker
setup, or install an ASIO driver.

## When it does not work

Run mpv from PowerShell as above: every message below appears there. Read the
**first** error. Later ones are often consequences of it.

### 1. `liborender unavailable (…) — decoding natively`

The player found no usable engine. The film plays, but without Omniphony. This
message is followed by `orender_create failed — … If no decoder bridge
loaded, list the bridge libraries in render.bridge_paths …`, which is
misleading here: the bridge is not the problem, the engine is.

- Run the step 1 check with `-v` to see each place the engine was looked for,
  and why a copy was rejected:
  `.\mpv -v --no-config --ao=null --vo=null --length=1 av://lavfi:sine 2>&1 | Select-String orender`
- `The specified module could not be found` in that output, for an
  `orender.dll` that is there: an engine older than 0.7.0, which needs the
  [Microsoft Visual C++ Redistributable (x64)](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist).
  Take the 0.7.0 player, whose engine carries that runtime inside.
- `orender: rejecting '…': ABI major …`: a stale engine, usually the one an
  older Studio left in `%LOCALAPPDATA%\omniphony\lib\`. Update that Studio,
  or delete the file.

### 2. `orender_create failed — decoding natively`, after the engine loaded

The engine is there but could not load the bridge. The reason is on the line
just before it, starting with `orender_create failed:`:

- `no decoder bridge found` with the folders searched: no
  `harletty_*_bridge.dll` is next to `mpv.exe`. Extract them there (step 2).
- `no usable decoder bridge in the auto-discovery folders`: the folder only
  holds libraries this engine refuses, usually the single
  `harletty_bridge.dll` of bridge 0.8. Extract the 0.9.0 bridge (step 2).
- `render.bridge_path (from config) '…' does not exist or is not a file`: the
  config names a path that is wrong. Fix it or remove it (step 2).
- `Bridge plugin … cannot be loaded: it was built against bridge_api …, older
  than the bridge_api 0.6.x this host loads` (or `newer than`): the bridge and
  the engine come from releases that do not match. With the 0.7.0 engine, use
  bridge 0.9.x. If the engine line of step 1 says `(studio install)`, the
  mismatch may be that engine's, not the player's.

### 3. The film plays, but nothing from Omniphony appears

No `engine ready` line, and no error:

- `--ad=orender` is missing.
- The track is not TrueHD, E-AC-3, AC-3, DTS or IAMF. Switch tracks with `#`
  in mpv.
- `--audio-spdif` is set (in the command or in `mpv.conf`): passthrough to the
  receiver takes precedence over the engine.

### 4. The engine or Studio is blocked as an unknown app

The builds are not code-signed yet. If SmartScreen stops an installer,
`mpv.exe` or Studio, choose *More info → Run anyway*. If your antivirus
quarantines `orender.dll` or one of the `harletty_*_bridge.dll` files, restore
it and add the folder as an exception.

### Headphones play only the front, or speakers only two channels

Look at the `AO:` line. Many channels on headphones means the binaural mode was
not active at start: check `output_mode: binaural` (step 3) and restart mpv.
`2ch` on speakers means the Windows output device is set to stereo: change its
speaker setup (step 3).

---

Something else, or none of this helps: open an issue on
[Omniphony](https://github.com/mgth/Omniphony/issues), with the terminal output
of `.\mpv -v --ad=orender film.mkv`.
