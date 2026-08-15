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

    // ── Direct gain computation ──────────────────────────────────────────────

    /// Compute panning gains for a source at an ADM cartesian position.
    ///
    /// Returns pure panning gains; distance attenuation / diffuse blending are
    /// applied by the shared decorators in `render_backend`, not here.
    pub fn get_gains_cartesian(&self, x: f32, y: f32, z: f32, spread: f32) -> Gains {
        let z = if self.allow_negative_z { z } else { z.max(0.0) };
        let (azimuth, elevation, _distance) = adm_to_spherical(x, y, z);
        self.gains_direct(azimuth, elevation, spread)
    }

    /// Compute panning gains for a source direction (no spread).
    pub fn get_gains(&self, azimuth_deg: f32, elevation_deg: f32) -> Gains {
        self.gains_direct(azimuth_deg, elevation_deg, 0.0)
    }

    /// Direct triangulation-based VBAP gains. The native backend stores the
    /// triangulated layout (plain data); the SAF backend rebuilds it per call
    /// because its FFI handle is not `Sync` and the evaluation layer samples the
    /// panner in parallel.
    #[inline]
    fn gains_direct(&self, azimuth_deg: f32, elevation_deg: f32, spread: f32) -> Gains {
        #[cfg(not(feature = "saf_vbap"))]
        {
            self.source
                .vbap_gains(azimuth_deg, elevation_deg, spread)
                .expect("native vbap3d failed while computing gains")
        }
        #[cfg(feature = "saf_vbap")]
        {
            saf_backend::SpartaVbapLayout::from_speaker_dirs(&self.speaker_dirs_deg)
                .expect("failed to initialize SAF VBAP layout")
                .vbap_gains(azimuth_deg, elevation_deg, spread)
                .expect("vbap3D failed while computing gains")
        }
    }
}
