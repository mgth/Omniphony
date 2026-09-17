//! The backend file editor (`controls/script-editor.js`).
//!
//! The file being edited lives on the renderer, not here: the editor asks for
//! its content over OSC and saves it the same way, so editing a scriptable
//! backend works when orender runs on another machine — bytes travel, never a
//! path that means nothing at the other end. The native Browse dialog is
//! therefore shown only when the renderer is this machine.
//!
//! The web hosts CodeMirror and offers eleven of its colour themes, because
//! CodeMirror's default is a light editor dropped into a dark panel. Here the
//! editor is drawn in the Studio's own palette and there is nothing to
//! correct, so the picker has no port: the highlighter below is the whole of
//! it, one Lua pass over the buffer.

use egui::text::LayoutJob;
use egui::{Color32, FontId, RichText, TextFormat, Ui};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::ui::{theme, widgets};

/// What the editor is editing: the param whose file it is, and what the
/// renderer says that file is.
#[derive(Clone, Debug, Default)]
pub struct ScriptEditor {
    pub backend: String,
    pub key: String,
    pub language: Option<String>,
    pub extensions: Vec<String>,
    /// The renderer is this machine, so a local path is meaningful and Browse
    /// is worth offering.
    pub renderer_is_local: bool,
    /// The name field: what a save writes under.
    pub name: String,
    /// The buffer.
    pub text: String,
    pub status: Option<(String, bool)>,
    /// Snapshot of the view buffer at request time, to protect ongoing typing.
    pending: Option<Pending>,
    saved_text: String,
    saved_name: String,
    confirm: Option<Action>,
}

#[derive(Clone, Debug)]
enum Pending {
    Load { text: String },
    Save { text: String, name: String },
}

impl ScriptEditor {
    fn dirty(&self) -> bool {
        self.text != self.saved_text || self.name != self.saved_name
    }
    fn needs_confirmation(&self) -> bool {
        self.dirty() || self.pending.is_some()
    }

    fn request_action(&mut self, action: Action) -> Option<Action> {
        if !matches!(action, Action::Save) && self.needs_confirmation() {
            self.confirm = Some(action);
            None
        } else {
            self.confirm = None;
            Some(action)
        }
    }

    fn cancel_wait(&mut self) {
        self.pending = None;
        self.status = Some((t("backend.file.editor.waitCancelled").to_owned(), false));
    }

    fn accept_content(&mut self, file: crate::osc::dispatch::BackendFile) {
        match self.pending.take() {
            Some(Pending::Load { text }) if text == self.text => {
                self.text = file.content;
                self.name = file.name;
                self.saved_text.clone_from(&self.text);
                self.saved_name.clone_from(&self.name);
                self.status = Some((t("backend.file.editor.loaded").to_owned(), false));
            }
            Some(Pending::Load { .. }) => {
                self.status = Some((t("backend.file.editor.editedDuringLoad").to_owned(), true));
            }
            Some(Pending::Save { text, name }) => {
                self.saved_text = text;
                self.saved_name = name;
                // Acknowledgement is about the submitted revision. Typing and
                // filename edits made since Save belong to the next revision.
                self.status = Some((t("backend.file.editor.saved").to_owned(), false));
            }
            None => {}
        }
    }
}

/// What one run of the highlighter found. Ranges are byte offsets into the
/// source, contiguous and covering it end to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    Plain,
    Comment,
    Str,
    Number,
    Keyword,
}

