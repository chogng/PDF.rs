use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    NeverCancelled, OpenXrefJob, XrefError, XrefErrorCode, XrefJobContext, XrefLimitConfig,
    XrefLimitKind, XrefLimits, XrefPhase, XrefPoll, XrefSection,
};

const PDF_LEN: u64 = 612;
const XREF_OFFSET: u64 = 449;
const TRAILER_START: u64 = 566;
const TRAILER_END: u64 = 591;

fn identity() -> SourceIdentity {
    SourceIdentity::new(SourceStableId::new([0x78; 32]), SourceRevision::new(11))
}

fn snapshot(len: Option<u64>) -> SourceSnapshot {
    SourceSnapshot::new(
        identity(),
        len,
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x42; 32]),
    )
}

// This is a project-authored structural fixture, not a checked-in generator
// artifact. One padded PDF comment deliberately reproduces the canonical M0
// generator's byte geometry without copying its generated metadata or digest.
fn canonical_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n%".to_vec();
    pdf.resize(185, b'x');
    pdf.push(b'\n');
    assert_eq!(pdf.len(), 186);

    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    assert_eq!(pdf.len(), 235);
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    assert_eq!(pdf.len(), 292);
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>\nendobj\n",
    );
    assert_eq!(pdf.len(), 396);
    pdf.extend_from_slice(b"4 0 obj\n<< /Length 4 >>\nstream\nq\nQ\n\nendstream\nendobj\n");
    assert_eq!(pdf.len(), usize::try_from(XREF_OFFSET).unwrap());
    pdf.extend_from_slice(
        b"xref\n0 5\n\
0000000000 65535 f \n\
0000000186 00000 n \n\
0000000235 00000 n \n\
0000000292 00000 n \n\
0000000396 00000 n \n\
trailer\n\
<< /Size 5 /Root 1 0 R >>\n\
startxref\n449\n%%EOF\n",
    );
    assert_eq!(pdf.len(), usize::try_from(PDF_LEN).unwrap());
    pdf
}

const ENTRY_HEADS: [&[u8]; 5] = [
    b"0000000000 65535 f",
    b"0000000186 00000 n",
    b"0000000235 00000 n",
    b"0000000292 00000 n",
    b"0000000396 00000 n",
];

fn table_pdf(subsections: &[(usize, usize)], row_ending: &[u8], trailer: &[u8]) -> Vec<u8> {
    assert_eq!(
        row_ending.len(),
        2,
        "xref rows must remain exactly 20 bytes"
    );
    let mut pdf = canonical_pdf();
    pdf.truncate(usize::try_from(XREF_OFFSET).unwrap());
    pdf.extend_from_slice(b"xref\n");
    for &(first, count) in subsections {
        pdf.extend_from_slice(format!("{first} {count}\n").as_bytes());
        for entry_head in ENTRY_HEADS.iter().skip(first).take(count) {
            pdf.extend_from_slice(entry_head);
            pdf.extend_from_slice(row_ending);
        }
    }
    pdf.extend_from_slice(b"trailer\n");
    pdf.extend_from_slice(trailer);
    pdf.extend_from_slice(b"\nstartxref\n449\n%%EOF\n");
    pdf
}

fn indirect_target_pdf(body: &[u8]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let offset = pdf.len();
    pdf.extend_from_slice(body);
    pdf.extend_from_slice(format!("startxref\n{offset}\n%%EOF\n").as_bytes());
    pdf
}

fn job(snapshot: SourceSnapshot, limits: XrefLimits) -> OpenXrefJob {
    OpenXrefJob::new(
        snapshot,
        XrefJobContext::new(
            JobId::new(31),
            ResumeCheckpoint::new(70),
            ResumeCheckpoint::new(71),
        ),
        limits,
        SyntaxLimits::default(),
    )
    .expect("test xref job is valid")
}

fn supplied_store(bytes: &[u8]) -> RangeStore {
    let source = snapshot(Some(u64::try_from(bytes.len()).unwrap()));
    let store = RangeStore::new(source, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(source, range, bytes.to_vec()).unwrap())
        .unwrap();
    store
}

