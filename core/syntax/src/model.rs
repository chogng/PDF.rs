use std::fmt;

use pdf_rs_bytes::SourceIdentity;

use crate::{SyntaxError, SyntaxErrorCode};

/// Checked absolute source byte span, including a valid empty boundary span.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteSpan {
    start: u64,
    len: u64,
}

impl ByteSpan {
    /// Creates a span and rejects exclusive-end overflow.
    pub fn new(start: u64, len: u64) -> Result<Self, SyntaxError> {
        if start.checked_add(len).is_none() {
            return Err(SyntaxError::for_code(
                SyntaxErrorCode::InternalState,
                Some(start),
            ));
        }
        Ok(Self { start, len })
    }

    pub(crate) fn from_bounds(start: u64, end_exclusive: u64) -> Result<Self, SyntaxError> {
        let len = end_exclusive
            .checked_sub(start)
            .ok_or_else(|| SyntaxError::for_code(SyntaxErrorCode::InternalState, Some(start)))?;
        Self::new(start, len)
    }

    /// Returns the first byte offset.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Returns the number of bytes.
    pub const fn len(self) -> u64 {
        self.len
    }

    /// Reports whether the span is empty.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Returns the exclusive end proven by construction.
    pub const fn end_exclusive(self) -> u64 {
        self.start + self.len
    }
}

/// One source-bound value with its exact raw byte span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Located<T> {
    source: SourceIdentity,
    span: ByteSpan,
    value: T,
}

impl<T> Located<T> {
    pub(crate) const fn new(source: SourceIdentity, span: ByteSpan, value: T) -> Self {
        Self {
            source,
            span,
            value,
        }
    }

    /// Returns the immutable source identity.
    pub const fn source(&self) -> SourceIdentity {
        self.source
    }

    /// Returns the exact raw source span.
    pub const fn span(&self) -> ByteSpan {
        self.span
    }

    /// Borrows the parsed value.
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Consumes the location wrapper and returns the parsed value.
    pub fn into_value(self) -> T {
        self.value
    }

    /// Transforms the value while preserving its immutable source and span.
    pub fn try_map<U, E>(self, transform: impl FnOnce(T) -> Result<U, E>) -> Result<Located<U>, E> {
        let value = transform(self.value)?;
        Ok(Located::new(self.source, self.span, value))
    }
}

/// Recognized PDF header version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdfHeader {
    major: u8,
    minor: u8,
}

impl PdfHeader {
    pub(crate) const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }

    /// Returns the major PDF version component.
    pub const fn major(self) -> u8 {
        self.major
    }

    /// Returns the minor PDF version component.
    pub const fn minor(self) -> u8 {
        self.minor
    }
}

/// Indirect PDF object identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectRef {
    number: u32,
    generation: u16,
}

impl ObjectRef {
    /// Creates an indirect object reference with a nonzero object number.
    pub fn new(number: u32, generation: u16) -> Result<Self, SyntaxError> {
        if number == 0 {
            return Err(SyntaxError::for_code(
                SyntaxErrorCode::InvalidReference,
                None,
            ));
        }
        Ok(Self { number, generation })
    }

    pub(crate) const fn from_valid_parts(number: u32, generation: u16) -> Self {
        Self { number, generation }
    }

    /// Returns the object number.
    pub const fn number(self) -> u32 {
        self.number
    }

    /// Returns the generation number.
    pub const fn generation(self) -> u16 {
        self.generation
    }
}

/// Real-number lexical notation retained for faithful rewriting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RealNotation {
    /// Decimal notation without an exponent.
    Decimal,
    /// Exponent notation using `e` or `E`.
    Exponent,
}

/// A PDF real number that preserves its validated raw lexeme.
#[derive(Clone, Eq, PartialEq)]
pub struct PdfReal {
    raw: Vec<u8>,
    notation: RealNotation,
}

impl PdfReal {
    pub(crate) fn new(raw: Vec<u8>, notation: RealNotation) -> Self {
        Self { raw, notation }
    }

