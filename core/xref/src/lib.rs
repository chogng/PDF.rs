//! Source-bound parsing of PDF cross-reference tables.
//!
//! The resumable bootstrap accepts a known-length immutable source, locates the final
//! `startxref`, and parses one traditional xref table and trailer. Missing ranges remain explicit
//! [`XrefPoll::Pending`] control flow. A separate bounded entry point validates one complete
//! caller-supplied unfiltered xref-stream payload without treating decoded coordinates as source
//! spans. A pure composer validates already-parsed newest-to-oldest traditional, stream, and
//! hybrid revision candidates. An explicit local-repair sibling first exhausts the unchanged
//! strict job, then permits only bounded fixed-row whitespace repair or a nearby unique final
//! traditional-xref anchor, retaining source-bound diagnostics and reusing normal validation.
//! Filter decoding, object repair, and product integration remain outside these entry points.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod job;
mod limits;
mod model;
mod parser;
mod repair;
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
pub use repair::{
    LocalXrefJobContext, LocalXrefPhase, LocalXrefPoll, LocallyParsedXrefSection, OpenLocalXrefJob,
    XrefRepairDiagnostic, XrefRepairKind, XrefRepairLimitConfig, XrefRepairLimits, XrefRepairStats,
};
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
