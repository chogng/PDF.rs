use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
    SupplyOutcome,
};
use pdf_rs_document::{
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex, DocumentError,
    DocumentErrorCategory, DocumentErrorCode, DocumentLimitKind, DocumentLimits,
    DocumentRecoverability, NeverCancelled as DocumentNeverCancelled, ObjectAttestationKind,
    RevisionAttestationJobContext, RevisionAttestationLimitConfig, RevisionAttestationLimits,
    RevisionAttestationPhase, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::{ObjectErrorCode, ObjectLimitConfig, ObjectLimitKind, ObjectLimits};
use pdf_rs_syntax::{ObjectRef, SyntaxErrorCode, SyntaxLimitConfig, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(7);
const ATTEST_JOB: JobId = JobId::new(201);
const SCAN_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(202);
const ENVELOPE_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(203);
const BOUNDARY_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(204);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    startxref: u64,
}

fn snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0x51; 32]), SourceRevision::new(11)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xa3; 32]),
    )
}

fn other_snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0x52; 32]), SourceRevision::new(12)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xa4; 32]),
    )
}

fn fixture(prexref: &[u8], size: u32, in_use: &[(u32, u64)]) -> Fixture {
    assert!(size >= 2);
    assert!(in_use.iter().any(|&(number, _)| number == 1));
    assert!(in_use.iter().all(|&(number, _)| number < size));

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
        assert_eq!(row.len(), 20, "traditional xref rows remain fixed-width");
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
    assert_eq!(result.startxref, 29);
    assert_eq!(result.bytes.len(), 131);
    result
}

fn legal_trivia_fixture() -> Fixture {
    let result = fixture(
        b"%PDF-1.7\n%prefix\r\n \t\x0c1 0 obj\n<<>>\nendobj\r\n%between\n\0\t2 0 obj\nnull\nendobj\n%tail\r\n \t",
        3,
        &[(1, 21), (2, 53)],
    );
    assert_eq!(result.startxref, 82);
    assert_eq!(result.bytes.len(), 204);
    result
}

fn comment_target_fixture() -> Fixture {
    let result = fixture(b"%PDF-1.7\n% fake 1 0 obj <<>> endobj\n", 2, &[(1, 16)]);
    assert_eq!(result.startxref, 36);
    result
}

fn string_target_fixture() -> Fixture {
    let result = fixture(b"%PDF-1.7\n( fake 1 0 obj <<>> endobj )\n", 2, &[(1, 16)]);
    assert_eq!(result.startxref, 38);
    result
}

fn unindexed_wrapper_fixture() -> Fixture {
    let result = fixture(
        b"%PDF-1.7\n9 0 obj\nnull\n 1 0 obj <<>> endobj\nendobj\n",
        2,
        &[(1, 23)],
    );
    assert_eq!(result.startxref, 50);
    result
}

fn crossing_array_fixture() -> Fixture {
    let result = fixture(
        b"%PDF-1.7\n1 0 obj\n[\n2 0 obj null endobj\n]\nendobj\n",
        3,
        &[(1, 9), (2, 19)],
    );
    assert_eq!(result.startxref, 48);
    result
}

fn stream_target_fixture() -> Fixture {
    let result = fixture(
        b"%PDF-1.7\n9 0 obj\n<< /Length 20 >>\nstream\n1 0 obj <<>> endobj\nendstream\nendobj\n",
        10,
        &[(1, 41), (9, 9)],
    );
    assert_eq!(result.startxref, 78);
    assert_eq!(result.bytes.len(), 342);
    result
}

