# Scriptable render backends (Lua)

Omniphony's **Script** render backend evaluates a user-supplied Lua function to
turn an object position into one gain per speaker. The renderer samples this
function only while building its precomputed gain table — never per audio
sample — so an embedded scripting language is fast enough, and you can iterate
on a panning law without recompiling.

## Selecting a script

In Studio, pick **Script** as the render backend; a **Script file** field
appears (it is a normal backend parameter). Point it at a `.lua` file. The
parameters your script declares via `params()` then appear as sliders.

In a config YAML the same lives in the generic backend-param store:

```yaml
render:
  render_backend: script
  backend_params:
    script:
      path: /path/to/your_backend.lua
      falloff: 0.1
```

The backend runs only in a precomputed evaluation mode; a realtime request is
forced to precomputed (calling Lua per sample is not viable).

## The contract

```lua
-- REQUIRED: gains per speaker for one position.
function gains(pos, speakers, state, params)
  -- pos      = { x=, y=, z= }            (raw ADM position)
  -- speakers = { {x=,y=,z=}, ... }       (unit speaker directions, in the room-relative space `room_scale(pos)` maps a position into)
  -- state    = value returned by setup(), or nil
  -- params   = { key = number, ... }     (values for params() below)
  -- return an array of #speakers finite numbers, in speaker order.
end

-- OPTIONAL: declare tunable params so Studio renders sliders.
function params()
  return { { key="falloff", label="Falloff", min=0, max=1, step=0.01,
             default=0.1, help="..." } }
end

-- OPTIONAL: one-time per-VM setup; its return value is passed as `state`.
function setup(speakers, params) return {} end
```

Distance attenuation and distance-diffuse are applied by Omniphony *around* your
script, so you only write the directional panning law.

## Engine helpers

The VM injects engine-provided functions you can call from your script:

- `vbap` — a **VBAP** object for the full layout. Call it (`vbap(pos [, spread])`)
  or use `vbap:gains(pos [, spread])` to get one gain per speaker in speaker order.
  Optional `spread` in `[0, 1]` (default 0). Using it when the layout can't be
  triangulated errors with a clear message.
- `vbap_new(speakers)` — build your **own** VBAP object from a chosen list of
  speaker directions (`{ {x=,y=,z=}, ... }`, e.g. a subset of the `speakers` table
  selected in `setup`). The returned object has `:gains(pos [, spread])`, `:count()`
  and is callable. Its gains are in the order of the list you passed, so map them
  back to full speaker indices yourself. Raises a clear error if the subset can't
  be triangulated.
- `normalize_energy(out)` — return a copy of a gain array scaled to **unit energy**
  (constant power); an all-zero input falls back to equal power, so you never emit
  silence or non-finite gains.

### Coordinates

`pos` is **real cartesian** (`{x, y, z}`, with the object's actual distance); each
`speakers[i]` is a **unit (normalized) cartesian** direction (distance 1). ADM
axes: X right(+)/left(-), Y front(+)/back(-), Z up(+)/down(-). Azimuth `0°` = front
(`+Y`), `+90°` = right (`+X`); elevation `0°` = level, `+90°` = up. Conversion
helpers (angles in **degrees**):

- `polar(p)` — cartesian `{x,y,z}` → **real polar** `{ az, el, dist }`. For
  **normalized polar**, set `dist = 1` (the angles are unchanged).
- `cartesian(s)` — polar `{ az, el, dist? }` → cartesian `{x,y,z}`. `dist` defaults
  to `1`, so `cartesian({ az=, el= })` is the unit direction and
  `cartesian(polar(p))` round-trips to `p`.
- `normalize(p)` — `{x,y,z}` → **normalized cartesian** unit direction (zero vector
  maps to zero).
- `distance(p)` — the radius `|p|`.

```lua
local q = polar(pos)        -- real polar
q.dist = 1                  -- normalized polar
local dir = normalize(pos)  -- normalized cartesian
local back = cartesian(q)   -- cartesian from polar
```

**Room ratios.** The conversions above are pure geometry on the value you pass —
they do **not** apply the room warp. `vbap`/`vbap_new` already warp the position
internally (so they match the built-in VBAP backend). For a custom law that needs
the same room-relative space the engine pans in, warp first with `room_scale(p)`:

```lua
local g = polar(room_scale(pos))   -- room-aware polar of the object
```

So the smallest useful script just defers to the engine:

```lua
function gains(pos, speakers, state, params)
  return normalize_energy(vbap(pos))
end
```

To use only a **subset** of speakers, build the panner once in `setup` and scatter
its gains back into full speaker order:

```lua
function setup(speakers, params)
  local subset, map = {}, {}
  for i, s in ipairs(speakers) do
    if math.abs(s.z) < 0.1 or s.z > 0.5 then        -- e.g. ground ring + tops
      subset[#subset + 1] = s
      map[#map + 1] = i
    end
  end
  return { v = vbap_new(subset), map = map, n = #speakers }
end

function gains(pos, speakers, state, params)
  local sub = state.v:gains(pos)
  local out = {}
  for i = 1, state.n do out[i] = 0.0 end
  for k, g in ipairs(sub) do out[state.map[k]] = g end
  return out
end
```

## Sandbox & limits

Scripts run sandboxed: only `math`, `table` and `string` are available (no
`io`, `os`, `require`, …). Each VM is memory-capped and each call is bounded by
an instruction budget, so an infinite loop fails the build instead of hanging
the renderer. A script that errors or returns non-finite / wrong-count gains is
rejected at build time (the engine smoke-tests every freshly built backend).

## Examples

- [`nearest_inverse_distance.lua`](nearest_inverse_distance.lua) — inverse-distance,
  constant-power panning, with `falloff`/`sharpness` params. A good template.
- [`vbap_blend.lua`](vbap_blend.lua) — defers to the engine: `vbap(pos, spread)`
  with a `spread` slider, then `normalize_energy`. Shows the engine helpers.
- [`vbap_subset.lua`](vbap_subset.lua) — builds a VBAP over a chosen speaker
  subset in `setup` (`vbap_new`) and scatters its gains back. Shows `vbap_new`.
