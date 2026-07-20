use std::fmt;
use std::mem;

use pdf_rs_bytes::{ByteSlice, SourceIdentity, SourceSnapshot};
use pdf_rs_syntax::{
    ByteSpan, InputExtent, Located, ObjectRef, PdfDictionary, PdfName, PdfReal, PdfString,
    SyntaxCancellation, SyntaxError, SyntaxErrorCode, SyntaxInput, SyntaxLimit, SyntaxLimitConfig,
    SyntaxLimits, SyntaxObject, SyntaxParser, SyntaxPoll,
};

use crate::{
    FramedStream, IndirectObject, IndirectObjectValue, ObjectCancellation, ObjectRecoverability,
};

const HARD_MAX_DECODED_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_OBJECTS: u64 = 1_000_000;
const HARD_MAX_HEADER_BYTES: u64 = 16 * 1024 * 1024;
const HARD_MAX_WORKING_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_RETAINED_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_RETAINED_VALUE_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_TOTAL_SYNTAX_BYTES: u64 = 256 * 1024 * 1024;
const CANCELLATION_INTERVAL: usize = 256;

/// Unvalidated deterministic limits for one decoded object-stream payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectStreamLimitConfig {
    /// Maximum bytes in the complete decoded payload.
    pub max_decoded_bytes: u64,
    /// Maximum embedded-object count declared by `/N`.
    pub max_objects: u64,
    /// Maximum decoded header bytes before `/First`.
    pub max_header_bytes: u64,
    /// Maximum allocator-reported temporary header/index capacity.
    pub max_working_bytes: u64,
    /// Maximum allocator-reported retained entry-vector capacity.
    pub max_retained_entry_bytes: u64,
    /// Maximum retained decoded scalar and container capacity.
    pub max_retained_value_bytes: u64,
    /// Maximum cumulative entry-window bytes submitted to the syntax parser.
    pub max_total_syntax_bytes: u64,
    /// Per-entry direct-object syntax profile.
    pub syntax: SyntaxLimits,
}

impl Default for ObjectStreamLimitConfig {
    fn default() -> Self {
        Self {
            max_decoded_bytes: 16 * 1024 * 1024,
            max_objects: 100_000,
            max_header_bytes: 4 * 1024 * 1024,
            max_working_bytes: 8 * 1024 * 1024,
            max_retained_entry_bytes: 32 * 1024 * 1024,
            max_retained_value_bytes: 64 * 1024 * 1024,
            max_total_syntax_bytes: 32 * 1024 * 1024,
            syntax: SyntaxLimits::default(),
        }
    }
}

/// Validated deterministic limits for one decoded object-stream payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectStreamLimits {
    max_decoded_bytes: u64,
    max_objects: u64,
    max_header_bytes: u64,
    max_working_bytes: u64,
    max_retained_entry_bytes: u64,
    max_retained_value_bytes: u64,
    max_total_syntax_bytes: u64,
    syntax: SyntaxLimits,
}

impl ObjectStreamLimits {
    /// Validates a complete object-stream resource profile.
    pub fn validate(config: ObjectStreamLimitConfig) -> Result<Self, ObjectStreamError> {
        if config.max_decoded_bytes == 0
            || config.max_decoded_bytes > HARD_MAX_DECODED_BYTES
            || config.max_objects == 0
            || config.max_objects > HARD_MAX_OBJECTS
            || config.max_header_bytes == 0
            || config.max_header_bytes > HARD_MAX_HEADER_BYTES
            || config.max_header_bytes > config.max_decoded_bytes
            || config.max_working_bytes == 0
            || config.max_working_bytes > HARD_MAX_WORKING_BYTES
            || config.max_retained_entry_bytes == 0
            || config.max_retained_entry_bytes > HARD_MAX_RETAINED_ENTRY_BYTES
            || config.max_retained_value_bytes == 0
            || config.max_retained_value_bytes > HARD_MAX_RETAINED_VALUE_BYTES
            || config.max_total_syntax_bytes == 0
            || config.max_total_syntax_bytes > HARD_MAX_TOTAL_SYNTAX_BYTES
        {
            return Err(ObjectStreamError::at_source(
                ObjectStreamErrorCode::InvalidLimits,
                None,
            ));
        }
        Ok(Self {
            max_decoded_bytes: config.max_decoded_bytes,
            max_objects: config.max_objects,
            max_header_bytes: config.max_header_bytes,
            max_working_bytes: config.max_working_bytes,
            max_retained_entry_bytes: config.max_retained_entry_bytes,
            max_retained_value_bytes: config.max_retained_value_bytes,
            max_total_syntax_bytes: config.max_total_syntax_bytes,
            syntax: config.syntax,
        })
    }

    /// Returns the complete decoded-payload byte ceiling.
    pub const fn max_decoded_bytes(self) -> u64 {
        self.max_decoded_bytes
    }

    /// Returns the embedded-object ceiling.
    pub const fn max_objects(self) -> u64 {
        self.max_objects
    }

    /// Returns the decoded header byte ceiling.
    pub const fn max_header_bytes(self) -> u64 {
        self.max_header_bytes
    }

    /// Returns the temporary header/index capacity ceiling.
    pub const fn max_working_bytes(self) -> u64 {
        self.max_working_bytes
    }

    /// Returns the retained entry-vector capacity ceiling.
    pub const fn max_retained_entry_bytes(self) -> u64 {
        self.max_retained_entry_bytes
    }

    /// Returns the retained decoded-value capacity ceiling.
    pub const fn max_retained_value_bytes(self) -> u64 {
        self.max_retained_value_bytes
    }

    /// Returns the cumulative syntax-window byte ceiling.
    pub const fn max_total_syntax_bytes(self) -> u64 {
        self.max_total_syntax_bytes
    }

    /// Returns the per-entry syntax profile.
    pub const fn syntax(self) -> SyntaxLimits {
        self.syntax
    }
}

impl Default for ObjectStreamLimits {
    fn default() -> Self {
        Self::validate(ObjectStreamLimitConfig::default())
            .expect("built-in object-stream limits satisfy hard ceilings")
    }
}

/// Object-stream budget dimension that rejected work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectStreamLimitKind {
    /// Complete decoded payload bytes.
    DecodedBytes,
    /// Declared embedded-object count.
    Objects,
    /// Header bytes before the first embedded object.
    HeaderBytes,
    /// Temporary header and duplicate-detection storage.
    WorkingBytes,
    /// Retained entry-vector capacity.
    RetainedEntries,
    /// Retained decoded scalar and container capacity.
    RetainedValues,
    /// Cumulative entry windows submitted to the syntax parser.
    TotalSyntaxBytes,
    /// Number of filters in canonical stream metadata.
    FilterCount,
    /// Allocator-visible bytes retained by the canonical filter plan.
    FilterPlanBytes,
    /// A fallible canonical-plan allocation failed within its validated count bound.
    FilterPlanAllocation,
}

/// Structured object-stream resource-limit context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectStreamLimit {
    kind: ObjectStreamLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl ObjectStreamLimit {
    /// Returns the rejected budget dimension.
    pub const fn kind(self) -> ObjectStreamLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns prior charged work.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the rejected addition or requirement.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable object-stream failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectStreamErrorCode {
    /// The caller supplied an invalid limit profile.
    InvalidLimits,
    /// Container, dictionary, payload, or snapshot evidence is inconsistent.
    SourceMismatch,
    /// A sealed decode proof does not authorize this exact framed container and payload.
    DecodeProofMismatch,
    /// `/Type`, `/N`, `/First`, direct filter metadata, or the container kind is invalid.
    InvalidDictionary,
    /// Filter metadata is unsupported by the selected object-stream profile.
    UnsupportedFilter,
    /// Header object-number/offset pairs are malformed or cross `/First`.
    InvalidHeader,
    /// Object numbers repeat in one object stream.
    DuplicateObjectNumber,
    /// An entry offset, slot boundary, or trailing region is invalid.
    InvalidEntryBoundary,
    /// The strict direct-object syntax parser rejected one entry.
    SyntaxFailure,
    /// Deterministic work or retained capacity exceeded its ceiling.
    ResourceLimit,
    /// The owning runtime cancelled parsing.
    Cancelled,
    /// A checked implementation invariant could not be maintained.
    InternalState,
}

/// Coarse object-stream failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectStreamErrorCategory {
    /// Invalid caller configuration.
    Configuration,
    /// Physical source proof or geometry mismatch.
    Source,
    /// Malformed object-stream metadata or decoded bytes.
    Syntax,
    /// A filtered stream is outside this unfiltered entry point.
    Unsupported,
    /// Deterministic resource exhaustion.
    Resource,
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal checked-state failure.
    Internal,
}

