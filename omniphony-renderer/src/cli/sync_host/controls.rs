//! The host's control surface: what OSC clients (the Studio) ask of the
//! output, and what the host tells them it did.
//!
//! The requested values live in an [`AudioControl`], behind the same handler
//! as the legacy host's ([`HostAudio`], here with the sync engine's rows and
//! capabilities): the output device, the latency target, and the two
//! settings of the latency loop. The host reads them back on every turn of
//! its loop ([`Controls::settings`]) and reopens its output when they
//! differ from what plays, so a write takes effect as it arrives. They reach
//! `config.yaml` only through Save, like every setting that changes the
//! render (`docs/persistence-policy.md`).

use std::sync::Arc;

use audio_input::{InputBackend, InputControl, InputMode};
use audio_output::control::{
    AudioControl, OutputDeviceOption, RequestedAudioOutputConfig, SourceClock,
};
use host_audio::HostAudio;
use renderer::live_params::RendererControl;
use runtime_control::HostControlHandler;

use super::{DEFAULT_TARGET_LATENCY_MS, HostConfig, RENDER_RATE, Source};

/// What the output is opened with: a change in any of it reopens it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSettings {
    /// End-to-end latency to hold (ms).
    pub target_ms: f64,
    /// PipeWire sink to play into (pinned); `None` is the session's choice.
    pub output_target: Option<String>,
    /// Phase-loop bandwidth (Hz); `None` is the source's own default.
    pub loop_bandwidth_hz: Option<f64>,
    /// Latency error beyond which the output realigns (ms).
    pub realign_threshold_ms: f64,
    /// The clock the source is taken to run on.
    pub source_clock: SourceClock,
}

/// See the [module docs](self).
pub struct Controls {
    audio: Arc<AudioControl>,
    host: Arc<HostAudio>,
}

fn output_devices() -> Vec<OutputDeviceOption> {
    audio_output::pipewire::list_pipewire_output_devices()
        .unwrap_or_default()
        .into_iter()
        .map(|(value, label)| OutputDeviceOption { value, label })
        .collect()
}

impl Controls {
    /// The controls of a run, seeded with what it was launched with.
    pub fn new(config: &HostConfig, renderer: Arc<RendererControl>) -> Self {
        let audio = Arc::new(AudioControl::new(RequestedAudioOutputConfig {
            output_device: config.output_target.clone(),
            latency_target_ms: Some(config.target_latency_ms.round().max(1.0) as u32),
            loop_bandwidth_hz: config.loop_bandwidth_hz.unwrap_or(0.0) as f32,
            realign_threshold_ms: config.realign_threshold_ms as f32,
            source_clock: config.source_clock,
            ..RequestedAudioOutputConfig::default()
        }));
        audio.set_available_output_devices(output_devices());
        audio.set_device_list_fetcher(output_devices);
        // The input is not this host's to reconfigure yet (it does not say
        // it is: `HostCapabilities::input`): its control only says what the
        // run was launched on, for the client to show.
        let input = Arc::new(InputControl::default());
        if let Source::OwnSink {
            node_name,
            description,
            ..
        } = &config.source
        {
            input.set_requested_mode(InputMode::Pipewire);
            input.set_input_state(
                InputMode::Pipewire,
                Some(InputBackend::Pipewire),
                None,
                None,
                Some(node_name.clone()),
                Some(description.clone()),
                None,
            );
        }
        let host = Arc::new(HostAudio::for_sync_engine(
            renderer,
            Arc::clone(&audio),
            input,
        ));
        // Launch values come from config.yaml, which is edited by hand:
        // bound them as an OSC write would be.
        host.bound_options_to_their_kinds();
        Self { audio, host }
    }

    /// The handler the OSC server delegates this host's domains to.
    pub fn handler(&self) -> Arc<dyn HostControlHandler> {
        Arc::clone(&self.host) as Arc<dyn HostControlHandler>
    }

