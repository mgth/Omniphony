//! Engine-level controls: renderer config save/reload, log level, ramp mode,
//! dynamic-range-control (DRC) tuning and the layout-export trigger.
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::host::channels::{CoordMode, Family, PlacementMode};
use crate::osc_contract;

fn control_save_config(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_SAVE_CONFIG.to_string(),
        },
    );
}

pub fn control_reload_config(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_RELOAD_CONFIG.to_string(),
        },
    );
}

/// Restart the renderer's pipeline, keeping the unsaved edits: they come
/// back unsaved. For a change only a restart applies (a new bridge), which
/// must not save everything else behind the user's back.
pub fn control_restart(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_RESTART.to_string(),
        },
    );
}

/// The Save button: the model remembers that a save was asked for — the
/// footer's indicator reads it — and the renderer is told. This is the only
/// way Studio writes the renderer's config (docs/persistence-policy.md).
pub fn request_save_config(state: &SharedState) {
    state.inner.lock().unwrap().save_requested = true;
    control_save_config(state);
}

/// Whether the renderer holds edits its config file does not: connected, and
/// its last word on the file was "unsaved". An unknown or stale answer (no
/// snapshot yet, a renderer gone) says no, so nothing ever asks about edits
/// in a renderer that is not there.
pub fn has_unsaved_edits(state: &SharedState) -> bool {
    renderer_connected(state) && state.inner.lock().unwrap().app.config_saved == Some(0)
}

/// Whether a renderer is registered to answer what Studio asks.
pub fn renderer_connected(state: &SharedState) -> bool {
    state.stats.connection_state() == crate::osc::ConnectionState::Connected
}

/// Where the last Save stands, for a flow that waits on it (save and quit).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// Asked for, no answer yet.
    Pending,
    /// The file matches the renderer.
    Saved,
    /// The renderer tried and could not.
    Failed(String),
    /// Answered, and the renderer still holds unsaved edits (another write
    /// landed in between).
    Unsaved,
}

pub fn save_outcome(state: &SharedState) -> SaveOutcome {
    let live = state.inner.lock().unwrap();
    if live.save_requested {
        return SaveOutcome::Pending;
    }
    if let Some(error) = live
        .app
        .save_error
        .as_ref()
        .filter(|e| !e.trim().is_empty())
    {
        return SaveOutcome::Failed(error.clone());
    }
    if live.app.config_saved == Some(1) {
        SaveOutcome::Saved
    } else {
        SaveOutcome::Unsaved
    }
}

pub fn control_log_level(state: &SharedState, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(
        trimmed.as_str(),
        "off" | "error" | "warn" | "info" | "debug" | "trace"
    ) {
        return;
    }
    state.inner.lock().unwrap().app.log_level = Some(trimmed.clone());
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_LOG_LEVEL.to_string(),
            value: trimmed,
        },
    );
}

pub fn control_ramp_mode(state: &SharedState, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "off" | "frame" | "sample") {
        return;
    }
    state.inner.lock().unwrap().app.audio.ramp_mode = Some(trimmed.clone());
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_RAMP_MODE.to_string(),
            value: trimmed,
        },
    );
}

/// Set any declared live option (the renderer's `options` registry) through
/// the generic `/omniphony/control/option [key, value]` address. The value is
/// a JSON scalar from the `data-option` binder: a string for enum/id options,
/// a bool for toggles (forwarded as int 0/1), a number for future scalar
/// kinds. Validation lives renderer-side against the registry spec — an
/// unknown key or a bad value is dropped there, per the OSC contract.
/// Pick the object generator, or `""` for none. Each generator keeps its
/// own parameter values, so nothing is dropped with the previous choice.
pub fn set_object_generator(state: &SharedState, id: &str) {
    control_option(
        state,
        "object_generator_id".to_owned(),
        serde_json::json!(id),
    );
}

pub fn control_option(state: &SharedState, key: String, value: serde_json::Value) {
    let k = key.trim().to_ascii_lowercase();
    if k.is_empty() {
        return;
    }
    let arg = match &value {
        serde_json::Value::String(s) => rosc::OscType::String(s.trim().to_ascii_lowercase()),
        serde_json::Value::Bool(b) => rosc::OscType::Int(if *b { 1 } else { 0 }),
        serde_json::Value::Number(n) => match n.as_f64() {
            Some(f) if f.is_finite() => rosc::OscType::Float(f as f32),
            _ => return,
        },
        _ => return,
    };
    // Optimistic, and only for an option that is actually going out: the
    // registry showing a value the renderer never heard about is the failure
    // this command exists to avoid.
    state.inner.lock().unwrap().set_option(&k, value);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OPTION.to_string(),
            args: vec![rosc::OscType::String(k), arg],
        },
    );
}

