//! # Scriptable render backend (Lua)
//!
//! A [`GainModel`] whose per-position gain function is written in **Lua** and
//! editable by the user. It depends on `renderer` through its **public API
//! only** (like `example_backend`), so it doubles as proof that a non-trivial
//! backend can live out-of-tree.
//!
//! ## Why a scripting language is viable here
//!
//! A backend is geometry-only and its `compute_gains` is invoked *only while the
//! precomputed gain table is sampled* (the host runs it once per grid point on a
//! build thread), never per audio sample. So the cost of crossing into an
//! embedded interpreter is paid once, at table-build time. Accordingly the
//! backend is **not** hot-path safe: [`ScriptFactory::realtime_capable`] returns
//! `false` and [`ScriptBackend::capabilities`] sets `supports_realtime = false`,
//! so the host forces a precomputed evaluation mode.
//!
//! ## The Lua contract
//!
//! ```lua
//! -- REQUIRED: one gain per speaker for a position.
//! function gains(pos, speakers, state, params)
//!   -- pos      = { x=, y=, z= }            (raw ADM position)
//!   -- speakers = { {x=,y=,z=}, ... }       (unit speaker directions)
//!   -- state    = value returned by setup(), or nil
//!   -- params   = { key = number, ... }     (resolved from the schema below)
//!   -- return an array of #speakers finite numbers, in speaker order.
//! end
//!
//! -- OPTIONAL: one-time per-VM setup; its return value is passed as `state`.
//! function setup(speakers, params) return {} end
//!
//! -- OPTIONAL: declare tunable params so Studio renders controls for them.
//! function params()
//!   return {
//!     { key = "falloff", label = "Falloff", min = 0.0, max = 1.0,
//!       step = 0.01, default = 0.1, help = "Softening near a speaker" },
//!   }
//! end
//! ```
//!
//! Distance attenuation and distance-diffuse are applied by the host *around*
//! this model, so the script only writes the directional panning law.
//!
//! ## Engine helpers (globals)
//!
//! The VM injects engine-provided helpers a script can call:
//!
//! ```lua
//! -- Full-layout VBAP gains for a position (one per speaker, speaker order).
//! local g = vbap(pos)              -- or vbap(pos, 0.2); also vbap:gains(pos)
//!
//! -- Build your own VBAP over a chosen speaker SUBSET (e.g. in setup), and use it
//! -- in gains. Returns gains in the subset's order, so map them back yourself.
//! function setup(speakers, params)
//!   local subset, map = {}, {}
//!   for i, s in ipairs(speakers) do
//!     if want(s) then subset[#subset+1] = s; map[#map+1] = i end
//!   end
//!   return { v = vbap_new(subset), map = map, n = #speakers }
//! end
//! function gains(pos, speakers, state, params)
//!   local sub, out = state.v:gains(pos), {}
//!   for i = 1, state.n do out[i] = 0.0 end
//!   for k, gain in ipairs(sub) do out[state.map[k]] = gain end
//!   return out
//! end
//!
//! -- Scale a gain array to unit energy (constant power); all-zero → equal power.
//! return normalize_energy(g)
//! ```
//!
//! So the smallest useful script is `return normalize_energy(vbap(pos))`. A VBAP
//! object (from `vbap` or `vbap_new`) is callable (`v(pos)`) and has `v:gains(pos)`
//! and `v:count()`.
//!
//! ### Coordinates
//!
//! `pos` is the **raw** ADM **real cartesian** position (`{x,y,z}`, actual
//! distance); each `speakers[i]` is a **unit (normalized) cartesian** direction
//! (distance 1). ADM axes: X right(+), Y front(+), Z up(+); azimuth 0° = front,
//! +90° = right; elevation 0° = level, +90° = up. Conversion helpers (angles in
//! degrees):
//!
//! ```lua
//! local q = polar(pos)            -- real polar      { az=, el=, dist= }
//! q.dist = 1                       -- normalized polar (az/el unchanged)
//! local d = normalize(pos)        -- normalized cartesian { x=, y=, z= }
//! local p = cartesian(q)          -- cartesian from polar ({az,el,dist?}); dist→1
//! local r = distance(pos)         -- radius |pos|
//! ```
//!
//! **Room ratios:** the conversions above are pure geometry on the value you pass
//! and do *not* apply the room warp. `vbap`/`vbap_new` already warp internally (so
//! they match the built-in VBAP backend); for a custom law that needs the same
//! room-relative space, warp first with `room_scale(pos)` — e.g. `polar(room_scale(pos))`.
//!
//! ## Sandbox
//!
//! Each VM exposes only `math`/`table`/`string` (no `io`/`os`/`require`/
//! `debug`), is memory-capped, and each call is bounded by an instruction
//! budget so a runaway script fails the build instead of hanging it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow, bail};
use mlua::{
    Function, HookTriggers, Lua, LuaOptions, MetaMethod, StdLib, Table, UserData, UserDataMethods,
    Value, VmState,
};

use renderer::backend_params::{ParamSpec, ParamValue};
use renderer::backend_registry::{
    BackendBuildCtx, BackendBuildPlan, BackendFactory, DynamicBackendPlan,
};
use renderer::render_backend::{
    BackendCapabilities, GainModel, RenderRequest, RenderResponse, room_scaled_position,
};
use renderer::spatial_vbap::{Gains, VbapPanner, adm_to_spherical, spherical_to_adm};
use renderer::speaker_layout::SpeakerLayout;

/// Per-VM heap cap: generous for honest scripts, low enough that a runaway
/// allocation aborts the build instead of exhausting the machine.
const MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
/// Instruction budget per `gains` call. A normal call is a few thousand
/// instructions; this only trips on an infinite loop.
const INSTRUCTION_BUDGET: u64 = 5_000_000;
/// How often the debug hook fires (VM instructions).
const HOOK_INTERVAL: u32 = 4096;
/// Param key holding the path to the `.lua` file.
const PATH_KEY: &str = "path";

