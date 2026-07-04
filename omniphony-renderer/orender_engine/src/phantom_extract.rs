//! Phantom-source extraction pre-stage (runs *before* the bed→height lift).
//!
//! Two selectable methods (the `method` param):
//!
//! **Broadband (default).** For each pair of bed channels — a ring of
//! azimuth-adjacent speakers, optionally widened to non-adjacent pairs — this
//! estimates the **correlated ("primary")** component shared by the two channels,
//! re-emits it as a discrete object at the **real panned position** (derived from
//! the component's relative level in each channel), and **subtracts** it from the
//! two source channels. The bed handed to the height stage therefore carries only
//! the decorrelated residual. It is the conceptual inverse of PAD (which keeps the
//! correlated part grounded and lifts the residual): here the correlated part
//! becomes a localized object and the residual stays in the bed (to be lifted by
//! PAD/copy_up afterwards). Zero latency.
//!
//! **Per-band (spectral).** STFT/intensity-vector analysis
//! ([`crate::phantom_spectral`]): each frequency bin gets its own direction of
//! arrival and directness, routed to eight azimuth sector objects — simultaneous
//! sources at different positions and frequencies are extracted independently.
//! Adds one FFT frame (1024 samples ≈ 21 ms at 48 kHz) of latency to the whole
//! bed. The `passes`/`center`/`sides` params apply to the broadband method only.
//!
//! Like the object generators it runs in the realtime audio thread: all setup is
//! in [`PhantomExtractStage::sync`]; the per-frame DSP in
//! [`PhantomExtractStage::process_and_extend`] does not allocate (steady state)
//! and never panics.

use bridge_api::RChannelLabel;
use renderer::live_params::SurroundPlacement;

use crate::object_gen::{
    ObjectGenParamSpec, PrepareCtx, SynthObjectSpec, channel_top_position, input_has_back,
    one_pole_coeff,
};
use crate::phantom_spectral::SpectralExtractor;

/// Time constant (ms) of the one-pole smoothers for the inter-channel statistics.
/// A bit longer than PAD's, so the derived pan position moves smoothly (no jittery
/// object motion in the 3D view).
const PHANTOM_STAT_TC_MS: f32 = 80.0;
/// Relative regularization on the Wiener denominator (fraction of the pair's mean
/// power) plus a tiny absolute floor — keeps the weight finite without capping the
/// large ratios that strong pans legitimately produce.
const PHANTOM_REG: f32 = 1.0e-2;
const PHANTOM_FLOOR: f32 = 1.0e-9;
/// Weight clamp. Unlike PAD (which clamps near unity) the phantom weight *is* the
/// pan ratio, which is large for hard-panned content, so the clamp is generous.
const PHANTOM_W_CLAMP: f32 = 16.0;
const PHANTOM_DEFAULT_STRENGTH: f32 = 0.5;
const PHANTOM_MAX_PASSES: usize = 3;
const PHANTOM_GAIN_DB: i8 = 0;
const PHANTOM_SIZE: [f32; 3] = [0.3, 0.3, 0.3];

/// Live-tunable params this stage declares (the schema the UI builds sliders from).
pub const PHANTOM_PARAM_SPECS: [ObjectGenParamSpec; 6] = [
    // Binary method switch (rendered as a switch by Studio): off = broadband
    // time-domain pairwise extraction, on = per-band spectral (DirAC-style)
    // extraction. See the module docs for the trade-offs.
    ObjectGenParamSpec {
        key: "method",
        label: "Per-band extraction",
        i18n_key: "twoDSources.phantomMethod",
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        unit: "",
    },
    ObjectGenParamSpec {
        key: "strength",
        label: "Extraction",
        i18n_key: "twoDSources.phantomStrength",
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: PHANTOM_DEFAULT_STRENGTH,
        unit: "",
    },
    ObjectGenParamSpec {
        key: "passes",
        label: "Passes",
        i18n_key: "twoDSources.phantomPasses",
        min: 1.0,
        max: 3.0,
        step: 1.0,
        default: 1.0,
        unit: "",
    },
    ObjectGenParamSpec {
        key: "lift",
        label: "Lift",
        i18n_key: "twoDSources.phantomLift",
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        unit: "",
    },
    // Binary mode (rendered as a switch by Studio): replace the front L-C-R split
    // (two phantoms) with a single relocalized centre. See `RelocalizeArc`.
    ObjectGenParamSpec {
        key: "center",
        label: "Relocalize center",
        i18n_key: "twoDSources.phantomCenter",
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        unit: "",
    },
    // Binary mode: in 7.1, relocate each surround (Ls between L/Lb, Rs between
    // R/Rb) as a single object instead of the joint side arcs. No-op without back
    // channels (5.1 and below).
    ObjectGenParamSpec {
        key: "sides",
        label: "Relocalize sides",
        i18n_key: "twoDSources.phantomSides",
        min: 0.0,
        max: 1.0,
        step: 1.0,
        default: 0.0,
        unit: "",
    },
];

/// JSON array of the declared params, published over OSC so Studio can build the
/// phantom-extraction sliders dynamically (same shape as the object-generator
/// `params` array).
pub fn phantom_schema_json() -> String {
    let arr: Vec<serde_json::Value> = PHANTOM_PARAM_SPECS
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
    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
}

/// One extracted phantom: the two source channels, the planar output slot, the two
/// source floor positions to interpolate between, and the persistent smoothed
/// statistics.
struct PhantomPair {
    a_ch: usize,
    b_ch: usize,
    out: usize,
    /// Source floor positions (z = 0); the phantom is interpolated between them.
    pos_a: [f64; 3],
    pos_b: [f64; 3],
    /// Smoothed auto/cross power for the Wiener weight.
    pow_a: f32,
    pow_b: f32,
    cross: f32,
    /// Smoothed energy of the correlated component in each channel → pan fraction.
    e_a: f32,
    e_b: f32,
}

impl PhantomPair {
    fn process(
        &mut self,
        bed: &mut [f32],
        c: usize,
        n: usize,
        strength: f32,
        alpha: f32,
        planar: &mut [Vec<f32>],
    ) {
        if self.a_ch >= c || self.b_ch >= c || self.out >= planar.len() {
            return;
        }
        for s in 0..n {
            let ia = s * c + self.a_ch;
            let ib = s * c + self.b_ch;
            let a = bed[ia];
            let b = bed[ib];

            self.pow_a += alpha * (a * a - self.pow_a);
            self.pow_b += alpha * (b * b - self.pow_b);
            self.cross += alpha * (a * b - self.cross);

            let reg = PHANTOM_REG * 0.5 * (self.pow_a + self.pow_b) + PHANTOM_FLOOR;
            let w_ab = (self.cross / (self.pow_b + reg)).clamp(-PHANTOM_W_CLAMP, PHANTOM_W_CLAMP);
            let w_ba = (self.cross / (self.pow_a + reg)).clamp(-PHANTOM_W_CLAMP, PHANTOM_W_CLAMP);

            let denom = (self.pow_a * self.pow_b).sqrt() + PHANTOM_FLOOR;
            let coh = (self.cross / denom).clamp(0.0, 1.0);
            let k = strength * coh;

            let ca = w_ab * b;
            let cb = w_ba * a;

            bed[ia] = a - k * ca;
            bed[ib] = b - k * cb;

            let ga = self.pow_a.sqrt();
            let gb = self.pow_b.sqrt();
            let gnorm = (self.pow_a + self.pow_b).sqrt() + PHANTOM_FLOOR;
            planar[self.out][s] = k * (ca * ga + cb * gb) / gnorm;

            self.e_a += alpha * (ca * ca - self.e_a);
            self.e_b += alpha * (cb * cb - self.e_b);
        }
    }

