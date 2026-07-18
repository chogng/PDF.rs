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
    /// Changes text state, text positioning, or text showing inside a text object.
    TextObject,
    /// Opens or closes a compatibility section.
    CompatibilityBoundary,
    /// Operates on the marked-content stack or emits a marked-content point.
    MarkedContent,
    /// Constructs or closes the current path outside a text object.
    PathConstruction,
    /// Paints or discards the current path outside a text object.
    PathPainting,
    /// Selects a pending clipping rule for the current path outside a text object.
    ClippingPath,
    /// Changes one registered line parameter in the current graphics state.
    LineState,
    /// Changes a registered device-color value in the current graphics state.
    DeviceColor,
    /// Paints one named external object outside a text object.
    XObject,
}

/// Exact top-level operand shape required before an initial known operator can execute.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorOperandShape {
    /// No operands.
    None,
    /// One PDF number.
    OneNumber,
    /// Two PDF numbers in source order.
    TwoNumbers,
    /// Three PDF numbers in source order.
    ThreeNumbers,
    /// Four PDF numbers in source order.
    FourNumbers,
    /// Six PDF numbers in source order.
    SixNumbers,
    /// One PDF integer.
    OneInteger,
    /// One array of PDF numbers followed by one PDF number.
    NumberArrayAndNumber,
    /// One PDF name.
    Name,
    /// One PDF name followed by one PDF number.
    NameAndNumber,
    /// One decoded PDF string.
    String,
    /// One direct PDF array whose element shape is validated by the operator.
    Array,
    /// Two PDF numbers followed by one decoded PDF string.
    TwoNumbersAndString,
    /// One PDF name followed by either a name or a direct dictionary.
    NameAndNameOrDictionary,
}

impl OperatorOperandShape {
    const fn operand_count(self) -> u8 {
        match self {
            Self::None => 0,
            Self::OneNumber | Self::OneInteger => 1,
            Self::TwoNumbers | Self::NumberArrayAndNumber | Self::NameAndNumber => 2,
            Self::ThreeNumbers | Self::TwoNumbersAndString => 3,
            Self::FourNumbers => 4,
            Self::SixNumbers => 6,
            Self::Name | Self::String | Self::Array => 1,
            Self::NameAndNameOrDictionary => 2,
        }
    }
}

/// Declarative initial VM outcome after an operator's operand shape has been validated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorFailurePolicy {
    /// Execute the operator under its structural and resource rules.
    Execute,
    /// Publish a structured unsupported outcome only after operand validation succeeds.
    ValidateThenUnsupported,
}

