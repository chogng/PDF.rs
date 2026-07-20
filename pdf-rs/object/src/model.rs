use std::fmt;

use pdf_rs_bytes::SourceSnapshot;
use pdf_rs_syntax::{ByteSpan, Located, ObjectRef, PdfDictionary, SyntaxLimits, SyntaxObject};

use crate::{
    ObjectError, ObjectErrorCode, ObjectJobContext, ObjectLimits, ObjectStats, ObjectWorkCaps,
};

/// Geometry authority used to construct one indirect-object framing target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndirectObjectTargetKind {
    /// A normal uncompressed xref entry whose object ends no later than its revision anchor.
    XrefEntry,
    /// An xref-stream container whose indirect object begins at the section anchor itself.
    XrefStreamAnchor,
}

/// Snapshot-bound location of one indirect object selected by cross-reference metadata.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct IndirectObjectTarget {
    snapshot: SourceSnapshot,
    reference: ObjectRef,
    xref_offset: u64,
    object_upper_bound: u64,
    revision_startxref: u64,
    kind: IndirectObjectTargetKind,
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
            kind: IndirectObjectTargetKind::XrefEntry,
        })
    }

    /// Binds an indirect xref-stream container that begins at its own section anchor.
    ///
    /// Unlike [`Self::new`], a primary xref-stream may extend beyond its own `startxref`
    /// to an independently supplied object bound. A hybrid supplement must instead end
    /// exactly at its owning traditional primary anchor. The ordinary xref-entry
    /// constructor retains its stricter `object_upper_bound <= revision_startxref`
    /// invariant.
    pub fn at_xref_stream_anchor(
        snapshot: SourceSnapshot,
        reference: ObjectRef,
        startxref: u64,
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
        let hybrid_upper_bound_mismatch =
            revision_startxref > startxref && object_upper_bound != revision_startxref;
        if startxref >= object_upper_bound
            || object_upper_bound > source_len
            || revision_startxref < startxref
            || revision_startxref >= source_len
            || hybrid_upper_bound_mismatch
        {
            let offset = if startxref >= object_upper_bound {
                startxref
            } else if object_upper_bound > source_len {
                object_upper_bound
            } else if revision_startxref < startxref || revision_startxref >= source_len {
                revision_startxref
            } else {
                object_upper_bound
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
            xref_offset: startxref,
            object_upper_bound,
            revision_startxref,
            kind: IndirectObjectTargetKind::XrefStreamAnchor,
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

    /// Returns the explicit geometry authority used to create this target.
    pub const fn kind(self) -> IndirectObjectTargetKind {
        self.kind
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
            .field("kind", &self.kind)
            .finish()
    }
}

/// One validated indirect object framed at its exact cross-reference offset.
#[derive(Eq, PartialEq)]
pub struct IndirectObject {
    snapshot: SourceSnapshot,
    reference: ObjectRef,
    revision_startxref: u64,
    target_kind: IndirectObjectTargetKind,
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
            target_kind: target.kind,
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

    /// Returns the explicit geometry authority carried from the framing target.
    pub const fn target_kind(&self) -> IndirectObjectTargetKind {
        self.target_kind
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
            .field("target_kind", &self.target_kind)
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
#[allow(
    clippy::large_enum_variant,
    reason = "validated one-shot object values stay inline instead of adding an untracked allocation"
)]
#[derive(Eq, PartialEq)]
pub enum IndirectObjectValue {
    /// One source-located direct PDF object.
    Direct(Located<SyntaxObject>),
    /// One dictionary and direct or resolved-indirect stream framing.
    Stream(FramedStream),
}

/// Source-located declaration of a stream payload length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclaredStreamLength {
    /// A nonnegative integer stored directly in the stream dictionary.
    Direct {
        /// Checked payload length in bytes.
        value: u64,
        /// Exact physical span of the dictionary operand.
        operand_span: ByteSpan,
    },
    /// An indirect reference whose integer value must be resolved separately.
    Indirect {
        /// Referenced integer object.
        reference: ObjectRef,
        /// Exact physical span of the dictionary operand.
        operand_span: ByteSpan,
    },
}

impl DeclaredStreamLength {
    /// Returns the exact physical span of the `/Length` operand.
    pub const fn operand_span(self) -> ByteSpan {
        match self {
            Self::Direct { operand_span, .. } | Self::Indirect { operand_span, .. } => operand_span,
        }
    }

    /// Returns the direct payload length, or `None` when resolution is required.
    pub const fn direct_value(self) -> Option<u64> {
        match self {
            Self::Direct { value, .. } => Some(value),
            Self::Indirect { .. } => None,
        }
    }

    /// Returns the indirect dependency, or `None` for a direct declaration.
    pub const fn indirect_reference(self) -> Option<ObjectRef> {
        match self {
            Self::Direct { .. } => None,
            Self::Indirect { reference, .. } => Some(reference),
        }
    }
}

/// Resolver-supplied proof metadata for an uncompressed indirect stream length.
///
/// The object resolver owns validation that `value_span` is the complete integer
/// value of `reference`. The staged boundary job independently verifies that the
/// snapshot and reference match the stream envelope before accepting it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedStreamLength {
    snapshot: SourceSnapshot,
    reference: ObjectRef,
    value: u64,
    value_span: ByteSpan,
}