    fn refresh(&self, lift: f64, specs: &mut [SynthObjectSpec]) {
        let t = (self.e_b / (self.e_a + self.e_b + PHANTOM_FLOOR)) as f64;
        if let Some(spec) = specs.get_mut(self.out) {
            spec.position = lerp_xy(self.pos_a, self.pos_b, t, lift);
        }
    }
}

/// Interpolate `a→b` by `t` in the horizontal plane, with a fixed `z` (the lift).
fn lerp_xy(a: [f64; 3], b: [f64; 3], t: f64, z: f64) -> [f64; 3] {
    [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]), z]
}

fn midpoint(a: [f64; 3], b: [f64; 3], z: f64) -> [f64; 3] {
    [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]), z]
}

fn phantom_spec(name: String, position: [f64; 3]) -> SynthObjectSpec {
    SynthObjectSpec {
        name,
        position,
        gain_db: PHANTOM_GAIN_DB,
        size: PHANTOM_SIZE,
    }
}

/// Joint extraction for a three-channel arc `a — m — b` whose middle channel `m`
/// is shared by the (a,m) and (m,b) phantoms. A sequential pairwise cascade would
/// peel `m` twice and asymmetrically (a symmetric source comes out off-centre with
/// the far end unbalanced). Here both phantoms are computed from the *original*
/// channels and `m`'s removal is split by its correlation balance toward `a` vs
/// `b`. Used for the front (L-C-R) and, in 7.1, the side arcs (L-Ls-Lb, R-Rs-Rb).
struct ArcTriplet {
    a_ch: usize,
    m_ch: usize,
    b_ch: usize,
    out_am: usize,
    out_mb: usize,
    pos_a: [f64; 3],
    pos_m: [f64; 3],
    pos_b: [f64; 3],
    pa: f32,
    pm: f32,
    pb: f32,
    x_am: f32,
    x_mb: f32,
    /// Smoothed per-side correlated energy of each phantom → pan positions.
    e_am_a: f32,
    e_am_m: f32,
    e_mb_m: f32,
    e_mb_b: f32,
}

impl ArcTriplet {
    fn process(
        &mut self,
        bed: &mut [f32],
        c: usize,
        n: usize,
        strength: f32,
        alpha: f32,
        planar: &mut [Vec<f32>],
    ) {
        if self.a_ch >= c
            || self.m_ch >= c
            || self.b_ch >= c
            || self.out_am >= planar.len()
            || self.out_mb >= planar.len()
        {
            return;
        }
        for s in 0..n {
            let a = bed[s * c + self.a_ch];
            let m = bed[s * c + self.m_ch];
            let b = bed[s * c + self.b_ch];

            self.pa += alpha * (a * a - self.pa);
            self.pm += alpha * (m * m - self.pm);
            self.pb += alpha * (b * b - self.pb);
            self.x_am += alpha * (a * m - self.x_am);
            self.x_mb += alpha * (m * b - self.x_mb);

            let reg_a = PHANTOM_REG * self.pa + PHANTOM_FLOOR;
            let reg_m = PHANTOM_REG * self.pm + PHANTOM_FLOOR;
            let reg_b = PHANTOM_REG * self.pb + PHANTOM_FLOOR;
            // a/b correlated with m (removed from a/b); m correlated with a/b (all
            // from the *original* channels, no cascade).
            let ca = (self.x_am / (self.pm + reg_m)).clamp(-PHANTOM_W_CLAMP, PHANTOM_W_CLAMP) * m;
            let cb = (self.x_mb / (self.pm + reg_m)).clamp(-PHANTOM_W_CLAMP, PHANTOM_W_CLAMP) * m;
            let cm_a = (self.x_am / (self.pa + reg_a)).clamp(-PHANTOM_W_CLAMP, PHANTOM_W_CLAMP) * a;
            let cm_b = (self.x_mb / (self.pb + reg_b)).clamp(-PHANTOM_W_CLAMP, PHANTOM_W_CLAMP) * b;

            // Coherences gate the extraction (a decorrelated middle stays put).
            let coh_am = (self.x_am / ((self.pa * self.pm).sqrt() + PHANTOM_FLOOR)).clamp(0.0, 1.0);
            let coh_mb = (self.x_mb / ((self.pm * self.pb).sqrt() + PHANTOM_FLOOR)).clamp(0.0, 1.0);
            let k_am = strength * coh_am;
            let k_mb = strength * coh_mb;

            // Split m's removal by its a/b correlation balance (50/50 when balanced)
            // so the shared middle is never peeled twice.
            let fsum = coh_am + coh_mb + PHANTOM_FLOOR;
            let cm_to_a = (coh_am / fsum) * cm_a; // m's share routed to the a-m phantom
            let cm_to_b = (coh_mb / fsum) * cm_b; // m's share routed to the m-b phantom

            bed[s * c + self.a_ch] = a - k_am * ca;
            bed[s * c + self.b_ch] = b - k_mb * cb;
            bed[s * c + self.m_ch] = m - (k_am * cm_to_a + k_mb * cm_to_b);

            let ga = self.pa.sqrt();
            let gm = self.pm.sqrt();
            let gb = self.pb.sqrt();
            let gnorm_am = (self.pa + self.pm).sqrt() + PHANTOM_FLOOR;
            let gnorm_mb = (self.pm + self.pb).sqrt() + PHANTOM_FLOOR;
            planar[self.out_am][s] = k_am * (ca * ga + cm_to_a * gm) / gnorm_am;
            planar[self.out_mb][s] = k_mb * (cb * gb + cm_to_b * gm) / gnorm_mb;

            self.e_am_a += alpha * (ca * ca - self.e_am_a);
            self.e_am_m += alpha * (cm_to_a * cm_to_a - self.e_am_m);
            self.e_mb_m += alpha * (cm_to_b * cm_to_b - self.e_mb_m);
            self.e_mb_b += alpha * (cb * cb - self.e_mb_b);
        }
    }

    fn refresh(&self, lift: f64, specs: &mut [SynthObjectSpec]) {
        let t_am = (self.e_am_m / (self.e_am_a + self.e_am_m + PHANTOM_FLOOR)) as f64;
        if let Some(spec) = specs.get_mut(self.out_am) {
            spec.position = lerp_xy(self.pos_a, self.pos_m, t_am, lift);
        }
        let t_mb = (self.e_mb_b / (self.e_mb_m + self.e_mb_b + PHANTOM_FLOOR)) as f64;
        if let Some(spec) = specs.get_mut(self.out_mb) {
            spec.position = lerp_xy(self.pos_m, self.pos_b, t_mb, lift);
        }
    }
}

