//! Independent bounded scalar Fast CPU tile rendering.
//!
//! The Fast path does not invoke the differential renderer and never obtains, slices, or
//! republishes its whole-page buffer. Its command binning, page mapping, coverage, sampling,
//! compositing, and publication code lives under this module.

mod error;
mod kernels;
mod limits;
mod model;
mod render;
mod stroke;

pub use error::{
    FastRasterError, FastRasterErrorCategory, FastRasterErrorCode, FastRasterLimit,
    FastRasterLimitKind,
};
pub use limits::{FastRasterLimitConfig, FastRasterLimits};
pub use model::{
    FastRasterAlgorithm, FastRasterCancellation, FastRasterIdentity, FastRasterStats, FastTile,
    FastTileBins, FastTileSet, NeverCancelled,
};
pub use render::FastRasterJob;
