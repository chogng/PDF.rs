use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    LocalXrefJobContext, LocalXrefPhase, LocalXrefPoll, NeverCancelled, OpenLocalXrefJob,
    XrefError, XrefErrorCode, XrefJobContext, XrefLimitConfig, XrefLimitKind, XrefLimits,
    XrefRepairKind, XrefRepairLimitConfig, XrefRepairLimits,
};

const TAIL_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(501);
const STRICT_SECTION_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(502);
const REPAIR_SCAN_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(503);
const REPAIR_SECTION_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(504);

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

fn table() -> Vec<u8> {
    b"xref\n0 2\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
trailer\n<< /Size 2 /Root 1 0 R >>\n"
        .to_vec()
}

fn canonical_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n".to_vec();
    let startxref = bytes.len();
    bytes.extend_from_slice(&table());
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    bytes
}

fn declared_offset_pdf(delta: i64) -> Vec<u8> {
    let mut bytes = canonical_pdf();
    let marker = b"startxref\n29";
    let value = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("canonical fixture has startxref")
        + b"startxref\n".len();
    let declared = 29_i64.checked_add(delta).unwrap();
    assert!((10..=99).contains(&declared));
    bytes[value..value + 2].copy_from_slice(format!("{declared:02}").as_bytes());
    bytes
}

fn whitespace_pdf(edits: &[usize]) -> Vec<u8> {
    let mut bytes = canonical_pdf();
    let row = bytes
        .windows(b"0000000009 00000 n \n".len())
        .position(|window| window == b"0000000009 00000 n \n")
        .expect("fixture contains object-one row");
    for relative in edits {
        bytes[row + relative] = b'\t';
    }
    bytes
}

fn multi_row_repair_pdf() -> Vec<u8> {
    let mut bytes = declared_offset_pdf(-1);
    for row in [
        b"0000000000 65535 f \n".as_slice(),
        b"0000000009 00000 n \n",
    ] {
        let start = bytes
            .windows(row.len())
            .position(|window| window == row)
            .expect("fixture contains both xref rows");
        bytes[start + 10] = b'\t';
    }
    bytes
}

fn ambiguous_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n".to_vec();
    let first = bytes.len();
    bytes.extend_from_slice(&table());
    let second = bytes.len();
    bytes.extend_from_slice(&table());
    assert!(second - first < 1024);
    bytes.extend_from_slice(format!("startxref\n{}\n%%EOF\n", second - 1).as_bytes());
    bytes
}

fn xref_stream_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let startxref = bytes.len();
    bytes.extend_from_slice(
        b"7 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Length 0 >>\nstream\n\nendstream\nendobj\n",
    );
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    bytes
}

fn context() -> LocalXrefJobContext {
    LocalXrefJobContext::new(
        XrefJobContext::new(JobId::new(500), TAIL_CHECKPOINT, STRICT_SECTION_CHECKPOINT),
        REPAIR_SCAN_CHECKPOINT,
        REPAIR_SECTION_CHECKPOINT,
    )
}

fn store(bytes: &[u8], tag: u8, supplied: bool) -> RangeStore {
    let snapshot = snapshot(u64::try_from(bytes.len()).unwrap(), tag);
    let store = RangeStore::new(snapshot, Default::default()).unwrap();
    if supplied {
        let range = ByteRange::new(0, u64::try_from(bytes.len()).unwrap()).unwrap();
        store
            .supply(RangeResponse::new(snapshot, range, bytes.to_vec()).unwrap())
            .unwrap();
    }
    store
}

fn job(source: SourceSnapshot, repair_limits: XrefRepairLimits) -> OpenLocalXrefJob {
    OpenLocalXrefJob::new(
        source,
        context(),
        XrefLimits::default(),
        repair_limits,
        SyntaxLimits::default(),
    )
    .expect("local xref job configuration is valid")
}

fn ready(bytes: &[u8], limits: XrefRepairLimits) -> pdf_rs_xref::LocallyParsedXrefSection {
    let store = store(bytes, 0x81, true);
    let mut job = job(store.snapshot(), limits);
    match job.poll(&store, &NeverCancelled) {
        LocalXrefPoll::Ready(section) => section,
        LocalXrefPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
        LocalXrefPoll::Failed(error) => panic!("expected local xref success, got {error}"),
    }
}

