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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

// ── tunables (mirrors of the former Lua constants) ────────────────────────

const BASE_RADIUS_RATIO: f64 = 0.015; // fraction of screen height
const HEADER_FONT_SIZE: i32 = 14;
// Object-name label height as a fraction of screen height (~65 px at 1080p),
// centred on the object. Scales with resolution so it looks the same on 4K.
const LABEL_FONT_RATIO: f64 = 0.06;
const CINEMA_ASPECT: f64 = 2.35; // pseudo-3D depth squeezes Y=+1 into this band

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
            labels_enabled: true, // on by default, like Studio's 3D view
            cfg: TrailCfg::default(),
        }
    }
}

struct Overlay {
    enabled: AtomicBool,
    /// `start.elapsed()` ms at the last FFI pull; self-gates per-frame work.
    last_pull_ms: AtomicU64,
    start: Instant,
    state: Mutex<OverlayState>,
    /// Path to the dedicated overlay-prefs file (owned by orender, persisted in
    /// real time, separate from the savable config). `None` until set by the host.
    prefs_path: Mutex<Option<PathBuf>>,
}

fn overlay() -> &'static Overlay {
    static OVERLAY: OnceLock<Overlay> = OnceLock::new();
    OVERLAY.get_or_init(|| Overlay {
        enabled: AtomicBool::new(true),
        last_pull_ms: AtomicU64::new(0),
        start: Instant::now(),
        state: Mutex::new(OverlayState::default()),
        prefs_path: Mutex::new(None),
    })
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

/// Master enable/disable (Studio toggle; also gates `is_active`).
pub fn set_enabled(on: bool) {
    overlay().enabled.store(on, Ordering::Relaxed);
    save_prefs();
}

/// Show/hide object labels (mirror of Studio's `objectLabelsEnabled`).
pub fn set_labels_enabled(on: bool) {
    if let Ok(mut s) = overlay().state.lock() {
        s.labels_enabled = on;
    }
    save_prefs();
}

/// Apply trail configuration (mirror of Studio's wire fields).
pub fn set_trail_config(enabled: bool, ttl_ms: u32, diffuse: bool, teleport_threshold: f64) {
    if let Ok(mut s) = overlay().state.lock() {
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
    }
    save_prefs();
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
            let Some((k, v)) = line.split_once('=') else { continue };
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
                "mode" => s.cfg.mode = if v.eq_ignore_ascii_case("diffuse") {
                    TrailMode::Diffuse
                } else {
                    TrailMode::Line
                },
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
    if !o.enabled.load(Ordering::Relaxed) || res_x == 0 || res_y == 0 {
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
    if b.len() >= 2 && matches!(b[0], b'a' | b'A' | b'v' | b'V') && matches!(b[1], b'_' | b':' | b'-')
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
    let cap = ((ttl_s * DIFFUSE_DOTS_PER_S).round() as usize).clamp(DIFFUSE_MIN_DOTS, DIFFUSE_MAX_DOTS);
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
    let band_h_frac = ((res_x / res_y) / CINEMA_ASPECT).min(1.0);
    let depth_span = 1.0 - band_h_frac;
    let base_radius = (res_y * BASE_RADIUS_RATIO).max(8.0);
    let label_fs = (res_y * LABEL_FONT_RATIO).round().max(12.0);

    let mut out: Vec<String> = Vec::new();
    out.push(build_wireframe(cx, cy, res_x, res_y, depth_span));

    // Snapshot the scene so we can mutate trails while iterating.
    let positions = s.positions.clone();
    let mut n_obj = 0;
    for &(id, x, y, z) in &positions {
        let (r, g, b) = object_color(id, s.tags.get(&id).copied());
        let col = ass_color(r, g, b);
        n_obj += 1;

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
                            t, cx, cy, res_x, res_y, depth_span, s.cfg.ttl_s, now, base_radius,
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
        set_labels_enabled(true);
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

    #[test]
    fn zero_resolution_returns_empty() {
        let _g = guard();
        assert!(build_ass(0, 0).is_empty());
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
        let depth_span = 1.0 - ((res_x / res_y) / CINEMA_ASPECT).min(1.0);
        let ttl = 30.0;
        let now = (n as f64 - 1.0) * 0.08;
        let evt = build_trail_diffuse(
            t, res_x / 2.0, res_y / 2.0, res_x, res_y, depth_span, ttl, now, 16.0, "&H0000FF&",
        )
        .expect("long diffuse trail renders");

        let last = t.points.last().unwrap();
        let (hx, hy, _) = project(last.x, last.y, last.z, res_x / 2.0, res_y / 2.0, res_x, res_y, depth_span);
        assert!(
            evt.contains(&format!("\\pos({:.1},{:.1})", hx, hy)),
            "the head dot (newest point) is always drawn"
        );
        // Particle count stays within the (TTL-scaled) budget ceiling.
        let cap = ((ttl * DIFFUSE_DOTS_PER_S).round() as usize).clamp(DIFFUSE_MIN_DOTS, DIFFUSE_MAX_DOTS);
        assert!(evt.matches("\\p1").count() <= cap);
    }

    #[test]
    fn prefs_round_trip_via_file() {
        let _g = guard();
        let mut path = std::env::temp_dir();
        path.push(format!("omniphony-overlay-test-{}.conf", std::process::id()));

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
}
