use std::path::PathBuf;

use clap::{
    ArgMatches, Args, CommandFactory, FromArgMatches, Parser as ClapParser, Subcommand, ValueEnum,
    parser::ValueSource,
};

use renderer::live_params::ChannelRenderMode;

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
    after_help = "If no command is given, `render` is assumed: `orender <INPUT>` is `orender render <INPUT>`.",
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
        let matches = Cli::command()
            .mut_subcommand("render", |render| {
                render.args(crate::cli::options::option_args())
            })
            .try_get_matches_from(args)?;
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
    /// The registered options given on the command line (the generated
    /// flags, `crate::cli::options`).
    pub fn option_values(&self) -> Vec<(&'static str, crate::cli::options::CliValue)> {
        self.matches
            .map(crate::cli::options::given_values)
            .unwrap_or_default()
    }

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

    /// Play a pipe through the new sync output (resampling rework, experimental).
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    SyncPlay(crate::cli::sync_host::SyncPlayArgs),

    /// List the realtime output devices (Windows only): the ASIO ones, or the
    /// WASAPI ones when no ASIO driver is installed
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
    /// Realtime audio output backend: option `output_backend`, resolved from
    /// the config (a `--output-backend` flag is folded in first).
    #[arg(skip)]
    pub output_backend: Option<OutputBackend>,
    /// Destination of the `file` backend (`-` = stdout): option `output_file`.
    #[arg(skip = String::from("-"))]
    pub output_file: String,
    /// Format of the `file` backend: option `output_file_format`.
    #[arg(skip = OutputFileFormatArg::RawF32)]
    pub output_file_format: OutputFileFormatArg,

    /// Presentation or substream selector passed to the bridge plugin.
    /// "best" selects the richest available presentation (default).
    /// Pass a number to request a specific substream (bridge-defined).
    #[arg(long, value_name = "VALUE", default_value = renderer::config_fields::presentation::DEFAULT)]
    pub presentation: String,

    /// Path to a format bridge plugin library; repeat it to load several, in
    /// load order (each stream goes to the bridge that decodes it).
    #[arg(long = "bridge-path", value_name = "FILE")]
    pub bridge_paths: Vec<PathBuf>,

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
    /// Output device or target name: option `output_device`.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    #[arg(skip)]
    pub output_device: Option<String>,
    /// Target buffer latency in milliseconds: option `latency_target`.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    #[arg(skip)]
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

    /// VBAP spreading coefficient (0.0 = point source, 1.0 = maximum spread)
    /// Deprecated: Use --vbap-distance-res instead for dynamic per-object spread
    #[arg(long, value_name = "SPREAD", default_value_t = renderer::config_fields::vbap_spread::DEFAULT)]
    pub vbap_spread: f32,

    /// Allow negative Z values for VBAP tables (floor below listener).
    #[arg(long, conflicts_with = "no_vbap_allow_negative_z")]
    pub vbap_allow_negative_z: bool,

    /// Disable negative Z values for VBAP tables.
    #[arg(long, conflicts_with = "vbap_allow_negative_z")]
    pub no_vbap_allow_negative_z: bool,

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

    /// Master gain in dB applied to VBAP output (default: 0.0 = unity gain)
    /// Use negative values to reduce output level (e.g., -6.0 for -6dB headroom)
    #[arg(
        long,
        value_name = "DB",
        default_value_t = renderer::config_fields::master_gain::DEFAULT,
        allow_hyphen_values = true
    )]
    pub master_gain: f32,

    /// How to render channel-based (non-object) content: `host` (let the sink
    /// handle the channels, no spatialization), `direct` (route each channel to
    /// its matching speaker), or `virtual` (virtualize each channel as an object
    /// at its speaker angle — the default).
    #[arg(long, value_enum, default_value_t = ChannelRenderModeArg::Spatial)]
    pub channel_render_mode: ChannelRenderModeArg,

    /// Disable automatic draining of buffered data from named pipes at startup
    /// (By default, orender drains FIFOs to minimize latency for real-time streams)
    #[arg(long)]
    pub no_drain_pipe: bool,

    /// Output sample rate in Hz: option `output_sample_rate` (unset = the
    /// stream's).
    #[arg(skip)]
    pub output_sample_rate: Option<u32>,

    /// Adaptive resampling (PI on the buffer fill): option
    /// `enable_adaptive_resampling`.
    #[arg(skip)]
    pub enable_adaptive_resampling: bool,

    // ── Backend parameters: not registry options (the per-backend param
    // bag). Override-only: when omitted the value is kept from the config
    // file / engine default. Consumed via `apply_render_cfg_overrides`.
    /// Barycenter backend: localization sharpness (0.0 = diffuse).
    /// Only meaningful with `--render-backend barycenter`.
    #[arg(long = "barycenter-localize", value_name = "AMOUNT")]
    pub barycenter_localize: Option<f32>,

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

    /// Policy reducing an object's (w, d, h) size to a scalar spread.
    #[arg(long = "size-to-spread-mode", value_enum)]
    pub size_to_spread_mode: Option<SizeToSpreadModeArg>,
    // The registered options (the core's live options and this host's audio
    // output and input) are not declared here: their flags are generated
    // from the registries (`crate::cli::options`) and folded into the config
    // before the fields above are resolved from it.
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
    /// ASIO audio output (Windows only; always compiled into Windows builds).
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
            _ => Err(format!("Unknown input backend: {s}")),
        }
    }
}
