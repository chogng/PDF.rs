use std::fmt;
use std::mem;

use pdf_rs_bytes::{SourceIdentity, SourceSnapshot};
use pdf_rs_syntax::{ByteSpan, ObjectRef, PdfDictionary, SyntaxObject};

use crate::{XrefCancellation, XrefRecoverability};

const HARD_MAX_DECODED_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_ENTRIES: u64 = 4_000_000;
const HARD_MAX_INDEX_PAIRS: u64 = 65_536;
const HARD_MAX_RETAINED_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_FIELD_WIDTH: u8 = 8;
const HARD_MAX_ROW_WIDTH: u8 = 24;
const CANCELLATION_INTERVAL: usize = 256;

/// Unvalidated deterministic limits for one decoded cross-reference stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefStreamLimitConfig {
    /// Maximum bytes in the complete decoded entry payload.
    pub max_decoded_bytes: u64,
    /// Maximum decoded cross-reference records.
    pub max_entries: u64,
    /// Maximum pairs in an explicit `/Index` array.
    pub max_index_pairs: u64,
    /// Maximum bytes in any one `/W` field.
    pub max_field_width: u8,
    /// Maximum allocator-reported entry-vector capacity bytes.
    pub max_retained_entry_bytes: u64,
}

impl Default for XrefStreamLimitConfig {
    fn default() -> Self {
        Self {
            max_decoded_bytes: 16 * 1024 * 1024,
            max_entries: 100_000,
            max_index_pairs: 4096,
            max_field_width: HARD_MAX_FIELD_WIDTH,
            max_retained_entry_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Validated deterministic limits for one decoded cross-reference stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefStreamLimits {
    max_decoded_bytes: u64,
    max_entries: u64,
    max_index_pairs: u64,
    max_field_width: u8,
    max_retained_entry_bytes: u64,
}

impl XrefStreamLimits {
    /// Validates a complete cross-reference-stream budget profile.
    pub fn validate(config: XrefStreamLimitConfig) -> Result<Self, XrefStreamError> {
        if config.max_decoded_bytes == 0
            || config.max_decoded_bytes > HARD_MAX_DECODED_BYTES
            || config.max_entries == 0
            || config.max_entries > HARD_MAX_ENTRIES
            || config.max_index_pairs == 0
            || config.max_index_pairs > HARD_MAX_INDEX_PAIRS
            || config.max_field_width == 0
            || config.max_field_width > HARD_MAX_FIELD_WIDTH
            || config.max_retained_entry_bytes == 0
            || config.max_retained_entry_bytes > HARD_MAX_RETAINED_ENTRY_BYTES
        {
            return Err(XrefStreamError::at_source(
                XrefStreamErrorCode::InvalidLimits,
                None,
            ));
        }
        Ok(Self {
            max_decoded_bytes: config.max_decoded_bytes,
            max_entries: config.max_entries,
            max_index_pairs: config.max_index_pairs,
            max_field_width: config.max_field_width,
            max_retained_entry_bytes: config.max_retained_entry_bytes,
        })
    }

    /// Returns the decoded payload byte ceiling.
    pub const fn max_decoded_bytes(self) -> u64 {
        self.max_decoded_bytes
    }

    /// Returns the record ceiling.
    pub const fn max_entries(self) -> u64 {
        self.max_entries
    }

    /// Returns the explicit `/Index` pair ceiling.
    pub const fn max_index_pairs(self) -> u64 {
        self.max_index_pairs
    }

    /// Returns the per-field width ceiling.
    pub const fn max_field_width(self) -> u8 {
        self.max_field_width
    }

    /// Returns the retained entry-vector capacity byte ceiling.
    pub const fn max_retained_entry_bytes(self) -> u64 {
        self.max_retained_entry_bytes
    }
}

impl Default for XrefStreamLimits {
    fn default() -> Self {
        Self::validate(XrefStreamLimitConfig::default())
            .expect("built-in xref-stream limits satisfy hard ceilings")
    }
}

/// Deterministic xref-stream budget that rejected work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefStreamLimitKind {
    /// Complete decoded payload bytes.
    DecodedBytes,
    /// Declared or parsed entry count.
    Entries,
    /// Explicit `/Index` pairs.
    IndexPairs,
    /// Retained entry-vector capacity bytes.
    RetainedEntries,
}

/// Stable machine-readable xref-stream failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefStreamErrorCode {
    /// The caller supplied an invalid limit profile.
    InvalidLimits,
    /// Dictionary or payload source identity/geometry is inconsistent.
    SourceMismatch,
    /// `/Type`, `/Size`, `/Root`, or `/Prev` is duplicated or malformed.
    InvalidDictionary,
    /// The unfiltered entry point received `/Filter` or `/DecodeParms`.
    UnsupportedFilter,
    /// `/W` is missing, duplicated, malformed, or outside the width profile.
    InvalidWidths,
    /// `/Index` is malformed, overlapping, unordered, or outside `/Size`.
    InvalidIndex,
    /// Payload length does not exactly match `/W` and `/Index` geometry.
    InvalidPayloadLength,
    /// One decoded row has an unknown type or an out-of-range field.
    InvalidEntry,
    /// Deterministic work or retained capacity exceeded its ceiling.
    ResourceLimit,
    /// The owning runtime cancelled parsing.
    Cancelled,
    /// A checked implementation invariant could not be maintained.
    InternalState,
}

/// Coarse xref-stream failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefStreamErrorCategory {
    /// Invalid caller configuration.
    Configuration,
    /// Source proof or geometry mismatch.
    Source,
    /// Malformed PDF stream metadata or decoded rows.
    Syntax,
    /// A filter is outside this unfiltered entry point.
    Unsupported,
    /// Deterministic resource exhaustion.
    Resource,
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal implementation failure.
    Internal,
}

