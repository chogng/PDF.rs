//! Source-bound parsing of PDF cross-reference tables.
//!
//! The resumable bootstrap accepts a known-length immutable source, locates the final
//! `startxref`, and parses one traditional xref table and trailer. Missing ranges remain explicit
//! [`XrefPoll::Pending`] control flow. A separate bounded entry point validates one complete
//! caller-supplied unfiltered xref-stream payload without treating decoded coordinates as source
//! spans. A pure composer validates already-parsed newest-to-oldest traditional, stream, and
//! hybrid revision candidates. Acquisition, filter decoding, object validation, and product
//! integration remain outside both entry points.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod job;
mod limits;
mod model;
mod parser;
mod revision;
mod stream;

pub use error::{
    XrefError, XrefErrorCategory, XrefErrorCode, XrefLimit, XrefLimitKind, XrefRecoverability,
};
pub use job::{
    NeverCancelled, OpenXrefJob, XrefCancellation, XrefJobContext, XrefPhase, XrefPoll, XrefStats,
};
pub use limits::{XrefLimitConfig, XrefLimits};
pub use model::{XrefEntry, XrefEntryKind, XrefSection};
pub use revision::{
    HybridSupplement, ResolvedXrefEntry, RevisionCandidate, RevisionChain, RevisionEntry,
    RevisionEntryKind, RevisionEntryOrigin, RevisionError, RevisionErrorCategory,
    RevisionErrorCode, RevisionId, RevisionLimitConfig, RevisionLimitKind, RevisionLimits,
    RevisionPrimaryKind, RevisionStats, compose_revision_chain,
};
pub use stream::{
    DecodedXrefSpan, XrefStream, XrefStreamEntry, XrefStreamEntryKind, XrefStreamError,
    XrefStreamErrorCategory, XrefStreamErrorCode, XrefStreamLimitConfig, XrefStreamLimitKind,
    XrefStreamLimits, XrefStreamStats, parse_unfiltered_xref_stream,
};