/// Alternative arc treatment that **relocalizes** the middle channel `m` of a
/// three-channel arc `a — m — b` instead of splitting it into two phantoms. `m` is
/// first mixed 50/50 into the two ends `a` and `b` (a classic phantom spread) and
/// removed from its own channel; the now-shared component is then re-extracted from
/// the (a, b) pair as a *single* localized object whose position follows the a/b
/// correlation balance. The net object count is therefore one (a relocated channel),
/// not the two of the joint arc — it relocalizes rather than adds. At `strength = 0`
/// it degrades to a pure phantom spread (no object). Used for the front (C between L
/// and R) and, in 7.1, the sides (Ls between L and Lb, Rs between R and Rb).
struct RelocalizeArc {
    a_ch: usize,
    b_ch: usize,
    m_ch: usize,
    out: usize,
    pos_a: [f64; 3],
    pos_b: [f64; 3],
    pow_a: f32,
    pow_b: f32,
    cross: f32,
    /// Smoothed correlated energy per end → pan fraction.
    e_a: f32,
    e_b: f32,
}

impl RelocalizeArc {
    fn process(
        &mut self,
        bed: &mut [f32],
        c: usize,
        n: usize,
        strength: f32,
        alpha: f32,
        planar: &mut [Vec<f32>],
    ) {
        if self.a_ch >= c || self.b_ch >= c || self.m_ch >= c || self.out >= planar.len() {
            return;
        }
        for s in 0..n {
            let ia = s * c + self.a_ch;
            let ib = s * c + self.b_ch;
            let im = s * c + self.m_ch;
            let mm = bed[im];
            // Spread the middle 50/50 into the two ends, then vacate its channel: it
            // now lives as a (perfectly correlated) phantom in the (a, b) pair.
            let ap = bed[ia] + 0.5 * mm;
            let bp = bed[ib] + 0.5 * mm;
            bed[im] = 0.0;

            self.pow_a += alpha * (ap * ap - self.pow_a);
            self.pow_b += alpha * (bp * bp - self.pow_b);
            self.cross += alpha * (ap * bp - self.cross);

            let reg = PHANTOM_REG * 0.5 * (self.pow_a + self.pow_b) + PHANTOM_FLOOR;
            let w_ab = (self.cross / (self.pow_b + reg)).clamp(-PHANTOM_W_CLAMP, PHANTOM_W_CLAMP);
            let w_ba = (self.cross / (self.pow_a + reg)).clamp(-PHANTOM_W_CLAMP, PHANTOM_W_CLAMP);
            let denom = (self.pow_a * self.pow_b).sqrt() + PHANTOM_FLOOR;
            let coh = (self.cross / denom).clamp(0.0, 1.0);
            let k = strength * coh;

            let ca = w_ab * bp;
            let cb = w_ba * ap;
            bed[ia] = ap - k * ca;
            bed[ib] = bp - k * cb;

            let ga = self.pow_a.sqrt();
            let gb = self.pow_b.sqrt();
            let gnorm = (self.pow_a + self.pow_b).sqrt() + PHANTOM_FLOOR;
            planar[self.out][s] = k * (ca * ga + cb * gb) / gnorm;

            self.e_a += alpha * (ca * ca - self.e_a);
            self.e_b += alpha * (cb * cb - self.e_b);
        }
    }

    fn refresh(&self, lift: f64, specs: &mut [SynthObjectSpec]) {
        let t = (self.e_b / (self.e_a + self.e_b + PHANTOM_FLOOR)) as f64;
        if let Some(spec) = specs.get_mut(self.out) {
            spec.position = lerp_xy(self.pos_a, self.pos_b, t, lift);
        }
    }
}

/// A three-channel arc slot: either split into two phantoms (the joint arc) or
/// collapsed to a single relocalized middle.
enum ArcUnit {
    Split(ArcTriplet),
    Reloc(RelocalizeArc),
}

impl ArcUnit {
    fn process(
        &mut self,
        bed: &mut [f32],
        c: usize,
        n: usize,
        strength: f32,
        alpha: f32,
        planar: &mut [Vec<f32>],
    ) {
        match self {
            ArcUnit::Split(t) => t.process(bed, c, n, strength, alpha, planar),
            ArcUnit::Reloc(r) => r.process(bed, c, n, strength, alpha, planar),
        }
    }

    fn refresh(&self, lift: f64, specs: &mut [SynthObjectSpec]) {
        match self {
            ArcUnit::Split(t) => t.refresh(lift, specs),
            ArcUnit::Reloc(r) => r.refresh(lift, specs),
        }
    }

    /// The vacated middle channel (relocalize units only), excluded from the ring
    /// pairing so it never yields a silent phantom.
    fn vacated(&self) -> Option<usize> {
        match self {
            ArcUnit::Reloc(r) => Some(r.m_ch),
            ArcUnit::Split(_) => None,
        }
    }
}

type Pos = Option<(usize, [f64; 3])>;

/// Build an `a—m—b` joint arc (two phantoms) if all three channels are present,
/// appending its specs and recording the ring pairs it subsumes in `covered`.
#[allow(clippy::too_many_arguments)]
fn build_arc(
    specs: &mut Vec<SynthObjectSpec>,
    lift: f64,
    a: Pos,
    m: Pos,
    b: Pos,
    la: RChannelLabel,
    lm: RChannelLabel,
    lb: RChannelLabel,
    covered: &mut Vec<(RChannelLabel, RChannelLabel)>,
) -> Option<ArcTriplet> {
    let ((a_ch, pos_a), (m_ch, pos_m), (b_ch, pos_b)) = (a?, m?, b?);
    let out_am = specs.len();
    let out_mb = out_am + 1;
    specs.push(phantom_spec(
        format!("Phantom_{la:?}_{lm:?}"),
        midpoint(pos_a, pos_m, lift),
    ));
    specs.push(phantom_spec(
        format!("Phantom_{lm:?}_{lb:?}"),
        midpoint(pos_m, pos_b, lift),
    ));
    covered.push((la, lm));
    covered.push((lm, lb));
    Some(ArcTriplet {
        a_ch,
        m_ch,
        b_ch,
        out_am,
        out_mb,
        pos_a,
        pos_m,
        pos_b,
        pa: 0.0,
        pm: 0.0,
        pb: 0.0,
        x_am: 0.0,
        x_mb: 0.0,
        e_am_a: 0.0,
        e_am_m: 0.0,
        e_mb_m: 0.0,
        e_mb_b: 0.0,
    })
}

