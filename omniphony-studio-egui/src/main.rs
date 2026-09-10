//! Spike: native egui/wgpu host for Omniphony Studio.
//!
//! Phase 0 of the frontend replacement study (see README.md). This binary
//! exists to answer measurable questions — frame rate under OSC load, idle
//! CPU, resident memory, CJK text, IME — not to be a product. It listens to
//! the same OSC addresses as the Tauri Studio, draws objects and speakers in a
//! wgpu viewport hosted by an egui paint callback, and floats fixed-extent
//! panels over the viewport so panel expansion can never resize the scene.

mod app;
mod model;
mod osc;
mod render;
mod stats;
mod view;
mod widgets;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "omniphony-studio-egui",
    about = "Native egui/wgpu spike for Omniphony Studio"
)]
pub struct Args {
    /// UDP port to listen on for OSC (0 = OS-assigned, printed at startup).
    #[arg(long, default_value_t = 0)]
    pub listen_port: u16,

    /// Register with a live renderer at host:port (e.g. 127.0.0.1:9000) and
    /// keep the heartbeat alive. Read-only: the spike never sends control
    /// changes, so it cannot disturb a live session.
    #[arg(long)]
    pub register: Option<String>,

    /// Number of synthetic moving objects fed over UDP loopback (0 = off).
    /// They travel through the real socket and parser, not a shortcut.
    #[arg(long, default_value_t = 0)]
    pub synthetic: u32,

    /// Synthetic feed rate in Hz.
    #[arg(long, default_value_t = 100.0)]
    pub rate: f32,

    /// Stop the synthetic feed after N seconds (0 = never). Used for the
    /// idle-CPU gate: the window must go quiet once updates stop.
    #[arg(long, default_value_t = 0.0)]
    pub synthetic_stop_after: f32,

    /// Directory of Studio layout files (`layouts/*.yaml`), loaded with the
    /// host's layout loader. A live renderer replaces the selection with its
    /// own `/state/layout`.
    #[arg(long, default_value = "../layouts")]
    pub layouts_dir: PathBuf,

    /// Layout key to show before a renderer sends its own (default: 7.1.4).
    #[arg(long)]
    pub layout_key: Option<String>,

    /// CJK-capable font file appended as a fallback face. Without it egui's
    /// bundled fonts render CJK as boxes. Default: probe common system paths.
    #[arg(long)]
    pub cjk_font: Option<PathBuf>,

    /// Print a stats line to stdout every N seconds while frames run (0 = off).
    #[arg(long, default_value_t = 0.0)]
    pub stats_interval: f32,

    /// Present without vsync, so the frame rate measures rendering headroom
    /// instead of the monitor's refresh rate.
    #[arg(long, default_value_t = false)]
    pub no_vsync: bool,

    /// Listener head model (glTF binary). Missing file → placeholder sphere.
    #[arg(
        long,
        default_value = "../omniphony-studio/assets/la_dame_de_brassempouy_centered.glb"
    )]
    pub head_model: PathBuf,

    /// Start with trails disabled (measurements).
    #[arg(long, default_value_t = false)]
    pub no_trails: bool,

    /// Start with the object energy field volume enabled (for tests and
    /// measurements; it is off by default like in the Studio).
    #[arg(long, default_value_t = false)]
    pub object_field: bool,
}

/// Probed in order when `--cjk-font` is not given.
const CJK_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJKjp-Regular.otf",
    "/usr/share/fonts/droid/DroidSansFallbackFull.ttf",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "C:\\Windows\\Fonts\\msgothic.ttc",
];

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default();
    if args.no_vsync {
        wgpu_options.surface.present_mode = wgpu::PresentMode::AutoNoVsync;
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_title("Omniphony Studio — egui spike"),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        ..Default::default()
    };

    eframe::run_native(
        "omniphony-studio-egui",
        options,
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx, args.cjk_font.as_deref());
            Ok(Box::new(app::StudioSpike::new(cc, args)?))
        }),
    )
}

/// Append one CJK-capable face to both font families. egui has no system
/// font fallback in 0.36 (it landed on `main` in September 2026), so the spike
/// bundles nothing and borrows a system font instead.
fn install_fonts(ctx: &egui::Context, explicit: Option<&Path>) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates: Vec<PathBuf> = match explicit {
        Some(p) => vec![p.to_path_buf()],
        None => CJK_FONT_CANDIDATES.iter().map(PathBuf::from).collect(),
    };
    let mut installed = None;
    for path in candidates {
        if let Ok(bytes) = std::fs::read(&path) {
            fonts.font_data.insert(
                "cjk-fallback".to_owned(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("cjk-fallback".to_owned());
            }
            installed = Some(path);
            break;
        }
    }
    match &installed {
        Some(p) => log::info!("[fonts] CJK fallback face: {}", p.display()),
        None => log::warn!("[fonts] no CJK font found; CJK labels will render as boxes"),
    }
    ctx.set_fonts(fonts);
}
