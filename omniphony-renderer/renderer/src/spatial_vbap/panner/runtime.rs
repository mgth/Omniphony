use super::*;

#[cfg(feature = "saf_vbap")]
struct SpartaVbapLayout {
    n_speakers: usize,
    n_faces: c_int,
    ls_groups: *mut c_int,
    layout_inv_mtx: *mut f32,
}

#[cfg(feature = "saf_vbap")]
impl SpartaVbapLayout {
    // SAF vbap3D expects spread in degrees (0 = VBAP, >0 = MDAP).
    // We keep orender's public spread model normalized in [0, 1] and map it here.
    const NORMALIZED_SPREAD_MAX_DEG: f32 = 180.0;

    #[inline]
    fn normalized_spread_to_degrees(spread: f32) -> f32 {
        spread.clamp(0.0, 1.0) * Self::NORMALIZED_SPREAD_MAX_DEG
    }

    fn from_speaker_dirs(speaker_dirs_deg: &[[f32; 2]]) -> Result<Self, String> {
        let n_speakers = speaker_dirs_deg.len();
        let mut ls_dirs = Vec::with_capacity(n_speakers * 2);
        for &[az, el] in speaker_dirs_deg {
            ls_dirs.push(az);
            ls_dirs.push(el);
        }

        let mut u_spkr: *mut f32 = std::ptr::null_mut();
        let mut num_vert: c_int = 0;
        let mut ls_groups: *mut c_int = std::ptr::null_mut();
        let mut n_faces: c_int = 0;

        unsafe {
            saf_ffi::findLsTriplets(
                ls_dirs.as_mut_ptr(),
                n_speakers as c_int,
                1,
                &mut u_spkr,
                &mut num_vert,
                &mut ls_groups,
                &mut n_faces,
            );
        }

        if num_vert <= 0 || n_faces <= 0 || u_spkr.is_null() || ls_groups.is_null() {
            if !u_spkr.is_null() {
                unsafe { libc::free(u_spkr as *mut libc::c_void) };
            }
            if !ls_groups.is_null() {
                unsafe { libc::free(ls_groups as *mut libc::c_void) };
            }
            return Err("findLsTriplets failed".to_string());
        }

        let mut layout_inv_mtx: *mut f32 = std::ptr::null_mut();
        unsafe {
            saf_ffi::invertLsMtx3D(u_spkr, ls_groups, n_faces, &mut layout_inv_mtx);
            libc::free(u_spkr as *mut libc::c_void);
        }

        if layout_inv_mtx.is_null() {
            unsafe { libc::free(ls_groups as *mut libc::c_void) };
            return Err("invertLsMtx3D failed".to_string());
        }

        Ok(Self {
            n_speakers,
            n_faces,
            ls_groups,
            layout_inv_mtx,
        })
    }

    fn vbap_gains(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        spread: f32,
    ) -> Result<Gains, String> {
        let mut src_dirs = [azimuth_deg, elevation_deg];
        let spread_deg = Self::normalized_spread_to_degrees(spread);
        let mut gain_mtx: *mut f32 = std::ptr::null_mut();
        unsafe {
            saf_ffi::vbap3D(
                src_dirs.as_mut_ptr(),
                1,
                self.n_speakers as c_int,
                self.ls_groups,
                self.n_faces,
                spread_deg,
                self.layout_inv_mtx,
                &mut gain_mtx,
            );
        }

        if gain_mtx.is_null() {
            return Err("vbap3D failed".to_string());
        }

        let gains = unsafe { std::slice::from_raw_parts(gain_mtx, self.n_speakers) };
        let out = Gains::from_slice(gains);
        unsafe { libc::free(gain_mtx as *mut libc::c_void) };
        Ok(out)
    }
}

#[cfg(feature = "saf_vbap")]
impl Drop for SpartaVbapLayout {
    fn drop(&mut self) {
        if !self.ls_groups.is_null() {
            unsafe { libc::free(self.ls_groups as *mut libc::c_void) };
        }
        if !self.layout_inv_mtx.is_null() {
            unsafe { libc::free(self.layout_inv_mtx as *mut libc::c_void) };
        }
    }
}

