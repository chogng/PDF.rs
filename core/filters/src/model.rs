use std::fmt;

use pdf_rs_bytes::{ByteSlice, SourceIdentity, SourceSnapshot};
use pdf_rs_syntax::{ByteSpan, ObjectRef};

use crate::limits::HARD_MAX_FILTERS;
use crate::{DecodeError, DecodeErrorCode, DecodeLimitKind, DecodeLimits};

/// Foundational PDF stream filter implemented by this crate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StreamFilter {
    /// The canonical PDF `FlateDecode` filter with a zlib wrapper.
    FlateDecode,
    /// The canonical PDF `ASCIIHexDecode` filter.
    AsciiHexDecode,
    /// The canonical PDF `ASCII85Decode` filter.
    Ascii85Decode,
    /// The canonical PDF `RunLengthDecode` filter.
    RunLengthDecode,
}

impl StreamFilter {
    /// Returns the canonical PDF name bytes without the leading slash.
    pub const fn canonical_pdf_name(self) -> &'static [u8] {
        match self {
            Self::FlateDecode => b"FlateDecode",
            Self::AsciiHexDecode => b"ASCIIHexDecode",
            Self::Ascii85Decode => b"ASCII85Decode",
            Self::RunLengthDecode => b"RunLengthDecode",
        }
    }
}

/// Ordered canonical stream-filter plan.
///
/// An empty plan selects the crate's internal identity path. `Identity` is not
/// accepted as a PDF filter name and is intentionally absent from
/// [`StreamFilter`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterPlan {
    filters: Vec<StreamFilter>,
}

impl FilterPlan {
    /// Builds an ordered plan from already canonical filter identifiers.
    pub fn new(filters: &[StreamFilter]) -> Result<Self, DecodeError> {
        validate_hard_filter_count(filters.len())?;
        let mut owned = Vec::new();
        owned.try_reserve_exact(filters.len()).map_err(|_| {
            DecodeError::resource(
                DecodeLimitKind::Allocation,
                u64::from(HARD_MAX_FILTERS),
                0,
                u64::try_from(filters.len()).unwrap_or(u64::MAX),
                None,
            )
        })?;
        owned.extend_from_slice(filters);
        Ok(Self { filters: owned })
    }

    /// Canonicalizes strict full PDF filter names in source order.
    ///
    /// Abbreviations and the non-standard name `Identity` are rejected as
    /// unsupported. Unknown name bytes are never retained in the returned error.
    pub fn from_pdf_names(names: &[&[u8]]) -> Result<Self, DecodeError> {
        validate_hard_filter_count(names.len())?;
        let mut filters = Vec::new();
        filters.try_reserve_exact(names.len()).map_err(|_| {
            DecodeError::resource(
                DecodeLimitKind::Allocation,
                u64::from(HARD_MAX_FILTERS),
                0,
                u64::try_from(names.len()).unwrap_or(u64::MAX),
                None,
            )
        })?;
        for (index, name) in names.iter().enumerate() {
            let filter = match *name {
                b"FlateDecode" => StreamFilter::FlateDecode,
                b"ASCIIHexDecode" => StreamFilter::AsciiHexDecode,
                b"ASCII85Decode" => StreamFilter::Ascii85Decode,
                b"RunLengthDecode" => StreamFilter::RunLengthDecode,
                _ => {
                    return Err(DecodeError::for_code(
                        DecodeErrorCode::UnsupportedFilter,
                        Some(u16::try_from(index).expect("hard filter count fits u16")),
                    ));
                }
            };
            filters.push(filter);
        }
        Ok(Self { filters })
    }

    /// Returns the canonical filters in decode order.
    pub fn filters(&self) -> &[StreamFilter] {
        &self.filters
    }

    /// Returns the number of explicit PDF filters.
    pub fn len(&self) -> usize {
        self.filters.len()
    }

    /// Reports whether no explicit PDF filter is present.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

fn validate_hard_filter_count(count: usize) -> Result<(), DecodeError> {
    if count > usize::from(HARD_MAX_FILTERS) {
        return Err(DecodeError::resource(
            DecodeLimitKind::FilterCount,
            u64::from(HARD_MAX_FILTERS),
            0,
            u64::try_from(count).unwrap_or(u64::MAX),
            None,
        ));
    }
    Ok(())
}

/// Version of deterministic stream-decoding fuel weights.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DecodeFuelScheduleVersion {
    /// One unit per setup/algorithm step, consumed input byte, and emitted output byte.
    M1V1,
}

impl DecodeFuelScheduleVersion {
    pub(crate) const fn layer_setup_cost(self) -> u64 {
        match self {
            Self::M1V1 => 1,
        }
    }

