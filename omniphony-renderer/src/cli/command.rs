use std::path::PathBuf;

use clap::{
    ArgMatches, Args, CommandFactory, FromArgMatches, Parser as ClapParser, Subcommand, ValueEnum,
    parser::ValueSource,
};

use renderer::live_params::{ChannelRenderMode, RampMode, SurroundPlacement};

pub const VERSION_INFO: &str = concat!(
    env!("VERGEN_GIT_DESCRIBE"),
    " Built: ",
    env!("BUILD_TIMESTAMP")
);

#[derive(Debug, Clone, ClapParser)]
#[command(
    name       = env!("CARGO_PKG_NAME"),
    version    = VERSION_INFO,
    author     = env!("CARGO_PKG_AUTHORS"),
    about      = env!("CARGO_PKG_DESCRIPTION"),
    long_about = None,
    after_help = "If no command is given, orender runs the default render flow.",
)]
pub struct Cli {
    /// Path to config file (default: ~/.config/omniphony/config.yaml on Linux,
    /// %ProgramData%\omniphony\config.yaml on Windows)
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Set the log level
    #[arg(long, global = true, value_enum, default_value_t = LogLevel::Info)]
    pub loglevel: LogLevel,

    /// Log output format.
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Plain)]
    pub log_format: LogFormat,

    /// Write effective configuration to the config file and exit (no runtime start).
    /// Saves only non-default values. Use --config to specify the target path.
    #[arg(long, global = true)]
    pub save_config: bool,

    /// Choose an operation to perform.
    #[command(subcommand)]
    pub command: Commands,
}

pub struct ParsedCli {
    pub cli: Cli,
    matches: ArgMatches,
}

impl ParsedCli {
    pub fn parse_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let matches = Cli::command().try_get_matches_from(args)?;
        let cli = Cli::from_arg_matches(&matches)?;
        Ok(Self { cli, matches })
    }

    pub fn is_explicit(&self, id: &str) -> bool {
        self.matches
            .value_source(id)
            .is_some_and(is_explicit_value_source)
    }

    pub fn render_sources(&self) -> RenderArgSources<'_> {
        RenderArgSources {
            matches: self.matches.subcommand_matches("render"),
        }
    }
}

pub struct RenderArgSources<'a> {
    matches: Option<&'a ArgMatches>,
}

impl RenderArgSources<'_> {
    pub fn is_explicit(&self, id: &str) -> bool {
        self.matches
            .and_then(|matches| matches.value_source(id))
            .is_some_and(is_explicit_value_source)
    }
}

fn is_explicit_value_source(source: ValueSource) -> bool {
    matches!(source, ValueSource::CommandLine | ValueSource::EnvVariable)
}

#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// Render the specified input stream to a realtime output backend.
    Render(RenderArgs),

    /// Run realtime live-input rendering without a bridge-fed decode path.
    InputLive(InputLiveArgs),

    /// Generate VBAP gain table from speaker layout configuration
    GenerateVbap(GenerateVbapArgs),

    /// List available ASIO output devices (Windows only)
    #[cfg(target_os = "windows")]
    ListAsioDevices,

    /// List available CoreAudio output devices (macOS only)
    #[cfg(target_os = "macos")]
    ListCoreaudioDevices,
}

#[derive(Debug, Clone, Args)]
pub struct RenderArgs {
    /// Input audio bitstream (use "-" for stdin)
    #[arg(value_name = "INPUT")]
    pub input: Option<PathBuf>,

    /// Realtime audio output backend.
    /// Defaults to PipeWire on Linux and ASIO on Windows when available.
    /// Pass `file` to write rendered audio to stdout / a file / a FIFO instead.
    #[arg(long = "output-backend", value_enum)]
    pub output_backend: Option<OutputBackend>,

    /// Destination for `--output-backend file`: `-` (stdout, default), or a
    /// path to a regular file or a named pipe (FIFO).
    #[arg(long = "output-file", value_name = "PATH", default_value = "-")]
    pub output_file: String,

    /// Container/format for `--output-backend file`.
    #[arg(long = "output-file-format", value_enum, default_value_t = OutputFileFormatArg::RawF32)]
    pub output_file_format: OutputFileFormatArg,

    /// Presentation or substream selector passed to the bridge plugin.
    /// "best" selects the richest available presentation (default).
    /// Pass a number to request a specific substream (bridge-defined).
    #[arg(long, value_name = "VALUE", default_value = renderer::config_fields::presentation::DEFAULT)]
    pub presentation: String,

    /// Path to the format bridge plugin library.
    #[arg(long, value_name = "FILE")]
    pub bridge_path: Option<PathBuf>,

    /// Enable bed conformance for spatial audio content
    #[arg(long, conflicts_with = "no_bed_conform")]
    pub bed_conform: bool,

    /// Override config file 'bed_conform' setting to false.
    #[arg(long, conflicts_with = "bed_conform")]
    pub no_bed_conform: bool,

    /// Enable OSC output for metadata (requires --osc-host and --osc-port)
    #[arg(long, conflicts_with = "no_osc")]
    pub osc: bool,

    /// Override config file 'osc' setting to false.
    #[arg(long, conflicts_with = "osc")]
    pub no_osc: bool,

    /// Enable OSC audio level metering (peak + RMS per object and speaker, 20 Hz bundles).
    /// Requires --osc and --enable-vbap.
    #[arg(long, conflicts_with = "no_osc_metering")]
    pub osc_metering: bool,