impl VbapPanner {
    #[inline]
    fn map_depth_with_room_ratios(
        depth: f32,
        front_ratio: f32,
        rear_ratio: f32,
        center_blend: f32,
    ) -> f32 {
        let d = depth.clamp(-1.0, 1.0);
        let blend = center_blend.clamp(0.0, 1.0);
        let center_ratio = rear_ratio + (front_ratio - rear_ratio) * blend;
        if d >= 0.0 {
            let t = d;
            let a = center_ratio - front_ratio;
            let b = 2.0 * (front_ratio - center_ratio);
            let mapped = a * t * t * t + b * t * t + center_ratio * t;
            mapped
        } else {
            let t = -d;
            let a = center_ratio - rear_ratio;
            let b = 2.0 * (rear_ratio - center_ratio);
            let mapped = a * t * t * t + b * t * t + center_ratio * t;
            -mapped
        }
    }

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

    /// Create a new VBAP panner with the specified speaker layout
    ///
    /// # Arguments
    ///
    /// * `speaker_dirs_deg` - Speaker positions as [azimuth, elevation] in degrees
    ///   - Azimuth: 0° = front, -90° = left, 90° = right, ±180° = rear
    ///   - Elevation: 0° = horizontal, 90° = zenith, -90° = nadir
    /// * `az_res_deg` - Azimuth resolution in degrees (1-10 recommended)
    /// * `el_res_deg` - Elevation resolution in degrees (1-10 recommended)
    /// * `spread` - Spreading coefficient (0.0 = point source, 1.0 = maximum spread)
    ///
    /// # Returns
    ///
    /// A new `VbapPanner` instance with pre-computed gain tables
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Speaker count is less than 3
    /// - Resolution is invalid (must be divisor of 360 for azimuth, 180 for elevation)
    /// - VBAP triangulation fails (speakers not in convex hull)
    ///
    /// **Note:** This method requires the `saf_vbap` feature to be enabled.
    /// Without it, use `load_from_file()` to load pre-generated VBAP tables.
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

        let layout = SpartaVbapLayout::from_speaker_dirs(speaker_dirs_deg)?;
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
            polar_distance_cache: None,
            precomputed_effects: false,
            #[cfg(feature = "saf_vbap")]
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

    pub fn table_mode(&self) -> VbapTableMode {
        self.table_mode
    }