/// Redacted object-stream error with separate source and decoded coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectStreamError {
    code: ObjectStreamErrorCode,
    coordinate: ObjectStreamErrorCoordinate,
    detail: ObjectStreamErrorDetail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectStreamErrorCoordinate {
    None,
    Source(u64),
    Decoded(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectStreamErrorDetail {
    None,
    Limit(ObjectStreamLimit),
    Syntax {
        code: SyntaxErrorCode,
        limit: Option<SyntaxLimit>,
    },
}

impl ObjectStreamError {
    pub(crate) fn at_source(code: ObjectStreamErrorCode, source_offset: Option<u64>) -> Self {
        Self {
            code,
            coordinate: source_offset.map_or(
                ObjectStreamErrorCoordinate::None,
                ObjectStreamErrorCoordinate::Source,
            ),
            detail: ObjectStreamErrorDetail::None,
        }
    }

    fn at_decoded(code: ObjectStreamErrorCode, decoded_offset: u64) -> Self {
        Self {
            code,
            coordinate: ObjectStreamErrorCoordinate::Decoded(decoded_offset),
            detail: ObjectStreamErrorDetail::None,
        }
    }

    pub(crate) fn resource(
        kind: ObjectStreamLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        source_offset: Option<u64>,
        decoded_offset: Option<u64>,
    ) -> Self {
        let mut error = Self::at_source(ObjectStreamErrorCode::ResourceLimit, source_offset);
        if let Some(decoded_offset) = decoded_offset {
            error.coordinate = ObjectStreamErrorCoordinate::Decoded(decoded_offset);
        }
        error.detail = ObjectStreamErrorDetail::Limit(ObjectStreamLimit {
            kind,
            limit,
            consumed,
            attempted,
        });
        error
    }

    fn from_syntax(error: SyntaxError, decoded_offset: u64) -> Self {
        let code = match error.category() {
            pdf_rs_syntax::SyntaxErrorCategory::Resource => ObjectStreamErrorCode::ResourceLimit,
            pdf_rs_syntax::SyntaxErrorCategory::Cancellation => ObjectStreamErrorCode::Cancelled,
            pdf_rs_syntax::SyntaxErrorCategory::Configuration
            | pdf_rs_syntax::SyntaxErrorCategory::Internal => ObjectStreamErrorCode::InternalState,
            pdf_rs_syntax::SyntaxErrorCategory::Integrity => ObjectStreamErrorCode::SourceMismatch,
            pdf_rs_syntax::SyntaxErrorCategory::Syntax => ObjectStreamErrorCode::SyntaxFailure,
        };
        let mut mapped = Self::at_decoded(code, error.offset().unwrap_or(decoded_offset));
        mapped.detail = ObjectStreamErrorDetail::Syntax {
            code: error.code(),
            limit: error.limit(),
        };
        mapped
    }

    /// Returns the stable error code.
    pub const fn code(self) -> ObjectStreamErrorCode {
        self.code
    }

    /// Returns the coarse failure category.
    pub const fn category(self) -> ObjectStreamErrorCategory {
        object_stream_policy(self.code).0
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> ObjectRecoverability {
        object_stream_policy(self.code).1
    }

    /// Returns the stable redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        object_stream_policy(self.code).2
    }

    /// Returns the physical source offset when one is relevant.
    pub const fn source_offset(self) -> Option<u64> {
        match self.coordinate {
            ObjectStreamErrorCoordinate::Source(offset) => Some(offset),
            ObjectStreamErrorCoordinate::None | ObjectStreamErrorCoordinate::Decoded(_) => None,
        }
    }

    /// Returns the relative decoded-payload offset when one is relevant.
    pub const fn decoded_offset(self) -> Option<u64> {
        match self.coordinate {
            ObjectStreamErrorCoordinate::Decoded(offset) => Some(offset),
            ObjectStreamErrorCoordinate::None | ObjectStreamErrorCoordinate::Source(_) => None,
        }
    }

    /// Returns resource-limit context when applicable.
    pub const fn limit(self) -> Option<ObjectStreamLimit> {
        match self.detail {
            ObjectStreamErrorDetail::Limit(limit) => Some(limit),
            ObjectStreamErrorDetail::None | ObjectStreamErrorDetail::Syntax { .. } => None,
        }
    }

    /// Returns the lower syntax code without exposing its source-coordinate error wrapper.
    ///
    /// Object-stream syntax is parsed in decoded coordinates, so publishing the lower
    /// `SyntaxError::offset` would incorrectly label a decoded offset as an absolute source offset.
    pub const fn syntax_code(self) -> Option<SyntaxErrorCode> {
        match self.detail {
            ObjectStreamErrorDetail::Syntax { code, .. } => Some(code),
            ObjectStreamErrorDetail::None | ObjectStreamErrorDetail::Limit(_) => None,
        }
    }

    /// Returns lower syntax resource evidence without exposing its source-coordinate wrapper.
    pub const fn syntax_limit(self) -> Option<SyntaxLimit> {
        match self.detail {
            ObjectStreamErrorDetail::Syntax { limit, .. } => limit,
            ObjectStreamErrorDetail::None | ObjectStreamErrorDetail::Limit(_) => None,
        }
    }
}

const fn object_stream_policy(
    code: ObjectStreamErrorCode,
) -> (
    ObjectStreamErrorCategory,
    ObjectRecoverability,
    &'static str,
) {
    match code {
        ObjectStreamErrorCode::InvalidLimits => (
            ObjectStreamErrorCategory::Configuration,
            ObjectRecoverability::CorrectConfiguration,
            "RPE-OBJECT-0101",
        ),
        ObjectStreamErrorCode::SourceMismatch => (
            ObjectStreamErrorCategory::Source,
            ObjectRecoverability::ReopenSource,
            "RPE-OBJECT-0102",
        ),
        ObjectStreamErrorCode::DecodeProofMismatch => (
            ObjectStreamErrorCategory::Internal,
            ObjectRecoverability::DoNotRetry,
            "RPE-OBJECT-0112",
        ),
        ObjectStreamErrorCode::InvalidDictionary => (
            ObjectStreamErrorCategory::Syntax,
            ObjectRecoverability::CorrectInput,
            "RPE-OBJECT-0103",
        ),
        ObjectStreamErrorCode::UnsupportedFilter => (
            ObjectStreamErrorCategory::Unsupported,
            ObjectRecoverability::UseSupportedFeature,
            "RPE-OBJECT-0104",
        ),
        ObjectStreamErrorCode::InvalidHeader => (
            ObjectStreamErrorCategory::Syntax,
            ObjectRecoverability::CorrectInput,
            "RPE-OBJECT-0105",
        ),
        ObjectStreamErrorCode::DuplicateObjectNumber => (
            ObjectStreamErrorCategory::Syntax,
            ObjectRecoverability::CorrectInput,
            "RPE-OBJECT-0106",
        ),
        ObjectStreamErrorCode::InvalidEntryBoundary => (
            ObjectStreamErrorCategory::Syntax,
            ObjectRecoverability::CorrectInput,
            "RPE-OBJECT-0107",
        ),
        ObjectStreamErrorCode::SyntaxFailure => (
            ObjectStreamErrorCategory::Syntax,
            ObjectRecoverability::CorrectInput,
            "RPE-OBJECT-0108",
        ),
        ObjectStreamErrorCode::ResourceLimit => (
            ObjectStreamErrorCategory::Resource,
            ObjectRecoverability::ReduceWorkload,
            "RPE-OBJECT-0109",
        ),
        ObjectStreamErrorCode::Cancelled => (
            ObjectStreamErrorCategory::Cancellation,
            ObjectRecoverability::AbandonOperation,
            "RPE-OBJECT-0110",
        ),
        ObjectStreamErrorCode::InternalState => (
            ObjectStreamErrorCategory::Internal,
            ObjectRecoverability::DoNotRetry,
            "RPE-OBJECT-0111",
        ),
    }
}

/// Relative half-open span in one decoded object-stream payload.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DecodedObjectSpan {
    start: u64,
    len: u64,
}

impl DecodedObjectSpan {
    fn new(start: u64, len: u64) -> Result<Self, ObjectStreamError> {
        start
            .checked_add(len)
            .ok_or_else(|| {
                ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, start)
            })
            .map(|_| Self { start, len })
    }

    /// Returns the relative decoded-payload start.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Returns the decoded span length.
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

/// One decoded-coordinate object value.
#[derive(Debug, Eq, PartialEq)]
pub struct DecodedLocatedObject {
    span: DecodedObjectSpan,
    value: DecodedObject,
}

impl DecodedLocatedObject {
    /// Returns the relative decoded span, never a physical source span.
    pub const fn span(&self) -> DecodedObjectSpan {
        self.span
    }

    /// Borrows the decoded semantic value.
    pub const fn value(&self) -> &DecodedObject {
        &self.value
    }
}

/// One decoded-coordinate dictionary key/value pair.
#[derive(Debug, Eq, PartialEq)]
pub struct DecodedDictionaryEntry {
    key_span: DecodedObjectSpan,
    key: PdfName,
    value: DecodedLocatedObject,
}

impl DecodedDictionaryEntry {
    /// Returns the decoded-coordinate key span.
    pub const fn key_span(&self) -> DecodedObjectSpan {
        self.key_span
    }

    /// Borrows the decoded key.
    pub const fn key(&self) -> &PdfName {
        &self.key
    }

    /// Borrows the decoded-coordinate value.
    pub const fn value(&self) -> &DecodedLocatedObject {
        &self.value
    }
}

/// Ordered decoded-coordinate array.
#[derive(Debug, Eq, PartialEq)]
pub struct DecodedArray(Vec<DecodedLocatedObject>);

impl DecodedArray {
    /// Returns values in decoded stream order.
    pub fn values(&self) -> &[DecodedLocatedObject] {
        &self.0
    }
}

/// Ordered decoded-coordinate dictionary preserving duplicate keys.
#[derive(Debug, Eq, PartialEq)]
pub struct DecodedDictionary(Vec<DecodedDictionaryEntry>);

impl DecodedDictionary {
    /// Returns entries in decoded stream order.
    pub fn entries(&self) -> &[DecodedDictionaryEntry] {
        &self.0
    }

    /// Returns the final decoded occurrence of one key.
    pub fn get(&self, key: &[u8]) -> Option<&DecodedLocatedObject> {
        self.0
            .iter()
            .rev()
            .find(|entry| entry.key.bytes() == key)
            .map(DecodedDictionaryEntry::value)
    }
}

/// Strict direct PDF object whose locations are decoded object-stream coordinates.
#[derive(Debug, Eq, PartialEq)]
pub enum DecodedObject {
    /// The null object.
    Null,
    /// A boolean object.
    Boolean(bool),
    /// A signed integer object.
    Integer(i64),
    /// A real number retaining its lexeme.
    Real(PdfReal),
    /// A decoded name.
    Name(PdfName),
    /// A decoded string.
    String(PdfString),
    /// An ordered array.
    Array(DecodedArray),
    /// An ordered dictionary.
    Dictionary(DecodedDictionary),
    /// An indirect reference.
    Reference(ObjectRef),
}

impl DecodedObject {
    /// Returns the integer value when applicable.
    pub const fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the dictionary when applicable.
    pub const fn as_dictionary(&self) -> Option<&DecodedDictionary> {
        match self {
            Self::Dictionary(value) => Some(value),
            _ => None,
        }
    }

    /// Returns the indirect reference when applicable.
    pub const fn as_reference(&self) -> Option<ObjectRef> {
        match self {
            Self::Reference(value) => Some(*value),
            _ => None,
        }
    }
}

/// One validated embedded object in decoded stream order.
#[derive(Debug, Eq, PartialEq)]
pub struct ObjectStreamEntry {
    index: u32,
    object_number: u32,
    value: DecodedLocatedObject,
}

impl ObjectStreamEntry {
    /// Returns the zero-based stream-header index.
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Returns the embedded generation-zero object number.
    pub const fn object_number(&self) -> u32 {
        self.object_number
    }

    /// Borrows the decoded-coordinate direct value.
    pub const fn value(&self) -> &DecodedLocatedObject {
        &self.value
    }
}

/// Deterministic work and retained-capacity evidence for one object stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectStreamStats {
    decoded_bytes: u64,
    header_bytes: u64,
    objects: u64,
    syntax_input_bytes: u64,
    retained_entry_bytes: u64,
    retained_value_bytes: u64,
}

