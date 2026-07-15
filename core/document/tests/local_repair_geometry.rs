use std::mem;
use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AttestLocalRepairRevisionJob, DocumentErrorCode, DocumentLimitConfig, DocumentLimitKind,
    DocumentLimits, EffectiveObjectOffset, LocalRepairPlanningRevision,
    LocalRevisionAttestationJobContext, LocalRevisionAttestationPoll, NeverCancelled,
    ObjectAttestationKind, RevisionAttestationLimitConfig, RevisionAttestationLimits,
    RevisionAttestationPhase, RevisionId,
};
use pdf_rs_object::{
    LocalObjectJobContext, LocalObjectPoll, NeverCancelled as NeverObjectCancelled,
    ObjectJobContext, ObjectLimits, ObjectRepairKind, ObjectRepairLimits, OpenLocalObjectJob,
    OpenObjectJob,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    LocalXrefJobContext, LocalXrefPoll, LocallyParsedXrefSection, OpenLocalXrefJob, XrefJobContext,
    XrefLimits, XrefRepairLimits,
};

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    actual_offsets: [u64; 3],
    declared_offsets: [u64; 3],
    startxref: u64,
}

impl Fixture {
    fn store(&self) -> RangeStore {
        let store = RangeStore::new(self.snapshot, Default::default()).unwrap();
        let range = ByteRange::new(0, u64::try_from(self.bytes.len()).unwrap()).unwrap();
        store
            .supply(RangeResponse::new(self.snapshot, range, self.bytes.clone()).unwrap())
            .unwrap();
        store
    }
}

fn fixture() -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut actual_offsets = [0_u64; 3];
    for (index, body) in [
        b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".as_slice(),
        b"2 0 obj\n42\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Length 3 >>\nstream\nDATA\nendstream\nendobj\n".as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        actual_offsets[index] = u64::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(body);
    }
    let startxref = u64::try_from(bytes.len()).unwrap();
    let declared_offsets = [actual_offsets[0], actual_offsets[1] + 1, actual_offsets[2]];
    bytes.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            declared_offsets[0], declared_offsets[1], declared_offsets[2]
        )
        .as_bytes(),
    );
    let len = u64::try_from(bytes.len()).unwrap();
    let snapshot = SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0xb1; 32]), SourceRevision::new(7)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xb2; 32]),
    );
    Fixture {
        bytes,
        snapshot,
        actual_offsets,
        declared_offsets,
        startxref,
    }
}

fn open_xref(fixture: &Fixture, store: &RangeStore) -> LocallyParsedXrefSection {
    open_xref_for_snapshot(fixture.snapshot, store)
}

