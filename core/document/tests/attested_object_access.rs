use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceError, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AttestRevisionJob, AttestedObject, AttestedObjectJobContext, AttestedObjectPhase,
    AttestedObjectPoll, AttestedRevisionIndex, CandidateRevisionIndex, DocumentError,
    DocumentErrorCategory, DocumentErrorCode, DocumentLimits, DocumentRecoverability,
    NeverCancelled as DocumentNeverCancelled, ObjectAttestationKind, RevisionAttestationJobContext,
    RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::{
    IndirectObjectValue, ObjectErrorCode, ObjectLimitConfig, ObjectLimitKind, ObjectLimits,
    ObjectWorkCaps,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits, SyntaxObject};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(7);
const ATTEST_JOB: JobId = JobId::new(301);
const ATTEST_SCAN: ResumeCheckpoint = ResumeCheckpoint::new(302);
const ATTEST_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(303);
const ATTEST_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(304);
const ACCESS_JOB: JobId = JobId::new(401);
const ACCESS_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(402);
const ACCESS_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(403);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    startxref: u64,
}

fn snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0x71; 32]), SourceRevision::new(17)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xb3; 32]),
    )
}

fn other_snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0x72; 32]), SourceRevision::new(18)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xb4; 32]),
    )
}

fn fixture(prexref: &[u8], size: u32, in_use: &[(u32, u64)]) -> Fixture {
    let mut bytes = prexref.to_vec();
    let startxref = u64::try_from(bytes.len()).expect("fixture length fits u64");
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for number in 0..size {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else if let Some((_, offset)) = in_use.iter().find(|&&(entry, _)| entry == number) {
            format!("{offset:010} 00000 n \n")
        } else {
            "0000000000 00000 f \n".to_owned()
        };
        assert_eq!(row.len(), 20);
        bytes.extend_from_slice(row.as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
            .as_bytes(),
    );
    let source = snapshot(u64::try_from(bytes.len()).expect("fixture length fits u64"));
    Fixture {
        bytes,
        snapshot: source,
        startxref,
    }
}

fn canonical_fixture() -> Fixture {
    let result = fixture(b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n", 2, &[(1, 9)]);
    assert_eq!((result.startxref, result.bytes.len()), (29, 131));
    result
}

fn all_evidence_kinds_fixture() -> Fixture {
    let bodies: &[(u32, &[u8])] = &[
        (1, b"1 0 obj\nnull\nendobj\n"),
        (2, b"2 0 obj\ntrue\nendobj\n"),
        (3, b"3 0 obj\n42\nendobj\n"),
        (4, b"4 0 obj\n1.5\nendobj\n"),
        (5, b"5 0 obj\n/Name\nendobj\n"),
        (6, b"6 0 obj\n(text)\nendobj\n"),
        (7, b"7 0 obj\n[]\nendobj\n"),
        (8, b"8 0 obj\n<<>>\nendobj\n"),
        (9, b"9 0 obj\n1 0 R\nendobj\n"),
        (
            10,
            b"10 0 obj\n<< /Length 1 >>\nstream\nX\nendstream\nendobj\n",
        ),
    ];
    let mut prexref = b"%PDF-1.7\n".to_vec();
    let mut in_use = Vec::new();
    for &(number, body) in bodies {
        in_use.push((
            number,
            u64::try_from(prexref.len()).expect("fixture offset fits u64"),
        ));
        prexref.extend_from_slice(body);
    }
    let result = fixture(&prexref, 12, &in_use);
    assert_eq!(result.startxref, 239);
    result
}

fn large_stream_fixture() -> Fixture {
    let payload = vec![b'P'; 8192];
    let mut prexref = b"%PDF-1.7\n1 0 obj\n<< /Length 8192 >>\nstream\n".to_vec();
    prexref.extend_from_slice(&payload);
    prexref.extend_from_slice(b"\nendstream\nendobj\n");
    let result = fixture(&prexref, 2, &[(1, 9)]);
    assert_eq!(result.startxref, 8253);
    result
}

fn indirect_length_fixture() -> Fixture {
    fixture(
        b"%PDF-1.7\n1 0 obj\n<< /Length 0     >>\nstream\n\nendstream\nendobj\n",
        2,
        &[(1, 9)],
    )
}

fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("test object references are nonzero")
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    supplied_store_with_bytes(fixture.snapshot, &fixture.bytes)
}

fn supplied_store_with_bytes(source: SourceSnapshot, bytes: &[u8]) -> RangeStore {
    let store = RangeStore::new(source, Default::default()).expect("RangeStore limits are valid");
    let range = ByteRange::new(
        0,
        u64::try_from(bytes.len()).expect("fixture length fits u64"),
    )
    .expect("complete fixture range is non-empty");
    store
        .supply(
            RangeResponse::new(source, range, bytes.to_vec())
                .expect("complete response matches its range"),
        )
        .expect("fixture fits the RangeStore budget");
    store
}

fn supply_range(store: &RangeStore, fixture: &Fixture, range: ByteRange) {
    let start = usize::try_from(range.start()).expect("fixture offset fits usize");
    let end = usize::try_from(range.end_exclusive()).expect("fixture offset fits usize");
    store
        .supply(
            RangeResponse::new(fixture.snapshot, range, fixture.bytes[start..end].to_vec())
                .expect("partial response matches its range"),
        )
        .expect("partial response fits the RangeStore budget");
}

fn parsed_xref(fixture: &Fixture) -> XrefSection {
    let store = supplied_store(fixture);
    let mut job = OpenXrefJob::new(
        fixture.snapshot,
        XrefJobContext::new(
            JobId::new(201),
            ResumeCheckpoint::new(202),
            ResumeCheckpoint::new(203),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .expect("xref job configuration is valid");
    match job.poll(&store, &XrefNeverCancelled) {
        XrefPoll::Ready(section) => section,
        XrefPoll::Pending { .. } => panic!("a completely supplied xref must not suspend"),
        XrefPoll::Failed(error) => panic!("self-authored xref must parse: {error}"),
    }
}

fn candidate(fixture: &Fixture) -> CandidateRevisionIndex {
    CandidateRevisionIndex::from_xref(
        &parsed_xref(fixture),
        REVISION_ID,
        DocumentLimits::default(),
        &DocumentNeverCancelled,
    )
    .expect("self-authored xref metadata yields a candidate")
}

fn ready_with_profiles(
    fixture: &Fixture,
    object_limits: ObjectLimits,
    syntax_limits: SyntaxLimits,
) -> AttestedRevisionIndex {
    let store = supplied_store(fixture);
    let mut job = AttestRevisionJob::new(
        candidate(fixture),
        RevisionAttestationJobContext::new(
            ATTEST_JOB,
            ATTEST_SCAN,
            ATTEST_ENVELOPE,
            ATTEST_BOUNDARY,
            RequestPriority::Metadata,
        ),
        RevisionAttestationLimits::default(),
        object_limits,
        syntax_limits,
    )
    .expect("attestation job configuration is valid");
    match job.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Ready(index) => index,
        RevisionAttestationPoll::Pending { .. } => {
            panic!("a completely supplied attestation must not suspend")
        }
        RevisionAttestationPoll::Failed(error) => {
            panic!("self-authored fixture must attest: {error}")
        }
    }
}

fn ready(fixture: &Fixture) -> AttestedRevisionIndex {
    ready_with_profiles(fixture, ObjectLimits::default(), SyntaxLimits::default())
}

fn access_context(priority: RequestPriority) -> AttestedObjectJobContext {
    AttestedObjectJobContext::new(ACCESS_JOB, ACCESS_ENVELOPE, ACCESS_BOUNDARY, priority)
}

fn full_caps(index: &AttestedRevisionIndex) -> ObjectWorkCaps {
    ObjectWorkCaps::new(
        index.object_limits().max_total_read_bytes(),
        index.object_limits().max_total_parse_bytes(),
    )
    .expect("retained object totals form valid work caps")
}

fn poll_ready(
    index: &AttestedRevisionIndex,
    reference: ObjectRef,
    store: &dyn ByteSource,
) -> (AttestedObject, pdf_rs_document::OpenAttestedObjectJob) {
    let mut job = index
        .open_object(
            reference,
            access_context(RequestPriority::VisiblePage),
            full_caps(index),
        )
        .expect("attested reference opens");
    let object = match job.poll(store, &DocumentNeverCancelled) {
        AttestedObjectPoll::Ready(object) => object,
        AttestedObjectPoll::Pending { .. } => panic!("complete source must not suspend"),
        AttestedObjectPoll::Failed(error) => panic!("attested object must reopen: {error}"),
    };
    (object, job)
}

fn failed_poll(
    job: &mut pdf_rs_document::OpenAttestedObjectJob,
    source: &dyn ByteSource,
) -> DocumentError {
    let error = match job.poll(source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Failed(error) => error,
        AttestedObjectPoll::Ready(_) => panic!("expected access failure, got Ready"),
        AttestedObjectPoll::Pending { .. } => panic!("complete or failing source must not pend"),
    };
    assert_eq!(job.phase(), AttestedObjectPhase::Failed);
    match job.poll(source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Failed(repeated) => assert_eq!(repeated, error),
        _ => panic!("terminal access failure must replay exactly"),
    }
    error
}

struct RecordingSource<'a> {
    store: &'a RangeStore,
    requests: Mutex<Vec<ReadRequest>>,
}

impl ByteSource for RecordingSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.store.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        self.requests
            .lock()
            .expect("request log is healthy")
            .push(request);
        self.store.poll(request)
    }
}

