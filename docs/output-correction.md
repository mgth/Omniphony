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
stage, so they currently go through the filters like programme audio. A
measurement client that needs the raw speaker turns the correction off first.

## Files

- Mono WAV, PCM (8/16/24/32-bit) or 32-bit float. A multichannel file is
  refused: one file per output.
- Any sample rate. A filter designed at another rate than the engine's is
  resampled when it is loaded, keeping its frequency response.
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

## Control and state

- `/omniphony/control/config/speakers s {"speakerEdits":[{"id":0,"correctionFilter":"/path/FL.wav"}]}`
  sets a speaker's filter, live; `"correctionFilter": null` removes it.
  Layout edits that add, remove, move or replace speakers carry the filters
  with their speakers.
- `/omniphony/control/option output_correction <0|1>` switches the correction.
- `/omniphony/state/speakers` reports, per speaker with a filter,
  `"correction": {"file", "state": "pending" | "loaded" | "error", "taps",
  "sourceRate", "error"}`.

## Latency and cost

The stage adds `127` samples of latency (one 128-sample block, 2.6 ms at
48 kHz) to every output once any output has a filter, and reports it through
the engine's output latency, which hosts use for A/V sync. With no filter
configured the stage is bypassed and adds nothing.

Filters are cut along a non-uniform partition ladder (128, 512, 2048 and
8192-sample blocks): the head of a filter runs on the small block, its tail on
large ones whose work is spread across the small blocks, so a long bass
correction costs little more than a short one. Filters are read, resampled
and partitioned on a background thread; the audio thread swaps them in with a
one-block crossfade and never allocates.
