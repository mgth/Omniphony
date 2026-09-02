# Binaural Headphone Output

The renderer has an independent **binaural output stage** for headphones: when
selected, the whole VBAP / crossover / speaker chain is bypassed and every
input channel (beds and objects) is rendered straight to 2-channel stereo
through an HRTF, with interaural time difference (ITD), shoebox early
reflections and live head tracking.

Per channel, per block:

```
position → rotate(head pose) → (azimuth, elevation, distance)
         → air-absorption low-pass (cutoff falls with distance)
         → per-ear ITD delay → per-ear HRIR convolution   (authored level, no 1/d)
         → + 6 first-order shoebox reflections (per-ear delay incl. the image's
             ITD + ILD pan, level relative to the direct: d_source / d_image)
         → + shared late-reverb tail (stereo FDN, send ∝ distance)
         → mix into [L, R]
```

The direct path applies **no distance attenuation**: object and bed levels
are authored by the mixer (Atmos object gain) and are respected as such.
Distance is carried by the *cues* around the direct sound instead — the
reflections and the reverb send are expressed relative to it, so their ratio
to the direct sound falls with distance the way it does in a room, and the
air absorption dulls far sources.

Measured cost: ~0.09 ms per 40-sample block for a 16-channel Atmos stream
(~11 % of the realtime budget), reflections included.

One exception: a bed mapped to a **`spatialize: false` speaker** (the LFE)
keeps its direct-routing intent. Sub-bass carries no usable direction, so the
channel skips the whole pipeline above and feeds **both ears equally at
constant power** (−3 dB each), dry and full-range — no HRIR, no ITD, no
reverb send, and head rotation has no effect on it. Level is unity overall
(no +10 dB LFE convention), matching the speaker path's untouched one-hot
routing.

## Enabling it

Set the output mode in `~/.config/omniphony/config.yaml`:

```yaml
render:
  binaural:
    output_mode: binaural      # "speaker" (default) restores the VBAP path
    unit_scale_m: 1.0
    hrir_source: saf
    head_tracking:
      osc_address: /gamerotationvector
      format: auto
```

> **mpv host**: `ad_orender` fixes the channel count when the decoder
> initialises, so the binaural mode must be **active at boot** (in the config)
> — toggling it during playback changes the render but not the negotiated
> channel layout. Restart mpv after switching modes.

Everything below is also live-tunable from the **Binaural / Headphones** panel
in Studio and over OSC (addresses listed at the end).

## Configuration reference (`render.binaural`)

| Key | Default | Meaning |
|---|---|---|
| `output_mode` | `speaker` | `binaural` enables the headphone stage |
| `unit_scale_m` | `1.0` | metres per ADM unit — isotropic distance scale (the anisotropic `room_ratio` is deliberately not used here) |
| `head_radius_m` | `0.0875` | effective head radius (half the inter-ear distance) for the Woodworth ITD model; fit it to the listener (clamped 0.05–0.15) |
| `hrir_source` | `saf` | `saf`/`kemar` (embedded measured KEMAR), `synthetic` (analytic head shadow), `sofa` (personalised set, needs the `sofa` build feature) |
| `hrtf_sofa_path` | — | SOFA file used when `hrir_source: sofa` |
| `head_tracking.osc_address` | — | OSC address carrying the orientation (empty disables tracking) |
| `head_tracking.format` | `auto` | `auto` / `quat` / `rotvec` / `euler` |
| `reflections.enabled` | `false` | shoebox early reflections (externalization) |
| `reflections.room_width_m` | `4.0` | room extent, x (clamped 1–20 m) |
| `reflections.room_depth_m` | `5.0` | room extent, y |
| `reflections.room_height_m` | `2.7` | room extent, z |
| `reflections.level` | `0.5` | per-reflection wall gain (0–1) |
| `reverb.enabled` | `false` | late-reverb tail (stereo FDN) |
| `reverb.level` | `0.25` | reverb return level (0–1) |
| `reverb.rt60_s` | `0.35` | broadband decay time (s) — living-room-ish, not a hall |
| `reverb.predelay_ms` | `20` | gap between direct sound and tail start |
| `air_absorption` | `true` | distance low-pass on the direct path (HF dies with distance — true outdoors too) |

## Head tracking

Any app or device that sends an orientation over OSC works; the address and
format are free. The reference setup is the Android app **Sensors2OSC** with
the phone strapped to the headband:

