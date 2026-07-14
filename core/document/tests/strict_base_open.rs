use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
    SupplyOutcome,
};
use pdf_rs_document::{
    DocumentErrorCategory, DocumentErrorCode, DocumentLimits,
    NeverCancelled as DocumentNeverCancelled, OpenStrictBaseRevisionJob,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionId, StrictBaseOpenContext,
    StrictBaseOpenLimits, StrictBaseOpenPhase, StrictBaseOpenPoll,
};
use pdf_rs_object::{ObjectLimitConfig, ObjectLimits};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{XrefErrorCode, XrefJobContext, XrefLimitConfig, XrefLimits, XrefPhase};

const REVISION_ID: RevisionId = RevisionId::new(7);
const OPEN_JOB: JobId = JobId::new(401);
const TAIL_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(402);
const SECTION_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(403);
const SCAN_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(404);
const ENVELOPE_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(405);
const BOUNDARY_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(406);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    startxref: u64,
}

fn snapshot(len: Option<u64>, tag: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([tag; 32]),
            SourceRevision::new(u64::from(tag)),
        ),
        len,
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [tag.wrapping_add(1); 32],
        ),
    )
}

fn fixture(prexref: &[u8], size: u32, in_use: &[(u32, u64)], tag: u8) -> Fixture {
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
    let source_len = u64::try_from(bytes.len()).expect("fixture length fits u64");
    Fixture {
        bytes,
        snapshot: snapshot(Some(source_len), tag),
        startxref,
    }
}

fn canonical_fixture() -> Fixture {
    let result = fixture(b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n", 2, &[(1, 9)], 0x61);
    assert_eq!(result.startxref, 29);
    assert_eq!(result.bytes.len(), 131);
    result
}

fn large_stream_fixture() -> Fixture {
    let payload = vec![b'P'; 8192];
    let mut prexref = b"%PDF-1.7\n1 0 obj\n<< /Length 8192 >>\nstream\n".to_vec();
    prexref.extend_from_slice(&payload);
    prexref.extend_from_slice(b"\nendstream\nendobj\n");
    fixture(&prexref, 2, &[(1, 9)], 0x62)
}

fn invalid_header_fixture() -> Fixture {
    fixture(b"%PDF-1.x\n1 0 obj\n<<>>\nendobj\n", 2, &[(1, 9)], 0x63)
}

fn duplicate_offset_fixture() -> Fixture {
    fixture(
        b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n",
        3,
        &[(1, 9), (2, 9)],
        0x64,
    )
}

fn context() -> StrictBaseOpenContext {
    StrictBaseOpenContext::new(
        XrefJobContext::new(OPEN_JOB, TAIL_CHECKPOINT, SECTION_CHECKPOINT),
        RevisionAttestationJobContext::new(
            OPEN_JOB,
            SCAN_CHECKPOINT,
            ENVELOPE_CHECKPOINT,
            BOUNDARY_CHECKPOINT,
            RequestPriority::VisiblePage,
        ),
    )
}

fn compact_xref_limits(source_len: u64) -> XrefLimits {
    XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: source_len,
        initial_tail_bytes: 32,
        max_tail_bytes: 64,
        initial_section_bytes: 64,
        max_section_bytes: 256,
        max_total_read_bytes: 320,
        max_total_parse_bytes: 320,
        max_subsections: 4,
        max_entries: 4,
    })
    .expect("large-stream fixture fits the compact xref profile")
}

fn stream_object_limits(source_len: u64) -> ObjectLimits {
    ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: 4096,
        max_envelope_bytes: 4096,
        initial_boundary_bytes: 256,
        max_boundary_bytes: 256,
        max_stream_bytes: source_len,
        max_total_read_bytes: 4352,
        max_total_parse_bytes: 4352,
    })
    .expect("large-stream fixture fits the compact object profile")
}