impl ObjectStreamStats {
    /// Returns complete decoded payload bytes validated.
    pub const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }

    /// Returns decoded header bytes validated.
    pub const fn header_bytes(self) -> u64 {
        self.header_bytes
    }

    /// Returns embedded objects parsed.
    pub const fn objects(self) -> u64 {
        self.objects
    }

    /// Returns cumulative entry-window bytes submitted to syntax parsing.
    pub const fn syntax_input_bytes(self) -> u64 {
        self.syntax_input_bytes
    }

    /// Returns allocator-reported retained entry-vector capacity bytes.
    pub const fn retained_entry_bytes(self) -> u64 {
        self.retained_entry_bytes
    }

    /// Returns retained decoded scalar and container capacity bytes.
    pub const fn retained_value_bytes(self) -> u64 {
        self.retained_value_bytes
    }
}

/// One validated object stream with separate physical and decoded coordinates.
#[derive(Eq, PartialEq)]
pub struct ObjectStream {
    snapshot: SourceSnapshot,
    container: ObjectRef,
    revision_startxref: u64,
    container_offset: u64,
    container_upper_bound: u64,
    encoded_payload_span: ByteSpan,
    first_object_offset: u64,
    header_extension_span: DecodedObjectSpan,
    extends: Option<ObjectRef>,
    entries: Vec<ObjectStreamEntry>,
    stats: ObjectStreamStats,
}

impl ObjectStream {
    /// Returns the immutable source identity.
    pub const fn source(&self) -> SourceIdentity {
        self.snapshot.identity()
    }

    /// Returns the complete immutable source snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the framed container object identity.
    pub const fn container(&self) -> ObjectRef {
        self.container
    }

    /// Returns the container revision anchor.
    pub const fn revision_startxref(&self) -> u64 {
        self.revision_startxref
    }

    /// Returns the container's validated physical xref offset.
    pub const fn container_offset(&self) -> u64 {
        self.container_offset
    }

    /// Returns the container's validated exclusive physical bound.
    pub const fn container_upper_bound(&self) -> u64 {
        self.container_upper_bound
    }

    /// Returns the physical source span of the unfiltered payload.
    pub const fn encoded_payload_span(&self) -> ByteSpan {
        self.encoded_payload_span
    }

    /// Returns `/First`, the first embedded object's decoded offset.
    pub const fn first_object_offset(&self) -> u64 {
        self.first_object_offset
    }

    /// Returns the uninterpreted decoded header tail after the `/N` standard pairs.
    ///
    /// ISO permits future extensions before `/First`; this span is retained as decoded-only
    /// provenance and is not interpreted as extra object-number/offset pairs.
    pub const fn header_extension_span(&self) -> DecodedObjectSpan {
        self.header_extension_span
    }

    /// Returns the optional object-stream collection predecessor declared by `/Extends`.
    ///
    /// This hint is retained for provenance only and never changes xref lookup semantics.
    pub const fn extends(&self) -> Option<ObjectRef> {
        self.extends
    }

