use super::*;

impl VbapPanner {
    // ── Gain source factory ─────────────────────────────────────────────────

    /// Build the appropriate [`VbapGainSource`] for this panner's feature set.
    ///
    /// - With `saf_vbap`: creates a [`SpartaVbapLayout`] from the stored speaker
    ///   directions (exact FFI triangulation).
    /// - Without `saf_vbap`: creates a [`TableGainSource`] that interpolates from
    ///   the pre-computed polar spread tables.
    fn make_gain_source(&self) -> Result<Box<dyn gain_source::VbapGainSource + '_>, String> {
        #[cfg(feature = "saf_vbap")]
        {
            let dirs = self.speaker_dirs_deg.as_deref().ok_or_else(|| {
                "Direct VBAP layout unavailable (speaker_dirs_deg missing)".to_string()
            })?;
            Ok(Box::new(saf_backend::SpartaVbapLayout::from_speaker_dirs(
                dirs,
            )?))
        }
        #[cfg(not(feature = "saf_vbap"))]
        {
            // If speaker directions are available, use the native VBAP backend
            // for direct gain computation (needed when gtable is empty).
            // Otherwise, fall back to table interpolation.
            if let Some(dirs) = self.speaker_dirs_deg.as_deref() {
                Ok(Box::new(
                    native_backend::NativeVbapLayout::from_speaker_dirs(dirs)?,
                ))
            } else {
                Ok(Box::new(gain_source::TableGainSource::new(self)))
            }
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    #[inline]
    fn cartesian_total_z_points(&self, z_size: usize, z_neg_size: usize) -> usize {
        if self.allow_negative_z && z_neg_size > 0 {
            z_size + z_neg_size
        } else {
            z_size
        }
    }

    #[inline]
    fn table_z_value(&self, zi: usize, z_size: usize, z_neg_size: usize) -> f32 {
        if self.allow_negative_z && z_neg_size > 0 {
            if zi < z_neg_size {
                -1.0 + (zi as f32 / z_neg_size as f32)
            } else {
                let pos_idx = zi - z_neg_size;
                pos_idx as f32 / ((z_size - 1) as f32)
            }
        } else {
            (zi as f32) / ((z_size - 1) as f32)
        }
    }

    #[inline]
    fn uses_full_elevation_grid(&self) -> bool {
        let full = ((180.0 / self.el_res_deg as f32) + 1.5) as usize;
        self.n_el >= full
    }

    #[inline]
    fn elevation_min(&self) -> f32 {
        if self.allow_negative_z { -90.0 } else { 0.0 }
    }

    #[inline]
    fn elevation_range(&self) -> f32 {
        90.0 - self.elevation_min()
    }

    #[inline]
    pub(crate) fn elevation_from_index(&self, idx: usize) -> f32 {
        let base = if self.allow_negative_z {
            (idx as f32 * self.el_res_deg as f32) - 90.0
        } else if self.uses_full_elevation_grid() {
            ((idx as f32 * self.el_res_deg as f32) - 90.0).max(0.0)
        } else {
            idx as f32 * self.el_res_deg as f32
        };
        base.clamp(self.elevation_min(), 90.0)
    }

    #[inline]
    fn recompute_grid_dims(&mut self) {
        self.n_az = ((360.0 / self.az_res_deg as f32) + 1.5) as usize;
        self.n_el = ((self.elevation_range() / self.el_res_deg as f32) + 1.5) as usize;
        self.n_gtable = self.n_az * self.n_el;
    }

    // ── Constructors ────────────────────────────────────────────────────────

    /// Create a new VBAP panner with the specified speaker layout.
    ///
    /// Requires the `saf_vbap` feature for triangulation via SAF FFI.
    #[cfg(feature = "saf_vbap")]
    pub fn new(
        speaker_dirs_deg: &[[f32; 2]],
        az_res_deg: i32,
        el_res_deg: i32,
        spread: f32,
    ) -> Result<Self, String> {
        let n_speakers = speaker_dirs_deg.len();

        if n_speakers < 3 {
            return Err("VBAP requires at least 3 speakers".to_string());
        }

        if az_res_deg < 1 || az_res_deg > 360 {
            return Err("Azimuth resolution must be between 1 and 360 degrees".to_string());
        }

        if el_res_deg < 1 || el_res_deg > 180 {
            return Err("Elevation resolution must be between 1 and 180 degrees".to_string());
        }

        let layout = saf_backend::SpartaVbapLayout::from_speaker_dirs(speaker_dirs_deg)?;
        let n_az = ((360.0 / az_res_deg as f32) + 1.5) as usize;
        let n_el = ((180.0 / el_res_deg as f32) + 1.5) as usize;
        let spread_table = SpreadTable {
            spread,
            gtable: Vec::new(),
        };

        Ok(VbapPanner {
            spread_tables: vec![spread_table],
            spread_resolution: 0.0,
            n_gtable: n_az * n_el,
            n_triangles: layout.n_faces as usize,
            n_speakers,
            az_res_deg,
            el_res_deg,
            n_az,
            n_el,
            table_mode: VbapTableMode::Polar,
            allow_negative_z: true,
            position_interpolation: true,
            cartesian_cache: None,
            speaker_dirs_deg: Some(speaker_dirs_deg.to_vec()),
        })
    }

    #[cfg(feature = "saf_vbap")]
    pub fn new_with_mode(
        speaker_dirs_deg: &[[f32; 2]],
        az_res_deg: i32,
        el_res_deg: i32,
        spread: f32,
        table_mode: VbapTableMode,
    ) -> Result<Self, String> {
        let p = Self::new(speaker_dirs_deg, az_res_deg, el_res_deg, spread)?;
        p.with_table_mode(table_mode)
    }

    /// Create a new VBAP panner using the pure-Rust native backend.
    ///
    /// Used when the `saf_vbap` feature is disabled.
    #[cfg(not(feature = "saf_vbap"))]
    pub fn new(
        speaker_dirs_deg: &[[f32; 2]],
        az_res_deg: i32,
        el_res_deg: i32,
        spread: f32,
    ) -> Result<Self, String> {
        let n_speakers = speaker_dirs_deg.len();

        if n_speakers < 3 {
            return Err("VBAP requires at least 3 speakers".to_string());
        }
        if az_res_deg < 1 || az_res_deg > 360 {
            return Err("Azimuth resolution must be between 1 and 360 degrees".to_string());
        }
        if el_res_deg < 1 || el_res_deg > 180 {
            return Err("Elevation resolution must be between 1 and 180 degrees".to_string());
        }

        let layout = native_backend::NativeVbapLayout::from_speaker_dirs(speaker_dirs_deg)?;
        let n_az = ((360.0 / az_res_deg as f32) + 1.5) as usize;
        let n_el = ((180.0 / el_res_deg as f32) + 1.5) as usize;

        Ok(VbapPanner {
            spread_tables: vec![SpreadTable { spread, gtable: Vec::new() }],
            spread_resolution: 0.0,
            n_gtable: n_az * n_el,
            n_triangles: layout.n_faces,
            n_speakers,
            az_res_deg,
            el_res_deg,
            n_az,
            n_el,
            table_mode: VbapTableMode::Polar,
            allow_negative_z: true,
            position_interpolation: true,
            cartesian_cache: None,
            speaker_dirs_deg: Some(speaker_dirs_deg.to_vec()),
        })
    }

    #[cfg(not(feature = "saf_vbap"))]
    pub fn new_with_mode(
        speaker_dirs_deg: &[[f32; 2]],
        az_res_deg: i32,
        el_res_deg: i32,
        spread: f32,
        table_mode: VbapTableMode,
    ) -> Result<Self, String> {
        let p = Self::new(speaker_dirs_deg, az_res_deg, el_res_deg, spread)?;
        p.with_table_mode(table_mode)
    }

    // ── Builder methods ─────────────────────────────────────────────────────

    pub fn table_mode(&self) -> VbapTableMode {
        self.table_mode
    }

    pub fn with_negative_z(mut self, allow_negative_z: bool) -> Self {
        self.allow_negative_z = allow_negative_z;
        if self.spread_tables.iter().all(|t| t.gtable.is_empty()) {
            self.recompute_grid_dims();
        }
        self.cartesian_cache = None;
        self
    }

    pub fn allow_negative_z(&self) -> bool {
        self.allow_negative_z
    }

    pub fn with_position_interpolation(mut self, enabled: bool) -> Self {
        self.position_interpolation = enabled;
        self
    }

    pub fn position_interpolation(&self) -> bool {
        self.position_interpolation
    }

    pub fn with_table_mode(mut self, table_mode: VbapTableMode) -> Result<Self, String> {
        self.table_mode = table_mode;
        match table_mode {
            VbapTableMode::Polar => {
                self.cartesian_cache = None;
            }
            VbapTableMode::Cartesian {
                x_size,
                y_size,
                z_size,
                z_neg_size,
            } => {
                if x_size < 2 || y_size < 2 || z_size < 2 {
                    return Err("Cartesian table sizes must be >= 2 for X/Y/Z+".to_string());
                }
                self.cartesian_cache =
                    Some(self.build_cartesian_cache(x_size, y_size, z_size, z_neg_size)?);
                log::info!(
                    "Generated cartesian VBAP cache: {}x{}x{}(+{}), {} spread tables",
                    x_size,
                    y_size,
                    z_size,
                    z_neg_size,
                    self.spread_tables.len()
                );
            }
        }
        Ok(self)
    }

    // ── Table generation ────────────────────────────────────────────────────

    fn build_cartesian_cache(
        &self,
        x_size: usize,
        y_size: usize,
        z_size: usize,
        z_neg_size: usize,
    ) -> Result<CartesianCache, String> {
        let total_z_points = self.cartesian_total_z_points(z_size, z_neg_size);
        let grid_points = x_size * y_size * total_z_points;
        let mut tables = Vec::with_capacity(self.spread_tables.len());
        let x_coords: Vec<f32> = (0..x_size)
            .map(|xi| -1.0 + 2.0 * (xi as f32) / ((x_size - 1) as f32))
            .collect();
        let y_coords: Vec<f32> = (0..y_size)
            .map(|yi| -1.0 + 2.0 * (yi as f32) / ((y_size - 1) as f32))
            .collect();
        let z_coords: Vec<f32> = (0..total_z_points)
            .map(|zi| self.table_z_value(zi, z_size, z_neg_size))
            .collect();

        let gain_source = self.make_gain_source()?;

        for table_idx in 0..self.spread_tables.len() {
            let mut table = Vec::with_capacity(grid_points * self.n_speakers);
            let spread = self.spread_tables[table_idx].spread;
            for &z in &z_coords {
                for &y in &y_coords {
                    for &x in &x_coords {
                        let (azimuth, elevation, _) = adm_to_spherical(x, y, z);
                        let gains = gain_source.compute_gains(azimuth, elevation, spread)?;
                        table.extend_from_slice(&gains[..]);
                    }
                }
            }
            tables.push(table);
        }

        Ok(CartesianCache {
            x_size,
            y_size,
            x_coords,
            y_coords,
            z_coords,
            tables,
        })
    }

    // ── Runtime lookup ──────────────────────────────────────────────────────

    pub fn get_gains_cartesian(
        &self,
        x: f32,
        y: f32,
        z: f32,
        spread: f32,
        distance_model: DistanceModel,
    ) -> Gains {
        let z = if self.allow_negative_z { z } else { z.max(0.0) };

        let distance = (x * x + y * y + z * z).sqrt();
        let gains = match self.table_mode {
            VbapTableMode::Polar => {
                let (azimuth, elevation, _) = adm_to_spherical(x, y, z);
                self.get_gains_with_spread(azimuth, elevation, spread)
            }
            VbapTableMode::Cartesian { .. } => self.get_gains_from_cartesian_cache(x, y, z),
        };

        VbapPanner::apply_gain(
            &gains,
            calculate_distance_attenuation(distance, distance_model),
        )
    }

    /// Get VBAP gains with dynamic spread (interpolated between pre-computed tables).
    pub fn get_gains_with_spread(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        spread: f32,
    ) -> Gains {
        // Direct gain computation path — used when the spread table hasn't been
        // pre-computed yet (gtable is empty). Falls back to the appropriate backend.
        if self
            .spread_tables
            .first()
            .map(|t| t.gtable.is_empty())
            .unwrap_or(false)
        {
            #[cfg(feature = "saf_vbap")]
            if let Some(dirs) = self.speaker_dirs_deg.as_deref() {
                let layout = saf_backend::SpartaVbapLayout::from_speaker_dirs(dirs)
                    .expect("failed to initialize direct VBAP layout");
                return layout
                    .vbap_gains(azimuth_deg, elevation_deg, spread)
                    .expect("vbap3D failed while computing gains");
            }
            #[cfg(not(feature = "saf_vbap"))]
            if let Some(dirs) = self.speaker_dirs_deg.as_deref() {
                let layout = native_backend::NativeVbapLayout::from_speaker_dirs(dirs)
                    .expect("failed to initialize native VBAP layout");
                return layout
                    .vbap_gains(azimuth_deg, elevation_deg, spread)
                    .expect("native vbap3d failed while computing gains");
            }
        }

        let (az0_idx, az1_idx, azt) = self.get_azimuth_idx(azimuth_deg);
        let (el0_idx, el1_idx, elt) = self.get_elevation_idx(elevation_deg);
        let (sp0_idx, sp1_idx, spt) = self.get_spread_idx(spread);

        let offset00 = (el0_idx * self.n_az + az0_idx) * self.n_speakers;
        let offset01 = (el1_idx * self.n_az + az0_idx) * self.n_speakers;
        let offset10 = (el0_idx * self.n_az + az1_idx) * self.n_speakers;
        let offset11 = (el1_idx * self.n_az + az1_idx) * self.n_speakers;

        if sp0_idx == sp1_idx {
            return self
                .get_gains_from_4(offset00, offset01, offset10, offset11, azt, elt, sp0_idx);
        }

        let g0 = self.get_gains_from_4(offset00, offset01, offset10, offset11, azt, elt, sp0_idx);
        let g1 = self.get_gains_from_4(offset00, offset01, offset10, offset11, azt, elt, sp1_idx);

        self.interpol(&g0, &g1, spt)
    }

    // ── Cache lookups ───────────────────────────────────────────────────────

    fn get_gains_from_cartesian_cache(&self, x: f32, y: f32, z: f32) -> Gains {
        let Some(cache) = self.cartesian_cache.as_ref() else {
            let z = if self.allow_negative_z { z } else { z.max(0.0) };
            let (azimuth, elevation, _) = adm_to_spherical(x, y, z);
            return self.get_gains_with_spread(azimuth, elevation, 0.0);
        };

        let raw_x = x.clamp(-1.0, 1.0);
        let raw_y = y.clamp(-1.0, 1.0);
        let raw_z = if self.allow_negative_z {
            z.clamp(-1.0, 1.0)
        } else {
            z.clamp(0.0, 1.0)
        };

        let (x0, x1, tx) = Self::axis_lookup(&cache.x_coords, raw_x);
        let (y0, y1, ty) = Self::axis_lookup(&cache.y_coords, raw_y);
        let (z0, z1, tz) = Self::axis_lookup(&cache.z_coords, raw_z);

        if !self.position_interpolation {
            return self.lookup_cartesian_nearest(
                cache,
                0,
                Self::nearest_idx(x0, x1, tx),
                Self::nearest_idx(y0, y1, ty),
                Self::nearest_idx(z0, z1, tz),
            );
        }

        self.lookup_cartesian_table(cache, 0, x0, x1, tx, y0, y1, ty, z0, z1, tz)
    }

    #[inline]
    fn axis_lookup(axis: &[f32], value: f32) -> (usize, usize, f32) {
        debug_assert!(!axis.is_empty());
        if axis.len() == 1 {
            return (0, 0, 0.0);
        }
        if value <= axis[0] {
            return (0, 0, 0.0);
        }
        let last = axis.len() - 1;
        if value >= axis[last] {
            return (last, last, 0.0);
        }
        for i in 0..last {
            let a = axis[i];
            let b = axis[i + 1];
            if value >= a && value <= b {
                let t = if (b - a).abs() <= 1e-9 {
                    0.0
                } else {
                    (value - a) / (b - a)
                };
                return (i, i + 1, t.clamp(0.0, 1.0));
            }
        }
        (last, last, 0.0)
    }

    #[inline]
    fn nearest_idx(i0: usize, i1: usize, t: f32) -> usize {
        if i0 == i1 || t < 0.5 { i0 } else { i1 }
    }

    #[inline]
    fn lookup_cartesian_nearest(
        &self,
        cache: &CartesianCache,
        table_idx: usize,
        x: usize,
        y: usize,
        z: usize,
    ) -> Gains {
        let table = &cache.tables[table_idx];
        let offset = ((z * cache.y_size + y) * cache.x_size + x) * self.n_speakers;
        Gains::from_slice(&table[offset..offset + self.n_speakers])
    }

    #[allow(clippy::too_many_arguments)]
    fn lookup_cartesian_table(
        &self,
        cache: &CartesianCache,
        table_idx: usize,
        x0: usize,
        x1: usize,
        tx: f32,
        y0: usize,
        y1: usize,
        ty: f32,
        z0: usize,
        z1: usize,
        tz: f32,
    ) -> Gains {
        let table = &cache.tables[table_idx];
        let idx = |x: usize, y: usize, z: usize| -> usize {
            ((z * cache.y_size + y) * cache.x_size + x) * self.n_speakers
        };

        let i000 = idx(x0, y0, z0);
        let i100 = idx(x1, y0, z0);
        let i010 = idx(x0, y1, z0);
        let i110 = idx(x1, y1, z0);
        let i001 = idx(x0, y0, z1);
        let i101 = idx(x1, y0, z1);
        let i011 = idx(x0, y1, z1);
        let i111 = idx(x1, y1, z1);

        let mut out = Gains::new(self.n_speakers);
        for s in 0..self.n_speakers {
            let c000 = table[i000 + s];
            let c100 = table[i100 + s];
            let c010 = table[i010 + s];
            let c110 = table[i110 + s];
            let c001 = table[i001 + s];
            let c101 = table[i101 + s];
            let c011 = table[i011 + s];
            let c111 = table[i111 + s];

            let c00 = c000 * (1.0 - tx) + c100 * tx;
            let c10 = c010 * (1.0 - tx) + c110 * tx;
            let c01 = c001 * (1.0 - tx) + c101 * tx;
            let c11 = c011 * (1.0 - tx) + c111 * tx;
            let c0 = c00 * (1.0 - ty) + c10 * ty;
            let c1 = c01 * (1.0 - ty) + c11 * ty;
            out.data[s] = c0 * (1.0 - tz) + c1 * tz;
        }
        out
    }

    // ── Interpolation helpers ───────────────────────────────────────────────

    fn apply_gain(gains: &Gains, gain: f32) -> Gains {
        let mut gains_out = Gains::new(gains.len);
        for i in 0..gains.len {
            gains_out.data[i] = gains.data[i] * gain;
        }
        gains_out
    }

    fn interpol(&self, gains_low: &Gains, gains_high: &Gains, t: f32) -> Gains {
        let mut gains_interp = Gains::new(self.n_speakers);
        for i in 0..self.n_speakers {
            gains_interp.data[i] = gains_low.data[i] * (1.0 - t) + gains_high.data[i] * t;
        }
        gains_interp
    }

    fn get_gains_from_1(&self, offset: usize, table_idx: usize) -> Gains {
        Gains::from_slice(&self.spread_tables[table_idx].gtable[offset..offset + self.n_speakers])
    }

    fn get_gains_from_2(
        &self,
        offset00: usize,
        offset01: usize,
        t: f32,
        table_idx: usize,
    ) -> Gains {
        let gains0 = self.get_gains_from_1(offset00, table_idx);
        let gains1 = self.get_gains_from_1(offset01, table_idx);

        self.interpol(&gains0, &gains1, t)
    }

    fn get_gains_from_4(
        &self,
        offset00: usize,
        offset01: usize,
        offset10: usize,
        offset11: usize,
        azt: f32,
        elt: f32,
        table_idx: usize,
    ) -> Gains {
        let gains_left = self.get_gains_from_2(offset00, offset01, elt, table_idx);
        let gains_right = self.get_gains_from_2(offset10, offset11, elt, table_idx);

        self.interpol(&gains_left, &gains_right, azt)
    }

    // ── Index computation ───────────────────────────────────────────────────

    fn get_azimuth_idx(&self, azimuth_deg: f32) -> (usize, usize, f32) {
        let az = (azimuth_deg + 180.0) / self.az_res_deg as f32;

        let az0_idx = (az.floor() as usize) % self.n_az;
        let az1_idx = (az.ceil() as usize) % self.n_az;
        let azt = az - (az0_idx as f32);

        (az0_idx, az1_idx, azt)
    }

    fn get_elevation_idx(&self, elevation_deg: f32) -> (usize, usize, f32) {
        let clamped = elevation_deg.clamp(self.elevation_min(), 90.0);
        let el = if self.allow_negative_z {
            (clamped + 90.0) / self.el_res_deg as f32
        } else if self.uses_full_elevation_grid() {
            (clamped + 90.0) / self.el_res_deg as f32
        } else {
            clamped / self.el_res_deg as f32
        };

        let max = self.n_el - 1;

        let el0_idx = (el.floor() as usize).min(max);
        let el1_idx = (el.ceil() as usize).min(max);
        let elt = el - (el0_idx as f32);

        (el0_idx, el1_idx, elt)
    }

    fn get_spread_idx(&self, spread: f32) -> (usize, usize, f32) {
        if self.spread_resolution == 0.0 || self.spread_tables.len() == 1 {
            return (0, 0, 0.0);
        }

        let spread_clamped = spread.clamp(0.0, 1.0);
        let max = self.spread_tables.len() - 1;
        let sp = spread_clamped / self.spread_resolution;
        let sp0_idx = (sp.floor() as usize).min(max);
        let sp1_idx = (sp.ceil() as usize).min(max);
        let spt = sp - (sp0_idx as f32);

        (sp0_idx, sp1_idx, spt)
    }

    // ── Public gain accessors ───────────────────────────────────────────────

    /// Get VBAP gains for a sound source at the specified direction (no spread).
    pub fn get_gains(&self, azimuth_deg: f32, elevation_deg: f32) -> Gains {
        // When the spread table hasn't been pre-computed (gtable is empty),
        // delegate to `get_gains_with_spread` which handles both SAF and native paths.
        if self
            .spread_tables
            .first()
            .map(|t| t.gtable.is_empty())
            .unwrap_or(false)
        {
            return self.get_gains_with_spread(azimuth_deg, elevation_deg, 0.0);
        }

        let mut az = azimuth_deg;
        while az < -180.0 {
            az += 360.0;
        }
        while az > 180.0 {
            az -= 360.0;
        }

        let el = elevation_deg.clamp(self.elevation_min(), 90.0);

        let az_idx = ((az + 180.0) / self.az_res_deg as f32).round() as usize % self.n_az;
        let el_idx = (if self.allow_negative_z || self.uses_full_elevation_grid() {
            ((el + 90.0) / self.el_res_deg as f32).round() as usize
        } else {
            (el / self.el_res_deg as f32).round() as usize
        })
        .min(self.n_el - 1);

        let source_idx = el_idx * self.n_az + az_idx;
        let offset = source_idx * self.n_speakers;

        Gains::from_slice(&self.spread_tables[0].gtable[offset..offset + self.n_speakers])
    }

    /// Pre-populate the polar gain table so hot-path lookups use bilinear
    /// interpolation rather than re-running triangulation on every call.
    ///
    /// Must be called after all builder methods (`with_negative_z`, etc.) have
    /// been applied.  Returns `Err` if the backing gain source cannot be built
    /// (e.g. degenerate speaker layout); the panner remains usable via the lazy
    /// per-call fallback in that case.
    pub fn populate_polar_table(&mut self) -> Result<(), String> {
        // Build the table inside a block so `gain_source` (which borrows `self`
        // immutably) is dropped before the mutable write to `spread_tables`.
        let gtable = {
            let gain_source = self.make_gain_source()?;
            let spread = self.spread_tables[0].spread;
            let (n_az, n_el, n_speakers, az_res_deg) =
                (self.n_az, self.n_el, self.n_speakers, self.az_res_deg);
            let mut table = Vec::with_capacity(n_az * n_el * n_speakers);
            for el_i in 0..n_el {
                let el = self.elevation_from_index(el_i);
                for az_i in 0..n_az {
                    let az = -180.0 + az_i as f32 * az_res_deg as f32;
                    let gains = gain_source.compute_gains(az, el, spread)?;
                    table.extend_from_slice(&gains[..]);
                }
            }
            table
        };
        self.spread_tables[0].gtable = gtable;
        Ok(())
    }

    pub fn num_speakers(&self) -> usize {
        self.n_speakers
    }

    pub fn num_triangles(&self) -> usize {
        self.n_triangles
    }

    pub fn azimuth_resolution(&self) -> i32 {
        self.az_res_deg
    }

    pub fn elevation_resolution(&self) -> i32 {
        self.el_res_deg
    }

    pub fn spread_resolution(&self) -> f32 {
        self.spread_resolution
    }
}
