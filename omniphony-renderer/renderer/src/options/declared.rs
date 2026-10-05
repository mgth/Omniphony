//! Options declared once.
//!
//! A row of [`declared_options!`] is the whole declaration of an option: its
//! value type and default, its kind and labels. The macro generates from it
//!
//! * the field of [`DeclaredOptions`] that holds the live value
//!   (`LiveParams::options`), and its default;
//! * the field of [`DeclaredOptionsConfig`] that holds the saved value
//!   (`RenderConfig::options`, flattened: the key is the option's key);
//! * the constant in [`defaults`] (what a CLI default or a test refers to);
//! * the [`OptionSpec`] row, whose `set` / `get_json` / `config_store` /
//!   `config_seed` follow from the value type ([`DeclaredValue`]).
//!
//! So adding a live option that fits here is one row plus the code that uses
//! the value. A row may replace any of the four functions when it needs to
//! (a legacy wire shape, a value always written).
//!
//! Options whose live value sits inside a larger structure (the binaural
//! stage, the room, the evaluation layer, the hybrid backend) are declared by
//! hand in `HAND_WIRED_ROWS`.

use super::{
    LegacyAddr, OptionDefault, OptionEnv, OptionFlags, OptionKind, OptionSpec, RawOptionValue,
    bool_canonical, clamp_to, raw_bool, raw_float, raw_int, raw_str, round6,
};
use crate::config::RenderConfig;
use crate::config::unknown_values::{EnumKey, keep_unknown};
use crate::live_params::{
    CrossoverType, LiveParams, OutputChannelMapping, PhantomExtractMode, RampMode,
    SurroundPlacement,
};
use omniphony_osc_contract as osc_contract;

/// How a value type declared here behaves on each layer.
pub trait DeclaredValue: Clone + Sized {
    /// The type of its declared default (`&'static str` for a `String`).
    type Default: Copy;
    fn from_default(default: Self::Default) -> Self;
    fn is_default(&self, default: Self::Default) -> bool;
    /// A client value, validated (and bounded) for `kind`.
    fn from_raw(raw: &RawOptionValue, kind: OptionKind) -> Option<Self>;
    /// The value applied, as echoed in logs.
    fn canonical(&self) -> String;
    fn to_json(&self) -> serde_json::Value;
    /// The value written to the config.
    fn stored(&self) -> Self {
        self.clone()
    }
    /// A value read from the config, bounded like a client write.
    fn from_config(self, _kind: OptionKind) -> Self {
        self
    }
}

impl DeclaredValue for bool {
    type Default = bool;
    fn from_default(default: bool) -> Self {
        default
    }
    fn is_default(&self, default: bool) -> bool {
        *self == default
    }
    fn from_raw(raw: &RawOptionValue, _kind: OptionKind) -> Option<Self> {
        raw_bool(raw)
    }
    fn canonical(&self) -> String {
        bool_canonical(*self)
    }
    fn to_json(&self) -> serde_json::Value {
        (*self).into()
    }
}

/// Saved to six decimals; a value within 1e-4 of the default is the default.
impl DeclaredValue for f32 {
    type Default = f32;
    fn from_default(default: f32) -> Self {
        default
    }
    fn is_default(&self, default: f32) -> bool {
        (*self - default).abs() <= 1e-4
    }
    fn from_raw(raw: &RawOptionValue, kind: OptionKind) -> Option<Self> {
        raw_float(raw, kind)
    }
    fn canonical(&self) -> String {
        format!("{self}")
    }
    fn to_json(&self) -> serde_json::Value {
        (*self).into()
    }
    fn stored(&self) -> Self {
        round6(*self)
    }
    fn from_config(self, kind: OptionKind) -> Self {
        clamp_to(kind, self)
    }
}

