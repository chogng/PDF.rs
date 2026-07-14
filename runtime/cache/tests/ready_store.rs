use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_cache::{
    ReadyAdmission, ReadyLookup, ReadyMissReason, ReadyRejectReason, ReadyStore, ReadyStoreBinding,
    ReadyStoreEpoch, ReadyStoreErrorCategory, ReadyStoreErrorCode, ReadyStoreKey,
    ReadyStoreLimitConfig, ReadyStoreLimitKind, ReadyStoreLimits, ReadyStoreRecoverability,
    ReadyStoreScope, ReadyStoreSessionId,
};
use pdf_rs_document::{
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex, DocumentCancellation,
    NeverCancelled as DocumentNeverCancelled, ReferenceChainJobContext, ReferenceChainLimitConfig,
    ReferenceChainLimits, ReferenceChainPoll, ResolveReferenceChainJob,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::{ObjectLimitConfig, ObjectLimits};
use pdf_rs_syntax::{ObjectRef, SyntaxLimitConfig, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
};

const REVISION_ID: RevisionId = RevisionId::new(41);
const SESSION_ID: ReadyStoreSessionId = ReadyStoreSessionId::new(0x05e5_510a);
const EPOCH: ReadyStoreEpoch = ReadyStoreEpoch::new(7);

struct CancelOnProbe {
    probes: AtomicU64,
    cancel_at: u64,
}

impl DocumentCancellation for CancelOnProbe {
    fn is_cancelled(&self) -> bool {
        self.probes.fetch_add(1, Ordering::Relaxed) + 1 >= self.cancel_at
    }
}

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

fn snapshot(len: u64, seed: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([seed; 32]),
            SourceRevision::new(u64::from(seed)),
        ),
        Some(len),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [seed.wrapping_add(1); 32],
        ),
    )
}

fn fixture(seed: u8) -> Fixture {
    let bodies: &[(u32, &[u8])] = &[
        (1, b"1 0 obj\n<< /Name (CACHE_SECRET_VALUE) >>\nendobj\n"),
        (2, b"2 0 obj\n[(two) (two-two)]\nendobj\n"),
        (3, b"3 0 obj\n42\nendobj\n"),
        (4, b"4 0 obj\n1 0 R\nendobj\n"),
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for &(number, body) in bodies {
        offsets.push((
            number,
            u64::try_from(bytes.len()).expect("fixture offset fits u64"),
        ));
        bytes.extend_from_slice(body);
    }
    let startxref = u64::try_from(bytes.len()).expect("fixture length fits u64");
    let size = 5_u32;
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for number in 0..size {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else {
            let offset = offsets
                .iter()
                .find(|(candidate, _)| *candidate == number)
                .map(|(_, offset)| *offset)
                .expect("every nonzero fixture object is in use");
            format!("{offset:010} 00000 n \n")
        };
        assert_eq!(row.len(), 20);
        bytes.extend_from_slice(row.as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
            .as_bytes(),
    );
    let source = snapshot(
        u64::try_from(bytes.len()).expect("fixture length fits u64"),
        seed,
    );
    Fixture {
        bytes,
        snapshot: source,
    }
}

fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("test references are nonzero")
}

fn store_for(fixture: &Fixture) -> RangeStore {
    let store =
        RangeStore::new(fixture.snapshot, Default::default()).expect("store limits validate");
    let range = ByteRange::new(
        0,
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
    )
    .expect("fixture range is nonempty");
    store
        .supply(
            RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone())
                .expect("fixture response matches its range"),
        )
        .expect("fixture fits byte-store limits");
    store
}

