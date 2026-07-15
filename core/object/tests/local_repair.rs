use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_object::{
    IndirectObjectTarget, IndirectObjectValue, LocalObjectJobContext, LocalObjectPhase,
    LocalObjectPoll, NeverCancelled, ObjectError, ObjectErrorCode, ObjectJobContext,
    ObjectLimitConfig, ObjectLimitKind, ObjectLimits, ObjectRepairKind, ObjectRepairLimitConfig,
    ObjectRepairLimits, OpenLocalObjectJob,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};

const STRICT_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(801);
const STRICT_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(802);
const CANDIDATE_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(803);
const CANDIDATE_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(804);
const HEADER_SCAN: ResumeCheckpoint = ResumeCheckpoint::new(805);
const LENGTH_SCAN: ResumeCheckpoint = ResumeCheckpoint::new(806);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    reference: ObjectRef,
    actual_offset: u64,
    upper_bound: u64,
    revision_startxref: u64,
}

impl Fixture {
    fn target(&self, declared_offset: u64) -> IndirectObjectTarget {
        IndirectObjectTarget::new(
            self.snapshot,
            self.reference,
            declared_offset,
            self.upper_bound,
            self.revision_startxref,
        )
        .unwrap()
    }

    fn store(&self, supplied: bool) -> RangeStore {
        let store = RangeStore::new(self.snapshot, Default::default()).unwrap();
        if supplied {
            let range = ByteRange::new(0, u64::try_from(self.bytes.len()).unwrap()).unwrap();
            store
                .supply(RangeResponse::new(self.snapshot, range, self.bytes.clone()).unwrap())
                .unwrap();
        }
        store
    }
}

fn snapshot(len: u64, tag: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([tag; 32]),
            SourceRevision::new(u64::from(tag)),
        ),
        Some(len),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [tag.wrapping_add(1); 32],
        ),
    )
}

fn fixture(body: &[u8], reference: ObjectRef, tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let actual_offset = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(body);
    let upper_bound = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"xref\n");
    let revision_startxref = upper_bound;
    let source = snapshot(u64::try_from(bytes.len()).unwrap(), tag);
    Fixture {
        bytes,
        snapshot: source,
        reference,
        actual_offset,
        upper_bound,
        revision_startxref,
    }
}

fn direct_fixture() -> Fixture {
    fixture(
        b"1 0 obj\n42\nendobj\n",
        ObjectRef::new(1, 0).unwrap(),
        0x91,
    )
}

fn stream_fixture(declared: u64, payload: &[u8], tag: u8) -> Fixture {
    let mut body = format!("2 0 obj\n<< /Length {declared} >>\nstream\n").into_bytes();
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\nendstream\nendobj\n");
    fixture(&body, ObjectRef::new(2, 0).unwrap(), tag)
}

fn context() -> LocalObjectJobContext {
    LocalObjectJobContext::new(
        ObjectJobContext::new(
            JobId::new(800),
            STRICT_ENVELOPE,
            STRICT_BOUNDARY,
            RequestPriority::Metadata,
        ),
        CANDIDATE_ENVELOPE,
        CANDIDATE_BOUNDARY,
        HEADER_SCAN,
        LENGTH_SCAN,
    )
}

fn limits(update: impl FnOnce(&mut ObjectRepairLimitConfig)) -> ObjectRepairLimits {
    let mut config = ObjectRepairLimitConfig::default();
    update(&mut config);
    ObjectRepairLimits::validate(config).unwrap()
}

fn ready(
    fixture: &Fixture,
    declared_offset: u64,
    repair_limits: ObjectRepairLimits,
) -> pdf_rs_object::LocallyFramedObject {
    let store = fixture.store(true);
    let mut job = OpenLocalObjectJob::new(
        fixture.target(declared_offset),
        context(),
        ObjectLimits::default(),
        repair_limits,
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &NeverCancelled) {
        LocalObjectPoll::Ready(object) => object,
        LocalObjectPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
        LocalObjectPoll::Failed(error) => panic!("expected local object success: {error}"),
    }
}

fn ready_with_object_limits(
    fixture: &Fixture,
    declared_offset: u64,
    object_limits: ObjectLimits,
    repair_limits: ObjectRepairLimits,
) -> pdf_rs_object::LocallyFramedObject {
    let store = fixture.store(true);
    let mut job = OpenLocalObjectJob::new(
        fixture.target(declared_offset),
        context(),
        object_limits,
        repair_limits,
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &NeverCancelled) {
        LocalObjectPoll::Ready(object) => object,
        LocalObjectPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
        LocalObjectPoll::Failed(error) => panic!("expected local object success: {error}"),
    }
}

