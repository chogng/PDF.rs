use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SmallRanges, SourceError, SourceErrorCode, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    NeverCancelSourceXrefStream, OpenSourceXrefStreamJob, SourceXrefStreamErrorCategory,
    SourceXrefStreamErrorCode, SourceXrefStreamJobContext, SourceXrefStreamLimitKind,
    SourceXrefStreamPhase, SourceXrefStreamPoll, SourceXrefStreamRecoverability,
};
use pdf_rs_object::{
    IndirectObjectTargetKind, IndirectObjectValue, ObjectErrorCode, ObjectLimitConfig,
    ObjectLimitKind, ObjectLimits,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{XrefStreamErrorCode, XrefStreamLimitConfig, XrefStreamLimits};

const JOB: JobId = JobId::new(701);
const ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(702);
const BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(703);
const PAYLOAD: ResumeCheckpoint = ResumeCheckpoint::new(704);

fn context() -> SourceXrefStreamJobContext {
    SourceXrefStreamJobContext::new(JOB, ENVELOPE, BOUNDARY, PAYLOAD, RequestPriority::Metadata)
}

fn snapshot(len: u64, tag: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([tag; 32]),
            SourceRevision::new(u64::from(tag)),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [tag ^ 0xa5; 32]),
    )
}

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    container: ObjectRef,
    startxref: u64,
    object_upper_bound: u64,
    revision_startxref: u64,
    payload_range: ByteRange,
}

fn fixture(
    payload: Vec<u8>,
    dictionary: impl FnOnce(u64) -> String,
    primary: bool,
    tag: u8,
) -> Fixture {
    let container = ObjectRef::new(9, 0).unwrap();
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let startxref = u64::try_from(bytes.len()).unwrap();
    let dictionary = dictionary(u64::try_from(payload.len()).unwrap());
    bytes.extend_from_slice(format!("9 0 obj\n{dictionary}\nstream\n").as_bytes());
    let payload_start = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let object_upper_bound = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"xref\n0 1\n");
    let revision_startxref = if primary {
        startxref
    } else {
        object_upper_bound
    };
    let source_len = u64::try_from(bytes.len()).unwrap();
    Fixture {
        bytes,
        snapshot: snapshot(source_len, tag),
        container,
        startxref,
        object_upper_bound,
        revision_startxref,
        payload_range: ByteRange::new(payload_start, u64::try_from(payload.len()).unwrap())
            .unwrap(),
    }
}

fn primary(tag: u8) -> Fixture {
    fixture(
        vec![1, 0, 9, 0],
        |length| format!("<< /Type /XRef /Size 10 /W [1 2 1] /Index [9 1] /Length {length} >>"),
        true,
        tag,
    )
}

fn hybrid(tag: u8) -> Fixture {
    fixture(
        vec![0, 0, 0, 255],
        |length| format!("<< /Type /XRef /Size 10 /W [1 2 1] /Index [0 1] /Length {length} >>"),
        false,
        tag,
    )
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(fixture.bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone()).unwrap())
        .unwrap();
    store
}

#[allow(
    clippy::result_large_err,
    reason = "test helpers preserve the complete copyable lower-layer error contract"
)]
fn job_with(
    fixture: &Fixture,
    container: ObjectRef,
    object_upper_bound: u64,
    object_limits: ObjectLimits,
    xref_limits: XrefStreamLimits,
) -> Result<OpenSourceXrefStreamJob, pdf_rs_document::SourceXrefStreamError> {
    OpenSourceXrefStreamJob::new(
        fixture.snapshot,
        container,
        fixture.startxref,
        object_upper_bound,
        fixture.revision_startxref,
        context(),
        object_limits,
        SyntaxLimits::default(),
        xref_limits,
    )
}

fn job(fixture: &Fixture) -> OpenSourceXrefStreamJob {
    job_with(
        fixture,
        fixture.container,
        fixture.object_upper_bound,
        ObjectLimits::default(),
        XrefStreamLimits::default(),
    )
    .unwrap()
}

fn run_ready(
    fixture: &Fixture,
) -> (
    OpenSourceXrefStreamJob,
    pdf_rs_document::SourceAcquiredXrefStream,
) {
    let store = supplied_store(fixture);
    let mut job = job(fixture);
    let ready = match job.poll(&store, &NeverCancelSourceXrefStream) {
        SourceXrefStreamPoll::Ready(ready) => ready,
        other => panic!("fully supplied fixture did not complete: {other:?}"),
    };
    (job, ready)
}

