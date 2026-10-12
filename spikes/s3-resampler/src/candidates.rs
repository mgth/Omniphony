//! Uniform harness interface over the candidates.
//!
//! `ratio` is always input frames per output frame (= rate(S)/rate(D)),
//! applied as a linear ramp from the previous call's value over this call.

use crate::polyphase::{Design, Interp, VarResampler};
use crate::ring::Ring;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, PolynomialDegree, Resampler, Resizable,
    SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

pub trait Candidate {
    fn name(&self) -> String;
    /// Produce exactly `n` frames into `out[..n*ch]`. False on ring underrun.
    fn process(&mut self, ring: &mut Ring, out: &mut [f32], n: usize, ratio: f64) -> bool;
    /// Exact input position of the next output frame, if the candidate
    /// exposes one.
    fn position(&self) -> Option<f64>;
    /// Per-frame positions the next `process(n, ratio)` will use, if exposed.
    fn planned_positions(&self, _n: usize, _ratio: f64, _out: &mut Vec<f64>) -> bool {
        false
    }
    /// rubato advances its index before computing a frame, so frame n sits
    /// at p_n + s_n in the harness's nominal law.
    fn steps_before_sampling(&self) -> bool {
        false
    }
}

pub struct InHouse {
    pub r: VarResampler,
    label: String,
}

impl InHouse {
    pub fn new(label: &str, ch: usize, d: Design, max_out: usize) -> Self {
        InHouse {
            r: VarResampler::new(ch, &d, max_out, 1.2),
            label: label.to_string(),
        }
    }
}

impl Candidate for InHouse {
    fn name(&self) -> String {
        self.label.clone()
    }
    fn process(&mut self, ring: &mut Ring, out: &mut [f32], n: usize, ratio: f64) -> bool {
        self.r.prepare(n, ratio);
        if !ring.read_into(self.r.input_slot()) {
            return false;
        }
        self.r.render(out);
        true
    }
    fn position(&self) -> Option<f64> {
        Some(self.r.position().as_f64())
    }
    fn planned_positions(&self, n: usize, ratio: f64, out: &mut Vec<f64>) -> bool {
        self.r.planned_positions(n, ratio, out);
        true
    }
}

/// rubato 5 `Async` with fixed output size (successor of 0.14's
/// `SincFixedOut` / `FastFixedOut`).
pub struct Rubato5 {
    rs: Async<f32>,
    ch: usize,
    scratch: Vec<f32>,
    chunk: usize,
    label: String,
}

pub enum Rubato5Kind {
    Sinc {
        len: usize,
        os: usize,
        interp: SincInterpolationType,
        window: WindowFunction,
        cutoff: Option<f32>,
    },
    Poly(PolynomialDegree),
}

impl Rubato5 {
    pub fn new(label: &str, ch: usize, kind: Rubato5Kind, max_out: usize) -> Self {
        Self::with_nominal(label, ch, kind, max_out, 1.0)
    }

    /// `nominal`: output rate / input rate the filters are designed for.
    pub fn with_nominal(label: &str, ch: usize, kind: Rubato5Kind, max_out: usize, nominal: f64) -> Self {
        let rs = match kind {
            Rubato5Kind::Sinc {
                len,
                os,
                interp,
                window,
                cutoff,
            } => {
                let p = SincInterpolationParameters {
                    sinc_len: len,
                    f_cutoff: cutoff,
                    oversampling_factor: os,
                    interpolation: interp,
                    window,
                };
                Async::<f32>::new_sinc(nominal, 1.01, &p, max_out, ch, FixedAsync::Output).unwrap()
            }
            Rubato5Kind::Poly(deg) => {
                Async::<f32>::new_poly(nominal, 1.01, deg, max_out, ch, FixedAsync::Output).unwrap()
            }
        };
        let max_in = rs.input_frames_max();
        Rubato5 {
            rs,
            ch,
            scratch: vec![0.0; max_in * ch],
            chunk: max_out,
            label: label.to_string(),
        }
    }
    pub fn output_delay(&self) -> usize {
        self.rs.output_delay()
    }
}