fn limits(_fixture: &Fixture) -> StrictBaseOpenLimits {
    StrictBaseOpenLimits::new(
        XrefLimits::default(),
        DocumentLimits::default(),
        RevisionAttestationLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
}

fn stream_limits(fixture: &Fixture) -> StrictBaseOpenLimits {
    let source_len = u64::try_from(fixture.bytes.len()).expect("fixture length fits u64");
    StrictBaseOpenLimits::new(
        compact_xref_limits(source_len),
        DocumentLimits::default(),
        RevisionAttestationLimits::default(),
        stream_object_limits(source_len),
        SyntaxLimits::default(),
    )
}

fn new_job(fixture: &Fixture, limits: StrictBaseOpenLimits) -> OpenStrictBaseRevisionJob {
    OpenStrictBaseRevisionJob::new(fixture.snapshot, REVISION_ID, context(), limits)
        .expect("fixture strict base-open configuration is valid")
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let range = ByteRange::new(
        0,
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
    )
    .unwrap();
    store
        .supply(
            RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone())
                .expect("complete fixture response matches its range"),
        )
        .expect("complete fixture fits the default Range store");
    store
}

fn supply_range(store: &RangeStore, fixture: &Fixture, range: ByteRange) -> SupplyOutcome {
    let start = usize::try_from(range.start()).expect("fixture offset fits usize");
    let end = usize::try_from(range.end_exclusive()).expect("fixture end fits usize");
    store
        .supply(
            RangeResponse::new(fixture.snapshot, range, fixture.bytes[start..end].to_vec())
                .expect("fixture response matches its range"),
        )
        .expect("fixture range fits the default Range store")
}

#[test]
fn complete_input_publishes_only_the_attested_typestate_and_exact_stats() {
    let fixture = canonical_fixture();
    let store = supplied_store(&fixture);
    let configured_limits = limits(&fixture);
    let mut job = new_job(&fixture, configured_limits);

    assert_eq!(job.snapshot(), fixture.snapshot);
    assert_eq!(job.revision_id(), REVISION_ID);
    assert_eq!(job.context(), context());
    assert_eq!(job.limits(), configured_limits);
    assert_eq!(job.phase(), StrictBaseOpenPhase::Xref(XrefPhase::Tail));
    assert_eq!(job.stats().xref().read_bytes(), 0);
    assert_eq!(job.stats().index(), None);
    assert_eq!(job.stats().attestation().objects_attested(), 0);

    let attested = match job.poll(&store, &DocumentNeverCancelled) {
        StrictBaseOpenPoll::Ready(index) => index,
        other => panic!("complete canonical fixture must open, got {other:?}"),
    };
    assert_eq!(job.phase(), StrictBaseOpenPhase::Ready);
    assert_eq!(attested.snapshot(), fixture.snapshot);
    assert_eq!(attested.revision_id(), REVISION_ID);
    assert_eq!(attested.startxref(), fixture.startxref);
    assert_eq!(attested.root(), ObjectRef::new(1, 0).unwrap());
    assert_eq!(attested.object_attestations().len(), 1);

    let stats = job.stats();
    assert_eq!(stats.xref().entries(), 2);
    assert_eq!(stats.index(), Some(attested.index_stats()));
    assert_eq!(stats.index().unwrap().total_entries(), 2);
    assert_eq!(stats.index().unwrap().in_use_entries(), 1);
    assert_eq!(stats.attestation(), attested.attestation_stats());
    assert_eq!(stats.attestation().objects_attested(), 1);

    let repeated = match job.poll(&store, &DocumentNeverCancelled) {
        StrictBaseOpenPoll::Failed(error) => error,
        other => panic!("a successful one-shot open must not publish twice: {other:?}"),
    };
    assert_eq!(
        repeated.document().unwrap().code(),
        DocumentErrorCode::JobAlreadyComplete
    );
    assert_eq!(job.phase(), StrictBaseOpenPhase::Ready);
    assert_eq!(job.stats(), stats);
}

