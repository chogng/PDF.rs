//! Ready-state session ownership for one bounded session-only cache.
//!
//! This crate owns the lifetime of exactly one [`pdf_rs_cache::ReadyStore`]
//! after a document has reached Ready. It rejects cache operations after close,
//! preserves move-only successful values on admission failure, and synchronously
//! drops all store values and fixed metadata before close returns. It does not
//! implement document opening, request draining, event publication, scheduling,
//! surface reclamation, or the complete protocol-visible Session state machine.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::large_enum_variant,
    clippy::result_large_err,
    reason = "proof-bearing admission failures stay inline so lifecycle rejection cannot lose a successful value"
)]

mod error;
mod owner;

pub use error::{
    ReadySessionAdmissionError, ReadySessionError, ReadySessionErrorCategory,
    ReadySessionErrorCode, ReadySessionRecoverability,
};
pub use owner::{
    ReadySessionCloseReport, ReadySessionOwner, ReadySessionPhase, ReadySessionResources,
};
