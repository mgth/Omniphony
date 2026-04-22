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

/// Derive crossover bands from the `freq_low` and `freq_high` fields of spatializable speakers.
///
/// A speaker is included in band `[lo, hi)` when its usable range overlaps the band:
/// `freq_low.unwrap_or(0.0) < hi && freq_high.unwrap_or(f32::INFINITY) >= lo`.
///
/// Returns a single all-inclusive band when no speaker defines a finite crossover edge,
/// which preserves the existing single-backend rendering behaviour.
pub fn compute_bands(layout: &SpeakerLayout) -> Vec<FreqBand> {
    let mut cutoffs: Vec<f32> = layout
        .speakers
        .iter()
        .filter(|s| s.spatialize)
        .flat_map(|speaker| [speaker.freq_low, speaker.freq_high])
        .flatten()
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
                .filter(|(_, s)| {
                    if !s.spatialize {
                        return false;
                    }
                    let speaker_lo = s.freq_low.unwrap_or(0.0);
                    let speaker_hi = s.freq_high.unwrap_or(f32::INFINITY);
                    speaker_lo < hi && speaker_hi >= lo
                })
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

#[cfg(test)]
mod tests {
    use super::compute_bands;
    use crate::speaker_layout::{Speaker, SpeakerLayout};

    fn spatial_speaker(name: &str) -> Speaker {
        Speaker::new(name, 0.0, 0.0)
    }

    fn layout(speakers: Vec<Speaker>) -> SpeakerLayout {
        SpeakerLayout {
            radius_m: 1.0,
            speakers,
        }
    }

    #[test]
    fn returns_single_band_without_cutoffs() {
        let bands = compute_bands(&layout(vec![spatial_speaker("full")]));
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].low_hz, 0.0);
        assert!(bands[0].high_hz.is_infinite());
        assert_eq!(bands[0].speaker_indices, vec![0]);
    }

    #[test]
    fn freq_low_only_keeps_overlap_routing() {
        let bands = compute_bands(&layout(vec![
            spatial_speaker("full"),
            spatial_speaker("mid").with_freq_low(80.0),
        ]));
        assert_eq!(bands.len(), 2);
        assert_eq!(bands[0].speaker_indices, vec![0]);
        assert_eq!(bands[1].speaker_indices, vec![0, 1]);
    }

    #[test]
    fn freq_high_only_limits_upper_bands() {
        let bands = compute_bands(&layout(vec![
            spatial_speaker("top"),
            spatial_speaker("low").with_freq_high(120.0),
            spatial_speaker("super").with_freq_low(150.0),
        ]));
        assert_eq!(bands.len(), 3);
        assert_eq!(bands[0].speaker_indices, vec![0, 1]);
        assert_eq!(bands[1].speaker_indices, vec![0, 1]);
        assert_eq!(bands[2].speaker_indices, vec![0, 2]);
    }

    #[test]
    fn mixed_cutoffs_follow_overlap_logic() {
        let bands = compute_bands(&layout(vec![
            spatial_speaker("sub").with_freq_high(80.0),
            spatial_speaker("mid")
                .with_freq_low(120.0)
                .with_freq_high(200.0),
            spatial_speaker("top").with_freq_low(250.0),
        ]));
        assert_eq!(bands.len(), 5);
        assert_eq!((bands[0].low_hz, bands[0].high_hz), (0.0, 80.0));
        assert_eq!(bands[0].speaker_indices, vec![0]);
        assert_eq!((bands[1].low_hz, bands[1].high_hz), (80.0, 120.0));
        assert_eq!(bands[1].speaker_indices, vec![0]);
        assert_eq!((bands[2].low_hz, bands[2].high_hz), (120.0, 200.0));
        assert_eq!(bands[2].speaker_indices, vec![1]);
        assert_eq!((bands[3].low_hz, bands[3].high_hz), (200.0, 250.0));
        assert_eq!(bands[3].speaker_indices, vec![1]);
        assert_eq!((bands[4].low_hz, bands[4].high_hz), (250.0, f32::INFINITY));
        assert_eq!(bands[4].speaker_indices, vec![2]);
    }
}