/// Build a relocalize-arc unit (one object that relocates the middle `m` of the
/// `a — m — b` arc) if all three channels are present. Records the a-m, m-b and a-b
/// ring pairs as covered so the wide passes don't re-extract the arc it consumed.
#[allow(clippy::too_many_arguments)]
fn build_relocalize_arc(
    specs: &mut Vec<SynthObjectSpec>,
    lift: f64,
    a: Pos,
    b: Pos,
    m: Pos,
    la: RChannelLabel,
    lb: RChannelLabel,
    lm: RChannelLabel,
    covered: &mut Vec<(RChannelLabel, RChannelLabel)>,
) -> Option<RelocalizeArc> {
    let ((a_ch, pos_a), (b_ch, pos_b), (m_ch, _)) = (a?, b?, m?);
    let out = specs.len();
    specs.push(phantom_spec(
        format!("Phantom_{lm:?}"),
        midpoint(pos_a, pos_b, lift),
    ));
    covered.push((la, lm));
    covered.push((lm, lb));
    covered.push((la, lb));
    Some(RelocalizeArc {
        a_ch,
        b_ch,
        m_ch,
        out,
        pos_a,
        pos_b,
        pow_a: 0.0,
        pow_b: 0.0,
        cross: 0.0,
        e_a: 0.0,
        e_b: 0.0,
    })
}

/// Build a single `(a,b)` phantom pair as an ordered unit (e.g. the back pair).
fn build_pair_unit(
    specs: &mut Vec<SynthObjectSpec>,
    lift: f64,
    a: Pos,
    b: Pos,
    la: RChannelLabel,
    lb: RChannelLabel,
    covered: &mut Vec<(RChannelLabel, RChannelLabel)>,
) -> Option<PhantomPair> {
    let ((a_ch, pos_a), (b_ch, pos_b)) = (a?, b?);
    let out = specs.len();
    specs.push(phantom_spec(
        format!("Phantom_{la:?}_{lb:?}"),
        midpoint(pos_a, pos_b, lift),
    ));
    covered.push((la, lb));
    Some(PhantomPair {
        a_ch,
        b_ch,
        out,
        pos_a,
        pos_b,
        pow_a: 0.0,
        pow_b: 0.0,
        cross: 0.0,
        e_a: 0.0,
        e_b: 0.0,
    })
}

/// Whether the unordered pair `{a, b}` is already handled by a triplet/back unit.
fn is_covered(
    a: RChannelLabel,
    b: RChannelLabel,
    covered: &[(RChannelLabel, RChannelLabel)],
) -> bool {
    covered
        .iter()
        .any(|&(x, y)| (x == a && y == b) || (x == b && y == a))
}

/// Extraction method selected by the `method` param.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExtractMethod {
    /// Time-domain pairwise Wiener extraction (zero latency).
    Broadband,
    /// Per-band STFT/DOA extraction (one FFT frame of latency).
    Spectral,
}

/// Signature of the inputs the current plan was built for; a change (including a
/// `passes` change) triggers a rebuild. Compared without per-frame allocation.
#[derive(Default)]
struct PlanSig {
    enabled: bool,
    labels: Vec<RChannelLabel>,
    rate: u32,
    passes: usize,
    center: bool,
    sides: bool,
    spectral: bool,
    /// The planned arc/sector positions depend on the Side/Back surround
    /// placement; a live toggle must re-plan or the phantoms keep interpolating
    /// between the stale source positions.
    placement: SurroundPlacement,
}

/// Host-side state for the phantom-extraction stage.
pub struct PhantomExtractStage {
    /// Arc slots, processed in this order so the cardinal stages claim the shared
    /// corner channels first: front (L-C-R), back (Lb-Rb pair), then the side arcs
    /// (L-Ls-Lb, R-Rs-Rb). Each is either split into two phantoms or relocalized to
    /// a single object (per the `center`/`sides` modes). `None` when the input lacks
    /// the channels.
    front: Option<ArcUnit>,
    back: Option<PhantomPair>,
    left: Option<ArcUnit>,
    right: Option<ArcUnit>,
    /// Remaining ring + wide pairs (those not subsumed by an arc/back unit).
    pairs: Vec<PhantomPair>,
    /// Per-band extractor; `Some` iff the plan was built with the spectral
    /// method (the arc/pair units above are then all `None`/empty).
    spectral: Option<SpectralExtractor>,
    specs: Vec<SynthObjectSpec>,
    /// Per-phantom planar audio scratch (persistent).
    planar: Vec<Vec<f32>>,
    /// Extended interleaved PCM (reduced bed + phantom object channels).
    pcm_ext: Vec<f32>,
    sig: PlanSig,
    strength: f32,
    passes: usize,
    lift: f32,
    /// Relocalize the front centre (single object) instead of the L-C-R split.
    center_relocalize: bool,
    /// Relocalize each 7.1 surround (single object) instead of the side arcs.
    sides_relocalize: bool,
    method: ExtractMethod,
    alpha: f32,
}

impl PhantomExtractStage {
    pub fn new() -> Self {
        Self {
            front: None,
            back: None,
            left: None,
            right: None,
            pairs: Vec::new(),
            spectral: None,
            specs: Vec::new(),
            planar: Vec::new(),
            pcm_ext: Vec::new(),
            sig: PlanSig::default(),
            strength: PHANTOM_DEFAULT_STRENGTH,
            passes: 1,
            lift: 0.0,
            center_relocalize: false,
            sides_relocalize: false,
            method: ExtractMethod::Broadband,
            alpha: 0.0,
        }
    }

    pub fn specs(&self) -> &[SynthObjectSpec] {
        &self.specs
    }

    /// (Re)build the pair plan if the selection/environment/`passes` changed, then
    /// refresh the dynamic phantom positions from the smoothed statistics (cheap,
    /// every frame). Returns the phantom count (`0` = inactive / no-op).
    pub fn sync(&mut self, enabled: bool, ctx: &PrepareCtx) -> usize {
        let spectral = self.method == ExtractMethod::Spectral;
        // `passes`/`center`/`sides` only shape the broadband plan; ignore their
        // changes while the spectral method is active (no pointless re-prime).
        let changed = self.sig.enabled != enabled
            || self.sig.rate != ctx.sample_rate
            || self.sig.spectral != spectral
            || self.sig.placement != ctx.surround_placement
            || (!spectral
                && (self.sig.passes != self.passes
                    || self.sig.center != self.center_relocalize
                    || self.sig.sides != self.sides_relocalize))
            || self.sig.labels.as_slice() != ctx.input_labels;
        if changed {
            self.sig.enabled = enabled;
            self.sig.rate = ctx.sample_rate;
            self.sig.passes = self.passes;
            self.sig.center = self.center_relocalize;
            self.sig.sides = self.sides_relocalize;
            self.sig.spectral = spectral;
            self.sig.placement = ctx.surround_placement;
            self.sig.labels.clear();
            self.sig.labels.extend_from_slice(ctx.input_labels);
            self.rebuild(enabled, ctx);
        }
        self.refresh_positions();
        self.specs.len()
    }

    /// Apply one declared parameter in place. A `passes` change re-plans on the next
    /// `sync`.
    pub fn set_param(&mut self, key: &str, value: f32, _sample_rate: u32) {
        match key {
            "method" => {
                self.method = if value >= 0.5 {
                    ExtractMethod::Spectral
                } else {
                    ExtractMethod::Broadband
                }
            }
            "strength" => self.strength = value.clamp(0.0, 1.0),
            "passes" => {
                self.passes = (value.round() as i64).clamp(1, PHANTOM_MAX_PASSES as i64) as usize
            }
            "lift" => self.lift = value.clamp(0.0, 1.0),
            "center" => self.center_relocalize = value >= 0.5,
            "sides" => self.sides_relocalize = value >= 0.5,
            _ => {}
        }
    }

