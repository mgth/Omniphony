//! Pluggable bed→height object generator stage.
//!
//! Turns channel-based ("2D") content that carries no height information into a
//! set of synthesized height *objects* placed above the listener, so an output
//! layout with top speakers (7.1.4, …) is exercised even when the source has
//! none (DTS core, E-AC3, plain 5.1/7.1). It mirrors the render-backend
//! extensibility: contributors add a compiled [`ObjectGeneratorFactory`] to the
//! [`ObjectGeneratorRegistry`]; the realtime DSP runs per frame in
//! [`ObjectGenerator::process`].
//!
//! The stage is OFF by default (live param `object_generator_id` empty/`none`).
//! It is a strict no-op — and costs nothing — when the output layout has no
//! height speaker, or when the input already carries height channels.
//!
//! Synthesized objects flow through the *existing* object path: their audio is
//! appended as extra object channels (so the VBAP mix spatializes them
//! unchanged), and their static descriptors are surfaced over OSC so they appear
//! in the Studio 3D view and object list like any ADM object.

use std::sync::Arc;

use bridge_api::RChannelLabel;
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use renderer::live_params::SurroundPlacement;
use renderer::speaker_layout::SpeakerLayout;

/// What a generator needs from its environment; lets the host gate the UI.
#[derive(Debug, Clone, Copy, Default)]
pub struct ObjectGenCapabilities {
    /// The generator only produces output on a height-capable output layout.
    pub requires_height_layer: bool,
}

/// One live-tunable parameter a generator declares, so the host can build a
/// control for it and the renderer can validate/apply it by key. Static per
/// generator type — the schema, not a value.
#[derive(Debug, Clone, Copy)]
pub struct ObjectGenParamSpec {
    /// Stable key used in the OSC param control and the value map.
    pub key: &'static str,
    /// English fallback label.
    pub label: &'static str,
    /// i18n key for a localized label in Studio (built-ins); empty for
    /// out-of-tree generators (the host then shows `label`).
    pub i18n_key: &'static str,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub default: f32,
    /// Display unit suffix (e.g. `"Hz"`, `"dB"`); empty for a bare number.
    pub unit: &'static str,
}

/// One synthesized object the generator emits. Position/name are static for a
/// given layout+input (planned in [`ObjectGenerator::prepare`]); only the audio
/// content varies per frame.
#[derive(Debug, Clone)]
pub struct SynthObjectSpec {
    /// Display name, surfaced over OSC to Studio (3D view + object list).
    pub name: String,
    /// Target position in ADM Cartesian coordinates `[x, y, z]`, `z > 0`
    /// (height): x ∈ [-1, 1] left/right · y ∈ [-1, 1] back/front · z floor/ceiling.
    pub position: [f64; 3],
    /// Object gain in dB (`-128` = muted).
    pub gain_db: i8,
    /// Object spatial extent `(w, d, h)` ∈ [0, 1]³.
    pub size: [f32; 3],
}

/// Read-only environment for [`ObjectGenerator::prepare`].
pub struct PrepareCtx<'a> {
    /// Input channel labels, in PCM channel order.
    pub input_labels: &'a [RChannelLabel],
    /// The active output speaker layout.
    pub output_layout: &'a SpeakerLayout,
    pub sample_rate: u32,
    /// Where a 4.x/5.x surround pair (`Ls`/`Rs`) sits — Side or Back — so the
    /// synthesized objects track the same choice as the virtual bed.
    pub surround_placement: SurroundPlacement,
}

/// One block of channel-based bed PCM handed to [`ObjectGenerator::process`].
pub struct BedFrame<'a> {
    /// Interleaved input PCM, `channel_count` channels.
    pub pcm: &'a [f32],
    pub channel_count: usize,
    pub sample_count: usize,
    pub sample_rate: u32,
}

/// A compiled bed→height object generator. Stateful, runs in the realtime audio
/// thread. Like a render backend it MUST NOT panic or allocate in
/// [`process`](ObjectGenerator::process); do all setup in
/// [`prepare`](ObjectGenerator::prepare).
pub trait ObjectGenerator: Send {
    fn capabilities(&self) -> ObjectGenCapabilities;
    /// Plan the (static) objects this generator emits for the current
    /// layout + input and (re)allocate internal state. Returns an empty vector
    /// when the generator is a no-op (e.g. no height layer, or the input already
    /// has height). Called once per layout/format change, not per frame.
    fn prepare(&mut self, ctx: &PrepareCtx) -> Vec<SynthObjectSpec>;
    /// Fill `out[k]` (length `bed.sample_count`) with the audio for the k-th
    /// planned object. `out.len()` equals the number of specs returned by the
    /// last `prepare`. Must not allocate or panic.
    fn process(&mut self, bed: &BedFrame, out: &mut [Vec<f32>]);

    /// The live-tunable parameters this generator declares (default: none). The
    /// host builds a control per entry and validates incoming values against it.
    fn param_schema(&self) -> &'static [ObjectGenParamSpec] {
        &[]
    }

    /// Apply one parameter by `key`, in place (no DSP-state reset). Unknown keys
    /// are ignored. Default: no params.
    fn set_param(&mut self, _key: &str, _value: f32, _sample_rate: u32) {}
}

/// Factory for an [`ObjectGenerator`], keyed by a stable string id (mirrors the
/// render-backend `BackendFactory`).
pub trait ObjectGeneratorFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    fn requires_height_layer(&self) -> bool;
    fn build(&self) -> Box<dyn ObjectGenerator>;
    /// i18n key for a localized name in Studio (built-ins); empty otherwise.
    fn i18n_key(&self) -> &'static str {
        ""
    }
    /// The parameter schema this generator exposes (default: none). Available
    /// without building an instance, so the host can publish it to the UI.
    fn param_schema(&self) -> &'static [ObjectGenParamSpec] {
        &[]
    }
}

/// String-keyed registry of generator factories: the shipped built-ins plus any
/// contributor-registered factory.
pub struct ObjectGeneratorRegistry {
    factories: Vec<Box<dyn ObjectGeneratorFactory>>,
}

impl ObjectGeneratorRegistry {
    /// Registry with the shipped built-in generators.
    pub fn with_builtins() -> Self {
        Self {
            factories: vec![
                Box::new(CopyUpFactory),
                Box::new(PadFactory),
                Box::new(DiracFactory),
            ],
        }
    }

    /// Register a contributor factory (out-of-tree generators).
    pub fn register(&mut self, factory: Box<dyn ObjectGeneratorFactory>) {
        self.factories.push(factory);
    }

    /// Build the generator for `id`, or `None` for the off sentinel
    /// (`""` / `"none"`) or an unknown id.
    pub fn build(&self, id: &str) -> Option<Box<dyn ObjectGenerator>> {
        let id = id.trim();
        if id.is_empty() || id.eq_ignore_ascii_case("none") {
            return None;
        }
        self.factories
            .iter()
            .find(|f| f.id().eq_ignore_ascii_case(id))
            .map(|f| f.build())
    }

    /// `(id, label, requires_height_layer)` for each registered generator, for
    /// state publication to the UI.
    pub fn list(&self) -> Vec<(&'static str, &'static str, bool)> {
        self.factories
            .iter()
            .map(|f| (f.id(), f.label(), f.requires_height_layer()))
            .collect()
    }

    /// JSON array of `{id,label,i18nKey,requiresHeightLayer,params:[…]}` for each
    /// registered generator, for the Studio UI to build the selector + the
    /// per-generator parameter sliders.
    pub fn listings_json(&self) -> String {
        let arr: Vec<serde_json::Value> = self
            .factories
            .iter()
            .map(|f| {
                let params: Vec<serde_json::Value> = f
                    .param_schema()
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "key": p.key,
                            "label": p.label,
                            "i18nKey": p.i18n_key,
                            "min": p.min,
                            "max": p.max,
                            "step": p.step,
                            "default": p.default,
                            "unit": p.unit,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "id": f.id(),
                    "label": f.label(),
                    "i18nKey": f.i18n_key(),
                    "requiresHeightLayer": f.requires_height_layer(),
                    "params": params,
                })
            })
            .collect();
        serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
    }
}

/// JSON listing of the built-in generators (id/label/schema), for the host to
/// publish to Studio over OSC without owning a registry instance.
pub fn builtin_listings_json() -> String {
    ObjectGeneratorRegistry::with_builtins().listings_json()
}

impl Default for ObjectGeneratorRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

// ─────────────────────────────── helpers ───────────────────────────────

const Z_EPS: f32 = 1e-3;

/// True when `layout` has at least one spatializable speaker above ear level.
pub fn layout_has_height(layout: &SpeakerLayout) -> bool {
    layout.speakers.iter().any(|s| s.spatialize && s.z > Z_EPS)
}

/// True when the input channel set already carries a height channel — then any
/// upmix is suppressed (the content is already 3D).
pub fn input_has_height(labels: &[RChannelLabel]) -> bool {
    labels.iter().any(|l| {
        matches!(
            l,
            RChannelLabel::Tfl
                | RChannelLabel::Tfr
                | RChannelLabel::Tsl
                | RChannelLabel::Tsr
                | RChannelLabel::Tbl
                | RChannelLabel::Tbr
                | RChannelLabel::Tc
                | RChannelLabel::Tfc
        )
    })
}

pub(crate) fn find_channel(labels: &[RChannelLabel], want: RChannelLabel) -> Option<usize> {
    labels.iter().position(|&l| l == want)
}

/// One-pole smoothing coefficient from a time constant: `α = 1 − exp(−1/(τ·fs))`,
/// with `τ = tc_ms` (ms). Shared by the inter-channel statistics smoothers (PAD,
/// phantom extraction). Idiom shared with `audio_output::iir::step_one_pole`.
pub(crate) fn one_pole_coeff(tc_ms: f32, fs: f32) -> f32 {
    let tau_samples = (tc_ms * 1.0e-3 * fs).max(1.0);
    1.0 - (-1.0 / tau_samples).exp()
}

/// Canonical top position of a bed channel: its floor position raised to the
/// height layer (`z = 1`). Mirrors the engine's channel→position convention
/// (`renderer/src/virtual_bed.rs`): x = right, y = front. `None` for channels
/// that should never be lifted (LFE) or carry no position (unknown).
pub(crate) fn top_position(label: RChannelLabel) -> Option<[f64; 3]> {
    use RChannelLabel::*;
    let pos = match label {
        L => [-1.0, 1.0, 1.0],
        R => [1.0, 1.0, 1.0],
        C => [0.0, 1.0, 1.0],
        Ls => [-1.0, 0.0, 1.0],
        Rs => [1.0, 0.0, 1.0],
        Lb => [-1.0, -1.0, 1.0],
        Rb => [1.0, -1.0, 1.0],
        Cb => [0.0, -1.0, 1.0],
        _ => return None,
    };
    Some(pos)
}