fn tail_garbage_fixture() -> Fixture {
    let result = fixture(b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\ngarbage\n", 2, &[(1, 9)]);
    assert_eq!(result.startxref, 37);
    result
}

fn unterminated_tail_comment_fixture() -> Fixture {
    let result = fixture(
        b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n%unterminated ",
        2,
        &[(1, 9)],
    );
    assert_eq!(result.startxref, 43);
    result
}

fn invalid_header_fixture() -> Fixture {
    let result = fixture(b"%PDF-1.x\n1 0 obj\n<<>>\nendobj\n", 2, &[(1, 9)]);
    assert_eq!(result.startxref, 29);
    result
}

fn invalid_header_ending_fixture() -> Fixture {
    let result = fixture(b"%PDF-1.7 1 0 obj\n<<>>\nendobj\n", 2, &[(1, 9)]);
    assert_eq!(result.startxref, 29);
    result
}

fn long_comment_fixture() -> Fixture {
    let result = fixture(
        b"%PDF-1.7\n%12345678\n1 0 obj\n<<>>\nendobj\n",
        2,
        &[(1, 19)],
    );
    assert_eq!(result.startxref, 39);
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
    fixture(&prexref, 12, &in_use)
}

fn large_stream_fixture() -> Fixture {
    let payload = vec![b'P'; 8192];
    let mut prexref = b"%PDF-1.7\n1 0 obj\n<< /Length 8192 >>\nstream\n".to_vec();
    prexref.extend_from_slice(&payload);
    prexref.extend_from_slice(b"\nendstream\nendobj\n");
    fixture(&prexref, 2, &[(1, 9)])
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default())
        .expect("fixture RangeStore limits are valid");
    let range = ByteRange::new(
        0,
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
    )
    .expect("complete fixture range is non-empty");
    store
        .supply(
            RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone())
                .expect("complete fixture response matches its range"),
        )
        .expect("fixture bytes fit the default RangeStore budget");
    store
}

fn parsed_xref(fixture: &Fixture) -> XrefSection {
    let store = supplied_store(fixture);
    let mut job = OpenXrefJob::new(
        fixture.snapshot,
        XrefJobContext::new(
            JobId::new(101),
            ResumeCheckpoint::new(102),
            ResumeCheckpoint::new(103),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .expect("public xref job configuration is valid");
    match job.poll(&store, &XrefNeverCancelled) {
        XrefPoll::Ready(section) => section,
        XrefPoll::Pending { .. } => panic!("a completely supplied xref fixture must not suspend"),
        XrefPoll::Failed(error) => panic!("self-authored xref fixture must parse: {error}"),
    }
}

fn candidate(fixture: &Fixture) -> CandidateRevisionIndex {
    CandidateRevisionIndex::from_xref(
        &parsed_xref(fixture),
        REVISION_ID,
        DocumentLimits::default(),
        &DocumentNeverCancelled,
    )
    .expect("self-authored xref metadata must yield a candidate index")
}

fn supply_range(store: &RangeStore, fixture: &Fixture, range: ByteRange) -> SupplyOutcome {
    let start = usize::try_from(range.start()).expect("fixture offset fits usize");
    let end = usize::try_from(range.end_exclusive()).expect("fixture offset fits usize");
    store
        .supply(
            RangeResponse::new(fixture.snapshot, range, fixture.bytes[start..end].to_vec())
                .expect("partial fixture response matches its range"),
        )
        .expect("partial fixture bytes fit the default RangeStore budget")
}

fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("test object references are nonzero")
}

fn attestation_context() -> RevisionAttestationJobContext {
    RevisionAttestationJobContext::new(
        ATTEST_JOB,
        SCAN_CHECKPOINT,
        ENVELOPE_CHECKPOINT,
        BOUNDARY_CHECKPOINT,
        RequestPriority::Metadata,
    )
}

fn new_attestation_job(fixture: &Fixture, limits: RevisionAttestationLimits) -> AttestRevisionJob {
    AttestRevisionJob::new(
        candidate(fixture),
        attestation_context(),
        limits,
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .expect("attestation job configuration works")
}

fn ready_with_limits(
    fixture: &Fixture,
    limits: RevisionAttestationLimits,
) -> (AttestedRevisionIndex, AttestRevisionJob) {
    let store = supplied_store(fixture);
    let mut job = new_attestation_job(fixture, limits);
    let attested = match job.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Ready(index) => index,
        RevisionAttestationPoll::Pending { .. } => {
            panic!("a completely supplied attestation fixture must not suspend")
        }
        RevisionAttestationPoll::Failed(error) => {
            panic!("self-authored attestation fixture must succeed: {error}")
        }
    };
    assert_eq!(job.phase(), RevisionAttestationPhase::Complete);
    (attested, job)
}

fn ready(fixture: &Fixture) -> (AttestedRevisionIndex, AttestRevisionJob) {
    ready_with_limits(fixture, RevisionAttestationLimits::default())
}

fn failed(fixture: &Fixture) -> (DocumentError, AttestRevisionJob, RangeStore) {
    let store = supplied_store(fixture);
    let mut job = new_attestation_job(fixture, RevisionAttestationLimits::default());
    let error = match job.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Failed(error) => error,
        RevisionAttestationPoll::Ready(_) => panic!("attack fixture must not publish an index"),
        RevisionAttestationPoll::Pending { .. } => {
            panic!("a completely supplied attack fixture must not suspend")
        }
    };
    assert_eq!(job.phase(), RevisionAttestationPhase::Failed);
    match job.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Failed(repeated) => assert_eq!(repeated, error),
        _ => panic!("a failed attestation job must retain its exact terminal error"),
    }
    (error, job, store)
}