impl DeclaredValue for usize {
    type Default = usize;
    fn from_default(default: usize) -> Self {
        default
    }
    fn is_default(&self, default: usize) -> bool {
        *self == default
    }
    fn from_raw(raw: &RawOptionValue, kind: OptionKind) -> Option<Self> {
        usize::try_from(raw_int(raw, kind)?).ok()
    }
    fn canonical(&self) -> String {
        self.to_string()
    }
    fn to_json(&self) -> serde_json::Value {
        (*self).into()
    }
    fn from_config(self, kind: OptionKind) -> Self {
        match kind {
            OptionKind::Int { min, max } => (self as i64).clamp(min, max) as usize,
            _ => self,
        }
    }
}

impl DeclaredValue for String {
    type Default = &'static str;
    fn from_default(default: &'static str) -> Self {
        default.to_owned()
    }
    fn is_default(&self, default: &'static str) -> bool {
        self == default
    }
    fn from_raw(raw: &RawOptionValue, _kind: OptionKind) -> Option<Self> {
        raw_str(raw).map(str::to_owned)
    }
    fn canonical(&self) -> String {
        self.clone()
    }
    fn to_json(&self) -> serde_json::Value {
        self.as_str().into()
    }
}

/// An enum read from its canonical spelling or an alias (`from_str`), and
/// written in its canonical spelling (`as_str`, which its serde derive
/// agrees with). A config value it does not know is kept, not refused
/// (`config::unknown_values`).
pub trait DeclaredEnum: Copy + PartialEq + serde::Serialize + 'static {
    fn as_str(self) -> &'static str;
    fn from_str(value: &str) -> Option<Self>;
}

macro_rules! declared_enum {
    ($($ty:ty),* $(,)?) => {$(
        impl DeclaredEnum for $ty {
            fn as_str(self) -> &'static str {
                <$ty>::as_str(self)
            }
            fn from_str(value: &str) -> Option<Self> {
                <$ty>::from_str(value)
            }
        }

        impl DeclaredValue for $ty {
            type Default = $ty;
            fn from_default(default: $ty) -> Self {
                default
            }
            fn is_default(&self, default: $ty) -> bool {
                *self == default
            }
            fn from_raw(raw: &RawOptionValue, _kind: OptionKind) -> Option<Self> {
                <$ty>::from_str(raw_str(raw)?)
            }
            fn canonical(&self) -> String {
                self.as_str().to_owned()
            }
            fn to_json(&self) -> serde_json::Value {
                self.as_str().into()
            }
        }
    )*};
}

declared_enum!(
    SurroundPlacement,
    OutputChannelMapping,
    PhantomExtractMode,
    CrossoverType,
    RampMode,
);

/// Reads an enum-typed config value through [`DeclaredEnum::from_str`]; a
/// value it refuses is kept for the section's `extra` ([`keep_unknown`]).
struct KeptEnum<T>(&'static str, std::marker::PhantomData<T>);

impl<'de, T: DeclaredEnum> serde::de::DeserializeSeed<'de> for KeptEnum<T> {
    type Value = Option<T>;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        keep_unknown(deserializer, None, self.0, "render.", |value| {
            use serde::de::Error;
            match &value {
                serde_yaml_ng::Value::Null => Ok(None),
                serde_yaml_ng::Value::String(s) => T::from_str(s)
                    .map(Some)
                    .ok_or_else(|| serde_yaml_ng::Error::custom(format!("unknown value `{s}`"))),
                _ => Err(serde_yaml_ng::Error::custom("expected a string")),
            }
        })
    }
}

/// The Rust type of a declared row's value, by category.
macro_rules! declared_ty {
    (Bool) => {
        bool
    };
    (Float) => {
        f32
    };
    (Int) => {
        usize
    };
    (Str) => {
        String
    };
    (Enum($ty:ty)) => {
        $ty
    };
}

/// A declared row's published default, by category (a const: the row is).
macro_rules! declared_default {
    (Bool, $value:expr) => {
        OptionDefault::Bool($value)
    };
    (Float, $value:expr) => {
        OptionDefault::Float($value)
    };
    (Int, $value:expr) => {
        OptionDefault::Int($value as i64)
    };
    (Str, $value:expr) => {
        OptionDefault::Str($value)
    };
    (Enum($ty:ty), $value:expr) => {
        OptionDefault::Str(<$ty>::as_str($value))
    };
}