fn supply_missing(store: &RangeStore, fixture: &Fixture, missing: &SmallRanges) {
    for range in missing.as_slice() {
        let start = usize::try_from(range.start()).unwrap();
        let end = usize::try_from(range.end_exclusive()).unwrap();
        store
            .supply(
                RangeResponse::new(fixture.snapshot, *range, fixture.bytes[start..end].to_vec())
                    .unwrap(),
            )
            .unwrap();
    }
}

fn failed(outcome: SourceXrefStreamPoll) -> pdf_rs_document::SourceXrefStreamError {
    match outcome {
        SourceXrefStreamPoll::Failed(error) => error,
        other => panic!("expected a structured failure, got {other:?}"),
    }
}

#[test]
fn primary_and_hybrid_acquisition_retain_exact_framed_source_evidence() {
    for fixture in [primary(0x71), hybrid(0x72)] {
        let (job, ready) = run_ready(&fixture);
        assert_eq!(job.phase(), SourceXrefStreamPhase::Complete);
        assert_eq!(ready.snapshot(), fixture.snapshot);
        assert_eq!(ready.container(), fixture.container);
        assert_eq!(
            ready.encoded_payload_span().start(),
            fixture.payload_range.start()
        );
        assert_eq!(
            ready.encoded_payload_span().len(),
            fixture.payload_range.len()
        );
        let framed = ready.framed_container();
        assert_eq!(
            framed.target_kind(),
            IndirectObjectTargetKind::XrefStreamAnchor
        );
        assert_eq!(framed.reference(), fixture.container);
        assert_eq!(framed.xref_offset(), fixture.startxref);
        assert_eq!(framed.object_upper_bound(), fixture.object_upper_bound);
        assert_eq!(framed.revision_startxref(), fixture.revision_startxref);
        let IndirectObjectValue::Stream(stream) = framed.value() else {
            panic!("source-acquired xref container must remain a framed stream")
        };
        assert_eq!(stream.data_span().start(), fixture.payload_range.start());
        assert_eq!(stream.data_span().len(), fixture.payload_range.len());
        assert_eq!(ready.stats().payload_read_attempts(), 1);
        assert_eq!(
            ready.stats().payload_read_bytes(),
            fixture.payload_range.len()
        );
        assert_eq!(
            ready.stats().xref_stream().unwrap().decoded_bytes(),
            fixture.payload_range.len()
        );
        assert_eq!(
            ready.stats().retained_proof_bytes(),
            framed.retained_heap_bytes()
                + ready.stats().xref_stream().unwrap().retained_entry_bytes()
        );
        assert!(!format!("{ready:?}").contains("[1, 0, 9, 0]"));
    }
}

