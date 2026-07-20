use std::sync::atomic::{AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_object::{
    IndirectObject, IndirectObjectTarget, IndirectObjectValue, NeverCancelled, ObjectCancellation,
    ObjectErrorCode, ObjectJobContext, ObjectLimitKind, ObjectLimits, ObjectPoll, ObjectStats,
    ObjectWorkCaps, OpenObjectJob,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimitKind, SyntaxLimits};

const MIB: u64 = 1024 * 1024;
const HARD_MAX_TOTAL_BYTES: u64 = 256 * MIB;

fn snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0x8d; 32]), SourceRevision::new(5)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x42; 32]),
    )
}

fn fixture(body: &[u8]) -> (Vec<u8>, u64) {
    let mut bytes = body.to_vec();
    let startxref = u64::try_from(bytes.len()).expect("fixture length fits u64");
    bytes.extend_from_slice(b"xref\n");
    (bytes, startxref)
}

fn supplied_store(bytes: &[u8]) -> RangeStore {
    let source = snapshot(u64::try_from(bytes.len()).expect("fixture length fits u64"));
    let store =
        RangeStore::new(source, Default::default()).expect("test RangeStore limits are valid");
    let range = ByteRange::new(0, source.len().expect("test source length is known"))
        .expect("complete fixture range is valid");
    store
        .supply(
            RangeResponse::new(source, range, bytes.to_vec())
                .expect("fixture response matches its range"),
        )
        .expect("fixture fits the test RangeStore");
    store
}

fn target(source: SourceSnapshot, startxref: u64) -> IndirectObjectTarget {
    IndirectObjectTarget::new(
        source,
        ObjectRef::new(1, 0).expect("test reference is valid"),
        0,
        startxref,
        startxref,
    )
    .expect("test target geometry is valid")
}

fn context() -> ObjectJobContext {
    ObjectJobContext::new(
        JobId::new(51),
        ResumeCheckpoint::new(61),
        ResumeCheckpoint::new(62),
        RequestPriority::Metadata,
    )
}

fn poll_once(mut open: OpenObjectJob, store: &RangeStore) -> (ObjectPoll, ObjectStats) {
    let poll = open.poll(store, &NeverCancelled);
    let stats = open.stats();
    (poll, stats)
}

fn ready(poll: ObjectPoll) -> IndirectObject {
    match poll {
        ObjectPoll::Ready(object) => object,
        ObjectPoll::Failed(error) => panic!("expected a ready object, got {error}"),
        ObjectPoll::Pending { .. } => panic!("a completely supplied fixture must not suspend"),
    }
}

fn failed(poll: ObjectPoll) -> pdf_rs_object::ObjectError {
    match poll {
        ObjectPoll::Failed(error) => error,
        ObjectPoll::Ready(_) => panic!("expected scoped work exhaustion, got a ready object"),
        ObjectPoll::Pending { .. } => panic!("a completely supplied fixture must not suspend"),
    }
}

struct PanicSource {
    snapshot: SourceSnapshot,
}

impl ByteSource for PanicSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("read work cap must be charged before polling the byte source")
    }
}

struct PanicAfterJobProbe {
    probes: AtomicUsize,
}

impl ObjectCancellation for PanicAfterJobProbe {
    fn is_cancelled(&self) -> bool {
        let probe = self.probes.fetch_add(1, Ordering::AcqRel);
        assert_eq!(
            probe, 0,
            "parse work cap must be charged before invoking the syntax parser"
        );
        false
    }
}

#[test]
fn work_caps_validate_positive_hard_bounded_values_and_getters() {
    for (read, parse) in [(1, 1), (HARD_MAX_TOTAL_BYTES, HARD_MAX_TOTAL_BYTES)] {
        let caps = ObjectWorkCaps::new(read, parse).expect("boundary caps must validate");
        assert_eq!(caps.max_read_bytes(), read);
        assert_eq!(caps.max_parse_bytes(), parse);
        assert_eq!(caps.max_retained_bytes(), None);
    }

    for retained in [0, 1, u64::MAX] {
        let caps = ObjectWorkCaps::new_with_retained_bytes(1, 1, retained)
            .expect("retained caps do not widen object work");
        assert_eq!(caps.max_read_bytes(), 1);
        assert_eq!(caps.max_parse_bytes(), 1);
        assert_eq!(caps.max_retained_bytes(), Some(retained));
    }

    for (read, parse) in [
        (0, 1),
        (1, 0),
        (HARD_MAX_TOTAL_BYTES + 1, 1),
        (1, HARD_MAX_TOTAL_BYTES + 1),
        (u64::MAX, u64::MAX),
    ] {
        assert_eq!(
            ObjectWorkCaps::new(read, parse).unwrap_err().code(),
            ObjectErrorCode::InvalidLimits
        );
    }
}

