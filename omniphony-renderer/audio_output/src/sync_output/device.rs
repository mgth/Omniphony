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
    /// How the audio system is to route them to the device's ports, where
    /// it routes by position (PipeWire). Ignored elsewhere: a device's
    /// channels are in order.
    pub channel_map: ChannelMap,
    /// The rate asked for (Hz).
    pub rate: u32,
    /// The cycle asked for (frames at `rate`), where it can be (PipeWire).
    pub quantum: u32,
}

/// How the stream's channels are tagged for the audio system to route them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelMap {
    /// No tags: the audio system links the channels in order, where it can.
    Unpositioned,
    /// The device's own channel map, in order: channel N is tagged with the
    /// device's position N, so that port N carries it. On a device that
    /// states no map, or one the audio system cannot route by, the channels
    /// stay untagged.
    DeviceOrder,
    /// One position per channel (`FL`, `FR`, …): the audio system routes
    /// each to the device port of that position. A name it does not know
    /// leaves its channel untagged.
    Named(Vec<String>),
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

    /// Why the audio system refused the stream, when it said so (PipeWire:
    /// no such target, no format in common with it).
    pub fn refusal(&self) -> Option<String> {
        self.0.refusal()
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

/// The host those devices belong to, as a client shows it (`ASIO`,
/// `WASAPI (fallback: …)`, `CoreAudio`); `None` where there is one host
/// only and nothing to say, or when none can be opened.
pub fn output_host_label() -> Option<&'static str> {
    imp::output_host_label()
}

#[cfg(all(target_os = "linux", not(feature = "cpal-check")))]
mod imp {
    use anyhow::Result;

    use super::{ChannelMap, SyncOutputConfig};
    use crate::pipewire::{channel_position_id, sink_channel_map};
    use crate::sync_output::core::OutputCore;
    use crate::sync_output::pipewire::{PipewireSyncOutput, PipewireSyncOutputConfig};

    pub struct Negotiated {
        config: SyncOutputConfig,
        /// The positions the channels are tagged with, from the map asked
        /// for; `None` leaves them untagged.
        positions: Option<Vec<String>>,
    }

    impl Negotiated {
        pub fn rate(&self) -> u32 {
            self.config.rate
        }

        pub fn host_label(&self) -> Option<&'static str> {
            None
        }