    /// Returns entries in stream-header order.
    pub fn entries(&self) -> &[ObjectStreamEntry] {
        &self.entries
    }

    /// Returns one zero-based stream-header entry.
    pub fn entry(&self, index: u32) -> Option<&ObjectStreamEntry> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.entries.get(index))
    }

    /// Returns deterministic work and retained-capacity evidence.
    pub const fn stats(&self) -> ObjectStreamStats {
        self.stats
    }
}

impl fmt::Debug for ObjectStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObjectStream")
            .field("snapshot", &self.snapshot)
            .field("container", &self.container)
            .field("revision_startxref", &self.revision_startxref)
            .field("container_offset", &self.container_offset)
            .field("container_upper_bound", &self.container_upper_bound)
            .field("encoded_payload_span", &self.encoded_payload_span)
            .field("first_object_offset", &self.first_object_offset)
            .field("header_extension_span", &self.header_extension_span)
            .field("extends", &self.extends)
            .field("entry_count", &self.entries.len())
            .field("stats", &self.stats)
            .finish()
    }
}

#[derive(Clone, Copy)]
struct HeaderEntry {
    object_number: u32,
    relative_offset: u64,
}

struct ParsedHeader {
    entries: Vec<HeaderEntry>,
    extension_span: DecodedObjectSpan,
}

struct SyntaxCancellationAdapter<'a>(&'a dyn ObjectCancellation);

impl SyntaxCancellation for SyntaxCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ObjectStreamPayloadCoordinates {
    Physical(u64),
    Decoded,
}

impl ObjectStreamPayloadCoordinates {
    const fn error_offsets(self) -> (Option<u64>, Option<u64>) {
        match self {
            Self::Physical(offset) => (Some(offset), None),
            Self::Decoded => (None, Some(0)),
        }
    }
}

/// Parses one complete unfiltered object stream from a framed container and exact source slice.
///
/// The payload slice must match the framed stream's immutable identity and physical data span.
/// Streams declaring `/Filter` or `/DecodeParms` are rejected; a future filtered entry point must
/// accept proof-bearing decoded bytes rather than treating decoded coordinates as source spans.
pub fn parse_unfiltered_object_stream(
    container: &IndirectObject,
    payload: &ByteSlice,
    limits: ObjectStreamLimits,
    cancellation: &(dyn ObjectCancellation + '_),
) -> Result<ObjectStream, ObjectStreamError> {
    check_cancelled(cancellation)?;
    let stream = require_generation_zero_stream(container)?;
    validate_source_geometry(container, stream.data_span(), payload)?;
    if has_unique_filter(stream.dictionary().value(), cancellation)?
        || unique_value(stream.dictionary().value(), b"DecodeParms", cancellation)?.is_some()
    {
        return Err(ObjectStreamError::at_source(
            ObjectStreamErrorCode::UnsupportedFilter,
            Some(stream.data_span().start()),
        ));
    }
    parse_decoded_object_stream(
        container,
        stream,
        payload.bytes(),
        ObjectStreamPayloadCoordinates::Physical(stream.data_span().start()),
        limits,
        cancellation,
    )
}

pub(crate) fn parse_decoded_object_stream(
    container: &IndirectObject,
    stream: &FramedStream,
    payload: &[u8],
    payload_coordinates: ObjectStreamPayloadCoordinates,
    limits: ObjectStreamLimits,
    cancellation: &(dyn ObjectCancellation + '_),
) -> Result<ObjectStream, ObjectStreamError> {
    let dictionary = stream.dictionary();
    require_name(dictionary.value(), b"Type", b"ObjStm", cancellation)?;
    let (object_count, object_count_offset) =
        require_nonnegative_u64(dictionary.value(), b"N", cancellation)?;
    if object_count > limits.max_objects {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::Objects,
            limits.max_objects,
            0,
            object_count,
            Some(object_count_offset),
            None,
        ));
    }
    let (first, first_offset) =
        require_nonnegative_u64(dictionary.value(), b"First", cancellation)?;
    let extends_entry = optional_reference(dictionary.value(), b"Extends", cancellation)?;
    let extends = extends_entry.map(|(reference, _)| reference);
    if extends
        .is_some_and(|reference| reference.generation() != 0 || reference == container.reference())
    {
        return Err(ObjectStreamError::at_source(
            ObjectStreamErrorCode::InvalidDictionary,
            extends_entry.map(|(_, offset)| offset),
        ));
    }
    if first > limits.max_header_bytes {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::HeaderBytes,
            limits.max_header_bytes,
            0,
            first,
            Some(first_offset),
            None,
        ));
    }
    let (payload_source_offset, payload_decoded_offset) = payload_coordinates.error_offsets();
    let payload_len = u64::try_from(payload.len()).map_err(|_| {
        ObjectStreamError::resource(
            ObjectStreamLimitKind::DecodedBytes,
            limits.max_decoded_bytes,
            0,
            u64::MAX,
            payload_source_offset,
            payload_decoded_offset,
        )
    })?;
    if payload_len > limits.max_decoded_bytes {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::DecodedBytes,
            limits.max_decoded_bytes,
            0,
            payload_len,
            payload_source_offset,
            payload_decoded_offset,
        ));
    }
    if first > payload_len {
        return Err(ObjectStreamError::at_decoded(
            ObjectStreamErrorCode::InvalidHeader,
            first,
        ));
    }
    let header_end = usize::try_from(first)
        .map_err(|_| ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, first))?;
    let ParsedHeader {
        entries: headers,
        extension_span,
    } = parse_header(&payload[..header_end], object_count, limits, cancellation)?;
    validate_unique_numbers(&headers, limits, cancellation)?;
    let entry_capacity = usize::try_from(object_count).map_err(|_| {
        ObjectStreamError::resource(
            ObjectStreamLimitKind::RetainedEntries,
            limits.max_retained_entry_bytes,
            0,
            u64::MAX,
            Some(stream.data_span().start()),
            None,
        )
    })?;
    let requested_entry_bytes = object_count
        .checked_mul(mem::size_of::<ObjectStreamEntry>() as u64)
        .ok_or_else(|| {
            ObjectStreamError::resource(
                ObjectStreamLimitKind::RetainedEntries,
                limits.max_retained_entry_bytes,
                0,
                u64::MAX,
                Some(stream.data_span().start()),
                None,
            )
        })?;
    if requested_entry_bytes > limits.max_retained_entry_bytes {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::RetainedEntries,
            limits.max_retained_entry_bytes,
            0,
            requested_entry_bytes,
            Some(stream.data_span().start()),
            None,
        ));
    }
    let mut entries = Vec::new();
    entries.try_reserve_exact(entry_capacity).map_err(|_| {
        ObjectStreamError::resource(
            ObjectStreamLimitKind::RetainedEntries,
            limits.max_retained_entry_bytes,
            0,
            requested_entry_bytes,
            Some(stream.data_span().start()),
            None,
        )
    })?;
    let retained_entry_bytes =
        capacity_bytes::<ObjectStreamEntry>(entries.capacity()).ok_or_else(|| {
            ObjectStreamError::resource(
                ObjectStreamLimitKind::RetainedEntries,
                limits.max_retained_entry_bytes,
                0,
                u64::MAX,
                Some(stream.data_span().start()),
                None,
            )
        })?;
    if retained_entry_bytes > limits.max_retained_entry_bytes {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::RetainedEntries,
            limits.max_retained_entry_bytes,
            0,
            retained_entry_bytes,
            Some(stream.data_span().start()),
            None,
        ));
    }

    let mut total_syntax_bytes = 0_u64;
    let mut retained_value_bytes = 0_u64;
    let mut conversion_work = 0_usize;
    for (index, header) in headers.iter().copied().enumerate() {
        if index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        let entry_start = first.checked_add(header.relative_offset).ok_or_else(|| {
            ObjectStreamError::at_decoded(ObjectStreamErrorCode::InvalidEntryBoundary, first)
        })?;
        let next_relative = headers
            .get(index + 1)
            .map_or(payload_len.checked_sub(first), |next| {
                Some(next.relative_offset)
            })
            .ok_or_else(|| {
                ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, first)
            })?;
        let entry_end = first.checked_add(next_relative).ok_or_else(|| {
            ObjectStreamError::at_decoded(ObjectStreamErrorCode::InvalidEntryBoundary, entry_start)
        })?;
        if entry_start >= entry_end || entry_end > payload_len {
            return Err(ObjectStreamError::at_decoded(
                ObjectStreamErrorCode::InvalidEntryBoundary,
                entry_start,
            ));
        }
        let entry_len = entry_end.checked_sub(entry_start).ok_or_else(|| {
            ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, entry_start)
        })?;
        total_syntax_bytes = charge(
            total_syntax_bytes,
            entry_len,
            limits.max_total_syntax_bytes,
            ObjectStreamLimitKind::TotalSyntaxBytes,
            Some(entry_start),
        )?;
        let start = usize::try_from(entry_start).map_err(|_| {
            ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, entry_start)
        })?;
        let end = usize::try_from(entry_end).map_err(|_| {
            ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, entry_end)
        })?;
        let input = SyntaxInput::new(
            container.snapshot().identity(),
            entry_start,
            &payload[start..end],
            InputExtent::KnownSourceEnd,
        )
        .map_err(|error| ObjectStreamError::from_syntax(error, entry_start))?;
        let adapter = SyntaxCancellationAdapter(cancellation);
        let child_syntax = syntax_limits_for_remaining_value_budget(
            limits.syntax,
            entry_len,
            limits.max_retained_value_bytes,
            retained_value_bytes,
            entry_start,
        )?;
        let mut parser = SyntaxParser::new_with_cancellation(input, child_syntax, &adapter)
            .map_err(|error| ObjectStreamError::from_syntax(error, entry_start))?;
        let located = match parser.parse_object() {
            SyntaxPoll::Ready(value) => value,
            SyntaxPoll::Failed(error) => {
                return Err(ObjectStreamError::from_syntax(error, entry_start));
            }
            SyntaxPoll::NeedMore { minimum_end } => {
                return Err(ObjectStreamError::at_decoded(
                    ObjectStreamErrorCode::InvalidEntryBoundary,
                    minimum_end,
                ));
            }
            SyntaxPoll::EndOfInput => {
                return Err(ObjectStreamError::at_decoded(
                    ObjectStreamErrorCode::InvalidEntryBoundary,
                    entry_start,
                ));
            }
        };
        if located.source() != container.snapshot().identity()
            || located.span().start() != entry_start
        {
            return Err(ObjectStreamError::at_decoded(
                ObjectStreamErrorCode::InvalidEntryBoundary,
                entry_start,
            ));
        }
        let parsed_end = located.span().end_exclusive();
        if parsed_end > entry_end || !only_pdf_trivia(payload, parsed_end, entry_end, cancellation)?
        {
            return Err(ObjectStreamError::at_decoded(
                ObjectStreamErrorCode::InvalidEntryBoundary,
                parsed_end.min(entry_end),
            ));
        }
        if matches!(located.value(), SyntaxObject::Reference(_)) {
            return Err(ObjectStreamError::at_decoded(
                ObjectStreamErrorCode::SyntaxFailure,
                entry_start,
            ));
        }
        let syntax_stats = parser.stats();
        let transient_container_bytes = syntax_stats.container_bytes();
        retained_value_bytes = charge_retained_value(
            retained_value_bytes,
            syntax_stats.owned_bytes(),
            limits.max_retained_value_bytes,
            transient_container_bytes,
            Some(entry_start),
        )?;
        let value = convert_located(
            located,
            &mut retained_value_bytes,
            limits.max_retained_value_bytes,
            transient_container_bytes,
            cancellation,
            &mut conversion_work,
        )?;
        entries.push(ObjectStreamEntry {
            index: u32::try_from(index).map_err(|_| {
                ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, entry_start)
            })?,
            object_number: header.object_number,
            value,
        });
    }
    check_cancelled(cancellation)?;

    Ok(ObjectStream {
        snapshot: container.snapshot(),
        container: container.reference(),
        revision_startxref: container.revision_startxref(),
        container_offset: container.xref_offset(),
        container_upper_bound: container.object_upper_bound(),
        encoded_payload_span: stream.data_span(),
        first_object_offset: first,
        header_extension_span: extension_span,
        extends,
        entries,
        stats: ObjectStreamStats {
            decoded_bytes: payload_len,
            header_bytes: first,
            objects: object_count,
            syntax_input_bytes: total_syntax_bytes,
            retained_entry_bytes,
            retained_value_bytes,
        },
    })
}