fn failed(
    fixture: &Fixture,
    declared_offset: u64,
    repair_limits: ObjectRepairLimits,
) -> ObjectError {
    let store = fixture.store(true);
    let mut job = OpenLocalObjectJob::new(
        fixture.target(declared_offset),
        context(),
        ObjectLimits::default(),
        repair_limits,
        SyntaxLimits::default(),
    )
    .unwrap();
    let error = match job.poll(&store, &NeverCancelled) {
        LocalObjectPoll::Failed(error) => error,
        LocalObjectPoll::Ready(_) => panic!("expected local object failure"),
        LocalObjectPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
    };
    assert!(matches!(
        job.poll(&store, &NeverCancelled),
        LocalObjectPoll::Failed(repeated) if repeated == error
    ));
    error
}

fn failed_with_object_limits(
    fixture: &Fixture,
    declared_offset: u64,
    object_limits: ObjectLimits,
    repair_limits: ObjectRepairLimits,
) -> ObjectError {
    let store = fixture.store(true);
    let mut job = OpenLocalObjectJob::new(
        fixture.target(declared_offset),
        context(),
        object_limits,
        repair_limits,
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &NeverCancelled) {
        LocalObjectPoll::Failed(error) => error,
        LocalObjectPoll::Ready(_) => panic!("expected local object failure"),
        LocalObjectPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
    }
}

#[test]
fn canonical_input_uses_only_the_strict_child() {
    let fixture = direct_fixture();
    let object = ready(
        &fixture,
        fixture.actual_offset,
        ObjectRepairLimits::default(),
    );
    assert!(object.diagnostics().is_empty());
    assert_eq!(object.declared_xref_offset(), fixture.actual_offset);
    assert_eq!(object.effective_xref_offset(), fixture.actual_offset);
    assert_eq!(object.reference(), fixture.reference);
    assert_eq!(object.stats().repair_scan_bytes(), 0);
    assert_eq!(object.stats().candidate().read_bytes(), 0);
}

#[test]
fn nearby_expected_header_is_normally_reframed_with_offset_evidence() {
    let fixture = direct_fixture();
    let declared = fixture.actual_offset - 1;
    let object = ready(&fixture, declared, ObjectRepairLimits::default());
    assert_eq!(object.declared_xref_offset(), declared);
    assert_eq!(object.effective_xref_offset(), fixture.actual_offset);
    assert_eq!(object.diagnostics().len(), 1);
    let diagnostic = object.diagnostics()[0];
    assert_eq!(diagnostic.kind(), ObjectRepairKind::ObjectOffset);
    assert_eq!(diagnostic.diagnostic_id(), "RPE-OBJECT-REPAIR-0001");
    assert_eq!(diagnostic.declared(), declared);
    assert_eq!(diagnostic.effective(), fixture.actual_offset);
    assert_eq!(diagnostic.reference(), fixture.reference);
    assert!(object.stats().strict().read_bytes() > 0);
    assert!(object.stats().candidate().read_bytes() > 0);
}

#[test]
fn direct_length_is_repaired_only_to_a_unique_strict_boundary() {
    let stream_fixture = stream_fixture(3, b"DATA", 0x92);
    let object = ready(
        &stream_fixture,
        stream_fixture.actual_offset,
        ObjectRepairLimits::default(),
    );
    assert_eq!(object.diagnostics().len(), 1);
    let diagnostic = object.diagnostics()[0];
    assert_eq!(diagnostic.kind(), ObjectRepairKind::DirectStreamLength);
    assert_eq!(diagnostic.diagnostic_id(), "RPE-OBJECT-REPAIR-0002");
    assert_eq!(diagnostic.declared(), 3);
    assert_eq!(diagnostic.effective(), 4);
    let IndirectObjectValue::Stream(stream) = object.value() else {
        panic!("fixture must publish a framed stream")
    };
    assert_eq!(stream.length_claim().declaration().direct_value(), Some(3));
    assert_eq!(stream.length_claim().value(), 4);
    assert_eq!(stream.data_span().len(), 4);

    let crlf = fixture(
        b"2 0 obj\r\n<< /Length 3 >>\r\nstream\r\nDATA\r\nendstream\r\nendobj\r\n",
        ObjectRef::new(2, 0).unwrap(),
        0x9c,
    );
    let repaired = ready(&crlf, crlf.actual_offset, ObjectRepairLimits::default());
    assert_eq!(repaired.diagnostics()[0].effective(), 4);

    let bare_cr = fixture(
        b"2 0 obj\n<< /Length 3 >>\nstream\nDATA\rendstream\nendobj\n",
        ObjectRef::new(2, 0).unwrap(),
        0x9d,
    );
    assert_eq!(
        failed(
            &bare_cr,
            bare_cr.actual_offset,
            ObjectRepairLimits::default()
        )
        .code(),
        ObjectErrorCode::LocalRepairFailed
    );
}