1. In Sensors2OSC, enable the **Game Rotation Vector** sensor — *not* the
   plain Rotation Vector. The standard sensor fuses the magnetometer, whose
   filtering adds 20–50 ms of latency and drifts near magnets (headphone
   drivers qualify). Game Rotation Vector is gyro+accelerometer only and
   tracks with no perceptible lag.
2. Point it at the renderer's OSC port (default `9000`) and set
   `head_tracking.osc_address: /gamerotationvector` (`format: auto` handles
   the 4/5-float quaternion payload).
3. If the renderer sees nothing while `tcpdump` does, check the host
   firewall: incoming UDP on the OSC port must be allowed.
4. Put the headphones on, look at the screen, press **Recenter** (Studio
   panel or `/omniphony/control/head/recenter`). That direction becomes
   "front".
5. If the scene rotates the wrong way, toggle **Invert rotation**.

`smoothing` (0–0.99, default 0.2) trades a little latency for pose stability.
It is a time constant, defined for a 30 Hz source: a 100 Hz tracker settles in
the same milliseconds for the same value, not three times slower;
with Game Rotation Vector you can usually lower it.

### Other sources

The setup above uses a phone, but any OSC orientation source works. For the
**Waves Nx Head Tracker** (Bluetooth LE, Linux/BlueZ) there is a small Rust
CLI — **[`nxosc`](https://github.com/mgth/nx-tracker-osc)** — that decodes the
tracker and emits the same `/gamerotationvector` feed, so it drops straight
into the steps above in place of Sensors2OSC:

```sh
nxosc run --profile omniphony --osc-address /gamerotationvector --osc-target 127.0.0.1:9000
```

Keep `head_tracking.osc_address: /gamerotationvector` and `format: auto`.
`nxosc` also has a `--profile scenerotator` mode to drive an IEM SceneRotator
directly instead.

## Usage tips

- **The room is YOUR room, not the scene's.** The reflections and the reverb
  tail model the *listening* room — a constant, small, dry space, exactly like
  the room around a loudspeaker setup. The mix's own acoustics (outdoor
  ambience, cathedral reverb…) are in the content and pass through untouched;
  the brain factors the constant listening-room signature out, and
  externalization actually works best when that signature plausibly matches
  the room you are sitting in. So: keep RT60 short and the levels modest, and
  set the room dimensions roughly to your actual room.
- **Externalization / "inside the head" feeling**: driven by the
  direct-to-reverberant ratio. The late tail (`reverb.*`) does most of the
  work, the early reflections add the room's geometry. Adjust **Reverb
  level** and **Reflection level** by ear — too high colours dialogue and
  sounds echoey, too low collapses back into the head.
- **Distance**: past ~1 m the brain judges distance mostly from the
  direct/reverb ratio, not loudness. The direct sound keeps its authored
  level at any distance (there is no 1/d on it — the mixer set that level),
  so the renderer moves the ratio from the other side: the reverb send grows
  in proportion to the distance (unity at 1.5 m, capped at 6 m) and each
  early reflection is levelled relative to the direct sound
  (`d_source / d_image`), exactly as if the direct had fallen as 1/d and
  been brought back up. Raising `unit_scale_m` therefore makes far objects
  genuinely *sound* far without making them quieter. Air absorption adds
  the matching "far sounds dull" high-frequency roll-off (bypassed within
  3 m, ~14 kHz cutoff at 10 m, ~5 kHz at 30 m).
- **Scale**: `unit_scale_m` sets how far "1 ADM unit" is in metres. At the
  default 1.0 the far wall of the mix is one metre from your nose — try 3–4
  for a room-sized stage. Size the reflection room to contain the scene
  (half-extents ≥ `unit_scale_m`, i.e. 6–8 m wide and deep for a scale of
  3–4): the image-source model pulls a source that sits outside the room
  back inside before mirroring it, which keeps the geometry valid but puts
  the reflections where the wall is, not where the object is.
- **ITD fit**: `head_radius_m` defaults to a KEMAR-ish 8.75 cm. If
  localisation feels smeared, measure ear-to-ear width and set half of it.