#[test]
fn one_pending_ticket_survives_boundary_before_payload_physical_delivery() {
    let mut payload = vec![0_u8; 2048 * 4];
    payload[9 * 4..9 * 4 + 4].copy_from_slice(&[1, 0, 9, 0]);
    let fixture = fixture(
        payload,
        |length| format!("<< /Type /XRef /Size 2048 /W [1 2 1] /Length {length} >>"),
        true,
        0x73,
    );
    let source_len = u64::try_from(fixture.bytes.len()).unwrap();
    let limits = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: 128,
        max_envelope_bytes: 256,
        initial_boundary_bytes: 64,
        max_boundary_bytes: 128,
        max_stream_bytes: 8192,
        max_total_read_bytes: 384,
        max_total_parse_bytes: 384,
    })
    .unwrap();
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = job_with(
        &fixture,
        fixture.container,
        fixture.object_upper_bound,
        limits,
        XrefStreamLimits::default(),
    )
    .unwrap();

    let envelope_missing = match job.poll(&store, &NeverCancelSourceXrefStream) {
        SourceXrefStreamPoll::Pending {
            missing,
            checkpoint,
            ..
        } => {
            assert_eq!(checkpoint, ENVELOPE);
            missing
        }
        other => panic!("empty source did not suspend envelope: {other:?}"),
    };
    supply_missing(&store, &fixture, &envelope_missing);

    let (payload_ticket, payload_missing) = match job.poll(&store, &NeverCancelSourceXrefStream) {
        SourceXrefStreamPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(checkpoint, PAYLOAD);
            (ticket, missing)
        }
        other => panic!("framed envelope did not request exact payload: {other:?}"),
    };
    let charged = job.stats();
    assert_eq!(charged.payload_read_attempts(), 1);
    assert_eq!(charged.payload_read_bytes(), 8192);
    match job.poll(&store, &NeverCancelSourceXrefStream) {
        SourceXrefStreamPoll::Pending {
            ticket, checkpoint, ..
        } => {
            assert_eq!(ticket, payload_ticket);
            assert_eq!(checkpoint, PAYLOAD);
        }
        other => panic!("payload ticket was not retained: {other:?}"),
    }
    assert_eq!(job.stats(), charged, "Pending re-polls must not re-charge");

    let boundary_start = fixture.payload_range.end_exclusive();
    let boundary_range =
        ByteRange::new(boundary_start, fixture.object_upper_bound - boundary_start).unwrap();
    let start = usize::try_from(boundary_range.start()).unwrap();
    let end = usize::try_from(boundary_range.end_exclusive()).unwrap();
    store
        .supply(
            RangeResponse::new(
                fixture.snapshot,
                boundary_range,
                fixture.bytes[start..end].to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    match job.poll(&store, &NeverCancelSourceXrefStream) {
        SourceXrefStreamPoll::Pending {
            ticket, checkpoint, ..
        } => {
            assert_eq!(ticket, payload_ticket);
            assert_eq!(checkpoint, PAYLOAD);
        }
        other => panic!("unsolicited boundary bytes changed the active payload ticket: {other:?}"),
    }
    supply_missing(&store, &fixture, &payload_missing);
    let ready = match job.poll(&store, &NeverCancelSourceXrefStream) {
        SourceXrefStreamPoll::Ready(ready) => ready,
        other => panic!("boundary-before-payload delivery did not finish: {other:?}"),
    };
    assert_eq!(ready.entries().len(), 2048);
    assert_eq!(ready.stats().payload_read_attempts(), 1);
}

#[test]
fn payload_then_boundary_pending_each_replay_one_checkpoint() {
    let fixture = fixture(
        vec![0, 0, 0, 255, 1, 0, 9, 0, 2, 0, 5, 7],
        |length| format!("<< /Type /XRef /Size 10 /W [1 2 1] /Index [0 3] /Length {length} >>"),
        false,
        0x81,
    );
    let source_len = u64::try_from(fixture.bytes.len()).unwrap();
    let envelope_ceiling =
        fixture.payload_range.start() - fixture.startxref + fixture.payload_range.len() / 2;
    let limits = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: 64,
        max_envelope_bytes: envelope_ceiling,
        initial_boundary_bytes: 16,
        max_boundary_bytes: 32,
        max_stream_bytes: 12,
        max_total_read_bytes: 192,
        max_total_parse_bytes: 192,
    })
    .unwrap();
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = job_with(
        &fixture,
        fixture.container,
        fixture.object_upper_bound,
        limits,
        XrefStreamLimits::default(),
    )
    .unwrap();

    let (payload_ticket, payload_missing) = loop {
        match job.poll(&store, &NeverCancelSourceXrefStream) {
            SourceXrefStreamPoll::Pending {
                missing,
                checkpoint: ENVELOPE,
                ..
            } => supply_missing(&store, &fixture, &missing),
            SourceXrefStreamPoll::Pending {
                ticket,
                missing,
                checkpoint: PAYLOAD,
            } => break (ticket, missing),
            other => panic!("envelope did not advance to payload Pending: {other:?}"),
        }
    };
    let payload_stats = job.stats();
    match job.poll(&store, &NeverCancelSourceXrefStream) {
        SourceXrefStreamPoll::Pending {
            ticket, checkpoint, ..
        } => {
            assert_eq!(ticket, payload_ticket);
            assert_eq!(checkpoint, PAYLOAD);
        }
        other => panic!("payload Pending did not replay: {other:?}"),
    }
    assert_eq!(job.stats(), payload_stats);
    supply_missing(&store, &fixture, &payload_missing);

    let (boundary_ticket, boundary_missing) = match job.poll(&store, &NeverCancelSourceXrefStream) {
        SourceXrefStreamPoll::Pending {
            ticket,
            missing,
            checkpoint: BOUNDARY,
        } => (ticket, missing),
        other => panic!("payload completion did not advance to boundary Pending: {other:?}"),
    };
    let boundary_stats = job.stats();
    match job.poll(&store, &NeverCancelSourceXrefStream) {
        SourceXrefStreamPoll::Pending {
            ticket, checkpoint, ..
        } => {
            assert_eq!(ticket, boundary_ticket);
            assert_eq!(checkpoint, BOUNDARY);
        }
        other => panic!("boundary Pending did not replay: {other:?}"),
    }
    assert_eq!(job.stats(), boundary_stats);
    supply_missing(&store, &fixture, &boundary_missing);
    let ready = loop {
        match job.poll(&store, &NeverCancelSourceXrefStream) {
            SourceXrefStreamPoll::Pending {
                missing,
                checkpoint: BOUNDARY,
                ..
            } => supply_missing(&store, &fixture, &missing),
            SourceXrefStreamPoll::Ready(ready) => break ready,
            other => panic!("supplied boundary did not finish acquisition: {other:?}"),
        }
    };
    assert_eq!(ready.entries().len(), 3);
    assert_eq!(ready.stats().payload_read_attempts(), 1);
    assert_eq!(ready.stats().object().boundary_attempts(), 2);
}