/// True when the input carries dedicated back-surround channels (7.x); then the
/// side surrounds are unambiguous and `surround_placement` does not apply.
pub(crate) fn input_has_back(labels: &[RChannelLabel]) -> bool {
    labels
        .iter()
        .any(|l| matches!(l, RChannelLabel::Lb | RChannelLabel::Rb | RChannelLabel::Cb))
}

/// Canonical top position of a bed channel, with a 4.x/5.x surround pair
/// (`Ls`/`Rs`) moved to the side or back per `surround_placement` — matching the
/// virtual bed — so synthesized objects track the same Side/Back choice.
pub(crate) fn channel_top_position(
    label: RChannelLabel,
    use_7_1: bool,
    placement: SurroundPlacement,
) -> Option<[f64; 3]> {
    let mut pos = top_position(label)?;
    if let Some((x, y, _)) =
        crate::virtual_bed::surround_placement_override(label, use_7_1, placement)
    {
        pos[0] = x as f64;
        pos[1] = y as f64;
    }
    Some(pos)
}

// ───────────────────────── built-in: copy_up ─────────────────────────

/// Simple reference generator: routes every spatializable floor channel straight
/// up to its matching top position (front, sides, back, center) with no
/// decorrelation. A basic, low-cost "lift" — also useful as the minimal example
/// for contributors. The PAD generator below adds ambience extraction for a more
/// natural result.
struct CopyUpFactory;

impl ObjectGeneratorFactory for CopyUpFactory {
    fn id(&self) -> &'static str {
        "copy_up"
    }
    fn label(&self) -> &'static str {
        "Direct copy to tops"
    }
    fn requires_height_layer(&self) -> bool {
        true
    }
    fn build(&self) -> Box<dyn ObjectGenerator> {
        Box::<CopyUpGenerator>::default()
    }
    fn i18n_key(&self) -> &'static str {
        "twoDSources.objectGenCopyUp"
    }
}

#[derive(Default)]
struct CopyUpGenerator {
    /// Source input channel index for each planned object (parallel to the specs
    /// returned by `prepare`).
    src_channels: Vec<usize>,
}

impl ObjectGenerator for CopyUpGenerator {
    fn capabilities(&self) -> ObjectGenCapabilities {
        ObjectGenCapabilities {
            requires_height_layer: true,
        }
    }

    fn prepare(&mut self, ctx: &PrepareCtx) -> Vec<SynthObjectSpec> {
        self.src_channels.clear();
        if !layout_has_height(ctx.output_layout) || input_has_height(ctx.input_labels) {
            return Vec::new();
        }
        const GAIN_DB: i8 = -6;
        const SIZE: [f32; 3] = [0.3, 0.3, 0.3];
        // Lift every spatializable bed channel (front, sides, back, center) to its
        // canonical top position; LFE / unknown channels have no `top_position`.
        let use_7_1 = input_has_back(ctx.input_labels);
        let mut specs = Vec::new();
        for (ch, &label) in ctx.input_labels.iter().enumerate() {
            let Some(position) = channel_top_position(label, use_7_1, ctx.surround_placement)
            else {
                continue;
            };
            self.src_channels.push(ch);
            specs.push(SynthObjectSpec {
                name: format!("Height_{label:?}_synth"),
                position,
                gain_db: GAIN_DB,
                size: SIZE,
            });
        }
        specs
    }

    fn process(&mut self, bed: &BedFrame, out: &mut [Vec<f32>]) {
        let c = bed.channel_count;
        for (k, &src) in self.src_channels.iter().enumerate() {
            if k >= out.len() || src >= c {
                continue;
            }
            let dst = &mut out[k];
            for s in 0..bed.sample_count {
                dst[s] = bed.pcm[s * c + src];
            }
        }
    }
}

// ─────────────────────────── built-in: pad ───────────────────────────

/// Minimal transposed-direct-form-II biquad, used by PAD to keep low frequencies
/// out of the height layer (a mild psychoacoustic "elevation" lean: bass stays
/// grounded, mids/highs rise).
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// RBJ high-pass at cutoff `fc` (Hz), quality `q`, sample rate `fs` (Hz).
    fn highpass(fs: f32, fc: f32, q: f32) -> Self {
        let mut b = Self {
            b0: 0.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        };
        b.set_highpass(fs, fc, q);
        b
    }

    /// Recompute the high-pass coefficients in place, preserving the filter state
    /// (`z1`/`z2`) so a live cutoff change does not click.
    fn set_highpass(&mut self, fs: f32, fc: f32, q: f32) {
        let w0 = std::f32::consts::TAU * (fc / fs).clamp(1.0e-4, 0.49);
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        self.b0 = ((1.0 + cos) / 2.0) / a0;
        self.b1 = (-(1.0 + cos)) / a0;
        self.b2 = ((1.0 + cos) / 2.0) / a0;
        self.a1 = (-2.0 * cos) / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Primary-Ambient Decomposition generator: extracts the decorrelated *ambient*
/// component of each floor pair (front L/R, surround L/R) with a 1-tap adaptive
/// canceller (NLMS) and lifts it to the matching top corners. Correlated /
/// panned "primary" content (dialogue, hard-panned sources) is cancelled and
/// stays grounded; diffuse / reverberant content rises. Additive: the floor mix
/// is left untouched.
struct PadFactory;

impl ObjectGeneratorFactory for PadFactory {
    fn id(&self) -> &'static str {
        "pad"
    }
    fn label(&self) -> &'static str {
        "Ambience to height (PAD)"
    }
    fn requires_height_layer(&self) -> bool {
        true
    }
    fn build(&self) -> Box<dyn ObjectGenerator> {
        Box::<PadGenerator>::default()
    }
    fn i18n_key(&self) -> &'static str {
        "twoDSources.objectGenPad"
    }
    fn param_schema(&self) -> &'static [ObjectGenParamSpec] {
        &PAD_PARAM_SPECS
    }
}

/// High-pass cutoff for the height layer (Hz): keep bass grounded.
const PAD_HPF_HZ: f32 = 300.0;
/// High-pass quality (Butterworth).
const PAD_HPF_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;
/// Default isolation depth (0..1): how much of the predicted primary is removed.
const PAD_DEFAULT_STRENGTH: f32 = 0.5;
/// Time constant (ms) of the one-pole smoothers for the inter-channel statistics
/// (auto/cross power). Long enough (tens of ms) that the derived Wiener weight is
/// quasi-stationary, so the residual gain does not jitter per sample — this is
/// what avoids the amplitude-modulation ("wind / mic saturation") artifact.
const PAD_STAT_TC_MS: f32 = 50.0;
/// Relative regularisation on the Wiener denominator (fraction of the pair's mean
/// power) so quiet passages and zero-crossings cannot blow the weight up.
const PAD_REG: f32 = 1.0e-2;
/// Absolute floor added to the denominator to keep it strictly positive in pure
/// silence (no NaN reaches the audio thread).
const PAD_FLOOR: f32 = 1.0e-9;
/// Clamp on the prediction weight: a correlated stereo weight is ≤ 1; the small
/// headroom prevents the residual from amplifying the partner channel.
const PAD_W_CLAMP: f32 = 1.2;
/// Object gain for the lifted ambience (dB). The ambient component is already
/// lower energy than the primary, so unity keeps it natural.
const PAD_GAIN_DB: i8 = 0;
/// Center → height defaults. The center has no stereo partner to decorrelate
/// against, so it is lifted as a high-passed, scaled send: a high cutoff lifts
/// only the "air" / reverb and keeps dialogue grounded.
const PAD_CENTER_DEFAULT_AMOUNT: f32 = 0.4;
const PAD_CENTER_DEFAULT_HPF_HZ: f32 = 3000.0;
const PAD_CENTER_HPF_MIN: f32 = 300.0;
const PAD_CENTER_HPF_MAX: f32 = 8000.0;

/// Live-tunable parameters PAD declares (the schema the UI builds sliders from).
const PAD_PARAM_SPECS: [ObjectGenParamSpec; 5] = [
    ObjectGenParamSpec {
        key: "strength",
        label: "Ambience strength",
        i18n_key: "twoDSources.padStrength",
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: PAD_DEFAULT_STRENGTH,
        unit: "",
    },
    ObjectGenParamSpec {
        key: "hpf_hz",
        label: "Bass cutoff",
        i18n_key: "twoDSources.padHpf",
        min: 20.0,
        max: 2000.0,
        step: 10.0,
        default: PAD_HPF_HZ,
        unit: "Hz",
    },
    ObjectGenParamSpec {
        key: "gain_db",
        label: "Height level",
        i18n_key: "twoDSources.padGain",
        min: -24.0,
        max: 24.0,
        step: 0.5,
        default: 0.0,
        unit: "dB",
    },
    ObjectGenParamSpec {
        key: "center_amount",
        label: "Center to height",
        i18n_key: "twoDSources.padCenterAmount",
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: PAD_CENTER_DEFAULT_AMOUNT,
        unit: "",
    },
    ObjectGenParamSpec {
        key: "center_hpf_hz",
        label: "Center bass cutoff",
        i18n_key: "twoDSources.padCenterHpf",
        min: PAD_CENTER_HPF_MIN,
        max: PAD_CENTER_HPF_MAX,
        step: 50.0,
        default: PAD_CENTER_DEFAULT_HPF_HZ,
        unit: "Hz",
    },
];

/// One floor pair (L, R) → two height objects (ambient of L, ambient of R), with
/// the slowly-smoothed inter-channel statistics and high-pass state that must
/// persist across frames.
struct AmbientPair {
    l_ch: usize,
    r_ch: usize,
    out_l: usize,
    out_r: usize,
    /// One-pole smoothed auto-power of each channel (`E[L²]`, `E[R²]`).
    pow_l: f32,
    pow_r: f32,
    /// One-pole smoothed inter-channel cross-power (`E[L·R]`). Symmetric, so it
    /// feeds both prediction weights.
    cross: f32,
    hpf_l: Biquad,
    hpf_r: Biquad,
}

/// The center channel lifted to a single top-center object. No stereo partner,
/// so no decorrelation: a high-passed, level-scaled send (dialogue-safe when the
/// cutoff is high).
struct CenterChannel {
    ch: usize,
    out: usize,
    hpf: Biquad,
}

