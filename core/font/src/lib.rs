//! Bounded, deterministic parsing of the registered foundational TrueType font profile.
//!
//! This crate owns a pure Rust parser and immutable project-defined outline model. It does not
//! consult platform font services, execute TrueType hint programs, perform PDF resource lookup,
//! or retain caller-provided font bytes. Parsing measures and validates the complete font before
//! allocating the published glyph and path buffers.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod limits;
mod model;
mod parse;

pub use error::{
    FontError, FontErrorCategory, FontErrorCode, FontLimit, FontLimitKind, FontRecoverability,
    FontUnsupported, FontUnsupportedKind,
};
pub use limits::{FontLimitConfig, FontLimits};
pub use model::{
    FontBounds, FontCancellation, FontCoordinate, FontParseOutcome, FontParseReport, FontPoint,
    FontProfile, FontStats, GlyphId, GlyphOutline, NeverCancelled, OutlineSegment, TrueTypeFont,
};
pub use parse::parse_truetype;
