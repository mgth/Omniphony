# Scriptable render backends (Lua)

Omniphony's **Script** render backend evaluates a user-supplied Lua function to
turn an object position into one gain per speaker. Because the renderer samples
this function only while building its precomputed gain table — never per audio
sample — an embedded scripting language is fast enough, and you can iterate on a
panning law without recompiling.

## Selecting a script

In Studio, pick **Script** as the render backend and point it at a `.lua` file.
In a config YAML, set:

```yaml
render:
  render_backend: script
  script_backend_path: /path/to/your_backend.lua
  script_backend_params:        # optional, exposed to the script as `params`
    falloff: 0.1
```

The backend runs only in a precomputed evaluation mode (polar or cartesian); a
realtime request is automatically resolved to a precomputed mode, since calling
Lua per sample is not viable.

## The contract

```lua
-- REQUIRED: gains per speaker for one position.
function gains(pos, speakers, state, params)
  -- pos      = { x =, y =, z = }            (room-transformed target)
  -- speakers = { { x=, y=, z= }, ... }      (room-transformed speaker dirs)
  -- state    = value returned by setup(), or nil
  -- params   = { key = number, ... }
  -- return an array of #speakers finite numbers, in speaker order.
end

-- OPTIONAL: one-time per-VM setup; its return value is passed as `state`.
function setup(speakers, params) return {} end
```

Distance attenuation and distance-diffuse are applied by Omniphony *around* your
script, so you only write the directional panning law.

## Sandbox & limits

Scripts run sandboxed: only `math`, `table` and `string` are available (no
`io`, `os`, `require`, …). Each VM is memory-capped and each `gains` call is
bounded by an instruction budget, so an infinite loop fails the build instead of
hanging the renderer.

## Examples

- [`nearest_inverse_distance.lua`](nearest_inverse_distance.lua) — inverse-distance,
  constant-power panning. A good template to copy from.
