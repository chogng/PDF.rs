use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_object::{
    IndirectObject, IndirectObjectTarget, IndirectObjectValue, NeverCancelled, ObjectError,
    ObjectErrorCategory, ObjectErrorCode, ObjectJobContext, ObjectLimitConfig, ObjectLimitKind,
    ObjectLimits, ObjectPhase, ObjectPoll, ObjectRecoverability, OpenObjectJob,
};
use pdf_rs_syntax::{
    InputExtent, ObjectRef, SyntaxInput, SyntaxLimits, SyntaxObject, SyntaxParser, SyntaxPoll,
};

const PDF_LEN: u64 = 612;
const XREF_OFFSET: u64 = 449;
const OBJECT_OFFSETS: [u64; 4] = [186, 235, 292, 396];

fn identity() -> SourceIdentity {
    SourceIdentity::new(SourceStableId::new([0x61; 32]), SourceRevision::new(13))
}

fn snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        identity(),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x27; 32]),
    )
}

fn object_ref(number: u32, generation: u16) -> ObjectRef {
    ObjectRef::new(number, generation).expect("test references are nonzero and in range")
}

// Project-authored structural fixture matching the canonical M0 generator geometry.
fn canonical_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n%".to_vec();
    pdf.resize(185, b'x');
    pdf.push(b'\n');
    assert_eq!(pdf.len(), 186);

    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    assert_eq!(pdf.len(), 235);
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    assert_eq!(pdf.len(), 292);
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>\nendobj\n",
    );
    assert_eq!(pdf.len(), 396);
    pdf.extend_from_slice(b"4 0 obj\n<< /Length 4 >>\nstream\nq\nQ\n\nendstream\nendobj\n");
    assert_eq!(pdf.len(), usize::try_from(XREF_OFFSET).unwrap());
    pdf.extend_from_slice(
        b"xref\n0 5\n\
0000000000 65535 f \n\
0000000186 00000 n \n\
0000000235 00000 n \n\
0000000292 00000 n \n\
0000000396 00000 n \n\
trailer\n\
<< /Size 5 /Root 1 0 R >>\n\
startxref\n449\n%%EOF\n",
    );
    assert_eq!(pdf.len(), usize::try_from(PDF_LEN).unwrap());
    pdf
}

fn supplied_store(bytes: &[u8]) -> RangeStore {
    let source = snapshot(u64::try_from(bytes.len()).unwrap());
    let store = RangeStore::new(source, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(source, range, bytes.to_vec()).unwrap())
        .unwrap();
    store
}

fn context(priority: RequestPriority) -> ObjectJobContext {
    ObjectJobContext::new(
        JobId::new(41),
        ResumeCheckpoint::new(80),
        ResumeCheckpoint::new(81),
        priority,
    )
}

fn target(
    source: SourceSnapshot,
    reference: ObjectRef,
    offset: u64,
    startxref: u64,
) -> IndirectObjectTarget {
    IndirectObjectTarget::new(source, reference, offset, startxref, startxref)
        .expect("test target geometry is valid")
}

fn bounded_target(
    source: SourceSnapshot,
    reference: ObjectRef,
    offset: u64,
    object_upper_bound: u64,
    startxref: u64,
) -> IndirectObjectTarget {
    IndirectObjectTarget::new(source, reference, offset, object_upper_bound, startxref)
        .expect("test target physical geometry is valid")
}

fn job(target: IndirectObjectTarget, limits: ObjectLimits) -> OpenObjectJob {
    OpenObjectJob::new(
        target,
        context(RequestPriority::VisiblePage),
        limits,
        SyntaxLimits::default(),
    )
    .expect("test object job is valid")
}

fn ready_at(
    bytes: &[u8],
    reference: ObjectRef,
    offset: u64,
    startxref: u64,
) -> (IndirectObject, OpenObjectJob) {
    let store = supplied_store(bytes);
    let mut open = job(
        target(store.snapshot(), reference, offset, startxref),
        ObjectLimits::default(),
    );
    let object = match open.poll(&store, &NeverCancelled) {
        ObjectPoll::Ready(object) => object,
        ObjectPoll::Pending { .. } => panic!("a completely supplied source must not suspend"),
        ObjectPoll::Failed(error) => panic!("expected a framed object, got {error}"),
    };
    (object, open)
}

fn failed_at(
    bytes: &[u8],
    reference: ObjectRef,
    offset: u64,
    startxref: u64,
    limits: ObjectLimits,
) -> ObjectError {
    let store = supplied_store(bytes);
    let mut open = job(
        target(store.snapshot(), reference, offset, startxref),
        limits,
    );
    match open.poll(&store, &NeverCancelled) {
        ObjectPoll::Failed(error) => error,
        ObjectPoll::Ready(_) => panic!("expected object failure, got a ready value"),
        ObjectPoll::Pending { .. } => panic!("a completely supplied source must not suspend"),
    }
}

fn standalone(body: &[u8]) -> (Vec<u8>, u64) {
    let mut bytes = body.to_vec();
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"xref\n");
    (bytes, startxref)
}

fn stream_body(dictionary: &[u8], payload: &[u8], delimiter: &[u8]) -> Vec<u8> {
    let mut body = b"1 0 obj\n".to_vec();
    body.extend_from_slice(dictionary);
    body.extend_from_slice(b"\nstream\n");
    body.extend_from_slice(payload);
    body.extend_from_slice(delimiter);
    body.extend_from_slice(b"endstream\nendobj\n");
    body
}

