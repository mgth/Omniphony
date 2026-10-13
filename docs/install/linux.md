# Linux: from nothing to a film playing

This page takes you from an empty machine to a film whose sound is rendered by
Omniphony, through **mpv-omniphony** (the player) and the **harletty bridge**
(the decoder). Studio, the control app, is optional and comes last.

Other systems: [Windows](windows.md) · [macOS](macos.md).

## The installer

`omniphony-v0.7.0-linux-x86_64.tar.gz`, on the
[v0.7.0](https://github.com/mgth/Omniphony/releases/tag/v0.7.0) release page,
holds the player, the engine and Studio of that release in one download, and
puts each where the others look for it. It needs no root and writes no config
file. It runs on any x86_64 distribution with glibc 2.38 or newer and
PipeWire.

```bash
tar -xzf ~/Downloads/omniphony-v0.7.0-linux-x86_64.tar.gz
cd omniphony-v0.7.0-linux-x86_64
./install.sh
```

| Installed | Where |
| --- | --- |
| The player, as `mpv-omniphony` | `~/.local/lib/omniphony/mpv-omniphony.AppImage`, linked from `~/.local/bin/` |
| Studio and `orender` | `~/.local/lib/omniphony/studio/`, linked from `~/.local/bin/` |
| The engine | `~/.local/share/omniphony/lib/`, where the player, Studio and `orender` look before their own copy |
| Menu entries | Omniphony Studio, and mpv-omniphony started with `--ad=orender` |

The distribution's own `mpv` is untouched: the player is `mpv-omniphony`.

**The decoder bridge is not in the installer.** Take it as in
[step 2](#2-the-decoder-bridge) and put its three libraries into
`~/.local/share/omniphony/lib/`, next to the engine: every host finds them
there without any option. The installer ends by saying whether it found a
bridge the engine accepts; run it again after adding one, or ask directly:

```bash
orender check-bridges
```

```text
host: bridge_api 0.6.x
loaded: /home/you/.local/share/omniphony/lib/libharletty_dolby_bridge.so (dolby)
loaded: /home/you/.local/share/omniphony/lib/libharletty_dts_bridge.so (dts, auro)
loaded: /home/you/.local/share/omniphony/lib/libharletty_iamf_bridge.so (iamf)
```

**The renderer as a service.** `./install.sh --service` also installs and
starts `omniphony-renderer` as a systemd user service, the same unit Studio's
*Install service* writes. Most setups do not need it: the player carries its
own engine. Without it, nothing runs in the background.

**Upgrade** by running the `install.sh` of the newer release. **Remove** with
`~/.local/lib/omniphony/uninstall.sh`: it deletes what the installer wrote and
keeps `config.yaml`, Studio's preferences, `mpv.conf` and the bridge.

Then go to [step 3](#3-headphones-or-speakers). The rest of this page is the
same install by hand.

## What you download

| Piece | Asset | From |
| --- | --- | --- |
| Player, with the engine inside | `mpv-omniphony-v0.7.0-linux-x86_64.zip` (Ubuntu 24.04) or `mpv-omniphony-v0.7.0-linux-x86_64.AppImage` | [mpv-v0.7.0](https://github.com/mgth/Omniphony/releases/tag/mpv-v0.7.0) |
| Decoder bridge | `harletty-bridge-v0.9.0-linux-x86_64.zip` | [harletty v0.9.0](https://github.com/harletty/harletty-bridge/releases/tag/v0.9.0) |
| Studio (optional) | `omniphony-studio-egui_0.7.0_amd64.deb`, `omniphony-studio-egui_0.7.0_x86_64.AppImage` or `omniphony-studio-egui-v0.7.0-linux-x86_64.tar.gz` | [v0.7.0](https://github.com/mgth/Omniphony/releases/tag/v0.7.0) |

The engine and the bridge must come from matching releases: the 0.7.0 engine
loads only a 0.9.x bridge, and refuses older and newer ones. When a newer
release is out, take the versions its release notes pair together.

The bridge zip holds one library per codec family:
`libharletty_dolby_bridge.so`, `libharletty_dts_bridge.so` and
`libharletty_iamf_bridge.so`. The engine loads all of them. Step 2 says what
to do when you upgrade from the single `libharletty_bridge.so` of 0.8.

### Which distribution each prebuilt runs on

| Asset | Built on | Runs on |
| --- | --- | --- |
| `mpv-omniphony-…-linux-x86_64.zip` | Ubuntu 24.04 | **Ubuntu 24.04 and its derivatives only.** It links the system's FFmpeg 6.1 (`libavcodec.so.60`) and libplacebo (`libplacebo.so.338`). |
| `mpv-omniphony-…-linux-x86_64.AppImage` | Ubuntu 24.04 | Any x86_64 desktop with glibc 2.38 or newer. It carries its own libraries. |
| `harletty-bridge-…-linux-x86_64.zip` | Ubuntu 24.04 | Any x86_64 distribution with glibc 2.39 or newer. It needs nothing else, except that the IAMF library (`libharletty_iamf_bridge.so`) also needs the system's Opus library, `libopus.so.0` (`libopus0` on Ubuntu, usually already installed). |
| `omniphony-studio-egui…` (deb, AppImage, archive) | Ubuntu 22.04 | Any x86_64 distribution with glibc 2.35 or newer and PipeWire. The deb is for Debian and Ubuntu. |

**On Arch and its derivatives**, skip the downloads: the whole stack is on the
AUR, built on your machine. `paru -S mpv-omniphony harletty-bridge` installs the
player, the engine and the bridge (its three family libraries, into
`/usr/lib/orender/`, where the engine looks without any configuration; an
upgrade removes the old `libharletty_bridge.so`). Continue at
[step 3](#3-headphones-or-speakers).

**On Fedora, openSUSE and other distributions**, the player zip does not start
(see [failure 1](#1-error-while-loading-shared-libraries)). Use
[the installer](#the-installer), or take the player's AppImage on its own:
make it executable, run it wherever the steps below say `./mpv`, and put the
bridge libraries in `~/.local/share/omniphony/lib/`, where the engine also
looks. The Studio prebuilts work as they are. The bridge prebuilt needs glibc
2.39, as the table says: it runs on Fedora 40 and newer. On Debian 12 (glibc
2.36) neither the AppImage nor the bridge prebuilt runs: build the player from
[mpv-omniphony](https://github.com/mgth/mpv-omniphony) and the bridge from
[harletty-bridge](https://github.com/harletty/harletty-bridge).

## 1. The player

```bash
mkdir -p ~/omniphony && cd ~/omniphony
unzip ~/Downloads/mpv-omniphony-v0.7.0-linux-x86_64.zip
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
orender: loaded liborender ABI 0.12 0.7.0 … from /home/you/omniphony/liborender.so.0 (next to mpv)
```

The text in the last brackets says which copy was loaded. `(next to mpv)` is
the one from the zip. `(studio install)` means a copy deployed by
Studio in `~/.local/share/omniphony/lib/` was found first. That copy wins over
the one next to mpv, so it must be of the same release as the bridge.

## 2. The decoder bridge

Unzip it **into the same folder**. The engine loads every `*_bridge.so` file
it finds next to the player, so no configuration is needed:

```bash
cd ~/omniphony
unzip ~/Downloads/harletty-bridge-v0.9.0-linux-x86_64.zip
```

That adds three libraries, one per codec family:

```text
libharletty_dolby_bridge.so   libharletty_dts_bridge.so   libharletty_iamf_bridge.so
```

The IAMF one needs the system's Opus library (`sudo apt install libopus0`,
`sudo dnf install opus`; usually already there). Without it that library is
refused and the two others still load.

If your `~/.config/omniphony/config.yaml` already exists (Studio writes it when
you press Save) and names bridges, those paths are used instead of the search,
and they must point at these files. Remove the key, or list the libraries
under `render.bridge_paths`, with absolute paths, in the order the engine
tries them:

```yaml
render:
  bridge_paths:
    - /home/you/omniphony/libharletty_dolby_bridge.so
    - /home/you/omniphony/libharletty_dts_bridge.so
    - /home/you/omniphony/libharletty_iamf_bridge.so
```

**Upgrading from 0.6.0 and bridge 0.8:**

- Delete the old `libharletty_bridge.so`. The engine refuses it, says so, and
  loads the others, but it has no use left.
- A `render.bridge_path` that still names `libharletty_bridge.so` keeps
  working in 0.7.0: the engine loads the family libraries found in the same
  folder in its place, and the next Save writes them as `render.bridge_paths`.
  Removing the line, so that the engine finds them on its own, works too.

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
takes TrueHD, E-AC-3, AC-3, DTS and IAMF tracks; any other track plays as in
plain mpv.

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

Take one of the three forms. The deb, on Debian and Ubuntu:

```bash
sudo apt install ~/Downloads/omniphony-studio-egui_0.7.0_amd64.deb
```

The AppImage: make it executable and run it. Or the archive, which runs from
its folder:

```bash
mkdir -p ~/omniphony/studio && cd ~/omniphony/studio
tar xzf ~/Downloads/omniphony-studio-egui-v0.7.0-linux-x86_64.tar.gz --strip-components=1
```

1. Start the player first: `~/omniphony/mpv --ad=orender film.mkv`. With the
   AUR packages the player is on your `PATH`: `mpv --ad=orender film.mkv`.
   With no config file the engine turns OSC on by itself, and the config
   Studio writes when you press Save keeps it on. A config that says
   `osc: false`, or one saved by 0.6.0 without `osc: true`, leaves it off:
   add `--ad-orender-osc` to the command, or set `osc: true` under `render:`.
2. Then, from another terminal, start `omniphony-studio-egui` (installed by
   the deb, or by the AUR's `paru -S omniphony-studio`), or
   `~/omniphony/studio/omniphony-studio-egui` from the archive. It connects by
   itself.

Start them in this order. If Studio finds no renderer for six seconds, it starts
its own (*Auto-start local renderer*, in the connection settings), and that one
holds the port the player needs. When you only use Studio with the player,
switch that option off.

Studio's own renderer uses the bridges your `mpv.conf` names
(`ad-orender-bridge-path=`, with absolute paths), else it looks next to its
`orender`, in `~/.local/share/omniphony/lib/`, then in `/usr/lib/orender/`
(where the AUR puts them). It does not know the player's folder: bridges that
only sit next to `mpv`, with no `mpv.conf` line naming them, are not found.
Without one it still runs, and Studio shows an orange *No decoder* banner:
films keep playing in the player. To give it the bridges too, copy the three
`libharletty_*_bridge.so` files into `~/.local/share/omniphony/lib/`, a folder
the player also searches, or list them in `~/.config/mpv/mpv.conf`, separated
by `:` (the player reads the same list):

```text
ad-orender-bridge-path=/home/you/omniphony/libharletty_dolby_bridge.so:/home/you/omniphony/libharletty_dts_bridge.so:/home/you/omniphony/libharletty_iamf_bridge.so
```

A line that still names `libharletty_bridge.so` keeps working in 0.7.0, even
once the file is deleted: the engine loads the family libraries of the same
folder in its place.

## 6. Optional: the engine at login

The Studio deb and the AUR `orender` package install the engine as a systemd
user service, `omniphony-renderer`. It runs
`/usr/bin/orender` on its own: it renders what arrives on its input pipe
(`/tmp/orender.pipe`) and answers Studio on OSC port 9000 without Studio
having to start it. The packages install it but do not turn it on. To run it
at every login, starting now:

```bash
systemctl --user enable --now omniphony-renderer
```

To stop it until the next login, `systemctl --user stop omniphony-renderer`.
To stop starting it at login, `systemctl --user disable --now omniphony-renderer`.
Its messages: `journalctl --user -u omniphony-renderer`.

- **It steps aside for the player.** When mpv starts with its engine's OSC on
  (step 5), the player's engine asks it to yield: it releases the OSC port and
  the audio output, waits, and takes both back when the player exits. A player
  with OSC off does not ask, and then both play at once: stop the service, or
  turn OSC on in the player.
- **Studio sees it.** The service controls in Studio's connection settings
  show it as installed and running, *Restart service* and *Uninstall service*
  act on it (uninstalling disables it; the packaged file stays), and Studio
  starts no renderer of its own while it runs.
- **It uses Studio's default settings**: OSC on `127.0.0.1:9000`, log level
  `info`. For other ones, press *Install service* in Studio: it
  writes the same unit with your settings to `~/.config/systemd/user/`, which
  takes precedence over the packaged one. Studio started from the portable
  archive does the same with its own `orender`.
- **Not from the AppImage.** Studio started from its AppImage refuses to
  install the service: the AppImage's `orender` disappears when Studio closes,
  and the service would no longer start. Install the deb or the AUR package
  instead.

## When it does not work

Run mpv from a terminal: every message below appears there. Read the **first**
error. Later ones are often consequences of it.

### 1. `error while loading shared libraries`

```text
./mpv: error while loading shared libraries: libavcodec.so.60: cannot open shared object file
```

The player zip only runs on Ubuntu 24.04. On Ubuntu 24.04, run
`sudo apt install mpv` and try again. On any other distribution, use the
installer or the AppImage: see
[Which distribution each prebuilt runs on](#which-distribution-each-prebuilt-runs-on).

### 2. `liborender unavailable (…) — decoding natively`

The player found no usable engine. The film plays, but without Omniphony. This
message is followed by `orender_create failed — … If no decoder bridge
loaded, list the bridge libraries in render.bridge_paths …`, which is
misleading here: the bridge is not the problem, the engine is.

- Run the step 1 check with `-v` to see each place the engine was looked for,
  and why a copy was rejected (`orender: rejecting '…': ABI major …`):
  `./mpv -v --no-config --ao=null --vo=null --length=1 av://lavfi:sine 2>&1 | grep orender`
- Check that `liborender.so.0` is still next to `mpv`.
- A stale engine in `~/.local/share/omniphony/lib/` (left by an older
  Studio) is tried first. Update that Studio, or delete the file.

### 3. `orender_create failed — decoding natively`, after the engine loaded

The engine is there but could not load the bridge. The reason is on the line
just before it, starting with `orender_create failed:`:

- `no decoder bridge found` with the folders searched: no
  `libharletty_*_bridge.so` is next to `mpv`. Unzip them there (step 2).
- `no usable decoder bridge in the auto-discovery folders`: the folder only
  holds libraries this engine refuses, usually the single
  `libharletty_bridge.so` of bridge 0.8. Unzip the 0.9.0 bridge (step 2).
- `render.bridge_path (from config) '…' does not exist or is not a file`:
  the config names a path that is wrong. Fix it or remove it (step 2).
- `Bridge plugin … cannot be loaded: it was built against bridge_api …, older
  than the bridge_api 0.6.x this host loads` (or `newer than`): the bridge and
  the engine come from releases that do not match. With the 0.7.0 engine, use
  bridge 0.9.x. If the engine line of step 1 says `(studio install)`, the
  mismatch may be that engine's, not the player's.

### 4. The film plays, but nothing from Omniphony appears

No `engine ready` line, and no error:

- `--ad=orender` is missing.
- The track is not TrueHD, E-AC-3, AC-3, DTS or IAMF. Switch tracks with `#`
  in mpv.
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
