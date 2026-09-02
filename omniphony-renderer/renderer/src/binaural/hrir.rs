//! Head-related impulse responses (HRIR) and their interpolation grid.
//!
//! The renderer convolves each ear with a per-direction FIR. The *source* of
//! those FIRs is abstracted by [`HrirProvider`] so a measured set (KEMAR / SOFA,
//! M3) can replace the built-in [`SyntheticHrir`] without touching the render
//! path. The interaural *delay* is intentionally **not** baked into these FIRs
//! (it is applied separately as a per-ear delay line, see [`super::itd`]), which
//! keeps all grid FIRs time-aligned so they can be linearly interpolated without
//! comb-filtering.

/// Kernel **capacity** per ear, in taps: the fixed size of every
/// [`HrirPair`] and of the convolver state. The number of taps actually
/// convolved is [`hrir_len`], which scales with the sample rate so the
/// kernel always spans the same [`HRIR_SPAN_S`] of response; this is the
/// span at 192 kHz.
pub const HRIR_LEN: usize = 512;

/// Time span of the convolved kernel: 128 taps at 48 kHz (≈2.7 ms), which
/// holds the head-shadow and pinna part of a time-aligned measured response.
/// A fixed tap count instead would shrink to 1.3 ms at 96 kHz and 0.7 ms at
/// 192 kHz, cutting the concha resonances (1–2 ms) mid-decay.
pub const HRIR_SPAN_S: f32 = 128.0 / 48_000.0;

/// Shortest kernel: the 48 kHz length, so rates at or below it are
/// bit-for-bit what they were with a fixed 128-tap kernel.
const HRIR_MIN_LEN: usize = 128;

/// Taps convolved at `sample_rate`: [`HRIR_SPAN_S`] worth, rounded up to a
/// multiple of 8 (the tap loop consumes the kernel in eight-lane chunks) and
/// clamped to `[128, HRIR_LEN]`. 128 at 44.1 and 48 kHz, 240 at 88.2 kHz,
/// 256 at 96 kHz, 472 at 176.4 kHz, 512 at 192 kHz.
pub fn hrir_len(sample_rate: u32) -> usize {
    let taps = (HRIR_SPAN_S * sample_rate as f32).ceil() as usize;
    taps.next_multiple_of(8).clamp(HRIR_MIN_LEN, HRIR_LEN)
}

/// A left/right pair of (minimum-delay) impulse responses.
#[derive(Clone)]
pub struct HrirPair {
    pub left: [f32; HRIR_LEN],
    pub right: [f32; HRIR_LEN],
}

impl HrirPair {
    fn zeroed() -> Self {
        Self {
            left: [0.0; HRIR_LEN],
            right: [0.0; HRIR_LEN],
        }
    }
}

/// Produces an HRIR pair for a given direction. Implementors must return
/// time-aligned FIRs (no bulk interaural delay) for safe interpolation.
pub trait HrirProvider {
    /// `az_deg`: 0 = front, positive = right. `el_deg`: 0 = horizontal, +90 = up.
    fn render(&self, az_deg: f32, el_deg: f32, sample_rate: u32) -> HrirPair;
}

/// Built-in analytic head model: the head-shadow stage of Brown & Duda, *A
/// Structural Model for Binaural Sound Synthesis* (1998), eq. (3) — a
/// one-pole, one-zero shelf per ear whose zero moves with the angle of
/// incidence:
///
/// ```text
/// H(ω, θ) = (1 + j·α(θ)·ω / 2ω₀) / (1 + j·ω / 2ω₀),   ω₀ = c / a
/// α(θ)    = (1 + α_min/2) + (1 − α_min/2) · cos(θ · 180° / θ_min)
/// ```
///
/// `θ` is the angle between the source and the ear's own axis (0° at the
/// ear, 90° in the median plane, 180° at the opposite ear), `a` the head
/// radius. The DC gain is **1 for every direction** — a head diffracts the
/// bass around itself, so the interaural level difference is nil below a
/// few hundred hertz and only opens up above the corner `2ω₀` (≈1.25 kHz
/// for a KEMAR-sized head). Above it the ear facing the source is boosted
/// (`α → 2`, +6 dB) and the shadowed one cut, down to `α_min` at `θ_min`
/// and back up a little at 180° (the bright spot behind the head).
///
/// The previous model was a one-pole low-pass whose *DC* gain fell to
/// −14 dB on the shadowed side, panning the bass by level in contradiction
/// with the ITD, which is the cue that actually carries lateralisation down
/// there. Self-contained (no measured data); the base of the parametric
/// pinna models and the no-pinna A/B reference.
pub struct SyntheticHrir {
    /// Head radius `a` for `ω₀ = c / a`: the same radius the ITD model uses,
    /// so a listener who fits `head_radius_m` moves the shelf corner and
    /// the interaural delay together.
    pub head_radius_m: f32,
}

impl Default for SyntheticHrir {
    fn default() -> Self {
        Self {
            head_radius_m: super::itd::DEFAULT_HEAD_RADIUS_M,
        }
    }
}

impl SyntheticHrir {
    /// `α_min` (Brown & Duda's recommended value): the deepest high-frequency
    /// shadow, reached at `θ_min`.
    const ALPHA_MIN: f32 = 0.05;
    /// `θ_min`: angle of the deepest shadow, past which the bright spot
    /// behind the head brings the level back up.
    const THETA_MIN_DEG: f32 = 150.0;
    const SPEED_OF_SOUND: f32 = 343.0;

    /// `α(θ)`, with `cos_theta` the cosine of the angle from the ear's axis.
    #[inline]
    fn alpha(cos_theta: f32) -> f32 {
        let theta_deg = cos_theta.clamp(-1.0, 1.0).acos().to_degrees();
        (1.0 + Self::ALPHA_MIN / 2.0)
            + (1.0 - Self::ALPHA_MIN / 2.0)
                * (theta_deg * 180.0 / Self::THETA_MIN_DEG).to_radians().cos()
    }

    /// Impulse response of the shelf: `h[n] = α·δ[n] + (1 − α)(1 − p)·pⁿ`
    /// with `p = exp(−2ω₀ / fs)`. The exponential term carries `1 − α` of
    /// the DC gain so the total is exactly 1; at high frequency only the
    /// impulse survives, leaving `α`.
    fn shelf_ir(&self, alpha: f32, sample_rate: u32, out: &mut [f32; HRIR_LEN]) {
        let w0 = Self::SPEED_OF_SOUND / self.head_radius_m.clamp(0.05, 0.15);
        let p = (-2.0 * w0 / sample_rate as f32).exp();
        let mut tail = (1.0 - alpha) * (1.0 - p);
        for (n, slot) in out.iter_mut().enumerate() {
            *slot = if n == 0 { alpha + tail } else { tail };
            tail *= p;
        }
    }
}