    fn rebuild(&mut self, enabled: bool, ctx: &PrepareCtx) {
        self.front = None;
        self.back = None;
        self.left = None;
        self.right = None;
        self.pairs.clear();
        self.spectral = None;
        self.specs.clear();
        let fs = ctx.sample_rate.max(1) as f32;
        self.alpha = one_pole_coeff(PHANTOM_STAT_TC_MS, fs);
        if !enabled {
            self.planar.clear();
            return;
        }
        if self.method == ExtractMethod::Spectral {
            if let Some(ext) = SpectralExtractor::prepare(ctx) {
                self.specs = ext.specs(self.lift as f64);
                self.spectral = Some(ext);
            }
            self.planar.truncate(self.specs.len());
            self.planar.resize_with(self.specs.len(), Vec::new);
            return;
        }
        // Present, positionable bed channels with their floor position + azimuth.
        // Honour the Side/Back surround placement (matching the virtual bed), so a
        // 4.x/5.x surround phantom sits where the user put the surrounds.
        let use_7_1 = input_has_back(ctx.input_labels);
        let mut chans: Vec<(usize, RChannelLabel, [f64; 3], f64)> = Vec::new();
        for (idx, &label) in ctx.input_labels.iter().enumerate() {
            if let Some(top) = channel_top_position(label, use_7_1, ctx.surround_placement) {
                let floor = [top[0], top[1], 0.0];
                let az = floor[0].atan2(floor[1]); // atan2(x, y): 0 = front, +90° = right
                chans.push((idx, label, floor, az));
            }
        }
        if chans.len() < 2 {
            self.planar.clear();
            return;
        }
        chans.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));
        let n = chans.len();
        let lift = self.lift as f64;
        let find =
            |want: RChannelLabel| chans.iter().find(|ch| ch.1 == want).map(|ch| (ch.0, ch.2));

        // Build the joint arcs + back pair in cascade order — front and back are
        // cardinal and claim the shared corner channels (L/R, Lb/Rb) before the
        // side arcs. Each records the ring pairs it subsumes in `covered`.
        use RChannelLabel::{C, L, Lb, Ls, R, Rb, Rs};
        let mut covered: Vec<(RChannelLabel, RChannelLabel)> = Vec::new();
        let specs = &mut self.specs;
        // Front (L-C-R) and the side arcs (L-Ls-Lb, R-Rs-Rb): each is either the joint
        // split (two phantoms) or, in the matching relocalize mode, a single relocated
        // middle. A relocalized arc vacates its middle channel (C, Ls or Rs), so those
        // are also excluded from the ring/wide pairing below.
        self.front = if self.center_relocalize {
            build_relocalize_arc(
                specs,
                lift,
                find(L),
                find(R),
                find(C),
                L,
                R,
                C,
                &mut covered,
            )
            .map(ArcUnit::Reloc)
        } else {
            build_arc(
                specs,
                lift,
                find(L),
                find(C),
                find(R),
                L,
                C,
                R,
                &mut covered,
            )
            .map(ArcUnit::Split)
        };
        self.back = build_pair_unit(specs, lift, find(Lb), find(Rb), Lb, Rb, &mut covered);
        self.left = if self.sides_relocalize {
            build_relocalize_arc(
                specs,
                lift,
                find(L),
                find(Lb),
                find(Ls),
                L,
                Lb,
                Ls,
                &mut covered,
            )
            .map(ArcUnit::Reloc)
        } else {
            build_arc(
                specs,
                lift,
                find(L),
                find(Ls),
                find(Lb),
                L,
                Ls,
                Lb,
                &mut covered,
            )
            .map(ArcUnit::Split)
        };
        self.right = if self.sides_relocalize {
            build_relocalize_arc(
                specs,
                lift,
                find(R),
                find(Rb),
                find(Rs),
                R,
                Rb,
                Rs,
                &mut covered,
            )
            .map(ArcUnit::Reloc)
        } else {
            build_arc(
                specs,
                lift,
                find(R),
                find(Rs),
                find(Rb),
                R,
                Rs,
                Rb,
                &mut covered,
            )
            .map(ArcUnit::Split)
        };
        // Channels vacated by a relocalize arc (never paired in the ring loop).
        let vacated: [Option<usize>; 3] = [
            self.front.as_ref().and_then(ArcUnit::vacated),
            self.left.as_ref().and_then(ArcUnit::vacated),
            self.right.as_ref().and_then(ArcUnit::vacated),
        ];
        let is_vacated = |ch: usize| vacated.iter().any(|v| *v == Some(ch));

        let passes = self.passes.clamp(1, PHANTOM_MAX_PASSES);
        // Ring distances 1..=passes around the azimuth circle, each unordered pair
        // once (distance d and n−d coincide for a cycle); skip what the arcs cover.
        let mut seen: Vec<(usize, usize)> = Vec::new();
        for d in 1..=passes {
            if d >= n {
                break;
            }
            for i in 0..n {
                let j = (i + d) % n;
                if i == j {
                    continue;
                }
                let key = if i < j { (i, j) } else { (j, i) };
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                let (a_ch, a_label, pos_a, _) = chans[key.0];
                let (b_ch, b_label, pos_b, _) = chans[key.1];
                // A relocalized middle is vacated; never pair it (silent phantom).
                if is_vacated(a_ch) || is_vacated(b_ch) {
                    continue;
                }
                if is_covered(a_label, b_label, &covered) {
                    continue;
                }
                let out = self.specs.len();
                self.pairs.push(PhantomPair {
                    a_ch,
                    b_ch,
                    out,
                    pos_a,
                    pos_b,
                    pow_a: 0.0,
                    pow_b: 0.0,
                    cross: 0.0,
                    e_a: 0.0,
                    e_b: 0.0,
                });
                self.specs.push(phantom_spec(
                    format!("Phantom_{a_label:?}_{b_label:?}"),
                    midpoint(pos_a, pos_b, lift),
                ));
            }
        }
        self.planar.truncate(self.specs.len());
        self.planar.resize_with(self.specs.len(), Vec::new);
    }

    /// Refresh every phantom's position from its smoothed per-side correlated
    /// energy, in the same order the units are processed.
    fn refresh_positions(&mut self) {
        let lift = self.lift as f64;
        if let Some(ext) = self.spectral.as_ref() {
            ext.refresh_positions(lift, &mut self.specs);
            return;
        }
        if let Some(u) = self.front.as_ref() {
            u.refresh(lift, &mut self.specs);
        }
        if let Some(p) = self.back.as_ref() {
            p.refresh(lift, &mut self.specs);
        }
        if let Some(u) = self.left.as_ref() {
            u.refresh(lift, &mut self.specs);
        }
        if let Some(u) = self.right.as_ref() {
            u.refresh(lift, &mut self.specs);
        }
        for pair in self.pairs.iter() {
            pair.refresh(lift, &mut self.specs);
        }
    }

    /// Run the extraction (mutating `bed` in place — the correlated component is
    /// subtracted from each source channel) and return the bed extended with the
    /// phantom object channels, plus the new channel count. Call only when
    /// [`sync`](Self::sync) returned `> 0`.
    pub fn process_and_extend(
        &mut self,
        bed: &mut [f32],
        channel_count: usize,
        sample_count: usize,
        _sample_rate: u32,
    ) -> (&[f32], usize) {
        self.process(bed, channel_count, sample_count);
        let m = self.specs.len();
        let out_ch = channel_count + m;
        self.pcm_ext.clear();
        self.pcm_ext.resize(sample_count * out_ch, 0.0);
        for s in 0..sample_count {
            let src = &bed[s * channel_count..s * channel_count + channel_count];
            let dst = &mut self.pcm_ext[s * out_ch..s * out_ch + out_ch];
            dst[..channel_count].copy_from_slice(src);
            for (k, buf) in self.planar.iter().enumerate().take(m) {
                dst[channel_count + k] = buf[s];
            }
        }
        (&self.pcm_ext, out_ch)
    }

    fn process(&mut self, bed: &mut [f32], c: usize, n: usize) {
        let strength = self.strength;
        let alpha = self.alpha;
        // Borrow the planar scratch out of `self` so the units can write it while
        // borrowing other `self` fields (disjoint, but this keeps it simple).
        let mut planar = std::mem::take(&mut self.planar);
        for buf in planar.iter_mut() {
            buf.clear();
            buf.resize(n, 0.0);
        }
        if let Some(ext) = self.spectral.as_mut() {
            ext.process(bed, c, n, strength, &mut planar);
            self.planar = planar;
            return;
        }
        // Cascade order: front, then back, then the side arcs (which share the
        // corner channels with front/back — those get first claim), then the rest.
        if let Some(u) = self.front.as_mut() {
            u.process(bed, c, n, strength, alpha, &mut planar);
        }
        if let Some(p) = self.back.as_mut() {
            p.process(bed, c, n, strength, alpha, &mut planar);
        }
        if let Some(u) = self.left.as_mut() {
            u.process(bed, c, n, strength, alpha, &mut planar);
        }
        if let Some(u) = self.right.as_mut() {
            u.process(bed, c, n, strength, alpha, &mut planar);
        }
        for pair in self.pairs.iter_mut() {
            pair.process(bed, c, n, strength, alpha, &mut planar);
        }
        self.planar = planar;
    }
}

