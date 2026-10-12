//! Named config profile controls: switch, create, delete and rename.
//!
//! Each command forwards the profile name(s) to the renderer over OSC; the
//! renderer answers with a fresh `/omniphony/state/profiles` broadcast (and,
//! after a switch, the full state bundle plus a topology recompute).

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::osc_contract;

/// Switch profiles. The renderer discards the outgoing profile's unsaved
/// edits, unless `save_first`: then it saves them — the Save button — and
/// switches only when the save succeeded (docs/persistence-policy.md).
pub fn control_profile_switch(state: &SharedState, value: String, save_first: bool) {
    let name = value.trim().to_string();
    if name.is_empty() {
        return;
    }
    let _session = state.connection_request.lock().unwrap();
    {
        let mut live = state.inner.lock().unwrap();
        live.layout_context_generation = live.layout_context_generation.wrapping_add(1);
        if save_first {
            live.save_requested = true;
        }
    }
    let mut args = vec![rosc::OscType::String(name)];
    if save_first {
        args.push(rosc::OscType::String("save".into()));
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_PROFILE_SWITCH.to_string(),
            args,
        },
    );
}

pub fn control_profile_create(state: &SharedState, value: String) {
    let name = value.trim().to_string();
    if name.is_empty() {
        return;
    }
    let _session = state.connection_request.lock().unwrap();
    {
        let mut live = state.inner.lock().unwrap();
        live.layout_context_generation = live.layout_context_generation.wrapping_add(1);
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_PROFILE_CREATE.to_string(),
            value: name,
        },
    );
}

pub fn control_profile_delete(state: &SharedState, value: String) {
    let name = value.trim().to_string();
    if name.is_empty() {
        return;
    }
    let _session = state.connection_request.lock().unwrap();
    {
        let mut live = state.inner.lock().unwrap();
        live.layout_context_generation = live.layout_context_generation.wrapping_add(1);
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_PROFILE_DELETE.to_string(),
            value: name,
        },
    );
}

pub fn control_profile_rename(state: &SharedState, old: String, new: String) {
    let old = old.trim().to_string();
    let new = new.trim().to_string();
    if old.is_empty() || new.is_empty() {
        return;
    }
    let _session = state.connection_request.lock().unwrap();
    {
        let mut live = state.inner.lock().unwrap();
        live.layout_context_generation = live.layout_context_generation.wrapping_add(1);
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_PROFILE_RENAME.to_string(),
            args: vec![rosc::OscType::String(old), rosc::OscType::String(new)],
        },
    );
}

/// A small read model that lets any frontend draw profiles without the host.
#[derive(Default)]
pub struct Snapshot {
    pub active: Option<String>,
    pub names: Vec<String>,
    /// The renderer holds unsaved edits, which a switch would discard.
    pub unsaved: bool,
}

impl Snapshot {
    pub fn of(app: &crate::model::app_state::AppState) -> Self {
        Self {
            active: app.active_profile.clone(),
            names: app.profile_names.clone(),
            unsaved: false,
        }
    }