/// How much of a source each ear "sees", `(left, right)` in `[0, 1]`: the
/// exposure the head model shades by, `(1 ∓ lateral) / 2`, with `lateral`
/// the sine of the lateral angle. 0.5 for both ears anywhere in the median
/// plane, 1 for the ear on the source's side of the interaural axis.
pub fn ear_exposure(az_deg: f32, el_deg: f32) -> (f32, f32) {
    let lateral = (az_deg.to_radians().sin() * el_deg.to_radians().cos()).clamp(-1.0, 1.0);
    (0.5 * (1.0 - lateral), 0.5 * (1.0 + lateral))
}

/// Pinna colouration left on the ear that faces away from the source.
///
/// The pinna's cavities are what carve the elevation notches, and they face
/// outward: the shadowed ear's pinna receives only what diffracts around the
/// head, and its notches are correspondingly shallow. The parametric models
/// used to apply their full colouration to both ears in every direction —
/// right near the median plane, wrong at 90°, where the hidden ear was
/// coloured as if it saw the source.
pub const PINNA_SHADOW_FLOOR: f32 = 0.3;

/// Factor on the pinna depth for an ear of the given exposure: 1 in the
/// median plane (exposure 0.5) and on the facing side, falling linearly to
/// [`PINNA_SHADOW_FLOOR`] for the fully shadowed ear. Continuous, and the
/// mirror of the source mirrors the factors.
#[inline]
pub fn pinna_shade(exposure: f32) -> f32 {
    (PINNA_SHADOW_FLOOR + (1.0 - PINNA_SHADOW_FLOOR) * 2.0 * exposure).min(1.0)
}

impl HrirProvider for SyntheticHrir {
    fn render(&self, az_deg: f32, el_deg: f32, sample_rate: u32) -> HrirPair {
        let az = az_deg.to_radians();
        let el = el_deg.to_radians();
        // Cosine of the angle from the right ear's axis (+X): the lateral
        // sine. The left ear sees the supplementary angle.
        let lateral = (az.sin() * el.cos()).clamp(-1.0, 1.0);
        let mut pair = HrirPair::zeroed();
        self.shelf_ir(Self::alpha(-lateral), sample_rate, &mut pair.left);
        self.shelf_ir(Self::alpha(lateral), sample_rate, &mut pair.right);
        pair
    }
}

/// Structural pinna model layered on the analytic head shadow — the exact
/// published model of Brown & Duda, *A Structural Model for Binaural Sound
/// Synthesis*, IEEE Trans. Speech & Audio Processing 6(5), 1998 (pinna stage of
/// Fig. 9; coefficients are Table I, p.485, verbatim).
///
/// The head term is the same per-ear shadow as [`SyntheticHrir`]; on top, a
/// train of 5 elevation/azimuth-dependent pinna **echoes** (events n=2..6; n=1
/// is the direct ridge ρ=1, τ=0, carried by the base IR) adds the comb-filter
/// notches that carry the elevation and front/back cues the bare synthetic
/// model cannot produce (its front and back HRIRs are identical on the median
/// plane). This is the "tune a few parameters" alternative to a captured set.
///
/// Per Brown & Duda the only per-listener parameter is `D_n`; the caller passes
/// the effective `d` (a published Table I preset scaled by the UI knob). `depth`
/// scales the echo amplitudes (0 → bare head shadow ≈ synthetic, 1 → full pinna
/// colouration) — our A/B aid, not part of the paper.
///
/// The reflection coefficients sum to zero, so the echo train has **unit DC
/// gain**: it colours the spectrum (notches/peaks) without changing the
/// broadband level, keeping an A/B against `synthetic`/`saf` level-fair.
pub struct ParametricPinnaHrir {
    /// Effective elevation factor `D_n` per event (a Table I preset × the UI
    /// scale). The only individualized parameter in the published model.
    pub d: [f32; 5],
    /// Echo strength in [0, 1]. 0 ⇒ head shadow only; 1 ⇒ full echoes.
    pub depth: f32,
    /// Head radius of the underlying head-shadow stage (see
    /// [`SyntheticHrir::head_radius_m`]).
    pub head_radius_m: f32,
}

impl ParametricPinnaHrir {
    // Brown & Duda (1998), Table I — exact published coefficients (events n=2..6).
    const RHO: [f32; 5] = [0.5, -1.0, 0.5, -0.25, 0.25]; // ρ_pn (Σ = 0 → unit DC)
    const A: [f32; 5] = [1.0, 5.0, 5.0, 5.0, 5.0]; // A_n (samples @ REF_FS)
    const B: [f32; 5] = [2.0, 4.0, 7.0, 11.0, 13.0]; // B_n (samples @ REF_FS)
    /// `D_n` column for subjects PB & NH (Table I) — the default preset.
    pub const D_PB_NH: [f32; 5] = [1.0, 0.5, 0.5, 0.5, 0.5];
    /// `D_n` column for subject RD (Table I) — alternate per-listener preset.
    pub const D_RD: [f32; 5] = [0.85, 0.35, 0.35, 0.35, 0.35];
    /// Sample rate the A/B delays are tabulated at ("32 samples ≈ 0.7 ms").
    const REF_FS: f32 = 44_100.0;

    /// Add one ear's pinna echo train with **fractional** delays. Brown & Duda
    /// "used linear interpolation to split the amplitude ρ between surrounding
    /// sample points": an echo at delay i+f lands as (1−f)·ρ at tap i and f·ρ at
    /// tap i+1, so notch frequencies move continuously with the delay instead of
    /// snapping to the sample grid. (The direct ρ₀=1 at τ₀=0 is already `src`.)
    fn apply_pinna(src: &[f32; HRIR_LEN], taus: &[f32; 5], depth: f32) -> [f32; HRIR_LEN] {
        let mut out = *src;
        for n in 0..5 {
            let g = Self::RHO[n] * depth;
            let tau = taus[n].max(0.0);
            let i = tau.floor() as usize;
            if i >= HRIR_LEN {
                continue;
            }
            let f = tau - tau.floor();
            let (w0, w1) = (g * (1.0 - f), g * f);
            for k in i..HRIR_LEN {
                out[k] += w0 * src[k - i];
            }
            for k in (i + 1)..HRIR_LEN {
                out[k] += w1 * src[k - (i + 1)];
            }
        }
        out
    }
}

impl ParametricPinnaHrir {
    /// Echo amplitude factor directly behind the head, relative to the
    /// front. The published model covers the frontal hemisphere only; behind
    /// it the pinna's cavities face away from the source and the reflections
    /// that carve the notches are weaker. Our extrapolation, not the paper's.
    const REAR_DEPTH: f32 = 0.5;

