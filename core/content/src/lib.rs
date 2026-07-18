//! Bounded scanning and sealed interpretation of acquired PDF Page content.
//!
//! The scanner accepts a caller-ordered borrowed stream sequence and publishes an immutable owned
//! operator program. The only public interpretation entry consumes a proof-bearing
//! [`pdf_rs_document::AcquiredPageContent`], scans its exact decoded streams internally, resolves
//! bounded inherited marked-content properties, plans proof-bound Image XObjects, resumes their
//! document-owned decoding, and atomically publishes an immutable Scene-bound interpreted Page.
//! The crate performs no Page-content acquisition, platform I/O, async scheduling, shared-cache
//! insertion, rendering, or external-engine fallback.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod ext_gstate;
mod font_limits;
mod graphics_limits;
mod image_limits;
mod limits;
mod model;
mod number;
mod scanner;
mod vm;
mod vm_error;
mod vm_limits;
mod vm_model;

pub use error::{
    ContentError, ContentErrorCategory, ContentErrorCode, ContentLimit, ContentLimitKind,
    ContentRecoverability,
};
pub use ext_gstate::{
    ContentExtGStateError, ContentExtGStateErrorKind, ContentExtGStateProfile,
    ContentExtGStateResource,
};
pub use font_limits::{ContentFontLimitConfig, ContentFontLimits};
pub use graphics_limits::{ContentGraphicsLimitConfig, ContentGraphicsLimits};
pub use image_limits::{ContentImageLimitConfig, ContentImageLimits};
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
pub use vm::{ContentFontProfile, ContentImageProfile, ContentVmPoll, InterpretPageJob};
pub use vm::{ContentFormPoll, InterpretFormJob};
pub use vm_error::{
    ContentFontLimit, ContentFontLimitKind, ContentGraphicsLimit, ContentGraphicsLimitKind,
    ContentImageLimit, ContentImageLimitKind, ContentUnsupported, ContentUnsupportedKind,
    ContentVmError, ContentVmErrorCategory, ContentVmErrorCode, ContentVmFailure, ContentVmLimit,
    ContentVmLimitKind, ContentVmRecoverability,
};
pub use vm_limits::{ContentVmLimitConfig, ContentVmLimits};
pub use vm_model::{
    ContentFontStats, ContentImageStats, ContentVmPhase, ContentVmStats, InterpretedForm,
    InterpretedPage, ResolvedFontUse, ResolvedImageUse, ResolvedPropertyUse,
};
