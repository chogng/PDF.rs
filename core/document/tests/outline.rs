use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_document::{
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex, DocumentError,
    DocumentErrorCategory, DocumentErrorCode, DocumentLimitKind,
    NeverCancelled as DocumentNeverCancelled, OutlineJobContext, OutlineLimitConfig, OutlineLimits,
    OutlinePhase, OutlinePoll, OutlineTargetKind, ReadOutlineJob, RevisionAttestationJobContext,
    RevisionAttestationLimits, RevisionAttestationPoll, RevisionId, TextStringLimitKind,
};
use pdf_rs_object::{ObjectLimitKind, ObjectLimits};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(31);
const ATTEST_JOB: JobId = JobId::new(1_901);
const ATTEST_SCAN: ResumeCheckpoint = ResumeCheckpoint::new(1_902);
const ATTEST_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(1_903);
const ATTEST_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(1_904);
const OUTLINE_JOB: JobId = JobId::new(2_001);
const OUTLINE_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(2_002);
const OUTLINE_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(2_003);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

fn snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0xb3; 32]), SourceRevision::new(43)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xe9; 32]),
    )
}

fn fixture(bodies: &[(u32, &[u8])], size: u32) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut in_use = Vec::new();
    for &(number, body) in bodies {
        let offset = u64::try_from(bytes.len()).expect("fixture offset fits u64");
        in_use.push((number, offset));
        bytes.extend_from_slice(body);
    }
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
    }
}

fn base_catalog(outlines: &[u8]) -> Vec<u8> {
    let mut body = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R".to_vec();
    body.extend_from_slice(outlines);
    body.extend_from_slice(b" >>\nendobj\n");
    body
}

fn no_outline_fixture() -> Fixture {
    let catalog = base_catalog(b"");
    fixture(
        &[
            (1, catalog.as_slice()),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
            ),
        ],
        3,
    )
}

fn empty_outline_fixture() -> Fixture {
    let catalog = base_catalog(b" /Outlines 3 0 R");
    fixture(
        &[
            (1, catalog.as_slice()),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
            ),
            (3, b"3 0 obj\n<< /Type /Outlines >>\nendobj\n"),
        ],
        4,
    )
}

fn nested_outline_fixture() -> Fixture {
    let catalog = base_catalog(b" /Outlines 3 0 R");
    fixture(
        &[
            (1, catalog.as_slice()),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 6 0 R /Count 2 >>\nendobj\n",
            ),
            (
                4,
                b"4 0 obj\n<< /Title <546f7020a0> /Parent 3 0 R /Next 6 0 R /First 5 0 R /Last 5 0 R /Count -1 /Dest [2 0 R /Fit] >>\nendobj\n",
            ),
            (
                5,
                b"5 0 obj\n<< /Title <FEFFD83DDE80> /Parent 4 0 R >>\nendobj\n",
            ),
            (
                6,
                b"6 0 obj\n<< /Title (Second) /Parent 3 0 R /Prev 4 0 R /A << /S /Named /N /NextPage >> >>\nendobj\n",
            ),
        ],
        7,
    )
}

fn outlined_fixture(objects: &[(u32, &[u8])], size: u32) -> Fixture {
    let catalog = base_catalog(b" /Outlines 3 0 R");
    let mut bodies = vec![
        (1, catalog.as_slice()),
        (
            2,
            b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n".as_slice(),
        ),
    ];
    bodies.extend_from_slice(objects);
    fixture(&bodies, size)
}

fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("test object reference is nonzero")
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    store
        .supply(
            RangeResponse::new(
                fixture.snapshot,
                ByteRange::new(
                    0,
                    u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
                )
                .unwrap(),
                fixture.bytes.clone(),
            )
            .expect("fixture response matches its exact range"),
        )
        .expect("fixture range fits store limits");
    store
}

fn supply_range(store: &RangeStore, fixture: &Fixture, range: ByteRange) {
    let start = usize::try_from(range.start()).expect("fixture offset fits usize");
    let end = usize::try_from(range.end_exclusive()).expect("fixture offset fits usize");
    store
        .supply(
            RangeResponse::new(fixture.snapshot, range, fixture.bytes[start..end].to_vec())
                .expect("fixture response matches its exact range"),
        )
        .expect("fixture range fits store limits");
}

