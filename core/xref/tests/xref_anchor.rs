use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_syntax::ObjectRef;
use pdf_rs_xref::{
    NeverCancelled, OpenXrefAnchorJob, XrefAnchorJobContext, XrefAnchorKind, XrefAnchorLimitConfig,
    XrefAnchorLimits, XrefAnchorPhase, XrefAnchorPoll, XrefErrorCode, XrefLimitKind,
};

fn snapshot(byte: u8, len: Option<u64>) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([byte; 32]), SourceRevision::new(8)),
        len,
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x29; 32]),
    )
}

fn context() -> XrefAnchorJobContext {
    XrefAnchorJobContext::new(JobId::new(92), ResumeCheckpoint::new(311))
}

fn supplied_store(bytes: &[u8], identity_byte: u8) -> RangeStore {
    let source = snapshot(identity_byte, Some(u64::try_from(bytes.len()).unwrap()));
    let store = RangeStore::new(source, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(source, range, bytes.to_vec()).unwrap())
        .unwrap();
    store
}

fn classify(bytes: &[u8]) -> Result<pdf_rs_xref::XrefAnchor, pdf_rs_xref::XrefError> {
    classify_with_bound(bytes, u64::try_from(bytes.len()).unwrap())
}

fn classify_with_bound(
    bytes: &[u8],
    upper_bound: u64,
) -> Result<pdf_rs_xref::XrefAnchor, pdf_rs_xref::XrefError> {
    let store = supplied_store(bytes, 0x41);
    let mut job = OpenXrefAnchorJob::new(
        store.snapshot(),
        0,
        upper_bound,
        context(),
        XrefAnchorLimits::default(),
    )?;
    match job.poll(&store, &NeverCancelled) {
        XrefAnchorPoll::Ready(anchor) => Ok(anchor),
        XrefAnchorPoll::Failed(error) => Err(error),
        XrefAnchorPoll::Pending { .. } => panic!("fully supplied anchor suspended"),
    }
}

#[test]
fn caller_physical_bound_never_fabricates_an_eof_delimiter() {
    let object = classify_with_bound(b"7 0 object\n", 7).unwrap_err();
    assert_eq!(object.code(), XrefErrorCode::InvalidXrefAnchor);
    assert_eq!(object.limit(), None);

    let crlf = classify_with_bound(b"xref\r\n0 1\r\n", 5).unwrap_err();
    assert_eq!(crlf.code(), XrefErrorCode::InvalidXrefAnchor);
    assert_eq!(crlf.limit(), None);

    let source_eof = classify(b"xref\r").expect("bare CR is complete only at actual source EOF");
    assert_eq!(source_eof.kind(), XrefAnchorKind::Traditional);
    assert_eq!(source_eof.header_span().len(), 5);
}

#[test]
fn exact_traditional_and_indirect_headers_are_distinct() {
    for (bytes, expected_len) in [
        (&b"xref\n0 1\n"[..], 5_u64),
        (&b"xref\r\n0 1\r\n"[..], 6),
        (&b"xref \t\n0 1\n"[..], 7),
    ] {
        let anchor = classify(bytes).unwrap();
        assert_eq!(anchor.kind(), XrefAnchorKind::Traditional);
        assert_eq!(anchor.stream_object(), None);
        assert_eq!(anchor.startxref(), 0);
        assert_eq!(anchor.header_span().start(), 0);
        assert_eq!(anchor.header_span().len(), expected_len);
        assert_eq!(anchor.snapshot().identity(), anchor.source());
    }

    for (bytes, expected) in [
        (
            &b"7 2 obj\n<< /Type /XRef >>"[..],
            ObjectRef::new(7, 2).unwrap(),
        ),
        (
            &b"+7 2 obj\n<< /Type /XRef >>"[..],
            ObjectRef::new(7, 2).unwrap(),
        ),
    ] {
        let anchor = classify(bytes).unwrap();
        assert_eq!(anchor.kind(), XrefAnchorKind::StreamObject(expected));
        assert_eq!(anchor.stream_object(), Some(expected));
        assert_eq!(anchor.header_span().start(), 0);
        assert!(anchor.header_span().end_exclusive() <= u64::try_from(bytes.len()).unwrap());
    }
}

#[test]
fn keyword_delimiter_leading_trivia_and_numeric_overflow_are_rejected() {
    for bytes in [
        &b"xref 0 1\n"[..],
        &b"xref0 1\n"[..],
        &b"xref/Name\n"[..],
        &b" xref\n"[..],
        &b" 7 0 obj\n"[..],
        &b"0 0 obj\n"[..],
        &b"4294967296 0 obj\n"[..],
        &b"7 65536 obj\n"[..],
        &b"7 0 object\n"[..],
    ] {
        let error = classify(bytes).unwrap_err();
        assert_eq!(
            error.code(),
            XrefErrorCode::InvalidXrefAnchor,
            "unexpected classification for {}",
            String::from_utf8_lossy(bytes)
        );
        assert_eq!(error.offset(), Some(0));
    }
}

