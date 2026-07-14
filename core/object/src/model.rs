use std::fmt;

use pdf_rs_bytes::SourceSnapshot;
use pdf_rs_syntax::{ByteSpan, Located, ObjectRef, PdfDictionary, SyntaxObject};

use crate::{ObjectError, ObjectErrorCode};

/// Snapshot-bound location of one indirect object selected by cross-reference metadata.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct IndirectObjectTarget {
    snapshot: SourceSnapshot,
    reference: ObjectRef,
    xref_offset: u64,
    object_upper_bound: u64,
    revision_startxref: u64,
}

impl IndirectObjectTarget {
    /// Validates and binds an indirect-object target to a known-length source snapshot.
    pub fn new(
        snapshot: SourceSnapshot,
        reference: ObjectRef,
        xref_offset: u64,
        object_upper_bound: u64,
        revision_startxref: u64,
    ) -> Result<Self, ObjectError> {
        let Some(source_len) = snapshot.len() else {
            return Err(ObjectError::for_code(
                ObjectErrorCode::UnknownSourceLength,
                Some(reference),
                None,
            ));
        };
        if xref_offset >= object_upper_bound
            || object_upper_bound > revision_startxref
            || revision_startxref >= source_len
        {
            let offset = if xref_offset >= object_upper_bound {
                xref_offset
            } else if object_upper_bound > revision_startxref {
                object_upper_bound
            } else {
                revision_startxref
            };
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidTarget,
                Some(reference),
                Some(offset),
            ));
        }
        Ok(Self {
            snapshot,
            reference,
            xref_offset,
            object_upper_bound,
            revision_startxref,
        })
    }

    /// Returns the complete immutable source snapshot owning the target.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the expected indirect-object number and generation.
    pub const fn reference(self) -> ObjectRef {
        self.reference
    }

    /// Returns the absolute offset supplied by cross-reference metadata.
    pub const fn xref_offset(self) -> u64 {
        self.xref_offset
    }

    /// Returns the exclusive physical bound supplied by the revision object index.
    pub const fn object_upper_bound(self) -> u64 {
        self.object_upper_bound
    }

    /// Returns the cross-reference offset anchoring the target revision.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }
}

impl fmt::Debug for IndirectObjectTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IndirectObjectTarget")
            .field("snapshot", &self.snapshot)
            .field("reference", &self.reference)
            .field("xref_offset", &self.xref_offset)
            .field("object_upper_bound", &self.object_upper_bound)
            .field("revision_startxref", &self.revision_startxref)
            .finish()
    }
}

/// One validated indirect object framed at its exact cross-reference offset.
#[derive(Eq, PartialEq)]
pub struct IndirectObject {
    snapshot: SourceSnapshot,
    reference: ObjectRef,
    revision_startxref: u64,
    xref_offset: u64,
    object_upper_bound: u64,
    header_span: ByteSpan,
    object_span: ByteSpan,
    endobj_span: ByteSpan,
    retained_heap_bytes: u64,
    value: IndirectObjectValue,
}

impl IndirectObject {
    pub(crate) const fn new(
        target: IndirectObjectTarget,
        header_span: ByteSpan,
        object_span: ByteSpan,
        endobj_span: ByteSpan,
        retained_heap_bytes: u64,
        value: IndirectObjectValue,
    ) -> Self {
        Self {
            snapshot: target.snapshot,
            reference: target.reference,
            revision_startxref: target.revision_startxref,
            xref_offset: target.xref_offset,
            object_upper_bound: target.object_upper_bound,
            header_span,
            object_span,
            endobj_span,
            retained_heap_bytes,
            value,
        }
    }

    /// Returns the complete immutable source snapshot owning this object.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the validated indirect-object number and generation.
    pub const fn reference(&self) -> ObjectRef {
        self.reference
    }

    /// Returns the cross-reference offset anchoring the object's revision.
    pub const fn revision_startxref(&self) -> u64 {
        self.revision_startxref
    }

    /// Returns the absolute offset at which the validated object header begins.
    pub const fn xref_offset(&self) -> u64 {
        self.xref_offset
    }

    /// Returns the exclusive physical bound used while framing this object.
    pub const fn object_upper_bound(&self) -> u64 {
        self.object_upper_bound
    }

    /// Returns the exact span of the object-number, generation, and `obj` header.
    pub const fn header_span(&self) -> ByteSpan {
        self.header_span
    }

    /// Returns the exact span from the object header through the `endobj` keyword.
    pub const fn object_span(&self) -> ByteSpan {
        self.object_span
    }

