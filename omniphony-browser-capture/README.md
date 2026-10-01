# Omniphony IAMF capture (prototype)

A Chrome extension that records the IAMF audio track (AOMedia Immersive
Audio Model and Formats, YouTube's itag 773) the browser plays, and saves it
both as the bytes the player appended (`.mp4`, fragmented) and as the raw OBU
stream orender's bridge reads (`.iamf`). It only captures: playback in the
browser is unchanged, nothing is sent anywhere, and the files are saved only
when you press **Save capture**.

It is the first step towards rendering YouTube's IAMF through orender; the
next is streaming the same bytes to orender live instead of saving them.

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
5. Play the `.iamf` through orender with a bridge built with the `iamf`
   feature (harletty-bridge):

   ```
   orender render --config <isolated config> --no-osc --no-continuous --enable-vbap iamf-capture-<video>-0.iamf
   ```

## Limits

- Captures what is appended, in append order: seeking backwards or the
  player re-buffering a range duplicates audio in the capture. Capture a
  straight play-through.
- One capture holds at most 512 MiB (about an hour of 7.1.4 Opus); past that
  it is marked truncated.
- Ads use their own source buffers and are not IAMF, so they are not captured.

## Tests

The MP4 → raw IAMF conversion (`iamf-mp4.js`) is checked against the libiamf
conformance vectors:

```
HARLETTY_IAMF_VECTORS=<libiamf tests dir> node --test omniphony-browser-capture/test/convert.test.mjs
```

The page script was checked in headless Chrome 154 playing the vector
`test_000220_f.mp4` through Media Source Extensions in uneven chunks: the
saved `.mp4` equals the source file and the `.iamf` equals the vector's raw
stream byte for byte.
