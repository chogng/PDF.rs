//! Resumable, source-bound parsing of traditional PDF cross-reference tables.
//!
//! This bootstrap accepts a known-length immutable source, locates the final
//! `startxref`, and parses one traditional xref table and trailer. Missing
//! ranges remain explicit [`XrefPoll::Pending`] control flow.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod job;
mod limits;
mod model;
mod parser;

pub use error::{
    XrefError, XrefErrorCategory, XrefErrorCode, XrefLimit, XrefLimitKind, XrefRecoverability,
};
pub use job::{
    NeverCancelled, OpenXrefJob, XrefCancellation, XrefJobContext, XrefPhase, XrefPoll, XrefStats,
};
pub use limits::{XrefLimitConfig, XrefLimits};
pub use model::{XrefEntry, XrefEntryKind, XrefSection};