    pub fn with_negative_z(mut self, allow_negative_z: bool) -> Self {
        self.allow_negative_z = allow_negative_z;
        if self.spread_tables.iter().all(|t| t.gtable.is_empty()) {
            self.recompute_grid_dims();
        }
        self.precomputed_effects = false;
        self.cartesian_cache = None;
        self.polar_distance_cache = None;
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
        self.precomputed_effects = false;
        self.polar_distance_cache = None;
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
                self.cartesian_cache = Some(self.build_cartesian_cache(
                    x_size,
                    y_size,
                    z_size,
                    z_neg_size,
                )?);
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

    pub fn has_precomputed_effects(&self) -> bool {
        self.precomputed_effects
    }

    pub fn precompute_effect_tables(
        mut self,
        distance_step: f32,
        distance_max: f32,
        spread_min: f32,
        spread_max: f32,
        distance_model: DistanceModel,
        spread_from_distance: bool,
        spread_distance_range: f32,
        spread_distance_curve: f32,
        distance_diffuse: bool,
        distance_diffuse_threshold: f32,
        distance_diffuse_curve: f32,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
    ) -> Result<Self, String> {
        if distance_step <= 0.0 {
            return Err("distance_step must be > 0".to_string());
        }
        if distance_max <= 0.0 {
            return Err("distance_max must be > 0".to_string());
        }
        match self.table_mode {
            VbapTableMode::Cartesian {
                x_size,
                y_size,
                z_size,
                z_neg_size,
            } => {
                self.cartesian_cache = Some(self.build_cartesian_effect_cache(
                    x_size,
                    y_size,
                    z_size,
                    z_neg_size,
                    spread_min,
                    spread_max,
                    distance_model,
                    spread_from_distance,
                    spread_distance_range,
                    spread_distance_curve,
                    distance_diffuse,
                    distance_diffuse_threshold,
                    distance_diffuse_curve,
                    room_ratio,
                    room_ratio_rear,
                    room_ratio_lower,
                    room_ratio_center_blend,
                )?);
                self.polar_distance_cache = None;
            }
            VbapTableMode::Polar => {
                self.polar_distance_cache = Some(self.build_polar_distance_effect_cache(
                    distance_step,
                    distance_max,
                    spread_min,
                    spread_max,
                    distance_model,
                    spread_from_distance,
                    spread_distance_range,
                    spread_distance_curve,
                    distance_diffuse,
                    distance_diffuse_threshold,
                    distance_diffuse_curve,
                    room_ratio,
                    room_ratio_rear,
                    room_ratio_lower,
                    room_ratio_center_blend,
                )?);
            }
        }
        self.precomputed_effects = true;
        Ok(self)
    }

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
        #[cfg(feature = "saf_vbap")]
        let direct_layout = self
            .speaker_dirs_deg
            .as_deref()
            .ok_or_else(|| "Direct VBAP layout unavailable (speaker_dirs_deg missing)".to_string())
            .and_then(SpartaVbapLayout::from_speaker_dirs)?;

        for table_idx in 0..self.spread_tables.len() {
            let mut table = Vec::with_capacity(grid_points * self.n_speakers);
            #[cfg(feature = "saf_vbap")]
            let spread = self.spread_tables[table_idx].spread;
            for &z in &z_coords {
                for &y in &y_coords {
                    for &x in &x_coords {
                        let (azimuth, elevation, _) = adm_to_spherical(x, y, z);
                        #[cfg(feature = "saf_vbap")]
                        let gains = direct_layout.vbap_gains(azimuth, elevation, spread)?;
                        #[cfg(not(feature = "saf_vbap"))]
                        let gains =
                            self.get_gains_with_spread_from_table(azimuth, elevation, table_idx);
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
            effect_space: false,
            room_ratio: [1.0, 1.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.5,
            tables,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_cartesian_effect_cache(
        &self,
        x_size: usize,
        y_size: usize,
        z_size: usize,
        z_neg_size: usize,
        spread_min: f32,
        spread_max: f32,
        distance_model: DistanceModel,
        spread_from_distance: bool,
        spread_distance_range: f32,
        spread_distance_curve: f32,
        distance_diffuse: bool,
        distance_diffuse_threshold: f32,
        distance_diffuse_curve: f32,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
    ) -> Result<CartesianCache, String> {
        let total_z_points = self.cartesian_total_z_points(z_size, z_neg_size);
        let grid_points = x_size * y_size * total_z_points;
        let mut table = Vec::with_capacity(grid_points * self.n_speakers);
        let raw_x_coords: Vec<f32> = (0..x_size)
            .map(|xi| -1.0 + 2.0 * (xi as f32) / ((x_size - 1) as f32))
            .collect();
        let raw_y_coords: Vec<f32> = (0..y_size)
            .map(|yi| -1.0 + 2.0 * (yi as f32) / ((y_size - 1) as f32))
            .collect();
        let raw_z_coords: Vec<f32> = (0..total_z_points)
            .map(|zi| self.table_z_value(zi, z_size, z_neg_size))
            .collect();
        let x_coords: Vec<f32> = raw_x_coords.iter().map(|x| *x * room_ratio[0]).collect();
        let y_coords: Vec<f32> = raw_y_coords
            .iter()
            .map(|y| {
                Self::map_depth_with_room_ratios(
                    *y,
                    room_ratio[1],
                    room_ratio_rear,
                    room_ratio_center_blend,
                )
            })
            .collect();
        let z_coords: Vec<f32> = raw_z_coords
            .iter()
            .map(|z| {
                if *z >= 0.0 {
                    *z * room_ratio[2]
                } else {
                    *z * room_ratio_lower
                }
            })
            .collect();
        #[cfg(feature = "saf_vbap")]
        let direct_layout = self
            .speaker_dirs_deg
            .as_deref()
            .ok_or_else(|| "Direct VBAP layout unavailable (speaker_dirs_deg missing)".to_string())
            .and_then(SpartaVbapLayout::from_speaker_dirs)?;

        for &z in &raw_z_coords {
            for &y in &raw_y_coords {
                for &x in &raw_x_coords {
                    #[cfg(feature = "saf_vbap")]
                    let gains = self.compute_effect_gains_for_position_with_layout(
                        &direct_layout,
                        x,
                        y,
                        z,
                        spread_min,
                        spread_max,
                        distance_model,
                        spread_from_distance,
                        spread_distance_range,
                        spread_distance_curve,
                        distance_diffuse,
                        distance_diffuse_threshold,
                        distance_diffuse_curve,
                        room_ratio,
                        room_ratio_rear,
                        room_ratio_lower,
                        room_ratio_center_blend,
                    )?;
                    #[cfg(not(feature = "saf_vbap"))]
                    let gains = self.compute_effect_gains_for_position(
                        x,
                        y,
                        z,
                        spread_min,
                        spread_max,
                        distance_model,
                        spread_from_distance,
                        spread_distance_range,
                        spread_distance_curve,
                        distance_diffuse,
                        distance_diffuse_threshold,
                        distance_diffuse_curve,
                        room_ratio,
                        room_ratio_rear,
                        room_ratio_lower,
                        room_ratio_center_blend,
                    );
                    table.extend_from_slice(&gains[..]);
                }
            }
        }

        Ok(CartesianCache {
            x_size,
            y_size,
            x_coords,
            y_coords,
            z_coords,
            effect_space: true,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            tables: vec![table],
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_polar_distance_effect_cache(
        &self,
        distance_step: f32,
        distance_max: f32,
        spread_min: f32,
        spread_max: f32,
        distance_model: DistanceModel,
        spread_from_distance: bool,
        spread_distance_range: f32,
        spread_distance_curve: f32,
        distance_diffuse: bool,
        distance_diffuse_threshold: f32,
        distance_diffuse_curve: f32,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
    ) -> Result<PolarDistanceCache, String> {
        let max_dist = distance_max.max(1e-6);
        let d_size = ((max_dist / distance_step).ceil() as usize) + 1;
        let mut table = Vec::with_capacity(d_size * self.n_el * self.n_az * self.n_speakers);
        #[cfg(feature = "saf_vbap")]
        let direct_layout = self
            .speaker_dirs_deg
            .as_deref()
            .ok_or_else(|| "Direct VBAP layout unavailable (speaker_dirs_deg missing)".to_string())
            .and_then(SpartaVbapLayout::from_speaker_dirs)?;

        for di in 0..d_size {
            let d = (di as f32 * distance_step).min(max_dist);
            for el in 0..self.n_el {
                let elevation = self.elevation_from_index(el);
                for az in 0..self.n_az {
                    let azimuth = (az as f32 * self.az_res_deg as f32) - 180.0;
                    let (x, y, z) = spherical_to_adm(azimuth, elevation, d);
                    #[cfg(feature = "saf_vbap")]
                    let gains = self.compute_effect_gains_for_position_with_layout(
                        &direct_layout,
                        x,
                        y,
                        z,
                        spread_min,
                        spread_max,
                        distance_model,
                        spread_from_distance,
                        spread_distance_range,
                        spread_distance_curve,
                        distance_diffuse,
                        distance_diffuse_threshold,
                        distance_diffuse_curve,
                        room_ratio,
                        room_ratio_rear,
                        room_ratio_lower,
                        room_ratio_center_blend,
                    )?;
                    #[cfg(not(feature = "saf_vbap"))]
                    let gains = self.compute_effect_gains_for_position(
                        x,
                        y,
                        z,
                        spread_min,
                        spread_max,
                        distance_model,
                        spread_from_distance,
                        spread_distance_range,
                        spread_distance_curve,
                        distance_diffuse,
                        distance_diffuse_threshold,
                        distance_diffuse_curve,
                        room_ratio,
                        room_ratio_rear,
                        room_ratio_lower,
                        room_ratio_center_blend,
                    );
                    table.extend_from_slice(&gains[..]);
                }
            }
        }

        Ok(PolarDistanceCache {
            d_size,
            d_step: distance_step,
            d_max: max_dist,
            table,
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(not(feature = "saf_vbap"))]
    fn compute_effect_gains_for_position(
        &self,
        x: f32,
        y: f32,
        z: f32,
        spread_min: f32,
        spread_max: f32,
        distance_model: DistanceModel,
        spread_from_distance: bool,
        spread_distance_range: f32,
        spread_distance_curve: f32,
        distance_diffuse: bool,
        distance_diffuse_threshold: f32,
        distance_diffuse_curve: f32,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
    ) -> Gains {
        let scaled_x = x * room_ratio[0];
        let scaled_y = Self::map_depth_with_room_ratios(
            y,
            room_ratio[1],
            room_ratio_rear,
            room_ratio_center_blend,
        );
        let scaled_z = if z >= 0.0 {
            z * room_ratio[2]
        } else {
            z * room_ratio_lower
        };
        let (azimuth, elevation, distance) = adm_to_spherical(scaled_x, scaled_y, scaled_z);
        let spread = if spread_from_distance {
            let normalized = 1.0 - (distance / spread_distance_range.max(1e-6));
            let t = normalized
                .clamp(0.0, 1.0)
                .powf(spread_distance_curve.max(0.0));
            (spread_min + t * (spread_max - spread_min)).clamp(0.0, 1.0)
        } else {
            spread_min.clamp(0.0, 1.0)
        };

        let direct = self.get_gains_with_spread(azimuth, elevation, spread);
        let directional = if distance_diffuse {
            let mirror = self.get_gains_with_spread(-azimuth, elevation, spread);
            let raw_distance = (x * x + y * y + z * z).sqrt();
            let t = (raw_distance / distance_diffuse_threshold.max(1e-6))
                .min(1.0)
                .powf(distance_diffuse_curve);
            let alpha = 0.5 + 0.5 * t;
            let w_direct = alpha.sqrt();
            let w_mirror = (1.0 - alpha).sqrt();
            let mut blended = Gains::zeroed(self.n_speakers);
            let mut e_direct = 0.0f32;
            let mut e_blended = 0.0f32;
            for i in 0..self.n_speakers {
                let g = w_direct * direct[i] + w_mirror * mirror[i];
                blended.set(i, g);
                e_direct += direct[i] * direct[i];
                e_blended += g * g;
            }
            if e_blended > 1e-12 && e_direct > 0.0 {
                let scale = (e_direct / e_blended).sqrt();
                for i in 0..self.n_speakers {
                    blended.set(i, blended[i] * scale);
                }
            }
            blended
        } else {
            direct
        };

        VbapPanner::apply_gain(
            &directional,
            calculate_distance_attenuation(distance, distance_model),
        )
    }

    #[cfg(feature = "saf_vbap")]
    #[allow(clippy::too_many_arguments)]
    fn compute_effect_gains_for_position_with_layout(
        &self,
        layout: &SpartaVbapLayout,
        x: f32,
        y: f32,
        z: f32,
        spread_min: f32,
        spread_max: f32,
        distance_model: DistanceModel,
        spread_from_distance: bool,
        spread_distance_range: f32,
        spread_distance_curve: f32,
        distance_diffuse: bool,
        distance_diffuse_threshold: f32,
        distance_diffuse_curve: f32,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
    ) -> Result<Gains, String> {
        let scaled_x = x * room_ratio[0];
        let scaled_y = Self::map_depth_with_room_ratios(
            y,
            room_ratio[1],
            room_ratio_rear,
            room_ratio_center_blend,
        );
        let scaled_z = if z >= 0.0 {
            z * room_ratio[2]
        } else {
            z * room_ratio_lower
        };
        let (azimuth, elevation, distance) = adm_to_spherical(scaled_x, scaled_y, scaled_z);
        let spread = if spread_from_distance {
            let normalized = 1.0 - (distance / spread_distance_range.max(1e-6));
            let t = normalized
                .clamp(0.0, 1.0)
                .powf(spread_distance_curve.max(0.0));
            (spread_min + t * (spread_max - spread_min)).clamp(0.0, 1.0)
        } else {
            spread_min.clamp(0.0, 1.0)
        };

        let direct = layout.vbap_gains(azimuth, elevation, spread)?;
        let directional = if distance_diffuse {
            let mirror = layout.vbap_gains(-azimuth, elevation, spread)?;
            let raw_distance = (x * x + y * y + z * z).sqrt();
            let t = (raw_distance / distance_diffuse_threshold.max(1e-6))
                .min(1.0)
                .powf(distance_diffuse_curve);
            let alpha = 0.5 + 0.5 * t;
            let w_direct = alpha.sqrt();
            let w_mirror = (1.0 - alpha).sqrt();
            let mut blended = Gains::zeroed(self.n_speakers);
            let mut e_direct = 0.0f32;
            let mut e_blended = 0.0f32;
            for i in 0..self.n_speakers {
                let g = w_direct * direct[i] + w_mirror * mirror[i];
                blended.set(i, g);
                e_direct += direct[i] * direct[i];
                e_blended += g * g;
            }
            if e_blended > 1e-12 && e_direct > 0.0 {
                let scale = (e_direct / e_blended).sqrt();
                for i in 0..self.n_speakers {
                    blended.set(i, blended[i] * scale);
                }
            }
            blended
        } else {
            direct
        };

        Ok(VbapPanner::apply_gain(
            &directional,
            calculate_distance_attenuation(distance, distance_model),
        ))
    }

    /// Get VBAP gains for a sound source from cartesian coordinates
    ///
    /// # Arguments
    ///
    /// * `x`:
    /// * `y`:
    /// * `z`:
    /// * `spread`:
    ///
    /// returns: Vec<f32, Global>
    ///
    /// # Examples
    ///
    /// ```
    ///
    /// ```
    pub fn get_gains_cartesian_spread_from_distance(
        &self,
        x: f32,
        y: f32,
        z: f32,
        distance_model: DistanceModel,
        spread_distance_range: f32,
        spread_distance_curve: f32,
    ) -> Gains {
        let z = if self.allow_negative_z { z } else { z.max(0.0) };
        // Convert rendering position to spherical
        let (final_azimuth, final_elevation, final_distance) = adm_to_spherical(x, y, z);

        // Calculate spread from distance using configurable range and curve
        // spread = (1.0 - distance/range)^curve, clamped to [0, 1]
        let normalized = 1.0 - (final_distance / spread_distance_range);
        let spread = normalized.max(0.0).min(1.0).powf(spread_distance_curve);

        let gains = self.get_gains_with_spread(final_azimuth, final_elevation, spread);

        VbapPanner::apply_gain(
            &gains,
            calculate_distance_attenuation(final_distance, distance_model),
        )
    }

    pub fn get_gains_cartesian(
        &self,
        x: f32,
        y: f32,
        z: f32,
        spread: f32,
        distance_model: DistanceModel,
    ) -> Gains {
        let z = if self.allow_negative_z { z } else { z.max(0.0) };
        if self.precomputed_effects {
            return match self.table_mode {
                VbapTableMode::Cartesian { .. } => self.get_gains_from_cartesian_cache(x, y, z),
                VbapTableMode::Polar => self.get_gains_from_polar_distance_cache(x, y, z),
            };
        }

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

    /// Get VBAP gains with dynamic spread (interpolated between pre-computed tables)
    ///
    /// # Arguments
    ///
    /// * `azimuth_deg` - Source azimuth in degrees
    /// * `elevation_deg` - Source elevation in degrees
    /// * `spread` - Spread coefficient (0.0 - 1.0, from object spread metadata)
    ///
    /// # Returns
    ///
    /// A vector of gains interpolated between the two closest spread tables
    ///
    /// # Behavior
    ///
    /// - Direct `saf_vbap` mode uses `vbap3D` with continuous spread
    /// - Non-saf_vbap mode interpolates from preloaded tables
    ///
    /// # Example
    ///
    /// ```no_run
    /// // With sp_res=0.25, tables at [0.0, 0.25, 0.5, 0.75, 1.0]
    /// // spread=0.6 → interpolate between table 0.5 (40%) and table 0.75 (60%)
    /// let gains = panner.get_gains_with_spread(45.0, 0.0, 0.6);
    /// ```
    pub fn get_gains_with_spread(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        spread: f32,
    ) -> Gains {
        #[cfg(feature = "saf_vbap")]
        // Direct vbap3D is only used as a last-resort path while spread tables are absent.
        // In normal runtime, we always use precomputed tables/caches.
        if self
            .spread_tables
            .first()
            .map(|t| t.gtable.is_empty())
            .unwrap_or(false)
        {
            if let Some(dirs) = self.speaker_dirs_deg.as_deref() {
                let layout = SpartaVbapLayout::from_speaker_dirs(dirs)
                    .expect("failed to initialize direct VBAP layout");
                return layout
                    .vbap_gains(azimuth_deg, elevation_deg, spread)
                    .expect("vbap3D failed while computing gains");
            }
        }

        let (az0_idx, az1_idx, azt) = self.get_azimuth_idx(azimuth_deg);
        let (el0_idx, el1_idx, elt) = self.get_elevation_idx(elevation_deg);
        let (sp0_idx, sp1_idx, spt) = self.get_spread_idx(spread);

        let offset00 = (el0_idx * self.n_az + az0_idx) * self.n_speakers;
        let offset01 = (el1_idx * self.n_az + az0_idx) * self.n_speakers;
        let offset10 = (el0_idx * self.n_az + az1_idx) * self.n_speakers;
        let offset11 = (el1_idx * self.n_az + az1_idx) * self.n_speakers;

        //       return self.get_gains_from_table(offset00, offset00, offset00, offset00, 0.0, 0.0, 0);

        // If spread exactly matches a table, use it directly
        if sp0_idx == sp1_idx {
            return self
                .get_gains_from_4(offset00, offset01, offset10, offset11, azt, elt, sp0_idx);
        }

        // Get gains from both tables
        let g0 = self.get_gains_from_4(offset00, offset01, offset10, offset11, azt, elt, sp0_idx);
        let g1 = self.get_gains_from_4(offset00, offset01, offset10, offset11, azt, elt, sp1_idx);

        self.interpol(&g0, &g1, spt)
    }

    #[cfg(not(feature = "saf_vbap"))]
    fn get_gains_with_spread_from_table(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        table_idx: usize,
    ) -> Gains {
        let (az0_idx, az1_idx, azt) = self.get_azimuth_idx(azimuth_deg);
        let (el0_idx, el1_idx, elt) = self.get_elevation_idx(elevation_deg);

        let offset00 = (el0_idx * self.n_az + az0_idx) * self.n_speakers;
        let offset01 = (el1_idx * self.n_az + az0_idx) * self.n_speakers;
        let offset10 = (el0_idx * self.n_az + az1_idx) * self.n_speakers;
        let offset11 = (el1_idx * self.n_az + az1_idx) * self.n_speakers;

        self.get_gains_from_4(offset00, offset01, offset10, offset11, azt, elt, table_idx)
    }

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

        let query_x = if cache.effect_space {
            raw_x * cache.room_ratio[0]
        } else {
            raw_x
        };
        let query_y = if cache.effect_space {
            Self::map_depth_with_room_ratios(
                raw_y,
                cache.room_ratio[1],
                cache.room_ratio_rear,
                cache.room_ratio_center_blend,
            )
        } else {
            raw_y
        };
        let query_z = if cache.effect_space {
            if raw_z >= 0.0 {
                raw_z * cache.room_ratio[2]
            } else {
                raw_z * cache.room_ratio_lower
            }
        } else {
            raw_z
        };

        let (x0, x1, tx) = Self::axis_lookup(&cache.x_coords, query_x);
        let (y0, y1, ty) = Self::axis_lookup(&cache.y_coords, query_y);
        let (z0, z1, tz) = Self::axis_lookup(&cache.z_coords, query_z);

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

    fn get_gains_from_polar_distance_cache(&self, x: f32, y: f32, z: f32) -> Gains {
        let Some(cache) = self.polar_distance_cache.as_ref() else {
            let z = if self.allow_negative_z { z } else { z.max(0.0) };
            let (azimuth, elevation, _) = adm_to_spherical(x, y, z);
            return self.get_gains_with_spread(azimuth, elevation, 0.0);
        };

        let z = if self.allow_negative_z { z } else { z.max(0.0) };
        let (azimuth, elevation, distance) = adm_to_spherical(x, y, z);
        let (az0, az1, azt) = self.get_azimuth_idx(azimuth);
        let (el0, el1, elt) = self.get_elevation_idx(elevation);
        let fd = (distance.min(cache.d_max) / cache.d_step).clamp(0.0, (cache.d_size - 1) as f32);
        let d0 = fd.floor() as usize;
        let d1 = fd.ceil() as usize;
        let dt = fd - d0 as f32;

        let idx = |d: usize, el: usize, az: usize| -> usize {
            ((d * self.n_el + el) * self.n_az + az) * self.n_speakers
        };

        if !self.position_interpolation {
            let az = Self::nearest_idx(az0, az1, azt);
            let el = Self::nearest_idx(el0, el1, elt);
            let d = Self::nearest_idx(d0, d1, dt);
            return Gains::from_slice(
                &cache.table[idx(d, el, az)..idx(d, el, az) + self.n_speakers],
            );
        }

        let mut out = Gains::new(self.n_speakers);
        for s in 0..self.n_speakers {
            let c000 = cache.table[idx(d0, el0, az0) + s];
            let c100 = cache.table[idx(d0, el0, az1) + s];
            let c010 = cache.table[idx(d0, el1, az0) + s];
            let c110 = cache.table[idx(d0, el1, az1) + s];
            let c001 = cache.table[idx(d1, el0, az0) + s];
            let c101 = cache.table[idx(d1, el0, az1) + s];
            let c011 = cache.table[idx(d1, el1, az0) + s];
            let c111 = cache.table[idx(d1, el1, az1) + s];

            let c00 = c000 * (1.0 - azt) + c100 * azt;
            let c10 = c010 * (1.0 - azt) + c110 * azt;
            let c01 = c001 * (1.0 - azt) + c101 * azt;
            let c11 = c011 * (1.0 - azt) + c111 * azt;
            let c0 = c00 * (1.0 - elt) + c10 * elt;
            let c1 = c01 * (1.0 - elt) + c11 * elt;
            out.data[s] = c0 * (1.0 - dt) + c1 * dt;
        }
        out
    }

    fn apply_gain(gains: &Gains, gain: f32) -> Gains {
        let mut gains_out = Gains::new(gains.len);
        for i in 0..gains.len {
            gains_out.data[i] = gains.data[i] * gain;
        }
        gains_out
    }

    // Linear interpolation
    fn interpol(&self, gains_low: &Gains, gains_high: &Gains, t: f32) -> Gains {
        let mut gains_interp = Gains::new(self.n_speakers);
        for i in 0..self.n_speakers {
            gains_interp.data[i] = gains_low.data[i] * (1.0 - t) + gains_high.data[i] * t;
        }
        gains_interp
    }

    /// Get gains from a specific spread table (internal helper)
    fn get_gains_from_1(&self, offset: usize, table_idx: usize) -> Gains {
        // Extract gains from specified table
        Gains::from_slice(&self.spread_tables[table_idx].gtable[offset..offset + self.n_speakers])
    }

    fn get_gains_from_2(
        &self,
        offset00: usize,
        offset01: usize,
        t: f32,
        table_idx: usize,
    ) -> Gains {
        // Extract gains from specified table
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
        // Extract gains from specified table
        let gains_left = self.get_gains_from_2(offset00, offset01, elt, table_idx);
        let gains_right = self.get_gains_from_2(offset10, offset11, elt, table_idx);

        self.interpol(&gains_left, &gains_right, azt)
    }

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

        // Clamp spread to valid range
        let spread_clamped = spread.clamp(0.0, 1.0);
        let max = self.spread_tables.len() - 1;
        // Find the two tables to interpolate between
        let sp = spread_clamped / self.spread_resolution;
        let sp0_idx = (sp.floor() as usize).min(max);
        let sp1_idx = (sp.ceil() as usize).min(max);
        let spt = sp - (sp0_idx as f32);

        (sp0_idx, sp1_idx, spt)
    }

    /// Get VBAP gains for a sound source at the specified direction
    ///
    /// # Arguments
    ///
    /// * `azimuth_deg` - Source azimuth in degrees (0° = front, ±180° = rear)
    /// * `elevation_deg` - Source elevation in degrees (0° = horizontal, 90° = zenith)
    ///
    /// # Returns
    ///
    /// A vector of gains (one per speaker), normalized so that sum(gains²) = 1
    ///
    /// # Performance
    ///
    /// This is an O(1) lookup operation using the pre-computed gain table
    pub fn get_gains(&self, azimuth_deg: f32, elevation_deg: f32) -> Gains {
        #[cfg(feature = "saf_vbap")]
        if self
            .spread_tables
            .first()
            .map(|t| t.gtable.is_empty())
            .unwrap_or(false)
        {
            return self.get_gains_with_spread(azimuth_deg, elevation_deg, 0.0);
        }

        //        return self.get_gains_with_spread(azimuth_deg, elevation_deg, 0.0);

        // Wrap azimuth to [-180, 180]
        let mut az = azimuth_deg;
        while az < -180.0 {
            az += 360.0;
        }
        while az > 180.0 {
            az -= 360.0;
        }

        // Clamp elevation to the configured table range.
        let el = elevation_deg.clamp(self.elevation_min(), 90.0);

        // Convert to grid indices (with wrapping/clamping)
        // SAF uses: azimuth from -180 to +180, elevation from -90 to +90
        let az_idx = ((az + 180.0) / self.az_res_deg as f32).round() as usize % self.n_az;
        let el_idx = (if self.allow_negative_z || self.uses_full_elevation_grid() {
            ((el + 90.0) / self.el_res_deg as f32).round() as usize
        } else {
            (el / self.el_res_deg as f32).round() as usize
        })
        .min(self.n_el - 1);

        // Calculate offset into flattened gain table
        // Layout: source_index = el_idx * n_az + az_idx (SAF indexing: i*N_azi + j)
        // Gain offset: source_index * n_speakers
        let source_idx = el_idx * self.n_az + az_idx;
        let offset = source_idx * self.n_speakers;

        // Extract gains for all speakers from first (or only) spread table
        Gains::from_slice(&self.spread_tables[0].gtable[offset..offset + self.n_speakers])
    }

    /// Get the number of speakers in the layout
    pub fn num_speakers(&self) -> usize {
        self.n_speakers
    }

    /// Get the number of triangles found during VBAP triangulation
    ///
    /// This can be useful for debugging speaker layouts. A typical 7.1.4 layout
    /// will have around 20-30 triangles.
    pub fn num_triangles(&self) -> usize {
        self.n_triangles
    }

    /// Get the azimuth resolution in degrees
    pub fn azimuth_resolution(&self) -> i32 {
        self.az_res_deg
    }

    /// Get the elevation resolution in degrees
    pub fn elevation_resolution(&self) -> i32 {
        self.el_res_deg
    }

    /// Get the spread resolution
    ///
    /// Returns 0.0 for single-table mode, or the step between spread tables
    /// for multi-spread mode (e.g., 0.25 for tables at 0.0, 0.25, 0.5, 0.75, 1.0)
    pub fn spread_resolution(&self) -> f32 {
        self.spread_resolution
    }
}
