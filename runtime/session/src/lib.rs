//! Bounded Range-resume arbitration and Ready-state session ownership.
//!
//! This crate owns two deliberately small runtime slices: snapshot-bound Range
//! subscriptions between resumable core jobs and an external scheduler, plus the
//! lifetime of exactly one [`pdf_rs_cache::ReadyStore`] after a document has
//! reached Ready. Neither slice performs file, network, platform, or async I/O.
//! They do not implement the complete protocol-visible Session state machine.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::large_enum_variant,
    clippy::result_large_err,
    reason = "proof-bearing admission failures stay inline so lifecycle rejection cannot lose a successful value"
)]

mod error;
mod owner;
mod range_resume;
mod range_resume_error;

pub use error::{
    ReadySessionAdmissionError, ReadySessionError, ReadySessionErrorCategory,
    ReadySessionErrorCode, ReadySessionRecoverability,
};
pub use owner::{
    ReadySessionCloseReport, ReadySessionOwner, ReadySessionPhase, ReadySessionResources,
};
pub use range_resume::{
    RangeResumeArbiter, RangeResumeCancelOutcome, RangeResumeDispatch, RangeResumeGeneration,
    RangeResumePhase, RangeResumeRegistrationOutcome, RangeResumeReleaseReport,
    RangeResumeResources, RangeResumeSupplyOutcome, RangeResumeTarget,
};
pub use range_resume_error::{
    RangeResumeError, RangeResumeErrorCategory, RangeResumeErrorCode, RangeResumeLimit,
    RangeResumeRecoverability,
};