fn validate_source_geometry(
    container: &IndirectObject,
    data_span: ByteSpan,
    payload: &ByteSlice,
) -> Result<(), ObjectStreamError> {
    if payload.identity() != container.snapshot().identity()
        || payload.range().start() != data_span.start()
        || payload.range().len() != data_span.len()
        || container
            .snapshot()
            .len()
            .is_some_and(|len| data_span.end_exclusive() > len)
        || data_span.start() < container.header_span().start()
        || data_span.end_exclusive() > container.object_span().end_exclusive()
    {
        return Err(ObjectStreamError::at_source(
            ObjectStreamErrorCode::SourceMismatch,
            Some(data_span.start()),
        ));
    }
    Ok(())
}

pub(crate) fn require_generation_zero_stream(
    container: &IndirectObject,
) -> Result<&FramedStream, ObjectStreamError> {
    let IndirectObjectValue::Stream(stream) = container.value() else {
        return Err(ObjectStreamError::at_source(
            ObjectStreamErrorCode::InvalidDictionary,
            Some(container.xref_offset()),
        ));
    };
    if container.reference().generation() != 0 {
        return Err(ObjectStreamError::at_source(
            ObjectStreamErrorCode::InvalidDictionary,
            Some(container.xref_offset()),
        ));
    }
    Ok(stream)
}

pub(crate) fn has_unique_filter(
    dictionary: &PdfDictionary,
    cancellation: &dyn ObjectCancellation,
) -> Result<bool, ObjectStreamError> {
    Ok(unique_value(dictionary, b"Filter", cancellation)?.is_some())
}

fn parse_header(
    header: &[u8],
    object_count: u64,
    limits: ObjectStreamLimits,
    cancellation: &dyn ObjectCancellation,
) -> Result<ParsedHeader, ObjectStreamError> {
    let requested = object_count
        .checked_mul(mem::size_of::<HeaderEntry>() as u64)
        .ok_or_else(|| {
            ObjectStreamError::resource(
                ObjectStreamLimitKind::WorkingBytes,
                limits.max_working_bytes,
                0,
                u64::MAX,
                None,
                Some(0),
            )
        })?;
    if requested > limits.max_working_bytes {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::WorkingBytes,
            limits.max_working_bytes,
            0,
            requested,
            None,
            Some(0),
        ));
    }
    let capacity = usize::try_from(object_count).map_err(|_| {
        ObjectStreamError::resource(
            ObjectStreamLimitKind::WorkingBytes,
            limits.max_working_bytes,
            0,
            requested,
            None,
            Some(0),
        )
    })?;
    let mut result = Vec::new();
    result.try_reserve_exact(capacity).map_err(|_| {
        ObjectStreamError::resource(
            ObjectStreamLimitKind::WorkingBytes,
            limits.max_working_bytes,
            0,
            requested,
            None,
            Some(0),
        )
    })?;
    let retained = capacity_bytes::<HeaderEntry>(result.capacity()).ok_or_else(|| {
        ObjectStreamError::resource(
            ObjectStreamLimitKind::WorkingBytes,
            limits.max_working_bytes,
            0,
            u64::MAX,
            None,
            Some(0),
        )
    })?;
    if retained > limits.max_working_bytes {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::WorkingBytes,
            limits.max_working_bytes,
            0,
            retained,
            None,
            Some(0),
        ));
    }
    let mut cursor = 0_usize;
    let mut previous_offset = None;
    let mut numeric_work = 0_usize;
    for index in 0..capacity {
        if index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        skip_pdf_trivia(header, &mut cursor, cancellation)?;
        let object_offset = cursor;
        let object_number = parse_unsigned(header, &mut cursor, cancellation, &mut numeric_work)?;
        let object_number = u32::try_from(object_number)
            .ok()
            .filter(|number| *number != 0)
            .ok_or_else(|| {
                ObjectStreamError::at_decoded(
                    ObjectStreamErrorCode::InvalidHeader,
                    object_offset as u64,
                )
            })?;
        require_header_separator(header, cursor)?;
        skip_pdf_trivia(header, &mut cursor, cancellation)?;
        let offset_offset = cursor;
        let relative_offset = parse_unsigned(header, &mut cursor, cancellation, &mut numeric_work)?;
        require_header_separator(header, cursor)?;
        if (index == 0 && relative_offset != 0)
            || previous_offset.is_some_and(|previous| relative_offset <= previous)
        {
            return Err(ObjectStreamError::at_decoded(
                ObjectStreamErrorCode::InvalidHeader,
                offset_offset as u64,
            ));
        }
        previous_offset = Some(relative_offset);
        result.push(HeaderEntry {
            object_number,
            relative_offset,
        });
    }
    let extension_start = u64::try_from(cursor)
        .map_err(|_| ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, 0))?;
    let extension_len = u64::try_from(header.len().saturating_sub(cursor)).map_err(|_| {
        ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, extension_start)
    })?;
    Ok(ParsedHeader {
        entries: result,
        extension_span: DecodedObjectSpan::new(extension_start, extension_len)?,
    })
}