static SCRIPT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    SCRIPT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// `mlua::Error` is not `Sync` without the `send` feature (the VM is kept
/// thread-local on purpose), so it can't flow through `?` into `anyhow`.
fn lua_err(e: mlua::Error) -> anyhow::Error {
    anyhow!("{e}")
}

// ===========================================================================
// The gain model
// ===========================================================================

/// A Lua-scripted gain model. `Send + Sync`: it holds only plain data; the
/// non-`Send` [`Lua`] VM lives in a `thread_local` cache keyed by `generation`,
/// so the host's parallel sampling gets one VM per worker with no locking.
pub struct ScriptBackend {
    source: Arc<str>,
    /// Unit speaker direction vectors, one per spatializable speaker.
    speakers: Vec<[f32; 3]>,
    /// Resolved numeric params handed to the script as a Lua table.
    params: Vec<(String, f64)>,
    /// The engine's VBAP panner for this layout, exposed to the script as the
    /// `vbap(pos)` helper. `None` when the geometry can't be triangulated (fewer
    /// than 3 spatializable speakers, or a degenerate set); `vbap` then errors.
    /// Shared (`Arc`) so each per-thread VM closure can hold it cheaply.
    panner: Option<Arc<VbapPanner>>,
    generation: u64,
}

impl ScriptBackend {
    /// Build and **eagerly validate** the backend: compile the source, run
    /// `setup`, and probe `gains` once so a broken script fails the build with a
    /// precise message instead of mid-sampling.
    pub fn new(
        source: impl Into<Arc<str>>,
        speakers: Vec<[f32; 3]>,
        params: Vec<(String, f64)>,
        panner: Option<Arc<VbapPanner>>,
    ) -> Result<Self> {
        let backend = Self {
            source: source.into(),
            speakers,
            params,
            panner,
            generation: next_generation(),
        };
        // Eager smoke: compile + setup + one probe call.
        let vm = backend.build_vm()?;
        vm.eval_gains([0.0, 0.0, 0.0], RoomParams::default())
            .map_err(|e| anyhow!("script `gains` failed on probe: {e}"))?;
        Ok(backend)
    }

    fn build_vm(&self) -> Result<ScriptVm> {
        ScriptVm::new(
            &self.source,
            &self.speakers,
            &self.params,
            self.panner.clone(),
        )
    }
}

thread_local! {
    static VM_CACHE: RefCell<Option<CachedVm>> = const { RefCell::new(None) };
    static INSTRUCTIONS: Cell<u64> = const { Cell::new(0) };
}

struct CachedVm {
    generation: u64,
    vm: ScriptVm,
}

impl GainModel for ScriptBackend {
    fn backend_id(&self) -> &'static str {
        "script"
    }

    fn backend_label(&self) -> &'static str {
        "Script"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            // Realtime would call Lua per audio sample — forbidden.
            supports_realtime: false,
            supports_precomputed_polar: true,
            supports_precomputed_cartesian: true,
            supports_position_interpolation: true,
            // Distance attenuation / diffuse are applied by the host decorators.
            supports_distance_model: true,
            supports_spread: false,
            supports_spread_from_distance: false,
            supports_event_size: false,
            supports_distance_diffuse: true,
            supports_table_export: true,
        }
    }

    fn speaker_count(&self) -> usize {
        self.speakers.len()
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let pos = [
            req.adm_position[0] as f32,
            req.adm_position[1] as f32,
            req.adm_position[2] as f32,
        ];
        let result = VM_CACHE.with(|cell| -> Result<Gains> {
            let mut slot = cell.borrow_mut();
            if slot.as_ref().map(|c| c.generation) != Some(self.generation) {
                *slot = Some(CachedVm {
                    generation: self.generation,
                    vm: self.build_vm()?,
                });
            }
            let vm = &slot.as_ref().expect("vm just inserted").vm;
            let values = vm.eval_gains(pos, RoomParams::from_request(req))?;
            let mut gains = Gains::zeroed(self.speakers.len());
            for (i, g) in values.into_iter().enumerate() {
                gains.set(i, g);
            }
            Ok(gains)
        });

        match result {
            Ok(gains) => RenderResponse { gains },
            Err(_) => {
                // A sampling-time error: emit non-finite gains so the host's
                // build-time smoke test rejects this backend (the eager probe in
                // `new` already caught the common cases with a precise message).
                let mut gains = Gains::zeroed(self.speakers.len());
                for i in 0..self.speakers.len() {
                    gains.set(i, f32::NAN);
                }
                RenderResponse { gains }
            }
        }
    }

    fn save_to_file(&self, _path: &std::path::Path, _layout: &SpeakerLayout) -> Result<()> {
        bail!("the scriptable backend does not support table export")
    }
}

// ===========================================================================
// The VM
// ===========================================================================

/// The room-ratio warp for the current request, shared into the engine helpers
/// so `vbap`/`room_scale` map a position into the same room-relative space the
/// built-in backends pan in. Constant within a table build; set per call from the
/// request. Default is identity (no warp).
#[derive(Clone, Copy)]
struct RoomParams {
    ratio: [f32; 3],
    rear: f32,
    lower: f32,
    center_blend: f32,
}

impl Default for RoomParams {
    fn default() -> Self {
        Self {
            ratio: [1.0; 3],
            rear: 1.0,
            lower: 1.0,
            center_blend: 0.5,
        }
    }
}

impl RoomParams {
    fn from_request(req: &RenderRequest) -> Self {
        Self {
            ratio: req.room_ratio,
            rear: req.room_ratio_rear,
            lower: req.room_ratio_lower,
            center_blend: req.room_ratio_center_blend,
        }
    }

    fn scale(&self, p: [f32; 3]) -> [f32; 3] {
        room_scaled_position(p, self.ratio, self.rear, self.lower, self.center_blend)
    }
}