    /// Returns the exact span of the terminal `endobj` keyword.
    pub const fn endobj_span(&self) -> ByteSpan {
        self.endobj_span
    }

    /// Returns allocator-reported syntax heap capacity retained by this object.
    ///
    /// The count includes decoded scalar buffers plus array and dictionary
    /// backing capacity. It excludes the inline object representation,
    /// allocator metadata, and stream payload bytes, which are not retained.
    pub const fn retained_heap_bytes(&self) -> u64 {
        self.retained_heap_bytes
    }

    /// Borrows the validated direct value or framed stream value.
    pub const fn value(&self) -> &IndirectObjectValue {
        &self.value
    }
}

impl fmt::Debug for IndirectObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IndirectObject")
            .field("snapshot", &self.snapshot)
            .field("reference", &self.reference)
            .field("revision_startxref", &self.revision_startxref)
            .field("xref_offset", &self.xref_offset)
            .field("object_upper_bound", &self.object_upper_bound)
            .field("header_span", &self.header_span)
            .field("object_span", &self.object_span)
            .field("endobj_span", &self.endobj_span)
            .field("retained_heap_bytes", &self.retained_heap_bytes)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// Validated body of one indirect object.
#[derive(Eq, PartialEq)]
pub enum IndirectObjectValue {
    /// One source-located direct PDF object.
    Direct(Located<SyntaxObject>),
    /// One dictionary and direct-length stream framing.
    Stream(FramedStream),
}

impl fmt::Debug for IndirectObjectValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct(_) => formatter.write_str("Direct([REDACTED])"),
            Self::Stream(_) => formatter.write_str("Stream([REDACTED])"),
        }
    }
}

/// Validated direct-length stream framing without retained stream payload bytes.
///
/// This model proves the dictionary, direct `/Length`, stream keywords, line
/// endings, and payload span. It retains neither payload bytes nor decoded
/// content; consumers request that range separately when needed.
#[derive(Eq, PartialEq)]
pub struct FramedStream {
    dictionary: Located<PdfDictionary>,
    length_value_span: ByteSpan,
    stream_keyword_span: ByteSpan,
    stream_line_ending_span: ByteSpan,
    data_span: ByteSpan,
    data_delimiter_span: ByteSpan,
    endstream_span: ByteSpan,
}

impl FramedStream {
    pub(crate) const fn new(
        dictionary: Located<PdfDictionary>,
        length_value_span: ByteSpan,
        stream_keyword_span: ByteSpan,
        stream_line_ending_span: ByteSpan,
        data_span: ByteSpan,
        data_delimiter_span: ByteSpan,
        endstream_span: ByteSpan,
    ) -> Self {
        Self {
            dictionary,
            length_value_span,
            stream_keyword_span,
            stream_line_ending_span,
            data_span,
            data_delimiter_span,
            endstream_span,
        }
    }

    /// Returns the source-located stream dictionary.
    pub const fn dictionary(&self) -> &Located<PdfDictionary> {
        &self.dictionary
    }

    /// Returns the exact span of the validated direct `/Length` value.
    pub const fn length_value_span(&self) -> ByteSpan {
        self.length_value_span
    }

    /// Returns the exact span of the `stream` keyword.
    pub const fn stream_keyword_span(&self) -> ByteSpan {
        self.stream_keyword_span
    }

    /// Returns the exact line-ending span immediately following `stream`.
    pub const fn stream_line_ending_span(&self) -> ByteSpan {
        self.stream_line_ending_span
    }

    /// Returns the checked source span of the unretained and undecoded payload.
    pub const fn data_span(&self) -> ByteSpan {
        self.data_span
    }

    /// Returns the exact line-ending span separating payload from `endstream`.
    pub const fn data_delimiter_span(&self) -> ByteSpan {
        self.data_delimiter_span
    }

    /// Returns the exact span of the `endstream` keyword.
    pub const fn endstream_span(&self) -> ByteSpan {
        self.endstream_span
    }
}

impl fmt::Debug for FramedStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FramedStream")
            .field("dictionary", &"[REDACTED]")
            .field("length_value_span", &self.length_value_span)
            .field("stream_keyword_span", &self.stream_keyword_span)
            .field("stream_line_ending_span", &self.stream_line_ending_span)
            .field("data_span", &self.data_span)
            .field("data_delimiter_span", &self.data_delimiter_span)
            .field("endstream_span", &self.endstream_span)
            .finish()
    }
}
