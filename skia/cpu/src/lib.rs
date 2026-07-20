//! Deterministic CPU executor for `pdf-rs-skia-core` drawing semantics.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod canvas;

pub use canvas::{Canvas, ClipRect, Surface, SurfaceLimits};