/// Apply a `staged` group of declared options (`/control/options/apply`):
/// every value staged since its last apply goes in at once. The pending flag
/// is cleared at once, as the renderer's echo will.
pub fn apply_option_group(state: &SharedState, group: &str) {
    let group = group.trim();
    if group.is_empty() {
        return;
    }
    state.inner.lock().unwrap().clear_group_pending(group);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OPTIONS_APPLY.to_string(),
            args: vec![rosc::OscType::String(group.to_owned())],
        },
    );
}

/// Remember a plugin parameter in the model, so the control that set it
/// reads its own value back instead of snapping until the renderer's echo
/// arrives.
fn remember_param(params: &mut Option<serde_json::Value>, key: &str, value: &serde_json::Value) {
    let params = params.get_or_insert_with(|| serde_json::Value::Object(Default::default()));
    if let Some(map) = params.as_object_mut() {
        map.insert(key.to_owned(), value.clone());
    }
}

/// A parameter of the object generator `generator`, remembered and sent as
/// `[generator, key, value]` — addressed by id, so it reaches the generator
/// the form shows whatever is selected. The value keeps its JSON type (a
/// switch sends a bool); the renderer reads it in the type the generator's
/// schema declares and clamps it.
pub fn set_object_generator_param(
    state: &SharedState,
    generator: &str,
    key: &str,
    value: serde_json::Value,
) {
    let (generator, key) = (generator.trim(), key.trim().to_ascii_lowercase());
    let Some(arg) = super::render::param_value_arg(&value) else {
        return;
    };
    if generator.is_empty() || key.is_empty() {
        return;
    }
    {
        let mut live = state.inner.lock().unwrap();
        let by_id = live
            .app
            .live_options
            .object_generator_param_values_by_id
            .get_or_insert_with(|| serde_json::Value::Object(Default::default()));
        if let Some(map) = by_id.as_object_mut() {
            let mut values = map.remove(generator);
            remember_param(&mut values, &key, &value);
            if let Some(values) = values {
                map.insert(generator.to_owned(), values);
            }
        }
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OBJECT_GENERATOR_PARAM.to_string(),
            args: vec![
                rosc::OscType::String(generator.to_owned()),
                rosc::OscType::String(key),
                arg,
            ],
        },
    );
}

/// A phantom-extraction parameter, remembered and sent as `[key, value]`,
/// the value in its JSON type.
pub fn set_phantom_extract_param(state: &SharedState, key: &str, value: serde_json::Value) {
    let key = key.trim().to_ascii_lowercase();
    let Some(arg) = super::render::param_value_arg(&value) else {
        return;
    };
    if key.is_empty() {
        return;
    }
    remember_param(
        &mut state
            .inner
            .lock()
            .unwrap()
            .app
            .live_options
            .phantom_param_values,
        &key,
        &value,
    );
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_PHANTOM_EXTRACT_PARAM.to_string(),
            args: vec![rosc::OscType::String(key), arg],
        },
    );
}

/// Which family the channel editor and the at-rest markers show. A view
/// choice that the core keeps, because the markers are a core service and
/// must follow the same tab as the editor.
pub fn select_placement_family(state: &SharedState, family: Family) {
    let mut live = state.inner.lock().unwrap();
    if live.editing_family == family {
        return;
    }
    live.editing_family = family;
    drop(live);
    (state.waker)();
}

/// A family's placement mode: `None` clears the family's own choice, so it
/// inherits (the generic mode, else its built-in default). Applied to the
/// model at once and sent; the renderer echoes the resolved block.
pub fn set_placement_mode(state: &SharedState, family: Family, mode: Option<PlacementMode>) {
    {
        let mut live = state.inner.lock().unwrap();
        let block = placement_block_mut(&mut live.app, family);
        block.insert(
            "mode".to_owned(),
            match mode {
                Some(mode) => serde_json::Value::String(mode.as_str().to_owned()),
                None => serde_json::Value::Null,
            },
        );
    }
    (state.waker)();
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_PLACEMENT_MODE.to_string(),
            args: vec![
                rosc::OscType::String(family.as_str().to_owned()),
                rosc::OscType::String(mode.map_or("inherit", PlacementMode::as_str).to_owned()),
            ],
        },
    );
}