/// Redacted xref-stream error with separate source and decoded coordinates.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct XrefStreamError {
    code: XrefStreamErrorCode,
    category: XrefStreamErrorCategory,
    recoverability: XrefRecoverability,
    diagnostic_id: &'static str,
    source_offset: Option<u64>,
    decoded_offset: Option<u64>,
    limit: Option<(XrefStreamLimitKind, u64, u64)>,
}

impl XrefStreamError {
    fn at_source(code: XrefStreamErrorCode, source_offset: Option<u64>) -> Self {
        let (category, recoverability, diagnostic_id) = policy(code);
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            source_offset,
            decoded_offset: None,
            limit: None,
        }
    }

    fn at_decoded(code: XrefStreamErrorCode, decoded_offset: u64) -> Self {
        let (category, recoverability, diagnostic_id) = policy(code);
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            source_offset: None,
            decoded_offset: Some(decoded_offset),
            limit: None,
        }
    }

    fn resource(
        kind: XrefStreamLimitKind,
        limit: u64,
        attempted: u64,
        source_offset: Option<u64>,
    ) -> Self {
        let mut error = Self::at_source(XrefStreamErrorCode::ResourceLimit, source_offset);
        error.limit = Some((kind, limit, attempted));
        error
    }

    /// Returns the stable failure code.
    pub const fn code(self) -> XrefStreamErrorCode {
        self.code
    }

    /// Returns the coarse failure category.
    pub const fn category(self) -> XrefStreamErrorCategory {
        self.category
    }

    /// Returns the stable approved recovery policy.
    pub const fn recoverability(self) -> XrefRecoverability {
        self.recoverability
    }

    /// Returns the stable redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns a physical source offset when the failure belongs to the dictionary or envelope.
    pub const fn source_offset(self) -> Option<u64> {
        self.source_offset
    }

    /// Returns a relative decoded-payload offset when the failure belongs to a row.
    pub const fn decoded_offset(self) -> Option<u64> {
        self.decoded_offset
    }

    /// Returns resource-limit context as kind, limit, and attempted amount.
    pub const fn limit(self) -> Option<(XrefStreamLimitKind, u64, u64)> {
        self.limit
    }
}

impl fmt::Debug for XrefStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XrefStreamError")
            .field("code", &self.code)
            .field("category", &self.category)
            .field("recoverability", &self.recoverability)
            .field("diagnostic_id", &self.diagnostic_id)
            .field("source_offset", &self.source_offset)
            .field("decoded_offset", &self.decoded_offset)
            .field("limit", &self.limit)
            .finish()
    }
}

impl fmt::Display for XrefStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {:?}", self.diagnostic_id, self.code)
    }
}

impl std::error::Error for XrefStreamError {}

