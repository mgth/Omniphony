//! Head-related impulse responses (HRIR) and their interpolation grid.
//!
//! The renderer convolves each ear with a per-direction FIR. The *source* of
//! those FIRs is abstracted by [`HrirProvider`] so a measured set (KEMAR / SOFA,
//! M3) can replace the built-in [`SyntheticHrir`] without touching the render
//! path. The interaural *delay* is intentionally **not** baked into these FIRs
//! (it is applied separately as a per-ear delay line, see [`super::itd`]), which
//! keeps all grid FIRs time-aligned so they can be linearly interpolated without
//! comb-filtering.

/// FIR length per ear. 128 taps @ 48 kHz (≈2.7 ms) captures the synthetic
/// head-shadow response and a time-aligned measured (KEMAR) HRIR.
pub const HRIR_LEN: usize = 128;

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

/// Built-in analytic head model: per-ear broadband gain (ILD) plus a one-pole
/// low-pass whose cutoff drops as the source moves contralateral (head shadow).
/// Self-contained (no measured data); a stand-in until a measured set is loaded.
pub struct SyntheticHrir;

impl SyntheticHrir {
    /// Minimum per-ear gain at full shadow (≈ −14 dB).
    const GAIN_MIN: f32 = 0.2;
    /// Low-pass cutoff at full shadow (Hz).
    const FC_SHADOW: f32 = 1200.0;

    fn one_pole_lowpass_ir(gain: f32, cutoff_hz: f32, sample_rate: u32, out: &mut [f32; HRIR_LEN]) {
        let fc = cutoff_hz.clamp(200.0, sample_rate as f32 * 0.49);
        let a = (-2.0 * std::f32::consts::PI * fc / sample_rate as f32).exp();
        // h[n] = gain*(1-a)*a^n — DC gain == `gain`.
        let mut p = gain * (1.0 - a);
        for slot in out.iter_mut() {
            *slot = p;
            p *= a;
        }
    }
}

impl HrirProvider for SyntheticHrir {
    fn render(&self, az_deg: f32, el_deg: f32, sample_rate: u32) -> HrirPair {
        let az = az_deg.to_radians();
        let el = el_deg.to_radians();
        // Lateral position: +1 fully right, −1 fully left, 0 median plane.
        let lateral = (az.sin() * el.cos()).clamp(-1.0, 1.0);
        let exposure_r = 0.5 * (1.0 + lateral);
        let exposure_l = 0.5 * (1.0 - lateral);

        let open = sample_rate as f32 * 0.45;
        let gain = |e: f32| Self::GAIN_MIN + (1.0 - Self::GAIN_MIN) * e;
        let cutoff = |e: f32| Self::FC_SHADOW + (open - Self::FC_SHADOW) * e;

        let mut pair = HrirPair::zeroed();
        Self::one_pole_lowpass_ir(
            gain(exposure_l),
            cutoff(exposure_l),
            sample_rate,
            &mut pair.left,
        );
        Self::one_pole_lowpass_ir(
            gain(exposure_r),
            cutoff(exposure_r),
            sample_rate,
            &mut pair.right,
        );
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

impl HrirProvider for ParametricPinnaHrir {
    fn render(&self, az_deg: f32, el_deg: f32, sample_rate: u32) -> HrirPair {
        // Base: the analytic head shadow + ILD (identical to SyntheticHrir).
        let mut pair = SyntheticHrir.render(az_deg, el_deg, sample_rate);
        if self.depth <= 1e-4 {
            return pair; // degenerates to the plain synthetic head model
        }

        // Brown & Duda (8): τ_n = A_n·cos(θ/2)·sin(D_n·(90°−φ)) + B_n, with θ the
        // azimuth and φ the elevation. cos(az/2) is 1 ahead and 0 behind, so the
        // delay spread — hence the notch pattern — collapses behind the head:
        // that is the front/back cue. (8) is validated for the frontal half
        // space only; the rear is our extrapolation.
        let az = az_deg.to_radians();
        let front = (0.5 * az).cos().clamp(0.0, 1.0);
        let el_term = (90.0 - el_deg).to_radians(); // 0 overhead, grows downward
        let fs_scale = sample_rate as f32 / Self::REF_FS;
        let mut taus = [0.0f32; 5];
        for n in 0..5 {
            let d = Self::B[n] + Self::A[n] * front * (self.d[n] * el_term).sin();
            taus[n] = (d * fs_scale).max(0.0);
        }

        pair.left = Self::apply_pinna(&pair.left, &taus, self.depth);
        pair.right = Self::apply_pinna(&pair.right, &taus, self.depth);
        pair
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
    const AZ_STEP_DEG: f32 = 10.0;
    const EL_STEP_DEG: f32 = 10.0;
    const EL_MIN_DEG: f32 = -40.0;
    const EL_MAX_DEG: f32 = 90.0;

    /// Precompute the grid from `provider` at `sample_rate`.
    pub fn new(provider: &dyn HrirProvider, sample_rate: u32) -> Self {
        let az_count = (360.0 / Self::AZ_STEP_DEG).round() as usize; // wrap: no duplicate at 360
        let el_count =
            ((Self::EL_MAX_DEG - Self::EL_MIN_DEG) / Self::EL_STEP_DEG).round() as usize + 1;
        let mut grid = Vec::with_capacity(az_count * el_count);
        for ei in 0..el_count {
            let el = Self::EL_MIN_DEG + ei as f32 * Self::EL_STEP_DEG;
            for ai in 0..az_count {
                let az = ai as f32 * Self::AZ_STEP_DEG;
                grid.push(provider.render(az, el, sample_rate));
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
            grid,
        }
    }

    /// Convenience constructor for the built-in synthetic model.
    pub fn synthetic(sample_rate: u32) -> Self {
        Self::new(&SyntheticHrir, sample_rate)
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
    /// [`EL_STEP_DEG`](Self::EL_STEP_DEG) — 10° — but `fa`/`fe` below are
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

        for n in 0..HRIR_LEN {
            out.left[n] =
                w00 * p00.left[n] + w10 * p10.left[n] + w01 * p01.left[n] + w11 * p11.left[n];
            out.right[n] =
                w00 * p00.right[n] + w10 * p10.right[n] + w01 * p01.right[n] + w11 * p11.right[n];
        }
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
        // Two directions a hair apart — well inside one lattice step (0.31°).
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
    /// the bound that makes the approximation defensible against a 10° grid.
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
        }
        .render(30.0, 20.0, 48_000);
        let s = SyntheticHrir.render(30.0, 20.0, 48_000);
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
        let s_front = SyntheticHrir.render(0.0, 0.0, 48_000);
        let s_back = SyntheticHrir.render(180.0, 0.0, 48_000);
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

    #[test]
    fn pinna_rd_preset_differs_from_pbnh() {
        // D_n is the model's only per-listener parameter (Table I); off the
        // horizon the two published columns must yield distinct HRIRs.
        let pbnh = ParametricPinnaHrir {
            d: ParametricPinnaHrir::D_PB_NH,
            depth: 1.0,
        }
        .render(0.0, 45.0, 48_000);
        let rd = ParametricPinnaHrir {
            d: ParametricPinnaHrir::D_RD,
            depth: 1.0,
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