/// Shared, per-thread handle to the current room warp (the VM is thread-local).
type RoomCell = Rc<Cell<RoomParams>>;

struct ScriptVm {
    _lua: Lua,
    gains_fn: Function,
    speaker_count: usize,
    speakers_tbl: Table,
    state: Value,
    params_tbl: Table,
    pos_tbl: Table,
    /// Updated each call from the request; read by the engine helpers.
    room: RoomCell,
}

impl ScriptVm {
    fn new(
        source: &str,
        speakers: &[[f32; 3]],
        params: &[(String, f64)],
        panner: Option<Arc<VbapPanner>>,
    ) -> Result<Self> {
        let lua = sandboxed_lua()?;
        // The room warp is shared into the engine helpers and refreshed per call.
        let room: RoomCell = Rc::new(Cell::new(RoomParams::default()));
        // Register the engine-provided helpers (`vbap`, `room_scale`, conversions,
        // `normalize_energy`) before executing the chunk so top-level code may use
        // them too.
        register_engine_api(&lua, panner, room.clone()).map_err(lua_err)?;
        lua.load(source)
            .exec()
            .map_err(|e| anyhow!("failed to load script: {e}"))?;
        let globals = lua.globals();

        let speakers_tbl = speakers_table(&lua, speakers).map_err(lua_err)?;
        let params_tbl = params_table(&lua, params).map_err(lua_err)?;

        let state = match globals.get::<Value>("setup").map_err(lua_err)? {
            Value::Function(setup) => {
                reset_instructions();
                setup
                    .call((speakers_tbl.clone(), params_tbl.clone()))
                    .map_err(|e| anyhow!("script `setup` failed: {e}"))?
            }
            Value::Nil => Value::Nil,
            other => bail!("`setup` must be a function, got {}", other.type_name()),
        };

        let gains_fn = match globals.get::<Value>("gains").map_err(lua_err)? {
            Value::Function(f) => f,
            Value::Nil => {
                bail!("script must define a `gains(pos, speakers, state, params)` function")
            }
            other => bail!("`gains` must be a function, got {}", other.type_name()),
        };

        let pos_tbl = lua.create_table().map_err(lua_err)?;

        Ok(Self {
            _lua: lua,
            gains_fn,
            speaker_count: speakers.len(),
            speakers_tbl,
            state,
            params_tbl,
            pos_tbl,
            room,
        })
    }

    fn eval_gains(&self, pos: [f32; 3], room: RoomParams) -> Result<Vec<f32>> {
        // Publish the current room warp for the engine helpers, then pass the raw
        // position to the script (it can warp via `room_scale` / `vbap`).
        self.room.set(room);
        self.pos_tbl.set("x", pos[0]).map_err(lua_err)?;
        self.pos_tbl.set("y", pos[1]).map_err(lua_err)?;
        self.pos_tbl.set("z", pos[2]).map_err(lua_err)?;

        reset_instructions();
        let result: Table = self
            .gains_fn
            .call((
                self.pos_tbl.clone(),
                self.speakers_tbl.clone(),
                self.state.clone(),
                self.params_tbl.clone(),
            ))
            .map_err(lua_err)?;

        let len = result.raw_len();
        if len != self.speaker_count {
            bail!(
                "`gains` returned {len} values but the layout has {} speakers",
                self.speaker_count
            );
        }
        let mut out = Vec::with_capacity(self.speaker_count);
        for i in 1..=self.speaker_count {
            let g: f32 = result.get(i).map_err(lua_err)?;
            if !g.is_finite() {
                bail!("`gains[{i}]` is not finite ({g})");
            }
            out.push(g);
        }
        Ok(out)
    }
}

fn sandboxed_lua() -> Result<Lua> {
    let lua = Lua::new_with(
        StdLib::MATH | StdLib::TABLE | StdLib::STRING,
        LuaOptions::default(),
    )
    .map_err(|e| anyhow!("failed to create Lua VM: {e}"))?;
    let _ = lua.set_memory_limit(MEMORY_LIMIT_BYTES);
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(HOOK_INTERVAL),
        |_lua, _debug| {
            let used = INSTRUCTIONS.with(|c| {
                let v = c.get() + HOOK_INTERVAL as u64;
                c.set(v);
                v
            });
            if used > INSTRUCTION_BUDGET {
                Err(mlua::Error::RuntimeError(
                    "script exceeded its instruction budget (possible infinite loop)".into(),
                ))
            } else {
                Ok(VmState::Continue)
            }
        },
    );
    Ok(lua)
}

fn reset_instructions() {
    INSTRUCTIONS.with(|c| c.set(0));
}

/// A VBAP panner exposed to Lua. Callable as `v(pos [, spread])` and via
/// `v:gains(pos [, spread])` (both return one gain per speaker, in the speaker
/// order the panner was built with), with `v:count()` for the speaker count.
/// Built either for the full layout (the global `vbap`) or by the script from a
/// chosen speaker subset via `vbap_new(speakers)`.
struct LuaVbap {
    panner: Arc<VbapPanner>,
    room: RoomCell,
}

impl LuaVbap {
    fn gains_table(&self, lua: &Lua, pos: &Table, spread: Option<f32>) -> mlua::Result<Table> {
        let x: f32 = pos.get("x")?;
        let y: f32 = pos.get("y")?;
        let z: f32 = pos.get("z")?;
        let spread = spread.unwrap_or(0.0).clamp(0.0, 1.0);
        // Warp the (raw) position by the current room ratios first, exactly like
        // the built-in VBAP backend, so `vbap(pos)` matches it.
        let [sx, sy, sz] = self.room.get().scale([x, y, z]);
        let gains = self.panner.get_gains_cartesian(sx, sy, sz, spread);
        let out = lua.create_table_with_capacity(gains.len(), 0)?;
        for (i, g) in gains.iter().enumerate() {
            out.set(i + 1, *g)?;
        }
        Ok(out)
    }
}

