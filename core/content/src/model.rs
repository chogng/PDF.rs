use std::fmt;

use pdf_rs_syntax::ObjectRef;

use crate::ContentLimits;

/// One borrowed, already-decoded content stream in exact page order.
#[derive(Clone, Copy)]
pub struct DecodedContentStream<'a> {
    object: ObjectRef,
    ordinal: u32,
    decoded: &'a [u8],
}

impl<'a> DecodedContentStream<'a> {
    /// Creates a borrowed stream descriptor.
    pub const fn new(object: ObjectRef, ordinal: u32, decoded: &'a [u8]) -> Self {
        Self {
            object,
            ordinal,
            decoded,
        }
    }

    /// Returns the indirect stream object identity.
    pub const fn object(self) -> ObjectRef {
        self.object
    }

    /// Returns the caller-declared zero-based stream ordinal.
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }

    /// Borrows the complete decoded stream bytes.
    pub const fn decoded(self) -> &'a [u8] {
        self.decoded
    }
}

impl fmt::Debug for DecodedContentStream<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedContentStream")
            .field("object", &self.object)
            .field("ordinal", &self.ordinal)
            .field("decoded_len", &self.decoded.len())
            .field("decoded", &"[REDACTED]")
            .finish()
    }
}

/// Exact decoded byte position in one page content stream.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentPosition {
    object: ObjectRef,
    stream_ordinal: u32,
    decoded_offset: u64,
}

impl ContentPosition {
    pub(crate) const fn new(object: ObjectRef, stream_ordinal: u32, decoded_offset: u64) -> Self {
        Self {
            object,
            stream_ordinal,
            decoded_offset,
        }
    }

    /// Returns the stream object identity.
    pub const fn object(self) -> ObjectRef {
        self.object
    }

    /// Returns the zero-based stream ordinal.
    pub const fn stream_ordinal(self) -> u32 {
        self.stream_ordinal
    }

    /// Returns the zero-based decoded byte offset.
    pub const fn decoded_offset(self) -> u64 {
        self.decoded_offset
    }
}

/// Checked decoded span contained in exactly one stream.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DecodedSpan {
    object: ObjectRef,
    stream_ordinal: u32,
    decoded_start: u64,
    decoded_len: u64,
}

impl DecodedSpan {
    pub(crate) const fn new(
        object: ObjectRef,
        stream_ordinal: u32,
        decoded_start: u64,
        decoded_len: u64,
    ) -> Self {
        Self {
            object,
            stream_ordinal,
            decoded_start,
            decoded_len,
        }
    }

    /// Returns the stream object identity.
    pub const fn object(self) -> ObjectRef {
        self.object
    }

    /// Returns the zero-based stream ordinal.
    pub const fn stream_ordinal(self) -> u32 {
        self.stream_ordinal
    }

    /// Returns the zero-based decoded start offset.
    pub const fn decoded_start(self) -> u64 {
        self.decoded_start
    }

    /// Returns the decoded byte length.
    pub const fn decoded_len(self) -> u64 {
        self.decoded_len
    }

    /// Returns the checked exclusive decoded end.
    pub const fn decoded_end_exclusive(self) -> u64 {
        self.decoded_start + self.decoded_len
    }

    /// Returns the first decoded position.
    pub const fn start(self) -> ContentPosition {
        ContentPosition::new(self.object, self.stream_ordinal, self.decoded_start)
    }

    /// Returns the exclusive-end decoded position.
    pub const fn end_exclusive(self) -> ContentPosition {
        ContentPosition::new(
            self.object,
            self.stream_ordinal,
            self.decoded_start + self.decoded_len,
        )
    }
}

/// Start and exclusive-end positions for an operand that may span streams.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContentExtent {
    start: ContentPosition,
    end_exclusive: ContentPosition,
}

impl ContentExtent {
    pub(crate) const fn new(start: ContentPosition, end_exclusive: ContentPosition) -> Self {
        Self {
            start,
            end_exclusive,
        }
    }