#[test]
fn retained_cap_is_enforced_by_the_child_syntax_parser_with_exact_lower_context() {
    let (bytes, startxref) =
        fixture(b"1 0 obj\n<< /Key (allocator-visible) /Items [null null] >>\nendobj\n");
    let store = supplied_store(&bytes);
    let limits = ObjectLimits::default();
    let baseline = OpenObjectJob::new(
        target(store.snapshot(), startxref),
        context(),
        limits,
        SyntaxLimits::default(),
    )
    .unwrap();
    let (baseline_poll, baseline_stats) = poll_once(baseline, &store);
    ready(baseline_poll);
    let exact_retained = baseline_stats.retained_heap_bytes();
    assert!(exact_retained > 1);

    let exact_caps = ObjectWorkCaps::new_with_retained_bytes(
        baseline_stats.read_bytes(),
        baseline_stats.parse_bytes(),
        exact_retained,
    )
    .unwrap();
    let exact = OpenObjectJob::new_with_work_caps(
        target(store.snapshot(), startxref),
        context(),
        limits,
        SyntaxLimits::default(),
        exact_caps,
    )
    .unwrap();
    let (exact_poll, exact_stats) = poll_once(exact, &store);
    ready(exact_poll);
    assert_eq!(exact_stats.retained_heap_bytes(), exact_retained);

    let one_less_caps = ObjectWorkCaps::new_with_retained_bytes(
        baseline_stats.read_bytes(),
        baseline_stats.parse_bytes(),
        exact_retained - 1,
    )
    .unwrap();
    let one_less = OpenObjectJob::new_with_work_caps(
        target(store.snapshot(), startxref),
        context(),
        limits,
        SyntaxLimits::default(),
        one_less_caps,
    )
    .unwrap();
    let (one_less_poll, one_less_stats) = poll_once(one_less, &store);
    let error = failed(one_less_poll);
    assert_eq!(error.code(), ObjectErrorCode::SyntaxFailure);
    assert_eq!(error.limit(), None);
    let syntax_error = error
        .syntax_error()
        .expect("object resource failure retains the lower syntax error");
    let limit = syntax_error
        .limit()
        .expect("lower syntax retained failure carries exact context");
    assert_eq!(limit.kind(), SyntaxLimitKind::RetainedBytes);
    assert_eq!(limit.limit(), exact_retained - 1);
    assert!(limit.consumed().checked_add(limit.attempted()).unwrap() > limit.limit());
    assert!(one_less_stats.retained_heap_bytes() <= limit.limit());
}

#[test]
fn job_rejects_work_caps_above_its_configured_totals() {
    let (bytes, startxref) = fixture(b"1 0 obj\nnull\nendobj\n");
    let store = supplied_store(&bytes);
    let limits = ObjectLimits::default();

    for caps in [
        ObjectWorkCaps::new(limits.max_total_read_bytes() + 1, 1).unwrap(),
        ObjectWorkCaps::new(1, limits.max_total_parse_bytes() + 1).unwrap(),
    ] {
        let error = OpenObjectJob::new_with_work_caps(
            target(store.snapshot(), startxref),
            context(),
            limits,
            SyntaxLimits::default(),
            caps,
        )
        .unwrap_err();
        assert_eq!(error.code(), ObjectErrorCode::InvalidLimits);
        assert_eq!(error.reference(), ObjectRef::new(1, 0).ok());
    }
}

