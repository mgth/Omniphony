//! Which cpal host Windows realtime output opens: ASIO first, WASAPI shared
//! mode when ASIO cannot serve.
//!
//! The decision is a pure function of what the two hosts report, so it is
//! compiled and tested on every platform; only the glue that asks cpal for
//! those reports (`cpal_output.rs`) is Windows-specific. It runs once, when
//! the output stream is opened, never on the audio path.

/// The host realtime output plays through on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostChoice {
    /// ASIO: the default and the first choice.
    Asio,
    /// WASAPI, cpal's default Windows host, in shared mode: the Windows mixer
    /// sets the channel count and the rate.
    WasapiFallback(FallbackReason),
}

/// Why output left ASIO for WASAPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    /// cpal could not open its ASIO host, or the host could not list devices.
    AsioUnavailable,
    /// The ASIO host lists no output device: no ASIO driver is installed, or
    /// none of the installed ones could be loaded.
    NoAsioDevice,
    /// The requested device is not an ASIO device but is a WASAPI one: a
    /// device chosen while output was on WASAPI, kept in the config.
    DeviceOnlyUnderWasapi,
}

impl HostChoice {
    /// The host's short name, for log lines and error messages.
    pub fn host_name(self) -> &'static str {
        match self {
            HostChoice::Asio => "ASIO",
            HostChoice::WasapiFallback(_) => "WASAPI",
        }
    }

    /// What Studio shows as the output host: the host, and why when it is
    /// not the one asked for.
    pub fn label(self) -> &'static str {
        match self {
            HostChoice::Asio => "ASIO",
            HostChoice::WasapiFallback(FallbackReason::AsioUnavailable) => {
                "WASAPI (fallback: ASIO unavailable)"
            }
            HostChoice::WasapiFallback(FallbackReason::NoAsioDevice) => {
                "WASAPI (fallback: no ASIO driver)"
            }
            HostChoice::WasapiFallback(FallbackReason::DeviceOnlyUnderWasapi) => {
                "WASAPI (fallback: device is not an ASIO device)"
            }
        }
    }
}

impl FallbackReason {
    /// The reason in a sentence, for the log line that announces the fallback.
    pub fn describe(self) -> &'static str {
        match self {
            FallbackReason::AsioUnavailable => "the ASIO host could not be opened",
            FallbackReason::NoAsioDevice => {
                "no ASIO output device was found (no ASIO driver installed, or none could be loaded)"
            }
            FallbackReason::DeviceOnlyUnderWasapi => {
                "the requested output device is not an ASIO device but is a WASAPI one"
            }
        }
    }
}

/// Choose the host for one output open.
///
/// - `asio_devices`: the ASIO host's output device names, or `None` when the
///   host could not be opened or could not list its devices.
/// - `requested`: the configured output device name, `None` for the default.
/// - `wasapi_devices`: lists the WASAPI output device names. Called only when
///   a requested device is not under ASIO, so the usual open enumerates one
///   host.
///
/// ASIO wins whenever it has an output device and the request does not name a
/// device only WASAPI has. A requested device found under neither host stays
/// on ASIO, so the open fails naming the device and the ASIO list rather than
/// silently playing somewhere else.
pub fn choose_host(
    asio_devices: Option<&[String]>,
    requested: Option<&str>,
    wasapi_devices: impl FnOnce() -> Vec<String>,
) -> HostChoice {
    let Some(asio_devices) = asio_devices else {
        return HostChoice::WasapiFallback(FallbackReason::AsioUnavailable);
    };
    if asio_devices.is_empty() {
        return HostChoice::WasapiFallback(FallbackReason::NoAsioDevice);
    }
    match requested {
        Some(name) if !asio_devices.iter().any(|device| device == name) => {
            if wasapi_devices().iter().any(|device| device == name) {
                HostChoice::WasapiFallback(FallbackReason::DeviceOnlyUnderWasapi)
            } else {
                HostChoice::Asio
            }
        }
        _ => HostChoice::Asio,
    }
}

