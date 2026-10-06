//! The "no bridge" runtime: a renderer with **no decoder**, kept up when the
//! decoder bridge can't be resolved or loaded so Studio can still register and
//! show *why* (a red banner from the live state's bridge error).
//!
//! Both hosts bring it up through [`NoBridgeRuntime`], so they publish the same
//! state; only the lifecycle around it is theirs:
//!
//! - the embedded host (liborender in mpv) returns NULL from `orender_create`,
//!   so mpv falls back to its own native decoder (audio keeps working), and
//!   keeps one of these alive process-globally until a real engine starts or
//!   the host exits. Nothing ever calls into it; it only serves OSC state.
//! - the CLI (`orender render`) takes the parts into its idle loop, with its
//!   host-audio controls attached, and waits for a reload with a working
//!   bridge, a quit, or a standby request.

use crate::channel_objects::ChannelObjectStages;
use crate::engine::OscOptions;
use crate::osc::OscSender;
use crate::renderer_build::{
    HostStateSeed, SpatialRendererParams, build_spatial_renderer, seed_host_state,
};
use anyhow::{Result, anyhow};
use renderer::config::RenderConfig;
use renderer::live_params::RendererControl;
use renderer::spatial_renderer::SpatialRenderer;
use renderer::speaker_layout::SpeakerLayout;
use runtime_control::HostControlHandler;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

/// What a host without a bridge builds its renderer against, in place of the
/// bridge's own declarations. The renderer never decodes or renders through
/// it; the grid only has to be valid for `build_spatial_renderer` to succeed
/// and the coordinate format for the OSC state to be coherent.
pub const NO_BRIDGE_VBAP_DEFAULTS: bridge_api::RVbapCartesianDefaults =
    bridge_api::RVbapCartesianDefaults::BALANCED;
/// See [`NO_BRIDGE_VBAP_DEFAULTS`].
pub const NO_BRIDGE_PREFERRED_MODE: bridge_api::RVbapTableMode =
    bridge_api::RVbapTableMode::Cartesian;
/// See [`NO_BRIDGE_VBAP_DEFAULTS`].
pub const NO_BRIDGE_COORDINATE_FORMAT: bridge_api::RCoordinateFormat =
    bridge_api::RCoordinateFormat::Cartesian;

/// Everything a host resolved for its normal start that still applies without
/// a decoder. Owned, so a host can build the runtime off its own thread
/// (liborender does: mpv falls back without waiting for the VBAP table).
pub struct NoBridgeSetup {
    /// The config file the host runs on (About, profiles, Save).
    pub config_path: Option<PathBuf>,
    /// The render section the host resolved: the file with any live-handoff
    /// sidecar, plus the host's own overrides.
    pub render_cfg: Option<RenderConfig>,
    /// Renderer construction params, resolved from `render_cfg` by the host
    /// (the CLI folds its flags in).
    pub renderer_params: SpatialRendererParams,
    /// The host's explicit speaker layout file, if it has one.
    pub speaker_layout_path: Option<PathBuf>,
    /// The bridge path the host itself was asked for; the config's comes from
    /// `render_cfg`. Recorded as the live `render.bridge_path`, so a Save from
    /// Studio keeps it instead of erasing it.
    pub requested_bridge_path: Option<PathBuf>,
    pub sample_rate: u32,
    /// The host's monitoring cadence fallback, meter then diag, in Hz.
    pub cadence_defaults_hz: (f32, f32),
    /// Why there is no bridge: the full load error, published shortened
    /// ([`summarize_bridge_error`]).
    pub bridge_error: String,
    /// The FFI shim's C-ABI pair when the host is liborender (`None` for
    /// Rust-linked hosts), mirrored so About stays complete.
    pub host_abi: Option<(u32, u32)>,
}

impl NoBridgeSetup {
    /// The embedded host's setup (liborender inside mpv): the inputs it gave
    /// `Engine::from_paths`, renderer params straight from the config (this
    /// host has no flags) and the embedded monitoring cadence fallback.
    pub fn embedded(
        config_path: Option<PathBuf>,
        render_cfg: Option<RenderConfig>,
        speaker_layout_path: Option<PathBuf>,
        requested_bridge_path: Option<PathBuf>,
        sample_rate: u32,
        bridge_error: String,
        host_abi: Option<(u32, u32)>,
    ) -> Self {
        Self {
            renderer_params: SpatialRendererParams::from_render_config(render_cfg.as_ref()),
            config_path,
            render_cfg,
            speaker_layout_path,
            requested_bridge_path,
            sample_rate,
            cadence_defaults_hz: (
                crate::engine::EMBEDDED_METER_RATE_HZ,
                crate::engine::EMBEDDED_DIAG_RATE_HZ,
            ),
            bridge_error,
            host_abi,
        }
    }
}

