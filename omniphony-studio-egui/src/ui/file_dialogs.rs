//! Native dialogs are futures polled by the composition root. No thread,
//! filesystem access, renderer command or timer belongs in this UI helper.
use crate::{app::StudioSpike, host::commands::layout_io::SessionToken};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

pub enum Purpose {
    Backend {
        backend: String,
        key: String,
    },
    Script {
        backend: String,
        key: String,
        navigation: Arc<()>,
    },
    ObjectClip,
    Sofa,
}
pub struct PendingPicker {
    purpose: Purpose,
    session: SessionToken,
    future: Pin<Box<dyn Future<Output = Option<Vec<rfd::FileHandle>>>>>,
}
struct Repaint(egui::Context);
impl Wake for Repaint {
    fn wake(self: Arc<Self>) {
        self.0.request_repaint();
    }
}

impl StudioSpike {
    pub(crate) fn pick_files(
        &mut self,
        ctx: &egui::Context,
        purpose: Purpose,
        extensions: &[String],
    ) {
        if self.file_picker.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new();
        if !extensions.is_empty() {
            let exts: Vec<&str> = extensions.iter().map(String::as_str).collect();
            dialog = dialog.add_filter("Files", &exts);
        }
        let multiple = matches!(purpose, Purpose::Sofa);
        let future: Pin<Box<dyn Future<Output = Option<Vec<rfd::FileHandle>>>>> = if multiple {
            Box::pin(dialog.pick_files())
        } else {
            Box::pin(async move { dialog.pick_file().await.map(|file| vec![file]) })
        };
        self.file_picker = Some(PendingPicker {
            purpose,
            session: SessionToken::new(&self.host),
            future,
        });
        ctx.request_repaint();
    }
    pub(crate) fn poll_file_picker(&mut self, ctx: &egui::Context) {
        let Some(mut picker) = self.file_picker.take() else {
            return;
        };
        let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
        let files = match picker
            .future
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
        {
            Poll::Pending => {
                self.file_picker = Some(picker);
                return;
            }
            Poll::Ready(None) => return,
            Poll::Ready(Some(files)) => files,
        };
        let paths: Vec<PathBuf> = files.iter().map(|file| file.path().to_owned()).collect();
        if matches!(picker.purpose, Purpose::Sofa) {
            self.import_selected_sofa(paths);
            return;
        }
        let Some(path) = paths.first() else {
            return;
        };
        let path = path.to_string_lossy().into_owned();
        match picker.purpose {
            Purpose::Script {
                backend,
                key,
                navigation,
            } => self.open_picked_script(
                ctx,
                &backend,
                &key,
                path,
                navigation,
                Arc::new(picker.session),
            ),
            purpose => {
                let accepted = picker
                    .session
                    .with_local_target(&self.host, || match purpose {
                        Purpose::Backend { backend, key } => {
                            crate::host::commands::render::control_backend_param(
                                &self.host,
                                key,
                                serde_json::json!(path),
                                Some(backend),
                            )
                        }
                        Purpose::ObjectClip => {
                            crate::host::commands::gain::control_object_test_clip(&self.host, path)
                        }
                        _ => {}
                    });
                if accepted.is_none() {
                    self.log(
                        "warn",
                        "files",
                        "File choice discarded: renderer session or profile changed",
                    );
                }
            }
        }
    }
}