fn failed(bytes: &[u8], limits: XrefRepairLimits) -> XrefError {
    let store = store(bytes, 0x82, true);
    let mut job = job(store.snapshot(), limits);
    let error = match job.poll(&store, &NeverCancelled) {
        LocalXrefPoll::Failed(error) => error,
        LocalXrefPoll::Ready(_) => panic!("expected local xref failure"),
        LocalXrefPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
    };
    assert!(matches!(
        job.poll(&store, &NeverCancelled),
        LocalXrefPoll::Failed(repeated) if repeated == error
    ));
    error
}

fn limits(update: impl FnOnce(&mut XrefRepairLimitConfig)) -> XrefRepairLimits {
    let mut config = XrefRepairLimitConfig::default();
    update(&mut config);
    XrefRepairLimits::validate(config).expect("test repair limits are valid")
}

#[test]
fn canonical_input_uses_only_the_strict_child_and_publishes_no_diagnostic() {
    let section = ready(&canonical_pdf(), XrefRepairLimits::default());
    assert_eq!(section.declared_startxref(), 29);
    assert_eq!(section.effective_startxref(), 29);
    assert!(section.diagnostics().is_empty());
    assert_eq!(section.entries().len(), 2);
    assert_eq!(section.root().number(), 1);
    assert_eq!(section.stats().repair_scan_bytes(), 0);
    assert_eq!(section.stats().candidate_section_attempts(), 0);
}

#[test]
fn bounded_whitespace_and_offset_repairs_revalidate_and_record_every_action() {
    let whitespace = ready(&whitespace_pdf(&[10]), XrefRepairLimits::default());
    assert_eq!(whitespace.diagnostics().len(), 1);
    let diagnostic = whitespace.diagnostics()[0];
    assert_eq!(diagnostic.kind(), XrefRepairKind::EntryWhitespace);
    assert_eq!(diagnostic.diagnostic_id(), "RPE-XREF-REPAIR-0002");
    assert_eq!(diagnostic.declared_startxref(), 29);
    assert_eq!(diagnostic.effective_startxref(), 29);
    assert_eq!(diagnostic.whitespace_edits(), 1);
    assert!(diagnostic.scan_bytes() > 0);

    let offset = ready(&declared_offset_pdf(-1), XrefRepairLimits::default());
    assert_eq!(offset.diagnostics().len(), 1);
    let diagnostic = offset.diagnostics()[0];
    assert_eq!(diagnostic.kind(), XrefRepairKind::StartXrefOffset);
    assert_eq!(diagnostic.diagnostic_id(), "RPE-XREF-REPAIR-0001");
    assert_eq!(diagnostic.declared_startxref(), 28);
    assert_eq!(diagnostic.effective_startxref(), 29);
    assert!(diagnostic.candidates_examined() >= 1);

    let mut combined = whitespace_pdf(&[10]);
    let value = combined
        .windows(b"startxref\n29".len())
        .position(|window| window == b"startxref\n29")
        .unwrap()
        + b"startxref\n".len();
    combined[value..value + 2].copy_from_slice(b"28");
    let combined = ready(&combined, XrefRepairLimits::default());
    assert_eq!(combined.diagnostics().len(), 2);
    assert_eq!(
        combined.diagnostics()[0].kind(),
        XrefRepairKind::StartXrefOffset
    );
    assert_eq!(
        combined.diagnostics()[1].kind(),
        XrefRepairKind::EntryWhitespace
    );
}

#[test]
fn semantic_row_damage_and_ambiguous_valid_anchors_are_never_repaired() {
    let mut invalid = canonical_pdf();
    let row = invalid
        .windows(b"0000000009 00000 n \n".len())
        .position(|window| window == b"0000000009 00000 n \n")
        .unwrap();
    invalid[row + 9] = b'x';
    assert_eq!(
        failed(&invalid, XrefRepairLimits::default()).code(),
        XrefErrorCode::LocalRepairFailed
    );

    assert_eq!(
        failed(&ambiguous_pdf(), XrefRepairLimits::default()).code(),
        XrefErrorCode::AmbiguousRepair
    );

    let mut illegal_whitespace = canonical_pdf();
    let row = illegal_whitespace
        .windows(b"0000000009 00000 n \n".len())
        .position(|window| window == b"0000000009 00000 n \n")
        .unwrap();
    illegal_whitespace[row + 10] = b'%';
    assert_eq!(
        failed(&illegal_whitespace, XrefRepairLimits::default()).code(),
        XrefErrorCode::LocalRepairFailed
    );
}