/// A family's own entries, applied and sent. The document is what the
/// channel editor built; the model shows it at once so the editor, the 3D
/// view and the audio agree before the renderer echoes.
pub fn set_placement_layout(state: &SharedState, family: Family, payload: serde_json::Value) {
    let value = serde_json::to_string(&payload).ok();
    preview_placement_layout(state, family, payload);
    if let Some(value) = value {
        control_placement_layout(state, family, value);
    }
}

/// Clear a family's own entries: it then uses the generic ones — or, for
/// the generic family itself, the defaults (LFE direct, unity trims, the
/// model's poses).
pub fn clear_placement_layout(state: &SharedState, family: Family) {
    {
        let mut live = state.inner.lock().unwrap();
        placement_block_mut(&mut live.app, family)
            .insert("layout".to_owned(), serde_json::Value::Null);
        if family.is_generic() {
            live.app.live_options.virtual_bed = None;
        }
    }
    (state.waker)();
    control_placement_layout(state, family, String::new());
}

/// Switch a family to manual mode with the poses it renders right now as
/// its entries — what you hear becomes what you edit, with no jump. The
/// renderer's own fixed-channel positions are taken when that family is
/// playing (they carry the declared angles and the Side/Back choice); the
/// model's poses otherwise.
pub fn switch_placement_to_manual(state: &SharedState, family: Family) {
    use crate::host::channels::{adm_to_polar, build_layout_payload, effective_channels_for};
    let payload = {
        let live = state.inner.lock().unwrap();
        let room = live.app.room_ratio.clone();
        let playing = crate::host::channels::playing_family(&live.app) == Some(family);
        let mut channels = effective_channels_for(&live.channels, &live.app, family);
        if playing {
            for channel in &mut channels {
                let Some(source) = live.app.sources.get(&channel.name) else {
                    continue;
                };
                if source.fixed != Some(true) {
                    continue;
                }
                let adm = [source.x, source.y, source.z];
                let (azimuth, elevation, distance) = adm_to_polar(&room, adm);
                channel.coord_mode = CoordMode::Cartesian;
                channel.x = adm[0];
                channel.y = adm[1];
                channel.z = adm[2];
                channel.azimuth = azimuth;
                channel.elevation = elevation;
                channel.distance = distance;
            }
        }
        build_layout_payload(&live.app, &channels)
    };
    // Entries first, then the mode: the plan flips once, with the entries
    // already in place.
    set_placement_layout(state, family, payload);
    set_placement_mode(state, family, Some(PlacementMode::Manual));
}

/// A drag in flight moves the local copy only: the entries are a whole
/// layout, and pushing one per pointer move would be a stream of layouts.
pub fn preview_placement_layout(state: &SharedState, family: Family, payload: serde_json::Value) {
    {
        let mut live = state.inner.lock().unwrap();
        if family.is_generic() {
            live.app.live_options.virtual_bed = Some(payload.clone());
        }
        placement_block_mut(&mut live.app, family).insert("layout".to_owned(), payload);
    }
    // The markers are published by a core service, so entries the UI just
    // changed have to reach the clock. Without this the markers would wait
    // for whatever else wakes it — an incoming packet, which is exactly what
    // a Studio editing offline does not have.
    (state.waker)();
}

fn control_placement_layout(state: &SharedState, family: Family, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_PLACEMENT_LAYOUT.to_string(),
            args: vec![
                rosc::OscType::String(family.as_str().to_owned()),
                rosc::OscType::String(value),
            ],
        },
    );
}

/// The family's object in the mirrored `placement` block, created on demand
/// so an optimistic edit lands somewhere even before the first echo.
fn placement_block_mut(
    app: &mut crate::model::app_state::AppState,
    family: Family,
) -> &mut serde_json::Map<String, serde_json::Value> {
    let placement = app
        .live_options
        .placement
        .get_or_insert_with(|| serde_json::Value::Object(Default::default()));
    if !placement.is_object() {
        *placement = serde_json::Value::Object(Default::default());
    }
    let families = placement.as_object_mut().expect("object");
    let block = families
        .entry(family.as_str().to_owned())
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    if !block.is_object() {
        *block = serde_json::Value::Object(Default::default());
    }
    block.as_object_mut().expect("object")
}

pub fn control_drc_mode(state: &SharedState, value: String) {
    // Applied here as well as sent: the control that changes the model is the
    // one that tells the renderer, so a view never writes it (ARCHITECTURE.md).
    // The renderer's echo replaces it a moment later.
    state.inner.lock().unwrap().app.drc_mode = Some(value.clone());
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_DRC_MODE.to_string(),
            value,
        },
    );
}

