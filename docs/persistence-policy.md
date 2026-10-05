# Persistence policy

What Omniphony remembers, where, and when. One rule decides it:

> **What changes how things look is kept the moment it changes. What changes
> what you hear, or how the engine behaves, is kept only when you press Save.**

Looks are cheap to get wrong and tedious to redo, so nobody should have to
confirm them: a panel width, a colour map or a camera angle survives a restart,
a crash or a kill. Sound is the opposite. A render setting written to disk
behind the user's back becomes the next session's starting point without their
say, so it waits in the engine, marked unsaved, until they either save it or
throw it away.

Every piece of state belongs to exactly one of the classes below. A new
setting, control or preference is classified **before** it is written, and its
class decides the plumbing.

## The classes

| Class | What it is | Stored in | Written when | Save button |
|---|---|---|---|---|
| **View** | How Studio (or the mpv overlay) shows things: layout, camera, toggles, colours, plots, tabs, test-tool settings, monitoring cadences | Studio: `studio-egui-prefs.json`. Engine: `config.yaml` for the cadences, `overlay-prefs.conf` for the overlay | At once (Studio debounces 600 ms) | Never lights it |
| **App** | How Studio reaches and launches the engine: host, ports, auto-start, keep-alive, last folders | `osc_config.json` | At once (200 ms debounce) | Never lights it |
| **Render / engine** | Anything that changes what is heard or how the engine behaves: backend, layout, gains, delays, binaural, placement, input, output, latency, options, profiles | `config.yaml` | **Only** on `/control/save_config` | Lights it on a real change |
| **Action** | A button that writes a file it names: layout export, backend script save, SOFA upload, installing the service | The file the action names | On the click | Never lights it |
| **Transient** | Gestures and live-only state: mutes, a manual head pose, test signals, subscriptions, resets, selections, open modals | Nowhere | Never | Never lights it |

### Exceptions, and why

An exception needs a reason stronger than convenience. These are the ones that
stand:

- **Head-tracker recenter and calibration** are written to `config.yaml` at
  once, as view state. They measure where the sensor sits on the listener's
  head, not a choice about the mix: a Save prompt after every recenter would be
  noise, and losing a calibration to a crash would be a chore.
- **Monitoring cadences** (`metering/rate_hz`, `diag/rate_hz`) live in
  `config.yaml` because the CLI and the embedded engine both start from them,
  but they only shape what clients display, so they are view state.
- **The live-handoff sidecar** (`config.live.yaml`) carries the *unsaved*
  render state across an engine handoff (mpv track change, standby renderer
  resuming, a `/control/restart`) for up to ten minutes. It never touches
  `config.yaml`, and what it restores comes back unsaved. It is a relay, not a
  save.
- **Transient state that is published but never saved** is still broadcast to
  every client (a mute shows on every Studio), it just never dirties the
  config.
- **Selection, open help cards, modals, a paused plot, the SOFA "go online"
  consent** stay transient: restoring them would surprise more than help, and
  the consent is a privacy choice that should be asked again.

## How each class is wired

### Engine (`omniphony-renderer`)

A control handler returns `ControlEffects`, and the class picks its shape:

- **Render / engine** — `ControlEffects::dirty(notify)`: marks the config
  dirty, broadcasts `/state/config/saved = 0`, publishes the value. Nothing is
  written; `persist::store_live_into_config` must know the field, or the Save
  it lights cannot keep it. Mark dirty **only on a real change**: a client
  re-sending the current value (keepalives, echoes, slider settle) must cost
  nothing.
- **View** — `ControlEffects::view(notify, PersistOp::…)`: publishes, writes
  that one field at once, leaves the config clean. The targeted write amends a
  pending handoff sidecar instead of discarding it, so the user's *other*
  unsaved edits are neither committed nor lost.
- **Transient** — `ControlEffects::transient(notify)` when the state must reach
  other clients, `ControlEffects::default()` for a pure action.

Registry options (`/control/option`) are render state: dirty on a real change,
saved by the Save — the host's too (audio output, live input). Applying a
staged group (`/control/options/apply`, and its aliases `/control/input/apply`
and `/control/config/input/apply`) is transient: it hands the staged values to
the input and publishes the new state; the staged writes already lit the Save
button. Profile operations write the profile list, never the unsaved
edits: a switch discards them unless it carries `"save"` (the full Save first,
no switch if it fails), create copies the live state into the new profile
only, rename and delete leave them pending.

Nothing writes the whole live state to `config.yaml` except the explicit Save.
A change only a restart can apply (a new bridge) uses `/control/restart`, which
carries the unsaved state over in the sidecar instead of saving it.

Every write starts from the file on disk (`Config::load_for_update`). A
`config.yaml` that fails to parse — the engine then runs on defaults and
publishes `config_status = parse_error` — is never written: the Save, a
profile operation and a targeted view write are all refused, and the Save
error says the file was left untouched. Fixing the file is not enough to save
again: until a Reload (a restart on the CLI) or a profile switch reads it back
into the live state, `config_status` stays `parse_error` and the Save is still
refused, since the live state it would write is those defaults. A live state
handed over to the next instance (a restart keeping it, mpv taking over)
carries that origin in the sidecar (`live_from_parse_error`), so the next
instance keeps `parse_error` too, whatever the file now holds.