impl UserData for LuaVbap {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("gains", |lua, this, (pos, spread): (Table, Option<f32>)| {
            this.gains_table(lua, &pos, spread)
        });
        methods.add_method("count", |_, this, ()| Ok(this.panner.num_speakers()));
        // `v(pos)` is sugar for `v:gains(pos)`.
        methods.add_meta_method(
            MetaMethod::Call,
            |lua, this, (pos, spread): (Table, Option<f32>)| this.gains_table(lua, &pos, spread),
        );
    }
}

/// Build a VBAP panner from a Lua array of speaker entries (`{ x=, y=, z= }` unit
/// directions, e.g. a subset of the `speakers` table), converting each to the
/// azimuth/elevation the engine panner needs. Shares the VM's room warp so its
/// `:gains` is room-aware like the global `vbap`.
fn build_lua_vbap(speakers: &Table, room: RoomCell) -> mlua::Result<LuaVbap> {
    let n = speakers.raw_len();
    let mut az_el = Vec::with_capacity(n);
    for i in 1..=n {
        let s: Table = speakers.get(i)?;
        let x: f32 = s.get("x")?;
        let y: f32 = s.get("y")?;
        let z: f32 = s.get("z")?;
        let (az, el, _dist) = adm_to_spherical(x, y, z);
        az_el.push([az, el]);
    }
    let panner = VbapPanner::new(&az_el, 5, 5, 0.0, Default::default())
        .map_err(|e| mlua::Error::RuntimeError(format!("vbap_new: {e}")))?;
    Ok(LuaVbap {
        panner: Arc::new(panner),
        room,
    })
}

/// Inject the engine-provided helpers as Lua globals:
///
/// - `vbap` — a VBAP object for the full layout (`vbap(pos [, spread])` or
///   `vbap:gains(pos [, spread])`), one gain per speaker in speaker order. It is a
///   function that errors with a clear message when the layout cannot be
///   triangulated (fewer than 3 spatializable speakers, or a degenerate set).
/// - `vbap_new(speakers)` — build a VBAP object from a chosen list of speaker
///   directions (e.g. a subset selected in `setup`); returns gains in *that* list's
///   order. Raises a clear error if the subset cannot be triangulated.
/// - `room_scale(p)` — warp a cartesian point by the current room ratios into the
///   room-relative space the backends pan in (the same warp `vbap` applies).
/// - `normalize_energy(out)` — return a copy of the gain array scaled to unit
///   energy (constant power); an all-zero input falls back to equal power so the
///   script never emits silence or non-finite gains.
fn register_engine_api(
    lua: &Lua,
    panner: Option<Arc<VbapPanner>>,
    room: RoomCell,
) -> mlua::Result<()> {
    let globals = lua.globals();

    // Constructor for a script-built VBAP over any speaker subset; it shares the
    // VM's room warp so its `:gains` is room-aware too.
    let room_for_new = room.clone();
    let vbap_new = lua.create_function(move |lua, speakers: Table| {
        let v = build_lua_vbap(&speakers, room_for_new.clone())?;
        lua.create_userdata(v)
    })?;
    globals.set("vbap_new", vbap_new)?;

    // Full-layout convenience object, or an erroring stub when not triangulable so
    // `vbap(pos)` still fails with a precise message at build time.
    match panner {
        Some(panner) => globals.set(
            "vbap",
            lua.create_userdata(LuaVbap {
                panner,
                room: room.clone(),
            })?,
        )?,
        None => {
            let unavailable =
                lua.create_function(|_, _: mlua::MultiValue| -> mlua::Result<Table> {
                    Err(mlua::Error::RuntimeError(
                        "vbap is unavailable: the layout has fewer than 3 spatializable \
                     speakers or could not be triangulated (use vbap_new(subset) to \
                     build one from a triangulable subset)"
                            .into(),
                    ))
                })?;
            globals.set("vbap", unavailable)?;
        }
    }

    let normalize = lua.create_function(|lua, gains: Table| {
        let n = gains.raw_len();
        let mut values = Vec::with_capacity(n);
        let mut sum_sq = 0.0f64;
        for i in 1..=n {
            let v: f64 = gains.get(i)?;
            sum_sq += v * v;
            values.push(v);
        }
        let out = lua.create_table_with_capacity(n, 0)?;
        if sum_sq > f64::EPSILON {
            let inv = 1.0 / sum_sq.sqrt();
            for (i, v) in values.iter().enumerate() {
                out.set(i + 1, v * inv)?;
            }
        } else if n > 0 {
            // Nothing won any gain — fall back to equal power instead of silence.
            let eq = (1.0 / n as f64).sqrt();
            for i in 1..=n {
                out.set(i, eq)?;
            }
        }
        Ok(out)
    })?;
    globals.set("normalize_energy", normalize)?;

    // Warp a cartesian point by the current room ratios (the room-relative "effect
    // space" the backends pan in). `vbap` applies this internally; a custom law
    // calls it explicitly before its own geometry/conversions.
    let room_for_scale = room.clone();
    let room_scale = lua.create_function(move |lua, p: Table| {
        let (x, y, z) = read_xyz(&p)?;
        let [sx, sy, sz] = room_for_scale.get().scale([x, y, z]);
        let out = lua.create_table_with_capacity(0, 3)?;
        out.set("x", sx)?;
        out.set("y", sy)?;
        out.set("z", sz)?;
        Ok(out)
    })?;
    globals.set("room_scale", room_scale)?;

    // ── Coordinate conversions (ADM; angles in degrees) ───────────────────────
    // ADM axes: X right(+)/left(-), Y front(+)/back(-), Z up(+)/down(-). Azimuth
    // 0° = front (+Y), +90° = right (+X); elevation 0° = horizontal, +90° = up.
    // `pos` arrives as real cartesian; `speakers[i]` as unit (normalized) cartesian.

    // Real polar of a cartesian point: { x=, y=, z= } -> { az=, el=, dist= }.
    // Normalized polar is the same with dist = 1 (az/el are unchanged).
    let polar = lua.create_function(|lua, p: Table| {
        let (x, y, z) = read_xyz(&p)?;
        let (az, el, dist) = adm_to_spherical(x, y, z);
        let out = lua.create_table_with_capacity(0, 3)?;
        out.set("az", az)?;
        out.set("el", el)?;
        out.set("dist", dist)?;
        Ok(out)
    })?;
    globals.set("polar", polar)?;

    // Cartesian from polar: { az=, el=, dist?= } -> { x=, y=, z= }. `dist` defaults
    // to 1, so `cartesian({ az=, el= })` is the unit (normalized) cartesian
    // direction, and `cartesian(polar(p))` round-trips to `p`.
    let cartesian = lua.create_function(|lua, s: Table| {
        let az: f32 = s.get("az")?;
        let el: f32 = s.get("el")?;
        let dist: f32 = s.get::<Option<f32>>("dist")?.unwrap_or(1.0);
        let (x, y, z) = spherical_to_adm(az, el, dist);
        let out = lua.create_table_with_capacity(0, 3)?;
        out.set("x", x)?;
        out.set("y", y)?;
        out.set("z", z)?;
        Ok(out)
    })?;
    globals.set("cartesian", cartesian)?;

    // Unit (normalized) cartesian direction of a point; the zero vector maps to
    // the zero vector (no usable direction).
    let normalize_vec = lua.create_function(|lua, p: Table| {
        let (x, y, z) = read_xyz(&p)?;
        let len = (x * x + y * y + z * z).sqrt();
        let out = lua.create_table_with_capacity(0, 3)?;
        let (nx, ny, nz) = if len > f32::EPSILON {
            (x / len, y / len, z / len)
        } else {
            (0.0, 0.0, 0.0)
        };
        out.set("x", nx)?;
        out.set("y", ny)?;
        out.set("z", nz)?;
        Ok(out)
    })?;
    globals.set("normalize", normalize_vec)?;

    // Distance (radius) of a cartesian point.
    let distance = lua.create_function(|_, p: Table| {
        let (x, y, z) = read_xyz(&p)?;
        Ok((x * x + y * y + z * z).sqrt())
    })?;
    globals.set("distance", distance)?;

    Ok(())
}

