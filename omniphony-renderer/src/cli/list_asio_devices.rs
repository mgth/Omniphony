//! List available ASIO output devices (Windows only)

use anyhow::Result;

/// Execute the list-asio-devices command
///
/// Lists all available ASIO output devices on the system.
pub fn cmd_list_asio_devices() -> Result<()> {
    println!();
    println!("Available ASIO devices:");
    println!();

    let devices = audio_output::asio::list_asio_devices()?;

    if devices.is_empty() {
        println!("  No ASIO devices found.");
        println!();
        println!("Make sure you have ASIO drivers installed.");
        println!("Common ASIO drivers:");
        println!("  - FlexASIO (universal ASIO driver)");
        println!("  - ASIO4ALL (universal ASIO driver)");
        println!("  - Manufacturer-specific drivers for your audio interface");
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