const fn policy(
    code: XrefStreamErrorCode,
) -> (XrefStreamErrorCategory, XrefRecoverability, &'static str) {
    match code {
        XrefStreamErrorCode::InvalidLimits => (
            XrefStreamErrorCategory::Configuration,
            XrefRecoverability::CorrectConfiguration,
            "RPE-XREF-0101",
        ),
        XrefStreamErrorCode::SourceMismatch => (
            XrefStreamErrorCategory::Source,
            XrefRecoverability::ReopenSource,
            "RPE-XREF-0102",
        ),
        XrefStreamErrorCode::InvalidDictionary => (
            XrefStreamErrorCategory::Syntax,
            XrefRecoverability::CorrectInput,
            "RPE-XREF-0103",
        ),
        XrefStreamErrorCode::UnsupportedFilter => (
            XrefStreamErrorCategory::Unsupported,
            XrefRecoverability::UseSupportedFeature,
            "RPE-XREF-0104",
        ),
        XrefStreamErrorCode::InvalidWidths => (
            XrefStreamErrorCategory::Syntax,
            XrefRecoverability::CorrectInput,
            "RPE-XREF-0105",
        ),
        XrefStreamErrorCode::InvalidIndex => (
            XrefStreamErrorCategory::Syntax,
            XrefRecoverability::CorrectInput,
            "RPE-XREF-0106",
        ),
        XrefStreamErrorCode::InvalidPayloadLength => (
            XrefStreamErrorCategory::Syntax,
            XrefRecoverability::CorrectInput,
            "RPE-XREF-0107",
        ),
        XrefStreamErrorCode::InvalidEntry => (
            XrefStreamErrorCategory::Syntax,
            XrefRecoverability::CorrectInput,
            "RPE-XREF-0108",
        ),
        XrefStreamErrorCode::ResourceLimit => (
            XrefStreamErrorCategory::Resource,
            XrefRecoverability::ReduceWorkload,
            "RPE-XREF-0109",
        ),
        XrefStreamErrorCode::Cancelled => (
            XrefStreamErrorCategory::Cancellation,
            XrefRecoverability::AbandonOperation,
            "RPE-XREF-0110",
        ),
        XrefStreamErrorCode::InternalState => (
            XrefStreamErrorCategory::Internal,
            XrefRecoverability::DoNotRetry,
            "RPE-XREF-0111",
        ),
    }
}

/// Relative half-open span in one decoded xref-stream payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedXrefSpan {
    start: u64,
    len: u64,
}

impl DecodedXrefSpan {
    /// Returns the relative decoded-payload start.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Returns the decoded row length.
    pub const fn len(self) -> u64 {
        self.len
    }

    /// Returns whether the decoded span is empty.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Returns the relative exclusive end.
    pub const fn end_exclusive(self) -> u64 {
        self.start + self.len
    }
}

/// Semantic payload of one xref-stream row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefStreamEntryKind {
    /// A free entry.
    Free {
        /// Next object number in the free chain.
        next_free: u32,
        /// Generation number.
        generation: u16,
    },
    /// An uncompressed indirect object at a physical source offset.
    Uncompressed {
        /// Absolute source offset of the indirect object header.
        offset: u64,
        /// Generation number.
        generation: u16,
    },
    /// An object stored inside an object stream.
    Compressed {
        /// Nonzero object number of the containing object stream.
        object_stream: u32,
        /// Zero-based index inside the object stream.
        index: u32,
    },
}

/// One source-bound record decoded from an xref-stream payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefStreamEntry {
    object_number: u32,
    decoded_span: DecodedXrefSpan,
    kind: XrefStreamEntryKind,
}

impl XrefStreamEntry {
    /// Returns the indexed object number.
    pub const fn object_number(self) -> u32 {
        self.object_number
    }

    /// Returns the row's relative decoded-payload span, not a physical source span.
    pub const fn decoded_span(self) -> DecodedXrefSpan {
        self.decoded_span
    }

    /// Returns the decoded row payload.
    pub const fn kind(self) -> XrefStreamEntryKind {
        self.kind
    }
}

/// Deterministic work and retained-capacity evidence for one parsed xref stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefStreamStats {
    decoded_bytes: u64,
    entries: u64,
    index_pairs: u64,
    retained_entry_bytes: u64,
}