impl ResolvedStreamLength {
    /// Derives proof metadata from one header-validated uncompressed integer object.
    ///
    /// This proves the physical object and integer syntax. A revision-aware resolver
    /// must still prove that the object is the effective definition selected for the
    /// declared reference.
    pub fn from_uncompressed_object(object: &IndirectObject) -> Result<Self, ObjectError> {
        let IndirectObjectValue::Direct(located) = object.value() else {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidStreamLength,
                Some(object.reference()),
                Some(object.object_span().start()),
            ));
        };
        let SyntaxObject::Integer(value) = located.value() else {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidStreamLength,
                Some(object.reference()),
                Some(located.span().start()),
            ));
        };
        let value = u64::try_from(*value).map_err(|_| {
            ObjectError::for_code(
                ObjectErrorCode::InvalidStreamLength,
                Some(object.reference()),
                Some(located.span().start()),
            )
        })?;
        if located.source() != object.snapshot().identity() {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidStreamLengthClaim,
                Some(object.reference()),
                Some(located.span().start()),
            ));
        }
        Ok(Self {
            snapshot: object.snapshot(),
            reference: object.reference(),
            value,
            value_span: located.span(),
        })
    }

    /// Returns the immutable source snapshot in which the integer was resolved.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the resolved integer object's reference.
    pub const fn reference(self) -> ObjectRef {
        self.reference
    }

    /// Returns the checked nonnegative integer value.
    pub const fn value(self) -> u64 {
        self.value
    }

    /// Returns the exact physical span of the resolved integer value.
    pub const fn value_span(self) -> ByteSpan {
        self.value_span
    }
}

/// Snapshot-bound payload-length claim accepted by stream boundary framing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamLengthClaim {
    snapshot: SourceSnapshot,
    owner: ObjectRef,
    declaration: DeclaredStreamLength,
    value: u64,
    resolved_value_span: Option<ByteSpan>,
}

impl StreamLengthClaim {
    pub(crate) const fn direct(
        snapshot: SourceSnapshot,
        owner: ObjectRef,
        declaration: DeclaredStreamLength,
        value: u64,
    ) -> Self {
        Self {
            snapshot,
            owner,
            declaration,
            value,
            resolved_value_span: None,
        }
    }

    pub(crate) const fn repaired_direct(
        snapshot: SourceSnapshot,
        owner: ObjectRef,
        declaration: DeclaredStreamLength,
        value: u64,
    ) -> Self {
        Self {
            snapshot,
            owner,
            declaration,
            value,
            resolved_value_span: None,
        }
    }

    /// Returns the source snapshot to which this claim is bound.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the stream object whose declaration this claim satisfies.
    pub const fn owner(self) -> ObjectRef {
        self.owner
    }

    /// Returns the original direct or indirect dictionary declaration.
    pub const fn declaration(self) -> DeclaredStreamLength {
        self.declaration
    }