fn limits(update: impl FnOnce(&mut RevisionAttestationLimitConfig)) -> RevisionAttestationLimits {
    let mut config = RevisionAttestationLimitConfig::default();
    update(&mut config);
    RevisionAttestationLimits::validate(config)
        .expect("test attestation limit profile is internally consistent")
}

fn assert_limit(error: DocumentError, expected: DocumentLimitKind, expected_limit: u64) {
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(error.category(), DocumentErrorCategory::Resource);
    assert_eq!(
        error.recoverability(),
        DocumentRecoverability::ReduceWorkload
    );
    let detail = error.limit().expect("resource errors retain limit detail");
    assert_eq!(detail.kind(), expected);
    assert_eq!(detail.limit(), expected_limit);
    assert!(detail.consumed() <= detail.limit());
    assert!(
        detail.consumed().saturating_add(detail.attempted()) > detail.limit(),
        "the attempted charge must cross its reported limit"
    );
}

fn poll_failure(
    job: &mut AttestRevisionJob,
    store: &RangeStore,
    expected: DocumentErrorCode,
) -> DocumentError {
    let error = match job.poll(store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Failed(error) => error,
        RevisionAttestationPoll::Ready(_) => panic!("expected {expected:?}, got Ready"),
        RevisionAttestationPoll::Pending { .. } => panic!("fully supplied source must not pend"),
    };
    assert_eq!(error.code(), expected);
    assert_eq!(job.phase(), RevisionAttestationPhase::Failed);
    match job.poll(store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Failed(repeated) => assert_eq!(repeated, error),
        _ => panic!("terminal failure must remain stable"),
    }
    error
}

#[test]
fn canonical_revision_publishes_exact_header_object_and_terminal_gap_evidence() {
    let fixture = canonical_fixture();
    let (attested, mut job) = ready(&fixture);

    assert_eq!(attested.snapshot(), fixture.snapshot);
    assert_eq!(attested.revision_id(), REVISION_ID);
    assert_eq!(attested.startxref(), 29);
    assert_eq!(attested.root(), object_ref(1));
    assert_eq!(attested.header().span().start(), 0);
    assert_eq!(attested.header().span().end_exclusive(), 8);
    assert_eq!(
        (
            attested.header().value().major(),
            attested.header().value().minor()
        ),
        (1, 7)
    );

    let evidence = attested.object_attestations();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].revision_id(), REVISION_ID);
    assert_eq!(evidence[0].reference(), object_ref(1));
    assert_eq!(evidence[0].xref_offset(), 9);
    assert_eq!(evidence[0].object_upper_bound(), 29);
    assert_eq!(
        (
            evidence[0].header_span().start(),
            evidence[0].header_span().end_exclusive()
        ),
        (9, 16)
    );
    assert_eq!(
        (
            evidence[0].object_span().start(),
            evidence[0].object_span().end_exclusive()
        ),
        (9, 28)
    );
    assert_eq!(
        (
            evidence[0].endobj_span().start(),
            evidence[0].endobj_span().end_exclusive()
        ),
        (22, 28)
    );
    assert_eq!(evidence[0].kind(), ObjectAttestationKind::Dictionary);

    let stats = attested.attestation_stats();
    assert_eq!(stats, job.stats());
    assert_eq!(stats.objects_attested(), 1);
    assert_eq!(stats.trivia_read_bytes(), 10);
    assert_eq!(stats.trivia_scan_bytes(), 2);
    assert!(stats.object_read_bytes() > 0);
    assert!(stats.object_parse_bytes() > 0);
    assert_eq!(stats.retained_evidence_bytes(), 192);

    match job.poll(&supplied_store(&fixture), &DocumentNeverCancelled) {
        RevisionAttestationPoll::Failed(error) => {
            assert_eq!(error.code(), DocumentErrorCode::JobAlreadyComplete)
        }
        _ => panic!("a completed one-shot job must reject another poll"),
    }
}

