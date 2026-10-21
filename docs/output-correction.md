# Output correction (room correction filters)

Each physical output can be convolved with a FIR filter: a room correction
designed for that speaker at the listening position. Omniphony applies the
filters; it does not design them. They come from Omniphony Companion or from
any tool that exports an impulse response as a mono WAV file (REW, rePhase,
DRC-FIR, …).

## Where the filter runs

On the speaker path only, after the mix and before each output's gain and
delay:

```
objects/beds → crossover + panning → speaker feeds → correction FIR → gain → delay → output
```

So the filter shapes the speaker's response and leaves level and time
alignment to the output gain and delay. The headphone paths (binaural, BRIR)
never use the filters: they belong to the room's speakers.

Measurement signals (`docs/measurement-protocol.md`) are injected before this
stage, so they go through the filters like programme audio. A measurement
client that needs the raw speaker begins its session with
`bypass_output_filters` (protocol 8): the outputs then play through the
identity, at the same latency, until the session ends.

## Files

- Mono WAV, PCM (8/16/24/32-bit) or 32-bit float. A multichannel file is
  refused: one file per output.
- At the engine's rate, or at any rate from 32 to 384 kHz: a filter designed
  at another rate than the engine's is resampled when it is loaded, keeping
  its magnitude and phase up to 90 % of the lower of the two Nyquist
  frequencies. To keep the start of the response, a resampled filter is moved
  later by the resampling kernel's half-width (about 1.3 ms, 2 ms at most);
  every output is then delayed by the same amount, filtered or not and with
  the correction on or off, and the delay is part of the reported latency.
  Filters at the engine rate add nothing.
- Up to 1.5 s long at the engine rate. Trailing zeros are dropped.
- The filter is applied as it is: no normalisation. A filter that boosts
  should come with its own headroom.

A file that cannot be used (missing, multichannel, too long, silent) leaves
its output unfiltered, and the reason is reported (see *State* below).

## Configuration

Per speaker, in the layout (`config.yaml`, `render.current_layout.speakers[]`
and layout files):

```yaml
- name: FL
  azimuth: -30.0
  elevation: 0.0
  correction_filter: /home/me/.config/omniphony/room-correction/FL.wav
```

The `output_correction` live option (default `true`) switches every filter on
or off at once. Off, the outputs keep going through the stage unfiltered: the
latency does not change, so switching is a fair A/B comparison.

Both are render settings: they reach `config.yaml` only through Save
(`docs/persistence-policy.md`).

### Filters from a measurement client

A measurement client (Omniphony Companion) may run on another machine than
the renderer, so it hands its filters over as samples rather than files
(`set_output_filters`, protocol 8). Such a filter is live state, kept by
output name over the layout: it takes the place of the speaker's file until
the Save writes it as a mono 32-bit float WAV file under
`room-correction/<profile>/` next to `config.yaml` and names it in the
speaker's `correction_filter`. Layout edits carry it with its speaker as they
carry a file; choosing a file for the speaker replaces it; a Reload, a
profile switch or an engine handoff drops it unsaved.

## Control and state

- `/omniphony/control/config/speakers s {"speakerEdits":[{"id":0,"correctionFilter":"/path/FL.wav"}]}`
  sets a speaker's filter, live; `"correctionFilter": null` removes it.
  Layout edits that add, remove, move or replace speakers carry the filters
  with their speakers.
- `/omniphony/control/option output_correction <0|1>` switches the correction.
- `/omniphony/state/speakers` reports, per speaker with a filter,
  `"correction": {"origin": "file" | "live", "file", "state": "pending" |
  "loaded" | "error", "taps", "sourceRate", "sourceTaps", "hash", "error"}`
  (`file` for a file only; `taps` at the engine rate, `sourceTaps` and
  `hash` at the filter's own rate), and `"correctionBypassed": true` while a
  measurement session holds every filter off.

## Latency and cost

The stage adds `127` samples of latency (one 128-sample block, 2.6 ms at
48 kHz), plus the move of a resampled filter (see *Files*), to every output
once any output has a filter, and reports it through
the engine's output latency, which hosts use for A/V sync. With no filter
configured the stage is bypassed and adds nothing.

Filters are cut along a non-uniform partition ladder (128, 512, 2048 and
8192-sample blocks): the head of a filter runs on the small block, its tail on
large ones whose work is spread across the small blocks, so a long bass
correction costs little more than a short one. Filters are read, resampled
and partitioned on a background thread, which also builds the buffers the
stage plays through; the audio thread swaps them in with a one-block
crossfade and hands what it replaces to a second thread that only frees, so
it neither allocates nor frees, live transitions included and even while
the first thread is busy preparing a set. Should that second thread fall
behind, the stage keeps playing the filters it has and takes the latest
change once it catches up, rather than free anything itself.