    pub(crate) const fn from_span(span: DecodedSpan) -> Self {
        Self::new(span.start(), span.end_exclusive())
    }

    /// Returns the first decoded position.
    pub const fn start(self) -> ContentPosition {
        self.start
    }

    /// Returns the exclusive decoded end position.
    pub const fn end_exclusive(self) -> ContentPosition {
        self.end_exclusive
    }

    /// Returns a single-stream span when both boundaries belong to the same stream.
    pub fn single_stream_span(self) -> Option<DecodedSpan> {
        if self.start.object == self.end_exclusive.object
            && self.start.stream_ordinal == self.end_exclusive.stream_ordinal
            && self.end_exclusive.decoded_offset >= self.start.decoded_offset
        {
            Some(DecodedSpan::new(
                self.start.object,
                self.start.stream_ordinal,
                self.start.decoded_offset,
                self.end_exclusive.decoded_offset - self.start.decoded_offset,
            ))
        } else {
            None
        }
    }
}

/// Decoded PDF name bytes owned by a scanned program.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentName(Vec<u8>);

impl ContentName {
    pub(crate) const fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrows decoded name bytes without assuming UTF-8.
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ContentName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentName")
            .field("len", &self.0.len())
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// Retained lexical notation for a scanned PDF real.
#[derive(Clone, Eq, PartialEq)]
pub struct ContentReal(Vec<u8>);

impl ContentReal {
    pub(crate) const fn new(raw: Vec<u8>) -> Self {
        Self(raw)
    }

    /// Borrows the validated original real-number lexeme.
    pub fn raw(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ContentReal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentReal")
            .field("len", &self.0.len())
            .field("raw", &"[REDACTED]")
            .finish()
    }
}

/// Source notation of a decoded PDF string operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentStringKind {
    /// Parenthesized literal syntax.
    Literal,
    /// Angle-bracket hexadecimal syntax.
    Hexadecimal,
}

/// Decoded PDF string bytes owned by a scanned program.
#[derive(Clone, Eq, PartialEq)]
pub struct ContentString {
    bytes: Vec<u8>,
    kind: ContentStringKind,
}

impl ContentString {
    pub(crate) const fn new(bytes: Vec<u8>, kind: ContentStringKind) -> Self {
        Self { bytes, kind }
    }

    /// Borrows decoded bytes without assuming a text encoding.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the source string notation.
    pub const fn kind(&self) -> ContentStringKind {
        self.kind
    }
}

impl fmt::Debug for ContentString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentString")
            .field("kind", &self.kind)
            .field("len", &self.bytes.len())
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// One ordered PDF dictionary key/value pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentDictionaryEntry {
    key_span: DecodedSpan,
    key: ContentName,
    value: LocatedOperand,
}

impl ContentDictionaryEntry {
    pub(crate) const fn new(
        key_span: DecodedSpan,
        key: ContentName,
        value: LocatedOperand,
    ) -> Self {
        Self {
            key_span,
            key,
            value,
        }
    }

    /// Returns the exact encoded name token span.
    pub const fn key_span(&self) -> DecodedSpan {
        self.key_span
    }

    /// Returns the decoded key.
    pub const fn key(&self) -> &ContentName {
        &self.key
    }

    /// Returns the ordered value.
    pub const fn value(&self) -> &LocatedOperand {
        &self.value
    }
}

/// Owned direct PDF operand accepted by the content scanner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContentOperand {
    /// The `null` object.
    Null,
    /// A boolean object.
    Boolean(bool),
    /// A signed integer object.
    Integer(i64),
    /// A validated real retaining its exact lexical bytes.
    Real(ContentReal),
    /// A decoded PDF name.
    Name(ContentName),
    /// A decoded literal or hexadecimal string.
    String(ContentString),
    /// An ordered array preserving duplicate and nested values.
    Array(Vec<LocatedOperand>),
    /// An ordered dictionary preserving duplicate keys.
    Dictionary(Vec<ContentDictionaryEntry>),
}