fn validate_unique_numbers(
    headers: &Vec<HeaderEntry>,
    limits: ObjectStreamLimits,
    cancellation: &dyn ObjectCancellation,
) -> Result<(), ObjectStreamError> {
    let requested = u64::try_from(headers.len())
        .ok()
        .and_then(|len| len.checked_mul(mem::size_of::<u32>() as u64))
        .ok_or_else(|| {
            ObjectStreamError::resource(
                ObjectStreamLimitKind::WorkingBytes,
                limits.max_working_bytes,
                0,
                u64::MAX,
                None,
                Some(0),
            )
        })?;
    let header_bytes = capacity_bytes::<HeaderEntry>(headers.capacity())
        .ok_or_else(|| ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, 0))?;
    let aggregate = header_bytes.checked_add(requested).ok_or_else(|| {
        ObjectStreamError::resource(
            ObjectStreamLimitKind::WorkingBytes,
            limits.max_working_bytes,
            header_bytes,
            u64::MAX,
            None,
            Some(0),
        )
    })?;
    if aggregate > limits.max_working_bytes {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::WorkingBytes,
            limits.max_working_bytes,
            header_bytes,
            requested,
            None,
            Some(0),
        ));
    }
    let mut numbers = Vec::new();
    numbers.try_reserve_exact(headers.len()).map_err(|_| {
        ObjectStreamError::resource(
            ObjectStreamLimitKind::WorkingBytes,
            limits.max_working_bytes,
            header_bytes,
            requested,
            None,
            Some(0),
        )
    })?;
    let number_bytes = capacity_bytes::<u32>(numbers.capacity()).ok_or_else(|| {
        ObjectStreamError::resource(
            ObjectStreamLimitKind::WorkingBytes,
            limits.max_working_bytes,
            header_bytes,
            u64::MAX,
            None,
            Some(0),
        )
    })?;
    if header_bytes
        .checked_add(number_bytes)
        .is_none_or(|actual| actual > limits.max_working_bytes)
    {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::WorkingBytes,
            limits.max_working_bytes,
            header_bytes,
            number_bytes,
            None,
            Some(0),
        ));
    }
    for (index, header) in headers.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        numbers.push(header.object_number);
    }
    cancellable_heapsort(&mut numbers, cancellation)?;
    for (index, pair) in numbers.windows(2).enumerate() {
        if index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        if pair[0] == pair[1] {
            return Err(ObjectStreamError::at_decoded(
                ObjectStreamErrorCode::DuplicateObjectNumber,
                0,
            ));
        }
    }
    Ok(())
}

fn cancellable_heapsort(
    values: &mut [u32],
    cancellation: &dyn ObjectCancellation,
) -> Result<(), ObjectStreamError> {
    let mut work = 0_usize;
    for root in (0..values.len() / 2).rev() {
        sift_down(values, root, values.len(), cancellation, &mut work)?;
    }
    for end in (1..values.len()).rev() {
        probe_work(cancellation, &mut work)?;
        values.swap(0, end);
        sift_down(values, 0, end, cancellation, &mut work)?;
    }
    check_cancelled(cancellation)
}

fn sift_down(
    values: &mut [u32],
    mut root: usize,
    end: usize,
    cancellation: &dyn ObjectCancellation,
    work: &mut usize,
) -> Result<(), ObjectStreamError> {
    loop {
        let Some(left) = root.checked_mul(2).and_then(|value| value.checked_add(1)) else {
            return Err(ObjectStreamError::at_decoded(
                ObjectStreamErrorCode::InternalState,
                0,
            ));
        };
        if left >= end {
            return Ok(());
        }
        probe_work(cancellation, work)?;
        let right = left + 1;
        let child = if right < end && values[right] > values[left] {
            right
        } else {
            left
        };
        if values[root] >= values[child] {
            return Ok(());
        }
        values.swap(root, child);
        root = child;
    }
}

fn probe_work(
    cancellation: &dyn ObjectCancellation,
    work: &mut usize,
) -> Result<(), ObjectStreamError> {
    if work.is_multiple_of(CANCELLATION_INTERVAL) {
        check_cancelled(cancellation)?;
    }
    *work = work
        .checked_add(1)
        .ok_or_else(|| ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, 0))?;
    Ok(())
}

fn parse_unsigned(
    bytes: &[u8],
    cursor: &mut usize,
    cancellation: &dyn ObjectCancellation,
    work: &mut usize,
) -> Result<u64, ObjectStreamError> {
    let start = *cursor;
    let mut value = 0_u64;
    while let Some(byte) = bytes.get(*cursor).copied() {
        if !byte.is_ascii_digit() {
            break;
        }
        probe_work(cancellation, work)?;
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or_else(|| {
                ObjectStreamError::at_decoded(ObjectStreamErrorCode::InvalidHeader, start as u64)
            })?;
        *cursor += 1;
    }
    if *cursor == start {
        return Err(ObjectStreamError::at_decoded(
            ObjectStreamErrorCode::InvalidHeader,
            start as u64,
        ));
    }
    Ok(value)
}

fn require_header_separator(bytes: &[u8], cursor: usize) -> Result<(), ObjectStreamError> {
    if bytes
        .get(cursor)
        .is_some_and(|byte| is_pdf_whitespace(*byte) || *byte == b'%')
    {
        Ok(())
    } else {
        Err(ObjectStreamError::at_decoded(
            ObjectStreamErrorCode::InvalidHeader,
            cursor as u64,
        ))
    }
}

fn only_pdf_trivia(
    bytes: &[u8],
    start: u64,
    end: u64,
    cancellation: &dyn ObjectCancellation,
) -> Result<bool, ObjectStreamError> {
    let start = usize::try_from(start)
        .map_err(|_| ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, start))?;
    let end = usize::try_from(end)
        .map_err(|_| ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, end))?;
    let Some(slice) = bytes.get(start..end) else {
        return Err(ObjectStreamError::at_decoded(
            ObjectStreamErrorCode::InternalState,
            start as u64,
        ));
    };
    let mut cursor = 0_usize;
    skip_pdf_trivia(slice, &mut cursor, cancellation)?;
    Ok(cursor == slice.len())
}

