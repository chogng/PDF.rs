//! Bounded ownership for complete immutable Native product tiles.
//!
//! This crate accepts only policy-bound successful tile outcomes, accounts actual retained
//! capacity, evicts deterministically, and closes synchronously. It performs no parsing,
//! rasterization, I/O, scheduling, persistence, or cross-session sharing.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::large_enum_variant,
    clippy::result_large_err,
    reason = "complete move-only tiles and rejection values stay inline so failure cannot allocate again"
)]

mod tile;

pub use tile::{
    NativeTile, NeverCancelledTileCache, TileAdmission, TileAdmitted, TileCache, TileCacheAddress,
    TileCacheAdmissionError, TileCacheBinding, TileCacheCancellation, TileCacheCloseReport,
    TileCacheError, TileCacheErrorCategory, TileCacheErrorCode, TileCacheLimit,
    TileCacheLimitConfig, TileCacheLimitKind, TileCacheLimits, TileCacheLookup,
    TileCacheMissReason, TileCacheOwnerId, TileCacheRecoverability, TileCacheScope,
    TileCacheSessionId, TileCacheStats, TileOutcomeKind, TileRejectReason, TileRejected,
    TileRenderOutcome, TileRetentionClass,
};