#[test]
fn pending_replay_and_reverse_physical_supply_cover_all_five_checkpoints() {
    let fixture = large_stream_fixture();
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = new_job(&fixture, stream_limits(&fixture));
    let mut checkpoints = Vec::new();
    let mut polls = 0_u32;
    let mut replay_checked = false;

    let attested = loop {
        polls += 1;
        assert!(polls < 64, "strict base opening must make bounded progress");
        match job.poll(&store, &DocumentNeverCancelled) {
            StrictBaseOpenPoll::Ready(index) => break index,
            StrictBaseOpenPoll::Failed(error) => {
                panic!("reverse physical supplies must eventually open: {error}")
            }
            StrictBaseOpenPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                checkpoints.push(checkpoint);
                let phase_before_supply = job.phase();
                let stats_before_replay = job.stats();
                if !replay_checked {
                    match job.poll(&store, &DocumentNeverCancelled) {
                        StrictBaseOpenPoll::Pending {
                            ticket: repeated_ticket,
                            missing: repeated_missing,
                            checkpoint: repeated_checkpoint,
                        } => {
                            assert_eq!(repeated_ticket, ticket);
                            assert_eq!(repeated_missing, missing);
                            assert_eq!(repeated_checkpoint, checkpoint);
                        }
                        other => panic!("an absent range must replay Pending: {other:?}"),
                    }
                    assert_eq!(job.stats(), stats_before_replay);
                    replay_checked = true;
                }

                let mut woke = false;
                for range in missing.as_slice().iter().copied().rev() {
                    if range.len() > 1 {
                        let lower_len = range.len() / 2;
                        let upper =
                            ByteRange::new(range.start() + lower_len, range.len() - lower_len)
                                .unwrap();
                        let upper_outcome = supply_range(&store, &fixture, upper);
                        assert!(
                            !upper_outcome.ready_tickets().contains(&ticket),
                            "an upper-half response must not wake a ticket missing its lower half"
                        );
                        assert_eq!(job.phase(), phase_before_supply);
                        let lower = ByteRange::new(range.start(), lower_len).unwrap();
                        woke |= supply_range(&store, &fixture, lower)
                            .ready_tickets()
                            .contains(&ticket);
                    } else {
                        woke |= supply_range(&store, &fixture, range)
                            .ready_tickets()
                            .contains(&ticket);
                    }
                }
                assert!(
                    woke,
                    "covering all missing bytes must make the ticket ready"
                );
                assert_eq!(
                    job.phase(),
                    phase_before_supply,
                    "Range supply must never resume parser work inline"
                );
                let subscriptions = store.take_subscriptions(ticket).unwrap();
                assert_eq!(subscriptions.len(), 1);
                assert_eq!(subscriptions[0].job(), OPEN_JOB);
                assert_eq!(subscriptions[0].checkpoint(), checkpoint);
                assert!(store.take_subscriptions(ticket).unwrap().is_empty());
                store.release_ticket(ticket).unwrap();
            }
        }
    };

    for expected in [
        TAIL_CHECKPOINT,
        SECTION_CHECKPOINT,
        SCAN_CHECKPOINT,
        ENVELOPE_CHECKPOINT,
        BOUNDARY_CHECKPOINT,
    ] {
        assert!(
            checkpoints.contains(&expected),
            "checkpoint {expected:?} must be observed in {checkpoints:?}"
        );
    }
    assert_eq!(job.phase(), StrictBaseOpenPhase::Ready);
    assert_eq!(job.stats().xref().entries(), 2);
    assert_eq!(job.stats().index().unwrap().in_use_entries(), 1);
    assert_eq!(job.stats().attestation().objects_attested(), 1);
    assert_eq!(attested.object_attestations().len(), 1);
    assert!(matches!(
        attested.object_attestations()[0].kind(),
        pdf_rs_document::ObjectAttestationKind::Stream { data_span, .. }
            if data_span.len() == 8192
    ));
}