    /// Returns the checked payload length in bytes.
    pub const fn value(self) -> u64 {
        self.value
    }

    /// Returns the resolved integer value span for an indirect declaration.
    pub const fn resolved_value_span(self) -> Option<ByteSpan> {
        self.resolved_value_span
    }
}

/// Validated stream dictionary and payload start awaiting exact-length framing.
#[derive(Eq, PartialEq)]
pub struct StreamEnvelope {
    pub(crate) target: IndirectObjectTarget,
    pub(crate) header_span: ByteSpan,
    pub(crate) dictionary: Located<PdfDictionary>,
    pub(crate) declared_length: DeclaredStreamLength,
    pub(crate) stream_keyword_span: ByteSpan,
    pub(crate) stream_line_ending_span: ByteSpan,
    pub(crate) data_start: u64,
    pub(crate) retained_heap_bytes: u64,
    pub(crate) context: ObjectJobContext,
    pub(crate) limits: ObjectLimits,
    pub(crate) work_caps: ObjectWorkCaps,
    pub(crate) syntax_limits: SyntaxLimits,
    pub(crate) stats: ObjectStats,
}

impl StreamEnvelope {
    #[allow(
        clippy::too_many_arguments,
        reason = "construction copies one validated parser record without an intermediate allocation"
    )]
    pub(crate) const fn new(
        target: IndirectObjectTarget,
        header_span: ByteSpan,
        dictionary: Located<PdfDictionary>,
        declared_length: DeclaredStreamLength,
        stream_keyword_span: ByteSpan,
        stream_line_ending_span: ByteSpan,
        data_start: u64,
        retained_heap_bytes: u64,
        context: ObjectJobContext,
        limits: ObjectLimits,
        work_caps: ObjectWorkCaps,
        syntax_limits: SyntaxLimits,
        stats: ObjectStats,
    ) -> Self {
        Self {
            target,
            header_span,
            dictionary,
            declared_length,
            stream_keyword_span,
            stream_line_ending_span,
            data_start,
            retained_heap_bytes,
            context,
            limits,
            work_caps,
            syntax_limits,
            stats,
        }
    }

    /// Returns the immutable source snapshot owning the envelope.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.target.snapshot()
    }

    /// Returns the xref-derived object target.
    pub const fn target(&self) -> IndirectObjectTarget {
        self.target
    }

    /// Returns the exact indirect-object header span.
    pub const fn header_span(&self) -> ByteSpan {
        self.header_span
    }

    /// Returns the source-located stream dictionary.
    pub const fn dictionary(&self) -> &Located<PdfDictionary> {
        &self.dictionary
    }

    /// Returns the direct value or indirect dependency declared by `/Length`.
    pub const fn declared_length(&self) -> DeclaredStreamLength {
        self.declared_length
    }

    /// Returns the exact span of the `stream` keyword.
    pub const fn stream_keyword_span(&self) -> ByteSpan {
        self.stream_keyword_span
    }

    /// Returns the exact line ending immediately following `stream`.
    pub const fn stream_line_ending_span(&self) -> ByteSpan {
        self.stream_line_ending_span
    }

    /// Returns the first physical byte of the opaque stream payload.
    pub const fn data_start(&self) -> u64 {
        self.data_start
    }

    /// Returns allocator-reported syntax heap capacity retained by the envelope.
    pub const fn retained_heap_bytes(&self) -> u64 {
        self.retained_heap_bytes
    }

    /// Returns the original job identity, checkpoints, and priority.
    pub const fn context(&self) -> ObjectJobContext {
        self.context
    }

    /// Returns the object limits sealed by the envelope phase.
    pub const fn limits(&self) -> ObjectLimits {
        self.limits
    }

    /// Returns the cumulative work caps sealed by the envelope phase.
    pub const fn work_caps(&self) -> ObjectWorkCaps {
        self.work_caps
    }

    /// Returns the syntax limits sealed by the envelope phase.
    pub const fn syntax_limits(&self) -> SyntaxLimits {
        self.syntax_limits
    }

    /// Returns cumulative object work already consumed by the envelope phase.
    pub const fn stats(&self) -> ObjectStats {
        self.stats
    }

    /// Creates the exact claim for a direct `/Length` declaration.
    pub fn direct_length_claim(&self) -> Result<StreamLengthClaim, ObjectError> {
        let DeclaredStreamLength::Direct { value, .. } = self.declared_length else {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidStreamLengthClaim,
                Some(self.target.reference()),
                Some(self.declared_length.operand_span().start()),
            ));
        };
        Ok(StreamLengthClaim::direct(
            self.target.snapshot(),
            self.target.reference(),
            self.declared_length,
            value,
        ))
    }

    /// Binds resolver proof metadata to this envelope's indirect dependency.
    pub fn resolved_length_claim(
        &self,
        resolution: ResolvedStreamLength,
    ) -> Result<StreamLengthClaim, ObjectError> {
        let DeclaredStreamLength::Indirect { reference, .. } = self.declared_length else {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidStreamLengthClaim,
                Some(self.target.reference()),
                Some(self.declared_length.operand_span().start()),
            ));
        };
        if resolution.snapshot != self.target.snapshot() || resolution.reference != reference {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidStreamLengthClaim,
                Some(self.target.reference()),
                Some(self.declared_length.operand_span().start()),
            ));
        }
        Ok(StreamLengthClaim {
            snapshot: self.target.snapshot(),
            owner: self.target.reference(),
            declaration: self.declared_length,
            value: resolution.value,
            resolved_value_span: Some(resolution.value_span),
        })
    }
}

