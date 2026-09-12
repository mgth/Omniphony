//! The SOFA HRTF browser (`#sofaBrowserModal`, `controls/sofa-browser.js`).
//!
//! Two views behind one dialog. The default is the local cache — the files
//! already on this machine, no network — where a click activates one, and the
//! per-row buttons send it to a renderer on another machine or delete it. The
//! other is sofacoustics.org, reached only after saying yes once per session,
//! where the Apache index is navigated folder by folder and a click downloads
//! and activates.
//!
//! Everything that touches the network or the disk runs on a worker thread and
//! answers through a channel. A database entry can be hundreds of megabytes,
//! and reading one for its licence line is a full parse: doing either on the
//! frame loop would freeze the window for as long as it takes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Color32, RichText, Sense, Ui};

use crate::app::StudioSpike;
use crate::host::commands::sofa::{self, LocalSofa, SofaEntry, SofaMeta};
use crate::ui::{theme, widgets};

/// Where the browser starts online: the per-subject HRTF sets live under
/// `database/`, not at the root.
const START_PATH: &str = "database/";

/// Which view the dialog is showing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum View {
    /// The local cache.
    #[default]
    Local,
    /// A folder of sofacoustics.org.
    Remote,
    /// "this will connect to sofacoustics.org" — asked once per session.
    OnlineConfirm,
    /// "delete all N cached files?"
    ConfirmDeleteAll,
}

/// A download in flight, shared with the thread doing it.
#[derive(Default)]
pub struct Download {
    bytes: AtomicU64,
    /// The server's Content-Length, or zero when it did not say.
    total: AtomicU64,
    cancel: AtomicBool,
}

/// What a finished job hands back to the frame loop.
pub enum Job {
    Listed(Result<Vec<LocalSofa>, String>),
    Browsed(Result<Vec<SofaEntry>, String>),
    Downloaded {
        name: String,
        remote: String,
        result: Result<(PathBuf, SofaMeta), String>,
    },
    Uploaded {
        name: String,
        result: Result<u32, String>,
    },
    Imported(Result<usize, String>),
}

#[derive(Default)]
pub struct SofaBrowser {
    pub view: View,
    /// Percent-encoded remote path, "" = root.
    pub path: String,
    pub entries: Vec<SofaEntry>,
    pub local: Vec<LocalSofa>,
    /// The status line: its text, and whether it is an error.
    pub status: Option<(String, bool)>,
    /// A job is running: every control that would start another is refused,
    /// exactly as the web's `busy` flag does.
    pub busy: bool,
    pub job: Option<Receiver<Job>>,
    pub download: Option<Arc<Download>>,
    /// Going online is agreed to once, and the agreement lasts the session.
    pub online_consent: bool,
    /// Remote path of the active file, so the entry stays marked while
    /// navigating away and back.
    pub active_remote: String,
}

impl SofaBrowser {
    fn status(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        self.status = if text.is_empty() {
            None
        } else {
            Some((text, error))
        };
    }
}

/// Compact label and severity for a `GLOBAL:License` attribute. SOFA files
/// embed anything from a short identifier to the full licence paragraph; the
/// common cases are classified and the rest truncated, with the full text in
/// the tooltip.
pub fn license_label(raw: &str) -> (String, bool) {
    let t = raw.trim();
    if t.is_empty() {
        return ("no license — ask the author".to_owned(), true);
    }
    let l = t.to_ascii_lowercase();
    let has = |xs: &[&str]| xs.iter().all(|x| l.contains(x));
    if has(&["cc0"]) || has(&["public domain"]) {
        return ("CC0 / public domain".to_owned(), false);
    }
    if has(&["by-nc-sa"]) || has(&["nc-sa"]) {
        return ("CC BY-NC-SA (non-commercial)".to_owned(), true);
    }
    if has(&["by-nc"]) || has(&["non-commercial"]) {
        return ("CC BY-NC (non-commercial)".to_owned(), true);
    }
    if has(&["by-sa"]) {
        return ("CC BY-SA".to_owned(), false);
    }
    if has(&["creativecommons", "/by/"]) {
        return ("CC BY".to_owned(), false);
    }
    if l.starts_with("cc by") || l == "cc-by" {
        return (
            if t.chars().count() > 30 {
                "CC BY".to_owned()
            } else {
                t.to_owned()
            },
            false,
        );
    }
    if has(&["mit license"]) || l == "mit" {
        return ("MIT".to_owned(), false);
    }
    if has(&["no license"]) {
        return ("no license — ask the author".to_owned(), true);
    }
    if t.chars().count() > 60 {
        let short: String = t.chars().take(57).collect();
        return (format!("{short}…"), false);
    }
    (t.to_owned(), false)
}