#[test]
fn offset_and_length_repairs_compose_without_losing_evidence() {
    let fixture = stream_fixture(5, b"DATA", 0x93);
    let object = ready(
        &fixture,
        fixture.actual_offset - 1,
        ObjectRepairLimits::default(),
    );
    assert_eq!(object.diagnostics().len(), 2);
    assert_eq!(
        object.diagnostics()[0].kind(),
        ObjectRepairKind::ObjectOffset
    );
    assert_eq!(
        object.diagnostics()[1].kind(),
        ObjectRepairKind::DirectStreamLength
    );
    assert_eq!(object.diagnostics()[1].declared(), 5);
    assert_eq!(object.diagnostics()[1].effective(), 4);
}

#[test]
fn semantic_damage_and_ambiguous_candidates_are_never_repaired() {
    let damaged = fixture(
        b"9 0 obj\n42\nendobj\n",
        ObjectRef::new(1, 0).unwrap(),
        0x94,
    );
    assert_eq!(
        failed(
            &damaged,
            damaged.actual_offset - 1,
            ObjectRepairLimits::default()
        )
        .code(),
        ObjectErrorCode::LocalRepairFailed
    );

    let ambiguous_header = fixture(
        b"1 0 obj\n1\nendobj\n 1 0 obj\n2\nendobj\n",
        ObjectRef::new(1, 0).unwrap(),
        0x95,
    );
    assert_eq!(
        failed(
            &ambiguous_header,
            ambiguous_header.actual_offset - 1,
            ObjectRepairLimits::default()
        )
        .code(),
        ObjectErrorCode::AmbiguousRepair
    );
    let error = failed(
        &ambiguous_header,
        ambiguous_header.actual_offset - 1,
        limits(|config| config.max_header_candidates = 1),
    );
    assert_eq!(
        error.limit().unwrap().kind(),
        ObjectLimitKind::RepairHeaderCandidates
    );

    let payload = b"A\nendstream\nendobj\nB";
    let ambiguous_length = stream_fixture(u64::try_from(payload.len()).unwrap() - 1, payload, 0x96);
    assert_eq!(
        failed(
            &ambiguous_length,
            ambiguous_length.actual_offset,
            ObjectRepairLimits::default()
        )
        .code(),
        ObjectErrorCode::AmbiguousRepair
    );
    let error = failed(
        &ambiguous_length,
        ambiguous_length.actual_offset,
        limits(|config| config.max_boundary_candidates = 1),
    );
    assert_eq!(
        error.limit().unwrap().kind(),
        ObjectLimitKind::RepairBoundaryCandidates
    );
}

#[test]
fn repair_deltas_and_scan_budget_are_exact() {
    let offset = direct_fixture();
    ready(
        &offset,
        offset.actual_offset - 1,
        limits(|config| config.max_object_offset_delta = 1),
    );
    assert_eq!(
        failed(
            &offset,
            offset.actual_offset - 2,
            limits(|config| config.max_object_offset_delta = 1)
        )
        .code(),
        ObjectErrorCode::LocalRepairFailed
    );

    let length = stream_fixture(3, b"DATA", 0x97);
    let baseline = ready(&length, length.actual_offset, ObjectRepairLimits::default());
    let exact_scan = baseline.stats().repair_scan_bytes();
    ready(
        &length,
        length.actual_offset,
        limits(|config| config.max_scan_bytes = exact_scan),
    );
    let error = failed(
        &length,
        length.actual_offset,
        limits(|config| config.max_scan_bytes = exact_scan - 1),
    );
    assert_eq!(error.code(), ObjectErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        ObjectLimitKind::RepairScanBytes
    );

    ready(
        &length,
        length.actual_offset,
        limits(|config| config.max_stream_length_delta = 1),
    );
    let length_two_away = stream_fixture(2, b"DATA", 0x9b);
    assert_eq!(
        failed(
            &length_two_away,
            length_two_away.actual_offset,
            limits(|config| config.max_stream_length_delta = 1)
        )
        .code(),
        ObjectErrorCode::LocalRepairFailed
    );
}

