# Speaker Layout Files

This directory contains standard speaker layout configurations for VBAP rendering.
Where a public reference layout is applicable, the corresponding ITU-R family is noted.

## Available Layouts

### 2.0.yaml
Standard stereo layout aligned with ITU-R BS.775.
- Speakers: 2

### 5.0.yaml
Standard 5.0 surround layout aligned with ITU-R BS.775.
- Speakers: 5

### 5.1.yaml
Standard 5.1 surround layout aligned with ITU-R BS.775.
- Speakers: 6 including LFE

### 6.1.yaml
Rear-center 6.1 surround layout.
- Speakers: 7 including LFE

### 7.1.yaml
Standard 7.1 surround layout in the same naming family as common ITU-R speaker sets.
- Speakers: 8 including LFE

### 7.1.2.yaml
7.1 bed plus 2 height channels.
- Speakers: 10 including LFE

### 7.1.4.yaml
7.1 bed plus 4 height channels, aligned with ITU-R BS.2051 style immersive layouts.
- Speakers: 12 including LFE

### 9.1.6.yaml
9.1 bed plus 6 height channels, aligned with ITU-R BS.2051 style immersive layouts.
- Speakers: 16 including LFE

## Usage

Use `--speaker-layout` to load a layout file when decoding with VBAP:

```bash
gsrd decode --enable-vbap --speaker-layout layouts/7.1.4.yaml input.bin
```

Or use a built-in preset in code:

```rust
use gsrd::speaker_layout::SpeakerLayout;

let layout = SpeakerLayout::preset("7.1.4")?;
```

## YAML Format

Speaker layout files use this format:

```yaml
speakers:
  - name: "FL"        # Speaker name (for reference)
    azimuth: -30.0    # Horizontal angle in degrees
    elevation: 0.0    # Vertical angle in degrees
    spatialize: true  # true = VBAP, false = direct bed routing (e.g. LFE)
  # ... more speakers
```

### Coordinate System

- **Azimuth**: -180° to +180°
  - 0° = front center
  - -90° = left
  - +90° = right
  - ±180° = rear center

- **Elevation**: -90° to +90°
  - 0° = horizontal plane (listener ear level)
  - +90° = zenith (directly overhead)
  - -90° = nadir (directly below)

## Creating Custom Layouts

You can create custom layout files by copying and modifying one of these standards.

**Requirements**:
1. At least 3 speakers (VBAP requirement)
2. Azimuth must be in range [-180, 180]
3. Elevation must be in range [-90, 90]
4. All speaker names must be unique

**Tips**:
- LFE should typically share the center position and use `spatialize: false`
- Height speakers are typically placed at 30-45° elevation
- Symmetrical layouts work best for spatial accuracy
- Avoid placing speakers too close together (< 10° separation)

## Example: Custom 5.1.2 Layout

```yaml
# 5.1 + 2 height speakers
speakers:
  # Front layer
  - name: "FL"
    azimuth: -30.0
    elevation: 0.0
    spatialize: true
  - name: "FR"
    azimuth: 30.0
    elevation: 0.0
    spatialize: true
  - name: "C"
    azimuth: 0.0
    elevation: 0.0
    spatialize: true
  - name: "LFE"
    azimuth: 0.0
    elevation: 0.0
    spatialize: false

  # Rear surround
  - name: "BL"
    azimuth: -110.0
    elevation: 0.0
    spatialize: true
  - name: "BR"
    azimuth: 110.0
    elevation: 0.0
    spatialize: true

  # Height layer
  - name: "TFL"
    azimuth: -30.0
    elevation: 35.0
    spatialize: true
  - name: "TFR"
    azimuth: 30.0
    elevation: 35.0
    spatialize: true
```

## Testing Your Layout

You can test if your layout file is valid by trying to load it:

```bash
# This validates the layout during startup
gsrd decode --enable-vbap --speaker-layout my_layout.yaml --help
```

If there are errors in the YAML format or speaker positions, gsrd will report them clearly.

## Reference Standards

- **ITU-R BS.775**: Multichannel stereophonic sound system with and without accompanying picture
- **ITU-R BS.2051-3**: Advanced sound system for programme production
- **SMPTE ST 2098-2**: Immersive audio bitstream specification
