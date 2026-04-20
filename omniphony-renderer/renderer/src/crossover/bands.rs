use crate::speaker_layout::SpeakerLayout;

/// A frequency band with the indices of speakers capable of reproducing it.
#[derive(Debug, Clone)]
pub struct FreqBand {
    /// Lower bound in Hz (0.0 for the sub band).
    pub low_hz: f32,
    /// Upper bound in Hz (`f32::INFINITY` for the top band).
    pub high_hz: f32,
    /// Indices into the full `SpeakerLayout::speakers` vec.
    pub speaker_indices: Vec<usize>,
}

/// Derive crossover bands from the `freq_low` fields of spatializable speakers.
///
/// A speaker is included in band `[lo, hi)` when `freq_low.unwrap_or(0.0) <= lo`
/// (i.e. it can reproduce down to `lo` Hz).
///
/// Returns a single all-inclusive band when no speaker defines `freq_low`, which
/// preserves the existing single-backend rendering behaviour.
pub fn compute_bands(layout: &SpeakerLayout) -> Vec<FreqBand> {
    let mut cutoffs: Vec<f32> = layout
        .speakers
        .iter()
        .filter(|s| s.spatialize)
        .filter_map(|s| s.freq_low)
        .collect();

    cutoffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    cutoffs.dedup_by(|a, b| (*a - *b).abs() < 0.1);

    if cutoffs.is_empty() {
        let indices = layout
            .speakers
            .iter()
            .enumerate()
            .filter(|(_, s)| s.spatialize)
            .map(|(i, _)| i)
            .collect();
        return vec![FreqBand {
            low_hz: 0.0,
            high_hz: f32::INFINITY,
            speaker_indices: indices,
        }];
    }

    let edges: Vec<f32> = std::iter::once(0.0_f32)
        .chain(cutoffs.iter().copied())
        .chain(std::iter::once(f32::INFINITY))
        .collect();

    edges
        .windows(2)
        .map(|w| {
            let lo = w[0];
            let hi = w[1];
            let speaker_indices = layout
                .speakers
                .iter()
                .enumerate()
                .filter(|(_, s)| s.spatialize && s.freq_low.unwrap_or(0.0) <= lo)
                .map(|(i, _)| i)
                .collect();
            FreqBand {
                low_hz: lo,
                high_hz: hi,
                speaker_indices,
            }
        })
        .collect()
}
