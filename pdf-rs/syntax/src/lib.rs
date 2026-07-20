//! Bounded, source-located PDF lexical and direct-object syntax.
//!
//! The crate parses one contiguous, immutable source window. Incomplete
//! windows return [`SyntaxPoll::NeedMore`]; a ByteSource-facing job can then
//! grow and retry from an explicit idempotent object boundary without turning
//! missing data into malformed syntax.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod limits;
mod model;
mod parser;

pub use error::{
    SyntaxError, SyntaxErrorCategory, SyntaxErrorCode, SyntaxLimit, SyntaxLimitKind,
    SyntaxRecoverability,
};
pub use limits::{SyntaxLimitConfig, SyntaxLimits};
pub use model::{
    ByteSpan, DictionaryEntry, Located, ObjectRef, PdfArray, PdfDictionary, PdfHeader, PdfName,
    PdfReal, PdfString, RealNotation, StringKind, SyntaxObject,
};
pub use parser::{
    InputExtent, NeverCancelled, RawBytes, SyntaxCancellation, SyntaxInput, SyntaxParser,
    SyntaxPoll, SyntaxStats,
};
