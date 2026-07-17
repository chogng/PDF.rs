//! Deterministic bounded viewport scheduling and stale-generation arbitration.
//!
//! [`ViewportScheduler`] accepts replaceable viewport and tile metadata, gives
//! registered sessions precharged queue reservations, and dispatches work using
//! a virtual-clock key plus bounded cross-session fairness. Lifecycle traffic
//! uses a separate critical queue. [`TerminalArbiter`] is the sole path from a
//! completion to a publishable resource.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod arbiter;
mod error;
mod id;
mod limits;
mod scheduler;
mod work;

pub use arbiter::{TerminalArbiter, TerminalArbiterLimits, TerminalSessionPhase};
pub use error::{
    CriticalAdmissionError, LimitConfigError, SchedulerError, SessionRegistrationError,
    TerminalArbiterError, WorkAdmissionError,
};
pub use id::{Generation, ResourceId, SessionId, WorkId};
pub use limits::SchedulerLimits;
pub use scheduler::{
    CloseReceipt, CriticalAdmission, GenerationAdvance, SchedulerDispatch, SchedulerPhase,
    ShutdownReceipt, SubmitOutcome, ViewportScheduler,
};
pub use work::{
    CompletionDiscardReason, CriticalDispatch, CriticalIngress, CriticalKind, Distance,
    FairnessEvidence, Priority, ReplaceKey, ReplaceableKind, ScheduledWork, SchedulingKey,
    ScrollRelation, TerminalDecision, TerminalSignal, WorkRequest,
};
