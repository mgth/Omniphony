//! Single source of truth for `RenderConfig` option fields.
//!
//! Historically each option's default lived in 3–4 places (clap
//! `default_value_t`, the FFI `from_render_config` `unwrap_or(..)`, and the
//! "skip if == default" thresholds in *both* config writers), which drift
//! independently. Each option declared here owns its default exactly once and
//! exposes a symmetric `get` / `store` pair so every read and write site goes
//! through the same logic:
//!
//! - [`DEFAULT`](self) — the canonical default value, referenced by clap and
//!   by the FFI reader instead of a hard-coded literal.
//! - `get(&RenderConfig) -> Option<T>` — typed read.
//! - `store(&mut RenderConfig, T)` — writes `Some(v)` only when `v` differs
//!   from the default, `None` otherwise. Both the CLI (`effective_to_config`)
//!   and the live (`save_live_config`) writers call this, so their skip-if-
//!   default behaviour can no longer diverge.
//!
//! The descriptor centralises only the *config* side (key + default + get/
//! store). Extracting the value from the runtime source (`RenderArgs` for the
//! CLI, `LiveParams` for the live path) stays a one-liner at the call site,
//! because those types live in other crates.

use crate::config::RenderConfig;

/// Declare a config option as a `{ DEFAULT, get, store }` descriptor module.
///
/// `eq` is the equality used by `store` to decide whether a value equals the
/// default; pass `i32::eq` / `bool::eq` for exact types or a tolerance closure
/// (e.g. `|a: &f32, b: &f32| (a - b).abs() < 1e-4`) for floats so the existing
/// writer thresholds are reproduced exactly.
macro_rules! render_field {
    (
        $(#[$meta:meta])*
        $vis:vis $name:ident : $ty:ty = $default:expr,
        field = $field:ident,
        eq = $eq:expr
    ) => {
        $(#[$meta])*
        $vis mod $name {
            use super::RenderConfig;

            /// Canonical default — the single copy of this field's default.
            pub const DEFAULT: $ty = $default;

            /// Read the configured value, if present.
            pub fn get(cfg: &RenderConfig) -> Option<$ty> {
                cfg.$field.clone()
            }

            /// Store `value`, writing `Some` only when it differs from
            /// [`DEFAULT`] (skip-if-default), `None` otherwise.
            pub fn store(cfg: &mut RenderConfig, value: $ty) {
                let eq: fn(&$ty, &$ty) -> bool = $eq;
                cfg.$field = if eq(&value, &DEFAULT) {
                    None
                } else {
                    Some(value)
                };
            }
        }
    };
}

/// Declare a `String`-typed config option. Same `{ DEFAULT, get, store }`
/// shape as [`render_field!`], but `DEFAULT` is a `&'static str` (String is
/// not const-constructible) and `store` accepts any `AsRef<str>` so call sites
/// can pass a `&str` or an owned `String`.
macro_rules! render_field_str {
    (
        $(#[$meta:meta])*
        $vis:vis $name:ident = $default:expr,
        field = $field:ident
    ) => {
        $(#[$meta])*
        $vis mod $name {
            use super::RenderConfig;

            /// Canonical default — the single copy of this field's default.
            pub const DEFAULT: &str = $default;

            /// Read the configured value, if present.
            pub fn get(cfg: &RenderConfig) -> Option<String> {
                cfg.$field.clone()
            }

            /// Store `value`, writing `Some` only when it differs from
            /// [`DEFAULT`] (skip-if-default), `None` otherwise.
            pub fn store(cfg: &mut RenderConfig, value: impl AsRef<str>) {
                let value = value.as_ref();
                cfg.$field = if value == DEFAULT {
                    None
                } else {
                    Some(value.to_string())
                };
            }
        }
    };
}

render_field! {
    /// Number of distance cells across the polar VBAP precompute table
    /// (`render.vbap_distance_res`). Pilot field for the descriptor scheme.
    pub vbap_distance_res: i32 = 8,
    field = vbap_distance_res,
    eq = i32::eq
}

render_field! {
    /// Polar VBAP azimuth cell count across [-180°, +180°]
    /// (`render.vbap_azimuth_resolution`).
    pub vbap_azimuth_resolution: i32 = 360,
    field = vbap_azimuth_resolution,
    eq = i32::eq
}

render_field! {
    /// Polar VBAP elevation cell count (`render.vbap_elevation_resolution`).
    pub vbap_elevation_resolution: i32 = 180,
    field = vbap_elevation_resolution,
    eq = i32::eq
}

render_field! {
    /// Maximum distance covered by the polar VBAP table
    /// (`render.vbap_distance_max`). Float field: the live writer used a 1e-4
    /// tolerance and the CLI writer `f32::EPSILON`; unified here on 1e-4 (the
    /// more robust threshold — only sub-1e-4 float noise is now treated as the
    /// default on the CLI side).
    pub vbap_distance_max: f32 = 2.0,
    field = vbap_distance_max,
    eq = |a: &f32, b: &f32| (a - b).abs() < 1e-4
}

render_field! {
    /// Minimum VBAP spread floor for point sources (`render.vbap_spread_min`).
    pub vbap_spread_min: f32 = 0.0,
    field = vbap_spread_min,
    eq = |a: &f32, b: &f32| *a == *b
}

render_field! {
    /// Maximum VBAP spread cap for fully-diffuse objects
    /// (`render.vbap_spread_max`).
    pub vbap_spread_max: f32 = 1.0,
    field = vbap_spread_max,
    eq = |a: &f32, b: &f32| *a == *b
}

render_field! {
    /// Interpolate between neighbouring VBAP table cells on lookup
    /// (`render.render_evaluation_position_interpolation`). Default is *on*
    /// (true), so only an explicit `false` is persisted. This also fixes a CLI
    /// writer asymmetry where disabling interpolation was previously dropped
    /// (it wrote `Some(true)` / `None`, never `Some(false)`).
    pub render_evaluation_position_interpolation: bool = true,
    field = render_evaluation_position_interpolation,
    eq = bool::eq
}

// ── Lot 2: distance / spread ──

render_field! {
    /// Distance at which distance-based spread reaches 0.0
    /// (`render.spread_distance_range`). The CLI writer used exact `!= 1.0`,
    /// the live writer a 1e-4 tolerance; unified on 1e-4.
    pub spread_distance_range: f32 = 1.0,
    field = spread_distance_range,
    eq = |a: &f32, b: &f32| (a - b).abs() < 1e-4
}

render_field! {
    /// Curve exponent for distance-based spread (`render.spread_distance_curve`).
    pub spread_distance_curve: f32 = 1.0,
    field = spread_distance_curve,
    eq = |a: &f32, b: &f32| (a - b).abs() < 1e-4
}

render_field! {
    /// Enable distance-based antipodal diffuse blending (`render.distance_diffuse`).
    pub distance_diffuse: bool = false,
    field = distance_diffuse,
    eq = bool::eq
}

render_field! {
    /// ADM distance at which the distance-diffuse blend reaches 100% direct
    /// (`render.distance_diffuse_threshold`).
    pub distance_diffuse_threshold: f32 = 1.0,
    field = distance_diffuse_threshold,
    eq = |a: &f32, b: &f32| (a - b).abs() < 1e-4
}

render_field! {
    /// Curve exponent for the distance-diffuse blend weight
    /// (`render.distance_diffuse_curve`).
    pub distance_diffuse_curve: f32 = 1.0,
    field = distance_diffuse_curve,
    eq = |a: &f32, b: &f32| (a - b).abs() < 1e-4
}

render_field_str! {
    /// Distance attenuation model id (`render.vbap_distance_model`), e.g.
    /// "none", "linear", "quadratic", "inverse-square".
    pub vbap_distance_model = "none",
    field = vbap_distance_model
}

// ── Lot 3: gain / loudness ──

render_field! {
    /// Master output gain in dB (`render.master_gain`). The CLI writer used
    /// exact `!= 0.0`, the live writer a 0.01 dB tolerance (it derives dB from
    /// the linear live gain); unified on 0.01 dB (sub-0.01 dB is inaudible).
    pub master_gain: f32 = 0.0,
    field = master_gain,
    eq = |a: &f32, b: &f32| (a - b).abs() <= 0.01
}

render_field! {
    /// Automatic gain reduction to avoid clipping (`render.auto_gain`).
    /// Persisted by both the CLI writer and the live path (it is a live param,
    /// tunable at runtime via `/omniphony/control/auto_gain`).
    pub auto_gain: bool = false,
    field = auto_gain,
    eq = bool::eq
}

render_field! {
    /// Target ceiling, in dBFS, that auto-gain corrects peaks down to
    /// (`render.auto_gain_ceiling_db`). Clipping is still *detected* at 0 dBFS;
    /// this is only the level peaks are brought back to, providing headroom so
    /// the correction fires less often. Live param, tunable via
    /// `/omniphony/control/auto_gain_ceiling`. Default −1 dBFS.
    pub auto_gain_ceiling_db: f32 = -1.0,
    field = auto_gain_ceiling_db,
    eq = |a: &f32, b: &f32| (a - b).abs() <= 0.01
}

render_field! {
    /// Loudness metadata correction toward -31 dBFS (`render.use_loudness`).
    pub use_loudness: bool = false,
    field = use_loudness,
    eq = bool::eq
}

// ── Lot 4: OSC ──

render_field! {
    /// Pre-enable OSC audio-level metering for the configured default target
    /// (`render.osc_metering`). Consumed by the CLI at startup
    /// (`OscSender::set_default_metering`); the live path does not persist it
    /// (Studio toggles metering per-client at runtime).
    pub osc_metering: bool = false,
    field = osc_metering,
    eq = bool::eq
}

render_field! {
    /// Enable OSC metadata output (`render.osc`).
    pub osc: bool = false,
    field = osc,
    eq = bool::eq
}

render_field_str! {
    /// OSC target host (`render.osc_host`).
    pub osc_host = "127.0.0.1",
    field = osc_host
}

render_field! {
    /// OSC target port (`render.osc_port`).
    pub osc_port: u16 = 9000,
    field = osc_port,
    eq = u16::eq
}

render_field! {
    /// OSC registration listener port (`render.osc_rx_port`).
    pub osc_rx_port: u16 = 9000,
    field = osc_rx_port,
    eq = u16::eq
}

// ── Lot 5a: remaining simple scalar bools/floats ──

render_field! {
    /// Enable VBAP spatial rendering (`render.enable_vbap`).
    pub enable_vbap: bool = false,
    field = enable_vbap,
    eq = bool::eq
}

render_field! {
    /// Deprecated fixed VBAP spread coefficient (`render.vbap_spread`).
    pub vbap_spread: f32 = 0.0,
    field = vbap_spread,
    eq = |a: &f32, b: &f32| *a == *b
}

render_field! {
    /// Continuous mode: keep waiting for new data at stream end
    /// (`render.continuous`).
    pub continuous: bool = false,
    field = continuous,
    eq = bool::eq
}

render_field! {
    /// Bed conformance for spatial content (`render.bed_conform`).
    pub bed_conform: bool = false,
    field = bed_conform,
    eq = bool::eq
}

render_field! {
    /// Render mode for channel-based / non-object content
    /// (`render.channel_render_mode`). Default `Spatial`.
    pub channel_render_mode: crate::live_params::ChannelRenderMode =
        crate::live_params::ChannelRenderMode::Spatial,
    field = channel_render_mode,
    eq = crate::live_params::ChannelRenderMode::eq
}

render_field! {
    /// Where the 4.x/5.x surround pair (`Ls`/`Rs`) is placed: side vs back
    /// (`render.surround_placement`). Default `Side`.
    pub surround_placement: crate::live_params::SurroundPlacement =
        crate::live_params::SurroundPlacement::Side,
    field = surround_placement,
    eq = crate::live_params::SurroundPlacement::eq
}

render_field! {
    /// How output channels map to device ports: by_index vs by_name
    /// (`render.output_channel_mapping`). Default `ByIndex`.
    pub output_channel_mapping: crate::live_params::OutputChannelMapping =
        crate::live_params::OutputChannelMapping::ByIndex,
    field = output_channel_mapping,
    eq = crate::live_params::OutputChannelMapping::eq
}

render_field_str! {
    /// Bed→height object generator id for channel content
    /// (`render.object_generator_id`). Empty / absent = off.
    pub object_generator_id = "",
    field = object_generator_id
}

render_field! {
    /// Phantom extraction algorithm (`render.phantom_extract_mode`).
    pub phantom_extract_mode: crate::live_params::PhantomExtractMode =
        crate::live_params::PhantomExtractMode::Off,
    field = phantom_extract_mode,
    eq = crate::live_params::PhantomExtractMode::eq
}

render_field! {
    /// Derive spread from object distance (`render.spread_from_distance`).
    pub spread_from_distance: bool = false,
    field = spread_from_distance,
    eq = bool::eq
}

render_field! {
    /// Enable adaptive resampling PI controller (`render.enable_adaptive_resampling`).
    pub enable_adaptive_resampling: bool = false,
    field = enable_adaptive_resampling,
    eq = bool::eq
}

// ── Lot 5c: special-cased options (custom descriptors) ──

render_field_str! {
    /// Object-transition ramp mode (`render.ramp_mode`): "off" | "frame" |
    /// "sample". Default "frame". Note: this fixes a CLI/live disagreement —
    /// the old CLI writer treated `Sample` as the default (persisted `Frame`!),
    /// while the live writer (correctly) treats `Frame` as the default.
    pub ramp_mode = "frame",
    field = ramp_mode
}

/// Presentation / substream selector (`render.presentation`). Special-cased:
/// the CLI works in strings ("best" or a number) while the config stores a
/// `u8` (with "best" → absent). Not expressible via the generic macros.
pub mod presentation {
    use super::RenderConfig;

    /// Canonical default — the single copy of this field's default.
    pub const DEFAULT: &str = "best";

    /// Read the configured substream number, if present.
    pub fn get(cfg: &RenderConfig) -> Option<u8> {
        cfg.presentation
    }

    /// Store the CLI string form: `DEFAULT` ("best") or an unparseable value
    /// clears to `None`; a numeric string is stored as the `u8` substream.
    pub fn store(cfg: &mut RenderConfig, value: &str) {
        cfg.presentation = if value == DEFAULT {
            None
        } else {
            value.parse::<u8>().ok()
        };
    }
}

#[cfg(test)]
mod tests {
    use crate::config::RenderConfig;

    #[test]
    fn store_non_default_sets_some() {
        let mut cfg = RenderConfig::default();
        super::vbap_distance_res::store(&mut cfg, 12);
        assert_eq!(cfg.vbap_distance_res, Some(12));
    }

    #[test]
    fn store_default_clears_to_none() {
        let mut cfg = RenderConfig {
            vbap_distance_res: Some(12),
            ..Default::default()
        };
        super::vbap_distance_res::store(&mut cfg, super::vbap_distance_res::DEFAULT);
        assert_eq!(cfg.vbap_distance_res, None);
    }

    #[test]
    fn get_round_trips_through_store() {
        let mut cfg = RenderConfig::default();
        assert_eq!(super::vbap_distance_res::get(&cfg), None);
        super::vbap_distance_res::store(&mut cfg, 5);
        assert_eq!(super::vbap_distance_res::get(&cfg), Some(5));
    }

    /// CLI and live writers now share `store`, so feeding the same value from
    /// either path must yield an identical config field (parity guard).
    #[test]
    fn cli_and_live_writers_agree() {
        let mut from_cli = RenderConfig::default();
        let mut from_live = RenderConfig::default();
        super::vbap_distance_res::store(&mut from_cli, 12); // CLI: args value
        super::vbap_distance_res::store(&mut from_live, 12); // live: LiveParams value
        assert_eq!(from_cli.vbap_distance_res, from_live.vbap_distance_res);
    }

    #[test]
    fn float_field_uses_tolerance() {
        let mut cfg = RenderConfig::default();
        super::vbap_distance_max::store(&mut cfg, 2.0);
        assert_eq!(cfg.vbap_distance_max, None, "exact default cleared");
        super::vbap_distance_max::store(&mut cfg, 2.000_05);
        assert_eq!(
            cfg.vbap_distance_max, None,
            "within 1e-4 treated as default"
        );
        super::vbap_distance_max::store(&mut cfg, 3.0);
        assert_eq!(cfg.vbap_distance_max, Some(3.0));
    }

    #[test]
    fn interpolation_default_is_on_and_false_persists() {
        let mut cfg = RenderConfig::default();
        // Default (on) is not written.
        super::render_evaluation_position_interpolation::store(&mut cfg, true);
        assert_eq!(cfg.render_evaluation_position_interpolation, None);
        // Disabling it IS persisted (the previous CLI writer dropped this).
        super::render_evaluation_position_interpolation::store(&mut cfg, false);
        assert_eq!(cfg.render_evaluation_position_interpolation, Some(false));
    }

    #[test]
    fn string_field_skips_default_and_accepts_str_or_string() {
        let mut cfg = RenderConfig::default();
        super::vbap_distance_model::store(&mut cfg, "none"); // &str, == default
        assert_eq!(cfg.vbap_distance_model, None);
        super::vbap_distance_model::store(&mut cfg, "inverse-square".to_string()); // String
        assert_eq!(cfg.vbap_distance_model.as_deref(), Some("inverse-square"));
        assert_eq!(
            super::vbap_distance_model::get(&cfg).as_deref(),
            Some("inverse-square")
        );
    }
}