impl XrefStreamStats {
    /// Returns complete decoded bytes validated.
    pub const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }

    /// Returns decoded record count.
    pub const fn entries(self) -> u64 {
        self.entries
    }

    /// Returns normalized `/Index` pair count.
    pub const fn index_pairs(self) -> u64 {
        self.index_pairs
    }

    /// Returns allocator-reported retained entry capacity bytes.
    pub const fn retained_entry_bytes(self) -> u64 {
        self.retained_entry_bytes
    }
}

/// One validated unfiltered xref-stream table.
#[derive(Clone, Eq, PartialEq)]
pub struct XrefStream {
    snapshot: SourceSnapshot,
    container: ObjectRef,
    encoded_payload_span: ByteSpan,
    declared_size: u32,
    root: Option<ObjectRef>,
    previous: Option<u64>,
    widths: [u8; 3],
    entries: Vec<XrefStreamEntry>,
    stats: XrefStreamStats,
}

impl XrefStream {
    /// Returns the immutable source identity.
    pub const fn source(&self) -> SourceIdentity {
        self.snapshot.identity()
    }

    /// Returns the complete immutable source snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the xref-stream container object identity.
    pub const fn container(&self) -> ObjectRef {
        self.container
    }

    /// Returns the physical source span of the unfiltered encoded payload.
    pub const fn encoded_payload_span(&self) -> ByteSpan {
        self.encoded_payload_span
    }

    /// Returns `/Size`.
    pub const fn declared_size(&self) -> u32 {
        self.declared_size
    }

    /// Returns optional `/Root` for later primary/hybrid composition.
    pub const fn root(&self) -> Option<ObjectRef> {
        self.root
    }

    /// Returns optional `/Prev` for later revision-chain composition.
    pub const fn previous(&self) -> Option<u64> {
        self.previous
    }

    /// Returns the validated three `/W` widths.
    pub const fn widths(&self) -> [u8; 3] {
        self.widths
    }

    /// Returns records in `/Index` order.
    pub fn entries(&self) -> &[XrefStreamEntry] {
        &self.entries
    }

    /// Returns deterministic work and retained-capacity evidence.
    pub const fn stats(&self) -> XrefStreamStats {
        self.stats
    }
}

impl fmt::Debug for XrefStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XrefStream")
            .field("snapshot", &self.snapshot)
            .field("container", &self.container)
            .field("encoded_payload_span", &self.encoded_payload_span)
            .field("declared_size", &self.declared_size)
            .field("root", &self.root)
            .field("previous", &self.previous)
            .field("widths", &self.widths)
            .field("entry_count", &self.entries.len())
            .field("stats", &self.stats)
            .finish()
    }
}

