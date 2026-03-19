# Omniphony

Consolidation monorepo for the Omniphony suite.

![Omniphony capture](Omniphony_capture.png)

Omniphony brings together two main components:

- `omniphony-renderer/`: real-time decoding, spatial rendering, and OSC control engine
- `omniphony-studio/`: supervision app, 3D visualization, live control, and layout management

The goal of this repository is to progressively merge the former separate projects into a single codebase.

## Donations

If Omniphony is useful to you, you can support the project with a donation. It helps maintain and improve the suite.

[![Donate](https://img.shields.io/badge/Donate-PayPal-green.svg)](https://www.paypal.com/cgi-bin/webscr?cmd=_donations&business=YLGYPSHWTQ5UW&lc=FR&item_name=Mgth%C2%A4cy_code=EUR&bn=PP%2dDonationsBF%3abtn_donateCC_LG%2egif%3aNonHosted)

## Components

### `omniphony-renderer`

`omniphony-renderer` is the core engine of the suite.

It loads a format bridge at runtime, decodes the input stream, and can then:

- send decoded audio to a real-time backend
- provide `pipewire` outputs on Linux or `asio` on Windows
- emit metadata and metering over OSC
- render objects to speaker feeds with VBAP
- load speaker layouts and precomputed VBAP tables

The project also includes the engine's supporting stack:

- `renderer`: VBAP engine, layouts, OSC output, runtime config
- `audio_output`: PipeWire and ASIO backends
- `spdif`: IEC61937 / S/PDIF parsing
- `bridge_api`: stable ABI interface for external bridges
- `sys`: platform integration, including Windows service support

### `omniphony-studio`

`omniphony-studio` is the suite's supervision and control interface.

This component does not render audio by itself. It connects to the renderer over OSC to:

- visualize objects and sources in a 3D scene
- monitor runtime state exposed by the engine
- receive positions, levels, and layout information
- register itself automatically with the renderer
- keep the session alive through OSC heartbeats
- control selected live engine parameters

The studio accepts multiple OSC input formats, including:

- legacy cartesian positions
- positions with the identifier embedded in the address
- spherical coordinates as `azimuth/elevation/distance`
- explicit source removal

## Repository Layout

- `omniphony-renderer/`
- `omniphony-studio/`
- `assets/`
- `scripts/`
