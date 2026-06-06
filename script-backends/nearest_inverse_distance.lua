-- Example Omniphony scriptable render backend.
--
-- A render backend maps an object position to one gain per speaker. Omniphony
-- evaluates this function only while it BUILDS the precomputed gain table (not
-- per audio sample), so plain Lua is fast enough here.
--
-- Contract
-- --------
--   gains(pos, speakers, state, params) -> { number, ... }   (REQUIRED)
--     pos      : { x =, y =, z = }            target position (room-transformed)
--     speakers : { { x=, y=, z= }, ... }      speaker directions (room-transformed)
--     state    : value returned by setup(), or nil
--     params   : { key = number, ... }        numeric params from the config/UI
--     return   : array of #speakers gains, same order as `speakers`.
--                Every value must be finite.
--
--   setup(speakers, params) -> state          (OPTIONAL)
--     Runs once per VM. Use it to precompute anything reused across calls.
--
-- Sandbox: only `math`, `table` and `string` are available — no io/os/require.
--
-- This example: inverse-distance panning. Each speaker's weight falls off with
-- its distance to the object; weights are normalised to constant power. The
-- `falloff` param controls the softening near a speaker (default 0.1).

function gains(pos, speakers, state, params)
  local falloff = params.falloff or 0.1
  local out = {}
  local energy = 0.0
  for i = 1, #speakers do
    local s = speakers[i]
    local dx, dy, dz = pos.x - s.x, pos.y - s.y, pos.z - s.z
    local d = math.sqrt(dx * dx + dy * dy + dz * dz)
    local w = 1.0 / (d + falloff)
    out[i] = w
    energy = energy + w * w
  end

  -- Constant-power normalisation.
  if energy > 1e-12 then
    local norm = math.sqrt(energy)
    for i = 1, #speakers do
      out[i] = out[i] / norm
    end
  end

  return out
end