struct PadGenerator {
    pairs: Vec<AmbientPair>,
    /// Center → top-center send (present only when the input carries a C channel).
    center: Option<CenterChannel>,
    /// Isolation depth (0..1): fraction of the predicted primary that is removed
    /// from each channel. `0` = clean copy of the floor up top, `1` = pure diffuse
    /// residual. Live-tunable via the `strength` param.
    strength: f32,
    /// Linear makeup gain applied to the lifted ambience.
    makeup: f32,
    /// One-pole smoothing coefficient for the statistics, derived from the sample
    /// rate in `prepare` (`α = 1 − exp(−1/(τ·fs))`).
    alpha: f32,
    /// Center send level (0..1) and high-pass cutoff (Hz); live-tunable.
    center_amount: f32,
    center_hpf_hz: f32,
}

impl Default for PadGenerator {
    fn default() -> Self {
        Self {
            pairs: Vec::new(),
            center: None,
            strength: PAD_DEFAULT_STRENGTH,
            makeup: 1.0,
            // Replaced in `prepare` once the sample rate is known; a safe non-zero
            // default keeps the smoother stable if `process` ever runs first.
            alpha: 0.0,
            center_amount: PAD_CENTER_DEFAULT_AMOUNT,
            center_hpf_hz: PAD_CENTER_DEFAULT_HPF_HZ,
        }
    }
}

impl ObjectGenerator for PadGenerator {
    fn capabilities(&self) -> ObjectGenCapabilities {
        ObjectGenCapabilities {
            requires_height_layer: true,
        }
    }

    fn prepare(&mut self, ctx: &PrepareCtx) -> Vec<SynthObjectSpec> {
        self.pairs.clear();
        self.center = None;
        if !layout_has_height(ctx.output_layout) || input_has_height(ctx.input_labels) {
            return Vec::new();
        }
        let fs = ctx.sample_rate.max(1) as f32;
        // One-pole smoothing coefficient for the statistics (τ = PAD_STAT_TC_MS).
        self.alpha = one_pole_coeff(PAD_STAT_TC_MS, fs);
        let labels = ctx.input_labels;
        let use_7_1 = input_has_back(labels);
        let hpf = Biquad::highpass(fs, PAD_HPF_HZ, PAD_HPF_Q);
        const SIZE: [f32; 3] = [0.5, 0.5, 0.5];

        // One decorrelated pair per top row, each lifted to its canonical position:
        // front (L/R) → top-front, sides (Ls/Rs) → top-side, back (Lb/Rb) → top-back.
        // A pair is emitted only when both its channels exist (5.1 → front+side,
        // 7.1 → front+side+back).
        let pair_defs: [(&str, &str, RChannelLabel, RChannelLabel); 3] = [
            (
                "Ambience_FL",
                "Ambience_FR",
                RChannelLabel::L,
                RChannelLabel::R,
            ),
            (
                "Ambience_SL",
                "Ambience_SR",
                RChannelLabel::Ls,
                RChannelLabel::Rs,
            ),
            (
                "Ambience_BL",
                "Ambience_BR",
                RChannelLabel::Lb,
                RChannelLabel::Rb,
            ),
        ];

        let mut specs = Vec::new();
        for (name_l, name_r, label_l, label_r) in pair_defs {
            let (Some(l_ch), Some(r_ch)) =
                (find_channel(labels, label_l), find_channel(labels, label_r))
            else {
                continue;
            };
            let (Some(pos_l), Some(pos_r)) = (
                channel_top_position(label_l, use_7_1, ctx.surround_placement),
                channel_top_position(label_r, use_7_1, ctx.surround_placement),
            ) else {
                continue;
            };
            let out_l = specs.len();
            let out_r = out_l + 1;
            self.pairs.push(AmbientPair {
                l_ch,
                r_ch,
                out_l,
                out_r,
                pow_l: 0.0,
                pow_r: 0.0,
                cross: 0.0,
                hpf_l: hpf,
                hpf_r: hpf,
            });
            specs.push(SynthObjectSpec {
                name: name_l.to_string(),
                position: pos_l,
                gain_db: PAD_GAIN_DB,
                size: SIZE,
            });
            specs.push(SynthObjectSpec {
                name: name_r.to_string(),
                position: pos_r,
                gain_db: PAD_GAIN_DB,
                size: SIZE,
            });
        }

        // Center → a single top-center object (no stereo partner; a high-passed
        // send whose level is `center_amount` and cutoff `center_hpf_hz`). Planned
        // whenever C exists so it shows in the 3D view; silent at amount 0.
        if let (Some(ch), Some(position)) = (
            find_channel(labels, RChannelLabel::C),
            channel_top_position(RChannelLabel::C, use_7_1, ctx.surround_placement),
        ) {
            let out = specs.len();
            self.center = Some(CenterChannel {
                ch,
                out,
                hpf: Biquad::highpass(fs, self.center_hpf_hz, PAD_HPF_Q),
            });
            specs.push(SynthObjectSpec {
                name: "Ambience_TC".to_string(),
                position,
                gain_db: PAD_GAIN_DB,
                size: SIZE,
            });
        }
        specs
    }

    fn process(&mut self, bed: &BedFrame, out: &mut [Vec<f32>]) {
        let c = bed.channel_count;
        let strength = self.strength;
        let makeup = self.makeup;
        let alpha = self.alpha;
        let center_gain = self.center_amount * makeup;
        for pair in self.pairs.iter_mut() {
            if pair.l_ch >= c
                || pair.r_ch >= c
                || pair.out_l >= out.len()
                || pair.out_r >= out.len()
            {
                continue;
            }
            for s in 0..bed.sample_count {
                let l = bed.pcm[s * c + pair.l_ch];
                let r = bed.pcm[s * c + pair.r_ch];

                // Slowly-smoothed inter-channel statistics (~PAD_STAT_TC_MS). Long
                // enough that the Wiener weight below is quasi-stationary, so the
                // residual gain does not jitter per sample (no AM / "wind").
                pair.pow_l += alpha * (l * l - pair.pow_l);
                pair.pow_r += alpha * (r * r - pair.pow_r);
                pair.cross += alpha * (l * r - pair.cross);

                // Relative regularisation: a fraction of the pair's mean power plus
                // a tiny absolute floor — quiet passages and zero-crossings can no
                // longer explode the weight.
                let reg = PAD_REG * 0.5 * (pair.pow_l + pair.pow_r) + PAD_FLOOR;
                let w_lr = (pair.cross / (pair.pow_r + reg)).clamp(-PAD_W_CLAMP, PAD_W_CLAMP);
                let w_rl = (pair.cross / (pair.pow_l + reg)).clamp(-PAD_W_CLAMP, PAD_W_CLAMP);

                // Ambient = each channel minus the `strength`-scaled part that is
                // predictable from its partner. strength=0 → clean copy up;
                // strength=1 → only the decorrelated (diffuse) residual rises.
                let amb_l = l - strength * w_lr * r;
                let amb_r = r - strength * w_rl * l;

                out[pair.out_l][s] = pair.hpf_l.process(amb_l) * makeup;
                out[pair.out_r][s] = pair.hpf_r.process(amb_r) * makeup;
            }
        }
        // Center → top-center: a plain high-passed, level-scaled send (no partner
        // to decorrelate against). A high cutoff keeps dialogue grounded.
        if let Some(cc) = self.center.as_mut() {
            if cc.ch < c && cc.out < out.len() {
                for s in 0..bed.sample_count {
                    let x = bed.pcm[s * c + cc.ch];
                    out[cc.out][s] = cc.hpf.process(x) * center_gain;
                }
            }
        }
    }

    fn param_schema(&self) -> &'static [ObjectGenParamSpec] {
        &PAD_PARAM_SPECS
    }

    fn set_param(&mut self, key: &str, value: f32, sample_rate: u32) {
        match key {
            "strength" => self.strength = value.clamp(0.0, 1.0),
            "gain_db" => self.makeup = 10.0_f32.powf(value.clamp(-24.0, 24.0) / 20.0),
            "hpf_hz" => {
                let fs = sample_rate.max(1) as f32;
                let fc = value.clamp(20.0, 2000.0);
                for pair in self.pairs.iter_mut() {
                    pair.hpf_l.set_highpass(fs, fc, PAD_HPF_Q);
                    pair.hpf_r.set_highpass(fs, fc, PAD_HPF_Q);
                }
            }
            "center_amount" => self.center_amount = value.clamp(0.0, 1.0),
            "center_hpf_hz" => {
                let fs = sample_rate.max(1) as f32;
                let fc = value.clamp(PAD_CENTER_HPF_MIN, PAD_CENTER_HPF_MAX);
                self.center_hpf_hz = fc;
                if let Some(cc) = self.center.as_mut() {
                    cc.hpf.set_highpass(fs, fc, PAD_HPF_Q);
                }
            }
            _ => {}
        }
    }
}

// ───────────────────────── built-in: dirac ─────────────────────────

/// DirAC (Directional Audio Coding) diffuse→height generator: a frequency-selective
/// successor to PAD. The channel bed is encoded to a virtual horizontal B-format
/// (W, X, Y), analysed by a short-time Fourier transform, and per frequency bin a
/// **diffuseness** ψ ∈ [0,1] is estimated from the (time-smoothed) active intensity
/// vs the energy. The diffuse part (ψ·field) is reconstructed and lifted to four
/// ceiling objects through per-object decorrelators, so reverberant / enveloping
/// content rises while panned / dialogue (ψ→0) stays grounded — and, unlike PAD, the
/// split is made independently per band. Additive: the floor mix is left untouched.
struct DiracFactory;

impl ObjectGeneratorFactory for DiracFactory {
    fn id(&self) -> &'static str {
        "dirac"
    }
    fn label(&self) -> &'static str {
        "Diffuse field to height (DirAC)"
    }
    fn requires_height_layer(&self) -> bool {
        true
    }
    fn build(&self) -> Box<dyn ObjectGenerator> {
        Box::<DiracGenerator>::default()
    }
    fn i18n_key(&self) -> &'static str {
        "twoDSources.objectGenDirac"
    }
    fn param_schema(&self) -> &'static [ObjectGenParamSpec] {
        &DIRAC_PARAM_SPECS
    }
}

