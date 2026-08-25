//! Runtime-selectable crossover engine.
//!
//! The render path carries one bank + one per-channel state, whichever
//! implementation the `crossover_type` live option selects:
//!
//! * [`LR4CrossoverBank`] — IIR Linkwitz-Riley: zero latency, magnitude-flat
//!   sum with a phase rotation around every cutoff.
//! * [`FirCrossoverBank`] — linear-phase FIR: the band sum is a pure delay
//!   (flat magnitude AND phase), at the price of a constant latency reported
//!   by [`CrossoverBank::latency_samples`].
//!
//! States are engine-specific, so they travel as [`CrossoverStates`] and are
//! (re)created through [`CrossoverBank::ensure_states`] whenever the bank they
//! were built for changed shape or implementation.

use super::filter::{BiquadState, LR4CrossoverBank, SmallBands};
use super::fir::{FirCrossoverBank, FirCrossoverState};

/// The active crossover implementation (see the module doc).
pub enum CrossoverBank {
    Lr4(LR4CrossoverBank),
    Fir(FirCrossoverBank),
}

/// Per-channel filter memory for whichever engine [`CrossoverBank`] holds.
pub enum CrossoverStates {
    Lr4(Vec<BiquadState>),
    /// Boxed: the FIR state owns FFT scratch and history buffers, far larger
    /// than the LR4 variant's Vec header.
    Fir(Box<FirCrossoverState>),
}

impl CrossoverBank {
    /// Number of output bands.
    pub fn num_bands(&self) -> usize {
        match self {
            Self::Lr4(bank) => bank.num_bands,
            Self::Fir(bank) => bank.num_bands,
        }
    }

    /// Constant input→output delay of the filtered path, in samples. Zero for
    /// the IIR engine. Unfiltered signal paths mixed alongside a filtered one
    /// must be delayed by this amount to stay time-aligned.
    pub fn latency_samples(&self) -> usize {
        match self {
            Self::Lr4(_) => 0,
            Self::Fir(bank) => bank.latency_samples(),
        }
    }

    /// Allocate fresh filter memory for one channel.
    pub fn make_states(&self) -> CrossoverStates {
        match self {
            Self::Lr4(bank) => {
                CrossoverStates::Lr4(vec![BiquadState::default(); bank.state_count()])
            }
            Self::Fir(bank) => CrossoverStates::Fir(Box::new(bank.make_state())),
        }
    }

    /// Do these states belong to a bank of this exact shape?
    pub fn states_compatible(&self, states: &CrossoverStates) -> bool {
        match (self, states) {
            (Self::Lr4(bank), CrossoverStates::Lr4(s)) => s.len() == bank.state_count(),
            (Self::Fir(bank), CrossoverStates::Fir(s)) => bank.state_compatible(s),
            _ => false,
        }
    }

    /// Return `slot`'s states, (re)creating them when absent or built for a
    /// different bank shape/engine.
    pub fn ensure_states<'a>(
        &self,
        slot: &'a mut Option<CrossoverStates>,
    ) -> &'a mut CrossoverStates {
        let compatible = slot.as_ref().is_some_and(|s| self.states_compatible(s));
        if !compatible {
            *slot = Some(self.make_states());
        }
        slot.as_mut().expect("just ensured")
    }

    /// Split `input` into `num_bands()` band samples using `states`.
    ///
    /// `states` must come from [`Self::ensure_states`] on this bank; on an
    /// engine mismatch (unreachable by construction) the sample passes through
    /// unsplit rather than panicking in the audio path.
    #[inline]
    pub fn process_sample(&self, input: f32, states: &mut CrossoverStates) -> SmallBands {
        match (self, states) {
            (Self::Lr4(bank), CrossoverStates::Lr4(s)) => bank.process_sample(input, s),
            (Self::Fir(bank), CrossoverStates::Fir(s)) => bank.process_sample(input, s),
            _ => {
                debug_assert!(false, "crossover states do not match the active bank");
                SmallBands::single(input)
            }
        }
    }

    /// Split a whole mono block into reusable per-band scratch buffers. Same
    /// contract as [`LR4CrossoverBank::process_block`].
    pub fn process_block<F>(
        &self,
        input_len: usize,
        states: &mut CrossoverStates,
        bands_out: &mut [Vec<f32>],
        sample_at: F,
    ) where
        F: FnMut(usize) -> f32,
    {
        match (self, states) {
            (Self::Lr4(bank), CrossoverStates::Lr4(s)) => {
                bank.process_block(input_len, s, bands_out, sample_at)
            }
            (Self::Fir(bank), CrossoverStates::Fir(s)) => {
                bank.process_block(input_len, s, bands_out, sample_at)
            }
            _ => {
                debug_assert!(false, "crossover states do not match the active bank");
                for band in bands_out.iter_mut().take(self.num_bands()) {
                    band.resize(input_len, 0.0);
                }
            }
        }
    }
}

