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
mod number;
mod scanner;
mod vm_error;
mod vm_limits;
mod vm_model;

pub use error::{
    ContentError, ContentErrorCategory, ContentErrorCode, ContentLimit, ContentLimitKind,
    ContentRecoverability,
};
pub use limits::{ContentLimitConfig, ContentLimits};
pub use model::{
    ContentDictionaryEntry, ContentExtent, ContentName, ContentOperand, ContentOperator,
    ContentOperatorSource, ContentPosition, ContentProgram, ContentReal, ContentScanStats,
    ContentString, ContentStringKind, DecodedContentStream, DecodedSpan, LocatedOperand,
    OperatorContext, OperatorFailurePolicy, OperatorKind, OperatorOperandShape, OperatorSpec,
    ScannedOperator,
};
pub use number::ContentNumber;
pub use scanner::{
    ContentCancellation, ContentScanJob, ContentScanPhase, ContentScanPoll, NeverCancelled,
    scan_content_streams,
};
pub use vm_error::{
    ContentVmError, ContentVmErrorCategory, ContentVmErrorCode, ContentVmLimit, ContentVmLimitKind,
    ContentVmRecoverability,
};
pub use vm_limits::{ContentVmLimitConfig, ContentVmLimits};
pub use vm_model::ContentVmStats;