struct PanicSource(SourceSnapshot);

impl ByteSource for PanicSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("source poll must not run")
    }
}

struct UnexpectedEofSource(SourceSnapshot);

impl ByteSource for UnexpectedEofSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        ReadPoll::EndOfFile
    }
}

struct FailingSource {
    snapshot: SourceSnapshot,
    error: SourceError,
}

impl ByteSource for FailingSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        ReadPoll::Failed(self.error)
    }
}

fn span(span: pdf_rs_syntax::ByteSpan) -> (u64, u64) {
    (span.start(), span.end_exclusive())
}

fn assert_canonical_direct(object: &AttestedObject, source: SourceSnapshot) {
    assert_eq!(object.snapshot(), source);
    assert_eq!(object.revision_id(), REVISION_ID);
    assert_eq!(object.revision_startxref(), 29);
    assert_eq!(object.reference(), object_ref(1));
    assert_eq!(object.attestation().xref_offset(), 9);
    assert_eq!(object.attestation().object_upper_bound(), 29);
    assert_eq!(span(object.header_span()), (9, 16));
    assert_eq!(span(object.object_span()), (9, 28));
    assert_eq!(span(object.endobj_span()), (22, 28));
    assert_eq!(
        object.attestation().kind(),
        ObjectAttestationKind::Dictionary
    );
    let IndirectObjectValue::Direct(value) = object.value() else {
        panic!("canonical object must remain direct")
    };
    assert_eq!(span(value.span()), (17, 21));
    assert!(matches!(value.value(), SyntaxObject::Dictionary(_)));
}

