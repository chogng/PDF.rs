//! Backend-neutral drawing semantics and immutable display-list foundations.
//!
//! This crate owns portable paint and display-list semantics.
//! CPU and GPU executors depend on it; it never depends on either executor.
//! The `pdf-rs-skia` facade selects the API available to consumers, including
//! PDF.rs. See `skia/README.md` for the subsystem boundary.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod display_list;
mod paint;

pub use display_list::{DisplayList, DisplayListBuilder, DrawCommand, GlyphRunId, ImageId, PathId};
pub use paint::{BlendMode, Color, Paint};
pub use pdf_rs_skia_error::{SkiaError, SkiaErrorCode};
pub use pdf_rs_skia_geometry::{Point, Rect, Scalar, Transform};
pub use pdf_rs_skia_path::{
    Angle, ArcDirection, ArcStart, ConicWeight, FillRule, Path, PathBounds, PathBuilder, PathVerb,
};
pub use pdf_rs_skia_text::{
    FontId, GlyphId, GlyphOutline, GlyphOutlineProvider, GlyphRun, OutlinePoint, OutlineSegment,
    PositionedGlyph, TextError, TextErrorCode, TextUnit,
};