/// STFT size and 50% hop. Fixed in v1 (≈21 ms frame at 48 kHz — a good time/freq
/// trade-off for diffuseness; latency is one frame, fine for a diffuse layer).
const DIRAC_FFT_SIZE: usize = 1024;
const DIRAC_HOP: usize = DIRAC_FFT_SIZE / 2;
/// Time constant (ms) of the per-bin one-pole smoothers (intensity + energy). ψ is
/// a ratio of *expectations*, so we smooth I and e and form ψ from the smoothed
/// values — long enough that the lifted level does not jitter on tonal material.
const DIRAC_STAT_TC_MS: f32 = 80.0;
/// Relative regularisation on the energy denominator (fraction of the frame's mean
/// bin energy) plus a tiny absolute floor, so silence reads ψ→0 (no noise-floor lift).
const DIRAC_REG: f32 = 1.0e-2;
const DIRAC_FLOOR: f32 = 1.0e-12;
const DIRAC_GAIN_DB: i8 = 0;
const DIRAC_DEFAULT_AMOUNT: f32 = 0.5;
const DIRAC_DEFAULT_BIAS: f32 = 0.0;
const DIRAC_DEFAULT_HPF_HZ: f32 = 500.0;
const DIRAC_HPF_MIN: f32 = 300.0;
const DIRAC_HPF_MAX: f32 = 8000.0;
/// Largest input block we size the output FIFO for (plus one FFT frame of priming).
const DIRAC_MAX_BLOCK: usize = 4096;
/// Per-ceiling-object Schroeder all-pass decorrelator delays (two coprime stages
/// each): flat magnitude (no comb colouration) yet decorrelated across the full band,
/// so the four ceiling feeds do not collapse to an overhead phantom centre.
const DIRAC_AP_DELAYS: [[usize; 2]; 4] = [[37, 211], [53, 229], [67, 251], [79, 271]];
const DIRAC_AP_G: f32 = 0.6;
/// Diffuse objects are spatially large (enveloping), unlike the point-like phantoms.
const DIRAC_SIZE: [f32; 3] = [0.6, 0.6, 0.6];

/// Live-tunable parameters DirAC declares (the schema the UI builds sliders from).
const DIRAC_PARAM_SPECS: [ObjectGenParamSpec; 3] = [
    ObjectGenParamSpec {
        key: "amount",
        label: "Diffuse level",
        i18n_key: "twoDSources.diracAmount",
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: DIRAC_DEFAULT_AMOUNT,
        unit: "",
    },
    ObjectGenParamSpec {
        key: "diffuse_bias",
        label: "Diffuse bias",
        i18n_key: "twoDSources.diracBias",
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: DIRAC_DEFAULT_BIAS,
        unit: "",
    },
    ObjectGenParamSpec {
        key: "hpf_hz",
        label: "Bass cutoff",
        i18n_key: "twoDSources.diracHpf",
        min: DIRAC_HPF_MIN,
        max: DIRAC_HPF_MAX,
        step: 50.0,
        default: DIRAC_DEFAULT_HPF_HZ,
        unit: "Hz",
    },
];

/// Flush sub-denormal magnitudes to zero — DirAC has feedback (all-pass) and long
/// one-pole memory, so on silence its state decays into the denormal range and
/// stalls the FPU. Cheap branch, applied to the smoothers and the all-pass storage.
#[inline]
pub(crate) fn flush_denorm(x: f32) -> f32 {
    if x.abs() < 1.0e-20 { 0.0 } else { x }
}

/// Build the raised-cosine high-pass bin mask (0 below `fc`, ~⅓-octave transition up
/// to `fc`, 1 above). A smooth ramp — a brick-wall bin cut would ring (pre-echo).
fn build_hpf_mask(mask: &mut [f32], fs: f32, fc: f32) {
    let bin_hz = fs / DIRAC_FFT_SIZE as f32;
    let f_lo = fc / 2.0_f32.powf(1.0 / 3.0); // ⅓ octave below fc
    let f_hi = fc.max(f_lo + bin_hz);
    for (b, m) in mask.iter_mut().enumerate() {
        let f = b as f32 * bin_hz;
        *m = if f <= f_lo {
            0.0
        } else if f >= f_hi {
            1.0
        } else {
            let t = (f - f_lo) / (f_hi - f_lo);
            0.5 - 0.5 * (std::f32::consts::PI * t).cos()
        };
    }
}

/// One Schroeder all-pass stage (unity magnitude, phase scrambled), used to
/// decorrelate the mono diffuse stream across the ceiling objects.
#[derive(Clone)]
struct AllPass {
    buf: Vec<f32>,
    idx: usize,
    g: f32,
}

impl AllPass {
    fn new(delay: usize, g: f32) -> Self {
        Self {
            buf: vec![0.0; delay.max(1)],
            idx: 0,
            g,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let buffered = self.buf[self.idx];
        let v = x + self.g * buffered;
        let y = buffered - self.g * v;
        self.buf[self.idx] = flush_denorm(v);
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        y
    }
}

struct DiracGenerator {
    /// Number of planned ceiling objects (`0` = not planned / no-op).
    objects: usize,
    /// `(input channel index, cos(az), sin(az))` per positionable bed channel, for
    /// the per-sample virtual B-format encode (LFE / unpositioned excluded).
    enc: Vec<(usize, f32, f32)>,
    fft_fwd: Arc<dyn RealToComplex<f32>>,
    fft_inv: Arc<dyn ComplexToReal<f32>>,
    /// Periodic (DFT-even) Hann, applied on both analysis and synthesis.
    win: Vec<f32>,
    /// Ring frame buffers for the time-domain W, X, Y (length N), `widx` = write pos.
    frame_w: Vec<f32>,
    frame_x: Vec<f32>,
    frame_y: Vec<f32>,
    widx: usize,
    /// Samples accumulated toward the next hop (0..H).
    fill: usize,
    /// realfft scratch (allocated once; `process_with_scratch` is the zero-alloc path).
    real_buf: Vec<f32>,
    spec_w: Vec<Complex<f32>>,
    spec_x: Vec<Complex<f32>>,
    spec_y: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    ifft_scratch: Vec<Complex<f32>>,
    real_out: Vec<f32>,
    /// Per-bin smoothed active intensity (i_x, i_y) and energy (e) → ψ.
    i_x: Vec<f32>,
    i_y: Vec<f32>,
    e: Vec<f32>,
    /// Raised-cosine high-pass bin mask (rebuilt on `hpf_hz` change).
    hpf_mask: Vec<f32>,
    /// Synthesis overlap-add accumulator (length N).
    ola: Vec<f32>,
    /// Output FIFO ring (primed with N zeros for fixed latency).
    out_fifo: Vec<f32>,
    fifo_read: usize,
    fifo_write: usize,
    fifo_len: usize,
    /// Per-ceiling-object decorrelators (two all-pass stages each).
    allpass: Vec<[AllPass; 2]>,
    /// Per-hop one-pole coefficient (derived in `prepare`).
    alpha: f32,
    amount: f32,
    diffuse_bias: f32,
    hpf_hz: f32,
    sample_rate: u32,
    /// Test hook: force ψ≡1 to verify the WOLA reconstruction gain in isolation.
    #[cfg(test)]
    force_diffuse: bool,
}

impl Default for DiracGenerator {
    fn default() -> Self {
        // The FFT size is fixed, so plan once here and reuse across `prepare` calls.
        let mut planner = RealFftPlanner::<f32>::new();
        let fft_fwd = planner.plan_fft_forward(DIRAC_FFT_SIZE);
        let fft_inv = planner.plan_fft_inverse(DIRAC_FFT_SIZE);
        Self {
            objects: 0,
            enc: Vec::new(),
            fft_fwd,
            fft_inv,
            win: Vec::new(),
            frame_w: Vec::new(),
            frame_x: Vec::new(),
            frame_y: Vec::new(),
            widx: 0,
            fill: 0,
            real_buf: Vec::new(),
            spec_w: Vec::new(),
            spec_x: Vec::new(),
            spec_y: Vec::new(),
            fft_scratch: Vec::new(),
            ifft_scratch: Vec::new(),
            real_out: Vec::new(),
            i_x: Vec::new(),
            i_y: Vec::new(),
            e: Vec::new(),
            hpf_mask: Vec::new(),
            ola: Vec::new(),
            out_fifo: Vec::new(),
            fifo_read: 0,
            fifo_write: 0,
            fifo_len: 0,
            allpass: Vec::new(),
            alpha: 0.0,
            amount: DIRAC_DEFAULT_AMOUNT,
            diffuse_bias: DIRAC_DEFAULT_BIAS,
            hpf_hz: DIRAC_DEFAULT_HPF_HZ,
            sample_rate: 48_000,
            #[cfg(test)]
            force_diffuse: false,
        }
    }
}

impl DiracGenerator {
    #[inline]
    fn push_fifo(&mut self, v: f32) {
        if self.fifo_len < self.out_fifo.len() {
            self.out_fifo[self.fifo_write] = v;
            self.fifo_write = (self.fifo_write + 1) % self.out_fifo.len();
            self.fifo_len += 1;
        }
    }

    #[inline]
    fn pop_fifo(&mut self) -> f32 {
        if self.fifo_len == 0 {
            return 0.0;
        }
        let v = self.out_fifo[self.fifo_read];
        self.fifo_read = (self.fifo_read + 1) % self.out_fifo.len();
        self.fifo_len -= 1;
        v
    }

