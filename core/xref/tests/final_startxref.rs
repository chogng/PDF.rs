use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    FinalStartXrefJobContext, FinalStartXrefPhase, FinalStartXrefPoll, NeverCancelled,
    OpenFinalStartXrefJob, OpenXrefJob, XrefErrorCode, XrefJobContext, XrefLimitConfig,
    XrefLimitKind, XrefLimits, XrefPoll,
};

fn snapshot(byte: u8, len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([byte; 32]), SourceRevision::new(4)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x71; 32]),
    )
}

fn fixture() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let startxref = bytes.len();
    bytes.extend_from_slice(
        b"xref\n0 2\n\
0000000000 65535 f \n\
0000000001 00000 n \n\
trailer\n\
<< /Size 2 /Root 1 0 R >>\n\
startxref\n",
    );
    bytes.extend_from_slice(startxref.to_string().as_bytes());
    bytes.extend_from_slice(b"\n%%EOF\n");
    bytes
}

fn context() -> FinalStartXrefJobContext {
    FinalStartXrefJobContext::new(JobId::new(91), ResumeCheckpoint::new(301))
}

fn supplied_store(bytes: &[u8], identity_byte: u8) -> RangeStore {
    let snapshot = snapshot(identity_byte, u64::try_from(bytes.len()).unwrap());
    let store = RangeStore::new(snapshot, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(snapshot, range, bytes.to_vec()).unwrap())
        .unwrap();
    store
}

#[test]
fn final_marker_job_returns_snapshot_anchor_and_tail_bound() {
    let bytes = fixture();
    let store = supplied_store(&bytes, 0x31);
    let mut job =
        OpenFinalStartXrefJob::new(store.snapshot(), context(), XrefLimits::default()).unwrap();

    let found = match job.poll(&store, &NeverCancelled) {
        FinalStartXrefPoll::Ready(found) => found,
        other => panic!("supplied final marker did not complete: {other:?}"),
    };
    let tail_start = bytes
        .windows(b"startxref".len())
        .rposition(|window| window == b"startxref")
        .unwrap();
    assert_eq!(found.snapshot(), store.snapshot());
    assert_eq!(found.source(), store.snapshot().identity());
    assert_eq!(found.startxref(), 9);
    assert_eq!(found.tail_start(), u64::try_from(tail_start).unwrap());
    assert!(found.startxref() < found.tail_start());
    assert_eq!(job.phase(), FinalStartXrefPhase::Complete);
    assert_eq!(job.stats().tail_attempts(), 1);
    assert_eq!(
        job.stats().read_bytes(),
        u64::try_from(bytes.len()).unwrap()
    );
    assert_eq!(
        job.stats().parse_bytes(),
        u64::try_from(bytes.len()).unwrap()
    );
    assert_eq!(job.context(), context());
    assert_eq!(job.limits(), XrefLimits::default());

    assert_eq!(
        match job.poll(&store, &NeverCancelled) {
            FinalStartXrefPoll::Failed(error) => error.code(),
            other => panic!("completed final-marker job replayed: {other:?}"),
        },
        XrefErrorCode::JobAlreadyComplete
    );
}

#[test]
fn legacy_open_reuses_the_same_final_discovery_without_changing_strict_output() {
    let bytes = fixture();
    let store = supplied_store(&bytes, 0x32);
    let mut final_job =
        OpenFinalStartXrefJob::new(store.snapshot(), context(), XrefLimits::default()).unwrap();
    let found = match final_job.poll(&store, &NeverCancelled) {
        FinalStartXrefPoll::Ready(found) => found,
        other => panic!("final-only job failed: {other:?}"),
    };

    let mut legacy = OpenXrefJob::new(
        store.snapshot(),
        XrefJobContext::new(
            context().job(),
            context().tail_checkpoint(),
            ResumeCheckpoint::new(302),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match legacy.poll(&store, &NeverCancelled) {
        XrefPoll::Ready(section) => assert_eq!(section.startxref(), found.startxref()),
        other => panic!("legacy strict open changed behavior: {other:?}"),
    }
    assert_eq!(legacy.discovered_startxref(), Some(found.startxref()));
    assert_eq!(
        legacy.stats().tail_attempts(),
        final_job.stats().tail_attempts()
    );
}

#[test]
fn pending_repoll_does_not_recharge_and_snapshot_change_is_terminal() {
    let bytes = fixture();
    let source = snapshot(0x33, u64::try_from(bytes.len()).unwrap());
    let store = RangeStore::new(source, Default::default()).unwrap();
    let mut job = OpenFinalStartXrefJob::new(source, context(), XrefLimits::default()).unwrap();

    let (ticket, missing) = match job.poll(&store, &NeverCancelled) {
        FinalStartXrefPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(checkpoint, context().tail_checkpoint());
            (ticket, missing)
        }
        other => panic!("empty store did not suspend: {other:?}"),
    };
    let charged = job.stats();
    match job.poll(&store, &NeverCancelled) {
        FinalStartXrefPoll::Pending {
            ticket: repeated,
            missing: repeated_missing,
            checkpoint,
        } => {
            assert_eq!(repeated, ticket);
            assert_eq!(repeated_missing, missing);
            assert_eq!(checkpoint, context().tail_checkpoint());
        }
        other => panic!("pending final marker did not remain stable: {other:?}"),
    }
    assert_eq!(job.stats(), charged);

    let foreign = supplied_store(&bytes, 0x34);
    let error = match job.poll(&foreign, &NeverCancelled) {
        FinalStartXrefPoll::Failed(error) => error,
        other => panic!("snapshot change was not terminal: {other:?}"),
    };
    assert_eq!(error.code(), XrefErrorCode::SnapshotMismatch);
    assert_eq!(job.phase(), FinalStartXrefPhase::Failed);
}

#[test]
fn cancellation_and_tail_limit_remain_structured() {
    let bytes = fixture();
    let store = supplied_store(&bytes, 0x35);
    let mut cancelled =
        OpenFinalStartXrefJob::new(store.snapshot(), context(), XrefLimits::default()).unwrap();
    let error = match cancelled.poll(&store, &AtomicBool::new(true)) {
        FinalStartXrefPoll::Failed(error) => error,
        other => panic!("cancelled discovery continued: {other:?}"),
    };
    assert_eq!(error.code(), XrefErrorCode::Cancelled);
    assert_eq!(cancelled.stats().read_bytes(), 0);

    let limits = XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: u64::try_from(bytes.len()).unwrap(),
        initial_tail_bytes: 8,
        max_tail_bytes: 8,
        initial_section_bytes: 1,
        max_section_bytes: 1,
        max_total_read_bytes: 9,
        max_total_parse_bytes: 9,
        max_subsections: 1,
        max_entries: 2,
    })
    .unwrap();
    let mut bounded = OpenFinalStartXrefJob::new(store.snapshot(), context(), limits).unwrap();
    let error = match bounded.poll(&store, &NeverCancelled) {
        FinalStartXrefPoll::Failed(error) => error,
        other => panic!("undersized tail limit did not fail: {other:?}"),
    };
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    assert_eq!(error.limit().unwrap().kind(), XrefLimitKind::TailBytes);
}
