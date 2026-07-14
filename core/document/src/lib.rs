//! Candidate-only revision indexing for bounded indirect-object framing.
//!
//! This crate turns one parsed xref section into an explicitly unauthenticated
//! physical interval index. It does not attest that an xref offset is at PDF
//! top level and must not be used as a trusted object resolver.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod index;
mod limits;
mod model;

pub use error::{
    DocumentError, DocumentErrorCategory, DocumentErrorCode, DocumentLimit, DocumentLimitKind,
    DocumentRecoverability,
};
pub use index::{DocumentCancellation, NeverCancelled};
pub use limits::{DocumentLimitConfig, DocumentLimits};
pub use model::{CandidateRevisionIndex, DocumentIndexStats, PhysicalObjectInterval, RevisionId};