    /// Brown & Duda (8): `τ_n = A_n·cos(θ/2)·sin(D_n·(90° − φ)) + B_n`, in
    /// **interaural-polar** coordinates — `θ` the lateral angle between the
    /// source and the median plane (−90°…90°), `φ` the angle around the
    /// interaural axis (0 ahead, 90 overhead, ±180 behind). That is the
    /// system the paper's head model is written in (its shadow depends on
    /// `θ` alone), and it is what makes `cos(θ/2)` stay within
    /// `[cos 45°, 1]`: the echo spread narrows toward the ears, never
    /// collapses.
    ///
    /// The formula is stated for the frontal hemisphere (`|φ| ≤ 90°`). Its
    /// analytic continuation behind the head sends echoes *ahead* of the
    /// direct sound (`τ < 0`, clamped to 0, where a `ρ = −1` echo would
    /// cancel the direct path outright), so the rear is not that. It is the
    /// frontal pattern at the mirrored elevation `180° − φ` — the elevation
    /// cue survives behind the head — with the echo amplitudes scaled toward
    /// [`REAR_DEPTH`](Self::REAR_DEPTH) as the source moves back. Both the
    /// delays and the scale are continuous through the overhead and
    /// underneath planes (`|φ| = 90°`), where the two halves meet.
    ///
    /// Returns the five delays in samples at `sample_rate`, and the echo
    /// amplitude factor.
    fn echo_train(az_deg: f32, el_deg: f32, sample_rate: u32, d: &[f32; 5]) -> ([f32; 5], f32) {
        let (az, el) = (az_deg.to_radians(), el_deg.to_radians());
        let lateral = (az.sin() * el.cos()).clamp(-1.0, 1.0);
        let theta = lateral.asin();
        // Angle around the interaural axis, from the front, in (−180°, 180°].
        let phi = el.sin().atan2(az.cos() * el.cos());
        let phi_front = if phi.abs() <= std::f32::consts::FRAC_PI_2 {
            phi
        } else {
            // Mirror through the vertical: 180° − φ, sign-preserving.
            phi.signum() * (std::f32::consts::PI - phi.abs())
        };
        // How far behind the source is: the rear component of its direction,
        // 0 anywhere on the frontal hemisphere's edge (sides, overhead) → 1
        // straight back. Taken from the direction itself rather than from
        // φ, which is undefined on the interaural axis and would flip the
        // factor at the sides.
        let rear = (-(az.cos() * el.cos())).max(0.0);
        let front = (0.5 * theta).cos();
        let el_term = std::f32::consts::FRAC_PI_2 - phi_front;
        let fs_scale = sample_rate as f32 / Self::REF_FS;
        let mut taus = [0.0f32; 5];
        for n in 0..5 {
            let tau = Self::B[n] + Self::A[n] * front * (d[n] * el_term).sin();
            taus[n] = (tau * fs_scale).max(0.0);
        }
        let scale = 1.0 - (1.0 - Self::REAR_DEPTH) * rear;
        (taus, scale)
    }
}

impl HrirProvider for ParametricPinnaHrir {
    fn render(&self, az_deg: f32, el_deg: f32, sample_rate: u32) -> HrirPair {
        // Base: the analytic head shadow + ILD (identical to SyntheticHrir).
        let head = SyntheticHrir {
            head_radius_m: self.head_radius_m,
        };
        let mut pair = head.render(az_deg, el_deg, sample_rate);
        if self.depth <= 1e-4 {
            return pair; // degenerates to the plain synthetic head model
        }
        let (taus, rear_scale) = Self::echo_train(az_deg, el_deg, sample_rate, &self.d);
        let depth = self.depth * rear_scale;
        // Each ear gets the echoes to the extent it faces the source.
        let (exp_l, exp_r) = ear_exposure(az_deg, el_deg);
        pair.left = Self::apply_pinna(&pair.left, &taus, depth * pinna_shade(exp_l));
        pair.right = Self::apply_pinna(&pair.right, &taus, depth * pinna_shade(exp_r));
        pair
    }
}

/// Taps over which a response that runs past the kernel length is faded
/// out before the cut (a raised cosine), instead of being cut dead. A hard
/// cut where the response is still non-zero is a step, and a step is
/// spectral ripple; eight taps at 48 kHz is 0.17 ms, well inside the tail.
const TRUNCATION_FADE_TAPS: usize = 8;

/// Zero `h` from `len` on. If the response actually extended past `len`,
/// its last [`TRUNCATION_FADE_TAPS`] taps before the cut are shaped by a
/// raised cosine first; a response that was already silent there is left
/// bit-for-bit alone (the embedded KEMAR set is exactly the kernel length
/// at 48 kHz, so it is unaffected).
pub(super) fn truncate_with_fade(h: &mut [f32; HRIR_LEN], len: usize) {
    let cut = h[len..].iter().any(|&x| x != 0.0);
    h[len..].fill(0.0);
    if !cut {
        return;
    }
    let fade = TRUNCATION_FADE_TAPS.min(len);
    for j in 0..fade {
        // From ≈1 at the first faded tap to ≈0 at the last.
        let w = 0.5 * (1.0 + (std::f32::consts::PI * (j + 1) as f32 / (fade + 1) as f32).cos());
        h[len - fade + j] *= w;
    }
}

/// A direction-indexed grid of HRIR pairs with bilinear (az × el) interpolation.
///
/// Built once from an [`HrirProvider`]; queried per object per frame on the
/// audio thread (cheap: 4 lookups + a sample-wise lerp).
pub struct HrirSet {
    az_count: usize,
    el_count: usize,
    el_min_deg: f32,
    el_max_deg: f32,
    /// Taps in use per kernel ([`hrir_len`] at the build rate). Every pair
    /// in `grid` is zero from this index on, and [`at`](Self::at) only
    /// interpolates this many.
    len: usize,
    /// Rate the set was built at.
    sample_rate: u32,
    /// Row-major `[el_idx * az_count + az_idx]`. Azimuth wraps; elevation clamps.
    grid: Vec<HrirPair>,
}

/// A direction snapped to the HRIR update lattice — see
/// [`HrirSet::quantize_direction`]. Equality is the whole point: two directions
/// with the same key interpolate to the same kernel, bit for bit.
///
/// The angles are held as their bit patterns so the type can derive `Eq`, and
/// because bitwise identity is exactly the property the render path needs —
/// `f32` equality would additionally have to reason about `NaN` and `-0.0`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DirectionKey {
    az_bits: u32,
    el_bits: u32,
}

impl DirectionKey {
    fn new(az_deg: f32, el_deg: f32) -> Self {
        Self {
            az_bits: az_deg.to_bits(),
            el_bits: el_deg.to_bits(),
        }
    }
    fn az_deg(self) -> f32 {
        f32::from_bits(self.az_bits)
    }
    fn el_deg(self) -> f32 {
        f32::from_bits(self.el_bits)
    }
}

impl HrirSet {
    /// Grid pitch. 5° follows the density of the embedded KEMAR set (836
    /// directions, ~7° apart): at 10° the set was decimated onto 504 nodes
    /// and then interpolated, which smoothed the elevation notches twice.
    /// 72 × 27 = 1 944 nodes of a 4 KB pair ≈ 8 MB per grid.
    const AZ_STEP_DEG: f32 = 5.0;
    const EL_STEP_DEG: f32 = 5.0;
    const EL_MIN_DEG: f32 = -40.0;
    const EL_MAX_DEG: f32 = 90.0;