fn parsed_xref(fixture: &Fixture) -> XrefSection {
    let store = supplied_store(fixture);
    let mut job = OpenXrefJob::new(
        fixture.snapshot,
        XrefJobContext::new(
            JobId::new(1_801),
            ResumeCheckpoint::new(1_802),
            ResumeCheckpoint::new(1_803),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &XrefNeverCancelled) {
        XrefPoll::Ready(section) => section,
        XrefPoll::Pending { .. } => panic!("complete xref source must not suspend"),
        XrefPoll::Failed(error) => panic!("self-authored xref must parse: {error}"),
    }
}

fn candidate(fixture: &Fixture) -> CandidateRevisionIndex {
    CandidateRevisionIndex::from_xref(
        &parsed_xref(fixture),
        REVISION_ID,
        pdf_rs_document::DocumentLimits::default(),
        &DocumentNeverCancelled,
    )
    .expect("self-authored xref yields a candidate")
}

fn ready_index(fixture: &Fixture) -> AttestedRevisionIndex {
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
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Ready(index) => index,
        RevisionAttestationPoll::Pending { .. } => {
            panic!("complete attestation source must not suspend")
        }
        RevisionAttestationPoll::Failed(error) => {
            panic!("self-authored revision must attest: {error}")
        }
    }
}

fn context() -> OutlineJobContext {
    OutlineJobContext::new(
        OUTLINE_JOB,
        OUTLINE_ENVELOPE,
        OUTLINE_BOUNDARY,
        RequestPriority::Metadata,
    )
}

fn compact_limits() -> OutlineLimits {
    limits_with(8, 4, 4, 64, 64, 256, 256)
}

fn limits_with(
    max_items: u64,
    max_depth: u64,
    max_siblings_per_level: u64,
    max_title_input_bytes: u64,
    max_title_utf8_bytes: u64,
    max_total_title_input_bytes: u64,
    max_total_title_utf8_bytes: u64,
) -> OutlineLimits {
    full_limits(
        max_items,
        max_depth,
        max_siblings_per_level,
        max_title_input_bytes,
        max_title_utf8_bytes,
        max_total_title_input_bytes,
        max_total_title_utf8_bytes,
        1 << 20,
        1 << 20,
        64 << 10,
    )
}

#[allow(clippy::too_many_arguments)]
fn full_limits(
    max_items: u64,
    max_depth: u64,
    max_siblings_per_level: u64,
    max_title_input_bytes: u64,
    max_title_utf8_bytes: u64,
    max_total_title_input_bytes: u64,
    max_total_title_utf8_bytes: u64,
    max_total_object_read_bytes: u64,
    max_total_object_parse_bytes: u64,
    max_retained_bytes: u64,
) -> OutlineLimits {
    OutlineLimits::validate(OutlineLimitConfig {
        max_items,
        max_depth,
        max_siblings_per_level,
        max_title_input_bytes,
        max_title_utf8_bytes,
        max_total_title_input_bytes,
        max_total_title_utf8_bytes,
        max_total_object_read_bytes,
        max_total_object_parse_bytes,
        max_retained_bytes,
    })
    .expect("compact test limits validate")
}

fn exact_shape_limits(read: u64, parse: u64, retained: u64) -> OutlineLimits {
    full_limits(3, 2, 2, 6, 7, 17, 17, read, parse, retained)
}

fn outline_job<'index>(
    index: &'index AttestedRevisionIndex,
    limits: OutlineLimits,
) -> ReadOutlineJob<'index> {
    index.read_outline(context(), limits).unwrap()
}

fn poll_failure(job: &mut ReadOutlineJob<'_>, source: &dyn ByteSource) -> DocumentError {
    let failure = match job.poll(source, &DocumentNeverCancelled) {
        OutlinePoll::Failed(error) => error,
        OutlinePoll::Ready(_) => panic!("expected failure, got Ready"),
        OutlinePoll::Pending { .. } => panic!("complete or failing source must not pend"),
    };
    assert_eq!(job.phase(), OutlinePhase::Failed);
    match job.poll(source, &DocumentNeverCancelled) {
        OutlinePoll::Failed(repeated) => assert_eq!(repeated, failure),
        _ => panic!("terminal failure must replay the same error"),
    }
    failure
}

fn fixture_failure(fixture: &Fixture, limits: OutlineLimits) -> DocumentError {
    let index = ready_index(fixture);
    let store = supplied_store(fixture);
    let mut job = outline_job(&index, limits);
    poll_failure(&mut job, &store)
}

fn assert_syntax_failure(fixture: Fixture, expected: DocumentErrorCode) {
    let failure = fixture_failure(&fixture, compact_limits());
    assert_eq!(failure.code(), expected);
    assert_eq!(failure.category(), DocumentErrorCategory::Syntax);
}

