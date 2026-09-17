//! Native picker state and presentation only. Paths, file bytes, normalization
//! and renderer mutations belong to the core. Polled by App::logic even when
//! the speakers panel is hidden; neither polling path waits for a result.
use crate::{
    host::commands::{
        SharedState,
        layout_io::{self, ImportRequest},
    },
    i18n::tf,
    model::layouts::Layout,
};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        mpsc::{Receiver, TryRecvError},
    },
    task::{Context, Poll, Wake, Waker},
};

type Picker = Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>;
enum Operation {
    Import {
        request: ImportRequest,
        remember: bool,
    },
    Export(Layout),
}
enum Pending {
    Directory {
        result: Receiver<Option<PathBuf>>,
        operation: Operation,
    },
    Picker {
        future: Picker,
        operation: Operation,
    },
    Read {
        result: Receiver<Result<Layout, String>>,
        request: ImportRequest,
        path: PathBuf,
    },
    Write {
        result: Receiver<Result<(), String>>,
        path: PathBuf,
    },
}
#[derive(Default)]
pub struct LayoutTransfer {
    pending: Option<Pending>,
    error: Option<String>,
}
struct Repaint(egui::Context);
impl Wake for Repaint {
    fn wake(self: Arc<Self>) {
        self.0.request_repaint();
    }
}

impl LayoutTransfer {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }
    pub fn import(&mut self, host: &Arc<SharedState>, presets: bool) {
        if self.busy() {
            return;
        }
        self.error = None;
        self.pending = Some(Pending::Directory {
            operation: Operation::Import {
                request: ImportRequest::new(host),
                remember: !presets,
            },
            result: layout_io::prepare_import(host, presets),
        });
    }
    pub fn export(&mut self, host: &Arc<SharedState>) {
        if self.busy() {
            return;
        }
        let Some(layout) = layout_io::selected_layout(host) else {
            return;
        };
        self.error = None;
        let name = layout_io::layout_export_file_name(Some(layout_io::default_layout_export_name(
            layout.clone(),
        )));
        self.pending = Some(Pending::Picker {
            future: Box::pin(
                rfd::AsyncFileDialog::new()
                    .add_filter("Layout YAML", &["yaml", "yml"])
                    .add_filter("Layout JSON", &["json"])
                    .set_file_name(name)
                    .save_file(),
            ),
            operation: Operation::Export(layout),
        });
    }
    pub fn show_status(&self, ui: &mut egui::Ui) {
        if self.busy() {
            ui.spinner();
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }
    /// True only after the core accepted an import: the adapter can reset the
    /// old speaker selection/draft. Closing or cancelling a picker changes neither.
    pub fn poll(&mut self, ctx: &egui::Context, host: &Arc<SharedState>) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };
        let mut imported = false;
        match pending {
            Pending::Directory { result, operation } => match result.try_recv() {
                Ok(directory) => {
                    let mut dialog =
                        rfd::AsyncFileDialog::new().add_filter("Layout", &["json", "yaml", "yml"]);
                    if let Some(dir) = directory {
                        dialog = dialog.set_directory(dir);
                    }
                    self.pending = Some(Pending::Picker {
                        future: Box::pin(dialog.pick_file()),
                        operation,
                    });
                    ctx.request_repaint();
                }
                Err(TryRecvError::Empty) => {
                    self.pending = Some(Pending::Directory { result, operation })
                }
                Err(TryRecvError::Disconnected) => {
                    self.failed(host, "Layout worker disconnected".into())
                }
            },
            Pending::Picker {
                mut future,
                operation,
            } => {
                let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
                match future.as_mut().poll(&mut Context::from_waker(&waker)) {
                    Poll::Pending => self.pending = Some(Pending::Picker { future, operation }),
                    Poll::Ready(None) => {}
                    Poll::Ready(Some(file)) => {
                        let path = file.path().to_owned();
                        self.pending = Some(match operation {
                            Operation::Import { request, remember } => Pending::Read {
                                result: layout_io::read_import(host, path.clone(), remember),
                                request,
                                path,
                            },
                            Operation::Export(layout) => Pending::Write {
                                result: layout_io::write_export(host, path.clone(), layout),
                                path,
                            },
                        });
                    }
                }
            }
            Pending::Read {
                result,
                request,
                path,
            } => match result.try_recv() {
                Ok(answer) => match answer.and_then(|layout| request.apply(host, layout)) {
                    Ok(()) => {
                        imported = true;
                        self.success(host, "log.layoutImported", &path);
                    }
                    Err(error) => {
                        self.failed(host, tf("log.layoutImportFailed", &[("error", &error)]))
                    }
                },
                Err(TryRecvError::Empty) => {
                    self.pending = Some(Pending::Read {
                        result,
                        request,
                        path,
                    })
                }
                Err(TryRecvError::Disconnected) => {
                    self.failed(host, "Layout worker disconnected".into())
                }
            },
            Pending::Write { result, path } => match result.try_recv() {
                Ok(Ok(())) => self.success(host, "log.layoutExported", &path),
                Ok(Err(error)) => {
                    self.failed(host, tf("log.layoutExportFailed", &[("error", &error)]))
                }
                Err(TryRecvError::Empty) => self.pending = Some(Pending::Write { result, path }),
                Err(TryRecvError::Disconnected) => {
                    self.failed(host, "Layout worker disconnected".into())
                }
            },
        }
        imported
    }
    fn success(&self, host: &SharedState, key: &str, path: &std::path::Path) {
        crate::host::commands::app::push_log(
            host,
            "info",
            "layout",
            tf(key, &[("path", &path.to_string_lossy())]),
        );
    }
    fn failed(&mut self, host: &SharedState, error: String) {
        crate::host::commands::app::push_log(host, "error", "layout", error.clone());
        self.error = Some(error);
    }
}
