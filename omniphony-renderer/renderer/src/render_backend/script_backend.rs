//! User-scriptable render backend.
//!
//! A [`ScriptBackend`] evaluates a per-position gain function written in Lua.
//! Because every spatial backend is geometry-only and its `compute_gains` is
//! invoked only while a [`super::SampledPolarEvaluator`] /
//! [`super::SampledCartesianEvaluator`] fills its gain table (never per audio
//! sample), the cost of crossing into an embedded interpreter is paid once, at
//! table-build time. This makes a slow scripting language viable here — but
//! *only* in the precomputed evaluation modes. The backend therefore declares
//! `supports_realtime = false`.
//!
//! Thread-safety: the struct itself is `Send + Sync` and holds only plain data
//! (the Lua *source*, the speakers, the params and a shared error slot). The
//! non-`Send` [`mlua::Lua`] lives in a `thread_local!` cache keyed by the
//! backend's `generation`, so the rayon-parallel sampling in
//! `SampledCartesianEvaluator::new` gets one independent VM per worker thread
//! with no locking on the sampling path.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow, bail};
use mlua::{Function, HookTriggers, Lua, LuaOptions, StdLib, Table, Value, VmState};
use parking_lot::Mutex;

use super::room_transform::room_scaled_position;
use super::{BackendCapabilities, GainModel, GainModelKind, RenderRequest, RenderResponse};
use crate::spatial_vbap::Gains;
use crate::speaker_layout::SpeakerLayout;

/// Per-VM heap cap. Generous enough for honest scripts, low enough that a
/// runaway allocation aborts the build instead of exhausting the machine.
const MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
/// Instruction budget enforced *per `gains` call*. A normal call for a couple
/// dozen speakers is a few thousand instructions; this only trips on a script
/// that loops forever.
const INSTRUCTION_BUDGET: u64 = 5_000_000;
/// How often the debug hook fires (in VM instructions).
const HOOK_INTERVAL: u32 = 4096;

/// Monotonic id handed to every constructed [`ScriptBackend`]. Doubles as the
/// thread-local VM cache key, so a freshly built backend (new source, reloaded
/// file, …) never reuses a stale per-thread VM.
static SCRIPT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    SCRIPT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// `mlua::Error` is not `Sync` without the `send` feature (the VM is kept
/// thread-local on purpose), so it can't flow through `?` into `anyhow`. Render
/// it to a string-backed error instead.
fn lua_err(e: mlua::Error) -> anyhow::Error {
    anyhow!("{e}")
}

/// Owned, `Send + Sync` parameter map handed to the script as a Lua table.
/// Numeric only for now (the common case for panning maths); extend later.
#[derive(Clone, Debug, Default)]
pub struct ScriptParams(pub Vec<(String, f64)>);

/// Room transform inputs, extracted from a [`RenderRequest`]. Constant across a
/// whole table build (the evaluator freezes the request template and only
/// varies `adm_position`), so the speakers are transformed once per VM.
#[derive(Clone, Copy)]
struct RoomParams {
    ratio: [f32; 3],
    rear: f32,
    lower: f32,
    center_blend: f32,
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

    fn identity() -> Self {
        Self {
            ratio: [1.0, 1.0, 1.0],
            rear: 1.0,
            lower: 1.0,
            center_blend: 0.5,
        }
    }

    fn apply(&self, position: [f32; 3]) -> [f32; 3] {
        room_scaled_position(
            position,
            self.ratio,
            self.rear,
            self.lower,
            self.center_blend,
        )
    }
}

pub struct ScriptBackend {
    /// Lua source. Owned so the backend stays `Send + Sync`.
    source: Arc<str>,
    /// Spatializable speaker positions (raw ADM xyz). Transformed per VM.
    speakers: Vec<[f32; 3]>,
    params: ScriptParams,
    generation: u64,
    /// First error observed while sampling, surfaced to the build via
    /// [`ScriptBackend::take_error`]. Shared with the build plan.
    error: Arc<Mutex<Option<String>>>,
}

impl ScriptBackend {
    /// Build and **eagerly validate** a script backend: compiles the source,
    /// runs `setup`, and probes `gains` at a few representative positions so a
    /// broken script fails the build immediately rather than mid-sampling.
    pub fn new(
        source: impl Into<Arc<str>>,
        speakers: Vec<[f32; 3]>,
        params: ScriptParams,
    ) -> Result<Self> {
        Self::with_error_slot(source, speakers, params, Arc::new(Mutex::new(None)))
    }