fn assert_limit_failure(
    fixture: &Fixture,
    limits: OutlineLimits,
    expected: DocumentLimitKind,
) -> DocumentError {
    let failure = fixture_failure(fixture, limits);
    assert_eq!(failure.code(), DocumentErrorCode::ResourceLimit);
    let detail = failure
        .limit()
        .expect("resource failure retains outline limit detail");
    assert_eq!(detail.kind(), expected);
    assert!(detail.consumed() <= detail.limit());
    assert!(detail.attempted() > 0);
    failure
}

#[test]
fn absent_and_empty_outline_roots_are_ready_with_empty_results() {
    for (fixture, expected_root, expected_objects) in [
        (no_outline_fixture(), None, 1),
        (empty_outline_fixture(), Some(object_ref(3)), 2),
    ] {
        let index = ready_index(&fixture);
        let store = supplied_store(&fixture);
        let mut job = outline_job(&index, compact_limits());

        assert_eq!(job.snapshot(), fixture.snapshot);
        assert_eq!(job.context(), context());
        assert_eq!(job.limits(), compact_limits());
        assert_eq!(job.phase(), OutlinePhase::Catalog);

        let outline = match job.poll(&store, &DocumentNeverCancelled) {
            OutlinePoll::Ready(outline) => outline,
            OutlinePoll::Pending { .. } => panic!("complete empty outline must not suspend"),
            OutlinePoll::Failed(error) => panic!("empty outline must be valid: {error}"),
        };
        assert_eq!(job.phase(), OutlinePhase::Ready);
        assert_eq!(outline.root(), expected_root);
        assert_eq!(outline.root_count(), None);
        assert_eq!(outline.visible_items(), 0);
        assert!(outline.items().is_empty());
        assert_eq!(outline.catalog().root(), object_ref(1));
        assert_eq!(outline.catalog().pages(), object_ref(2));
        assert_eq!(outline.catalog().snapshot(), fixture.snapshot);
        assert_eq!(outline.stats(), job.stats());
        assert_eq!(outline.stats().objects_started(), expected_objects);
        assert_eq!(outline.stats().items_started(), 0);
        assert_eq!(outline.stats().title_input_bytes(), 0);
        assert_eq!(outline.stats().title_utf8_bytes(), 0);
    }
}

#[test]
fn nested_outline_is_preorder_bound_counted_and_redacted() {
    let fixture = nested_outline_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let limits = compact_limits();
    let mut job = outline_job(&index, limits);

    let outcome = job.poll(&store, &DocumentNeverCancelled);
    let poll_debug = format!("{outcome:?}");
    assert!(!poll_debug.contains("Top"));
    assert!(!poll_debug.contains("Second"));
    assert!(!poll_debug.contains('€'));
    assert!(!poll_debug.contains('🚀'));
    let outline = match outcome {
        OutlinePoll::Ready(outline) => outline,
        OutlinePoll::Pending { .. } => panic!("complete nested outline must not suspend"),
        OutlinePoll::Failed(error) => panic!("valid nested outline must parse: {error}"),
    };

    assert_eq!(job.phase(), OutlinePhase::Ready);
    assert_eq!(outline.root(), Some(object_ref(3)));
    assert_eq!(outline.root_count(), Some(2));
    assert_eq!(outline.visible_items(), 2);
    assert_eq!(outline.catalog().snapshot(), fixture.snapshot);
    assert_eq!(outline.catalog().revision_id(), REVISION_ID);
    assert_eq!(outline.catalog().revision_startxref(), index.startxref());
    assert_eq!(outline.catalog().root(), object_ref(1));
    assert_eq!(outline.catalog().pages(), object_ref(2));

    let items = outline.items();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].reference(), object_ref(4));
    assert_eq!(items[0].parent_index(), None);
    assert_eq!(items[0].depth(), 1);
    assert_eq!(items[0].title(), "Top €");
    assert_eq!(items[0].declared_count(), Some(-1));
    assert_eq!(items[0].count(), -1);
    assert_eq!(items[0].target_kind(), OutlineTargetKind::Destination);
    assert_eq!(items[0].direct_children(), 1);
    assert_eq!(items[0].visible_descendants(), 0);
    assert_eq!(items[0].visible_descendants_if_open(), 1);

    assert_eq!(items[1].reference(), object_ref(5));
    assert_eq!(items[1].parent_index(), Some(0));
    assert_eq!(items[1].depth(), 2);
    assert_eq!(items[1].title(), "🚀");
    assert_eq!(items[1].declared_count(), None);
    assert_eq!(items[1].count(), 0);
    assert_eq!(items[1].target_kind(), OutlineTargetKind::None);
    assert_eq!(items[1].direct_children(), 0);
    assert_eq!(items[1].visible_descendants(), 0);

    assert_eq!(items[2].reference(), object_ref(6));
    assert_eq!(items[2].parent_index(), None);
    assert_eq!(items[2].depth(), 1);
    assert_eq!(items[2].title(), "Second");
    assert_eq!(items[2].declared_count(), None);
    assert_eq!(items[2].count(), 0);
    assert_eq!(items[2].target_kind(), OutlineTargetKind::Action);
    assert_eq!(items[2].direct_children(), 0);
    assert_eq!(items[2].visible_descendants(), 0);

    let stats = outline.stats();
    assert_eq!(stats, job.stats());
    assert_eq!(stats.objects_started(), 5);
    assert_eq!(stats.items_started(), 3);
    assert_eq!(stats.max_depth(), 2);
    assert_eq!(stats.max_siblings_per_level(), 2);
    assert_eq!(stats.title_input_bytes(), 17);
    assert_eq!(stats.title_utf8_bytes(), 17);
    assert!(stats.title_reserved_utf8_bytes() >= stats.title_utf8_bytes());
    assert!(stats.reserved_working_bytes() > 0);
    assert!(stats.reserved_result_bytes() >= stats.title_reserved_utf8_bytes());
    assert_eq!(stats.peak_retained_bytes(), stats.reserved_bytes());
    assert!(stats.object_read_bytes() > 0);
    assert!(stats.object_parse_bytes() > 0);
    assert!(stats.reserved_bytes() > 0);

    let result_debug = format!("{outline:?}");
    assert!(!result_debug.contains("Top"));
    assert!(!result_debug.contains("Second"));
    assert!(!result_debug.contains('€'));
    assert!(!result_debug.contains('🚀'));

    match job.poll(&store, &DocumentNeverCancelled) {
        OutlinePoll::Failed(error) => {
            assert_eq!(error.code(), DocumentErrorCode::JobAlreadyComplete)
        }
        _ => panic!("a completed one-shot outline job must reject repoll"),
    }
    assert_eq!(job.phase(), OutlinePhase::Ready);
    assert_eq!(job.stats(), stats);
}

