//! Resumable, source-bound framing of one indirect PDF object.
//!
//! The bootstrap validates an xref-derived target against its exact indirect
//! object header. Stream objects are framed with separate envelope and terminal
//! boundary reads, so their opaque payload need not be resident or contiguous.
//! Successful values also report the allocator-visible syntax heap capacity
//! they retain, without treating discarded retries as resident state.
//! An explicit sibling job can run that strict path first and then attempt one
//! bounded, proof-bearing local header or direct-length repair; the strict
//! entry points never search or recover implicitly.
//! Filtered object streams are accepted only by a consuming proof-bound entry
//! that keeps the complete framed container, sealed decoder attestation, and
//! decoded-coordinate semantic result inseparable.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod filtered_object_stream;
mod job;
mod limits;
mod model;
mod object_stream;
mod parser;
mod repair;
mod staged;

pub use error::{
    ObjectError, ObjectErrorCategory, ObjectErrorCode, ObjectLimit, ObjectLimitKind,
    ObjectRecoverability,
};
pub use filtered_object_stream::{FilteredObjectStream, parse_filtered_object_stream};
pub use job::{
    NeverCancelled, ObjectCancellation, ObjectJobContext, ObjectPhase, ObjectPoll, ObjectStats,
    OpenObjectJob,
};
pub use limits::{ObjectLimitConfig, ObjectLimits, ObjectWorkCaps};
pub use model::{
    DeclaredStreamLength, FramedStream, IndirectObject, IndirectObjectTarget,
    IndirectObjectTargetKind, IndirectObjectValue, ResolvedStreamLength, StreamEnvelope,
    StreamLengthClaim,
};
pub use object_stream::{
    DecodedArray, DecodedDictionary, DecodedDictionaryEntry, DecodedLocatedObject, DecodedObject,
    DecodedObjectSpan, ObjectStream, ObjectStreamEntry, ObjectStreamError,
    ObjectStreamErrorCategory, ObjectStreamErrorCode, ObjectStreamLimit, ObjectStreamLimitConfig,
    ObjectStreamLimitKind, ObjectStreamLimits, ObjectStreamStats, parse_unfiltered_object_stream,
};
pub use repair::{
    LocalObjectJobContext, LocalObjectPhase, LocalObjectPoll, LocallyFramedObject,
    ObjectRepairDiagnostic, ObjectRepairKind, ObjectRepairLimitConfig, ObjectRepairLimits,
    ObjectRepairStats, ObjectRepairWorkCaps, OpenLocalObjectJob,
};
pub use staged::{ObjectEnvelopePoll, OpenObjectEnvelopeJob, OpenStreamBoundaryJob};
