use pdf_rs_bytes::SourceIdentity;
use pdf_rs_syntax::{
    ByteSpan, InputExtent, Located, ObjectRef, PdfDictionary, SyntaxCancellation, SyntaxInput,
    SyntaxLimits, SyntaxObject, SyntaxParser, SyntaxPoll,
};

use crate::job::ObjectCancellation;
use crate::{ObjectError, ObjectErrorCode, ObjectLimitKind, ObjectLimits};

const CANCELLATION_PROBE_INTERVAL: u16 = 256;

#[derive(Clone, Copy)]
pub(crate) struct EnvelopeContext {
    pub(crate) source: SourceIdentity,
    pub(crate) reference: ObjectRef,
    pub(crate) xref_offset: u64,
    pub(crate) object_upper_bound: u64,
    pub(crate) limits: ObjectLimits,
    pub(crate) syntax_limits: SyntaxLimits,
}

pub(crate) enum EnvelopeParse {
    NeedMore { minimum_end: u64 },
    Direct(ParsedDirect),
    Stream(ParsedStreamEnvelope),
}

pub(crate) struct ParsedDirect {
    pub(crate) header_span: ByteSpan,
    pub(crate) value: Located<SyntaxObject>,
    pub(crate) endobj_span: ByteSpan,
}

pub(crate) struct ParsedStreamEnvelope {
    pub(crate) header_span: ByteSpan,
    pub(crate) dictionary: Located<PdfDictionary>,
    pub(crate) length_value_span: ByteSpan,
    pub(crate) stream_keyword_span: ByteSpan,
    pub(crate) stream_line_ending_span: ByteSpan,
    pub(crate) data_span: ByteSpan,
}

pub(crate) enum BoundaryParse {
    NeedMore { minimum_end: u64 },
    Complete(ParsedBoundary),
}

pub(crate) struct ParsedBoundary {
    pub(crate) data_delimiter_span: ByteSpan,
    pub(crate) endstream_span: ByteSpan,
    pub(crate) endobj_span: ByteSpan,
}

struct SyntaxCancellationAdapter<'a>(&'a dyn ObjectCancellation);

impl SyntaxCancellation for SyntaxCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

pub(crate) fn parse_envelope(
    context: EnvelopeContext,
    bytes: &[u8],
    extent: InputExtent,
    cancellation: &dyn ObjectCancellation,
) -> Result<EnvelopeParse, ObjectError> {
    let reference = context.reference;
    let input = SyntaxInput::new(context.source, context.xref_offset, bytes, extent)
        .map_err(|error| ObjectError::from_syntax(error, Some(reference)))?;
    let adapter = SyntaxCancellationAdapter(cancellation);
    let mut parser = SyntaxParser::new_with_cancellation(input, context.syntax_limits, &adapter)
        .map_err(|error| ObjectError::from_syntax(error, Some(reference)))?;

    let number = match required(
        parser.parse_object(),
        reference,
        context.xref_offset,
        ObjectErrorCode::InvalidObjectHeader,
    )? {
        Required::Ready(value) => value,
        Required::NeedMore(minimum_end) => return Ok(EnvelopeParse::NeedMore { minimum_end }),
    };
    if number.span().start() != context.xref_offset
        || number.value().as_integer() != Some(i64::from(reference.number()))
    {
        return Err(ObjectError::for_code(
            ObjectErrorCode::InvalidObjectHeader,
            Some(reference),
            Some(number.span().start()),
        ));
    }

    let generation_offset = parser.position();
    let generation = match required(
        parser.parse_object(),
        reference,
        generation_offset,
        ObjectErrorCode::InvalidObjectHeader,
    )? {
        Required::Ready(value) => value,
        Required::NeedMore(minimum_end) => return Ok(EnvelopeParse::NeedMore { minimum_end }),
    };
    if generation.value().as_integer() != Some(i64::from(reference.generation())) {
        return Err(ObjectError::for_code(
            ObjectErrorCode::InvalidObjectHeader,
            Some(reference),
            Some(generation.span().start()),
        ));
    }

    let obj_keyword = match required(
        parser.parse_keyword(),
        reference,
        parser.position(),
        ObjectErrorCode::InvalidObjectHeader,
    )? {
        Required::Ready(value) => value,
        Required::NeedMore(minimum_end) => return Ok(EnvelopeParse::NeedMore { minimum_end }),
    };
    if obj_keyword.bytes() != b"obj" {
        return Err(ObjectError::for_code(
            ObjectErrorCode::InvalidObjectHeader,
            Some(reference),
            Some(obj_keyword.span().start()),
        ));
    }
    let header_span = span_from_bounds(
        context.xref_offset,
        obj_keyword.span().end_exclusive(),
        reference,
    )?;

    let body = match required(
        parser.parse_object(),
        reference,
        parser.position(),
        ObjectErrorCode::InvalidObjectEnvelope,
    )? {
        Required::Ready(value) => value,
        Required::NeedMore(minimum_end) => return Ok(EnvelopeParse::NeedMore { minimum_end }),
    };
    let terminal = match required(
        parser.parse_keyword(),
        reference,
        parser.position(),
        ObjectErrorCode::InvalidObjectEnvelope,
    )? {
        Required::Ready(value) => value,
        Required::NeedMore(minimum_end) => return Ok(EnvelopeParse::NeedMore { minimum_end }),
    };

    match terminal.bytes() {
        b"endobj" => Ok(EnvelopeParse::Direct(ParsedDirect {
            header_span,
            value: body,
            endobj_span: terminal.span(),
        })),
        b"stream" => {
            let dictionary = body.try_map(|value| match value {
                SyntaxObject::Dictionary(dictionary) => Ok(dictionary),
                _ => Err(ObjectError::for_code(
                    ObjectErrorCode::InvalidObjectEnvelope,
                    Some(reference),
                    Some(terminal.span().start()),
                )),
            })?;
            let (stream_length, length_value_span) =
                direct_stream_length(&dictionary, reference, cancellation)?;
            if stream_length > context.limits.max_stream_bytes() {
                return Err(ObjectError::resource(
                    ObjectLimitKind::StreamBytes,
                    context.limits.max_stream_bytes(),
                    0,
                    stream_length,
                    Some(reference),
                    Some(length_value_span.start()),
                ));
            }

            let stream_line_ending_span = match required(
                parser.consume_stream_line_ending(),
                reference,
                terminal.span().end_exclusive(),
                ObjectErrorCode::InvalidStreamBoundary,
            )? {
                Required::Ready(value) => value,
                Required::NeedMore(minimum_end) => {
                    return Ok(EnvelopeParse::NeedMore { minimum_end });
                }
            };
            let data_start = stream_line_ending_span.end_exclusive();
            let data_end = data_start.checked_add(stream_length).ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InvalidStreamLength,
                    Some(reference),
                    Some(length_value_span.start()),
                )
            })?;
            if data_end >= context.object_upper_bound {
                return Err(ObjectError::for_code(
                    ObjectErrorCode::ObjectCrossesPhysicalBound,
                    Some(reference),
                    Some(context.object_upper_bound),
                ));
            }
            let data_span = ByteSpan::new(data_start, stream_length).map_err(|_| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(reference),
                    Some(data_start),
                )
            })?;
            Ok(EnvelopeParse::Stream(ParsedStreamEnvelope {
                header_span,
                dictionary,
                length_value_span,
                stream_keyword_span: terminal.span(),
                stream_line_ending_span,
                data_span,
            }))
        }
        _ => Err(ObjectError::for_code(
            ObjectErrorCode::InvalidObjectEnvelope,
            Some(reference),
            Some(terminal.span().start()),
        )),
    }
}