fn ready_index_with_profiles(
    fixture: &Fixture,
    object_limits: ObjectLimits,
    syntax_limits: SyntaxLimits,
) -> AttestedRevisionIndex {
    let store = store_for(fixture);
    let mut xref = OpenXrefJob::new(
        fixture.snapshot,
        XrefJobContext::new(
            JobId::new(101),
            ResumeCheckpoint::new(102),
            ResumeCheckpoint::new(103),
        ),
        XrefLimits::default(),
        syntax_limits,
    )
    .expect("xref job validates");
    let section = match xref.poll(&store, &XrefNeverCancelled) {
        XrefPoll::Ready(section) => section,
        XrefPoll::Pending { .. } => panic!("complete xref input must not suspend"),
        XrefPoll::Failed(error) => panic!("self-authored xref must parse: {error}"),
    };
    let candidate = CandidateRevisionIndex::from_xref(
        &section,
        REVISION_ID,
        Default::default(),
        &DocumentNeverCancelled,
    )
    .expect("self-authored xref yields a candidate");
    let mut attest = AttestRevisionJob::new(
        candidate,
        RevisionAttestationJobContext::new(
            JobId::new(201),
            ResumeCheckpoint::new(202),
            ResumeCheckpoint::new(203),
            ResumeCheckpoint::new(204),
            RequestPriority::Metadata,
        ),
        RevisionAttestationLimits::default(),
        object_limits,
        syntax_limits,
    )
    .expect("attestation job validates");
    match attest.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Ready(index) => index,
        RevisionAttestationPoll::Pending { .. } => {
            panic!("complete attestation input must not suspend")
        }
        RevisionAttestationPoll::Failed(error) => {
            panic!("self-authored revision must attest: {error}")
        }
    }
}

fn ready_index(fixture: &Fixture) -> AttestedRevisionIndex {
    ready_index_with_profiles(fixture, ObjectLimits::default(), SyntaxLimits::default())
}

fn resolve<'index>(
    index: &'index AttestedRevisionIndex,
    store: &RangeStore,
    root: ObjectRef,
    limits: ReferenceChainLimits,
) -> pdf_rs_document::ResolvedReference {
    let mut job: ResolveReferenceChainJob<'index> = index
        .resolve_reference_chain(
            root,
            ReferenceChainJobContext::new(
                JobId::new(301),
                ResumeCheckpoint::new(302),
                ResumeCheckpoint::new(303),
                RequestPriority::VisiblePage,
            ),
            limits,
        )
        .expect("resolution job validates");
    match job.poll(store, &DocumentNeverCancelled) {
        ReferenceChainPoll::Ready(value) => value,
        ReferenceChainPoll::Pending { .. } => panic!("complete resolution input must not suspend"),
        ReferenceChainPoll::Failed(error) => panic!("self-authored chain must resolve: {error}"),
    }
}

fn alternate_resolution_limits() -> ReferenceChainLimits {
    let mut config = ReferenceChainLimitConfig::default();
    config.max_objects -= 1;
    ReferenceChainLimits::validate(config).expect("alternate limits remain valid")
}

fn resolution_limits_with_read_delta(delta: u64) -> ReferenceChainLimits {
    let mut config = ReferenceChainLimitConfig::default();
    config.max_total_object_read_bytes = config
        .max_total_object_read_bytes
        .checked_sub(delta)
        .expect("test delta stays beneath the default read budget");
    ReferenceChainLimits::validate(config).expect("varied read budget remains valid")
}

fn retained_heap_bytes(value: &pdf_rs_document::ResolvedReference) -> u64 {
    let footprint = value.try_resident_footprint().unwrap();
    footprint
        .syntax_heap_bytes()
        .checked_add(footprint.chain_capacity_bytes())
        .unwrap()
}

fn cache_limits(
    max_entries: u64,
    max_value_bytes: u64,
    max_resident_bytes: u64,
) -> ReadyStoreLimits {
    ReadyStoreLimits::validate(ReadyStoreLimitConfig {
        max_entries,
        max_value_bytes,
        max_resident_bytes,
    })
    .expect("test cache limits validate")
}

fn key(binding: ReadyStoreBinding, root: ObjectRef, limits: ReferenceChainLimits) -> ReadyStoreKey {
    ReadyStoreKey::new(binding, root, limits)
}

fn admit(
    cache: &mut ReadyStore,
    key: ReadyStoreKey,
    value: pdf_rs_document::ResolvedReference,
) -> pdf_rs_cache::ReadyAdmitted {
    match cache
        .try_admit(key, value, &DocumentNeverCancelled)
        .expect("admission has no internal failure")
    {
        ReadyAdmission::Admitted(admitted) => admitted,
        ReadyAdmission::Rejected(rejected) => {
            panic!("value should be admitted: {:?}", rejected.reason())
        }
    }
}