/// How a declared row's config value is read, by category.
macro_rules! declared_seed {
    (Enum($ty:ty), $key:expr) => {
        KeptEnum::<$ty>($key, std::marker::PhantomData)
    };
    ($cat:ident, $key:expr) => {
        std::marker::PhantomData::<Option<declared_ty!($cat)>>
    };
}

/// The [`EnumKey`] of an enum row (`None` for any other category).
macro_rules! declared_enum_key {
    (Enum($ty:ty), $name:ident) => {
        Some(EnumKey {
            parent: None,
            key: stringify!($name),
            chosen: |render: &RenderConfig| {
                render
                    .options
                    .$name
                    .as_ref()
                    .is_some_and(|value| !value.is_default(defaults::$name))
            },
            clear: |render: &mut RenderConfig| render.options.$name = None,
        })
    };
    ($cat:ident, $name:ident) => {
        None
    };
}

/// `$given` when the row supplies it, `$fallback` otherwise.
macro_rules! given_or {
    ($given:expr ; $fallback:expr) => {
        $given
    };
    (; $fallback:expr) => {
        $fallback
    };
}

macro_rules! declared_options {
    ($(
        $(#[doc = $doc:literal])*
        $name:ident: $cat:ident $(($ety:ty))? = $default:expr => {
            kind: $kind:expr,
            flags: $flags:expr,
            group: $group:expr,
            i18n: $i18n:expr,
            help: $help:expr,
            alias: $alias:expr,
            $(set: $set:expr,)?
            $(store: $store:expr,)?
            $(seed: $seed:expr,)?
        }
    )*) => {
        /// The declared options' defaults: the one copy of each.
        #[allow(non_upper_case_globals)]
        pub mod defaults {
            use super::*;
            $(
                pub const $name: <declared_ty!($cat $(($ety))?) as DeclaredValue>::Default = $default;
            )*
        }

        /// The live values of the declared options (`LiveParams::options`).
        #[derive(Debug, Clone, PartialEq)]
        pub struct DeclaredOptions {
            $(
                $(#[doc = $doc])*
                pub $name: declared_ty!($cat $(($ety))?),
            )*
        }

        impl Default for DeclaredOptions {
            fn default() -> Self {
                Self {
                    $($name: DeclaredValue::from_default(defaults::$name),)*
                }
            }
        }

        /// The saved values of the declared options (`RenderConfig::options`,
        /// flattened into `render`). Absent = the default.
        #[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
        pub struct DeclaredOptionsConfig {
            $(
                $(#[doc = $doc])*
                #[serde(skip_serializing_if = "Option::is_none")]
                pub $name: Option<declared_ty!($cat $(($ety))?)>,
            )*
        }

        impl<'de> serde::Deserialize<'de> for DeclaredOptionsConfig {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Fields;
                impl<'de> serde::de::Visitor<'de> for Fields {
                    type Value = DeclaredOptionsConfig;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("the declared options")
                    }
                    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                        let mut config = DeclaredOptionsConfig::default();
                        while let Some(key) = map.next_key::<String>()? {
                            match key.as_str() {
                                $(
                                    stringify!($name) => {
                                        config.$name = map.next_value_seed(
                                            declared_seed!($cat $(($ety))?, stringify!($name)),
                                        )?;
                                    }
                                )*
                                _ => {
                                    map.next_value::<serde::de::IgnoredAny>()?;
                                }
                            }
                        }
                        Ok(config)
                    }
                }
                deserializer.deserialize_struct("DeclaredOptionsConfig", DECLARED_KEYS, Fields)
            }
        }

        /// Write one declared option into a config as a save would, absent at
        /// its default. (A row's `config_store` may do more: the
        /// synthetic-object master is always written, the phantom mode drops
        /// the legacy keys it replaced.)
        pub mod store {
            use super::*;
            $(
                pub fn $name(render: &mut RenderConfig, value: declared_ty!($cat $(($ety))?)) {
                    render.options.$name = (!value.is_default(defaults::$name)).then(|| value.stored());
                }
            )*
        }

        const DECLARED_KEYS: &[&str] = &[$(stringify!($name)),*];

        /// The enum-typed keys of [`DeclaredOptionsConfig`], for the save
        /// side of `config::unknown_values`.
        pub(crate) static DECLARED_ENUM_KEYS: &[Option<EnumKey<RenderConfig>>] = &[
            $(declared_enum_key!($cat $(($ety))?, $name),)*
        ];

        /// The registry rows of the declared options.
        pub(super) const DECLARED_ROWS: &[OptionSpec] = &[$(
            OptionSpec {
                key: stringify!($name),
                kind: $kind,
                default: declared_default!($cat $(($ety))?, defaults::$name),
                flags: $flags,
                group: $group,
                i18n_key: $i18n,
                help_i18n_key: $help,
                legacy_control_addr: $alias,
                set: given_or!($($set)? ; |live: &mut LiveParams, raw: &RawOptionValue, _env: &OptionEnv| {
                    let value = <declared_ty!($cat $(($ety))?) as DeclaredValue>::from_raw(raw, $kind)?;
                    let canonical = value.canonical();
                    live.options.$name = value;
                    Some(canonical)
                }),
                get_json: |live: &LiveParams| live.options.$name.to_json(),
                config_store: given_or!($($store)? ; |render: &mut RenderConfig, live: &LiveParams, _env: &OptionEnv| {
                    let value = &live.options.$name;
                    render.options.$name = (!value.is_default(defaults::$name)).then(|| value.stored());
                }),
                config_seed: given_or!($($seed)? ; |live: &mut LiveParams, render: &RenderConfig, _env: &OptionEnv| {
                    if let Some(value) = render.options.$name.clone() {
                        live.options.$name = value.from_config($kind);
                    }
                }),
            },
        )*];
    };
}