fn ready_with_limits(bytes: &[u8], limits: XrefLimits) -> XrefSection {
    let store = supplied_store(bytes);
    let mut open = job(store.snapshot(), limits);
    match open.poll(&store, &NeverCancelled) {
        XrefPoll::Ready(section) => section,
        XrefPoll::Pending { .. } => panic!("a completely supplied source must not remain pending"),
        XrefPoll::Failed(error) => panic!("expected a ready xref, got {error}"),
    }
}

fn failed(bytes: &[u8], limits: XrefLimits) -> XrefError {
    let store = supplied_store(bytes);
    let mut open = job(store.snapshot(), limits);
    match open.poll(&store, &NeverCancelled) {
        XrefPoll::Failed(error) => error,
        XrefPoll::Ready(_) => panic!("expected xref failure, got a ready section"),
        XrefPoll::Pending { .. } => panic!("a completely supplied source must not remain pending"),
    }
}

fn compact_limits(update: impl FnOnce(&mut XrefLimitConfig)) -> XrefLimits {
    let mut config = XrefLimitConfig {
        max_source_bytes: PDF_LEN,
        initial_tail_bytes: 21,
        max_tail_bytes: 21,
        initial_section_bytes: 142,
        max_section_bytes: 142,
        max_total_read_bytes: 163,
        max_total_parse_bytes: 163,
        max_subsections: 1,
        max_entries: 5,
    };
    update(&mut config);
    XrefLimits::validate(config).expect("test limits are internally consistent")
}

fn growing_window_limits(max_total_read_bytes: u64, max_total_parse_bytes: u64) -> XrefLimits {
    XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: PDF_LEN,
        initial_tail_bytes: 20,
        max_tail_bytes: 21,
        initial_section_bytes: 142,
        max_section_bytes: 142,
        max_total_read_bytes,
        max_total_parse_bytes,
        max_subsections: 1,
        max_entries: 5,
    })
    .expect("growing-window test limits are internally consistent")
}

#[test]
fn canonical_geometry_yields_trailer_root_and_all_entries() {
    let section = ready_with_limits(&canonical_pdf(), compact_limits(|_| {}));
    assert_eq!(section.source(), identity());
    assert_eq!(section.snapshot(), snapshot(Some(PDF_LEN)));
    assert_eq!(section.startxref(), XREF_OFFSET);
    assert_eq!(section.span().start(), XREF_OFFSET);
    assert_eq!(section.span().end_exclusive(), TRAILER_END);
    assert_eq!(section.declared_size(), 5);
    assert_eq!(
        (section.root().number(), section.root().generation()),
        (1, 0)
    );
    assert_eq!(section.entries().len(), 5);
    assert_eq!(section.trailer().source(), identity());
    assert_eq!(section.trailer().span().start(), TRAILER_START);
    assert_eq!(section.trailer().span().end_exclusive(), TRAILER_END);

    let expected = [
        (0, 65_535, true, 0),
        (1, 0, false, 186),
        (2, 0, false, 235),
        (3, 0, false, 292),
        (4, 0, false, 396),
    ];
    for (number, generation, is_free, offset) in expected {
        let entry = section.entry(number).expect("canonical object is indexed");
        assert_eq!(entry.object_number(), number);
        assert_eq!(entry.generation(), generation);
        match entry.kind() {
            pdf_rs_xref::XrefEntryKind::Free { next_free } => {
                assert!(is_free);
                assert_eq!(next_free, offset);
            }
            pdf_rs_xref::XrefEntryKind::InUse { offset: actual } => {
                assert!(!is_free);
                assert_eq!(actual, u64::from(offset));
            }
        }
    }
}