    /// Override config file 'osc_metering' setting to false.
    #[arg(long, conflicts_with = "osc_metering")]
    pub no_osc_metering: bool,

    /// OSC target host
    #[arg(long, value_name = "HOST", default_value = renderer::config_fields::osc_host::DEFAULT)]
    pub osc_host: String,

    /// OSC target port
    #[arg(long, value_name = "PORT", default_value_t = renderer::runtime_env::default_osc_port())]
    pub osc_port: u16,

    /// OSC registration listener port. Clients register by sending /omniphony/register
    /// to this port and receive the speaker config + all subsequent broadcasts.
    #[arg(long, value_name = "PORT", default_value_t = renderer::runtime_env::default_osc_rx_port())]
    pub osc_rx_port: u16,

    /// Run as a yieldable (standby) instance: shut down cleanly when another
    /// local instance requests the OSC port via /omniphony/control/yield_port.
    /// Used by Studio-launched standby renderers so an mpv-embedded renderer
    /// can take over (and hand back) seamlessly.
    #[arg(long)]
    pub osc_yield: bool,

    /// Output device or target name.
    /// PipeWire: node target name (e.g. "omniphony_router")
    /// ASIO: device name as listed by `orender list-asio-devices`
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    #[arg(
        long,
        value_name = "NAME",
        visible_alias = "sink",
        alias = "asio-device-name"
    )]
    pub output_device: Option<String>,

    /// Target buffer latency in milliseconds.
    /// Playback starts once the ring buffer has reached this level, and the PI
    /// controller (--enable-adaptive-resampling) maintains it at this level.
    /// Default: 500 (Linux/PipeWire), 220 (Windows/ASIO).
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    #[arg(long, value_name = "MS")]
    pub latency_target_ms: Option<u32>,

    /// [LINUX ONLY] PipeWire processing quantum in frames (~21ms at 48kHz for 1024 frames).
    /// Smaller values reduce hardware latency but increase CPU load. Default: 1024.
    #[cfg(target_os = "linux")]
    #[arg(long, value_name = "FRAMES")]
    pub pw_quantum: Option<u32>,

    /// Continuous mode: don't exit when stream ends, wait for new data
    #[arg(long, conflicts_with = "no_continuous")]
    pub continuous: bool,

    /// Override config file 'continuous' setting to false.
    #[arg(long, conflicts_with = "continuous")]
    pub no_continuous: bool,

    /// Enable VBAP spatial rendering for spatial audio objects
    #[arg(long, conflicts_with = "disable_vbap")]
    pub enable_vbap: bool,

    /// Override config file 'enable_vbap' setting to false.
    #[arg(long, conflicts_with = "enable_vbap")]
    pub disable_vbap: bool,

    /// Speaker layout configuration file (YAML)
    #[arg(long, value_name = "LAYOUT")]
    pub speaker_layout: Option<PathBuf>,

    /// Load pre-computed VBAP gain table from binary file (faster initialization)
    /// If specified, --vbap-azimuth-resolution, --vbap-elevation-resolution,
    /// and --vbap-distance-res are ignored
    #[arg(long, value_name = "FILE")]
    pub vbap_table: Option<PathBuf>,

    /// Number of azimuth cells across full range [-180°, +180°]
    #[arg(
        long = "evaluation-polar-azimuth-resolution",
        value_name = "DEG",
        default_value_t = renderer::config_fields::vbap_azimuth_resolution::DEFAULT
    )]
    pub evaluation_polar_azimuth_resolution: i32,

    /// Number of elevation cells across full range:
    /// [-90°, +90°] when negative Z is enabled, otherwise [0°, +90°]
    #[arg(
        long = "evaluation-polar-elevation-resolution",
        value_name = "DEG",
        default_value_t = renderer::config_fields::vbap_elevation_resolution::DEFAULT
    )]
    pub evaluation_polar_elevation_resolution: i32,

    /// VBAP spreading coefficient (0.0 = point source, 1.0 = maximum spread)
    /// Deprecated: Use --vbap-distance-res instead for dynamic per-object spread
    #[arg(long, value_name = "SPREAD", default_value_t = renderer::config_fields::vbap_spread::DEFAULT)]
    pub vbap_spread: f32,

    /// Number of distance cells across full range [0, vbap-distance-max]
    /// Higher values = denser precompute but higher memory
    #[arg(
        long = "evaluation-polar-distance-res",
        value_name = "RESOLUTION",
        default_value_t = renderer::config_fields::vbap_distance_res::DEFAULT
    )]
    pub evaluation_polar_distance_res: i32,

    /// Maximum distance covered by polar VBAP precomputed table
    #[arg(
        long = "evaluation-polar-distance-max",
        value_name = "DISTANCE",
        default_value_t = renderer::config_fields::vbap_distance_max::DEFAULT
    )]
    pub evaluation_polar_distance_max: f32,

    /// Interpolate between neighbouring VBAP table positions during lookup.
    /// Disable this to use nearest-cell lookup for lower CPU cost.
    #[arg(
        long = "render-evaluation-position-interpolation",
        conflicts_with = "no_render_evaluation_position_interpolation"
    )]
    pub render_evaluation_position_interpolation: bool,

    /// Disable interpolation between neighbouring VBAP table positions.
    #[arg(
        long = "no-render-evaluation-position-interpolation",
        conflicts_with = "render_evaluation_position_interpolation"
    )]
    pub no_render_evaluation_position_interpolation: bool,

    /// VBAP pre-computed table mode.
    /// - `polar`: pre-compute gains over azimuth/elevation (current behavior)
    /// - `cartesian`: pre-compute gains over x/y/z ADM grid
    #[arg(long = "render-evaluation-mode", value_enum, default_value_t = EvaluationModeArg::Polar)]
    pub render_evaluation_mode: EvaluationModeArg,

    /// Cartesian VBAP cell count on X axis (used only when --vbap-table-mode cartesian)
    #[arg(long = "evaluation-cartesian-x-size", value_name = "SIZE")]
    pub evaluation_cartesian_x_size: Option<usize>,

    /// Cartesian VBAP cell count on Y axis (used only when --vbap-table-mode cartesian)
    #[arg(long = "evaluation-cartesian-y-size", value_name = "SIZE")]
    pub evaluation_cartesian_y_size: Option<usize>,

    /// Cartesian VBAP cell count on Z axis (used only when --vbap-table-mode cartesian)
    #[arg(long = "evaluation-cartesian-z-size", value_name = "SIZE")]
    pub evaluation_cartesian_z_size: Option<usize>,

    /// Cartesian VBAP cell count on negative Z axis (used only when --vbap-table-mode cartesian)
    #[arg(long = "evaluation-cartesian-z-neg-size", value_name = "SIZE")]
    pub evaluation_cartesian_z_neg_size: Option<usize>,

    /// Allow negative Z values for VBAP tables (floor below listener).
    #[arg(long, conflicts_with = "no_vbap_allow_negative_z")]
    pub vbap_allow_negative_z: bool,

    /// Disable negative Z values for VBAP tables.
    #[arg(long, conflicts_with = "vbap_allow_negative_z")]
    pub no_vbap_allow_negative_z: bool,

    /// Distance attenuation model (none, linear, quadratic, inverse-square)
    #[arg(long, value_name = "MODEL", default_value = renderer::config_fields::vbap_distance_model::DEFAULT)]
    pub vbap_distance_model: String,

    /// Calculate spread from distance (1.0 at distance=0, 0.0 at distance>=1.0)
    /// When enabled, overrides object spread metadata for spread calculation
    #[arg(long, conflicts_with = "no_spread_from_distance")]
    pub spread_from_distance: bool,

    /// Override config file 'spread_from_distance' setting to false.
    #[arg(long, conflicts_with = "spread_from_distance")]
    pub no_spread_from_distance: bool,

    /// Distance at which spread reaches 0.0 (only used with --spread-from-distance)
    /// Lower values = objects become localized sooner, higher values = stay diffuse longer
    #[arg(long, value_name = "DISTANCE", default_value_t = renderer::config_fields::spread_distance_range::DEFAULT)]
    pub spread_distance_range: f32,

    /// Curve exponent for distance-based spread (only used with --spread-from-distance)
    /// 1.0 = linear, 2.0 = quadratic (slower near, faster far), 0.5 = sqrt (faster near, slower far)
    #[arg(long, value_name = "EXPONENT", default_value_t = renderer::config_fields::spread_distance_curve::DEFAULT)]
    pub spread_distance_curve: f32,

    /// Minimum VBAP spread applied when the object spread is 0.0 (point source)
    /// Allows setting a spread floor so objects are never fully localized
    #[arg(long, value_name = "SPREAD", default_value_t = renderer::config_fields::vbap_spread_min::DEFAULT)]
    pub vbap_spread_min: f32,

    /// Maximum VBAP spread applied when the object spread is 1.0 (fully diffuse)
    /// Allows capping spread so objects never fully decorrelate
    #[arg(long, value_name = "SPREAD", default_value_t = renderer::config_fields::vbap_spread_max::DEFAULT)]
    pub vbap_spread_max: f32,

    /// Enable detailed logging of object positions during VBAP spatialization
    /// Shows ADM coordinates when objects move or ramp between positions
    #[arg(long)]
    pub log_object_positions: bool,

    /// Room ratio for spatial rendering: width,length,height (default: 1.0,2.0,1.0)
    /// Scales ADM coordinates before VBAP processing to match room proportions.
    /// Example: --room-ratio 1.0,2.0,1.0 for a room twice as long as wide
    #[arg(long, value_name = "W,L,H", default_value = "1.0,2.0,1.0")]
    pub room_ratio: String,

    /// Rear depth ratio used by the non-linear depth warp (`depth < 0`).
    /// If omitted, defaults to room-ratio length (same front/rear behaviour).
    #[arg(long, value_name = "RATIO")]
    pub room_ratio_rear: Option<f32>,

    /// Lower height ratio used for negative Z coordinates.
    /// If omitted, defaults to 0.5.
    #[arg(long, value_name = "RATIO")]
    pub room_ratio_lower: Option<f32>,

    /// Center blend for non-linear depth warp: 0.0 = rear-biased, 1.0 = front-biased.
    /// 0.5 keeps the center ratio at the midpoint between front and rear ratios.
    #[arg(long, value_name = "BLEND")]
    pub room_ratio_center_blend: Option<f32>,

    /// Master gain in dB applied to VBAP output (default: 0.0 = unity gain)
    /// Use negative values to reduce output level (e.g., -6.0 for -6dB headroom)
    #[arg(
        long,
        value_name = "DB",
        default_value_t = renderer::config_fields::master_gain::DEFAULT,
        allow_hyphen_values = true
    )]
    pub master_gain: f32,

    /// Enable automatic gain reduction to prevent clipping
    /// When enabled, gain is automatically reduced if output exceeds 0dBFS
    #[arg(long, conflicts_with = "no_auto_gain")]
    pub auto_gain: bool,

    /// Override config file 'auto_gain' setting to false.
    #[arg(long, conflicts_with = "auto_gain")]
    pub no_auto_gain: bool,

    /// Target ceiling in dBFS that auto-gain corrects peaks down to (default: -1.0).
    /// Clipping is still detected at 0 dBFS; this only sets how much headroom the
    /// correction leaves, so corrections fire less often.
    #[arg(long, value_name = "DB", allow_hyphen_values = true)]
    pub auto_gain_ceiling: Option<f32>,

    /// Enable loudness metadata correction to a -31 dBFS target
    /// Adjusts gain based on the stream's dialogue_level metadata
    /// (e.g., dialogue_level=-27 dBFS -> applies -4 dB correction toward -31 dBFS)
    #[arg(long, conflicts_with = "no_loudness")]
    pub use_loudness: bool,

    /// Override config file 'use_loudness' (loudness metadata correction) to false.
    #[arg(long, conflicts_with = "use_loudness")]
    pub no_loudness: bool,

    /// Enable distance-based antipodal diffuse blending.
    /// Objects near the origin are blended with their antipodal mirror (same
    /// elevation, opposite horizontal direction), fading to fully directional
    /// as ADM distance approaches --distance-diffuse-threshold.
    #[arg(long)]
    pub distance_diffuse: bool,

    /// ADM distance at which distance-diffuse blend reaches 100% direct.
    /// (pre-room_ratio, 1.0 = surface of the ADM unit sphere)
    #[arg(long, value_name = "DISTANCE", default_value_t = renderer::config_fields::distance_diffuse_threshold::DEFAULT)]
    pub distance_diffuse_threshold: f32,

    /// Curve exponent for distance-diffuse blend weight.
    /// 1.0 = linear, 2.0 = slow-near (stays diffuse longer), 0.5 = fast-near.
    #[arg(long, value_name = "EXPONENT", default_value_t = renderer::config_fields::distance_diffuse_curve::DEFAULT)]
    pub distance_diffuse_curve: f32,

    /// Ramp processing mode for object transitions.
    /// - `sample`: smooth on every rendered sample (current behaviour)
    /// - `frame`: update once per decoded audio frame
    /// - `off`: jump immediately to the new value
    #[arg(long, value_enum, default_value_t = RampModeArg::Frame)]
    pub ramp_mode: RampModeArg,

    /// How to render channel-based (non-object) content: `host` (let the sink
    /// handle the channels, no spatialization), `direct` (route each channel to
    /// its matching speaker), or `virtual` (virtualize each channel as an object
    /// at its speaker angle — the default).
    #[arg(long, value_enum, default_value_t = ChannelRenderModeArg::Spatial)]
    pub channel_render_mode: ChannelRenderModeArg,

    /// Where to place the surround pair (`Ls`/`Rs`) of a 4.x/5.x source rendered
    /// through the virtual bed: `side` (the default) or `back`. Sources that
    /// already carry back channels (7.x) ignore this.
    #[arg(long, value_enum, default_value_t = SurroundPlacementArg::Side)]
    pub surround_placement: SurroundPlacementArg,

    /// Disable automatic draining of buffered data from named pipes at startup
    /// (By default, orender drains FIFOs to minimize latency for real-time streams)
    #[arg(long)]
    pub no_drain_pipe: bool,

    /// Output sample rate in Hz (48000, 96000, 192000, etc.)
    /// If not specified, uses the stream's native sample rate (48000 Hz).
    /// Higher rates require upsampling and may improve audio quality.
    #[arg(long, value_name = "HZ")]
    pub output_sample_rate: Option<u32>,

    /// Enable adaptive resampling to maintain buffer stability
    /// Uses a PI controller to dynamically adjust the playback rate
    /// based on buffer fill level. Disabled by default.
    /// Works with both ASIO (Windows) and PipeWire (Linux) outputs.
    #[arg(long, conflicts_with = "disable_adaptive_resampling")]
    pub enable_adaptive_resampling: bool,

    /// Override config file 'enable_adaptive_resampling' setting to false.
    #[arg(long, conflicts_with = "enable_adaptive_resampling")]
    pub disable_adaptive_resampling: bool,

    /// Recompute adaptive resampling every N audio callbacks.
    /// Lower values react faster but can make the control loop more nervous.
    #[arg(long, value_name = "CALLBACKS")]
    pub adaptive_resampling_update_interval_callbacks: Option<u32>,

    // ── Render backend selection (Partie 1) ──
    // Override-only: when omitted the value is kept from the config file /
    // engine default. Consumed via `apply_render_cfg_overrides`.
    /// Spatial render backend. Defaults to the config value, else VBAP.
    #[arg(long = "render-backend", value_enum)]
    pub render_backend: Option<RenderBackendArg>,

    /// Barycenter backend: localization sharpness (0.0 = diffuse).
    /// Only meaningful with `--render-backend barycenter`.
    #[arg(long = "barycenter-localize", value_name = "AMOUNT")]
    pub barycenter_localize: Option<f32>,

    /// Hybrid backend: inner backend mixed in at ratio = 1 (cube surface).
    #[arg(long = "hybrid-external-backend", value_enum)]
    pub hybrid_external_backend: Option<HybridInnerBackendArg>,

    /// Hybrid backend: inner backend mixed in at ratio = 0 (centre).
    #[arg(long = "hybrid-internal-backend", value_enum)]
    pub hybrid_internal_backend: Option<HybridInnerBackendArg>,

    /// Hybrid backend: blend curve smoothing in [0, 1].
    #[arg(long = "hybrid-curve-smoothing", value_name = "AMOUNT")]
    pub hybrid_curve_smoothing: Option<f32>,

    /// Hybrid backend: blend distance metric.
    #[arg(long = "hybrid-metric", value_enum)]
    pub hybrid_metric: Option<DistanceMetricArg>,

    // ── Experimental distance backend params (Partie 1) ──
    /// Experimental distance backend: minimum distance floor.
    #[arg(long = "experimental-distance-distance-floor", value_name = "DISTANCE")]
    pub experimental_distance_distance_floor: Option<f32>,

    /// Experimental distance backend: minimum number of active speakers.
    #[arg(
        long = "experimental-distance-min-active-speakers",
        value_name = "COUNT"
    )]
    pub experimental_distance_min_active_speakers: Option<usize>,

    /// Experimental distance backend: maximum number of active speakers.
    #[arg(
        long = "experimental-distance-max-active-speakers",
        value_name = "COUNT"
    )]
    pub experimental_distance_max_active_speakers: Option<usize>,

    /// Experimental distance backend: position-error floor.
    #[arg(
        long = "experimental-distance-position-error-floor",
        value_name = "ERROR"
    )]
    pub experimental_distance_position_error_floor: Option<f32>,

    /// Experimental distance backend: nearest-speaker position-error scale.
    #[arg(
        long = "experimental-distance-position-error-nearest-scale",
        value_name = "SCALE"
    )]
    pub experimental_distance_position_error_nearest_scale: Option<f32>,

    /// Experimental distance backend: span position-error scale.
    #[arg(
        long = "experimental-distance-position-error-span-scale",
        value_name = "SCALE"
    )]
    pub experimental_distance_position_error_span_scale: Option<f32>,

    // ── Distance metrics & size-to-spread (Partie 2) ──
    /// Distance metric for the distance-model stage.
    #[arg(long = "distance-model-metric", value_enum)]
    pub distance_model_metric: Option<DistanceMetricArg>,

    /// Distance metric for the distance-diffuse stage.
    #[arg(long = "distance-diffuse-metric", value_enum)]
    pub distance_diffuse_metric: Option<DistanceMetricArg>,

    /// ADM axes negated to build the diffuse mirror image: any combination of
    /// x, y and z (`xy` — the default half-turn about the vertical axis — `y`
    /// for a front/back reflection, `xyz` for a point inversion), or `none`.
    #[arg(long = "distance-diffuse-mirror-axes", value_name = "AXES")]
    pub distance_diffuse_mirror_axes: Option<String>,

    /// Policy reducing an object's (w, d, h) size to a scalar spread.
    #[arg(long = "size-to-spread-mode", value_enum)]
    pub size_to_spread_mode: Option<SizeToSpreadModeArg>,

    // ── Adaptive resampling PI tuning (Partie 3) ──
    // Override-only; standalone (host-audio) only — mpv owns the audio chain so
    // these have no effect there. `integral_discharge_ratio` is intentionally
    // NOT exposed (non-operative on the current controller).
    /// PI proportional gain in the near band.
    #[arg(long = "adaptive-resampling-kp-near", value_name = "GAIN")]
    pub adaptive_resampling_kp_near: Option<f32>,

    /// PI integral gain.
    #[arg(long = "adaptive-resampling-ki", value_name = "GAIN")]
    pub adaptive_resampling_ki: Option<f32>,

    /// Maximum playback-rate adjustment (fraction, e.g. 0.10).
    #[arg(long = "adaptive-resampling-max-adjust", value_name = "FRACTION")]
    pub adaptive_resampling_max_adjust: Option<f32>,

    /// Enable far-mode (aggressive recovery far from target).
    #[arg(long = "adaptive-resampling-enable-far-mode", value_name = "BOOL")]
    pub adaptive_resampling_enable_far_mode: Option<bool>,

    /// Force silence while in far-mode.
    #[arg(
        long = "adaptive-resampling-force-silence-in-far-mode",
        value_name = "BOOL"
    )]
    pub adaptive_resampling_force_silence_in_far_mode: Option<bool>,

    /// Hard-recover on the high side while in far-mode.
    #[arg(
        long = "adaptive-resampling-hard-recover-high-in-far-mode",
        value_name = "BOOL"
    )]
    pub adaptive_resampling_hard_recover_high_in_far_mode: Option<bool>,

    /// Hard-recover on the low side while in far-mode.
    #[arg(
        long = "adaptive-resampling-hard-recover-low-in-far-mode",
        value_name = "BOOL"
    )]
    pub adaptive_resampling_hard_recover_low_in_far_mode: Option<bool>,

    /// Fade-in duration (ms) when returning from far-mode.
    #[arg(
        long = "adaptive-resampling-far-mode-return-fade-in-ms",
        value_name = "MS"
    )]
    pub adaptive_resampling_far_mode_return_fade_in_ms: Option<u32>,

    /// High-recover entry margin (ms).
    #[arg(
        long = "adaptive-resampling-high-recover-entry-margin-ms",
        value_name = "MS"
    )]
    pub adaptive_resampling_high_recover_entry_margin_ms: Option<u32>,

    /// Low-recover settle-stable duration (ms).
    #[arg(
        long = "adaptive-resampling-low-recover-settle-stable-ms",
        value_name = "MS"
    )]
    pub adaptive_resampling_low_recover_settle_stable_ms: Option<f32>,

    /// Low-recover entry margin (ms).
    #[arg(
        long = "adaptive-resampling-low-recover-entry-margin-ms",
        value_name = "MS"
    )]
    pub adaptive_resampling_low_recover_entry_margin_ms: Option<f32>,

    /// Low-recover exit margin (ms).
    #[arg(
        long = "adaptive-resampling-low-recover-exit-margin-ms",
        value_name = "MS"
    )]
    pub adaptive_resampling_low_recover_exit_margin_ms: Option<f32>,

    /// Low-recover settle margin (ms).
    #[arg(
        long = "adaptive-resampling-low-recover-settle-margin-ms",
        value_name = "MS"
    )]
    pub adaptive_resampling_low_recover_settle_margin_ms: Option<f32>,

    /// Low-recover refill delta-alpha.
    #[arg(
        long = "adaptive-resampling-low-recover-refill-delta-alpha",
        value_name = "ALPHA"
    )]
    pub adaptive_resampling_low_recover_refill_delta_alpha: Option<f32>,

    /// Control-output smoothing cutoff (Hz).
    #[arg(
        long = "adaptive-resampling-control-smoothing-cutoff-hz",
        value_name = "HZ"
    )]
    pub adaptive_resampling_control_smoothing_cutoff_hz: Option<f32>,

    /// Control-output smoothing filter order.
    #[arg(
        long = "adaptive-resampling-control-smoothing-order",
        value_name = "ORDER"
    )]
    pub adaptive_resampling_control_smoothing_order: Option<u32>,

    /// Use the pre-bridge clock as the timing reference.
    #[arg(long = "adaptive-resampling-use-pre-bridge-clock", value_name = "BOOL")]
    pub adaptive_resampling_use_pre_bridge_clock: Option<bool>,

    /// Use output pacing for the control loop.
    #[arg(long = "adaptive-resampling-use-output-pacing", value_name = "BOOL")]
    pub adaptive_resampling_use_output_pacing: Option<bool>,

    /// Disable backpressure on the decode queue.
    #[arg(long = "adaptive-resampling-disable-backpressure", value_name = "BOOL")]
    pub adaptive_resampling_disable_backpressure: Option<bool>,
}