#[test]
fn unsupported_resource_and_cancelled_strict_failures_never_enter_repair() {
    let stream = xref_stream_pdf();
    let stream_store = store(&stream, 0x87, true);
    let mut unsupported = job(stream_store.snapshot(), XrefRepairLimits::default());
    assert!(matches!(
        unsupported.poll(&stream_store, &NeverCancelled),
        LocalXrefPoll::Failed(error) if error.code() == XrefErrorCode::UnsupportedXrefStream
    ));
    assert_eq!(unsupported.stats().repair_scan_bytes(), 0);

    let canonical = canonical_pdf();
    let canonical_store = store(&canonical, 0x88, true);
    let source_len = u64::try_from(canonical.len()).unwrap();
    let tiny_tail = XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: source_len,
        initial_tail_bytes: 4,
        max_tail_bytes: 4,
        initial_section_bytes: 1,
        max_section_bytes: 1,
        max_total_read_bytes: 5,
        max_total_parse_bytes: 5,
        max_subsections: 1,
        max_entries: 2,
    })
    .unwrap();
    let mut resource = OpenLocalXrefJob::new(
        canonical_store.snapshot(),
        context(),
        tiny_tail,
        XrefRepairLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        resource.poll(&canonical_store, &NeverCancelled),
        LocalXrefPoll::Failed(error)
            if error.code() == XrefErrorCode::ResourceLimit
                && error.limit().unwrap().kind() == XrefLimitKind::TailBytes
    ));
    assert_eq!(resource.stats().repair_scan_bytes(), 0);

    let cancelled = AtomicBool::new(true);
    let mut cancelled_job = job(canonical_store.snapshot(), XrefRepairLimits::default());
    assert!(matches!(
        cancelled_job.poll(&canonical_store, &cancelled),
        LocalXrefPoll::Failed(error) if error.code() == XrefErrorCode::Cancelled
    ));
    assert_eq!(cancelled_job.stats().repair_scan_bytes(), 0);
    cancelled.store(false, Ordering::Release);
    assert!(matches!(
        cancelled_job.poll(&canonical_store, &cancelled),
        LocalXrefPoll::Failed(error) if error.code() == XrefErrorCode::Cancelled
    ));
}

#[test]
fn candidate_validation_propagates_unsupported_and_resource_failures() {
    let mut incremental = declared_offset_pdf(-1);
    let trailer_end = incremental
        .windows(b" /Root 1 0 R >>".len())
        .position(|window| window == b" /Root 1 0 R >>")
        .expect("fixture has a trailer root")
        + b" /Root 1 0 R".len();
    incremental.splice(trailer_end..trailer_end, b" /Prev 1".iter().copied());

    let error = failed(&incremental, XrefRepairLimits::default());
    assert_eq!(error.code(), XrefErrorCode::UnsupportedIncrementalRevision);

    let constrained = declared_offset_pdf(-1);
    let source = store(&constrained, 0x8a, true);
    let source_len = u64::try_from(constrained.len()).unwrap();
    let xref_limits = XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: source_len,
        initial_tail_bytes: source_len,
        max_tail_bytes: source_len,
        initial_section_bytes: source_len,
        max_section_bytes: source_len,
        max_total_read_bytes: source_len * 2,
        max_total_parse_bytes: source_len * 2,
        max_subsections: 4,
        max_entries: 1,
    })
    .unwrap();
    let mut open = OpenLocalXrefJob::new(
        source.snapshot(),
        context(),
        xref_limits,
        XrefRepairLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        open.poll(&source, &NeverCancelled),
        LocalXrefPoll::Failed(error)
            if error.code() == XrefErrorCode::ResourceLimit
                && error.limit().unwrap().kind() == XrefLimitKind::Entries
    ));
    assert!(open.stats().repair_scan_bytes() > 0);
}

#[test]
fn growing_candidate_windows_do_not_recharge_the_same_whitespace_edit() {
    let bytes = whitespace_pdf(&[10]);
    let source = store(&bytes, 0x89, true);
    let source_len = u64::try_from(bytes.len()).unwrap();
    let xref_limits = XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: source_len,
        initial_tail_bytes: source_len,
        max_tail_bytes: source_len,
        initial_section_bytes: 64,
        max_section_bytes: 128,
        max_total_read_bytes: 1024,
        max_total_parse_bytes: 1024,
        max_subsections: 4,
        max_entries: 4,
    })
    .unwrap();
    let mut open = OpenLocalXrefJob::new(
        source.snapshot(),
        context(),
        xref_limits,
        limits(|config| config.max_whitespace_edits = 1),
        SyntaxLimits::default(),
    )
    .unwrap();
    let section = match open.poll(&source, &NeverCancelled) {
        LocalXrefPoll::Ready(section) => section,
        LocalXrefPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
        LocalXrefPoll::Failed(error) => panic!("growing repair failed: {error}"),
    };
    assert!(section.stats().candidate_section_attempts() > 1);
    assert_eq!(section.stats().whitespace_edits(), 1);
}