#[test]
fn pending_repoll_preserves_ticket_checkpoint_and_work() {
    let bytes = b"17 4 obj\n<< /Type /XRef >>";
    let source = snapshot(0x42, Some(u64::try_from(bytes.len()).unwrap()));
    let store = RangeStore::new(source, Default::default()).unwrap();
    let mut job = OpenXrefAnchorJob::new(
        source,
        0,
        u64::try_from(bytes.len()).unwrap(),
        context(),
        XrefAnchorLimits::default(),
    )
    .unwrap();

    let (ticket, missing) = match job.poll(&store, &NeverCancelled) {
        XrefAnchorPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(checkpoint, context().checkpoint());
            (ticket, missing)
        }
        other => panic!("empty store did not suspend: {other:?}"),
    };
    let charged = job.stats();
    match job.poll(&store, &NeverCancelled) {
        XrefAnchorPoll::Pending {
            ticket: repeated,
            missing: repeated_missing,
            checkpoint,
        } => {
            assert_eq!(repeated, ticket);
            assert_eq!(repeated_missing, missing);
            assert_eq!(checkpoint, context().checkpoint());
        }
        other => panic!("pending anchor did not remain stable: {other:?}"),
    }
    assert_eq!(job.stats(), charged);
    assert_eq!(charged.attempts(), 1);
    assert_eq!(charged.read_bytes(), u64::try_from(bytes.len()).unwrap());
    assert_eq!(charged.parse_bytes(), 0);

    let requested = missing.as_slice()[0];
    let outcome = store
        .supply(RangeResponse::new(source, requested, bytes.to_vec()).unwrap())
        .unwrap();
    assert_eq!(outcome.ready_tickets(), &[ticket]);
    let anchor = match job.poll(&store, &NeverCancelled) {
        XrefAnchorPoll::Ready(anchor) => anchor,
        other => panic!("supplied anchor did not resume: {other:?}"),
    };
    assert_eq!(anchor.stream_object(), ObjectRef::new(17, 4).ok());
    assert_eq!(job.phase(), XrefAnchorPhase::Complete);
    assert_eq!(
        job.stats().parse_bytes(),
        u64::try_from(bytes.len()).unwrap()
    );
}

#[test]
fn source_change_and_cancellation_are_terminal_before_parse() {
    let bytes = b"7 0 obj\n";
    let bound = snapshot(0x43, Some(u64::try_from(bytes.len()).unwrap()));
    let foreign = supplied_store(bytes, 0x44);
    let mut changed = OpenXrefAnchorJob::new(
        bound,
        0,
        u64::try_from(bytes.len()).unwrap(),
        context(),
        XrefAnchorLimits::default(),
    )
    .unwrap();
    let error = match changed.poll(&foreign, &NeverCancelled) {
        XrefAnchorPoll::Failed(error) => error,
        other => panic!("source change did not fail: {other:?}"),
    };
    assert_eq!(error.code(), XrefErrorCode::SnapshotMismatch);
    assert_eq!(changed.stats().read_bytes(), 0);
    assert_eq!(changed.phase(), XrefAnchorPhase::Failed);

    let store = supplied_store(bytes, 0x45);
    let mut cancelled = OpenXrefAnchorJob::new(
        store.snapshot(),
        0,
        u64::try_from(bytes.len()).unwrap(),
        context(),
        XrefAnchorLimits::default(),
    )
    .unwrap();
    let error = match cancelled.poll(&store, &AtomicBool::new(true)) {
        XrefAnchorPoll::Failed(error) => error,
        other => panic!("cancelled classifier continued: {other:?}"),
    };
    assert_eq!(error.code(), XrefErrorCode::Cancelled);
    assert_eq!(cancelled.stats().read_bytes(), 0);
}

#[test]
fn exact_probe_and_source_limits_are_enforced_before_unbounded_work() {
    let bytes = b"1                0 obj\n";
    let store = supplied_store(bytes, 0x46);
    let limits = XrefAnchorLimits::validate(XrefAnchorLimitConfig {
        max_source_bytes: 64,
        max_anchor_bytes: 8,
    })
    .unwrap();
    let mut bounded = OpenXrefAnchorJob::new(
        store.snapshot(),
        0,
        u64::try_from(bytes.len()).unwrap(),
        context(),
        limits,
    )
    .unwrap();
    let error = match bounded.poll(&store, &NeverCancelled) {
        XrefAnchorPoll::Failed(error) => error,
        other => panic!("oversized header escaped its probe limit: {other:?}"),
    };
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    let limit = error.limit().unwrap();
    assert_eq!(limit.kind(), XrefLimitKind::AnchorBytes);
    assert_eq!(
        (limit.limit(), limit.consumed(), limit.attempted()),
        (8, 8, 1)
    );

    let source = snapshot(0x47, Some(65));
    let error = OpenXrefAnchorJob::new(source, 1, 64, context(), limits).unwrap_err();
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    assert_eq!(error.limit().unwrap().kind(), XrefLimitKind::SourceBytes);

    for config in [
        XrefAnchorLimitConfig {
            max_source_bytes: 0,
            max_anchor_bytes: 8,
        },
        XrefAnchorLimitConfig {
            max_source_bytes: 64,
            max_anchor_bytes: 4,
        },
        XrefAnchorLimitConfig {
            max_source_bytes: 64,
            max_anchor_bytes: 65,
        },
    ] {
        assert_eq!(
            XrefAnchorLimits::validate(config).unwrap_err().code(),
            XrefErrorCode::InvalidLimits
        );
    }
}

#[test]
fn source_and_caller_bounds_are_validated_at_construction() {
    let unknown = snapshot(0x48, None);
    assert_eq!(
        OpenXrefAnchorJob::new(unknown, 0, 8, context(), XrefAnchorLimits::default())
            .unwrap_err()
            .code(),
        XrefErrorCode::UnknownSourceLength
    );
    let source = snapshot(0x49, Some(16));
    for (startxref, upper_bound) in [(8, 8), (8, 17), (16, 17)] {
        assert_eq!(
            OpenXrefAnchorJob::new(
                source,
                startxref,
                upper_bound,
                context(),
                XrefAnchorLimits::default(),
            )
            .unwrap_err()
            .code(),
            XrefErrorCode::StartXrefOutOfBounds
        );
    }
}