#[test]
fn direct_length_is_exact_and_cannot_cross_the_caller_bound() {
    let exact = primary(0x74);
    let (_, ready) = run_ready(&exact);
    assert_eq!(
        ready.encoded_payload_span().len(),
        exact.payload_range.len()
    );

    let short = fixture(
        vec![1, 0, 9, 0],
        |_| "<< /Type /XRef /Size 10 /W [1 2 1] /Index [9 1] /Length 3 >>".to_owned(),
        true,
        0x75,
    );
    let store = supplied_store(&short);
    let error = failed(job(&short).poll(&store, &NeverCancelSourceXrefStream));
    assert_eq!(error.code(), SourceXrefStreamErrorCode::ObjectFailure);
    assert_eq!(
        error.object_error().unwrap().code(),
        ObjectErrorCode::InvalidStreamBoundary
    );

    let crossing = fixture(
        vec![1, 0, 9, 0],
        |_| "<< /Type /XRef /Size 10 /W [1 2 1] /Index [9 1] /Length 999 >>".to_owned(),
        true,
        0x76,
    );
    let store = supplied_store(&crossing);
    let error = failed(job(&crossing).poll(&store, &NeverCancelSourceXrefStream));
    assert_eq!(error.code(), SourceXrefStreamErrorCode::ObjectFailure);
    assert_eq!(
        error.object_error().unwrap().code(),
        ObjectErrorCode::ObjectCrossesPhysicalBound
    );
}

#[test]
fn indirect_length_is_stably_unsupported_during_bootstrap() {
    let fixture = fixture(
        vec![1, 0, 9, 0],
        |_| "<< /Type /XRef /Size 10 /W [1 2 1] /Index [9 1] /Length 2 0 R >>".to_owned(),
        true,
        0x77,
    );
    let store = supplied_store(&fixture);
    let mut job = job(&fixture);
    let error = failed(job.poll(&store, &NeverCancelSourceXrefStream));
    assert_eq!(
        error.code(),
        SourceXrefStreamErrorCode::UnsupportedIndirectLength
    );
    assert_eq!(error.category(), SourceXrefStreamErrorCategory::Unsupported);
    assert_eq!(
        error.recoverability(),
        SourceXrefStreamRecoverability::UseSupportedFeature
    );
    assert_eq!(error.dependency(), ObjectRef::new(2, 0).ok());
    assert_eq!(
        job.poll(&store, &NeverCancelSourceXrefStream),
        SourceXrefStreamPoll::Failed(error)
    );
}