/// Stable known operator identity used by the initial Content VM.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OperatorKind {
    /// Save graphics state (`q`).
    SaveGraphicsState,
    /// Restore graphics state (`Q`).
    RestoreGraphicsState,
    /// Apply a named external graphics-state dictionary (`gs`).
    SetGraphicsState,
    /// Concatenate current transformation matrix (`cm`).
    ConcatMatrix,
    /// Begin text object (`BT`).
    BeginText,
    /// End text object (`ET`).
    EndText,
    /// Set character spacing (`Tc`).
    SetCharacterSpacing,
    /// Set word spacing (`Tw`).
    SetWordSpacing,
    /// Set horizontal scaling percentage (`Tz`).
    SetHorizontalScaling,
    /// Set text leading (`TL`).
    SetTextLeading,
    /// Select the text font and size (`Tf`).
    SetTextFont,
    /// Set text rendering mode (`Tr`).
    SetTextRenderMode,
    /// Set text rise (`Ts`).
    SetTextRise,
    /// Move the text line matrix (`Td`).
    MoveTextPosition,
    /// Move the text line matrix and set leading (`TD`).
    MoveTextPositionSetLeading,
    /// Set the text and text-line matrices (`Tm`).
    SetTextMatrix,
    /// Move to the start of the next text line (`T*`).
    MoveToNextTextLine,
    /// Show one decoded text string (`Tj`).
    ShowText,
    /// Show strings with numeric position adjustments (`TJ`).
    ShowTextAdjusted,
    /// Move to the next line and show text (`'`).
    MoveNextLineShowText,
    /// Set spacing, move to the next line, and show text (`"`).
    SetSpacingMoveNextLineShowText,
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
    /// Begin a new subpath (`m`).
    MoveTo,
    /// Append a straight-line segment (`l`).
    LineTo,
    /// Append a cubic Bézier segment (`c`).
    CubicCurveTo,
    /// Append a cubic Bézier segment replicating the initial control point (`v`).
    CubicCurveToReplicateInitial,
    /// Append a cubic Bézier segment replicating the final control point (`y`).
    CubicCurveToReplicateFinal,
    /// Close the current subpath (`h`).
    ClosePath,
    /// Append a closed rectangular subpath (`re`).
    Rectangle,
    /// Stroke the current path (`S`).
    StrokePath,
    /// Close and stroke the current path (`s`).
    CloseAndStrokePath,
    /// Fill the current path using the nonzero winding rule (`f`).
    FillNonzero,
    /// Fill using the legacy nonzero winding alias (`F`).
    FillNonzeroLegacy,
    /// Fill the current path using the even-odd rule (`f*`).
    FillEvenOdd,
    /// Fill nonzero and then stroke the current path (`B`).
    FillStrokeNonzero,
    /// Fill even-odd and then stroke the current path (`B*`).
    FillStrokeEvenOdd,
    /// Close, fill nonzero, and stroke the current path (`b`).
    CloseFillStrokeNonzero,
    /// Close, fill even-odd, and stroke the current path (`b*`).
    CloseFillStrokeEvenOdd,
    /// End the current path without painting (`n`).
    EndPath,
    /// Select nonzero clipping for the current path (`W`).
    ClipNonzero,
    /// Select even-odd clipping for the current path (`W*`).
    ClipEvenOdd,
    /// Set line width (`w`).
    SetLineWidth,
    /// Set line-cap style (`J`).
    SetLineCap,
    /// Set line-join style (`j`).
    SetLineJoin,
    /// Set miter limit (`M`).
    SetMiterLimit,
    /// Set line-dash pattern and phase (`d`).
    SetLineDash,
    /// Set stroking DeviceGray (`G`).
    SetStrokingGray,
    /// Set nonstroking DeviceGray (`g`).
    SetNonstrokingGray,
    /// Set stroking DeviceRGB (`RG`).
    SetStrokingRgb,
    /// Set nonstroking DeviceRGB (`rg`).
    SetNonstrokingRgb,
    /// Set stroking DeviceCMYK (`K`).
    SetStrokingCmyk,
    /// Set nonstroking DeviceCMYK (`k`).
    SetNonstrokingCmyk,
    /// Paint one named external object (`Do`).
    PaintXObject,
}