    /// Like [`ScriptBackend::new`] but reusing an externally-owned error slot,
    /// so the caller (the build plan) can read a sampling-time failure after the
    /// gain table has been built.
    pub fn with_error_slot(
        source: impl Into<Arc<str>>,
        speakers: Vec<[f32; 3]>,
        params: ScriptParams,
        error: Arc<Mutex<Option<String>>>,
    ) -> Result<Self> {
        let backend = Self {
            source: source.into(),
            speakers,
            params,
            generation: next_generation(),
            error,
        };
        backend.validate()?;
        Ok(backend)
    }

    /// Shared error slot, cloned into the build plan so it can convert a
    /// sampling-time failure into an `Err` after the table is built.
    pub fn error_slot(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.error)
    }

    /// Take the first sampling error, if any.
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().take()
    }

    fn record_error(&self, message: String) {
        let mut slot = self.error.lock();
        if slot.is_none() {
            *slot = Some(message);
        }
    }

    /// Compile + `setup` + a couple of probe calls on a throwaway VM.
    fn validate(&self) -> Result<()> {
        let vm = self.build_vm(RoomParams::identity())?;
        let probes = [
            [0.0, 0.0, 0.0],
            [0.5, 0.5, 0.5],
            [1.0, 0.0, 0.0],
            [0.0, -1.0, 0.5],
        ];
        for probe in probes {
            vm.eval_gains(probe)
                .map_err(|e| anyhow!("script `gains` failed at probe {probe:?}: {e}"))?;
        }
        Ok(())
    }

    fn build_vm(&self, room: RoomParams) -> Result<ScriptVm> {
        ScriptVm::new(&self.source, &self.speakers, &self.params, room)
    }
}

impl GainModel for ScriptBackend {
    fn kind(&self) -> GainModelKind {
        GainModelKind::Script
    }

    fn backend_id(&self) -> &'static str {
        "script"
    }

    fn backend_label(&self) -> &'static str {
        "Script"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            // Realtime would call Lua per audio sample — forbidden. Only the
            // precomputed (table-building) modes are allowed.
            supports_realtime: false,
            supports_precomputed_polar: true,
            supports_precomputed_cartesian: true,
            supports_position_interpolation: true,
            // Distance attenuation / diffuse are applied by the Rust decorators
            // around this model, so the capabilities are advertised here.
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
        let mut gains = Gains::zeroed(self.speakers.len());

        let result = VM_CACHE.with(|cell| -> Result<()> {
            let mut slot = cell.borrow_mut();
            if slot.as_ref().map(|c| c.generation) != Some(self.generation) {
                *slot = Some(CachedVm {
                    generation: self.generation,
                    vm: self.build_vm(RoomParams::from_request(req))?,
                });
            }
            let vm = &slot.as_ref().expect("vm just inserted").vm;
            let target = RoomParams::from_request(req).apply(req.adm_position.map(|v| v as f32));
            let values = vm.eval_gains(target)?;
            for (i, g) in values.into_iter().enumerate() {
                gains.set(i, g);
            }
            Ok(())
        });

        if let Err(e) = result {
            // Leave gains zeroed; the build converts a recorded error to `Err`.
            self.record_error(e.to_string());
        }

        RenderResponse { gains }
    }

    fn save_to_file(&self, _path: &std::path::Path, _speaker_layout: &SpeakerLayout) -> Result<()> {
        bail!("Saving a precomputed table is only supported for the VBAP backend")
    }
}

thread_local! {
    static VM_CACHE: RefCell<Option<CachedVm>> = const { RefCell::new(None) };
    /// Per-call instruction counter consulted by the debug hook.
    static INSTRUCTIONS: Cell<u64> = const { Cell::new(0) };
}

struct CachedVm {
    generation: u64,
    vm: ScriptVm,
}

/// A compiled, sandboxed Lua VM bound to one script + one speaker set.
struct ScriptVm {
    _lua: Lua,
    gains_fn: Function,
    speaker_count: usize,
    // Reusable handles, rebuilt per VM (cheap Rc clones when passed to Lua).
    speakers_tbl: Table,
    state: Value,
    params_tbl: Table,
    pos_tbl: Table,
}