impl CrossoverStates {
    /// Zero the filter memory in place (no reallocation), so a new signal
    /// never splices into the previous one's tail.
    pub fn reset(&mut self) {
        match self {
            Self::Lr4(states) => {
                for s in states.iter_mut() {
                    *s = BiquadState::default();
                }
            }
            Self::Fir(state) => state.reset(),
        }
    }
}

/// Fixed whole-sample delay line — compensates unfiltered paths for the FIR
/// crossover's constant latency. Zero steady-state allocations.
pub struct IntegerDelay {
    buf: Vec<f32>,
    pos: usize,
}

impl IntegerDelay {
    /// A delay of exactly `delay` samples (`delay ≥ 1`; use no delay line at
    /// all for zero).
    pub fn new(delay: usize) -> Self {
        Self {
            buf: vec![0.0; delay.max(1)],
            pos: 0,
        }
    }

    /// The configured delay in samples.
    pub fn delay(&self) -> usize {
        self.buf.len()
    }

    /// Push one sample in, take the sample from `delay()` samples ago out.
    #[inline]
    pub fn push(&mut self, input: f32) -> f32 {
        let out = self.buf[self.pos];
        self.buf[self.pos] = input;
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_delay_delays_by_exactly_n() {
        let mut d = IntegerDelay::new(3);
        let out: Vec<f32> = (1..=6).map(|v| d.push(v as f32)).collect();
        assert_eq!(out, vec![0.0, 0.0, 0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn ensure_states_recreates_on_engine_switch() {
        let lr4 = CrossoverBank::Lr4(LR4CrossoverBank::new(&[120.0], 48000));
        let fir = CrossoverBank::Fir(FirCrossoverBank::with_taps(&[120.0], 48000, 1023, 90.0));
        let mut slot = None;
        lr4.ensure_states(&mut slot);
        assert!(matches!(slot, Some(CrossoverStates::Lr4(_))));
        assert!(!fir.states_compatible(slot.as_ref().unwrap()));
        fir.ensure_states(&mut slot);
        assert!(matches!(slot, Some(CrossoverStates::Fir(_))));
        assert!(fir.states_compatible(slot.as_ref().unwrap()));
    }

    /// Both engines agree on the dispatch surface: same band count for the
    /// same cutoffs, and the enum reports latency only for the FIR engine.
    #[test]
    fn dispatch_reports_engine_properties() {
        let lr4 = CrossoverBank::Lr4(LR4CrossoverBank::new(&[80.0, 2000.0], 48000));
        let fir = CrossoverBank::Fir(FirCrossoverBank::with_taps(
            &[80.0, 2000.0],
            48000,
            1023,
            90.0,
        ));
        assert_eq!(lr4.num_bands(), 3);
        assert_eq!(fir.num_bands(), 3);
        assert_eq!(lr4.latency_samples(), 0);
        assert!(fir.latency_samples() > 0);

        let mut slot = None;
        let states = fir.ensure_states(&mut slot);
        let bands = fir.process_sample(1.0, states);
        assert_eq!(bands.len(), 3);
    }
}