#[derive(Debug, Clone, Args)]
pub struct InputLiveArgs {
    /// Realtime input backend.
    #[arg(long = "input-backend", value_enum)]
    pub input_backend: Option<InputBackend>,

    /// Input endpoint node name exposed to the host audio graph.
    #[arg(long = "input-node", value_name = "NAME")]
    pub input_node: Option<String>,

    /// Human-readable input endpoint description.
    #[arg(long = "input-description", value_name = "LABEL")]
    pub input_description: Option<String>,

    /// Layout used to derive fixed source positions for incoming channels.
    #[arg(long = "input-layout", value_name = "LAYOUT")]
    pub input_layout: Option<PathBuf>,

    /// Number of incoming channels expected from the live input backend.
    #[arg(long = "input-channels", value_name = "COUNT")]
    pub input_channels: Option<u16>,

    /// Requested input sample rate for the live backend.
    #[arg(long = "input-sample-rate", value_name = "HZ")]
    pub input_sample_rate: Option<u32>,

    /// Requested input sample format.
    #[arg(long = "input-format", value_enum)]
    pub input_format: Option<InputSampleFormatArg>,

    /// Channel-to-fixed-object mapping preset.
    #[arg(long = "input-map", value_enum)]
    pub input_map: Option<InputMapModeArg>,