impl fmt::Debug for StreamEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamEnvelope")
            .field("target", &self.target)
            .field("header_span", &self.header_span)
            .field("dictionary", &"[REDACTED]")
            .field("declared_length", &self.declared_length)
            .field("stream_keyword_span", &self.stream_keyword_span)
            .field("stream_line_ending_span", &self.stream_line_ending_span)
            .field("data_start", &self.data_start)
            .field("retained_heap_bytes", &self.retained_heap_bytes)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("work_caps", &self.work_caps)
            .field("syntax_limits", &self.syntax_limits)
            .field("stats", &self.stats)
            .finish()
    }
}

impl fmt::Debug for IndirectObjectValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct(_) => formatter.write_str("Direct([REDACTED])"),
            Self::Stream(_) => formatter.write_str("Stream([REDACTED])"),
        }
    }
}

/// Validated exact-length stream framing without retained stream payload bytes.
///
/// This model proves the dictionary, snapshot-bound `/Length` claim, stream
/// keywords, line endings, and payload span. It retains neither payload bytes
/// nor decoded content; consumers request that range separately when needed.
#[derive(Eq, PartialEq)]
pub struct FramedStream {
    dictionary: Located<PdfDictionary>,
    length_claim: StreamLengthClaim,
    stream_keyword_span: ByteSpan,
    stream_line_ending_span: ByteSpan,
    data_span: ByteSpan,
    data_delimiter_span: ByteSpan,
    endstream_span: ByteSpan,
}

impl FramedStream {
    pub(crate) const fn new(
        dictionary: Located<PdfDictionary>,
        length_claim: StreamLengthClaim,
        stream_keyword_span: ByteSpan,
        stream_line_ending_span: ByteSpan,
        data_span: ByteSpan,
        data_delimiter_span: ByteSpan,
        endstream_span: ByteSpan,
    ) -> Self {
        Self {
            dictionary,
            length_claim,
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

    /// Returns the exact span of the direct value or indirect `/Length` operand.
    pub const fn length_value_span(&self) -> ByteSpan {
        self.length_claim.declaration().operand_span()
    }

    /// Returns the snapshot-bound length claim used to frame this stream.
    pub const fn length_claim(&self) -> StreamLengthClaim {
        self.length_claim
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
            .field("length_claim", &self.length_claim)
            .field("stream_keyword_span", &self.stream_keyword_span)
            .field("stream_line_ending_span", &self.stream_line_ending_span)
            .field("data_span", &self.data_span)
            .field("data_delimiter_span", &self.data_delimiter_span)
            .field("endstream_span", &self.endstream_span)
            .finish()
    }
}