    pub(crate) const fn input_byte_cost(self) -> u64 {
        match self {
            Self::M1V1 => 1,
        }
    }

    pub(crate) const fn output_byte_cost(self) -> u64 {
        match self {
            Self::M1V1 => 1,
        }
    }

    pub(crate) const fn algorithm_step_cost(self) -> u64 {
        match self {
            Self::M1V1 => 1,
        }
    }
}

/// Strict decoding semantics selected for one request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DecodeProfile {
    /// M1 strict profile with required end markers and no trailing data.
    M1StrictV1,
}

impl DecodeProfile {
    /// Returns the deterministic fuel schedule bound to this profile.
    pub const fn fuel_schedule(self) -> DecodeFuelScheduleVersion {
        match self {
            Self::M1StrictV1 => DecodeFuelScheduleVersion::M1V1,
        }
    }
}

/// Fully bound request for one exact physical encoded stream slice.
pub struct DecodeRequest {
    pub(crate) snapshot: SourceSnapshot,
    pub(crate) owner: ObjectRef,
    pub(crate) dictionary_span: ByteSpan,
    pub(crate) encoded_span: ByteSpan,
    pub(crate) encoded: ByteSlice,
    pub(crate) plan: FilterPlan,
    pub(crate) profile: DecodeProfile,
    pub(crate) limits: DecodeLimits,
}

impl DecodeRequest {
    /// Validates and binds the complete immutable decode request.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        snapshot: SourceSnapshot,
        owner: ObjectRef,
        dictionary_span: ByteSpan,
        encoded_span: ByteSpan,
        encoded: ByteSlice,
        plan: FilterPlan,
        profile: DecodeProfile,
        limits: DecodeLimits,
    ) -> Result<Self, DecodeError> {
        if encoded.identity() != snapshot.identity() {
            return Err(DecodeError::for_code(DecodeErrorCode::SourceChanged, None));
        }
        let range = encoded.range();
        if range.start() != encoded_span.start() || range.len() != encoded_span.len() {
            return Err(DecodeError::for_code(DecodeErrorCode::InvalidRequest, None));
        }
        if snapshot.len().is_some_and(|source_len| {
            encoded_span.end_exclusive() > source_len
                || dictionary_span.end_exclusive() > source_len
        }) {
            return Err(DecodeError::for_code(DecodeErrorCode::InvalidRequest, None));
        }
        Ok(Self {
            snapshot,
            owner,
            dictionary_span,
            encoded_span,
            encoded,
            plan,
            profile,
            limits,
        })
    }
}

impl fmt::Debug for DecodeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodeRequest")
            .field("snapshot", &self.snapshot)
            .field("owner", &self.owner)
            .field("dictionary_span", &self.dictionary_span)
            .field("encoded_span", &self.encoded_span)
            .field("encoded", &self.encoded)
            .field("plan", &self.plan)
            .field("profile", &self.profile)
            .field("limits", &self.limits)
            .finish()
    }
}

/// Zero-based position in decoded output, never in physical source bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DecodedOffset(u64);

impl DecodedOffset {
    /// Creates a decoded-relative offset.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the decoded-relative numeric value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Checked half-open range in decoded output coordinates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DecodedRange {
    start: DecodedOffset,
    len: u64,
}

impl DecodedRange {
    /// Creates a decoded-relative range and rejects exclusive-end overflow.
    pub const fn new(start: DecodedOffset, len: u64) -> Option<Self> {
        if start.0.checked_add(len).is_some() {
            Some(Self { start, len })
        } else {
            None
        }
    }

    /// Returns the first decoded-relative byte position.
    pub const fn start(self) -> DecodedOffset {
        self.start
    }

    /// Returns the number of decoded bytes.
    pub const fn len(self) -> u64 {
        self.len
    }

    /// Reports whether the range contains no decoded bytes.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Returns the checked decoded-relative exclusive end.
    pub const fn end_exclusive(self) -> DecodedOffset {
        DecodedOffset(self.start.0 + self.len)
    }
}

/// Sealed evidence binding decoded bytes to their complete decode context.
///
/// This type intentionally does not implement [`Clone`]. Callers may only
/// borrow it from a successful [`DecodedStream`].
pub struct DecodeAttestation {
    pub(crate) snapshot: SourceSnapshot,
    pub(crate) source_identity: SourceIdentity,
    pub(crate) owner: ObjectRef,
    pub(crate) dictionary_span: ByteSpan,
    pub(crate) encoded_span: ByteSpan,
    pub(crate) encoded: ByteSlice,
    pub(crate) plan: FilterPlan,
    pub(crate) profile: DecodeProfile,
    pub(crate) limits: DecodeLimits,
    pub(crate) fuel_schedule: DecodeFuelScheduleVersion,
    pub(crate) fuel_consumed: u64,
    pub(crate) cumulative_output_bytes: u64,
    pub(crate) peak_retained_capacity_bytes: u64,
    pub(crate) decoded_length: u64,
}

