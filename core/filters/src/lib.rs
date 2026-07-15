//! Snapshot-attested, bounded decoding for foundational PDF stream filters.
//!
//! Decoding consumes one exact physical [`pdf_rs_bytes::ByteSlice`]. Successful
//! output remains inseparable from a sealed attestation that records its source,
//! object owner, dictionary and encoded geometry, canonical filter plan, strict
//! profile, deterministic budgets, fuel, and decoded length. Decoded positions
//! use only [`DecodedOffset`] and [`DecodedRange`]; they are never represented as
//! physical source spans.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod decode;
mod error;
mod limits;
mod model;

pub use decode::{DecodeCancellation, NeverCancelled, decode_stream};
pub use error::{
    DecodeError, DecodeErrorCategory, DecodeErrorCode, DecodeLimit, DecodeLimitKind,
    DecodeRecoverability,
};
pub use limits::{DecodeLimitConfig, DecodeLimits};
pub use model::{
    DecodeAttestation, DecodeFuelScheduleVersion, DecodeProfile, DecodeRequest, DecodedOffset,
    DecodedRange, DecodedStream, FilterPlan, StreamFilter,
};
