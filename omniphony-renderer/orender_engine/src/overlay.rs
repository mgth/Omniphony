//! In-process spatial overlay generator for the mpv host.
//!
//! Historically the front-view object overlay was produced outside the
//! renderer: orender broadcast OSC → omniphony-studio rebuilt a compact CSV →
//! pushed it to mpv over the JSON IPC socket → a ~530-line Lua script parsed it
//! and built the ASS markup. orender already owns the positions and meter
//! levels first-hand, so this module moves the whole rendering into Rust: it
//! holds the latest scene + per-object motion trails and emits the finished ASS
//! `osd-overlay` payload. The only residual host code is a tiny Lua shim that
//! `ffi.load`s this library, pulls the ASS string via the FFI getter, and calls
//! `osd-overlay` (the mpv decoder thread cannot issue that command itself).
//!
//! State is a process-global singleton: a single spatial stream plays at a time
//! in the mpv process, and the FFI getter has no session handle. The overlay is
//! **self-gating** — it only does per-frame work once a consumer (the Lua shim)
//! has pulled recently, so the CLI host (which never pulls) pays nothing.
//!
//! Rendering mirrors the former `omniphony-overlay.lua` pixel-for-pixel: same
//! pseudo-3D projection, wireframe cube, depth lines, circles, trails (line /
//! diffuse), colour palette and constants.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

// ── tunables (mirrors of the former Lua constants) ────────────────────────

const BASE_RADIUS_RATIO: f64 = 0.015; // fraction of screen height
const HEADER_FONT_SIZE: i32 = 14;
// Object-name label height as a fraction of screen height (~65 px at 1080p),
// centred on the object. Scales with resolution so it looks the same on 4K.
const LABEL_FONT_RATIO: f64 = 0.06;
const CINEMA_ASPECT: f64 = 2.35; // pseudo-3D depth squeezes Y=+1 into this band
// Aspect cap for the depth squeeze. Without it, as the frame widens toward
// CINEMA_ASPECT the squeeze (and the perceived depth) shrinks to nothing; capping
// at 16:9 keeps screens wider than 16:9 at the same vertical perspective as 16:9.
const PERSPECTIVE_MAX_ASPECT: f64 = 16.0 / 9.0;

// ── object energy heatmap (mpv overlay = depth slices only, 3 planes) ─────
// Mirror of Studio's client-side `object-energy-heatmap.js`, restricted here to
// the "depth" axis (Omniphony Y, the into-screen direction of the pseudo-3D
// projection) with exactly 3 planes. The planes are flattened into a single
// BGRA bitmap (colour + alpha track the theoretical inverse-square energy) and
// drawn under the ASS layer via mpv's `overlay-add`.
const FIELD_BANDS: usize = 3; // default number of flattened depth planes (1..=12)
const FIELD_FALLOFF_R: f64 = 0.12; // inverse-square cap radius (normalised units)
const FIELD_OPACITY: f64 = 0.5; // global alpha multiplier over the energy alpha
// Internal raster size cap per axis. The flattened bitmap is rendered at the
// front plane's on-screen size, clamped to this; the field is smooth so mpv
// upscales to the display rect with no visible loss. Bounds CPU on big windows /
// constrained hardware.
const FIELD_BITMAP_MAX: usize = 256;

/// Bezier-approximated unit circle of radius 100, scaled per object via
/// `\fscx/\fscy` so libass keeps the path parse cached.
const UNIT_CIRCLE: &str = "m -100 0 b -100 -55 -55 -100 0 -100 \
b 55 -100 100 -55 100 0 \
b 100 55 55 100 0 100 \
b -55 100 -100 55 -100 0";

const TRAIL_MIN_POINT_INTERVAL_S: f64 = 0.07;
// Safety ceiling only — the live trail length is driven by the TTL (a long TTL
// keeps more points). At the 70 ms sample interval this caps the trail at
// ~18 s, bounding libass load / memory on long durations.
const TRAIL_MAX_POINTS: usize = 256;

// Diffuse-mode rendering parameters (see the former Lua for the rationale).
const DIFFUSE_SPACING_FACTOR: f64 = 0.65;
const DIFFUSE_MAX_SUBDIV: i32 = 16;
const DIFFUSE_BLUR: i32 = 4;
// Particle budget per object. When the trail needs more dots than the budget we
// *thin uniformly* over its whole length (lower density) rather than truncating
// the tail — so there is no hard, motion-dependent cutoff. The budget scales
// with the TTL (longer trail → more dots) up to a ceiling that bounds libass
// blur cost. Each dot is one blurred ASS event; the blur is the costly part.
const DIFFUSE_MIN_DOTS: usize = 128;
const DIFFUSE_MAX_DOTS: usize = 512;
const DIFFUSE_DOTS_PER_S: f64 = 40.0;

const Y_LINE_BORD: i32 = 3;
const Y_TICK_HALF: f64 = 5.0; // half-length of the Y=0 perpendicular tick, px

/// How long after the last FFI pull the overlay keeps doing per-frame work.
const ACTIVE_TIMEOUT_MS: u64 = 1000;

/// Mirror of `OBJECT_COLOR_PALETTE` in omniphony-studio so the overlay shows the
/// same colour Studio's 3D view picks for the same object.
const STUDIO_PALETTE: [&str; 16] = [
    "FF6B6B", "4ECDC4", "FFE66D", "5DADE2", "AF7AC5", "F5B041", "58D68D", "EC7063", "48C9B0",
    "F4D03F", "5499C7", "A569BD", "EB984E", "45B39D", "7FB3D5", "F1948A",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum TrailMode {
    Line,
    Diffuse,
}

struct TrailCfg {
    enabled: bool,
    ttl_s: f64,
    mode: TrailMode,
    /// Squared XYZ distance above which a step is a teleport (segment skipped).
    teleport_sq: f64,
}

impl Default for TrailCfg {
    fn default() -> Self {
        // Match omniphony-studio's UI defaults (trails on, diffuse, 7 s,
        // teleport threshold 0.5 → squared 0.25) so the overlay shows trails
        // out of the box without an OSC controller. Studio can still override
        // these live over OSC when it is connected.
        Self {
            enabled: true,
            ttl_s: 7.0,
            mode: TrailMode::Diffuse,
            teleport_sq: 0.25,
        }
    }
}

#[derive(Clone, Copy)]
struct TrailPoint {
    x: f64,
    y: f64,
    z: f64,
    t: f64,
    brk: bool,
}

#[derive(Default)]
struct Trail {
    points: Vec<TrailPoint>,
    last_t: f64,
}

struct OverlayState {
    /// Latest frame's object positions `(id, x, y, z)`, in render order.
    positions: Vec<(u32, f64, f64, f64)>,
    /// Latest per-object RMS dBFS, keyed by object id. Updated independently of
    /// `positions` (positions arrive per metadata frame; levels per meter poll).
    levels: HashMap<u32, f64>,
    trails: HashMap<u32, Trail>,
    /// A/B colour override per object id (set by Studio over OSC control).
    tags: HashMap<u32, char>,
    /// Formatted display label per object id (mirrors Studio's 3D-view labels).
    labels: HashMap<u32, String>,
    /// Whether object labels are drawn (mirrors Studio's `objectLabelsEnabled`).
    labels_enabled: bool,
    /// Whether the objects themselves (markers + labels + trails + depth lines)
    /// are drawn. A display-only switch (mirrors Studio's "show objects"); it does
    /// not change `labels_enabled` or the trail config, so those come back as set
    /// when objects are shown again. The energy heatmap is unaffected.
    objects_visible: bool,
    /// Whether the energy heatmap is drawn at all (mirrors Studio's "Object
    /// energy field" toggle). Transient — Studio owns the value and re-pushes it
    /// on connect (`syncMpvOverlayPrefs`).
    heatmap_enabled: bool,
    /// Number of flattened depth planes in the energy heatmap (1..=12). Mirrors
    /// Studio's "Planes per axis" slider; defaults to `FIELD_BANDS`.
    heatmap_bands: usize,
    /// Colour gradient index for the energy heatmap, mirroring Studio's
    /// `OBJECT_ENERGY_COLORMAPS`: 0 heatmap, 1 blue→white, 2 white→red,
    /// 3 red (alpha-only), 4 custom. See `heatmap_rgb`.
    heatmap_colormap: u8,
    /// User-defined gradient stops `[pos, r, g, b]` (all in [0,1], sorted by pos)
    /// used when `heatmap_colormap == 4`. Transient — Studio owns it and re-pushes
    /// it on connect (`syncMpvOverlayPrefs`). Empty ⇒ greyscale fallback.
    heatmap_custom_stops: Vec<[f32; 4]>,
    cfg: TrailCfg,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self {
            positions: Vec::new(),
            levels: HashMap::new(),
            trails: HashMap::new(),
            tags: HashMap::new(),
            labels: HashMap::new(),
            labels_enabled: true,   // on by default, like Studio's 3D view
            objects_visible: true,  // on by default
            heatmap_enabled: false, // off by default; Studio pushes its toggle on connect
            heatmap_bands: FIELD_BANDS,
            heatmap_colormap: 1, // blue→white, like Studio's default
            heatmap_custom_stops: Vec::new(),
            cfg: TrailCfg::default(),
        }
    }
}