#[test]
fn pending_tail_read_wakes_only_after_partial_supplies_cover_the_ticket() {
    let pdf = canonical_pdf();
    let source = snapshot(Some(PDF_LEN));
    let store = RangeStore::new(source, Default::default()).unwrap();
    let mut open = job(source, compact_limits(|_| {}));

    let (ticket, requested, checkpoint) = match open.poll(&store, &NeverCancelled) {
        XrefPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(missing.len(), 1);
            (ticket, missing.as_slice()[0], checkpoint)
        }
        _ => panic!("an empty RangeStore must suspend the xref job"),
    };
    assert_eq!(checkpoint.value(), 70);
    assert_eq!(requested.start(), PDF_LEN - 21);
    assert_eq!(requested.len(), 21);
    for _ in 0..3 {
        match open.poll(&store, &NeverCancelled) {
            XrefPoll::Pending {
                ticket: repeated,
                missing,
                checkpoint,
            } => {
                assert_eq!(repeated, ticket);
                assert_eq!(missing.as_slice(), &[requested]);
                assert_eq!(checkpoint.value(), 70);
            }
            _ => panic!("re-polling absent tail bytes must preserve the suspension"),
        }
    }

    let first = ByteRange::new(requested.start(), 7).unwrap();
    let first_start = usize::try_from(first.start()).unwrap();
    let first_end = usize::try_from(first.end_exclusive()).unwrap();
    let outcome = store
        .supply(RangeResponse::new(source, first, pdf[first_start..first_end].to_vec()).unwrap())
        .unwrap();
    assert!(outcome.ready_tickets().is_empty());

    let second = ByteRange::new(first.end_exclusive(), requested.len() - first.len()).unwrap();
    let second_start = usize::try_from(second.start()).unwrap();
    let second_end = usize::try_from(second.end_exclusive()).unwrap();
    let outcome = store
        .supply(RangeResponse::new(source, second, pdf[second_start..second_end].to_vec()).unwrap())
        .unwrap();
    assert_eq!(outcome.ready_tickets(), &[ticket]);

    let (section_ticket, section_range) = match open.poll(&store, &NeverCancelled) {
        XrefPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(missing.len(), 1);
            assert_eq!(checkpoint.value(), 71);
            (ticket, missing.as_slice()[0])
        }
        _ => panic!("the separated xref window must cause a second suspension"),
    };
    assert_eq!(section_range.start(), XREF_OFFSET);
    assert_eq!(section_range.len(), 142);
    for _ in 0..3 {
        match open.poll(&store, &NeverCancelled) {
            XrefPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                assert_eq!(ticket, section_ticket);
                assert_eq!(missing.as_slice(), &[section_range]);
                assert_eq!(checkpoint.value(), 71);
            }
            _ => panic!("re-polling absent section bytes must preserve the suspension"),
        }
    }
    let start = usize::try_from(section_range.start()).unwrap();
    let end = usize::try_from(section_range.end_exclusive()).unwrap();
    let outcome = store
        .supply(RangeResponse::new(source, section_range, pdf[start..end].to_vec()).unwrap())
        .unwrap();
    assert_eq!(outcome.ready_tickets(), &[section_ticket]);

    let section = match open.poll(&store, &NeverCancelled) {
        XrefPoll::Ready(section) => section,
        _ => panic!("the same job must resume to completion after both windows arrive"),
    };
    assert_eq!(section.startxref(), XREF_OFFSET);
    assert_eq!(section.entries().len(), 5);
}

