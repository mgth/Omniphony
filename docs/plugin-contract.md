# The plugin contract

Three kinds of component can be added to the renderer without touching its
core, and they all follow one contract:

| Kind | Builds | Trait | Registered with |
|---|---|---|---|
| Render backend | a gain model | `BackendFactory` | `RendererControl::register_backend` |
| Object generator | synthesized height objects from a 2-D bed | `ObjectGeneratorFactory` | `Engine::register_object_generator` |
| Phantom extraction | one built-in stage | `PhantomExtractPlugin` | — (built in) |

What is common to them — identity, parameters, registration, storage,
persistence, publication, control — is described here once. How to write each
kind is in its own guide:
[render backends](custom-render-backend-integration.md), and
[object generators](#writing-an-object-generator) below.

The code lives in
[`renderer/src/plugin.rs`](../omniphony-renderer/renderer/src/plugin.rs) (the
trait, the registry, the value store) and
[`renderer/src/backend_params.rs`](../omniphony-renderer/renderer/src/backend_params.rs)
(the parameter schema).

## Identity: `PluginFactory`

```rust
pub trait PluginFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str { self.id() }
    fn i18n_key(&self) -> Option<&'static str> { None }
    fn param_schema(&self) -> Vec<ParamSpec> { Vec::new() }
    fn param_schema_for(&self, params: &ParamMap) -> Vec<ParamSpec> { self.param_schema() }
}
```

`BackendFactory` and `ObjectGeneratorFactory` extend it with how to build
their kind; a plugin implements both traits. The id is what the host selects
it by, matched **exactly**. `i18n_key` names a localized label in Studio's
catalogues (built-ins); an out-of-tree plugin leaves it `None` and its `label`
is shown. `param_schema_for` lets a plugin whose parameters depend on its own
values (the scriptable backend) report a dynamic schema.

## Parameters: `ParamSpec`

A plugin declares its tunables as data. Nothing else in the renderer or in
either Studio is written per parameter: the host stores the values
generically, and the Studios draw the controls from the schema.

```rust
fn param_schema(&self) -> Vec<ParamSpec> {
    vec![
        ParamSpec::float("strength", "Strength", 0.0, 1.0, 0.01, 0.5),
        ParamSpec::float("hpf_hz", "Bass cutoff", 20.0, 2000.0, 10.0, 300.0).unit("Hz"),
        ParamSpec::int("passes", "Passes", 1, 3, 1),
        ParamSpec::bool("center", "Relocalize center", false),
    ]
}
```

| Builder | Control in Studio | Value |
|---|---|---|
| `ParamSpec::float(key, label, min, max, step, default)` | slider | `ParamValue::Float` |
| `ParamSpec::int(key, label, min, max, default)` | slider in steps of 1 | `ParamValue::Int` |
| `ParamSpec::bool(key, label, default)` | switch | `ParamValue::Bool` |
| `ParamSpec { kind: ParamKind::Enum { options }, .. }` | select | `ParamValue::Text` |
| `ParamSpec::path(..)` / `ParamSpec::file(..)` | file field (backends only) | `ParamValue::Text` |

Chainable:

- `.unit("Hz")` — a suffix on the slider's readout (which shows as many
  decimals as the step means);
- `.i18n("twoDSources.padHpf")` — the key of a localized label (built-ins);
- `.help("…")` — a one-line description, opened from the parameter's name;
- `.requires("capability")` — the control only applies under a condition of
  its plugin: a backend capability (`supports_distance_model`, …), or, for the
  phantom stage, the method (`broadband` / `spectral`). Studio keeps the
  control editable and dims it with a note while the condition is not met.

An incoming value is read **in the declared type** (`ParamSpec::coerce`): a
float for an int is rounded, a number for a switch is on at `>= 0.5` (how a
float-only client spells one), an enum value must be one of its options. A
value the declared type cannot read is refused over OSC. Bounds are the
plugin's to clamp, where it applies the value.

## Registration: one rule

`PluginRegistry<F>` is the registry of every kind (`BackendRegistry` and
`ObjectGeneratorRegistry` are its two instances). Its rule:

- ids are exact (`"pad"`, not `"PAD"`);
- **a later registration with the same id replaces the earlier one**, in its
  place — a host can override a built-in by registering its own under the
  built-in's id.

"No generator" is a *selection* (`object_generator_id` empty or `none`), not
an entry of the registry.

## Values: one store, kept by Save

`RendererControl` holds every plugin's values in one store,
`PluginParams`: `kind → plugin id → key → ParamValue`. Each kind is its own id
namespace, so a generator named like a backend cannot collide with it. The
store is sparse: an absent key is the plugin's declared default.

Values reach `config.yaml` only through the Save button (and the quit and
profile-switch prompts): they change the render, so they are the render class
of [the persistence policy](persistence-policy.md). In `render`:

