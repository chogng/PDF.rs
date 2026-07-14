mod support;

use std::sync::atomic::AtomicBool;

use pdf_rs_cache::{
    ReadyAdmission, ReadyLookup, ReadyStoreBinding, ReadyStoreEpoch, ReadyStoreErrorCode,
    ReadyStoreLimits, ReadyStoreSessionId,
};
use pdf_rs_document::{DocumentCancellation, NeverCancelled, ReferenceChainLimits};
use pdf_rs_session::{
    ReadySessionErrorCategory, ReadySessionErrorCode, ReadySessionOwner, ReadySessionPhase,
    ReadySessionRecoverability,
};

use support::{
    EPOCH, SESSION_ID, binding, fixture, key, object_ref, ready_index, resolve, store_for,
};

struct MustNotProbeCancellation;

impl DocumentCancellation for MustNotProbeCancellation {
    fn is_cancelled(&self) -> bool {
        panic!("a closed owner must reject before probing cancellation")
    }
}

#[test]
fn ready_owner_exposes_its_complete_binding_and_precharged_resources() {
    let fixture = fixture(0x61);
    let index = ready_index(&fixture);
    let binding = binding(&index);
    let owner = ReadySessionOwner::new(binding, ReadyStoreLimits::default())
        .expect("default Ready-store limits construct a session owner");

    assert_eq!(owner.session_id(), SESSION_ID);
    assert_eq!(owner.binding().unwrap(), binding);
    assert_eq!(owner.phase(), ReadySessionPhase::Ready);
    assert_eq!(owner.close_report(), None);

    let stats = owner.stats().unwrap();
    let resources = owner.resources();
    assert_eq!(resources.entries(), stats.entries());
    assert_eq!(resources.metadata_bytes(), stats.metadata_bytes());
    assert_eq!(resources.value_heap_bytes(), stats.value_heap_bytes());
    assert_eq!(resources.resident_bytes(), stats.resident_bytes());
    assert_eq!(resources.entries(), 0);
    assert!(resources.metadata_bytes() > 0);
    assert_eq!(resources.value_heap_bytes(), 0);
    assert_eq!(resources.resident_bytes(), resources.metadata_bytes());
}

#[test]
fn ready_owner_delegates_admission_and_returns_borrowed_exact_hits() {
    let fixture = fixture(0x62);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = binding(&index);
    let limits = ReferenceChainLimits::default();
    let exact = key(binding, object_ref(1), limits);
    let mut owner = ReadySessionOwner::new(binding, ReadyStoreLimits::default()).unwrap();

    let admitted = owner
        .try_admit(
            exact,
            resolve(&index, &source, object_ref(1), limits),
            &NeverCancelled,
        )
        .expect("admission has no internal failure");
    match admitted {
        ReadyAdmission::Admitted(admitted) => {
            assert!(!admitted.replaced());
            assert_eq!(admitted.evicted(), 0);
        }
        ReadyAdmission::Rejected(rejected) => {
            panic!("exact value should be admitted: {:?}", rejected.reason())
        }
    }

    match owner.lookup(exact, &NeverCancelled).unwrap() {
        ReadyLookup::Hit(value) => {
            assert_eq!(value.root(), object_ref(1));
            assert_eq!(value.limits(), limits);
        }
        ReadyLookup::Miss(reason) => panic!("exact resident key must hit: {reason:?}"),
    }
    let stats = owner.stats().unwrap();
    assert_eq!(stats.entries(), 1);
    assert_eq!(stats.admissions(), 1);
    assert_eq!(stats.hits(), 1);
    assert_eq!(owner.resources().entries(), 1);
    assert!(owner.resources().value_heap_bytes() > 0);
}

#[test]
fn nonempty_close_releases_owned_resources_and_repeats_the_same_report() {
    let fixture = fixture(0x63);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = binding(&index);
    let limits = ReferenceChainLimits::default();
    let mut owner = ReadySessionOwner::new(binding, ReadyStoreLimits::default()).unwrap();
    let admission = owner
        .try_admit(
            key(binding, object_ref(1), limits),
            resolve(&index, &source, object_ref(1), limits),
            &NeverCancelled,
        )
        .unwrap();
    assert!(matches!(admission, ReadyAdmission::Admitted(_)));
    let before = owner.stats().unwrap();
    assert_eq!(before.entries(), 1);
    assert!(before.value_heap_bytes() > 0);

    let report = owner.close();
    assert_eq!(report.session_id(), SESSION_ID);
    assert_eq!(report.released_entries(), before.entries());
    assert_eq!(report.released_metadata_bytes(), before.metadata_bytes());
    assert_eq!(
        report.released_value_heap_bytes(),
        before.value_heap_bytes()
    );
    assert_eq!(report.released_resident_bytes(), before.resident_bytes());
    assert_eq!(report.peak_resident_bytes(), before.peak_resident_bytes());
    assert_eq!(owner.phase(), ReadySessionPhase::Closed);
    assert_eq!(owner.close_report(), Some(report));

    let resources = owner.resources();
    assert_eq!(resources.entries(), 0);
    assert_eq!(resources.metadata_bytes(), 0);
    assert_eq!(resources.value_heap_bytes(), 0);
    assert_eq!(resources.resident_bytes(), 0);
    assert_eq!(owner.close(), report);
    assert_eq!(owner.close_report(), Some(report));

    for error in [owner.stats().unwrap_err(), owner.binding().unwrap_err()] {
        assert_eq!(error.code(), ReadySessionErrorCode::SessionClosed);
        assert_eq!(error.category(), ReadySessionErrorCategory::Lifecycle);
        assert_eq!(
            error.recoverability(),
            ReadySessionRecoverability::OpenNewSession
        );
    }
}