#[test]
fn equality_limits_accept_the_fixture_and_one_less_is_structured() {
    ready_with_limits(&canonical_pdf(), compact_limits(|_| {}));

    let source = snapshot(Some(PDF_LEN));
    let limits = compact_limits(|config| config.max_source_bytes = PDF_LEN - 1);
    let error = OpenXrefJob::new(
        source,
        XrefJobContext::new(
            JobId::new(1),
            ResumeCheckpoint::new(1),
            ResumeCheckpoint::new(2),
        ),
        limits,
        SyntaxLimits::default(),
    )
    .expect_err("source length above the configured maximum is rejected");
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    let limit = error.limit().expect("resource errors carry limit context");
    assert_eq!(limit.kind(), XrefLimitKind::SourceBytes);
    assert_eq!((limit.limit(), limit.attempted()), (PDF_LEN - 1, PDF_LEN));

    let error = failed(
        &canonical_pdf(),
        compact_limits(|config| config.max_entries = 4),
    );
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    let limit = error.limit().expect("entry overflow carries limit context");
    assert_eq!(limit.kind(), XrefLimitKind::Entries);
    assert_eq!((limit.limit(), limit.attempted()), (4, 5));

    let error = failed(
        &canonical_pdf(),
        compact_limits(|config| {
            config.initial_section_bytes = 141;
            config.max_section_bytes = 141;
        }),
    );
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().expect("section budget context").kind(),
        XrefLimitKind::SectionBytes
    );
}

#[test]
fn geometric_ready_window_retries_are_charged_cumulatively() {
    let pdf = canonical_pdf();
    ready_with_limits(&pdf, growing_window_limits(183, 183));

    let error = failed(&pdf, growing_window_limits(182, 183));
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().expect("read budget context").kind(),
        XrefLimitKind::TotalReadBytes
    );

    let error = failed(&pdf, growing_window_limits(183, 182));
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().expect("parse budget context").kind(),
        XrefLimitKind::TotalParseBytes
    );
}

#[test]
fn malformed_and_logically_truncated_rows_fail_at_stable_offsets() {
    let mut bad_digit = canonical_pdf();
    bad_digit[478 + 9] = b'x';
    let error = failed(&bad_digit, compact_limits(|_| {}));
    assert_eq!(error.code(), XrefErrorCode::InvalidEntry);
    assert_eq!(error.offset(), Some(478));

    let mut bad_status = canonical_pdf();
    bad_status[478 + 17] = b'x';
    assert_eq!(
        failed(&bad_status, compact_limits(|_| {})).code(),
        XrefErrorCode::InvalidEntry
    );

    let mut bad_generation = canonical_pdf();
    bad_generation[478 + 11..478 + 16].copy_from_slice(b"65536");
    assert_eq!(
        failed(&bad_generation, compact_limits(|_| {})).code(),
        XrefErrorCode::InvalidEntry
    );

    let mut truncated_subsection = canonical_pdf();
    truncated_subsection[456] = b'6';
    let error = failed(
        &truncated_subsection,
        compact_limits(|config| config.max_entries = 6),
    );
    assert_eq!(error.code(), XrefErrorCode::InvalidEntry);

    let mut final_truncation = canonical_pdf();
    final_truncation.truncate(606);
    assert_eq!(
        failed(&final_truncation, compact_limits(|_| {})).code(),
        XrefErrorCode::InvalidStartXref
    );
}

#[test]
fn strict_open_never_repairs_local_xref_whitespace_or_startxref_offsets() {
    let mut noncanonical_whitespace = canonical_pdf();
    noncanonical_whitespace[478 + 10] = b'\t';
    let whitespace_error = failed(&noncanonical_whitespace, compact_limits(|_| {}));
    assert_eq!(whitespace_error.code(), XrefErrorCode::InvalidEntry);
    assert_eq!(whitespace_error.offset(), Some(478));

    let mut nearby_startxref = canonical_pdf();
    let tail_value = nearby_startxref
        .windows(b"startxref\n449".len())
        .position(|window| window == b"startxref\n449")
        .expect("canonical fixture contains its final startxref value")
        + b"startxref\n".len();
    nearby_startxref[tail_value..tail_value + 3].copy_from_slice(b"448");
    assert_eq!(&nearby_startxref[449..453], b"xref");

    let offset_error = failed(&nearby_startxref, compact_limits(|_| {}));
    assert_eq!(offset_error.code(), XrefErrorCode::InvalidXrefKeyword);
    assert_eq!(offset_error.offset(), Some(448));
}