impl DecodeAttestation {
    /// Returns the immutable source snapshot bound to the decode.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the immutable source identity bound to every physical span.
    pub const fn source_identity(&self) -> SourceIdentity {
        self.source_identity
    }

    /// Returns the indirect object that owns the stream.
    pub const fn owner(&self) -> ObjectRef {
        self.owner
    }

    /// Returns the exact physical source span of the owning dictionary.
    pub const fn dictionary_span(&self) -> ByteSpan {
        self.dictionary_span
    }

    /// Returns the exact physical source span of the encoded bytes.
    pub const fn encoded_span(&self) -> ByteSpan {
        self.encoded_span
    }

    /// Borrows the exact stable physical encoded byte slice.
    pub const fn encoded(&self) -> &ByteSlice {
        &self.encoded
    }

    /// Returns the canonical ordered filter plan.
    pub const fn filter_plan(&self) -> &FilterPlan {
        &self.plan
    }

    /// Returns the strict decode profile.
    pub const fn profile(&self) -> DecodeProfile {
        self.profile
    }

    /// Returns all deterministic limits bound to the result.
    pub const fn limits(&self) -> DecodeLimits {
        self.limits
    }

    /// Returns the versioned fuel schedule used by the decode.
    pub const fn fuel_schedule(&self) -> DecodeFuelScheduleVersion {
        self.fuel_schedule
    }

    /// Returns deterministic fuel consumed by the successful decode.
    pub const fn fuel_consumed(&self) -> u64 {
        self.fuel_consumed
    }

    /// Returns cumulative bytes emitted across every layer.
    pub const fn cumulative_output_bytes(&self) -> u64 {
        self.cumulative_output_bytes
    }

    /// Returns peak allocator-reported capacity retained by simultaneous outputs.
    pub const fn peak_retained_capacity_bytes(&self) -> u64 {
        self.peak_retained_capacity_bytes
    }

    /// Returns the final decoded byte length.
    pub const fn decoded_length(&self) -> u64 {
        self.decoded_length
    }
}

impl fmt::Debug for DecodeAttestation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodeAttestation")
            .field("snapshot", &self.snapshot)
            .field("source_identity", &self.source_identity)
            .field("owner", &self.owner)
            .field("dictionary_span", &self.dictionary_span)
            .field("encoded_span", &self.encoded_span)
            .field("encoded", &self.encoded)
            .field("plan", &self.plan)
            .field("profile", &self.profile)
            .field("limits", &self.limits)
            .field("fuel_schedule", &self.fuel_schedule)
            .field("fuel_consumed", &self.fuel_consumed)
            .field("cumulative_output_bytes", &self.cumulative_output_bytes)
            .field(
                "peak_retained_capacity_bytes",
                &self.peak_retained_capacity_bytes,
            )
            .field("decoded_length", &self.decoded_length)
            .finish()
    }
}

/// Immutable decoded bytes inseparable from their sealed attestation.
///
/// This type intentionally does not implement [`Clone`] and exposes no
/// consuming byte extraction that could discard the attestation.
pub struct DecodedStream {
    pub(crate) bytes: Vec<u8>,
    pub(crate) attestation: DecodeAttestation,
}

impl DecodedStream {
    /// Borrows the final decoded bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the final decoded byte length.
    pub const fn len(&self) -> u64 {
        self.attestation.decoded_length
    }

    /// Reports whether the decoded result is empty.
    pub const fn is_empty(&self) -> bool {
        self.attestation.decoded_length == 0
    }

    /// Returns the complete decoded-relative range.
    pub const fn decoded_range(&self) -> DecodedRange {
        DecodedRange {
            start: DecodedOffset(0),
            len: self.attestation.decoded_length,
        }
    }

    /// Borrows a checked decoded-relative subrange.
    pub fn slice(&self, range: DecodedRange) -> Option<&[u8]> {
        if range.end_exclusive().value() > self.attestation.decoded_length {
            return None;
        }
        let start = usize::try_from(range.start().value()).ok()?;
        let end = usize::try_from(range.end_exclusive().value()).ok()?;
        self.bytes.get(start..end)
    }

    /// Borrows the sealed decode attestation.
    pub const fn attestation(&self) -> &DecodeAttestation {
        &self.attestation
    }
}

impl fmt::Debug for DecodedStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedStream")
            .field("bytes", &"[REDACTED]")
            .field("attestation", &self.attestation)
            .finish()
    }
}
