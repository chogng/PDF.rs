use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_object::{
    IndirectObjectTarget, NeverCancelled, ObjectError, ObjectErrorCategory, ObjectErrorCode,
    ObjectJobContext, ObjectLimitConfig, ObjectLimitKind, ObjectLimits, ObjectPoll,
    ObjectRecoverability, OpenObjectJob,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};

const DIRECT_BODY: &[u8] = b"1 0 obj\n(null)\nendobj\n";
const STREAM_PAYLOAD: &[u8] = b"ABC";
const EXPECTED_DIRECT_RETRY_BYTES: u64 = 53;

fn identity() -> SourceIdentity {
    SourceIdentity::new(SourceStableId::new([0x73; 32]), SourceRevision::new(17))
}

fn snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        identity(),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x31; 32]),
    )
}

fn reference() -> ObjectRef {
    ObjectRef::new(1, 0).expect("the matrix fixture uses a valid indirect reference")
}

fn context() -> ObjectJobContext {
    ObjectJobContext::new(
        JobId::new(73),
        ResumeCheckpoint::new(91),
        ResumeCheckpoint::new(92),
        RequestPriority::VisiblePage,
    )
}

fn fixture(body: &[u8]) -> (Vec<u8>, u64) {
    let mut bytes = body.to_vec();
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"xref\n");
    (bytes, startxref)
}

