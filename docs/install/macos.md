# macOS: from nothing to a film playing

This page takes you from an empty machine to a film whose sound is rendered by
Omniphony, through **mpv-omniphony** (the player) and the **harletty bridge**
(the decoder). Studio, the control app, is optional and comes last.

The prebuilts are for **Apple Silicon** (M1 and later) only.

Other systems: [Linux](linux.md) · [Windows](windows.md).

## What you download

| Piece | Asset | From |
| --- | --- | --- |
| Player, with the engine inside | `mpv-omniphony-v0.6.0-macos-arm64.zip` | [mpv-v0.6.0](https://github.com/mgth/Omniphony/releases/tag/mpv-v0.6.0) |
| Decoder bridge | `harletty-bridge-v0.8.0-macos-arm64.zip` | [harletty v0.8.0](https://github.com/harletty/harletty-bridge/releases/tag/v0.8.0) |
| Studio (optional) | `omniphony-studio-egui-v0.6.0-macos-arm64.zip` | [v0.6.0](https://github.com/mgth/Omniphony/releases/tag/v0.6.0) |

The engine and the bridge must come from matching releases: the 0.6.0 engine
loads only a 0.8.x bridge, and refuses older and newer ones. When a newer
release is out, take the versions its release notes pair together.

From the next release, the bridge zip holds one library per codec family
instead of one for all: `libharletty_dolby_bridge.dylib`,
`libharletty_dts_bridge.dylib` and `libharletty_iamf_bridge.dylib`. The engine
of that release loads all of them. Step 2 says what changes when you upgrade.

None of these is notarized by Apple yet, so macOS refuses to open them until
you clear the download quarantine, as shown at each step. All commands below
are for the Terminal.

## 1. The player

```bash
cd ~/Downloads
unzip mpv-omniphony-v0.6.0-macos-arm64.zip
mv mpv-omniphony.app /Applications/
xattr -dr com.apple.quarantine /Applications/mpv-omniphony.app
```

The engine, `liborender.dylib`, is inside the app, next to the player binary
`/Applications/mpv-omniphony.app/Contents/MacOS/mpv`. To type `mpv` instead of
that path, add an alias to `~/.zshrc`:

```bash
echo "alias mpv=/Applications/mpv-omniphony.app/Contents/MacOS/mpv" >> ~/.zshrc
source ~/.zshrc
```

**Check:** play one second of a test tone, without sound or picture:

```bash
mpv --no-config --ao=null --vo=null --length=1 av://lavfi:sine
```

The first line names the engine and where it came from:

```text
orender: loaded liborender ABI 0.8 0.6.0 … from /Applications/mpv-omniphony.app/Contents/MacOS/liborender.dylib (next to mpv)
```

The text in the last brackets says which copy was loaded. `(next to mpv)` is
the one from the app. `(studio install)` means a copy deployed by
Studio in `~/Library/Application Support/omniphony/lib/` was found first. That
copy wins over the one in the app, so it must be of the same release as the
bridge.

## 2. The decoder bridge

Keep the bridge in a folder of your own. Do not put it inside the app: that
breaks the app's signature.

```bash
mkdir -p ~/omniphony && cd ~/omniphony
unzip ~/Downloads/harletty-bridge-v0.8.0-macos-arm64.zip     # libharletty_bridge.dylib
xattr -d com.apple.quarantine libharletty_bridge.dylib 2>/dev/null
```

Then tell the engine where it is, in `~/.config/omniphony/config.yaml` (the same
path as on Linux, not `~/Library`):

```bash
mkdir -p ~/.config/omniphony
cat >> ~/.config/omniphony/config.yaml <<EOF
render:
  bridge_path: $HOME/omniphony/libharletty_bridge.dylib
EOF
```

Only append this if the file has no `render:` section yet (Studio writes one when you press Save).
Otherwise, put the `bridge_path:` line under the `render:` already there. The
path must be absolute: `~` is not expanded.

**From the next release**, the zip holds three libraries, one per codec
family. Clear the quarantine of each, delete the old `libharletty_bridge.dylib`
when you upgrade, and list them under `render.bridge_paths`, in the order the
engine tries them:

```bash
cd ~/omniphony
unzip ~/Downloads/harletty-bridge-<version>-macos-arm64.zip   # the three libharletty_*_bridge.dylib
xattr -d com.apple.quarantine libharletty_*_bridge.dylib 2>/dev/null
rm -f libharletty_bridge.dylib
```

```yaml
render:
  bridge_paths:
    - /Users/you/omniphony/libharletty_dolby_bridge.dylib
    - /Users/you/omniphony/libharletty_dts_bridge.dylib
    - /Users/you/omniphony/libharletty_iamf_bridge.dylib
```

A `bridge_path` that still names `libharletty_bridge.dylib` keeps working for
that release: the engine loads the family libraries found in the same folder in
its place, and the next Save writes them as `render.bridge_paths`.

The check for this step is the first playback, in step 4.

## 3. Headphones or speakers

**Headphones.** Add the binaural mode under `render:` in the same file, so it
reads:

```yaml
render:
  bridge_path: /Users/you/omniphony/libharletty_bridge.dylib
  binaural:
    output_mode: binaural
```

**Speakers.** Without that, the engine renders to a 7.1.4 layout (12 channels).
For another room, choose the layout in Studio ([step 5](#5-optional-studio)) and
save. Then give macOS as many channels: open *Audio MIDI Setup*, select your
output device, set its channel count in *Format* if it offers several, then
*Configure Speakers…* and pick your layout. mpv folds the render down to what
the device takes.

The mode is read when the player starts. After changing it, restart mpv.

## 4. Play a film

```bash
mpv --ad=orender ~/Movies/film.mkv
```

`--ad=orender` is required: without it mpv decodes the sound itself. The engine
takes TrueHD, E-AC-3, AC-3 and DTS tracks; any other track plays as in plain mpv.

**Check**, in the Terminal, within a few seconds of the start:

```text
[… INFO  orender_engine::engine] bridge loaded + configured in 0.02s
[… INFO  orender_engine::engine] engine ready in 0.35s (bridge load + VBAP table + renderer build)
AO: [coreaudio] 48000Hz … 2ch float
```

- `bridge loaded` and `engine ready` come from the engine: the bridge is
  loaded and the renderer is built.
- `AO:` comes from mpv: the output device is open. The channel count is `2ch`
  for headphones and your device's count for speakers.

To make `--ad=orender` the default, add `ad=orender` to `~/.config/mpv/mpv.conf`.
Studio's *Activate in mpv config* switch does exactly that. Opening a film by
double-clicking the app also uses that file.

## 5. Optional: Studio

Studio shows the objects in the room and changes the render live. It talks to
the engine inside mpv over OSC.

```bash
cd ~/omniphony
unzip ~/Downloads/omniphony-studio-egui-v0.6.0-macos-arm64.zip
xattr -dr com.apple.quarantine omniphony-studio-egui-v0.6.0-macos-arm64
```

1. Start the player first, with OSC on: `mpv --ad=orender --ad-orender-osc film.mkv`
   (or set `osc: true` under `render:` in the config).
   From the next release, the flag is only needed with a config file that
   leaves OSC off: with no config file the engine turns OSC on by itself, and
   the config Studio then writes when you press Save keeps it on. (A config
   that says `osc: false`, or one saved before that release without
   `osc: true`, leaves it off.)
2. Then, from another Terminal window, start
   `~/omniphony/omniphony-studio-egui-v0.6.0-macos-arm64/omniphony-studio-egui`.
   It connects by itself.

Start them in this order. If Studio finds no renderer for six seconds, it starts
its own (*Auto-start local renderer*, in the connection settings), and that one
holds the port the player needs. When you only use Studio with the player,
switch that option off.

Studio's own renderer reads the same `config.yaml`, so the `bridge_path` (or,
from the next release, `bridge_paths`) of step 2 serves it too. Without one
there, it uses the bridges named in `~/.config/mpv/mpv.conf`
(`ad-orender-bridge-path=`, with absolute paths; from the next release,
several separated by `:`, all handed to the renderer).
Without a bridge it still runs, and Studio shows an orange *No decoder* banner:
films keep playing in the player.

## When it does not work

Run mpv from the Terminal: every message below appears there. Read the
**first** error. Later ones are often consequences of it.

### 1. "mpv-omniphony.app is damaged" or "cannot be opened"

The download quarantine is still set. Run the `xattr` line of the step concerned
(1 for the player, 5 for Studio), then open it again.

### 2. `liborender unavailable (…) — decoding natively`

The player found no usable engine. The film plays, but without Omniphony. This
message is followed by `orender_create failed — … Set render.bridge_path`,
which is misleading here: the bridge is not the problem, the engine is.

- Run the step 1 check with `-v` to see each place the engine was looked for,
  and why a copy was rejected (`orender: rejecting '…': ABI major …`):
  `mpv -v --no-config --ao=null --vo=null --length=1 av://lavfi:sine 2>&1 | grep orender`
- A stale engine in `~/Library/Application Support/omniphony/lib/` (left by an
  older Studio) is tried first. Update that Studio, or delete the file.

### 3. `orender_create failed — decoding natively`, after the engine loaded

The engine is there but could not load the bridge. The reason is on the line
just before it, starting with `orender_create failed:`:

- `render.bridge_path '…' (from config) does not exist or is not a file`: the
  path in the config is wrong or not absolute (step 2).
- `No bridge plugin found` with the folders searched (from the next release,
  `no decoder bridge found`): the config has no `bridge_path` (or
  `bridge_paths`), or mpv did not read that config (step 2).
- `Failed to load bridge plugin from …` with `not valid for use in process` or
  `not allowed`: the bridge is still quarantined. Run its `xattr` line (step 2).
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

Look at the `AO:` line. Many channels on headphones means the binaural mode was
not active at start: check `output_mode: binaural` (step 3) and restart mpv.
`2ch` on speakers means the output device is configured as stereo in *Audio
MIDI Setup* (step 3).

---

Something else, or none of this helps: open an issue on
[Omniphony](https://github.com/mgth/Omniphony/issues), with the Terminal output
of `mpv -v --ad=orender film.mkv`.