    /// Run one STFT frame: analyse W/X/Y, update the per-bin diffuseness, reconstruct
    /// the diffuse stream, and overlap-add H finished output samples into the FIFO.
    fn run_frame(&mut self) {
        let n = DIRAC_FFT_SIZE;
        let nb = self.spec_w.len();

        // Assemble the windowed frames (oldest→newest from the ring) and transform.
        for i in 0..n {
            self.real_buf[i] = self.win[i] * self.frame_w[(self.widx + i) % n];
        }
        let _ = self.fft_fwd.process_with_scratch(
            &mut self.real_buf,
            &mut self.spec_w,
            &mut self.fft_scratch,
        );
        for i in 0..n {
            self.real_buf[i] = self.win[i] * self.frame_x[(self.widx + i) % n];
        }
        let _ = self.fft_fwd.process_with_scratch(
            &mut self.real_buf,
            &mut self.spec_x,
            &mut self.fft_scratch,
        );
        for i in 0..n {
            self.real_buf[i] = self.win[i] * self.frame_y[(self.widx + i) % n];
        }
        let _ = self.fft_fwd.process_with_scratch(
            &mut self.real_buf,
            &mut self.spec_y,
            &mut self.fft_scratch,
        );

        // Relative regularisation from the broadband (pressure) energy of this frame.
        let mut e_sum = 0.0f32;
        for b in 0..nb {
            e_sum += self.spec_w[b].norm_sqr();
        }
        let reg = DIRAC_REG * (e_sum / nb.max(1) as f32) + DIRAC_FLOOR;

        let alpha = self.alpha;
        let bias = self.diffuse_bias;
        let inv_1mb = 1.0 / (1.0 - bias).max(1.0e-3);
        #[cfg(test)]
        let force = self.force_diffuse;
        #[cfg(not(test))]
        let force = false;

        for b in 0..nb {
            let w = self.spec_w[b];
            let x = self.spec_x[b];
            let y = self.spec_y[b];
            // Active intensity components: Re(conj(W)·X), Re(conj(W)·Y); pressure
            // energy |W|² (self-normalising for a channel-derived virtual B-format).
            let ix = w.re * x.re + w.im * x.im;
            let iy = w.re * y.re + w.im * y.im;
            let ew = w.norm_sqr();
            self.i_x[b] = flush_denorm(self.i_x[b] + alpha * (ix - self.i_x[b]));
            self.i_y[b] = flush_denorm(self.i_y[b] + alpha * (iy - self.i_y[b]));
            self.e[b] = flush_denorm(self.e[b] + alpha * (ew - self.e[b]));

            let psi = if force {
                1.0
            } else {
                let imag = (self.i_x[b] * self.i_x[b] + self.i_y[b] * self.i_y[b]).sqrt();
                (1.0 - imag / (self.e[b] + reg)).clamp(0.0, 1.0)
            };
            let shaped = ((psi - bias) * inv_1mb).clamp(0.0, 1.0);
            let g = shaped.sqrt() * self.hpf_mask[b];
            self.spec_w[b] = w * g;
        }
        // The DC and Nyquist bins must be purely real for the inverse real FFT.
        self.spec_w[0].im = 0.0;
        if nb > 1 {
            self.spec_w[nb - 1].im = 0.0;
        }

        let _ = self.fft_inv.process_with_scratch(
            &mut self.spec_w,
            &mut self.real_out,
            &mut self.ifft_scratch,
        );

        // WOLA: synthesis window + (1/N)·(4/3) [unnormalised iFFT × Hann² COLA at 50%].
        let scale = (1.0 / n as f32) * (4.0 / 3.0);
        for i in 0..n {
            self.ola[i] += scale * self.win[i] * self.real_out[i];
        }
        for i in 0..DIRAC_HOP {
            self.push_fifo(self.ola[i]);
        }
        self.ola.copy_within(DIRAC_HOP..n, 0);
        for v in self.ola[n - DIRAC_HOP..n].iter_mut() {
            *v = 0.0;
        }
    }
}

impl ObjectGenerator for DiracGenerator {
    fn capabilities(&self) -> ObjectGenCapabilities {
        ObjectGenCapabilities {
            requires_height_layer: true,
        }
    }

    fn prepare(&mut self, ctx: &PrepareCtx) -> Vec<SynthObjectSpec> {
        self.objects = 0;
        self.enc.clear();
        if !layout_has_height(ctx.output_layout) || input_has_height(ctx.input_labels) {
            return Vec::new();
        }
        let fs = ctx.sample_rate.max(1) as f32;
        self.sample_rate = ctx.sample_rate.max(1);
        // Per-hop smoothing coefficient (the smoother steps once per hop, not sample).
        self.alpha = one_pole_coeff(DIRAC_STAT_TC_MS, fs / DIRAC_HOP as f32);

        // Virtual horizontal B-format encoder: az = atan2(x, y) → cos = y/r (front),
        // sin = x/r (right); equivalent to the engine convention, no trig per sample.
        let use_7_1 = input_has_back(ctx.input_labels);
        for (idx, &label) in ctx.input_labels.iter().enumerate() {
            if let Some(pos) = channel_top_position(label, use_7_1, ctx.surround_placement) {
                let (x, y) = (pos[0] as f32, pos[1] as f32);
                let r = (x * x + y * y).sqrt();
                let (ca, sa) = if r > 1.0e-6 {
                    (y / r, x / r)
                } else {
                    (0.0, 0.0)
                };
                self.enc.push((idx, ca, sa));
            }
        }
        if self.enc.is_empty() {
            return Vec::new();
        }

        let n = DIRAC_FFT_SIZE;
        let nb = n / 2 + 1;
        self.win = (0..n)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
            .collect();
        self.frame_w = vec![0.0; n];
        self.frame_x = vec![0.0; n];
        self.frame_y = vec![0.0; n];
        self.widx = 0;
        self.fill = 0;
        self.real_buf = self.fft_fwd.make_input_vec();
        self.spec_w = self.fft_fwd.make_output_vec();
        self.spec_x = self.fft_fwd.make_output_vec();
        self.spec_y = self.fft_fwd.make_output_vec();
        self.real_out = self.fft_inv.make_output_vec();
        self.fft_scratch = vec![Complex::new(0.0, 0.0); self.fft_fwd.get_scratch_len()];
        self.ifft_scratch = vec![Complex::new(0.0, 0.0); self.fft_inv.get_scratch_len()];
        self.i_x = vec![0.0; nb];
        self.i_y = vec![0.0; nb];
        self.e = vec![0.0; nb];
        self.hpf_mask = vec![0.0; nb];
        build_hpf_mask(&mut self.hpf_mask, fs, self.hpf_hz);
        self.ola = vec![0.0; n];
        self.out_fifo = vec![0.0; DIRAC_MAX_BLOCK + n];
        self.fifo_read = 0;
        self.fifo_write = 0;
        self.fifo_len = 0;
        for _ in 0..n {
            self.push_fifo(0.0); // prime: fixed latency = one FFT frame
        }
        self.allpass = DIRAC_AP_DELAYS
            .iter()
            .map(|d| {
                [
                    AllPass::new(d[0], DIRAC_AP_G),
                    AllPass::new(d[1], DIRAC_AP_G),
                ]
            })
            .collect();

        // Four fixed ceiling objects (the diffuse field is directionless → spread it).
        let defs: [(&str, [f64; 3]); 4] = [
            ("Diffuse_TFL", [-1.0, 1.0, 1.0]),
            ("Diffuse_TFR", [1.0, 1.0, 1.0]),
            ("Diffuse_TBL", [-1.0, -1.0, 1.0]),
            ("Diffuse_TBR", [1.0, -1.0, 1.0]),
        ];
        let specs: Vec<SynthObjectSpec> = defs
            .iter()
            .map(|(name, pos)| SynthObjectSpec {
                name: name.to_string(),
                position: *pos,
                gain_db: DIRAC_GAIN_DB,
                size: DIRAC_SIZE,
            })
            .collect();
        self.objects = specs.len();
        specs
    }

    fn process(&mut self, bed: &BedFrame, out: &mut [Vec<f32>]) {
        if self.objects == 0 {
            return;
        }
        let c = bed.channel_count;
        let n = bed.sample_count;
        let amount = self.amount;
        // Borrow the encoder out of `self` so the per-sample loop can mutate the
        // frame buffers while reading it (disjoint, but this satisfies the borrow ck).
        let enc = std::mem::take(&mut self.enc);
        for s in 0..n {
            let base = s * c;
            let mut w = 0.0f32;
            let mut x = 0.0f32;
            let mut y = 0.0f32;
            for &(ch, ca, sa) in enc.iter() {
                if ch < c {
                    let v = bed.pcm[base + ch];
                    w += v;
                    x += ca * v;
                    y += sa * v;
                }
            }
            self.frame_w[self.widx] = w;
            self.frame_x[self.widx] = x;
            self.frame_y[self.widx] = y;
            self.widx = (self.widx + 1) % DIRAC_FFT_SIZE;
            self.fill += 1;
            if self.fill >= DIRAC_HOP {
                self.fill = 0;
                self.run_frame();
            }

            let d = self.pop_fifo();
            for (k, ap) in self.allpass.iter_mut().enumerate() {
                if k >= out.len() {
                    break;
                }
                let [a0, a1] = ap;
                out[k][s] = a1.process(a0.process(d)) * amount;
            }
        }
        self.enc = enc;
    }

    fn param_schema(&self) -> &'static [ObjectGenParamSpec] {
        &DIRAC_PARAM_SPECS
    }

    fn set_param(&mut self, key: &str, value: f32, sample_rate: u32) {
        match key {
            "amount" => self.amount = value.clamp(0.0, 1.0),
            "diffuse_bias" => self.diffuse_bias = value.clamp(0.0, 1.0),
            "hpf_hz" => {
                self.hpf_hz = value.clamp(DIRAC_HPF_MIN, DIRAC_HPF_MAX);
                if !self.hpf_mask.is_empty() {
                    let fs = sample_rate.max(1) as f32;
                    build_hpf_mask(&mut self.hpf_mask, fs, self.hpf_hz);
                }
            }
            _ => {}
        }
    }
}

// ─────────────────────────── host integration ───────────────────────────

/// Signature of the inputs the current plan was built for; a change triggers a
/// rebuild + re-plan. Compared field-by-field without per-frame allocation.
#[derive(Default)]
struct PlanSig {
    id: String,
    out_n: usize,
    out_height: bool,
    labels: Vec<RChannelLabel>,
    rate: u32,
    /// Planned positions depend on the Side/Back surround placement
    /// ([`channel_top_position`]); a live toggle must re-plan or the already
    /// synthesized objects keep their stale row.
    placement: SurroundPlacement,
}

/// Host-side state for the object-generator stage: the registry, the active
/// generator instance, its current plan, and reusable per-frame buffers.
pub struct ObjectGenStage {
    registry: ObjectGeneratorRegistry,
    generator: Option<Box<dyn ObjectGenerator>>,
    specs: Vec<SynthObjectSpec>,
    /// Per-object planar audio scratch (persistent; one Vec per planned object).
    planar: Vec<Vec<f32>>,
    /// Extended interleaved PCM (bed channels + synthesized object channels).
    pcm_ext: Vec<f32>,
    sig: PlanSig,
}

impl ObjectGenStage {
    pub fn new() -> Self {
        Self {
            registry: ObjectGeneratorRegistry::with_builtins(),
            generator: None,
            specs: Vec::new(),
            planar: Vec::new(),
            pcm_ext: Vec::new(),
            sig: PlanSig::default(),
        }
    }

    pub fn registry(&self) -> &ObjectGeneratorRegistry {
        &self.registry
    }

    /// Register a host-supplied (out-of-tree) generator factory.
    pub fn register(&mut self, factory: Box<dyn ObjectGeneratorFactory>) {
        self.registry.register(factory);
    }

    pub fn specs(&self) -> &[SynthObjectSpec] {
        &self.specs
    }