/// One owned operand paired with decoded coordinate evidence.
#[derive(Clone, Eq, PartialEq)]
pub struct LocatedOperand {
    extent: ContentExtent,
    value: ContentOperand,
}

impl LocatedOperand {
    pub(crate) const fn new(extent: ContentExtent, value: ContentOperand) -> Self {
        Self { extent, value }
    }

    /// Returns the complete decoded extent.
    pub const fn extent(&self) -> ContentExtent {
        self.extent
    }

    /// Borrows the owned operand.
    pub const fn value(&self) -> &ContentOperand {
        &self.value
    }

    /// Consumes the location wrapper.
    pub fn into_parts(self) -> (ContentExtent, ContentOperand) {
        (self.extent, self.value)
    }
}

impl fmt::Debug for LocatedOperand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocatedOperand")
            .field("extent", &self.extent)
            .field("kind", &operand_kind_name(&self.value))
            .finish()
    }
}

fn operand_kind_name(value: &ContentOperand) -> &'static str {
    match value {
        ContentOperand::Null => "Null",
        ContentOperand::Boolean(_) => "Boolean",
        ContentOperand::Integer(_) => "Integer",
        ContentOperand::Real(_) => "Real",
        ContentOperand::Name(_) => "Name",
        ContentOperand::String(_) => "String",
        ContentOperand::Array(_) => "Array",
        ContentOperand::Dictionary(_) => "Dictionary",
    }
}

/// Structural context declared by a known operator specification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorContext {
    /// No additional structural context is required by the scanner table.
    Any,
    /// Opens or closes a text object.
    TextObjectBoundary,
    /// Opens or closes a compatibility section.
    CompatibilityBoundary,
    /// Operates on the marked-content stack or emits a marked-content point.
    MarkedContent,
}

/// Stable known operator identity used by the initial Content VM.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OperatorKind {
    /// Save graphics state (`q`).
    SaveGraphicsState,
    /// Restore graphics state (`Q`).
    RestoreGraphicsState,
    /// Concatenate current transformation matrix (`cm`).
    ConcatMatrix,
    /// Begin text object (`BT`).
    BeginText,
    /// End text object (`ET`).
    EndText,
    /// Begin compatibility section (`BX`).
    BeginCompatibility,
    /// End compatibility section (`EX`).
    EndCompatibility,
    /// Marked-content point (`MP`).
    MarkedContentPoint,
    /// Marked-content point with properties (`DP`).
    MarkedContentPointProperties,
    /// Begin marked-content sequence (`BMC`).
    BeginMarkedContent,
    /// Begin marked-content sequence with properties (`BDC`).
    BeginMarkedContentProperties,
    /// End marked-content sequence (`EMC`).
    EndMarkedContent,
}

/// Declarative scanner/VM metadata for one known operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperatorSpec {
    token: &'static [u8],
    min_operands: u8,
    max_operands: u8,
    context: OperatorContext,
    base_fuel: u16,
}

impl OperatorSpec {
    /// Returns the exact case-sensitive operator token.
    pub const fn token(self) -> &'static [u8] {
        self.token
    }

    /// Returns the minimum operand count expected by the VM.
    pub const fn min_operands(self) -> u8 {
        self.min_operands
    }

    /// Returns the maximum operand count expected by the VM.
    pub const fn max_operands(self) -> u8 {
        self.max_operands
    }

    /// Returns the structural context class.
    pub const fn context(self) -> OperatorContext {
        self.context
    }

    /// Returns the declared base VM fuel cost.
    pub const fn base_fuel(self) -> u16 {
        self.base_fuel
    }
}

impl OperatorKind {
    pub(crate) fn from_token(token: &[u8]) -> Option<Self> {
        match token {
            b"q" => Some(Self::SaveGraphicsState),
            b"Q" => Some(Self::RestoreGraphicsState),
            b"cm" => Some(Self::ConcatMatrix),
            b"BT" => Some(Self::BeginText),
            b"ET" => Some(Self::EndText),
            b"BX" => Some(Self::BeginCompatibility),
            b"EX" => Some(Self::EndCompatibility),
            b"MP" => Some(Self::MarkedContentPoint),
            b"DP" => Some(Self::MarkedContentPointProperties),
            b"BMC" => Some(Self::BeginMarkedContent),
            b"BDC" => Some(Self::BeginMarkedContentProperties),
            b"EMC" => Some(Self::EndMarkedContent),
            _ => None,
        }
    }

