//! List available CoreAudio output devices (macOS only)

use anyhow::Result;

/// Execute the list-coreaudio-devices command
///
/// Lists all available CoreAudio output devices on the system.
pub fn cmd_list_coreaudio_devices() -> Result<()> {
    println!();
    println!("Available CoreAudio devices:");
    println!();

    let devices: Vec<String> = audio_output::sync_output::list_output_devices()
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    if devices.is_empty() {
        println!("  No CoreAudio output devices found.");
    } else {
        for (idx, device) in devices.iter().enumerate() {
            println!("  {}. {}", idx + 1, device);
        }
        println!();
        println!("Use --output-device with the exact device name to select a device.");
    }

    println!();
    Ok(())
}