fn compact_limits(
    source_bytes: u64,
    envelope_bytes: u64,
    boundary_bytes: u64,
    stream_bytes: u64,
) -> ObjectLimits {
    let total = envelope_bytes.checked_add(boundary_bytes).unwrap();
    ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_bytes,
        initial_envelope_bytes: envelope_bytes,
        max_envelope_bytes: envelope_bytes,
        initial_boundary_bytes: boundary_bytes,
        max_boundary_bytes: boundary_bytes,
        max_stream_bytes: stream_bytes,
        max_total_read_bytes: total,
        max_total_parse_bytes: total,
    })
    .expect("compact test limits are internally consistent")
}

fn syntax_retained_heap_bytes(bytes: &[u8]) -> u64 {
    let input = SyntaxInput::new(identity(), 0, bytes, InputExtent::KnownSourceEnd).unwrap();
    let mut parser = SyntaxParser::new(input, SyntaxLimits::default()).unwrap();
    assert!(matches!(parser.parse_object(), SyntaxPoll::Ready(_)));
    parser
        .stats()
        .owned_bytes()
        .checked_add(parser.stats().container_bytes())
        .expect("small test syntax footprint fits u64")
}

#[test]
fn canonical_direct_objects_validate_their_exact_headers_and_spans() {
    let pdf = canonical_pdf();
    for (index, offset) in OBJECT_OFFSETS.into_iter().take(3).enumerate() {
        let number = u32::try_from(index + 1).unwrap();
        let (object, _) = ready_at(&pdf, object_ref(number, 0), offset, XREF_OFFSET);
        assert_eq!(object.snapshot(), snapshot(PDF_LEN));
        assert_eq!(object.reference(), object_ref(number, 0));
        assert_eq!(object.revision_startxref(), XREF_OFFSET);
        assert_eq!(object.xref_offset(), offset);
        assert_eq!(object.header_span().start(), offset);
        assert!(object.header_span().end_exclusive() < object.endobj_span().start());
        assert_eq!(object.object_span().start(), offset);
        assert_eq!(
            object.object_span().end_exclusive(),
            object.endobj_span().end_exclusive()
        );
        assert!(matches!(
            object.value(),
            IndirectObjectValue::Direct(value)
                if matches!(value.value(), SyntaxObject::Dictionary(_))
        ));
    }
}

#[test]
fn canonical_stream_returns_exact_unread_payload_and_terminal_geometry() {
    let pdf = canonical_pdf();
    let (object, mut open) = ready_at(&pdf, object_ref(4, 0), 396, XREF_OFFSET);
    assert_eq!(
        (
            object.header_span().start(),
            object.header_span().end_exclusive()
        ),
        (396, 403)
    );
    assert_eq!(
        (
            object.object_span().start(),
            object.object_span().end_exclusive()
        ),
        (396, 448)
    );
    assert_eq!(
        (
            object.endobj_span().start(),
            object.endobj_span().end_exclusive()
        ),
        (442, 448)
    );

    let IndirectObjectValue::Stream(stream) = object.value() else {
        panic!("object four must be a stream")
    };
    assert_eq!(
        (
            stream.dictionary().span().start(),
            stream.dictionary().span().end_exclusive()
        ),
        (404, 419)
    );
    assert_eq!(
        (
            stream.length_value_span().start(),
            stream.length_value_span().end_exclusive()
        ),
        (415, 416)
    );
    assert_eq!(
        (
            stream.stream_keyword_span().start(),
            stream.stream_keyword_span().end_exclusive()
        ),
        (420, 426)
    );
    assert_eq!(
        (
            stream.stream_line_ending_span().start(),
            stream.stream_line_ending_span().end_exclusive()
        ),
        (426, 427)
    );
    assert_eq!(
        (
            stream.data_span().start(),
            stream.data_span().end_exclusive()
        ),
        (427, 431)
    );
    assert_eq!(&pdf[427..431], b"q\nQ\n");
    assert_eq!(
        (
            stream.data_delimiter_span().start(),
            stream.data_delimiter_span().end_exclusive()
        ),
        (431, 432)
    );
    assert_eq!(
        (
            stream.endstream_span().start(),
            stream.endstream_span().end_exclusive()
        ),
        (432, 441)
    );
    assert_eq!(open.phase(), ObjectPhase::Complete);
    assert_eq!(open.stats().declared_stream_bytes(), 4);
    assert_eq!(
        match open.poll(&supplied_store(&pdf), &NeverCancelled) {
            ObjectPoll::Failed(error) => error.code(),
            _ => panic!("a completed job must reject a second poll"),
        },
        ObjectErrorCode::JobAlreadyComplete
    );
    assert_eq!(open.phase(), ObjectPhase::Complete);
}