#[test]
fn complete_binding_and_key_round_trip_every_cache_dimension() {
    let fixture = fixture(0x51);
    let index = ready_index(&fixture);
    let binding = ReadyStoreBinding::for_index(&index, SESSION_ID, EPOCH);
    let limits = ReferenceChainLimits::default();
    let key = key(binding, object_ref(4), limits);

    assert_eq!(binding.snapshot(), index.snapshot());
    assert_eq!(binding.session_id(), SESSION_ID);
    assert_eq!(binding.session_id().value(), 0x05e5_510a);
    assert_eq!(binding.revision_id(), index.revision_id());
    assert_eq!(binding.revision_startxref(), index.startxref());
    assert_eq!(binding.object_limits(), index.object_limits());
    assert_eq!(binding.syntax_limits(), index.syntax_limits());
    assert_eq!(binding.epoch(), EPOCH);
    assert_eq!(binding.epoch().value(), 7);
    assert_eq!(key.binding(), binding);
    assert_eq!(key.root(), object_ref(4));
    assert_eq!(key.resolution_limits(), limits);
}

#[test]
fn borrowed_hit_requires_complete_key_and_checks_cancellation_first() {
    let fixture = fixture(0x52);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = ReadyStoreBinding::for_index(&index, SESSION_ID, EPOCH);
    let limits = ReferenceChainLimits::default();
    let exact = key(binding, object_ref(1), limits);
    let mut cache = ReadyStore::new(binding, ReadyStoreLimits::default()).unwrap();
    admit(
        &mut cache,
        exact,
        resolve(&index, &source, object_ref(1), limits),
    );

    match cache.lookup(exact, &DocumentNeverCancelled).unwrap() {
        ReadyLookup::Hit(value) => {
            assert_eq!(value.root(), object_ref(1));
            assert_eq!(value.limits(), limits);
        }
        ReadyLookup::Miss(reason) => panic!("exact key must hit, got {reason:?}"),
    }
    assert!(matches!(
        cache
            .lookup(
                key(binding, object_ref(1), alternate_resolution_limits()),
                &DocumentNeverCancelled,
            )
            .unwrap(),
        ReadyLookup::Miss(ReadyMissReason::NotFound)
    ));
    let other_epoch = ReadyStoreBinding::for_index(&index, SESSION_ID, ReadyStoreEpoch::new(8));
    assert!(matches!(
        cache
            .lookup(
                key(other_epoch, object_ref(1), limits),
                &DocumentNeverCancelled,
            )
            .unwrap(),
        ReadyLookup::Miss(ReadyMissReason::BindingMismatch)
    ));

    let cancelled = AtomicBool::new(true);
    let error = cache.lookup(exact, &cancelled).unwrap_err();
    assert_eq!(error.code(), ReadyStoreErrorCode::Cancelled);
    assert!(cancelled.load(Ordering::Relaxed));
    let stats = cache.stats();
    assert_eq!((stats.hits(), stats.misses()), (1, 2));
}

#[test]
fn opaque_session_identity_prevents_cross_session_keys_from_hitting_or_entering() {
    let fixture = fixture(0x5a);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let limits = ReferenceChainLimits::default();
    let binding = ReadyStoreBinding::for_index(&index, SESSION_ID, EPOCH);
    let other_session = ReadyStoreSessionId::new(SESSION_ID.value() + 1);
    let other_binding = ReadyStoreBinding::for_index(&index, other_session, EPOCH);
    let exact = key(binding, object_ref(1), limits);
    let foreign = key(other_binding, object_ref(1), limits);
    let mut cache = ReadyStore::new(binding, ReadyStoreLimits::default()).unwrap();
    admit(
        &mut cache,
        exact,
        resolve(&index, &source, object_ref(1), limits),
    );

    assert!(matches!(
        cache.lookup(foreign, &DocumentNeverCancelled).unwrap(),
        ReadyLookup::Miss(ReadyMissReason::BindingMismatch)
    ));
    let rejected = match cache
        .try_admit(
            foreign,
            resolve(&index, &source, object_ref(1), limits),
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        ReadyAdmission::Rejected(rejected) => rejected,
        ReadyAdmission::Admitted(_) => panic!("a foreign session key must not enter the store"),
    };
    assert_eq!(rejected.reason(), ReadyRejectReason::BindingMismatch);
    assert_eq!(rejected.limit(), None);
    assert_eq!(rejected.into_value().root(), object_ref(1));
    assert_eq!(cache.stats().entries(), 1);
}

