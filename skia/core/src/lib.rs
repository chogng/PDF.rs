//! Backend-neutral drawing semantics and immutable display-list foundations.
//!
//! This crate owns portable geometry, paint, paths, images, and stable errors.
//! CPU and GPU executors depend on it; it never depends on either executor.
//! PDF.rs consumes this public API through an adapter and does not define its
//! types or semantics. See `skia/README.md` for the subsystem boundary.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod display_list;
mod error;
mod geometry;
mod image;
mod paint;
mod path;

pub use display_list::{DisplayList, DisplayListBuilder, DrawCommand, GlyphRunId, ImageId, PathId};
pub use error::{SkiaError, SkiaErrorCode};
pub use geometry::{Point, Rect, Scalar, Transform};
pub use image::Image;
pub use paint::{BlendMode, Color, Paint};
pub use path::{ArcDirection, ArcStart, FillRule, Path, PathBuilder, PathVerb};
pub use pdf_rs_skia_text::{
    FontId, GlyphId, GlyphOutline, GlyphOutlineProvider, GlyphRun, OutlinePoint, OutlineSegment,
    PositionedGlyph, TextError, TextErrorCode, TextUnit,
};