    /// Precompute the grid from `provider` at `sample_rate`, without
    /// diffuse-field equalisation (see [`build`](Self::build)).
    pub fn new(provider: &dyn HrirProvider, sample_rate: u32) -> Self {
        Self::build(provider, sample_rate, false)
    }

    /// Precompute the grid from `provider` at `sample_rate`. With
    /// `diffuse_field_eq` the set is divided by its own diffuse-field
    /// response before the level normalisation (see
    /// [`diffuse_field`](super::diffuse_field)).
    pub fn build(provider: &dyn HrirProvider, sample_rate: u32, diffuse_field_eq: bool) -> Self {
        let az_count = (360.0 / Self::AZ_STEP_DEG).round() as usize; // wrap: no duplicate at 360
        let el_count =
            ((Self::EL_MAX_DEG - Self::EL_MIN_DEG) / Self::EL_STEP_DEG).round() as usize + 1;
        let len = hrir_len(sample_rate);
        let mut grid = Vec::with_capacity(az_count * el_count);
        for ei in 0..el_count {
            let el = Self::EL_MIN_DEG + ei as f32 * Self::EL_STEP_DEG;
            for ai in 0..az_count {
                let az = ai as f32 * Self::AZ_STEP_DEG;
                let mut pair = provider.render(az, el, sample_rate);
                // Providers fill the capacity; the set is what gets
                // convolved, so truncate here — before the level
                // normalization, which must weigh the taps in use.
                truncate_with_fade(&mut pair.left, len);
                truncate_with_fade(&mut pair.right, len);
                grid.push(pair);
            }
        }

        if diffuse_field_eq {
            let weights = Self::node_weights(az_count, el_count);
            if super::diffuse_field::equalise(&mut grid, &weights, len, sample_rate) {
                log::debug!("HRIR set diffuse-field equalised");
            }
        }

        // Level-normalize the set to a shared reference: the cos(el)-weighted
        // mean per-ear energy over the sphere (a diffuse-field-style level)
        // becomes exactly 1.0. Without this every source sat at whatever
        // level its data or model happened to have — switching `hrir_source`
        // live jumped the loudness by several dB, and the output-gain parity
        // with the speaker path was not actually guaranteed (issue #157).
        // cos(el) compensates the denser sampling of the grid near the pole.
        let mut num = 0.0f64;
        let mut den = 0.0f64;
        for ei in 0..el_count {
            let el = Self::EL_MIN_DEG + ei as f32 * Self::EL_STEP_DEG;
            let w = el.to_radians().cos().max(0.0) as f64;
            for ai in 0..az_count {
                let p = &grid[ei * az_count + ai];
                let e: f64 = p
                    .left
                    .iter()
                    .chain(p.right.iter())
                    .map(|&x| (x as f64) * (x as f64))
                    .sum();
                num += w * e / 2.0;
                den += w;
            }
        }
        let mean = num / den.max(1e-12);
        if mean > 1e-12 {
            let scale = (1.0 / mean.sqrt()) as f32;
            for p in grid.iter_mut() {
                for v in p.left.iter_mut() {
                    *v *= scale;
                }
                for v in p.right.iter_mut() {
                    *v *= scale;
                }
            }
            log::debug!(
                "HRIR set normalized: mean energy {mean:.4} → gain {:+.2} dB",
                -10.0 * mean.log10()
            );
        }

        Self {
            az_count,
            el_count,
            el_min_deg: Self::EL_MIN_DEG,
            el_max_deg: Self::EL_MAX_DEG,
            len,
            sample_rate,
            grid,
        }
    }

    /// Taps in use per kernel — what the convolvers must run over.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// A set with no taps in use; never true of a built set.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// cos(el) weight of every grid node, row-major like `grid`: the
    /// sphere-area compensation both the level normalisation and the
    /// diffuse-field average use.
    fn node_weights(az_count: usize, el_count: usize) -> Vec<f32> {
        let mut w = Vec::with_capacity(az_count * el_count);
        for ei in 0..el_count {
            let el = Self::EL_MIN_DEG + ei as f32 * Self::EL_STEP_DEG;
            let cw = el.to_radians().cos().max(0.0);
            w.extend(std::iter::repeat_n(cw, az_count));
        }
        w
    }

    /// Diffuse-field power response of the set, `(hz, power)` per bin —
    /// the quantity [`build`](Self::build) flattens.
    pub fn diffuse_field_response(&self) -> Vec<(f32, f32)> {
        let weights = Self::node_weights(self.az_count, self.el_count);
        super::diffuse_field::diffuse_field_power(
            &self.grid,
            &weights,
            self.len,
            self.sample_rate(),
        )
    }

    /// Sample rate the set was built for (recovered from its kernel length).
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Convenience constructor for the built-in synthetic model at the
    /// default head radius.
    pub fn synthetic(sample_rate: u32) -> Self {
        Self::new(&SyntheticHrir::default(), sample_rate)
    }

    #[inline]
    fn pair(&self, az_idx: usize, el_idx: usize) -> &HrirPair {
        &self.grid[el_idx * self.az_count + az_idx]
    }

    /// Snap a direction onto the HRIR *update lattice*.
    ///
    /// [`at`](Self::at) is the single most expensive per-block operation in the
    /// binaural path — a bilinear blend of four 1 KB pairs gathered from a
    /// ~700 KB table — and its result then has to be compared against the
    /// convolvers' current kernels to find out whether anything moved at all.
    /// Both costs are wasted whenever an object barely turned.
    ///
    /// The grid is measured every [`AZ_STEP_DEG`](Self::AZ_STEP_DEG) /
    /// [`EL_STEP_DEG`](Self::EL_STEP_DEG) — 5° — but `fa`/`fe` below are
    /// continuous, so today a 0.01° move yields a numerically different kernel
    /// and arms a full crossfade. That is precision the measurements do not
    /// contain: below the lattice we are only interpolating measurement noise.
    ///
    /// The key carries the very angles [`at_key`](Self::at_key) will feed to
    /// [`at`](Self::at), so "same key ⇒ same kernel" holds by construction
    /// rather than by argument — and `subdiv = None`
    /// ([`HrirUpdateLattice::Exact`](crate::live_params::HrirUpdateLattice::Exact))
    /// degenerates to an exact-direction cache that skips only when nothing
    /// moved at all, which is bit-identical to never skipping.
    pub fn quantize_direction(
        &self,
        az_deg: f32,
        el_deg: f32,
        subdiv: Option<i32>,
    ) -> DirectionKey {
        self.key_at(az_deg, el_deg, subdiv)
    }

    fn key_at(&self, az_deg: f32, el_deg: f32, subdiv: Option<i32>) -> DirectionKey {
        let az = az_deg.rem_euclid(360.0);
        // Elevation is clamped here, exactly as `at` clamps it, so two
        // directions past the pole share one key instead of missing the skip.
        let el = el_deg.clamp(self.el_min_deg, self.el_max_deg);
        let Some(subdiv) = subdiv else {
            return DirectionKey::new(az, el);
        };
        let step_az = Self::AZ_STEP_DEG / subdiv as f32;
        let step_el = Self::EL_STEP_DEG / subdiv as f32;
        DirectionKey::new(
            ((az / step_az).round() * step_az).rem_euclid(360.0),
            (el / step_el).round() * step_el,
        )
    }