/// `1.6 MB`, for the progress readout (the listing's sizes come from the
/// server as they are).
fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

impl StudioSpike {
    /// The cache directory: `<config dir>/hrtf`, beside the rest of this
    /// host's state. The path is what goes to the renderer, so a renderer
    /// sharing the filesystem reads the file from here.
    pub(crate) fn sofa_dir(&self) -> PathBuf {
        self.config_dir.join("hrtf")
    }

    /// Open the dialog on the local cache.
    pub(crate) fn open_sofa_browser(&mut self) {
        let mut browser = SofaBrowser {
            path: START_PATH.to_owned(),
            ..Default::default()
        };
        browser.online_consent = self.sofa_browser.as_ref().is_some_and(|b| b.online_consent);
        self.sofa_browser = Some(browser);
        self.list_local_sofa();
    }

    pub(crate) fn sofa_browser_modal(&mut self, ctx: &egui::Context) {
        self.poll_sofa_job();
        if self.sofa_browser.is_none() {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("sofa-browser"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_max_width(560.0);
                self.sofa_browser_body(ui);
            });
        if modal.should_close() {
            self.sofa_browser = None;
        }
    }

    fn sofa_browser_body(&mut self, ui: &mut Ui) {
        let Some(browser) = &self.sofa_browser else {
            return;
        };
        let remote = browser.view == View::Remote;
        let busy = browser.busy;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(if remote {
                    "SOFA HRTF database — sofacoustics.org"
                } else {
                    "SOFA HRTF files — downloaded"
                })
                .size(theme::FONT_SIZE_TITLE)
                .color(theme::TEXT_STRONG),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The one control that changes which half of the dialog you
                // are in, and the only way online.
                let label = if remote {
                    "← Local files"
                } else {
                    "Online database…"
                };
                if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                    self.toggle_sofa_source();
                }
            });
        });
        if remote {
            self.sofa_crumbs(ui);
            widgets::note(
                ui,
                "Licensing varies per database (CC BY, non-commercial, sometimes none) — each \
                 file's embedded license is shown once downloaded. Files are fetched directly \
                 from sofacoustics.org to your machine.",
            );
        }
        if let Some((text, error)) = self.sofa_browser.as_ref().and_then(|b| b.status.clone()) {
            ui.label(
                RichText::new(text)
                    .size(theme::FONT_SIZE_SMALL)
                    .color(if error { theme::ERROR } else { theme::TEXT }),
            );
        }
        self.sofa_download_row(ui);
        ui.add_space(theme::ROW_GAP);
        let max_height = ui.ctx().content_rect().height() * 0.5;
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, theme::SECTION_RULE))
            .corner_radius(theme::CONTROL_RADIUS)
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| match self.sofa_browser.as_ref().map(|b| &b.view) {
                        Some(View::Remote) => self.sofa_remote_list(ui),
                        Some(View::OnlineConfirm) => self.sofa_online_confirm(ui),
                        Some(View::ConfirmDeleteAll) => self.sofa_confirm_delete_all(ui),
                        _ => self.sofa_local_list(ui),
                    });
            });
        ui.add_space(theme::PANEL_GAP);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(crate::i18n::t("common.close")).clicked() {
                self.sofa_browser = None;
            }
        });
    }

    /// `data / database / hutubs` — every segment navigates back to itself.
    fn sofa_crumbs(&mut self, ui: &mut Ui) {
        let Some(browser) = &self.sofa_browser else {
            return;
        };
        let path = browser.path.clone();
        let mut go: Option<String> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if ui
                .link(RichText::new("data").size(theme::FONT_SIZE_SMALL))
                .clicked()
            {
                go = Some(String::new());
            }
            let mut acc = String::new();
            for seg in path.split('/').filter(|s| !s.is_empty()) {
                acc.push_str(seg);
                acc.push('/');
                ui.label(
                    RichText::new("/")
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_DIM),
                );
                if ui
                    .link(RichText::new(sofa::percent_decode(seg)).size(theme::FONT_SIZE_SMALL))
                    .clicked()
                {
                    go = Some(acc.clone());
                }
            }
        });
        if let Some(path) = go {
            self.browse_sofa(&path);
        }
    }

    /// The progress bar, only while a download is in flight.
    fn sofa_download_row(&mut self, ui: &mut Ui) {
        let Some(download) = self.sofa_browser.as_ref().and_then(|b| b.download.clone()) else {
            return;
        };
        let bytes = download.bytes.load(Ordering::Relaxed);
        let total = download.total.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            let width = ui.available_width() - 130.0;
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(width.max(60.0), 8.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, 4.0, Color32::from_white_alpha(20));
            // An unknown length has no fraction to show: the bar fills, dimmed,
            // and the byte counter carries the news that something is moving.
            let (fraction, colour) = if total > 0 {
                ((bytes as f32 / total as f32).clamp(0.0, 1.0), theme::ACCENT)
            } else {
                (1.0, theme::ACCENT.gamma_multiply(0.35))
            };
            let mut filled = rect;
            filled.set_width(rect.width() * fraction);
            ui.painter().rect_filled(filled, 4.0, colour);
            ui.label(
                RichText::new(if total > 0 {
                    format!("{} / {}", mb(bytes), mb(total))
                } else {
                    mb(bytes)
                })
                .size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_MUTED),
            );
            // Deliberately not gated by `busy`: cancelling is only meaningful
            // while a download is in flight, which is exactly when busy is set.
            if ui.button("Cancel").clicked() {
                download.cancel.store(true, Ordering::Relaxed);
            }
        });
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn sofa_local_list(&mut self, ui: &mut Ui) {
        let Some(browser) = &self.sofa_browser else {
            return;
        };
        let busy = browser.busy;
        let files = browser.local.clone();
        let active = self.active_sofa_path();
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !files.is_empty()
                    && ui
                        .add_enabled(!busy, egui::Button::new("🗑 Delete all"))
                        .clicked()
                    && let Some(browser) = &mut self.sofa_browser
                {
                    browser.view = View::ConfirmDeleteAll;
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("＋ Import…"))
                    .on_hover_text("Copy .sofa files from this machine into the cache")
                    .clicked()
                {
                    self.import_sofa_from_disk();
                }
            });
        });
        ui.separator();
        if files.is_empty() {
            widgets::note(
                ui,
                "No downloaded SOFA files yet — use “Online database…” or “＋ Import…”.",
            );
            return;
        }
        let mut activate: Option<PathBuf> = None;
        let mut upload: Option<LocalSofa> = None;
        let mut delete: Option<LocalSofa> = None;
        for file in &files {
            let is_active = active.as_deref() == Some(file.path.as_path());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    let name = if is_active {
                        format!("🎧 {}  ✓ active", file.name)
                    } else {
                        format!("🎧 {}", file.name)
                    };
                    if ui
                        .add(
                            egui::Label::new(RichText::new(name).size(theme::FONT_SIZE).color(
                                if is_active {
                                    theme::ACCENT
                                } else {
                                    theme::TEXT
                                },
                            ))
                            .selectable(false)
                            .sense(Sense::click()),
                        )
                        .clicked()
                        && !busy
                    {
                        activate = Some(file.path.clone());
                    }
                    license_line(ui, &file.meta);
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("🗑"))
                        .on_hover_text("Delete this cached file")
                        .clicked()
                    {
                        delete = Some(file.clone());
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("⇪"))
                        .on_hover_text(
                            "Send to the renderer over OSC and activate there (for a renderer \
                             on another machine)",
                        )
                        .clicked()
                    {
                        upload = Some(file.clone());
                    }
                    ui.label(
                        RichText::new(&file.size)
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_MUTED),
                    );
                });
            });
        }
        if let Some(path) = activate {
            self.activate_sofa(&path);
        }
        if let Some(file) = upload {
            self.upload_sofa(&file);
        }
        if let Some(file) = delete {
            self.delete_sofa(&[file.path.clone()], &format!("Deleted {}.", file.name));
        }
    }

    fn sofa_remote_list(&mut self, ui: &mut Ui) {
        let Some(browser) = &self.sofa_browser else {
            return;
        };
        let busy = browser.busy;
        let path = browser.path.clone();
        let active_remote = browser.active_remote.clone();
        let entries = browser.entries.clone();
        let mut go: Option<String> = None;
        let mut download: Option<SofaEntry> = None;
        if !path.is_empty()
            && ui
                .add(
                    egui::Label::new(
                        // The web's "⬑" is not in the bundled faces, and a
                        // missing glyph draws as a box: the plain up arrow is.
                        RichText::new("↑ ..")
                            .size(theme::FONT_SIZE)
                            .color(theme::TEXT_MUTED),
                    )
                    .selectable(false)
                    .sense(Sense::click()),
                )
                .clicked()
        {
            let mut parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            parts.pop();
            go = Some(if parts.is_empty() {
                String::new()
            } else {
                format!("{}/", parts.join("/"))
            });
        }
        if entries.is_empty() {
            widgets::note(ui, "(no subfolders or .sofa files here)");
        }
        for entry in &entries {
            let is_active = !entry.dir && format!("{path}{}", entry.href) == active_remote;
            ui.horizontal(|ui| {
                let label = if entry.dir {
                    format!("📁 {}/", entry.name)
                } else if is_active {
                    format!("🎧 {}  ✓ active", entry.name)
                } else {
                    format!("🎧 {}", entry.name)
                };
                if ui
                    .add(
                        egui::Label::new(RichText::new(label).size(theme::FONT_SIZE).color(
                            if is_active {
                                theme::ACCENT
                            } else {
                                theme::TEXT
                            },
                        ))
                        .selectable(false)
                        .sense(Sense::click()),
                    )
                    .clicked()
                    && !busy
                {
                    if entry.dir {
                        go = Some(format!("{path}{}", entry.href));
                    } else {
                        download = Some(entry.clone());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !entry.dir {
                        ui.label(
                            RichText::new(&entry.size)
                                .size(theme::FONT_SIZE_SMALL)
                                .color(theme::TEXT_MUTED),
                        );
                    }
                });
            });
        }
        if let Some(path) = go {
            self.browse_sofa(&path);
        }
        if let Some(entry) = download {
            self.download_sofa(&entry);
        }
    }

    fn sofa_online_confirm(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new(
                "This will connect to sofacoustics.org (the public SOFA conventions database) \
                 to browse and download HRTF files over the internet. Nothing is sent besides \
                 the directory and file requests.",
            )
            .size(theme::FONT_SIZE)
            .color(theme::TEXT),
        );
        ui.add_space(theme::PANEL_GAP);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Connect").clicked() {
                    let path = if let Some(browser) = &mut self.sofa_browser {
                        browser.online_consent = true;
                        browser.view = View::Remote;
                        browser.path.clone()
                    } else {
                        return;
                    };
                    self.browse_sofa(&path);
                }
                if ui.button("Cancel").clicked() {
                    self.list_local_sofa();
                }
            });
        });
    }

    fn sofa_confirm_delete_all(&mut self, ui: &mut Ui) {
        let files: Vec<LocalSofa> = self
            .sofa_browser
            .as_ref()
            .map(|b| b.local.clone())
            .unwrap_or_default();
        ui.label(
            RichText::new(format!(
                "Delete all {} cached SOFA files? The currently active HRTF stays loaded in the \
                 renderer until you switch.",
                files.len()
            ))
            .size(theme::FONT_SIZE)
            .color(theme::TEXT),
        );
        ui.add_space(theme::PANEL_GAP);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(RichText::new("Delete all").color(theme::ERROR))
                    .clicked()
                {
                    let paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
                    let note = format!("Deleted {} files.", paths.len());
                    self.delete_sofa(&paths, &note);
                }
                if ui.button("Cancel").clicked() {
                    self.list_local_sofa();
                }
            });
        });
    }

    /// The active file, as the renderer reports it (`binaural.hrtfSofaPath`),
    /// so the highlight survives a Studio restart.
    fn active_sofa_path(&self) -> Option<PathBuf> {
        let live = self.live.lock().unwrap();
        let path = live
            .app
            .binaural
            .as_ref()?
            .get("hrtfSofaPath")?
            .as_str()
            .filter(|s| !s.is_empty())?;
        Some(PathBuf::from(path))
    }

    fn toggle_sofa_source(&mut self) {
        let Some(browser) = &mut self.sofa_browser else {
            return;
        };
        if browser.busy {
            return;
        }
        match browser.view {
            View::Remote => self.list_local_sofa(),
            _ if browser.online_consent => {
                browser.view = View::Remote;
                let path = browser.path.clone();
                self.browse_sofa(&path);
            }
            _ => {
                browser.view = View::OnlineConfirm;
                browser.status = None;
            }
        }
    }

    // ── the jobs ────────────────────────────────────────────────────────────

    /// Start `work` on a thread, and mark the browser busy until it answers.
    fn start_sofa_job(&mut self, work: impl FnOnce() -> Job + Send + 'static) {
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            // The answer arrives while nothing on screen is moving, so ask for
            // the frame that will show it.
            ctx.request_repaint();
        });
        if let Some(browser) = &mut self.sofa_browser {
            browser.busy = true;
            browser.job = Some(rx);
        }
    }

    pub(crate) fn list_local_sofa(&mut self) {
        if let Some(browser) = &mut self.sofa_browser {
            browser.view = View::Local;
            browser.status = None;
        }
        let dir = self.sofa_dir();
        self.start_sofa_job(move || Job::Listed(sofa::list_local(&dir)));
    }

    fn browse_sofa(&mut self, path: &str) {
        let path = path.to_owned();
        if let Some(browser) = &mut self.sofa_browser {
            browser.view = View::Remote;
            browser.path = path.clone();
            browser.status(String::from("Loading…"), false);
        }
        self.start_sofa_job(move || Job::Browsed(sofa::browse(&path)));
    }

    fn download_sofa(&mut self, entry: &SofaEntry) {
        let dir = self.sofa_dir();
        let download = Arc::new(Download::default());
        let name = entry.name.clone();
        let remote = format!(
            "{}{}",
            self.sofa_browser
                .as_ref()
                .map(|b| b.path.clone())
                .unwrap_or_default(),
            entry.href
        );
        if let Some(browser) = &mut self.sofa_browser {
            browser.status(format!("Downloading {name} ({})…", entry.size), false);
            browser.download = Some(download.clone());
        }
        let job_name = name.clone();
        let job_remote = remote.clone();
        self.start_sofa_job(move || {
            let progress = |bytes: u64, total: Option<u64>| {
                download.bytes.store(bytes, Ordering::Relaxed);
                download.total.store(total.unwrap_or(0), Ordering::Relaxed);
            };
            let result = sofa::download(&dir, &job_remote, &download.cancel, progress)
                // The licence is read here, on the same thread, because a
                // fresh download has no sidecar yet and parsing is the slow
                // part of the whole operation.
                .map(|path| {
                    let meta = sofa::file_meta(&path);
                    (path, meta)
                });
            Job::Downloaded {
                name: job_name,
                remote: job_remote,
                result,
            }
        });
    }

    fn upload_sofa(&mut self, file: &LocalSofa) {
        let dir = self.sofa_dir();
        let path = file.path.clone();
        let name = file.name.clone();
        let tx = self.host.osc_tx.clone();
        if let Some(browser) = &mut self.sofa_browser {
            browser.status(format!("Uploading {name} to the renderer…"), false);
        }
        self.start_sofa_job(move || Job::Uploaded {
            name,
            result: sofa::upload_to_renderer(&tx, &dir, &path),
        });
    }

    fn delete_sofa(&mut self, paths: &[PathBuf], note: &str) {
        let dir = self.sofa_dir();
        let paths = paths.to_vec();
        match sofa::delete_local(&dir, &paths) {
            Ok(()) => {
                self.list_local_sofa();
                if let Some(browser) = &mut self.sofa_browser {
                    browser.status(note.to_owned(), false);
                }
            }
            Err(error) => {
                if let Some(browser) = &mut self.sofa_browser {
                    browser.status(error, true);
                }
            }
        }
    }

    fn import_sofa_from_disk(&mut self) {
        let Some(picked) = rfd::FileDialog::new()
            .set_title("Import SOFA files")
            .add_filter("SOFA", &["sofa"])
            .pick_files()
        else {
            return;
        };
        let dir = self.sofa_dir();
        self.start_sofa_job(move || {
            let mut done = 0usize;
            for src in &picked {
                if let Err(error) = sofa::import_local(&dir, src) {
                    return Job::Imported(Err(error));
                }
                done += 1;
            }
            Job::Imported(Ok(done))
        });
    }

    /// Tell the renderer to use this file, and mark it active.
    fn activate_sofa(&mut self, path: &Path) {
        crate::host::commands::binaural::control_hrir_source(
            &self.host,
            format!("sofa:{}", path.display()),
        );
    }

    fn poll_sofa_job(&mut self) {
        let Some(rx) = self.sofa_browser.as_ref().and_then(|b| b.job.as_ref()) else {
            return;
        };
        let job = match rx.try_recv() {
            Ok(job) => job,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                if let Some(browser) = &mut self.sofa_browser {
                    browser.job = None;
                    browser.busy = false;
                }
                return;
            }
        };
        if let Some(browser) = &mut self.sofa_browser {
            browser.job = None;
            browser.busy = false;
        }
        match job {
            Job::Listed(Ok(files)) => {
                if let Some(browser) = &mut self.sofa_browser {
                    browser.local = files;
                }
            }
            Job::Browsed(Ok(entries)) => {
                if let Some(browser) = &mut self.sofa_browser {
                    browser.entries = entries;
                    browser.status = None;
                }
            }
            Job::Listed(Err(error)) | Job::Browsed(Err(error)) => {
                if let Some(browser) = &mut self.sofa_browser {
                    browser.status(error, true);
                }
            }
            Job::Downloaded {
                name,
                remote,
                result,
            } => {
                if let Some(browser) = &mut self.sofa_browser {
                    browser.download = None;
                }
                match result {
                    Ok((path, meta)) => {
                        self.activate_sofa(&path);
                        let (label, warn) = license_label(&meta.license);
                        if let Some(browser) = &mut self.sofa_browser {
                            browser.active_remote = remote;
                            browser.status(format!("✓ Active: {name} — ⚖ {label}"), warn);
                        }
                    }
                    Err(error) => {
                        let cancelled = error.contains("cancelled");
                        if let Some(browser) = &mut self.sofa_browser {
                            if cancelled {
                                browser.status(String::from("Download cancelled."), false);
                            } else {
                                browser.status(error, true);
                            }
                        }
                    }
                }
            }
            Job::Uploaded { name, result } => {
                if let Some(browser) = &mut self.sofa_browser {
                    match result {
                        Ok(chunks) => browser.status(
                            format!(
                                "✓ Sent {name} ({chunks} chunks) — the renderer stores and \
                                 activates it (see the HRTF file line)."
                            ),
                            false,
                        ),
                        Err(error) => browser.status(error, true),
                    }
                }
            }
            Job::Imported(result) => {
                let note = match result {
                    Ok(done) => Ok(format!(
                        "✓ Imported {done} file{}.",
                        if done > 1 { "s" } else { "" }
                    )),
                    Err(error) => Err(error),
                };
                self.list_local_sofa();
                if let Some(browser) = &mut self.sofa_browser {
                    match note {
                        Ok(text) => browser.status(text, false),
                        Err(error) => browser.status(error, true),
                    }
                }
            }
        }
    }
}

