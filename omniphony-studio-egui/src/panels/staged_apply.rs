//! The Apply of a `staged` group of declared options, drawn from the schema.
//!
//! A staged group's writes wait for an apply (`docs/live-options-registry.md`,
//! "Host options and `Staged` groups"). The renderer publishes which groups
//! are staged in `/state/options_schema` and whether each holds values not
//! applied yet in `/state/host_options`; this row shows the latter and
//! applies the group. It knows no group: a section asks for its own by key,
//! and a group a newer renderer stages gets the same row.
//!
//! It complements a section's own Apply, not replaces it: the Audio Input
//! section still sends its edits as one document and applies them at once
//! (restarting the engine when the source changes kind); this row appears
//! only when values were staged some other way — another client, a script,
//! a dedicated address — and are still waiting.

use egui::Ui;

use crate::app::StudioSpike;
use crate::host::commands::engine;
use crate::i18n::{t, tf};
use crate::ui::widgets;

impl StudioSpike {
    /// The pending-apply row of staged group `key`, or nothing while it has
    /// nothing waiting (or the renderer does not stage it). Drawn at the end
    /// of its section, so appearing never moves a control under the pointer.
    pub(crate) fn staged_apply_row(&mut self, ui: &mut Ui, key: &str) {
        let Some(group) = self
            .host
            .read()
            .staged_group(key)
            .filter(|group| group.pending)
        else {
            return;
        };
        ui.horizontal(|ui| {
            widgets::note(
                ui,
                &tf("options.stagedPending", &[("group", t(&group.i18n_key))]),
            );
            if ui.button(t("options.applyStaged")).clicked() {
                engine::apply_option_group(&self.host, &group.key);
            }
        });
    }
}