#[test]
fn admission_rejections_return_the_successful_move_only_value() {
    let fixture = fixture(0x53);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = ReadyStoreBinding::for_index(&index, SESSION_ID, EPOCH);
    let limits = ReferenceChainLimits::default();
    let mut cache = ReadyStore::new(binding, ReadyStoreLimits::default()).unwrap();

    let value = resolve(&index, &source, object_ref(1), limits);
    let other_epoch = ReadyStoreBinding::for_index(&index, SESSION_ID, ReadyStoreEpoch::new(9));
    let rejected = match cache
        .try_admit(
            key(other_epoch, object_ref(1), limits),
            value,
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        ReadyAdmission::Rejected(rejected) => rejected,
        ReadyAdmission::Admitted(_) => panic!("other epoch must not be admitted"),
    };
    assert_eq!(rejected.reason(), ReadyRejectReason::BindingMismatch);
    assert_eq!(rejected.into_value().root(), object_ref(1));

    let value = resolve(&index, &source, object_ref(1), limits);
    let rejected = match cache
        .try_admit(
            key(binding, object_ref(2), limits),
            value,
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        ReadyAdmission::Rejected(rejected) => rejected,
        ReadyAdmission::Admitted(_) => panic!("wrong root must not be admitted"),
    };
    assert_eq!(rejected.reason(), ReadyRejectReason::RootMismatch);
    assert_eq!(rejected.into_value().root(), object_ref(1));

    let value = resolve(&index, &source, object_ref(1), limits);
    let rejected = match cache
        .try_admit(
            key(binding, object_ref(1), alternate_resolution_limits()),
            value,
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        ReadyAdmission::Rejected(rejected) => rejected,
        ReadyAdmission::Admitted(_) => panic!("wrong limits must not be admitted"),
    };
    assert_eq!(
        rejected.reason(),
        ReadyRejectReason::ResolutionProfileMismatch
    );
    assert_eq!(rejected.into_value().root(), object_ref(1));

    let value = resolve(&index, &source, object_ref(1), limits);
    let cancelled = AtomicBool::new(true);
    let failure = cache
        .try_admit(key(binding, object_ref(1), limits), value, &cancelled)
        .unwrap_err();
    assert_eq!(failure.error().code(), ReadyStoreErrorCode::Cancelled);
    assert_eq!(failure.into_value().root(), object_ref(1));
    assert_eq!(cache.stats().entries(), 0);
}