/// `⚖ CC BY-NC (non-commercial) — Acoustics Research Institute`, with the full
/// licence text and the author's contact in the tooltip.
fn license_line(ui: &mut Ui, meta: &SofaMeta) {
    let (label, warn) = license_label(&meta.license);
    let mut line = format!("⚖ {label}");
    if !meta.organization.is_empty() {
        line.push_str(" — ");
        line.push_str(&meta.organization);
    }
    let response = ui.label(
        RichText::new(line)
            .size(theme::FONT_SIZE_SMALL)
            .color(if warn { theme::WARN } else { theme::TEXT_DIM }),
    );
    let mut tip = meta.license.trim().to_owned();
    if !meta.author.is_empty() {
        if !tip.is_empty() {
            tip.push('\n');
        }
        tip.push_str(&format!("Contact: {}", meta.author));
    }
    if !tip.is_empty() {
        response.on_hover_text(tip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classifier's job is to say "you may not ship this" at a glance: the
    /// non-commercial variants and a missing licence are the warnings, the
    /// permissive ones are not.
    #[test]
    fn non_commercial_and_missing_licences_warn() {
        assert_eq!(
            license_label("CC BY-NC-SA 4.0"),
            ("CC BY-NC-SA (non-commercial)".to_owned(), true)
        );
        assert_eq!(
            license_label("Creative Commons non-commercial"),
            ("CC BY-NC (non-commercial)".to_owned(), true)
        );
        assert_eq!(
            license_label("  "),
            ("no license — ask the author".to_owned(), true)
        );
        assert_eq!(
            license_label("CC0"),
            ("CC0 / public domain".to_owned(), false)
        );
        assert_eq!(
            license_label("https://creativecommons.org/licenses/by/4.0/"),
            ("CC BY".to_owned(), false)
        );
        assert_eq!(license_label("MIT"), ("MIT".to_owned(), false));
    }

    /// A licence paragraph is truncated rather than allowed to push the row
    /// out of the dialog; the full text stays in the tooltip.
    #[test]
    fn a_licence_paragraph_is_truncated() {
        let long = "x".repeat(120);
        let (label, warn) = license_label(&long);
        assert_eq!(label.chars().count(), 58);
        assert!(label.ends_with('…') && !warn);
        // A short one is shown as it is.
        assert_eq!(license_label("Ask the lab").0, "Ask the lab");
    }
}