/// A decoder-less renderer publishing a host's live state and the bridge
/// error, plus (once [`start_osc`](Self::start_osc) ran) the OSC server that
/// serves it. Dropping it shuts the OSC server down.
pub struct NoBridgeRuntime {
    // Declared first so it drops first: the listener stops before the control
    // it reads from goes away.
    osc: Option<OscSender>,
    renderer: SpatialRenderer,
}

impl NoBridgeRuntime {
    /// Build the renderer and publish the state: the layout the real engine
    /// would use, the channel-object catalogues, the host state
    /// ([`seed_host_state`]) and the bridge error. No OSC yet: a host attaches
    /// its own controls to [`control`](Self::control) first.
    pub fn build(setup: NoBridgeSetup) -> Result<Self> {
        let render_cfg = setup.render_cfg.as_ref();
        let layout = resolve_layout(setup.speaker_layout_path.as_deref(), render_cfg)?;
        let renderer = build_spatial_renderer(
            &setup.renderer_params,
            layout,
            setup.sample_rate,
            NO_BRIDGE_VBAP_DEFAULTS,
            NO_BRIDGE_PREFERRED_MODE,
            render_cfg,
        )?;

        let control = renderer.renderer_control();
        // The generator catalogue, the phantom-extraction schema and the
        // fixed-channel catalogue, as a decoding host publishes them.
        ChannelObjectStages::new().publish_static_state(&control);
        seed_host_state(
            &control,
            &HostStateSeed {
                config_path: setup.config_path.as_deref(),
                render_cfg,
                requested_bridge_path: setup.requested_bridge_path.as_deref(),
                cadence_defaults_hz: setup.cadence_defaults_hz,
            },
        );
        if let Some((major, minor)) = setup.host_abi {
            control.set_host_abi(major, minor);
        }
        control.set_bridge_error(Some(summarize_bridge_error(&setup.bridge_error)));

        Ok(Self {
            osc: None,
            renderer,
        })
    }

    /// The renderer's live control, for the host's own additions before
    /// [`start_osc`](Self::start_osc).
    pub fn control(&self) -> Arc<RendererControl> {
        self.renderer.renderer_control()
    }

    /// Start the OSC server on the renderer's control, with the host's own
    /// control handler if it has one (the CLI's audio output/input).
    /// `request_yield`: whether to ask a local holder of the port to yield — a
    /// host that runs only to show a banner must not evict a healthy standby.
    pub fn start_osc(
        &mut self,
        opts: &OscOptions,
        host_handler: Option<Arc<dyn HostControlHandler>>,
        request_yield: bool,
    ) -> Result<()> {
        let target = SocketAddrV4::from_str(&format!("{}:{}", opts.host, opts.port_out))
            .map_err(|e| anyhow!("invalid OSC target {}:{}: {e}", opts.host, opts.port_out))?;
        let mut sender = OscSender::new(target)?;
        log::info!("OSC output enabled: {}:{}", opts.host, opts.port_out);
        if opts.metering {
            sender.set_default_metering(true);
        }
        sender.attach_renderer_control(self.renderer.renderer_control());
        if let Some(handler) = host_handler {
            sender.attach_host_handler(handler);
        }
        sender.start_listener(opts.port_in, request_yield)?;
        self.osc = Some(sender);
        Ok(())
    }

    /// Hand the renderer and the OSC server over to a host that drives its
    /// own loop around them (the CLI's idle runtime).
    pub fn into_parts(self) -> (SpatialRenderer, Option<OscSender>) {
        (self.renderer, self.osc)
    }
}

/// The layout the real engine would start on: the host's explicit file, else
/// the config's, else the 7.1.4 preset. A file that does not load is logged and
/// skipped rather than fatal: this runtime exists to report the bridge error,
/// and the host's normal start reports the layout error once a bridge loads.
fn resolve_layout(
    explicit: Option<&Path>,
    render_cfg: Option<&RenderConfig>,
) -> Result<SpeakerLayout> {
    if let Some(path) = explicit {
        match SpeakerLayout::from_file(path) {
            Ok(layout) => return Ok(layout),
            Err(e) => log::warn!(
                "speaker layout '{}' did not load ({e:#}); the no-bridge runtime uses the configured layout",
                path.display()
            ),
        }
    }
    match render_cfg.and_then(|c| c.current_layout.clone()) {
        Some(layout) => Ok(layout),
        None => SpeakerLayout::preset("7.1.4"),
    }
}