#[test]
fn boundary_candidate_attempts_and_child_validation_work_are_aggregate_bounded() {
    let overlong_boundary = fixture(
        b"2 0 obj\n<< /Length 3 >>\nstream\nDATA\nendstream                    endobj\n",
        ObjectRef::new(2, 0).unwrap(),
        0x9e,
    );
    let source_len = u64::try_from(overlong_boundary.bytes.len()).unwrap();
    let boundary_capped = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: 48,
        max_envelope_bytes: 48,
        initial_boundary_bytes: 8,
        max_boundary_bytes: 16,
        max_stream_bytes: source_len,
        max_total_read_bytes: 256,
        max_total_parse_bytes: 256,
    })
    .unwrap();
    assert_eq!(
        failed_with_object_limits(
            &overlong_boundary,
            overlong_boundary.actual_offset,
            boundary_capped,
            ObjectRepairLimits::default(),
        )
        .code(),
        ObjectErrorCode::LocalRepairFailed
    );

    let one_invalid_anchor = fixture(
        b"2 0 obj\n<< /Length 3 >>\nstream\nDATA\nendstream\nnope\n",
        ObjectRef::new(2, 0).unwrap(),
        0x9f,
    );
    assert_eq!(
        failed(
            &one_invalid_anchor,
            one_invalid_anchor.actual_offset,
            limits(|config| config.max_boundary_candidates = 1),
        )
        .code(),
        ObjectErrorCode::LocalRepairFailed
    );
    let two_invalid_anchors = fixture(
        b"2 0 obj\n<< /Length 3 >>\nstream\nDATA\nendstream\nnope X\nendstream\nnope\n",
        ObjectRef::new(2, 0).unwrap(),
        0xa1,
    );
    let error = failed(
        &two_invalid_anchors,
        two_invalid_anchors.actual_offset,
        limits(|config| config.max_boundary_candidates = 1),
    );
    assert_eq!(
        error.limit().unwrap().kind(),
        ObjectLimitKind::RepairBoundaryCandidates
    );

    let length = stream_fixture(3, b"DATA", 0xa0);
    let source_len = u64::try_from(length.bytes.len()).unwrap();
    let envelope_bytes = length.upper_bound - length.actual_offset + 1;
    let baseline_limits = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: envelope_bytes,
        max_envelope_bytes: envelope_bytes,
        initial_boundary_bytes: 32,
        max_boundary_bytes: 32,
        max_stream_bytes: source_len,
        max_total_read_bytes: 256,
        max_total_parse_bytes: 256,
    })
    .unwrap();
    let baseline = ready_with_object_limits(
        &length,
        length.actual_offset,
        baseline_limits,
        ObjectRepairLimits::default(),
    );
    let exact_read = baseline
        .stats()
        .strict()
        .read_bytes()
        .checked_add(baseline.stats().envelope_replay().read_bytes())
        .unwrap();
    let exact_parse = baseline
        .stats()
        .strict()
        .parse_bytes()
        .checked_add(baseline.stats().envelope_replay().parse_bytes())
        .unwrap();
    let exact_limits = ObjectLimits::validate(ObjectLimitConfig {
        max_total_read_bytes: exact_read,
        max_total_parse_bytes: exact_parse,
        ..ObjectLimitConfig {
            max_source_bytes: source_len,
            initial_envelope_bytes: envelope_bytes,
            max_envelope_bytes: envelope_bytes,
            initial_boundary_bytes: 32,
            max_boundary_bytes: 32,
            max_stream_bytes: source_len,
            max_total_read_bytes: 256,
            max_total_parse_bytes: 256,
        }
    })
    .unwrap();
    ready_with_object_limits(
        &length,
        length.actual_offset,
        exact_limits,
        ObjectRepairLimits::default(),
    );
    let one_less_read = ObjectLimits::validate(ObjectLimitConfig {
        max_total_read_bytes: exact_read - 1,
        ..ObjectLimitConfig {
            max_source_bytes: source_len,
            initial_envelope_bytes: envelope_bytes,
            max_envelope_bytes: envelope_bytes,
            initial_boundary_bytes: 32,
            max_boundary_bytes: 32,
            max_stream_bytes: source_len,
            max_total_read_bytes: 256,
            max_total_parse_bytes: exact_parse,
        }
    })
    .unwrap();
    let error = failed_with_object_limits(
        &length,
        length.actual_offset,
        one_less_read,
        ObjectRepairLimits::default(),
    );
    assert_eq!(
        error.limit().unwrap().kind(),
        ObjectLimitKind::TotalReadBytes
    );
    let one_less_parse = ObjectLimits::validate(ObjectLimitConfig {
        max_total_parse_bytes: exact_parse - 1,
        ..ObjectLimitConfig {
            max_source_bytes: source_len,
            initial_envelope_bytes: envelope_bytes,
            max_envelope_bytes: envelope_bytes,
            initial_boundary_bytes: 32,
            max_boundary_bytes: 32,
            max_stream_bytes: source_len,
            max_total_read_bytes: exact_read,
            max_total_parse_bytes: 256,
        }
    })
    .unwrap();
    let error = failed_with_object_limits(
        &length,
        length.actual_offset,
        one_less_parse,
        ObjectRepairLimits::default(),
    );
    assert_eq!(
        error.limit().unwrap().kind(),
        ObjectLimitKind::TotalParseBytes
    );
}

