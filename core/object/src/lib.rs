//! Resumable, source-bound framing of one indirect PDF object.
//!
//! The bootstrap validates an xref-derived target against its exact indirect
//! object header. Stream objects are framed with separate envelope and terminal
//! boundary reads, so their opaque payload need not be resident or contiguous.
//! Successful values also report the allocator-visible syntax heap capacity
//! they retain, without treating discarded retries as resident state.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod job;
mod limits;
mod model;
mod object_stream;
mod parser;
mod staged;

pub use error::{
    ObjectError, ObjectErrorCategory, ObjectErrorCode, ObjectLimit, ObjectLimitKind,
    ObjectRecoverability,
};
pub use job::{
    NeverCancelled, ObjectCancellation, ObjectJobContext, ObjectPhase, ObjectPoll, ObjectStats,
    OpenObjectJob,
};
pub use limits::{ObjectLimitConfig, ObjectLimits, ObjectWorkCaps};
pub use model::{
    DeclaredStreamLength, FramedStream, IndirectObject, IndirectObjectTarget, IndirectObjectValue,
    ResolvedStreamLength, StreamEnvelope, StreamLengthClaim,
};
pub use object_stream::{
    DecodedArray, DecodedDictionary, DecodedDictionaryEntry, DecodedLocatedObject, DecodedObject,
    DecodedObjectSpan, ObjectStream, ObjectStreamEntry, ObjectStreamError,
    ObjectStreamErrorCategory, ObjectStreamErrorCode, ObjectStreamLimit, ObjectStreamLimitConfig,
    ObjectStreamLimitKind, ObjectStreamLimits, ObjectStreamStats, parse_unfiltered_object_stream,
};
pub use staged::{ObjectEnvelopePoll, OpenObjectEnvelopeJob, OpenStreamBoundaryJob};
