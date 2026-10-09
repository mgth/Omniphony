-- Example Omniphony scriptable render backend.
--
-- Omniphony evaluates this only while it BUILDS the precomputed gain table (not
-- per audio sample), so plain Lua is fast enough here.
--
-- Contract
-- --------
--   gains(pos, speakers, state, params) -> { number, ... }   (REQUIRED)
--     pos      : { x=, y=, z= }            target position (raw ADM)
--     speakers : { {x=,y=,z=}, ... }       unit speaker directions, in the
--                                          room-relative space `room_scale(pos)`
--                                          maps a position into
--     state    : value returned by setup(), or nil
--     params   : { key = number, ... }     values for the params declared below
--     return   : array of #speakers finite gains, same order as `speakers`.
--
--   params() -> { {key=,label=,min=,max=,step=,default=,help=}, ... }  (OPTIONAL)
--     Declares tunable parameters so Studio renders sliders for them.
--
--   setup(speakers, params) -> state    (OPTIONAL, one-time per VM)
--
-- Sandbox: only `math`, `table`, `string` are available — no io/os/require.
--
-- This example: inverse-distance panning, constant-power normalised.

function params()
  return {
    { key = "falloff", label = "Falloff", min = 0.001, max = 1.0, step = 0.001,
      default = 0.1, help = "Softening near a speaker (smaller = sharper)." },
    { key = "sharpness", label = "Sharpness", min = 0.5, max = 8.0, step = 0.1,
      default = 2.0, help = "Distance falloff exponent (higher = tighter)." },
  }
end

function gains(pos, speakers, state, params)
  local falloff = params.falloff or 0.1
  local sharpness = params.sharpness or 2.0
  -- The speakers are given in the room-relative space; the raw position is
  -- warped into it the same way, or a non-unit room would pull the object
  -- off the speaker it sits on.
  local p = room_scale(pos)
  local out = {}
  local energy = 0.0
  for i = 1, #speakers do
    local s = speakers[i]
    local dx, dy, dz = p.x - s.x, p.y - s.y, p.z - s.z
    local d = math.sqrt(dx * dx + dy * dy + dz * dz)
    local w = 1.0 / (d + falloff) ^ sharpness
    out[i] = w
    energy = energy + w * w
  end

  if energy > 1e-12 then
    local norm = math.sqrt(energy)
    for i = 1, #speakers do
      out[i] = out[i] / norm
    end
  end

  return out
end
