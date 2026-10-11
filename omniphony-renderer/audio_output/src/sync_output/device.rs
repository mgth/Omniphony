//! One output device for the sync stage, whatever the platform: PipeWire on
//! Linux, cpal (ASIO or WASAPI, CoreAudio) on Windows and macOS.
//!
//! Opening is in two steps, because the two worlds differ on who converts a
//! rate the device does not run at. PipeWire takes any rate and converts in
//! the graph. A cpal device plays the rates it lists and no other, so the
//! output core has to resample to one of them, and it is built from its
//! rates: [`negotiate`] says what rate the device will run at, the caller
//! builds its [`OutputCore`] for it, and [`Negotiated::start`] plays.
//!
//! The `cpal-check` feature selects the cpal side on Linux too (through
//! cpal's ALSA host), so that it is compiled, and can be run, where the
//! project is developed.

use anyhow::Result;

use super::core::OutputCore;

/// What an output is opened for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutputConfig {
    /// The stream's name in the audio system (PipeWire's `node.name`).
    pub node_name: String,
    /// The device to play on, by the name [`list_output_devices`] gives;
    /// `None` is the system's default.
    pub target: Option<String>,
    /// Channels the ring holds.
    pub channels: u32,
    /// Their positions (`FL`, `FR`, …), where the audio system routes by
    /// them (PipeWire). Ignored elsewhere: a device's channels are in order.
    pub positions: Option<Vec<String>>,
    /// The rate asked for (Hz).
    pub rate: u32,
    /// The cycle asked for (frames at `rate`), where it can be (PipeWire).
    pub quantum: u32,
}

/// A device that agreed to a format, not started yet.
pub struct Negotiated(imp::Negotiated);

impl Negotiated {
    /// The rate the device will run at (Hz): the one asked for when it can,
    /// else one it has. The output core converts to it.
    pub fn rate(&self) -> u32 {
        self.0.rate()
    }

    /// The host the device belongs to, as a client shows it (`ASIO`,
    /// `WASAPI (fallback: …)`, `CoreAudio`); `None` where there is one host
    /// only and nothing to say.
    pub fn host_label(&self) -> Option<&'static str> {
        self.0.host_label()
    }

    /// Start playing what `core` produces.
    pub fn start(self, core: OutputCore) -> Result<SyncOutput> {
        self.0.start(core).map(SyncOutput)
    }
}

/// A running output. Dropping it stops the stream.
pub struct SyncOutput(imp::Output);

impl SyncOutput {
    /// Whether the device is taking audio (PipeWire: the stream is linked
    /// and running; cpal: it was started and reported no error since).
    pub fn is_streaming(&self) -> bool {
        self.0.is_streaming()
    }
}

/// Find the device `config` names and agree on a format with it.
pub fn negotiate(config: &SyncOutputConfig) -> Result<Negotiated> {
    imp::negotiate(config).map(Negotiated)
}

/// The devices an output can be opened on: the name to ask for, and a label
/// to show.
pub fn list_output_devices() -> Vec<(String, String)> {
    imp::list_output_devices()
}

#[cfg(all(target_os = "linux", not(feature = "cpal-check")))]
mod imp {
    use anyhow::Result;

    use super::SyncOutputConfig;
    use crate::sync_output::core::OutputCore;
    use crate::sync_output::pipewire::{PipewireSyncOutput, PipewireSyncOutputConfig};

    pub struct Negotiated(SyncOutputConfig);

    impl Negotiated {
        pub fn rate(&self) -> u32 {
            self.0.rate
        }

        pub fn host_label(&self) -> Option<&'static str> {
            None
        }

