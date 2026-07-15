use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, ResumeCheckpoint, SourceErrorCode,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled, OpenTraditionalRevisionJob, TraditionalRevisionJobContext,
    TraditionalRevisionPhase, TraditionalRevisionPoll, TraditionalRevisionSection, XrefError,
    XrefErrorCode, XrefLimitConfig, XrefLimitKind, XrefLimits, XrefRecoverability,
};

const STARTXREF: u64 = 128;

fn identity(byte: u8) -> SourceIdentity {
    SourceIdentity::new(SourceStableId::new([byte; 32]), SourceRevision::new(4))
}

fn snapshot(bytes: &[u8], identity: SourceIdentity) -> SourceSnapshot {
    SourceSnapshot::new(
        identity,
        Some(u64::try_from(bytes.len()).unwrap()),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x52; 32]),
    )
}

fn fixture(subsections: &[(&str, &[&str])], trailer: &str) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.resize(usize::try_from(STARTXREF).unwrap(), b'x');
    bytes.extend_from_slice(b"xref\n");
    for (header, rows) in subsections {
        bytes.extend_from_slice(header.as_bytes());
        bytes.push(b'\n');
        for row in *rows {
            assert_eq!(row.len(), 18);
            bytes.extend_from_slice(row.as_bytes());
            bytes.extend_from_slice(b" \n");
        }
    }
    bytes.extend_from_slice(b"trailer\n");
    bytes.extend_from_slice(trailer.as_bytes());
    bytes.push(b'\n');
    bytes
}

fn canonical() -> Vec<u8> {
    fixture(
        &[
            ("2 1", &["0000000040 00000 n"]),
            ("7 1", &["0000000060 00000 n"]),
        ],
        "<< /Size 8 /Root 1 0 R /Prev 20 /XRefStm 80 >>",
    )
}

fn context() -> TraditionalRevisionJobContext {
    TraditionalRevisionJobContext::new(JobId::new(901), ResumeCheckpoint::new(902))
}

fn job(
    snapshot: SourceSnapshot,
    upper_bound: u64,
    limits: XrefLimits,
) -> OpenTraditionalRevisionJob {
    OpenTraditionalRevisionJob::new(
        snapshot,
        STARTXREF,
        upper_bound,
        context(),
        limits,
        SyntaxLimits::default(),
    )
    .unwrap()
}