    /// What is asked for now.
    pub fn settings(&self) -> OutputSettings {
        let requested = self.audio.requested_snapshot();
        OutputSettings {
            target_ms: requested
                .latency_target_ms
                .map_or(DEFAULT_TARGET_LATENCY_MS, f64::from),
            output_target: requested.output_device,
            loop_bandwidth_hz: (requested.loop_bandwidth_hz > 0.0)
                .then(|| f64::from(requested.loop_bandwidth_hz)),
            realign_threshold_ms: f64::from(requested.realign_threshold_ms),
            source_clock: requested.source_clock,
        }
    }

    /// The output opened with `settings`: publish what plays.
    pub fn opened(&self, settings: &OutputSettings) {
        self.audio.set_audio_state(RENDER_RATE, "f32");
        self.audio
            .set_effective_output_device(settings.output_target.clone());
        self.audio.set_audio_error(None);
        self.host.mark_output_changed();
    }

    /// The latency held in place of the target, when the stream needs more
    /// (`None`: the target itself). Published for the client to warn about.
    pub fn held_target(&self, held_ms: Option<f64>) {
        self.audio.set_latency_held_ms(held_ms);
        self.host.mark_output_changed();
    }

    /// The output could not be opened: publish why, for the client to show.
    pub fn failed(&self, error: String) {
        self.audio.set_effective_output_device(None);
        self.audio.set_audio_error(Some(error));
        self.host.mark_output_changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orender_engine::{OscOverrides, OscSettings};
    use renderer::options::RawOptionValue;
    use renderer::test_support::fixture_control;

    fn config() -> HostConfig {
        HostConfig {
            config_path: None,
            resolved_config: None,
            speaker_layout: None,
            channel_render_mode: None,
            bridge_paths: Vec::new(),
            source: Source::Pipe {
                path: "/in".into(),
                drain: true,
            },
            target_latency_ms: 180.0,
            loop_bandwidth_hz: None,
            realign_threshold_ms: 20.0,
            output_target: Some("dac".into()),
            output: crate::cli::sync_host::OutputKind::Device,
            source_clock: SourceClock::Auto,
            continuous: true,
            offline: false,
            quantum: 1024,
            osc: OscSettings::resolve(None, &OscOverrides::default(), false),
            seconds: None,
            report_every: 0.0,
        }
    }

    /// The controls start from what the run was launched with, and a write
    /// through the handler (an OSC option) is what the host reads next.
    #[test]
    fn a_written_option_is_the_next_setting() {
        let controls = Controls::new(&config(), fixture_control());
        let launched = controls.settings();
        assert_eq!(
            launched,
            OutputSettings {
                target_ms: 180.0,
                output_target: Some("dac".into()),
                loop_bandwidth_hz: None,
                realign_threshold_ms: 20.0,
                source_clock: SourceClock::Auto,
            }
        );

        let handler = controls.handler();
        let applied = handler.apply_options(&[
            ("latency_target", RawOptionValue::Number(120.0)),
            ("output_device", RawOptionValue::Str("other")),
            ("loop_bandwidth_hz", RawOptionValue::Number(0.01)),
            ("realign_threshold_ms", RawOptionValue::Number(35.0)),
            ("source_clock", RawOptionValue::Str("none")),
        ]);
        assert!(applied.changed);
        assert!(applied.results.iter().all(Option::is_some));
        let now = controls.settings();
        assert_eq!(now.target_ms, 120.0);
        assert_eq!(now.output_target.as_deref(), Some("other"));
        assert!((now.loop_bandwidth_hz.unwrap() - 0.01).abs() < 1e-9);
        assert_eq!(now.realign_threshold_ms, 35.0);
        assert_eq!(now.source_clock, SourceClock::None);
        assert_ne!(now, launched);

        // Unset: the host's own default, not zero.
        handler.apply_options(&[("latency_target", RawOptionValue::Null)]);
        assert_eq!(controls.settings().target_ms, DEFAULT_TARGET_LATENCY_MS);
    }

    /// The sync engine's host declares its own rows and none of the legacy
    /// resampler's: a write to one of those is refused, not stored for an
    /// output that would ignore it.
    #[test]
    fn the_legacy_tuning_is_not_this_hosts() {
        let controls = Controls::new(&config(), fixture_control());
        let handler = controls.handler();
        let schema = handler.options_schema();
        let keys: Vec<&str> = schema
            .iter()
            .map(|entry| entry["key"].as_str().unwrap())
            .collect();
        assert_eq!(
            keys,
            [
                "output_device",
                "latency_target",
                "loop_bandwidth_hz",
                "source_clock",
                "realign_threshold_ms"
            ]
        );
        let refused = handler.apply_options(&[
            ("adaptive_resampling_kp_near", RawOptionValue::Number(2.0)),
            ("live_input_clock_mode", RawOptionValue::Str("dac")),
            ("output_sample_rate", RawOptionValue::Number(96_000.0)),
        ]);
        assert!(refused.results.iter().all(Option::is_none));
        assert!(!handler.capabilities().adaptive_resampling);
        assert!(!handler.capabilities().input);
    }

    /// What the host does on its own reaches the clients through the audio
    /// state: the latency held in place of the target, an output that is not
    /// playing, and the generation that sends the snapshot out again.
    #[test]
    fn what_the_output_did_is_in_the_audio_state() {
        fn audio_state(handler: &dyn HostControlHandler) -> serde_json::Value {
            handler
                .extend_snapshot()
                .into_iter()
                .find_map(|packet| match packet {
                    rosc::OscPacket::Message(m)
                        if m.addr == runtime_control::osc_contract::STATE_AUDIO =>
                    {
                        match m.args.first() {
                            Some(rosc::OscType::String(json)) => serde_json::from_str(json).ok(),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .expect("the audio state")
        }
        let controls = Controls::new(&config(), fixture_control());
        let handler = controls.handler();
        let before = handler.state_generation();
        assert!(audio_state(handler.as_ref())["latencyTargetHeldMs"].is_null());

        controls.held_target(Some(98.5));
        assert_eq!(audio_state(handler.as_ref())["latencyTargetHeldMs"], 98.5);
        assert!(handler.state_generation() > before);
        controls.held_target(None);
        assert!(audio_state(handler.as_ref())["latencyTargetHeldMs"].is_null());

        controls.failed("dac is not playing".into());
        let state = audio_state(handler.as_ref());
        assert_eq!(state["error"], "dac is not playing");
        assert!(state["outputDeviceEffective"].is_null());
        controls.opened(&controls.settings());
        let state = audio_state(handler.as_ref());
        assert!(state["error"].is_null());
        assert_eq!(state["outputDeviceEffective"], "dac");
    }

    /// Save writes this host's settings and leaves the rest of the file's
    /// audio section alone: the legacy tuning and the live input are kept as
    /// they were read.
    #[test]
    fn save_keeps_what_this_host_does_not_own() {
        let controls = Controls::new(&config(), fixture_control());
        let handler = controls.handler();
        handler.apply_options(&[("realign_threshold_ms", RawOptionValue::Number(35.0))]);
        let mut render: renderer::config::RenderConfig = serde_yaml_ng::from_str(
            "adaptive_resampling_kp_near: 3.5\nlive_input:\n  node: my-sink\n  clock_mode: dac\n",
        )
        .unwrap();
        handler.amend_saved_config(&mut render);
        assert_eq!(render.latency_target, Some(180));
        assert_eq!(render.output_device.as_deref(), Some("dac"));
        assert_eq!(render.realign_threshold_ms, Some(35.0));
        assert_eq!(render.loop_bandwidth_hz, None);
        assert_eq!(render.adaptive_resampling_kp_near, Some(3.5));
        let live = render.live_input.expect("the live input is kept");
        assert_eq!(live.node.as_deref(), Some("my-sink"));
        assert!(live.clock_mode.is_some());
    }
}