#[test]
fn dictionary_filter_container_and_self_failures_remain_distinct() {
    for (dictionary, expected) in [
        (
            "<< /Type /XRef /Size 10 /W [1 2 1] /Index [9 1] /Filter /FlateDecode /Length 4 >>",
            XrefStreamErrorCode::UnsupportedFilter,
        ),
        (
            "<< /Type /ObjStm /Size 10 /W [1 2 1] /Index [9 1] /Length 4 >>",
            XrefStreamErrorCode::InvalidDictionary,
        ),
    ] {
        let fixture = fixture(
            vec![1, 0, 9, 0],
            |_| dictionary.to_owned(),
            true,
            if matches!(expected, XrefStreamErrorCode::UnsupportedFilter) {
                0x78
            } else {
                0x79
            },
        );
        let store = supplied_store(&fixture);
        let error = failed(job(&fixture).poll(&store, &NeverCancelSourceXrefStream));
        assert_eq!(error.code(), SourceXrefStreamErrorCode::XrefStreamFailure);
        assert_eq!(error.xref_stream_error().unwrap().code(), expected);
    }

    let wrong_self = fixture(
        vec![1, 0, 8, 0],
        |length| format!("<< /Type /XRef /Size 10 /W [1 2 1] /Index [9 1] /Length {length} >>"),
        true,
        0x7a,
    );
    let store = supplied_store(&wrong_self);
    let mut wrong_self_job = job(&wrong_self);
    let error = failed(wrong_self_job.poll(&store, &NeverCancelSourceXrefStream));
    assert_eq!(error.code(), SourceXrefStreamErrorCode::InvalidSelfEntry);
    assert!(
        wrong_self_job.stats().xref_stream().is_some(),
        "completed child-parser work remains cumulative on a later proof failure"
    );

    let hybrid_outside_size = fixture(
        vec![0, 0, 0, 255],
        |length| format!("<< /Type /XRef /Size 1 /W [1 2 1] /Index [0 1] /Length {length} >>"),
        false,
        0x82,
    );
    let store = supplied_store(&hybrid_outside_size);
    let error = failed(job(&hybrid_outside_size).poll(&store, &NeverCancelSourceXrefStream));
    assert_eq!(error.code(), SourceXrefStreamErrorCode::InvalidSelfEntry);

    let wrong_container = primary(0x7b);
    let store = supplied_store(&wrong_container);
    let mut job = job_with(
        &wrong_container,
        ObjectRef::new(8, 0).unwrap(),
        wrong_container.object_upper_bound,
        ObjectLimits::default(),
        XrefStreamLimits::default(),
    )
    .unwrap();
    let error = failed(job.poll(&store, &NeverCancelSourceXrefStream));
    assert_eq!(error.code(), SourceXrefStreamErrorCode::ObjectFailure);
    assert_eq!(
        error.object_error().unwrap().code(),
        ObjectErrorCode::InvalidObjectHeader
    );

    let invalid_bound = job_with(
        &wrong_container,
        wrong_container.container,
        wrong_container.startxref,
        ObjectLimits::default(),
        XrefStreamLimits::default(),
    )
    .unwrap_err();
    assert_eq!(
        invalid_bound.code(),
        SourceXrefStreamErrorCode::ObjectFailure
    );
    assert_eq!(
        invalid_bound.object_error().unwrap().code(),
        ObjectErrorCode::InvalidTarget
    );
}

#[test]
fn cancellation_source_change_and_terminal_replay_are_stable() {
    let fixture = primary(0x7c);
    let store = supplied_store(&fixture);
    let cancelled = AtomicBool::new(true);
    let mut cancelled_job = job(&fixture);
    let error = failed(cancelled_job.poll(&store, &cancelled));
    assert_eq!(error.code(), SourceXrefStreamErrorCode::Cancelled);
    cancelled.store(false, Ordering::Release);
    assert_eq!(
        cancelled_job.poll(&store, &cancelled),
        SourceXrefStreamPoll::Failed(error)
    );

    let mut changed_job = job(&fixture);
    let foreign_snapshot = snapshot(u64::try_from(fixture.bytes.len()).unwrap(), 0x7d);
    let foreign = RangeStore::new(foreign_snapshot, Default::default()).unwrap();
    let changed = failed(changed_job.poll(&foreign, &NeverCancelSourceXrefStream));
    assert_eq!(changed.code(), SourceXrefStreamErrorCode::SnapshotMismatch);
    assert_eq!(changed.category(), SourceXrefStreamErrorCategory::Source);
    assert_eq!(
        changed_job.poll(&store, &NeverCancelSourceXrefStream),
        SourceXrefStreamPoll::Failed(changed)
    );

    let pending_store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut pending_cancel = job(&fixture);
    assert!(matches!(
        pending_cancel.poll(&pending_store, &NeverCancelSourceXrefStream),
        SourceXrefStreamPoll::Pending { .. }
    ));
    let pending_stats = pending_cancel.stats();
    cancelled.store(true, Ordering::Release);
    let error = failed(pending_cancel.poll(&pending_store, &cancelled));
    assert_eq!(error.code(), SourceXrefStreamErrorCode::Cancelled);
    assert_eq!(pending_cancel.stats(), pending_stats);

    let changed_store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut pending_changed = job(&fixture);
    assert!(matches!(
        pending_changed.poll(&changed_store, &NeverCancelSourceXrefStream),
        SourceXrefStreamPoll::Pending { .. }
    ));
    changed_store.signal_source_changed().unwrap();
    let error = failed(pending_changed.poll(&changed_store, &NeverCancelSourceXrefStream));
    let object = error.object_error().unwrap();
    assert_eq!(object.code(), ObjectErrorCode::SourceFailure);
    assert_eq!(
        object.source_error().unwrap().code(),
        SourceErrorCode::SourceChanged
    );

    let (mut complete, _) = run_ready(&fixture);
    cancelled.store(true, Ordering::Release);
    let replay = failed(complete.poll(&foreign, &cancelled));
    assert_eq!(replay.code(), SourceXrefStreamErrorCode::JobAlreadyComplete);
}

