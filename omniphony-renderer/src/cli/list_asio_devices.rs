//! List the realtime output devices (Windows only): ASIO, or WASAPI when
//! output falls back to it.

use anyhow::Result;

/// Execute the list-asio-devices command
///
/// Lists the output devices of the host realtime output opens: the ASIO
/// devices, or, when ASIO has none, the WASAPI ones output falls back to.
pub fn cmd_list_asio_devices() -> Result<()> {
    let (host, devices) = audio_output::list_output_host_devices()?;
    let fallback = host != audio_output::host_choice::HostChoice::Asio.label();

    println!();
    println!("Output host: {host}");
    println!();

    if fallback {
        println!("No ASIO device is available, so realtime output uses WASAPI (shared mode).");
        println!("WASAPI plays through the Windows mixer: the output gets the channel count");
        println!("of the device's speaker setup in Windows sound settings, and a layout with");
        println!("more channels than that is refused. For more channels, or lower latency,");
        println!("install an ASIO driver:");
        println!("  - Manufacturer-specific drivers for your audio interface");
        println!("  - FlexASIO (universal ASIO driver)");
        println!("  - ASIO4ALL (universal ASIO driver)");
        println!();
    }

    if devices.is_empty() {
        println!("  No output devices found.");
    } else {
        println!(
            "Available {} devices:",
            if fallback { "WASAPI" } else { "ASIO" }
        );
        println!();
        for (idx, device) in devices.iter().enumerate() {
            println!("  {}. {}", idx + 1, device);
        }
        println!();
        println!("Use --output-device with the exact device name to select a device.");
    }

    println!();
    Ok(())
}
