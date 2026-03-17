# Speaker Layout Files

This directory contains example speaker layout configurations for VBAP spatial rendering.

## Available Layouts

### 5.1.yaml
Standard 5.1 surround sound configuration (ITU-R BS.775)
- **Speakers**: 6 (including LFE)
- **Use case**: Basic surround sound systems

### 7.1.4.yaml
Immersive audio 7.1.4 configuration (ITU-R BS.2051-3 Config 4+5+0)
- **Speakers**: 12 (including LFE)
- **Use case**: Common immersive home theater setup
- **Height speakers**: 4 at 45° elevation

### 9.1.6.yaml
Immersive audio 9.1.6 configuration (ITU-R BS.2051-3 Config 6+4+0)
- **Speakers**: 16 (including LFE)
- **Use case**: High-end immersive home theater
- **Height speakers**: 6 at 45° elevation

## Usage

Use the `--speaker-layout` flag to specify a layout file when decoding with VBAP:

```bash
gsrd decode --enable-vbap --speaker-layout layouts/7.1.4.yaml input.thd
```

Or use a preset name directly in code:

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

You can create your own layout files by copying and modifying one of the examples.

**Requirements**:
1. At least 3 speakers (VBAP requirement)
2. Azimuth must be in range [-180, 180]
3. Elevation must be in range [-90, 90]
4. All speaker names must be unique

**Tips**:
- LFE should typically share position with center speaker for VBAP purposes
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
  - name: "FR"
    azimuth: 30.0
    elevation: 0.0
  - name: "C"
    azimuth: 0.0
    elevation: 0.0
  - name: "LFE"
    azimuth: 0.0
    elevation: 0.0

  # Rear surround
  - name: "BL"
    azimuth: -110.0
    elevation: 0.0
  - name: "BR"
    azimuth: 110.0
    elevation: 0.0

  # Height layer
  - name: "TFL"
    azimuth: -30.0
    elevation: 35.0
  - name: "TFR"
    azimuth: 30.0
    elevation: 35.0
```

## Testing Your Layout

You can test if your layout file is valid by trying to load it:

```bash
# This will validate the layout during build
gsrd decode --enable-vbap --speaker-layout my_layout.yaml --help
```

If there are errors in the YAML format or speaker positions, gsrd will report them clearly.

## Reference Standards

- **ITU-R BS.775**: Multichannel stereophonic sound system with and without accompanying picture
- **ITU-R BS.2051-3**: Advanced sound system for programme production
- **SMPTE ST 2098-2**: Immersive Audio Bitstream Specification (ADM)