        pub fn start(self, core: OutputCore) -> Result<Output> {
            let Self { config, positions } = self;
            PipewireSyncOutput::start(
                PipewireSyncOutputConfig {
                    node_name: config.node_name,
                    target: config.target,
                    channels: config.channels,
                    positions,
                    rate: config.rate,
                    quantum: config.quantum,
                },
                core,
            )
            .map(Output)
        }
    }

    /// The tags for `channels` channels played in order on `target`, whose
    /// map is `map`: its first `channels` positions. None when the device
    /// states no map, or one made of names PipeWire cannot route by (a null
    /// sink's `1 2 3 …`): the session manager then links in order, which is
    /// the same thing. A device with fewer channels than the stream is
    /// refused: a channel left without a port of its own is not dropped by
    /// the session manager but put on whatever port is free, a MIDI one
    /// included, and the whole stream fails with it.
    fn device_order_positions(
        target: &str,
        map: Option<&[String]>,
        channels: u32,
    ) -> Result<Option<Vec<String>>> {
        let Some(map) = map else {
            return Ok(None);
        };
        if map.iter().any(|name| channel_position_id(name).is_none()) {
            log::info!(
                "PipeWire sync output: {target} has a channel map PipeWire does not route by \
                 ({}): the channels are linked in order, untagged",
                map.join(" ")
            );
            return Ok(None);
        }
        let needed = channels as usize;
        if map.len() < needed {
            anyhow::bail!(
                "{target} has {} channels; this output needs {needed} (by index, port N \
                 carries speaker N)",
                map.len()
            );
        }
        Ok(Some(map[..needed].to_vec()))
    }

    pub struct Output(PipewireSyncOutput);

    impl Output {
        pub fn is_streaming(&self) -> bool {
            self.0.is_streaming()
        }

        pub fn refusal(&self) -> Option<String> {
            self.0.refusal()
        }
    }

    /// PipeWire plays any rate: nothing to agree on but the tags the
    /// channels carry, which a device named by the config may give.
    pub fn negotiate(config: &SyncOutputConfig) -> Result<Negotiated> {
        let positions = match &config.channel_map {
            ChannelMap::Unpositioned => None,
            ChannelMap::Named(names) => Some(names.clone()),
            ChannelMap::DeviceOrder => match config.target.as_deref() {
                // The session's default sink is whichever the session
                // manager picks: no map to read until a device is named.
                None => None,
                Some(target) => {
                    // A daemon that does not answer is not a reason to play
                    // nothing: the stream goes out untagged, as before.
                    let map = sink_channel_map(target).unwrap_or_else(|e| {
                        log::warn!(
                            "PipeWire sync output: the channel map of {target} could not be \
                             read ({e:#}); the channels are left untagged"
                        );
                        None
                    });
                    let positions =
                        device_order_positions(target, map.as_deref(), config.channels)?;
                    if let Some(positions) = &positions {
                        log::info!(
                            "PipeWire sync output: {target} routes by position; the channels take \
                             its map in order ({})",
                            positions.join(" ")
                        );
                    }
                    positions
                }
            },
        };
        Ok(Negotiated {
            config: config.clone(),
            positions,
        })
    }

    pub fn list_output_devices() -> Vec<(String, String)> {
        crate::pipewire::list_pipewire_output_devices().unwrap_or_default()
    }

    pub fn output_host_label() -> Option<&'static str> {
        None
    }

    #[cfg(test)]
    mod tests {
        use super::device_order_positions;

        fn names(list: &[&str]) -> Vec<String> {
            list.iter().map(|s| s.to_string()).collect()
        }

        /// A device's map, in order, as far as the stream is wide.
        #[test]
        fn the_channels_take_the_devices_map_in_order() {
            let sink = names(&[
                "FL", "FR", "FC", "LFE", "SL", "SR", "RL", "RR", "AUX0", "AUX1", "AUX2", "AUX3",
                "AUX4", "AUX5", "AUX6", "AUX7",
            ]);
            assert_eq!(
                device_order_positions("nj2-sink", Some(&sink), 16).unwrap(),
                Some(sink.clone())
            );
            // A narrower stream takes the first ports: a binaural pair on
            // FL and FR.
            assert_eq!(
                device_order_positions("nj2-sink", Some(&sink), 2).unwrap(),
                Some(names(&["FL", "FR"]))
            );
        }

        /// No map, or one PipeWire cannot route by: untagged, linked in
        /// order by the session manager.
        #[test]
        fn a_map_pipewire_does_not_know_leaves_the_channels_untagged() {
            assert_eq!(device_order_positions("sink", None, 16).unwrap(), None);
            let router = names(&["1", "2", "3", "4"]);
            assert_eq!(
                device_order_positions("truehdd_router", Some(&router), 4).unwrap(),
                None
            );
            let partly = names(&["FL", "FR", "3", "4"]);
            assert_eq!(
                device_order_positions("sink", Some(&partly), 4).unwrap(),
                None
            );
        }

        /// A device too narrow for the stream is refused, and the message
        /// says by how much.
        #[test]
        fn a_device_with_fewer_channels_is_refused() {
            let stereo = names(&["FL", "FR"]);
            let error = device_order_positions("usb-dac", Some(&stereo), 16).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("usb-dac has 2 channels"), "{message}");
            assert!(message.contains("needs 16"), "{message}");
        }
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

        /// (A cpal device that cannot be opened fails at `negotiate`.)
        pub fn refusal(&self) -> Option<String> {
            None
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

    pub fn output_host_label() -> Option<&'static str> {
        let _one_at_a_time = host_lock();
        output_host(None).ok().map(|opened| opened.label)
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
