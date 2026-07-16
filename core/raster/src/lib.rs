//! Deterministic Native raster foundations.
//!
//! This crate consumes immutable backend-neutral Scene values and owns project-defined raster
//! output values. The initial Reference profile publishes only an opaque canonical pixel buffer
//! for the current non-painting Scene subset. It deliberately does not own worker/session Surface
//! handles, platform graphics integration, page-to-device geometry, path coverage, fonts, images,
//! or external-engine fallback.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Single-threaded deterministic Reference raster foundations.
pub mod reference;