impl ScriptVm {
    fn new(
        source: &str,
        speakers: &[[f32; 3]],
        params: &ScriptParams,
        room: RoomParams,
    ) -> Result<Self> {
        // Sandbox: whitelist only safe stdlib — no io/os/package/debug.
        let lua = Lua::new_with(
            StdLib::MATH | StdLib::TABLE | StdLib::STRING,
            LuaOptions::default(),
        )
        .map_err(|e| anyhow!("failed to create Lua VM: {e}"))?;

        let _ = lua.set_memory_limit(MEMORY_LIMIT_BYTES);

        // Abort runaway scripts (e.g. infinite loops) within a per-call budget.
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

        lua.load(source)
            .exec()
            .map_err(|e| anyhow!("failed to load script: {e}"))?;
        let globals = lua.globals();

        let speakers_tbl = transformed_speakers_table(&lua, speakers, room)
            .map_err(|e| anyhow!("failed to build speakers table: {e}"))?;
        let params_tbl =
            params_table(&lua, params).map_err(|e| anyhow!("failed to build params table: {e}"))?;

        // Optional `setup(speakers, params) -> state`.
        let state = match globals.get::<Value>("setup").map_err(lua_err)? {
            Value::Function(setup) => {
                reset_instruction_counter();
                setup
                    .call((speakers_tbl.clone(), params_tbl.clone()))
                    .map_err(|e| anyhow!("script `setup` failed: {e}"))?
            }
            Value::Nil => Value::Nil,
            other => bail!("`setup` must be a function, got {}", other.type_name()),
        };

        let gains_fn: Function = match globals.get::<Value>("gains").map_err(lua_err)? {
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
        })
    }

    /// Call `gains` for a (room-transformed) position, returning one gain per
    /// speaker. Errors on a Lua failure or a wrong-length result.
    fn eval_gains(&self, pos: [f32; 3]) -> Result<Vec<f32>> {
        self.pos_tbl.set("x", pos[0]).map_err(lua_err)?;
        self.pos_tbl.set("y", pos[1]).map_err(lua_err)?;
        self.pos_tbl.set("z", pos[2]).map_err(lua_err)?;

        reset_instruction_counter();
        let result: Table = self
            .gains_fn
            .call((
                self.pos_tbl.clone(),
                self.speakers_tbl.clone(),
                self.state.clone(),
                self.params_tbl.clone(),
            ))
            .map_err(|e| anyhow!("{e}"))?;

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

fn reset_instruction_counter() {
    INSTRUCTIONS.with(|c| c.set(0));
}

fn transformed_speakers_table(
    lua: &Lua,
    speakers: &[[f32; 3]],
    room: RoomParams,
) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(speakers.len(), 0)?;
    for (i, raw) in speakers.iter().enumerate() {
        let p = room.apply(*raw);
        let entry = lua.create_table_with_capacity(0, 3)?;
        entry.set("x", p[0])?;
        entry.set("y", p[1])?;
        entry.set("z", p[2])?;
        table.set(i + 1, entry)?;
    }
    Ok(table)
}

