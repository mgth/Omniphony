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

use super::{DEFAULT_TARGET_LATENCY_MS, HostConfig, InputRequest, OutputKind, Source};
use audio_output::FileSinkFormat;

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
    /// Rate the device is opened at (Hz); `None` is the stream's own.
    pub output_rate_hz: Option<u32>,
    /// Where the rendered audio goes: the device, or a file.
    pub output: OutputKind,
}

/// Where the rendered audio goes, from what a client asked for and what the
/// run was launched on. As on the legacy host: a backend named replaces the
/// run's (`device` is this platform's, an unknown one changes nothing), and
/// a file or a format named apply while the output is a file. A file output
/// with no file named anywhere has an empty destination: nothing is written
/// until one is (standard output is `-`, and has to be asked for).
fn resolve_output(
    launch: &OutputKind,
    named_file: Option<&str>,
    backend: Option<&str>,
    file: Option<String>,
    format: Option<&str>,
) -> OutputKind {
    let to_file = match backend.map(|b| b.trim().to_ascii_lowercase()).as_deref() {
        Some("file") => true,
        Some("device" | "pipewire") => false,
        _ => matches!(launch, OutputKind::File { .. }),
    };
    if !to_file {
        return OutputKind::Device;
    }
    let (launch_file, launch_format) = match launch {
        OutputKind::File {
            destination,
            format,
        } => (Some(destination.as_str()), *format),
        OutputKind::Device => (named_file, FileSinkFormat::RawF32),
    };
    OutputKind::File {
        destination: file
            .or_else(|| launch_file.map(str::to_string))
            .unwrap_or_default(),
        format: match format.map(|f| f.trim().to_ascii_lowercase()).as_deref() {
            Some("caf") => FileSinkFormat::Caf,
            Some("raw_f32" | "rawf32" | "raw" | "f32") => FileSinkFormat::RawF32,
            _ => launch_format,
        },
    }
}

/// See the [module docs](self).
pub struct Controls {
    audio: Arc<AudioControl>,
    input: Arc<InputControl>,
    host: Arc<HostAudio>,
    /// The output the run was launched on, and the file its config names.
    launch_output: OutputKind,
    named_file: Option<String>,
}