#[test]
fn final_startxref_requires_line_boundaries() {
    let mut pdf = canonical_pdf();
    let tail_start = pdf
        .windows(b"startxref".len())
        .rposition(|window| window == b"startxref")
        .expect("canonical tail contains startxref");
    pdf.truncate(tail_start);
    pdf.extend_from_slice(b"startxref 449 %%EOF\n");

    assert_eq!(
        failed(&pdf, XrefLimits::default()).code(),
        XrefErrorCode::InvalidStartXref
    );
}

#[test]
fn trailer_size_must_match_the_complete_base_table() {
    let pdf = table_pdf(&[(0, 5)], b" \n", b"<< /Size 6 /Root 1 0 R >>");

    assert_eq!(
        failed(&pdf, XrefLimits::default()).code(),
        XrefErrorCode::InvalidEntry
    );
}

#[test]
fn incomplete_xref_stream_shapes_are_invalid_xref_targets() {
    let invalid_targets: &[&[u8]] = &[
        b"7 0 obj\n<< /Type /XRef /W [1 2 1] /Length 0 >>\nstream\n\n",
        b"7 0 obj\n<< /Type /XRef /Size 1 /W [1 2] /Length 0 >>\nstream\n\n",
        b"7 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Length -1 >>\nstream\n\n",
        b"7 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Length 0 >>\nendobj\n",
    ];

    for body in invalid_targets {
        let pdf = indirect_target_pdf(body);
        assert_eq!(
            failed(&pdf, XrefLimits::default()).code(),
            XrefErrorCode::InvalidXrefKeyword,
            "invalid stream-shaped target: {}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn contiguous_subsections_form_one_complete_base_table() {
    let pdf = table_pdf(&[(0, 3), (3, 2)], b" \n", b"<< /Size 5 /Root 1 0 R >>");
    let section = ready_with_limits(&pdf, XrefLimits::default());

    assert_eq!(section.entries().len(), 5);
    assert_eq!(section.entry(0).unwrap().object_number(), 0);
    assert_eq!(section.entry(4).unwrap().object_number(), 4);
}

#[test]
fn overlapping_and_out_of_order_subsections_are_rejected() {
    for subsections in [&[(0, 3), (2, 3)][..], &[(3, 2), (0, 3)][..]] {
        let pdf = table_pdf(subsections, b" \n", b"<< /Size 5 /Root 1 0 R >>");
        assert_eq!(
            failed(&pdf, XrefLimits::default()).code(),
            XrefErrorCode::InvalidSubsection
        );
    }
}

#[test]
fn crlf_and_cr_fixed_width_entry_endings_are_accepted() {
    for row_ending in [&b"\r\n"[..], &b" \r"[..]] {
        let pdf = table_pdf(&[(0, 5)], row_ending, b"<< /Size 5 /Root 1 0 R >>");
        let section = ready_with_limits(&pdf, XrefLimits::default());
        assert_eq!(section.entries().len(), 5);
    }
}

#[test]
fn backward_prev_and_in_range_xrefstm_are_explicitly_unsupported() {
    let previous = table_pdf(&[(0, 5)], b" \n", b"<< /Size 5 /Root 1 0 R /Prev 10 >>");
    assert_eq!(
        failed(&previous, XrefLimits::default()).code(),
        XrefErrorCode::UnsupportedIncrementalRevision
    );

    let hybrid = table_pdf(&[(0, 5)], b" \n", b"<< /Size 5 /Root 1 0 R /XRefStm 100 >>");
    assert_eq!(
        failed(&hybrid, XrefLimits::default()).code(),
        XrefErrorCode::UnsupportedHybridXref
    );
}

#[test]
fn tail_and_subsection_caps_return_their_exact_limit_kind() {
    let tail_limits = compact_limits(|config| {
        config.initial_tail_bytes = 20;
        config.max_tail_bytes = 20;
    });
    let error = failed(&canonical_pdf(), tail_limits);
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    let limit = error.limit().expect("tail exhaustion carries context");
    assert_eq!(limit.kind(), XrefLimitKind::TailBytes);
    assert_eq!(
        (limit.limit(), limit.consumed(), limit.attempted()),
        (20, 20, 1)
    );

    let pdf = table_pdf(&[(0, 3), (3, 2)], b" \n", b"<< /Size 5 /Root 1 0 R >>");
    let subsection_limits = XrefLimits::validate(XrefLimitConfig {
        max_subsections: 1,
        ..XrefLimitConfig::default()
    })
    .unwrap();
    let error = failed(&pdf, subsection_limits);
    assert_eq!(error.code(), XrefErrorCode::ResourceLimit);
    let limit = error
        .limit()
        .expect("subsection exhaustion carries context");
    assert_eq!(limit.kind(), XrefLimitKind::Subsections);
    assert_eq!(
        (limit.limit(), limit.consumed(), limit.attempted()),
        (1, 1, 1)
    );
}

#[test]
fn xref_streams_are_explicitly_unsupported() {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let offset = pdf.len();
    pdf.extend_from_slice(
        b"7 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Length 0 >>\nstream\n\nendstream\nendobj\n",
    );
    pdf.extend_from_slice(format!("startxref\n{offset}\n%%EOF\n").as_bytes());

    assert_eq!(
        failed(&pdf, XrefLimits::default()).code(),
        XrefErrorCode::UnsupportedXrefStream
    );

    let source_len = u64::try_from(pdf.len()).unwrap();
    let growing_probe = XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: source_len,
        initial_tail_bytes: source_len,
        max_tail_bytes: source_len,
        initial_section_bytes: 1,
        max_section_bytes: source_len,
        max_total_read_bytes: source_len * 4,
        max_total_parse_bytes: source_len * 4,
        max_subsections: 1,
        max_entries: 1,
    })
    .unwrap();
    assert_eq!(
        failed(&pdf, growing_probe).code(),
        XrefErrorCode::UnsupportedXrefStream
    );
}

