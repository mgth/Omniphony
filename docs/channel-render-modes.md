# Fixed-channel source processing

This module controls sources whose spatial description is a set of labelled,
fixed channels: stereo, 5.1, 7.1, or a fixed 7.1.4 presentation. A stream that
actually carries dynamic objects keeps that fact (`has_objects`) and bypasses
the synthetic-object stages described below.

## Fixed-channel rendering

Omniphony renders every fixed channel according to its entry in the virtual-bed
layout:

- `spatialize: true` places the channel at its configured fixed position and
  renders it through VBAP over the output layout;
- `spatialize: false` routes the channel directly to the output speaker with
  the matching label. LFE is direct by default, so it reaches the subwoofer
  rather than becoming a spatialized source.

Direct and virtualized channels can coexist in one source. Disabling every
channel's `spatialize` flag is therefore the explicit fixed-speaker-only mode;
it does not require, and does not create, any objects.

Studio always exposes the common 7.1.4 catalogue (`L`, `R`, `C`, `LFE`, `Ls`,
`Rs`, `Lb`, `Rb`, `TFL`, `TFR`, `TBL`, `TBR`) so the layout can be prepared
offline. Configured extra labels and labels discovered in the current session
are merged into that catalogue. Controls remain editable when no compatible
stream is playing.

`surround_placement` selects whether the single surround pair of a 4.x/5.x
source is treated as side or back. Sources with dedicated back channels ignore
this choice.

## Synthetic objects

`synthetic_objects_enabled` is a master gate for renderer-generated objects. It
does not change the fixed-channel routes and it does not erase its child
settings when turned off.

Two independent stages can be configured and can run together:

1. Phantom extraction runs first and subtracts the extracted contribution from
   the fixed bed before exposing it as planar synthetic objects.
2. The selected height generator then derives height objects from the remaining
   fixed-channel signal.

Phantom extraction has three explicit modes:

- `off`: no extraction;
- `broadband`: correlated extraction over the full band;
- `spectral`: per-band extraction.

The height generator is selected with `object_generator_id`; `none` disables
it. A selected generator only runs for a fixed-channel source without existing
height channels and when the output layout has height speakers.

Studio reports why each configured stage is currently inactive (`master_off`,
`no_stream`, `object_stream`, `input_has_height`, `output_has_no_height`, or
`insufficient_channels`) instead of hiding or disabling its controls.

## Configuration and live control

The persistent fields live under `render`:

```yaml
render:
  surround_placement: side
  synthetic_objects_enabled: false
  phantom_extract_mode: off
  object_generator_id: none
  virtual_bed:
    speakers:
      - name: LFE
        spatialize: false
```

The declared options are available through `/omniphony/control/option` and the
dedicated compatibility addresses documented in
[`osc-control-contract.md`](osc-control-contract.md). Old `phantom_enabled` plus
the phantom `method` parameter migrate to `phantom_extract_mode`. The former
global `render.channel_render_mode` key is read only for compatibility and is
dropped on save.

## Host override

The engine still has an internal `spatial`/`host` switch for an embedding host.
It is intentionally absent from Studio and persistent user options:

- the mpv bridge uses it for its explicit decoder override and fallback probe;
- `orender decode --channel-render-mode host|spatial` keeps the diagnostic CLI
  path (`direct` and `virtual` remain aliases of `spatial`).

In normal Studio operation the policy is always `spatial`, with the actual
direct-versus-virtual decision made per channel as described above.