const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// Split Lua source into coloured runs.
///
/// Deliberately a lexer and not a parser: it has to be right about where a
/// comment or a string ends, because that is what mis-colours the rest of the
/// file, and it has to be cheap enough to run on the buffer every frame the
/// editor is open.
pub fn tokenize(src: &str) -> Vec<(usize, usize, Tok)> {
    let bytes = src.as_bytes();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let push = |out: &mut Vec<(usize, usize, Tok)>, start: usize, end: usize, tok: Tok| {
        if start >= end {
            return;
        }
        // Neighbouring plain runs are one run: a format change per character
        // would be a section in the layout job for every space.
        if let Some(last) = out.last_mut()
            && last.2 == tok
            && last.1 == start
        {
            last.1 = end;
            return;
        }
        out.push((start, end, tok));
    };
    let mut i = 0;
    while i < bytes.len() {
        let rest = &src[i..];
        if rest.starts_with("--") {
            let end = if rest[2..].starts_with("[[") {
                rest[4..]
                    .find("]]")
                    .map(|p| i + 4 + p + 2)
                    .unwrap_or(bytes.len())
            } else {
                rest.find('\n').map(|p| i + p).unwrap_or(bytes.len())
            };
            push(&mut out, i, end, Tok::Comment);
            i = end;
            continue;
        }
        if rest.starts_with("[[") {
            let end = rest[2..]
                .find("]]")
                .map(|p| i + 2 + p + 2)
                .unwrap_or(bytes.len());
            push(&mut out, i, end, Tok::Str);
            i = end;
            continue;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            let mut j = i + 1;
            while j < bytes.len() {
                match bytes[j] {
                    b'\\' => j += 2,
                    // An unterminated string stops at the line end rather than
                    // painting the rest of the file: a half-typed quote is the
                    // normal state of a buffer being edited.
                    b'\n' => break,
                    c if c == quote => {
                        j += 1;
                        break;
                    }
                    _ => j += 1,
                }
            }
            let end = j.min(bytes.len());
            push(&mut out, i, end, Tok::Str);
            i = end;
            continue;
        }
        if bytes[i].is_ascii_digit() {
            let mut j = i;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.' || bytes[j] == b'_')
            {
                j += 1;
            }
            push(&mut out, i, j, Tok::Number);
            i = j;
            continue;
        }
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            let tok = if KEYWORDS.contains(&&src[i..j]) {
                Tok::Keyword
            } else {
                Tok::Plain
            };
            push(&mut out, i, j, tok);
            i = j;
            continue;
        }
        // Anything else is one character of plain text, and multi-byte
        // characters are stepped over whole.
        let step = src[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        push(&mut out, i, i + step, Tok::Plain);
        i += step;
    }
    out
}

fn colour(tok: Tok) -> Color32 {
    match tok {
        Tok::Plain => theme::TEXT,
        Tok::Comment => theme::TEXT_DIM,
        Tok::Str => theme::OK,
        Tok::Number => theme::WARN,
        Tok::Keyword => theme::ACCENT,
    }
}

/// The buffer as a layout job, one section per coloured run.
fn highlight(src: &str, language: Option<&str>, wrap_width: f32) -> LayoutJob {
    let font = FontId::monospace(theme::FONT_SIZE);
    let mut job = LayoutJob {
        wrap: egui::text::TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        ..Default::default()
    };
    // Only Lua is coloured, as in the web: a file of unknown language is shown
    // as it is rather than lit up by another language's rules.
    if language != Some("lua") {
        job.append(
            src,
            0.0,
            TextFormat::simple(font.clone(), colour(Tok::Plain)),
        );
        return job;
    }
    for (start, end, tok) in tokenize(src) {
        job.append(
            &src[start..end],
            0.0,
            TextFormat::simple(font.clone(), colour(tok)),
        );
    }
    job
}

