//! Pure Rust, backend-neutral 2D drawing foundations.
//!
//! `pdf-rs-skia` intentionally owns no PDF parsing, platform surface, GPU, font lookup, file,
//! network, or foreign-function dependency. It is the reusable canvas layer that PDF, editors,
//! annotation tools, and other products can adapt into. The initial CPU backend is deterministic:
//! Q16 coordinates, center-sample path filling, top-left RGBA8 surfaces, and explicit resource
//! limits.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod canvas;
mod geometry;
mod paint;
mod path;

pub use canvas::{Canvas, ClipRect, SkiaError, SkiaErrorCode, Surface, SurfaceLimits};
pub use geometry::{Point, Rect, Scalar, Transform};
pub use paint::{BlendMode, Color, Paint};
pub use path::{FillRule, Path, PathBuilder, PathVerb};