#[test]
fn all_pdf_whitespace_and_terminated_comments_are_valid_top_level_trivia() {
    let fixture = legal_trivia_fixture();
    let (attested, _) = ready(&fixture);
    assert_eq!(attested.object_attestations().len(), 2);

    let first = attested.attestation(object_ref(1)).unwrap();
    assert_eq!((first.xref_offset(), first.object_upper_bound()), (21, 53));
    assert_eq!(first.object_span().end_exclusive(), 40);
    assert_eq!(first.kind(), ObjectAttestationKind::Dictionary);

    let second = attested.attestation(object_ref(2)).unwrap();
    assert_eq!(
        (second.xref_offset(), second.object_upper_bound()),
        (53, 82)
    );
    assert_eq!(second.object_span().end_exclusive(), 72);
    assert_eq!(second.kind(), ObjectAttestationKind::Null);
    assert_eq!(attested.attestation_stats().trivia_scan_bytes(), 36);
}

#[test]
fn header_version_and_mandatory_line_ending_are_attested_at_source_zero() {
    for (fixture, expected_offset, retains_syntax) in [
        (invalid_header_fixture(), 0, true),
        (invalid_header_ending_fixture(), 8, false),
    ] {
        let (error, job, _) = failed(&fixture);
        assert_eq!(error.code(), DocumentErrorCode::InvalidDocumentHeader);
        assert_eq!(error.offset(), Some(expected_offset));
        assert_eq!(job.stats().objects_attested(), 0);
        if retains_syntax {
            assert_eq!(
                error.syntax_error().map(|syntax| syntax.code()),
                Some(SyntaxErrorCode::InvalidHeader)
            );
        } else {
            assert!(error.syntax_error().is_none());
        }
    }
}

#[test]
fn job_construction_requires_exact_eight_byte_header_syntax_capacity() {
    let fixture = canonical_fixture();
    let object_limits = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: u64::try_from(fixture.bytes.len()).unwrap(),
        initial_envelope_bytes: 1,
        max_envelope_bytes: 7,
        initial_boundary_bytes: 1,
        max_boundary_bytes: 7,
        max_stream_bytes: 1,
        max_total_read_bytes: 14,
        max_total_parse_bytes: 14,
    })
    .unwrap();
    let tiny_syntax = |input_bytes, token_bytes| {
        SyntaxLimits::validate(SyntaxLimitConfig {
            max_input_bytes: input_bytes,
            max_token_bytes: token_bytes,
            max_comment_bytes: 1,
            max_name_bytes: 1,
            max_string_source_bytes: 1,
            max_string_decoded_bytes: 1,
            max_owned_bytes: 1,
            max_total_tokens: 16,
            max_container_entries: 8,
            max_container_bytes: 1024,
            max_container_depth: 4,
        })
        .unwrap()
    };

    for (input_bytes, token_bytes) in [(7, 7), (8, 7)] {
        let error = AttestRevisionJob::new(
            candidate(&fixture),
            attestation_context(),
            RevisionAttestationLimits::default(),
            object_limits,
            tiny_syntax(input_bytes, token_bytes),
        )
        .unwrap_err();
        assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
    }

    AttestRevisionJob::new(
        candidate(&fixture),
        attestation_context(),
        RevisionAttestationLimits::default(),
        object_limits,
        tiny_syntax(8, 8),
    )
    .expect("eight header input and token bytes are exactly sufficient");
}