use super::{
    AUTO_GAIN_CEILING_DB_KIND, CROSSOVER, CROSSOVER_FIR_TRANSITION_RATIO_KIND,
    DIALOGUE_GAIN_DB_KIND, DRC_WEIGHT_KIND, SAMPLE_RAMP_STRIDE_KIND,
};

declared_options! {
    /// Where the 4.x/5.x surround pair (`Ls`/`Rs`) is placed through the
    /// virtual bed: side or back. Consulted only for channel content without
    /// dedicated back channels; 7.x sources ignore it.
    surround_placement: Enum(SurroundPlacement) = SurroundPlacement::Side => {
        kind: OptionKind::Enum(&["side", "back"]),
        flags: OptionFlags::REPLAN,
        group: None,
        i18n: "twoDSources.surroundLabel",
        help: None,
        alias: LegacyAddr::Exact(osc_contract::CONTROL_SURROUND_PLACEMENT),
    }

    /// Global permission for renderer-synthesized objects. When false, both the
    /// phantom extractor and the height generator are bypassed without clearing
    /// their configured selections or parameters.
    synthetic_objects_enabled: Bool = false => {
        kind: OptionKind::Bool,
        flags: OptionFlags::REPLAN,
        group: None,
        i18n: "twoDSources.syntheticObjectsLabel",
        help: Some("help.syntheticObjects"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_SYNTHETIC_OBJECTS),
        // Always saved, false included: an explicit false must keep
        // suppressing remembered non-off child selections after a reload.
        store: |render: &mut RenderConfig, live: &LiveParams, _env: &OptionEnv| {
            render.options.synthetic_objects_enabled = Some(live.options.synthetic_objects_enabled);
        },
    }

    /// Decode on a thread of its own, overlapping the render (a performance
    /// switch). Honoured by the liborender engine only when its host lets the
    /// option decide (`orender_set_option("decode_thread", "live")`), since the
    /// host must then stamp its output from the input timestamps carried with the
    /// audio and drain at end of stream. The standalone renderer always decodes on
    /// its own thread and ignores it.
    decode_thread: Bool = false => {
        kind: OptionKind::Bool,
        // No REPLAN: nothing synthesized depends on where decoding runs.
        // Embedded only: the standalone renderer always decodes on a thread
        // of its own, so there the option is inert.
        flags: OptionFlags::EMBEDDED_ONLY,
        group: None,
        i18n: "renderer.decodeThreadLabel",
        help: Some("help.decodeThread"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_DECODE_THREAD),
    }

    /// How output channels map to device ports: positionless `ByIndex` (port N
    /// = layout speaker N) or positional `ByName`. Consulted when the output
    /// stream is (re)configured.
    output_channel_mapping: Enum(OutputChannelMapping) = OutputChannelMapping::ByIndex => {
        kind: OptionKind::Enum(&["by_index", "by_name"]),
        flags: OptionFlags::NONE,
        group: None,
        i18n: "audio.channelMapping",
        help: None,
        alias: LegacyAddr::Exact(osc_contract::CONTROL_OUTPUT_CHANNEL_MAPPING),
    }

    /// The bed→height object generator (2D upmix): synthesizes height objects
    /// from channel-based content so a height-capable layout (7.1.4, …) is
    /// exercised when the source has no height. Empty / `"none"` = disabled.
    /// Consulted only for channel content without spatial objects. Each generator
    /// keeps its own parameter values (the plugin store is keyed by generator
    /// id), so a change of selection clears nothing.
    object_generator_id: Str = "" => {
        kind: OptionKind::Str,
        flags: OptionFlags::REPLAN,
        group: None,
        i18n: "twoDSources.objectGeneratorLabel",
        help: Some("help.objectGenerator"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_OBJECT_GENERATOR),
    }

    /// Phantom-source extraction algorithm. `Off` disables only this stage; the
    /// synthesized-object master may independently suppress it.
    phantom_extract_mode: Enum(PhantomExtractMode) = PhantomExtractMode::Off => {
        kind: OptionKind::Enum(&["off", "broadband", "spectral"]),
        flags: OptionFlags::REPLAN,
        group: None,
        i18n: "twoDSources.phantomLabel",
        help: Some("help.phantomExtract"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_PHANTOM_EXTRACT),
        set: |live: &mut LiveParams, raw: &RawOptionValue, _env: &OptionEnv| {
            let mode = match raw {
                RawOptionValue::Str(s) => PhantomExtractMode::from_str(s)?,
                // The old boolean address: enabling selects the historical
                // broadband default.
                RawOptionValue::Number(n) => {
                    if *n == 0.0 {
                        PhantomExtractMode::Off
                    } else {
                        PhantomExtractMode::Broadband
                    }
                }
                RawOptionValue::Bool(false) => PhantomExtractMode::Off,
                RawOptionValue::Bool(true) => PhantomExtractMode::Broadband,
                RawOptionValue::Numbers(_) | RawOptionValue::Null => return None,
            };
            live.options.phantom_extract_mode = mode;
            Some(mode.as_str().to_string())
        },
        // Also drops the legacy keys the mode replaced.
        store: |render: &mut RenderConfig, live: &LiveParams, _env: &OptionEnv| {
            let mode = live.options.phantom_extract_mode;
            render.options.phantom_extract_mode =
                (!mode.is_default(defaults::phantom_extract_mode)).then_some(mode);
            render.phantom_enabled = None;
            if let Some(params) = render.phantom_params.as_mut() {
                params.remove("method");
                if params.is_empty() {
                    render.phantom_params = None;
                }
            }
        },
    }

    /// Crossover filter implementation: minimum-latency IIR (`lr4`) or
    /// linear-phase FIR (`fir`, ~0.1 s of latency). The speaker stage compares it
    /// against the bank it built every frame, so a flip takes effect without a
    /// topology change.
    crossover_type: Enum(CrossoverType) = CrossoverType::Lr4 => {
        kind: OptionKind::Enum(&["lr4", "fir"]),
        // No REPLAN: the speaker stage compares the live value against the
        // bank it built every frame and rebuilds the filter bank itself; no
        // synthesized-object topology depends on it.
        flags: OptionFlags::NONE,
        group: Some(&CROSSOVER),
        i18n: "renderer.crossoverTypeLabel",
        help: Some("help.crossoverType"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_CROSSOVER_TYPE),
    }

    /// FIR crossover transition width as a fraction of the lowest cutoff (the
    /// Kaiser design's `transition_ratio`): smaller = steeper bands but more
    /// taps, latency and ringing. Only consulted by the `fir` engine; the speaker
    /// stage rebuilds the bank live when it moves.
    crossover_fir_transition_ratio: Float = 0.5 => {
        kind: CROSSOVER_FIR_TRANSITION_RATIO_KIND,
        // No REPLAN, same as crossover_type.
        flags: OptionFlags::NONE,
        group: Some(&CROSSOVER),
        i18n: "renderer.crossoverTransitionLabel",
        help: Some("help.crossoverFirTransition"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_CROSSOVER_FIR_TRANSITION_RATIO),
    }

    // ── Gain stage, loudness, transitions, DRC ──────────────────────────
    //
    // The flat snapshot keys (`autoGain`, `autoGainCeilingDb`, `rampMode`,
    // `/state/loudness` `enabled`, `/state/input` `drcMode` / `drcWeight`)
    // are still emitted. None re-plans anything: they are read per frame
    // (gain stage, ramps) or pushed to the decoder.

    /// Automatic gain reduction: the gain stage permanently lowers the output
    /// gain on detected clipping (peak hold, no recovery).
    auto_gain: Bool = false => {
        kind: OptionKind::Bool,
        flags: OptionFlags::NONE,
        group: None,
        i18n: "autoGain.title",
        help: Some("help.master.autoGain"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_AUTO_GAIN),
    }

    /// Ceiling (dBFS) auto-gain corrects detected peaks down to. Clipping is
    /// detected at 0 dBFS (peak > 1.0); when it fires, the master gain is lowered
    /// so the peak lands at this level, leaving headroom so corrections fire less
    /// often.
    auto_gain_ceiling_db: Float = -1.0 => {
        kind: AUTO_GAIN_CEILING_DB_KIND,
        flags: OptionFlags::NONE,
        group: None,
        i18n: "autoGain.ceiling",
        help: Some("help.master.ceiling"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_AUTO_GAIN_CEILING),
        // Seeded as configured (not clamped); only a client write is bounded.
        seed: |live: &mut LiveParams, render: &RenderConfig, _env: &OptionEnv| {
            if let Some(db) = render.options.auto_gain_ceiling_db {
                live.options.auto_gain_ceiling_db = db;
            }
        },
    }

    /// Apply the stream's loudness (dialogue normalisation) metadata.
    use_loudness: Bool = false => {
        kind: OptionKind::Bool,
        flags: OptionFlags::NONE,
        group: None,
        i18n: "section.loudness",
        help: Some("help.drc.loudness"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_LOUDNESS),
    }

    /// Ramp processing mode for object moves and gain transitions.
    ramp_mode: Enum(RampMode) = RampMode::Frame => {
        kind: OptionKind::Enum(&["off", "frame", "interp", "sample"]),
        flags: OptionFlags::NONE,
        group: None,
        i18n: "audio.rampMode",
        help: None,
        alias: LegacyAddr::Exact(osc_contract::CONTROL_RAMP_MODE),
    }

    /// `RampMode::Sample`: samples between two gain lookups of a moving object,
    /// interpolated linearly in between; 1 is a lookup per sample. In
    /// `[1, MAX_SAMPLE_RAMP_STRIDE]`. No REPLAN: the speaker stage reads it every
    /// block, so a change mid-movement takes effect at the next segment.
    sample_ramp_stride: Int = 8 => {
        kind: SAMPLE_RAMP_STRIDE_KIND,
        flags: OptionFlags::NONE,
        group: None,
        i18n: "audio.sampleRampStride",
        help: Some("help.audio.sampleRampStride"),
        alias: LegacyAddr::None,
    }

    /// Dynamic Range Control mode. Free-form: the modes are the bridge's
    /// (`supportedDrcModes` on `/state/input`), not a set the renderer knows.
    drc_mode: Str = "Off" => {
        kind: OptionKind::Str,
        flags: OptionFlags::NONE,
        group: None,
        i18n: "input.drc",
        help: Some("help.drc.mode"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_INPUT_DRC_MODE),
    }

    /// DRC weighting in [0, 1]: 1 applies the full bridge-decoded DRC gain, 0
    /// bypasses it, values between scale the dB reduction linearly
    /// (effective_gain = bridge_gain.powf(drc_weight)).
    drc_weight: Float = 1.0 => {
        kind: DRC_WEIGHT_KIND,
        flags: OptionFlags::NONE,
        group: None,
        i18n: "input.drc_weight",
        help: Some("help.drc.weight"),
        alias: LegacyAddr::Exact(osc_contract::CONTROL_INPUT_DRC_WEIGHT),
    }

    /// Gain in dB on the channels the bridge tags as dialogue
    /// (`FormatBridge::channel_tags`); 0 leaves them as the stream mixed them.
    /// Applied with the PCM conversion, so before the upmix stages.
    dialogue_gain_db: Float = 0.0 => {
        kind: DIALOGUE_GAIN_DB_KIND,
        flags: OptionFlags::NONE,
        group: None,
        i18n: "input.dialogue_gain",
        help: Some("help.drc.dialogueGain"),
        alias: LegacyAddr::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn parse(yaml: &str) -> RenderConfig {
        serde_yaml_ng::from_str::<Config>(yaml)
            .expect("config parses")
            .render
            .expect("render section")
    }

    /// A declared key is read into `options`, an alias of an enum value
    /// included, and never spills into `extra`, which still takes the keys
    /// nothing declares.
    #[test]
    fn declared_keys_land_in_options_and_unknown_keys_in_extra() {
        let render = parse(
            "render:\n  dialogue_gain_db: -3\n  drc_mode: Light\n  ramp_mode: per_frame\n  \
             surround_placement: rear\n  sample_ramp_stride: 4\n  not_an_option: 1\n",
        );
        assert_eq!(render.options.dialogue_gain_db, Some(-3.0));
        assert_eq!(render.options.drc_mode.as_deref(), Some("Light"));
        assert_eq!(render.options.ramp_mode, Some(RampMode::Frame));
        assert_eq!(
            render.options.surround_placement,
            Some(SurroundPlacement::Back)
        );
        assert_eq!(render.options.sample_ramp_stride, Some(4));
        let extra: Vec<_> = render.extra.keys().filter_map(|k| k.as_str()).collect();
        assert_eq!(extra, ["not_an_option"]);
    }

    /// A save writes each declared key under its own name, in its canonical
    /// spelling, and leaves out the ones at their default.
    #[test]
    fn a_declared_key_is_saved_canonically_and_omitted_at_its_default() {
        let mut render = RenderConfig::default();
        store::ramp_mode(&mut render, RampMode::Interp);
        store::surround_placement(&mut render, SurroundPlacement::Back);
        store::drc_weight(&mut render, 0.123_456_78);
        store::dialogue_gain_db(&mut render, defaults::dialogue_gain_db);
        let yaml = serde_yaml_ng::to_string(&render).unwrap();
        assert!(yaml.contains("ramp_mode: interp"), "{yaml}");
        assert!(yaml.contains("surround_placement: back"), "{yaml}");
        assert!(yaml.contains("drc_weight: 0.123457"), "{yaml}");
        assert!(!yaml.contains("dialogue_gain_db"), "{yaml}");
        let reread: RenderConfig = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(reread.options, render.options);
    }
}