#[test]
fn successful_envelopes_report_exact_retained_syntax_heap_without_payload_bytes() {
    let (scalar, scalar_startxref) = standalone(b"1 0 obj\n42\nendobj\n");
    let (scalar, scalar_job) = ready_at(&scalar, object_ref(1, 0), 0, scalar_startxref);
    assert_eq!(scalar.retained_heap_bytes(), 0);
    assert_eq!(scalar_job.stats().retained_heap_bytes(), 0);

    let direct_syntax = b"<< /A (abc) /B [1 2 3] >>";
    let direct_body = b"1 0 obj\n<< /A (abc) /B [1 2 3] >>\nendobj\n";
    let (direct, direct_startxref) = standalone(direct_body);
    let (direct, direct_job) = ready_at(&direct, object_ref(1, 0), 0, direct_startxref);
    let direct_expected = syntax_retained_heap_bytes(direct_syntax);
    assert!(direct_expected > 0);
    assert_eq!(direct.retained_heap_bytes(), direct_expected);
    assert_eq!(direct_job.stats().retained_heap_bytes(), direct_expected);

    let payload = vec![b'x'; 4096];
    let stream_syntax = b"<< /Length 4096 /Meta [1 2] >>";
    let stream_body = stream_body(stream_syntax, &payload, b"\n");
    let (stream, stream_startxref) = standalone(&stream_body);
    let (stream, stream_job) = ready_at(&stream, object_ref(1, 0), 0, stream_startxref);
    let stream_expected = syntax_retained_heap_bytes(stream_syntax);
    assert!(stream_expected > 0);
    assert!(stream_expected < u64::try_from(payload.len()).unwrap());
    assert_eq!(stream.retained_heap_bytes(), stream_expected);
    assert_eq!(stream_job.stats().retained_heap_bytes(), stream_expected);
    assert_eq!(
        stream_job.stats().declared_stream_bytes(),
        u64::try_from(payload.len()).unwrap()
    );
}

#[test]
fn discarded_envelope_retries_do_not_accumulate_retained_heap_capacity() {
    let syntax = b"<< /A (abc) /B [1 2 3] /C << /D /Name >> >>";
    let body = b"1 0 obj\n<< /A (abc) /B [1 2 3] /C << /D /Name >> >>\nendobj\n";
    let (bytes, startxref) = standalone(body);
    let source = snapshot(u64::try_from(bytes.len()).unwrap());
    let store = supplied_store(&bytes);
    let max_envelope_bytes = startxref;
    let limits = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source.len().unwrap(),
        initial_envelope_bytes: 8,
        max_envelope_bytes,
        initial_boundary_bytes: 1,
        max_boundary_bytes: 1,
        max_stream_bytes: 1,
        max_total_read_bytes: 1024,
        max_total_parse_bytes: 1024,
    })
    .unwrap();
    let mut open = job(
        target(store.snapshot(), object_ref(1, 0), 0, startxref),
        limits,
    );
    let object = match open.poll(&store, &NeverCancelled) {
        ObjectPoll::Ready(object) => object,
        ObjectPoll::Pending { .. } => panic!("fully supplied retry fixture must not suspend"),
        ObjectPoll::Failed(error) => panic!("retry fixture must frame: {error}"),
    };
    assert!(open.stats().envelope_attempts() > 1);
    let expected = syntax_retained_heap_bytes(syntax);
    assert_eq!(object.retained_heap_bytes(), expected);
    assert_eq!(open.stats().retained_heap_bytes(), expected);
}

#[test]
fn direct_scalar_array_and_dictionary_values_are_supported() {
    for (body, expected) in [
        (b"1 0 obj\n42\nendobj\n".as_slice(), "integer"),
        (b"1 0 obj\n[1 true null]\nendobj\n".as_slice(), "array"),
        (
            b"1 0 obj\n<< /Answer 42 >>\nendobj\n".as_slice(),
            "dictionary",
        ),
    ] {
        let (bytes, startxref) = standalone(body);
        let (object, _) = ready_at(&bytes, object_ref(1, 0), 0, startxref);
        let IndirectObjectValue::Direct(value) = object.value() else {
            panic!("direct fixture became a stream")
        };
        assert!(matches!(
            (expected, value.value()),
            ("integer", SyntaxObject::Integer(42))
                | ("array", SyntaxObject::Array(_))
                | ("dictionary", SyntaxObject::Dictionary(_))
        ));
    }
}

struct RecordingSource<'a> {
    store: &'a RangeStore,
    requests: Mutex<Vec<ReadRequest>>,
}

impl ByteSource for RecordingSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.store.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<pdf_rs_bytes::ByteSlice> {
        self.requests.lock().unwrap().push(request);
        self.store.poll(request)
    }
}

struct UnexpectedEofSource(SourceSnapshot);

impl ByteSource for UnexpectedEofSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<pdf_rs_bytes::ByteSlice> {
        ReadPoll::EndOfFile
    }
}

