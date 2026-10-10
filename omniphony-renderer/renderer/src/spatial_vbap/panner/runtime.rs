use super::*;

impl VbapPanner {
    // ── Constructors ─────────────────────────────────────────────────────────

    /// Create a VBAP panner from speaker directions (azimuth, elevation degrees).
    ///
    /// `az_res_deg` / `el_res_deg` are validated for range (kept for API parity),
    /// but the panner no longer precomputes any grid — gains are computed directly
    /// from the triangulation. `_spread` is unused (the initial-spread table is
    /// gone); all precomputation now lives in the evaluation layer.
    ///
    /// `mode` selects the out-of-hull rendering strategy; it is baked at
    /// construction because `VirtualPoles` shapes the triangulation itself.
    /// The SAF backend (feature `saf_vbap`) ignores it and keeps its own
    /// out-of-hull behaviour.
    pub fn new(
        speaker_dirs_deg: &[[f32; 2]],
        az_res_deg: i32,
        el_res_deg: i32,
        _spread: f32,
        mode: crate::spatial_vbap::OutOfHullMode,
    ) -> Result<Self, String> {
        let n_speakers = speaker_dirs_deg.len();
        if n_speakers < 3 {
            return Err("VBAP requires at least 3 speakers".to_string());
        }
        if !(1..=360).contains(&az_res_deg) {
            return Err("Azimuth resolution must be between 1 and 360 degrees".to_string());
        }
        if !(1..=180).contains(&el_res_deg) {
            return Err("Elevation resolution must be between 1 and 180 degrees".to_string());
        }

        #[cfg(not(feature = "saf_vbap"))]
        {
            let source =
                native_backend::NativeVbapLayout::from_speaker_dirs(speaker_dirs_deg, mode)?;
            Ok(VbapPanner {
                n_triangles: source.n_faces,
                n_virtual_centres: source.n_centres,
                n_speakers,
                allow_negative_z: true,
                source,
            })
        }
        #[cfg(feature = "saf_vbap")]
        {
            let _ = mode; // SAF keeps its own out-of-hull behaviour
            let layout = saf_backend::SpartaVbapLayout::from_speaker_dirs(speaker_dirs_deg)?;
            Ok(VbapPanner {
                n_triangles: layout.n_faces as usize,
                n_virtual_centres: 0,
                n_speakers,
                allow_negative_z: true,
                speaker_dirs_deg: speaker_dirs_deg.to_vec(),
            })
        }
    }

    // ── Builder methods ──────────────────────────────────────────────────────

    pub fn with_negative_z(mut self, allow_negative_z: bool) -> Self {
        self.allow_negative_z = allow_negative_z;
        self
    }

    pub fn allow_negative_z(&self) -> bool {
        self.allow_negative_z
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    pub fn num_speakers(&self) -> usize {
        self.n_speakers
    }

    pub fn num_triangles(&self) -> usize {
        self.n_triangles
    }

    /// Virtual loudspeakers at the centre of coplanar hull faces.
    pub fn num_virtual_centres(&self) -> usize {
        self.n_virtual_centres
    }

    // ── Direct gain computation ──────────────────────────────────────────────

    /// The working memory the `*_into` methods compute on, sized for this
    /// panner's layout. A caller makes one off the render thread, keeps it,
    /// and hands it back on every call.
    pub fn new_scratch(&self) -> VbapScratch {
        VbapScratch {
            #[cfg(not(feature = "saf_vbap"))]
            native: self.source.new_scratch(),
        }
    }

    /// Write the panning gains for a source at an ADM cartesian position into
    /// `out`, one per speaker of the layout, on a scratch from
    /// [`Self::new_scratch`]. Allocates nothing (but under `saf_vbap`).
    ///
    /// Pure panning gains; distance attenuation / diffuse blending are
    /// applied by the shared decorators in `render_backend`, not here.
    pub fn gains_cartesian_into(
        &self,
        x: f32,
        y: f32,
        z: f32,
        spread: f32,
        scratch: &mut VbapScratch,
        out: &mut [f32],
    ) {
        let z = if self.allow_negative_z { z } else { z.max(0.0) };
        let (azimuth, elevation, _distance) = adm_to_spherical(x, y, z);
        self.gains_direct(azimuth, elevation, spread, scratch, out)
    }

    /// Write the panning gains for a source direction with a spread into
    /// `out`, as [`Self::gains_cartesian_into`] does for a position, but for a
    /// bare direction: nothing clamps it to the horizon. The volumetric
    /// backend pans the direction opposite an object with it.
    pub fn gains_spread_into(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        spread: f32,
        scratch: &mut VbapScratch,
        out: &mut [f32],
    ) {
        self.gains_direct(azimuth_deg, elevation_deg, spread, scratch, out)
    }

    /// [`Self::gains_cartesian_into`] in a vector and on a scratch of its
    /// own: for tests and one-off queries, off the render thread.
    pub fn get_gains_cartesian(&self, x: f32, y: f32, z: f32, spread: f32) -> Vec<f32> {
        let mut gains = vec![0.0; self.n_speakers];
        self.gains_cartesian_into(x, y, z, spread, &mut self.new_scratch(), &mut gains);
        gains
    }

    /// Panning gains for a source direction (no spread), in a vector and on
    /// a scratch of their own: for tests and one-off queries.
    pub fn get_gains(&self, azimuth_deg: f32, elevation_deg: f32) -> Vec<f32> {
        let mut gains = vec![0.0; self.n_speakers];
        self.gains_direct(
            azimuth_deg,
            elevation_deg,
            0.0,
            &mut self.new_scratch(),
            &mut gains,
        );
        gains
    }

    /// Direct triangulation-based VBAP gains. The native backend stores the
    /// triangulated layout (plain data) and computes on the caller's scratch,
    /// allocating nothing; the SAF backend rebuilds the layout per call
    /// because its FFI handle is not `Sync` and the evaluation layer samples
    /// the panner in parallel, and SAF allocates the gains it returns.
    ///
    /// One gain per speaker on both sides; a shorter `out` takes what fits.
    #[inline]
    fn gains_direct(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        spread: f32,
        scratch: &mut VbapScratch,
        out: &mut [f32],
    ) {
        #[cfg(not(feature = "saf_vbap"))]
        self.source
            .vbap_gains_into(azimuth_deg, elevation_deg, spread, &mut scratch.native, out);
        #[cfg(feature = "saf_vbap")]
        {
            let _ = scratch;
            let gains = saf_backend::SpartaVbapLayout::from_speaker_dirs(&self.speaker_dirs_deg)
                .expect("failed to initialize SAF VBAP layout")
                .vbap_gains(azimuth_deg, elevation_deg, spread)
                .expect("vbap3D failed while computing gains");
            for (out, &gain) in out.iter_mut().zip(&gains) {
                *out = gain;
            }
        }
    }
}
