//! Deterministic M0 comparison artifacts and PNG output.
//!
//! The crate deliberately has no external dependencies. Semantic artifacts use
//! fixed field ordering and integer coordinates for canonical JSON, while pixel
//! comparisons and PNG encoding use checked arithmetic throughout.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod artifact;
mod json;
mod pixel;
mod png;

pub use artifact::{
    ArtifactKind, ParseArtifact, ParseDiagnostic, ParseObject, SceneArtifact, SceneCommand,
    SectionDiff, SemanticDiffSummary, TextArtifact, TextRun, WritingMode, compare_parse,
    compare_scene, compare_text,
};
pub use json::{CanonicalJson, canonical_json_string};
pub use pixel::{
    PixelArtifact, PixelBufferRole, PixelDiff, PixelDiffSummary, PixelError, compare_pixels,
};
pub use png::{
    PixelPngBundle, PngError, adler32, crc32, encode_pixel_comparison_pngs, encode_rgba_png,
};