        pub fn start(self, core: OutputCore) -> Result<Output> {
            let config = self.0;
            PipewireSyncOutput::start(
                PipewireSyncOutputConfig {
                    node_name: config.node_name,
                    target: config.target,
                    channels: config.channels,
                    positions: config.positions,
                    rate: config.rate,
                    quantum: config.quantum,
                },
                core,
            )
            .map(Output)
        }
    }

    pub struct Output(PipewireSyncOutput);

    impl Output {
        pub fn is_streaming(&self) -> bool {
            self.0.is_streaming()
        }
    }

    /// PipeWire plays any rate: there is nothing to agree on.
    pub fn negotiate(config: &SyncOutputConfig) -> Result<Negotiated> {
        Ok(Negotiated(config.clone()))
    }

    pub fn list_output_devices() -> Vec<(String, String)> {
        crate::pipewire::list_pipewire_output_devices().unwrap_or_default()
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", feature = "cpal-check"))]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use anyhow::{Result, anyhow};
    use cpal::traits::{DeviceTrait, HostTrait};

    use super::SyncOutputConfig;
    use crate::host_choice::too_few_channels_message;
    use crate::sync_output::core::OutputCore;
    use crate::sync_output::cpal_adapter::CpalSyncOutput;

    /// One thread at a time opens a host and walks its devices: the engine
    /// thread opens outputs while the control thread lists the devices for a
    /// client, and the ASIO and COM layers under cpal are not to be entered
    /// from two threads at once.
    static HOST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn host_lock() -> std::sync::MutexGuard<'static, ()> {
        HOST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The cpal host outputs are opened on.
    struct OutputHost {
        host: cpal::Host,
        /// Its short name (`ASIO`, `WASAPI`, `CoreAudio`), for messages.
        name: &'static str,
        /// What a client shows, with the fallback's reason on Windows.
        label: &'static str,
        /// WASAPI shared mode: the Windows mixer fixes the channel count.
        shared_mode: bool,
    }

    #[cfg(target_os = "windows")]
    fn device_names(host: &cpal::Host) -> Option<Vec<String>> {
        host.output_devices()
            .map(|devices| devices.filter_map(|d| d.name().ok()).collect())
            .map_err(|e| log::debug!("{:?} host cannot list output devices: {e}", host.id()))
            .ok()
    }

    /// The ASIO host when it has an output device, else WASAPI in shared
    /// mode (`host_choice::choose_host`, the legacy output's rule). A device
    /// only WASAPI lists selects WASAPI.
    #[cfg(target_os = "windows")]
    fn output_host(requested: Option<&str>) -> Result<OutputHost> {
        use crate::host_choice::{HostChoice, choose_host};

        let asio = cpal::host_from_id(cpal::HostId::Asio)
            .map_err(|e| log::debug!("ASIO host not available: {e:?}"))
            .ok();
        let asio_devices = asio.as_ref().and_then(device_names);
        let choice = choose_host(asio_devices.as_deref(), requested, || {
            cpal::host_from_id(cpal::HostId::Wasapi)
                .ok()
                .as_ref()
                .and_then(device_names)
                .unwrap_or_default()
        });
        let host = match (choice, asio) {
            (HostChoice::Asio, Some(host)) => host,
            (HostChoice::Asio, None) => return Err(anyhow!("ASIO host not available")),
            (HostChoice::WasapiFallback(reason), _) => {
                log::warn!(
                    "ASIO output not used: {}; falling back to WASAPI (shared mode)",
                    reason.describe()
                );
                cpal::host_from_id(cpal::HostId::Wasapi)
                    .map_err(|e| anyhow!("WASAPI host not available either: {e:?}"))?
            }
        };
        Ok(OutputHost {
            host,
            name: choice.host_name(),
            label: choice.label(),
            shared_mode: matches!(choice, HostChoice::WasapiFallback(_)),
        })
    }

    #[cfg(target_os = "macos")]
    fn output_host(_requested: Option<&str>) -> Result<OutputHost> {
        Ok(OutputHost {
            host: cpal::default_host(),
            name: "CoreAudio",
            label: "CoreAudio",
            shared_mode: false,
        })
    }

    /// `cpal-check` on Linux: cpal's default host there (ALSA).
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn output_host(_requested: Option<&str>) -> Result<OutputHost> {
        Ok(OutputHost {
            host: cpal::default_host(),
            name: "ALSA",
            label: "ALSA",
            shared_mode: false,
        })
    }

    pub struct Negotiated {
        device: cpal::Device,
        config: cpal::StreamConfig,
        format: cpal::SampleFormat,
        label: &'static str,
    }

    impl Negotiated {
        pub fn rate(&self) -> u32 {
            self.config.sample_rate.0
        }

        pub fn host_label(&self) -> Option<&'static str> {
            Some(self.label)
        }

        pub fn start(self, core: OutputCore) -> Result<Output> {
            let _one_at_a_time = host_lock();
            let failed = Arc::new(AtomicBool::new(false));
            let stream = CpalSyncOutput::start_watched(
                &self.device,
                &self.config,
                self.format,
                core,
                Arc::clone(&failed),
            )?;
            Ok(Output {
                _stream: stream,
                failed,
            })
        }
    }

    pub struct Output {
        _stream: CpalSyncOutput,
        /// Set by the stream's error callback (a device that went away).
        failed: Arc<AtomicBool>,
    }

    impl Output {
        pub fn is_streaming(&self) -> bool {
            !self.failed.load(Ordering::Relaxed)
        }
    }

    /// One format a device lists: a channel count, a range of rates, a
    /// sample format.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Offer {
        pub channels: u16,
        pub min_rate: u32,
        pub max_rate: u32,
        pub f32: bool,
    }

    /// The offer to open and the rate to open it at, for `channels` at
    /// `rate`: among the offers wide enough, the rate asked for when one
    /// has it, else the device's own (`default_rate`) when one has that,
    /// else the nearest one the first of them has; then the narrowest
    /// offer at that rate, in `f32` when there is a choice. `None` when no
    /// offer is wide enough.
    pub(super) fn choose_offer(
        offers: &[Offer],
        channels: u32,
        rate: u32,
        default_rate: Option<u32>,
    ) -> Option<(usize, u32)> {
        let wide = |o: &&Offer| u32::from(o.channels) >= channels;
        let has = |o: &Offer, rate: u32| o.min_rate <= rate && rate <= o.max_rate;
        let any_has = |rate: u32| offers.iter().filter(wide).any(|o| has(o, rate));
        let first = offers.iter().find(wide)?;
        let rate = if any_has(rate) {
            rate
        } else if let Some(own) = default_rate.filter(|own| any_has(*own)) {
            own
        } else {
            rate.clamp(first.min_rate, first.max_rate)
        };
        offers
            .iter()
            .enumerate()
            .filter(|(_, o)| wide(o) && has(o, rate))
            .min_by_key(|(_, o)| (o.channels, !o.f32))
            .map(|(index, _)| (index, rate))
    }

    pub fn negotiate(config: &SyncOutputConfig) -> Result<Negotiated> {
        let _one_at_a_time = host_lock();
        let OutputHost {
            host,
            name,
            label,
            shared_mode,
        } = output_host(config.target.as_deref())?;
        let device = match config.target.as_deref() {
            Some(target) => host
                .output_devices()?
                .find(|device| device.name().is_ok_and(|n| n == target))
                .ok_or_else(|| anyhow!("{name} output device '{target}' not found"))?,
            None => host
                .default_output_device()
                .ok_or_else(|| anyhow!("no default {name} output device"))?,
        };
        let device_name = device.name().unwrap_or_default();
        let ranges: Vec<cpal::SupportedStreamConfigRange> =
            device.supported_output_configs()?.collect();
        let offers: Vec<Offer> = ranges
            .iter()
            .map(|r| Offer {
                channels: r.channels(),
                min_rate: r.min_sample_rate().0,
                max_rate: r.max_sample_rate().0,
                f32: r.sample_format() == cpal::SampleFormat::F32,
            })
            .collect();
        let default_rate = device
            .default_output_config()
            .ok()
            .map(|c| c.sample_rate().0);
        let Some((index, rate)) = choose_offer(&offers, config.channels, config.rate, default_rate)
        else {
            // A device with fewer channels than the output is refused, never
            // played with channels dropped.
            let offered = offers.iter().map(|o| o.channels).max().unwrap_or(0);
            return Err(anyhow!(too_few_channels_message(
                name,
                shared_mode,
                &device_name,
                offered,
                config.channels,
            )));
        };
        let range = &ranges[index];
        if rate != config.rate {
            log::info!(
                "{name} device '{device_name}' does not run at {} Hz: playing at {rate} Hz",
                config.rate
            );
        }
        log::info!(
            "{label} output on '{device_name}': {} channels at {rate} Hz, {:?}",
            range.channels(),
            range.sample_format()
        );
        Ok(Negotiated {
            format: range.sample_format(),
            config: range.with_sample_rate(cpal::SampleRate(rate)).config(),
            device,
            label,
        })
    }

    pub fn list_output_devices() -> Vec<(String, String)> {
        let _one_at_a_time = host_lock();
        output_host(None)
            .and_then(|opened| Ok(opened.host.output_devices()?.collect::<Vec<_>>()))
            .map_err(|e| log::debug!("listing the output devices: {e:#}"))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|device| device.name().ok())
            .map(|name| (name.clone(), name))
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn offer(channels: u16, min_rate: u32, max_rate: u32, f32: bool) -> Offer {
            Offer {
                channels,
                min_rate,
                max_rate,
                f32,
            }
        }

        /// The rate asked for when the device has it, on the narrowest
        /// offer wide enough, in `f32` when there is a choice.
        #[test]
        fn the_rate_asked_for_on_the_narrowest_offer() {
            let offers = [
                offer(2, 44_100, 192_000, true),
                offer(16, 44_100, 96_000, false),
                offer(12, 44_100, 96_000, false),
                offer(12, 44_100, 96_000, true),
            ];
            assert_eq!(
                choose_offer(&offers, 12, 48_000, Some(96_000)),
                Some((3, 48_000))
            );
            assert_eq!(choose_offer(&offers, 2, 192_000, None), Some((0, 192_000)));
            assert_eq!(choose_offer(&offers, 14, 48_000, None), Some((1, 48_000)));
        }

        /// A device that does not run at the rate asked for plays at its
        /// own, or at the nearest it has: the output core converts. (The
        /// Windows mixer has one rate; an ASIO device often has one too.)
        #[test]
        fn a_rate_the_device_lacks_becomes_one_it_has() {
            let mixer = [offer(8, 48_000, 48_000, true)];
            assert_eq!(
                choose_offer(&mixer, 6, 44_100, Some(48_000)),
                Some((0, 48_000))
            );
            assert_eq!(choose_offer(&mixer, 6, 96_000, None), Some((0, 48_000)));
            let ranged = [offer(8, 88_200, 192_000, true)];
            assert_eq!(
                choose_offer(&ranged, 8, 44_100, Some(96_000)),
                Some((0, 96_000))
            );
            assert_eq!(
                choose_offer(&ranged, 8, 44_100, Some(32_000)),
                Some((0, 88_200))
            );
        }

        /// Too few channels is no offer at all: nothing is dropped.
        #[test]
        fn a_device_too_narrow_is_refused() {
            let stereo = [offer(2, 44_100, 48_000, true)];
            assert_eq!(choose_offer(&stereo, 6, 48_000, Some(48_000)), None);
            assert_eq!(choose_offer(&[], 2, 48_000, None), None);
        }
    }
}