fn params_table(lua: &Lua, params: &ScriptParams) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(0, params.0.len())?;
    for (key, value) in &params.0 {
        table.set(key.as_str(), *value)?;
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spatial_vbap::DistanceModel;

    const NEAREST: &str = r#"
        function gains(pos, speakers, state, params)
          local out = {}
          local best, bestd = 1, math.huge
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

    fn request(position: [f64; 3]) -> RenderRequest {
        RenderRequest {
            adm_position: position,
            event_size: [0.0, 0.0, 0.0],
            size_to_spread_mode: Default::default(),
            spread_min: 0.0,
            spread_max: 0.0,
            spread_from_distance: false,
            spread_distance_range: 1.0,
            spread_distance_curve: 1.0,
            room_ratio: [1.0, 1.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.0,
            use_distance_diffuse: false,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            distance_model: DistanceModel::None,
            barycenter_localize: 0.0,
            experimental_distance_distance_floor: 0.0,
            experimental_distance_min_active_speakers: 1,
            experimental_distance_max_active_speakers: 2,
            experimental_distance_position_error_floor: 0.0,
            experimental_distance_position_error_nearest_scale: 0.0,
            experimental_distance_position_error_span_scale: 0.0,
        }
    }

    fn backend(source: &str) -> Result<ScriptBackend> {
        ScriptBackend::new(source, speakers(), ScriptParams::default())
    }

    #[test]
    fn nearest_speaker_script_selects_closest() {
        let model = backend(NEAREST).expect("valid script");
        // Closest to speaker index 1 ([1,0,0]).
        let gains = model.compute_gains(&request([0.9, 0.0, 0.0])).gains;
        assert!(model.take_error().is_none());
        assert_eq!(gains.len(), 4);
        assert_eq!(gains[1], 1.0);
        assert_eq!(gains[0], 0.0);
        assert_eq!(gains[2], 0.0);
        assert_eq!(gains[3], 0.0);
    }

    #[test]
    fn params_reach_the_script() {
        let src = r#"
            function gains(pos, speakers, state, params)
              local out = {}
              for i = 1, #speakers do out[i] = params.level or 0.0 end
              return out
            end
        "#;
        let model = ScriptBackend::new(src, speakers(), ScriptParams(vec![("level".into(), 0.25)]))
            .expect("valid script");
        let gains = model.compute_gains(&request([0.0, 0.0, 0.0])).gains;
        assert!(gains.iter().all(|g| (*g - 0.25).abs() < 1e-6));
    }

    #[test]
    fn setup_state_is_passed_to_gains() {
        let src = r#"
            function setup(speakers, params)
              return { n = #speakers }
            end
            function gains(pos, speakers, state, params)
              local out = {}
              for i = 1, #speakers do out[i] = state.n end
              return out
            end
        "#;
        let model = backend(src).expect("valid script");
        let gains = model.compute_gains(&request([0.0, 0.0, 0.0])).gains;
        assert!(gains.iter().all(|g| (*g - 4.0).abs() < 1e-6));
    }

    #[test]
    fn syntax_error_fails_construction() {
        assert!(backend("function gains( this is not lua").is_err());
    }

    #[test]
    fn missing_gains_function_fails_construction() {
        assert!(backend("local x = 1").is_err());
    }

    #[test]
    fn wrong_length_result_fails_construction() {
        let src = r#"
            function gains(pos, speakers, state, params)
              return { 1.0, 2.0 } -- only 2 of 4
            end
        "#;
        assert!(backend(src).is_err());
    }

    #[test]
    fn non_finite_result_fails_construction() {
        let src = r#"
            function gains(pos, speakers, state, params)
              local out = {}
              for i = 1, #speakers do out[i] = 1.0 / 0.0 end
              return out
            end
        "#;
        assert!(backend(src).is_err());
    }

    #[test]
    fn sandbox_denies_os_and_io() {
        assert!(backend("function gains(p,s,st,pa) return os.time() end").is_err());
        assert!(backend("function gains(p,s,st,pa) io.write('x') return {} end").is_err());
    }

    #[test]
    fn infinite_loop_is_aborted() {
        let src = r#"
            function gains(pos, speakers, state, params)
              while true do end
            end
        "#;
        assert!(backend(src).is_err());
    }

    /// The shipped example script must load and match a Rust reference of the
    /// same inverse-distance law (identity room transform ⇒ raw positions).
    #[test]
    fn shipped_example_matches_reference() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../script-backends/nearest_inverse_distance.lua"
        );
        let source = std::fs::read_to_string(path).expect("example script is readable");
        let model = ScriptBackend::new(source, speakers(), ScriptParams::default())
            .expect("example script is valid");

        let reference = |pos: [f32; 3]| -> Vec<f32> {
            let sp = speakers();
            let mut out = vec![0.0f32; sp.len()];
            let mut energy = 0.0f32;
            for (i, s) in sp.iter().enumerate() {
                let d =
                    ((pos[0] - s[0]).powi(2) + (pos[1] - s[1]).powi(2) + (pos[2] - s[2]).powi(2))
                        .sqrt();
                let w = 1.0 / (d + 0.1);
                out[i] = w;
                energy += w * w;
            }
            let norm = energy.sqrt();
            for v in &mut out {
                *v /= norm;
            }
            out
        };

        for pos in [
            [0.0, 0.0, 0.0],
            [0.7, -0.3, 0.2],
            [-0.5, 0.9, -0.4],
            [1.0, 0.0, 0.0],
        ] {
            let gains = model
                .compute_gains(&request([pos[0] as f64, pos[1] as f64, pos[2] as f64]))
                .gains;
            assert!(model.take_error().is_none(), "no script error at {pos:?}");
            let expected = reference(pos);
            for (i, (g, e)) in gains.iter().zip(expected.iter()).enumerate() {
                assert!(
                    (g - e).abs() < 1e-4,
                    "speaker {i} at {pos:?}: got {g}, expected {e}"
                );
            }
        }
    }
}