#[test]
fn pending_repolls_replay_requests_and_do_not_double_charge_stats() {
    let fixture = nested_outline_fixture();
    let index = ready_index(&fixture);
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = outline_job(&index, compact_limits());
    let mut pending_polls = 0;

    loop {
        match job.poll(&store, &DocumentNeverCancelled) {
            OutlinePoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                pending_polls += 1;
                let charged = job.stats();
                match job.poll(&store, &DocumentNeverCancelled) {
                    OutlinePoll::Pending {
                        ticket: repeated_ticket,
                        missing: repeated_missing,
                        checkpoint: repeated_checkpoint,
                    } => {
                        assert_eq!(repeated_ticket, ticket);
                        assert_eq!(repeated_missing, missing);
                        assert_eq!(repeated_checkpoint, checkpoint);
                    }
                    _ => panic!("unchanged source must replay the same Pending request"),
                }
                assert_eq!(job.stats(), charged);
                for range in missing.as_slice() {
                    supply_range(&store, &fixture, *range);
                }
            }
            OutlinePoll::Ready(outline) => {
                assert_eq!(outline.items().len(), 3);
                assert_eq!(outline.stats(), job.stats());
                break;
            }
            OutlinePoll::Failed(error) => {
                panic!("incrementally supplied valid outline must finish: {error}")
            }
        }
    }
    assert!(pending_polls >= 2);
}