#[test]
fn exact_and_one_less_value_metadata_and_resident_limits_are_distinct() {
    let fixture = fixture(0x54);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = ReadyStoreBinding::for_index(&index, SESSION_ID, EPOCH);
    let resolution_limits = ReferenceChainLimits::default();

    let measured = resolve(&index, &source, object_ref(4), resolution_limits);
    let footprint = measured.try_resident_footprint().unwrap();
    let heap_bytes = footprint
        .syntax_heap_bytes()
        .checked_add(footprint.chain_capacity_bytes())
        .unwrap();
    drop(measured);

    let probe = ReadyStore::new(
        binding,
        cache_limits(1, footprint.total_bytes(), 64 * 1024 * 1024),
    )
    .unwrap();
    let metadata_bytes = probe.stats().metadata_bytes();
    assert!(metadata_bytes > 0);
    drop(probe);

    let metadata_error = ReadyStore::new(
        binding,
        cache_limits(1, 1, metadata_bytes.checked_sub(1).unwrap()),
    )
    .unwrap_err();
    assert_eq!(metadata_error.code(), ReadyStoreErrorCode::ResourceLimit);
    assert_eq!(metadata_error.category(), ReadyStoreErrorCategory::Resource);
    assert_eq!(
        metadata_error.recoverability(),
        ReadyStoreRecoverability::ReduceWorkload
    );
    assert_eq!(metadata_error.diagnostic_id(), "RPE-CACHE-0003");
    let metadata_limit = metadata_error.limit().unwrap();
    assert_eq!(metadata_limit.kind(), ReadyStoreLimitKind::ResidentBytes);
    assert_eq!(metadata_limit.limit(), metadata_bytes - 1);
    assert_eq!(metadata_limit.consumed(), 0);
    assert_eq!(metadata_limit.attempted(), metadata_bytes);
    assert_eq!(metadata_limit.scope(), ReadyStoreScope::Session(SESSION_ID));
    assert_eq!(metadata_limit.reference(), None);

    let mut exact = ReadyStore::new(
        binding,
        cache_limits(
            1,
            footprint.total_bytes(),
            metadata_bytes.checked_add(heap_bytes).unwrap(),
        ),
    )
    .unwrap();
    admit(
        &mut exact,
        key(binding, object_ref(4), resolution_limits),
        resolve(&index, &source, object_ref(4), resolution_limits),
    );
    assert_eq!(
        exact.stats().resident_bytes(),
        metadata_bytes.checked_add(heap_bytes).unwrap()
    );
    assert_eq!(
        exact.stats().resident_bytes(),
        exact
            .stats()
            .metadata_bytes()
            .checked_add(
                footprint
                    .total_bytes()
                    .checked_sub(footprint.inline_bytes())
                    .unwrap()
            )
            .unwrap()
    );

    let mut value_one_less = ReadyStore::new(
        binding,
        cache_limits(
            1,
            footprint.total_bytes().checked_sub(1).unwrap(),
            64 * 1024 * 1024,
        ),
    )
    .unwrap();
    let rejected = match value_one_less
        .try_admit(
            key(binding, object_ref(4), resolution_limits),
            resolve(&index, &source, object_ref(4), resolution_limits),
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        ReadyAdmission::Rejected(rejected) => rejected,
        ReadyAdmission::Admitted(_) => panic!("one-less value limit must reject"),
    };
    assert_eq!(rejected.reason(), ReadyRejectReason::ValueTooLarge);
    let limit = rejected.limit().expect("byte rejection carries context");
    assert_eq!(limit.kind(), ReadyStoreLimitKind::ValueBytes);
    assert_eq!(limit.limit(), footprint.total_bytes() - 1);
    assert_eq!(limit.consumed(), 0);
    assert_eq!(limit.attempted(), footprint.total_bytes());
    assert_eq!(limit.scope(), ReadyStoreScope::Session(SESSION_ID));
    assert_eq!(limit.reference(), Some(object_ref(4)));
    assert_eq!(rejected.into_value().root(), object_ref(4));

    let mut resident_one_less = ReadyStore::new(
        binding,
        cache_limits(
            1,
            footprint.total_bytes(),
            metadata_bytes
                .checked_add(heap_bytes)
                .and_then(|bytes| bytes.checked_sub(1))
                .unwrap(),
        ),
    )
    .unwrap();
    let small_value = resolve(&index, &source, object_ref(3), resolution_limits);
    let small_heap = retained_heap_bytes(&small_value);
    assert!(small_heap < heap_bytes);
    admit(
        &mut resident_one_less,
        key(binding, object_ref(3), resolution_limits),
        small_value,
    );
    let rejected = match resident_one_less
        .try_admit(
            key(binding, object_ref(4), resolution_limits),
            resolve(&index, &source, object_ref(4), resolution_limits),
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        ReadyAdmission::Rejected(rejected) => rejected,
        ReadyAdmission::Admitted(_) => panic!("one-less resident limit must reject"),
    };
    assert_eq!(rejected.reason(), ReadyRejectReason::ResidentLimit);
    let limit = rejected
        .limit()
        .expect("resident rejection carries context");
    assert_eq!(limit.kind(), ReadyStoreLimitKind::ResidentBytes);
    assert_eq!(
        limit.limit(),
        metadata_bytes.checked_add(heap_bytes).unwrap() - 1
    );
    assert_eq!(
        limit.consumed(),
        metadata_bytes.checked_add(small_heap).unwrap()
    );
    assert_eq!(limit.attempted(), heap_bytes);
    assert_eq!(limit.scope(), ReadyStoreScope::Session(SESSION_ID));
    assert_eq!(limit.reference(), Some(object_ref(4)));
    assert_eq!(rejected.into_value().root(), object_ref(4));
}