    /// (Re)build + plan if the selection or environment changed; returns the
    /// number of synthesized objects (`0` = inactive / no-op).
    pub fn sync(&mut self, desired_id: &str, ctx: &PrepareCtx) -> usize {
        let did = desired_id.trim();
        let out_n = ctx.output_layout.speakers.len();
        let out_height = layout_has_height(ctx.output_layout);
        let unchanged = self.sig.id == did
            && self.sig.out_n == out_n
            && self.sig.out_height == out_height
            && self.sig.rate == ctx.sample_rate
            && self.sig.placement == ctx.surround_placement
            && self.sig.labels.as_slice() == ctx.input_labels;
        if !unchanged {
            self.sig.id.clear();
            self.sig.id.push_str(did);
            self.sig.out_n = out_n;
            self.sig.out_height = out_height;
            self.sig.rate = ctx.sample_rate;
            self.sig.placement = ctx.surround_placement;
            self.sig.labels.clear();
            self.sig.labels.extend_from_slice(ctx.input_labels);

            self.generator = self.registry.build(did);
            self.specs = match self.generator.as_mut() {
                Some(g) => g.prepare(ctx),
                None => Vec::new(),
            };
            self.planar.truncate(self.specs.len());
            self.planar.resize_with(self.specs.len(), Vec::new);
        }
        self.specs.len()
    }

    /// Apply one live-tunable parameter to the active generator (in place, no DSP
    /// reset). Cheap and idempotent — the engine pushes the active generator's
    /// params each frame, so a fresh generator (after a rebuild) re-receives them.
    pub fn set_param(&mut self, key: &str, value: f32, sample_rate: u32) {
        if let Some(g) = self.generator.as_mut() {
            g.set_param(key, value, sample_rate);
        }
    }

    /// Run the per-frame DSP and return the bed PCM extended with the
    /// synthesized object channels, plus the new channel count. Call only when
    /// [`sync`](Self::sync) returned `> 0`.
    pub fn fill_and_extend(
        &mut self,
        bed_pcm: &[f32],
        channel_count: usize,
        sample_count: usize,
        sample_rate: u32,
    ) -> (&[f32], usize) {
        let m = self.specs.len();
        for buf in self.planar.iter_mut() {
            buf.clear();
            buf.resize(sample_count, 0.0);
        }
        if let Some(generator) = self.generator.as_mut() {
            let bed = BedFrame {
                pcm: bed_pcm,
                channel_count,
                sample_count,
                sample_rate,
            };
            generator.process(&bed, &mut self.planar);
        }
        let out_ch = channel_count + m;
        self.pcm_ext.clear();
        self.pcm_ext.resize(sample_count * out_ch, 0.0);
        for s in 0..sample_count {
            let src = &bed_pcm[s * channel_count..s * channel_count + channel_count];
            let dst = &mut self.pcm_ext[s * out_ch..s * out_ch + out_ch];
            dst[..channel_count].copy_from_slice(src);
            for (k, buf) in self.planar.iter().enumerate().take(m) {
                dst[channel_count + k] = buf[s];
            }
        }
        (&self.pcm_ext, out_ch)
    }
}