fn open_xref_for_snapshot(
    snapshot: SourceSnapshot,
    store: &RangeStore,
) -> LocallyParsedXrefSection {
    let strict = XrefJobContext::new(
        JobId::new(900),
        ResumeCheckpoint::new(901),
        ResumeCheckpoint::new(902),
    );
    let mut job = OpenLocalXrefJob::new(
        snapshot,
        LocalXrefJobContext::new(
            strict,
            ResumeCheckpoint::new(903),
            ResumeCheckpoint::new(904),
        ),
        XrefLimits::default(),
        XrefRepairLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(store, &pdf_rs_xref::NeverCancelled) {
        LocalXrefPoll::Ready(section) => section,
        LocalXrefPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
        LocalXrefPoll::Failed(error) => panic!("local xref failed: {error}"),
    }
}

fn object_context() -> LocalObjectJobContext {
    LocalObjectJobContext::new(
        ObjectJobContext::new(
            JobId::new(910),
            ResumeCheckpoint::new(911),
            ResumeCheckpoint::new(912),
            RequestPriority::Metadata,
        ),
        ResumeCheckpoint::new(913),
        ResumeCheckpoint::new(914),
        ResumeCheckpoint::new(915),
        ResumeCheckpoint::new(916),
    )
}

fn attestation_context() -> LocalRevisionAttestationJobContext {
    LocalRevisionAttestationJobContext::new(ResumeCheckpoint::new(917), object_context())
}

fn rebuilt(
    fixture: &Fixture,
    store: &RangeStore,
) -> pdf_rs_document::LocallyRebuiltCandidateRevision {
    let (plan, evidence) = plan_and_evidence(fixture, store);
    plan.rebuild(evidence, &NeverCancelled).unwrap()
}

fn attestation_job(
    fixture: &Fixture,
    store: &RangeStore,
    limits: RevisionAttestationLimits,
) -> AttestLocalRepairRevisionJob {
    AttestLocalRepairRevisionJob::new(
        rebuilt(fixture, store),
        attestation_context(),
        limits,
        ObjectLimits::default(),
        ObjectRepairLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap()
}

fn plan_and_evidence(
    fixture: &Fixture,
    store: &RangeStore,
) -> (LocalRepairPlanningRevision, Vec<EffectiveObjectOffset>) {
    plan_and_evidence_with_limits(fixture, store, DocumentLimits::default())
}

fn plan_and_evidence_with_limits(
    fixture: &Fixture,
    store: &RangeStore,
    limits: DocumentLimits,
) -> (LocalRepairPlanningRevision, Vec<EffectiveObjectOffset>) {
    let xref = open_xref(fixture, store);
    let plan = LocalRepairPlanningRevision::new(xref, RevisionId::new(1), limits, &NeverCancelled)
        .unwrap();
    let references: Vec<ObjectRef> = plan
        .physical_intervals()
        .iter()
        .map(|interval| interval.reference())
        .collect();
    let mut evidence = Vec::with_capacity(references.len());
    for reference in references {
        let target = plan.unattested_target(reference).unwrap();
        let mut job = OpenLocalObjectJob::new(
            target,
            object_context(),
            ObjectLimits::default(),
            ObjectRepairLimits::default(),
            SyntaxLimits::default(),
        )
        .unwrap();
        let object = match job.poll(store, &NeverObjectCancelled) {
            LocalObjectPoll::Ready(object) => object,
            LocalObjectPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
            LocalObjectPoll::Failed(error) => panic!("local object failed: {error}"),
        };
        evidence.push(EffectiveObjectOffset::from_locally_framed(&object).unwrap());
    }
    (plan, evidence)
}

#[test]
fn retained_plan_and_second_sort_have_exact_aggregate_boundaries() {
    let fixture = fixture();
    let store = fixture.store();
    let (baseline_plan, baseline_evidence) = plan_and_evidence(&fixture, &store);
    let index_bytes = baseline_plan.index_stats().logical_index_bytes();
    let initial_sort_steps = baseline_plan.index_stats().sort_steps();
    let baseline_rebuilt = baseline_plan
        .rebuild(baseline_evidence, &NeverCancelled)
        .unwrap();
    let plan_bytes = baseline_rebuilt.geometry_stats().plan_bytes();
    let total_sort_steps = baseline_rebuilt.index_stats().sort_steps();
    assert!(total_sort_steps > initial_sort_steps);

    let exact_limits = DocumentLimits::validate(DocumentLimitConfig {
        max_logical_index_bytes: index_bytes + plan_bytes,
        max_sort_steps: total_sort_steps,
        ..DocumentLimitConfig::default()
    })
    .unwrap();
    let (plan, evidence) = plan_and_evidence_with_limits(&fixture, &store, exact_limits);
    plan.rebuild(evidence, &NeverCancelled).unwrap();

    let one_less_plan = DocumentLimits::validate(DocumentLimitConfig {
        max_logical_index_bytes: index_bytes + plan_bytes - 1,
        max_sort_steps: total_sort_steps,
        ..DocumentLimitConfig::default()
    })
    .unwrap();
    let (plan, evidence) = plan_and_evidence_with_limits(&fixture, &store, one_less_plan);
    let error = plan.rebuild(evidence, &NeverCancelled).unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        DocumentLimitKind::LogicalIndexBytes
    );

    let one_less_sort = DocumentLimits::validate(DocumentLimitConfig {
        max_logical_index_bytes: index_bytes + plan_bytes,
        max_sort_steps: total_sort_steps - 1,
        ..DocumentLimitConfig::default()
    })
    .unwrap();
    let (plan, evidence) = plan_and_evidence_with_limits(&fixture, &store, one_less_sort);
    let error = plan.rebuild(evidence, &NeverCancelled).unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(error.limit().unwrap().kind(), DocumentLimitKind::SortSteps);
}

#[test]
fn complete_proof_plan_rebuilds_all_effective_intervals_before_attestation() {
    let fixture = fixture();
    let store = fixture.store();
    let (plan, evidence) = plan_and_evidence(&fixture, &store);
    assert_eq!(evidence.len(), 3);
    assert!(!evidence[0].is_offset_repaired());
    assert!(evidence[1].is_offset_repaired());
    assert_eq!(evidence[1].declared_offset(), fixture.declared_offsets[1]);
    assert_eq!(evidence[1].effective_offset(), fixture.actual_offsets[1]);
    assert_eq!(
        evidence[1].offset_diagnostic().unwrap().diagnostic_id(),
        "RPE-OBJECT-REPAIR-0001"
    );
    assert!(!evidence[2].is_offset_repaired());
    assert_eq!(evidence[2].diagnostics().len(), 1);
    assert_eq!(
        evidence[2].diagnostics()[0].kind(),
        ObjectRepairKind::DirectStreamLength
    );

    let original_sort_steps = plan.index_stats().sort_steps();
    let rebuilt = plan.rebuild(evidence, &NeverCancelled).unwrap();
    assert_eq!(
        rebuilt
            .physical_intervals()
            .iter()
            .map(|interval| interval.xref_offset())
            .collect::<Vec<_>>(),
        fixture.actual_offsets
    );
    assert_eq!(
        rebuilt.physical_intervals()[0].object_upper_bound(),
        fixture.actual_offsets[1]
    );
    assert_eq!(
        rebuilt.physical_intervals()[1].object_upper_bound(),
        fixture.actual_offsets[2]
    );
    assert_eq!(
        rebuilt
            .interval(ObjectRef::new(2, 0).unwrap())
            .unwrap()
            .xref_offset(),
        fixture.actual_offsets[1]
    );
    assert_eq!(
        rebuilt.physical_intervals()[2].object_upper_bound(),
        fixture.startxref
    );
    assert_eq!(rebuilt.geometry_stats().repaired_offsets(), 1);
    assert_eq!(rebuilt.geometry_stats().object_repairs(), 2);
    assert_eq!(
        rebuilt.object_offset_evidence()[2].diagnostics()[0].kind(),
        ObjectRepairKind::DirectStreamLength
    );
    assert_eq!(
        rebuilt.geometry_stats().plan_bytes(),
        u64::try_from(3 * mem::size_of::<EffectiveObjectOffset>()).unwrap()
    );
    assert_eq!(
        rebuilt.index_stats().sort_steps(),
        original_sort_steps + rebuilt.geometry_stats().additional_sort_steps()
    );
    assert!(rebuilt.xref_diagnostics().is_empty());
    assert!(format!("{rebuilt:?}").contains("[UNATTESTED]"));
}

#[test]
fn incomplete_reordered_foreign_bound_and_cancelled_plans_never_publish_geometry() {
    let fixture = fixture();
    let store = fixture.store();
    let (plan, mut evidence) = plan_and_evidence(&fixture, &store);
    evidence.pop();
    assert_eq!(
        plan.rebuild(evidence, &NeverCancelled).unwrap_err().code(),
        DocumentErrorCode::InternalState
    );

    let (plan, mut evidence) = plan_and_evidence(&fixture, &store);
    evidence.swap(0, 1);
    assert_eq!(
        plan.rebuild(evidence, &NeverCancelled).unwrap_err().code(),
        DocumentErrorCode::InternalState
    );

    let (plan, evidence) = plan_and_evidence(&fixture, &store);
    let cancelled = AtomicBool::new(true);
    assert_eq!(
        plan.rebuild(evidence, &cancelled).unwrap_err().code(),
        DocumentErrorCode::Cancelled
    );

    let xref = open_xref(&fixture, &store);
    let plan = LocalRepairPlanningRevision::new(
        xref,
        RevisionId::new(1),
        DocumentLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    let reference = ObjectRef::new(2, 0).unwrap();
    let interval = *plan
        .physical_intervals()
        .iter()
        .find(|interval| interval.reference() == reference)
        .unwrap();
    let widened = pdf_rs_object::IndirectObjectTarget::new(
        fixture.snapshot,
        reference,
        interval.xref_offset(),
        fixture.startxref,
        fixture.startxref,
    )
    .unwrap();
    let mut object_job = OpenLocalObjectJob::new(
        widened,
        object_context(),
        ObjectLimits::default(),
        ObjectRepairLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let widened_evidence = match object_job.poll(&store, &NeverObjectCancelled) {
        LocalObjectPoll::Ready(object) => {
            EffectiveObjectOffset::from_locally_framed(&object).unwrap()
        }
        outcome => panic!("widened target did not complete: {outcome:?}"),
    };
    let (_, mut evidence) = plan_and_evidence(&fixture, &store);
    evidence[1] = widened_evidence;
    assert_eq!(
        plan.rebuild(evidence, &NeverCancelled).unwrap_err().code(),
        DocumentErrorCode::InternalState
    );
}

#[test]
fn top_level_attestation_publishes_only_the_repaired_typestate_and_complete_ledger() {
    let fixture = fixture();
    let store = fixture.store();
    let mut job = attestation_job(&fixture, &store, RevisionAttestationLimits::default());
    assert_eq!(job.phase(), RevisionAttestationPhase::Prefix);
    let repaired = match job.poll(&store, &NeverCancelled) {
        LocalRevisionAttestationPoll::Ready(repaired) => repaired,
        outcome => panic!("complete repaired fixture did not attest: {outcome:?}"),
    };
    assert_eq!(job.phase(), RevisionAttestationPhase::Complete);
    assert_eq!(repaired.snapshot(), fixture.snapshot);
    assert_eq!(repaired.startxref(), fixture.startxref);
    assert_eq!(repaired.root(), ObjectRef::new(1, 0).unwrap());
    assert_eq!(repaired.object_attestations().len(), 3);
    assert_eq!(repaired.object_repair_evidence().len(), 3);
    assert!(repaired.xref_diagnostics().is_empty());
    assert!(repaired.object_repair_evidence()[1].is_offset_repaired());
    assert_eq!(
        repaired.object_repair_evidence()[2].diagnostics()[0].kind(),
        ObjectRepairKind::DirectStreamLength
    );
    assert!(matches!(
        repaired
            .attestation(ObjectRef::new(3, 0).unwrap())
            .unwrap()
            .kind(),
        ObjectAttestationKind::Stream { data_span, .. } if data_span.len() == 4
    ));
    assert_eq!(repaired.geometry_stats().repaired_offsets(), 1);
    assert_eq!(repaired.geometry_stats().object_repairs(), 2);
    assert_eq!(repaired.repair_limits(), ObjectRepairLimits::default());
    assert_eq!(repaired.attestation_stats().objects_attested(), 3);
    let debug = format!("{repaired:?}");
    assert!(debug.contains("[LOCALLY_REPAIRED]"));
    assert!(!debug.contains("DATA"));
    assert!(matches!(
        job.poll(&store, &NeverCancelled),
        LocalRevisionAttestationPoll::Failed(error)
            if error.code() == DocumentErrorCode::JobAlreadyComplete
    ));
}

#[test]
fn repaired_attestation_enforces_context_cancellation_and_semantic_replay() {
    let fixture = fixture();
    let store = fixture.store();
    let duplicate =
        LocalRevisionAttestationJobContext::new(ResumeCheckpoint::new(911), object_context());
    assert_eq!(
        AttestLocalRepairRevisionJob::new(
            rebuilt(&fixture, &store),
            duplicate,
            RevisionAttestationLimits::default(),
            ObjectLimits::default(),
            ObjectRepairLimits::default(),
            SyntaxLimits::default(),
        )
        .unwrap_err()
        .code(),
        DocumentErrorCode::InvalidAttestationJobContext
    );

    let cancelled = AtomicBool::new(true);
    let mut job = attestation_job(&fixture, &store, RevisionAttestationLimits::default());
    assert!(matches!(
        job.poll(&store, &cancelled),
        LocalRevisionAttestationPoll::Failed(error)
            if error.code() == DocumentErrorCode::Cancelled
    ));
    assert_eq!(job.phase(), RevisionAttestationPhase::Failed);
    assert!(matches!(
        job.poll(&store, &NeverCancelled),
        LocalRevisionAttestationPoll::Failed(error)
            if error.code() == DocumentErrorCode::Cancelled
    ));

    let mut changed = fixture.bytes.clone();
    let marker = changed
        .windows(b"/Length 3".len())
        .position(|window| window == b"/Length 3")
        .unwrap();
    changed[marker + b"/Length ".len()] = b'4';
    let changed_store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(changed.len()).unwrap()).unwrap();
    changed_store
        .supply(RangeResponse::new(fixture.snapshot, range, changed).unwrap())
        .unwrap();
    let mut job = attestation_job(&fixture, &store, RevisionAttestationLimits::default());
    assert!(matches!(
        job.poll(&changed_store, &NeverCancelled),
        LocalRevisionAttestationPoll::Failed(error)
            if error.code() == DocumentErrorCode::ObjectAttestationFailure
    ));
}

#[test]
fn repaired_attestation_aggregate_object_work_accepts_exact_and_rejects_one_less() {
    let fixture = fixture();
    let store = fixture.store();
    let mut baseline = attestation_job(&fixture, &store, RevisionAttestationLimits::default());
    let repaired = match baseline.poll(&store, &NeverCancelled) {
        LocalRevisionAttestationPoll::Ready(repaired) => repaired,
        outcome => panic!("baseline repaired attestation failed: {outcome:?}"),
    };
    let stats = repaired.attestation_stats();
    let exact = RevisionAttestationLimits::validate(RevisionAttestationLimitConfig {
        max_total_object_read_bytes: stats.object_read_bytes(),
        max_total_object_parse_bytes: stats.object_parse_bytes(),
        ..RevisionAttestationLimitConfig::default()
    })
    .unwrap();
    let mut exact_job = attestation_job(&fixture, &store, exact);
    assert!(matches!(
        exact_job.poll(&store, &NeverCancelled),
        LocalRevisionAttestationPoll::Ready(_)
    ));

    let one_less = RevisionAttestationLimits::validate(RevisionAttestationLimitConfig {
        max_total_object_read_bytes: stats.object_read_bytes() - 1,
        max_total_object_parse_bytes: stats.object_parse_bytes(),
        ..RevisionAttestationLimitConfig::default()
    })
    .unwrap();
    let mut one_less_job = attestation_job(&fixture, &store, one_less);
    let error = match one_less_job.poll(&store, &NeverCancelled) {
        LocalRevisionAttestationPoll::Failed(error) => error,
        outcome => panic!("one-less aggregate read unexpectedly completed: {outcome:?}"),
    };
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        DocumentLimitKind::AttestationObjectReadBytes
    );
}

#[test]
fn effective_physical_reordering_keeps_each_repair_proof_bound_to_its_object() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let object_two_offset = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"2 0 obj\n0\nendobj\n");
    let object_one_offset = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    while bytes.len() < 80 {
        bytes.push(b' ');
    }
    bytes.extend_from_slice(b"% Xpadding\n");
    while bytes.len() < 100 {
        bytes.push(b' ');
    }
    let startxref = u64::try_from(bytes.len()).unwrap();
    let object_one_declared = object_one_offset - 3;
    let object_two_declared = 82;
    assert!(object_one_declared < object_two_declared);
    assert!(object_two_offset < object_one_offset);
    bytes.extend_from_slice(
        format!(
            "xref\n0 3\n0000000000 65535 f \n{object_one_declared:010} 00000 n \n{object_two_declared:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let snapshot = SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0xc1; 32]), SourceRevision::new(8)),
        Some(u64::try_from(bytes.len()).unwrap()),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xc2; 32]),
    );
    let store = RangeStore::new(snapshot, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(snapshot, range, bytes).unwrap())
        .unwrap();

    let xref = open_xref_for_snapshot(snapshot, &store);
    let exact_two = pdf_rs_object::IndirectObjectTarget::new(
        snapshot,
        ObjectRef::new(2, 0).unwrap(),
        object_two_offset,
        startxref,
        startxref,
    )
    .unwrap();
    let mut exact_two_job = OpenObjectJob::new(
        exact_two,
        object_context().strict(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        exact_two_job.poll(&store, &NeverObjectCancelled),
        pdf_rs_object::ObjectPoll::Ready(_)
    ));
    let plan = LocalRepairPlanningRevision::new(
        xref,
        RevisionId::new(2),
        DocumentLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    assert_eq!(
        plan.physical_intervals()
            .iter()
            .map(|interval| interval.reference())
            .collect::<Vec<_>>(),
        [ObjectRef::new(1, 0).unwrap(), ObjectRef::new(2, 0).unwrap()]
    );
    let repair_limits = ObjectRepairLimits::validate(pdf_rs_object::ObjectRepairLimitConfig {
        max_object_offset_delta: 96,
        ..pdf_rs_object::ObjectRepairLimitConfig::default()
    })
    .unwrap();
    let references = plan
        .physical_intervals()
        .iter()
        .map(|interval| interval.reference())
        .collect::<Vec<_>>();
    let mut evidence = Vec::with_capacity(references.len());
    for reference in references {
        let mut job = OpenLocalObjectJob::new(
            plan.unattested_target(reference).unwrap(),
            object_context(),
            ObjectLimits::default(),
            repair_limits,
            SyntaxLimits::default(),
        )
        .unwrap();
        let object = match job.poll(&store, &NeverObjectCancelled) {
            LocalObjectPoll::Ready(object) => object,
            LocalObjectPoll::Failed(error) => {
                panic!(
                    "reordering repair probe failed: {error:?} stats={:?}",
                    job.stats()
                )
            }
            outcome => panic!("reordering repair probe failed: {outcome:?}"),
        };
        evidence.push(EffectiveObjectOffset::from_locally_framed(&object).unwrap());
    }
    let rebuilt = plan.rebuild(evidence, &NeverCancelled).unwrap();
    let expected_order = [ObjectRef::new(2, 0).unwrap(), ObjectRef::new(1, 0).unwrap()];
    assert_eq!(
        rebuilt
            .physical_intervals()
            .iter()
            .map(|interval| interval.reference())
            .collect::<Vec<_>>(),
        expected_order
    );
    assert_eq!(
        rebuilt
            .object_offset_evidence()
            .iter()
            .map(|proof| proof.reference())
            .collect::<Vec<_>>(),
        expected_order
    );
    let mut job = AttestLocalRepairRevisionJob::new(
        rebuilt,
        attestation_context(),
        RevisionAttestationLimits::default(),
        ObjectLimits::default(),
        repair_limits,
        SyntaxLimits::default(),
    )
    .unwrap();
    let repaired = match job.poll(&store, &NeverCancelled) {
        LocalRevisionAttestationPoll::Ready(repaired) => repaired,
        outcome => panic!("reordered effective geometry did not attest: {outcome:?}"),
    };
    assert_eq!(
        repaired
            .object_repair_evidence()
            .iter()
            .map(|proof| proof.reference())
            .collect::<Vec<_>>(),
        expected_order
    );
    assert_eq!(repaired.object_attestations().len(), 2);
}