/// Upper bound of the bridge error text carried in the live state. abi_stable
/// reports a layout mismatch with every type layout it compared — 150 KB for a
/// bridge built against another `bridge_api` — which no UI shows and which
/// alone overflows a UDP datagram. The published text keeps the first line and
/// the verdicts (`Error:` lines with their expected/found values); the full
/// report stays in the log.
const BRIDGE_ERROR_MAX_BYTES: usize = 2048;

/// Shorten a bridge load error to what a UI can show (see
/// [`BRIDGE_ERROR_MAX_BYTES`]); a short error passes through unchanged.
pub fn summarize_bridge_error(text: &str) -> String {
    if text.len() <= BRIDGE_ERROR_MAX_BYTES {
        return text.to_string();
    }
    let mut out = text.lines().next().unwrap_or("").trim_end().to_string();
    truncate_at_char_boundary(&mut out, BRIDGE_ERROR_MAX_BYTES / 2);

    // abi_stable's verdicts: an `Error:` line, then `Expected:` / `Found:`
    // labels each followed by an indented value (possibly several lines),
    // up to a blank line. Flatten each verdict on one line, keep each once.
    let mut verdicts: Vec<String> = Vec::new();
    let mut lines = text.lines().map(str::trim).peekable();
    while let Some(line) = lines.next() {
        if !line.starts_with("Error:") {
            continue;
        }
        let mut verdict = line.to_string();
        while let Some(next) = lines.peek().copied() {
            // A verdict ends at a blank line, the next verdict, or the
            // "N error(s) inside" tally that closes a nested layout.
            if next.is_empty() || next.starts_with("Error:") || next.contains("error(s)") {
                break;
            }
            lines.next();
            verdict.push(' ');
            match next.strip_suffix(':') {
                Some(label) => verdict.push_str(&label.to_lowercase()),
                None => verdict.push_str(next),
            }
        }
        if !verdicts.contains(&verdict) {
            verdicts.push(verdict);
        }
    }
    let trailer = format!(
        "\n(full report: {} bytes, see the renderer log)",
        text.len()
    );
    for verdict in verdicts {
        if out.len() + 1 + verdict.len() + trailer.len() > BRIDGE_ERROR_MAX_BYTES {
            break;
        }
        out.push('\n');
        out.push_str(&verdict);
    }
    out.push_str(&trailer);
    out
}