pub(crate) fn parse_boundary(
    source: SourceIdentity,
    reference: ObjectRef,
    data_end: u64,
    bytes: &[u8],
    extent: InputExtent,
    syntax_limits: SyntaxLimits,
    cancellation: &dyn ObjectCancellation,
) -> Result<BoundaryParse, ObjectError> {
    let input = SyntaxInput::new(source, data_end, bytes, extent)
        .map_err(|error| ObjectError::from_syntax(error, Some(reference)))?;
    let adapter = SyntaxCancellationAdapter(cancellation);
    let mut parser = SyntaxParser::new_with_cancellation(input, syntax_limits, &adapter)
        .map_err(|error| ObjectError::from_syntax(error, Some(reference)))?;

    let data_delimiter_span = match required(
        parser.consume_stream_line_ending(),
        reference,
        data_end,
        ObjectErrorCode::InvalidStreamBoundary,
    )? {
        Required::Ready(value) => value,
        Required::NeedMore(minimum_end) => return Ok(BoundaryParse::NeedMore { minimum_end }),
    };
    let endstream = match required(
        parser.parse_keyword(),
        reference,
        data_delimiter_span.end_exclusive(),
        ObjectErrorCode::InvalidStreamBoundary,
    )? {
        Required::Ready(value) => value,
        Required::NeedMore(minimum_end) => return Ok(BoundaryParse::NeedMore { minimum_end }),
    };
    if endstream.span().start() != data_delimiter_span.end_exclusive()
        || endstream.bytes() != b"endstream"
    {
        return Err(ObjectError::for_code(
            ObjectErrorCode::InvalidStreamBoundary,
            Some(reference),
            Some(endstream.span().start()),
        ));
    }

    let endobj = match required(
        parser.parse_keyword(),
        reference,
        endstream.span().end_exclusive(),
        ObjectErrorCode::InvalidStreamBoundary,
    )? {
        Required::Ready(value) => value,
        Required::NeedMore(minimum_end) => return Ok(BoundaryParse::NeedMore { minimum_end }),
    };
    if endobj.bytes() != b"endobj" {
        return Err(ObjectError::for_code(
            ObjectErrorCode::InvalidStreamBoundary,
            Some(reference),
            Some(endobj.span().start()),
        ));
    }

    Ok(BoundaryParse::Complete(ParsedBoundary {
        data_delimiter_span,
        endstream_span: endstream.span(),
        endobj_span: endobj.span(),
    }))
}