    /// The read model with the renderer's unsaved state, which needs the
    /// connection as well as the model.
    pub fn read(state: &SharedState) -> Self {
        Self {
            unsaved: super::engine::has_unsaved_edits(state),
            ..Self::of(&state.read().app)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameEditor {
    Create,
    Rename(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Switch, discarding any unsaved edit of the outgoing profile.
    Switch(String),
    /// Save the unsaved edits into the outgoing profile, then switch.
    SaveAndSwitch(String),
    Submit {
        editor: NameEditor,
        name: String,
    },
    Delete(String),
}

#[derive(Debug, PartialEq, Eq)]
enum Resolved {
    Switch(String),
    SaveAndSwitch(String),
    CreateAndSwitch(String),
    Rename(String, String),
    SwitchAndDelete { keep: String, delete: String },
}

fn resolve(action: Action, snapshot: &Snapshot) -> Option<Resolved> {
    let exists = |name: &str| snapshot.names.iter().any(|n| n == name);
    match action {
        Action::Switch(name) => exists(&name).then_some(Resolved::Switch(name)),
        Action::SaveAndSwitch(name) => exists(&name).then_some(Resolved::SaveAndSwitch(name)),
        Action::Submit { editor, name } => {
            let name = name.trim().to_owned();
            if name.is_empty() {
                return None;
            }
            match editor {
                NameEditor::Create if exists(&name) => Some(Resolved::Switch(name)),
                NameEditor::Create => Some(Resolved::CreateAndSwitch(name)),
                NameEditor::Rename(old) => (snapshot.active.as_deref() == Some(old.as_str())
                    && old != name
                    && !exists(&name))
                .then_some(Resolved::Rename(old, name)),
            }
        }
        Action::Delete(delete) => {
            if !exists(&delete) {
                return None;
            }
            let keep = snapshot.names.iter().find(|n| *n != &delete)?.clone();
            Some(Resolved::SwitchAndDelete { keep, delete })
        }
    }
}

/// Revalidate against the latest renderer echo, then send in the required
/// order. Do not hold the model lock while sending, or optimistically mutate it.
pub fn apply(state: &SharedState, action: Action) {
    let resolved = resolve(action, &Snapshot::of(&state.read().app));
    match resolved {
        Some(Resolved::Switch(name)) => control_profile_switch(state, name, false),
        Some(Resolved::SaveAndSwitch(name)) => control_profile_switch(state, name, true),
        // The new profile is made from what the user hears, unsaved edits
        // included, so switching to it discards nothing.
        Some(Resolved::CreateAndSwitch(name)) => {
            control_profile_create(state, name.clone());
            control_profile_switch(state, name, false);
        }
        Some(Resolved::Rename(old, name)) => control_profile_rename(state, old, name),
        // The profile being deleted takes its unsaved edits with it; the
        // delete confirmation already asked.
        Some(Resolved::SwitchAndDelete { keep, delete }) => {
            control_profile_switch(state, keep, false);
            control_profile_delete(state, delete);
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Snapshot {
        Snapshot {
            active: Some("A".into()),
            names: vec!["A".into(), "B".into()],
            ..Default::default()
        }
    }
    fn submit(editor: NameEditor, name: &str) -> Action {
        Action::Submit {
            editor,
            name: name.into(),
        }
    }
    #[test]
    fn creation_trims_and_switches_an_existing_name() {
        assert_eq!(
            resolve(submit(NameEditor::Create, "  B  "), &snapshot()),
            Some(Resolved::Switch("B".into()))
        );
        assert_eq!(
            resolve(submit(NameEditor::Create, " C "), &snapshot()),
            Some(Resolved::CreateAndSwitch("C".into()))
        );
        assert_eq!(resolve(submit(NameEditor::Create, "  "), &snapshot()), None);
    }
    #[test]
    fn a_stale_rename_cannot_rename_the_new_selection_or_overwrite_a_profile() {
        for name in ["A", "B"] {
            assert_eq!(
                resolve(submit(NameEditor::Rename("A".into()), name), &snapshot()),
                None
            );
        }
        let mut current = snapshot();
        current.active = Some("B".into());
        assert_eq!(
            resolve(submit(NameEditor::Rename("A".into()), "C"), &current),
            None
        );
        assert_eq!(
            resolve(submit(NameEditor::Rename("A".into()), "C"), &snapshot()),
            Some(Resolved::Rename("A".into(), "C".into()))
        );
    }
    #[test]
    fn deletion_revalidates_the_remaining_profiles() {
        let mut current = snapshot();
        assert_eq!(
            resolve(Action::Delete("A".into()), &current),
            Some(Resolved::SwitchAndDelete {
                keep: "B".into(),
                delete: "A".into()
            })
        );
        current.names = vec!["A".into()];
        assert_eq!(resolve(Action::Delete("A".into()), &current), None);
        assert_eq!(resolve(Action::Delete("B".into()), &current), None);
        assert_eq!(resolve(Action::Switch("B".into()), &current), None);
    }

    /// "Save and switch" asks the renderer to save before it switches, in one
    /// message, so a failed save cannot be followed by the switch anyway.
    #[test]
    fn save_and_switch_is_one_message_carrying_the_save() {
        let (state, rx) =
            crate::host::commands::tests::state_with_outbox(std::sync::Arc::new(|| {}));
        {
            let mut live = state.inner.lock().unwrap();
            live.app.active_profile = Some("A".into());
            live.app.profile_names = vec!["A".into(), "B".into()];
        }
        apply(&state, Action::SaveAndSwitch("B".into()));
        let sent: Vec<_> = rx.try_iter().collect();
        assert_eq!(sent.len(), 1);
        let crate::osc::Control::Send { address, args } = &sent[0] else {
            panic!("{sent:?}");
        };
        assert_eq!(address, osc_contract::CONTROL_PROFILE_SWITCH);
        assert_eq!(
            args,
            &vec![
                rosc::OscType::String("B".into()),
                rosc::OscType::String("save".into())
            ]
        );
        assert!(state.inner.lock().unwrap().save_requested);

        apply(&state, Action::Switch("B".into()));
        let crate::osc::Control::Send { args, .. } = rx.try_recv().unwrap() else {
            panic!();
        };
        assert_eq!(args, vec![rosc::OscType::String("B".into())]);
    }
}