#[test]
fn repair_limits_are_exact_and_report_the_rejected_dimension() {
    let scan_fixture = declared_offset_pdf(-1);
    let baseline = ready(&scan_fixture, XrefRepairLimits::default());
    let exact_scan = baseline.stats().repair_scan_bytes();
    ready(
        &scan_fixture,
        limits(|config| config.max_scan_bytes = exact_scan),
    );
    let error = failed(
        &scan_fixture,
        limits(|config| config.max_scan_bytes = exact_scan - 1),
    );
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        XrefLimitKind::RepairScanBytes
    );

    let two_edits = whitespace_pdf(&[10, 16]);
    let error = failed(&two_edits, limits(|config| config.max_whitespace_edits = 1));
    assert_eq!(
        error.limit().unwrap().kind(),
        XrefLimitKind::RepairWhitespaceEdits
    );

    let multi_row = multi_row_repair_pdf();
    let baseline = ready(&multi_row, XrefRepairLimits::default());
    assert_eq!(baseline.diagnostics().len(), 3);
    let exact_working = baseline.stats().repair_working_bytes();
    assert!(exact_working > u64::try_from(multi_row.len()).unwrap() / 2);
    ready(
        &multi_row,
        limits(|config| config.max_working_bytes = exact_working),
    );
    let error = failed(
        &multi_row,
        limits(|config| config.max_working_bytes = exact_working - 1),
    );
    assert_eq!(
        error.limit().unwrap().kind(),
        XrefLimitKind::RepairWorkingBytes
    );

    let exact_diagnostic_bytes = baseline.stats().diagnostic_bytes();
    ready(
        &multi_row,
        limits(|config| config.max_diagnostic_bytes = exact_diagnostic_bytes),
    );
    let error = failed(
        &multi_row,
        limits(|config| config.max_diagnostic_bytes = exact_diagnostic_bytes - 1),
    );
    assert_eq!(
        error.limit().unwrap().kind(),
        XrefLimitKind::RepairDiagnosticBytes
    );

    let error = failed(&ambiguous_pdf(), limits(|config| config.max_candidates = 1));
    assert_eq!(
        error.limit().unwrap().kind(),
        XrefLimitKind::RepairCandidates
    );
    assert_eq!(
        failed(&ambiguous_pdf(), limits(|config| config.max_repairs = 1)).code(),
        XrefErrorCode::AmbiguousRepair
    );

    let mut combined = whitespace_pdf(&[10]);
    let value = combined
        .windows(b"startxref\n29".len())
        .position(|window| window == b"startxref\n29")
        .unwrap()
        + b"startxref\n".len();
    combined[value..value + 2].copy_from_slice(b"28");
    let error = failed(&combined, limits(|config| config.max_repairs = 1));
    assert_eq!(
        error.limit().unwrap().kind(),
        XrefLimitKind::RepairDiagnostics
    );
}

#[test]
fn repair_anchor_delta_accepts_equality_and_rejects_one_byte_beyond() {
    let two_away = declared_offset_pdf(-2);
    let section = ready(&two_away, limits(|config| config.max_startxref_delta = 2));
    assert_eq!(section.effective_startxref(), 29);
    assert_eq!(section.declared_startxref(), 27);

    assert_eq!(
        failed(&two_away, limits(|config| config.max_startxref_delta = 1)).code(),
        XrefErrorCode::LocalRepairFailed
    );
}

