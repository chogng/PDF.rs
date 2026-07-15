//! Bounded Range-resume arbitration and Ready-state session ownership.
//!
//! This crate owns four deliberately small runtime slices: snapshot-bound Range
//! subscriptions, generation-gated execution of one strict base-open job, a
//! single-job coordinator that closes their actor-turn gap, and the lifetime of
//! exactly one [`pdf_rs_cache::ReadyStore`] after a document has reached Ready.
//! None performs file, network, platform, or async I/O. Together they still do
//! not implement the complete protocol-visible Session state machine or a
//! general-purpose scheduler.

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
mod strict_base_open_coordinator;
mod strict_base_open_owner;

pub use error::{
    ReadySessionAdmissionError, ReadySessionError, ReadySessionErrorCategory,
    ReadySessionErrorCode, ReadySessionRecoverability,
};
pub use owner::{
    ReadySessionCloseReport, ReadySessionOwner, ReadySessionPhase, ReadySessionResources,
};
pub use range_resume::{
    RangeResumeArbiter, RangeResumeArbiterId, RangeResumeCancelOutcome, RangeResumeCompletion,
    RangeResumeDispatch, RangeResumeFailureOutcome, RangeResumeFailurePermit,
    RangeResumeGeneration, RangeResumePermit, RangeResumePhase, RangeResumeRegistrationOutcome,
    RangeResumeReleaseReport, RangeResumeResources, RangeResumeSupplyOutcome, RangeResumeTarget,
};
pub use range_resume_error::{
    RangeResumeError, RangeResumeErrorCategory, RangeResumeErrorCode, RangeResumeLimit,
    RangeResumeRecoverability,
};
pub use strict_base_open_coordinator::{
    StrictBaseOpenCoordinator, StrictBaseOpenCoordinatorCancel,
    StrictBaseOpenCoordinatorCloseReport, StrictBaseOpenCoordinatorFailure,
    StrictBaseOpenCoordinatorPhase, StrictBaseOpenCoordinatorResources,
    StrictBaseOpenCoordinatorRun, StrictBaseOpenCoordinatorSourceChange, StrictBaseOpenIngress,
    StrictBaseOpenIngressRejectReason, StrictBaseOpenReady,
};
pub use strict_base_open_owner::{
    StrictBaseOpenJobOwner, StrictBaseOpenOwnerCancelOutcome, StrictBaseOpenOwnerCloseReport,
    StrictBaseOpenOwnerFail, StrictBaseOpenOwnerPhase, StrictBaseOpenOwnerPoll,
    StrictBaseOpenOwnerResources, StrictBaseOpenOwnerResume,
    StrictBaseOpenOwnerSourceChangeOutcome, StrictBaseOpenOwnerStart,
    StrictBaseOpenResumeDiscardReason,
};
