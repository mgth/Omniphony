//! Generate VBAP gain tables from speaker layout configuration

use anyhow::Result;

use super::command::GenerateVbapArgs;
use renderer::spatial_vbap::VbapPanner;
use renderer::speaker_layout::SpeakerLayout;

/// Execute the generate-vbap command
///
/// Loads a speaker layout from YAML and generates pre-computed VBAP gain tables
/// that can be used during playback for faster initialization.
pub fn cmd_generate_vbap(args: &GenerateVbapArgs) -> Result<()> {
    log::info!("Generating VBAP gain tables");
    log::info!("  Speaker layout: {}", args.speaker_layout.display());
    log::info!("  Output file: {}", args.output.display());
    log::info!("  Azimuth resolution: {}°", args.az_res);
    log::info!("  Elevation resolution: {}°", args.el_res);
    log::info!("  Spread resolution: {}", args.spread_res);

    // Load speaker layout from YAML file
    log::info!("Loading speaker layout...");
    let layout = SpeakerLayout::from_file(&args.speaker_layout)?;

    log::info!(
        "Loaded speaker layout: {} speakers ({})",
        layout.num_speakers(),
        layout.speaker_names().join(", ")
    );

    // Get spatializable speakers (excludes LFE, etc.)
    let (spatializable_positions, mapping) = layout.spatializable_positions();

    log::info!(
        "Spatializable speakers: {} of {}",
        spatializable_positions.len(),
        layout.num_speakers()
    );

    // Log excluded speakers
    let excluded: Vec<&str> = layout
        .speakers
        .iter()
        .filter(|s| !s.spatialize)
        .map(|s| s.name.as_str())
        .collect();
    if !excluded.is_empty() {
        log::info!("Excluded from VBAP: {}", excluded.join(", "));
    }

    // Log speaker positions for verification
    for (vbap_idx, &speaker_idx) in mapping.iter().enumerate() {
        let speaker = &layout.speakers[speaker_idx];
        let pos = &spatializable_positions[vbap_idx];
        log::debug!(
            "  VBAP[{}] = Speaker {} '{}' at az={:.1}° el={:.1}°",
            vbap_idx,
            speaker_idx,
            speaker.name,
            pos[0],
            pos[1]
        );
    }

    // Create VBAP panner with specified parameters
    log::info!("Generating VBAP gain tables (this may take a few seconds)...");
    let start_time = std::time::Instant::now();

    if args.spread_res > 0.0 {
        log::warn!(
            "spread_res is ignored in direct vbap3D mode; using a single continuous-spread panner"
        );
    }
    let vbap = VbapPanner::new(&spatializable_positions, args.az_res, args.el_res, 0.0)
        .map_err(|e| anyhow::anyhow!("Failed to create VBAP panner: {}", e))?;

    let elapsed = start_time.elapsed();
    log::info!("VBAP tables generated in {:.2}s", elapsed.as_secs_f64());
    log::info!("  Triangulation: {} triangles found", vbap.num_triangles());

    // Save to binary file (includes speaker layout)
    log::info!("Saving gain tables to {}...", args.output.display());
    vbap.save_to_file(&args.output, &layout)
        .map_err(|e| anyhow::anyhow!("Failed to save VBAP table: {}", e))?;

    // Report file size
    if let Ok(metadata) = std::fs::metadata(&args.output) {
        let size_kb = metadata.len() as f64 / 1024.0;
        log::info!("Output file size: {:.2} KB", size_kb);
    }

    println!();
    println!("✓ VBAP gain table generated successfully");
    println!();
    println!("Summary:");
    println!("  Speakers:          {}", layout.num_speakers());
    println!("  Spatializable:     {}", spatializable_positions.len());
    println!("  Triangles:         {}", vbap.num_triangles());
    println!("  Azimuth res:       {}°", args.az_res);
    println!("  Elevation res:     {}°", args.el_res);
    println!("  Spread res:        {}", args.spread_res);
    println!("  Generation time:   {:.2}s", elapsed.as_secs_f64());
    println!("  Output file:       {}", args.output.display());
    println!();
    println!("This VBAP table is self-contained and includes the speaker layout.");
    println!("You can now use it with gsrd decode by specifying:");
    println!("  --enable-vbap --vbap-table {}", args.output.display());
    println!();
    println!(
        "Note: --speaker-layout is optional when using --vbap-table (layout is embedded in the file)"
    );
    println!();

    Ok(())
}
