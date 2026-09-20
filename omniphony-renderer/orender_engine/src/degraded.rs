//! Degraded "no decoder" reporter.
//!
//! When the decoder bridge can't be resolved or loaded, the embedded (mpv) host
//! returns NULL from `orender_create` so mpv falls back to its own native
//! decoder (audio keeps working). To still tell Studio *why* spatial rendering
//! didn't engage, we bring up a minimal renderer + OSC server with **no
//! decoder**, whose live-state snapshot carries the bridge error. Studio
//! registers the normal way (so its address is known — no guessing) and shows a
//! red banner.
//!
//! `orender_ffi` keeps one of these alive process-globally for the lifetime of
//! the host. Nothing ever calls into it; it exists only to serve OSC state.

use crate::engine::OscOptions;
use crate::osc::OscSender;
use crate::renderer_build::{SpatialRendererParams, build_spatial_renderer};
use anyhow::Result;
use renderer::config::Config;
use renderer::spatial_renderer::SpatialRenderer;
use renderer::speaker_layout::SpeakerLayout;
use std::net::SocketAddrV4;
use std::path::Path;
use std::str::FromStr;

// VBAP grid for the decoder-less reporter (mirrors the CLI's idle-bridge
// constants in `cli/decode/session_run.rs`). We never decode or render through
// it; the grid only has to be valid for `build_spatial_renderer` to succeed.
const REPORTER_VBAP_DEFAULTS: bridge_api::RVbapCartesianDefaults =
    bridge_api::RVbapCartesianDefaults {
        x_size: 62,
        y_size: 62,
        z_size: 15,
        allow_negative_z: false,
    };
const REPORTER_PREFERRED_MODE: bridge_api::RVbapTableMode = bridge_api::RVbapTableMode::Cartesian;

/// A decoder-less renderer + OSC server, kept alive only to report the bridge
/// error to Studio. Dropping it shuts the OSC server down.
pub struct DegradedReporter {
    // Kept alive; never called into. `_osc` owns the listener thread that
    // answers Studio's registration with the live-state bundle (which carries
    // the bridge error). `_renderer` owns the `RendererControl` that bundle
    // reads from.
    _osc: OscSender,
    _renderer: SpatialRenderer,
}

/// Build and start the degraded reporter. `config_yaml_path` seeds the layout /
/// room / OSC settings exactly as the real engine would, so Studio sees a
/// coherent (if non-decoding) state alongside `bridge_error`. `host_abi` is the
/// FFI shim's C-ABI pair when the host is liborender (`None` for Rust-linked
/// hosts), mirrored so About stays complete in the degraded state. The returned
/// value must be kept alive to keep the OSC server up.
pub fn start_degraded_reporter(
    config_yaml_path: Option<&Path>,
    sample_rate: u32,
    osc: OscOptions,
    bridge_error: String,
    host_abi: Option<(u32, u32)>,
) -> Result<DegradedReporter> {
    let render_cfg = config_yaml_path
        .map(Config::load_or_default)
        .and_then(|c| c.render);

    let layout = match render_cfg.as_ref().and_then(|c| c.current_layout.clone()) {
        Some(layout) => layout,
        None => SpeakerLayout::preset("7.1.4")?,
    };

    let params = SpatialRendererParams::from_render_config(render_cfg.as_ref());
    let renderer = build_spatial_renderer(
        &params,
        layout,
        sample_rate,
        REPORTER_VBAP_DEFAULTS,
        REPORTER_PREFERRED_MODE,
        render_cfg.as_ref(),
    )?;

    let control = renderer.renderer_control();
    control.set_bridge_error(Some(summarize_bridge_error(&bridge_error)));
    if let Some((major, minor)) = host_abi {
        control.set_host_abi(major, minor);
    }
    // Mirror the real engine so About's config diagnostics stay meaningful.
    if let Some(path) = config_yaml_path {
        control.set_config_path(path.to_path_buf());
        control.set_config_status(Some(Config::load_status(path).as_str().to_string()));
    }

    let target = SocketAddrV4::from_str(&format!("{}:{}", osc.host, osc.port_out))?;
    let mut sender = OscSender::new(target)?;
    sender.attach_renderer_control(control);
    // Never request a yield here: the degraded reporter is only a banner and
    // must not evict a healthy standby renderer holding the port.
    sender.start_listener(osc.port_in, false)?;

    Ok(DegradedReporter {
        _osc: sender,
        _renderer: renderer,
    })
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
