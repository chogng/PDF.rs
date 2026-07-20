//! Stable public API for the reusable Skia-like graphics engine.
//!
//! Applications depend on this crate. Geometry, text, CPU, GPU, and platform
//! backends remain implementation layers within the Skia workspace.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub use pdf_rs_skia_core::*;
pub use pdf_rs_skia_cpu::*;
