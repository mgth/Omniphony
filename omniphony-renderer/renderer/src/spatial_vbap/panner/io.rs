use super::*;

impl VbapPanner {
    /// Save VBAP gain tables to a binary file
    ///
    /// The binary format includes:
    /// - Header with metadata (version, dimensions, etc.)
    /// - All spread tables with their gain data
    ///
    /// This allows pre-computing VBAP tables offline and loading them for faster startup.
    ///
    /// # Binary Format (v3)
    ///
    /// ```text
    /// Header (60 bytes):
    ///   u32: Magic number (0x56424150 = "VBAP")
    ///   u32: Format version (3 = compressed + speaker layout)
    ///   u32: Number of speakers (total)
    ///   u32: Azimuth resolution (degrees)
    ///   u32: Elevation resolution (degrees)
    ///   u32: Number of azimuth grid points
    ///   u32: Number of elevation grid points
    ///   u32: Number of gain table entries
    ///   u32: Number of triangles
    ///   u32: Number of spread tables
    ///   f32: Spread resolution
    ///   u32: Compression flag (1 = compressed with zlib)
    ///   u32: Speaker layout data size (uncompressed)
    ///   [20 bytes reserved for future use]
    ///
    /// Speaker layout data (uncompressed):
    ///   For each speaker:
    ///     u32: Name length (bytes)
    ///     [name_length bytes]: UTF-8 speaker name
    ///     f32: Azimuth (degrees)
    ///     f32: Elevation (degrees)
    ///     u8: Spatialize flag (0 or 1)
    ///
    /// Compressed data block (zlib):
    ///   For each spread table:
    ///     f32: Spread value
    ///     [n_gtable * n_speakers * f32]: Gain table data
    /// ```
    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &crate::speaker_layout::SpeakerLayout,
    ) -> Result<(), String> {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        let mut file =
            std::fs::File::create(path).map_err(|e| format!("Failed to create file: {}", e))?;

        // Write header (uncompressed)
        const MAGIC: u32 = 0x56424150; // "VBAP"
        const VERSION: u32 = 4; // Version 4 = compressed + speaker layout + expanded gains (all speakers)
        const COMPRESSION_FLAG: u32 = 1; // 1 = zlib compressed

        // Serialize speaker layout data first to get its size
        let mut layout_data = Vec::new();
        for speaker in &speaker_layout.speakers {
            let name_bytes = speaker.name.as_bytes();
            layout_data.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
            layout_data.extend_from_slice(name_bytes);
            layout_data.extend_from_slice(&speaker.azimuth.to_le_bytes());
            layout_data.extend_from_slice(&speaker.elevation.to_le_bytes());
            layout_data.push(if speaker.spatialize { 1 } else { 0 });
        }
        let layout_data_size = layout_data.len();

        file.write_all(&MAGIC.to_le_bytes())
            .map_err(|e| format!("Failed to write magic: {}", e))?;
        file.write_all(&VERSION.to_le_bytes())
            .map_err(|e| format!("Failed to write version: {}", e))?;
        // Write total number of speakers in layout (not just spatializable ones)
        file.write_all(&(speaker_layout.speakers.len() as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write n_speakers: {}", e))?;
        file.write_all(&(self.az_res_deg as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write az_res_deg: {}", e))?;
        file.write_all(&(self.el_res_deg as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write el_res_deg: {}", e))?;
        file.write_all(&(self.n_az as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write n_az: {}", e))?;
        file.write_all(&(self.n_el as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write n_el: {}", e))?;
        file.write_all(&(self.n_gtable as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write n_gtable: {}", e))?;
        file.write_all(&(self.n_triangles as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write n_triangles: {}", e))?;
        file.write_all(&(self.spread_tables.len() as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write spread_tables.len: {}", e))?;
        file.write_all(&self.spread_resolution.to_le_bytes())
            .map_err(|e| format!("Failed to write spread_resolution: {}", e))?;
        file.write_all(&COMPRESSION_FLAG.to_le_bytes())
            .map_err(|e| format!("Failed to write compression_flag: {}", e))?;
        file.write_all(&(layout_data_size as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write layout_data_size: {}", e))?;

        // Write 20 bytes of reserved space (reduced from 24 to make room for layout size)
        let reserved = [0u8; 20];
        file.write_all(&reserved)
            .map_err(|e| format!("Failed to write reserved: {}", e))?;

        // Write speaker layout data (uncompressed)
        file.write_all(&layout_data)
            .map_err(|e| format!("Failed to write speaker layout data: {}", e))?;

        // Get mapping from VBAP index to total speaker index
        // This allows us to expand the gain table to include all speakers
        let (_, vbap_to_speaker_mapping) = speaker_layout.spatializable_positions();
        let n_total_speakers = speaker_layout.speakers.len();
        let n_spatializable = self.n_speakers;

        // Prepare data to compress
        // Expand gains from n_spatializable to n_total_speakers (zeros for non-spatializable)
        let mut uncompressed_data = Vec::new();
        for table in &self.spread_tables {
            uncompressed_data.extend_from_slice(&table.spread.to_le_bytes());

            // Process each direction entry in the gain table
            for dir_idx in 0..self.n_gtable {
                // Create expanded gains for this direction (all zeros initially)
                let mut expanded_gains = vec![0.0_f32; n_total_speakers];

                // Copy spatializable gains to their correct positions
                let base_offset = dir_idx * n_spatializable;
                let direct_gains = if table.gtable.is_empty() {
                    let az_idx = dir_idx % self.n_az;
                    let el_idx = dir_idx / self.n_az;
                    let azimuth = (az_idx as f32 * self.az_res_deg as f32) - 180.0;
                    let elevation = self.elevation_from_index(el_idx);
                    Some(self.get_gains_with_spread(azimuth, elevation, table.spread))
                } else {
                    None
                };
                for (vbap_idx, &speaker_idx) in vbap_to_speaker_mapping.iter().enumerate() {
                    expanded_gains[speaker_idx] = if let Some(ref gains) = direct_gains {
                        gains[vbap_idx]
                    } else {
                        table.gtable[base_offset + vbap_idx]
                    };
                }

                // Write expanded gains
                for &gain in &expanded_gains {
                    uncompressed_data.extend_from_slice(&gain.to_le_bytes());
                }
            }
        }

        let uncompressed_size = uncompressed_data.len();

        // Compress data with zlib (level 6 = default, good balance of speed/compression)
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
        encoder
            .write_all(&uncompressed_data)
            .map_err(|e| format!("Failed to compress data: {}", e))?;
        let compressed_data = encoder
            .finish()
            .map_err(|e| format!("Failed to finish compression: {}", e))?;

        // Write compressed size and compressed data
        file.write_all(&(compressed_data.len() as u32).to_le_bytes())
            .map_err(|e| format!("Failed to write compressed size: {}", e))?;
        file.write_all(&compressed_data)
            .map_err(|e| format!("Failed to write compressed data: {}", e))?;

        file.flush()
            .map_err(|e| format!("Failed to flush file: {}", e))?;

        let compressed_size = compressed_data.len();
        let total_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let compression_ratio = (uncompressed_size as f64) / (compressed_size as f64);

        log::info!(
            "Saved VBAP gain table to {}: {} total speakers ({} spatializable), {}x{} resolution, {} spread tables",
            path.display(),
            n_total_speakers,
            n_spatializable,
            self.az_res_deg,
            self.el_res_deg,
            self.spread_tables.len(),
        );
        log::info!(
            "  Compression: {} bytes → {} bytes (ratio: {:.2}x, total file: {} bytes)",
            uncompressed_size,
            compressed_size,
            compression_ratio,
            total_size
        );

        Ok(())
    }
}