    /// How to treat the LFE input channel when present.
    #[arg(long = "input-lfe-mode", value_enum)]
    pub input_lfe_mode: Option<InputLfeModeArg>,

    /// Realtime audio output backend.
    #[arg(long = "output-backend", value_enum)]
    pub output_backend: Option<OutputBackend>,

    /// Output device or target name.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    #[arg(
        long,
        value_name = "NAME",
        visible_alias = "sink",
        alias = "asio-device-name"
    )]
    pub output_device: Option<String>,

    /// Target buffer latency in milliseconds.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    #[arg(long, value_name = "MS")]
    pub latency_target_ms: Option<u32>,

    /// [LINUX ONLY] PipeWire processing quantum in frames.
    #[cfg(target_os = "linux")]
    #[arg(long, value_name = "FRAMES")]
    pub pw_quantum: Option<u32>,

    /// Enable VBAP spatial rendering for the fixed input objects.
    #[arg(long, conflicts_with = "disable_vbap")]
    pub enable_vbap: bool,

    /// Override config file 'enable_vbap' setting to false.
    #[arg(long, conflicts_with = "enable_vbap")]
    pub disable_vbap: bool,

    /// Speaker layout configuration file (YAML)
    #[arg(long, value_name = "LAYOUT")]
    pub speaker_layout: Option<PathBuf>,

    /// Enable OSC output for metadata (requires --osc-host and --osc-port)
    #[arg(long, conflicts_with = "no_osc")]
    pub osc: bool,

    /// Override config file 'osc' setting to false.
    #[arg(long, conflicts_with = "osc")]
    pub no_osc: bool,

    /// Enable OSC audio level metering.
    #[arg(long, conflicts_with = "no_osc_metering")]
    pub osc_metering: bool,

    /// Override config file 'osc_metering' setting to false.
    #[arg(long, conflicts_with = "osc_metering")]
    pub no_osc_metering: bool,

    /// OSC target host
    #[arg(long, value_name = "HOST", default_value = renderer::config_fields::osc_host::DEFAULT)]
    pub osc_host: String,

    /// OSC target port
    #[arg(long, value_name = "PORT", default_value_t = renderer::runtime_env::default_osc_port())]
    pub osc_port: u16,

    /// OSC registration listener port.
    #[arg(long, value_name = "PORT", default_value_t = renderer::runtime_env::default_osc_rx_port())]
    pub osc_rx_port: u16,
}