struct Overlay {
    enabled: AtomicBool,
    /// Whether a live session is actually spatial-rendering. A host can keep a
    /// session alive while *not* rendering through it — mpv in "host" mode keeps
    /// the engine (and this overlay's OSC link) alive but decodes channel audio
    /// natively. While false the overlay draws nothing at all (the wireframe cube
    /// included), so it disappears instead of lingering as an empty box. Default
    /// true so hosts that never touch it (the CLI) are unaffected.
    rendering: AtomicBool,
    /// Number of live orender render sessions (one per `orender_create` that has
    /// not yet been destroyed). The overlay only draws while at least one session
    /// is decoding: without `--ad=orender` no session is ever created, so the Lua
    /// shim — which loads unconditionally — pulls an empty payload and draws
    /// nothing instead of a permanent, scene-less wireframe box.
    sessions: AtomicUsize,
    /// `start.elapsed()` ms at the last FFI pull; self-gates per-frame work.
    last_pull_ms: AtomicU64,
    start: Instant,
    state: Mutex<OverlayState>,
    /// Bumped by every change to the *display* preferences (never by per-frame
    /// scene data). The OSC layer polls it and republishes the state, which is
    /// what keeps Studio's switches honest when mpv flips one by keybind: the
    /// overlay is a process-global singleton with two writers, so neither side
    /// may assume its own mirror is the truth.
    generation: AtomicU64,
    /// Path to the dedicated overlay-prefs file (owned by orender, persisted in
    /// real time, separate from the savable config). `None` until set by the host.
    prefs_path: Mutex<Option<PathBuf>>,
}

fn overlay() -> &'static Overlay {
    static OVERLAY: OnceLock<Overlay> = OnceLock::new();
    OVERLAY.get_or_init(|| Overlay {
        enabled: AtomicBool::new(true),
        rendering: AtomicBool::new(true),
        sessions: AtomicUsize::new(0),
        last_pull_ms: AtomicU64::new(0),
        start: Instant::now(),
        state: Mutex::new(OverlayState::default()),
        generation: AtomicU64::new(0),
        prefs_path: Mutex::new(None),
    })
}

/// True while at least one orender render session is live (see [`Overlay::sessions`]).
fn session_active() -> bool {
    overlay().sessions.load(Ordering::Relaxed) > 0
}

/// Suppress or resume *all* overlay drawing (wireframe cube included) for a live
/// session, independent of the master [`set_enabled`] preference. A host with a
/// live engine that is not spatial-rendering — mpv decoding channel audio
/// natively in "host" mode — sets this `false` so the overlay disappears, then
/// `true` to bring it back. Distinct from [`clear`], which flushes scene data but
/// keeps the cube (e.g. on seek).
pub fn set_rendering(on: bool) {
    overlay().rendering.store(on, Ordering::Relaxed);
}

/// Register the start of an orender render session. Called by the host when a
/// renderer is created (`orender_create`); the overlay stays blank until the
/// first session arms it.
pub fn session_started() {
    overlay().sessions.fetch_add(1, Ordering::Relaxed);
}

/// Register the end of an orender render session (`orender_destroy`). Saturates
/// at zero, and clears the scene + trails when the last session goes away so the
/// overlay can't linger past the stream it belonged to.
pub fn session_ended() {
    let o = overlay();
    let mut cur = o.sessions.load(Ordering::Relaxed);
    loop {
        if cur == 0 {
            return; // unbalanced end; nothing to do
        }
        match o
            .sessions
            .compare_exchange_weak(cur, cur - 1, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => {
                if cur == 1 {
                    clear();
                }
                return;
            }
            Err(observed) => cur = observed,
        }
    }
}

fn now_secs() -> f64 {
    overlay().start.elapsed().as_secs_f64()
}

// ── public surface (engine feed + control) ────────────────────────────────

/// True when the overlay is enabled *and* a consumer pulled within the last
/// second. The engine checks this each frame to decide whether to spend any
/// effort feeding the overlay; the CLI host never pulls, so it stays false.
pub fn is_active() -> bool {
    let o = overlay();
    if !o.enabled.load(Ordering::Relaxed) {
        return false;
    }
    let now = o.start.elapsed().as_millis() as u64;
    now.saturating_sub(o.last_pull_ms.load(Ordering::Relaxed)) <= ACTIVE_TIMEOUT_MS
}

/// Replace the current object positions `(id, x, y, z, name)`. Cheap (stores
/// positions + formatted labels); trails are advanced at draw time, mirroring
/// the former Lua redraw flow. The label is cleaned like Studio's 3D view.
pub fn update_positions(objects: Vec<(u32, f64, f64, f64, String)>) {
    if let Ok(mut s) = overlay().state.lock() {
        s.positions.clear();
        s.labels.clear();
        for (id, x, y, z, name) in objects {
            let label = format_object_label(&name);
            if !label.is_empty() {
                s.labels.insert(id, label);
            }
            s.positions.push((id, x, y, z));
        }
    }
}

/// Update per-object RMS levels `(id, rms_dbfs)`.
pub fn update_levels(levels: &[(u32, f64)]) {
    if let Ok(mut s) = overlay().state.lock() {
        for &(id, rms) in levels {
            s.levels.insert(id, rms);
        }
    }
}

/// Drop all scene + trail state (seek / stream end / session teardown).
pub fn clear() {
    if let Ok(mut s) = overlay().state.lock() {
        s.positions.clear();
        s.levels.clear();
        s.trails.clear();
        s.labels.clear();
    }
}

/// Monotonic counter over the display preferences. The OSC layer compares it
/// per poll and rebroadcasts `/omniphony/state/overlay` when it moves.
pub fn state_generation() -> u64 {
    overlay().generation.load(Ordering::Relaxed)
}

fn bump_generation() {
    overlay().generation.fetch_add(1, Ordering::Relaxed);
}

/// Mutate the display state under the lock, then publish (and optionally
/// persist) the change.
///
/// Every display mutator goes through this rather than locking directly, so a
/// new control cannot be added that changes what is drawn without the change
/// being announced — the omission that let Studio and an mpv keybind drift
/// apart silently.
fn with_display_state<T>(persist: bool, f: impl FnOnce(&mut OverlayState) -> T) -> Option<T> {
    let out = {
        let mut s = overlay().state.lock().ok()?;
        f(&mut s)
    };
    bump_generation();
    if persist {
        save_prefs();
    }
    Some(out)
}

/// Master enable/disable (Studio toggle; also gates `is_active`).
pub fn set_enabled(on: bool) {
    overlay().enabled.store(on, Ordering::Relaxed);
    bump_generation();
    save_prefs();
}

/// Show/hide object labels (mirror of Studio's `objectLabelsEnabled`).
pub fn set_labels_enabled(on: bool) {
    with_display_state(true, |s| s.labels_enabled = on);
}

/// Show/hide the objects (markers + labels + trails + depth lines). Display-only:
/// does not touch `labels_enabled` or the trail config, and leaves the energy
/// heatmap alone. Transient (not persisted) — Studio owns the value and re-pushes
/// it; orender defaults to showing objects.
pub fn set_objects_visible(on: bool) {
    with_display_state(false, |s| s.objects_visible = on);
}

/// Enable/disable drawing the energy heatmap (mirrors Studio's "Object energy
/// field" toggle). Transient — Studio owns the value and re-pushes it on connect.
pub fn set_heatmap_enabled(on: bool) {
    with_display_state(false, |s| s.heatmap_enabled = on);
}

/// Set the number of flattened depth planes in the energy heatmap (clamped to
/// 1..=12, mirroring Studio's "Planes per axis" slider). Transient.
pub fn set_heatmap_bands(count: usize) {
    with_display_state(false, |s| s.heatmap_bands = count.clamp(1, 12));
}

/// Set the energy-heatmap colour gradient (mirrors Studio's gradient selector).
/// Index follows `OBJECT_ENERGY_COLORMAPS`: 0 heatmap, 1 blue→white, 2 white→red,
/// 3 red (alpha-only). Transient — Studio owns the value and re-pushes it.
pub fn set_heatmap_colormap(idx: usize) {
    with_display_state(false, |s| s.heatmap_colormap = idx.min(4) as u8);
}

/// Set the custom-gradient stops `[pos, r, g, b]` (used when the colormap index is
/// 4). Sorted by `pos` so the interpolation in `heatmap_rgb` can assume order.
/// Transient — Studio owns the value and re-pushes it on connect.
pub fn set_heatmap_custom_stops(mut stops: Vec<[f32; 4]>) {
    stops.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
    with_display_state(false, |s| s.heatmap_custom_stops = stops);
}

/// Apply trail configuration (mirror of Studio's wire fields).
pub fn set_trail_config(enabled: bool, ttl_ms: u32, diffuse: bool, teleport_threshold: f64) {
    with_display_state(true, |s| {
        s.cfg.enabled = enabled;
        s.cfg.ttl_s = (ttl_ms as f64) / 1000.0;
        s.cfg.mode = if diffuse {
            TrailMode::Diffuse
        } else {
            TrailMode::Line
        };
        if teleport_threshold > 0.0 {
            s.cfg.teleport_sq = teleport_threshold * teleport_threshold;
        }
        if !s.cfg.enabled {
            s.trails.clear();
        }
    });
}

// ── toggles (host keybinds: flip current state, return the new one) ──────────
//
// These exist so the mpv shim can bind a key to "flip" a control and show the
// resulting state in the OSD, without keeping its own mirror that could drift
// from Studio's OSC pushes. Each flips the *current* value atomically and
// returns the new one; persistence mirrors the matching `set_*` (enable / labels
// / trails persist; objects / heatmap / bands / colormap are transient, owned by
// Studio and re-pushed on connect).