#[test]
fn canonical_direct_reopens_with_exact_proof_and_stats() {
    let fixture = canonical_fixture();
    let index = ready(&fixture);
    let store = supplied_store(&fixture);
    let (object, mut job) = poll_ready(&index, object_ref(1), &store);

    assert_canonical_direct(&object, fixture.snapshot);
    assert_eq!(job.phase(), AttestedObjectPhase::Complete);
    assert_eq!(job.stats().read_bytes(), 21);
    assert_eq!(job.stats().parse_bytes(), 21);
    assert_eq!(job.stats().envelope_attempts(), 1);
    assert_eq!(job.stats().boundary_attempts(), 0);
    assert_eq!(job.stats().declared_stream_bytes(), 0);

    match job.poll(&store, &DocumentNeverCancelled) {
        AttestedObjectPoll::Failed(error) => {
            assert_eq!(error.code(), DocumentErrorCode::JobAlreadyComplete);
            assert_eq!(error.reference(), Some(object_ref(1)));
            assert_eq!(error.offset(), Some(9));
        }
        _ => panic!("a completed access job is one-shot"),
    }
}

#[test]
fn canonical_small_and_large_streams_reopen_with_exact_unretained_payload_geometry() {
    let small = all_evidence_kinds_fixture();
    let small_index = ready(&small);
    let small_store = supplied_store(&small);
    let (object, job) = poll_ready(&small_index, object_ref(10), &small_store);
    let evidence = object.attestation();
    assert_eq!(
        (evidence.xref_offset(), evidence.object_upper_bound()),
        (188, 239)
    );
    assert_eq!(span(evidence.header_span()), (188, 196));
    assert_eq!(span(evidence.object_span()), (188, 238));
    assert_eq!(span(evidence.endobj_span()), (232, 238));
    let ObjectAttestationKind::Stream {
        data_span,
        endstream_span,
    } = evidence.kind()
    else {
        panic!("object ten must retain stream evidence")
    };
    assert_eq!(span(data_span), (220, 221));
    assert_eq!(span(endstream_span), (222, 231));
    let IndirectObjectValue::Stream(stream) = object.value() else {
        panic!("object ten must reopen as a stream")
    };
    assert_eq!(span(stream.dictionary().span()), (197, 212));
    assert_eq!(span(stream.length_value_span()), (208, 209));
    assert_eq!(span(stream.stream_keyword_span()), (213, 219));
    assert_eq!(span(stream.stream_line_ending_span()), (219, 220));
    assert_eq!(span(stream.data_span()), (220, 221));
    assert_eq!(span(stream.data_delimiter_span()), (221, 222));
    assert_eq!(span(stream.endstream_span()), (222, 231));
    assert_eq!(
        (job.stats().read_bytes(), job.stats().parse_bytes()),
        (70, 70)
    );
    assert_eq!(
        (
            job.stats().envelope_attempts(),
            job.stats().boundary_attempts()
        ),
        (1, 1)
    );
    assert_eq!(job.stats().declared_stream_bytes(), 1);

    let large = large_stream_fixture();
    let large_index = ready(&large);
    let large_store = supplied_store(&large);
    let (object, job) = poll_ready(&large_index, object_ref(1), &large_store);
    let evidence = object.attestation();
    assert_eq!(
        (evidence.xref_offset(), evidence.object_upper_bound()),
        (9, 8253)
    );
    assert_eq!(span(evidence.header_span()), (9, 16));
    assert_eq!(span(evidence.object_span()), (9, 8252));
    assert_eq!(span(evidence.endobj_span()), (8246, 8252));
    let ObjectAttestationKind::Stream {
        data_span,
        endstream_span,
    } = evidence.kind()
    else {
        panic!("large object must retain stream evidence")
    };
    assert_eq!(span(data_span), (43, 8235));
    assert_eq!(span(endstream_span), (8236, 8245));
    let IndirectObjectValue::Stream(stream) = object.value() else {
        panic!("large object must reopen as a stream")
    };
    assert_eq!(span(stream.dictionary().span()), (17, 35));
    assert_eq!(span(stream.length_value_span()), (28, 32));
    assert_eq!(span(stream.stream_keyword_span()), (36, 42));
    assert_eq!(span(stream.stream_line_ending_span()), (42, 43));
    assert_eq!(span(stream.data_span()), (43, 8235));
    assert_eq!(span(stream.data_delimiter_span()), (8235, 8236));
    assert_eq!(span(stream.endstream_span()), (8236, 8245));
    assert_eq!(
        (job.stats().read_bytes(), job.stats().parse_bytes()),
        (4114, 4114)
    );
    assert_eq!(
        (
            job.stats().envelope_attempts(),
            job.stats().boundary_attempts()
        ),
        (1, 1)
    );
    assert_eq!(job.stats().declared_stream_bytes(), 8192);
}

