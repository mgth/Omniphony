/// One set of gains per render band, back to back in one buffer:
/// `[band][speaker]`, each set one gain per speaker of the layout.
///
/// The buffer is sized for the layout and its band count the first time it is
/// shaped, and reused from then on: reading an object's gains into it
/// allocates nothing, and a set is as wide as the layout, not as a fixed
/// capacity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BandGains {
    gains: Vec<f32>,
    n_bands: usize,
    num_speakers: usize,
}

impl BandGains {
    /// No band at all: the state before the first [`shape`](Self::shape).
    pub const fn new() -> Self {
        Self {
            gains: Vec::new(),
            n_bands: 0,
            num_speakers: 0,
        }
    }

    /// Make this hold `n_bands` sets of `num_speakers` gains. A buffer that
    /// already has that shape — the steady state — is left as it is, contents
    /// included: whoever shapes it writes every gain next. Another shape
    /// starts from silence.
    #[inline]
    pub fn shape(&mut self, n_bands: usize, num_speakers: usize) {
        if self.n_bands != n_bands || self.num_speakers != num_speakers {
            self.gains.clear();
            self.gains.resize(n_bands * num_speakers, 0.0);
            self.n_bands = n_bands;
            self.num_speakers = num_speakers;
        }
    }

    /// Hold nothing, keeping the allocation.
    pub fn clear(&mut self) {
        self.gains.clear();
        self.n_bands = 0;
        self.num_speakers = 0;
    }

    pub fn n_bands(&self) -> usize {
        self.n_bands
    }

    /// Gains per band: the speaker count of the layout.
    pub fn num_speakers(&self) -> usize {
        self.num_speakers
    }

    /// True when there is no band.
    pub fn is_empty(&self) -> bool {
        self.n_bands == 0
    }

    pub fn same_shape(&self, other: &Self) -> bool {
        self.n_bands == other.n_bands && self.num_speakers == other.num_speakers
    }

    /// Become a copy of `other`, in this buffer's own allocation when it is
    /// large enough.
    #[inline]
    pub fn copy_from(&mut self, other: &Self) {
        self.gains.clear();
        self.gains.extend_from_slice(&other.gains);
        self.n_bands = other.n_bands;
        self.num_speakers = other.num_speakers;
    }

    /// The sets in band order, one slice of `num_speakers` gains each. A
    /// layout without speakers has nothing to iterate.
    #[inline]
    pub fn bands(&self) -> std::slice::ChunksExact<'_, f32> {
        self.gains.chunks_exact(self.num_speakers.max(1))
    }

    /// [`bands`](Self::bands), to write them.
    #[inline]
    pub fn bands_mut(&mut self) -> std::slice::ChunksExactMut<'_, f32> {
        self.gains.chunks_exact_mut(self.num_speakers.max(1))
    }

    /// Every gain, `[band][speaker]`.
    #[inline]
    pub fn as_slice(&self) -> &[f32] {
        &self.gains
    }

    /// [`as_slice`](Self::as_slice), to write it.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.gains
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shape_is_kept_with_its_contents_and_another_starts_silent() {
        let mut gains = BandGains::new();
        assert!(gains.is_empty() && gains.bands().next().is_none());

        gains.shape(2, 3);
        assert_eq!((gains.n_bands(), gains.num_speakers()), (2, 3));
        assert_eq!(gains.as_slice(), [0.0; 6]);
        gains
            .as_mut_slice()
            .copy_from_slice(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        gains.shape(2, 3);
        assert_eq!(
            gains.bands().collect::<Vec<_>>(),
            [&[1.0, 2.0, 3.0][..], &[4.0, 5.0, 6.0][..]]
        );

        // Same length, another shape: not the same gains.
        gains.shape(3, 2);
        assert_eq!(gains.as_slice(), [0.0; 6]);
        assert_eq!(gains.bands().count(), 3);
    }

    #[test]
    fn a_copy_takes_the_shape_and_the_gains() {
        let mut source = BandGains::new();
        source.shape(1, 2);
        source.as_mut_slice().copy_from_slice(&[0.25, 0.75]);
        let mut copy = BandGains::new();
        copy.shape(4, 4);
        assert!(!copy.same_shape(&source));
        copy.copy_from(&source);
        assert_eq!(copy, source);
        copy.clear();
        assert!(copy.is_empty() && copy.as_slice().is_empty());
    }

    #[test]
    fn a_layout_without_speakers_has_bands_and_nothing_to_iterate() {
        let mut gains = BandGains::new();
        gains.shape(4, 0);
        assert_eq!(gains.n_bands(), 4);
        assert!(!gains.is_empty());
        assert!(gains.bands().next().is_none());
    }
}