impl Default for PhantomExtractStage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::speaker_layout::{Speaker, SpeakerLayout};

    const LABELS_5_1: [RChannelLabel; 6] = [
        RChannelLabel::L,
        RChannelLabel::R,
        RChannelLabel::C,
        RChannelLabel::LFE,
        RChannelLabel::Ls,
        RChannelLabel::Rs,
    ];

    const LABELS_7_1: [RChannelLabel; 8] = [
        RChannelLabel::L,
        RChannelLabel::R,
        RChannelLabel::C,
        RChannelLabel::LFE,
        RChannelLabel::Ls,
        RChannelLabel::Rs,
        RChannelLabel::Lb,
        RChannelLabel::Rb,
    ];

    fn dummy_layout() -> SpeakerLayout {
        // The phantom stage ignores the output layout (planar objects render on
        // whatever speakers exist); any non-empty layout works.
        SpeakerLayout {
            radius_m: 1.0,
            speakers: vec![
                Speaker::from_cartesian("FL", -1.0, 1.0, 0.0, true, 0.0),
                Speaker::from_cartesian("FR", 1.0, 1.0, 0.0, true, 0.0),
            ],
        }
    }

    fn ctx<'a>(labels: &'a [RChannelLabel], layout: &'a SpeakerLayout) -> PrepareCtx<'a> {
        PrepareCtx {
            input_labels: labels,
            output_layout: layout,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        }
    }

    fn sine(f: f32) -> impl Fn(usize) -> f32 {
        move |s: usize| (std::f32::consts::TAU * f * s as f32 / 48_000.0).sin()
    }

    #[test]
    fn disabled_is_noop() {
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        assert_eq!(st.sync(false, &ctx(&LABELS_5_1, &layout)), 0);
    }

    #[test]
    fn plans_ring_pairs() {
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        // 5 positionable channels (L,R,C,Ls,Rs; LFE excluded) → ring of 5 pairs.
        assert_eq!(st.sync(true, &ctx(&LABELS_5_1, &layout)), 5);
    }

    #[test]
    fn passes_widen_the_pair_set() {
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        let ring = st.sync(true, &ctx(&LABELS_5_1, &layout));
        st.set_param("passes", 2.0, 48_000);
        let widened = st.sync(true, &ctx(&LABELS_5_1, &layout));
        assert!(
            widened > ring,
            "passes=2 ({widened}) should add pairs over ring ({ring})"
        );
    }

    #[test]
    fn extracts_panned_source_and_removes_it() {
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.set_param("strength", 1.0, 48_000);
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        // A correlated source panned between adjacent channels L and C (0.8 / 0.2).
        let c = 6usize;
        let n = 6000usize;
        let s = sine(700.0);
        let mut bed = vec![0.0f32; c * n];
        let mut in_l = 0.0f32;
        for i in 0..n {
            let v = s(i) * 0.5;
            bed[i * c] = 0.8 * v; // L (index 0)
            bed[i * c + 2] = 0.2 * v; // C (index 2)
            if i >= n / 2 {
                in_l += (0.8 * v).powi(2);
            }
        }
        let k = st
            .specs()
            .iter()
            .position(|sp| sp.name == "Phantom_L_C")
            .unwrap();
        let (pcm, out_ch) = st.process_and_extend(&mut bed, c, n, 48_000);
        let tail = n / 2..n;
        let ph: f32 = tail.clone().map(|i| pcm[i * out_ch + c + k].powi(2)).sum();
        let l_red: f32 = tail.map(|i| pcm[i * out_ch].powi(2)).sum();
        assert!(
            ph > 0.2 * in_l,
            "phantom should carry the panned energy ({ph} vs in_l {in_l})"
        );
        assert!(
            l_red < 0.1 * in_l,
            "L should be largely emptied ({l_red} vs {in_l})"
        );
        // Position: refresh from the converged stats and check it sits toward L.
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        let pos = st.specs()[k].position;
        assert!(
            pos[0] < -0.5,
            "phantom should localize toward L, x = {}",
            pos[0]
        );
    }

    #[test]
    fn leaves_decorrelated_pair() {
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.set_param("strength", 1.0, 48_000);
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        let c = 6usize;
        let n = 6000usize;
        let (s1, s2) = (sine(700.0), sine(1130.0)); // independent tones
        let mut bed = vec![0.0f32; c * n];
        let mut in_l = 0.0f32;
        for i in 0..n {
            bed[i * c] = s1(i) * 0.5; // L
            bed[i * c + 2] = s2(i) * 0.5; // C
            if i >= n / 2 {
                in_l += (s1(i) * 0.5).powi(2);
            }
        }
        let k = st
            .specs()
            .iter()
            .position(|sp| sp.name == "Phantom_L_C")
            .unwrap();
        let (pcm, out_ch) = st.process_and_extend(&mut bed, c, n, 48_000);
        let tail = n / 2..n;
        let ph: f32 = tail.clone().map(|i| pcm[i * out_ch + c + k].powi(2)).sum();
        let l_red: f32 = tail.map(|i| pcm[i * out_ch].powi(2)).sum();
        assert!(
            ph < 0.05 * in_l,
            "decorrelated pair → near-silent phantom ({ph} vs {in_l})"
        );
        assert!(
            l_red > 0.7 * in_l,
            "decorrelated L should be mostly retained ({l_red} vs {in_l})"
        );
    }

    #[test]
    fn front_triplet_keeps_a_symmetric_source_symmetric() {
        // A correlated source equal in L, C, R. The old (L,C)→(C,R) cascade left R
        // unbalanced and localised it off-centre; the joint triplet must remove
        // from L and R symmetrically and place two phantoms straddling the centre.
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.set_param("strength", 1.0, 48_000);
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        let c = 6usize;
        let n = 6000usize;
        let s = sine(700.0);
        let mut bed = vec![0.0f32; c * n];
        let mut in_l = 0.0f32;
        for i in 0..n {
            let v = s(i) * 0.5;
            bed[i * c] = v; // L
            bed[i * c + 1] = v; // R
            bed[i * c + 2] = v; // C
            if i >= n / 2 {
                in_l += v * v;
            }
        }
        let i_lc = st
            .specs()
            .iter()
            .position(|sp| sp.name == "Phantom_L_C")
            .unwrap();
        let i_cr = st
            .specs()
            .iter()
            .position(|sp| sp.name == "Phantom_C_R")
            .unwrap();
        let (pcm, out_ch) = st.process_and_extend(&mut bed, c, n, 48_000);
        let tail = n / 2..n;
        let l_red: f32 = tail.clone().map(|i| pcm[i * out_ch].powi(2)).sum();
        let r_red: f32 = tail.map(|i| pcm[i * out_ch + 1].powi(2)).sum();
        assert!(
            l_red < 0.2 * in_l,
            "L should be reduced ({l_red} vs {in_l})"
        );
        assert!(
            (l_red - r_red).abs() < 0.1 * in_l + 1.0e-6,
            "L and R must be reduced symmetrically ({l_red} vs {r_red})"
        );
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        let x_lc = st.specs()[i_lc].position[0];
        let x_cr = st.specs()[i_cr].position[0];
        assert!(
            x_lc < 0.0 && x_cr > 0.0,
            "phantoms should straddle the centre ({x_lc}, {x_cr})"
        );
        assert!(
            (x_lc + x_cr).abs() < 0.15,
            "phantoms should be symmetric ({x_lc} vs {x_cr})"
        );
    }

    #[test]
    fn builds_front_back_and_side_arcs_in_7_1() {
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        // front(2) + back(1) + left(2) + right(2); every ring distance-1 pair is
        // covered, so passes=1 yields exactly these 7.
        let n_specs = st.sync(true, &ctx(&LABELS_7_1, &layout));
        assert_eq!(n_specs, 7);
        let names: Vec<&str> = st.specs().iter().map(|s| s.name.as_str()).collect();
        for want in [
            "Phantom_L_C",
            "Phantom_C_R",
            "Phantom_Lb_Rb",
            "Phantom_L_Ls",
            "Phantom_Ls_Lb",
            "Phantom_R_Rs",
            "Phantom_Rs_Rb",
        ] {
            assert!(names.contains(&want), "missing {want} in {names:?}");
        }
    }

    #[test]
    fn side_arc_keeps_a_symmetric_source_symmetric_in_7_1() {
        // L = Ls = Lb (correlated, on the left arc): like the front, the joint side
        // arc must reduce the two ends (L and Lb) symmetrically.
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.set_param("strength", 1.0, 48_000);
        st.sync(true, &ctx(&LABELS_7_1, &layout));
        let c = 8usize; // L=0, Ls=4, Lb=6
        let n = 6000usize;
        let s = sine(700.0);
        let mut bed = vec![0.0f32; c * n];
        let mut in_l = 0.0f32;
        for i in 0..n {
            let v = s(i) * 0.5;
            bed[i * c] = v; // L
            bed[i * c + 4] = v; // Ls
            bed[i * c + 6] = v; // Lb
            if i >= n / 2 {
                in_l += v * v;
            }
        }
        let (pcm, out_ch) = st.process_and_extend(&mut bed, c, n, 48_000);
        let tail = n / 2..n;
        let l_red: f32 = tail.clone().map(|i| pcm[i * out_ch].powi(2)).sum();
        let lb_red: f32 = tail.map(|i| pcm[i * out_ch + 6].powi(2)).sum();
        assert!(
            l_red < 0.2 * in_l,
            "L should be reduced ({l_red} vs {in_l})"
        );
        assert!(
            (l_red - lb_red).abs() < 0.1 * in_l + 1.0e-6,
            "L and Lb must be reduced symmetrically ({l_red} vs {lb_red})"
        );
    }

    #[test]
    fn relocalize_center_collapses_front_to_one_object() {
        // With centre-relocalize on, the front L-C-R split is replaced by a single
        // "Phantom_C": the L-C / C-R phantoms are gone and the ring count drops by one.
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.set_param("center", 1.0, 48_000);
        let n_specs = st.sync(true, &ctx(&LABELS_5_1, &layout));
        let names: Vec<&str> = st.specs().iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"Phantom_C"),
            "missing Phantom_C in {names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| *n == "Phantom_L_C" || *n == "Phantom_C_R"),
            "front split should be gone in {names:?}"
        );
        // 5.1 ring is 5 phantoms in split mode; relocalize replaces the two front
        // phantoms with one and drops the C channel from the wide pairing → 4.
        assert_eq!(n_specs, 4);
    }

    #[test]
    fn relocalize_center_moves_centre_to_object_and_empties_c() {
        // A centre-only source (C = v, L = R = 0) is spread, re-extracted as one
        // localized object near the centre, and the C channel is vacated.
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.set_param("center", 1.0, 48_000);
        st.set_param("strength", 1.0, 48_000);
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        let c = 6usize;
        let n = 6000usize;
        let s = sine(700.0);
        let mut bed = vec![0.0f32; c * n];
        let mut in_c = 0.0f32;
        for i in 0..n {
            let v = s(i) * 0.5;
            bed[i * c + 2] = v; // C (index 2)
            if i >= n / 2 {
                in_c += v * v;
            }
        }
        let k = st
            .specs()
            .iter()
            .position(|sp| sp.name == "Phantom_C")
            .unwrap();
        let (pcm, out_ch) = st.process_and_extend(&mut bed, c, n, 48_000);
        let tail = n / 2..n;
        let ph: f32 = tail.clone().map(|i| pcm[i * out_ch + c + k].powi(2)).sum();
        let c_red: f32 = tail.clone().map(|i| pcm[i * out_ch + 2].powi(2)).sum();
        let lr_red: f32 = tail
            .map(|i| pcm[i * out_ch].powi(2) + pcm[i * out_ch + 1].powi(2))
            .sum();
        assert!(
            ph > 0.2 * in_c,
            "object should carry the centre energy ({ph} vs in_c {in_c})"
        );
        assert!(c_red < 1.0e-6, "C channel must be vacated ({c_red})");
        assert!(
            lr_red < 0.1 * in_c,
            "spread centre should be re-extracted, leaving L/R near silent ({lr_red} vs {in_c})"
        );
        // Position: symmetric centre → object sits near the centre line.
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        let pos = st.specs()[k].position;
        assert!(
            pos[0].abs() < 0.15,
            "centre object should be centred, x = {}",
            pos[0]
        );
    }

    #[test]
    fn relocalize_sides_collapses_each_side_to_one_object_in_7_1() {
        // With sides-relocalize on, each 7.1 side arc (L-Ls-Lb, R-Rs-Rb) is replaced
        // by a single relocated surround ("Phantom_Ls" / "Phantom_Rs"); the front
        // split is untouched.
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.set_param("sides", 1.0, 48_000);
        let n_specs = st.sync(true, &ctx(&LABELS_7_1, &layout));
        let names: Vec<&str> = st.specs().iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"Phantom_Ls"),
            "missing Phantom_Ls in {names:?}"
        );
        assert!(
            names.contains(&"Phantom_Rs"),
            "missing Phantom_Rs in {names:?}"
        );
        assert!(
            names.contains(&"Phantom_L_C"),
            "front split should remain in {names:?}"
        );
        assert!(
            !names.iter().any(|n| matches!(
                *n,
                "Phantom_L_Ls" | "Phantom_Ls_Lb" | "Phantom_R_Rs" | "Phantom_Rs_Rb"
            )),
            "side splits should be gone in {names:?}"
        );
        // front split (2) + back (1) + left reloc (1) + right reloc (1) = 5.
        assert_eq!(n_specs, 5);
    }

    #[test]
    fn relocalize_sides_moves_surround_to_object_and_empties_it() {
        // A surround-only source (Ls = v) is spread into L/Lb, re-extracted as one
        // localized object, and the Ls channel is vacated.
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.set_param("sides", 1.0, 48_000);
        st.set_param("strength", 1.0, 48_000);
        st.sync(true, &ctx(&LABELS_7_1, &layout));
        let c = 8usize;
        let n = 6000usize;
        let s = sine(700.0);
        let mut bed = vec![0.0f32; c * n];
        let mut in_ls = 0.0f32;
        for i in 0..n {
            let v = s(i) * 0.5;
            bed[i * c + 4] = v; // Ls (index 4)
            if i >= n / 2 {
                in_ls += v * v;
            }
        }
        let k = st
            .specs()
            .iter()
            .position(|sp| sp.name == "Phantom_Ls")
            .unwrap();
        let (pcm, out_ch) = st.process_and_extend(&mut bed, c, n, 48_000);
        let tail = n / 2..n;
        let ph: f32 = tail.clone().map(|i| pcm[i * out_ch + c + k].powi(2)).sum();
        let ls_red: f32 = tail.map(|i| pcm[i * out_ch + 4].powi(2)).sum();
        assert!(
            ph > 0.2 * in_ls,
            "object should carry the surround energy ({ph} vs {in_ls})"
        );
        assert!(ls_red < 1.0e-6, "Ls channel must be vacated ({ls_red})");
    }

    #[test]
    fn lift_raises_phantom_z() {
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.set_param("lift", 0.8, 48_000);
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        assert!(
            st.specs()
                .iter()
                .all(|sp| (sp.position[2] - 0.8).abs() < 1.0e-6)
        );
    }

    #[test]
    fn replans_on_surround_placement_change() {
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        assert_eq!(st.sync(true, &ctx(&LABELS_5_1, &layout)), 5);
        let find_ls_y = |st: &PhantomExtractStage| {
            st.specs()
                .iter()
                .find(|s| s.name.starts_with("Phantom_Ls"))
                .expect("Ls-anchored phantom planned")
                .position[1]
        };
        let y_side = find_ls_y(&st);
        let ctx_back = PrepareCtx {
            input_labels: &LABELS_5_1,
            output_layout: &layout,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Back,
        };
        assert_eq!(st.sync(true, &ctx_back), 5);
        let y_back = find_ls_y(&st);
        assert!(
            y_side > y_back + 0.5,
            "Ls-anchored phantom must follow a live placement change \
             (side y = {y_side}, back y = {y_back})"
        );
    }

    #[test]
    fn method_switch_replans() {
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        assert_eq!(st.sync(true, &ctx(&LABELS_5_1, &layout)), 5);
        st.set_param("method", 1.0, 48_000);
        assert_eq!(st.sync(true, &ctx(&LABELS_5_1, &layout)), 8);
        let names: Vec<&str> = st.specs().iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"Direct_F") && names.contains(&"Direct_FL"),
            "spectral plan should expose the sector objects, got {names:?}"
        );
        st.set_param("method", 0.0, 48_000);
        assert_eq!(
            st.sync(true, &ctx(&LABELS_5_1, &layout)),
            5,
            "switching back must restore the broadband ring plan"
        );
    }

    #[test]
    fn spectral_method_extracts_via_stage() {
        // End-to-end through the stage dispatch: a centre-only source moves to
        // the front sector object and the C channel empties (single-channel
        // source → fully direct).
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.set_param("method", 1.0, 48_000);
        st.set_param("strength", 1.0, 48_000);
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        let c = 6usize;
        let n = 24_000usize;
        let s = sine(700.0);
        let mut bed = vec![0.0f32; c * n];
        let mut in_c = 0.0f32;
        for i in 0..n {
            let v = s(i) * 0.5;
            bed[i * c + 2] = v; // C
            if i >= n / 2 {
                in_c += v * v;
            }
        }
        let k = st
            .specs()
            .iter()
            .position(|sp| sp.name == "Direct_F")
            .unwrap();
        let (pcm, out_ch) = st.process_and_extend(&mut bed, c, n, 48_000);
        let tail = n / 2..n;
        let ph: f32 = tail.clone().map(|i| pcm[i * out_ch + c + k].powi(2)).sum();
        let c_res: f32 = tail.map(|i| pcm[i * out_ch + 2].powi(2)).sum();
        assert!(
            ph > 0.5 * in_c,
            "front object should carry the centre energy ({ph} vs {in_c})"
        );
        assert!(
            c_res < 0.1 * in_c,
            "C should be largely emptied ({c_res} vs {in_c})"
        );
        // Position: converged stats keep the object at the front centre.
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        let pos = st.specs()[k].position;
        assert!(
            pos[0].abs() < 0.15 && pos[1] > 0.85,
            "front object should localize at the front centre, got {pos:?}"
        );
    }

    #[test]
    fn finite_output() {
        let mut st = PhantomExtractStage::new();
        let layout = dummy_layout();
        st.sync(true, &ctx(&LABELS_5_1, &layout));
        let c = 6usize;
        let n = 2000usize;
        let s = sine(500.0);
        let mut bed = vec![0.0f32; c * n];
        for i in 0..n {
            bed[i * c] = s(i) * 0.5;
            bed[i * c + 1] = s(i) * 0.3;
            bed[i * c + 2] = s(i) * 0.2;
        }
        let (pcm, _) = st.process_and_extend(&mut bed, c, n, 48_000);
        assert!(pcm.iter().all(|x| x.is_finite()));
    }
}