#[test]
fn shape_and_aggregate_title_limits_accept_exact_values_and_reject_one_less() {
    let fixture = nested_outline_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let exact = exact_shape_limits(1 << 20, 1 << 20, 64 << 10);
    let mut exact_job = outline_job(&index, exact);
    let exact_outline = match exact_job.poll(&store, &DocumentNeverCancelled) {
        OutlinePoll::Ready(outline) => outline,
        OutlinePoll::Pending { .. } => panic!("complete exact-limit source must not suspend"),
        OutlinePoll::Failed(error) => panic!("exact outline limits must pass: {error}"),
    };
    assert_eq!(exact_outline.items().len(), 3);
    assert_eq!(exact_outline.stats().max_depth(), 2);
    assert_eq!(exact_outline.stats().max_siblings_per_level(), 2);
    assert_eq!(exact_outline.stats().title_input_bytes(), 17);
    assert_eq!(exact_outline.stats().title_utf8_bytes(), 17);

    for (limits, kind, lower_kind) in [
        (
            full_limits(3, 2, 2, 5, 7, 17, 17, 1 << 20, 1 << 20, 64 << 10),
            DocumentLimitKind::OutlineTitleInputBytes,
            TextStringLimitKind::InputBytes,
        ),
        (
            full_limits(3, 2, 2, 6, 6, 17, 17, 1 << 20, 1 << 20, 64 << 10),
            DocumentLimitKind::OutlineTitleUtf8Bytes,
            TextStringLimitKind::Utf8Bytes,
        ),
    ] {
        let failure = assert_limit_failure(&fixture, limits, kind);
        let lower = failure
            .text_string_error()
            .expect("per-title failure retains decoder detail");
        assert_eq!(lower.limit().unwrap().kind(), lower_kind);
    }

    for (limits, kind, ceiling) in [
        (
            full_limits(2, 2, 2, 6, 7, 17, 17, 1 << 20, 1 << 20, 64 << 10),
            DocumentLimitKind::OutlineItems,
            2,
        ),
        (
            full_limits(3, 1, 2, 6, 7, 17, 17, 1 << 20, 1 << 20, 64 << 10),
            DocumentLimitKind::OutlineDepth,
            1,
        ),
        (
            full_limits(3, 2, 1, 6, 7, 17, 17, 1 << 20, 1 << 20, 64 << 10),
            DocumentLimitKind::OutlineSiblings,
            1,
        ),
        (
            full_limits(3, 2, 2, 6, 7, 16, 17, 1 << 20, 1 << 20, 64 << 10),
            DocumentLimitKind::OutlineTotalTitleInputBytes,
            16,
        ),
        (
            full_limits(3, 2, 2, 6, 7, 17, 16, 1 << 20, 1 << 20, 64 << 10),
            DocumentLimitKind::OutlineTotalTitleUtf8Bytes,
            16,
        ),
    ] {
        let failure = assert_limit_failure(&fixture, limits, kind);
        assert_eq!(failure.limit().unwrap().limit(), ceiling);
    }
}

#[test]
fn aggregate_object_work_accepts_measured_values_and_rejects_one_less() {
    let fixture = nested_outline_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let mut baseline = outline_job(&index, exact_shape_limits(1 << 20, 1 << 20, 64 << 10));
    let baseline_stats = match baseline.poll(&store, &DocumentNeverCancelled) {
        OutlinePoll::Ready(outline) => outline.stats(),
        OutlinePoll::Pending { .. } => panic!("complete baseline source must not suspend"),
        OutlinePoll::Failed(error) => panic!("baseline outline must pass: {error}"),
    };
    let read = baseline_stats.object_read_bytes();
    let parse = baseline_stats.object_parse_bytes();
    assert!(read > 1);
    assert!(parse > 1);

    let mut exact = outline_job(&index, exact_shape_limits(read, parse, 64 << 10));
    let exact_stats = match exact.poll(&store, &DocumentNeverCancelled) {
        OutlinePoll::Ready(outline) => outline.stats(),
        OutlinePoll::Pending { .. } => panic!("complete exact-work source must not suspend"),
        OutlinePoll::Failed(error) => panic!("exact measured work must pass: {error}"),
    };
    assert_eq!(exact_stats.object_read_bytes(), read);
    assert_eq!(exact_stats.object_parse_bytes(), parse);

    let read_error = assert_limit_failure(
        &fixture,
        exact_shape_limits(read - 1, parse, 64 << 10),
        DocumentLimitKind::OutlineObjectReadBytes,
    );
    assert_eq!(
        read_error
            .object_error()
            .and_then(|error| error.limit())
            .map(pdf_rs_object::ObjectLimit::kind),
        Some(ObjectLimitKind::TotalReadBytes)
    );
    let parse_error = assert_limit_failure(
        &fixture,
        exact_shape_limits(read, parse - 1, 64 << 10),
        DocumentLimitKind::OutlineObjectParseBytes,
    );
    assert_eq!(
        parse_error
            .object_error()
            .and_then(|error| error.limit())
            .map(pdf_rs_object::ObjectLimit::kind),
        Some(ObjectLimitKind::TotalParseBytes)
    );
}