fn stream_body(data_delimiter: &[u8]) -> Vec<u8> {
    let mut body = b"1 0 obj\n<< /Length 3 >>\nstream\r\n".to_vec();
    body.extend_from_slice(STREAM_PAYLOAD);
    body.extend_from_slice(data_delimiter);
    body.extend_from_slice(b"endstream\nendobj\n");
    body
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

fn limits(config: ObjectLimitConfig) -> ObjectLimits {
    ObjectLimits::validate(config).expect("matrix limit profile must be internally consistent")
}

fn config(
    max_source_bytes: u64,
    initial_envelope_bytes: u64,
    max_envelope_bytes: u64,
    initial_boundary_bytes: u64,
    max_boundary_bytes: u64,
    max_stream_bytes: u64,
    total_bytes: (u64, u64),
) -> ObjectLimitConfig {
    ObjectLimitConfig {
        max_source_bytes,
        initial_envelope_bytes,
        max_envelope_bytes,
        initial_boundary_bytes,
        max_boundary_bytes,
        max_stream_bytes,
        max_total_read_bytes: total_bytes.0,
        max_total_parse_bytes: total_bytes.1,
    }
}

fn open_job(
    bytes: &[u8],
    startxref: u64,
    object_limits: ObjectLimits,
) -> (RangeStore, OpenObjectJob) {
    let store = supplied_store(bytes);
    let target = IndirectObjectTarget::new(store.snapshot(), reference(), 0, startxref, startxref)
        .expect("matrix target geometry must be valid");
    let open = OpenObjectJob::new(target, context(), object_limits, SyntaxLimits::default())
        .expect("matrix job configuration must be valid");
    (store, open)
}

fn assert_ready(bytes: &[u8], startxref: u64, object_limits: ObjectLimits) -> OpenObjectJob {
    let (store, mut open) = open_job(bytes, startxref, object_limits);
    match open.poll(&store, &NeverCancelled) {
        ObjectPoll::Ready(_) => open,
        ObjectPoll::Pending { .. } => panic!("a completely supplied matrix source must not pend"),
        ObjectPoll::Failed(error) => panic!("expected a framed matrix object, got {error}"),
    }
}

fn assert_stable_failure(
    bytes: &[u8],
    startxref: u64,
    object_limits: ObjectLimits,
    expected: ObjectErrorCode,
) -> ObjectError {
    let (store, mut open) = open_job(bytes, startxref, object_limits);
    let error = match open.poll(&store, &NeverCancelled) {
        ObjectPoll::Failed(error) => error,
        ObjectPoll::Ready(_) => panic!("expected {expected:?}, got a ready object"),
        ObjectPoll::Pending { .. } => panic!("a completely supplied matrix source must not pend"),
    };
    assert_eq!(error.code(), expected, "physical-bound cut at {startxref}");
    assert_eq!(error.reference(), Some(reference()));
    if expected == ObjectErrorCode::ObjectCrossesPhysicalBound {
        assert_eq!(error.offset(), Some(startxref));
        assert_eq!(error.recoverability(), ObjectRecoverability::CorrectInput);
        assert_eq!(error.diagnostic_id(), "RPE-OBJECT-0021");
    }
    assert_eq!(
        open.poll(&store, &NeverCancelled),
        ObjectPoll::Failed(error),
        "failed re-poll at physical-bound cut {startxref}"
    );
    error
}

fn find(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("fixture marker must be present")
}

fn assert_limit(
    error: ObjectError,
    expected_kind: ObjectLimitKind,
    expected_limit: u64,
    expected_consumed: u64,
    expected_attempted: u64,
    expected_offset: Option<u64>,
) {
    assert_eq!(error.code(), ObjectErrorCode::ResourceLimit);
    assert_eq!(error.category(), ObjectErrorCategory::Resource);
    assert_eq!(error.reference(), Some(reference()));
    assert_eq!(error.offset(), expected_offset);
    let detail = error
        .limit()
        .expect("resource failures retain limit detail");
    assert_eq!(
        (
            detail.kind(),
            detail.limit(),
            detail.consumed(),
            detail.attempted(),
        ),
        (
            expected_kind,
            expected_limit,
            expected_consumed,
            expected_attempted,
        )
    );
}

#[test]
fn every_direct_envelope_split_grows_to_the_exact_complete_window() {
    let (bytes, startxref) = fixture(DIRECT_BODY);
    let source_len = u64::try_from(bytes.len()).unwrap();
    let required = u64::try_from(DIRECT_BODY.len()).unwrap();

    for initial in 1..=required {
        let open = assert_ready(
            &bytes,
            startxref,
            limits(config(
                source_len,
                initial,
                required,
                1,
                1,
                1,
                (source_len * 8, source_len * 8),
            )),
        );
        assert!(open.stats().envelope_attempts() >= 1, "split {initial}");
        assert_eq!(open.stats().boundary_attempts(), 0, "split {initial}");
    }
}

#[test]
fn every_stream_envelope_and_boundary_split_grows_without_losing_crlf() {
    let body = stream_body(b"\r\n");
    let (bytes, startxref) = fixture(&body);
    let source_len = u64::try_from(bytes.len()).unwrap();
    let data_start = u64::try_from(find(&body, b"stream\r\n") + b"stream\r\n".len()).unwrap();
    let data_end = data_start + u64::try_from(STREAM_PAYLOAD.len()).unwrap();
    let boundary_required = startxref - data_end;

    for initial_envelope in 1..=data_start {
        let open = assert_ready(
            &bytes,
            startxref,
            limits(config(
                source_len,
                initial_envelope,
                data_start,
                boundary_required,
                boundary_required,
                u64::try_from(STREAM_PAYLOAD.len()).unwrap(),
                (source_len * 8, source_len * 8),
            )),
        );
        assert_eq!(
            open.stats().boundary_attempts(),
            1,
            "split {initial_envelope}"
        );
    }

    for initial_boundary in 1..=boundary_required {
        let open = assert_ready(
            &bytes,
            startxref,
            limits(config(
                source_len,
                data_start,
                data_start,
                initial_boundary,
                boundary_required,
                u64::try_from(STREAM_PAYLOAD.len()).unwrap(),
                (source_len * 8, source_len * 8),
            )),
        );
        assert_eq!(
            open.stats().envelope_attempts(),
            1,
            "split {initial_boundary}"
        );
    }
}

#[test]
fn every_direct_physical_bound_cut_requires_the_delimiter_after_endobj() {
    let (bytes, complete_startxref) = fixture(DIRECT_BODY);
    for cut in 1..=complete_startxref {
        if cut == complete_startxref {
            assert_ready(&bytes, cut, ObjectLimits::default());
        } else {
            let error = assert_stable_failure(
                &bytes,
                cut,
                ObjectLimits::default(),
                ObjectErrorCode::ObjectCrossesPhysicalBound,
            );
            assert_eq!(error.category(), ObjectErrorCategory::Syntax);
        }
    }
}

#[test]
fn every_stream_physical_bound_cut_has_one_stable_geometry_outcome() {
    let body = stream_body(b"\r\n");
    let (bytes, complete_startxref) = fixture(&body);

    for cut in 1..=complete_startxref {
        if cut == complete_startxref {
            let open = assert_ready(&bytes, cut, ObjectLimits::default());
            assert_eq!(open.stats().declared_stream_bytes(), 3);
        } else {
            let error = assert_stable_failure(
                &bytes,
                cut,
                ObjectLimits::default(),
                ObjectErrorCode::ObjectCrossesPhysicalBound,
            );
            assert_eq!(error.category(), ObjectErrorCategory::Syntax, "cut {cut}");
        }
    }
}

#[test]
fn bare_cr_after_payload_is_a_stream_boundary_error() {
    let body = stream_body(b"\r");
    let (bytes, startxref) = fixture(&body);
    let error = assert_stable_failure(
        &bytes,
        startxref,
        ObjectLimits::default(),
        ObjectErrorCode::InvalidStreamBoundary,
    );
    let data_start = u64::try_from(find(&body, b"stream\r\n") + b"stream\r\n".len()).unwrap();
    assert_eq!(error.offset(), Some(data_start + 3));
}

#[test]
fn source_envelope_boundary_and_stream_limits_accept_exact_and_reject_one_more() {
    let (direct_bytes, direct_startxref) = fixture(DIRECT_BODY);
    let direct_source_len = u64::try_from(direct_bytes.len()).unwrap();
    let direct_required = u64::try_from(DIRECT_BODY.len()).unwrap();
    let exact_direct = config(
        direct_source_len,
        direct_required,
        direct_required,
        1,
        1,
        1,
        (direct_required + 1, direct_required + 1),
    );
    assert_ready(&direct_bytes, direct_startxref, limits(exact_direct));

    let source_error = OpenObjectJob::new(
        IndirectObjectTarget::new(
            snapshot(direct_source_len),
            reference(),
            0,
            direct_startxref,
            direct_startxref,
        )
        .unwrap(),
        context(),
        limits(config(
            direct_source_len - 1,
            direct_required,
            direct_required,
            1,
            1,
            1,
            (direct_required + 1, direct_required + 1),
        )),
        SyntaxLimits::default(),
    )
    .expect_err("source one byte above its limit must fail");
    assert_limit(
        source_error,
        ObjectLimitKind::SourceBytes,
        direct_source_len - 1,
        0,
        direct_source_len,
        None,
    );

    let envelope_error = assert_stable_failure(
        &direct_bytes,
        direct_startxref,
        limits(config(
            direct_source_len,
            direct_required - 1,
            direct_required - 1,
            1,
            1,
            1,
            (direct_required, direct_required),
        )),
        ObjectErrorCode::ResourceLimit,
    );
    assert_limit(
        envelope_error,
        ObjectLimitKind::EnvelopeBytes,
        direct_required - 1,
        direct_required - 1,
        1,
        Some(0),
    );

    let stream_body = stream_body(b"\r\n");
    let (stream_bytes, stream_startxref) = fixture(&stream_body);
    let stream_source_len = u64::try_from(stream_bytes.len()).unwrap();
    let data_start =
        u64::try_from(find(&stream_body, b"stream\r\n") + b"stream\r\n".len()).unwrap();
    let data_end = data_start + 3;
    let boundary_required = stream_startxref - data_end;
    let exact_stream = config(
        stream_source_len,
        data_start,
        data_start,
        boundary_required,
        boundary_required,
        3,
        (
            data_start + boundary_required,
            data_start + boundary_required,
        ),
    );
    assert_ready(&stream_bytes, stream_startxref, limits(exact_stream));

    let boundary_error = assert_stable_failure(
        &stream_bytes,
        stream_startxref,
        limits(config(
            stream_source_len,
            data_start,
            data_start,
            boundary_required - 1,
            boundary_required - 1,
            3,
            (
                data_start + boundary_required - 1,
                data_start + boundary_required - 1,
            ),
        )),
        ObjectErrorCode::ResourceLimit,
    );
    assert_limit(
        boundary_error,
        ObjectLimitKind::BoundaryBytes,
        boundary_required - 1,
        boundary_required - 1,
        1,
        Some(data_end),
    );

    let stream_error = assert_stable_failure(
        &stream_bytes,
        stream_startxref,
        limits(config(
            stream_source_len,
            data_start,
            data_start,
            boundary_required,
            boundary_required,
            2,
            (
                data_start + boundary_required,
                data_start + boundary_required,
            ),
        )),
        ObjectErrorCode::ResourceLimit,
    );
    let length_offset = u64::try_from(find(&stream_body, b"3 >>")).unwrap();
    assert_limit(
        stream_error,
        ObjectLimitKind::StreamBytes,
        2,
        0,
        3,
        Some(length_offset),
    );
}

#[test]
fn geometric_total_read_and_parse_limits_accept_exact_and_reject_one_less() {
    let (bytes, startxref) = fixture(DIRECT_BODY);
    let source_len = u64::try_from(bytes.len()).unwrap();
    let envelope_required = u64::try_from(DIRECT_BODY.len()).unwrap();
    let profile = |read, parse| {
        limits(config(
            source_len,
            1,
            envelope_required,
            1,
            1,
            1,
            (read, parse),
        ))
    };

    let exact = assert_ready(
        &bytes,
        startxref,
        profile(EXPECTED_DIRECT_RETRY_BYTES, EXPECTED_DIRECT_RETRY_BYTES),
    );
    assert_eq!(exact.stats().read_bytes(), EXPECTED_DIRECT_RETRY_BYTES);
    assert_eq!(exact.stats().parse_bytes(), EXPECTED_DIRECT_RETRY_BYTES);
    assert_eq!(exact.stats().envelope_attempts(), 6);

    let read_error = assert_stable_failure(
        &bytes,
        startxref,
        profile(EXPECTED_DIRECT_RETRY_BYTES - 1, EXPECTED_DIRECT_RETRY_BYTES),
        ObjectErrorCode::ResourceLimit,
    );
    assert_limit(
        read_error,
        ObjectLimitKind::TotalReadBytes,
        EXPECTED_DIRECT_RETRY_BYTES - 1,
        31,
        envelope_required,
        Some(0),
    );

    let parse_error = assert_stable_failure(
        &bytes,
        startxref,
        profile(EXPECTED_DIRECT_RETRY_BYTES, EXPECTED_DIRECT_RETRY_BYTES - 1),
        ObjectErrorCode::ResourceLimit,
    );
    assert_limit(
        parse_error,
        ObjectLimitKind::TotalParseBytes,
        EXPECTED_DIRECT_RETRY_BYTES - 1,
        31,
        envelope_required,
        Some(0),
    );
}
