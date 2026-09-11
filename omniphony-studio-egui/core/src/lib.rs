//! The Studio's core: the renderer protocol, the application and session
//! state, and everything that happens over time. It knows nothing of the UI
//! toolkit. `omniphony-studio-egui` draws it today, and CI checks that no UI
//! crate ever enters this crate's dependency graph, so the next toolkit can
//! draw it too. See `../ARCHITECTURE.md`.

pub mod auto_tune;
pub mod host;
pub mod i18n;
pub mod model;
pub mod osc;
pub mod stats;
