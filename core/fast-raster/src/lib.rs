//! Independent bounded Fast CPU raster foundations.
//!
//! This crate consumes complete immutable product render plans and their bound Scenes. It owns
//! deterministic command binning, scalar tile kernels, resource limits, cooperative cancellation,
//! and atomic immutable tile publication. The independent M3 Reference renderer remains in
//! `pdf-rs-raster` and is only a development-time differential target.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Bounded deterministic Fast CPU product-tile renderer.
pub mod fast;