#[test]
fn every_retained_value_kind_reopens_with_the_same_exact_evidence() {
    let fixture = all_evidence_kinds_fixture();
    let index = ready(&fixture);
    let store = supplied_store(&fixture);
    let expected_kinds = [
        ObjectAttestationKind::Null,
        ObjectAttestationKind::Boolean,
        ObjectAttestationKind::Integer,
        ObjectAttestationKind::Real,
        ObjectAttestationKind::Name,
        ObjectAttestationKind::String,
        ObjectAttestationKind::Array,
        ObjectAttestationKind::Dictionary,
        ObjectAttestationKind::Reference,
    ];
    let offsets = [9, 29, 49, 67, 86, 107, 129, 147, 167];
    let uppers = [29, 49, 67, 86, 107, 129, 147, 167, 188];

    for (index_in_table, kind) in expected_kinds.into_iter().enumerate() {
        let number = u32::try_from(index_in_table + 1).expect("small fixture index fits u32");
        let reference = object_ref(number);
        let retained = index
            .attestation(reference)
            .expect("fixture evidence exists");
        assert_eq!(retained.kind(), kind);
        let (object, _) = poll_ready(&index, reference, &store);
        assert_eq!(object.attestation(), retained);
        assert_eq!(object.attestation().xref_offset(), offsets[index_in_table]);
        assert_eq!(
            object.attestation().object_upper_bound(),
            uppers[index_in_table]
        );
        assert_eq!(object.attestation().kind(), kind);
        assert!(matches!(object.value(), IndirectObjectValue::Direct(_)));
    }

    let retained = index
        .attestation(object_ref(10))
        .expect("stream evidence exists");
    let (object, _) = poll_ready(&index, object_ref(10), &store);
    assert_eq!(object.attestation(), retained);
    assert!(matches!(object.value(), IndirectObjectValue::Stream(_)));
}

#[test]
fn exact_lookup_errors_precede_child_configuration_and_leave_index_reusable() {
    let fixture = all_evidence_kinds_fixture();
    let index = ready(&fixture);
    let bad_context = AttestedObjectJobContext::new(
        ACCESS_JOB,
        ACCESS_ENVELOPE,
        ACCESS_ENVELOPE,
        RequestPriority::Metadata,
    );
    let valid_caps = full_caps(&index);
    let oversized_caps = ObjectWorkCaps::new(
        index.object_limits().max_total_read_bytes() + 1,
        index.object_limits().max_total_parse_bytes(),
    )
    .expect("one above the profile remains beneath the implementation hard cap");
    let cases = [
        (
            ObjectRef::new(1, 1).expect("wrong generation is a valid reference"),
            DocumentErrorCode::GenerationMismatch,
        ),
        (object_ref(11), DocumentErrorCode::FreeObject),
        (object_ref(12), DocumentErrorCode::MissingObject),
    ];

    for (reference, expected) in cases {
        for result in [
            index.open_object(reference, bad_context, valid_caps),
            index.open_object(
                reference,
                access_context(RequestPriority::Metadata),
                oversized_caps,
            ),
        ] {
            let error = result.expect_err("lookup must fail before child construction");
            assert_eq!(error.code(), expected);
            assert_eq!(error.category(), DocumentErrorCategory::Lookup);
            assert_eq!(
                error.recoverability(),
                DocumentRecoverability::CorrectReference
            );
            assert_eq!(error.reference(), Some(reference));
            assert_eq!(error.offset(), None);
        }
    }

    let context_error = index
        .open_object(object_ref(1), bad_context, valid_caps)
        .expect_err("equal checkpoints must be rejected for a valid reference");
    assert_eq!(
        context_error.code(),
        DocumentErrorCode::InvalidObjectAccessJobContext
    );
    let caps_error = index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            oversized_caps,
        )
        .expect_err("caps above the retained profile must be rejected");
    assert_eq!(caps_error.code(), DocumentErrorCode::InvalidLimits);
    assert_eq!(
        caps_error.object_error_code(),
        Some(ObjectErrorCode::InvalidLimits)
    );

    let store = supplied_store(&fixture);
    let (object, _) = poll_ready(&index, object_ref(1), &store);
    assert_eq!(object.attestation().kind(), ObjectAttestationKind::Null);
}

