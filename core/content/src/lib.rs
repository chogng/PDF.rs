//! Pure, bounded scanning of already-decoded PDF page content streams.
//!
//! The crate accepts a caller-ordered borrowed stream sequence and publishes an immutable owned
//! operator program. It performs no source acquisition, filter decoding, object resolution,
//! platform I/O, resource lookup, graphics interpretation, or Scene construction.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod limits;
mod model;
mod scanner;

pub use error::{
    ContentError, ContentErrorCategory, ContentErrorCode, ContentLimit, ContentLimitKind,
    ContentRecoverability,
};
pub use limits::{ContentLimitConfig, ContentLimits};
pub use model::{
    ContentDictionaryEntry, ContentExtent, ContentName, ContentOperand, ContentOperator,
    ContentOperatorSource, ContentPosition, ContentProgram, ContentReal, ContentScanStats,
    ContentString, ContentStringKind, DecodedContentStream, DecodedSpan, LocatedOperand,
    OperatorContext, OperatorKind, OperatorSpec, ScannedOperator,
};
pub use scanner::{
    ContentCancellation, ContentScanJob, ContentScanPhase, ContentScanPoll, NeverCancelled,
    scan_content_streams,
};
