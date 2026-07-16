use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_object::{
    DeclaredStreamLength, IndirectObjectTarget, IndirectObjectValue, NeverCancelled,
    ObjectEnvelopePoll, ObjectErrorCode, ObjectJobContext, ObjectLimitKind, ObjectLimits,
    ObjectPoll, ObjectWorkCaps, OpenObjectEnvelopeJob, OpenObjectJob, OpenStreamBoundaryJob,
    ResolvedStreamLength,
};
use pdf_rs_syntax::{ByteSpan, ObjectRef, SyntaxLimits};

fn snapshot(len: u64, marker: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([marker; 32]),
            SourceRevision::new(u64::from(marker)),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [marker ^ 0x5a; 32]),
    )
}

fn reference(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("test object references are valid")
}

fn context() -> ObjectJobContext {
    ObjectJobContext::new(
        JobId::new(107),
        ResumeCheckpoint::new(141),
        ResumeCheckpoint::new(142),
        RequestPriority::VisiblePage,
    )
}

struct Fixture {
    bytes: Vec<u8>,
    object_upper_bound: u64,
    startxref: u64,
    length_value_span: ByteSpan,
}

fn indirect_fixture(payload_len: usize) -> Fixture {
    indirect_fixture_with_reference(payload_len, 2)
}

fn indirect_fixture_with_reference(payload_len: usize, declared_reference: u32) -> Fixture {
    let mut bytes =
        format!("1 0 obj\n<< /Length {declared_reference} 0 R /Meta [(kept)] >>\nstream\n")
            .into_bytes();
    bytes.extend(std::iter::repeat_n(b'P', payload_len));
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let object_upper_bound = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"2 0 obj\n");
    let length_value_start = u64::try_from(bytes.len()).unwrap();
    let length = payload_len.to_string();
    bytes.extend_from_slice(length.as_bytes());
    let length_value_span =
        ByteSpan::new(length_value_start, u64::try_from(length.len()).unwrap()).unwrap();
    bytes.extend_from_slice(b"\nendobj\n");
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"xref\n");
    Fixture {
        bytes,
        object_upper_bound,
        startxref,
        length_value_span,
    }
}

fn direct_fixture(payload: &[u8]) -> Fixture {
    let mut bytes = format!("1 0 obj\n<< /Length {} >>\nstream\n", payload.len()).into_bytes();
    let marker = b"/Length ";
    let operand_start = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap()
        + marker.len();
    let length_value_span = ByteSpan::new(
        u64::try_from(operand_start).unwrap(),
        u64::try_from(payload.len().to_string().len()).unwrap(),
    )
    .unwrap();
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let object_upper_bound = u64::try_from(bytes.len()).unwrap();
    let startxref = object_upper_bound;
    bytes.extend_from_slice(b"xref\n");
    Fixture {
        bytes,
        object_upper_bound,
        startxref,
        length_value_span,
    }
}

fn target(source: SourceSnapshot, fixture: &Fixture) -> IndirectObjectTarget {
    IndirectObjectTarget::new(
        source,
        reference(1),
        0,
        fixture.object_upper_bound,
        fixture.startxref,
    )
    .unwrap()
}

fn supplied_store(bytes: &[u8], marker: u8) -> RangeStore {
    let source = snapshot(u64::try_from(bytes.len()).unwrap(), marker);
    let store = RangeStore::new(source, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(source, range, bytes.to_vec()).unwrap())
        .unwrap();
    store
}