#[test]
fn retained_capacity_accepts_exact_measurement_and_rejects_one_less() {
    let fixture = nested_outline_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let mut baseline = outline_job(&index, exact_shape_limits(1 << 20, 1 << 20, 64 << 10));
    let retained = match baseline.poll(&store, &DocumentNeverCancelled) {
        OutlinePoll::Ready(outline) => outline.stats().reserved_bytes(),
        OutlinePoll::Pending { .. } => panic!("complete retained baseline must not suspend"),
        OutlinePoll::Failed(error) => panic!("retained baseline must pass: {error}"),
    };
    assert!(retained > 1);

    let exact_limits = exact_shape_limits(1 << 20, 1 << 20, retained);
    let mut exact = outline_job(&index, exact_limits);
    let exact_outline = match exact.poll(&store, &DocumentNeverCancelled) {
        OutlinePoll::Ready(outline) => outline,
        OutlinePoll::Pending { .. } => panic!("complete exact-retained source must not suspend"),
        OutlinePoll::Failed(error) => panic!("exact retained capacity must pass: {error}"),
    };
    assert_eq!(exact_outline.stats().reserved_bytes(), retained);

    let low_limits = exact_shape_limits(1 << 20, 1 << 20, retained - 1);
    let error = match index.read_outline(context(), low_limits) {
        Err(error) => error,
        Ok(mut job) => poll_failure(&mut job, &store),
    };
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    let detail = error.limit().expect("retained failure has limit detail");
    assert_eq!(detail.kind(), DocumentLimitKind::OutlineRetainedBytes);
    assert_eq!(detail.limit(), retained - 1);
    assert!(detail.attempted() > 0);
}

#[test]
fn malformed_title_errors_redact_title_from_debug_and_display() {
    let secret = "outline-title-private";
    let fixture = outlined_fixture(
        &[
            (
                3,
                b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n",
            ),
            (
                4,
                b"4 0 obj\n<< /Title (outline-title-private\\177) /Parent 3 0 R >>\nendobj\n",
            ),
        ],
        5,
    );
    let error = fixture_failure(&fixture, compact_limits());
    assert_eq!(error.code(), DocumentErrorCode::InvalidOutlineTitle);
    assert!(!format!("{error:?}").contains(secret));
    assert!(!format!("{error}").contains(secret));
    let lower = error
        .text_string_error()
        .expect("malformed title retains redacted decoder context");
    assert!(!format!("{lower:?}").contains(secret));
    assert!(!format!("{lower}").contains(secret));
}

#[test]
fn root_and_item_first_last_pairs_are_strict() {
    assert_syntax_failure(
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Count 1 >>\nendobj\n",
                ),
                (4, b"4 0 obj\n<< /Title (Only) /Parent 3 0 R >>\nendobj\n"),
            ],
            5,
        ),
        DocumentErrorCode::InvalidOutlineDictionary,
    );
    assert_syntax_failure(
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (Parent) /Parent 3 0 R /First 5 0 R /Count -1 >>\nendobj\n",
                ),
                (
                    5,
                    b"5 0 obj\n<< /Title (Child) /Parent 4 0 R >>\nendobj\n",
                ),
            ],
            6,
        ),
        DocumentErrorCode::InvalidOutlineItem,
    );
}

#[test]
fn parent_prev_next_and_declared_last_must_close_each_sibling_chain() {
    let cases = [
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (Wrong parent) /Parent 2 0 R >>\nendobj\n",
                ),
            ],
            5,
        ),
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 5 0 R /Count 2 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (First) /Parent 3 0 R /Next 5 0 R >>\nendobj\n",
                ),
                (5, b"5 0 obj\n<< /Title (Second) /Parent 3 0 R >>\nendobj\n"),
            ],
            6,
        ),
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 5 0 R /Count 2 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (First) /Parent 3 0 R /Next 5 0 R >>\nendobj\n",
                ),
                (
                    5,
                    b"5 0 obj\n<< /Title (Second) /Parent 3 0 R /Prev 6 0 R >>\nendobj\n",
                ),
                (6, b"6 0 obj\n<< /Title (Other) /Parent 3 0 R >>\nendobj\n"),
            ],
            7,
        ),
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 5 0 R /Count 1 >>\nendobj\n",
                ),
                (4, b"4 0 obj\n<< /Title (Only) /Parent 3 0 R >>\nendobj\n"),
                (
                    5,
                    b"5 0 obj\n<< /Title (Unreached) /Parent 3 0 R >>\nendobj\n",
                ),
            ],
            6,
        ),
    ];

    let expected = [
        DocumentErrorCode::OutlineParentMismatch,
        DocumentErrorCode::OutlineSiblingMismatch,
        DocumentErrorCode::OutlineSiblingMismatch,
        DocumentErrorCode::OutlineSiblingMismatch,
    ];
    for (fixture, expected) in cases.into_iter().zip(expected) {
        assert_syntax_failure(fixture, expected);
    }
}