#[test]
fn physical_object_bounds_stop_framing_before_later_bytes_are_requested() {
    let mut direct = b"1 0 obj\n[null\n".to_vec();
    let direct_upper = u64::try_from(direct.len()).unwrap();
    direct.extend_from_slice(b"2 0 obj\nnull\nendobj\n]\nendobj\n");
    let direct_startxref = u64::try_from(direct.len()).unwrap();
    direct.extend_from_slice(b"xref\n");

    let direct_store = supplied_store(&direct);
    let direct_source = RecordingSource {
        store: &direct_store,
        requests: Mutex::new(Vec::new()),
    };
    let direct_target = bounded_target(
        direct_store.snapshot(),
        object_ref(1, 0),
        0,
        direct_upper,
        direct_startxref,
    );
    assert_eq!(direct_target.object_upper_bound(), direct_upper);
    assert_eq!(direct_target.revision_startxref(), direct_startxref);
    let mut direct_job = job(direct_target, ObjectLimits::default());
    let direct_error = match direct_job.poll(&direct_source, &NeverCancelled) {
        ObjectPoll::Failed(error) => error,
        other => panic!("an unterminated first interval must fail, got {other:?}"),
    };
    assert_eq!(
        direct_error.code(),
        ObjectErrorCode::ObjectCrossesPhysicalBound
    );
    assert_eq!(direct_error.offset(), Some(direct_upper));
    assert!(
        direct_source
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.range().end_exclusive() <= direct_upper)
    );

    let mut stream = b"1 0 obj\n<< /Length 4 >>\nstream\nABCD".to_vec();
    let stream_upper = u64::try_from(stream.len()).unwrap();
    stream.extend_from_slice(b"\nendstream\nendobj\n");
    let stream_startxref = u64::try_from(stream.len()).unwrap();
    stream.extend_from_slice(b"xref\n");

    let stream_store = supplied_store(&stream);
    let stream_source = RecordingSource {
        store: &stream_store,
        requests: Mutex::new(Vec::new()),
    };
    let mut stream_job = job(
        bounded_target(
            stream_store.snapshot(),
            object_ref(1, 0),
            0,
            stream_upper,
            stream_startxref,
        ),
        ObjectLimits::default(),
    );
    let stream_error = match stream_job.poll(&stream_source, &NeverCancelled) {
        ObjectPoll::Failed(error) => error,
        other => panic!("a stream ending at its physical bound must fail, got {other:?}"),
    };
    assert_eq!(
        stream_error.code(),
        ObjectErrorCode::ObjectCrossesPhysicalBound
    );
    assert_eq!(stream_error.offset(), Some(stream_upper));
    assert_eq!(stream_job.stats().boundary_attempts(), 0);
    assert!(
        stream_source
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.range().end_exclusive() <= stream_upper)
    );

    let mut terminal = b"1 0 obj\n<< /Length 4 >>\nstream\nABCD\nendst".to_vec();
    let terminal_upper = u64::try_from(terminal.len()).unwrap();
    terminal.extend_from_slice(b"ream\nendobj\n");
    let terminal_startxref = u64::try_from(terminal.len()).unwrap();
    terminal.extend_from_slice(b"xref\n");

    let terminal_store = supplied_store(&terminal);
    let terminal_source = RecordingSource {
        store: &terminal_store,
        requests: Mutex::new(Vec::new()),
    };
    let mut terminal_job = job(
        bounded_target(
            terminal_store.snapshot(),
            object_ref(1, 0),
            0,
            terminal_upper,
            terminal_startxref,
        ),
        ObjectLimits::default(),
    );
    let terminal_error = match terminal_job.poll(&terminal_source, &NeverCancelled) {
        ObjectPoll::Failed(error) => error,
        other => panic!("a split stream terminal must fail at its bound, got {other:?}"),
    };
    assert_eq!(
        terminal_error.code(),
        ObjectErrorCode::ObjectCrossesPhysicalBound
    );
    assert_eq!(terminal_error.offset(), Some(terminal_upper));
    assert_eq!(terminal_job.stats().boundary_attempts(), 1);
    assert!(
        terminal_source
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.range().end_exclusive() <= terminal_upper)
    );

    let legal_body = b"1 0 obj\nnull\nendobj\n";
    let legal_upper = u64::try_from(legal_body.len()).unwrap();
    let mut legal = legal_body.to_vec();
    legal.extend_from_slice(b"unused bytes before xref\n");
    let legal_startxref = u64::try_from(legal.len()).unwrap();
    legal.extend_from_slice(b"xref\n");

    let legal_store = supplied_store(&legal);
    let legal_source = RecordingSource {
        store: &legal_store,
        requests: Mutex::new(Vec::new()),
    };
    let mut legal_job = job(
        bounded_target(
            legal_store.snapshot(),
            object_ref(1, 0),
            0,
            legal_upper,
            legal_startxref,
        ),
        ObjectLimits::default(),
    );
    let object = match legal_job.poll(&legal_source, &NeverCancelled) {
        ObjectPoll::Ready(object) => object,
        other => panic!("a complete bounded object must frame, got {other:?}"),
    };
    assert_eq!(object.object_upper_bound(), legal_upper);
    assert_eq!(object.revision_startxref(), legal_startxref);
    assert!(object.object_span().end_exclusive() <= legal_upper);
    assert!(
        legal_source
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.range().end_exclusive() <= legal_upper)
    );
}

#[test]
fn in_range_source_eof_is_not_misclassified_as_malformed_object_syntax() {
    let source = snapshot(64);
    let target = target(source, object_ref(1, 0), 0, 32);
    let mut open = job(target, ObjectLimits::default());
    let error = match open.poll(&UnexpectedEofSource(source), &NeverCancelled) {
        ObjectPoll::Failed(error) => error,
        _ => panic!("an unexpected in-range EOF must be terminal"),
    };
    assert_eq!(error.code(), ObjectErrorCode::UnexpectedEndOfSource);
    assert_eq!(error.category(), ObjectErrorCategory::Source);
    assert_eq!(error.recoverability(), ObjectRecoverability::ReopenSource);
    assert_eq!(error.reference(), Some(object_ref(1, 0)));
    assert_eq!(error.offset(), Some(0));
}

