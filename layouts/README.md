# Speaker Layout Files

This directory contains speaker layout configurations used by Omniphony.
Where a public reference layout is applicable, the corresponding ITU-R family is noted.

The current convention for the checked-in layouts is:

- `coord_mode: "cartesian"` for every speaker entry
- normalized coordinates in the Omniphony Cartesian space
- `x`, `y`, and `z` written with one decimal place
- `LFE` speakers marked with `spatialize: false`

## Available Layouts

### 2.0.yaml
Standard stereo layout aligned with ITU-R BS.775, stored in normalized Cartesian coordinates.
- Speakers: 2

### 2.1.yaml
Stereo plus LFE draft layout for PCM bridge work, stored in normalized Cartesian coordinates.
- Speakers: 3 including LFE

### 4.0.yaml
Quadraphonic draft layout for PCM bridge work, stored in normalized Cartesian coordinates.
- Speakers: 4

### 4.1.yaml
Quadraphonic plus LFE draft layout for PCM bridge work, stored in normalized Cartesian coordinates.
- Speakers: 5 including LFE

### 5.0.yaml
Standard 5.0 surround layout aligned with ITU-R BS.775, stored in normalized Cartesian coordinates.
- Speakers: 5

### 5.1.yaml
Standard 5.1 surround layout aligned with ITU-R BS.775, stored in normalized Cartesian coordinates.
- Speakers: 6 including LFE

### 6.1.yaml
Rear-center 6.1 surround layout stored in normalized Cartesian coordinates.
- Speakers: 7 including LFE

### 7.1.yaml
Standard 7.1 surround layout in the same naming family as common ITU-R speaker sets, stored in normalized Cartesian coordinates.
- Speakers: 8 including LFE

### 7.1.2.yaml
7.1 bed plus 2 height channels, stored in normalized Cartesian coordinates.
- Speakers: 10 including LFE

### 7.1.4.yaml
7.1 bed plus 4 height channels, aligned with ITU-R BS.2051 style immersive layouts and stored in normalized Cartesian coordinates.
- Speakers: 12 including LFE

### 9.1.6.yaml
9.1 bed plus 6 height channels, aligned with ITU-R BS.2051 style immersive layouts and stored in normalized Cartesian coordinates.
- Speakers: 16 including LFE

## Usage

Use `--speaker-layout` to load a layout file when decoding with VBAP:

```bash
orender render --enable-vbap --speaker-layout layouts/7.1.4.yaml input.bin
```

Or use a built-in preset in code:

```rust
use omniphony_renderer::speaker_layout::SpeakerLayout;

let layout = SpeakerLayout::preset("7.1.4")?;
```

## YAML Format

Speaker layout files use this format:

```yaml
name: "layout name"          # Optional
radius_m: 1                  # Optional UI scale
speakers:
  - name: "FL"                  # Speaker name (for reference)
    coord_mode: "cartesian"     # Repository default
    x: -1.0                     # Right/left
    y: 1.0                      # Front/back
    z: 0.0                      # Up/down
    spatialize: true            # false for direct/non-VBAP speakers such as LFE
    delay_ms: 0                 # Optional per-speaker delay
  # ... more speakers
```

### Coordinate System

- **Cartesian**
  - `x`: right positive, left negative
  - `y`: front positive, rear negative
  - `z`: up positive, down negative
  - checked-in layouts use normalized values and keep one decimal place

- **Polar**
  - still supported by the parser for external/custom layouts
  - azimuth: `0°` = front, `-90°` = left, `+90°` = right, `±180°` = rear
  - elevation: `0°` = horizontal, `+90°` = zenith, `-90°` = nadir

## Creating Custom Layouts

You can create custom layout files by copying and modifying one of these standards.

**Requirements**:
1. At least 3 speakers for VBAP-capable layouts
2. All speaker names must be unique
3. Cartesian coordinates should stay within the normalized Omniphony room space
4. LFE should be marked `spatialize: false`

**Tips**:
- LFE should remain non-spatialized
- Height speakers typically use positive `z`
- Symmetrical layouts work best for spatial accuracy
- Keep checked-in coordinates normalized and formatted with one decimal place

## Example: Custom 5.1.2 Layout

```yaml
# 5.1 + 2 height speakers
name: "custom 5.1.2"
radius_m: 1
speakers:
  # Front layer
  - name: "FL"
    coord_mode: "cartesian"
    x: -1.0
    y: 1.0
    z: 0.0
    spatialize: true
  - name: "FR"
    coord_mode: "cartesian"
    x: 1.0
    y: 1.0
    z: 0.0
    spatialize: true
  - name: "C"
    coord_mode: "cartesian"
    x: 0.0
    y: 1.0
    z: 0.0
    spatialize: true
  - name: "LFE"
    coord_mode: "cartesian"
    x: 0.0
    y: 1.0
    z: 0.0
    spatialize: false

  # Rear surround
  - name: "BL"
    coord_mode: "cartesian"
    x: -1.0
    y: -1.0
    z: 0.0
    spatialize: true
  - name: "BR"
    coord_mode: "cartesian"
    x: 1.0
    y: -1.0
    z: 0.0
    spatialize: true

  # Height layer
  - name: "TFL"
    coord_mode: "cartesian"
    x: -1.0
    y: 1.0
    z: 1.0
    spatialize: true
  - name: "TFR"
    coord_mode: "cartesian"
    x: 1.0
    y: 1.0
    z: 1.0
    spatialize: true
```

## Testing Your Layout

You can test if your layout file is valid by trying to load it:

```bash
# This validates the layout during startup
orender render --enable-vbap --speaker-layout my_layout.yaml --help
```

If there are errors in the YAML format or speaker positions, orender will report them clearly.

## Reference Standards

- **ITU-R BS.775**: Multichannel stereophonic sound system with and without accompanying picture
- **ITU-R BS.2051-3**: Advanced sound system for programme production
- **SMPTE ST 2098-2**: Immersive audio bitstream specification