fn skip_pdf_trivia(
    bytes: &[u8],
    cursor: &mut usize,
    cancellation: &dyn ObjectCancellation,
) -> Result<(), ObjectStreamError> {
    let mut work = 0_usize;
    loop {
        while bytes
            .get(*cursor)
            .is_some_and(|byte| is_pdf_whitespace(*byte))
        {
            if work.is_multiple_of(CANCELLATION_INTERVAL) {
                check_cancelled(cancellation)?;
            }
            *cursor += 1;
            work += 1;
        }
        if bytes.get(*cursor) != Some(&b'%') {
            return Ok(());
        }
        while let Some(byte) = bytes.get(*cursor) {
            if work.is_multiple_of(CANCELLATION_INTERVAL) {
                check_cancelled(cancellation)?;
            }
            *cursor += 1;
            work += 1;
            if matches!(*byte, b'\r' | b'\n') {
                break;
            }
        }
    }
}

const fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}

fn convert_located(
    located: Located<SyntaxObject>,
    retained_value_bytes: &mut u64,
    limit: u64,
    transient_container_bytes: u64,
    cancellation: &dyn ObjectCancellation,
    conversion_work: &mut usize,
) -> Result<DecodedLocatedObject, ObjectStreamError> {
    probe_work(cancellation, conversion_work)?;
    let span = decoded_span(located.span())?;
    let value = convert_object(
        located.into_value(),
        retained_value_bytes,
        limit,
        transient_container_bytes,
        cancellation,
        conversion_work,
    )?;
    Ok(DecodedLocatedObject { span, value })
}

fn convert_object(
    object: SyntaxObject,
    retained_value_bytes: &mut u64,
    limit: u64,
    transient_container_bytes: u64,
    cancellation: &dyn ObjectCancellation,
    conversion_work: &mut usize,
) -> Result<DecodedObject, ObjectStreamError> {
    match object {
        SyntaxObject::Null => Ok(DecodedObject::Null),
        SyntaxObject::Boolean(value) => Ok(DecodedObject::Boolean(value)),
        SyntaxObject::Integer(value) => Ok(DecodedObject::Integer(value)),
        SyntaxObject::Real(value) => Ok(DecodedObject::Real(value)),
        SyntaxObject::Name(value) => Ok(DecodedObject::Name(value)),
        SyntaxObject::String(value) => Ok(DecodedObject::String(value)),
        SyntaxObject::Reference(value) => Ok(DecodedObject::Reference(value)),
        SyntaxObject::Array(array) => {
            let values = array.into_values();
            let requested = u64::try_from(values.len())
                .ok()
                .and_then(|len| len.checked_mul(mem::size_of::<DecodedLocatedObject>() as u64))
                .ok_or_else(|| {
                    ObjectStreamError::resource(
                        ObjectStreamLimitKind::RetainedValues,
                        limit,
                        *retained_value_bytes,
                        u64::MAX,
                        None,
                        None,
                    )
                })?;
            ensure_retained_value_available(
                *retained_value_bytes,
                requested,
                limit,
                transient_container_bytes,
                None,
            )?;
            let mut converted = Vec::new();
            converted.try_reserve_exact(values.len()).map_err(|_| {
                ObjectStreamError::resource(
                    ObjectStreamLimitKind::RetainedValues,
                    limit,
                    *retained_value_bytes,
                    requested,
                    None,
                    None,
                )
            })?;
            let converted_bytes = capacity_bytes::<DecodedLocatedObject>(converted.capacity())
                .ok_or_else(|| {
                    ObjectStreamError::resource(
                        ObjectStreamLimitKind::RetainedValues,
                        limit,
                        *retained_value_bytes,
                        u64::MAX,
                        None,
                        None,
                    )
                })?;
            *retained_value_bytes = charge_retained_value(
                *retained_value_bytes,
                converted_bytes,
                limit,
                transient_container_bytes,
                None,
            )?;
            for value in values {
                converted.push(convert_located(
                    value,
                    retained_value_bytes,
                    limit,
                    transient_container_bytes,
                    cancellation,
                    conversion_work,
                )?);
            }
            Ok(DecodedObject::Array(DecodedArray(converted)))
        }
        SyntaxObject::Dictionary(dictionary) => {
            let entries = dictionary.into_entries();
            let requested = u64::try_from(entries.len())
                .ok()
                .and_then(|len| len.checked_mul(mem::size_of::<DecodedDictionaryEntry>() as u64))
                .ok_or_else(|| {
                    ObjectStreamError::resource(
                        ObjectStreamLimitKind::RetainedValues,
                        limit,
                        *retained_value_bytes,
                        u64::MAX,
                        None,
                        None,
                    )
                })?;
            ensure_retained_value_available(
                *retained_value_bytes,
                requested,
                limit,
                transient_container_bytes,
                None,
            )?;
            let mut converted = Vec::new();
            converted.try_reserve_exact(entries.len()).map_err(|_| {
                ObjectStreamError::resource(
                    ObjectStreamLimitKind::RetainedValues,
                    limit,
                    *retained_value_bytes,
                    requested,
                    None,
                    None,
                )
            })?;
            let converted_bytes = capacity_bytes::<DecodedDictionaryEntry>(converted.capacity())
                .ok_or_else(|| {
                    ObjectStreamError::resource(
                        ObjectStreamLimitKind::RetainedValues,
                        limit,
                        *retained_value_bytes,
                        u64::MAX,
                        None,
                        None,
                    )
                })?;
            *retained_value_bytes = charge_retained_value(
                *retained_value_bytes,
                converted_bytes,
                limit,
                transient_container_bytes,
                None,
            )?;
            for entry in entries {
                probe_work(cancellation, conversion_work)?;
                let (key, value) = entry.into_parts();
                let key_span = decoded_span(key.span())?;
                let key = key.into_value();
                let value = convert_located(
                    value,
                    retained_value_bytes,
                    limit,
                    transient_container_bytes,
                    cancellation,
                    conversion_work,
                )?;
                converted.push(DecodedDictionaryEntry {
                    key_span,
                    key,
                    value,
                });
            }
            Ok(DecodedObject::Dictionary(DecodedDictionary(converted)))
        }
    }
}

fn decoded_span(span: ByteSpan) -> Result<DecodedObjectSpan, ObjectStreamError> {
    DecodedObjectSpan::new(span.start(), span.len())
}

fn syntax_limits_for_remaining_value_budget(
    configured: SyntaxLimits,
    entry_len: u64,
    retained_limit: u64,
    retained_value_bytes: u64,
    decoded_offset: u64,
) -> Result<SyntaxLimits, ObjectStreamError> {
    let remaining = retained_limit
        .checked_sub(retained_value_bytes)
        .ok_or_else(|| {
            ObjectStreamError::resource(
                ObjectStreamLimitKind::RetainedValues,
                retained_limit,
                retained_value_bytes,
                1,
                None,
                Some(decoded_offset),
            )
        })?;
    if remaining <= 1 || entry_len == 0 {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::RetainedValues,
            retained_limit,
            retained_value_bytes,
            2,
            None,
            Some(decoded_offset),
        ));
    }
    let max_owned_bytes = configured
        .max_owned_bytes()
        .min(entry_len)
        .min(remaining - 1);
    let max_container_bytes =
        configured
            .max_container_bytes()
            .min(remaining.checked_sub(max_owned_bytes).ok_or_else(|| {
                ObjectStreamError::at_decoded(ObjectStreamErrorCode::InternalState, decoded_offset)
            })?);
    if max_owned_bytes == 0 || max_container_bytes == 0 {
        return Err(ObjectStreamError::resource(
            ObjectStreamLimitKind::RetainedValues,
            retained_limit,
            retained_value_bytes,
            2,
            None,
            Some(decoded_offset),
        ));
    }
    let max_input_bytes = configured.max_input_bytes().min(entry_len);
    let max_token_bytes = configured.max_token_bytes().min(max_input_bytes);
    let max_name_bytes = configured
        .max_name_bytes()
        .min(max_token_bytes)
        .min(max_owned_bytes);
    let max_string_source_bytes = configured.max_string_source_bytes().min(max_input_bytes);
    let max_string_decoded_bytes = configured.max_string_decoded_bytes().min(max_owned_bytes);
    SyntaxLimits::validate(SyntaxLimitConfig {
        max_input_bytes,
        max_token_bytes,
        max_comment_bytes: configured.max_comment_bytes().min(max_token_bytes),
        max_name_bytes,
        max_string_source_bytes,
        max_string_decoded_bytes,
        max_owned_bytes,
        max_total_tokens: configured.max_total_tokens(),
        max_container_entries: configured.max_container_entries(),
        max_container_bytes,
        max_container_depth: configured.max_container_depth(),
    })
    .map_err(|error| ObjectStreamError::from_syntax(error, decoded_offset))
}