#[test]
fn ordinary_indirect_object_at_startxref_is_not_misclassified_as_xref_stream() {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let offset = pdf.len();
    pdf.extend_from_slice(b"7 0 obj\n<< /Type /Catalog >>\nendobj\n");
    pdf.extend_from_slice(format!("startxref\n{offset}\n%%EOF\n").as_bytes());

    assert_eq!(
        failed(&pdf, XrefLimits::default()).code(),
        XrefErrorCode::InvalidXrefKeyword
    );
}

#[test]
fn forward_prev_pointer_is_an_invalid_trailer_not_an_incremental_revision() {
    let mut pdf = canonical_pdf();
    pdf.truncate(566);
    pdf.extend_from_slice(b"<< /Size 5 /Root 1 0 R /Prev 500 >>\nstartxref\n449\n%%EOF\n");
    assert!(u64::try_from(pdf.len()).unwrap() > 500);

    assert_eq!(
        failed(&pdf, XrefLimits::default()).code(),
        XrefErrorCode::InvalidTrailer
    );

    let mut hybrid = canonical_pdf();
    hybrid.truncate(566);
    hybrid.extend_from_slice(b"<< /Size 5 /Root 1 0 R /XRefStm 500 >>\nstartxref\n449\n%%EOF\n");
    assert!(u64::try_from(hybrid.len()).unwrap() > 500);
    assert_eq!(
        failed(&hybrid, XrefLimits::default()).code(),
        XrefErrorCode::InvalidTrailer
    );
}

#[test]
fn clipped_suffix_does_not_accept_startxref_inside_a_longer_word() {
    let pdf = b"%PDF-1.7\nnotstartxref\n9\n%%EOF\n";
    let marker_start = pdf
        .windows(b"startxref".len())
        .position(|window| window == b"startxref")
        .unwrap();
    let source_len = u64::try_from(pdf.len()).unwrap();
    let initial_tail = source_len - u64::try_from(marker_start).unwrap();
    let limits = XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: source_len,
        initial_tail_bytes: initial_tail,
        max_tail_bytes: source_len,
        initial_section_bytes: 1,
        max_section_bytes: 1,
        max_total_read_bytes: source_len * 3,
        max_total_parse_bytes: source_len * 3,
        max_subsections: 1,
        max_entries: 1,
    })
    .unwrap();

    assert_eq!(failed(pdf, limits).code(), XrefErrorCode::InvalidStartXref);
}