#[test]
fn repair_pending_repoll_does_not_recharge_and_source_change_is_terminal() {
    let stream = stream_fixture(3, b"DATA", 0x98);
    let store = stream.store(false);
    let source_len = u64::try_from(stream.bytes.len()).unwrap();
    let object_limits = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: 48,
        max_envelope_bytes: 48,
        initial_boundary_bytes: 8,
        max_boundary_bytes: 32,
        max_stream_bytes: source_len,
        max_total_read_bytes: 256,
        max_total_parse_bytes: 256,
    })
    .unwrap();
    let mut job = OpenLocalObjectJob::new(
        stream.target(stream.actual_offset),
        context(),
        object_limits,
        ObjectRepairLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let mut observed_scan = false;
    loop {
        match job.poll(&store, &NeverCancelled) {
            LocalObjectPoll::Pending {
                missing,
                checkpoint,
                ..
            } => {
                if checkpoint == LENGTH_SCAN {
                    observed_scan = true;
                    let charged = job.stats().repair_scan_bytes();
                    assert!(matches!(
                        job.poll(&store, &NeverCancelled),
                        LocalObjectPoll::Pending { checkpoint: repeated, .. }
                            if repeated == checkpoint
                    ));
                    assert_eq!(job.stats().repair_scan_bytes(), charged);
                }
                for range in missing.as_slice() {
                    let start = usize::try_from(range.start()).unwrap();
                    let end = usize::try_from(range.end_exclusive()).unwrap();
                    store
                        .supply(
                            RangeResponse::new(
                                store.snapshot(),
                                *range,
                                stream.bytes[start..end].to_vec(),
                            )
                            .unwrap(),
                        )
                        .unwrap();
                }
            }
            LocalObjectPoll::Ready(_) => {
                assert!(observed_scan);
                break;
            }
            LocalObjectPoll::Failed(error) => panic!("repair resume failed: {error}"),
        }
    }

    let foreign = fixture(
        b"2 0 obj\n<< /Length 3 >>\nstream\nDATA\nendstream\nendobj\n",
        ObjectRef::new(2, 0).unwrap(),
        0x99,
    );
    let mut changed = OpenLocalObjectJob::new(
        stream.target(stream.actual_offset),
        context(),
        ObjectLimits::default(),
        ObjectRepairLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let foreign_store = foreign.store(true);
    assert!(matches!(
        changed.poll(&foreign_store, &NeverCancelled),
        LocalObjectPoll::Failed(error) if error.code() == ObjectErrorCode::SnapshotMismatch
    ));
    assert_eq!(changed.phase(), LocalObjectPhase::Failed);
}

#[test]
fn unsupported_and_cancelled_strict_failures_stay_terminal_and_candidate_resource_propagates() {
    let indirect = fixture(
        b"2 0 obj\n<< /Length 3 0 R >>\nstream\nDATA\nendstream\nendobj\n",
        ObjectRef::new(2, 0).unwrap(),
        0x9a,
    );
    assert_eq!(
        failed(
            &indirect,
            indirect.actual_offset,
            ObjectRepairLimits::default()
        )
        .code(),
        ObjectErrorCode::UnsupportedIndirectLength
    );

    let offset = direct_fixture();
    let store = offset.store(true);
    let source_len = u64::try_from(offset.bytes.len()).unwrap();
    let constrained = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: 10,
        max_envelope_bytes: 10,
        initial_boundary_bytes: 1,
        max_boundary_bytes: 1,
        max_stream_bytes: 1,
        max_total_read_bytes: 11,
        max_total_parse_bytes: 11,
    })
    .unwrap();
    let mut candidate_resource = OpenLocalObjectJob::new(
        offset.target(offset.actual_offset - 1),
        context(),
        constrained,
        ObjectRepairLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        candidate_resource.poll(&store, &NeverCancelled),
        LocalObjectPoll::Failed(error)
            if error.code() == ObjectErrorCode::ResourceLimit
                && error.limit().unwrap().kind() == ObjectLimitKind::TotalReadBytes
    ));
    assert!(candidate_resource.stats().repair_scan_bytes() > 0);

    let fixture = direct_fixture();
    let store = fixture.store(true);
    let cancelled = AtomicBool::new(true);
    let mut job = OpenLocalObjectJob::new(
        fixture.target(fixture.actual_offset),
        context(),
        ObjectLimits::default(),
        ObjectRepairLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        job.poll(&store, &cancelled),
        LocalObjectPoll::Failed(error) if error.code() == ObjectErrorCode::Cancelled
    ));
    assert_eq!(job.stats().repair_scan_bytes(), 0);
    cancelled.store(false, Ordering::Release);
    assert!(matches!(
        job.poll(&store, &cancelled),
        LocalObjectPoll::Failed(error) if error.code() == ObjectErrorCode::Cancelled
    ));
}