#[test]
fn cancellation_is_committed_after_semantic_parse_before_ready_publication() {
    struct CancelOnProbe {
        cancel_at: usize,
        probes: AtomicUsize,
    }

    impl pdf_rs_document::SourceXrefStreamCancellation for CancelOnProbe {
        fn is_cancelled(&self) -> bool {
            self.probes.fetch_add(1, Ordering::AcqRel) + 1 >= self.cancel_at
        }
    }

    let fixture = primary(0x83);
    let store = supplied_store(&fixture);
    let mut observed_commit_probe = false;
    for cancel_at in 1..=512 {
        let cancellation = CancelOnProbe {
            cancel_at,
            probes: AtomicUsize::new(0),
        };
        let mut job = job(&fixture);
        if let SourceXrefStreamPoll::Failed(error) = job.poll(&store, &cancellation)
            && error.code() == SourceXrefStreamErrorCode::Cancelled
            && job.stats().xref_stream().is_some()
        {
            observed_commit_probe = true;
            break;
        }
    }
    assert!(
        observed_commit_probe,
        "Ready publication must have a cancellation commit probe after child parse stats land"
    );
}

#[test]
fn payload_eof_and_mismatched_ready_geometry_are_distinct() {
    enum PayloadOutcome {
        EndOfFile,
        WrongRange,
    }

    struct PayloadSource {
        store: RangeStore,
        payload: ByteRange,
        outcome: PayloadOutcome,
    }

    impl ByteSource for PayloadSource {
        fn snapshot(&self) -> SourceSnapshot {
            self.store.snapshot()
        }

        fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
            if request.range() != self.payload {
                return self.store.poll(request);
            }
            match self.outcome {
                PayloadOutcome::EndOfFile => ReadPoll::EndOfFile,
                PayloadOutcome::WrongRange => {
                    let wrong =
                        ByteRange::new(self.payload.start(), self.payload.len() - 1).unwrap();
                    self.store.poll(ReadRequest::new(
                        wrong,
                        request.priority(),
                        request.job(),
                        request.checkpoint(),
                    ))
                }
            }
        }
    }

    let fixture = primary(0x84);
    for (outcome, expected) in [
        (
            PayloadOutcome::EndOfFile,
            SourceXrefStreamErrorCode::UnexpectedEndOfSource,
        ),
        (
            PayloadOutcome::WrongRange,
            SourceXrefStreamErrorCode::SourceGeometryMismatch,
        ),
    ] {
        let source = PayloadSource {
            store: supplied_store(&fixture),
            payload: fixture.payload_range,
            outcome,
        };
        let error = failed(job(&fixture).poll(&source, &NeverCancelSourceXrefStream));
        assert_eq!(error.code(), expected);
        assert_eq!(error.offset(), Some(fixture.payload_range.start()));
    }
}

