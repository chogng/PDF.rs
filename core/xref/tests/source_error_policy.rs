use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeStore, RangeStoreLimitConfig, RangeStoreLimits, ReadPoll,
    ReadRequest, RequestPriority, ResumeCheckpoint, SourceErrorCategory, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    NeverCancelled, OpenXrefJob, XrefErrorCategory, XrefErrorCode, XrefJobContext, XrefLimits,
    XrefPoll, XrefRecoverability,
};

const SOURCE_LEN: u64 = 612;

fn snapshot() -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0x51; 32]), SourceRevision::new(3)),
        Some(SOURCE_LEN),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x52; 32]),
    )
}

fn open() -> OpenXrefJob {
    OpenXrefJob::new(
        snapshot(),
        XrefJobContext::new(
            JobId::new(31),
            ResumeCheckpoint::new(70),
            ResumeCheckpoint::new(71),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .expect("test job configuration is valid")
}

#[test]
fn lower_resource_failures_keep_resource_policy() {
    let config = RangeStoreLimitConfig {
        max_tickets: 1,
        ..RangeStoreLimitConfig::default()
    };
    let store = RangeStore::new(snapshot(), RangeStoreLimits::validate(config).unwrap()).unwrap();
    let occupying = ReadRequest::new(
        ByteRange::new(0, 1).unwrap(),
        RequestPriority::BackgroundPrefetch,
        JobId::new(99),
        ResumeCheckpoint::new(1),
    );
    assert!(matches!(store.poll(occupying), ReadPoll::Pending { .. }));

    let error = match open().poll(&store, &NeverCancelled) {
        XrefPoll::Failed(error) => error,
        _ => panic!("a full ticket ledger must reject the xref source request"),
    };
    assert_eq!(error.code(), XrefErrorCode::SourceFailure);
    assert_eq!(error.category(), XrefErrorCategory::Resource);
    assert_eq!(error.recoverability(), XrefRecoverability::ReduceWorkload);
    assert_eq!(
        error.source_error().map(|source| source.category()),
        Some(SourceErrorCategory::Resource)
    );
}

#[test]
fn lower_lifecycle_failures_become_configuration_failures() {
    let store = RangeStore::new(snapshot(), RangeStoreLimits::default()).unwrap();
    let conflicting = ReadRequest::new(
        ByteRange::new(0, SOURCE_LEN).unwrap(),
        RequestPriority::Metadata,
        JobId::new(31),
        ResumeCheckpoint::new(999),
    );
    assert!(matches!(store.poll(conflicting), ReadPoll::Pending { .. }));

    let error = match open().poll(&store, &NeverCancelled) {
        XrefPoll::Failed(error) => error,
        _ => panic!("conflicting checkpoints must reject the xref source request"),
    };
    assert_eq!(error.code(), XrefErrorCode::SourceFailure);
    assert_eq!(error.category(), XrefErrorCategory::Configuration);
    assert_eq!(
        error.recoverability(),
        XrefRecoverability::CorrectConfiguration
    );
    assert_eq!(
        error.source_error().map(|source| source.category()),
        Some(SourceErrorCategory::Lifecycle)
    );
}