/// Read `{ x=, y=, z= }` from a Lua table as `f32`s.
fn read_xyz(t: &Table) -> mlua::Result<(f32, f32, f32)> {
    let x: f32 = t.get("x")?;
    let y: f32 = t.get("y")?;
    let z: f32 = t.get("z")?;
    Ok((x, y, z))
}

fn speakers_table(lua: &Lua, speakers: &[[f32; 3]]) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(speakers.len(), 0)?;
    for (i, s) in speakers.iter().enumerate() {
        let entry = lua.create_table_with_capacity(0, 3)?;
        entry.set("x", s[0])?;
        entry.set("y", s[1])?;
        entry.set("z", s[2])?;
        table.set(i + 1, entry)?;
    }
    Ok(table)
}

fn params_table(lua: &Lua, params: &[(String, f64)]) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(0, params.len())?;
    for (key, value) in params {
        table.set(key.as_str(), *value)?;
    }
    Ok(table)
}

// ===========================================================================
// The factory
// ===========================================================================

/// Registers [`ScriptBackend`] under the id `"script"`.
pub struct ScriptFactory;

impl BackendFactory for ScriptFactory {
    fn id(&self) -> &'static str {
        "script"
    }

    fn label(&self) -> &'static str {
        "Script"
    }

    fn realtime_capable(&self) -> bool {
        false
    }

    fn param_schema(&self) -> Vec<ParamSpec> {
        vec![
            // Editable file: the UI offers Browse (when the renderer is local) and
            // a Lua editor. The value is a handle resolved to an absolute renderer
            // path by the host before this backend reads it.
            ParamSpec::file(PATH_KEY, "Script file", "", true, Some("lua"), vec!["lua"])
                .help("A .lua file implementing gains(pos, speakers, state, params)."),
        ]
    }

    fn param_schema_for(
        &self,
        params: &std::collections::HashMap<String, ParamValue>,
    ) -> Vec<ParamSpec> {
        let mut schema = self.param_schema();
        // If a file is selected, ask it which params it declares and surface
        // those controls too.
        if let Some(path) = params.get(PATH_KEY).and_then(ParamValue::as_str) {
            if !path.trim().is_empty() {
                if let Ok(source) = std::fs::read_to_string(path) {
                    schema.extend(script_declared_params(&source));
                }
            }
        }
        schema
    }

    fn build_plan(&self, ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan> {
        // Unit speaker directions from the layout (captured on the build thread).
        let (azimuth_elevation, _) = ctx.layout.spatializable_positions();
        let speakers: Vec<[f32; 3]> = azimuth_elevation
            .iter()
            .map(|[az, el]| {
                let (x, y, z) = spherical_to_adm(*az, *el, 1.0);
                [x, y, z]
            })
            .collect();

        // The engine VBAP panner the script can call via `vbap(pos)`. Built from
        // the same speaker order as the script's gains, so its output lines up.
        // Resolution is irrelevant (gains are computed directly from the
        // triangulation); `None` if the geometry can't be triangulated.
        let panner = VbapPanner::new(&azimuth_elevation, 5, 5, 0.0, Default::default())
            .ok()
            .map(Arc::new);

        let path = ctx
            .backend_param(self.id(), PATH_KEY)
            .and_then(ParamValue::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        // Resolve the path → source, and the declared params → values, now. A
        // missing/unreadable file produces a builder that fails with a clear
        // message (surfaced via the recompute error banner), rather than `None`
        // (which would silently keep the previous backend).
        let load = (!path.is_empty())
            .then(|| std::fs::read_to_string(&path))
            .transpose();

        let (source, params) = match load {
            Ok(Some(source)) => {
                let values = script_declared_params(&source)
                    .into_iter()
                    .filter_map(|spec| {
                        let v = ctx
                            .backend_param(self.id(), spec.key)
                            .and_then(ParamValue::as_f32)
                            .or_else(|| spec.default.as_f32())?;
                        Some((spec.key.to_string(), v as f64))
                    })
                    .collect::<Vec<_>>();
                (Some(source), values)
            }
            Ok(None) => (None, Vec::new()),
            Err(e) => {
                let msg = format!("script backend: cannot read '{path}': {e}");
                return Some(BackendBuildPlan::Dynamic(DynamicBackendPlan::new(
                    "script",
                    move || bail!("{msg}"),
                )));
            }
        };

        let builder_msg = "script backend: no script file selected".to_string();
        Some(BackendBuildPlan::Dynamic(DynamicBackendPlan::new(
            "script",
            move || match &source {
                Some(source) => Ok(Box::new(ScriptBackend::new(
                    source.as_str(),
                    speakers.clone(),
                    params.clone(),
                    panner.clone(),
                )?) as Box<dyn GainModel>),
                None => bail!("{builder_msg}"),
            },
        )))
    }
}

/// Run a script's optional `params()` function in a throwaway sandboxed VM and
/// map its descriptors to [`ParamSpec`]s. Any failure yields an empty list (the
/// backend still works with the script's own defaults).
fn script_declared_params(source: &str) -> Vec<ParamSpec> {
    fn try_read(source: &str) -> Result<Vec<ParamSpec>> {
        let lua = sandboxed_lua()?;
        lua.load(source).exec().map_err(lua_err)?;
        let params_fn = match lua.globals().get::<Value>("params").map_err(lua_err)? {
            Value::Function(f) => f,
            _ => return Ok(Vec::new()),
        };
        reset_instructions();
        let list: Table = params_fn.call(()).map_err(lua_err)?;
        let mut specs = Vec::new();
        for entry in list.sequence_values::<Table>() {
            let entry = entry.map_err(lua_err)?;
            let key: String = entry.get("key").map_err(lua_err)?;
            // `ParamSpec` needs a `&'static str` key; leak the (few, build-time)
            // script-declared keys to satisfy that without reworking the schema.
            let key: &'static str = Box::leak(key.into_boxed_str());
            let label: String = entry.get("label").unwrap_or_else(|_| key.to_string());
            let label: &'static str = Box::leak(label.into_boxed_str());
            let min: f32 = entry.get("min").unwrap_or(0.0);
            let max: f32 = entry.get("max").unwrap_or(1.0);
            let step: f32 = entry.get("step").unwrap_or(0.01);
            let default: f32 = entry.get("default").unwrap_or(0.0);
            let mut spec = ParamSpec::float(key, label, min, max, step, default);
            if let Ok(help) = entry.get::<String>("help") {
                let help: &'static str = Box::leak(help.into_boxed_str());
                spec = spec.help(help);
            }
            specs.push(spec);
        }
        Ok(specs)
    }
    try_read(source).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::backend_params::ParamKind;

    const NEAREST: &str = r#"
        function gains(pos, speakers, state, params)
          local out, best, bestd = {}, 1, math.huge
          for i = 1, #speakers do
            out[i] = 0.0
            local s = speakers[i]
            local dx, dy, dz = pos.x - s.x, pos.y - s.y, pos.z - s.z
            local d = dx*dx + dy*dy + dz*dz
            if d < bestd then bestd, best = d, i end
          end
          out[best] = 1.0
          return out
        end
    "#;

    fn speakers() -> Vec<[f32; 3]> {
        vec![
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ]
    }

    fn request(pos: [f64; 3]) -> RenderRequest {
        RenderRequest {
            adm_position: pos,
            event_size: [0.0; 3],
            room_ratio: [1.0, 1.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.5,
            use_distance_diffuse: false,
            diffuse_mirror_axes: renderer::spatial_vbap::MirrorAxes::default(),
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            distance_model: Default::default(),
        }
    }

    fn backend(src: &str) -> Result<ScriptBackend> {
        ScriptBackend::new(src, speakers(), Vec::new(), None)
    }

    /// A non-coplanar layout (4 base speakers + 1 overhead) that triangulates, so
    /// the `vbap` helper is available. Returns the az/el pairs and the matching
    /// unit directions in the same speaker order.
    fn layout_3d() -> (Vec<[f32; 2]>, Vec<[f32; 3]>) {
        let az_el = vec![
            [-30.0, 0.0],
            [30.0, 0.0],
            [-110.0, 0.0],
            [110.0, 0.0],
            [0.0, 45.0],
        ];
        let dirs = az_el
            .iter()
            .map(|[az, el]| {
                let (x, y, z) = spherical_to_adm(*az, *el, 1.0);
                [x, y, z]
            })
            .collect();
        (az_el, dirs)
    }

    fn panner_for(az_el: &[[f32; 2]]) -> Option<Arc<VbapPanner>> {
        VbapPanner::new(az_el, 5, 5, 0.0, Default::default())
            .ok()
            .map(Arc::new)
    }

    #[test]
    fn nearest_script_selects_closest() {
        let model = backend(NEAREST).expect("valid script");
        let gains = model.compute_gains(&request([0.9, 0.0, 0.0])).gains;
        assert_eq!(gains.len(), 4);
        assert_eq!(gains[1], 1.0);
        assert!(gains.iter().all(|g| g.is_finite()));
    }

    #[test]
    fn params_and_setup_reach_the_script() {
        let src = r#"
            function setup(speakers, params) return { n = #speakers } end
            function gains(pos, speakers, state, params)
              local out = {}
              for i = 1, #speakers do out[i] = (params.level or 0.0) + state.n end
              return out
            end
        "#;
        let model = ScriptBackend::new(src, speakers(), vec![("level".into(), 0.25)], None)
            .expect("valid script");
        let gains = model.compute_gains(&request([0.0, 0.0, 0.0])).gains;
        assert!(gains.iter().all(|g| (*g - 4.25).abs() < 1e-6));
    }

    #[test]
    fn vbap_and_normalize_helpers_are_available() {
        let (az_el, dirs) = layout_3d();
        let panner = panner_for(&az_el);
        assert!(panner.is_some(), "the 3-D layout should triangulate");
        // A script that defers entirely to the engine: VBAP gains, energy-normalised.
        let src = r#"
            function gains(pos, speakers, state, params)
              return normalize_energy(vbap(pos))
            end
        "#;
        let model = ScriptBackend::new(src, dirs, Vec::new(), panner).expect("valid script");
        let gains = model.compute_gains(&request([0.3, 0.6, 0.2])).gains;
        assert_eq!(gains.len(), 5);
        assert!(gains.iter().all(|g| g.is_finite()));
        let energy: f32 = gains.iter().map(|g| g * g).sum();
        assert!((energy - 1.0).abs() < 1e-4, "normalised energy={energy}");
    }

    #[test]
    fn script_can_build_a_vbap_over_a_subset() {
        let (az_el, dirs) = layout_3d();
        // Aim straight at the overhead speaker (full index 5), which is in the subset.
        let (x, y, z) = spherical_to_adm(0.0, 45.0, 1.0);
        let src = r#"
            function setup(speakers, params)
              local subset, map = {}, {}
              for _, idx in ipairs({1, 2, 5}) do
                subset[#subset+1] = speakers[idx]
                map[#map+1] = idx
              end
              local v = vbap_new(subset)
              assert(v:count() == 3)
              return { v = v, map = map, n = #speakers }
            end
            function gains(pos, speakers, state, params)
              local sub = state.v:gains(pos)
              local out = {}
              for i = 1, state.n do out[i] = 0.0 end
              for k, g in ipairs(sub) do out[state.map[k]] = g end
              return out
            end
        "#;
        // The full-layout panner is irrelevant here — the script builds its own.
        let model =
            ScriptBackend::new(src, dirs, Vec::new(), panner_for(&az_el)).expect("valid script");
        let gains = model
            .compute_gains(&request([x as f64, y as f64, z as f64]))
            .gains;
        assert_eq!(gains.len(), 5);
        assert!(gains.iter().all(|g| g.is_finite()));
        // Speakers outside the chosen subset {1,2,5} are never touched.
        assert_eq!(gains[2], 0.0);
        assert_eq!(gains[3], 0.0);
        // Aiming at the overhead speaker (subset member, full index 5) wins it.
        assert!(gains[4] > 0.9, "overhead gain {}", gains[4]);
    }

    #[test]
    fn coordinate_helpers_convert_and_round_trip() {
        // The script exercises every coordinate helper and returns markers in the
        // gain array (length must equal #speakers = 4 here).
        let src = r#"
            function gains(pos, speakers, state, params)
              -- pos is front-up-right-ish; check polar() axis conventions.
              local front = polar({ x = 0.0, y = 2.0, z = 0.0 })   -- az 0, dist 2
              local right = polar({ x = 1.0, y = 0.0, z = 0.0 })   -- az +90
              local up    = polar({ x = 0.0, y = 0.0, z = 1.0 })   -- el +90
              -- round trip cartesian(polar(p)) == p
              local p = { x = 0.3, y = 0.5, z = -0.2 }
              local rt = cartesian(polar(p))
              -- normalized cartesian has unit length; default dist is 1.
              local n = normalize({ x = 0.0, y = 3.0, z = 0.0 })
              return {
                math.abs(front.az) + math.abs(front.dist - 2.0),
                math.abs(right.az - 90.0),
                math.abs(up.el - 90.0),
                math.abs(rt.x - p.x) + math.abs(rt.y - p.y) + math.abs(rt.z - p.z)
                  + math.abs(distance(n) - 1.0),
              }
            end
        "#;
        let model = backend(src).expect("valid script");
        let gains = model.compute_gains(&request([0.0, 0.0, 0.0])).gains;
        assert_eq!(gains.len(), 4);
        // Every marker is a near-zero error term.
        for (i, g) in gains.iter().enumerate() {
            assert!(g.abs() < 1e-3, "coordinate marker {i} off by {g}");
        }
    }

    #[test]
    fn room_scale_applies_the_request_room_ratio() {
        // `room_scale` warps by the live room ratios, matching the built-in
        // backends. With room_ratio.x = 2, x=0.5 maps to 1.0.
        let src = r#"
            function gains(pos, speakers, state, params)
              local s = room_scale({ x = 0.5, y = 0.0, z = 0.0 })
              local out = {}
              for i = 1, #speakers do out[i] = 0.0 end
              out[1] = s.x
              return out
            end
        "#;
        let model = backend(src).expect("valid script");
        let mut req = request([0.0, 0.0, 0.0]);
        req.room_ratio = [2.0, 1.0, 1.0];
        let gains = model.compute_gains(&req).gains;
        assert!(
            (gains[0] - 1.0).abs() < 1e-4,
            "room_scaled x = {}",
            gains[0]
        );
    }

    #[test]
    fn vbap_errors_clearly_when_layout_cannot_triangulate() {
        // `speakers()` is coplanar (all z=0) → no panner → vbap() must error, and
        // the eager probe in `new` surfaces it with a precise message.
        let src = r#"
            function gains(pos, speakers, state, params)
              return vbap(pos)
            end
        "#;
        let err = ScriptBackend::new(src, speakers(), Vec::new(), None)
            .err()
            .expect("script calling vbap without triangulation must fail to build");
        assert!(err.to_string().contains("vbap"), "got: {err}");
    }

    #[test]
    fn normalize_energy_falls_back_to_equal_power_for_all_zero() {
        let src = r#"
            function gains(pos, speakers, state, params)
              local out = {}
              for i = 1, #speakers do out[i] = 0.0 end
              return normalize_energy(out)
            end
        "#;
        let model = backend(src).expect("valid script");
        let gains = model.compute_gains(&request([0.0, 0.0, 0.0])).gains;
        let energy: f32 = gains.iter().map(|g| g * g).sum();
        assert!((energy - 1.0).abs() < 1e-4, "equal-power energy={energy}");
        let first = gains[0];
        assert!(gains.iter().all(|g| (g - first).abs() < 1e-6));
    }

    #[test]
    fn broken_scripts_fail_construction() {
        assert!(backend("function gains( not lua").is_err(), "syntax");
        assert!(backend("local x = 1").is_err(), "missing gains");
        assert!(
            backend("function gains(p,s,st,pa) return {1,2} end").is_err(),
            "wrong length"
        );
        assert!(
            backend("function gains(p,s,st,pa) local o={} for i=1,#s do o[i]=1/0 end return o end")
                .is_err(),
            "non-finite"
        );
    }

    #[test]
    fn sandbox_denies_dangerous_stdlib() {
        assert!(backend("function gains(p,s,st,pa) return os.time() end").is_err());
        assert!(backend("function gains(p,s,st,pa) io.write('x') return {} end").is_err());
        assert!(backend("function gains(p,s,st,pa) require('os') return {} end").is_err());
        assert!(backend("function gains(p,s,st,pa) return { debug.getinfo(1) } end").is_err());
    }

    #[test]
    fn infinite_loop_and_runaway_allocation_are_bounded() {
        assert!(backend("function gains(p,s,st,pa) while true do end end").is_err());
        assert!(
            backend(
                "function gains(p,s,st,pa) local b=string.rep('x',256*1024*1024) return {#b} end"
            )
            .is_err()
        );
    }

    #[test]
    fn sampling_time_error_yields_non_finite_for_the_smoke_test() {
        // Passes the eager probe at [0,0,0] but errors elsewhere; compute_gains
        // then returns non-finite gains so the host smoke test rejects it.
        let src = r#"
            function gains(pos, speakers, state, params)
              if pos.x < -0.5 then error("boom") end
              local out = {}
              for i = 1, #speakers do out[i] = 0.0 end
              out[1] = 1.0
              return out
            end
        "#;
        let model = backend(src).expect("eager probe at origin passes");
        let bad = model.compute_gains(&request([-1.0, 0.0, 0.0])).gains;
        assert!(bad.iter().any(|g| !g.is_finite()));
    }

    #[test]
    fn factory_declares_path_and_is_not_realtime() {
        let f = ScriptFactory;
        assert!(!f.realtime_capable());
        let schema = f.param_schema();
        assert!(matches!(
            schema[0].kind,
            ParamKind::File { editable: true, .. }
        ));
        assert_eq!(schema[0].key, "path");
    }

    #[test]
    fn script_declared_params_are_parsed() {
        let src = r#"
            function params()
              return { { key="falloff", label="Falloff", min=0.0, max=1.0, default=0.1 } }
            end
            function gains(p,s,st,pa) return {} end
        "#;
        let specs = script_declared_params(src);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].key, "falloff");
        assert!(matches!(specs[0].kind, ParamKind::Float { .. }));
        // A script without params() declares none.
        assert!(script_declared_params("function gains(p,s,st,pa) return {} end").is_empty());
    }

    #[test]
    fn shipped_example_loads_and_declares_its_params() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../script-backends/nearest_inverse_distance.lua"
        );
        let source = std::fs::read_to_string(path).expect("example script readable");
        let model = ScriptBackend::new(source.clone(), speakers(), Vec::new(), None)
            .expect("example is valid");
        let gains = model.compute_gains(&request([0.7, -0.3, 0.2])).gains;
        let energy: f32 = gains.iter().map(|g| g * g).sum();
        assert!(
            (energy - 1.0).abs() < 1e-4,
            "constant-power, energy={energy}"
        );

        let keys: Vec<&str> = script_declared_params(&source)
            .iter()
            .map(|s| s.key)
            .collect();
        assert!(keys.contains(&"falloff") && keys.contains(&"sharpness"));
    }

    #[test]
    fn shipped_vbap_blend_example_uses_the_engine_helpers() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../script-backends/vbap_blend.lua"
        );
        let source = std::fs::read_to_string(path).expect("vbap_blend script readable");
        let (az_el, dirs) = layout_3d();
        let params = vec![("spread".to_string(), 0.2)];
        let model = ScriptBackend::new(source, dirs, params, panner_for(&az_el))
            .expect("vbap_blend is valid");
        let gains = model.compute_gains(&request([0.4, 0.5, 0.3])).gains;
        assert_eq!(gains.len(), 5);
        let energy: f32 = gains.iter().map(|g| g * g).sum();
        assert!(
            (energy - 1.0).abs() < 1e-4,
            "constant-power, energy={energy}"
        );
    }

    #[test]
    fn shipped_vbap_subset_example_builds_a_subset_panner() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../script-backends/vbap_subset.lua"
        );
        let source = std::fs::read_to_string(path).expect("vbap_subset script readable");
        let (_az_el, dirs) = layout_3d();
        // floor_only = 0 → ground ring + the overhead speaker (triangulable).
        let params = vec![("floor_only".to_string(), 0.0)];
        let model = ScriptBackend::new(source, dirs, params, None).expect("vbap_subset is valid");
        let gains = model.compute_gains(&request([0.4, 0.5, 0.3])).gains;
        assert_eq!(gains.len(), 5);
        assert!(gains.iter().all(|g| g.is_finite()));
    }
}