#[test]
fn closed_lookup_wins_over_foreign_binding_and_cancellation() {
    let fixture = fixture(0x64);
    let index = ready_index(&fixture);
    let binding = binding(&index);
    let limits = ReferenceChainLimits::default();
    let mut owner = ReadySessionOwner::new(binding, ReadyStoreLimits::default()).unwrap();
    owner.close();

    let foreign_binding = ReadyStoreBinding::for_index(
        &index,
        ReadyStoreSessionId::new(SESSION_ID.value() + 1),
        ReadyStoreEpoch::new(EPOCH.value() + 1),
    );
    let error = owner
        .lookup(
            key(foreign_binding, object_ref(1), limits),
            &MustNotProbeCancellation,
        )
        .unwrap_err();
    assert_eq!(error.code(), ReadySessionErrorCode::SessionClosed);
    assert_eq!(error.category(), ReadySessionErrorCategory::Lifecycle);
    assert_eq!(
        error.recoverability(),
        ReadySessionRecoverability::OpenNewSession
    );
}

#[test]
fn closed_admission_returns_the_move_only_value_and_redacts_debug_output() {
    let fixture = fixture(0x65);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = binding(&index);
    let limits = ReferenceChainLimits::default();
    let mut owner = ReadySessionOwner::new(binding, ReadyStoreLimits::default()).unwrap();
    owner.close();

    let foreign_binding = ReadyStoreBinding::for_index(
        &index,
        ReadyStoreSessionId::new(SESSION_ID.value() + 1),
        ReadyStoreEpoch::new(EPOCH.value() + 1),
    );
    let failure = owner
        .try_admit(
            key(foreign_binding, object_ref(1), limits),
            resolve(&index, &source, object_ref(1), limits),
            &MustNotProbeCancellation,
        )
        .unwrap_err();
    assert_eq!(failure.error().code(), ReadySessionErrorCode::SessionClosed);
    assert_eq!(
        failure.error().recoverability(),
        ReadySessionRecoverability::OpenNewSession
    );
    let debug = format!("{failure:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("SESSION_SECRET_VALUE"));
    assert!(!debug.contains("IndirectObject"));
    assert_eq!(failure.into_value().root(), object_ref(1));
}

#[test]
fn empty_close_releases_precharged_metadata_and_reports_zero_value_state() {
    let fixture = fixture(0x66);
    let index = ready_index(&fixture);
    let binding = binding(&index);
    let mut owner = ReadySessionOwner::new(binding, ReadyStoreLimits::default()).unwrap();
    let before = owner.stats().unwrap();
    assert_eq!(before.entries(), 0);
    assert_eq!(before.value_heap_bytes(), 0);
    assert!(before.metadata_bytes() > 0);

    let report = owner.close();
    assert_eq!(report.released_entries(), 0);
    assert_eq!(report.released_metadata_bytes(), before.metadata_bytes());
    assert_eq!(report.released_value_heap_bytes(), 0);
    assert_eq!(report.released_resident_bytes(), before.metadata_bytes());
    assert_eq!(report.peak_resident_bytes(), before.peak_resident_bytes());
    assert_eq!(owner.close_report(), Some(report));
    assert_eq!(owner.resources().resident_bytes(), 0);
}

#[test]
fn ready_cache_cancellation_keeps_lower_error_identity_and_move_ownership() {
    let fixture = fixture(0x67);
    let source = store_for(&fixture);
    let index = ready_index(&fixture);
    let binding = binding(&index);
    let limits = ReferenceChainLimits::default();
    let exact = key(binding, object_ref(1), limits);
    let mut owner = ReadySessionOwner::new(binding, ReadyStoreLimits::default()).unwrap();
    let cancelled = AtomicBool::new(true);

    let lookup_error = owner.lookup(exact, &cancelled).unwrap_err();
    assert_eq!(
        lookup_error.code(),
        ReadySessionErrorCode::ReadyStore(ReadyStoreErrorCode::Cancelled)
    );
    assert_eq!(
        lookup_error.category(),
        ReadySessionErrorCategory::Cancellation
    );
    assert_eq!(
        lookup_error.recoverability(),
        ReadySessionRecoverability::AbandonOperation
    );

    let failure = owner
        .try_admit(
            exact,
            resolve(&index, &source, object_ref(1), limits),
            &cancelled,
        )
        .unwrap_err();
    assert_eq!(
        failure.error().code(),
        ReadySessionErrorCode::ReadyStore(ReadyStoreErrorCode::Cancelled)
    );
    assert_eq!(
        failure.error().category(),
        ReadySessionErrorCategory::Cancellation
    );
    assert_eq!(
        failure.error().recoverability(),
        ReadySessionRecoverability::AbandonOperation
    );
    assert_eq!(failure.into_value().root(), object_ref(1));
    assert_eq!(owner.stats().unwrap().entries(), 0);
}