#[test]
fn disconnected_envelope_and_boundary_reads_frame_a_large_missing_payload() {
    let prefix = b"%\n";
    let mut payload = vec![b'P'; 8192];
    payload[127..146].copy_from_slice(b"endstream\nendobj\nXX");
    let body = stream_body(b"<< /Length 8192 >>", &payload, b"\n");
    let mut bytes = prefix.to_vec();
    let offset = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&body);
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"xref\n");
    let data_start =
        offset + u64::try_from(b"1 0 obj\n<< /Length 8192 >>\nstream\n".len()).unwrap();
    let data_end = data_start + u64::try_from(payload.len()).unwrap();
    let envelope_start = offset - 1;
    let envelope_len = data_start - envelope_start;
    let boundary_len = startxref - data_end;
    let source = snapshot(u64::try_from(bytes.len()).unwrap());
    let store = RangeStore::new(source, Default::default()).unwrap();
    let recording = RecordingSource {
        store: &store,
        requests: Mutex::new(Vec::new()),
    };
    let limits = compact_limits(
        u64::try_from(bytes.len()).unwrap(),
        envelope_len,
        boundary_len,
        u64::try_from(payload.len()).unwrap(),
    );
    let mut open = job(target(source, object_ref(1, 0), offset, startxref), limits);

    let (envelope_ticket, envelope_range) = match open.poll(&recording, &NeverCancelled) {
        ObjectPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(checkpoint, ResumeCheckpoint::new(80));
            assert_eq!(missing.len(), 1);
            (ticket, missing.as_slice()[0])
        }
        _ => panic!("an empty store must suspend on the envelope"),
    };
    assert_eq!(
        envelope_range,
        ByteRange::new(envelope_start, envelope_len).unwrap()
    );
    let stats = open.stats();
    assert_eq!(stats.retained_heap_bytes(), 0);
    match open.poll(&recording, &NeverCancelled) {
        ObjectPoll::Pending {
            ticket, missing, ..
        } => {
            assert_eq!(ticket, envelope_ticket);
            assert_eq!(missing.as_slice(), &[envelope_range]);
        }
        _ => panic!("re-polling missing bytes must preserve the ticket"),
    }
    assert_eq!(open.stats(), stats);

    let envelope_start_usize = usize::try_from(envelope_range.start()).unwrap();
    let envelope_end_usize = usize::try_from(envelope_range.end_exclusive()).unwrap();
    store
        .supply(
            RangeResponse::new(
                source,
                envelope_range,
                bytes[envelope_start_usize..envelope_end_usize].to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    let (boundary_ticket, boundary_range) = match open.poll(&recording, &NeverCancelled) {
        ObjectPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(checkpoint, ResumeCheckpoint::new(81));
            assert_eq!(open.phase(), ObjectPhase::StreamBoundary);
            (ticket, missing.as_slice()[0])
        }
        _ => panic!("the disconnected terminal boundary must suspend separately"),
    };
    assert_eq!(
        boundary_range,
        ByteRange::new(data_end, boundary_len).unwrap()
    );
    assert!(envelope_range.end_exclusive() <= data_start);
    assert!(boundary_range.start() >= data_end);
    let boundary_stats = open.stats();
    let retained_heap_bytes = syntax_retained_heap_bytes(b"<< /Length 8192 >>");
    assert!(retained_heap_bytes > 0);
    assert_eq!(boundary_stats.retained_heap_bytes(), retained_heap_bytes);
    match open.poll(&recording, &NeverCancelled) {
        ObjectPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(ticket, boundary_ticket);
            assert_eq!(missing.as_slice(), &[boundary_range]);
            assert_eq!(checkpoint, ResumeCheckpoint::new(81));
        }
        _ => panic!("re-polling the missing boundary must preserve its ticket"),
    }
    assert_eq!(open.stats(), boundary_stats);

    let data_end_usize = usize::try_from(data_end).unwrap();
    let startxref_usize = usize::try_from(startxref).unwrap();
    store
        .supply(
            RangeResponse::new(
                source,
                boundary_range,
                bytes[data_end_usize..startxref_usize].to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    let object = match open.poll(&recording, &NeverCancelled) {
        ObjectPoll::Ready(object) => object,
        other => panic!("two supplied framing ranges must finish the job: {other:?}"),
    };
    let IndirectObjectValue::Stream(stream) = object.value() else {
        panic!("large fixture must remain a stream")
    };
    assert_eq!(stream.data_span().start(), data_start);
    assert_eq!(stream.data_span().len(), 8192);
    assert_eq!(object.retained_heap_bytes(), retained_heap_bytes);
    assert_eq!(open.stats().retained_heap_bytes(), retained_heap_bytes);
    assert_eq!(open.stats().envelope_attempts(), 1);
    assert_eq!(open.stats().boundary_attempts(), 1);
    assert_eq!(open.stats().read_bytes(), envelope_len + boundary_len);
    assert_eq!(open.stats().parse_bytes(), envelope_len + boundary_len);
    assert_eq!(open.stats().declared_stream_bytes(), 8192);
    for request in recording.requests.lock().unwrap().iter() {
        assert_eq!(request.priority(), RequestPriority::VisiblePage);
        assert!(request.range() == envelope_range || request.range() == boundary_range);
    }
}

#[test]
fn cancellation_at_the_boundary_phase_is_terminal_and_repoll_is_stable() {
    let payload = vec![b'Z'; 16];
    let body = stream_body(b"<< /Length 16 >>", &payload, b"\n");
    let (bytes, startxref) = standalone(&body);
    let data_start = u64::try_from(b"1 0 obj\n<< /Length 16 >>\nstream\n".len()).unwrap();
    let data_end = data_start + 16;
    let envelope_len = data_start;
    let boundary_len = startxref - data_end;
    let source = snapshot(u64::try_from(bytes.len()).unwrap());
    let store = RangeStore::new(source, Default::default()).unwrap();
    let envelope_range = ByteRange::new(0, envelope_len).unwrap();
    store
        .supply(
            RangeResponse::new(
                source,
                envelope_range,
                bytes[..usize::try_from(envelope_len).unwrap()].to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    let limits = compact_limits(
        u64::try_from(bytes.len()).unwrap(),
        envelope_len,
        boundary_len,
        16,
    );
    let mut open = job(target(source, object_ref(1, 0), 0, startxref), limits);
    assert!(matches!(
        open.poll(&store, &NeverCancelled),
        ObjectPoll::Pending {
            checkpoint,
            ..
        } if checkpoint == ResumeCheckpoint::new(81)
    ));
    assert_eq!(open.phase(), ObjectPhase::StreamBoundary);
    let retained_heap_bytes = syntax_retained_heap_bytes(b"<< /Length 16 >>");
    assert_eq!(open.stats().retained_heap_bytes(), retained_heap_bytes);

    let flag = AtomicBool::new(true);
    let error = match open.poll(&store, &flag) {
        ObjectPoll::Failed(error) => error,
        _ => panic!("boundary-phase cancellation must be terminal"),
    };
    assert_eq!(error.code(), ObjectErrorCode::Cancelled);
    assert_eq!(open.phase(), ObjectPhase::Failed);
    assert_eq!(open.stats().retained_heap_bytes(), retained_heap_bytes);
    assert_eq!(
        open.poll(&store, &NeverCancelled),
        ObjectPoll::Failed(error)
    );
}

#[test]
fn exact_header_checks_reject_number_generation_whitespace_and_token_middle_offsets() {
    let (number_mismatch, number_xref) = standalone(b"1 0 obj\nnull\nendobj\n");
    assert_eq!(
        failed_at(
            &number_mismatch,
            object_ref(2, 0),
            0,
            number_xref,
            ObjectLimits::default(),
        )
        .code(),
        ObjectErrorCode::InvalidObjectHeader
    );

    let (generation_mismatch, generation_xref) = standalone(b"1 1 obj\nnull\nendobj\n");
    assert_eq!(
        failed_at(
            &generation_mismatch,
            object_ref(1, 0),
            0,
            generation_xref,
            ObjectLimits::default(),
        )
        .code(),
        ObjectErrorCode::InvalidObjectHeader
    );

    let (leading_space, leading_xref) = standalone(b" 1 0 obj\nnull\nendobj\n");
    assert_eq!(
        failed_at(
            &leading_space,
            object_ref(1, 0),
            0,
            leading_xref,
            ObjectLimits::default(),
        )
        .code(),
        ObjectErrorCode::InvalidObjectHeader
    );

    let (token_middle, middle_xref) = standalone(b"11 0 obj\nnull\nendobj\n");
    assert_eq!(
        failed_at(
            &token_middle,
            object_ref(1, 0),
            1,
            middle_xref,
            ObjectLimits::default(),
        )
        .code(),
        ObjectErrorCode::InvalidObjectHeader
    );

    for prefix in [b'%', b'/', b'(', b'<', b'[', b'{', b')', b'>', b']', b'}'] {
        let mut embedded = vec![prefix];
        embedded.extend_from_slice(b"1 0 obj\nnull\nendobj\n");
        let startxref = u64::try_from(embedded.len()).unwrap();
        embedded.extend_from_slice(b"xref\n");
        assert_eq!(
            failed_at(
                &embedded,
                object_ref(1, 0),
                1,
                startxref,
                ObjectLimits::default(),
            )
            .code(),
            ObjectErrorCode::InvalidObjectHeader
        );
    }
}

#[test]
fn strict_open_never_searches_for_a_nearby_object_header() {
    let (bytes, startxref) = standalone(b" 1 0 obj\nnull\nendobj\n");
    let error = failed_at(
        &bytes,
        object_ref(1, 0),
        0,
        startxref,
        ObjectLimits::default(),
    );
    assert_eq!(error.code(), ObjectErrorCode::InvalidObjectHeader);
    assert_eq!(error.offset(), Some(1));

    let (object, _) = ready_at(&bytes, object_ref(1, 0), 1, startxref);
    assert_eq!(object.reference(), object_ref(1, 0));
    assert_eq!(object.object_span().start(), 1);
}

#[test]
fn object_envelope_rejects_wrong_keywords_and_non_dictionary_streams() {
    for (body, expected) in [
        (
            b"1 0 obx\nnull\nendobj\n".as_slice(),
            ObjectErrorCode::InvalidObjectHeader,
        ),
        (
            b"1 0 obj\nnull\nfinish\n".as_slice(),
            ObjectErrorCode::InvalidObjectEnvelope,
        ),
        (
            b"1 0 obj\n42\nstream\n\nendstream\nendobj\n".as_slice(),
            ObjectErrorCode::InvalidObjectEnvelope,
        ),
    ] {
        let (bytes, startxref) = standalone(body);
        assert_eq!(
            failed_at(
                &bytes,
                object_ref(1, 0),
                0,
                startxref,
                ObjectLimits::default(),
            )
            .code(),
            expected
        );
    }
}

#[test]
fn physical_object_boundary_never_masquerades_as_a_token_delimiter() {
    for body in [
        b"1 0 obj\nnull\nendobj".as_slice(),
        b"1 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj".as_slice(),
    ] {
        let (bytes, startxref) = standalone(body);
        let error = failed_at(
            &bytes,
            object_ref(1, 0),
            0,
            startxref,
            ObjectLimits::default(),
        );
        assert_eq!(error.code(), ObjectErrorCode::ObjectCrossesPhysicalBound);
        assert_eq!(error.category(), ObjectErrorCategory::Syntax);
    }
}

#[test]
fn stream_length_policy_distinguishes_malformed_unsupported_and_resource_cases() {
    for (dictionary, expected) in [
        (b"<< >>".as_slice(), ObjectErrorCode::MissingStreamLength),
        (
            b"<< /Length 0 /Length 0 >>".as_slice(),
            ObjectErrorCode::DuplicateStreamLength,
        ),
        (
            b"<< /Length -1 >>".as_slice(),
            ObjectErrorCode::InvalidStreamLength,
        ),
        (
            b"<< /Length 1.5 >>".as_slice(),
            ObjectErrorCode::InvalidStreamLength,
        ),
        (
            b"<< /Length /four >>".as_slice(),
            ObjectErrorCode::InvalidStreamLength,
        ),
        (
            b"<< /Length 2 0 R >>".as_slice(),
            ObjectErrorCode::UnsupportedIndirectLength,
        ),
    ] {
        let body = stream_body(dictionary, b"", b"\n");
        let (bytes, startxref) = standalone(&body);
        assert_eq!(
            failed_at(
                &bytes,
                object_ref(1, 0),
                0,
                startxref,
                ObjectLimits::default(),
            )
            .code(),
            expected
        );
    }

    let body = stream_body(b"<< /Length 5 >>", b"12345", b"\n");
    let (bytes, startxref) = standalone(&body);
    let limits = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: 1024,
        initial_envelope_bytes: 64,
        max_envelope_bytes: 128,
        initial_boundary_bytes: 16,
        max_boundary_bytes: 32,
        max_stream_bytes: 4,
        max_total_read_bytes: 512,
        max_total_parse_bytes: 512,
    })
    .unwrap();
    let error = failed_at(&bytes, object_ref(1, 0), 0, startxref, limits);
    assert_eq!(error.code(), ObjectErrorCode::ResourceLimit);
    assert_eq!(error.limit().unwrap().kind(), ObjectLimitKind::StreamBytes);

    let (bytes, startxref) = standalone(b"1 0 obj\n<< /Length 2 0 R >>\nstream\r");
    assert_eq!(
        failed_at(
            &bytes,
            object_ref(1, 0),
            0,
            startxref,
            ObjectLimits::default(),
        )
        .code(),
        ObjectErrorCode::UnsupportedIndirectLength,
        "legacy direct-only framing must reject indirect Length before stream-line validation"
    );
}

#[test]
fn exact_stream_boundary_rejects_wrong_lengths_trivia_and_keywords_without_scanning() {
    for body in [
        stream_body(b"<< /Length 2 >>", b"ABC", b"\n"),
        stream_body(b"<< /Length 4 >>", b"ABC", b"\n"),
        stream_body(b"<< /Length 3 >>", b"ABC", b"\n \n"),
        {
            let mut value = stream_body(b"<< /Length 3 >>", b"ABC", b"\n");
            let position = value
                .windows(b"endstream".len())
                .position(|window| window == b"endstream")
                .unwrap();
            value.splice(
                position..position + b"endstream".len(),
                b"endstreamx".iter().copied(),
            );
            value
        },
    ] {
        let (bytes, startxref) = standalone(&body);
        assert_eq!(
            failed_at(
                &bytes,
                object_ref(1, 0),
                0,
                startxref,
                ObjectLimits::default(),
            )
            .code(),
            ObjectErrorCode::InvalidStreamBoundary
        );
    }
}

#[test]
fn strict_open_never_searches_for_a_nearby_stream_boundary() {
    let wrong = stream_body(b"<< /Length 2 >>", b"ABC", b"\n");
    let (wrong_bytes, wrong_startxref) = standalone(&wrong);
    let error = failed_at(
        &wrong_bytes,
        object_ref(1, 0),
        0,
        wrong_startxref,
        ObjectLimits::default(),
    );
    assert_eq!(error.code(), ObjectErrorCode::InvalidStreamBoundary);

    let corrected = stream_body(b"<< /Length 3 >>", b"ABC", b"\n");
    let (corrected_bytes, corrected_startxref) = standalone(&corrected);
    let (object, _) = ready_at(&corrected_bytes, object_ref(1, 0), 0, corrected_startxref);
    assert!(matches!(object.value(), IndirectObjectValue::Stream(_)));
}

#[test]
fn stream_line_endings_accept_lf_and_crlf_but_reject_bare_cr() {
    let lf = stream_body(b"<< /Length 0 >>", b"", b"\n");
    let (bytes, startxref) = standalone(&lf);
    assert!(matches!(
        ready_at(&bytes, object_ref(1, 0), 0, startxref).0.value(),
        IndirectObjectValue::Stream(_)
    ));

    let mut crlf = b"1 0 obj\n<< /Length 0 >>\nstream\r\n".to_vec();
    crlf.extend_from_slice(b"\r\nendstream\nendobj\n");
    let (bytes, startxref) = standalone(&crlf);
    assert!(matches!(
        ready_at(&bytes, object_ref(1, 0), 0, startxref).0.value(),
        IndirectObjectValue::Stream(_)
    ));

    let mut bare_cr = b"1 0 obj\n<< /Length 0 >>\nstream\r".to_vec();
    bare_cr.extend_from_slice(b"\rendstream\nendobj\n");
    let (bytes, startxref) = standalone(&bare_cr);
    assert_eq!(
        failed_at(
            &bytes,
            object_ref(1, 0),
            0,
            startxref,
            ObjectLimits::default(),
        )
        .code(),
        ObjectErrorCode::InvalidStreamBoundary
    );
}

#[test]
fn target_snapshot_cancellation_and_job_context_are_terminal_and_classified() {
    let unknown = SourceSnapshot::new(
        identity(),
        None,
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x27; 32]),
    );
    assert_eq!(
        IndirectObjectTarget::new(unknown, object_ref(1, 0), 0, 10, 10)
            .unwrap_err()
            .code(),
        ObjectErrorCode::UnknownSourceLength
    );
    assert_eq!(
        IndirectObjectTarget::new(snapshot(20), object_ref(1, 0), 10, 10, 10)
            .unwrap_err()
            .code(),
        ObjectErrorCode::InvalidTarget
    );
    assert_eq!(
        IndirectObjectTarget::new(snapshot(20), object_ref(1, 0), 1, 19, 21)
            .unwrap_err()
            .code(),
        ObjectErrorCode::InvalidTarget
    );
    assert_eq!(
        IndirectObjectTarget::new(snapshot(20), object_ref(1, 0), 1, 19, 20)
            .unwrap_err()
            .code(),
        ObjectErrorCode::InvalidTarget
    );
    assert_eq!(
        IndirectObjectTarget::new(snapshot(20), object_ref(1, 0), 1, 19, 18)
            .unwrap_err()
            .code(),
        ObjectErrorCode::InvalidTarget
    );

    let (bytes, startxref) = standalone(b"1 0 obj\nnull\nendobj\n");
    let store = supplied_store(&bytes);
    let target = target(store.snapshot(), object_ref(1, 0), 0, startxref);
    let equal_context = ObjectJobContext::new(
        JobId::new(1),
        ResumeCheckpoint::new(2),
        ResumeCheckpoint::new(2),
        RequestPriority::Metadata,
    );
    assert_eq!(
        OpenObjectJob::new(
            target,
            equal_context,
            ObjectLimits::default(),
            SyntaxLimits::default(),
        )
        .unwrap_err()
        .code(),
        ObjectErrorCode::InvalidJobContext
    );

    let mut cancelled = job(target, ObjectLimits::default());
    let flag = AtomicBool::new(true);
    let error = match cancelled.poll(&store, &flag) {
        ObjectPoll::Failed(error) => error,
        _ => panic!("pre-cancelled job must fail"),
    };
    assert_eq!(error.code(), ObjectErrorCode::Cancelled);
    assert_eq!(error.category(), ObjectErrorCategory::Cancellation);
    assert_eq!(cancelled.phase(), ObjectPhase::Failed);
    assert_eq!(
        cancelled.poll(&store, &NeverCancelled),
        ObjectPoll::Failed(error)
    );

    let mismatched_snapshot = SourceSnapshot::new(
        identity(),
        Some(u64::try_from(bytes.len()).unwrap()),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x99; 32]),
    );
    let mismatched_store = RangeStore::new(mismatched_snapshot, Default::default()).unwrap();
    let mut mismatched = job(target, ObjectLimits::default());
    assert_eq!(
        match mismatched.poll(&mismatched_store, &NeverCancelled) {
            ObjectPoll::Failed(error) => error.code(),
            _ => panic!("full snapshot mismatch must be terminal"),
        },
        ObjectErrorCode::SnapshotMismatch
    );
}