#[test]
fn embedded_targets_and_crossing_objects_never_publish_partial_attestation() {
    struct Case {
        name: &'static str,
        fixture: fn() -> Fixture,
        code: DocumentErrorCode,
        reference: u32,
        offset: u64,
        object_code: Option<ObjectErrorCode>,
    }

    let cases = [
        Case {
            name: "comment target",
            fixture: comment_target_fixture,
            code: DocumentErrorCode::UnterminatedTopLevelComment,
            reference: 1,
            offset: 9,
            object_code: None,
        },
        Case {
            name: "literal-string target",
            fixture: string_target_fixture,
            code: DocumentErrorCode::TopLevelData,
            reference: 1,
            offset: 9,
            object_code: None,
        },
        Case {
            name: "unindexed-object wrapper",
            fixture: unindexed_wrapper_fixture,
            code: DocumentErrorCode::TopLevelData,
            reference: 1,
            offset: 9,
            object_code: None,
        },
        Case {
            name: "indexed array crossing next interval",
            fixture: crossing_array_fixture,
            code: DocumentErrorCode::ObjectAttestationFailure,
            reference: 1,
            offset: 19,
            object_code: Some(ObjectErrorCode::ObjectCrossesPhysicalBound),
        },
        Case {
            name: "indexed stream swallowing a physical-later root target",
            fixture: stream_target_fixture,
            code: DocumentErrorCode::ObjectAttestationFailure,
            reference: 9,
            offset: 41,
            object_code: Some(ObjectErrorCode::ObjectCrossesPhysicalBound),
        },
    ];

    for case in cases {
        let fixture = (case.fixture)();
        let (error, job, _) = failed(&fixture);
        assert_eq!(error.code(), case.code, "{}", case.name);
        assert_eq!(
            error.reference(),
            Some(object_ref(case.reference)),
            "{}",
            case.name
        );
        assert_eq!(error.offset(), Some(case.offset), "{}", case.name);
        assert_eq!(error.object_error_code(), case.object_code, "{}", case.name);
        assert_eq!(
            job.phase(),
            RevisionAttestationPhase::Failed,
            "{}",
            case.name
        );
    }
}

#[test]
fn terminal_gap_rejects_data_and_comments_open_at_startxref_after_object_framing() {
    for (fixture, code, offset) in [
        (tail_garbage_fixture(), DocumentErrorCode::TopLevelData, 29),
        (
            unterminated_tail_comment_fixture(),
            DocumentErrorCode::UnterminatedTopLevelComment,
            29,
        ),
    ] {
        let (error, job, _) = failed(&fixture);
        assert_eq!(error.code(), code);
        assert_eq!(error.reference(), None);
        assert_eq!(error.offset(), Some(offset));
        assert_eq!(job.stats().objects_attested(), 1);
        assert_eq!(job.phase(), RevisionAttestationPhase::Failed);
    }
}