#[test]
fn configuration_rejects_zero_hard_overrides_and_duplicate_checkpoints() {
    const HARD: ObjectRepairLimitConfig = ObjectRepairLimitConfig {
        max_object_offset_delta: 4096,
        max_stream_length_delta: 64 * 1024,
        max_scan_bytes: 64 * 1024 * 1024,
        max_header_candidates: 64,
        max_boundary_candidates: 64,
    };
    assert!(ObjectRepairLimits::validate(HARD).is_ok());
    let mutations: [fn(&mut ObjectRepairLimitConfig); 5] = [
        |config| config.max_object_offset_delta = 0,
        |config| config.max_stream_length_delta = 0,
        |config| config.max_scan_bytes = 0,
        |config| config.max_header_candidates = 0,
        |config| config.max_boundary_candidates = 0,
    ];
    for mutation in mutations {
        let mut config = ObjectRepairLimitConfig::default();
        mutation(&mut config);
        assert_eq!(
            ObjectRepairLimits::validate(config).unwrap_err().code(),
            ObjectErrorCode::InvalidRepairLimits
        );
    }
    for config in [
        ObjectRepairLimitConfig {
            max_object_offset_delta: HARD.max_object_offset_delta + 1,
            ..HARD
        },
        ObjectRepairLimitConfig {
            max_stream_length_delta: HARD.max_stream_length_delta + 1,
            ..HARD
        },
        ObjectRepairLimitConfig {
            max_scan_bytes: HARD.max_scan_bytes + 1,
            ..HARD
        },
        ObjectRepairLimitConfig {
            max_header_candidates: HARD.max_header_candidates + 1,
            ..HARD
        },
        ObjectRepairLimitConfig {
            max_boundary_candidates: HARD.max_boundary_candidates + 1,
            ..HARD
        },
    ] {
        assert_eq!(
            ObjectRepairLimits::validate(config).unwrap_err().code(),
            ObjectErrorCode::InvalidRepairLimits
        );
    }

    let fixture = direct_fixture();
    let duplicate = LocalObjectJobContext::new(
        context().strict(),
        STRICT_ENVELOPE,
        CANDIDATE_BOUNDARY,
        HEADER_SCAN,
        LENGTH_SCAN,
    );
    assert_eq!(
        OpenLocalObjectJob::new(
            fixture.target(fixture.actual_offset),
            duplicate,
            ObjectLimits::default(),
            ObjectRepairLimits::default(),
            SyntaxLimits::default(),
        )
        .unwrap_err()
        .code(),
        ObjectErrorCode::InvalidRepairJobContext
    );
}