fn supplied_store(bytes: &[u8], identity: SourceIdentity) -> RangeStore {
    let snapshot = snapshot(bytes, identity);
    let store = RangeStore::new(snapshot, Default::default()).unwrap();
    store
        .supply(
            RangeResponse::new(
                snapshot,
                ByteRange::new(0, u64::try_from(bytes.len()).unwrap()).unwrap(),
                bytes.to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    store
}

fn ready(bytes: &[u8]) -> TraditionalRevisionSection {
    let store = supplied_store(bytes, identity(0x31));
    let mut open = job(
        store.snapshot(),
        u64::try_from(bytes.len()).unwrap(),
        XrefLimits::default(),
    );
    match open.poll(&store, &NeverCancelled) {
        TraditionalRevisionPoll::Ready(section) => section,
        TraditionalRevisionPoll::Pending { .. } => panic!("fully supplied section stayed pending"),
        TraditionalRevisionPoll::Failed(error) => panic!("valid revision failed: {error}"),
    }
}

fn failed(bytes: &[u8]) -> XrefError {
    let store = supplied_store(bytes, identity(0x32));
    let mut open = job(
        store.snapshot(),
        u64::try_from(bytes.len()).unwrap(),
        XrefLimits::default(),
    );
    match open.poll(&store, &NeverCancelled) {
        TraditionalRevisionPoll::Failed(error) => error,
        TraditionalRevisionPoll::Ready(_) => panic!("invalid revision was accepted"),
        TraditionalRevisionPoll::Pending { .. } => panic!("fully supplied section stayed pending"),
    }
}

fn limits(update: impl FnOnce(&mut XrefLimitConfig)) -> XrefLimits {
    let mut config = XrefLimitConfig {
        max_source_bytes: 4096,
        initial_tail_bytes: 1,
        max_tail_bytes: 1,
        initial_section_bytes: 16,
        max_section_bytes: 512,
        max_total_read_bytes: 4096,
        max_total_parse_bytes: 4096,
        max_subsections: 16,
        max_entries: 64,
    };
    update(&mut config);
    XrefLimits::validate(config).unwrap()
}

#[test]
fn sparse_update_retains_prev_hybrid_root_and_source_geometry() {
    let bytes = canonical();
    let section = ready(&bytes);
    assert_eq!(section.source(), identity(0x31));
    assert_eq!(section.snapshot(), snapshot(&bytes, identity(0x31)));
    assert_eq!(section.startxref(), STARTXREF);
    assert_eq!(section.span().start(), STARTXREF);
    assert_eq!(
        section.span().end_exclusive(),
        u64::try_from(bytes.len() - 1).unwrap()
    );
    assert_eq!(section.declared_size(), 8);
    assert_eq!(section.root(), Some(ObjectRef::new(1, 0).unwrap()));
    assert_eq!(section.previous(), Some(20));
    assert_eq!(section.xref_stream(), Some(80));
    assert_eq!(
        section
            .entries()
            .iter()
            .map(|entry| entry.object_number())
            .collect::<Vec<_>>(),
        [2, 7]
    );
    assert_eq!(section.trailer().source(), identity(0x31));
    assert_eq!(section.clone().into_entries(), section.entries());
    let debug = format!("{section:?}");
    assert!(debug.contains("TraditionalRevisionSection"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn caller_bound_can_end_before_the_source_without_consuming_later_bytes() {
    let mut bytes = canonical();
    let upper_bound = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"later revision bytes that are outside this section");
    let store = supplied_store(&bytes, identity(0x33));
    let mut open = job(store.snapshot(), upper_bound, XrefLimits::default());

    let section = match open.poll(&store, &NeverCancelled) {
        TraditionalRevisionPoll::Ready(section) => section,
        other => panic!("caller-bounded section did not complete: {other:?}"),
    };
    assert_eq!(open.upper_bound(), upper_bound);
    assert_eq!(section.span().end_exclusive(), upper_bound - 1);
    assert!(upper_bound < store.snapshot().len().unwrap());
    assert_eq!(open.stats().read_bytes(), upper_bound - STARTXREF);
}

#[test]
fn older_rootless_and_base_hybrid_metadata_remain_explicit_candidates() {
    let rootless = fixture(
        &[("2 1", &["0000000040 00000 n"])],
        "<< /Size 5 /Prev 20 >>",
    );
    let rootless = ready(&rootless);
    assert_eq!(rootless.root(), None);
    assert_eq!(rootless.previous(), Some(20));
    assert_eq!(rootless.xref_stream(), None);

    let hybrid_base = fixture(
        &[("0 1", &["0000000000 65535 f"])],
        "<< /Size 5 /Root 1 0 R /XRefStm 80 >>",
    );
    let hybrid_base = ready(&hybrid_base);
    assert_eq!(hybrid_base.root(), Some(ObjectRef::new(1, 0).unwrap()));
    assert_eq!(hybrid_base.previous(), None);
    assert_eq!(hybrid_base.xref_stream(), Some(80));
}

#[test]
fn trailer_offsets_are_backward_ordered_unique_and_well_typed() {
    for trailer in [
        "<< /Size 8 /Prev -1 >>",
        "<< /Size 8 /Prev 128 >>",
        "<< /Size 8 /Prev 129 >>",
        "<< /Size 8 /XRefStm -1 >>",
        "<< /Size 8 /XRefStm 128 >>",
        "<< /Size 8 /Prev 80 /XRefStm 20 >>",
        "<< /Size 8 /Prev 20 /Prev 21 >>",
        "<< /Size 8 /XRefStm 80 /XRefStm 81 >>",
        "<< /Size 8 /Root 1 >>",
    ] {
        let bytes = fixture(&[("2 1", &["0000000040 00000 n"])], trailer);
        assert_eq!(failed(&bytes).code(), XrefErrorCode::InvalidTrailer);
    }
}

#[test]
fn sparse_rows_remain_strictly_bounded_and_object_zero_is_conditional() {
    let invalid_cases = [
        fixture(
            &[("8 1", &["0000000040 00000 n"])],
            "<< /Size 8 /Prev 20 >>",
        ),
        fixture(
            &[("2 1", &["0000000128 00000 n"])],
            "<< /Size 8 /Prev 20 >>",
        ),
        fixture(
            &[("0 1", &["0000000000 00000 f"])],
            "<< /Size 8 /Prev 20 >>",
        ),
        fixture(
            &[("0 1", &["0000000040 65535 n"])],
            "<< /Size 8 /Prev 20 >>",
        ),
        fixture(
            &[
                ("2 1", &["0000000040 00000 n"]),
                ("2 1", &["0000000060 00000 n"]),
            ],
            "<< /Size 8 /Prev 20 >>",
        ),
    ];
    for bytes in invalid_cases {
        assert!(matches!(
            failed(&bytes).code(),
            XrefErrorCode::InvalidEntry
                | XrefErrorCode::InvalidSubsection
                | XrefErrorCode::InvalidTrailer
        ));
    }

    let without_zero = fixture(
        &[("7 1", &["0000000060 00000 n"])],
        "<< /Size 8 /Prev 20 >>",
    );
    assert_eq!(ready(&without_zero).entries()[0].object_number(), 7);
}

#[test]
fn upper_half_before_lower_resumes_once_without_duplicate_charging() {
    let bytes = canonical();
    let source = snapshot(&bytes, identity(0x41));
    let store = RangeStore::new(source, Default::default()).unwrap();
    let upper_bound = u64::try_from(bytes.len()).unwrap();
    let mut open = job(source, upper_bound, XrefLimits::default());

    let (ticket, missing, checkpoint) = match open.poll(&store, &NeverCancelled) {
        TraditionalRevisionPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => (ticket, missing, checkpoint),
        other => panic!("empty store must suspend: {other:?}"),
    };
    assert_eq!(checkpoint, context().section_checkpoint());
    assert_eq!(
        missing.as_slice(),
        &[ByteRange::new(STARTXREF, upper_bound - STARTXREF).unwrap()]
    );
    let charged = open.stats();

    let midpoint = STARTXREF + (upper_bound - STARTXREF) / 2;
    let upper = ByteRange::new(midpoint, upper_bound - midpoint).unwrap();
    store
        .supply(
            RangeResponse::new(
                source,
                upper,
                bytes[usize::try_from(midpoint).unwrap()..].to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    match open.poll(&store, &NeverCancelled) {
        TraditionalRevisionPoll::Pending {
            ticket: repeated,
            checkpoint: repeated_checkpoint,
            ..
        } => {
            assert_eq!(repeated, ticket);
            assert_eq!(repeated_checkpoint, checkpoint);
        }
        other => panic!("partial reverse supply must stay pending: {other:?}"),
    }
    assert_eq!(open.stats(), charged);

    let lower = ByteRange::new(STARTXREF, midpoint - STARTXREF).unwrap();
    store
        .supply(
            RangeResponse::new(
                source,
                lower,
                bytes[usize::try_from(STARTXREF).unwrap()..usize::try_from(midpoint).unwrap()]
                    .to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    assert!(matches!(
        open.poll(&store, &NeverCancelled),
        TraditionalRevisionPoll::Ready(_)
    ));
    assert_eq!(open.phase(), TraditionalRevisionPhase::Complete);
    assert_eq!(open.stats().section_attempts(), 1);
    assert_eq!(open.stats().read_bytes(), upper_bound - STARTXREF);
    assert_eq!(open.stats().parse_bytes(), upper_bound - STARTXREF);
    assert_eq!(open.stats().entries(), 2);
}

#[test]
fn cancellation_snapshot_mismatch_and_source_change_are_stable_terminal_states() {
    let bytes = canonical();
    let store = supplied_store(&bytes, identity(0x42));
    let mut cancelled = job(
        store.snapshot(),
        u64::try_from(bytes.len()).unwrap(),
        XrefLimits::default(),
    );
    let cancellation = AtomicBool::new(true);
    assert!(matches!(
        cancelled.poll(&store, &cancellation),
        TraditionalRevisionPoll::Failed(error) if error.code() == XrefErrorCode::Cancelled
    ));
    assert_eq!(cancelled.stats().read_bytes(), 0);
    assert_eq!(cancelled.phase(), TraditionalRevisionPhase::Failed);

    let expected = snapshot(&bytes, identity(0x43));
    let foreign = RangeStore::new(snapshot(&bytes, identity(0x44)), Default::default()).unwrap();
    let mut mismatched = job(
        expected,
        u64::try_from(bytes.len()).unwrap(),
        XrefLimits::default(),
    );
    assert!(matches!(
        mismatched.poll(&foreign, &NeverCancelled),
        TraditionalRevisionPoll::Failed(error)
            if error.code() == XrefErrorCode::SnapshotMismatch
    ));

    let source = snapshot(&bytes, identity(0x45));
    let changing = RangeStore::new(source, Default::default()).unwrap();
    let mut changed = job(
        source,
        u64::try_from(bytes.len()).unwrap(),
        XrefLimits::default(),
    );
    assert!(matches!(
        changed.poll(&changing, &NeverCancelled),
        TraditionalRevisionPoll::Pending { .. }
    ));
    changing.signal_source_changed().unwrap();
    let error = match changed.poll(&changing, &NeverCancelled) {
        TraditionalRevisionPoll::Failed(error) => error,
        other => panic!("source change must terminate the job: {other:?}"),
    };
    assert_eq!(error.code(), XrefErrorCode::SourceFailure);
    assert_eq!(error.recoverability(), XrefRecoverability::ReopenSource);
    assert_eq!(
        error.source_error().map(|source| source.code()),
        Some(SourceErrorCode::SourceChanged)
    );
    assert!(matches!(
        changed.poll(&changing, &NeverCancelled),
        TraditionalRevisionPoll::Failed(replayed) if replayed == error
    ));
}

#[test]
fn geometric_work_section_and_anchor_bounds_fail_on_exact_dimensions() {
    let bytes = canonical();
    let store = supplied_store(&bytes, identity(0x51));
    let upper_bound = u64::try_from(bytes.len()).unwrap();
    let section_len = upper_bound - STARTXREF;
    let measured_limits = limits(|config| config.max_section_bytes = section_len);
    let mut measured = job(store.snapshot(), upper_bound, measured_limits);
    let required_section_bytes = match measured.poll(&store, &NeverCancelled) {
        TraditionalRevisionPoll::Ready(section) => section.span().len(),
        other => panic!("measured revision must be ready: {other:?}"),
    };
    assert!(measured.stats().section_attempts() > 1);

    let exact_read = measured.stats().read_bytes();
    let exact_parse = measured.stats().parse_bytes();
    let exact = limits(|config| {
        config.max_section_bytes = section_len;
        config.max_total_read_bytes = exact_read;
        config.max_total_parse_bytes = exact_parse;
    });
    let mut exact_job = job(store.snapshot(), upper_bound, exact);
    assert!(matches!(
        exact_job.poll(&store, &NeverCancelled),
        TraditionalRevisionPoll::Ready(_)
    ));

    for (read_limit, parse_limit, expected_kind) in [
        (exact_read - 1, 4096, XrefLimitKind::TotalReadBytes),
        (4096, exact_parse - 1, XrefLimitKind::TotalParseBytes),
    ] {
        let constrained = limits(|config| {
            config.max_section_bytes = section_len;
            config.max_total_read_bytes = read_limit;
            config.max_total_parse_bytes = parse_limit;
        });
        let mut open = job(store.snapshot(), upper_bound, constrained);
        let error = match open.poll(&store, &NeverCancelled) {
            TraditionalRevisionPoll::Failed(error) => error,
            other => panic!("one-less cumulative work must fail: {other:?}"),
        };
        assert_eq!(error.limit().map(|limit| limit.kind()), Some(expected_kind));
    }

    let constrained = limits(|config| {
        config.initial_section_bytes = 16;
        config.max_section_bytes = required_section_bytes - 1;
    });
    let mut open = job(store.snapshot(), upper_bound, constrained);
    let error = match open.poll(&store, &NeverCancelled) {
        TraditionalRevisionPoll::Failed(error) => error,
        other => panic!("one-less section window must fail: {other:?}"),
    };
    assert_eq!(
        error.limit().map(|limit| limit.kind()),
        Some(XrefLimitKind::SectionBytes)
    );

    let clipped_bound = STARTXREF + required_section_bytes - 1;
    let mut clipped = job(store.snapshot(), clipped_bound, XrefLimits::default());
    assert!(matches!(
        clipped.poll(&store, &NeverCancelled),
        TraditionalRevisionPoll::Failed(error)
            if matches!(error.code(), XrefErrorCode::InvalidTrailer | XrefErrorCode::SyntaxFailure)
    ));
    assert!(
        OpenTraditionalRevisionJob::new(
            store.snapshot(),
            STARTXREF,
            upper_bound + 1,
            context(),
            XrefLimits::default(),
            SyntaxLimits::default(),
        )
        .is_err()
    );
}

#[test]
fn completed_job_is_one_shot_and_preserves_complete_phase() {
    let bytes = canonical();
    let store = supplied_store(&bytes, identity(0x61));
    let mut open = job(
        store.snapshot(),
        u64::try_from(bytes.len()).unwrap(),
        XrefLimits::default(),
    );
    assert!(matches!(
        open.poll(&store, &NeverCancelled),
        TraditionalRevisionPoll::Ready(_)
    ));
    assert!(matches!(
        open.poll(&store, &NeverCancelled),
        TraditionalRevisionPoll::Failed(error)
            if error.code() == XrefErrorCode::JobAlreadyComplete
    ));
    assert_eq!(open.phase(), TraditionalRevisionPhase::Complete);
}

#[test]
fn source_subsection_and_declared_size_limits_accept_equality_and_reject_one_less() {
    let bytes = canonical();
    let source_len = u64::try_from(bytes.len()).unwrap();
    let upper_bound = source_len;
    let exact = limits(|config| {
        config.max_source_bytes = source_len;
        config.max_section_bytes = upper_bound - STARTXREF;
        config.max_subsections = 2;
        config.max_entries = 8;
    });
    let store = supplied_store(&bytes, identity(0x71));
    let mut exact_job = job(store.snapshot(), upper_bound, exact);
    assert!(matches!(
        exact_job.poll(&store, &NeverCancelled),
        TraditionalRevisionPoll::Ready(_)
    ));

    let source_tight = limits(|config| {
        config.max_source_bytes = source_len - 1;
        config.max_section_bytes = upper_bound - STARTXREF;
    });
    let error = OpenTraditionalRevisionJob::new(
        store.snapshot(),
        STARTXREF,
        upper_bound,
        context(),
        source_tight,
        SyntaxLimits::default(),
    )
    .unwrap_err();
    assert_eq!(
        error.limit().map(|limit| limit.kind()),
        Some(XrefLimitKind::SourceBytes)
    );

    let subsection_tight = limits(|config| config.max_subsections = 1);
    let mut subsection_job = job(store.snapshot(), upper_bound, subsection_tight);
    let error = match subsection_job.poll(&store, &NeverCancelled) {
        TraditionalRevisionPoll::Failed(error) => error,
        other => panic!("one-less subsection ceiling must fail: {other:?}"),
    };
    assert_eq!(
        error.limit().map(|limit| limit.kind()),
        Some(XrefLimitKind::Subsections)
    );

    let size_tight = limits(|config| config.max_entries = 7);
    let mut size_job = job(store.snapshot(), upper_bound, size_tight);
    let error = match size_job.poll(&store, &NeverCancelled) {
        TraditionalRevisionPoll::Failed(error) => error,
        other => panic!("one-less declared Size ceiling must fail: {other:?}"),
    };
    assert_eq!(
        error.limit().map(|limit| limit.kind()),
        Some(XrefLimitKind::Entries)
    );
}

#[test]
fn unknown_empty_and_invalid_anchor_shapes_fail_before_polling() {
    let bytes = canonical();
    let validator = SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x52; 32]);
    let unknown = SourceSnapshot::new(identity(0x81), None, validator);
    assert_eq!(
        OpenTraditionalRevisionJob::new(
            unknown,
            STARTXREF,
            STARTXREF + 1,
            context(),
            XrefLimits::default(),
            SyntaxLimits::default(),
        )
        .unwrap_err()
        .code(),
        XrefErrorCode::UnknownSourceLength
    );

    let empty = SourceSnapshot::new(identity(0x82), Some(0), validator);
    assert_eq!(
        OpenTraditionalRevisionJob::new(
            empty,
            0,
            1,
            context(),
            XrefLimits::default(),
            SyntaxLimits::default(),
        )
        .unwrap_err()
        .code(),
        XrefErrorCode::EmptySource
    );

    let known = snapshot(&bytes, identity(0x83));
    for (start, upper) in [
        (STARTXREF, STARTXREF),
        (STARTXREF + 1, STARTXREF),
        (STARTXREF, u64::try_from(bytes.len()).unwrap() + 1),
    ] {
        assert_eq!(
            OpenTraditionalRevisionJob::new(
                known,
                start,
                upper,
                context(),
                XrefLimits::default(),
                SyntaxLimits::default(),
            )
            .unwrap_err()
            .code(),
            XrefErrorCode::StartXrefOutOfBounds
        );
    }
}
