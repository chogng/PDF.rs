use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeStore, RangeStoreLimitConfig, RangeStoreLimits,
    ReadPoll, ReadRequest, RequestPriority, ResumeCheckpoint, SourceError, SourceErrorCategory,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_object::{
    IndirectObjectTarget, NeverCancelled, ObjectError, ObjectErrorCategory, ObjectErrorCode,
    ObjectJobContext, ObjectLimits, ObjectPoll, ObjectRecoverability, OpenObjectJob,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};

const SOURCE_LEN: u64 = 128;
const OBJECT_OFFSET: u64 = 16;
const REVISION_STARTXREF: u64 = 96;

fn snapshot() -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0x61; 32]), SourceRevision::new(7)),
        Some(SOURCE_LEN),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x62; 32]),
    )
}

fn reference() -> ObjectRef {
    ObjectRef::new(4, 0).expect("the test reference is a valid nonzero indirect object")
}

fn open() -> OpenObjectJob {
    let target = IndirectObjectTarget::new(
        snapshot(),
        reference(),
        OBJECT_OFFSET,
        REVISION_STARTXREF,
        REVISION_STARTXREF,
    )
    .expect("test target geometry is valid");
    OpenObjectJob::new(
        target,
        ObjectJobContext::new(
            JobId::new(31),
            ResumeCheckpoint::new(70),
            ResumeCheckpoint::new(71),
            RequestPriority::Metadata,
        ),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .expect("test object job configuration is valid")
}

struct FailingSource {
    snapshot: SourceSnapshot,
    error: SourceError,
}

impl ByteSource for FailingSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        ReadPoll::Failed(self.error)
    }
}

fn object_error(lower: SourceError) -> ObjectError {
    let source = FailingSource {
        snapshot: snapshot(),
        error: lower,
    };
    match open().poll(&source, &NeverCancelled) {
        ObjectPoll::Failed(error) => error,
        ObjectPoll::Ready(_) => panic!("a failing byte source cannot frame an object"),
        ObjectPoll::Pending { .. } => panic!("a failing byte source cannot suspend an object job"),
    }
}

fn resource_error() -> SourceError {
    let config = RangeStoreLimitConfig {
        max_tickets: 1,
        ..RangeStoreLimitConfig::default()
    };
    let store = RangeStore::new(snapshot(), RangeStoreLimits::validate(config).unwrap()).unwrap();
    let occupying = ReadRequest::new(
        ByteRange::new(0, 1).unwrap(),
        RequestPriority::BackgroundPrefetch,
        JobId::new(90),
        ResumeCheckpoint::new(1),
    );
    assert!(matches!(store.poll(occupying), ReadPoll::Pending { .. }));
    let rejected = ReadRequest::new(
        ByteRange::new(2, 1).unwrap(),
        RequestPriority::BackgroundPrefetch,
        JobId::new(91),
        ResumeCheckpoint::new(2),
    );
    match store.poll(rejected) {
        ReadPoll::Failed(error) => error,
        _ => panic!("a full ticket ledger must produce a source resource failure"),
    }
}

fn lifecycle_error() -> SourceError {
    let store = RangeStore::new(snapshot(), RangeStoreLimits::default()).unwrap();
    let range = ByteRange::new(0, 1).unwrap();
    let first = ReadRequest::new(
        range,
        RequestPriority::Metadata,
        JobId::new(92),
        ResumeCheckpoint::new(3),
    );
    assert!(matches!(store.poll(first), ReadPoll::Pending { .. }));
    let conflicting = ReadRequest::new(
        range,
        RequestPriority::Metadata,
        JobId::new(92),
        ResumeCheckpoint::new(4),
    );
    match store.poll(conflicting) {
        ReadPoll::Failed(error) => error,
        _ => panic!("conflicting checkpoints must produce a source lifecycle failure"),
    }
}

fn assert_location_and_lower_error(error: ObjectError, lower: SourceError) {
    assert_eq!(error.code(), ObjectErrorCode::SourceFailure);
    assert_eq!(error.reference(), Some(reference()));
    assert_eq!(error.offset(), Some(OBJECT_OFFSET));
    assert_eq!(error.source_error(), Some(lower));
}

#[test]
fn lower_resource_failures_keep_resource_policy_reference_and_offset() {
    let lower = resource_error();
    assert_eq!(lower.category(), SourceErrorCategory::Resource);

    let error = object_error(lower);
    assert_eq!(error.category(), ObjectErrorCategory::Resource);
    assert_eq!(error.recoverability(), ObjectRecoverability::ReduceWorkload);
    assert_location_and_lower_error(error, lower);
}

#[test]
fn lower_input_and_lifecycle_failures_become_configuration_failures() {
    let input = ByteRange::new(0, 0).expect_err("a zero-length source range is invalid input");
    let lifecycle = lifecycle_error();
    assert_eq!(input.category(), SourceErrorCategory::Input);
    assert_eq!(lifecycle.category(), SourceErrorCategory::Lifecycle);

    for lower in [input, lifecycle] {
        let error = object_error(lower);
        assert_eq!(error.category(), ObjectErrorCategory::Configuration);
        assert_eq!(
            error.recoverability(),
            ObjectRecoverability::CorrectConfiguration
        );
        assert_location_and_lower_error(error, lower);
    }
}

#[test]
fn lower_availability_failure_preserves_retry_source_policy() {
    let lower = SourceError::source_unavailable();
    assert_eq!(lower.category(), SourceErrorCategory::Availability);

    let error = object_error(lower);
    assert_eq!(error.category(), ObjectErrorCategory::Source);
    assert_eq!(error.recoverability(), ObjectRecoverability::RetrySource);
    assert_location_and_lower_error(error, lower);
}