#[test]
fn privately_minted_access_job_remains_valid_after_its_index_is_dropped() {
    let fixture = canonical_fixture();
    let store = supplied_store(&fixture);
    let mut job = {
        let index = ready(&fixture);
        index
            .open_object(
                object_ref(1),
                access_context(RequestPriority::Metadata),
                full_caps(&index),
            )
            .expect("index privately mints a detached job")
    };
    let object = match job.poll(&store, &DocumentNeverCancelled) {
        AttestedObjectPoll::Ready(object) => object,
        other => panic!("detached job must retain its complete proof: {other:?}"),
    };
    assert_canonical_direct(&object, fixture.snapshot);
}

#[test]
fn direct_pending_preserves_ticket_checkpoint_and_charges_until_explicit_resume() {
    let fixture = canonical_fixture();
    let index = ready(&fixture);
    let empty = RangeStore::new(fixture.snapshot, Default::default()).expect("empty store works");
    let source = RecordingSource {
        store: &empty,
        requests: Mutex::new(Vec::new()),
    };
    let mut job = index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::VisiblePage),
            full_caps(&index),
        )
        .expect("attested object opens");
    let expected = ByteRange::new(8, 21).expect("canonical request is non-empty");
    let (ticket, stats) = match job.poll(&source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(checkpoint, ACCESS_ENVELOPE);
            assert_eq!(missing.as_slice(), &[expected]);
            (ticket, job.stats())
        }
        other => panic!("empty store must pend on the envelope: {other:?}"),
    };
    assert_eq!(job.phase(), AttestedObjectPhase::Envelope);
    match job.poll(&source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Pending {
            ticket: repeated,
            missing,
            checkpoint,
        } => {
            assert_eq!(repeated, ticket);
            assert_eq!(missing.as_slice(), &[expected]);
            assert_eq!(checkpoint, ACCESS_ENVELOPE);
        }
        other => panic!("re-poll must preserve pending state: {other:?}"),
    }
    assert_eq!(job.stats(), stats);

    let first = ByteRange::new(8, 10).expect("partial range works");
    supply_range(&empty, &fixture, first);
    match job.poll(&source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Pending {
            ticket: repeated,
            missing,
            ..
        } => {
            assert_eq!(repeated, ticket);
            assert_eq!(missing.as_slice(), &[ByteRange::new(18, 11).unwrap()]);
        }
        other => panic!("partial supply must remain pending: {other:?}"),
    }
    assert_eq!(job.stats(), stats);
    supply_range(&empty, &fixture, ByteRange::new(18, 11).unwrap());
    assert_eq!(job.phase(), AttestedObjectPhase::Envelope);
    let object = match job.poll(&source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Ready(object) => object,
        other => panic!("explicit poll after supply must finish: {other:?}"),
    };
    assert_canonical_direct(&object, fixture.snapshot);
    assert_eq!(job.stats().read_bytes(), 21);
    assert_eq!(job.stats().parse_bytes(), 21);
}