#[test]
fn lower_source_failure_and_size_work_limits_keep_original_details() {
    struct PayloadFailureSource {
        store: RangeStore,
        payload: ByteRange,
    }

    impl ByteSource for PayloadFailureSource {
        fn snapshot(&self) -> SourceSnapshot {
            self.store.snapshot()
        }

        fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
            if request.range() == self.payload {
                ReadPoll::Failed(SourceError::source_unavailable())
            } else {
                self.store.poll(request)
            }
        }
    }

    let failure_fixture = primary(0x7e);
    let source = PayloadFailureSource {
        store: supplied_store(&failure_fixture),
        payload: failure_fixture.payload_range,
    };
    let mut job = job(&failure_fixture);
    let error = failed(job.poll(&source, &NeverCancelSourceXrefStream));
    assert_eq!(error.code(), SourceXrefStreamErrorCode::SourceFailure);
    assert_eq!(
        error.source_error(),
        Some(SourceError::source_unavailable())
    );
    assert_eq!(
        error.recoverability(),
        SourceXrefStreamRecoverability::RetrySource
    );

    let fixture = primary(0x80);
    let source_len = u64::try_from(fixture.bytes.len()).unwrap();
    let stream_too_small = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: 64,
        max_envelope_bytes: 96,
        initial_boundary_bytes: 32,
        max_boundary_bytes: 32,
        max_stream_bytes: 3,
        max_total_read_bytes: 192,
        max_total_parse_bytes: 192,
    })
    .unwrap();
    let store = supplied_store(&fixture);
    let mut limited = job_with(
        &fixture,
        fixture.container,
        fixture.object_upper_bound,
        stream_too_small,
        XrefStreamLimits::default(),
    )
    .unwrap();
    let error = failed(limited.poll(&store, &NeverCancelSourceXrefStream));
    let object = error.object_error().unwrap();
    assert_eq!(object.code(), ObjectErrorCode::ResourceLimit);
    assert_eq!(object.limit().unwrap().kind(), ObjectLimitKind::StreamBytes);

    let xref_too_small = XrefStreamLimits::validate(XrefStreamLimitConfig {
        max_decoded_bytes: 3,
        ..XrefStreamLimitConfig::default()
    })
    .unwrap();
    let mut limited = job_with(
        &fixture,
        fixture.container,
        fixture.object_upper_bound,
        ObjectLimits::default(),
        xref_too_small,
    )
    .unwrap();
    let error = failed(limited.poll(&store, &NeverCancelSourceXrefStream));
    assert_eq!(error.code(), SourceXrefStreamErrorCode::ResourceLimit);
    let limit = error.limit().unwrap();
    assert_eq!(limit.kind(), SourceXrefStreamLimitKind::PayloadBytes);
    assert_eq!(limit.limit(), 3);
    assert_eq!(limit.attempted(), 4);
    assert_eq!(limited.stats().payload_read_attempts(), 0);

    let envelope_too_small = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: 16,
        max_envelope_bytes: 32,
        initial_boundary_bytes: 16,
        max_boundary_bytes: 32,
        max_stream_bytes: 4,
        max_total_read_bytes: 64,
        max_total_parse_bytes: 64,
    })
    .unwrap();
    let mut limited = job_with(
        &fixture,
        fixture.container,
        fixture.object_upper_bound,
        envelope_too_small,
        XrefStreamLimits::default(),
    )
    .unwrap();
    let error = failed(limited.poll(&store, &NeverCancelSourceXrefStream));
    let object = error.object_error().unwrap();
    assert_eq!(object.code(), ObjectErrorCode::ResourceLimit);
    assert_eq!(
        object.limit().unwrap().kind(),
        ObjectLimitKind::EnvelopeBytes
    );
}

#[test]
fn checkpoints_are_pairwise_distinct_before_any_child_is_created() {
    let fixture = primary(0x7f);
    for invalid in [
        SourceXrefStreamJobContext::new(
            JOB,
            ENVELOPE,
            ENVELOPE,
            PAYLOAD,
            RequestPriority::Metadata,
        ),
        SourceXrefStreamJobContext::new(
            JOB,
            ENVELOPE,
            BOUNDARY,
            ENVELOPE,
            RequestPriority::Metadata,
        ),
        SourceXrefStreamJobContext::new(
            JOB,
            ENVELOPE,
            BOUNDARY,
            BOUNDARY,
            RequestPriority::Metadata,
        ),
    ] {
        let error = OpenSourceXrefStreamJob::new(
            fixture.snapshot,
            fixture.container,
            fixture.startxref,
            fixture.object_upper_bound,
            fixture.revision_startxref,
            invalid,
            ObjectLimits::default(),
            SyntaxLimits::default(),
            XrefStreamLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), SourceXrefStreamErrorCode::InvalidJobContext);
        assert_eq!(error.diagnostic_id(), "RPE-SOURCE-XREF-0001");
    }
}
