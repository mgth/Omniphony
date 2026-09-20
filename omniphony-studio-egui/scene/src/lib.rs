//! Omniphony Studio's 3D scene.
//!
//! Two halves and a hard line between them. `view` decides what the scene
//! shows — projection, geometry, colour, the overlay it composes but does not
//! paint — and `render` draws it with wgpu into an offscreen target the host
//! composites. Neither names a UI toolkit, which is what makes them the part
//! of the Studio a migration carries over rather than rewrites.
//!
//! The model comes from `omniphony-studio-core`; nothing here writes it.

pub mod render;
pub mod view;

pub use omniphony_studio_core::{host, i18n, model, osc};