#[test]
fn large_stream_pending_uses_two_checkpoints_and_never_reads_payload_middle() {
    let fixture = large_stream_fixture();
    let index = ready(&fixture);
    let empty = RangeStore::new(fixture.snapshot, Default::default()).expect("empty store works");
    let source = RecordingSource {
        store: &empty,
        requests: Mutex::new(Vec::new()),
    };
    let mut job = index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::VisiblePage),
            full_caps(&index),
        )
        .expect("large stream opens");
    let envelope = ByteRange::new(8, 4096).unwrap();
    let envelope_ticket = match job.poll(&source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(checkpoint, ACCESS_ENVELOPE);
            assert_eq!(missing.as_slice(), &[envelope]);
            ticket
        }
        other => panic!("empty store must pend on large envelope: {other:?}"),
    };
    let envelope_stats = job.stats();
    match job.poll(&source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Pending {
            ticket, missing, ..
        } => {
            assert_eq!(ticket, envelope_ticket);
            assert_eq!(missing.as_slice(), &[envelope]);
        }
        other => panic!("re-poll must preserve envelope ticket: {other:?}"),
    }
    assert_eq!(job.stats(), envelope_stats);
    supply_range(&empty, &fixture, envelope);
    assert_eq!(job.phase(), AttestedObjectPhase::Envelope);

    let boundary = ByteRange::new(8235, 18).unwrap();
    let boundary_ticket = match job.poll(&source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(checkpoint, ACCESS_BOUNDARY);
            assert_eq!(missing.as_slice(), &[boundary]);
            ticket
        }
        other => panic!("supplied envelope must advance to boundary: {other:?}"),
    };
    assert_eq!(job.phase(), AttestedObjectPhase::StreamBoundary);
    let boundary_stats = job.stats();
    match job.poll(&source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(ticket, boundary_ticket);
            assert_eq!(missing.as_slice(), &[boundary]);
            assert_eq!(checkpoint, ACCESS_BOUNDARY);
        }
        other => panic!("re-poll must preserve boundary ticket: {other:?}"),
    }
    assert_eq!(job.stats(), boundary_stats);
    supply_range(&empty, &fixture, boundary);
    assert_eq!(job.phase(), AttestedObjectPhase::StreamBoundary);
    let object = match job.poll(&source, &DocumentNeverCancelled) {
        AttestedObjectPoll::Ready(object) => object,
        other => panic!("explicit boundary resume must finish: {other:?}"),
    };
    assert!(matches!(object.value(), IndirectObjectValue::Stream(_)));
    assert_eq!(
        (job.stats().read_bytes(), job.stats().parse_bytes()),
        (4114, 4114)
    );

    for request in source
        .requests
        .lock()
        .expect("request log is healthy")
        .iter()
    {
        assert_eq!(request.priority(), RequestPriority::VisiblePage);
        assert_eq!(request.job(), ACCESS_JOB);
        assert!(request.range() == envelope || request.range() == boundary);
        assert!(
            request.range().end_exclusive() <= 4104 || request.range().start() >= 8235,
            "opaque payload middle must never be requested"
        );
    }
}

#[test]
fn snapshot_mismatch_precedes_cancellation_and_any_source_read() {
    let fixture = canonical_fixture();
    let index = ready(&fixture);
    let mut job = index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&index),
        )
        .expect("attested object opens");
    let wrong = PanicSource(other_snapshot(
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
    ));
    let cancelled = AtomicBool::new(true);
    let error = match job.poll(&wrong, &cancelled) {
        AttestedObjectPoll::Failed(error) => error,
        _ => panic!("snapshot mismatch must fail before cancellation and read"),
    };
    assert_eq!(error.code(), DocumentErrorCode::SourceSnapshotMismatch);
    assert_eq!(error.category(), DocumentErrorCategory::Source);
    assert_eq!(error.recoverability(), DocumentRecoverability::ReopenSource);
    assert_eq!(
        error.object_error_code(),
        Some(ObjectErrorCode::SnapshotMismatch)
    );
    assert_eq!(job.stats().read_bytes(), 0);
    assert_eq!(job.phase(), AttestedObjectPhase::Failed);

    match job.poll(&supplied_store(&fixture), &DocumentNeverCancelled) {
        AttestedObjectPoll::Failed(repeated) => assert_eq!(repeated, error),
        _ => panic!("correct later source cannot resurrect a snapshot failure"),
    }
}

#[test]
fn pre_envelope_and_boundary_cancellation_are_terminal() {
    let direct = canonical_fixture();
    let direct_index = ready(&direct);
    let direct_store = supplied_store(&direct);
    let mut pre = direct_index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&direct_index),
        )
        .unwrap();
    let flag = AtomicBool::new(true);
    let error = match pre.poll(&direct_store, &flag) {
        AttestedObjectPoll::Failed(error) => error,
        _ => panic!("pre-cancelled job must fail"),
    };
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
    assert_eq!(error.category(), DocumentErrorCategory::Cancellation);
    assert_eq!(
        error.recoverability(),
        DocumentRecoverability::AbandonOperation
    );
    assert_eq!(pre.stats().read_bytes(), 0);

    let empty = RangeStore::new(direct.snapshot, Default::default()).unwrap();
    let mut envelope = direct_index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&direct_index),
        )
        .unwrap();
    assert!(matches!(
        envelope.poll(&empty, &DocumentNeverCancelled),
        AttestedObjectPoll::Pending { checkpoint, .. } if checkpoint == ACCESS_ENVELOPE
    ));
    let error = match envelope.poll(&empty, &flag) {
        AttestedObjectPoll::Failed(error) => error,
        _ => panic!("envelope-pending cancellation must fail"),
    };
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);

    let large = large_stream_fixture();
    let large_index = ready(&large);
    let partial = RangeStore::new(large.snapshot, Default::default()).unwrap();
    supply_range(&partial, &large, ByteRange::new(8, 4096).unwrap());
    let mut boundary = large_index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&large_index),
        )
        .unwrap();
    assert!(matches!(
        boundary.poll(&partial, &DocumentNeverCancelled),
        AttestedObjectPoll::Pending { checkpoint, .. } if checkpoint == ACCESS_BOUNDARY
    ));
    assert_eq!(boundary.phase(), AttestedObjectPhase::StreamBoundary);
    let error = match boundary.poll(&partial, &flag) {
        AttestedObjectPoll::Failed(error) => error,
        _ => panic!("boundary-pending cancellation must fail"),
    };
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
    flag.store(false, Ordering::Release);
    match boundary.poll(&supplied_store(&large), &flag) {
        AttestedObjectPoll::Failed(repeated) => assert_eq!(repeated, error),
        _ => panic!("cancelled boundary job must remain terminal"),
    }
}

