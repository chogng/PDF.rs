use std::mem;
use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    DocumentErrorCode, DocumentLimitConfig, DocumentLimitKind, DocumentLimits,
    EffectiveObjectOffset, LocalRepairPlanningRevision, NeverCancelled, RevisionId,
};
use pdf_rs_object::{
    LocalObjectJobContext, LocalObjectPoll, NeverCancelled as NeverObjectCancelled,
    ObjectJobContext, ObjectLimits, ObjectRepairKind, ObjectRepairLimits, OpenLocalObjectJob,
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
    let strict = XrefJobContext::new(
        JobId::new(900),
        ResumeCheckpoint::new(901),
        ResumeCheckpoint::new(902),
    );
    let mut job = OpenLocalXrefJob::new(
        fixture.snapshot,
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