#[test]
fn cross_phase_context_conflicts_are_rejected_before_child_construction() {
    let fixture = canonical_fixture();
    let mismatched_job = StrictBaseOpenContext::new(
        context().xref(),
        RevisionAttestationJobContext::new(
            JobId::new(999),
            SCAN_CHECKPOINT,
            ENVELOPE_CHECKPOINT,
            BOUNDARY_CHECKPOINT,
            RequestPriority::Metadata,
        ),
    );
    let duplicate_checkpoint = StrictBaseOpenContext::new(
        context().xref(),
        RevisionAttestationJobContext::new(
            OPEN_JOB,
            TAIL_CHECKPOINT,
            ENVELOPE_CHECKPOINT,
            BOUNDARY_CHECKPOINT,
            RequestPriority::Metadata,
        ),
    );

    for invalid in [mismatched_job, duplicate_checkpoint] {
        let error = OpenStrictBaseRevisionJob::new(
            fixture.snapshot,
            REVISION_ID,
            invalid,
            limits(&fixture),
        )
        .unwrap_err();
        let document = error
            .document()
            .expect("cross-phase errors are document errors");
        assert_eq!(
            document.code(),
            DocumentErrorCode::InvalidStrictBaseOpenContext
        );
        assert_eq!(document.category(), DocumentErrorCategory::Configuration);
        assert_eq!(document.diagnostic_id(), "RPE-DOCUMENT-0046");
        assert_eq!(document.reference(), None);
        assert_eq!(document.offset(), None);
    }
}

#[test]
fn constructor_and_poll_preserve_the_complete_failing_child_layer() {
    let unknown = snapshot(None, 0x70);
    let constructor_error = OpenStrictBaseRevisionJob::new(
        unknown,
        REVISION_ID,
        context(),
        StrictBaseOpenLimits::new(
            XrefLimits::default(),
            DocumentLimits::default(),
            RevisionAttestationLimits::default(),
            ObjectLimits::default(),
            SyntaxLimits::default(),
        ),
    )
    .unwrap_err();
    assert_eq!(
        constructor_error.xref().unwrap().code(),
        XrefErrorCode::UnknownSourceLength
    );
    assert!(constructor_error.document().is_none());

    let duplicate = duplicate_offset_fixture();
    let duplicate_store = supplied_store(&duplicate);
    let mut duplicate_job = new_job(&duplicate, limits(&duplicate));
    let duplicate_error = match duplicate_job.poll(&duplicate_store, &DocumentNeverCancelled) {
        StrictBaseOpenPoll::Failed(error) => error,
        other => panic!("duplicate candidate offsets must fail: {other:?}"),
    };
    assert_eq!(
        duplicate_error.document().unwrap().code(),
        DocumentErrorCode::DuplicatePhysicalOffset
    );
    assert!(duplicate_error.xref().is_none());
    assert_eq!(duplicate_job.phase(), StrictBaseOpenPhase::Failed);
    match duplicate_job.poll(&duplicate_store, &DocumentNeverCancelled) {
        StrictBaseOpenPoll::Failed(repeated) => assert_eq!(repeated, duplicate_error),
        other => panic!("candidate failure must be stable: {other:?}"),
    }

    let invalid_header = invalid_header_fixture();
    let invalid_store = supplied_store(&invalid_header);
    let mut invalid_job = new_job(&invalid_header, limits(&invalid_header));
    let invalid_error = match invalid_job.poll(&invalid_store, &DocumentNeverCancelled) {
        StrictBaseOpenPoll::Failed(error) => error,
        other => panic!("invalid header must fail attestation: {other:?}"),
    };
    assert_eq!(
        invalid_error.document().unwrap().code(),
        DocumentErrorCode::InvalidDocumentHeader
    );
    assert!(invalid_error.xref().is_none());
    assert_eq!(invalid_job.phase(), StrictBaseOpenPhase::Failed);

    let debug = format!("{invalid_error:?}");
    let display = invalid_error.to_string();
    assert!(!debug.contains("%PDF-1.x"));
    assert!(!display.contains("%PDF-1.x"));
}