/// Parses one complete, explicitly unfiltered xref-stream payload.
///
/// This entry point rejects `/Filter` and `/DecodeParms`. Filtered payloads must first cross a
/// separate proof-bearing decode boundary; decoded row coordinates are never exposed as physical
/// source [`ByteSpan`] values.
pub fn parse_unfiltered_xref_stream(
    snapshot: SourceSnapshot,
    container: ObjectRef,
    dictionary: &PdfDictionary,
    encoded_payload_span: ByteSpan,
    payload: &[u8],
    limits: XrefStreamLimits,
    cancellation: &(dyn XrefCancellation + '_),
) -> Result<XrefStream, XrefStreamError> {
    validate_source_geometry(snapshot, dictionary, encoded_payload_span, payload)?;
    check_cancelled(cancellation)?;

    if unique_value(dictionary, b"Filter", cancellation)?.is_some()
        || unique_value(dictionary, b"DecodeParms", cancellation)?.is_some()
    {
        return Err(XrefStreamError::at_source(
            XrefStreamErrorCode::UnsupportedFilter,
            Some(encoded_payload_span.start()),
        ));
    }
    require_name(dictionary, b"Type", b"XRef", cancellation)?;
    let declared_size = require_positive_u32(dictionary, b"Size", cancellation)?;
    if u64::from(declared_size) > limits.max_entries {
        return Err(XrefStreamError::resource(
            XrefStreamLimitKind::Entries,
            limits.max_entries,
            u64::from(declared_size),
            value_offset(dictionary, b"Size"),
        ));
    }
    let root = optional_reference(dictionary, b"Root", cancellation)?;
    let previous = optional_nonnegative_u64(dictionary, b"Prev", cancellation)?;
    let widths = parse_widths(dictionary, limits, cancellation)?;
    let index = parse_index(dictionary, declared_size, limits, cancellation)?;

    let row_width = widths
        .iter()
        .try_fold(0_u8, |total, width| total.checked_add(*width))
        .filter(|value| *value != 0 && *value <= HARD_MAX_ROW_WIDTH)
        .ok_or_else(|| {
            XrefStreamError::at_source(
                XrefStreamErrorCode::InvalidWidths,
                value_offset(dictionary, b"W"),
            )
        })?;
    let entry_count = index.iter().try_fold(0_u64, |total, (_, count)| {
        total.checked_add(u64::from(*count))
    });
    let entry_count = entry_count.ok_or_else(|| {
        XrefStreamError::at_source(
            XrefStreamErrorCode::InvalidIndex,
            value_offset(dictionary, b"Index"),
        )
    })?;
    if entry_count > limits.max_entries {
        return Err(XrefStreamError::resource(
            XrefStreamLimitKind::Entries,
            limits.max_entries,
            entry_count,
            value_offset(dictionary, b"Index"),
        ));
    }
    let expected_len = entry_count
        .checked_mul(u64::from(row_width))
        .ok_or_else(|| {
            XrefStreamError::at_source(
                XrefStreamErrorCode::InvalidPayloadLength,
                Some(encoded_payload_span.start()),
            )
        })?;
    let payload_len = u64::try_from(payload.len()).map_err(|_| {
        XrefStreamError::resource(
            XrefStreamLimitKind::DecodedBytes,
            limits.max_decoded_bytes,
            u64::MAX,
            Some(encoded_payload_span.start()),
        )
    })?;
    if payload_len > limits.max_decoded_bytes {
        return Err(XrefStreamError::resource(
            XrefStreamLimitKind::DecodedBytes,
            limits.max_decoded_bytes,
            payload_len,
            Some(encoded_payload_span.start()),
        ));
    }
    if payload_len != expected_len {
        return Err(XrefStreamError::at_source(
            XrefStreamErrorCode::InvalidPayloadLength,
            Some(encoded_payload_span.start()),
        ));
    }

    let requested_entry_bytes = entry_count
        .checked_mul(mem::size_of::<XrefStreamEntry>() as u64)
        .ok_or_else(|| {
            XrefStreamError::resource(
                XrefStreamLimitKind::RetainedEntries,
                limits.max_retained_entry_bytes,
                u64::MAX,
                Some(encoded_payload_span.start()),
            )
        })?;
    if requested_entry_bytes > limits.max_retained_entry_bytes {
        return Err(XrefStreamError::resource(
            XrefStreamLimitKind::RetainedEntries,
            limits.max_retained_entry_bytes,
            requested_entry_bytes,
            Some(encoded_payload_span.start()),
        ));
    }
    let capacity = usize::try_from(entry_count).map_err(|_| {
        XrefStreamError::resource(
            XrefStreamLimitKind::RetainedEntries,
            limits.max_retained_entry_bytes,
            requested_entry_bytes,
            Some(encoded_payload_span.start()),
        )
    })?;
    let mut entries = Vec::new();
    entries.try_reserve_exact(capacity).map_err(|_| {
        XrefStreamError::resource(
            XrefStreamLimitKind::RetainedEntries,
            limits.max_retained_entry_bytes,
            requested_entry_bytes,
            Some(encoded_payload_span.start()),
        )
    })?;
    let retained_entry_bytes = u64::try_from(entries.capacity())
        .ok()
        .and_then(|capacity| capacity.checked_mul(mem::size_of::<XrefStreamEntry>() as u64))
        .ok_or_else(|| {
            XrefStreamError::resource(
                XrefStreamLimitKind::RetainedEntries,
                limits.max_retained_entry_bytes,
                u64::MAX,
                Some(encoded_payload_span.start()),
            )
        })?;
    if retained_entry_bytes > limits.max_retained_entry_bytes {
        return Err(XrefStreamError::resource(
            XrefStreamLimitKind::RetainedEntries,
            limits.max_retained_entry_bytes,
            retained_entry_bytes,
            Some(encoded_payload_span.start()),
        ));
    }

    let mut cursor = 0_usize;
    for (range_index, (first, count)) in index.iter().copied().enumerate() {
        if range_index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        for relative in 0..count {
            if entries.len().is_multiple_of(CANCELLATION_INTERVAL) {
                check_cancelled(cancellation)?;
            }
            let object_number = first.checked_add(relative).ok_or_else(|| {
                XrefStreamError::at_source(
                    XrefStreamErrorCode::InvalidIndex,
                    value_offset(dictionary, b"Index"),
                )
            })?;
            let row_start = cursor;
            let field_1 = read_field(payload, &mut cursor, widths[0])?;
            let field_2 = read_field(payload, &mut cursor, widths[1])?;
            let field_3 = read_field(payload, &mut cursor, widths[2])?;
            let entry_type = if widths[0] == 0 { 1 } else { field_1 };
            let decoded_start = u64::try_from(row_start).map_err(|_| {
                XrefStreamError::at_source(XrefStreamErrorCode::InternalState, None)
            })?;
            let kind = match entry_type {
                0 => XrefStreamEntryKind::Free {
                    next_free: u32::try_from(field_2).map_err(|_| {
                        XrefStreamError::at_decoded(
                            XrefStreamErrorCode::InvalidEntry,
                            decoded_start,
                        )
                    })?,
                    generation: u16::try_from(field_3).map_err(|_| {
                        XrefStreamError::at_decoded(
                            XrefStreamErrorCode::InvalidEntry,
                            decoded_start,
                        )
                    })?,
                },
                1 => XrefStreamEntryKind::Uncompressed {
                    offset: field_2,
                    generation: u16::try_from(field_3).map_err(|_| {
                        XrefStreamError::at_decoded(
                            XrefStreamErrorCode::InvalidEntry,
                            decoded_start,
                        )
                    })?,
                },
                2 => XrefStreamEntryKind::Compressed {
                    object_stream: u32::try_from(field_2)
                        .ok()
                        .filter(|value| *value != 0)
                        .ok_or_else(|| {
                            XrefStreamError::at_decoded(
                                XrefStreamErrorCode::InvalidEntry,
                                decoded_start,
                            )
                        })?,
                    index: u32::try_from(field_3).map_err(|_| {
                        XrefStreamError::at_decoded(
                            XrefStreamErrorCode::InvalidEntry,
                            decoded_start,
                        )
                    })?,
                },
                _ => {
                    return Err(XrefStreamError::at_decoded(
                        XrefStreamErrorCode::InvalidEntry,
                        decoded_start,
                    ));
                }
            };
            entries.push(XrefStreamEntry {
                object_number,
                decoded_span: DecodedXrefSpan {
                    start: decoded_start,
                    len: u64::from(row_width),
                },
                kind,
            });
        }
    }
    check_cancelled(cancellation)?;
    if cursor != payload.len() {
        return Err(XrefStreamError::at_source(
            XrefStreamErrorCode::InternalState,
            Some(encoded_payload_span.start()),
        ));
    }

    Ok(XrefStream {
        snapshot,
        container,
        encoded_payload_span,
        declared_size,
        root,
        previous,
        widths,
        entries,
        stats: XrefStreamStats {
            decoded_bytes: payload_len,
            entries: entry_count,
            index_pairs: u64::try_from(index.len()).map_err(|_| {
                XrefStreamError::at_source(XrefStreamErrorCode::InternalState, None)
            })?,
            retained_entry_bytes,
        },
    })
}

fn validate_source_geometry(
    snapshot: SourceSnapshot,
    dictionary: &PdfDictionary,
    encoded_span: ByteSpan,
    payload: &[u8],
) -> Result<(), XrefStreamError> {
    let payload_len = u64::try_from(payload.len()).map_err(|_| {
        XrefStreamError::at_source(
            XrefStreamErrorCode::SourceMismatch,
            Some(encoded_span.start()),
        )
    })?;
    if payload_len != encoded_span.len()
        || snapshot
            .len()
            .is_some_and(|source_len| encoded_span.end_exclusive() > source_len)
    {
        return Err(XrefStreamError::at_source(
            XrefStreamErrorCode::SourceMismatch,
            Some(encoded_span.start()),
        ));
    }
    for entry in dictionary.entries() {
        for (source, span) in [
            (entry.key().source(), entry.key().span()),
            (entry.value().source(), entry.value().span()),
        ] {
            if source != snapshot.identity()
                || snapshot
                    .len()
                    .is_some_and(|source_len| span.end_exclusive() > source_len)
            {
                return Err(XrefStreamError::at_source(
                    XrefStreamErrorCode::SourceMismatch,
                    Some(span.start()),
                ));
            }
        }
    }
    Ok(())
}

fn parse_widths(
    dictionary: &PdfDictionary,
    limits: XrefStreamLimits,
    cancellation: &dyn XrefCancellation,
) -> Result<[u8; 3], XrefStreamError> {
    let value = unique_value(dictionary, b"W", cancellation)?
        .ok_or_else(|| XrefStreamError::at_source(XrefStreamErrorCode::InvalidWidths, None))?;
    let SyntaxObject::Array(array) = value else {
        return Err(XrefStreamError::at_source(
            XrefStreamErrorCode::InvalidWidths,
            value_offset(dictionary, b"W"),
        ));
    };
    if array.values().len() != 3 {
        return Err(XrefStreamError::at_source(
            XrefStreamErrorCode::InvalidWidths,
            value_offset(dictionary, b"W"),
        ));
    }
    let mut widths = [0_u8; 3];
    for (index, value) in array.values().iter().enumerate() {
        let width = value
            .value()
            .as_integer()
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value <= limits.max_field_width)
            .ok_or_else(|| {
                XrefStreamError::at_source(
                    XrefStreamErrorCode::InvalidWidths,
                    Some(value.span().start()),
                )
            })?;
        widths[index] = width;
    }
    Ok(widths)
}