#[derive(Debug, Clone, Args)]
pub struct GenerateVbapArgs {
    /// Speaker layout configuration file (YAML)
    #[arg(long, value_name = "LAYOUT")]
    pub speaker_layout: PathBuf,

    /// Output path for binary VBAP gain table
    #[arg(long, short = 'o', value_name = "FILE")]
    pub output: PathBuf,

    /// VBAP azimuth resolution in degrees (1-10)
    #[arg(long, value_name = "DEG", default_value_t = 1)]
    pub az_res: i32,

    /// VBAP elevation resolution in degrees (1-10)
    #[arg(long, value_name = "DEG", default_value_t = 1)]
    pub el_res: i32,

    /// VBAP spread resolution (step between pre-computed spread tables)
    /// Use 0.0 for single table with spread=0, or e.g. 0.25 for dynamic spread support
    #[arg(long, value_name = "RESOLUTION", default_value_t = 0.25)]
    pub spread_res: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, ValueEnum)]
pub enum LogLevel {
    /// Disable logging output.
    Off,
    /// No output except errors.
    Error,
    /// Show warnings and errors.
    Warn,
    /// Show info, warnings and errors (default).
    Info,
    /// Show debug, info, warnings and errors.
    Debug,
    /// Show all log messages including trace.
    Trace,
}