fn capacity_bytes<T>(capacity: usize) -> Option<u64> {
    u64::try_from(capacity)
        .ok()
        .and_then(|capacity| capacity.checked_mul(mem::size_of::<T>() as u64))
}

fn charge(
    consumed: u64,
    attempted: u64,
    limit: u64,
    kind: ObjectStreamLimitKind,
    decoded_offset: Option<u64>,
) -> Result<u64, ObjectStreamError> {
    let total = consumed.checked_add(attempted).ok_or_else(|| {
        ObjectStreamError::resource(kind, limit, consumed, u64::MAX, None, decoded_offset)
    })?;
    if total > limit {
        return Err(ObjectStreamError::resource(
            kind,
            limit,
            consumed,
            attempted,
            None,
            decoded_offset,
        ));
    }
    Ok(total)
}

fn ensure_retained_value_available(
    consumed: u64,
    attempted: u64,
    limit: u64,
    transient_reserved: u64,
    decoded_offset: Option<u64>,
) -> Result<(), ObjectStreamError> {
    consumed
        .checked_add(attempted)
        .and_then(|total| total.checked_add(transient_reserved))
        .filter(|peak| *peak <= limit)
        .map(|_| ())
        .ok_or_else(|| {
            ObjectStreamError::resource(
                ObjectStreamLimitKind::RetainedValues,
                limit,
                consumed.saturating_add(transient_reserved),
                attempted,
                None,
                decoded_offset,
            )
        })
}

fn charge_retained_value(
    consumed: u64,
    attempted: u64,
    limit: u64,
    transient_reserved: u64,
    decoded_offset: Option<u64>,
) -> Result<u64, ObjectStreamError> {
    ensure_retained_value_available(
        consumed,
        attempted,
        limit,
        transient_reserved,
        decoded_offset,
    )?;
    consumed.checked_add(attempted).ok_or_else(|| {
        ObjectStreamError::resource(
            ObjectStreamLimitKind::RetainedValues,
            limit,
            consumed.saturating_add(transient_reserved),
            u64::MAX,
            None,
            decoded_offset,
        )
    })
}

fn unique_value<'a>(
    dictionary: &'a pdf_rs_syntax::PdfDictionary,
    key: &[u8],
    cancellation: &dyn ObjectCancellation,
) -> Result<Option<&'a Located<SyntaxObject>>, ObjectStreamError> {
    let mut found = None;
    for (index, entry) in dictionary.entries().iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        if entry.key().value().bytes() == key {
            if found.is_some() {
                return Err(ObjectStreamError::at_source(
                    ObjectStreamErrorCode::InvalidDictionary,
                    Some(entry.key().span().start()),
                ));
            }
            found = Some(entry.value());
        }
    }
    Ok(found)
}

fn require_name(
    dictionary: &pdf_rs_syntax::PdfDictionary,
    key: &[u8],
    expected: &[u8],
    cancellation: &dyn ObjectCancellation,
) -> Result<(), ObjectStreamError> {
    match unique_value(dictionary, key, cancellation)? {
        Some(value) => match value.value() {
            SyntaxObject::Name(name) if name.bytes() == expected => Ok(()),
            _ => Err(ObjectStreamError::at_source(
                ObjectStreamErrorCode::InvalidDictionary,
                Some(value.span().start()),
            )),
        },
        None => Err(ObjectStreamError::at_source(
            ObjectStreamErrorCode::InvalidDictionary,
            None,
        )),
    }
}

fn require_nonnegative_u64(
    dictionary: &pdf_rs_syntax::PdfDictionary,
    key: &[u8],
    cancellation: &dyn ObjectCancellation,
) -> Result<(u64, u64), ObjectStreamError> {
    match unique_value(dictionary, key, cancellation)? {
        Some(located) => match located.value() {
            SyntaxObject::Integer(value) => u64::try_from(*value)
                .map(|value| (value, located.span().start()))
                .map_err(|_| {
                    ObjectStreamError::at_source(
                        ObjectStreamErrorCode::InvalidDictionary,
                        Some(located.span().start()),
                    )
                }),
            _ => Err(ObjectStreamError::at_source(
                ObjectStreamErrorCode::InvalidDictionary,
                Some(located.span().start()),
            )),
        },
        None => Err(ObjectStreamError::at_source(
            ObjectStreamErrorCode::InvalidDictionary,
            None,
        )),
    }
}

fn optional_reference(
    dictionary: &pdf_rs_syntax::PdfDictionary,
    key: &[u8],
    cancellation: &dyn ObjectCancellation,
) -> Result<Option<(ObjectRef, u64)>, ObjectStreamError> {
    match unique_value(dictionary, key, cancellation)? {
        None => Ok(None),
        Some(located) => match located.value() {
            SyntaxObject::Reference(reference) => Ok(Some((*reference, located.span().start()))),
            _ => Err(ObjectStreamError::at_source(
                ObjectStreamErrorCode::InvalidDictionary,
                Some(located.span().start()),
            )),
        },
    }
}

pub(crate) fn check_cancelled(
    cancellation: &dyn ObjectCancellation,
) -> Result<(), ObjectStreamError> {
    if cancellation.is_cancelled() {
        Err(ObjectStreamError::at_source(
            ObjectStreamErrorCode::Cancelled,
            None,
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pdf_rs_bytes::{SourceRevision, SourceStableId};

    use super::*;

    struct CancelOnSecondProbe(AtomicUsize);

    impl ObjectCancellation for CancelOnSecondProbe {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::AcqRel) >= 1
        }
    }

    #[test]
    fn recursive_conversion_observes_fixed_interval_cancellation() {
        let mut bytes = Vec::from(b"[".as_slice());
        for _ in 0..(CANCELLATION_INTERVAL + 8) {
            bytes.extend_from_slice(b"null ");
        }
        bytes.push(b']');
        let source = SourceIdentity::new(SourceStableId::new([0x91; 32]), SourceRevision::new(1));
        let input = SyntaxInput::new(source, 0, &bytes, InputExtent::KnownSourceEnd).unwrap();
        let mut parser = SyntaxParser::new(input, SyntaxLimits::default()).unwrap();
        let located = match parser.parse_object() {
            SyntaxPoll::Ready(value) => value,
            other => panic!("conversion fixture must parse: {other:?}"),
        };
        let syntax_stats = parser.stats();
        let mut retained_value_bytes = syntax_stats.owned_bytes();
        let mut conversion_work = 0_usize;
        let cancellation = CancelOnSecondProbe(AtomicUsize::new(0));

        let error = convert_located(
            located,
            &mut retained_value_bytes,
            u64::MAX,
            syntax_stats.container_bytes(),
            &cancellation,
            &mut conversion_work,
        )
        .unwrap_err();
        assert_eq!(error.code(), ObjectStreamErrorCode::Cancelled);
        assert!(conversion_work >= CANCELLATION_INTERVAL);
        assert!(cancellation.0.load(Ordering::Acquire) >= 2);
    }

    #[test]
    fn long_unsigned_header_scan_observes_fixed_interval_cancellation() {
        let mut bytes = vec![b'0'; CANCELLATION_INTERVAL + 8];
        bytes.extend_from_slice(b"10 ");
        let mut cursor = 0_usize;
        let mut work = 0_usize;
        let cancellation = CancelOnSecondProbe(AtomicUsize::new(0));

        let error = parse_unsigned(&bytes, &mut cursor, &cancellation, &mut work).unwrap_err();
        assert_eq!(error.code(), ObjectStreamErrorCode::Cancelled);
        assert!(cursor >= CANCELLATION_INTERVAL);
        assert!(cancellation.0.load(Ordering::Acquire) >= 2);
    }
}
