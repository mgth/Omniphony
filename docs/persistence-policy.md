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
saved by the Save. Profile operations write the profile list, never the unsaved
edits: a switch discards them unless it carries `"save"` (the full Save first,
no switch if it fails), create copies the live state into the new profile
only, rename and delete leave them pending.

Nothing writes the whole live state to `config.yaml` except the explicit Save.
A change only a restart can apply (a new bridge) uses `/control/restart`, which
carries the unsaved state over in the sidecar instead of saving it.

### Studio (`omniphony-studio-egui`)

- **View** state lives in `crate::prefs::Prefs` and is written through the
  debounced `json_store::Writer`: a change marks the prefs dirty and the next
  frame submits them. Never persist through egui's memory (`ctx.memory`,
  `ctx.data`): it is not saved, and another toolkit would not have it
  (`ARCHITECTURE.md`).
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

## Known deviations

Still to be brought in line; each is removed from this list by the change that
fixes it.

- **Studio view state not yet kept**: camera, window size and position, open
  sections, tabs, the Speakers and Grid switches, the speaker-test settings,
  the log panel.