impl Default for LogLevel {
    fn default() -> Self {
        Self::Info
    }
}

impl std::str::FromStr for LogLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "error" => Ok(Self::Error),
            "warn" | "warning" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(format!("Unknown log level: {s}")),
        }
    }
}

impl LogLevel {
    /// Convert LogLevel to log::LevelFilter
    pub fn to_level_filter(self) -> log::LevelFilter {
        match self {
            LogLevel::Off => log::LevelFilter::Off,
            LogLevel::Error => log::LevelFilter::Error,
            LogLevel::Warn => log::LevelFilter::Warn,
            LogLevel::Info => log::LevelFilter::Info,
            LogLevel::Debug => log::LevelFilter::Debug,
            LogLevel::Trace => log::LevelFilter::Trace,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, ValueEnum)]
pub enum LogFormat {
    /// Colorized human-readable text.
    Plain,
    /// Structured JSON per log record.
    Json,
}

impl Default for LogFormat {
    fn default() -> Self {
        Self::Plain
    }
}

impl std::str::FromStr for LogFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "plain" => Ok(Self::Plain),
            "json" => Ok(Self::Json),
            _ => Err(format!("Unknown log format: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq)]
pub enum OutputBackend {
    /// PipeWire audio output (streaming, Linux only).
    #[cfg(target_os = "linux")]
    Pipewire,
    /// ASIO audio output (Windows only, requires 'asio' feature).
    #[cfg(target_os = "windows")]
    Asio,
    /// CoreAudio audio output (macOS only).
    #[cfg(target_os = "macos")]
    Coreaudio,
    /// Write rendered interleaved f32 to stdout / a file / a named pipe (FIFO).
    /// Non-realtime: no device clock, no adaptive resampling. See
    /// `--output-file` and `--output-file-format`.
    File,
    /// Placeholder used when no realtime output backend is compiled in.
    #[value(skip)]
    Unsupported,
}

/// Container/format for the `file` output backend.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum OutputFileFormatArg {
    /// Headerless interleaved 32-bit float, little-endian
    /// (consumable by `ffmpeg -f f32le -ar <sr> -ac <n>`).
    RawF32,
    /// Streaming Core Audio Format (LPCM 32-bit float, data size = -1).
    Caf,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum InputBackend {
    #[cfg(target_os = "linux")]
    Pipewire,
    #[cfg(target_os = "windows")]
    Asio,
    #[value(skip)]
    Unsupported,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum InputMapModeArg {
    #[value(name = "7.1-fixed")]
    SevenOneFixed,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum InputLfeModeArg {
    Object,
    Direct,
    Drop,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum InputSampleFormatArg {
    F32,
    S16,
}

impl OutputBackend {
    pub fn platform_default() -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            return Some(Self::Pipewire);
        }
        #[cfg(target_os = "windows")]
        {
            return Some(Self::Asio);
        }
        #[cfg(target_os = "macos")]
        {
            return Some(Self::Coreaudio);
        }
        #[allow(unreachable_code)]
        None
    }

    /// True when this backend is the cpal-based local output for the current
    /// platform (ASIO on Windows, CoreAudio on macOS). Lets the shared
    /// latency-target re-init and runtime-reset logic stay platform-agnostic.
    pub fn is_local_cpal(self) -> bool {
        #[cfg(target_os = "windows")]
        let local = matches!(self, Self::Asio);
        #[cfg(target_os = "macos")]
        let local = matches!(self, Self::Coreaudio);
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let local = {
            let _ = self;
            false
        };
        local
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum EvaluationModeArg {
    Polar,
    Cartesian,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum RampModeArg {
    Off,
    Frame,
    Sample,
    Interp,
}

impl From<RampModeArg> for RampMode {
    fn from(value: RampModeArg) -> Self {
        match value {
            RampModeArg::Off => RampMode::Off,
            RampModeArg::Frame => RampMode::Frame,
            RampModeArg::Sample => RampMode::Sample,
            RampModeArg::Interp => RampMode::Interp,
        }
    }
}

/// How channel-based (non-object) content is rendered. See
/// [`ChannelRenderMode`]. The legacy `direct`/`virtual` values are accepted as
/// aliases of `spatial` (placement is now per-channel in the virtual bed).
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum ChannelRenderModeArg {
    Host,
    #[value(alias = "virtual", alias = "direct")]
    Spatial,
}

impl From<ChannelRenderModeArg> for ChannelRenderMode {
    fn from(value: ChannelRenderModeArg) -> Self {
        match value {
            ChannelRenderModeArg::Host => ChannelRenderMode::Host,
            ChannelRenderModeArg::Spatial => ChannelRenderMode::Spatial,
        }
    }
}

impl From<ChannelRenderMode> for ChannelRenderModeArg {
    fn from(value: ChannelRenderMode) -> Self {
        match value {
            ChannelRenderMode::Host => ChannelRenderModeArg::Host,
            ChannelRenderMode::Spatial => ChannelRenderModeArg::Spatial,
        }
    }
}

/// Where the 4.x/5.x surround pair is placed. See [`SurroundPlacement`].
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum SurroundPlacementArg {
    #[value(alias = "sides")]
    Side,
    #[value(alias = "rear")]
    Back,
}

impl From<SurroundPlacementArg> for SurroundPlacement {
    fn from(value: SurroundPlacementArg) -> Self {
        match value {
            SurroundPlacementArg::Side => SurroundPlacement::Side,
            SurroundPlacementArg::Back => SurroundPlacement::Back,
        }
    }
}

impl From<SurroundPlacement> for SurroundPlacementArg {
    fn from(value: SurroundPlacement) -> Self {
        match value {
            SurroundPlacement::Side => SurroundPlacementArg::Side,
            SurroundPlacement::Back => SurroundPlacementArg::Back,
        }
    }
}

/// Spatial render backend selector. The string forms match the canonical
/// backend ids in `renderer::render_backend` (`canonical_builtin_backend_id`).
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum RenderBackendArg {
    Vbap,
    Barycenter,
    ExperimentalDistance,
    Hybrid,
}

impl RenderBackendArg {
    /// Canonical config id (`render.render_backend`).
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Vbap => "vbap",
            Self::Barycenter => "barycenter",
            Self::ExperimentalDistance => "experimental_distance",
            Self::Hybrid => "hybrid",
        }
    }
}

/// Inner backend accepted by the hybrid backend (no nested hybrid).
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum HybridInnerBackendArg {
    Vbap,
    Barycenter,
    ExperimentalDistance,
}

impl HybridInnerBackendArg {
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Vbap => "vbap",
            Self::Barycenter => "barycenter",
            Self::ExperimentalDistance => "experimental_distance",
        }
    }
}

/// Distance metric selector (`spherical` / `chebyshev`).
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum DistanceMetricArg {
    Spherical,
    Chebyshev,
}

impl DistanceMetricArg {
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Spherical => "spherical",
            Self::Chebyshev => "chebyshev",
        }
    }
}

/// Size-to-spread reduction policy (`render.size_to_spread_mode`).
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum SizeToSpreadModeArg {
    Max,
    Mean,
    ProjectionPerpendicular,
}

impl From<SizeToSpreadModeArg> for renderer::render_backend::SizeToSpreadMode {
    fn from(value: SizeToSpreadModeArg) -> Self {
        use renderer::render_backend::SizeToSpreadMode;
        match value {
            SizeToSpreadModeArg::Max => SizeToSpreadMode::Max,
            SizeToSpreadModeArg::Mean => SizeToSpreadMode::Mean,
            SizeToSpreadModeArg::ProjectionPerpendicular => {
                SizeToSpreadMode::ProjectionPerpendicular
            }
        }
    }
}

impl std::str::FromStr for OutputBackend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            #[cfg(target_os = "linux")]
            "pipewire" => Ok(Self::Pipewire),
            #[cfg(target_os = "windows")]
            "asio" => Ok(Self::Asio),
            #[cfg(target_os = "macos")]
            "coreaudio" | "core-audio" => Ok(Self::Coreaudio),
            "file" => Ok(Self::File),
            // Platform-agnostic alias for the realtime device backend, so a
            // host (e.g. Studio) can request "device" without knowing whether
            // that means PipeWire, ASIO or CoreAudio.
            "device" => {
                Self::platform_default().ok_or_else(|| "No device output backend".to_string())
            }
            _ => Err(format!("Unknown output backend: {s}")),
        }
    }
}

impl std::str::FromStr for InputBackend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            #[cfg(target_os = "linux")]
            "pipewire" => Ok(Self::Pipewire),
            #[cfg(target_os = "windows")]
            "asio" => Ok(Self::Asio),
            _ => Err(format!("Unknown input backend: {s}")),
        }
    }
}