/// The error an open gives when the device offers fewer channels than the
/// output needs, instead of dropping the channels it has no room for.
///
/// `shared_mode` is WASAPI's: there the Windows mixer, set by the speaker
/// setup, fixes the channel count (often 2 or 8), so the message says where to
/// raise it.
pub fn too_few_channels_message(
    host_name: &str,
    shared_mode: bool,
    device: &str,
    offered: u16,
    needed: u32,
) -> String {
    let mut message = format!(
        "{host_name} device '{device}' offers at most {offered} output channel(s); \
         this output needs {needed}"
    );
    if shared_mode {
        message.push_str(
            ". WASAPI shared mode plays through the Windows mixer, whose channel count \
             is the device's speaker setup: raise it (Settings > System > Sound > the \
             device > Speaker setup), choose a smaller layout or headphones, or install \
             an ASIO driver for the device",
        );
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    fn unused() -> Vec<String> {
        panic!("WASAPI must not be enumerated for this request")
    }

    #[test]
    fn asio_with_a_device_is_chosen_for_the_default_device() {
        let asio = names(&["Focusrite USB ASIO"]);
        assert_eq!(choose_host(Some(&asio), None, unused), HostChoice::Asio);
    }

    #[test]
    fn a_requested_asio_device_stays_on_asio() {
        let asio = names(&["ASIO4ALL v2", "Focusrite USB ASIO"]);
        assert_eq!(
            choose_host(Some(&asio), Some("Focusrite USB ASIO"), unused),
            HostChoice::Asio
        );
    }

    #[test]
    fn an_unavailable_asio_host_falls_back_to_wasapi() {
        assert_eq!(
            choose_host(None, None, unused),
            HostChoice::WasapiFallback(FallbackReason::AsioUnavailable)
        );
        assert_eq!(
            choose_host(None, Some("Speakers (Realtek(R) Audio)"), unused),
            HostChoice::WasapiFallback(FallbackReason::AsioUnavailable)
        );
    }

    #[test]
    fn no_asio_driver_falls_back_to_wasapi() {
        assert_eq!(
            choose_host(Some(&[]), None, unused),
            HostChoice::WasapiFallback(FallbackReason::NoAsioDevice)
        );
        // The requested name is then looked up under WASAPI by the open
        // itself, which fails naming the WASAPI list if it is not there.
        assert_eq!(
            choose_host(Some(&[]), Some("Anything"), unused),
            HostChoice::WasapiFallback(FallbackReason::NoAsioDevice)
        );
    }

    #[test]
    fn a_requested_device_only_wasapi_has_selects_wasapi() {
        let asio = names(&["ASIO4ALL v2"]);
        let choice = choose_host(Some(&asio), Some("Speakers (Realtek(R) Audio)"), || {
            names(&["Speakers (Realtek(R) Audio)", "HDMI (NVIDIA)"])
        });
        assert_eq!(
            choice,
            HostChoice::WasapiFallback(FallbackReason::DeviceOnlyUnderWasapi)
        );
    }

    #[test]
    fn a_requested_device_neither_host_has_stays_on_asio() {
        let asio = names(&["ASIO4ALL v2"]);
        let choice = choose_host(Some(&asio), Some("Unplugged interface"), || {
            names(&["Speakers (Realtek(R) Audio)"])
        });
        assert_eq!(choice, HostChoice::Asio);
    }

    #[test]
    fn labels_name_the_host_and_the_reason() {
        assert_eq!(HostChoice::Asio.label(), "ASIO");
        assert_eq!(HostChoice::Asio.host_name(), "ASIO");
        let fallback = HostChoice::WasapiFallback(FallbackReason::NoAsioDevice);
        assert_eq!(fallback.host_name(), "WASAPI");
        assert_eq!(fallback.label(), "WASAPI (fallback: no ASIO driver)");
        for reason in [
            FallbackReason::AsioUnavailable,
            FallbackReason::NoAsioDevice,
            FallbackReason::DeviceOnlyUnderWasapi,
        ] {
            let label = HostChoice::WasapiFallback(reason).label();
            assert!(label.starts_with("WASAPI (fallback: "), "{label}");
            assert!(!reason.describe().is_empty());
        }
    }

    #[test]
    fn too_few_channels_names_the_counts_and_the_wasapi_remedy() {
        let asio = too_few_channels_message("ASIO", false, "Card", 8, 12);
        assert_eq!(
            asio,
            "ASIO device 'Card' offers at most 8 output channel(s); this output needs 12"
        );
        let wasapi = too_few_channels_message("WASAPI", true, "Speakers", 2, 12);
        assert!(wasapi.starts_with(
            "WASAPI device 'Speakers' offers at most 2 output channel(s); this output needs 12."
        ));
        assert!(wasapi.contains("Speaker setup"), "{wasapi}");
        assert!(wasapi.contains("ASIO driver"), "{wasapi}");
    }
}