#[test]
fn fixed_evidence_classifies_every_value_kind_and_uses_exact_lookup_states() {
    let fixture = all_evidence_kinds_fixture();
    let (attested, _) = ready(&fixture);
    assert_eq!(attested.object_attestations().len(), 10);

    let expected = [
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
    for (number, kind) in (1_u32..=9).zip(expected) {
        assert_eq!(
            attested.attestation(object_ref(number)).unwrap().kind(),
            kind
        );
    }
    let stream = attested.attestation(object_ref(10)).unwrap();
    let ObjectAttestationKind::Stream {
        data_span,
        endstream_span,
    } = stream.kind()
    else {
        panic!("object ten must retain stream framing evidence")
    };
    assert_eq!(data_span.len(), 1);
    assert_eq!(
        fixture.bytes[usize::try_from(data_span.start()).unwrap()],
        b'X'
    );
    assert_eq!(endstream_span.len(), 9);

    let wrong_generation = attested
        .attestation(ObjectRef::new(1, 1).unwrap())
        .unwrap_err();
    assert_eq!(
        wrong_generation.code(),
        DocumentErrorCode::GenerationMismatch
    );
    let free = attested.attestation(object_ref(11)).unwrap_err();
    assert_eq!(free.code(), DocumentErrorCode::FreeObject);
    let missing = attested.attestation(object_ref(12)).unwrap_err();
    assert_eq!(missing.code(), DocumentErrorCode::MissingObject);
}

#[test]
fn pending_ranges_are_stable_partially_supplied_and_only_resume_on_explicit_poll() {
    let fixture = large_stream_fixture();
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = new_attestation_job(&fixture, RevisionAttestationLimits::default());

    let (scan_ticket, scan_range, initial_stats) = match job.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(checkpoint, SCAN_CHECKPOINT);
            assert_eq!(missing.len(), 1);
            (ticket, missing.as_slice()[0], job.stats())
        }
        other => panic!("an empty store must suspend the prefix scan: {other:?}"),
    };
    assert_eq!(scan_range, ByteRange::new(0, 9).unwrap());
    assert_eq!(job.phase(), RevisionAttestationPhase::Prefix);

    for _ in 0..3 {
        match job.poll(&store, &DocumentNeverCancelled) {
            RevisionAttestationPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                assert_eq!(ticket, scan_ticket);
                assert_eq!(missing.as_slice(), &[scan_range]);
                assert_eq!(checkpoint, SCAN_CHECKPOINT);
            }
            _ => panic!("absent prefix bytes must preserve one ticket"),
        }
        assert_eq!(job.stats(), initial_stats);
    }

    let first = ByteRange::new(scan_range.start(), 4).unwrap();
    assert!(
        supply_range(&store, &fixture, first)
            .ready_tickets()
            .is_empty()
    );
    match job.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(ticket, scan_ticket);
            assert_eq!(missing.as_slice(), &[ByteRange::new(4, 5).unwrap()]);
            assert_eq!(checkpoint, SCAN_CHECKPOINT);
        }
        _ => panic!("a partial response must keep the prefix suspended"),
    }
    assert_eq!(job.stats(), initial_stats);

    let remainder = ByteRange::new(first.end_exclusive(), 5).unwrap();
    let outcome = supply_range(&store, &fixture, remainder);
    assert_eq!(outcome.ready_tickets(), &[scan_ticket]);
    assert_eq!(job.phase(), RevisionAttestationPhase::Prefix);

    let mut saw_scan = true;
    let mut saw_envelope = false;
    let mut saw_boundary = false;
    let mut polls = 0_u32;
    let attested = loop {
        polls += 1;
        assert!(polls < 64, "attestation must make bounded progress");
        match job.poll(&store, &DocumentNeverCancelled) {
            RevisionAttestationPoll::Ready(index) => break index,
            RevisionAttestationPoll::Failed(error) => {
                panic!("partial supplies must eventually attest: {error}")
            }
            RevisionAttestationPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                saw_scan |= checkpoint == SCAN_CHECKPOINT;
                saw_envelope |= checkpoint == ENVELOPE_CHECKPOINT;
                saw_boundary |= checkpoint == BOUNDARY_CHECKPOINT;
                let ranges = missing.as_slice().to_vec();
                let phase_before_supply = job.phase();
                let mut woke = false;
                for range in ranges {
                    let outcome = supply_range(&store, &fixture, range);
                    woke |= outcome.ready_tickets().contains(&ticket);
                }
                assert!(woke, "covering every missing range must wake its ticket");
                assert_eq!(
                    job.phase(),
                    phase_before_supply,
                    "supply must not resume inline"
                );
            }
        }
    };
    assert!(saw_scan && saw_envelope && saw_boundary);
    assert_eq!(job.phase(), RevisionAttestationPhase::Complete);
    assert!(matches!(
        attested.attestation(object_ref(1)).unwrap().kind(),
        ObjectAttestationKind::Stream { data_span, .. } if data_span.len() == 8192
    ));
}

#[test]
fn source_snapshot_mismatch_fails_before_any_read_and_cannot_switch_sources() {
    let fixture = canonical_fixture();
    let mut job = new_attestation_job(&fixture, RevisionAttestationLimits::default());
    let wrong = RangeStore::new(
        other_snapshot(u64::try_from(fixture.bytes.len()).unwrap()),
        Default::default(),
    )
    .unwrap();

    let error = match job.poll(&wrong, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Failed(error) => error,
        _ => panic!("a different snapshot must fail before requesting bytes"),
    };
    assert_eq!(error.code(), DocumentErrorCode::SourceSnapshotMismatch);
    assert_eq!(error.offset(), None);
    assert_eq!(job.stats().trivia_read_bytes(), 0);
    assert_eq!(job.stats().objects_attested(), 0);
    assert_eq!(job.phase(), RevisionAttestationPhase::Failed);

    match job.poll(&supplied_store(&fixture), &DocumentNeverCancelled) {
        RevisionAttestationPoll::Failed(repeated) => assert_eq!(repeated, error),
        _ => panic!("a snapshot failure must remain terminal on a later correct source"),
    }
}

