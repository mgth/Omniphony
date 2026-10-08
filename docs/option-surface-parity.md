# Option parity matrix (CLI / Studio live / mpv-omniphony)

This document lists the Omniphony renderer's options and where each one can be
set among the three control surfaces, with the reason for every gap (justified,
or to be fixed). It was the reference for the parity work on
`feat/option-surface-parity`.

## The three surfaces

| Surface | Mechanism | When |
|---|---|---|
| **CLI** (`orender`) | clap flags → YAML config | at start (+ `--save-config`) |
| **Studio live** | OSC `/omniphony/control/*` | live |
| **mpv-omniphony** | OSC `/omniphony/control/*` (through `liborender`) | live |

Studio and mpv share **the same OSC surface**: an option editable in one is
usually editable in the other. The difference comes from the **capabilities**
the renderer announces (`runtime_control/src/snapshot.rs::build_renderer_capabilities_json`):
embedded in mpv, `liborender` has **no audio** of its own (mpv owns the audio
chain), so the `audio` and `input` domains are withdrawn.

Sources of truth: `omniphony-renderer/src/cli/command.rs` (CLI),
`renderer/src/config.rs` + `renderer/src/config_fields.rs` (config),
`renderer/src/options.rs` (live option registry),
`runtime_control/src/osc.rs` + `orender_engine/src/osc/dispatch.rs` +
`runtime_control/src/command.rs` (OSC), `osc-contract/src/lib.rs` (addresses).

Status: ✅ OK · 🟡 **justified** gap (do not fix) · 🔴 gap **to fix**.

---

## Matrix

### Core spatialisation (VBAP / evaluation)

| Option | CLI | OSC/Studio | mpv | Status | Why |
|---|:--:|:--:|:--:|:--:|---|
| `enable_vbap` | ✅ | ✅ | ✅ | ✅ | — |
| polar resolutions (az/el/dist/dist-max) | ✅ | ✅ | ✅ | ✅ | — |
| `evaluation_grid` (`bridge` / `custom`) | ✅ | ✅ | ✅ | ✅ | — |
| Cartesian grid (x/y/z/z-neg) | ✅ | ✅ | ✅ | ✅ | editable in `custom` only: the bridge's hint otherwise. |
| `render_evaluation_mode` (polar/cartesian) | ✅ | ✅ | ✅ | ✅ | editable in `custom` only (a concrete mode). |
| `position_interpolation` | ✅ | ✅ | ✅ | ✅ | — |
| `vbap_allow_negative_z` | ✅ | ✅ | ✅ | ✅ | registry option, editable in `custom` only. |
| `vbap_table` (precomputed table) | ✅ | — | — | 🟡 | a *load-time* path, not editable live (reinitialisation). |
| `speaker_layout` / `current_layout` | ✅ | ✅ | ✅ | ✅ | live editing through `config/layout`. |

### Backend selection

| Option | CLI | OSC/Studio | mpv | Status | Why |
|---|:--:|:--:|:--:|:--:|---|
| `render_backend` (vbap/barycenter/experimental_distance/hybrid) | ✅ | ✅ | ✅ | ✅ | **Fixed (part 1)**: `--render-backend`. |
| `barycenter` (localize) | ✅ | ✅ | ✅ | ✅ | **Fixed (part 1)**: `--barycenter-localize`. |
| `experimental_distance_*` (6 parameters) | ✅ | ✅ | ✅ | ✅ | **Fixed (part 1)**: `--experimental-distance-*`. |
| `hybrid_external/internal/smoothing/metric` | ✅ | ✅ | ✅ | ✅ | **Fixed (part 1)**: `--hybrid-external-backend`, `--hybrid-internal-backend`, `--hybrid-curve-smoothing`, `--hybrid-metric`. |
| `hybrid_curve` (`Vec<[f32;2]>`) | — | ✅ | ✅ | 🟡 | a curve drawn in Studio's canvas editor; not a fit for a CLI flag. Stays Studio-only. |

### Distance / spread

| Option | CLI | OSC/Studio | mpv | Status | Why |
|---|:--:|:--:|:--:|:--:|---|
| `spread_from_distance`, `spread_distance_range/curve` | ✅ | ✅ | ✅ | ✅ | — |
| `vbap_spread_min/max` | ✅ | ✅ | ✅ | ✅ | — |
| `distance_diffuse` (+threshold/curve) | ✅ | ✅ | ✅ | ✅ | — |
| `vbap_distance_model` (none/linear/…) | ✅ | ✅ | ✅ | ✅ | — |
| `distance_model_metric` (spherical/chebyshev) | ✅ | ✅ | ✅ | ✅ | **Fixed (part 2)**: `--distance-model-metric`. |
| `distance_diffuse_metric` (spherical/chebyshev) | ✅ | ✅ | ✅ | ✅ | **Fixed (part 2)**: `--distance-diffuse-metric` (+ `--distance-diffuse-mirror-axes`). |
| `size_to_spread_mode` (max/mean/projection_perpendicular) | ✅ | ✅ | ✅ | ✅ | **Fixed (part 2)**: `--size-to-spread-mode`. |

### Gain / loudness