fn output_devices() -> Vec<OutputDeviceOption> {
    audio_output::sync_output::list_output_devices()
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
        // The input starts from what the config states (which a Save writes
        // back), and what runs is what the run resolved it to.
        let input = Arc::new(InputControl::default());
        let request = &config.input_request;
        input.set_requested_mode(request.mode);
        input.set_requested_node_name(request.node.clone());
        input.set_requested_node_description(request.description.clone());
        let host = Arc::new(HostAudio::for_sync_engine(
            renderer,
            Arc::clone(&audio),
            Arc::clone(&input),
        ));
        // Launch values come from config.yaml, which is edited by hand:
        // bound them as an OSC write would be.
        host.bound_options_to_their_kinds();
        let controls = Self {
            audio,
            input,
            host,
            launch_output: config.output.clone(),
            named_file: config.output_file.clone(),
        };
        controls.input_applied(&config.source);
        controls
    }

    /// Whether a client asked for the input it staged to be applied, since
    /// the last call.
    pub fn take_input_apply(&self) -> bool {
        self.input.take_apply_pending()
    }

    /// The input as it is asked for now.
    pub fn input_request(&self) -> InputRequest {
        let requested = self.input.requested_snapshot();
        InputRequest {
            mode: requested.mode,
            node: requested.node_name,
            description: requested.node_description,
        }
    }

    /// The capture of `source` is the one running: publish it.
    pub fn input_applied(&self, source: &Source) {
        match source {
            Source::OwnSink {
                node_name,
                description,
                ..
            } => self.input.set_input_state(
                InputMode::Pipewire,
                Some(InputBackend::Pipewire),
                None,
                None,
                Some(node_name.clone()),
                Some(description.clone()),
                None,
            ),
            Source::Pipe { .. } | Source::File { .. } => self.input.set_input_state(
                InputMode::Bridge,
                None,
                None,
                None,
                None,
                None,
                Some("bridge-decoded".to_string()),
            ),
        }
        self.input.set_input_error(None);
    }

    /// The input asked for could not be had: publish why, for the client to
    /// show beside what still runs.
    pub fn input_failed(&self, error: String) {
        self.input.set_input_error(Some(error));
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
            output_rate_hz: requested.output_sample_rate_hz,
            output: resolve_output(
                &self.launch_output,
                self.named_file.as_deref(),
                requested.output_backend.as_deref(),
                requested.output_file,
                requested.output_file_format.as_deref(),
            ),
        }
    }

    /// The file the output is asked to be is being written: nothing to
    /// report as wrong, and no device plays.
    pub fn writing(&self, rate: u32) {
        self.audio.set_audio_state(rate, "f32");
        self.audio.set_effective_output_device(None);
        self.audio.set_audio_error(None);
        self.host.mark_output_changed();
    }

    /// The output opened with `settings`, at `device_rate` Hz, on `host`
    /// where the platform has several: publish what plays.
    pub fn opened(&self, settings: &OutputSettings, device_rate: u32, host: Option<&'static str>) {
        self.audio.set_audio_state(device_rate, "f32");
        self.audio.set_effective_output_host(host);
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
            input_request: InputRequest {
                mode: InputMode::Bridge,
                node: Some("my-sink".into()),
                description: None,
            },
            input_path: Some("/in".into()),
            drain_pipe: true,
            output_file: None,
            vbap_table: None,
            log_object_positions: false,
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
                output_rate_hz: None,
                output: OutputKind::Device,
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
                "output_backend",
                "output_file",
                "output_file_format",
                "output_sample_rate",
                "latency_target",
                "loop_bandwidth_hz",
                "source_clock",
                "realign_threshold_ms",
                "input_mode",
                "live_input_node",
                "live_input_description"
            ]
        );
        let refused = handler.apply_options(&[
            ("adaptive_resampling_kp_near", RawOptionValue::Number(2.0)),
            ("live_input_clock_mode", RawOptionValue::Str("dac")),
            ("live_input_lfe_mode", RawOptionValue::Str("drop")),
        ]);
        assert!(refused.results.iter().all(Option::is_none));
        assert!(!handler.capabilities().adaptive_resampling);
        assert!(handler.capabilities().input);
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
        controls.opened(&controls.settings(), 44_100, Some("ASIO"));
        let state = audio_state(handler.as_ref());
        assert!(state["error"].is_null());
        assert_eq!(state["outputDeviceEffective"], "dac");
        assert_eq!(state["outputHost"], "ASIO");
    }

    /// The output goes where a client says: a backend named replaces the
    /// run's, a file and a format apply while the output is a file, and a
    /// file output with no file named writes nowhere until one is.
    #[test]
    fn the_output_follows_the_backend_and_the_file_asked_for() {
        let file = |destination: &str, format| OutputKind::File {
            destination: destination.into(),
            format,
        };
        let raw = FileSinkFormat::RawF32;
        let device = OutputKind::Device;
        // Launched on the device.
        assert_eq!(resolve_output(&device, None, None, None, None), device);
        assert_eq!(
            resolve_output(&device, None, None, Some("a.f32".into()), Some("caf")),
            device,
            "a file named while the device plays waits for the backend"
        );
        assert_eq!(
            resolve_output(&device, None, Some("file"), None, None),
            file("", raw)
        );
        assert_eq!(
            resolve_output(&device, Some("cfg.caf"), Some(" File "), None, Some("CAF")),
            file("cfg.caf", FileSinkFormat::Caf)
        );
        assert_eq!(
            resolve_output(
                &device,
                Some("cfg.caf"),
                Some("file"),
                Some("-".into()),
                None
            ),
            file("-", raw)
        );
        assert_eq!(
            resolve_output(&device, None, Some("asio"), None, None),
            device
        );
        // Launched on a file.
        let launched = file("out.f32", raw);
        assert_eq!(resolve_output(&launched, None, None, None, None), launched);
        assert_eq!(
            resolve_output(&launched, None, None, Some("b.caf".into()), Some("caf")),
            file("b.caf", FileSinkFormat::Caf)
        );
        assert_eq!(
            resolve_output(&launched, None, Some("device"), None, None),
            device
        );
        assert_eq!(
            resolve_output(&launched, None, Some("pipewire"), None, None),
            device
        );
        assert_eq!(
            resolve_output(&launched, None, Some("nonsense"), None, None),
            launched
        );

        // Through the controls: a write is the next setting.
        let controls = Controls::new(&config(), fixture_control());
        let handler = controls.handler();
        handler.apply_options(&[
            ("output_backend", RawOptionValue::Str("file")),
            ("output_file", RawOptionValue::Str("/tmp/x.caf")),
            ("output_file_format", RawOptionValue::Str("caf")),
        ]);
        assert_eq!(
            controls.settings().output,
            file("/tmp/x.caf", FileSinkFormat::Caf)
        );
        handler.apply_options(&[("output_backend", RawOptionValue::Str("device"))]);
        assert_eq!(controls.settings().output, device);
    }

    /// Save writes this host's settings and leaves the rest of the file's
    /// audio section alone: the legacy tuning, and what of the live input
    /// this host has no row for, are kept as they were read.
    #[test]
    fn save_keeps_what_this_host_does_not_own() {
        let controls = Controls::new(&config(), fixture_control());
        let handler = controls.handler();
        handler.apply_options(&[
            ("realign_threshold_ms", RawOptionValue::Number(35.0)),
            ("input_mode", RawOptionValue::Str("pipewire")),
        ]);
        let mut render: renderer::config::RenderConfig = serde_yaml_ng::from_str(
            "adaptive_resampling_kp_near: 3.5\nlive_input:\n  node: my-sink\n  clock_mode: dac\n  lfe_mode: drop\n",
        )
        .unwrap();
        handler.amend_saved_config(&mut render);
        assert_eq!(render.latency_target, Some(180));
        assert_eq!(render.output_device.as_deref(), Some("dac"));
        assert_eq!(render.realign_threshold_ms, Some(35.0));
        assert_eq!(render.loop_bandwidth_hz, None);
        assert_eq!(render.adaptive_resampling_kp_near, Some(3.5));
        assert_eq!(
            render.input_mode,
            Some(renderer::config::InputModeConfig::Pipewire)
        );
        let live = render.live_input.expect("the live input is kept");
        assert_eq!(live.node.as_deref(), Some("my-sink"));
        assert_eq!(live.description, None);
        assert!(live.clock_mode.is_some());
        assert_eq!(
            live.lfe_mode,
            Some(renderer::config::InputLfeModeConfig::Drop)
        );
    }

    /// The input is staged: writes change what is asked for, and nothing is
    /// to be done until the group is applied, once. What runs is published
    /// beside what is asked for, with why a request could not be had.
    #[test]
    fn a_staged_input_is_taken_on_apply() {
        fn input_state(handler: &dyn HostControlHandler) -> serde_json::Value {
            handler
                .extend_snapshot()
                .into_iter()
                .find_map(|packet| match packet {
                    rosc::OscPacket::Message(m)
                        if m.addr == runtime_control::osc_contract::STATE_INPUT =>
                    {
                        match m.args.first() {
                            Some(rosc::OscType::String(json)) => serde_json::from_str(json).ok(),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .expect("the input state")
        }
        let controls = Controls::new(&config(), fixture_control());
        let handler = controls.handler();
        // Launched on a pipe, with the config naming a sink for later.
        assert_eq!(
            controls.input_request(),
            config().input_request,
            "the controls start from the config"
        );
        let state = input_state(handler.as_ref());
        assert_eq!(state["mode"], "pipe_bridge");
        assert_eq!(state["activeMode"], "pipe_bridge");

        handler.apply_options(&[
            ("input_mode", RawOptionValue::Str("pipewire")),
            ("live_input_node", RawOptionValue::Str("den")),
            ("live_input_description", RawOptionValue::Str("The den")),
        ]);
        assert!(!controls.take_input_apply(), "staged, not applied");
        assert_eq!(input_state(handler.as_ref())["activeMode"], "pipe_bridge");

        handler.apply_option_group("live_input").expect("a group");
        assert!(controls.take_input_apply());
        assert!(!controls.take_input_apply(), "taken once");
        let request = controls.input_request();
        assert_eq!(request.mode, InputMode::Pipewire);
        assert_eq!(request.node.as_deref(), Some("den"));

        // The host opens it and says so; or says why it could not.
        let sink = request.source(None, true, None).unwrap();
        controls.input_applied(&sink);
        let state = input_state(handler.as_ref());
        assert_eq!(state["activeMode"], "pipewire");
        assert_eq!(state["applied"]["node"], "den");
        assert_eq!(state["applied"]["description"], "The den");
        assert!(state["applied"]["error"].is_null());
        controls.input_failed("no such pipe".into());
        assert_eq!(
            input_state(handler.as_ref())["applied"]["error"],
            "no such pipe"
        );
    }
}