- **HRTF**: the embedded measured KEMAR (`saf`) is the best generic default —
  but generic HRTFs rarely deliver elevation: the up/down cues are spectral
  notches carved by *your* pinna, the most individual part of spatial
  hearing. If elevation feels flat or the image sits too high, go HRTF
  shopping: the **Browse…** button next to the HRTF select opens the
  sofacoustics.org database (HUTUBS has 96 measured subjects under
  `database/hutubs/` — try the `*_HRIRs_measured.sofa` files of a dozen
  subjects and keep the best match). A click downloads and activates the
  file live. `synthetic` is the no-measured-HRTF baseline (analytic head
  shadow, no pinna colouration) — useful as an A/B reference.
  SOFA support is compiled into liborender by default (`sofa` feature).
- **Head-tracking reaction latency under mpv**: rendered audio waits in mpv's
  output queue, so rotation is only audible once that queue drains. Set
  `audio-buffer=0.05` in `mpv.conf` (default is 0.2 s) to cut the dominant
  term. The Studio 3D head has its own low-latency pose channel and is not
  affected by the audio buffer.
- The output is plain stereo FL/FR — no special player-side configuration
  beyond a stereo sink.

## HRTF data licensing

The SOFA *format* is an open AES standard; the *data* is not uniformly
licensed — sofacoustics.org aggregates databases that each keep their own
terms (HUTUBS is CC BY 4.0; some Aachen/ITA sets are CC BY-NC-SA; some files
carry no license at all). Accordingly:

- Omniphony never redistributes SOFA data: the browser downloads straight
  from sofacoustics.org to your machine, on demand, with a local cache (the
  app is just a user agent, like a web browser).
- Each file's embedded `GLOBAL:License` / `AuthorContact` / `Organization`
  attributes are read after download and shown in the browser (local list and
  post-download status); non-commercial or missing licenses are flagged in
  amber. A missing license legally means all rights reserved — contact the
  author before anything beyond private listening.
- The only bundled HRTF data is the embedded SAF KEMAR set (ISC license).
- If you redistribute downloaded files yourself, the file's own license
  applies to you — prefer CC BY / CC0 databases (e.g. HUTUBS).

## OSC control surface

| Address | Args | Meaning |
|---|---|---|
| `/omniphony/control/output_mode` | `s: speaker\|binaural` | select the output stage |
| `/omniphony/control/binaural/hrir_source` | `s: synthetic\|saf\|sofa:<path>` | HRIR set |
| `/omniphony/control/binaural/unit_scale` | `f` (m/unit) | distance scale |
| `/omniphony/control/binaural/head_radius` | `f` (m) | ITD head radius |
| `/omniphony/control/binaural/reflections/enabled` | `i\|f` (bool) | reflections on/off |
| `/omniphony/control/binaural/reflections/level` | `f` (0–1) | reflection gain |
| `/omniphony/control/binaural/reflections/room_width` | `f` (m) | room x |
| `/omniphony/control/binaural/reflections/room_depth` | `f` (m) | room y |
| `/omniphony/control/binaural/reflections/room_height` | `f` (m) | room z |
| `/omniphony/control/binaural/reverb/enabled` | `i\|f` (bool) | late tail on/off |
| `/omniphony/control/binaural/reverb/level` | `f` (0–1) | reverb return level |
| `/omniphony/control/binaural/reverb/rt60` | `f` (s) | decay time |
| `/omniphony/control/binaural/reverb/predelay` | `f` (ms) | pre-delay |
| `/omniphony/control/binaural/air_absorption` | `i\|f` (bool) | distance HF roll-off |
| `/omniphony/control/head/orientation` | `fff` (euler) | set pose directly |
| `/omniphony/control/head/quat` | `ffff` | set pose directly |
| `/omniphony/control/head/recenter` | — | current orientation becomes "front" |
| `/omniphony/control/head/tracking/address` | `s` | tracking OSC address ("" disables) |
| `/omniphony/control/head/tracking/format` | `s` | `auto\|quat\|rotvec\|euler` |
| `/omniphony/control/head/tracking/smoothing` | `f` (0–0.99) | pose smoothing |
| `/omniphony/control/head/tracking/invert` | `i` (bool) | mirror the rotation |

State broadcast: the `binaural` object inside `/omniphony/state/renderer`
(10 Hz when the pose moves) — including `hrirEffective`, the set actually
being convolved, and `hrirError`: when a SOFA file cannot be loaded the
renderer falls back to the embedded KEMAR set, and these two say so
(`hrirSource` keeps the request) — plus a dedicated lightweight
`/omniphony/state/head_pose` (`ffff` = w x y z, ~30 Hz) for low-latency pose
consumers such as the Studio 3D head.