#[test]
fn active_cycles_and_completed_cross_parent_duplicates_are_distinct() {
    assert_syntax_failure(
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 6 0 R /Count 2 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (First) /Parent 3 0 R /Next 5 0 R >>\nendobj\n",
                ),
                (
                    5,
                    b"5 0 obj\n<< /Title (Second) /Parent 3 0 R /Prev 4 0 R /Next 4 0 R >>\nendobj\n",
                ),
                (6, b"6 0 obj\n<< /Title (Last) /Parent 3 0 R >>\nendobj\n"),
            ],
            7,
        ),
        DocumentErrorCode::OutlineCycle,
    );

    let tight_cycle = outlined_fixture(
        &[
            (
                3,
                b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 6 0 R /Count 2 >>\nendobj\n",
            ),
            (
                4,
                b"4 0 obj\n<< /Title (First) /Parent 3 0 R /Next 5 0 R >>\nendobj\n",
            ),
            (
                5,
                b"5 0 obj\n<< /Title (Second) /Parent 3 0 R /Prev 4 0 R /Next 4 0 R >>\nendobj\n",
            ),
            (6, b"6 0 obj\n<< /Title (Last) /Parent 3 0 R >>\nendobj\n"),
        ],
        7,
    );
    let failure = fixture_failure(&tight_cycle, limits_with(2, 2, 2, 64, 64, 256, 256));
    assert_eq!(failure.code(), DocumentErrorCode::OutlineCycle);

    assert_syntax_failure(
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 2 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (Top) /Parent 3 0 R /First 5 0 R /Last 5 0 R /Count 1 >>\nendobj\n",
                ),
                (
                    5,
                    b"5 0 obj\n<< /Title (Child) /Parent 4 0 R /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n",
                ),
            ],
            6,
        ),
        DocumentErrorCode::OutlineCycle,
    );

    assert_syntax_failure(
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 6 0 R /Count 2 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (First) /Parent 3 0 R /Next 6 0 R /First 5 0 R /Last 5 0 R /Count -1 >>\nendobj\n",
                ),
                (
                    5,
                    b"5 0 obj\n<< /Title (Shared) /Parent 4 0 R >>\nendobj\n",
                ),
                (
                    6,
                    b"6 0 obj\n<< /Title (Second) /Parent 3 0 R /Prev 4 0 R /First 5 0 R /Last 5 0 R /Count -1 >>\nendobj\n",
                ),
            ],
            7,
        ),
        DocumentErrorCode::DuplicateOutlineItem,
    );

    let tight_duplicate = outlined_fixture(
        &[
            (
                3,
                b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 6 0 R /Count 2 >>\nendobj\n",
            ),
            (
                4,
                b"4 0 obj\n<< /Title (First) /Parent 3 0 R /Next 6 0 R /First 5 0 R /Last 5 0 R /Count -1 >>\nendobj\n",
            ),
            (
                5,
                b"5 0 obj\n<< /Title (Shared) /Parent 4 0 R >>\nendobj\n",
            ),
            (
                6,
                b"6 0 obj\n<< /Title (Second) /Parent 3 0 R /Prev 4 0 R /First 5 0 R /Last 5 0 R /Count -1 >>\nendobj\n",
            ),
        ],
        7,
    );
    let failure = fixture_failure(&tight_duplicate, limits_with(3, 2, 2, 64, 64, 256, 256));
    assert_eq!(failure.code(), DocumentErrorCode::DuplicateOutlineItem);
}