fn direct_stream_length(
    dictionary: &Located<PdfDictionary>,
    reference: ObjectRef,
    cancellation: &dyn ObjectCancellation,
) -> Result<(u64, ByteSpan), ObjectError> {
    let mut found = None;
    let mut countdown = CANCELLATION_PROBE_INTERVAL;
    for entry in dictionary.value().entries() {
        if countdown == CANCELLATION_PROBE_INTERVAL && cancellation.is_cancelled() {
            return Err(ObjectError::for_code(
                ObjectErrorCode::Cancelled,
                Some(reference),
                Some(entry.key().span().start()),
            ));
        }
        countdown = if countdown == 1 {
            CANCELLATION_PROBE_INTERVAL
        } else {
            countdown - 1
        };
        if entry.key().value().bytes() != b"Length" {
            continue;
        }
        if found.is_some() {
            return Err(ObjectError::for_code(
                ObjectErrorCode::DuplicateStreamLength,
                Some(reference),
                Some(entry.value().span().start()),
            ));
        }
        found = Some(entry.value());
    }

    let value = found.ok_or_else(|| {
        ObjectError::for_code(
            ObjectErrorCode::MissingStreamLength,
            Some(reference),
            Some(dictionary.span().start()),
        )
    })?;
    match value.value() {
        SyntaxObject::Integer(length) => {
            let length = u64::try_from(*length).map_err(|_| {
                ObjectError::for_code(
                    ObjectErrorCode::InvalidStreamLength,
                    Some(reference),
                    Some(value.span().start()),
                )
            })?;
            Ok((length, value.span()))
        }
        SyntaxObject::Reference(_) => Err(ObjectError::for_code(
            ObjectErrorCode::UnsupportedIndirectLength,
            Some(reference),
            Some(value.span().start()),
        )),
        _ => Err(ObjectError::for_code(
            ObjectErrorCode::InvalidStreamLength,
            Some(reference),
            Some(value.span().start()),
        )),
    }
}

enum Required<T> {
    Ready(T),
    NeedMore(u64),
}

fn required<T>(
    poll: SyntaxPoll<T>,
    reference: ObjectRef,
    fallback_offset: u64,
    end_code: ObjectErrorCode,
) -> Result<Required<T>, ObjectError> {
    match poll {
        SyntaxPoll::Ready(value) => Ok(Required::Ready(value)),
        SyntaxPoll::NeedMore { minimum_end } => Ok(Required::NeedMore(minimum_end)),
        SyntaxPoll::EndOfInput => Err(ObjectError::for_code(
            end_code,
            Some(reference),
            Some(fallback_offset),
        )),
        SyntaxPoll::Failed(error) => Err(ObjectError::from_syntax_for(
            end_code,
            error,
            Some(reference),
        )),
    }
}

fn span_from_bounds(
    start: u64,
    end_exclusive: u64,
    reference: ObjectRef,
) -> Result<ByteSpan, ObjectError> {
    let len = end_exclusive.checked_sub(start).ok_or_else(|| {
        ObjectError::for_code(ObjectErrorCode::InternalState, Some(reference), Some(start))
    })?;
    ByteSpan::new(start, len).map_err(|_| {
        ObjectError::for_code(ObjectErrorCode::InternalState, Some(reference), Some(start))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pdf_rs_bytes::{SourceRevision, SourceStableId};

    use super::*;

    struct CancelOnProbe {
        probes: AtomicUsize,
        cancel_at: usize,
    }

    impl ObjectCancellation for CancelOnProbe {
        fn is_cancelled(&self) -> bool {
            self.probes.fetch_add(1, Ordering::AcqRel) + 1 >= self.cancel_at
        }
    }

    #[test]
    fn object_owned_length_scan_checks_cancellation_after_at_most_256_entries() {
        let mut bytes = b"<<".to_vec();
        for _ in 0..300 {
            bytes.extend_from_slice(b" /K 0");
        }
        bytes.extend_from_slice(b" /Length 0 >>");
        let source = SourceIdentity::new(SourceStableId::new([0x52; 32]), SourceRevision::new(1));
        let input = SyntaxInput::new(source, 0, &bytes, InputExtent::KnownSourceEnd).unwrap();
        let mut syntax = SyntaxParser::new(input, SyntaxLimits::default()).unwrap();
        let SyntaxPoll::Ready(value) = syntax.parse_object() else {
            panic!("the generated test dictionary must parse")
        };
        let dictionary = value
            .try_map(|value| match value {
                SyntaxObject::Dictionary(dictionary) => Ok(dictionary),
                _ => Err(()),
            })
            .unwrap();
        let cancellation = CancelOnProbe {
            probes: AtomicUsize::new(0),
            cancel_at: 2,
        };

        let error = direct_stream_length(&dictionary, ObjectRef::new(1, 0).unwrap(), &cancellation)
            .unwrap_err();
        assert_eq!(error.code(), ObjectErrorCode::Cancelled);
        assert_eq!(cancellation.probes.load(Ordering::Acquire), 2);
    }
}