```yaml
backend_params:          # backends: id → key → value
  vbap: { spread_max: 1.0 }
generator_params:        # object generators: id → key → value
  pad: { strength: 0.8, hpf_hz: 400.0 }
phantom_extract_params:  # the phantom stage: key → value
  center: true
  passes: 2
```

Older configs are read and migrated on load, and the old keys dropped on the
next Save: `object_generator_params` (one flat map, which belonged to the
selected `object_generator_id`) and `phantom_params` (float-only; its `method`
entry is `phantom_extract_mode`'s migration). A value no declared parameter
can read is kept as the file had it; keys the renderer does not know are
preserved as ever.

A backend reads its values when it is built (`BackendBuildCtx::backend_param`).
The synthesizing stages are handed theirs through `set_param` when a value
changes or the stage is rebuilt — never per sample: the render thread checks
one atomic generation counter per frame and only reads the store when it
moved.

## Publication: one format

Each registry is published as a list of `PluginListing`s, the serde of
`{ id, label, i18nKey?, params: [ParamSpec] }`:

| Where | What |
|---|---|
| `/state/renderer` → `renderBackendState.availableBackends` | the backends |
| `/state/object_generators` | the object generators |
| `/state/phantom` | the phantom stage (one listing) |

and the stored values beside them in `/state/renderer`:
`renderBackendState.backendParamValuesById` (`{id: {key: value}}`),
`objectGeneratorParamValuesById` (the same, per generator) and
`phantomParamValues` (`{key: value}`).

A parameter spec on the wire:

```json
{ "key": "hpf_hz", "label": "Bass cutoff", "i18nKey": "twoDSources.padHpf",
  "unit": "Hz", "kind": { "type": "float", "min": 20.0, "max": 2000.0, "step": 10.0 },
  "default": 300.0 }
```

Both Studios draw backends, generators and the phantom stage with one
generated form (`plugin_params_form` in the native Studio,
`controls/plugin-params.js` in the web one).

## Control

| Address | Args |
|---|---|
| `/omniphony/control/backend/param` | `[key, value]` for the selected backend, or `[backend_id, key, value]` |
| `/omniphony/control/object_generator/param` | `[key, value]` for the selected generator, or `[generator_id, key, value]` |
| `/omniphony/control/phantom_extract/param` | `[key, value]` |

`value` is an OSC float, int, bool or string, read in the declared type.
A write marks the configuration unsaved (the Save button lights on every
client) and is published with the next state bundle.

## Writing an object generator

An object generator turns channel-based content that carries no height into
synthesized height objects: its audio is appended as extra object channels, so
the object path spatializes it unchanged. It runs only on channel content,
when the output layout has a height layer, and when the synthesized-objects
master is on. See the built-ins in
[`orender_engine/src/object_gen.rs`](../omniphony-renderer/orender_engine/src/object_gen.rs):
`copy_up` is the minimal one, `pad` and `dirac` declare parameters.

1. **The generator** — `ObjectGenerator`, stateful, run in the audio thread:

   ```rust
   impl ObjectGenerator for MyGenerator {
       fn capabilities(&self) -> ObjectGenCapabilities { .. }
       // Plan the static objects for this layout + input (allocate here).
       fn prepare(&mut self, ctx: &PrepareCtx) -> Vec<SynthObjectSpec> { .. }
       // Fill out[k] with the k-th object's audio. No allocation, no panic.
       fn process(&mut self, bed: &BedFrame, out: &mut [Vec<f32>]) { .. }
       // Apply one stored parameter in place (no DSP reset).
       fn set_param(&mut self, key: &str, value: &ParamValue, sample_rate: u32) {
           match (key, value.as_f32()) {
               ("strength", Some(v)) => self.strength = v.clamp(0.0, 1.0),
               _ => {}
           }
       }
   }
   ```

   `set_param` is called when a value changes and after every rebuild of the
   generator (a fresh instance starts at its defaults), in the audio thread:
   it must not allocate. Read the value leniently (`as_f32`, `as_switch`).

2. **The factory** — the plugin and how to build it:

   ```rust
   impl PluginFactory for MyFactory {
       fn id(&self) -> &'static str { "my_generator" }
       fn label(&self) -> &'static str { "My generator" }
       fn param_schema(&self) -> Vec<ParamSpec> {
           vec![ParamSpec::float("strength", "Strength", 0.0, 1.0, 0.01, 0.5)]
       }
   }

   impl ObjectGeneratorFactory for MyFactory {
       fn requires_height_layer(&self) -> bool { true }
       fn build(&self) -> Box<dyn ObjectGenerator> { Box::<MyGenerator>::default() }
   }
   ```

3. **Register it** at startup, before OSC is enabled:

   ```rust
   engine.register_object_generator(Box::new(MyFactory));
   ```

   It appears in both Studios' generator selector with its parameters, is
   selectable by `object_generator_id = "my_generator"`, and its values are
   stored under `generator_params.my_generator`.
