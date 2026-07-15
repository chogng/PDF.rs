//! Bounded Range-resume arbitration and M1 strict-document session ownership.
//!
//! This crate owns five deliberately small runtime slices: deterministic
//! snapshot-bound Range grouping, snapshot-bound Range subscriptions,
//! generation-gated execution of one strict base-open job, a single-job
//! coordinator that closes their actor-turn gap, and the lifetime of exactly one
//! [`pdf_rs_cache::ReadyStore`] after a document has reached Ready.
//! [`M1StrictDocumentSession`] composes the execution owners with one page-count
//! slot and one outline slot for the M1 service boundary. None performs file,
//! network, platform, or async I/O. The M1 actor is not a complete product
//! Session or a general-purpose scheduler.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::large_enum_variant,
    clippy::result_large_err,
    reason = "proof-bearing admission failures stay inline so lifecycle rejection cannot lose a successful value"
)]

mod error;
mod m1_session;
mod owner;
mod range_coalescer;
mod range_resume;
mod range_resume_error;
mod strict_base_open_coordinator;
mod strict_base_open_owner;

pub use error::{
    ReadySessionAdmissionError, ReadySessionError, ReadySessionErrorCategory,
    ReadySessionErrorCode, ReadySessionRecoverability,
};
pub use m1_session::{
    M1OpeningParserAudit, M1RequestId, M1RequestIdentity, M1Service, M1ServiceFailure,
    M1SessionCancel, M1SessionCancelRejectReason, M1SessionClose, M1SessionCloseReport,
    M1SessionFailure, M1SessionIngress, M1SessionIngressRejectReason, M1SessionPhase,
    M1SessionRequestError, M1SessionResources, M1SessionRun, M1SessionWait,
    M1StrictDocumentSession,
};
pub use owner::{
    ReadySessionCloseReport, ReadySessionOwner, ReadySessionPhase, ReadySessionResources,
};
pub use range_coalescer::{
    CoalescedRangeGroup, NeverCancelledRangeCoalescer, RangeCoalescerCancellation,
    RangeCoalescerError, RangeCoalescerErrorCategory, RangeCoalescerErrorCode, RangeCoalescerLimit,
    RangeCoalescerLimitConfig, RangeCoalescerLimitKind, RangeCoalescerLimits,
    RangeCoalescerRecoverability, RangeCoalescerRequest, RangeCoalescingPlan,
    RangeRequestCoalescer, RangeRequestId,
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