Every save writes `schema_version` at the top of the file
(`CONFIG_SCHEMA_VERSION` in `renderer/src/config.rs`). A build that reads a
higher one than it knows runs on what it understands of the file, publishes
`config_status = newer_schema` and refuses every write to it, as for a
parse error. The version is bumped only when a build changes what an existing
key means, or moves or retires one. New keys and new enum values need no bump:
an older build keeps both through a save. A key it does not model lives in the
section's `extra`. An enum value it does not know does not fail the file: the
field falls back to its default with a warning, and the value is kept in
`extra` under its own key. A save then writes it back unless the field holds a
choice of this build's: a value other than the one the absent key stands for.
Files from builds older than the key carry no `schema_version`, and those
builds save their content under the newer number, so the key only protects
from the build that introduced it onwards.

A write that goes through replaces the file atomically (temp file, sync,
rename) and a Save or a profile operation keeps the previous one as
`config.yaml.bak`. A targeted view write and the handoff sidecar are written
the same way but without the `.bak` and without syncing the directory, so a
view change never rotates away the file as it was before the last Save. Where
the rename would fail or change what the file is — a directory that is not
writable, a file with other hard links or owned by another user or group — the
file is rewritten in place instead, as before (not atomic; the `.bak` is then
best-effort). A file without write permission (`chmod a-w`, an ACL) is
refused, as before, although the directory would allow the rename; a symlink
is written through even when its target does not exist yet, creating it.

### Studio (`omniphony-studio-egui`)

- **View** state lives in `crate::prefs::Prefs` and is written through the
  debounced `json_store::Writer`: a change marks the prefs dirty and the next
  frame submits them. `prefs::display` holds the Display panel's settings,
  `prefs::view` the rest of the view — camera (taken at rest), window size,
  position and maximised state, open sections, tabs, the speaker-test
  settings. Never persist through egui's memory (`ctx.memory`, `ctx.data`):
  it is not saved, and another toolkit would not have it (`ARCHITECTURE.md`);
  sections, whose open state egui animates, report every toggle back to
  `prefs::view` (`ui::section::take_changed_open_states`).
- A new field of the scene's `ViewSettings` or `VolumeSettings` does not
  compile until `prefs::display::every_display_setting_is_classified` names
  it as kept or not kept, with the reason.
- **App** state lives in `RuntimeConfig` (`osc_config.json`), written through
  the same kind of writer.
- **Render / engine** state is sent to the engine and nowhere else. Studio
  sends `/control/save_config` only from the Save button and the quit prompt,
  through `commands::engine::request_save_config` — and asks for a save before
  a profile switch only when the user picks *Save and switch*. No panel, no
  Apply, no profile change saves on the user's behalf.

### Leaving with unsaved edits

The footer's indicator is the engine's word on its file (`/state/config/saved`),
shown only while connected: a renderer that is gone has nothing to save. Where
unsaved edits would be dropped, Studio asks first — and only then:

- **Closing Studio** holds the close back (`guard_quit_request`, run from
  `App::logic` so it works behind a minimised window) and offers *Save and
  quit*, *Quit without saving* or *Cancel*. *Save and quit* closes once the
  engine confirms the save; a failure brings the prompt back with the reason.
  The prompt also says whether quitting stops the renderer (one Studio launched
  without keep-alive) or leaves it running with the edits.
- **Reload** asks before discarding them.
- **Switching profile** asks: *Save and switch*, *Switch without saving* or
  *Cancel*.

Limits: on macOS, Cmd+Q from the application menu terminates without a close
event, so only the window's close button is guarded; a Studio killed by a
signal asks nothing.

## Adding something

1. Say which class it is. When in doubt: *does it change a sample that reaches
   a speaker or the headphones, or what the engine does with one?* Then it is
   render / engine.
2. Wire it as its class says above. For a render / engine setting, add it to
   `store_live_into_config` (and the host's `amend_saved_config` for host
   fields) and seed it back at start, or the Save lights for nothing.
3. Name its class in the control's row of
   [`docs/osc-control-contract.md`](osc-control-contract.md) when it is not
   render / engine.

## How the rule is held

Written rules drift; these fail the build instead:

- `omniphony-renderer/runtime_control/tests/persistence_policy.rs` — every
  `PersistOp` (a write that bypasses Save) must be one of the view-state
  exceptions above, and only the Save handler, the profile operations and the
  shutdown handoff may call the whole-state writers.
- `live_options_conformance::every_option_dirties_on_a_change_and_only_then`
  — every registry option lights the Save button on a real change and never
  on a re-send of the current value.
- The native Studio's architecture test, rule `save-config` — only the Save
  button, the quit prompt and the profile-switch prompt save.
- `prefs::display::every_display_setting_is_classified` — a new display
  setting does not compile until it is classified.

When one of them fails, decide which class the new thing is; do not widen the
allow-list to get the change through. A genuine new exception is added here,
under *Exceptions, and why*, with its reason, in the same change.

## Known deviations

None. A change that has to leave one behind lists it here, with the reason.
