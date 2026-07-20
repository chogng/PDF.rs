//! Snapshot-bound byte access for the Native PDF engine.
//!
//! This crate owns source identity, checked byte ranges, resumable read
//! requests, and a bounded in-memory Range store. It deliberately performs no
//! file, network, or async-runtime I/O; hosts inject validated response bytes.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod identity;
mod range;
mod source;
mod store;

pub use error::{
    SourceError, SourceErrorCategory, SourceErrorCode, SourceLimit, SourceLimitKind,
    SourceRecoverability,
};
pub use identity::{
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
pub use range::{ByteRange, SmallRanges};
pub use source::{
    ByteSlice, ByteSource, DataTicket, JobId, RangeResponse, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, ResumeSubscription,
};
pub use store::{RangeStore, RangeStoreLimitConfig, RangeStoreLimits, SupplyOutcome, TicketStatus};