    /// Returns the validated original numeric lexeme.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Returns the retained lexical notation.
    pub const fn notation(&self) -> RealNotation {
        self.notation
    }
}

impl fmt::Debug for PdfReal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PdfReal")
            .field("notation", &self.notation)
            .field("raw", &"[REDACTED]")
            .finish()
    }
}

/// Decoded PDF name bytes.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PdfName(Vec<u8>);

impl PdfName {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrows decoded name bytes without assuming UTF-8.
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for PdfName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PdfName([REDACTED])")
    }
}

/// Source encoding of a decoded PDF string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StringKind {
    /// Parenthesized literal string.
    Literal,
    /// Angle-bracket hexadecimal string.
    Hexadecimal,
}

/// Decoded PDF string bytes with their source encoding kind.
#[derive(Clone, Eq, PartialEq)]
pub struct PdfString {
    bytes: Vec<u8>,
    kind: StringKind,
}

impl PdfString {
    pub(crate) fn new(bytes: Vec<u8>, kind: StringKind) -> Self {
        Self { bytes, kind }
    }

    /// Borrows decoded string bytes without assuming a text encoding.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns whether the source used literal or hexadecimal syntax.
    pub const fn kind(&self) -> StringKind {
        self.kind
    }
}

impl fmt::Debug for PdfString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PdfString")
            .field("kind", &self.kind)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// Ordered direct-object array.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PdfArray(Vec<Located<SyntaxObject>>);

impl PdfArray {
    pub(crate) fn new(values: Vec<Located<SyntaxObject>>) -> Self {
        Self(values)
    }

    /// Returns array values in source order.
    pub fn values(&self) -> &[Located<SyntaxObject>] {
        &self.0
    }
}

/// One ordered dictionary key/value pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DictionaryEntry {
    key: Located<PdfName>,
    value: Located<SyntaxObject>,
}

impl DictionaryEntry {
    pub(crate) const fn new(key: Located<PdfName>, value: Located<SyntaxObject>) -> Self {
        Self { key, value }
    }

    /// Returns the decoded source-located key.
    pub const fn key(&self) -> &Located<PdfName> {
        &self.key
    }

    /// Returns the source-located value.
    pub const fn value(&self) -> &Located<SyntaxObject> {
        &self.value
    }
}

/// Ordered PDF dictionary preserving duplicate keys for later policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PdfDictionary(Vec<DictionaryEntry>);

impl PdfDictionary {
    pub(crate) fn new(entries: Vec<DictionaryEntry>) -> Self {
        Self(entries)
    }

    /// Returns entries in source order.
    pub fn entries(&self) -> &[DictionaryEntry] {
        &self.0
    }

    /// Returns the final source occurrence of a decoded key.
    pub fn get(&self, key: &[u8]) -> Option<&Located<SyntaxObject>> {
        self.0
            .iter()
            .rev()
            .find(|entry| entry.key.value().bytes() == key)
            .map(DictionaryEntry::value)
    }
}

/// Strict direct PDF object syntax.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyntaxObject {
    /// The `null` object.
    Null,
    /// A boolean object.
    Boolean(bool),
    /// A signed integer object.
    Integer(i64),
    /// A real-number object retaining its lexeme.
    Real(PdfReal),
    /// A decoded name object.
    Name(PdfName),
    /// A decoded string object.
    String(PdfString),
    /// An ordered array object.
    Array(PdfArray),
    /// An ordered dictionary object.
    Dictionary(PdfDictionary),
    /// An indirect object reference.
    Reference(ObjectRef),
}

impl SyntaxObject {
    /// Returns the integer value when this object is an integer.
    pub const fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the dictionary when this object is a dictionary.
    pub const fn as_dictionary(&self) -> Option<&PdfDictionary> {
        match self {
            Self::Dictionary(value) => Some(value),
            _ => None,
        }
    }

    /// Returns the indirect reference when this object is a reference.
    pub const fn as_reference(&self) -> Option<ObjectRef> {
        match self {
            Self::Reference(value) => Some(*value),
            _ => None,
        }
    }
}