#[test]
fn xref_and_attestation_cancellation_are_terminal_without_publication() {
    let fixture = large_stream_fixture();
    let empty = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let cancelled = AtomicBool::new(true);
    let mut pre = new_job(&fixture, stream_limits(&fixture));
    let pre_error = match pre.poll(&empty, &cancelled) {
        StrictBaseOpenPoll::Failed(error) => error,
        other => panic!("pre-cancellation must stop xref opening: {other:?}"),
    };
    assert_eq!(pre_error.xref().unwrap().code(), XrefErrorCode::Cancelled);
    assert_eq!(pre.stats().xref().read_bytes(), 0);
    assert_eq!(pre.stats().index(), None);
    assert_eq!(pre.phase(), StrictBaseOpenPhase::Failed);

    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut mid = new_job(&fixture, stream_limits(&fixture));
    let flag = AtomicBool::new(false);
    let mut polls = 0_u32;
    loop {
        polls += 1;
        assert!(polls < 32);
        match mid.poll(&store, &flag) {
            StrictBaseOpenPoll::Ready(_) => panic!("missing prefix bytes must suspend attestation"),
            StrictBaseOpenPoll::Failed(error) => {
                panic!("uncancelled fixture must reach attestation Pending: {error}")
            }
            StrictBaseOpenPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } if checkpoint == SCAN_CHECKPOINT => {
                let charged = mid.stats();
                assert!(charged.index().is_some());
                flag.store(true, Ordering::Release);
                let error = match mid.poll(&store, &flag) {
                    StrictBaseOpenPoll::Failed(error) => error,
                    other => panic!("mid-attestation cancellation must fail: {other:?}"),
                };
                assert_eq!(
                    error.document().unwrap().code(),
                    DocumentErrorCode::Cancelled
                );
                assert_eq!(mid.stats(), charged);
                assert_eq!(mid.phase(), StrictBaseOpenPhase::Failed);

                let mut woke = false;
                for range in missing.as_slice().iter().copied() {
                    woke |= supply_range(&store, &fixture, range)
                        .ready_tickets()
                        .contains(&ticket);
                }
                assert!(woke);
                match mid.poll(&store, &DocumentNeverCancelled) {
                    StrictBaseOpenPoll::Failed(repeated) => assert_eq!(repeated, error),
                    other => panic!("late bytes must not resurrect a cancelled job: {other:?}"),
                }
                break;
            }
            StrictBaseOpenPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                let mut woke = false;
                for range in missing.as_slice().iter().copied() {
                    woke |= supply_range(&store, &fixture, range)
                        .ready_tickets()
                        .contains(&ticket);
                }
                assert!(woke);
                let subscriptions = store.take_subscriptions(ticket).unwrap();
                assert_eq!(subscriptions[0].checkpoint(), checkpoint);
                store.release_ticket(ticket).unwrap();
            }
        }
    }
}

#[test]
fn snapshot_mismatch_wins_before_cancellation_and_remains_terminal() {
    let fixture = canonical_fixture();
    let wrong_snapshot = snapshot(Some(131), 0x72);
    let wrong_store = RangeStore::new(wrong_snapshot, Default::default()).unwrap();
    let cancelled = AtomicBool::new(true);
    let mut job = new_job(&fixture, limits(&fixture));

    let failure = match job.poll(&wrong_store, &cancelled) {
        StrictBaseOpenPoll::Failed(error) => error,
        other => panic!("snapshot mismatch must fail before cancellation: {other:?}"),
    };
    assert_eq!(
        failure.xref().unwrap().code(),
        XrefErrorCode::SnapshotMismatch
    );
    assert_eq!(job.stats().xref().read_bytes(), 0);
    assert_eq!(job.stats().index(), None);
    assert_eq!(job.phase(), StrictBaseOpenPhase::Failed);

    match job.poll(&supplied_store(&fixture), &DocumentNeverCancelled) {
        StrictBaseOpenPoll::Failed(repeated) => assert_eq!(repeated, failure),
        other => panic!("a later correct source must not switch the job snapshot: {other:?}"),
    }
}