#[test]
fn pre_and_mid_pending_cancellation_are_terminal_without_partial_publication() {
    let fixture = canonical_fixture();

    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut pre = new_attestation_job(&fixture, RevisionAttestationLimits::default());
    let cancelled = AtomicBool::new(true);
    let pre_error = match pre.poll(&store, &cancelled) {
        RevisionAttestationPoll::Failed(error) => error,
        _ => panic!("pre-cancellation must stop before a request"),
    };
    assert_eq!(pre_error.code(), DocumentErrorCode::Cancelled);
    assert_eq!(pre.stats().trivia_read_bytes(), 0);
    assert_eq!(pre.stats().objects_attested(), 0);

    let mut mid = new_attestation_job(&fixture, RevisionAttestationLimits::default());
    let (ticket, ranges) = match mid.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Pending {
            ticket, missing, ..
        } => (ticket, missing.as_slice().to_vec()),
        _ => panic!("empty store must first suspend"),
    };
    let charged = mid.stats();
    let flag = AtomicBool::new(false);
    flag.store(true, Ordering::Release);
    let error = match mid.poll(&store, &flag) {
        RevisionAttestationPoll::Failed(error) => error,
        _ => panic!("cancellation while pending must be terminal"),
    };
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
    assert_eq!(mid.stats(), charged);
    assert_eq!(mid.phase(), RevisionAttestationPhase::Failed);

    let mut woke = false;
    for range in ranges {
        woke |= supply_range(&store, &fixture, range)
            .ready_tickets()
            .contains(&ticket);
    }
    assert!(woke);
    match mid.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Failed(repeated) => assert_eq!(repeated, error),
        _ => panic!("supplying an abandoned ticket must not resurrect the job"),
    }
}

#[test]
fn exact_observed_budgets_succeed_and_one_less_fails_in_each_dimension() {
    let fixture = legal_trivia_fixture();
    let (baseline, _) = ready(&fixture);
    let stats = baseline.attestation_stats();
    assert_eq!(stats.objects_attested(), 2);
    assert!(stats.trivia_read_bytes() > 9);
    assert!(stats.object_read_bytes() > 1);
    assert!(stats.object_parse_bytes() > 1);
    assert!(stats.retained_evidence_bytes() > 1);

    let exact = RevisionAttestationLimitConfig {
        max_source_bytes: u64::try_from(fixture.bytes.len()).unwrap(),
        max_objects: stats.objects_attested(),
        scan_chunk_bytes: 9,
        max_trivia_bytes: stats.trivia_read_bytes(),
        max_comment_bytes: 8,
        max_total_object_read_bytes: stats.object_read_bytes(),
        max_total_object_parse_bytes: stats.object_parse_bytes(),
        max_retained_evidence_bytes: stats.retained_evidence_bytes(),
    };
    let (at_exact, _) = ready_with_limits(
        &fixture,
        RevisionAttestationLimits::validate(exact).unwrap(),
    );
    assert_eq!(at_exact.attestation_stats(), stats);

    for (config, kind, reported_limit) in [
        (
            RevisionAttestationLimitConfig {
                max_source_bytes: exact.max_source_bytes - 1,
                ..exact
            },
            DocumentLimitKind::AttestationSourceBytes,
            exact.max_source_bytes - 1,
        ),
        (
            RevisionAttestationLimitConfig {
                max_objects: exact.max_objects - 1,
                ..exact
            },
            DocumentLimitKind::AttestationObjects,
            exact.max_objects - 1,
        ),
        (
            RevisionAttestationLimitConfig {
                max_retained_evidence_bytes: exact.max_retained_evidence_bytes - 1,
                ..exact
            },
            DocumentLimitKind::AttestationEvidenceBytes,
            exact.max_retained_evidence_bytes - 1,
        ),
    ] {
        let error = AttestRevisionJob::new(
            candidate(&fixture),
            attestation_context(),
            RevisionAttestationLimits::validate(config).unwrap(),
            ObjectLimits::default(),
            SyntaxLimits::default(),
        )
        .unwrap_err();
        assert_limit(error, kind, reported_limit);
    }

    let config = RevisionAttestationLimitConfig {
        max_trivia_bytes: exact.max_trivia_bytes - 1,
        ..exact
    };
    let store = supplied_store(&fixture);
    let mut job = new_attestation_job(
        &fixture,
        RevisionAttestationLimits::validate(config).unwrap(),
    );
    let error = poll_failure(&mut job, &store, DocumentErrorCode::ResourceLimit);
    assert_limit(
        error,
        DocumentLimitKind::AttestationTriviaBytes,
        exact.max_trivia_bytes - 1,
    );
}