#[test]
fn deterministic_lru_replacement_and_clear_preserve_owner_accounting() {
    let fixture = fixture(0x55);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = ReadyStoreBinding::for_index(&index, SESSION_ID, EPOCH);
    let limits = ReferenceChainLimits::default();
    let mut cache =
        ReadyStore::new(binding, cache_limits(2, 8 * 1024 * 1024, 64 * 1024 * 1024)).unwrap();

    for root in [object_ref(1), object_ref(2)] {
        admit(
            &mut cache,
            key(binding, root, limits),
            resolve(&index, &source, root, limits),
        );
    }
    assert!(matches!(
        cache
            .lookup(key(binding, object_ref(1), limits), &DocumentNeverCancelled,)
            .unwrap(),
        ReadyLookup::Hit(_)
    ));
    let admitted = admit(
        &mut cache,
        key(binding, object_ref(3), limits),
        resolve(&index, &source, object_ref(3), limits),
    );
    assert!(!admitted.replaced());
    assert_eq!(admitted.evicted(), 1);
    assert!(matches!(
        cache
            .lookup(key(binding, object_ref(2), limits), &DocumentNeverCancelled,)
            .unwrap(),
        ReadyLookup::Miss(ReadyMissReason::NotFound)
    ));
    for root in [object_ref(1), object_ref(3)] {
        assert!(matches!(
            cache
                .lookup(key(binding, root, limits), &DocumentNeverCancelled)
                .unwrap(),
            ReadyLookup::Hit(_)
        ));
    }

    let replacement = admit(
        &mut cache,
        key(binding, object_ref(1), limits),
        resolve(&index, &source, object_ref(1), limits),
    );
    assert!(replacement.replaced());
    assert_eq!(replacement.evicted(), 0);
    let before_clear = cache.stats();
    assert_eq!(before_clear.entries(), 2);
    assert_eq!(before_clear.evictions(), 1);
    assert_eq!(before_clear.replacements(), 1);
    assert_eq!(cache.clear(), 2);
    let after_clear = cache.stats();
    assert_eq!(after_clear.entries(), 0);
    assert_eq!(after_clear.value_heap_bytes(), 0);
    assert_eq!(after_clear.resident_bytes(), after_clear.metadata_bytes());
    assert!(after_clear.peak_resident_bytes() >= after_clear.resident_bytes());
}

#[test]
fn byte_pressure_evicts_multiple_oldest_values_in_one_linear_plan() {
    let fixture = fixture(0x5b);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = ReadyStoreBinding::for_index(&index, SESSION_ID, EPOCH);

    let small_root = object_ref(3);
    let first_small_limits = resolution_limits_with_read_delta(1);
    let small_probe = resolve(&index, &source, small_root, first_small_limits);
    let small_heap = retained_heap_bytes(&small_probe);
    drop(small_probe);
    assert!(small_heap > 0);

    let (large_root, large_heap, large_total) = [object_ref(1), object_ref(2), object_ref(4)]
        .into_iter()
        .map(|root| {
            let value = resolve(&index, &source, root, ReferenceChainLimits::default());
            let footprint = value.try_resident_footprint().unwrap();
            let result = (root, retained_heap_bytes(&value), footprint.total_bytes());
            drop(value);
            result
        })
        .max_by_key(|(_, heap, _)| *heap)
        .expect("fixture has candidate large values");
    assert!(large_heap > small_heap);

    let victims = large_heap
        .checked_add(small_heap - 1)
        .unwrap()
        .checked_div(small_heap)
        .unwrap();
    assert!(victims >= 2);
    let old_count = victims.checked_add(1).unwrap();
    let max_entries = old_count.checked_add(1).unwrap();
    let max_heap = old_count.checked_mul(small_heap).unwrap();
    let probe = ReadyStore::new(
        binding,
        cache_limits(max_entries, large_total, 64 * 1024 * 1024),
    )
    .unwrap();
    let metadata_bytes = probe.stats().metadata_bytes();
    drop(probe);
    let resident_limit = metadata_bytes.checked_add(max_heap).unwrap();
    assert!(resident_limit >= large_total);
    let mut cache = ReadyStore::new(
        binding,
        cache_limits(max_entries, large_total, resident_limit),
    )
    .unwrap();

    for delta in 1..=old_count {
        let limits = resolution_limits_with_read_delta(delta);
        admit(
            &mut cache,
            key(binding, small_root, limits),
            resolve(&index, &source, small_root, limits),
        );
    }
    assert_eq!(cache.stats().value_heap_bytes(), max_heap);
    assert!(matches!(
        cache
            .lookup(
                key(binding, small_root, first_small_limits),
                &DocumentNeverCancelled,
            )
            .unwrap(),
        ReadyLookup::Hit(_)
    ));

    let admitted = admit(
        &mut cache,
        key(binding, large_root, ReferenceChainLimits::default()),
        resolve(&index, &source, large_root, ReferenceChainLimits::default()),
    );
    assert_eq!(admitted.evicted(), victims);
    assert!(!admitted.replaced());
    assert_eq!(cache.stats().entries(), 2);
    assert_eq!(
        cache.stats().value_heap_bytes(),
        small_heap.checked_add(large_heap).unwrap()
    );
    assert!(matches!(
        cache
            .lookup(
                key(binding, small_root, first_small_limits),
                &DocumentNeverCancelled,
            )
            .unwrap(),
        ReadyLookup::Hit(_)
    ));
    for delta in 2..=old_count {
        assert!(matches!(
            cache
                .lookup(
                    key(
                        binding,
                        small_root,
                        resolution_limits_with_read_delta(delta),
                    ),
                    &DocumentNeverCancelled,
                )
                .unwrap(),
            ReadyLookup::Miss(ReadyMissReason::NotFound)
        ));
    }
}