/// Declarative scanner/VM metadata for one known operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperatorSpec {
    token: &'static [u8],
    min_operands: u8,
    max_operands: u8,
    operand_shape: OperatorOperandShape,
    context: OperatorContext,
    failure_policy: OperatorFailurePolicy,
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

    /// Returns the exact operand type shape validated by the VM.
    pub const fn operand_shape(self) -> OperatorOperandShape {
        self.operand_shape
    }

    /// Returns the structural context class.
    pub const fn context(self) -> OperatorContext {
        self.context
    }

    /// Returns the declared outcome after successful operand validation.
    pub const fn failure_policy(self) -> OperatorFailurePolicy {
        self.failure_policy
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
            b"gs" => Some(Self::SetGraphicsState),
            b"cm" => Some(Self::ConcatMatrix),
            b"BT" => Some(Self::BeginText),
            b"ET" => Some(Self::EndText),
            b"Tc" => Some(Self::SetCharacterSpacing),
            b"Tw" => Some(Self::SetWordSpacing),
            b"Tz" => Some(Self::SetHorizontalScaling),
            b"TL" => Some(Self::SetTextLeading),
            b"Tf" => Some(Self::SetTextFont),
            b"Tr" => Some(Self::SetTextRenderMode),
            b"Ts" => Some(Self::SetTextRise),
            b"Td" => Some(Self::MoveTextPosition),
            b"TD" => Some(Self::MoveTextPositionSetLeading),
            b"Tm" => Some(Self::SetTextMatrix),
            b"T*" => Some(Self::MoveToNextTextLine),
            b"Tj" => Some(Self::ShowText),
            b"TJ" => Some(Self::ShowTextAdjusted),
            b"'" => Some(Self::MoveNextLineShowText),
            b"\"" => Some(Self::SetSpacingMoveNextLineShowText),
            b"BX" => Some(Self::BeginCompatibility),
            b"EX" => Some(Self::EndCompatibility),
            b"MP" => Some(Self::MarkedContentPoint),
            b"DP" => Some(Self::MarkedContentPointProperties),
            b"BMC" => Some(Self::BeginMarkedContent),
            b"BDC" => Some(Self::BeginMarkedContentProperties),
            b"EMC" => Some(Self::EndMarkedContent),
            b"m" => Some(Self::MoveTo),
            b"l" => Some(Self::LineTo),
            b"c" => Some(Self::CubicCurveTo),
            b"v" => Some(Self::CubicCurveToReplicateInitial),
            b"y" => Some(Self::CubicCurveToReplicateFinal),
            b"h" => Some(Self::ClosePath),
            b"re" => Some(Self::Rectangle),
            b"S" => Some(Self::StrokePath),
            b"s" => Some(Self::CloseAndStrokePath),
            b"f" => Some(Self::FillNonzero),
            b"F" => Some(Self::FillNonzeroLegacy),
            b"f*" => Some(Self::FillEvenOdd),
            b"B" => Some(Self::FillStrokeNonzero),
            b"B*" => Some(Self::FillStrokeEvenOdd),
            b"b" => Some(Self::CloseFillStrokeNonzero),
            b"b*" => Some(Self::CloseFillStrokeEvenOdd),
            b"n" => Some(Self::EndPath),
            b"W" => Some(Self::ClipNonzero),
            b"W*" => Some(Self::ClipEvenOdd),
            b"w" => Some(Self::SetLineWidth),
            b"J" => Some(Self::SetLineCap),
            b"j" => Some(Self::SetLineJoin),
            b"M" => Some(Self::SetMiterLimit),
            b"d" => Some(Self::SetLineDash),
            b"G" => Some(Self::SetStrokingGray),
            b"g" => Some(Self::SetNonstrokingGray),
            b"RG" => Some(Self::SetStrokingRgb),
            b"rg" => Some(Self::SetNonstrokingRgb),
            b"K" => Some(Self::SetStrokingCmyk),
            b"k" => Some(Self::SetNonstrokingCmyk),
            b"Do" => Some(Self::PaintXObject),
            _ => None,
        }
    }

    /// Returns the declarative specification for this known operator.
    pub const fn spec(self) -> OperatorSpec {
        match self {
            Self::SaveGraphicsState => spec(
                b"q",
                OperatorOperandShape::None,
                OperatorContext::Any,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::RestoreGraphicsState => spec(
                b"Q",
                OperatorOperandShape::None,
                OperatorContext::Any,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::SetGraphicsState => spec(
                b"gs",
                OperatorOperandShape::Name,
                OperatorContext::Any,
                OperatorFailurePolicy::Execute,
                2,
            ),
            Self::ConcatMatrix => spec(
                b"cm",
                OperatorOperandShape::SixNumbers,
                OperatorContext::Any,
                OperatorFailurePolicy::Execute,
                4,
            ),
            Self::BeginText => spec(
                b"BT",
                OperatorOperandShape::None,
                OperatorContext::TextObjectBoundary,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::EndText => spec(
                b"ET",
                OperatorOperandShape::None,
                OperatorContext::TextObjectBoundary,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::SetCharacterSpacing => text_state_spec(b"Tc", OperatorOperandShape::OneNumber, 2),
            Self::SetWordSpacing => text_state_spec(b"Tw", OperatorOperandShape::OneNumber, 2),
            Self::SetHorizontalScaling => {
                text_state_spec(b"Tz", OperatorOperandShape::OneNumber, 2)
            }
            Self::SetTextLeading => text_state_spec(b"TL", OperatorOperandShape::OneNumber, 2),
            Self::SetTextFont => text_state_spec(b"Tf", OperatorOperandShape::NameAndNumber, 3),
            Self::SetTextRenderMode => text_state_spec(b"Tr", OperatorOperandShape::OneInteger, 2),
            Self::SetTextRise => text_state_spec(b"Ts", OperatorOperandShape::OneNumber, 2),
            Self::MoveTextPosition => text_spec(b"Td", OperatorOperandShape::TwoNumbers, 3),
            Self::MoveTextPositionSetLeading => {
                text_spec(b"TD", OperatorOperandShape::TwoNumbers, 4)
            }
            Self::SetTextMatrix => text_spec(b"Tm", OperatorOperandShape::SixNumbers, 7),
            Self::MoveToNextTextLine => text_spec(b"T*", OperatorOperandShape::None, 2),
            Self::ShowText => text_spec(b"Tj", OperatorOperandShape::String, 2),
            Self::ShowTextAdjusted => text_spec(b"TJ", OperatorOperandShape::Array, 3),
            Self::MoveNextLineShowText => text_spec(b"'", OperatorOperandShape::String, 3),
            Self::SetSpacingMoveNextLineShowText => {
                text_spec(b"\"", OperatorOperandShape::TwoNumbersAndString, 5)
            }
            Self::BeginCompatibility => spec(
                b"BX",
                OperatorOperandShape::None,
                OperatorContext::CompatibilityBoundary,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::EndCompatibility => spec(
                b"EX",
                OperatorOperandShape::None,
                OperatorContext::CompatibilityBoundary,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::MarkedContentPoint => spec(
                b"MP",
                OperatorOperandShape::Name,
                OperatorContext::MarkedContent,
                OperatorFailurePolicy::ValidateThenUnsupported,
                2,
            ),
            Self::MarkedContentPointProperties => spec(
                b"DP",
                OperatorOperandShape::NameAndNameOrDictionary,
                OperatorContext::MarkedContent,
                OperatorFailurePolicy::ValidateThenUnsupported,
                3,
            ),
            Self::BeginMarkedContent => spec(
                b"BMC",
                OperatorOperandShape::Name,
                OperatorContext::MarkedContent,
                OperatorFailurePolicy::Execute,
                2,
            ),
            Self::BeginMarkedContentProperties => spec(
                b"BDC",
                OperatorOperandShape::NameAndNameOrDictionary,
                OperatorContext::MarkedContent,
                OperatorFailurePolicy::Execute,
                3,
            ),
            Self::EndMarkedContent => spec(
                b"EMC",
                OperatorOperandShape::None,
                OperatorContext::MarkedContent,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::MoveTo => spec(
                b"m",
                OperatorOperandShape::TwoNumbers,
                OperatorContext::PathConstruction,
                OperatorFailurePolicy::Execute,
                3,
            ),
            Self::LineTo => spec(
                b"l",
                OperatorOperandShape::TwoNumbers,
                OperatorContext::PathConstruction,
                OperatorFailurePolicy::Execute,
                3,
            ),
            Self::CubicCurveTo => spec(
                b"c",
                OperatorOperandShape::SixNumbers,
                OperatorContext::PathConstruction,
                OperatorFailurePolicy::Execute,
                7,
            ),
            Self::CubicCurveToReplicateInitial => spec(
                b"v",
                OperatorOperandShape::FourNumbers,
                OperatorContext::PathConstruction,
                OperatorFailurePolicy::Execute,
                5,
            ),
            Self::CubicCurveToReplicateFinal => spec(
                b"y",
                OperatorOperandShape::FourNumbers,
                OperatorContext::PathConstruction,
                OperatorFailurePolicy::Execute,
                5,
            ),
            Self::ClosePath => spec(
                b"h",
                OperatorOperandShape::None,
                OperatorContext::PathConstruction,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::Rectangle => spec(
                b"re",
                OperatorOperandShape::FourNumbers,
                OperatorContext::PathConstruction,
                OperatorFailurePolicy::Execute,
                5,
            ),
            Self::StrokePath => spec(
                b"S",
                OperatorOperandShape::None,
                OperatorContext::PathPainting,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::CloseAndStrokePath => spec(
                b"s",
                OperatorOperandShape::None,
                OperatorContext::PathPainting,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::FillNonzero => spec(
                b"f",
                OperatorOperandShape::None,
                OperatorContext::PathPainting,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::FillNonzeroLegacy => spec(
                b"F",
                OperatorOperandShape::None,
                OperatorContext::PathPainting,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::FillEvenOdd => spec(
                b"f*",
                OperatorOperandShape::None,
                OperatorContext::PathPainting,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::FillStrokeNonzero => spec(
                b"B",
                OperatorOperandShape::None,
                OperatorContext::PathPainting,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::FillStrokeEvenOdd => spec(
                b"B*",
                OperatorOperandShape::None,
                OperatorContext::PathPainting,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::CloseFillStrokeNonzero => spec(
                b"b",
                OperatorOperandShape::None,
                OperatorContext::PathPainting,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::CloseFillStrokeEvenOdd => spec(
                b"b*",
                OperatorOperandShape::None,
                OperatorContext::PathPainting,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::EndPath => spec(
                b"n",
                OperatorOperandShape::None,
                OperatorContext::PathPainting,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::ClipNonzero => spec(
                b"W",
                OperatorOperandShape::None,
                OperatorContext::ClippingPath,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::ClipEvenOdd => spec(
                b"W*",
                OperatorOperandShape::None,
                OperatorContext::ClippingPath,
                OperatorFailurePolicy::Execute,
                1,
            ),
            Self::SetLineWidth => spec(
                b"w",
                OperatorOperandShape::OneNumber,
                OperatorContext::LineState,
                OperatorFailurePolicy::Execute,
                2,
            ),
            Self::SetLineCap => spec(
                b"J",
                OperatorOperandShape::OneInteger,
                OperatorContext::LineState,
                OperatorFailurePolicy::Execute,
                2,
            ),
            Self::SetLineJoin => spec(
                b"j",
                OperatorOperandShape::OneInteger,
                OperatorContext::LineState,
                OperatorFailurePolicy::Execute,
                2,
            ),
            Self::SetMiterLimit => spec(
                b"M",
                OperatorOperandShape::OneNumber,
                OperatorContext::LineState,
                OperatorFailurePolicy::Execute,
                2,
            ),
            Self::SetLineDash => spec(
                b"d",
                OperatorOperandShape::NumberArrayAndNumber,
                OperatorContext::LineState,
                OperatorFailurePolicy::Execute,
                3,
            ),
            Self::SetStrokingGray => spec(
                b"G",
                OperatorOperandShape::OneNumber,
                OperatorContext::DeviceColor,
                OperatorFailurePolicy::Execute,
                2,
            ),
            Self::SetNonstrokingGray => spec(
                b"g",
                OperatorOperandShape::OneNumber,
                OperatorContext::DeviceColor,
                OperatorFailurePolicy::Execute,
                2,
            ),
            Self::SetStrokingRgb => spec(
                b"RG",
                OperatorOperandShape::ThreeNumbers,
                OperatorContext::DeviceColor,
                OperatorFailurePolicy::Execute,
                4,
            ),
            Self::SetNonstrokingRgb => spec(
                b"rg",
                OperatorOperandShape::ThreeNumbers,
                OperatorContext::DeviceColor,
                OperatorFailurePolicy::Execute,
                4,
            ),
            Self::SetStrokingCmyk => spec(
                b"K",
                OperatorOperandShape::FourNumbers,
                OperatorContext::DeviceColor,
                OperatorFailurePolicy::Execute,
                5,
            ),
            Self::SetNonstrokingCmyk => spec(
                b"k",
                OperatorOperandShape::FourNumbers,
                OperatorContext::DeviceColor,
                OperatorFailurePolicy::Execute,
                5,
            ),
            Self::PaintXObject => spec(
                b"Do",
                OperatorOperandShape::Name,
                OperatorContext::XObject,
                OperatorFailurePolicy::Execute,
                3,
            ),
        }
    }
}

const fn spec(
    token: &'static [u8],
    operand_shape: OperatorOperandShape,
    context: OperatorContext,
    failure_policy: OperatorFailurePolicy,
    base_fuel: u16,
) -> OperatorSpec {
    let operands = operand_shape.operand_count();
    OperatorSpec {
        token,
        min_operands: operands,
        max_operands: operands,
        operand_shape,
        context,
        failure_policy,
        base_fuel,
    }
}

const fn text_spec(
    token: &'static [u8],
    operand_shape: OperatorOperandShape,
    base_fuel: u16,
) -> OperatorSpec {
    spec(
        token,
        operand_shape,
        OperatorContext::TextObject,
        OperatorFailurePolicy::Execute,
        base_fuel,
    )
}

const fn text_state_spec(
    token: &'static [u8],
    operand_shape: OperatorOperandShape,
    base_fuel: u16,
) -> OperatorSpec {
    spec(
        token,
        operand_shape,
        OperatorContext::Any,
        OperatorFailurePolicy::Execute,
        base_fuel,
    )
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
