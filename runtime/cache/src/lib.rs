//! Session-scoped ownership for immutable proof-bearing Ready values.
//!
//! This crate owns bounded cache metadata at the runtime boundary. One logical
//! document actor mutates a [`ReadyStore`] through exclusive borrows, while
//! callers receive only borrowed hits. The store is neither persistent nor
//! cross-session and does not coalesce in-flight resolution work.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::large_enum_variant,
    clippy::result_large_err,
    reason = "move-only proof-bearing values and complete lower errors stay inline so rejection cannot fail another allocation"
)]

mod binding;
mod error;
mod limits;
mod store;

pub use binding::{ReadyStoreBinding, ReadyStoreEpoch, ReadyStoreKey, ReadyStoreSessionId};
pub use error::{
    ReadyStoreAdmissionError, ReadyStoreError, ReadyStoreErrorCategory, ReadyStoreErrorCode,
    ReadyStoreLimit, ReadyStoreLimitKind, ReadyStoreRecoverability, ReadyStoreScope,
};
pub use limits::{ReadyStoreLimitConfig, ReadyStoreLimits};
pub use store::{
    ReadyAdmission, ReadyAdmitted, ReadyLookup, ReadyMissReason, ReadyRejectReason, ReadyRejected,
    ReadyStore, ReadyStoreStats,
};