#[test]
fn cumulative_retry_windows_enforce_read_and_parse_budgets() {
    let (bytes, startxref) = standalone(b"1 0 obj\n<< /LongName (value) >>\nendobj\n");
    for (read_budget, parse_budget, expected) in [
        (17, 64, ObjectLimitKind::TotalReadBytes),
        (64, 17, ObjectLimitKind::TotalParseBytes),
    ] {
        let limits = ObjectLimits::validate(ObjectLimitConfig {
            max_source_bytes: 128,
            initial_envelope_bytes: 4,
            max_envelope_bytes: 16,
            initial_boundary_bytes: 1,
            max_boundary_bytes: 1,
            max_stream_bytes: 1,
            max_total_read_bytes: read_budget,
            max_total_parse_bytes: parse_budget,
        })
        .unwrap();
        let error = failed_at(&bytes, object_ref(1, 0), 0, startxref, limits);
        assert_eq!(error.code(), ObjectErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), expected);
    }
}

#[test]
fn diagnostics_and_debug_output_do_not_expose_object_or_stream_content() {
    let secret = "object-secret-needle";
    let body = format!("1 0 obj\n({secret})\nfinish\n");
    let (bytes, startxref) = standalone(body.as_bytes());
    let error = failed_at(
        &bytes,
        object_ref(1, 0),
        0,
        startxref,
        ObjectLimits::default(),
    );
    assert!(!format!("{error}").contains(secret));
    assert!(!format!("{error:?}").contains(secret));

    let stream_body = stream_body(b"<< /Length 20 >>", secret.as_bytes(), b"\n");
    let (bytes, startxref) = standalone(&stream_body);
    let (object, open) = ready_at(&bytes, object_ref(1, 0), 0, startxref);
    assert!(!format!("{object:?}").contains(secret));
    assert!(!format!("{open:?}").contains(secret));
}