#[test]
fn declared_counts_obey_sign_subtree_and_root_visibility_formulas() {
    assert_syntax_failure(
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count -1 >>\nendobj\n",
                ),
                (4, b"4 0 obj\n<< /Title (Only) /Parent 3 0 R >>\nendobj\n"),
            ],
            5,
        ),
        DocumentErrorCode::InvalidOutlineDictionary,
    );

    for fixture in [
        outlined_fixture(
            &[(3, b"3 0 obj\n<< /Type /Outlines /Count 0 >>\nendobj\n")],
            4,
        ),
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R >>\nendobj\n",
                ),
                (4, b"4 0 obj\n<< /Title (Only) /Parent 3 0 R >>\nendobj\n"),
            ],
            5,
        ),
    ] {
        assert_syntax_failure(fixture, DocumentErrorCode::OutlineCountMismatch);
    }

    let cases = [
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 3 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (Top) /Parent 3 0 R /First 5 0 R /Last 5 0 R /Count 1 >>\nendobj\n",
                ),
                (
                    5,
                    b"5 0 obj\n<< /Title (Child) /Parent 4 0 R >>\nendobj\n",
                ),
            ],
            6,
        ),
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 2 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (Top) /Parent 3 0 R /First 5 0 R /Last 5 0 R /Count -2 >>\nendobj\n",
                ),
                (
                    5,
                    b"5 0 obj\n<< /Title (Child) /Parent 4 0 R >>\nendobj\n",
                ),
            ],
            6,
        ),
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (Top) /Parent 3 0 R /First 5 0 R /Last 5 0 R >>\nendobj\n",
                ),
                (
                    5,
                    b"5 0 obj\n<< /Title (Child) /Parent 4 0 R >>\nendobj\n",
                ),
            ],
            6,
        ),
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (Leaf) /Parent 3 0 R /Count 1 >>\nendobj\n",
                ),
            ],
            5,
        ),
    ];

    for fixture in cases {
        assert_syntax_failure(fixture, DocumentErrorCode::OutlineCountMismatch);
    }

    let leaf_zero = outlined_fixture(
        &[
            (
                3,
                b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n",
            ),
            (
                4,
                b"4 0 obj\n<< /Title (Leaf) /Parent 3 0 R /Count 0 >>\nendobj\n",
            ),
        ],
        5,
    );
    let index = ready_index(&leaf_zero);
    let store = supplied_store(&leaf_zero);
    let mut job = outline_job(&index, compact_limits());
    let outline = match job.poll(&store, &DocumentNeverCancelled) {
        OutlinePoll::Ready(outline) => outline,
        OutlinePoll::Pending { .. } => panic!("complete leaf-zero source must not suspend"),
        OutlinePoll::Failed(error) => panic!("leaf Count zero is formula-consistent: {error}"),
    };
    assert_eq!(outline.items()[0].declared_count(), Some(0));
}

#[test]
fn title_and_activation_targets_are_strict_and_direct_only() {
    assert_syntax_failure(
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n",
                ),
                (4, b"4 0 obj\n<< /Title <7f> /Parent 3 0 R >>\nendobj\n"),
            ],
            5,
        ),
        DocumentErrorCode::InvalidOutlineTitle,
    );
    assert_syntax_failure(
        outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n",
                ),
                (
                    4,
                    b"4 0 obj\n<< /Title (Conflict) /Parent 3 0 R /Dest /Here /A << /S /Named /N /NextPage >> >>\nendobj\n",
                ),
            ],
            5,
        ),
        DocumentErrorCode::InvalidOutlineTarget,
    );

    let indirect_root_type = outlined_fixture(
        &[
            (3, b"3 0 obj\n<< /Type 5 0 R >>\nendobj\n"),
            (5, b"5 0 obj\n/Outlines\nendobj\n"),
        ],
        6,
    );
    let failure = fixture_failure(&indirect_root_type, compact_limits());
    assert_eq!(
        failure.code(),
        DocumentErrorCode::UnsupportedOutlineRepresentation
    );
    assert_eq!(failure.category(), DocumentErrorCategory::Unsupported);

    for (item, target) in [
        (
            b"4 0 obj\n<< /Title 5 0 R /Parent 3 0 R >>\nendobj\n".as_slice(),
            b"5 0 obj\n(Indirect title)\nendobj\n".as_slice(),
        ),
        (
            b"4 0 obj\n<< /Title (Indirect count) /Parent 3 0 R /Count 5 0 R >>\nendobj\n",
            b"5 0 obj\n0\nendobj\n",
        ),
        (
            b"4 0 obj\n<< /Title (Indirect dest) /Parent 3 0 R /Dest 5 0 R >>\nendobj\n",
            b"5 0 obj\n/Here\nendobj\n",
        ),
        (
            b"4 0 obj\n<< /Title (Indirect action) /Parent 3 0 R /A 5 0 R >>\nendobj\n",
            b"5 0 obj\n<< /S /Named /N /NextPage >>\nendobj\n",
        ),
    ] {
        let fixture = outlined_fixture(
            &[
                (
                    3,
                    b"3 0 obj\n<< /Type /Outlines /First 4 0 R /Last 4 0 R /Count 1 >>\nendobj\n",
                ),
                (4, item),
                (5, target),
            ],
            6,
        );
        let failure = fixture_failure(&fixture, compact_limits());
        assert_eq!(
            failure.code(),
            DocumentErrorCode::UnsupportedOutlineRepresentation
        );
        assert_eq!(failure.category(), DocumentErrorCategory::Unsupported);
    }
}