| Option | CLI | OSC/Studio | mpv | Status | Why |
|---|:--:|:--:|:--:|:--:|---|
| `master_gain` | ✅ | ✅ | ✅ | ✅ | — |
| `use_loudness` | ✅ | ✅ | ✅ | ✅ | — |
| `auto_gain` | ✅ | ✅ | ✅ | ✅ | **Fixed**: `auto_gain` is now a live parameter (`/omniphony/control/auto_gain`), a Studio switch, saved with the config. Renderer domain, so it works in mpv too. |

### Room geometry

| Option | CLI | OSC/Studio | mpv | Status | Why |
|---|:--:|:--:|:--:|:--:|---|
| `room_ratio` + rear/lower/center_blend | ✅ | ✅ | ✅ | ✅ | — |
| `room_*_m` (metres) | (ratios) | ✅ | ✅ | 🟡 | an alternative representation; the CLI says the same through `--room-ratio*`. Nothing missing. |

### Bed conformance

| Option | CLI | OSC/Studio | mpv | Status | Why |
|---|:--:|:--:|:--:|:--:|---|
| `bed_conform` | ✅ | — | — | 🟡 | **Justified** (revised): `bed_conform` is not a renderer-domain parameter but an **output conformance** mode tied to the CLI's audio writer (`src/cli/decode/`: a raw 7.1.2 bed plus object channels out, the writer recreated when the channel count changes). The beds themselves are handled by the engine, in the CLI and in mpv alike. mpv owns its output chain, so the raw conformance mode does not apply there. Not ported. |

### Audio output / latency / resampling (host audio)

| Option | CLI | OSC/Studio | mpv | Status | Why |
|---|:--:|:--:|:--:|:--:|---|
| `output_device`, `output_sample_rate` | ✅ | ✅ | 🟡 | 🟡 | mpv owns the audio chain, so the `audio` domain is withdrawn from the capabilities. **Justified.** |
| `latency_target` | ✅ | ✅ | 🟡 | 🟡 | same. **Justified.** |
| `pw_quantum` | ✅ | — | — | 🟡 | PipeWire, *load-time*. **Justified.** |
| `enable_adaptive_resampling` | ✅ | ✅ | 🟡 | 🟡 | resampling belongs to the audio host; meaningless in mpv. **Justified.** |
| PI tuning (`kp_near`, `ki`, `max_adjust`, far mode, margins…) | ✅ | ✅ | 🟡 | 🟡 | **Fixed for the standalone renderer (part 3)**: `--adaptive-resampling-kp-near`, `--adaptive-resampling-ki`, `--adaptive-resampling-max-adjust`, `--adaptive-resampling-*-far-mode*`, the recovery margins. In mpv: meaningless (no audio host), so justified. |
| `adaptive_resampling_integral_discharge_ratio` | — | (✅) | — | 🟡 | **has no effect**, so deliberately not exposed on the CLI (see the note). |
| `ramp_mode` | ✅ | ✅ | 🟡 | 🟡 | handled by mpv's render pipeline when embedded. **Justified.** |

### Live input

| Option | CLI (`input-live`) | OSC/Studio | mpv | Status | Why |
|---|:--:|:--:|:--:|:--:|---|
| `live_input.*` (backend/node/channels/format/clock/map/lfe) | ✅ | ✅ | 🟡 | 🟡 | mpv supplies the decoded input, so the `input` domain is withdrawn. **Justified.** |

### OSC / monitoring / miscellaneous

| Option | CLI | OSC/Studio | mpv | Status | Why |
|---|:--:|:--:|:--:|:--:|---|
| `osc`, `osc_host`, `osc_port`, `osc_rx_port` | ✅ | n/a | n/a | ✅ | the configuration of the OSC transport itself. |
| `osc_metering` | ✅ (startup) | ✅ (per client) | ✅ | ✅ | the CLI turns it on at start; Studio/mpv toggle it per client, live. |
| `meter_rate` / `diag_rate` (rates) | 🔴 | ✅ | ✅ | 🔴 | no CLI flag. **Out of scope for that round** (to fix later). |
| `drc_mode` / `drc_weight` | 🔴 | ✅ | ✅ | 🔴 | no CLI flag. **Out of scope for that round.** |
| `presentation` (substream) | ✅ | — | — | 🟡 | the bridge's *load-time* selection. **Justified.** |
| `bridge_path` | ✅ | ✅ | (host) | ✅ | editable through `render/bridge_path`. |
| `continuous`, `no_drain_pipe`, `log_object_positions` | ✅ | — | — | 🟡 | *load-time* / debug behaviours. **Justified.** |

---

## Summary of the gaps

Done:

1. **Backend selection on the CLI**: `--render-backend` + the barycenter / hybrid / experimental_distance parameters → **part 1**.
2. **Metrics and size_to_spread on the CLI**: `--distance-model-metric`, `--distance-diffuse-metric`, `--size-to-spread-mode` → **part 2**.
3. **Resampling tuning on the CLI** (standalone): the PI flags beyond enable/update-interval → **part 3**.
4. **`auto_gain`**: a live OSC/Studio control → **part 4**. `bed_conform`: **re-assessed as a justified gap** (an output mode tied to the CLI, beds already handled by the engine), not ported.

Still to fix (🔴): `meter_rate`/`diag_rate` and `drc_mode`/`drc_weight` still
have no CLI flag.

## Note: `integral_discharge_ratio`

`adaptive_resampling_integral_discharge_ratio` **has no effect** in the current
adaptive-resampling PI implementation. It is deliberately **left out** of every
CLI flag and of every tuning or recommendation tool, even where documentation
still mentions it.