fn assert_resource_error(
    error: DocumentError,
    kind: ObjectLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
) {
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(error.category(), DocumentErrorCategory::Resource);
    assert_eq!(
        error.recoverability(),
        DocumentRecoverability::ReduceWorkload
    );
    assert_eq!(error.limit(), None);
    let lower = error
        .object_error()
        .expect("complete lower object error is retained");
    assert_eq!(lower.code(), ObjectErrorCode::ResourceLimit);
    let detail = lower.limit().expect("lower resource detail is retained");
    assert_eq!(detail.kind(), kind);
    assert_eq!(detail.limit(), limit);
    assert_eq!(detail.consumed(), consumed);
    assert_eq!(detail.attempted(), attempted);
}

#[test]
fn direct_and_stream_scoped_caps_accept_exact_and_reject_one_less() {
    let direct = canonical_fixture();
    let direct_index = ready(&direct);
    let direct_store = supplied_store(&direct);
    for (read, parse, expected_kind) in [
        (20, 21, ObjectLimitKind::TotalReadBytes),
        (21, 20, ObjectLimitKind::TotalParseBytes),
    ] {
        let mut job = direct_index
            .open_object(
                object_ref(1),
                access_context(RequestPriority::Metadata),
                ObjectWorkCaps::new(read, parse).unwrap(),
            )
            .unwrap();
        let error = failed_poll(&mut job, &direct_store);
        assert_resource_error(error, expected_kind, 20, 0, 21);
        if expected_kind == ObjectLimitKind::TotalReadBytes {
            assert_eq!(
                (job.stats().read_bytes(), job.stats().parse_bytes()),
                (0, 0)
            );
        } else {
            assert_eq!(
                (job.stats().read_bytes(), job.stats().parse_bytes()),
                (21, 0)
            );
        }
    }
    let mut exact = direct_index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            ObjectWorkCaps::new(21, 21).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        exact.poll(&direct_store, &DocumentNeverCancelled),
        AttestedObjectPoll::Ready(_)
    ));

    let stream = all_evidence_kinds_fixture();
    let stream_index = ready(&stream);
    let stream_store = supplied_store(&stream);
    for (read, parse, expected_kind) in [
        (69, 70, ObjectLimitKind::TotalReadBytes),
        (70, 69, ObjectLimitKind::TotalParseBytes),
    ] {
        let mut job = stream_index
            .open_object(
                object_ref(10),
                access_context(RequestPriority::Metadata),
                ObjectWorkCaps::new(read, parse).unwrap(),
            )
            .unwrap();
        let error = failed_poll(&mut job, &stream_store);
        assert_resource_error(error, expected_kind, 69, 52, 18);
    }
    let mut exact = stream_index
        .open_object(
            object_ref(10),
            access_context(RequestPriority::Metadata),
            ObjectWorkCaps::new(70, 70).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        exact.poll(&stream_store, &DocumentNeverCancelled),
        AttestedObjectPoll::Ready(_)
    ));
}

#[test]
fn reopening_uses_the_persisted_object_profile_and_preserves_local_resource_detail() {
    let fixture = large_stream_fixture();
    let object_limits = ObjectLimits::validate(ObjectLimitConfig {
        max_stream_bytes: 8192,
        ..ObjectLimitConfig::default()
    })
    .expect("tight stream profile is valid");
    let index = ready_with_profiles(&fixture, object_limits, SyntaxLimits::default());
    assert_eq!(index.object_limits(), object_limits);
    let mut changed = fixture.bytes.clone();
    let position = changed
        .windows(4)
        .position(|window| window == b"8192")
        .expect("fixture contains its declared stream length");
    changed[position..position + 4].copy_from_slice(b"9999");
    let store = supplied_store_with_bytes(fixture.snapshot, &changed);
    let mut job = index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&index),
        )
        .unwrap();
    assert_eq!(job.object_limits(), object_limits);
    assert_eq!(job.syntax_limits(), index.syntax_limits());
    let error = failed_poll(&mut job, &store);
    assert_resource_error(error, ObjectLimitKind::StreamBytes, 8192, 0, 9999);
}

fn assert_evidence_mismatch(
    error: DocumentError,
    lower: Option<ObjectErrorCode>,
    expected_offset: u64,
) {
    assert_eq!(
        error.code(),
        DocumentErrorCode::AttestedObjectEvidenceMismatch
    );
    assert_eq!(error.category(), DocumentErrorCategory::Internal);
    assert_eq!(error.recoverability(), DocumentRecoverability::DoNotRetry);
    assert_eq!(error.reference(), Some(object_ref(1)));
    assert_eq!(error.offset(), Some(expected_offset));
    assert_eq!(error.object_error_code(), lower);
}