    /// Returns the declarative specification for this known operator.
    pub const fn spec(self) -> OperatorSpec {
        match self {
            Self::SaveGraphicsState => spec(b"q", 0, OperatorContext::Any, 1),
            Self::RestoreGraphicsState => spec(b"Q", 0, OperatorContext::Any, 1),
            Self::ConcatMatrix => spec(b"cm", 6, OperatorContext::Any, 4),
            Self::BeginText => spec(b"BT", 0, OperatorContext::TextObjectBoundary, 1),
            Self::EndText => spec(b"ET", 0, OperatorContext::TextObjectBoundary, 1),
            Self::BeginCompatibility => spec(b"BX", 0, OperatorContext::CompatibilityBoundary, 1),
            Self::EndCompatibility => spec(b"EX", 0, OperatorContext::CompatibilityBoundary, 1),
            Self::MarkedContentPoint => spec(b"MP", 1, OperatorContext::MarkedContent, 2),
            Self::MarkedContentPointProperties => spec(b"DP", 2, OperatorContext::MarkedContent, 3),
            Self::BeginMarkedContent => spec(b"BMC", 1, OperatorContext::MarkedContent, 2),
            Self::BeginMarkedContentProperties => {
                spec(b"BDC", 2, OperatorContext::MarkedContent, 3)
            }
            Self::EndMarkedContent => spec(b"EMC", 0, OperatorContext::MarkedContent, 1),
        }
    }
}

const fn spec(
    token: &'static [u8],
    operands: u8,
    context: OperatorContext,
    base_fuel: u16,
) -> OperatorSpec {
    OperatorSpec {
        token,
        min_operands: operands,
        max_operands: operands,
        context,
        base_fuel,
    }
}

/// Known or lexically valid unknown content operator.
#[derive(Clone, Eq, PartialEq)]
pub enum ContentOperator {
    /// An operator represented in the stable initial table.
    Known(OperatorKind),
    /// A syntactically valid regular token not present in the stable table.
    Unknown(Vec<u8>),
}

impl ContentOperator {
    /// Returns the known identity when this is a table operator.
    pub const fn known(&self) -> Option<OperatorKind> {
        match self {
            Self::Known(kind) => Some(*kind),
            Self::Unknown(_) => None,
        }
    }

    /// Returns the exact operator token bytes.
    pub fn token(&self) -> &[u8] {
        match self {
            Self::Known(kind) => kind.spec().token(),
            Self::Unknown(token) => token,
        }
    }

    /// Reports whether the scanner classified this as unknown rather than malformed.
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

impl fmt::Debug for ContentOperator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Known(kind) => formatter.debug_tuple("Known").field(kind).finish(),
            Self::Unknown(bytes) => formatter
                .debug_struct("Unknown")
                .field("len", &bytes.len())
                .field("token", &"[REDACTED]")
                .finish(),
        }
    }
}

/// Exact decoded source evidence for one operator token.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContentOperatorSource {
    span: DecodedSpan,
    page_operator_ordinal: u64,
}

impl ContentOperatorSource {
    pub(crate) const fn new(span: DecodedSpan, page_operator_ordinal: u64) -> Self {
        Self {
            span,
            page_operator_ordinal,
        }
    }

    /// Returns the exact operator-token decoded span.
    pub const fn span(self) -> DecodedSpan {
        self.span
    }

    /// Returns the zero-based page-global operator ordinal.
    pub const fn page_operator_ordinal(self) -> u64 {
        self.page_operator_ordinal
    }
}