pub fn control_drc_weight(state: &SharedState, value: f32) {
    let clamped = value.clamp(0.0, 1.0);
    state.inner.lock().unwrap().app.drc_weight = Some(clamped);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_INPUT_DRC_WEIGHT.to_string(),
            value: clamped,
        },
    );
}

pub fn control_export_layout(state: &SharedState, name: Option<String>) {
    if let Some(raw) = name {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            send_control(
                &state.osc_tx,
                OscControlMsg::SendString {
                    address: osc_contract::CONTROL_LAYOUT_EXPORT.to_string(),
                    value: trimmed.to_string(),
                },
            );
            return;
        }
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_LAYOUT_EXPORT.to_string(),
        },
    );
}

#[cfg(test)]
mod placement_tests {
    use super::*;
    use crate::host::channels::{LayoutSource, PlacementMode, family_placement};

    /// A mode edit lands in the mirrored block at once (optimistic), an
    /// `inherit` clears it, and clearing the entries leaves the family on
    /// the generic ones.
    #[test]
    fn placement_commands_update_the_model_before_the_echo() {
        let state = crate::host::commands::tests::state();
        set_placement_mode(&state, Family::named("dts"), Some(PlacementMode::Sphere));
        {
            let live = state.inner.lock().unwrap();
            let dts = family_placement(&live.app, Family::named("dts"));
            assert_eq!(dts.own_mode, Some(PlacementMode::Sphere));
            assert_eq!(dts.effective_mode, PlacementMode::Sphere);
            assert_eq!(
                family_placement(&live.app, Family::named("dolby")).effective_mode,
                PlacementMode::Room,
                "another family is untouched"
            );
        }
        set_placement_mode(&state, Family::named("dts"), None);
        assert_eq!(
            family_placement(&state.inner.lock().unwrap().app, Family::named("dts")).own_mode,
            None
        );

        let entries = serde_json::json!({ "radius_m": 1.0, "speakers": [
            { "name": "LFE", "coord_mode": "cartesian", "x": 0.0, "y": 1.0, "z": 0.0, "spatialize": false }
        ] });
        set_placement_layout(&state, Family::named("dts"), entries);
        assert_eq!(
            family_placement(&state.inner.lock().unwrap().app, Family::named("dts")).layout_source,
            LayoutSource::Own
        );
        clear_placement_layout(&state, Family::named("dts"));
        assert_eq!(
            family_placement(&state.inner.lock().unwrap().app, Family::named("dts")).layout_source,
            LayoutSource::None,
            "no generic entries either"
        );
    }

    /// Switching to manual seeds the family's entries with what it renders
    /// now: in room mode, the corners.
    #[test]
    fn switching_to_manual_seeds_the_entries_from_the_current_poses() {
        let state = crate::host::commands::tests::state();
        {
            let mut live = state.inner.lock().unwrap();
            // A bridge family that is a sphere by default, as the renderer
            // reports it.
            live.app.live_options.placement = Some(serde_json::json!({
                "auro": { "label": "Auro-3D", "defaultMode": "sphere" }
            }));
            let app = std::mem::take(&mut live.app);
            live.channels.refresh(&app);
            live.app = app;
        }
        switch_placement_to_manual(&state, Family::named("auro"));
        let live = state.inner.lock().unwrap();
        let auro = family_placement(&live.app, Family::named("auro"));
        assert_eq!(auro.own_mode, Some(PlacementMode::Manual));
        assert_eq!(auro.layout_source, LayoutSource::Own);
        let ls = crate::host::channels::effective_channels_for(
            &live.channels,
            &live.app,
            Family::named("auro"),
        )
        .into_iter()
        .find(|c| c.name == "Ls")
        .expect("Ls");
        // Auro's default mode is sphere: the seed is its nominal direction,
        // kept as a polar entry.
        assert_eq!(ls.coord_mode, CoordMode::Polar);
        assert_eq!((ls.azimuth, ls.elevation), (-110.0, 0.0));
        let family = live.editing_family;
        drop(live);
        select_placement_family(&state, Family::named("pcm"));
        assert_ne!(state.inner.lock().unwrap().editing_family, family);
    }
}