#[test]
fn section_debug_redacts_trailer_values() {
    let mut pdf = canonical_pdf();
    pdf.truncate(566);
    pdf.extend_from_slice(
        b"<< /Size 5 /Root 1 0 R /Secret (xref-secret-needle) >>\nstartxref\n449\n%%EOF\n",
    );
    let section = ready_with_limits(&pdf, XrefLimits::default());
    let debug = format!("{section:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("xref-secret-needle"));
}

#[test]
fn job_context_and_source_shape_are_validated_before_polling() {
    let equal = ResumeCheckpoint::new(8);
    let error = OpenXrefJob::new(
        snapshot(Some(PDF_LEN)),
        XrefJobContext::new(JobId::new(1), equal, equal),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .expect_err("phase checkpoints must be distinct");
    assert_eq!(error.code(), XrefErrorCode::InvalidJobContext);

    let context = XrefJobContext::new(
        JobId::new(2),
        ResumeCheckpoint::new(9),
        ResumeCheckpoint::new(10),
    );
    let error = OpenXrefJob::new(
        snapshot(None),
        context,
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .expect_err("tail seeking requires a known source length");
    assert_eq!(error.code(), XrefErrorCode::UnknownSourceLength);

    let error = OpenXrefJob::new(
        snapshot(Some(0)),
        context,
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .expect_err("an empty source cannot contain an xref");
    assert_eq!(error.code(), XrefErrorCode::EmptySource);
    assert_eq!(error.offset(), Some(0));
}

#[test]
fn cancellation_and_snapshot_mismatch_are_terminal_job_failures() {
    let pdf = canonical_pdf();
    let store = supplied_store(&pdf);
    let mut cancelled_job = job(store.snapshot(), compact_limits(|_| {}));
    let cancellation = AtomicBool::new(false);
    cancellation.store(true, Ordering::Release);
    match cancelled_job.poll(&store, &cancellation) {
        XrefPoll::Failed(error) => assert_eq!(error.code(), XrefErrorCode::Cancelled),
        _ => panic!("a pre-cancelled job must fail before reading"),
    }
    assert_eq!(cancelled_job.phase(), XrefPhase::Failed);

    let foreign_identity =
        SourceIdentity::new(SourceStableId::new([0x99; 32]), SourceRevision::new(12));
    let foreign = SourceSnapshot::new(
        foreign_identity,
        Some(PDF_LEN),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x24; 32]),
    );
    let foreign_store = RangeStore::new(foreign, Default::default()).unwrap();
    let mut mismatched_job = job(snapshot(Some(PDF_LEN)), compact_limits(|_| {}));
    match mismatched_job.poll(&foreign_store, &NeverCancelled) {
        XrefPoll::Failed(error) => assert_eq!(error.code(), XrefErrorCode::SnapshotMismatch),
        _ => panic!("a job cannot poll a different immutable source"),
    }
    assert_eq!(mismatched_job.phase(), XrefPhase::Failed);
}

#[test]
fn completed_job_is_one_shot_without_losing_complete_phase() {
    let pdf = canonical_pdf();
    let store = supplied_store(&pdf);
    let mut open = job(store.snapshot(), compact_limits(|_| {}));
    assert!(matches!(
        open.poll(&store, &NeverCancelled),
        XrefPoll::Ready(_)
    ));
    assert_eq!(open.phase(), XrefPhase::Complete);

    match open.poll(&store, &NeverCancelled) {
        XrefPoll::Failed(error) => assert_eq!(error.code(), XrefErrorCode::JobAlreadyComplete),
        _ => panic!("a completed one-shot job cannot return its section twice"),
    }
    assert_eq!(open.phase(), XrefPhase::Complete);
}