fn parse_index(
    dictionary: &PdfDictionary,
    declared_size: u32,
    limits: XrefStreamLimits,
    cancellation: &dyn XrefCancellation,
) -> Result<Vec<(u32, u32)>, XrefStreamError> {
    let Some(value) = unique_value(dictionary, b"Index", cancellation)? else {
        return Ok(vec![(0, declared_size)]);
    };
    let SyntaxObject::Array(array) = value else {
        return Err(XrefStreamError::at_source(
            XrefStreamErrorCode::InvalidIndex,
            value_offset(dictionary, b"Index"),
        ));
    };
    if array.values().is_empty() || !array.values().len().is_multiple_of(2) {
        return Err(XrefStreamError::at_source(
            XrefStreamErrorCode::InvalidIndex,
            value_offset(dictionary, b"Index"),
        ));
    }
    let pairs = u64::try_from(array.values().len() / 2).map_err(|_| {
        XrefStreamError::resource(
            XrefStreamLimitKind::IndexPairs,
            limits.max_index_pairs,
            u64::MAX,
            value_offset(dictionary, b"Index"),
        )
    })?;
    if pairs > limits.max_index_pairs {
        return Err(XrefStreamError::resource(
            XrefStreamLimitKind::IndexPairs,
            limits.max_index_pairs,
            pairs,
            value_offset(dictionary, b"Index"),
        ));
    }
    let pair_capacity = usize::try_from(pairs).map_err(|_| {
        XrefStreamError::resource(
            XrefStreamLimitKind::IndexPairs,
            limits.max_index_pairs,
            pairs,
            value_offset(dictionary, b"Index"),
        )
    })?;
    let mut result = Vec::new();
    result.try_reserve_exact(pair_capacity).map_err(|_| {
        XrefStreamError::resource(
            XrefStreamLimitKind::IndexPairs,
            limits.max_index_pairs,
            pairs,
            value_offset(dictionary, b"Index"),
        )
    })?;
    let mut previous_end = 0_u32;
    for (pair_index, pair) in array.values().chunks_exact(2).enumerate() {
        if pair_index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        let first = nonnegative_u32(pair[0].value()).ok_or_else(|| {
            XrefStreamError::at_source(
                XrefStreamErrorCode::InvalidIndex,
                Some(pair[0].span().start()),
            )
        })?;
        let count = nonnegative_u32(pair[1].value())
            .filter(|value| *value != 0)
            .ok_or_else(|| {
                XrefStreamError::at_source(
                    XrefStreamErrorCode::InvalidIndex,
                    Some(pair[1].span().start()),
                )
            })?;
        let end = first
            .checked_add(count)
            .filter(|end| *end <= declared_size)
            .ok_or_else(|| {
                XrefStreamError::at_source(
                    XrefStreamErrorCode::InvalidIndex,
                    Some(pair[0].span().start()),
                )
            })?;
        if !result.is_empty() && first < previous_end {
            return Err(XrefStreamError::at_source(
                XrefStreamErrorCode::InvalidIndex,
                Some(pair[0].span().start()),
            ));
        }
        previous_end = end;
        result.push((first, count));
    }
    Ok(result)
}