    /// The interpolated pair for a key from [`quantize_direction`](Self::quantize_direction).
    pub fn at_key(&self, key: DirectionKey, out: &mut HrirPair) {
        self.at(key.az_deg(), key.el_deg(), out);
    }

    /// Bilinearly-interpolated HRIR pair for an arbitrary direction.
    /// `az_deg`: 0 = front, positive = right. `el_deg`: 0 = horizontal.
    pub fn at(&self, az_deg: f32, el_deg: f32, out: &mut HrirPair) {
        // Azimuth: wrap into [0, az_count) cells.
        let az_norm = az_deg.rem_euclid(360.0) / Self::AZ_STEP_DEG;
        let a0 = az_norm.floor() as usize % self.az_count;
        let a1 = (a0 + 1) % self.az_count;
        let fa = az_norm - az_norm.floor();

        // Elevation: clamp into the grid, then interpolate between rows.
        let el_clamped = el_deg.clamp(self.el_min_deg, self.el_max_deg);
        let el_norm = (el_clamped - self.el_min_deg) / Self::EL_STEP_DEG;
        let e0 = (el_norm.floor() as usize).min(self.el_count - 1);
        let e1 = (e0 + 1).min(self.el_count - 1);
        let fe = el_norm - el_norm.floor();

        let p00 = self.pair(a0, e0);
        let p10 = self.pair(a1, e0);
        let p01 = self.pair(a0, e1);
        let p11 = self.pair(a1, e1);

        let w00 = (1.0 - fa) * (1.0 - fe);
        let w10 = fa * (1.0 - fe);
        let w01 = (1.0 - fa) * fe;
        let w11 = fa * fe;

        // Only the taps in use: past `len` every node is zero, and the
        // convolvers never read that far.
        for n in 0..self.len {
            out.left[n] =
                w00 * p00.left[n] + w10 * p10.left[n] + w01 * p01.left[n] + w11 * p11.left[n];
            out.right[n] =
                w00 * p00.right[n] + w10 * p10.right[n] + w01 * p01.right[n] + w11 * p11.right[n];
        }
    }

    /// The largest absolute tap in the whole grid.
    ///
    /// Zero exactly when every direction renders silence — and that is a state
    /// the level normalization in [`new`](Self::new) cannot rescue, since a
    /// zero mean energy skips the rescale. Build-time only; the audio thread
    /// never asks.
    pub fn peak(&self) -> f32 {
        self.grid
            .iter()
            .flat_map(|p| p.left.iter().chain(p.right.iter()))
            .fold(0.0f32, |m, &x| m.max(x.abs()))
    }

