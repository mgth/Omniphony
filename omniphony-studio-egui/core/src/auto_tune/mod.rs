//! The PI auto-tune procedure (`auto-tune/`), ported module for module.
//!
//! The renderer's adaptive resampler is a PI controller, and its gains depend
//! on the machine it runs on. The procedure walks a Ziegler-Nichols-style
//! sweep: raise the proportional gain until the loop rings, back off, add the
//! integral term, then run long enough to size the rate limit from what the
//! link actually needed.

//! The procedure is the unit under test: the detectors and the machine are
//! driven from tables of samples, so what it decides is checked rather than
//! watched. Some of what they expose is read only by those tests until the
//! wizard is wired to them.
#![allow(dead_code)]

pub mod detectors;
pub mod machine;