fn require_name(
    dictionary: &PdfDictionary,
    key: &[u8],
    expected: &[u8],
    cancellation: &dyn XrefCancellation,
) -> Result<(), XrefStreamError> {
    match unique_value(dictionary, key, cancellation)? {
        Some(SyntaxObject::Name(name)) if name.bytes() == expected => Ok(()),
        _ => Err(XrefStreamError::at_source(
            XrefStreamErrorCode::InvalidDictionary,
            value_offset(dictionary, key),
        )),
    }
}

fn require_positive_u32(
    dictionary: &PdfDictionary,
    key: &[u8],
    cancellation: &dyn XrefCancellation,
) -> Result<u32, XrefStreamError> {
    unique_value(dictionary, key, cancellation)?
        .and_then(nonnegative_u32)
        .filter(|value| *value != 0)
        .ok_or_else(|| {
            XrefStreamError::at_source(
                XrefStreamErrorCode::InvalidDictionary,
                value_offset(dictionary, key),
            )
        })
}

fn optional_reference(
    dictionary: &PdfDictionary,
    key: &[u8],
    cancellation: &dyn XrefCancellation,
) -> Result<Option<ObjectRef>, XrefStreamError> {
    match unique_value(dictionary, key, cancellation)? {
        None => Ok(None),
        Some(SyntaxObject::Reference(reference)) => Ok(Some(*reference)),
        Some(_) => Err(XrefStreamError::at_source(
            XrefStreamErrorCode::InvalidDictionary,
            value_offset(dictionary, key),
        )),
    }
}