#[test]
fn repair_checkpoint_resumes_without_recharging_and_snapshot_change_is_terminal() {
    let bytes = declared_offset_pdf(-1);
    let range_store = store(&bytes, 0x83, false);
    let source_len = u64::try_from(bytes.len()).unwrap();
    let xref_limits = XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: source_len,
        initial_tail_bytes: 24,
        max_tail_bytes: 32,
        initial_section_bytes: 16,
        max_section_bytes: 128,
        max_total_read_bytes: 512,
        max_total_parse_bytes: 512,
        max_subsections: 4,
        max_entries: 4,
    })
    .unwrap();
    let mut open = OpenLocalXrefJob::new(
        range_store.snapshot(),
        context(),
        xref_limits,
        XrefRepairLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let mut observed_repair_pending = false;
    loop {
        match open.poll(&range_store, &NeverCancelled) {
            LocalXrefPoll::Pending {
                missing,
                checkpoint,
                ..
            } => {
                if checkpoint == REPAIR_SCAN_CHECKPOINT {
                    observed_repair_pending = true;
                    let charged = open.stats().repair_scan_bytes();
                    assert!(matches!(
                        open.poll(&range_store, &NeverCancelled),
                        LocalXrefPoll::Pending { checkpoint: repeated, .. }
                            if repeated == checkpoint
                    ));
                    assert_eq!(open.stats().repair_scan_bytes(), charged);
                }
                for range in missing.as_slice() {
                    let start = usize::try_from(range.start()).unwrap();
                    let end = usize::try_from(range.end_exclusive()).unwrap();
                    range_store
                        .supply(
                            RangeResponse::new(
                                range_store.snapshot(),
                                *range,
                                bytes[start..end].to_vec(),
                            )
                            .unwrap(),
                        )
                        .unwrap();
                }
            }
            LocalXrefPoll::Ready(section) => {
                assert!(observed_repair_pending);
                assert_eq!(section.effective_startxref(), 29);
                break;
            }
            LocalXrefPoll::Failed(error) => panic!("repair resume failed: {error}"),
        }
    }

    let foreign = store(&bytes, 0x84, true);
    let mut changed = job(
        snapshot(u64::try_from(bytes.len()).unwrap(), 0x85),
        XrefRepairLimits::default(),
    );
    assert!(matches!(
        changed.poll(&foreign, &AtomicBool::new(false)),
        LocalXrefPoll::Failed(error) if error.code() == XrefErrorCode::SnapshotMismatch
    ));
    assert_eq!(changed.phase(), LocalXrefPhase::Failed);
}

#[test]
fn configuration_rejects_zero_hard_ceiling_overrides_and_duplicate_checkpoints() {
    const HARD: XrefRepairLimitConfig = XrefRepairLimitConfig {
        max_startxref_delta: 64 * 1024,
        max_scan_bytes: 64 * 1024 * 1024,
        max_working_bytes: 64 * 1024 * 1024,
        max_candidates: 256,
        max_whitespace_edits: 4096,
        max_repairs: 4096,
        max_diagnostic_bytes: 1024 * 1024,
    };
    assert!(XrefRepairLimits::validate(HARD).is_ok());
    let mutations: [fn(&mut XrefRepairLimitConfig); 7] = [
        |config| config.max_startxref_delta = 0,
        |config| config.max_scan_bytes = 0,
        |config| config.max_working_bytes = 0,
        |config| config.max_candidates = 0,
        |config| config.max_whitespace_edits = 0,
        |config| config.max_repairs = 0,
        |config| config.max_diagnostic_bytes = 0,
    ];
    for mutation in mutations {
        let mut config = XrefRepairLimitConfig::default();
        mutation(&mut config);
        assert_eq!(
            XrefRepairLimits::validate(config).unwrap_err().code(),
            XrefErrorCode::InvalidRepairLimits
        );
    }
    let over = [
        XrefRepairLimitConfig {
            max_startxref_delta: HARD.max_startxref_delta + 1,
            ..HARD
        },
        XrefRepairLimitConfig {
            max_scan_bytes: HARD.max_scan_bytes + 1,
            ..HARD
        },
        XrefRepairLimitConfig {
            max_working_bytes: HARD.max_working_bytes + 1,
            ..HARD
        },
        XrefRepairLimitConfig {
            max_candidates: HARD.max_candidates + 1,
            ..HARD
        },
        XrefRepairLimitConfig {
            max_whitespace_edits: HARD.max_whitespace_edits + 1,
            ..HARD
        },
        XrefRepairLimitConfig {
            max_repairs: HARD.max_repairs + 1,
            ..HARD
        },
        XrefRepairLimitConfig {
            max_diagnostic_bytes: HARD.max_diagnostic_bytes + 1,
            ..HARD
        },
    ];
    for config in over {
        assert_eq!(
            XrefRepairLimits::validate(config).unwrap_err().code(),
            XrefErrorCode::InvalidRepairLimits
        );
    }

    let bytes = canonical_pdf();
    let source = snapshot(u64::try_from(bytes.len()).unwrap(), 0x86);
    let invalid = LocalXrefJobContext::new(
        XrefJobContext::new(JobId::new(500), TAIL_CHECKPOINT, STRICT_SECTION_CHECKPOINT),
        STRICT_SECTION_CHECKPOINT,
        REPAIR_SECTION_CHECKPOINT,
    );
    assert_eq!(
        OpenLocalXrefJob::new(
            source,
            invalid,
            XrefLimits::default(),
            XrefRepairLimits::default(),
            SyntaxLimits::default(),
        )
        .unwrap_err()
        .code(),
        XrefErrorCode::InvalidRepairJobContext
    );
}