    /// True when every node of the grid holds the same kernel — the provider
    /// collapsed onto a single response, so the set carries no direction
    /// information at all and the image cannot move.
    pub fn is_direction_invariant(&self) -> bool {
        let Some((first, rest)) = self.grid.split_first() else {
            return true;
        };
        rest.iter()
            .all(|p| p.left == first.left && p.right == first.right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn energy(h: &[f32; HRIR_LEN]) -> f32 {
        h.iter().map(|&x| x * x).sum()
    }

    /// The contract the render path relies on to skip work: same key ⇒ the
    /// *identical* kernel, bit for bit. If this ever weakens to "almost the
    /// same", the skip silently freezes a stale kernel instead of updating it.
    #[test]
    fn same_key_yields_a_bit_identical_kernel() {
        let set = HrirSet::synthetic(48_000);
        let mut a = HrirPair::zeroed();
        let mut b = HrirPair::zeroed();
        // Two directions a hair apart — well inside one lattice step (0.16°).
        let k1 = set.key_at(31.700, 12.400, Some(32));
        let k2 = set.key_at(31.705, 12.402, Some(32));
        assert_eq!(k1, k2, "directions within a lattice step must share a key");
        set.at_key(k1, &mut a);
        set.at_key(k2, &mut b);
        assert_eq!(a.left, b.left);
        assert_eq!(a.right, b.right);
    }

    /// Without a lattice the cache must be exact: it may only skip when the
    /// direction did not move at all. That is what makes `DIR_SUBDIV = None`
    /// bit-identical to never skipping.
    #[test]
    fn the_exact_lattice_only_matches_an_unmoved_direction() {
        let set = HrirSet::synthetic(48_000);
        let base = set.key_at(31.7, 12.4, None);
        assert_eq!(base, set.key_at(31.7, 12.4, None), "same direction");
        assert_ne!(base, set.key_at(31.700_01, 12.4, None), "a hair of azimuth");
        assert_ne!(
            base,
            set.key_at(31.7, 12.400_01, None),
            "a hair of elevation"
        );
    }

    /// The lattice must still *track* — a move of a few degrees has to produce
    /// a new key at any setting, or objects would freeze at their first
    /// direction.
    #[test]
    fn a_real_move_changes_the_key() {
        let set = HrirSet::synthetic(48_000);
        for subdiv in [None, Some(512), Some(32)] {
            let base = set.key_at(31.7, 12.4, subdiv);
            assert_ne!(base, set.key_at(33.0, 12.4, subdiv), "azimuth {subdiv:?}");
            assert_ne!(base, set.key_at(31.7, 14.0, subdiv), "elevation {subdiv:?}");
        }
    }

    /// Azimuth wraps: 0° and 360° are the same direction, so they must not
    /// produce two keys (which would cost a pointless kernel rebuild per lap).
    #[test]
    fn azimuth_wraps_to_a_single_key() {
        let set = HrirSet::synthetic(48_000);
        assert_eq!(
            set.key_at(0.0, 0.0, Some(32)),
            set.key_at(360.0, 0.0, Some(32))
        );
        assert_eq!(
            set.key_at(-90.0, 0.0, Some(32)),
            set.key_at(270.0, 0.0, Some(32))
        );
    }

    /// Elevation clamps exactly where `at` clamps it, so directions past the
    /// pole collapse onto one key instead of missing the skip.
    #[test]
    fn elevation_past_the_pole_shares_one_key() {
        let set = HrirSet::synthetic(48_000);
        assert_eq!(
            set.key_at(10.0, 95.0, Some(32)),
            set.key_at(10.0, 120.0, Some(32))
        );
    }

    /// Snapping must stay within half a lattice step of the true direction —
    /// the bound that makes the approximation defensible against a 5° grid.
    #[test]
    fn snapping_error_stays_under_half_a_lattice_step() {
        let set = HrirSet::synthetic(48_000);
        for subdiv in [512, 128, 32] {
            let step = HrirSet::AZ_STEP_DEG / subdiv as f32;
            for &az in &[0.0f32, 7.3, 91.6, 179.9, 271.4, 359.8] {
                let snapped = set.key_at(az, 0.0, Some(subdiv)).az_deg();
                let raw = (snapped - az.rem_euclid(360.0)).abs();
                let err = raw.min(360.0 - raw);
                assert!(
                    err <= step / 2.0 + 1e-4,
                    "subdiv {subdiv}, az {az}: snapped to {snapped}, error {err}"
                );
            }
        }
    }

    /// cos(el)-weighted mean per-ear grid energy — the shared reference level
    /// every built set is normalized to (issue #157).
    fn grid_mean_energy(set: &HrirSet) -> f64 {
        let mut num = 0.0f64;
        let mut den = 0.0f64;
        for ei in 0..set.el_count {
            let el = HrirSet::EL_MIN_DEG + ei as f32 * HrirSet::EL_STEP_DEG;
            let w = el.to_radians().cos().max(0.0) as f64;
            for ai in 0..set.az_count {
                let p = &set.grid[ei * set.az_count + ai];
                let e: f64 = p
                    .left
                    .iter()
                    .chain(p.right.iter())
                    .map(|&x| (x as f64) * (x as f64))
                    .sum();
                num += w * e / 2.0;
                den += w;
            }
        }
        num / den
    }

    /// Every HRIR source must land on the same reference level after build,
    /// so a live `hrir_source` switch keeps the loudness steady and the
    /// speaker/headphone gain parity actually holds (issue #157).
    #[test]
    fn all_sources_share_the_reference_level() {
        let sets: Vec<(&str, HrirSet)> = vec![
            ("synthetic", HrirSet::synthetic(48_000)),
            (
                "pinna",
                HrirSet::new(
                    &ParametricPinnaHrir {
                        d: ParametricPinnaHrir::D_PB_NH,
                        depth: 1.0,
                        head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
                    },
                    48_000,
                ),
            ),
            (
                "prtf",
                HrirSet::new(
                    &crate::binaural::prtf::SpagnolPrtfHrir {
                        depth: 1.0,
                        freq_scale: 1.0,
                        head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
                    },
                    48_000,
                ),
            ),
            (
                "saf",
                HrirSet::new(
                    &crate::binaural::measured::MeasuredHrirData::saf_kemar(),
                    48_000,
                ),
            ),
        ];
        for (name, set) in &sets {
            let mean = grid_mean_energy(set);
            assert!(
                (mean - 1.0).abs() < 1e-3,
                "{name}: grid mean energy {mean} != 1.0"
            );
        }
    }

    #[test]
    fn pinna_depth_zero_matches_synthetic() {
        // depth = 0 must collapse to the bare head-shadow model.
        let p = ParametricPinnaHrir {
            d: ParametricPinnaHrir::D_PB_NH,
            depth: 0.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(30.0, 20.0, 48_000);
        let s = SyntheticHrir::default().render(30.0, 20.0, 48_000);
        assert_eq!(p.left, s.left);
        assert_eq!(p.right, s.right);
    }

    #[test]
    fn pinna_distinguishes_front_from_back_unlike_synthetic() {
        let sumsq_diff = |a: &[f32; HRIR_LEN], b: &[f32; HRIR_LEN]| -> f32 {
            a.iter().zip(b.iter()).map(|(x, y)| (x - y) * (x - y)).sum()
        };
        // On the median plane the bare synthetic model is front/back (near-)
        // identical (lateral = sin(az)·cos(el) ≈ 0 at az 0 and 180; only the
        // f32 sin(π) residual differs): effectively no front/back cue.
        let s_front = SyntheticHrir::default().render(0.0, 0.0, 48_000);
        let s_back = SyntheticHrir::default().render(180.0, 0.0, 48_000);
        let s_diff = sumsq_diff(&s_front.left, &s_back.left);
        assert!(
            s_diff < 1e-6,
            "synthetic front/back should be ~identical: {s_diff}"
        );

        // The pinna echoes break that symmetry — front and back clearly differ,
        // and that difference dominates the synthetic residual.
        let pinna = ParametricPinnaHrir {
            d: ParametricPinnaHrir::D_PB_NH,
            depth: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        let p_front = pinna.render(0.0, 0.0, 48_000);
        let p_back = pinna.render(180.0, 0.0, 48_000);
        let p_diff = sumsq_diff(&p_front.left, &p_back.left);
        assert!(p_diff > 1e-3, "pinna front/back too similar: {p_diff}");
        assert!(
            p_diff > s_diff * 1e3,
            "pinna cue not dominant: p={p_diff} s={s_diff}"
        );
    }

    /// Behind the head the elevation cue must survive: the rear is the
    /// mirrored frontal pattern, not a collapse to a single echo train.
    #[test]
    fn pinna_keeps_an_elevation_cue_behind_the_head() {
        let pinna = ParametricPinnaHrir {
            d: ParametricPinnaHrir::D_PB_NH,
            depth: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        let level = pinna.render(180.0, 0.0, 48_000);
        let high = pinna.render(180.0, 40.0, 48_000);
        let diff: f32 = level
            .left
            .iter()
            .zip(high.left.iter())
            .map(|(a, b)| (a - b) * (a - b))
            .sum();
        assert!(diff > 1e-3, "no elevation cue behind the head: {diff}");
    }

    /// The front and rear halves meet overhead: a source just ahead of the
    /// zenith and one just behind it must render (nearly) the same pair.
    #[test]
    fn pinna_is_continuous_through_the_zenith() {
        let pinna = ParametricPinnaHrir {
            d: ParametricPinnaHrir::D_PB_NH,
            depth: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        let ahead = pinna.render(0.0, 89.0, 48_000);
        let behind = pinna.render(180.0, 89.0, 48_000);
        let max_diff = ahead
            .left
            .iter()
            .zip(behind.left.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_diff < 0.05, "discontinuity at the zenith: {max_diff}");
    }

    /// The shadowed ear keeps a shallower version of the pinna colouration;
    /// the median plane is untouched (both ears at exposure 0.5 → factor 1).
    #[test]
    fn pinna_is_shallower_on_the_shadowed_ear() {
        assert_eq!(pinna_shade(0.5), 1.0);
        assert_eq!(pinna_shade(1.0), 1.0);
        assert!((pinna_shade(0.0) - PINNA_SHADOW_FLOOR).abs() < 1e-6);
        let pinna = ParametricPinnaHrir {
            d: ParametricPinnaHrir::D_PB_NH,
            depth: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        // Colouration energy relative to the ear's own head-shadow base, so
        // the shadowed ear's lower level does not enter the comparison.
        let colour = |az: f32, ear: fn(&HrirPair) -> &[f32; HRIR_LEN]| -> f32 {
            let p = pinna.render(az, 0.0, 48_000);
            let s = SyntheticHrir::default().render(az, 0.0, 48_000);
            let diff: f32 = ear(&p)
                .iter()
                .zip(ear(&s).iter())
                .map(|(a, b)| (a - b) * (a - b))
                .sum();
            diff / energy(ear(&s))
        };
        // Source hard right: the right ear's colouration is the full one, the
        // left ear's is scaled by the floor (energy ratio ≈ floor² = 0.09).
        let (right, left) = (colour(90.0, |p| &p.right), colour(90.0, |p| &p.left));
        let ratio = left / right;
        assert!(
            ratio < 0.2 && ratio > 0.04,
            "shadowed/facing colouration energy ratio {ratio}, expected ≈ 0.09"
        );
        // Median plane: both ears equally coloured.
        let (fl, fr) = (colour(0.0, |p| &p.left), colour(0.0, |p| &p.right));
        assert!((fl - fr).abs() < 1e-6 * fl.max(1e-12));
    }

    #[test]
    fn pinna_rd_preset_differs_from_pbnh() {
        // D_n is the model's only per-listener parameter (Table I); off the
        // horizon the two published columns must yield distinct HRIRs.
        let pbnh = ParametricPinnaHrir {
            d: ParametricPinnaHrir::D_PB_NH,
            depth: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(0.0, 45.0, 48_000);
        let rd = ParametricPinnaHrir {
            d: ParametricPinnaHrir::D_RD,
            depth: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(0.0, 45.0, 48_000);
        let diff: f32 = pbnh
            .left
            .iter()
            .zip(rd.left.iter())
            .map(|(a, b)| (a - b) * (a - b))
            .sum();
        assert!(diff > 1e-4, "PB&NH and RD presets too similar: {diff}");
    }

    #[test]
    fn pinna_fractional_delay_is_continuous_in_elevation() {
        // Fractional (interpolated) echo delays: a tiny elevation step must not
        // jump the HRIR the way snapping a delay to an integer sample would
        // (an integer snap would shift a ρ≈1 echo by a whole tap → ~0.4 jump).
        let pinna = ParametricPinnaHrir {
            d: ParametricPinnaHrir::D_PB_NH,
            depth: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        let a = pinna.render(0.0, 30.0, 48_000);
        let b = pinna.render(0.0, 30.5, 48_000);
        let max_diff = a
            .left
            .iter()
            .zip(b.left.iter())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_diff < 0.05,
            "fractional-delay discontinuity: {max_diff}"
        );
    }

    /// Left/right mirror symmetry: every *modelled* provider (synthetic, pinna,
    /// PRTF) has no ear-specific data, so rendering the mirrored direction must
    /// swap the ears exactly: `render(az).left == render(-az).right`. Measured
    /// sets (SAF/SOFA) are exempt — real ears are not symmetric. The grid feeds
    /// azimuths in [0°, 360°), so the mirror of `az` is `360 - az`; a provider
    /// that assumes signed azimuths breaks precisely on that convention (heard
    /// as a brighter left hemisphere with `pinna`).
    #[test]
    fn modelled_providers_are_left_right_symmetric() {
        let providers: Vec<(&str, Box<dyn HrirProvider>)> = vec![
            ("synthetic", Box::new(SyntheticHrir::default())),
            (
                "pinna",
                Box::new(ParametricPinnaHrir {
                    d: ParametricPinnaHrir::D_PB_NH,
                    depth: 1.0,
                    head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
                }),
            ),
            (
                "prtf",
                Box::new(crate::binaural::prtf::SpagnolPrtfHrir {
                    depth: 1.0,
                    freq_scale: 1.0,
                    head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
                }),
            ),
        ];
        for (name, p) in &providers {
            for el in [-30.0f32, 0.0, 30.0] {
                for az in [10.0f32, 30.0, 90.0, 150.0] {
                    let a = p.render(az, el, 48_000);
                    let b = p.render(360.0 - az, el, 48_000);
                    let d_lr: f32 = a
                        .left
                        .iter()
                        .zip(b.right.iter())
                        .map(|(x, y)| (x - y) * (x - y))
                        .sum();
                    let d_rl: f32 = a
                        .right
                        .iter()
                        .zip(b.left.iter())
                        .map(|(x, y)| (x - y) * (x - y))
                        .sum();
                    assert!(
                        d_lr < 1e-8 && d_rl < 1e-8,
                        "{name}: az {az}/{} el {el} not mirror-symmetric \
                         (L↔R sumsq {d_lr:e} / {d_rl:e})",
                        360.0 - az
                    );
                }
            }
        }
    }

    /// The kernel spans 2.67 ms at every rate: 128 taps up to 48 kHz (so
    /// those rates are unchanged), then proportionally more, in multiples
    /// of eight, up to the capacity at 192 kHz.
    #[test]
    fn kernel_length_follows_the_sample_rate() {
        assert_eq!(hrir_len(44_100), 128);
        assert_eq!(hrir_len(48_000), 128);
        assert_eq!(hrir_len(88_200), 240);
        assert_eq!(hrir_len(96_000), 256);
        assert_eq!(hrir_len(176_400), 472);
        assert_eq!(hrir_len(192_000), 512);
        for fs in [44_100, 48_000, 88_200, 96_000, 176_400, 192_000] {
            assert_eq!(
                hrir_len(fs) % 8,
                0,
                "{fs}: not a multiple of the lane count"
            );
            assert!(hrir_len(fs) <= HRIR_LEN);
        }
    }

    /// A set built at 96 kHz convolves 256 taps, and every node is zero past
    /// them — the convolver's contract for the taps it does not read.
    #[test]
    fn a_96k_set_uses_256_taps_and_is_silent_beyond() {
        let set = HrirSet::new(
            &crate::binaural::measured::MeasuredHrirData::saf_kemar().resampled_to(96_000),
            96_000,
        );
        assert_eq!(set.len(), 256);
        assert!(set.grid.iter().all(|p| {
            p.left[256..]
                .iter()
                .chain(&p.right[256..])
                .all(|&x| x == 0.0)
        }));
        // The measured response really extends past the old 128-tap cut at
        // this rate: taps 128..256 must carry energy somewhere on the grid.
        let tail: f32 = set
            .grid
            .iter()
            .map(|p| p.left[128..256].iter().map(|x| x * x).sum::<f32>())
            .sum();
        assert!(tail > 0.0, "nothing past tap 128 at 96 kHz");
    }

    /// Brown & Duda's head shadow keeps the bass: the DC gain of both ears
    /// is 1 in every direction, including the fully shadowed one, where the
    /// previous model sat at −14 dB.
    #[test]
    fn synthetic_head_has_unity_dc_gain_everywhere() {
        for (az, el) in [
            (0.0f32, 0.0f32),
            (90.0, 0.0),
            (270.0, 0.0),
            (150.0, 0.0),
            (90.0, 45.0),
        ] {
            let p = SyntheticHrir::default().render(az, el, 48_000);
            let dc_l: f32 = p.left.iter().sum();
            let dc_r: f32 = p.right.iter().sum();
            assert!((dc_l - 1.0).abs() < 1e-3, "({az}, {el}) left DC {dc_l}");
            assert!((dc_r - 1.0).abs() < 1e-3, "({az}, {el}) right DC {dc_r}");
        }
    }

    /// High-frequency gain is α(θ): +6 dB on the ear facing the source,
    /// deep shadow on the other, a small bright spot straight across.
    #[test]
    fn synthetic_head_shelves_the_treble_by_incidence() {
        let nyquist = |h: &[f32; HRIR_LEN]| -> f32 {
            h.iter()
                .enumerate()
                .map(|(n, &x)| if n % 2 == 0 { x } else { -x })
                .sum()
        };
        let p = SyntheticHrir::default().render(90.0, 0.0, 48_000);
        let (l, r) = (nyquist(&p.left), nyquist(&p.right));
        // The exponential term still contributes (1 − α)(1 − p)/(1 + p) at
        // Nyquist, hence ≈ 1.92 rather than exactly α = 2.
        assert!(
            (r - 2.0).abs() < 0.1,
            "ipsilateral HF gain {r}, expected ≈ 2"
        );
        // The left ear is 180° from the source axis: past θ_min, in the
        // bright spot, α ≈ 0.24 — darker than the median plane's 0.72.
        assert!(l > 0.15 && l < 0.35, "contralateral HF gain {l}");
        let m = SyntheticHrir::default().render(0.0, 0.0, 48_000);
        let med = nyquist(&m.left);
        assert!(med > 0.6 && med < 0.85, "median-plane HF gain {med}");
        // Deepest shadow at θ_min = 150° from the ear axis, i.e. 60° past
        // the median plane on the far side.
        let deep = SyntheticHrir::default().render(-60.0, 0.0, 48_000);
        let d = nyquist(&deep.right);
        // α_min = 0.05 plus the same ≈ 0.08 tail term as above.
        assert!(d < 0.2, "θ_min shadow gain {d}");
    }

    /// The shelf corner follows the head radius: a larger head has a lower
    /// corner (2ω₀ = 2c/a), so its shadowed-ear response holds more of its
    /// energy in the exponential tail and less in the leading impulse.
    #[test]
    fn synthetic_shelf_corner_follows_the_head_radius() {
        let small = SyntheticHrir {
            head_radius_m: 0.07,
        }
        .render(90.0, 0.0, 48_000);
        let large = SyntheticHrir {
            head_radius_m: 0.10,
        }
        .render(90.0, 0.0, 48_000);
        // Left ear is shadowed: h[0] = α + (1 − α)(1 − p), p = exp(−2c/(a·fs)).
        // A larger a → larger p → smaller (1 − p) → smaller h[0].
        assert!(
            large.left[0] < small.left[0],
            "{} vs {}",
            large.left[0],
            small.left[0]
        );
        // The tail decays as pⁿ: a larger head has the slower decay (lower
        // corner 2c/a), and the ratio of consecutive taps is p itself.
        let decay = |h: &[f32; HRIR_LEN]| h[2] / h[1];
        let (p_small, p_large) = (decay(&small.left), decay(&large.left));
        assert!(p_large > p_small, "decay {p_large} vs {p_small}");
        let expected = |a: f32| (-2.0 * 343.0 / (a * 48_000.0)).exp();
        assert!((p_small - expected(0.07)).abs() < 1e-3, "{p_small}");
        assert!((p_large - expected(0.10)).abs() < 1e-3, "{p_large}");
        assert_eq!(
            SyntheticHrir::default().head_radius_m,
            crate::binaural::itd::DEFAULT_HEAD_RADIUS_M
        );
    }

    /// The grid has the pitch it says. The KEMAR build time is printed for
    /// the record (85 ms in a release build on a desktop; several seconds in
    /// a debug test build on a loaded CI runner) and deliberately not gated:
    /// a wall-clock bound in a debug build under a parallel suite is a coin
    /// toss, and it runs on the rebuild worker anyway.
    #[test]
    fn five_degree_grid_has_1944_nodes() {
        let t0 = std::time::Instant::now();
        let set = HrirSet::new(
            &crate::binaural::measured::MeasuredHrirData::saf_kemar(),
            48_000,
        );
        let elapsed = t0.elapsed();
        println!(
            "[measure] 5° KEMAR grid: {} nodes in {elapsed:?}",
            set.grid.len()
        );
        assert_eq!(set.az_count, 72);
        assert_eq!(set.el_count, 27);
        assert_eq!(set.grid.len(), 1_944);
    }

    /// A response that runs past the kernel length is faded over its last
    /// taps before the cut; one that was already silent there is untouched.
    #[test]
    fn truncation_fades_only_what_it_cuts() {
        let len = 128;
        // Runs to the capacity: faded.
        let mut long = [1.0f32; HRIR_LEN];
        truncate_with_fade(&mut long, len);
        assert!(long[len..].iter().all(|&x| x == 0.0));
        assert_eq!(long[len - TRUNCATION_FADE_TAPS - 1], 1.0);
        let tail = &long[len - TRUNCATION_FADE_TAPS..len];
        assert!(
            tail.windows(2).all(|w| w[1] < w[0]),
            "fade not monotonic: {tail:?}"
        );
        assert!(tail[0] > 0.9 && *tail.last().unwrap() < 0.1, "{tail:?}");
        // Silent past the cut: bit-identical.
        let mut short = [0.0f32; HRIR_LEN];
        for (i, v) in short.iter_mut().enumerate().take(len) {
            *v = (i as f32 * 0.37).sin();
        }
        let before = short;
        truncate_with_fade(&mut short, len);
        assert_eq!(short, before);
    }

    #[test]
    fn right_source_is_louder_in_right_ear() {
        let set = HrirSet::synthetic(48_000);
        let mut p = HrirPair::zeroed();
        set.at(90.0, 0.0, &mut p);
        assert!(energy(&p.right) > energy(&p.left) * 2.0);
    }

    #[test]
    fn front_source_is_symmetric() {
        let set = HrirSet::synthetic(48_000);
        let mut p = HrirPair::zeroed();
        set.at(0.0, 0.0, &mut p);
        assert!((energy(&p.left) - energy(&p.right)).abs() < 1e-4);
    }

    #[test]
    fn interpolation_is_continuous_across_cells() {
        // A 0.1° step either side of a grid node must not jump the response.
        let set = HrirSet::synthetic(48_000);
        let mut a = HrirPair::zeroed();
        let mut b = HrirPair::zeroed();
        set.at(29.99, 5.0, &mut a);
        set.at(30.01, 5.0, &mut b);
        let max_diff = a
            .left
            .iter()
            .zip(b.left.iter())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(max_diff < 1e-3, "discontinuity {max_diff}");
    }

    #[test]
    fn azimuth_wraps_at_360() {
        let set = HrirSet::synthetic(48_000);
        let mut a = HrirPair::zeroed();
        let mut b = HrirPair::zeroed();
        set.at(1.0, 0.0, &mut a);
        set.at(361.0, 0.0, &mut b);
        for n in 0..HRIR_LEN {
            assert!((a.left[n] - b.left[n]).abs() < 1e-6);
        }
    }
}