fn open_envelope(store: &RangeStore, fixture: &Fixture) -> pdf_rs_object::StreamEnvelope {
    let mut open = OpenObjectEnvelopeJob::new(
        target(store.snapshot(), fixture),
        context(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match open.poll(store, &NeverCancelled) {
        ObjectEnvelopePoll::Stream(envelope) => envelope,
        other => panic!("expected a staged stream envelope, got {other:?}"),
    }
}

fn length_job(store: &RangeStore, fixture: &Fixture) -> OpenObjectJob {
    let target = IndirectObjectTarget::new(
        store.snapshot(),
        reference(2),
        fixture.object_upper_bound,
        fixture.startxref,
        fixture.startxref,
    )
    .unwrap();
    OpenObjectJob::new(
        target,
        context(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap()
}

fn resolved_length(store: &RangeStore, fixture: &Fixture) -> ResolvedStreamLength {
    let mut open = length_job(store, fixture);
    let object = match open.poll(store, &NeverCancelled) {
        ObjectPoll::Ready(object) => object,
        other => panic!("supplied length object must frame, got {other:?}"),
    };
    ResolvedStreamLength::from_uncompressed_object(&object).unwrap()
}

#[test]
fn indirect_length_is_an_explicit_same_snapshot_dependency() {
    let fixture = indirect_fixture(3);
    let store = supplied_store(&fixture.bytes, 0x41);
    let envelope = open_envelope(&store, &fixture);

    assert_eq!(envelope.snapshot(), store.snapshot());
    assert_eq!(envelope.target().reference(), reference(1));
    assert_eq!(
        envelope.declared_length(),
        DeclaredStreamLength::Indirect {
            reference: reference(2),
            operand_span: envelope.declared_length().operand_span(),
        }
    );
    assert_eq!(
        envelope.declared_length().indirect_reference(),
        Some(reference(2))
    );
    assert_eq!(
        envelope.direct_length_claim().unwrap_err().code(),
        ObjectErrorCode::InvalidStreamLengthClaim
    );

    let resolution = resolved_length(&store, &fixture);
    let claim = envelope.resolved_length_claim(resolution).unwrap();
    assert_eq!(claim.snapshot(), store.snapshot());
    assert_eq!(claim.owner(), reference(1));
    assert_eq!(claim.value(), 3);
    assert_eq!(claim.resolved_value_span(), Some(fixture.length_value_span));

    let mut boundary = OpenStreamBoundaryJob::new(envelope, claim).unwrap();
    let object = match boundary.poll(&store, &NeverCancelled) {
        ObjectPoll::Ready(object) => object,
        other => panic!("expected resolved stream framing, got {other:?}"),
    };
    let IndirectObjectValue::Stream(stream) = object.value() else {
        panic!("the staged result must remain a stream")
    };
    assert_eq!(stream.length_claim(), claim);
    assert_eq!(stream.data_span().len(), 3);
    assert_eq!(boundary.stats().declared_stream_bytes(), 3);
    assert_eq!(boundary.stats().boundary_attempts(), 1);
}

#[test]
fn direct_length_uses_the_same_staged_boundary_contract() {
    let fixture = direct_fixture(b"ABC");
    let store = supplied_store(&fixture.bytes, 0x48);
    let envelope = open_envelope(&store, &fixture);
    assert_eq!(
        envelope.declared_length(),
        DeclaredStreamLength::Direct {
            value: 3,
            operand_span: fixture.length_value_span,
        }
    );
    let claim = envelope.direct_length_claim().unwrap();
    assert_eq!(claim.value(), 3);
    assert_eq!(claim.resolved_value_span(), None);
    let mut boundary = OpenStreamBoundaryJob::new(envelope, claim).unwrap();
    let object = match boundary.poll(&store, &NeverCancelled) {
        ObjectPoll::Ready(object) => object,
        other => panic!("direct staged length must frame, got {other:?}"),
    };
    let IndirectObjectValue::Stream(stream) = object.value() else {
        panic!("direct staged result must be a stream")
    };
    assert_eq!(stream.length_claim(), claim);
    assert_eq!(stream.data_span().len(), 3);
}

#[test]
fn boundary_phase_continues_the_envelope_cumulative_work_cap() {
    let fixture = direct_fixture(b"ABC");
    let store = supplied_store(&fixture.bytes, 0x49);
    let limits = ObjectLimits::default();
    let mut baseline_open = OpenObjectEnvelopeJob::new(
        target(store.snapshot(), &fixture),
        context(),
        limits,
        SyntaxLimits::default(),
    )
    .unwrap();
    let baseline_envelope = match baseline_open.poll(&store, &NeverCancelled) {
        ObjectEnvelopePoll::Stream(envelope) => envelope,
        other => panic!("baseline envelope must complete, got {other:?}"),
    };
    let envelope_stats = baseline_envelope.stats();
    let baseline_claim = baseline_envelope.direct_length_claim().unwrap();
    let mut baseline_boundary =
        OpenStreamBoundaryJob::new(baseline_envelope, baseline_claim).unwrap();
    assert!(matches!(
        baseline_boundary.poll(&store, &NeverCancelled),
        ObjectPoll::Ready(_)
    ));
    let total_stats = baseline_boundary.stats();
    assert!(total_stats.read_bytes() > envelope_stats.read_bytes());
    assert!(total_stats.parse_bytes() > envelope_stats.parse_bytes());

    let caps =
        ObjectWorkCaps::new(total_stats.read_bytes() - 1, total_stats.parse_bytes()).unwrap();
    let mut capped_open = OpenObjectEnvelopeJob::new_with_work_caps(
        target(store.snapshot(), &fixture),
        context(),
        limits,
        SyntaxLimits::default(),
        caps,
    )
    .unwrap();
    let capped_envelope = match capped_open.poll(&store, &NeverCancelled) {
        ObjectEnvelopePoll::Stream(envelope) => envelope,
        other => panic!("one-less aggregate cap must still admit the envelope, got {other:?}"),
    };
    assert_eq!(
        capped_envelope.stats().read_bytes(),
        envelope_stats.read_bytes()
    );
    let capped_claim = capped_envelope.direct_length_claim().unwrap();
    let mut capped_boundary = OpenStreamBoundaryJob::new(capped_envelope, capped_claim).unwrap();
    let error = match capped_boundary.poll(&store, &NeverCancelled) {
        ObjectPoll::Failed(error) => error,
        other => panic!("one-less aggregate cap must reject boundary work, got {other:?}"),
    };
    assert_eq!(error.code(), ObjectErrorCode::ResourceLimit);
    let detail = error.limit().unwrap();
    assert_eq!(detail.kind(), ObjectLimitKind::TotalReadBytes);
    assert_eq!(detail.consumed(), envelope_stats.read_bytes());
    assert_eq!(detail.limit(), total_stats.read_bytes() - 1);
    assert_eq!(capped_boundary.stats().boundary_attempts(), 0);
}

#[test]
fn staged_retained_caps_are_sealed_and_can_only_tighten_above_retained_state() {
    let fixture = direct_fixture(b"ABC");
    let store = supplied_store(&fixture.bytes, 0x50);
    let limits = ObjectLimits::default();

    let baseline_envelope = open_envelope(&store, &fixture);
    let retained = baseline_envelope.retained_heap_bytes();
    assert!(retained > 0);
    let exact_claim = baseline_envelope.direct_length_claim().unwrap();
    let exact_caps = ObjectWorkCaps::new_with_retained_bytes(
        limits.max_total_read_bytes(),
        limits.max_total_parse_bytes(),
        retained,
    )
    .unwrap();
    let mut exact =
        OpenStreamBoundaryJob::new_with_work_caps(baseline_envelope, exact_claim, exact_caps)
            .expect("an uncapped envelope may be tightened to its exact retained capacity");
    assert!(matches!(
        exact.poll(&store, &NeverCancelled),
        ObjectPoll::Ready(_)
    ));

    let below_envelope = open_envelope(&store, &fixture);
    let below_claim = below_envelope.direct_length_claim().unwrap();
    let below_caps = ObjectWorkCaps::new_with_retained_bytes(
        limits.max_total_read_bytes(),
        limits.max_total_parse_bytes(),
        retained - 1,
    )
    .unwrap();
    assert_eq!(
        OpenStreamBoundaryJob::new_with_work_caps(below_envelope, below_claim, below_caps,)
            .unwrap_err()
            .code(),
        ObjectErrorCode::InvalidLimits
    );

    let mut capped_open = OpenObjectEnvelopeJob::new_with_work_caps(
        target(store.snapshot(), &fixture),
        context(),
        limits,
        SyntaxLimits::default(),
        exact_caps,
    )
    .unwrap();
    let capped_envelope = match capped_open.poll(&store, &NeverCancelled) {
        ObjectEnvelopePoll::Stream(envelope) => envelope,
        other => panic!("exact retained envelope must complete, got {other:?}"),
    };
    let capped_claim = capped_envelope.direct_length_claim().unwrap();
    let uncapped_replacement = ObjectWorkCaps::new(
        limits.max_total_read_bytes(),
        limits.max_total_parse_bytes(),
    )
    .unwrap();
    assert_eq!(
        OpenStreamBoundaryJob::new_with_work_caps(
            capped_envelope,
            capped_claim,
            uncapped_replacement,
        )
        .unwrap_err()
        .code(),
        ObjectErrorCode::InvalidLimits
    );
}

#[test]
fn boundary_continuation_caps_can_only_tighten_unspent_envelope_work() {
    let fixture = indirect_fixture(3);
    let store = supplied_store(&fixture.bytes, 0x4a);
    let limits = ObjectLimits::default();

    let baseline_envelope = open_envelope(&store, &fixture);
    let envelope_stats = baseline_envelope.stats();
    let baseline_claim = baseline_envelope
        .resolved_length_claim(resolved_length(&store, &fixture))
        .unwrap();
    let mut baseline = OpenStreamBoundaryJob::new(baseline_envelope, baseline_claim).unwrap();
    assert!(matches!(
        baseline.poll(&store, &NeverCancelled),
        ObjectPoll::Ready(_)
    ));
    let complete = baseline.stats();

    let exact_envelope = open_envelope(&store, &fixture);
    let exact_claim = exact_envelope
        .resolved_length_claim(resolved_length(&store, &fixture))
        .unwrap();
    let exact_caps = ObjectWorkCaps::new(complete.read_bytes(), complete.parse_bytes()).unwrap();
    let mut exact =
        OpenStreamBoundaryJob::new_with_work_caps(exact_envelope, exact_claim, exact_caps).unwrap();
    assert!(matches!(
        exact.poll(&store, &NeverCancelled),
        ObjectPoll::Ready(_)
    ));

    let one_less_envelope = open_envelope(&store, &fixture);
    let one_less_claim = one_less_envelope
        .resolved_length_claim(resolved_length(&store, &fixture))
        .unwrap();
    let one_less_caps =
        ObjectWorkCaps::new(complete.read_bytes() - 1, complete.parse_bytes()).unwrap();
    let mut one_less =
        OpenStreamBoundaryJob::new_with_work_caps(one_less_envelope, one_less_claim, one_less_caps)
            .unwrap();
    let error = match one_less.poll(&store, &NeverCancelled) {
        ObjectPoll::Failed(error) => error,
        other => panic!("one-less continuation must fail before boundary work: {other:?}"),
    };
    let detail = error.limit().unwrap();
    assert_eq!(detail.kind(), ObjectLimitKind::TotalReadBytes);
    assert_eq!(detail.limit(), complete.read_bytes() - 1);
    assert_eq!(detail.consumed(), envelope_stats.read_bytes());
    assert_eq!(
        detail.attempted(),
        complete.read_bytes() - envelope_stats.read_bytes()
    );
    assert_eq!(one_less.stats().boundary_attempts(), 0);

    let below_consumed_envelope = open_envelope(&store, &fixture);
    let below_consumed_claim = below_consumed_envelope
        .resolved_length_claim(resolved_length(&store, &fixture))
        .unwrap();
    let below_consumed_caps = ObjectWorkCaps::new(
        envelope_stats.read_bytes() - 1,
        envelope_stats.parse_bytes(),
    )
    .unwrap();
    assert_eq!(
        OpenStreamBoundaryJob::new_with_work_caps(
            below_consumed_envelope,
            below_consumed_claim,
            below_consumed_caps,
        )
        .unwrap_err()
        .code(),
        ObjectErrorCode::InvalidLimits
    );

    let wider_envelope = open_envelope(&store, &fixture);
    let wider_claim = wider_envelope
        .resolved_length_claim(resolved_length(&store, &fixture))
        .unwrap();
    let wider_caps = ObjectWorkCaps::new(
        limits.max_total_read_bytes() + 1,
        limits.max_total_parse_bytes(),
    )
    .unwrap();
    assert_eq!(
        OpenStreamBoundaryJob::new_with_work_caps(wider_envelope, wider_claim, wider_caps)
            .unwrap_err()
            .code(),
        ObjectErrorCode::InvalidLimits
    );
}

#[test]
fn mismatched_resolution_reference_or_snapshot_is_rejected_before_reads() {
    let fixture = indirect_fixture_with_reference(3, 3);
    let store = supplied_store(&fixture.bytes, 0x42);
    let envelope = open_envelope(&store, &fixture);
    let wrong_reference = resolved_length(&store, &fixture);
    assert_eq!(
        envelope
            .resolved_length_claim(wrong_reference)
            .unwrap_err()
            .code(),
        ObjectErrorCode::InvalidStreamLengthClaim
    );

    let matching_fixture = indirect_fixture(3);
    let matching_store = supplied_store(&matching_fixture.bytes, 0x43);
    let matching_envelope = open_envelope(&matching_store, &matching_fixture);
    let other_store = supplied_store(&matching_fixture.bytes, 0x44);
    let wrong_snapshot = resolved_length(&other_store, &matching_fixture);
    assert_eq!(
        matching_envelope
            .resolved_length_claim(wrong_snapshot)
            .unwrap_err()
            .diagnostic_id(),
        "RPE-OBJECT-0022"
    );
}

#[test]
fn sparse_staged_reads_skip_the_unrequested_payload_tail_and_resume_by_checkpoint() {
    let fixture = indirect_fixture(8192);
    let source = snapshot(u64::try_from(fixture.bytes.len()).unwrap(), 0x44);
    let store = RangeStore::new(source, Default::default()).unwrap();
    let mut open = OpenObjectEnvelopeJob::new(
        target(source, &fixture),
        context(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();

    let (envelope_missing, envelope_checkpoint) = match open.poll(&store, &NeverCancelled) {
        ObjectEnvelopePoll::Pending {
            missing,
            checkpoint,
            ..
        } => (missing, checkpoint),
        other => panic!("empty range store must suspend the envelope, got {other:?}"),
    };
    assert_eq!(envelope_checkpoint, context().envelope_checkpoint());
    let mut supplied_bytes = 0_u64;
    for range in envelope_missing.as_slice() {
        let start = usize::try_from(range.start()).unwrap();
        let end = usize::try_from(range.end_exclusive()).unwrap();
        supplied_bytes += range.len();
        store
            .supply(RangeResponse::new(source, *range, fixture.bytes[start..end].to_vec()).unwrap())
            .unwrap();
    }
    let envelope = match open.poll(&store, &NeverCancelled) {
        ObjectEnvelopePoll::Stream(envelope) => envelope,
        other => panic!("supplied envelope range must resume, got {other:?}"),
    };
    let mut length_open = length_job(&store, &fixture);
    let length_missing = match length_open.poll(&store, &NeverCancelled) {
        ObjectPoll::Pending {
            missing,
            checkpoint,
            ..
        } => {
            assert_eq!(checkpoint, context().envelope_checkpoint());
            missing
        }
        other => panic!("unsupplied length object must suspend, got {other:?}"),
    };
    for range in length_missing.as_slice() {
        let start = usize::try_from(range.start()).unwrap();
        let end = usize::try_from(range.end_exclusive()).unwrap();
        supplied_bytes += range.len();
        store
            .supply(RangeResponse::new(source, *range, fixture.bytes[start..end].to_vec()).unwrap())
            .unwrap();
    }
    let length_object = match length_open.poll(&store, &NeverCancelled) {
        ObjectPoll::Ready(object) => object,
        other => panic!("supplied length object must resume, got {other:?}"),
    };
    let resolution = ResolvedStreamLength::from_uncompressed_object(&length_object).unwrap();
    let claim = envelope.resolved_length_claim(resolution).unwrap();
    let mut boundary = OpenStreamBoundaryJob::new(envelope, claim).unwrap();
    let (boundary_missing, boundary_checkpoint) = match boundary.poll(&store, &NeverCancelled) {
        ObjectPoll::Pending {
            missing,
            checkpoint,
            ..
        } => (missing, checkpoint),
        other => panic!("missing exact payload-end bytes must suspend, got {other:?}"),
    };
    assert_eq!(boundary_checkpoint, context().boundary_checkpoint());
    for range in boundary_missing.as_slice() {
        let start = usize::try_from(range.start()).unwrap();
        let end = usize::try_from(range.end_exclusive()).unwrap();
        supplied_bytes += range.len();
        store
            .supply(RangeResponse::new(source, *range, fixture.bytes[start..end].to_vec()).unwrap())
            .unwrap();
    }
    assert!(matches!(
        boundary.poll(&store, &NeverCancelled),
        ObjectPoll::Ready(_)
    ));
    assert!(
        supplied_bytes < 8192,
        "staged framing must not fetch the complete opaque payload"
    );
}

#[test]
fn staged_jobs_reject_source_change_and_cancellation_without_losing_terminal_state() {
    let fixture = indirect_fixture(3);
    let source = snapshot(u64::try_from(fixture.bytes.len()).unwrap(), 0x45);
    let mut open = OpenObjectEnvelopeJob::new(
        target(source, &fixture),
        context(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let changed = RangeStore::new(
        snapshot(u64::try_from(fixture.bytes.len()).unwrap(), 0x46),
        Default::default(),
    )
    .unwrap();
    let error = match open.poll(&changed, &NeverCancelled) {
        ObjectEnvelopePoll::Failed(error) => error,
        other => panic!("source change must fail before reading, got {other:?}"),
    };
    assert_eq!(error.code(), ObjectErrorCode::SnapshotMismatch);
    assert_eq!(
        open.poll(&changed, &NeverCancelled),
        ObjectEnvelopePoll::Failed(error)
    );

    let store = supplied_store(&fixture.bytes, 0x47);
    let envelope = open_envelope(&store, &fixture);
    let claim = envelope
        .resolved_length_claim(resolved_length(&store, &fixture))
        .unwrap();
    let mut boundary = OpenStreamBoundaryJob::new(envelope, claim).unwrap();
    let cancelled = AtomicBool::new(true);
    let error = match boundary.poll(&store, &cancelled) {
        ObjectPoll::Failed(error) => error,
        other => panic!("cancellation must be terminal, got {other:?}"),
    };
    assert_eq!(error.code(), ObjectErrorCode::Cancelled);
    assert_eq!(
        boundary.poll(&store, &NeverCancelled),
        ObjectPoll::Failed(error)
    );
}