#[test]
fn cancellation_during_long_cache_scans_preserves_values_lru_and_accounting() {
    const RESIDENT_ENTRIES: u64 = 300;

    let fixture = fixture(0x5c);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = ReadyStoreBinding::for_index(&index, SESSION_ID, EPOCH);
    let root = object_ref(3);
    let mut cache = ReadyStore::new(
        binding,
        cache_limits(RESIDENT_ENTRIES, 8 * 1024 * 1024, 64 * 1024 * 1024),
    )
    .unwrap();
    for delta in 1..=RESIDENT_ENTRIES {
        let limits = resolution_limits_with_read_delta(delta);
        admit(
            &mut cache,
            key(binding, root, limits),
            resolve(&index, &source, root, limits),
        );
    }
    let before = cache.stats();
    let lookup_cancellation = CancelOnProbe {
        probes: AtomicU64::new(0),
        cancel_at: 3,
    };
    let lookup_error = cache
        .lookup(
            key(
                binding,
                root,
                resolution_limits_with_read_delta(RESIDENT_ENTRIES),
            ),
            &lookup_cancellation,
        )
        .unwrap_err();
    assert_eq!(lookup_error.code(), ReadyStoreErrorCode::Cancelled);
    assert_eq!(cache.stats(), before);
    assert!(lookup_cancellation.probes.load(Ordering::Relaxed) >= 3);

    let incoming_limits = resolution_limits_with_read_delta(RESIDENT_ENTRIES + 1);
    let incoming = resolve(&index, &source, root, incoming_limits);
    let cancellation = CancelOnProbe {
        probes: AtomicU64::new(0),
        cancel_at: 3,
    };
    let failure = cache
        .try_admit(key(binding, root, incoming_limits), incoming, &cancellation)
        .unwrap_err();
    assert_eq!(failure.error().code(), ReadyStoreErrorCode::Cancelled);
    assert_eq!(cache.stats(), before);
    assert!(cancellation.probes.load(Ordering::Relaxed) >= 3);

    let admitted = admit(
        &mut cache,
        key(binding, root, incoming_limits),
        failure.into_value(),
    );
    assert_eq!(admitted.evicted(), 1);
    assert!(matches!(
        cache
            .lookup(
                key(binding, root, resolution_limits_with_read_delta(1),),
                &DocumentNeverCancelled,
            )
            .unwrap(),
        ReadyLookup::Miss(ReadyMissReason::NotFound)
    ));
}

#[test]
fn source_and_parser_profile_mismatch_cannot_enter_an_existing_session_store() {
    let primary_fixture = fixture(0x56);
    let other_fixture = fixture(0x57);
    let source = store_for(&primary_fixture);
    let other_source = store_for(&other_fixture);
    let index = ready_index(&primary_fixture);
    let other_index = ready_index(&other_fixture);
    let binding = ReadyStoreBinding::for_index(&index, SESSION_ID, EPOCH);
    let limits = ReferenceChainLimits::default();
    let mut cache = ReadyStore::new(binding, ReadyStoreLimits::default()).unwrap();

    let other_value = resolve(&other_index, &other_source, object_ref(1), limits);
    let rejected = match cache
        .try_admit(
            key(binding, object_ref(1), limits),
            other_value,
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        ReadyAdmission::Rejected(rejected) => rejected,
        ReadyAdmission::Admitted(_) => panic!("other snapshot must not be admitted"),
    };
    assert_eq!(rejected.reason(), ReadyRejectReason::BindingMismatch);
    assert_eq!(rejected.into_value().root(), object_ref(1));

    let mut object_config = ObjectLimitConfig::default();
    object_config.max_total_read_bytes -= 1;
    let object_limits = ObjectLimits::validate(object_config).unwrap();
    let mut syntax_config = SyntaxLimitConfig::default();
    syntax_config.max_total_tokens -= 1;
    let syntax_limits = SyntaxLimits::validate(syntax_config).unwrap();
    let alternate_index = ready_index_with_profiles(&primary_fixture, object_limits, syntax_limits);
    let alternate_value = resolve(&alternate_index, &source, object_ref(1), limits);
    let rejected = match cache
        .try_admit(
            key(binding, object_ref(1), limits),
            alternate_value,
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        ReadyAdmission::Rejected(rejected) => rejected,
        ReadyAdmission::Admitted(_) => panic!("other parser profile must not be admitted"),
    };
    assert_eq!(rejected.reason(), ReadyRejectReason::BindingMismatch);
    assert_eq!(rejected.into_value().root(), object_ref(1));
    assert_eq!(cache.stats().entries(), 0);
}