/// Flip the master enable and return the new state. Persisted.
pub fn toggle_enabled() -> bool {
    let o = overlay();
    let new = !o.enabled.load(Ordering::Relaxed);
    o.enabled.store(new, Ordering::Relaxed);
    bump_generation();
    save_prefs();
    new
}

/// Flip object-label visibility and return the new state. Persisted.
pub fn toggle_labels() -> bool {
    with_display_state(true, |s| {
        s.labels_enabled = !s.labels_enabled;
        s.labels_enabled
    })
    .unwrap_or(false)
}

/// Flip object visibility (markers + labels + trails + depth lines) and return
/// the new state. Transient (display-only), like [`set_objects_visible`].
pub fn toggle_objects() -> bool {
    with_display_state(false, |s| {
        s.objects_visible = !s.objects_visible;
        s.objects_visible
    })
    .unwrap_or(false)
}

/// Flip whether the trails are drawn and return the new state. Clears the trail
/// buffers when disabling (so they don't reappear on re-enable). Persisted, like
/// [`set_trail_config`].
pub fn toggle_trails() -> bool {
    with_display_state(true, |s| {
        s.cfg.enabled = !s.cfg.enabled;
        if !s.cfg.enabled {
            s.trails.clear();
        }
        s.cfg.enabled
    })
    .unwrap_or(false)
}

/// Flip the energy heatmap and return the new state. Transient, like
/// [`set_heatmap_enabled`].
pub fn toggle_heatmap() -> bool {
    with_display_state(false, |s| {
        s.heatmap_enabled = !s.heatmap_enabled;
        s.heatmap_enabled
    })
    .unwrap_or(false)
}

/// Advance the heatmap colour gradient to the next index (wraps 0..=4, mirroring
/// `OBJECT_ENERGY_COLORMAPS`) and return the new index. Transient.
pub fn cycle_heatmap_colormap() -> usize {
    with_display_state(false, |s| {
        s.heatmap_colormap = (s.heatmap_colormap + 1) % 5;
        s.heatmap_colormap as usize
    })
    .unwrap_or(0)
}

/// Step the heatmap depth-plane count by `delta` (clamped to 1..=12) and return
/// the new count. Transient, like [`set_heatmap_bands`].
pub fn adjust_heatmap_bands(delta: i32) -> usize {
    with_display_state(false, |s| {
        let next = (s.heatmap_bands as i32 + delta).clamp(1, 12) as usize;
        s.heatmap_bands = next;
        next
    })
    .unwrap_or(FIELD_BANDS)
}

/// The display preferences as JSON, for `/omniphony/state/overlay`.
///
/// Only what a client mirrors in its UI — per-frame scene data (positions,
/// levels, trails, labels) is excluded: it changes every frame and would turn
/// the generation counter into a broadcast storm.
pub fn display_state_json() -> String {
    let o = overlay();
    let enabled = o.enabled.load(Ordering::Relaxed);
    let Ok(s) = o.state.lock() else {
        return "{}".to_string();
    };
    serde_json::json!({
        "enabled": enabled,
        "labelsEnabled": s.labels_enabled,
        "objectsVisible": s.objects_visible,
        "heatmapEnabled": s.heatmap_enabled,
        "heatmapBands": s.heatmap_bands,
        "heatmapColormap": s.heatmap_colormap,
        "trailsEnabled": s.cfg.enabled,
        "trailTtlMs": (s.cfg.ttl_s * 1000.0).round() as u32,
        "trailMode": match s.cfg.mode {
            TrailMode::Diffuse => "diffuse",
            TrailMode::Line => "line",
        },
        "trailTeleportThreshold": s.cfg.teleport_sq.sqrt(),
    })
    .to_string()
}

// ── persistence (orender-owned, real-time, separate from the savable config) ─

