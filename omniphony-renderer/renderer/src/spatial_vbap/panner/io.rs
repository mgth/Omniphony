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

    /// Load VBAP gain tables from a binary file
    ///
    /// Loads pre-computed VBAP tables saved with `save_to_file()`.
    /// This allows skipping expensive table generation for faster startup.
    ///
    /// Supports v3 (compressed + speaker layout), v2 (compressed), and v1 (uncompressed) formats.
    ///
    /// # Returns
    ///
    /// A tuple of (VbapPanner, Option<SpeakerLayout>)
    /// - v3 files include speaker layout (Some)
    /// - v1/v2 files don't include speaker layout (None)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - File cannot be read
    /// - File format is invalid (wrong magic/version)
    /// - Data is corrupted or incomplete
    pub fn load_from_file(
        path: &std::path::Path,
    ) -> Result<(Self, Option<crate::speaker_layout::SpeakerLayout>), String> {
        use flate2::read::ZlibDecoder;
        use std::io::Read;

        let mut file =
            std::fs::File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;

        // Read and validate header
        const MAGIC: u32 = 0x56424150; // "VBAP"

        let mut magic_buf = [0u8; 4];
        file.read_exact(&mut magic_buf)
            .map_err(|e| format!("Failed to read magic: {}", e))?;
        let magic = u32::from_le_bytes(magic_buf);
        if magic != MAGIC {
            return Err(format!(
                "Invalid magic number: expected 0x{:08X}, got 0x{:08X}",
                MAGIC, magic
            ));
        }

        let mut version_buf = [0u8; 4];
        file.read_exact(&mut version_buf)
            .map_err(|e| format!("Failed to read version: {}", e))?;
        let version = u32::from_le_bytes(version_buf);

        // Read common header fields
        let n_speakers = read_u32(&mut file, "n_speakers")? as usize;
        let az_res_deg = read_u32(&mut file, "az_res_deg")? as i32;
        let el_res_deg = read_u32(&mut file, "el_res_deg")? as i32;
        let n_az = read_u32(&mut file, "n_az")? as usize;
        let n_el = read_u32(&mut file, "n_el")? as usize;
        let n_gtable = read_u32(&mut file, "n_gtable")? as usize;
        let n_triangles = read_u32(&mut file, "n_triangles")? as usize;
        let num_spread_tables = read_u32(&mut file, "num_spread_tables")? as usize;
        let spread_resolution = read_f32(&mut file, "spread_resolution")?;

        // Read spread tables and optional speaker layout based on version
        // Returns: (spread_tables, speaker_layout, actual_n_speakers)
        // For v1/v2: actual_n_speakers is from header (spatializable count)
        // For v3: actual_n_speakers is calculated from loaded layout (spatializable count)
        let (spread_tables, speaker_layout, actual_n_speakers) = match version {
            1 => {
                // Version 1: Uncompressed format (no speaker layout)
                // Skip reserved bytes (28 bytes)
                let mut reserved = [0u8; 28];
                file.read_exact(&mut reserved)
                    .map_err(|e| format!("Failed to read reserved: {}", e))?;

                let mut spread_tables = Vec::with_capacity(num_spread_tables);
                for _ in 0..num_spread_tables {
                    let spread = read_f32(&mut file, "spread")?;

                    let total_elements = n_gtable * n_speakers;
                    let mut gtable = Vec::with_capacity(total_elements);
                    for _ in 0..total_elements {
                        let gain = read_f32(&mut file, "gain")?;
                        gtable.push(gain);
                    }

                    spread_tables.push(SpreadTable { spread, gtable });
                }

                log::info!(
                    "Loaded VBAP gain table (v1 uncompressed) from {}: {} speakers, {}x{} resolution, {} spread tables",
                    path.display(),
                    n_speakers,
                    az_res_deg,
                    el_res_deg,
                    spread_tables.len()
                );

                (spread_tables, None, n_speakers)
            }
            2 => {
                // Version 2: Compressed format
                let compression_flag = read_u32(&mut file, "compression_flag")?;

                // Skip reserved bytes (24 bytes in v2)
                let mut reserved = [0u8; 24];
                file.read_exact(&mut reserved)
                    .map_err(|e| format!("Failed to read reserved: {}", e))?;

                if compression_flag != 1 {
                    return Err(format!(
                        "Unsupported compression flag: {}",
                        compression_flag
                    ));
                }

                // Read compressed size and data
                let compressed_size = read_u32(&mut file, "compressed_size")? as usize;
                let mut compressed_data = vec![0u8; compressed_size];
                file.read_exact(&mut compressed_data)
                    .map_err(|e| format!("Failed to read compressed data: {}", e))?;

                // Decompress
                let mut decoder = ZlibDecoder::new(&compressed_data[..]);
                let mut uncompressed_data = Vec::new();
                decoder
                    .read_to_end(&mut uncompressed_data)
                    .map_err(|e| format!("Failed to decompress data: {}", e))?;

                // Parse uncompressed data
                let mut spread_tables = Vec::with_capacity(num_spread_tables);
                let mut offset = 0;

                for _ in 0..num_spread_tables {
                    if offset + 4 > uncompressed_data.len() {
                        return Err("Incomplete spread table data".to_string());
                    }

                    let spread = f32::from_le_bytes([
                        uncompressed_data[offset],
                        uncompressed_data[offset + 1],
                        uncompressed_data[offset + 2],
                        uncompressed_data[offset + 3],
                    ]);
                    offset += 4;

                    let total_elements = n_gtable * n_speakers;
                    let required_bytes = total_elements * 4;

                    if offset + required_bytes > uncompressed_data.len() {
                        return Err(format!(
                            "Incomplete gain table data: need {} bytes, have {} bytes",
                            required_bytes,
                            uncompressed_data.len() - offset
                        ));
                    }

                    let mut gtable = Vec::with_capacity(total_elements);
                    for _ in 0..total_elements {
                        let gain = f32::from_le_bytes([
                            uncompressed_data[offset],
                            uncompressed_data[offset + 1],
                            uncompressed_data[offset + 2],
                            uncompressed_data[offset + 3],
                        ]);
                        offset += 4;
                        gtable.push(gain);
                    }

                    spread_tables.push(SpreadTable { spread, gtable });
                }

                let compression_ratio = (uncompressed_data.len() as f64) / (compressed_size as f64);
                log::info!(
                    "Loaded VBAP gain table (v2 compressed) from {}: {} speakers, {}x{} resolution, {} spread tables",
                    path.display(),
                    n_speakers,
                    az_res_deg,
                    el_res_deg,
                    spread_tables.len()
                );
                log::info!(
                    "  Decompression: {} bytes → {} bytes (ratio: {:.2}x)",
                    compressed_size,
                    uncompressed_data.len(),
                    compression_ratio
                );

                (spread_tables, None, n_speakers)
            }
            3 => {
                // Version 3: Compressed format + speaker layout
                let compression_flag = read_u32(&mut file, "compression_flag")?;
                let layout_data_size = read_u32(&mut file, "layout_data_size")? as usize;

                // Skip reserved bytes (20 bytes in v3)
                let mut reserved = [0u8; 20];
                file.read_exact(&mut reserved)
                    .map_err(|e| format!("Failed to read reserved: {}", e))?;

                if compression_flag != 1 {
                    return Err(format!(
                        "Unsupported compression flag: {}",
                        compression_flag
                    ));
                }

                // Read speaker layout data (uncompressed)
                let mut layout_data = vec![0u8; layout_data_size];
                file.read_exact(&mut layout_data)
                    .map_err(|e| format!("Failed to read speaker layout data: {}", e))?;

                // Parse speaker layout
                let mut offset = 0;
                let mut speakers = Vec::new();

                for _ in 0..n_speakers {
                    if offset + 4 > layout_data.len() {
                        return Err("Incomplete speaker layout data".to_string());
                    }

                    let name_len = u32::from_le_bytes([
                        layout_data[offset],
                        layout_data[offset + 1],
                        layout_data[offset + 2],
                        layout_data[offset + 3],
                    ]) as usize;
                    offset += 4;

                    if offset + name_len > layout_data.len() {
                        return Err("Incomplete speaker name data".to_string());
                    }

                    let name = String::from_utf8(layout_data[offset..offset + name_len].to_vec())
                        .map_err(|e| format!("Invalid UTF-8 in speaker name: {}", e))?;
                    offset += name_len;

                    if offset + 9 > layout_data.len() {
                        return Err("Incomplete speaker position data".to_string());
                    }

                    let azimuth = f32::from_le_bytes([
                        layout_data[offset],
                        layout_data[offset + 1],
                        layout_data[offset + 2],
                        layout_data[offset + 3],
                    ]);
                    offset += 4;

                    let elevation = f32::from_le_bytes([
                        layout_data[offset],
                        layout_data[offset + 1],
                        layout_data[offset + 2],
                        layout_data[offset + 3],
                    ]);
                    offset += 4;

                    let spatialize = layout_data[offset] != 0;
                    offset += 1;

                    speakers.push(crate::speaker_layout::Speaker::from_polar(
                        name, azimuth, elevation, 1.0, spatialize, 0.0,
                    ));
                }

                let speaker_layout = Some(crate::speaker_layout::SpeakerLayout {
                    radius_m: 1.0,
                    speakers: speakers.clone(),
                });

                // Calculate spatializable speaker count for gain table dimensions
                // (gain tables are indexed by spatializable speakers, not total speakers)
                let n_spatializable = speakers.iter().filter(|s| s.spatialize).count();

                // Read compressed size and data
                let compressed_size = read_u32(&mut file, "compressed_size")? as usize;
                let mut compressed_data = vec![0u8; compressed_size];
                file.read_exact(&mut compressed_data)
                    .map_err(|e| format!("Failed to read compressed data: {}", e))?;

                // Decompress
                let mut decoder = ZlibDecoder::new(&compressed_data[..]);
                let mut uncompressed_data = Vec::new();
                decoder
                    .read_to_end(&mut uncompressed_data)
                    .map_err(|e| format!("Failed to decompress data: {}", e))?;

                // Parse uncompressed data
                let mut spread_tables = Vec::with_capacity(num_spread_tables);
                let mut offset = 0;

                for _ in 0..num_spread_tables {
                    if offset + 4 > uncompressed_data.len() {
                        return Err("Incomplete spread table data".to_string());
                    }

                    let spread = f32::from_le_bytes([
                        uncompressed_data[offset],
                        uncompressed_data[offset + 1],
                        uncompressed_data[offset + 2],
                        uncompressed_data[offset + 3],
                    ]);
                    offset += 4;

                    // Use spatializable count for gain table dimensions
                    let total_elements = n_gtable * n_spatializable;
                    let required_bytes = total_elements * 4;

                    if offset + required_bytes > uncompressed_data.len() {
                        return Err(format!(
                            "Incomplete gain table data: need {} bytes, have {} bytes",
                            required_bytes,
                            uncompressed_data.len() - offset
                        ));
                    }

                    let mut gtable = Vec::with_capacity(total_elements);
                    for _ in 0..total_elements {
                        let gain = f32::from_le_bytes([
                            uncompressed_data[offset],
                            uncompressed_data[offset + 1],
                            uncompressed_data[offset + 2],
                            uncompressed_data[offset + 3],
                        ]);
                        offset += 4;
                        gtable.push(gain);
                    }

                    spread_tables.push(SpreadTable { spread, gtable });
                }

                let compression_ratio = (uncompressed_data.len() as f64) / (compressed_size as f64);
                log::info!(
                    "Loaded VBAP gain table (v3 compressed + speaker layout) from {}: {} total speakers ({} spatializable), {}x{} resolution, {} spread tables",
                    path.display(),
                    n_speakers,
                    n_spatializable,
                    az_res_deg,
                    el_res_deg,
                    spread_tables.len()
                );
                log::info!(
                    "  Decompression: {} bytes → {} bytes (ratio: {:.2}x)",
                    compressed_size,
                    uncompressed_data.len(),
                    compression_ratio
                );

                (spread_tables, speaker_layout, n_spatializable)
            }
            4 => {
                // Version 4: Compressed format + speaker layout + expanded gains (all speakers)
                // Gains are stored for ALL speakers (including non-spatializable with 0.0)
                // This enables SIMD optimization during rendering
                let compression_flag = read_u32(&mut file, "compression_flag")?;
                let layout_data_size = read_u32(&mut file, "layout_data_size")? as usize;

                // Skip reserved bytes (20 bytes in v4)
                let mut reserved = [0u8; 20];
                file.read_exact(&mut reserved)
                    .map_err(|e| format!("Failed to read reserved: {}", e))?;

                if compression_flag != 1 {
                    return Err(format!(
                        "Unsupported compression flag: {}",
                        compression_flag
                    ));
                }

                // Read speaker layout data (uncompressed)
                let mut layout_data = vec![0u8; layout_data_size];
                file.read_exact(&mut layout_data)
                    .map_err(|e| format!("Failed to read speaker layout data: {}", e))?;

                // Parse speaker layout
                let mut offset = 0;
                let mut speakers = Vec::new();

                for _ in 0..n_speakers {
                    if offset + 4 > layout_data.len() {
                        return Err("Incomplete speaker layout data".to_string());
                    }

                    let name_len = u32::from_le_bytes([
                        layout_data[offset],
                        layout_data[offset + 1],
                        layout_data[offset + 2],
                        layout_data[offset + 3],
                    ]) as usize;
                    offset += 4;

                    if offset + name_len > layout_data.len() {
                        return Err("Incomplete speaker name data".to_string());
                    }

                    let name = String::from_utf8(layout_data[offset..offset + name_len].to_vec())
                        .map_err(|e| format!("Invalid UTF-8 in speaker name: {}", e))?;
                    offset += name_len;

                    if offset + 9 > layout_data.len() {
                        return Err("Incomplete speaker position data".to_string());
                    }

                    let azimuth = f32::from_le_bytes([
                        layout_data[offset],
                        layout_data[offset + 1],
                        layout_data[offset + 2],
                        layout_data[offset + 3],
                    ]);
                    offset += 4;

                    let elevation = f32::from_le_bytes([
                        layout_data[offset],
                        layout_data[offset + 1],
                        layout_data[offset + 2],
                        layout_data[offset + 3],
                    ]);
                    offset += 4;

                    let spatialize = layout_data[offset] != 0;
                    offset += 1;

                    speakers.push(crate::speaker_layout::Speaker::from_polar(
                        name, azimuth, elevation, 1.0, spatialize, 0.0,
                    ));
                }

                let speaker_layout = Some(crate::speaker_layout::SpeakerLayout {
                    radius_m: 1.0,
                    speakers,
                });

                // Read compressed size and data
                let compressed_size = read_u32(&mut file, "compressed_size")? as usize;
                let mut compressed_data = vec![0u8; compressed_size];
                file.read_exact(&mut compressed_data)
                    .map_err(|e| format!("Failed to read compressed data: {}", e))?;

                // Decompress
                let mut decoder = ZlibDecoder::new(&compressed_data[..]);
                let mut uncompressed_data = Vec::new();
                decoder
                    .read_to_end(&mut uncompressed_data)
                    .map_err(|e| format!("Failed to decompress data: {}", e))?;

                // Parse uncompressed data
                // v4: gains are stored for ALL speakers (n_speakers total)
                let mut spread_tables = Vec::with_capacity(num_spread_tables);
                let mut offset = 0;

                for _ in 0..num_spread_tables {
                    if offset + 4 > uncompressed_data.len() {
                        return Err("Incomplete spread table data".to_string());
                    }

                    let spread = f32::from_le_bytes([
                        uncompressed_data[offset],
                        uncompressed_data[offset + 1],
                        uncompressed_data[offset + 2],
                        uncompressed_data[offset + 3],
                    ]);
                    offset += 4;

                    // v4: use total speaker count for gain table dimensions
                    let total_elements = n_gtable * n_speakers;
                    let required_bytes = total_elements * 4;

                    if offset + required_bytes > uncompressed_data.len() {
                        return Err(format!(
                            "Incomplete gain table data: need {} bytes, have {} bytes",
                            required_bytes,
                            uncompressed_data.len() - offset
                        ));
                    }

                    let mut gtable = Vec::with_capacity(total_elements);
                    for _ in 0..total_elements {
                        let gain = f32::from_le_bytes([
                            uncompressed_data[offset],
                            uncompressed_data[offset + 1],
                            uncompressed_data[offset + 2],
                            uncompressed_data[offset + 3],
                        ]);
                        offset += 4;
                        gtable.push(gain);
                    }

                    spread_tables.push(SpreadTable { spread, gtable });
                }

                let compression_ratio = (uncompressed_data.len() as f64) / (compressed_size as f64);
                log::info!(
                    "Loaded VBAP gain table (v4 expanded) from {}: {} speakers, {}x{} resolution, {} spread tables",
                    path.display(),
                    n_speakers,
                    az_res_deg,
                    el_res_deg,
                    spread_tables.len()
                );
                log::info!(
                    "  Decompression: {} bytes → {} bytes (ratio: {:.2}x)",
                    compressed_size,
                    uncompressed_data.len(),
                    compression_ratio
                );

                // v4: use total speaker count (gains include zeros for non-spatializable)
                (spread_tables, speaker_layout, n_speakers)
            }
            _ => {
                return Err(format!("Unsupported version: {}", version));
            }
        };

        let panner = VbapPanner {
            spread_tables,
            spread_resolution,
            n_gtable,
            n_triangles,
            n_speakers: actual_n_speakers,
            az_res_deg,
            el_res_deg,
            n_az,
            n_el,
            table_mode: VbapTableMode::Polar,
            allow_negative_z: true,
            position_interpolation: true,
            cartesian_cache: None,
            #[cfg(feature = "saf_vbap")]
            speaker_dirs_deg: None,
        };

        Ok((panner, speaker_layout))
    }
}

// Helper functions for binary I/O
fn read_u32<R: std::io::Read>(reader: &mut R, name: &str) -> Result<u32, String> {
    let mut buf = [0u8; 4];
    reader
        .read_exact(&mut buf)
        .map_err(|e| format!("Failed to read {}: {}", name, e))?;
    Ok(u32::from_le_bytes(buf))
}

fn read_f32<R: std::io::Read>(reader: &mut R, name: &str) -> Result<f32, String> {
    let mut buf = [0u8; 4];
    reader
        .read_exact(&mut buf)
        .map_err(|e| format!("Failed to read {}: {}", name, e))?;
    Ok(f32::from_le_bytes(buf))
}
