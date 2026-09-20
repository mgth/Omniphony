# Fixed-channel placement: Sphere, Room, Manual

Where a fixed channel goes — a labelled PCM channel of a stream, as opposed
to a dynamic object positioned by metadata — is a policy chosen **per source
family**, because the formats disagree about where their speakers are.
Dolby's bed lives in a cube whose corners are the speakers: `L` is the
front-left corner of the room, whatever angle that makes. Auro-3D states an
angle for every speaker and asks for them all equidistant from the listener:
a sphere. DTS states ITU angles for its lower layer.

Engine: `renderer/src/placement.rs` (the state) and
`orender_engine/src/virtual_bed.rs` (the resolution). Contract background:
[channel-object-contract.md](channel-object-contract.md) ("Declared poses").

## The three modes

| Mode | A virtualised channel becomes | Its nominal position comes from |
|---|---|---|
| **Sphere** | a direction on the listener's sphere, independent of the room | the angle the bridge declares for the label (Auro's setup table, DTS's ETSI table), else the renderer's nominal angle table (BS.2051 where it names the position) |
| **Room** | a corner of the room model, stretched with the room ratio like any object at that position | the corner catalogue (the bundled `layouts/legacy/5.1.yaml`/`7.1.yaml`, then `fallback_virtual_bed_pose`); declared angles are ignored |
| **Manual** | the family's own layout entry, cartesian or polar as written | the entry; a channel without one falls back to Room |

Room is the historical behaviour and the Dolby one: in a cube `L` renders at
45°, not at ITU's 30°, because the model is topological, not angular. Sphere
and Room therefore do not coincide even in a cubic room.

In **every** mode the family's entries still decide two things per channel:
`spatialize` (virtualised, or routed direct to the speaker of the same
label — the LFE's default) and `gain_db` (the input trim). The mode only
decides where a virtualised channel goes.

The Side/Back surround placement (`surround_placement`) is a room-model
detail: for a source without a back pair it moves the room corner of
`Ls`/`Rs` and of the height-tier pair above them (`Lhs`/`Rhs`). It never
moves a sphere direction or a manual entry.

The height tier (`Lh`/`Rh`/`Ch`/`Lhs`/`Rhs`, 30° over the floor speaker of
the same name) has a corner too: on the wall above its floor speaker, at the
height that makes 30° in a cube.

## Families and inheritance

The bridge declares the family of the current presentation
(`FormatBridge::source_family`, a string, read when the labels change):

| Family | Declared by | Built-in default mode |
|---|---|---|
| `dolby` | AC-3, E-AC-3, TrueHD (with or without objects) | Room |
| `dts` | DTS, DTS-HD, DTS:X | Room (its ETSI angles serve Sphere) |
| `auro` | an unfolded Auro-3D carrier | Sphere |
| `pcm` | the reference WAV bridge | Room |
| `generic` | anything else, or an older bridge | Room |

`generic` is also the base the others inherit from: a family with no mode
of its own takes the generic mode when one is set, else its built-in
default; a family with no layout of its own uses the generic layout.

## Config

```yaml
render:
  placement:
    generic:
      mode: manual          # sphere | room | manual; absent = inherit / built-in
      layout:               # the family's entries, speaker-layout schema
        radius_m: 1.0
        speakers:
          - { name: LFE, coord_mode: cartesian, x: 0, y: 1, z: 0, spatialize: false, gain_db: -3 }
          - { name: Ls,  coord_mode: polar, azimuth: -110, elevation: 0, distance: 1 }
    auro:
      mode: sphere
    dts:
      layout: { speakers: [ … ] }   # own entries, mode inherited
```

A config from before this feature carries a single `render.virtual_bed`,
which applied to every stream. It migrates on load into `placement.generic`
in **manual** mode with those entries — every family inherits it — so the
sound is the same after the upgrade; the next save writes `placement` and
drops `virtual_bed`. The key is omitted when everything is at its default.
Profiles carry it like any other `render` key.

## OSC

- `/omniphony/control/placement/mode [family, mode]` — `mode` is `sphere`,
  `room`, `manual`, or `inherit` to clear the family's own choice.
- `/omniphony/control/placement/layout [family, yaml]` — the family's
  entries as a YAML speaker layout; an empty string clears them.
- `/omniphony/control/virtual_bed [yaml]` — legacy, the generic family's
  entries.

All three persist on the next save and re-plan the current stream. A
family's entries are a *partial* set: one channel, or none, is a legitimate
list (only the channels named get their routing, trim or manual pose from
it), so the layout is parsed without the VBAP minimum an output layout
needs.

The `/omniphony/state/renderer` snapshot carries a `placement` block, one
entry per family: `mode` and `layout` (the family's own, `null` when
inherited), `effectiveMode`, and `layoutSource` (`own`, `generic` or
`none`). `fixedChannelProcessing.family` names the family of the stream
being rendered; the legacy `virtualBed` key mirrors the generic entries.

## Checking it end to end

`scripts/placement_e2e.py` runs `orender render` on a fixed-channel stream
with a bridge, registers as an OSC client, and prints the direction of a
few fixed channels as the renderer reports them, then switches the Auro
family through room, manual and back over OSC and prints again. On an
Auro-3D 13.1 carrier extract the sphere phase shows `L` at −30°, `Ls` at
−110° and `Lhs` at −110°/30°; the room phase the corners (`L` −45° in the
cube it forces, `Ls` −90°, `Lhs` −90°/30° on the wall above it); the manual
phase the entries it sent, the rest falling back to the room.

## Engine notes

- Both channel planners key their cache on the family and its effective
  placement, compared by value under the read lock on every frame (a short
  slice and a few scalars), so an edit re-plans on the next frame without
  relying on an options-epoch bump; a steady stream never re-plans.
- The display objects of fixed channels are named by their canonical label
  whatever gave the pose (a layout entry's own spelling, a corner, a
  direction), so a channel keeps its name across the modes.
- The published fixed-channel catalogue carries, per label, the room corner
  (`x`/`y`/`z`) and the nominal direction (`azimuth`/`elevation`), so an
  editor can show either mode's default for a family that is not playing.
  The `Ls`/`Rs` direction uses the 5.1 convention (±110°); a 7.x source
  renders its side pair at ±90° in sphere mode.