#[test]
fn aggregate_object_work_one_less_retains_both_parent_and_lower_limit_evidence() {
    let fixture = canonical_fixture();
    let (baseline, _) = ready(&fixture);
    let stats = baseline.attestation_stats();

    for (config, parent_kind, lower_kind, reported_limit) in [
        (
            RevisionAttestationLimitConfig {
                max_total_object_read_bytes: stats.object_read_bytes() - 1,
                ..RevisionAttestationLimitConfig::default()
            },
            DocumentLimitKind::AttestationObjectReadBytes,
            ObjectLimitKind::TotalReadBytes,
            stats.object_read_bytes() - 1,
        ),
        (
            RevisionAttestationLimitConfig {
                max_total_object_parse_bytes: stats.object_parse_bytes() - 1,
                ..RevisionAttestationLimitConfig::default()
            },
            DocumentLimitKind::AttestationObjectParseBytes,
            ObjectLimitKind::TotalParseBytes,
            stats.object_parse_bytes() - 1,
        ),
    ] {
        let store = supplied_store(&fixture);
        let mut job = new_attestation_job(
            &fixture,
            RevisionAttestationLimits::validate(config).unwrap(),
        );
        let error = poll_failure(&mut job, &store, DocumentErrorCode::ResourceLimit);
        let lower = error
            .object_error()
            .expect("aggregate object work errors retain the complete lower error");
        assert_eq!(lower.code(), ObjectErrorCode::ResourceLimit);
        assert_eq!(lower.limit().unwrap().kind(), lower_kind);
        assert_limit(error, parent_kind, reported_limit);
    }
}

#[test]
fn object_local_total_work_ties_are_not_misreported_as_parent_aggregate_limits() {
    let fixture = canonical_fixture();
    for (object_read, object_parse, parent_update, expected_lower_kind) in [
        (
            22,
            100,
            (
                22,
                RevisionAttestationLimitConfig::default().max_total_object_parse_bytes,
            ),
            ObjectLimitKind::TotalReadBytes,
        ),
        (
            100,
            22,
            (
                RevisionAttestationLimitConfig::default().max_total_object_read_bytes,
                22,
            ),
            ObjectLimitKind::TotalParseBytes,
        ),
    ] {
        let object_limits = ObjectLimits::validate(ObjectLimitConfig {
            max_source_bytes: u64::try_from(fixture.bytes.len()).unwrap(),
            initial_envelope_bytes: 1,
            max_envelope_bytes: 21,
            initial_boundary_bytes: 1,
            max_boundary_bytes: 1,
            max_stream_bytes: 1,
            max_total_read_bytes: object_read,
            max_total_parse_bytes: object_parse,
        })
        .unwrap();
        let parent_limits = limits(|config| {
            config.max_total_object_read_bytes = parent_update.0;
            config.max_total_object_parse_bytes = parent_update.1;
        });
        let mut job = AttestRevisionJob::new(
            candidate(&fixture),
            attestation_context(),
            parent_limits,
            object_limits,
            SyntaxLimits::default(),
        )
        .unwrap();
        let store = supplied_store(&fixture);
        let error = poll_failure(&mut job, &store, DocumentErrorCode::ResourceLimit);
        assert_eq!(error.category(), DocumentErrorCategory::Resource);
        assert!(
            error.limit().is_none(),
            "an equal child cap is not a parent aggregate limit"
        );
        let lower = error
            .object_error()
            .expect("object-local work failure retains its complete lower error");
        assert_eq!(lower.code(), ObjectErrorCode::ResourceLimit);
        assert_eq!(lower.limit().unwrap().kind(), expected_lower_kind);
    }
}

#[test]
fn single_comment_budget_accepts_exact_length_and_rejects_one_less() {
    let fixture = long_comment_fixture();
    let (attested, _) = ready_with_limits(&fixture, limits(|config| config.max_comment_bytes = 9));
    assert_eq!(attested.object_attestations().len(), 1);

    let store = supplied_store(&fixture);
    let mut job = new_attestation_job(&fixture, limits(|config| config.max_comment_bytes = 8));
    let error = poll_failure(&mut job, &store, DocumentErrorCode::ResourceLimit);
    assert_eq!(error.offset(), Some(9));
    assert_limit(error, DocumentLimitKind::AttestationCommentBytes, 8);
}