#[test]
fn debug_surfaces_redact_cached_and_rejected_semantic_values() {
    let fixture = fixture(0x58);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = ReadyStoreBinding::for_index(&index, SESSION_ID, EPOCH);
    let limits = ReferenceChainLimits::default();
    let mut cache = ReadyStore::new(binding, ReadyStoreLimits::default()).unwrap();
    admit(
        &mut cache,
        key(binding, object_ref(1), limits),
        resolve(&index, &source, object_ref(1), limits),
    );
    let cache_debug = format!("{cache:?}");
    let lookup_debug = format!(
        "{:?}",
        cache
            .lookup(key(binding, object_ref(1), limits), &DocumentNeverCancelled,)
            .unwrap()
    );
    let oversized_value = resolve(&index, &source, object_ref(1), limits);
    let oversized_bytes = oversized_value
        .try_resident_footprint()
        .unwrap()
        .total_bytes();
    let mut rejecting = ReadyStore::new(
        binding,
        cache_limits(1, oversized_bytes - 1, 64 * 1024 * 1024),
    )
    .unwrap();
    let rejected = match rejecting
        .try_admit(
            key(binding, object_ref(1), limits),
            oversized_value,
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        ReadyAdmission::Rejected(rejected) => rejected,
        ReadyAdmission::Admitted(_) => panic!("one-less value ceiling must reject"),
    };
    let rejected_debug = format!("{rejected:?}");
    assert_eq!(rejected.into_value().root(), object_ref(1));

    let cancelled = AtomicBool::new(true);
    let failure = rejecting
        .try_admit(
            key(binding, object_ref(1), limits),
            resolve(&index, &source, object_ref(1), limits),
            &cancelled,
        )
        .unwrap_err();
    let failure_debug = format!("{failure:?}");
    assert_eq!(failure.into_value().root(), object_ref(1));

    for debug in [&cache_debug, &lookup_debug, &rejected_debug, &failure_debug] {
        assert!(!debug.contains("CACHE_SECRET_VALUE"));
        assert!(!debug.contains("IndirectObject"));
    }
}

#[test]
fn limit_validation_rejects_zero_inconsistent_and_unbounded_profiles() {
    let defaults = ReadyStoreLimitConfig::default();
    let validated = ReadyStoreLimits::validate(defaults).unwrap();
    assert_eq!(validated.max_entries(), defaults.max_entries);
    assert_eq!(validated.max_value_bytes(), defaults.max_value_bytes);
    assert_eq!(validated.max_resident_bytes(), defaults.max_resident_bytes);

    for config in [
        ReadyStoreLimitConfig {
            max_entries: 0,
            ..defaults
        },
        ReadyStoreLimitConfig {
            max_value_bytes: 0,
            ..defaults
        },
        ReadyStoreLimitConfig {
            max_resident_bytes: 0,
            ..defaults
        },
        ReadyStoreLimitConfig {
            max_value_bytes: defaults.max_resident_bytes + 1,
            ..defaults
        },
        ReadyStoreLimitConfig {
            max_entries: u64::MAX,
            ..defaults
        },
        ReadyStoreLimitConfig {
            max_value_bytes: u64::MAX,
            max_resident_bytes: u64::MAX,
            ..defaults
        },
    ] {
        assert_eq!(
            ReadyStoreLimits::validate(config).unwrap_err().code(),
            ReadyStoreErrorCode::InvalidLimits
        );
    }
}