/// One scanned operator and all immediately preceding top-level operands.
#[derive(Clone, Eq, PartialEq)]
pub struct ScannedOperator {
    operator: ContentOperator,
    operands: Vec<LocatedOperand>,
    source: ContentOperatorSource,
}

impl ScannedOperator {
    pub(crate) const fn new(
        operator: ContentOperator,
        operands: Vec<LocatedOperand>,
        source: ContentOperatorSource,
    ) -> Self {
        Self {
            operator,
            operands,
            source,
        }
    }

    /// Returns the known-or-unknown operator identity.
    pub const fn operator(&self) -> &ContentOperator {
        &self.operator
    }

    /// Returns operands in source order.
    pub fn operands(&self) -> &[LocatedOperand] {
        &self.operands
    }

    /// Returns exact operator-token provenance.
    pub const fn source(&self) -> ContentOperatorSource {
        self.source
    }
}

impl fmt::Debug for ScannedOperator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScannedOperator")
            .field("operator", &self.operator)
            .field("operand_count", &self.operands.len())
            .field("source", &self.source)
            .finish()
    }
}

/// Deterministic complete-scan work and ownership statistics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContentScanStats {
    pub(crate) streams: u32,
    pub(crate) total_decoded_bytes: u64,
    pub(crate) tokens: u64,
    pub(crate) max_token_bytes: u64,
    pub(crate) operands: u64,
    pub(crate) max_operands_per_operator: u32,
    pub(crate) max_nesting_depth: u16,
    pub(crate) operators: u64,
    pub(crate) unknown_operators: u64,
    pub(crate) fuel: u64,
    pub(crate) retained_bytes: u64,
}

impl ContentScanStats {
    /// Returns the admitted stream count.
    pub const fn streams(self) -> u32 {
        self.streams
    }

    /// Returns the aggregate decoded byte count.
    pub const fn total_decoded_bytes(self) -> u64 {
        self.total_decoded_bytes
    }

    /// Returns the lexical token count.
    pub const fn tokens(self) -> u64 {
        self.tokens
    }

    /// Returns the largest raw token length.
    pub const fn max_token_bytes(self) -> u64 {
        self.max_token_bytes
    }

    /// Returns the complete direct and nested operand count.
    pub const fn operands(self) -> u64 {
        self.operands
    }

    /// Returns the largest top-level operand group.
    pub const fn max_operands_per_operator(self) -> u32 {
        self.max_operands_per_operator
    }

    /// Returns the deepest array/dictionary nesting reached.
    pub const fn max_nesting_depth(self) -> u16 {
        self.max_nesting_depth
    }

    /// Returns the published operator count.
    pub const fn operators(self) -> u64 {
        self.operators
    }

    /// Returns the lexically valid unknown operator count.
    pub const fn unknown_operators(self) -> u64 {
        self.unknown_operators
    }

    /// Returns deterministic scanner work units.
    pub const fn fuel(self) -> u64 {
        self.fuel
    }

    /// Returns allocator-reported capacity retained by the program.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }
}

/// Immutable owned output of one complete ordered content scan.
#[derive(Eq, PartialEq)]
pub struct ContentProgram {
    operators: Vec<ScannedOperator>,
    limits: ContentLimits,
    stats: ContentScanStats,
}

impl ContentProgram {
    pub(crate) const fn new(
        operators: Vec<ScannedOperator>,
        limits: ContentLimits,
        stats: ContentScanStats,
    ) -> Self {
        Self {
            operators,
            limits,
            stats,
        }
    }

    /// Returns operators in exact page execution order.
    pub fn operators(&self) -> &[ScannedOperator] {
        &self.operators
    }

    /// Returns the validated limit profile used for construction.
    pub const fn limits(&self) -> ContentLimits {
        self.limits
    }

    /// Returns complete deterministic scan statistics.
    pub const fn stats(&self) -> ContentScanStats {
        self.stats
    }
}

impl fmt::Debug for ContentProgram {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentProgram")
            .field("operator_count", &self.operators.len())
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("operators", &"[REDACTED]")
            .finish()
    }
}
