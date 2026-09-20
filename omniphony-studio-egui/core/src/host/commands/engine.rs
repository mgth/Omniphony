//! Engine-level controls: renderer config save/reload, log level, ramp mode,
//! dynamic-range-control (DRC) tuning and the layout-export trigger.
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::host::channels::{CoordMode, Family, PlacementMode};
use crate::osc_contract;

pub fn control_save_config(state: &SharedState) {
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

/// The Save button: the model remembers that a save was asked for — the
/// footer's indicator reads it — and the renderer is told. The bootstrap path
/// of the input apply wants only the message, and calls
/// [`control_save_config`].
pub fn request_save_config(state: &SharedState) {
    state.inner.lock().unwrap().save_requested = true;
    control_save_config(state);
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
/// Pick the object generator, or `""` for none.
///
/// Its own command because changing it drops state: the renderer forgets the
/// previous generator's parameter overrides, so the local copy has to go with
/// it or the form would show the old generator's values under the new one's
/// name until the next snapshot.
pub fn set_object_generator(state: &SharedState, id: &str) {
    state
        .inner
        .lock()
        .unwrap()
        .app
        .live_options
        .object_generator_params = None;
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

/// Set a live object-generator parameter (PAD: `strength` / `hpf_hz` /
/// `gain_db`). Sent as `[key, value]`; the renderer clamps and applies it live.
/// Remember a live parameter in the model, so the slider that set it reads
/// its own value back instead of snapping until the renderer's echo arrives.
fn remember_param(params: &mut Option<serde_json::Value>, key: &str, value: f64) {
    let params = params.get_or_insert_with(|| serde_json::Value::Object(Default::default()));
    if let Some(map) = params.as_object_mut() {
        map.insert(key.to_owned(), serde_json::json!(value));
    }
}

/// A generator parameter, remembered and sent.
pub fn set_object_generator_param(state: &SharedState, key: &str, value: f64) {
    remember_param(
        &mut state
            .inner
            .lock()
            .unwrap()
            .app
            .live_options
            .object_generator_params,
        key,
        value,
    );
    control_object_generator_param(state, key.to_owned(), value as f32);
}

/// A phantom-extraction parameter, remembered and sent.
pub fn set_phantom_extract_param(state: &SharedState, key: &str, value: f64) {
    remember_param(
        &mut state.inner.lock().unwrap().app.live_options.phantom_params,
        key,
        value,
    );
    control_phantom_extract_param(state, key.to_owned(), value as f32);
}

pub fn control_object_generator_param(state: &SharedState, key: String, value: f32) {
    let k = key.trim().to_ascii_lowercase();
    // Any non-empty key is accepted; the renderer validates it against the active
    // generator's declared schema and clamps the value.
    if k.is_empty() || !value.is_finite() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OBJECT_GENERATOR_PARAM.to_string(),
            args: vec![rosc::OscType::String(k), rosc::OscType::Float(value)],
        },
    );
}

/// Set a live phantom-extraction parameter (`strength` / `passes` / `lift`). Sent
/// as `[key, value]`; the renderer clamps and applies it live.
pub fn control_phantom_extract_param(state: &SharedState, key: String, value: f32) {
    let k = key.trim().to_ascii_lowercase();
    if k.is_empty() || !value.is_finite() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_PHANTOM_EXTRACT_PARAM.to_string(),
            args: vec![rosc::OscType::String(k), rosc::OscType::Float(value)],
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
        if family == Family::Generic {
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
        if family == Family::Generic {
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
        set_placement_mode(&state, Family::Dts, Some(PlacementMode::Sphere));
        {
            let live = state.inner.lock().unwrap();
            let dts = family_placement(&live.app, Family::Dts);
            assert_eq!(dts.own_mode, Some(PlacementMode::Sphere));
            assert_eq!(dts.effective_mode, PlacementMode::Sphere);
            assert_eq!(
                family_placement(&live.app, Family::Dolby).effective_mode,
                PlacementMode::Room,
                "another family is untouched"
            );
        }
        set_placement_mode(&state, Family::Dts, None);
        assert_eq!(
            family_placement(&state.inner.lock().unwrap().app, Family::Dts).own_mode,
            None
        );

        let entries = serde_json::json!({ "radius_m": 1.0, "speakers": [
            { "name": "LFE", "coord_mode": "cartesian", "x": 0.0, "y": 1.0, "z": 0.0, "spatialize": false }
        ] });
        set_placement_layout(&state, Family::Dts, entries);
        assert_eq!(
            family_placement(&state.inner.lock().unwrap().app, Family::Dts).layout_source,
            LayoutSource::Own
        );
        clear_placement_layout(&state, Family::Dts);
        assert_eq!(
            family_placement(&state.inner.lock().unwrap().app, Family::Dts).layout_source,
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
            let app = std::mem::take(&mut live.app);
            live.channels.refresh(&app);
            live.app = app;
        }
        switch_placement_to_manual(&state, Family::Auro);
        let live = state.inner.lock().unwrap();
        let auro = family_placement(&live.app, Family::Auro);
        assert_eq!(auro.own_mode, Some(PlacementMode::Manual));
        assert_eq!(auro.layout_source, LayoutSource::Own);
        let ls =
            crate::host::channels::effective_channels_for(&live.channels, &live.app, Family::Auro)
                .into_iter()
                .find(|c| c.name == "Ls")
                .expect("Ls");
        // Auro's built-in mode is sphere: the seed is its nominal direction,
        // kept as a polar entry.
        assert_eq!(ls.coord_mode, CoordMode::Polar);
        assert_eq!((ls.azimuth, ls.elevation), (-110.0, 0.0));
        let family = live.editing_family;
        drop(live);
        select_placement_family(&state, Family::Pcm);
        assert_ne!(state.inner.lock().unwrap().editing_family, family);
    }
}