impl Default for ObjectGenStage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::speaker_layout::Speaker;

    fn layout(speakers: &[(&str, f32, f32, f32)]) -> SpeakerLayout {
        SpeakerLayout {
            radius_m: 1.0,
            speakers: speakers
                .iter()
                .map(|&(name, x, y, z)| Speaker::from_cartesian(name, x, y, z, true, 0.0))
                .collect(),
        }
    }

    fn flat_5_1() -> Vec<(&'static str, f32, f32, f32)> {
        vec![
            ("FL", -1.0, 1.0, 0.0),
            ("FR", 1.0, 1.0, 0.0),
            ("C", 0.0, 1.0, 0.0),
            ("LFE", 0.0, 1.0, 0.0),
            ("Ls", -1.0, -1.0, 0.0),
            ("Rs", 1.0, -1.0, 0.0),
        ]
    }

    fn layout_7_1_4() -> SpeakerLayout {
        let mut s = flat_5_1();
        s.extend_from_slice(&[
            ("TFL", -1.0, 1.0, 1.0),
            ("TFR", 1.0, 1.0, 1.0),
            ("TBL", -1.0, -1.0, 1.0),
            ("TBR", 1.0, -1.0, 1.0),
        ]);
        layout(&s)
    }

    const LABELS_5_1: [RChannelLabel; 6] = [
        RChannelLabel::L,
        RChannelLabel::R,
        RChannelLabel::C,
        RChannelLabel::LFE,
        RChannelLabel::Ls,
        RChannelLabel::Rs,
    ];

    #[test]
    fn no_op_without_height_layer() {
        let mut g = CopyUpGenerator::default();
        let out = layout(&flat_5_1());
        let specs = g.prepare(&PrepareCtx {
            input_labels: &LABELS_5_1,
            output_layout: &out,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        });
        assert!(specs.is_empty());
    }

    #[test]
    fn no_op_when_input_already_has_height() {
        let mut g = CopyUpGenerator::default();
        let out = layout_7_1_4();
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::Tfl,
            RChannelLabel::Tfr,
        ];
        let specs = g.prepare(&PrepareCtx {
            input_labels: &labels,
            output_layout: &out,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        });
        assert!(specs.is_empty());
    }

    #[test]
    fn plans_five_top_objects_for_5_1_on_7_1_4() {
        let mut g = CopyUpGenerator::default();
        let out = layout_7_1_4();
        let specs = g.prepare(&PrepareCtx {
            input_labels: &LABELS_5_1,
            output_layout: &out,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        });
        // L, R, C, Ls, Rs lifted (LFE skipped) — sides + center now included.
        assert_eq!(specs.len(), 5);
        assert!(
            specs.iter().all(|s| s.position[2] > 0.0),
            "all objects above ear level"
        );
    }

    #[test]
    fn stage_replans_on_surround_placement_change() {
        use renderer::live_params::SurroundPlacement;
        let mut stage = ObjectGenStage::new();
        let out = layout_7_1_4();
        let ctx_side = PrepareCtx {
            input_labels: &LABELS_5_1,
            output_layout: &out,
            sample_rate: 48_000,
            surround_placement: SurroundPlacement::Side,
        };
        assert_eq!(stage.sync("copy_up", &ctx_side), 5);
        let ls_y_side = stage
            .specs()
            .iter()
            .find(|s| s.name.contains("Ls"))
            .expect("Ls object planned")
            .position[1];
        let ctx_back = PrepareCtx {
            surround_placement: SurroundPlacement::Back,
            ..ctx_side
        };
        assert_eq!(stage.sync("copy_up", &ctx_back), 5);
        let ls_y_back = stage
            .specs()
            .iter()
            .find(|s| s.name.contains("Ls"))
            .expect("Ls object planned")
            .position[1];
        assert!(
            ls_y_side > ls_y_back + 0.5,
            "Ls object must move to the back row on a live placement change \
             (side y = {ls_y_side}, back y = {ls_y_back})"
        );
    }

    #[test]
    fn process_copies_source_channels_up() {
        let mut g = CopyUpGenerator::default();
        let out = layout_7_1_4();
        let _ = g.prepare(&PrepareCtx {
            input_labels: &LABELS_5_1,
            output_layout: &out,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        });
        // 2 sample frames, 6 channels: channel value = channel index + sample*10.
        let c = 6usize;
        let n = 2usize;
        let mut pcm = vec![0.0f32; c * n];
        for s in 0..n {
            for ch in 0..c {
                pcm[s * c + ch] = ch as f32 + (s * 10) as f32;
            }
        }
        let mut outbuf: Vec<Vec<f32>> = vec![vec![0.0; n]; 5];
        g.process(
            &BedFrame {
                pcm: &pcm,
                channel_count: c,
                sample_count: n,
                sample_rate: 48_000,
            },
            &mut outbuf,
        );
        // Generic lift order = input label order minus LFE: [L, R, C, Ls, Rs].
        // Object 0 ← L (idx 0); object 2 ← C (idx 2); object 3 ← Ls (idx 4).
        assert_eq!(outbuf[0], vec![0.0, 10.0]);
        assert_eq!(outbuf[2], vec![2.0, 12.0]);
        assert_eq!(outbuf[3], vec![4.0, 14.0]);
    }

    // ── PAD ──

    /// Run PAD at full isolation depth over `n` samples with the given L/R source
    /// signals (5.1 → 7.1.4) and return the energy of the front-left ambience
    /// object over the converged tail.
    fn pad_front_ambient_energy(
        l: impl Fn(usize) -> f32,
        r: impl Fn(usize) -> f32,
        n: usize,
    ) -> f32 {
        let mut g = PadGenerator::default();
        let out_layout = layout_7_1_4();
        let specs = g.prepare(&PrepareCtx {
            input_labels: &LABELS_5_1,
            output_layout: &out_layout,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        });
        // 5.1 → front (L/R) + side (Ls/Rs) + center (C) = 5 objects.
        assert_eq!(specs.len(), 5);
        // Full cancellation so the correlated/decorrelated contrast is maximal.
        g.set_param("strength", 1.0, 48_000);
        let c = 6usize;
        let mut pcm = vec![0.0f32; c * n];
        for s in 0..n {
            pcm[s * c] = l(s); // L (channel 0)
            pcm[s * c + 1] = r(s); // R (channel 1)
        }
        let mut outbuf: Vec<Vec<f32>> = vec![vec![0.0; n]; specs.len()];
        g.process(
            &BedFrame {
                pcm: &pcm,
                channel_count: c,
                sample_count: n,
                sample_rate: 48_000,
            },
            &mut outbuf,
        );
        outbuf[0][n / 2..].iter().map(|&x| x * x).sum()
    }

    #[test]
    fn pad_no_op_without_height_layer() {
        let mut g = PadGenerator::default();
        let out = layout(&flat_5_1());
        assert!(
            g.prepare(&PrepareCtx {
                input_labels: &LABELS_5_1,
                output_layout: &out,
                sample_rate: 48_000,
                surround_placement: renderer::live_params::SurroundPlacement::Side,
            })
            .is_empty()
        );
    }

    #[test]
    fn pad_plans_five_objects_for_5_1_on_7_1_4() {
        let mut g = PadGenerator::default();
        let out = layout_7_1_4();
        let specs = g.prepare(&PrepareCtx {
            input_labels: &LABELS_5_1,
            output_layout: &out,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        });
        // front L/R + side Ls/Rs + center C (no back pair in 5.1).
        assert_eq!(specs.len(), 5);
        assert!(specs.iter().all(|s| s.position[2] > 0.0));
        // The side pair sits on the top-side row (y = 0).
        assert!(
            specs
                .iter()
                .any(|s| s.position[1].abs() < 1.0e-6 && s.position[0] < 0.0),
            "expected a top-side-left object at y≈0"
        );
        // The center sits at top-front-center.
        assert!(
            specs
                .iter()
                .any(|s| s.name == "Ambience_TC" && s.position == [0.0, 1.0, 1.0])
        );
    }

    #[test]
    fn surround_placement_back_moves_the_5_1_surround_to_the_back_row() {
        // A 4.x/5.x source (no Lb/Rb) under Back placement: the Ls/Rs-derived
        // objects must lift to the back row (y = -1), matching the virtual bed —
        // not stay on the side (y = 0).
        let mut g = PadGenerator::default();
        let specs = g.prepare(&PrepareCtx {
            input_labels: &LABELS_5_1,
            output_layout: &layout_7_1_4(),
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Back,
        });
        assert!(
            specs
                .iter()
                .any(|s| s.name == "Ambience_SL" && (s.position[1] + 1.0).abs() < 1.0e-6),
            "Ls phantom should move to the back row (y = -1) under Back placement"
        );
        // The front pair is unaffected (still y = 1).
        assert!(
            specs
                .iter()
                .any(|s| s.name == "Ambience_FL" && (s.position[1] - 1.0).abs() < 1.0e-6)
        );
    }

    #[test]
    fn pad_cancels_correlated_keeps_decorrelated() {
        let sine =
            |f: f32| move |s: usize| (std::f32::consts::TAU * f * s as f32 / 48_000.0).sin() * 0.5;
        let n = 4000;
        // Fully correlated (L == R): the primary is cancelled → little ambience.
        let correlated = pad_front_ambient_energy(sine(1000.0), sine(1000.0), n);
        // Decorrelated (different tones): nothing to cancel → ambience retained.
        let decorrelated = pad_front_ambient_energy(sine(1000.0), sine(1310.0), n);
        assert!(
            correlated.is_finite() && decorrelated.is_finite(),
            "outputs must be finite (no NaN reaches the audio thread)"
        );
        assert!(
            correlated < 0.25 * decorrelated,
            "correlated ambience {correlated} should be << decorrelated {decorrelated}"
        );
    }

    #[test]
    fn pad_output_is_stable_and_bounded() {
        // A steady, fully-correlated tone (L == R) at full strength. The old
        // per-sample NLMS produced gusty bursts near zero-crossings ("wind / mic
        // saturation"); the smoothed-Wiener residual must instead stay small,
        // bounded, and smooth.
        let amp = 0.5f32;
        let freq = 700.0f32;
        let n = 8000usize;
        let c = 6usize;
        let mut pcm = vec![0.0f32; c * n];
        for s in 0..n {
            let v = (std::f32::consts::TAU * freq * s as f32 / 48_000.0).sin() * amp;
            pcm[s * c] = v; // L
            pcm[s * c + 1] = v; // R
        }
        let mut g = PadGenerator::default();
        let out_layout = layout_7_1_4();
        let _ = g.prepare(&PrepareCtx {
            input_labels: &LABELS_5_1,
            output_layout: &out_layout,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        });
        g.set_param("strength", 1.0, 48_000);
        let mut out: Vec<Vec<f32>> = vec![vec![0.0; n]; 4];
        g.process(
            &BedFrame {
                pcm: &pcm,
                channel_count: c,
                sample_count: n,
                sample_rate: 48_000,
            },
            &mut out,
        );
        let tail = &out[0][n / 2..];
        assert!(
            tail.iter().all(|x| x.is_finite()),
            "no NaN/Inf reaches output"
        );
        let peak = tail.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        assert!(
            peak < 0.1 * amp,
            "correlated residual peak {peak} should be ≪ input {amp}"
        );
        let max_step = tail
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_step < 0.02,
            "output must be smooth (no per-sample gain jumps); max step was {max_step}"
        );
    }

    #[test]
    fn pad_finite_across_strength_sweep() {
        // Mixed L/R (a shared/correlated component plus independent tones) at every
        // strength: outputs must stay finite and bounded — no saturation at any
        // setting.
        let n = 6000usize;
        let c = 6usize;
        let mut pcm = vec![0.0f32; c * n];
        let mut peak_in = 0.0f32;
        for s in 0..n {
            let t = s as f32 / 48_000.0;
            let common = (std::f32::consts::TAU * 500.0 * t).sin() * 0.4;
            let l = common + (std::f32::consts::TAU * 900.0 * t).sin() * 0.2;
            let r = common + (std::f32::consts::TAU * 1100.0 * t).sin() * 0.2;
            pcm[s * c] = l;
            pcm[s * c + 1] = r;
            peak_in = peak_in.max(l.abs()).max(r.abs());
        }
        let out_layout = layout_7_1_4();
        for &strength in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let mut g = PadGenerator::default();
            let _ = g.prepare(&PrepareCtx {
                input_labels: &LABELS_5_1,
                output_layout: &out_layout,
                sample_rate: 48_000,
                surround_placement: renderer::live_params::SurroundPlacement::Side,
            });
            g.set_param("strength", strength, 48_000);
            let mut out: Vec<Vec<f32>> = vec![vec![0.0; n]; 4];
            g.process(
                &BedFrame {
                    pcm: &pcm,
                    channel_count: c,
                    sample_count: n,
                    sample_rate: 48_000,
                },
                &mut out,
            );
            for (ch, buf) in out.iter().enumerate() {
                assert!(
                    buf.iter().all(|x| x.is_finite()),
                    "finite output at strength {strength} (ch {ch})"
                );
                let peak = buf[n / 2..].iter().fold(0.0f32, |m, &x| m.max(x.abs()));
                assert!(
                    peak <= 2.5 * peak_in,
                    "strength {strength} ch {ch}: peak {peak} must stay bounded (≤ 2.5×{peak_in})"
                );
            }
        }
    }

    #[test]
    fn pad_declares_five_params() {
        let g = PadGenerator::default();
        let keys: Vec<&str> = g.param_schema().iter().map(|p| p.key).collect();
        assert_eq!(g.param_schema().len(), 5);
        for key in [
            "strength",
            "hpf_hz",
            "gain_db",
            "center_amount",
            "center_hpf_hz",
        ] {
            assert!(keys.contains(&key), "missing param {key}");
        }
    }

    #[test]
    fn pad_plans_seven_objects_for_7_1() {
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::Ls,
            RChannelLabel::Rs,
            RChannelLabel::Lb,
            RChannelLabel::Rb,
        ];
        let mut g = PadGenerator::default();
        let specs = g.prepare(&PrepareCtx {
            input_labels: &labels,
            output_layout: &layout_7_1_4(),
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        });
        // front + side + back pairs (6) + center (1) = 7.
        assert_eq!(specs.len(), 7);
        // Three distinct top rows are represented: front y=1, side y=0, back y=-1.
        for y in [1.0, 0.0, -1.0] {
            assert!(
                specs
                    .iter()
                    .any(|s| (s.position[1] - y).abs() < 1.0e-6 && s.position[0] < 0.0),
                "expected a left object on the y={y} top row"
            );
        }
        assert!(
            specs
                .iter()
                .any(|s| s.name == "Ambience_TC" && s.position == [0.0, 1.0, 1.0])
        );
    }

    #[test]
    fn pad_center_amount_and_hpf() {
        // The center object is the last spec; drive only the C channel and measure
        // its output. amount=0 → silent; a high cutoff attenuates a low tone more
        // than a low cutoff (dialogue-safe).
        let n = 4000usize;
        let c = 6usize;
        let center_energy = |amount: f32, hpf_hz: f32, freq: f32| -> f32 {
            let mut g = PadGenerator::default();
            let specs = g.prepare(&PrepareCtx {
                input_labels: &LABELS_5_1,
                output_layout: &layout_7_1_4(),
                sample_rate: 48_000,
                surround_placement: renderer::live_params::SurroundPlacement::Side,
            });
            let center_idx = specs.iter().position(|s| s.name == "Ambience_TC").unwrap();
            g.set_param("center_amount", amount, 48_000);
            g.set_param("center_hpf_hz", hpf_hz, 48_000);
            let mut pcm = vec![0.0f32; c * n];
            for s in 0..n {
                pcm[s * c + 2] = (std::f32::consts::TAU * freq * s as f32 / 48_000.0).sin() * 0.5;
            }
            let mut out: Vec<Vec<f32>> = vec![vec![0.0; n]; specs.len()];
            g.process(
                &BedFrame {
                    pcm: &pcm,
                    channel_count: c,
                    sample_count: n,
                    sample_rate: 48_000,
                },
                &mut out,
            );
            out[center_idx][n / 2..].iter().map(|&x| x * x).sum()
        };
        // amount=0 is silent.
        assert_eq!(center_energy(0.0, 3000.0, 200.0), 0.0);
        // A 200 Hz center tone: a high cutoff (dialogue-safe) attenuates it far more
        // than a low cutoff.
        let air_only = center_energy(1.0, 3000.0, 200.0);
        let full = center_energy(1.0, 300.0, 200.0);
        assert!(
            air_only < 0.25 * full,
            "high center cutoff should keep low-freq center grounded ({air_only} vs {full})"
        );
    }

    #[test]
    fn builtin_listings_json_includes_declared_schema() {
        let json = builtin_listings_json();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.is_array());
        assert!(json.contains("copy_up") && json.contains("\"pad\""));
        for key in ["strength", "hpf_hz", "gain_db"] {
            assert!(json.contains(key), "schema should declare {key}");
        }
    }

    #[test]
    fn set_param_gain_scales_height_output() {
        let out_layout = layout_7_1_4();
        let n = 1500usize;
        let c = 6usize;
        let mut pcm = vec![0.0f32; c * n];
        for s in 0..n {
            pcm[s * c] = (s as f32 * 0.11).sin() * 0.5;
            pcm[s * c + 1] = (s as f32 * 0.07).sin() * 0.5;
        }
        let run = |gain_db: Option<f32>| {
            let mut g = PadGenerator::default();
            let _ = g.prepare(&PrepareCtx {
                input_labels: &LABELS_5_1,
                output_layout: &out_layout,
                sample_rate: 48_000,
                surround_placement: renderer::live_params::SurroundPlacement::Side,
            });
            if let Some(db) = gain_db {
                g.set_param("gain_db", db, 48_000);
            }
            let mut out: Vec<Vec<f32>> = vec![vec![0.0; n]; 4];
            g.process(
                &BedFrame {
                    pcm: &pcm,
                    channel_count: c,
                    sample_count: n,
                    sample_rate: 48_000,
                },
                &mut out,
            );
            out[0][n / 2..].iter().map(|&x| x * x).sum::<f32>()
        };
        let unity = run(None);
        let attenuated = run(Some(-24.0));
        assert!(
            attenuated < 0.05 * unity,
            "gain_db=-24 should attenuate the height output ({attenuated} vs {unity})"
        );
    }

    // A minimal out-of-tree generator, to prove host registration surfaces it in
    // both the published schema and `build`.
    static DUMMY_PARAMS: [ObjectGenParamSpec; 1] = [ObjectGenParamSpec {
        key: "amount",
        label: "Amount",
        i18n_key: "",
        min: 0.0,
        max: 1.0,
        step: 0.1,
        default: 0.5,
        unit: "",
    }];

    struct DummyGenerator;
    impl ObjectGenerator for DummyGenerator {
        fn capabilities(&self) -> ObjectGenCapabilities {
            ObjectGenCapabilities::default()
        }
        fn prepare(&mut self, _ctx: &PrepareCtx) -> Vec<SynthObjectSpec> {
            Vec::new()
        }
        fn process(&mut self, _bed: &BedFrame, _out: &mut [Vec<f32>]) {}
        fn param_schema(&self) -> &'static [ObjectGenParamSpec] {
            &DUMMY_PARAMS
        }
    }

    struct DummyFactory;
    impl ObjectGeneratorFactory for DummyFactory {
        fn id(&self) -> &'static str {
            "dummy"
        }
        fn label(&self) -> &'static str {
            "Dummy"
        }
        fn requires_height_layer(&self) -> bool {
            false
        }
        fn build(&self) -> Box<dyn ObjectGenerator> {
            Box::new(DummyGenerator)
        }
        fn param_schema(&self) -> &'static [ObjectGenParamSpec] {
            &DUMMY_PARAMS
        }
    }

    #[test]
    fn registry_lists_and_builds_out_of_tree_generator() {
        let mut reg = ObjectGeneratorRegistry::with_builtins();
        reg.register(Box::new(DummyFactory));
        let json = reg.listings_json();
        assert!(json.contains("dummy") && json.contains("Amount"), "{json}");
        assert!(reg.build("dummy").is_some());
        // The built-ins are still listed alongside the registered generator.
        assert!(json.contains("copy_up") && json.contains("\"pad\""));
    }

    // ───────────────────────── DirAC diffuse→height ─────────────────────────

    /// Deterministic broadband noise in [-1, 1].
    fn xorshift(seed: u32) -> impl FnMut() -> f32 {
        let mut s = seed | 1;
        move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        }
    }

    fn dirac_on_7_1_4() -> (DiracGenerator, Vec<SynthObjectSpec>) {
        let mut g = DiracGenerator::default();
        let out_layout = layout_7_1_4();
        let specs = g.prepare(&PrepareCtx {
            input_labels: &LABELS_5_1,
            output_layout: &out_layout,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        });
        (g, specs)
    }

    fn dirac_run(g: &mut DiracGenerator, pcm: &[f32], c: usize, n: usize) -> Vec<Vec<f32>> {
        let mut out = vec![vec![0.0f32; n]; 4];
        g.process(
            &BedFrame {
                pcm,
                channel_count: c,
                sample_count: n,
                sample_rate: 48_000,
            },
            &mut out,
        );
        out
    }

    /// Summed energy of all four ceiling objects over the converged tail.
    fn ceiling_tail_energy(out: &[Vec<f32>], skip: usize) -> f32 {
        out.iter()
            .map(|o| o[skip..].iter().map(|&x| x * x).sum::<f32>())
            .sum()
    }

    #[test]
    fn dirac_no_op_without_height_layer() {
        let mut g = DiracGenerator::default();
        let out = layout(&flat_5_1());
        assert!(
            g.prepare(&PrepareCtx {
                input_labels: &LABELS_5_1,
                output_layout: &out,
                sample_rate: 48_000,
                surround_placement: renderer::live_params::SurroundPlacement::Side,
            })
            .is_empty()
        );
    }

    #[test]
    fn dirac_no_op_when_input_already_has_height() {
        let mut g = DiracGenerator::default();
        let out = layout_7_1_4();
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::Tfl,
            RChannelLabel::Tfr,
        ];
        assert!(
            g.prepare(&PrepareCtx {
                input_labels: &labels,
                output_layout: &out,
                sample_rate: 48_000,
                surround_placement: renderer::live_params::SurroundPlacement::Side,
            })
            .is_empty()
        );
    }

    #[test]
    fn dirac_plans_four_ceiling_objects() {
        let (_g, specs) = dirac_on_7_1_4();
        assert_eq!(specs.len(), 4);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        for want in ["Diffuse_TFL", "Diffuse_TFR", "Diffuse_TBL", "Diffuse_TBR"] {
            assert!(names.contains(&want), "missing {want} in {names:?}");
        }
    }

    #[test]
    fn dirac_lifts_decorrelated_more_than_correlated() {
        let c = 6usize;
        let n = 48_000usize;
        let skip = 12_000usize; // past priming + smoother convergence

        // Directional: a hard-panned source (single channel L) is a plane wave, ψ→0,
        // and must stay grounded. (Identical signal spread over L+R+C would itself be
        // partly diffuse — distinct azimuths — which is correct, just not the contrast
        // we want here.)
        let (mut g, _) = dirac_on_7_1_4();
        g.set_param("amount", 1.0, 48_000);
        let mut src = xorshift(1);
        let mut pcm = vec![0.0f32; c * n];
        for s in 0..n {
            pcm[s * c] = src() * 0.3; // L only
        }
        let corr_e = ceiling_tail_energy(&dirac_run(&mut g, &pcm, c, n), skip);

        // Decorrelated: independent noise in L, R, Ls, Rs → diffuse field, ψ→1.
        let (mut g, _) = dirac_on_7_1_4();
        g.set_param("amount", 1.0, 48_000);
        let (mut a, mut b, mut d, mut e) = (xorshift(2), xorshift(3), xorshift(4), xorshift(5));
        let mut pcm = vec![0.0f32; c * n];
        for s in 0..n {
            pcm[s * c] = a() * 0.3; // L
            pcm[s * c + 1] = b() * 0.3; // R
            pcm[s * c + 4] = d() * 0.3; // Ls
            pcm[s * c + 5] = e() * 0.3; // Rs
        }
        let decor_e = ceiling_tail_energy(&dirac_run(&mut g, &pcm, c, n), skip);

        assert!(
            decor_e > 4.0 * corr_e,
            "decorrelated should lift far more than correlated ({decor_e} vs {corr_e})"
        );
    }

    #[test]
    fn dirac_reconstruction_gain_is_unity() {
        // Force ψ≡1 and a low cutoff so a high tone passes the mask unattenuated; the
        // diffuse stream should then reconstruct the encoded W energy (≈ unity).
        let c = 6usize;
        let n = 48_000usize;
        let skip = 8_000usize;
        let (mut g, _) = dirac_on_7_1_4();
        g.force_diffuse = true;
        g.set_param("amount", 1.0, 48_000);
        g.set_param("hpf_hz", 300.0, 48_000);
        let mut pcm = vec![0.0f32; c * n];
        let mut in_e = 0.0f32;
        for s in 0..n {
            let v = (std::f32::consts::TAU * 2000.0 * s as f32 / 48_000.0).sin() * 0.4;
            pcm[s * c + 2] = v; // C only → W = X = v (az = 0), Y = 0
            if s >= skip {
                in_e += v * v;
            }
        }
        let out = dirac_run(&mut g, &pcm, c, n);
        // Each ceiling object carries a decorrelated (energy-preserving) copy of the
        // diffuse stream; per-object energy should match the input W energy.
        let obj_e: f32 = out[0][skip..].iter().map(|&x| x * x).sum();
        let ratio = obj_e / in_e;
        assert!(
            (0.5..1.7).contains(&ratio),
            "reconstruction gain off (ratio {ratio}: obj {obj_e} vs in {in_e})"
        );
    }

    #[test]
    fn dirac_is_frequency_selective() {
        // LF-correlated (150 Hz, L=R) + HF-decorrelated (independent noise high-passed
        // at 2 kHz, in Ls/Rs) occupy disjoint bands. The lift must follow the HF band
        // and reject the LF — something PAD's pairwise time-domain split cannot do.
        let c = 6usize;
        let n = 48_000usize;
        let skip = 12_000usize;
        let fs = 48_000.0f32;

        let lf = |s: usize| (std::f32::consts::TAU * 150.0 * s as f32 / fs).sin() * 0.4;
        let mut build = |with_lf: bool, with_hf: bool| -> f32 {
            let (mut g, _) = dirac_on_7_1_4();
            g.set_param("amount", 1.0, 48_000);
            let (mut na, mut nb) = (xorshift(11), xorshift(22));
            let mut hp_l = Biquad::highpass(fs, 2000.0, PAD_HPF_Q);
            let mut hp_r = Biquad::highpass(fs, 2000.0, PAD_HPF_Q);
            let mut pcm = vec![0.0f32; c * n];
            for s in 0..n {
                if with_lf {
                    pcm[s * c] = lf(s); // L
                    pcm[s * c + 1] = lf(s); // R
                }
                if with_hf {
                    pcm[s * c + 4] = hp_l.process(na() * 0.5); // Ls
                    pcm[s * c + 5] = hp_r.process(nb() * 0.5); // Rs
                }
            }
            ceiling_tail_energy(&dirac_run(&mut g, &pcm, c, n), skip)
        };

        let mixed = build(true, true);
        let hf_only = build(false, true);
        let lf_only = build(true, false);

        assert!(
            mixed > 4.0 * lf_only,
            "HF-decorrelated band should dominate the lift ({mixed} vs lf {lf_only})"
        );
        assert!(
            (mixed - hf_only).abs() < 0.6 * hf_only,
            "adding the LF-correlated tone must barely change the lift ({mixed} vs {hf_only})"
        );
    }

    #[test]
    fn dirac_finite_bounded_and_silence_decays() {
        let c = 6usize;
        let n = 24_000usize;
        for amount in [0.0f32, 0.3, 0.7, 1.0] {
            let (mut g, _) = dirac_on_7_1_4();
            g.set_param("amount", amount, 48_000);
            let mut src = xorshift(7);
            let mut pcm = vec![0.0f32; c * n];
            for s in 0..n {
                pcm[s * c] = src() * 0.5;
                pcm[s * c + 1] = src() * 0.5;
            }
            let out = dirac_run(&mut g, &pcm, c, n);
            assert!(
                out.iter().all(|o| o.iter().all(|x| x.is_finite())),
                "non-finite output at amount {amount}"
            );
            assert!(
                out.iter().all(|o| o.iter().all(|x| x.abs() < 8.0)),
                "output not bounded at amount {amount}"
            );
        }
        // Silence in → output decays to ~0 (denormal / relative-eps guard).
        let (mut g, _) = dirac_on_7_1_4();
        g.set_param("amount", 1.0, 48_000);
        let pcm = vec![0.0f32; c * n];
        let out = dirac_run(&mut g, &pcm, c, n);
        let tail = ceiling_tail_energy(&out, n / 2);
        assert!(tail < 1.0e-6, "silence should not lift anything ({tail})");
    }
}
