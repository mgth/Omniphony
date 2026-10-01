//! How the servo estimates the source's position and rate from the capture
//! point's readings.
//!
//! - **`own`** (orender clocks the source): the readings are exact, so a
//!   [`Dll`] filters the timestamps and learns the rate.
//! - **`follow`** (the source has its own clock and its bytes arrive late by
//!   a one-sided jitter): an [`ArrivalEnvelope`] first fits the source's line
//!   to the earliest arrivals (upper convex hull), which removes the jitter
//!   without biasing the phase or the rate at any drift. The fitted line
//!   still steps when the hull edge it rests on changes, so a DLL tracks it
//!   to hand the servo a position and a rate that move smoothly together.

use crate::dll::{Dll, DllConfig};
use crate::envelope::ArrivalEnvelope;

#[derive(Debug, Clone)]
pub(crate) enum SourceEstimator {
    Dll(Dll),
    /// Boxed: the hull's fixed buffers are ~5 KiB; allocated once, here.
    Envelope {
        fit: Box<ArrivalEnvelope>,
        dll: Dll,
    },
}

impl SourceEstimator {
    pub(crate) fn new(dll: DllConfig, follow: bool, nominal_rate: f64) -> Self {
        if follow {
            Self::Envelope {
                fit: Box::new(ArrivalEnvelope::new(nominal_rate)),
                dll: Dll::new(dll, nominal_rate),
            }
        } else {
            Self::Dll(Dll::new(dll, nominal_rate))
        }
    }

    fn dll(&self) -> &Dll {
        match self {
            Self::Dll(d) | Self::Envelope { dll: d, .. } => d,
        }
    }

    pub(crate) fn reset(&mut self) {
        match self {
            Self::Dll(d) => d.reset(),
            Self::Envelope { fit, dll } => {
                fit.reset();
                dll.reset();
            }
        }
    }

    pub(crate) fn is_tracking(&self) -> bool {
        self.dll().is_tracking()
    }

    pub(crate) fn observe(&mut self, t: f64, received: f64) {
        match self {
            Self::Dll(d) => d.observe(t, received),
            Self::Envelope { fit, dll } => {
                fit.observe(t, received);
                if let Some(p) = fit.position_at(t) {
                    dll.observe(t, p);
                }
            }
        }
    }

    /// The source came back after a gap: same clock, new phase. The hull fit
    /// starts over (the gap breaks the line); the DLL keeps its rate.
    pub(crate) fn restart_phase(&mut self, t: f64, received: f64) {
        match self {
            Self::Dll(d) => d.restart_phase(t, received),
            Self::Envelope { fit, dll } => {
                fit.reset();
                fit.observe(t, received);
                dll.restart_phase(t, fit.position_at(t).unwrap_or(received));
            }
        }
    }

    pub(crate) fn rate(&self) -> Option<f64> {
        self.dll().rate()
    }

    pub(crate) fn position_at(&self, t: f64) -> Option<f64> {
        self.dll().position_at(t)
    }
}