/// Point the overlay at its prefs file and load it. Called once by the host at
/// startup. The overlay display params live here now (enable, labels, trails),
/// auto-persisted on every change — independent of `config.yaml` / the save
/// command. Missing or unreadable file → keep the defaults.
pub fn load_prefs(path: &Path) {
    let o = overlay();
    *o.prefs_path.lock().unwrap() = Some(path.to_path_buf());
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    if let Ok(mut s) = o.state.lock() {
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "enabled" => o.enabled.store(v != "0", Ordering::Relaxed),
                "labels" => s.labels_enabled = v != "0",
                "trails_enabled" => s.cfg.enabled = v != "0",
                "ttl_ms" => {
                    if let Ok(ms) = v.parse::<u32>() {
                        s.cfg.ttl_s = ms as f64 / 1000.0;
                    }
                }
                "mode" => {
                    s.cfg.mode = if v.eq_ignore_ascii_case("diffuse") {
                        TrailMode::Diffuse
                    } else {
                        TrailMode::Line
                    }
                }
                "teleport" => {
                    if let Ok(th) = v.parse::<f64>() {
                        if th > 0.0 {
                            s.cfg.teleport_sq = th * th;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // Restored values are a state change like any other: publish them so a
    // client connecting later sees what is actually drawn.
    bump_generation();
}

/// Write the current overlay prefs to the prefs file (best-effort, no-op until
/// `load_prefs` set a path). Runs on the OSC control thread, never the audio path.
fn save_prefs() {
    let o = overlay();
    let Some(path) = o.prefs_path.lock().unwrap().clone() else {
        return;
    };
    let enabled = o.enabled.load(Ordering::Relaxed);
    let Ok(s) = o.state.lock() else { return };
    let mode = match s.cfg.mode {
        TrailMode::Diffuse => "diffuse",
        TrailMode::Line => "line",
    };
    let body = format!(
        "enabled={}\nlabels={}\ntrails_enabled={}\nttl_ms={}\nmode={}\nteleport={:.3}\n",
        enabled as u8,
        s.labels_enabled as u8,
        s.cfg.enabled as u8,
        (s.cfg.ttl_s * 1000.0).round() as u32,
        mode,
        s.cfg.teleport_sq.sqrt(),
    );
    let _ = std::fs::write(&path, body);
}

/// Set or clear an A/B colour tag for an object id.
pub fn set_tag(id: u32, tag: Option<char>) {
    if let Ok(mut s) = overlay().state.lock() {
        match tag {
            Some(c) => {
                s.tags.insert(id, c);
            }
            None => {
                s.tags.remove(&id);
            }
        }
    }
}

/// Build the ASS `osd-overlay` payload for the given OSD resolution. Returns an
/// empty string when disabled or there is nothing to draw. Records the pull so
/// `is_active` stays true. Called from the host (Lua) thread.
pub fn build_ass(res_x: u32, res_y: u32) -> String {
    let o = overlay();
    o.last_pull_ms
        .store(o.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    if !o.enabled.load(Ordering::Relaxed)
        || !o.rendering.load(Ordering::Relaxed)
        || !session_active()
        || res_x == 0
        || res_y == 0
    {
        return String::new();
    }
    let now = now_secs();
    let mut s = match o.state.lock() {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    render(&mut s, res_x as f64, res_y as f64, now)
}

// ── labels (mirror of omniphony-studio's getObjectDisplayName/formatObjectLabel) ─

/// Clean an object name into a display label, matching Studio's 3D-view rules:
/// strip a leading `a_`/`v_`/`obj_` (or `:`/`-` separator) prefix, then keep the
/// part after the first remaining underscore.
fn format_object_label(name: &str) -> String {
    let mut name = name.trim();
    // Strip ^[av][_:-]
    let b = name.as_bytes();
    if b.len() >= 2
        && matches!(b[0], b'a' | b'A' | b'v' | b'V')
        && matches!(b[1], b'_' | b':' | b'-')
    {
        name = &name[2..];
    }
    // Strip ^obj[_:-]
    let b = name.as_bytes();
    if b.len() >= 4 && name[..3].eq_ignore_ascii_case("obj") && matches!(b[3], b'_' | b':' | b'-') {
        name = &name[4..];
    }
    // Keep the part after the first underscore, if any.
    if let Some(idx) = name.find('_') {
        let cleaned = &name[idx + 1..];
        if !cleaned.is_empty() {
            return cleaned.to_string();
        }
    }
    name.to_string()
}

/// Make a string safe to drop into ASS event text: the override-block braces and
/// the escape backslash would otherwise be interpreted by libass.
fn ass_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '{' | '}' | '\\' => ' ',
            _ => c,
        })
        .collect()
}

// ── colours (ported from omniphony-studio osc_listener.rs) ─────────────────

fn parse_hex(hex: &str) -> (u8, u8, u8) {
    let b = hex.as_bytes();
    if b.len() < 6 {
        return (128, 128, 128);
    }
    let h = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(128);
    (h(0), h(2), h(4))
}

fn tag_color_hex(tag: char) -> Option<&'static str> {
    match tag {
        'A' | 'a' => Some("FF8B6B"),
        'B' | 'b' => Some("62D7C7"),
        _ => None,
    }
}

fn object_color(id: u32, tag: Option<char>) -> (u8, u8, u8) {
    if let Some(c) = tag.and_then(tag_color_hex) {
        return parse_hex(c);
    }
    // Object ids are numeric here, so the palette index is just `id % N`
    // (matches Studio's `id.parse::<i64>()` fast path).
    parse_hex(STUDIO_PALETTE[(id as usize) % STUDIO_PALETTE.len()])
}

/// ASS colour literal `&HBBGGRR&`.
fn ass_color(r: u8, g: u8, b: u8) -> String {
    format!("&H{:02X}{:02X}{:02X}&", b, g, r)
}

// ── geometry ───────────────────────────────────────────────────────────────

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.clamp(lo, hi)
}

fn dbfs_to_scale(dbfs: f64, min_scale: f64, max_scale: f64) -> f64 {
    let c = clamp(dbfs, -100.0, 0.0);
    let n = (c + 100.0) / 100.0;
    min_scale + n * (max_scale - min_scale)
}

/// Depth-squeeze span for the pseudo-3D projection (how much the far plane,
/// Y=+1, shrinks). The aspect is capped at 16:9 so wider screens keep the same
/// vertical perspective as 16:9 instead of flattening out toward CINEMA_ASPECT.
fn depth_span_for(res_x: f64, res_y: f64) -> f64 {
    let aspect = (res_x / res_y).min(PERSPECTIVE_MAX_ASPECT);
    1.0 - (aspect / CINEMA_ASPECT).min(1.0)
}

/// Pseudo-3D front-view projection (identical to the former Lua `project_vertex`
/// / `project_trail_point`): X drives screen X, Z drives screen Y, Y drives the
/// depth-squeeze factor `s`.
fn project(
    x: f64,
    y: f64,
    z: f64,
    cx: f64,
    cy: f64,
    res_x: f64,
    res_y: f64,
    depth_span: f64,
) -> (f64, f64, f64) {
    let depth_t = clamp((y + 1.0) * 0.5, 0.0, 1.0);
    let s = 1.0 - depth_t * depth_span;
    let sx = cx + x * (res_x / 2.0) * s;
    let sy = cy - (z - 0.5) * res_y * s;
    (sx, sy, s)
}

// 8 edges of the unit cube X∈{-1,+1} × Y∈{-1,+1} × Z∈{0,+1}. The Y=-1 face is
// omitted (its edges trace the screen border anyway).
const CUBE_EDGES: [[f64; 6]; 8] = [
    [-1.0, 1.0, 0.0, 1.0, 1.0, 0.0],
    [-1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
    [-1.0, 1.0, 0.0, -1.0, 1.0, 1.0],
    [1.0, 1.0, 0.0, 1.0, 1.0, 1.0],
    [-1.0, -1.0, 0.0, -1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0, 1.0, 1.0, 0.0],
    [-1.0, -1.0, 1.0, -1.0, 1.0, 1.0],
    [1.0, -1.0, 1.0, 1.0, 1.0, 1.0],
];

fn build_wireframe(cx: f64, cy: f64, res_x: f64, res_y: f64, depth_span: f64) -> String {
    let mut segs = String::new();
    for e in CUBE_EDGES.iter() {
        let (x1, y1, _) = project(e[0], e[1], e[2], cx, cy, res_x, res_y, depth_span);
        let (x2, y2, _) = project(e[3], e[4], e[5], cx, cy, res_x, res_y, depth_span);
        if !segs.is_empty() {
            segs.push(' ');
        }
        segs.push_str(&format!("m {:.1} {:.1} l {:.1} {:.1}", x1, y1, x2, y2));
    }
    format!(
        "{{\\an7\\pos(0,0)\\bord1\\1a&HFF&\\3c&HFFFFFF&\\3a&H80&\\p1}}{}{{\\p0}}",
        segs
    )
}

/// Vertical depth indicator at the object's (X, Z), spanning Y∈[-1,+1], with a
/// perpendicular tick at Y=0.
fn build_y_axis_line(
    x: f64,
    z: f64,
    cx: f64,
    cy: f64,
    res_x: f64,
    res_y: f64,
    depth_span: f64,
    r: u8,
    g: u8,
    b: u8,
) -> String {
    let (x1, y1, _) = project(x, -1.0, z, cx, cy, res_x, res_y, depth_span);
    let (x2, y2, _) = project(x, 1.0, z, cx, cy, res_x, res_y, depth_span);
    let xm = (x1 + x2) * 0.5;
    let ym = (y1 + y2) * 0.5;
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    let (px, py) = if len > 0.5 {
        (-dy / len, dx / len)
    } else {
        (1.0, 0.0)
    };
    let tx1 = xm - px * Y_TICK_HALF;
    let ty1 = ym - py * Y_TICK_HALF;
    let tx2 = xm + px * Y_TICK_HALF;
    let ty2 = ym + py * Y_TICK_HALF;
    format!(
        "{{\\an7\\pos(0,0)\\bord{}\\blur1\\1a&HFF&\\3c{}\\3a&H80&\\p1}}\
m {:.1} {:.1} l {:.1} {:.1} m {:.1} {:.1} l {:.1} {:.1}{{\\p0}}",
        Y_LINE_BORD,
        ass_color(r, g, b),
        x1,
        y1,
        x2,
        y2,
        tx1,
        ty1,
        tx2,
        ty2
    )
}

// ── object energy heatmap ──────────────────────────────────────────────────

/// Heatmap ramp (port of Studio's `heatmapColor`), `t` ∈ [0, 1]. Colour stops
/// blue → cyan → green → yellow → red, interpolated in RGB. Adjacent stops share
/// a channel at 1.0 so every transition stays vivid (no dark bands), and the
/// stop spacing widens the warm (yellow) band, which a uniform hue sweep renders
/// too narrow.
const HEATMAP_STOPS: [(f64, f64, f64, f64); 5] = [
    (0.00, 0.0, 0.0, 1.0), // blue
    (0.25, 0.0, 1.0, 1.0), // cyan
    (0.48, 0.0, 1.0, 0.0), // green
    (0.70, 1.0, 1.0, 0.0), // yellow
    (1.00, 1.0, 0.0, 0.0), // red
];

/// Map `t` ∈ [0, 1] to an RGB colour for the selected `colormap` (mirror of
/// Studio's `objectEnergyColor`), returned as floats in [0,255] left unquantised
/// on purpose — the caller dithers each channel before rounding to 8-bit (see
/// `build_heatmap_bitmap`) to avoid ramp banding after mpv upscales the small
/// raster. The alpha is applied by the caller (it always encodes the value);
/// colormap 3 (red) keeps the colour constant so only the alpha conveys energy;
/// colormap 4 (custom) interpolates `custom` (sorted `[pos, r, g, b]` stops).
fn heatmap_rgb(t: f64, colormap: u8, custom: &[[f32; 4]]) -> (f64, f64, f64) {
    let t = clamp(t, 0.0, 1.0);
    let c = |v: f64| v * 255.0;
    match colormap {
        4 => custom_stops_rgb(t, custom),     // custom user gradient
        3 => (255.0, 0.0, 0.0),               // red: alpha-only
        2 => (255.0, c(1.0 - t), c(1.0 - t)), // white → red
        1 => (c(t), c(t), 255.0),             // blue → white
        _ => {
            let mut i = 0;
            while i + 1 < HEATMAP_STOPS.len() && t > HEATMAP_STOPS[i + 1].0 {
                i += 1;
            }
            let (t0, r0, g0, b0) = HEATMAP_STOPS[i];
            let (t1, r1, g1, b1) = HEATMAP_STOPS[(i + 1).min(HEATMAP_STOPS.len() - 1)];
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            (
                c(r0 + (r1 - r0) * f),
                c(g0 + (g1 - g0) * f),
                c(b0 + (b1 - b0) * f),
            )
        }
    }
}

/// Interpolate the custom gradient (sorted `[pos, r, g, b]` stops) at `t`, as RGB
/// floats in [0,255]. Empty ⇒ greyscale fallback so a missing push still renders.
fn custom_stops_rgb(t: f64, stops: &[[f32; 4]]) -> (f64, f64, f64) {
    let g = |v: f32| v as f64 * 255.0;
    match stops.len() {
        0 => (t * 255.0, t * 255.0, t * 255.0),
        _ => {
            let first = &stops[0];
            if t <= first[0] as f64 {
                return (g(first[1]), g(first[2]), g(first[3]));
            }
            let last = &stops[stops.len() - 1];
            if t >= last[0] as f64 {
                return (g(last[1]), g(last[2]), g(last[3]));
            }
            for w in stops.windows(2) {
                let (a, b) = (&w[0], &w[1]);
                if t <= b[0] as f64 {
                    let span = (b[0] - a[0]) as f64;
                    let f = if span > 0.0 {
                        (t - a[0] as f64) / span
                    } else {
                        0.0
                    };
                    return (
                        g(a[1]) + (g(b[1]) - g(a[1])) * f,
                        g(a[2]) + (g(b[2]) - g(a[2])) * f,
                        g(a[3]) + (g(b[3]) - g(a[3])) * f,
                    );
                }
            }
            (g(last[1]), g(last[2]), g(last[3]))
        }
    }
}

// 8×8 ordered-dither (Bayer) matrix, values 0..=63. Breaks the 8-bit banding of
// the energy ramp/alpha before quantisation; mpv's bilinear upscale of the small
// raster then averages the pattern back into a smooth gradient. Deterministic and
// allocation-free (cheap enough for the per-frame, per-pixel overlay path).
#[rustfmt::skip]
const BAYER8: [u8; 64] = [
     0, 48, 12, 60,  3, 51, 15, 63,
    32, 16, 44, 28, 35, 19, 47, 31,
     8, 56,  4, 52, 11, 59,  7, 55,
    40, 24, 36, 20, 43, 27, 39, 23,
     2, 50, 14, 62,  1, 49, 13, 61,
    34, 18, 46, 30, 33, 17, 45, 29,
    10, 58,  6, 54,  9, 57,  5, 53,
    42, 26, 38, 22, 41, 25, 37, 21,
];

/// Centred ordered-dither offset for pixel (i, j): one threshold in [-0.5, 0.5)
/// of a single 8-bit step. Same offset for all channels of a pixel → debands
/// without adding chroma noise and keeps premultiplied colour/alpha consistent.
#[inline]
fn dither_offset(i: usize, j: usize) -> f64 {
    (BAYER8[(j & 7) * 8 + (i & 7)] as f64 + 0.5) / 64.0 - 0.5
}

/// Dither then quantise a [0,255] float channel to 8-bit.
#[inline]
fn dither_u8(v: f64, d: f64) -> u8 {
    (v + d).round().clamp(0.0, 255.0) as u8
}

/// A finished heatmap bitmap ready for mpv's `overlay-add` (BGRA, premultiplied
/// alpha). `pixels` is `w*h*4` bytes; mpv scales the `w×h` source to the on-screen
/// `dw×dh` rect at `(x, y)`.
pub struct HeatmapBitmap {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub dw: i32,
    pub dh: i32,
    pub pixels: Vec<u8>,
}

/// Render the depth-sliced energy field, flattened into a single BGRA bitmap.
/// The three depth planes (slicing along Omniphony Y, the into-screen axis) are
/// composited back → front into one image sized to the front plane's on-screen
/// rect; nearer planes appear larger and blend over farther ones. Returns `None`
/// when there is no audible object.
fn build_heatmap_bitmap(s: &OverlayState, res_x: f64, res_y: f64) -> Option<HeatmapBitmap> {
    if !s.heatmap_enabled {
        return None;
    }
    let cx = res_x / 2.0;
    let cy = res_y / 2.0;
    let depth_span = depth_span_for(res_x, res_y);

    // Gather audible objects as (x, y, z, linear energy). Silence floor mirrors
    // Studio (RMS ≤ -100 dBFS ⇒ no contribution).
    let objs: Vec<(f64, f64, f64, f64)> = s
        .positions
        .iter()
        .filter_map(|&(id, x, y, z)| {
            let rms = s.levels.get(&id).copied().unwrap_or(-100.0);
            if rms <= -100.0 {
                return None;
            }
            Some((x, y, z, 10f64.powf(rms / 10.0)))
        })
        .collect();
    if objs.is_empty() {
        return None;
    }
    let r2 = FIELD_FALLOFF_R * FIELD_FALLOFF_R;
    let bands_n = s.heatmap_bands.clamp(1, 12);

    // Per-band depth Y and its on-screen scale `s_k` (front plane has the largest
    // scale → biggest rect, and contains the others since all are centred).
    let bands: Vec<(f64, f64)> = (0..bands_n)
        .map(|b| {
            let yb = -1.0 + (2.0 * b as f64 + 1.0) / bands_n as f64;
            let depth_t = clamp((yb + 1.0) * 0.5, 0.0, 1.0);
            (yb, 1.0 - depth_t * depth_span)
        })
        .collect();
    let s_front = bands.iter().fold(0.0f64, |m, &(_, sk)| m.max(sk));
    if s_front <= 0.0 {
        return None;
    }

    // Display rect (front plane) and internal raster size (adaptive, capped).
    let dw = (res_x * s_front).round().max(1.0);
    let dh = (res_y * s_front).round().max(1.0);
    let x = (cx - dw * 0.5).round();
    let y = (cy - dh * 0.5).round();
    let w = (dw as usize).clamp(8, FIELD_BITMAP_MAX);
    let h = (dh as usize).clamp(8, FIELD_BITMAP_MAX);
    let cxg = w as f64 * 0.5;
    let cyg = h as f64 * 0.5;

    // Pass 1: energy per band per pixel (0 outside the band's centred sub-rect),
    // tracking the global max for normalisation.
    let mut grids: Vec<Vec<f32>> = Vec::with_capacity(bands_n);
    let mut max_e = 0.0f64;
    for &(yb, sk) in &bands {
        let scale = sk / s_front; // ≤ 1
        let half_w = cxg * scale;
        let half_h = cyg * scale;
        let mut g = vec![0f32; w * h];
        if half_w >= 0.5 && half_h >= 0.5 {
            for j in 0..h {
                let v = (j as f64 + 0.5 - cyg) / half_h; // [-1, 1]
                if !(-1.0..=1.0).contains(&v) {
                    continue;
                }
                let zc = 0.5 - v * 0.5; // top → Z=1, bottom → Z=0
                for i in 0..w {
                    let u = (i as f64 + 0.5 - cxg) / half_w; // [-1, 1] → X
                    if !(-1.0..=1.0).contains(&u) {
                        continue;
                    }
                    let mut e = 0.0;
                    for &(ox, oy, oz, oe) in &objs {
                        let dx = u - ox;
                        let dy = yb - oy;
                        let dz = zc - oz;
                        e += oe / (dx * dx + dy * dy + dz * dz + r2);
                    }
                    if e > max_e {
                        max_e = e;
                    }
                    g[j * w + i] = e as f32;
                }
            }
        }
        grids.push(g);
    }
    if max_e <= 0.0 {
        return None;
    }
    let inv = 1.0 / max_e;

    // Pass 2: combine the depth planes by the strongest energy per pixel, then
    // colourise once. Alpha-over compositing of separately-coloured translucent
    // planes muddied overlaps — complementary hues (e.g. yellow over cyan) blend
    // toward dark, and the alpha accumulates so overlaps turned more opaque and
    // darker. A single ramp hue per pixel from the combined field avoids that
    // entirely, with alpha bounded by FIELD_OPACITY, while the planes' nested
    // on-screen extents still convey the pseudo-3D depth.
    let colormap = s.heatmap_colormap;
    let custom = s.heatmap_custom_stops.as_slice();
    let mut pixels = vec![0u8; w * h * 4];
    for j in 0..h {
        for i in 0..w {
            let idx = j * w + i;
            let mut e = 0.0f32;
            for g in grids.iter().take(bands_n) {
                if g[idx] > e {
                    e = g[idx];
                }
            }
            let t = clamp(e as f64 * inv, 0.0, 1.0);
            if t < 0.01 {
                continue; // leave fully transparent
            }
            let (rf, gf, bf) = heatmap_rgb(t, colormap, custom);
            let a = t * FIELD_OPACITY; // [0, FIELD_OPACITY]
            let d = dither_offset(i, j);
            let o = idx * 4;
            // Premultiplied BGRA, ordered-dithered before 8-bit quantisation so
            // the ramp/alpha don't band once mpv upscales this raster.
            pixels[o] = dither_u8(bf * a, d);
            pixels[o + 1] = dither_u8(gf * a, d);
            pixels[o + 2] = dither_u8(rf * a, d);
            pixels[o + 3] = dither_u8(a * 255.0, d);
        }
    }

    Some(HeatmapBitmap {
        x: x as i32,
        y: y as i32,
        w: w as i32,
        h: h as i32,
        dw: dw as i32,
        dh: dh as i32,
        pixels,
    })
}

/// Public entry: lock the scene and render the flattened heatmap bitmap. Does
/// not touch the trails or the pull clock (the ASS pull already advances those).
pub fn build_heatmap(res_x: u32, res_y: u32) -> Option<HeatmapBitmap> {
    let o = overlay();
    if !o.enabled.load(Ordering::Relaxed) || !session_active() || res_x == 0 || res_y == 0 {
        return None;
    }
    let s = o.state.lock().ok()?;
    build_heatmap_bitmap(&s, res_x as f64, res_y as f64)
}

// ── trails ───────────────────────────────────────────────────────────────

fn trail_append(s: &mut OverlayState, id: u32, x: f64, y: f64, z: f64, now: f64) {
    if !s.cfg.enabled {
        s.trails.remove(&id);
        return;
    }
    let teleport_sq = s.cfg.teleport_sq;
    let ttl_s = s.cfg.ttl_s;
    let t = s.trails.entry(id).or_default();
    // Always take the first point; otherwise throttle to the sample interval.
    // (Unlike the former Lua, our clock starts near zero, so a fresh trail's
    // `last_t == 0.0` must not gate the opening sample.)
    if !t.points.is_empty() && now - t.last_t < TRAIL_MIN_POINT_INTERVAL_S {
        return;
    }
    t.last_t = now;
    let brk = match t.points.last() {
        Some(last) => {
            let dx = x - last.x;
            let dy = y - last.y;
            let dz = z - last.z;
            dx * dx + dy * dy + dz * dz > teleport_sq
        }
        None => false,
    };
    t.points.push(TrailPoint {
        x,
        y,
        z,
        t: now,
        brk,
    });
    // Prune by TTL + cap, keeping the tail (newest points).
    let cutoff = now - ttl_s;
    let mut first = 0;
    while first < t.points.len() && t.points[first].t < cutoff {
        first += 1;
    }
    if first > 0 || t.points.len() > TRAIL_MAX_POINTS {
        let lo = first.max(t.points.len().saturating_sub(TRAIL_MAX_POINTS));
        t.points.drain(..lo);
    }
}

fn build_trail_line(
    t: &Trail,
    cx: f64,
    cy: f64,
    res_x: f64,
    res_y: f64,
    depth_span: f64,
    r: u8,
    g: u8,
    b: u8,
) -> Option<String> {
    // Emit each consecutive pair as its own subpath so libass strokes the
    // polyline rather than auto-closing it into a polygon.
    let mut segs = String::new();
    for i in 0..t.points.len().saturating_sub(1) {
        if t.points[i + 1].brk {
            continue;
        }
        let p1 = &t.points[i];
        let p2 = &t.points[i + 1];
        let (x1, y1, _) = project(p1.x, p1.y, p1.z, cx, cy, res_x, res_y, depth_span);
        let (x2, y2, _) = project(p2.x, p2.y, p2.z, cx, cy, res_x, res_y, depth_span);
        if !segs.is_empty() {
            segs.push(' ');
        }
        segs.push_str(&format!("m {:.1} {:.1} l {:.1} {:.1}", x1, y1, x2, y2));
    }
    if segs.is_empty() {
        return None;
    }
    Some(format!(
        "{{\\an7\\pos(0,0)\\bord1\\1a&HFF&\\3c{}\\3a&H70&\\p1}}{}{{\\p0}}",
        ass_color(r, g, b),
        segs
    ))
}

fn emit_diffuse_dot(
    sx: f64,
    sy: f64,
    age_fade: f64,
    base_radius: f64,
    depth_scale: f64,
    col: &str,
) -> String {
    // Size and opacity track the particle's *age* (age_fade: 1 at the head,
    // 0 at the TTL), not its index in the buffer — so the trail fades smoothly
    // by ancienneté regardless of how the particle budget is allocated, and the
    // budget truncation lands on already-transparent (oldest) particles.
    let r_factor = 0.30 + 0.70 * age_fade;
    let dot_r = base_radius * r_factor * depth_scale;
    let alpha = 1.0 - 0.70 * age_fade;
    let alpha_hex = ((alpha * 255.0 + 0.5) as i32).clamp(0, 255);
    format!(
        "{{\\an7\\pos({:.1},{:.1})\\bord0\\blur{}\\fscx{:.1}\\fscy{:.1}\\1c{}\\1a&H{:02X}&\\p1}}{}{{\\p0}}",
        sx, sy, DIFFUSE_BLUR, dot_r, dot_r, col, alpha_hex, UNIT_CIRCLE
    )
}

#[allow(clippy::too_many_arguments)]
fn build_trail_diffuse(
    t: &Trail,
    cx: f64,
    cy: f64,
    res_x: f64,
    res_y: f64,
    depth_span: f64,
    ttl_s: f64,
    now: f64,
    base_radius: f64,
    col: &str,
) -> Option<String> {
    let count = t.points.len();
    if count < 2 {
        return None;
    }
    // Pre-project every stored buffer point once.
    let pts: Vec<(f64, f64, f64, f64, bool)> = t
        .points
        .iter()
        .map(|p| {
            let (sx, sy, s) = project(p.x, p.y, p.z, cx, cy, res_x, res_y, depth_span);
            (sx, sy, s, p.t, p.brk)
        })
        .collect();

    let target_spacing = (base_radius * DIFFUSE_SPACING_FACTOR).max(2.0);

    // Collect every candidate particle along the whole trail (oldest → newest),
    // by spacing-based subdivision. Cheap params only — no string formatting
    // yet. `age_fade` drives size + opacity, so the tail fades by age.
    let mut cands: Vec<(f64, f64, f64, f64)> = Vec::new(); // (sx, sy, depth_scale, age_fade)
    for i in 0..count - 1 {
        if pts[i + 1].4 {
            // teleport break: don't interpolate across the jump
            continue;
        }
        let (p1x, p1y, p1s, p1t, _) = pts[i];
        let (p2x, p2y, p2s, p2t, _) = pts[i + 1];
        let dx = p2x - p1x;
        let dy = p2y - p1y;
        let seg_len = (dx * dx + dy * dy).sqrt();
        let subdiv = ((seg_len / target_spacing).ceil() as i32).clamp(1, DIFFUSE_MAX_SUBDIV);
        for sub in 0..subdiv {
            let frac = sub as f64 / subdiv as f64;
            let pt = p1t + frac * (p2t - p1t);
            let age = now - pt;
            if age <= ttl_s {
                cands.push((
                    p1x + frac * dx,
                    p1y + frac * dy,
                    p1s + frac * (p2s - p1s),
                    1.0 - (age / ttl_s),
                ));
            }
        }
    }
    // Head: the newest point itself (start of no segment), always the last cand.
    let (hsx, hsy, hs, hpt, _) = pts[count - 1];
    if now - hpt <= ttl_s {
        cands.push((hsx, hsy, hs, 1.0 - ((now - hpt) / ttl_s)));
    }
    if cands.is_empty() {
        return None;
    }

    // Budget scales with the TTL (longer trail → more dots), capped for libass
    // blur cost. The budget is deliberately generous so a normal trail is drawn
    // at full spacing-based density; only an extreme (very long *and* fast)
    // trail exceeds it, in which case we keep the newest `cap` candidates and
    // drop the oldest — which are already faded toward transparency by age, so
    // the drop is invisible (no hard cutoff).
    let cap =
        ((ttl_s * DIFFUSE_DOTS_PER_S).round() as usize).clamp(DIFFUSE_MIN_DOTS, DIFFUSE_MAX_DOTS);
    let n = cands.len();
    let start = n.saturating_sub(cap);

    // Emit oldest → newest so the freshest particle is drawn on top (correct
    // z-order; faded old dots must not paint over fresh ones).
    let out: Vec<String> = cands[start..]
        .iter()
        .map(|&(sx, sy, s, age_fade)| emit_diffuse_dot(sx, sy, age_fade, base_radius, s, col))
        .collect();
    Some(out.join("\n"))
}

// ── core ───────────────────────────────────────────────────────────────────

fn render(s: &mut OverlayState, res_x: f64, res_y: f64, now: f64) -> String {
    let cx = res_x / 2.0;
    let cy = res_y / 2.0;
    let depth_span = depth_span_for(res_x, res_y);
    let base_radius = (res_y * BASE_RADIUS_RATIO).max(8.0);
    let label_fs = (res_y * LABEL_FONT_RATIO).round().max(12.0);

    let mut out: Vec<String> = Vec::new();
    // The energy heatmap is a separate BGRA bitmap overlay (built via
    // `build_heatmap`), composited by mpv *under* this ASS layer — see the Lua
    // shim. The ASS layer only draws the cube wireframe, objects and trails.
    out.push(build_wireframe(cx, cy, res_x, res_y, depth_span));

    // Snapshot the scene so we can mutate trails while iterating.
    let positions = s.positions.clone();
    let n_obj = positions.len();
    // Objects (markers + labels + trails + depth lines) are a display-only group:
    // when hidden the cube wireframe and header remain, and the energy heatmap is
    // untouched. Mirrors Studio's "show objects" switch.
    if s.objects_visible {
        for &(id, x, y, z) in &positions {
            let (r, g, b) = object_color(id, s.tags.get(&id).copied());
            let col = ass_color(r, g, b);

            let (sx, sy, sdepth) = project(x, y, z, cx, cy, res_x, res_y, depth_span);
            let rms = s.levels.get(&id).copied().unwrap_or(-100.0);
            let level_scale = dbfs_to_scale(rms, 0.5, 2.4);
            let pct = base_radius * level_scale * sdepth;

            // Sample into the trail buffer, then draw the trail under the circle.
            trail_append(s, id, x, y, z, now);
            if s.cfg.enabled {
                if let Some(t) = s.trails.get(&id) {
                    if t.points.len() >= 2 {
                        let evt = match s.cfg.mode {
                            TrailMode::Diffuse => build_trail_diffuse(
                                t,
                                cx,
                                cy,
                                res_x,
                                res_y,
                                depth_span,
                                s.cfg.ttl_s,
                                now,
                                base_radius,
                                &col,
                            ),
                            TrailMode::Line => {
                                build_trail_line(t, cx, cy, res_x, res_y, depth_span, r, g, b)
                            }
                        };
                        if let Some(evt) = evt {
                            out.push(evt);
                        }
                    }
                }
            }

            out.push(build_y_axis_line(
                x, z, cx, cy, res_x, res_y, depth_span, r, g, b,
            ));

            out.push(format!(
            "{{\\an7\\pos({:.1},{:.1})\\bord1\\fscx{:.1}\\fscy{:.1}\\1c{}\\3c&H000000&\\1a&H30&\\3a&H80&\\p1}}{}{{\\p0}}",
            sx, sy, pct, pct, col, UNIT_CIRCLE
        ));

            // Object label centred on the object (like Studio's 3D view), large,
            // white text + black outline for readability over the video.
            if s.labels_enabled {
                if let Some(label) = s.labels.get(&id) {
                    out.push(format!(
                        "{{\\an5\\pos({:.1},{:.1})\\fs{:.0}\\bord2\\1c&HFFFFFF&\\3c&H000000&}}{}",
                        sx,
                        sy,
                        label_fs,
                        ass_escape(label)
                    ));
                }
            }
        }
    }

    // Prune trails for objects gone longer than the TTL (avoids unbounded
    // growth; live objects are kept by `trail_append`).
    let cutoff = now - s.cfg.ttl_s;
    s.trails.retain(|_, t| t.last_t >= cutoff);

    let header = format!(
        "{{\\an9\\pos({:.1},{:.1})\\bord1\\fs{}\\1c&HFFFFFF&\\3c&H000000&}}{} objects",
        res_x - 12.0,
        10.0,
        HEADER_FONT_SIZE,
        n_obj
    );

    let mut body = out.join("\n");
    body.push('\n');
    body.push_str(&header);
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    // The overlay state is a process-global singleton, so the tests must not
    // run concurrently against it. Serialise them and reset to a known state.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Detach any persistence path so tests never touch the filesystem.
        *overlay().prefs_path.lock().unwrap() = None;
        clear();
        set_enabled(true);
        set_rendering(true);
        set_labels_enabled(true);
        set_objects_visible(true);
        set_heatmap_enabled(true);
        // Arm exactly one render session so the draw paths are reachable; the
        // no-session gate is exercised on its own in `no_session_returns_empty`.
        overlay().sessions.store(1, Ordering::Relaxed);
        g
    }

    #[test]
    fn empty_scene_still_draws_the_cube_and_header() {
        let _g = guard();
        update_positions(Vec::new());
        let ass = build_ass(1920, 1080);
        assert!(ass.contains("\\p1"), "should contain a drawing");
        assert!(ass.contains("0 objects"), "header reports object count");
    }

    #[test]
    fn disabled_returns_empty() {
        let _g = guard();
        set_enabled(false);
        let ass = build_ass(1920, 1080);
        assert!(ass.is_empty());
    }

    // A live session that is not spatial-rendering (mpv in host mode) must draw
    // nothing at all — not even the wireframe cube — until rendering resumes.
    #[test]
    fn not_rendering_returns_empty() {
        let _g = guard();
        update_positions(vec![(0, 0.0, 0.0, 0.5, String::new())]);
        set_rendering(false);
        assert!(build_ass(1920, 1080).is_empty(), "host mode draws no cube");
        set_rendering(true);
        assert!(
            !build_ass(1920, 1080).is_empty(),
            "spatial mode draws again"
        );
    }

    #[test]
    fn zero_resolution_returns_empty() {
        let _g = guard();
        assert!(build_ass(0, 0).is_empty());
    }

    // Without an orender session (e.g. mpv started without `--ad=orender`) the
    // overlay must draw nothing — not a permanent, scene-less wireframe box —
    // even when enabled and fed positions. `session_ended` clears the scene as
    // the last session goes away.
    #[test]
    fn no_session_returns_empty() {
        let _g = guard();
        update_positions(vec![(0, 0.0, 0.0, 0.5, String::new())]);
        update_levels(&[(0, -6.0)]);
        // guard() armed one session; end it → back to the no-session state.
        session_ended();
        assert!(!session_active(), "session counter should be back to zero");
        assert!(
            build_ass(1920, 1080).is_empty(),
            "ASS must be empty with no session"
        );
        assert!(
            build_heatmap(1920, 1080).is_none(),
            "heatmap must be absent with no session"
        );
        // A fresh session re-arms the overlay (positions were cleared on the last
        // session end, so feed them again to get a non-empty draw).
        session_started();
        update_positions(vec![(0, 0.0, 0.0, 0.5, String::new())]);
        assert!(
            build_ass(1920, 1080).contains("\\p1"),
            "ASS draws once a session is live"
        );
    }

    // `session_ended` without a matching `session_started` must not underflow.
    #[test]
    fn session_counter_saturates_at_zero() {
        let _g = guard();
        overlay().sessions.store(0, Ordering::Relaxed);
        session_ended();
        session_ended();
        assert_eq!(overlay().sessions.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn toggles_flip_and_return_new_state() {
        let _g = guard();
        // guard() leaves enable/labels/objects/heatmap on; each toggle flips and
        // reports the resulting value.
        assert!(!toggle_enabled());
        assert!(toggle_enabled());
        assert!(!toggle_labels());
        assert!(toggle_labels());
        assert!(!toggle_objects());
        assert!(toggle_objects());
        assert!(!toggle_heatmap());
        assert!(toggle_heatmap());
    }

    #[test]
    fn toggle_trails_clears_buffers_when_disabling() {
        let _g = guard();
        // Build up a trail, then disable: the buffer must be dropped so it can't
        // reappear on re-enable.
        update_positions(vec![(0, 0.0, 0.0, 0.5, String::new())]);
        let now = now_secs();
        trail_append(&mut overlay().state.lock().unwrap(), 0, 0.0, 0.0, 0.5, now);
        assert!(!overlay().state.lock().unwrap().trails.is_empty());
        assert!(!toggle_trails(), "trails start enabled (default) → off");
        assert!(overlay().state.lock().unwrap().trails.is_empty());
        assert!(toggle_trails(), "back on");
    }

    #[test]
    fn heatmap_bands_clamp_and_colormap_wraps() {
        let _g = guard();
        set_heatmap_bands(3);
        assert_eq!(adjust_heatmap_bands(2), 5);
        assert_eq!(adjust_heatmap_bands(-10), 1, "clamps at 1");
        assert_eq!(adjust_heatmap_bands(100), 12, "clamps at 12");
        set_heatmap_colormap(3);
        assert_eq!(cycle_heatmap_colormap(), 4);
        assert_eq!(cycle_heatmap_colormap(), 0, "wraps 4 → 0");
    }

    // The energy heatmap is now a separate BGRA bitmap (drawn under the ASS via
    // mpv's overlay-add), not part of the ASS string.
    #[test]
    fn heatmap_bitmap_renders_for_audible_object() {
        let _g = guard();
        update_positions(vec![(0, 0.0, 0.0, 0.5, String::new())]);
        update_levels(&[(0, -6.0)]);
        let bmp = build_heatmap(1920, 1080).expect("audible object yields a heatmap bitmap");
        assert_eq!(bmp.pixels.len(), (bmp.w * bmp.h * 4) as usize);
        assert!(bmp.w > 0 && bmp.h > 0 && bmp.dw > 0 && bmp.dh > 0);
        // At least one pixel must be non-transparent (alpha byte set).
        assert!(
            bmp.pixels.chunks_exact(4).any(|p| p[3] > 0),
            "heatmap should have visible (non-zero alpha) pixels"
        );
    }

    #[test]
    fn heatmap_bitmap_absent_when_silent() {
        let _g = guard();
        update_positions(vec![(0, 0.0, 0.0, 0.5, String::new())]);
        update_levels(&[(0, -120.0)]);
        assert!(
            build_heatmap(1920, 1080).is_none(),
            "no heatmap when every object is below the silence floor"
        );
    }

    #[test]
    fn custom_stops_interpolate_and_fall_back() {
        // Empty ⇒ greyscale fallback (colour tracks the value on all channels).
        let (r, g, b) = custom_stops_rgb(0.5, &[]);
        assert!((r - 127.5).abs() < 1e-6 && (g - 127.5).abs() < 1e-6 && (b - 127.5).abs() < 1e-6);

        // Black→white: midpoint is mid-grey, ends clamp to the edge stops.
        let stops = [[0.0, 0.0, 0.0, 0.0], [1.0, 1.0, 1.0, 1.0]];
        assert_eq!(custom_stops_rgb(0.0, &stops), (0.0, 0.0, 0.0));
        let (mr, mg, mb) = custom_stops_rgb(0.5, &stops);
        assert!(
            (mr - 127.5).abs() < 1e-3 && (mg - 127.5).abs() < 1e-3 && (mb - 127.5).abs() < 1e-3
        );
        assert_eq!(custom_stops_rgb(1.0, &stops), (255.0, 255.0, 255.0));
        // Below the first / above the last stop clamps, doesn't extrapolate.
        assert_eq!(custom_stops_rgb(-1.0, &stops), (0.0, 0.0, 0.0));
        assert_eq!(custom_stops_rgb(2.0, &stops), (255.0, 255.0, 255.0));
    }

    #[test]
    fn depth_span_frozen_at_or_above_16_9() {
        let base = depth_span_for(1920.0, 1080.0); // 16:9
        assert!(base > 0.0, "16:9 keeps a real perspective squeeze");
        // Screens wider than 16:9 keep the exact 16:9 vertical perspective.
        for (w, h) in [(2560.0, 1080.0), (3440.0, 1440.0), (4000.0, 1000.0)] {
            assert!(
                (depth_span_for(w, h) - base).abs() < 1e-9,
                "wider-than-16:9 must not flatten the perspective"
            );
        }
        // Narrower than 16:9 keeps the previous (stronger) perspective.
        assert!(depth_span_for(1440.0, 1080.0) > base); // 4:3
    }

    #[test]
    fn heatmap_bitmap_none_when_disabled() {
        let _g = guard();
        update_positions(vec![(0, 0.0, 0.0, 0.5, String::new())]);
        update_levels(&[(0, -6.0)]);
        set_enabled(false);
        assert!(build_heatmap(1920, 1080).is_none());
    }

    #[test]
    fn heatmap_bitmap_honours_band_count() {
        let _g = guard();
        update_positions(vec![(0, 0.0, 0.0, 0.5, String::new())]);
        update_levels(&[(0, -6.0)]);
        // Exercise the band-count extremes (1 and 12) — the composite loop must
        // index exactly `bands_n` grids, never the const default.
        for bands in [1usize, 5, 12] {
            set_heatmap_bands(bands);
            let bmp = build_heatmap(1920, 1080).expect("bitmap for any band count");
            assert_eq!(bmp.pixels.len(), (bmp.w * bmp.h * 4) as usize);
        }
        // Out-of-range is clamped, not panicking.
        set_heatmap_bands(999);
        assert!(build_heatmap(1920, 1080).is_some());
    }

    #[test]
    fn objects_hidden_keeps_cube_and_header() {
        let _g = guard();
        update_positions(vec![(0, 0.5, 0.0, 0.5, "a_obj_Voice".to_string())]);
        crate::overlay::set_objects_visible(false);
        let ass = build_ass(1920, 1080);
        assert!(ass.contains("1 objects"), "header still reports the count");
        // The object circle uses `\fscx`; hidden objects must not emit one.
        assert!(
            !ass.contains("\\fscx"),
            "no object marker drawn when hidden"
        );
    }

    #[test]
    fn object_renders_a_circle_and_counts() {
        let _g = guard();
        update_positions(vec![(0, 0.0, 1.0, 0.5, "Obj_0".to_string())]);
        update_levels(&[(0, -20.0)]);
        let ass = build_ass(1920, 1080);
        assert!(ass.contains("1 objects"));
        // Object 0 → first palette colour FF6B6B → ass &HBBGGRR& = &H6B6BFF&.
        assert!(ass.contains("&H6B6BFF&"), "palette colour 0 present: {ass}");
    }

    #[test]
    fn center_object_projects_to_screen_centre() {
        let _g = guard();
        // X=0, Z=0.5 → screen centre; Y=1 (front) → no depth squeeze on a
        // display narrower than 2.35:1.
        update_positions(vec![(0, 0.0, 1.0, 0.5, "Obj_0".to_string())]);
        let ass = build_ass(1920, 1080);
        assert!(ass.contains("\\pos(960.0,540.0)"), "centre circle: {ass}");
    }

    #[test]
    fn label_formatting_matches_studio() {
        assert_eq!(format_object_label("a_dialog"), "dialog");
        assert_eq!(format_object_label("obj_5"), "5");
        assert_eq!(format_object_label("Obj_5"), "5");
        assert_eq!(format_object_label("music_left"), "left");
        assert_eq!(format_object_label("Ambience"), "Ambience");
    }

    #[test]
    fn label_drawn_and_toggleable() {
        let _g = guard();
        update_positions(vec![(0, 0.0, 1.0, 0.5, "a_Dialogue".to_string())]);
        let ass = build_ass(1920, 1080);
        assert!(ass.contains("Dialogue"), "label drawn: {ass}");
        set_labels_enabled(false);
        let ass = build_ass(1920, 1080);
        assert!(!ass.contains("Dialogue"), "label hidden when disabled");
        set_labels_enabled(true);
    }

    #[test]
    fn trail_accumulates_and_renders() {
        // Drives the trail internals with explicit timestamps (the public path
        // throttles appends to 70 ms, which is awkward to exercise in a test).
        let mut s = OverlayState::default();
        s.cfg.enabled = true;
        s.cfg.mode = TrailMode::Line;
        // Small move (below the teleport threshold) so the segment is drawn.
        trail_append(&mut s, 0, -0.1, 1.0, 0.5, 0.0);
        trail_append(&mut s, 0, 0.0, 1.0, 0.5, 0.1);
        let t = s.trails.get(&0).expect("trail buffered");
        assert_eq!(t.points.len(), 2, "two points accumulated");
        let evt = build_trail_line(t, 960.0, 540.0, 1920.0, 1080.0, 0.0, 255, 0, 0);
        assert!(evt.is_some(), "line trail renders a segment");
        assert!(evt.unwrap().contains("\\p1"), "trail is a drawing");
    }

    #[test]
    fn trail_teleport_breaks_the_segment() {
        let mut s = OverlayState::default();
        s.cfg.enabled = true;
        s.cfg.teleport_sq = 0.25; // threshold 0.5
        trail_append(&mut s, 0, -0.9, 0.0, 0.5, 0.0);
        trail_append(&mut s, 0, 0.9, 0.0, 0.5, 0.1); // far jump → teleport
        let t = s.trails.get(&0).unwrap();
        assert!(t.points[1].brk, "large jump flagged as a teleport");
        // The single pair is a teleport, so the line trail emits nothing.
        assert!(build_trail_line(t, 960.0, 540.0, 1920.0, 1080.0, 0.0, 255, 0, 0).is_none());
    }

    #[test]
    fn diffuse_long_trail_always_draws_the_head() {
        // A long, far-moving trail that would exhaust the particle budget on the
        // old tail must still draw a dot at the newest point (the object).
        let mut s = OverlayState::default();
        s.cfg.enabled = true;
        s.cfg.mode = TrailMode::Diffuse;
        s.cfg.ttl_s = 30.0;
        let n = 200usize;
        for i in 0..n {
            let x = -1.0 + 2.0 * (i as f64) / (n as f64); // small steps, no teleport
            trail_append(&mut s, 0, x, 0.0, 0.5, i as f64 * 0.08);
        }
        let t = s.trails.get(&0).unwrap();

        let (res_x, res_y) = (1920.0_f64, 1080.0_f64);
        let depth_span = depth_span_for(res_x, res_y);
        let ttl = 30.0;
        let now = (n as f64 - 1.0) * 0.08;
        let evt = build_trail_diffuse(
            t,
            res_x / 2.0,
            res_y / 2.0,
            res_x,
            res_y,
            depth_span,
            ttl,
            now,
            16.0,
            "&H0000FF&",
        )
        .expect("long diffuse trail renders");

        let last = t.points.last().unwrap();
        let (hx, hy, _) = project(
            last.x,
            last.y,
            last.z,
            res_x / 2.0,
            res_y / 2.0,
            res_x,
            res_y,
            depth_span,
        );
        assert!(
            evt.contains(&format!("\\pos({:.1},{:.1})", hx, hy)),
            "the head dot (newest point) is always drawn"
        );
        // Particle count stays within the (TTL-scaled) budget ceiling.
        let cap =
            ((ttl * DIFFUSE_DOTS_PER_S).round() as usize).clamp(DIFFUSE_MIN_DOTS, DIFFUSE_MAX_DOTS);
        assert!(evt.matches("\\p1").count() <= cap);
    }

    #[test]
    fn prefs_round_trip_via_file() {
        let _g = guard();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "omniphony-overlay-test-{}.conf",
            std::process::id()
        ));

        // A live change auto-persists to the file.
        load_prefs(&path);
        set_labels_enabled(false);
        set_trail_config(true, 12000, false, 0.8);
        let written = std::fs::read_to_string(&path).expect("prefs persisted");
        assert!(written.contains("labels=0"));
        assert!(written.contains("ttl_ms=12000"));
        assert!(written.contains("mode=line"));

        // Loading the file applies it (labels off here → no label drawn).
        load_prefs(&path);
        update_positions(vec![(0, 0.0, 1.0, 0.5, "a_Dialogue".to_string())]);
        assert!(!build_ass(1920, 1080).contains("Dialogue"));

        std::fs::remove_file(&path).ok();
        *overlay().prefs_path.lock().unwrap() = None;
    }

    #[test]
    fn is_active_tracks_pull_and_enable() {
        let _g = guard();
        let _ = build_ass(640, 480); // records a pull
        assert!(is_active());
        set_enabled(false);
        assert!(!is_active());
    }

    /// Every display mutator must move the generation and be visible in the
    /// published state — that is what keeps a client's switches honest when the
    /// *other* writer (an mpv keybind through the FFI toggles) flips something.
    #[test]
    fn display_changes_bump_the_generation_and_reach_the_published_state() {
        let _g = guard();
        let json = |()| -> serde_json::Value {
            serde_json::from_str(&display_state_json()).expect("published state is JSON")
        };

        // A keybind-style toggle, the case Studio cannot otherwise observe.
        let before = state_generation();
        let labels_now = toggle_labels();
        assert!(
            state_generation() > before,
            "toggling labels must publish a change"
        );
        assert_eq!(json(())["labelsEnabled"], labels_now);

        // A Studio-style setter on a transient (non-persisted) field.
        let before = state_generation();
        set_heatmap_bands(9);
        assert!(state_generation() > before, "band count must publish");
        assert_eq!(json(())["heatmapBands"], 9);

        // The master enable lives in an atomic, not the state lock — easy to
        // forget, so pin it too.
        let before = state_generation();
        set_enabled(false);
        assert!(state_generation() > before, "master enable must publish");
        assert_eq!(json(())["enabled"], false);

        // Trail config travels as several fields; all of them must round-trip.
        let before = state_generation();
        set_trail_config(true, 2500, true, 0.75);
        assert!(state_generation() > before, "trail config must publish");
        let v = json(());
        assert_eq!(v["trailsEnabled"], true);
        assert_eq!(v["trailTtlMs"], 2500);
        assert_eq!(v["trailMode"], "diffuse");
        assert!((v["trailTeleportThreshold"].as_f64().unwrap() - 0.75).abs() < 1e-6);
    }

    /// Per-frame scene data must NOT bump the generation: it changes every
    /// frame and would turn the state broadcast into a flood.
    #[test]
    fn scene_updates_do_not_publish() {
        let _g = guard();
        let before = state_generation();
        update_positions(vec![(1, 0.1, 0.2, 0.3, "obj".to_string())]);
        update_levels(&[(1, -12.0)]);
        assert_eq!(
            state_generation(),
            before,
            "positions/levels are per-frame data, not a display preference"
        );
    }
}