fn optional_nonnegative_u64(
    dictionary: &PdfDictionary,
    key: &[u8],
    cancellation: &dyn XrefCancellation,
) -> Result<Option<u64>, XrefStreamError> {
    match unique_value(dictionary, key, cancellation)? {
        None => Ok(None),
        Some(SyntaxObject::Integer(value)) => u64::try_from(*value).map(Some).map_err(|_| {
            XrefStreamError::at_source(
                XrefStreamErrorCode::InvalidDictionary,
                value_offset(dictionary, key),
            )
        }),
        Some(_) => Err(XrefStreamError::at_source(
            XrefStreamErrorCode::InvalidDictionary,
            value_offset(dictionary, key),
        )),
    }
}

fn unique_value<'a>(
    dictionary: &'a PdfDictionary,
    key: &[u8],
    cancellation: &dyn XrefCancellation,
) -> Result<Option<&'a SyntaxObject>, XrefStreamError> {
    let mut value = None;
    for (index, entry) in dictionary.entries().iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        if entry.key().value().bytes() == key {
            if value.is_some() {
                return Err(XrefStreamError::at_source(
                    XrefStreamErrorCode::InvalidDictionary,
                    Some(entry.key().span().start()),
                ));
            }
            value = Some(entry.value().value());
        }
    }
    Ok(value)
}

fn value_offset(dictionary: &PdfDictionary, key: &[u8]) -> Option<u64> {
    dictionary
        .entries()
        .iter()
        .find(|entry| entry.key().value().bytes() == key)
        .map(|entry| entry.value().span().start())
}

fn nonnegative_u32(value: &SyntaxObject) -> Option<u32> {
    value
        .as_integer()
        .and_then(|value| u32::try_from(value).ok())
}

fn read_field(payload: &[u8], cursor: &mut usize, width: u8) -> Result<u64, XrefStreamError> {
    let start = *cursor;
    let end = start
        .checked_add(usize::from(width))
        .ok_or_else(|| XrefStreamError::at_source(XrefStreamErrorCode::InternalState, None))?;
    let bytes = payload
        .get(start..end)
        .ok_or_else(|| XrefStreamError::at_source(XrefStreamErrorCode::InternalState, None))?;
    let mut value = 0_u64;
    for byte in bytes {
        value = value
            .checked_shl(8)
            .ok_or_else(|| XrefStreamError::at_source(XrefStreamErrorCode::InternalState, None))?
            | u64::from(*byte);
    }
    *cursor = end;
    Ok(value)
}

fn check_cancelled(cancellation: &dyn XrefCancellation) -> Result<(), XrefStreamError> {
    if cancellation.is_cancelled() {
        Err(XrefStreamError::at_source(
            XrefStreamErrorCode::Cancelled,
            None,
        ))
    } else {
        Ok(())
    }
}
