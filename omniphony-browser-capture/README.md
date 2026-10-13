# Omniphony IAMF capture (prototype)

A Chrome extension for the IAMF audio track (AOMedia Immersive Audio Model
and Formats, YouTube's itag 773) the browser plays. It can

- **capture** it: save the bytes the player appended (`.mp4`, fragmented)
  and the raw OBU stream orender's bridge reads (`.iamf`), when you press
  **Save capture**;
- **stream it live to orender**: the browser is muted and a local orender,
  started by a native messaging host, plays the IAMF track in time with the
  picture.

Nothing leaves the machine.

## Why the TV client

youtube.com in a desktop browser never offers IAMF, even to a Chrome that
decodes it. YouTube's TV client (`youtube.com/tv`) asks the browser whether it
can decode `audio/mp4; codecs="iamf.001.001.Opus"` and serves it when the
answer is yes. The extension presents `youtube.com/tv` tabs as a TV (user
agent and client hints, in the page and on the tab's requests); other YouTube
tabs are left alone.

## Requirements

- Chrome 153 or later: IAMF decoding in Media Source Extensions shipped
  there. Firefox cannot decode IAMF, so the TV client would not ask for it.
- Videos that carry IAMF, e.g. the "Eclipsa Audio" playlist
  (`PL_r7wm6hOG9rAD8d9ruYpVMo__jR0_eNL`). Paid or rented films are
  DRM-protected and cannot be captured.

## Use

1. `chrome://extensions` → enable **Developer mode** → **Load unpacked** →
   select this directory.
2. Open a video on youtube.com, open the extension's popup and click
   **Open in YouTube TV** (or go to `https://www.youtube.com/tv` directly).
   Sign in or choose to watch as a guest, then play the video.
3. The popup shows what the player asked for and the source buffers it
   created. An `audio/mp4; codecs="iamf…"` buffer means YouTube is serving
   IAMF and it is being captured; an `audio/webm; codecs="opus"` one means
   it is not (the video may have no IAMF track). Stats for nerds in the TV
   player shows the audio itag (773 for IAMF).
4. Let it play for as long as you want to capture, then **Save capture**:
   `iamf-capture-<video>-<n>.mp4` and `.iamf` land in your downloads.
5. Play the `.iamf` through orender with a bridge that decodes IAMF
   (harletty's `harletty_iamf_bridge`, or a combined harletty bridge of
   0.8.x built with its `iamf` feature):

   ```
   orender render --config <isolated config> --no-osc --no-continuous --enable-vbap iamf-capture-<video>-0.iamf
   ```

## Live to orender

### Install the host (once)

```
host/install-host.sh --orender <path to orender> --config <isolated config.yaml>
```

The orender must load a bridge that decodes IAMF: harletty's
`libharletty_iamf_bridge.so` (or a combined 0.8.x `libharletty_bridge.so`
built with its `iamf` feature), named in `render.bridge_paths` in that
config. Use an isolated copy of your config:
the host's orender runs alongside the live one (it already passes
`--no-osc`), so the config must not point it at the live input pipe. The
installer writes a launcher under `~/.local/share/omniphony/` and the host
manifest for Chrome and Chromium, allowing only this extension
(`jkoonfghgdmdknbimiahfjfpclfmaflj`, fixed by the manifest's `key`). Logs:
`~/.local/state/omniphony/iamf-host.log` and `iamf-orender.log`.

### Use

Play an IAMF video in YouTube TV, then **Start live** in the popup.

How it keeps time: the page demuxes every appended segment into temporal
units with their presentation times, and sends each one when the video's
`currentTime` plus a lead reaches it. orender plays what it receives as it
receives it, so the lead is orender's latency: 0.2 s for a (re)started
orender to produce its first sample, plus **Latency comp.**, your
calibration of the output latency (default 150 ms; `+` makes the sound come
earlier, `−` later; it restarts orender to apply). Pause and seek stop
orender; play restarts it at the playhead (a ~0.35 s gap). An ad playing in
the same video element stops it too.

Checked in headless Chrome 154 with the extension, the host and orender
(file output) on a YouTube capture streamed through MSE: 50 units/s sent
while playing; seek, pause, resume and live-off restart or stop orender as
described; the rendered audio equals an offline render of the same capture
bit for bit, starting at the unit the playhead had reached plus the lead.

## Limits

- Captures what is appended, in append order: seeking backwards or the
  player re-buffering a range duplicates audio in the capture. Capture a
  straight play-through.
- One capture holds at most 512 MiB (about an hour of 7.1.4 Opus); past that
  it is marked truncated.
- Ads use their own source buffers and are not IAMF, so they are not captured.
- Live: the browser's clock and the audio device's drift apart slowly
  (parts per million); nothing corrects it yet, so a long video may need a
  pause/play to realign. The lead is a manual calibration, not measured.
- Live: the host and its installer are Linux-only for now.

## Tests

The MP4 → raw IAMF conversion and the incremental demuxer (`iamf-mp4.js`)
are checked against the libiamf conformance vectors, and the demuxer against
a YouTube capture when one is given:

```
HARLETTY_IAMF_VECTORS=<libiamf tests dir> OMNIPHONY_IAMF_CAPTURES=<dir of captures> \
  node --test omniphony-browser-capture/test/convert.test.mjs
```

The page script was checked in headless Chrome 154 playing the vector
`test_000220_f.mp4` through Media Source Extensions in uneven chunks: the
saved `.mp4` equals the source file and the `.iamf` equals the vector's raw
stream byte for byte.