#[derive(Debug, PartialEq)]
enum QuitPrompt {
    None,
    Script,
    AutoTune,
}
fn guard_quit_request(ctx: &egui::Context, script_pending: bool, auto_tune: bool) -> QuitPrompt {
    if !ctx.input(|i| i.viewport().close_requested()) || (!script_pending && !auto_tune) {
        return QuitPrompt::None;
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
    if script_pending {
        QuitPrompt::Script
    } else {
        QuitPrompt::AutoTune
    }
}

impl StudioSpike {
    /// eframe also calls logic while hidden; quit guards must not depend on UI.
    pub(crate) fn guard_unsaved_quit(&mut self, ctx: &egui::Context) {
        match guard_quit_request(
            ctx,
            self.script_editor
                .as_ref()
                .is_some_and(ScriptEditor::needs_confirmation),
            self.auto_tune_running(),
        ) {
            QuitPrompt::Script => {
                if let Some(editor) = &mut self.script_editor {
                    editor.confirm = Some(Action::Quit);
                }
            }
            QuitPrompt::AutoTune => self.auto_tune_quit_asked = true,
            QuitPrompt::None => {}
        }
    }

    /// Open the editor for one backend file param.
    pub(crate) fn open_script_editor(
        &mut self,
        backend: &str,
        key: &str,
        language: Option<String>,
        extensions: Vec<String>,
    ) {
        self.script_editor = Some(ScriptEditor {
            backend: backend.to_owned(),
            key: key.to_owned(),
            language,
            extensions,
            renderer_is_local: crate::host::commands::app::renderer_is_local(&self.host),
            ..Default::default()
        });
        crate::host::commands::render::backend_file_list(&self.host, backend.to_owned());
        self.request_backend_file(None);
    }

    /// Ask the renderer for content: a name previews a managed file, none
    /// reads the param's current handle.
    fn request_backend_file(&mut self, name: Option<String>) {
        let Some(editor) = &mut self.script_editor else {
            return;
        };
        editor.status = Some((t("backend.file.editor.loading").to_owned(), false));
        editor.pending = Some(Pending::Load {
            text: editor.text.clone(),
        });
        let (backend, key) = (editor.backend.clone(), editor.key.clone());
        crate::host::commands::render::backend_file_get(&self.host, backend, key, name);
    }

    pub(crate) fn script_editor_modal(&mut self, ctx: &egui::Context) {
        self.poll_backend_file();
        if self.script_editor.is_none() {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("script-editor"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                let content = ctx.content_rect();
                ui.set_width((content.width() * 0.92).min(840.0));
                self.script_editor_body(ui, (content.height() * 0.88).min(660.0));
            });
        if modal.should_close() {
            self.request_script_action(Action::Close, ctx);
        }
    }

    fn request_script_action(&mut self, action: Action, ctx: &egui::Context) {
        let ready = self
            .script_editor
            .as_mut()
            .and_then(|editor| editor.request_action(action));
        if let Some(action) = ready {
            self.apply_script_action(action, ctx);
        }
        ctx.request_repaint();
    }

    fn script_editor_body(&mut self, ui: &mut Ui, height: f32) {
        let Some(editor) = &self.script_editor else {
            return;
        };
        let (backend, key) = (editor.backend.clone(), editor.key.clone());
        let renderer_is_local = editor.renderer_is_local;
        let files: Vec<String> = {
            let live = self.host.read();
            live.backend_files
                .get(&backend)
                .cloned()
                .unwrap_or_default()
        };
        let mut close = false;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("backend.file.editor.title"))
                    .size(theme::FONT_SIZE_TITLE)
                    .color(theme::TEXT_STRONG),
            );
            ui.label(
                RichText::new(format!("{backend} · {key}"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t("backend.file.editor.close")).clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.request_script_action(Action::Close, ui.ctx());
        }
        let mut confirmed = None;
        if let Some(editor) = &mut self.script_editor
            && editor.confirm.is_some()
        {
            ui.colored_label(theme::WARN, t("backend.file.editor.discardPrompt"));
            ui.horizontal(|ui| {
                if ui.button(t("backend.file.editor.keepEditing")).clicked() {
                    editor.confirm = None;
                }
                if ui.button(t("backend.file.editor.discard")).clicked() {
                    confirmed = editor.confirm.take();
                }
            });
        }
        if let Some(action) = confirmed {
            self.apply_script_action(action, ui.ctx());
        }
        if self.script_editor.is_none() {
            return;
        }
        ui.add_space(theme::ROW_GAP);

        let mut open: Option<String> = None;
        let mut action = None;
        let busy = self
            .script_editor
            .as_ref()
            .is_some_and(|e| e.pending.is_some());
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("script-editor-managed")
                    .selected_text(t("backend.file.editor.pickerDefault"))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for name in &files {
                            if ui.selectable_label(false, name).clicked() {
                                open = Some(name.clone());
                            }
                        }
                    });
                let Some(editor) = &mut self.script_editor else {
                    return;
                };
                ui.add(
                    egui::TextEdit::singleline(&mut editor.name)
                        .desired_width(160.0)
                        .hint_text(t("backend.file.editor.filenamePlaceholder")),
                );
                if renderer_is_local && ui.button(t("backend.file.browse")).clicked() {
                    action = Some(Action::Browse);
                }
                if ui.button(t("backend.file.editor.new")).clicked() {
                    action = Some(Action::New);
                }
                if ui.button(t("backend.file.editor.reload")).clicked() {
                    action = Some(Action::Reload);
                }
                if ui.button(t("backend.file.editor.save")).clicked() {
                    action = Some(Action::Save);
                }
            });
        });

        if busy
            && ui.button(t("backend.file.editor.cancelWait")).clicked()
            && let Some(editor) = &mut self.script_editor
        {
            editor.cancel_wait();
            crate::host::services::backend_files::cancel(&self.host);
        }

        // The editor itself takes what the toolbar and the status line leave.
        let body_height = (height - 90.0).max(160.0);
        let language = self.script_editor.as_ref().and_then(|e| e.language.clone());
        if let Some(editor) = &mut self.script_editor {
            let mut layouter = |ui: &Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
                let job = highlight(buffer.as_str(), language.as_deref(), wrap_width);
                ui.ctx().fonts_mut(|f| f.layout_job(job))
            };
            egui::ScrollArea::vertical()
                .max_height(body_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_sized(
                        [ui.available_width(), body_height],
                        egui::TextEdit::multiline(&mut editor.text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .layouter(&mut layouter),
                    );
                });
            if let Some((text, error)) = &editor.status {
                ui.label(
                    RichText::new(text)
                        .size(theme::FONT_SIZE_SMALL)
                        .color(if *error {
                            theme::ERROR
                        } else {
                            theme::TEXT_MUTED
                        }),
                );
            } else {
                ui.label(RichText::new(" ").size(theme::FONT_SIZE_SMALL));
            }
        }

        if let Some(name) = open {
            self.request_script_action(Action::Open(name), ui.ctx());
        }
        if let Some(action) = action {
            self.request_script_action(action, ui.ctx());
        }
    }

    fn apply_script_action(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Close | Action::Quit => {
                crate::host::services::backend_files::cancel(&self.host);
                self.script_editor = None;
                if matches!(action, Action::Quit) {
                    if self.auto_tune_running() {
                        self.auto_tune_quit_asked = true;
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            Action::Open(name) => {
                if let Some(editor) = &mut self.script_editor {
                    editor.name = name.clone();
                }
                self.request_backend_file(Some(name));
            }
            Action::Browse => {
                let extensions = self
                    .script_editor
                    .as_ref()
                    .map(|e| e.extensions.clone())
                    .unwrap_or_default();
                if let Some(path) = crate::ui::file_dialogs::pick_backend_file_path(extensions) {
                    if let Some(editor) = &mut self.script_editor {
                        editor.name = path.clone();
                    }
                    self.request_backend_file(Some(path));
                }
            }
            Action::New => {
                crate::host::services::backend_files::cancel(&self.host);
                if let Some(editor) = &mut self.script_editor {
                    let ext = editor
                        .extensions
                        .first()
                        .cloned()
                        .unwrap_or("txt".to_owned());
                    editor.name = format!("untitled.{ext}");
                    editor.text.clear();
                    editor.pending = None;
                    editor.status = Some((t("backend.file.editor.new").to_owned(), false));
                }
            }
            Action::Reload => {
                let name = self
                    .script_editor
                    .as_ref()
                    .map(|e| e.name.trim().to_owned())
                    .filter(|n| !n.is_empty());
                self.request_backend_file(name);
            }
            Action::Save => self.save_backend_file(),
        }
    }

    fn save_backend_file(&mut self) {
        let Some(editor) = &mut self.script_editor else {
            return;
        };
        let name = editor.name.trim().to_owned();
        if name.is_empty() {
            editor.status = Some((t("backend.file.editor.nameRequired").to_owned(), true));
            return;
        }
        editor.status = Some((t("backend.file.editor.saving").to_owned(), false));
        editor.pending = Some(Pending::Save {
            text: editor.text.clone(),
            name: name.clone(),
        });
        let (backend, key, content) = (
            editor.backend.clone(),
            editor.key.clone(),
            editor.text.clone(),
        );
        crate::host::commands::render::backend_file_put(&self.host, backend, key, name, content);
    }

    /// Adopt a content message addressed to the open editor. Taking it rather
    /// than reading it is what makes "the answer to my request" well defined:
    /// the field holds one message, and a second editor session must not adopt
    /// the first one's.
    fn poll_backend_file(&mut self) {
        let Some(editor) = &self.script_editor else {
            return;
        };
        if editor.pending.is_none() {
            return;
        }
        let (backend, key) = (editor.backend.clone(), editor.key.clone());
        if let Some(message) =
            crate::host::commands::app::take_backend_file_error(&self.host, &backend, &key)
        {
            if let Some(editor) = &mut self.script_editor {
                editor.pending = None;
                use crate::osc::dispatch::BackendFileFailure;
                let message = match message {
                    BackendFileFailure::Renderer(message) => message,
                    BackendFileFailure::TimedOut => t("backend.file.editor.timedOut").to_owned(),
                    BackendFileFailure::ConnectionChanged => {
                        t("backend.file.editor.connectionChanged").to_owned()
                    }
                };
                editor.status = Some((message, true));
            }
            return;
        }
        if let Some(file) =
            crate::host::commands::app::take_backend_file(&self.host, &backend, &key)
            && let Some(editor) = &mut self.script_editor
        {
            editor.accept_content(file);
        }
    }
}

#[derive(Clone, Debug)]
enum Action {
    Close,
    Quit,
    Open(String),
    Browse,
    New,
    Reload,
    Save,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> crate::osc::dispatch::BackendFile {
        crate::osc::dispatch::BackendFile {
            backend: "script".into(),
            key: "file".into(),
            name: "saved.lua".into(),
            content: "remote".into(),
        }
    }

    #[test]
    fn cancelling_wait_preserves_buffer_and_ignores_the_abandoned_reply() {
        let mut editor = ScriptEditor {
            text: "unsaved".into(),
            name: "next.lua".into(),
            pending: Some(Pending::Save {
                text: "submitted".into(),
                name: "saved.lua".into(),
            }),
            ..Default::default()
        };
        editor.cancel_wait();
        editor.accept_content(reply());
        assert!(editor.pending.is_none());
        assert_eq!(editor.text, "unsaved");
        assert_eq!(editor.name, "next.lua");
    }

    #[test]
    fn save_ack_does_not_replace_newer_typing() {
        let mut editor = ScriptEditor {
            text: "newer revision".into(),
            name: "next.lua".into(),
            pending: Some(Pending::Save {
                text: "submitted".into(),
                name: "saved.lua".into(),
            }),
            ..Default::default()
        };
        editor.accept_content(reply());
        assert_eq!(editor.text, "newer revision");
        assert_eq!(editor.name, "next.lua");
        assert!(editor.pending.is_none());
        assert!(!editor.status.unwrap().1);
    }

    #[test]
    fn load_preserves_typing_started_after_request() {
        let mut editor = ScriptEditor {
            text: "typed".into(),
            pending: Some(Pending::Load {
                text: String::new(),
            }),
            ..Default::default()
        };
        editor.accept_content(reply());
        assert_eq!(editor.text, "typed");
        assert!(editor.status.unwrap().1);
    }

    #[test]
    fn load_replaces_unchanged_buffer_once() {
        let mut editor = ScriptEditor {
            pending: Some(Pending::Load {
                text: String::new(),
            }),
            ..Default::default()
        };
        editor.accept_content(reply());
        assert_eq!(editor.text, "remote");
        editor.text = "typed".into();
        editor.accept_content(reply());
        assert_eq!(editor.text, "typed");
    }

    #[test]
    fn destructive_actions_require_confirmation_and_save_acks_track_revisions() {
        let mut editor = ScriptEditor::default();
        assert!(editor.request_action(Action::Close).is_some());
        editor.text = "unsaved".into();
        for action in [
            Action::Close,
            Action::Quit,
            Action::New,
            Action::Reload,
            Action::Browse,
            Action::Open("another.lua".into()),
        ] {
            assert!(editor.request_action(action).is_none());
            assert!(editor.confirm.is_some());
            editor.confirm = None; // Keep editing
            assert_eq!(editor.text, "unsaved");
        }
        assert!(editor.request_action(Action::Save).is_some());
        editor.name = "saved.lua".into();
        editor.pending = Some(Pending::Save {
            text: editor.text.clone(),
            name: editor.name.clone(),
        });
        editor.accept_content(reply());
        assert!(!editor.dirty());
        editor.pending = Some(Pending::Save {
            text: editor.text.clone(),
            name: editor.name.clone(),
        });
        editor.text.push_str(" newer");
        editor.accept_content(reply());
        assert!(editor.dirty());
    }

    #[test]
    fn hidden_window_close_is_cancelled_without_a_ui_pass() {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
        viewport.minimized = Some(true);
        viewport.events.push(egui::ViewportEvent::Close);
        for (script, tune, expected) in [
            (true, false, QuitPrompt::Script),
            (true, true, QuitPrompt::Script),
            (false, true, QuitPrompt::AutoTune),
        ] {
            let output = ctx.run_logic(&input, |ctx| {
                assert_eq!(guard_quit_request(ctx, script, tune), expected)
            });
            let commands = &output.viewport_commands[&egui::ViewportId::ROOT];
            assert!(
                commands
                    .iter()
                    .any(|c| matches!(c, egui::ViewportCommand::CancelClose))
            );
            assert!(
                commands
                    .iter()
                    .any(|c| matches!(c, egui::ViewportCommand::Minimized(false)))
            );
        }
        let output = ctx.run_logic(&input, |ctx| {
            assert_eq!(guard_quit_request(ctx, false, false), QuitPrompt::None)
        });
        assert!(output.viewport_commands.is_empty());
    }

    fn kinds(src: &str) -> Vec<(&str, Tok)> {
        tokenize(src)
            .into_iter()
            .map(|(a, b, tok)| (&src[a..b], tok))
            .collect()
    }

    #[test]
    fn the_runs_cover_the_source_exactly() {
        let src = "local x = 1 -- note\n";
        let mut at = 0;
        for (start, end, _) in tokenize(src) {
            assert_eq!(start, at, "a gap or an overlap between runs");
            at = end;
        }
        assert_eq!(at, src.len());
    }

    #[test]
    fn keywords_strings_numbers_and_comments_are_told_apart() {
        assert_eq!(
            kinds("local s = \"hi\" -- why\n"),
            vec![
                ("local", Tok::Keyword),
                (" s = ", Tok::Plain),
                ("\"hi\"", Tok::Str),
                (" ", Tok::Plain),
                ("-- why", Tok::Comment),
                ("\n", Tok::Plain),
            ]
        );
        assert_eq!(kinds("0x1f")[0], ("0x1f", Tok::Number));
        // `local` inside a word is not the keyword.
        assert_eq!(kinds("locally")[0], ("locally", Tok::Plain));
    }

    /// The two ways a run can swallow the rest of the file if its end is got
    /// wrong: a long comment and a quote left open while typing.
    #[test]
    fn a_long_comment_ends_and_an_open_quote_stops_at_the_line() {
        assert_eq!(
            kinds("--[[ a ]] b"),
            vec![("--[[ a ]]", Tok::Comment), (" b", Tok::Plain)]
        );
        assert_eq!(
            kinds("x = \"open\nlocal y"),
            vec![
                ("x = ", Tok::Plain),
                ("\"open", Tok::Str),
                ("\n", Tok::Plain),
                ("local", Tok::Keyword),
                (" y", Tok::Plain),
            ]
        );
        // An escaped quote does not end the string.
        assert_eq!(kinds("\"a\\\"b\"")[0].1, Tok::Str);
    }

    #[test]
    fn a_multibyte_character_is_stepped_over_whole() {
        let src = "-- é\nx";
        assert_eq!(
            kinds(src),
            vec![("-- é", Tok::Comment), ("\nx", Tok::Plain)]
        );
    }
}
