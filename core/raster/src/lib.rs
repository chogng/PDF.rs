//! Deterministic Native raster foundations.
//!
//! This crate consumes immutable backend-neutral Scene values and owns project-defined raster
//! output values. [`mod@reference`] is the independently reviewed M3 differential target.
//! [`mod@fast`] is the bounded product-tile implementation: it consumes a complete product
//! [`pdf_rs_policy::RenderPlan`], bins immutable Scene commands by conservative bounds, executes
//! independent scalar kernels, and publishes only complete immutable tiles.
//!
//! Neither implementation owns worker/session Surface handles, platform graphics integration,
//! filesystem or network access, or an external-engine fallback.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Bounded deterministic Fast CPU product-tile renderer.
pub mod fast;
/// Single-threaded deterministic Reference raster foundations.
pub mod reference;