impl Candidate for Rubato5 {
    fn name(&self) -> String {
        self.label.clone()
    }
    fn steps_before_sampling(&self) -> bool {
        true
    }
    fn process(&mut self, ring: &mut Ring, out: &mut [f32], n: usize, ratio: f64) -> bool {
        if n != self.chunk {
            self.rs.set_chunk_size(n).unwrap();
            self.chunk = n;
        }
        // rubato's ratio is output/input
        self.rs.set_resample_ratio(1.0 / ratio, true).unwrap();
        let need = self.rs.input_frames_next();
        let c = self.ch;
        if !ring.read_into(&mut self.scratch[..need * c]) {
            return false;
        }
        let input = InterleavedSlice::new(&self.scratch[..need * c], c, need).unwrap();
        let mut output = InterleavedSlice::new_mut(&mut out[..n * c], c, n).unwrap();
        let (_, produced) = self.rs.process_into_buffer(&input, &mut output, None).unwrap();
        debug_assert_eq!(produced, n);
        true
    }
    fn position(&self) -> Option<f64> {
        None
    }
}

pub fn inhouse_designs() -> Vec<(&'static str, Design)> {
    vec![
        ("ih_t64_h32", Design::RECOMMENDED),
        (
            "ih_t96_h32",
            Design {
                taps: 96,
                segments: 32,
                beta: 14.0,
                cutoff: 1.0,
                interp: Interp::Hermite,
            },
        ),
        (
            "ih_t48_h32",
            Design {
                taps: 48,
                segments: 32,
                beta: 12.0,
                cutoff: 1.0,
                interp: Interp::Hermite,
            },
        ),
        (
            "ih_t32_h32",
            Design {
                taps: 32,
                segments: 32,
                beta: 9.0,
                cutoff: 1.0,
                interp: Interp::Hermite,
            },
        ),
        (
            "ih_t64_l256",
            Design {
                taps: 64,
                segments: 256,
                beta: 14.0,
                cutoff: 1.0,
                interp: Interp::Linear,
            },
        ),
        (
            "ih_t64_l1024",
            Design {
                taps: 64,
                segments: 1024,
                beta: 14.0,
                cutoff: 1.0,
                interp: Interp::Linear,
            },
        ),
    ]
}

pub fn all_candidates(ch: usize, max_out: usize) -> Vec<Box<dyn Candidate>> {
    let mut v: Vec<Box<dyn Candidate>> = Vec::new();
    for (name, d) in inhouse_designs() {
        v.push(Box::new(InHouse::new(name, ch, d, max_out)));
    }
    // Today's parameters (256 taps, 256x oversampling, linear, BH2, fc 0.95).
    v.push(Box::new(Rubato5::new(
        "rb5_sinc256_lin",
        ch,
        Rubato5Kind::Sinc {
            len: 256,
            os: 256,
            interp: SincInterpolationType::Linear,
            window: WindowFunction::BlackmanHarris2,
            cutoff: Some(0.95),
        },
        max_out,
    )));
    // rubato 5 defaults (256 taps, auto cutoff, 128x, cubic).
    v.push(Box::new(Rubato5::new(
        "rb5_sinc256_cub",
        ch,
        Rubato5Kind::Sinc {
            len: 256,
            os: 128,
            interp: SincInterpolationType::Cubic,
            window: WindowFunction::BlackmanHarris2,
            cutoff: None,
        },
        max_out,
    )));
    // A cheaper rubato sinc, comparable tap count to the in-house design.
    v.push(Box::new(Rubato5::new(
        "rb5_sinc64_cub",
        ch,
        Rubato5Kind::Sinc {
            len: 64,
            os: 128,
            interp: SincInterpolationType::Cubic,
            window: WindowFunction::BlackmanHarris2,
            cutoff: None,
        },
        max_out,
    )));
    v.push(Box::new(Rubato5::new(
        "rb5_poly_septic",
        ch,
        Rubato5Kind::Poly(PolynomialDegree::Septic),
        max_out,
    )));
    v.push(Box::new(Rubato5::new(
        "rb5_poly_cubic",
        ch,
        Rubato5Kind::Poly(PolynomialDegree::Cubic),
        max_out,
    )));
    v
}