#[cfg(test)]
mod unsaved_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// The quit prompt and the Reload confirmation ask only about a renderer
    /// that is there and said "unsaved".
    #[test]
    fn unsaved_edits_need_a_connected_renderer_that_said_so() {
        let state = crate::host::commands::tests::state();
        assert!(!has_unsaved_edits(&state), "nothing heard yet");
        state.inner.lock().unwrap().app.config_saved = Some(0);
        assert!(!has_unsaved_edits(&state), "not connected");
        state.stats.registered.store(true, Ordering::Relaxed);
        assert!(has_unsaved_edits(&state));
        state.inner.lock().unwrap().app.config_saved = Some(1);
        assert!(!has_unsaved_edits(&state));
    }

    #[test]
    fn a_save_is_pending_until_the_renderer_answers() {
        let state = crate::host::commands::tests::state();
        state.inner.lock().unwrap().app.config_saved = Some(0);
        request_save_config(&state);
        assert_eq!(save_outcome(&state), SaveOutcome::Pending);
        {
            let mut live = state.inner.lock().unwrap();
            live.save_requested = false;
            live.app.config_saved = Some(1);
        }
        assert_eq!(save_outcome(&state), SaveOutcome::Saved);
        state.inner.lock().unwrap().app.save_error = Some("read-only".into());
        assert_eq!(
            save_outcome(&state),
            SaveOutcome::Failed("read-only".into())
        );
    }

    /// What the renderer sends for a save, in order: the old error cleared,
    /// the file written, then `saved = 1`. Only that last one answers it.
    #[test]
    fn a_save_is_answered_by_saved_not_by_the_error_it_clears_first() {
        use crate::osc::{dispatch::apply_event, parser::OscEvent};
        let state = crate::host::commands::tests::state();
        state.stats.registered.store(true, Ordering::Relaxed);
        state.inner.lock().unwrap().app.config_saved = Some(0);
        request_save_config(&state);
        let event = |ev| apply_event(&mut state.inner.lock().unwrap(), ev);
        event(OscEvent::StateConfigSaveError {
            message: String::new(),
        });
        assert_eq!(save_outcome(&state), SaveOutcome::Pending);
        event(OscEvent::StateConfigSaved { saved: true });
        assert_eq!(save_outcome(&state), SaveOutcome::Saved);
    }
}

#[cfg(test)]
mod staged_group_tests {
    use super::*;
    use crate::host::commands::tests::{sent_addresses, state_with_outbox};
    use crate::osc::dispatch::StagedGroup;

    /// A schema with one staged group (`live_input`) and one live one.
    fn schema() -> serde_json::Value {
        serde_json::json!([
            {"key": "room_ratio_rear", "group": {"key": "room", "mode": "live", "i18nKey": "room.title"}},
            {"key": "input_mode", "group": {"key": "live_input", "mode": "staged", "i18nKey": "section.audioInput"}},
            {"key": "live_input_node", "group": {"key": "live_input", "mode": "staged", "i18nKey": "section.audioInput"}},
            {"key": "decode_thread"}
        ])
    }

    #[test]
    fn staged_groups_come_from_the_schema_with_their_pending_flag() {
        let (state, _rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        {
            let mut live = state.inner.lock().unwrap();
            live.options_schema = Some(schema());
            // No host options yet (or the embedded engine): nothing pending.
            assert_eq!(
                live.staged_groups(),
                vec![StagedGroup {
                    key: "live_input".into(),
                    i18n_key: "section.audioInput".into(),
                    pending: false,
                }]
            );
            live.host_options = Some(serde_json::json!({
                "options": {}, "applied": {}, "pending": {"live_input": true}
            }));
            assert!(live.staged_group("live_input").is_some_and(|g| g.pending));
            assert_eq!(
                live.staged_group("room"),
                None,
                "a live group is not staged"
            );
        }
    }

    /// The Apply sends the generic group apply and clears the flag at once,
    /// so a second click is not offered before the renderer's echo.
    #[test]
    fn applying_a_group_sends_it_and_clears_its_flag() {
        let (state, rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        {
            let mut live = state.inner.lock().unwrap();
            live.options_schema = Some(schema());
            live.host_options = Some(serde_json::json!({"pending": {"live_input": true}}));
        }
        apply_option_group(&state, "live_input");
        assert_eq!(
            sent_addresses(&rx),
            vec![osc_contract::CONTROL_OPTIONS_APPLY.to_string()]
        );
        assert!(
            state
                .inner
                .lock()
                .unwrap()
                .staged_group("live_input")
                .is_some_and(|g| !g.pending)
        );
        apply_option_group(&state, "  ");
        assert!(sent_addresses(&rx).is_empty(), "no group, nothing sent");
    }
}