#[test]
fn same_snapshot_kind_and_impossible_lower_failures_never_publish_attested_values() {
    let canonical = canonical_fixture();
    let index = ready(&canonical);

    let mut changed_kind = canonical.bytes.clone();
    changed_kind[17..21].copy_from_slice(b"null");
    let store = supplied_store_with_bytes(canonical.snapshot, &changed_kind);
    let mut job = index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&index),
        )
        .unwrap();
    let error = failed_poll(&mut job, &store);
    assert_evidence_mismatch(error, None, 9);

    let mut changed_header = canonical.bytes.clone();
    changed_header[9] = b'2';
    let store = supplied_store_with_bytes(canonical.snapshot, &changed_header);
    let mut job = index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&index),
        )
        .unwrap();
    let error = failed_poll(&mut job, &store);
    assert_evidence_mismatch(error, Some(ObjectErrorCode::InvalidObjectHeader), 9);
    let lower = error
        .object_error()
        .expect("lower header error is retained");
    assert_eq!(lower.reference(), Some(object_ref(1)));
    assert_eq!(lower.offset(), Some(9));

    let indirect = indirect_length_fixture();
    let indirect_index = ready(&indirect);
    let mut changed_length = indirect.bytes.clone();
    let position = changed_length
        .windows(5)
        .position(|window| window == b"0    ")
        .expect("fixture contains padded direct length");
    changed_length[position..position + 5].copy_from_slice(b"1 0 R");
    let store = supplied_store_with_bytes(indirect.snapshot, &changed_length);
    let mut job = indirect_index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&indirect_index),
        )
        .unwrap();
    let error = failed_poll(&mut job, &store);
    assert_evidence_mismatch(error, Some(ObjectErrorCode::UnsupportedIndirectLength), 28);
}

#[test]
fn lower_source_failure_and_in_range_eof_keep_the_complete_chain() {
    let fixture = canonical_fixture();
    let index = ready(&fixture);
    let lower = SourceError::source_unavailable();
    let source = FailingSource {
        snapshot: fixture.snapshot,
        error: lower,
    };
    let mut job = index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&index),
        )
        .unwrap();
    let error = failed_poll(&mut job, &source);
    assert_eq!(error.code(), DocumentErrorCode::SourceFailure);
    assert_eq!(error.category(), DocumentErrorCategory::Source);
    assert_eq!(
        error.object_error_code(),
        Some(ObjectErrorCode::SourceFailure)
    );
    assert_eq!(error.source_error(), Some(lower));
    assert_eq!(error.reference(), Some(object_ref(1)));
    assert_eq!(error.offset(), Some(9));

    let eof = UnexpectedEofSource(fixture.snapshot);
    let mut job = index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&index),
        )
        .unwrap();
    let error = failed_poll(&mut job, &eof);
    assert_eq!(error.code(), DocumentErrorCode::UnexpectedEndOfSource);
    assert_eq!(error.category(), DocumentErrorCategory::Source);
    assert_eq!(error.recoverability(), DocumentRecoverability::ReopenSource);
    assert_eq!(
        error.object_error_code(),
        Some(ObjectErrorCode::UnexpectedEndOfSource)
    );
}

#[test]
fn debug_surfaces_redact_values_payloads_private_evidence_and_child_state() {
    let fixture = all_evidence_kinds_fixture();
    let index = ready(&fixture);
    let store = supplied_store(&fixture);
    let mut job = index
        .open_object(
            object_ref(6),
            access_context(RequestPriority::Metadata),
            full_caps(&index),
        )
        .unwrap();
    let index_debug = format!("{index:?}");
    let job_debug = format!("{job:?}");
    let object = match job.poll(&store, &DocumentNeverCancelled) {
        AttestedObjectPoll::Ready(object) => object,
        other => panic!("string object must reopen: {other:?}"),
    };
    let object_debug = format!("{object:?}");
    for debug in [&index_debug, &job_debug, &object_debug] {
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("(text)"));
        assert!(!debug.contains("OpenObjectJob"));
        assert!(!debug.contains("IndirectObjectTarget"));
        assert!(!debug.contains("0xb3"));
    }

    let large = large_stream_fixture();
    let large_index = ready(&large);
    let large_store = supplied_store(&large);
    let (large_object, _) = poll_ready(&large_index, object_ref(1), &large_store);
    let debug = format!("{large_object:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("PPPPPPPP"));

    let mut changed = canonical_fixture();
    let changed_index = ready(&changed);
    changed.bytes[9] = b'2';
    let changed_store = supplied_store_with_bytes(changed.snapshot, &changed.bytes);
    let mut failed = changed_index
        .open_object(
            object_ref(1),
            access_context(RequestPriority::Metadata),
            full_caps(&changed_index),
        )
        .unwrap();
    let error = failed_poll(&mut failed, &changed_store);
    let debug = format!("{error:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("InvalidObjectHeader {"));
}