#[test]
fn exact_and_one_less_scoped_caps_cover_direct_and_stream_jobs() {
    let fixtures = [
        (b"1 0 obj\n<< /Key (value) >>\nendobj\n".as_slice(), false),
        (
            b"1 0 obj\n<< /Length 4 >>\nstream\nq\nQ\n\nendstream\nendobj\n".as_slice(),
            true,
        ),
    ];

    for (body, expect_stream) in fixtures {
        let (bytes, startxref) = fixture(body);
        let store = supplied_store(&bytes);
        let limits = ObjectLimits::default();
        let baseline = OpenObjectJob::new(
            target(store.snapshot(), startxref),
            context(),
            limits,
            SyntaxLimits::default(),
        )
        .unwrap();
        let (baseline_poll, baseline_stats) = poll_once(baseline, &store);
        let baseline_object = ready(baseline_poll);
        assert_eq!(
            matches!(baseline_object.value(), IndirectObjectValue::Stream(_)),
            expect_stream
        );
        assert!(baseline_stats.read_bytes() > 1);
        assert!(baseline_stats.parse_bytes() > 1);

        let exact_caps =
            ObjectWorkCaps::new(baseline_stats.read_bytes(), baseline_stats.parse_bytes()).unwrap();
        let exact = OpenObjectJob::new_with_work_caps(
            target(store.snapshot(), startxref),
            context(),
            limits,
            SyntaxLimits::default(),
            exact_caps,
        )
        .unwrap();
        assert_eq!(exact.work_caps(), exact_caps);
        let (exact_poll, exact_stats) = poll_once(exact, &store);
        assert_eq!(ready(exact_poll), baseline_object);
        assert_eq!(exact_stats, baseline_stats);

        let read_cap = baseline_stats.read_bytes() - 1;
        let read_limited = OpenObjectJob::new_with_work_caps(
            target(store.snapshot(), startxref),
            context(),
            limits,
            SyntaxLimits::default(),
            ObjectWorkCaps::new(read_cap, baseline_stats.parse_bytes()).unwrap(),
        )
        .unwrap();
        let (read_poll, read_stats) = poll_once(read_limited, &store);
        let read_error = failed(read_poll);
        assert_eq!(read_error.code(), ObjectErrorCode::ResourceLimit);
        assert_eq!(
            read_error.limit().map(|limit| limit.kind()),
            Some(ObjectLimitKind::TotalReadBytes)
        );
        assert_eq!(
            read_error.limit().map(|limit| limit.limit()),
            Some(read_cap)
        );
        assert!(read_stats.read_bytes() <= read_cap);

        let parse_cap = baseline_stats.parse_bytes() - 1;
        let parse_limited = OpenObjectJob::new_with_work_caps(
            target(store.snapshot(), startxref),
            context(),
            limits,
            SyntaxLimits::default(),
            ObjectWorkCaps::new(baseline_stats.read_bytes(), parse_cap).unwrap(),
        )
        .unwrap();
        let (parse_poll, parse_stats) = poll_once(parse_limited, &store);
        let parse_error = failed(parse_poll);
        assert_eq!(parse_error.code(), ObjectErrorCode::ResourceLimit);
        assert_eq!(
            parse_error.limit().map(|limit| limit.kind()),
            Some(ObjectLimitKind::TotalParseBytes)
        );
        assert_eq!(
            parse_error.limit().map(|limit| limit.limit()),
            Some(parse_cap)
        );
        assert!(parse_stats.parse_bytes() <= parse_cap);
    }
}

#[test]
fn scoped_caps_are_charged_before_source_poll_and_parser_invocation() {
    let (bytes, startxref) = fixture(b"1 0 obj\nnull\nendobj\n");
    let source = snapshot(u64::try_from(bytes.len()).unwrap());
    let limits = ObjectLimits::default();
    let mut read_limited = OpenObjectJob::new_with_work_caps(
        target(source, startxref),
        context(),
        limits,
        SyntaxLimits::default(),
        ObjectWorkCaps::new(1, limits.max_total_parse_bytes()).unwrap(),
    )
    .unwrap();
    let read_error = failed(read_limited.poll(&PanicSource { snapshot: source }, &NeverCancelled));
    assert_eq!(
        read_error.limit().map(|limit| limit.kind()),
        Some(ObjectLimitKind::TotalReadBytes)
    );

    let store = supplied_store(&bytes);
    let mut parse_limited = OpenObjectJob::new_with_work_caps(
        target(store.snapshot(), startxref),
        context(),
        limits,
        SyntaxLimits::default(),
        ObjectWorkCaps::new(limits.max_total_read_bytes(), 1).unwrap(),
    )
    .unwrap();
    let cancellation = PanicAfterJobProbe {
        probes: AtomicUsize::new(0),
    };
    let parse_error = failed(parse_limited.poll(&store, &cancellation));
    assert_eq!(
        parse_error.limit().map(|limit| limit.kind()),
        Some(ObjectLimitKind::TotalParseBytes)
    );
    assert_eq!(cancellation.probes.load(Ordering::Acquire), 1);
}

#[test]
fn legacy_constructor_matches_explicit_configured_total_caps() {
    let (bytes, startxref) =
        fixture(b"1 0 obj\n<< /Length 4 >>\nstream\nq\nQ\n\nendstream\nendobj\n");
    let store = supplied_store(&bytes);
    let limits = ObjectLimits::default();
    let configured_caps = ObjectWorkCaps::new(
        limits.max_total_read_bytes(),
        limits.max_total_parse_bytes(),
    )
    .unwrap();

    let legacy = OpenObjectJob::new(
        target(store.snapshot(), startxref),
        context(),
        limits,
        SyntaxLimits::default(),
    )
    .unwrap();
    assert_eq!(legacy.work_caps(), configured_caps);
    assert_eq!(legacy.work_caps().max_retained_bytes(), None);
    let explicit = OpenObjectJob::new_with_work_caps(
        target(store.snapshot(), startxref),
        context(),
        limits,
        SyntaxLimits::default(),
        configured_caps,
    )
    .unwrap();

    let (legacy_poll, legacy_stats) = poll_once(legacy, &store);
    let (explicit_poll, explicit_stats) = poll_once(explicit, &store);
    assert_eq!(ready(legacy_poll), ready(explicit_poll));
    assert_eq!(legacy_stats, explicit_stats);
}