fn truncate_at_char_boundary(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config with a bridge that is not there, a small evaluation grid (the
    /// renderer builds in a debug test run in no time) and one seeded field.
    const CONFIG: &str = "render:
  bridge_path: /nonexistent/libconfig_bridge.so
  ramp_mode: sample
  evaluation_cartesian_x_size: 9
  evaluation_cartesian_y_size: 9
  evaluation_cartesian_z_size: 5
";

    fn temp_config(tag: &str) -> (PathBuf, RenderConfig) {
        let dir =
            std::env::temp_dir().join(format!("orender-no-bridge-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.yaml");
        std::fs::write(&path, CONFIG).expect("write config");
        let render_cfg = renderer::config::Config::load(&path)
            .expect("parse config")
            .render
            .expect("render section");
        (path, render_cfg)
    }

    /// The embedded host's no-bridge runtime publishes what the engine would
    /// have: the bridge error, the bridge path it was asked for (so a Save
    /// from Studio keeps it), the config's path, status and seed, its cadence
    /// fallback and its ABI. The reporter this replaces published only the
    /// error, the ABI and the config path/status: a Save from it wrote an
    /// empty `render.bridge_path` and unseeded runtime state into the config.
    #[test]
    fn the_embedded_setup_publishes_the_engine_state_and_the_bridge_error() {
        let (path, render_cfg) = temp_config("embedded");
        let runtime = NoBridgeRuntime::build(NoBridgeSetup::embedded(
            Some(path.clone()),
            Some(render_cfg),
            None,
            Some(PathBuf::from("/nonexistent/libhost_bridge.so")),
            44_100,
            "bridge path '/nonexistent/libhost_bridge.so' does not exist".to_string(),
            Some((0, 7)),
        ))
        .expect("no-bridge runtime");
        let control = runtime.control();

        assert_eq!(
            control.bridge_error().as_deref(),
            Some("bridge path '/nonexistent/libhost_bridge.so' does not exist")
        );
        assert_eq!(
            control.bridge_path(),
            Some(PathBuf::from("/nonexistent/libhost_bridge.so"))
        );
        assert_eq!(control.config_path(), Some(path));
        assert_eq!(control.config_status().as_deref(), Some("loaded"));
        assert_eq!(control.host_abi(), Some((0, 7)));
        assert_eq!(
            control.live.read().options.ramp_mode,
            renderer::live_params::RampMode::Sample
        );
        assert_eq!(
            control.meter_rate_hz(),
            crate::engine::EMBEDDED_METER_RATE_HZ
        );
        assert!(!control.object_generator_listings().is_empty());
        assert!(runtime.into_parts().1.is_none(), "no OSC before start_osc");
    }

    /// Without a host override the config's bridge path is the one recorded,
    /// and a layout file that does not load does not keep the runtime down.
    #[test]
    fn the_config_bridge_path_and_layout_stand_in_for_the_host_s() {
        let (path, mut render_cfg) = temp_config("config-bridge");
        let layout = SpeakerLayout::preset("5.1").expect("preset layout");
        let speakers = layout.num_speakers();
        render_cfg.current_layout = Some(layout);
        let runtime = NoBridgeRuntime::build(NoBridgeSetup::embedded(
            Some(path),
            Some(render_cfg),
            Some(PathBuf::from("/nonexistent/layout.yaml")),
            None,
            48_000,
            "no bridge".to_string(),
            None,
        ))
        .expect("no-bridge runtime");
        let control = runtime.control();
        assert_eq!(
            control.bridge_path(),
            Some(PathBuf::from("/nonexistent/libconfig_bridge.so"))
        );
        assert_eq!(control.editable_layout().num_speakers(), speakers);
        assert_eq!(control.host_abi(), None);
    }

    #[test]
    fn a_short_bridge_error_is_published_verbatim() {
        let text = "Failed to load bridge plugin from /x/libfoo.so: file not found";
        assert_eq!(summarize_bridge_error(text), text);
    }

    #[test]
    fn a_layout_mismatch_report_keeps_its_first_line_and_distinct_verdicts() {
        let mut text = String::from("Failed to load bridge plugin from /x/libfoo.so: \n");
        let layout_noise = "Compared <this>:\n    --- Type Layout ---\n    type:PrefixRef<'a, BridgeLib>\n    size:8 align:8\n    data:\n        Struct with Fields:\n\n";
        for _ in 0..200 {
            text.push_str(layout_noise);
            text.push_str(
                "Error:incompatible package versions\nExpected:\n    0.3.0\nFound:\n    0.4.0\n\n",
            );
        }
        text.push_str(
            "Error:unexpected variant\nExpected:\n    \"Unknown\"\nFound:\n    \"Lh\"\n1 error(s)inside:\n\n",
        );
        assert!(text.len() > 20 * BRIDGE_ERROR_MAX_BYTES);

        let summary = summarize_bridge_error(&text);
        assert!(summary.len() <= BRIDGE_ERROR_MAX_BYTES, "{}", summary.len());
        assert!(summary.starts_with("Failed to load bridge plugin from /x/libfoo.so:"));
        assert_eq!(
            summary
                .matches("Error:incompatible package versions expected 0.3.0 found 0.4.0")
                .count(),
            1
        );
        assert!(summary.contains("Error:unexpected variant expected \"Unknown\" found \"Lh\""));
        assert!(!summary.contains("error(s)"));
        assert!(summary.contains(&format!("full report: {} bytes", text.len())));
        assert!(!summary.contains("Type Layout"));
    }

    #[test]
    fn a_long_error_without_verdicts_is_still_bounded() {
        let text = format!(
            "Failed to load bridge plugin from /x/libfoo.so: {}",
            "y".repeat(10_000)
        );
        let summary = summarize_bridge_error(&text);
        assert!(summary.len() <= BRIDGE_ERROR_MAX_BYTES);
        assert!(summary.starts_with("Failed to load bridge plugin"));
        assert!(summary.ends_with("see the renderer log)"));
    }
}
