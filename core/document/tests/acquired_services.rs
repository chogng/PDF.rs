use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    ResumeCheckpoint, SmallRanges, SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId,
    SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AcquiredObjectJobContext, AcquiredObjectPoll, AcquiredOutline, AcquiredOutlinePoll,
    AcquiredPageCount, AcquiredPageCountPoll, DocumentError, DocumentErrorCode, DocumentLimitKind,
    NeverCancelSourceRevisionChain, NeverCancelled, OpenSourceRevisionChainJob, OutlineLimitConfig,
    OutlineLimits, PageTreeLimitConfig, PageTreeLimits, RevisionResolverLimits,
    SourceAcquiredDocument, SourceAcquiredDocumentLimitConfig, SourceAcquiredDocumentLimits,
    SourceRevisionChainJobContext, SourceRevisionChainLimits, SourceRevisionChainPoll,
};
use pdf_rs_filters::{DecodeLimitConfig, DecodeLimits};
use pdf_rs_object::{ObjectLimitConfig, ObjectLimits};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{RevisionLimits, XrefAnchorLimits, XrefLimits, XrefStreamLimits};

const CHAIN_JOB: JobId = JobId::new(42_001);
const CHAIN_TAIL: ResumeCheckpoint = ResumeCheckpoint::new(42_002);
const CHAIN_ANCHOR: ResumeCheckpoint = ResumeCheckpoint::new(42_003);
const CHAIN_TRADITIONAL: ResumeCheckpoint = ResumeCheckpoint::new(42_004);
const CHAIN_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(42_005);
const CHAIN_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(42_006);
const CHAIN_PAYLOAD: ResumeCheckpoint = ResumeCheckpoint::new(42_007);

const OBJECT_JOB: JobId = JobId::new(42_101);
const OBJECT_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(42_102);
const OBJECT_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(42_103);
const LENGTH_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(42_104);
const LENGTH_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(42_105);
const OBJECT_PAYLOAD: ResumeCheckpoint = ResumeCheckpoint::new(42_106);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

fn snapshot(len: u64, tag: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([tag; 32]),
            SourceRevision::new(u64::from(tag)),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [tag ^ 0x3c; 32]),
    )
}

fn fixture(bytes: Vec<u8>, tag: u8) -> Fixture {
    Fixture {
        snapshot: snapshot(u64::try_from(bytes.len()).unwrap(), tag),
        bytes,
    }
}

fn push_object(bytes: &mut Vec<u8>, number: u32, body: &[u8]) -> u64 {
    let offset = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(b"\nendobj\n");
    offset
}

fn append_stream_entry(payload: &mut Vec<u8>, kind: u8, field_two: u32, field_three: u16) {
    payload.push(kind);
    payload.extend_from_slice(&field_two.to_be_bytes());
    payload.extend_from_slice(&field_three.to_be_bytes());
}

fn ascii_hex(bytes: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = Vec::with_capacity(bytes.len() * 2 + 1);
    for byte in bytes {
        output.push(HEX[usize::from(byte >> 4)]);
        output.push(HEX[usize::from(byte & 0x0f)]);
    }
    output.push(b'>');
    output
}

fn append_full_traditional_xref(
    bytes: &mut Vec<u8>,
    offsets: &[Option<u64>],
    root: ObjectRef,
    trailer_suffix: &str,
) -> u64 {
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
    for (number, offset) in offsets.iter().enumerate() {
        if number == 0 {
            bytes.extend_from_slice(b"0000000000 65535 f \n");
        } else if let Some(offset) = offset {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        } else {
            bytes.extend_from_slice(b"0000000000 00000 f \n");
        }
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {} {} R{} >>\nstartxref\n{startxref}\n%%EOF\n",
            offsets.len(),
            root.number(),
            root.generation(),
            trailer_suffix
        )
        .as_bytes(),
    );
    startxref
}

fn service_values() -> Vec<(u32, Vec<u8>)> {
    service_values_with_pages(b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>")
}

fn service_values_with_pages(pages: &[u8]) -> Vec<(u32, Vec<u8>)> {
    vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>".to_vec(),
        ),
        (2, pages.to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (
            4,
            b"<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>".to_vec(),
        ),
        (
            5,
            b"<< /Title (One) /Parent 4 0 R /Dest [3 0 R /Fit] >>".to_vec(),
        ),
    ]
}

fn build_object_stream(entries: &[(u32, Vec<u8>)], padding: usize) -> (usize, Vec<u8>) {
    let mut values = Vec::new();
    let mut relative_offsets = Vec::with_capacity(entries.len());
    for (_, value) in entries {
        relative_offsets.push(values.len());
        values.extend_from_slice(value);
        values.push(b' ');
    }
    values.resize(values.len() + padding, b' ');
    let mut header = String::new();
    for ((number, _), offset) in entries.iter().zip(relative_offsets) {
        header.push_str(&format!("{number} {offset} "));
    }
    let first = header.len();
    let mut decoded = header.into_bytes();
    decoded.extend_from_slice(&values);
    (first, decoded)
}

fn traditional_services_fixture(tag: u8) -> Fixture {
    traditional_services_fixture_with_pages(tag, b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>")
}

fn traditional_services_fixture_with_pages(tag: u8, pages: &[u8]) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = [None; 6];
    for (number, body) in service_values_with_pages(pages) {
        offsets[usize::try_from(number).unwrap()] = Some(push_object(&mut bytes, number, &body));
    }
    append_full_traditional_xref(&mut bytes, &offsets, ObjectRef::new(1, 0).unwrap(), "");
    fixture(bytes, tag)
}

fn oversized_catalog_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = [None; 6];
    let mut catalog = b"<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Pad (".to_vec();
    catalog.resize(catalog.len() + 8 * 1024, b'A');
    catalog.extend_from_slice(b") >>");
    offsets[1] = Some(push_object(&mut bytes, 1, &catalog));
    for (number, body) in service_values()
        .into_iter()
        .filter(|(number, _)| *number != 1)
    {
        offsets[usize::try_from(number).unwrap()] = Some(push_object(&mut bytes, number, &body));
    }
    append_full_traditional_xref(&mut bytes, &offsets, ObjectRef::new(1, 0).unwrap(), "");
    fixture(bytes, tag)
}

fn primary_stream_services_fixture(tag: u8, filtered: bool, padding: usize) -> Fixture {
    primary_stream_services_fixture_with_pages(
        tag,
        filtered,
        padding,
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
    )
}

fn primary_stream_services_fixture_with_pages(
    tag: u8,
    filtered: bool,
    padding: usize,
    pages: &[u8],
) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let values = service_values_with_pages(pages);
    let (first, decoded) = build_object_stream(&values, padding);
    let payload = if filtered {
        ascii_hex(&decoded)
    } else {
        decoded
    };
    let object_stream = u64::try_from(bytes.len()).unwrap();
    let filter = if filtered {
        " /Filter /ASCIIHexDecode"
    } else {
        ""
    };
    bytes.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /ObjStm /N 5 /First {first}{filter} /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let startxref = u64::try_from(bytes.len()).unwrap();
    let mut xref = Vec::new();
    append_stream_entry(&mut xref, 0, 0, u16::MAX);
    for index in 0_u16..5 {
        append_stream_entry(&mut xref, 2, 6, index);
    }
    append_stream_entry(&mut xref, 1, u32::try_from(object_stream).unwrap(), 0);
    append_stream_entry(&mut xref, 1, u32::try_from(startxref).unwrap(), 0);
    bytes.extend_from_slice(
        format!(
            "7 0 obj\n<< /Type /XRef /Size 8 /Root 1 0 R /W [1 4 2] /Length {} >>\nstream\n",
            xref.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    fixture(bytes, tag)
}

fn terminal_service_targets_fixture(
    tag: u8,
    terminal_target: u32,
    terminal_kind: u8,
    requested_generation: u16,
    terminal_container: u32,
) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let values = vec![
        (
            1,
            format!(
                "<< /Type /Catalog /Pages 2 {requested_generation} R /Outlines 4 {requested_generation} R >>"
            )
            .into_bytes(),
        ),
        (
            2,
            b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (
            4,
            b"<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>".to_vec(),
        ),
        (
            5,
            b"<< /Title (One) /Parent 4 0 R /Dest [3 0 R /Fit] >>".to_vec(),
        ),
    ];
    let (first, payload) = build_object_stream(&values, 0);
    let object_stream = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /ObjStm /N 5 /First {first} /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let startxref = u64::try_from(bytes.len()).unwrap();
    let mut xref = Vec::new();
    append_stream_entry(&mut xref, 0, 0, u16::MAX);
    append_stream_entry(&mut xref, 2, 6, 0);
    if terminal_target == 2 {
        append_stream_entry(
            &mut xref,
            terminal_kind,
            terminal_container,
            if terminal_kind == 2 { 1 } else { 0 },
        );
    } else {
        append_stream_entry(&mut xref, 2, 6, 1);
    }
    append_stream_entry(&mut xref, 2, 6, 2);
    if terminal_target == 4 {
        append_stream_entry(
            &mut xref,
            terminal_kind,
            terminal_container,
            if terminal_kind == 2 { 3 } else { 0 },
        );
    } else {
        append_stream_entry(&mut xref, 2, 6, 3);
    }
    append_stream_entry(&mut xref, 2, 6, 4);
    append_stream_entry(&mut xref, 1, u32::try_from(object_stream).unwrap(), 0);
    append_stream_entry(&mut xref, 1, u32::try_from(startxref).unwrap(), 0);
    bytes.extend_from_slice(
        format!(
            "7 0 obj\n<< /Type /XRef /Size 8 /Root 1 0 R /W [1 4 2] /Length {} >>\nstream\n",
            xref.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    fixture(bytes, tag)
}

fn hybrid_services_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = [None; 8];
    for (number, body) in service_values()
        .into_iter()
        .filter(|(number, _)| !matches!(number, 3 | 5))
    {
        offsets[usize::try_from(number).unwrap()] = Some(push_object(&mut bytes, number, &body));
    }
    let supplement_values = vec![
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (
            5,
            b"<< /Title (One) /Parent 4 0 R /Dest [3 0 R /Fit] >>".to_vec(),
        ),
    ];
    let (first, payload) = build_object_stream(&supplement_values, 0);
    let object_stream = u64::try_from(bytes.len()).unwrap();
    offsets[6] = Some(object_stream);
    bytes.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /ObjStm /N 2 /First {first} /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let hybrid = u64::try_from(bytes.len()).unwrap();
    offsets[7] = Some(hybrid);
    let mut xref = Vec::new();
    append_stream_entry(&mut xref, 2, 6, 0);
    append_stream_entry(&mut xref, 2, 6, 1);
    bytes.extend_from_slice(
        format!(
            "7 0 obj\n<< /Type /XRef /Size 8 /W [1 4 2] /Index [3 1 5 1] /Length {} >>\nstream\n",
            xref.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    for offset in [offsets[1].unwrap(), offsets[2].unwrap()] {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(b"4 1\n");
    bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[4].unwrap()).as_bytes());
    bytes.extend_from_slice(b"6 2\n");
    for offset in offsets[6..=7].iter().map(|offset| offset.unwrap()) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 8 /Root 1 0 R /XRefStm {hybrid} >>\nstartxref\n{startxref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    fixture(bytes, tag)
}

fn incremental_services_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut base_offsets = [None; 6];
    base_offsets[1] = Some(push_object(
        &mut bytes,
        1,
        b"<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>",
    ));
    base_offsets[2] = Some(push_object(
        &mut bytes,
        2,
        b"<< /Type /Pages /Count 0 /Kids [] >>",
    ));
    base_offsets[4] = Some(push_object(&mut bytes, 4, b"<< /Type /Outlines >>"));
    let base =
        append_full_traditional_xref(&mut bytes, &base_offsets, ObjectRef::new(1, 0).unwrap(), "");

    let updated_pages = push_object(&mut bytes, 2, b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>");
    let page = push_object(&mut bytes, 3, b"<< /Type /Page /Parent 2 0 R >>");
    let updated_outline = push_object(
        &mut bytes,
        4,
        b"<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>",
    );
    let item = push_object(
        &mut bytes,
        5,
        b"<< /Title (New) /Parent 4 0 R /A << /S /Named /N /NextPage >> >>",
    );
    let newest = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"xref\n2 4\n");
    for offset in [updated_pages, page, updated_outline, item] {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R /Prev {base} >>\nstartxref\n{newest}\n%%EOF\n")
            .as_bytes(),
    );
    fixture(bytes, tag)
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(fixture.bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone()).unwrap())
        .unwrap();
    store
}

fn chain_context() -> SourceRevisionChainJobContext {
    SourceRevisionChainJobContext::new(
        CHAIN_JOB,
        CHAIN_TAIL,
        CHAIN_ANCHOR,
        CHAIN_TRADITIONAL,
        CHAIN_ENVELOPE,
        CHAIN_BOUNDARY,
        CHAIN_PAYLOAD,
    )
}

fn object_context() -> AcquiredObjectJobContext {
    AcquiredObjectJobContext::new(
        OBJECT_JOB,
        OBJECT_ENVELOPE,
        OBJECT_BOUNDARY,
        LENGTH_ENVELOPE,
        LENGTH_BOUNDARY,
        OBJECT_PAYLOAD,
        pdf_rs_bytes::RequestPriority::Metadata,
    )
}

fn duplicate_checkpoint_context() -> AcquiredObjectJobContext {
    AcquiredObjectJobContext::new(
        OBJECT_JOB,
        OBJECT_ENVELOPE,
        OBJECT_ENVELOPE,
        LENGTH_ENVELOPE,
        LENGTH_BOUNDARY,
        OBJECT_PAYLOAD,
        pdf_rs_bytes::RequestPriority::Metadata,
    )
}

fn acquired_document(fixture: &Fixture) -> SourceAcquiredDocument {
    acquired_document_with(fixture, SourceAcquiredDocumentLimits::default())
}

fn acquired_document_with(
    fixture: &Fixture,
    limits: SourceAcquiredDocumentLimits,
) -> SourceAcquiredDocument {
    let store = supplied_store(fixture);
    let mut job = OpenSourceRevisionChainJob::new_with_decode_limits(
        fixture.snapshot,
        chain_context(),
        SourceRevisionChainLimits::default(),
        XrefLimits::default(),
        XrefAnchorLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
        XrefStreamLimits::default(),
        DecodeLimits::default(),
        RevisionLimits::default(),
    )
    .unwrap();
    let chain = match job.poll(&store, &NeverCancelSourceRevisionChain) {
        SourceRevisionChainPoll::Ready(chain) => chain,
        other => panic!("fully supplied chain did not complete: {other:?}"),
    };
    SourceAcquiredDocument::new(chain, limits, &NeverCancelled).unwrap()
}

fn compact_page_limits(read: u64, parse: u64, retained: u64) -> PageTreeLimits {
    PageTreeLimits::validate(PageTreeLimitConfig {
        max_nodes: 8,
        max_depth: 4,
        max_pages: 4,
        max_kids_per_node: 4,
        max_total_object_read_bytes: read,
        max_total_object_parse_bytes: parse,
        max_retained_traversal_bytes: retained,
    })
    .unwrap()
}

fn default_page_limits() -> PageTreeLimits {
    compact_page_limits(1 << 20, 1 << 20, 64 << 10)
}

fn compact_outline_limits(read: u64, parse: u64, retained: u64) -> OutlineLimits {
    OutlineLimits::validate(OutlineLimitConfig {
        max_items: 8,
        max_depth: 4,
        max_siblings_per_level: 4,
        max_title_input_bytes: 64,
        max_title_utf8_bytes: 64,
        max_total_title_input_bytes: 256,
        max_total_title_utf8_bytes: 256,
        max_total_object_read_bytes: read,
        max_total_object_parse_bytes: parse,
        max_retained_bytes: retained,
    })
    .unwrap()
}

fn default_outline_limits() -> OutlineLimits {
    compact_outline_limits(1 << 20, 1 << 20, 64 << 10)
}

fn ready_page_count(
    document: &SourceAcquiredDocument,
    source: &dyn ByteSource,
    limits: PageTreeLimits,
) -> AcquiredPageCount {
    let mut job = document
        .count_acquired_pages(object_context(), limits)
        .unwrap();
    match job.poll(source, &NeverCancelled) {
        AcquiredPageCountPoll::Ready(value) => value,
        other => panic!("fully supplied page service did not complete: {other:?}"),
    }
}

fn ready_outline(
    document: &SourceAcquiredDocument,
    source: &dyn ByteSource,
    limits: OutlineLimits,
) -> AcquiredOutline {
    let mut job = document
        .read_acquired_outline(object_context(), limits)
        .unwrap();
    match job.poll(source, &NeverCancelled) {
        AcquiredOutlinePoll::Ready(value) => value,
        other => panic!("fully supplied outline service did not complete: {other:?}"),
    }
}

fn assert_services(fixture: &Fixture, label: &str, expected_title: &str) {
    let store = supplied_store(fixture);
    let document = acquired_document(fixture);
    for number in 1..=5 {
        let reference = ObjectRef::new(number, 0).unwrap();
        assert!(
            document.locator(number).is_some(),
            "{label}: effective object {number} must exist"
        );
        assert!(
            document.object_source_offset(reference).is_some(),
            "{label}: effective object {number} must be resolvable: {:?}",
            document.locator(number)
        );
    }
    let page_count = ready_page_count(&document, &store, default_page_limits());
    assert_eq!(page_count.page_count(), 1);
    assert_eq!(page_count.catalog().root(), ObjectRef::new(1, 0).unwrap());
    assert_eq!(page_count.catalog().pages(), ObjectRef::new(2, 0).unwrap());
    assert!(page_count.stats().object_read_bytes() > 0);
    assert!(page_count.stats().object_parse_bytes() > 0);

    let outline = ready_outline(&document, &store, default_outline_limits());
    assert_eq!(outline.root(), Some(ObjectRef::new(4, 0).unwrap()));
    assert_eq!(outline.root_count(), Some(1));
    assert_eq!(outline.visible_items(), 1);
    assert_eq!(outline.items().len(), 1);
    assert_eq!(
        outline.items()[0].reference(),
        ObjectRef::new(5, 0).unwrap()
    );
    assert_eq!(outline.items()[0].title(), expected_title);
    assert!(outline.stats().object_read_bytes() > 0);
    assert!(outline.stats().object_parse_bytes() > 0);
}

fn supply_missing(store: &RangeStore, fixture: &Fixture, missing: &SmallRanges) {
    for range in missing.as_slice().iter().rev().copied() {
        let start = usize::try_from(range.start()).unwrap();
        let end = usize::try_from(range.end_exclusive()).unwrap();
        store
            .supply(
                RangeResponse::new(fixture.snapshot, range, fixture.bytes[start..end].to_vec())
                    .unwrap(),
            )
            .unwrap();
    }
}

fn page_failure(
    document: &SourceAcquiredDocument,
    source: &dyn ByteSource,
    limits: PageTreeLimits,
) -> DocumentError {
    let Ok(mut job) = document.count_acquired_pages(object_context(), limits) else {
        return document
            .count_acquired_pages(object_context(), limits)
            .unwrap_err();
    };
    match job.poll(source, &NeverCancelled) {
        AcquiredPageCountPoll::Failed(error) => error,
        other => panic!("expected page failure, got {other:?}"),
    }
}

fn outline_failure(
    document: &SourceAcquiredDocument,
    source: &dyn ByteSource,
    limits: OutlineLimits,
) -> DocumentError {
    let Ok(mut job) = document.read_acquired_outline(object_context(), limits) else {
        return document
            .read_acquired_outline(object_context(), limits)
            .unwrap_err();
    };
    match job.poll(source, &NeverCancelled) {
        AcquiredOutlinePoll::Failed(error) => error,
        other => panic!("expected outline failure, got {other:?}"),
    }
}

#[test]
fn page_and_outline_services_cover_revision_and_object_representations() {
    let traditional = traditional_services_fixture(0xc1);
    assert_services(&traditional, "traditional", "One");

    let primary_unfiltered = primary_stream_services_fixture(0xc2, false, 0);
    assert_services(&primary_unfiltered, "primary-unfiltered", "One");

    let primary_filtered = primary_stream_services_fixture(0xc3, true, 0);
    assert_services(&primary_filtered, "primary-filtered", "One");

    let hybrid = hybrid_services_fixture(0xc4);
    assert_services(&hybrid, "hybrid", "One");

    let incremental = incremental_services_fixture(0xc5);
    assert_services(&incremental, "incremental", "New");
}

#[test]
fn page_constructor_rejects_duplicate_checkpoints_before_retained_reservation() {
    let fixture = traditional_services_fixture(0xd1);
    let document = acquired_document(&fixture);
    let limits = compact_page_limits(1, 1, 1);
    let error = document
        .count_acquired_pages(duplicate_checkpoint_context(), limits)
        .unwrap_err();
    assert_eq!(
        error.code(),
        DocumentErrorCode::InvalidRevisionResolverJobContext
    );
    assert!(
        error.limit().is_none(),
        "checkpoint validation must precede the deliberately impossible retained reservation"
    );
}

#[test]
fn page_parent_null_and_type_semantics_match_in_physical_and_decoded_coordinates() {
    let null_pages = b"<< /Type /Pages /Parent null /Count 1 /Kids [3 0 R] >>";
    for fixture in [
        traditional_services_fixture_with_pages(0xd2, null_pages),
        primary_stream_services_fixture_with_pages(0xd3, true, 0, null_pages),
    ] {
        let store = supplied_store(&fixture);
        let document = acquired_document(&fixture);
        assert_eq!(
            ready_page_count(&document, &store, default_page_limits()).page_count(),
            1
        );
    }

    let invalid_pages = b"<< /Type /Pages /Parent 7 /Count 1 /Kids [3 0 R] >>";
    for fixture in [
        traditional_services_fixture_with_pages(0xd4, invalid_pages),
        primary_stream_services_fixture_with_pages(0xd5, true, 0, invalid_pages),
    ] {
        let store = supplied_store(&fixture);
        let document = acquired_document(&fixture);
        let error = page_failure(&document, &store, default_page_limits());
        assert_eq!(error.code(), DocumentErrorCode::InvalidPageTreeNode);
    }
}

#[test]
fn sparse_reverse_supply_resumes_both_services_and_replays_pending_ticket() {
    let fixture = primary_stream_services_fixture(0xc6, true, 8 * 1024);
    let document = acquired_document(&fixture);

    let page_store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut page = document
        .count_acquired_pages(object_context(), default_page_limits())
        .unwrap();
    let (ticket, missing, checkpoint, stats) = match page.poll(&page_store, &NeverCancelled) {
        AcquiredPageCountPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => (ticket, missing, checkpoint, page.stats()),
        other => panic!("empty page source must pend: {other:?}"),
    };
    match page.poll(&page_store, &NeverCancelled) {
        AcquiredPageCountPoll::Pending {
            ticket: repeated_ticket,
            missing: repeated_missing,
            checkpoint: repeated_checkpoint,
        } => {
            assert_eq!(repeated_ticket, ticket);
            assert_eq!(repeated_missing, missing);
            assert_eq!(repeated_checkpoint, checkpoint);
            assert_eq!(page.stats(), stats);
        }
        other => panic!("unchanged page source must replay Pending: {other:?}"),
    }
    supply_missing(&page_store, &fixture, &missing);
    let mut checkpoints = vec![checkpoint];
    let page_count = loop {
        match page.poll(&page_store, &NeverCancelled) {
            AcquiredPageCountPoll::Pending {
                missing,
                checkpoint,
                ..
            } => {
                checkpoints.push(checkpoint);
                supply_missing(&page_store, &fixture, &missing);
            }
            AcquiredPageCountPoll::Ready(value) => break value,
            AcquiredPageCountPoll::Failed(error) => panic!("sparse page service failed: {error:?}"),
        }
    };
    assert_eq!(page_count.page_count(), 1);
    assert!(checkpoints.contains(&OBJECT_ENVELOPE));
    assert!(checkpoints.contains(&OBJECT_BOUNDARY));
    assert!(checkpoints.contains(&OBJECT_PAYLOAD));

    let outline_store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut outline = document
        .read_acquired_outline(object_context(), default_outline_limits())
        .unwrap();
    let mut checkpoints = Vec::new();
    let ready = loop {
        match outline.poll(&outline_store, &NeverCancelled) {
            AcquiredOutlinePoll::Pending {
                missing,
                checkpoint,
                ..
            } => {
                checkpoints.push(checkpoint);
                supply_missing(&outline_store, &fixture, &missing);
            }
            AcquiredOutlinePoll::Ready(value) => break value,
            AcquiredOutlinePoll::Failed(error) => {
                panic!("sparse outline service failed: {error:?}")
            }
        }
    };
    assert_eq!(ready.items()[0].title(), "One");
    assert!(checkpoints.contains(&OBJECT_ENVELOPE));
    assert!(checkpoints.contains(&OBJECT_BOUNDARY));
    assert!(checkpoints.contains(&OBJECT_PAYLOAD));
}

#[test]
fn service_cancellation_source_change_and_terminal_replay_are_stable() {
    let fixture = primary_stream_services_fixture(0xc7, true, 0);
    let document = acquired_document(&fixture);
    let empty = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let cancellation = AtomicBool::new(false);
    let mut page = document
        .count_acquired_pages(object_context(), default_page_limits())
        .unwrap();
    assert!(matches!(
        page.poll(&empty, &cancellation),
        AcquiredPageCountPoll::Pending { .. }
    ));
    cancellation.store(true, Ordering::Release);
    let cancelled = match page.poll(&empty, &cancellation) {
        AcquiredPageCountPoll::Failed(error) => error,
        other => panic!("cancelled page service must fail: {other:?}"),
    };
    assert_eq!(cancelled.code(), DocumentErrorCode::Cancelled);
    assert!(matches!(
        page.poll(&empty, &NeverCancelled),
        AcquiredPageCountPoll::Failed(repeated) if repeated == cancelled
    ));

    struct SnapshotOnly(SourceSnapshot);
    impl ByteSource for SnapshotOnly {
        fn snapshot(&self) -> SourceSnapshot {
            self.0
        }

        fn poll(&self, _: ReadRequest) -> ReadPoll<ByteSlice> {
            panic!("snapshot mismatch must precede lower polling")
        }
    }
    let changed = SnapshotOnly(snapshot(fixture.snapshot.len().unwrap(), 0xe7));
    let mut mismatch = document
        .read_acquired_outline(object_context(), default_outline_limits())
        .unwrap();
    let error = match mismatch.poll(&changed, &NeverCancelled) {
        AcquiredOutlinePoll::Failed(error) => error,
        other => panic!("source-changed outline must fail: {other:?}"),
    };
    assert_eq!(error.code(), DocumentErrorCode::SourceSnapshotMismatch);

    let full = supplied_store(&fixture);
    let mut ready = document
        .read_acquired_outline(object_context(), default_outline_limits())
        .unwrap();
    assert!(matches!(
        ready.poll(&full, &NeverCancelled),
        AcquiredOutlinePoll::Ready(_)
    ));
    assert!(matches!(
        ready.poll(&changed, &cancellation),
        AcquiredOutlinePoll::Failed(error)
            if error.code() == DocumentErrorCode::JobAlreadyComplete
    ));
}

#[test]
fn service_aggregate_limits_accept_measured_exact_and_reject_one_less() {
    let fixture = primary_stream_services_fixture(0xc8, true, 0);
    let store = supplied_store(&fixture);
    let document = acquired_document(&fixture);

    let page_stats = ready_page_count(&document, &store, default_page_limits()).stats();
    assert!(page_stats.object_read_bytes() > 1);
    assert!(page_stats.object_parse_bytes() > 1);
    assert!(page_stats.reserved_traversal_bytes() > 1);
    let exact_page = compact_page_limits(
        page_stats.object_read_bytes(),
        page_stats.object_parse_bytes(),
        page_stats.reserved_traversal_bytes(),
    );
    assert_eq!(
        ready_page_count(&document, &store, exact_page).page_count(),
        1
    );
    for (limits, expected) in [
        (
            compact_page_limits(
                page_stats.object_read_bytes() - 1,
                page_stats.object_parse_bytes(),
                page_stats.reserved_traversal_bytes(),
            ),
            DocumentLimitKind::PageTreeObjectReadBytes,
        ),
        (
            compact_page_limits(
                page_stats.object_read_bytes(),
                page_stats.object_parse_bytes() - 1,
                page_stats.reserved_traversal_bytes(),
            ),
            DocumentLimitKind::PageTreeObjectParseBytes,
        ),
        (
            compact_page_limits(
                page_stats.object_read_bytes(),
                page_stats.object_parse_bytes(),
                page_stats.reserved_traversal_bytes() - 1,
            ),
            DocumentLimitKind::PageTreeTraversalBytes,
        ),
    ] {
        let error = page_failure(&document, &store, limits);
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), expected);
    }

    let outline_stats = ready_outline(&document, &store, default_outline_limits()).stats();
    assert!(outline_stats.object_read_bytes() > 1);
    assert!(outline_stats.object_parse_bytes() > 1);
    assert!(outline_stats.reserved_bytes() > 1);
    let exact_outline = compact_outline_limits(
        outline_stats.object_read_bytes(),
        outline_stats.object_parse_bytes(),
        outline_stats.reserved_bytes(),
    );
    assert_eq!(
        ready_outline(&document, &store, exact_outline).visible_items(),
        1
    );
    for (limits, expected) in [
        (
            compact_outline_limits(
                outline_stats.object_read_bytes() - 1,
                outline_stats.object_parse_bytes(),
                outline_stats.reserved_bytes(),
            ),
            DocumentLimitKind::OutlineObjectReadBytes,
        ),
        (
            compact_outline_limits(
                outline_stats.object_read_bytes(),
                outline_stats.object_parse_bytes() - 1,
                outline_stats.reserved_bytes(),
            ),
            DocumentLimitKind::OutlineObjectParseBytes,
        ),
        (
            compact_outline_limits(
                outline_stats.object_read_bytes(),
                outline_stats.object_parse_bytes(),
                outline_stats.reserved_bytes() - 1,
            ),
            DocumentLimitKind::OutlineRetainedBytes,
        ),
    ] {
        let error = outline_failure(&document, &store, limits);
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), expected);
    }
}

#[test]
fn terminal_target_classification_precedes_page_and_outline_remainders() {
    for (tag, terminal_kind, generation, container, expected) in [
        (0xd6, 0, 0, 0, DocumentErrorCode::FreeObject),
        (0xd8, 3, 0, 0, DocumentErrorCode::NullObject),
        (0xda, 2, 1, 6, DocumentErrorCode::GenerationMismatch),
        (
            0xdc,
            2,
            0,
            5,
            DocumentErrorCode::InvalidObjectStreamContainer,
        ),
        (
            0xde,
            2,
            0,
            7,
            DocumentErrorCode::UnsupportedXrefStreamContainer,
        ),
    ] {
        let page_fixture =
            terminal_service_targets_fixture(tag, 2, terminal_kind, generation, container);
        let store = supplied_store(&page_fixture);
        let document = acquired_document(&page_fixture);

        let mut page = document
            .count_acquired_pages(object_context(), default_page_limits())
            .unwrap();
        let page_error = match page.poll(&store, &NeverCancelled) {
            AcquiredPageCountPoll::Failed(error) => error,
            other => panic!("terminal Pages target must fail: {other:?}"),
        };
        assert_eq!(page_error.code(), expected);
        assert!(page_error.limit().is_none());
        let page_stats = page.stats();
        assert_eq!(page_stats.objects_started(), 1);
        let tight_page = compact_page_limits(
            page_stats.object_read_bytes(),
            page_stats.object_parse_bytes(),
            64 << 10,
        );
        let tight_page_error = page_failure(&document, &store, tight_page);
        assert_eq!(tight_page_error.code(), expected);
        assert!(
            tight_page_error.limit().is_none(),
            "terminal Pages classification must beat an exhausted service remainder"
        );

        let outline_fixture = terminal_service_targets_fixture(
            tag.wrapping_add(1),
            4,
            terminal_kind,
            generation,
            container,
        );
        let outline_store = supplied_store(&outline_fixture);
        let outline_document = acquired_document(&outline_fixture);
        let mut outline = outline_document
            .read_acquired_outline(object_context(), default_outline_limits())
            .unwrap();
        let outline_error = match outline.poll(&outline_store, &NeverCancelled) {
            AcquiredOutlinePoll::Failed(error) => error,
            other => panic!("terminal Outlines target must fail: {other:?}"),
        };
        assert_eq!(outline_error.code(), expected);
        assert!(outline_error.limit().is_none());
        let outline_stats = outline.stats();
        assert_eq!(outline_stats.objects_started(), 1);
        let tight_outline = compact_outline_limits(
            outline_stats.object_read_bytes(),
            outline_stats.object_parse_bytes(),
            64 << 10,
        );
        let tight_outline_error = outline_failure(&outline_document, &outline_store, tight_outline);
        assert_eq!(tight_outline_error.code(), expected);
        assert!(
            tight_outline_error.limit().is_none(),
            "terminal Outlines classification must beat an exhausted service remainder"
        );
    }
}

#[test]
fn decoder_parent_caps_preserve_exact_service_aggregate_accounting() {
    let fixture = primary_stream_services_fixture(0xdb, true, 8 * 1024);
    let store = supplied_store(&fixture);
    let document = acquired_document(&fixture);
    let mut root = document
        .open_object(document.root(), object_context())
        .unwrap();
    let baseline = match root.poll(&store, &NeverCancelled) {
        AcquiredObjectPoll::Ready(object) => object.stats(),
        other => panic!("baseline compressed Catalog must resolve: {other:?}"),
    };
    assert!(baseline.decode_output_bytes() > 32);
    let parse_cap = baseline.resolver().total_parse_bytes() + 32;

    let page_error = page_failure(
        &document,
        &store,
        compact_page_limits(1 << 20, parse_cap, 64 << 10),
    );
    let page_limit = page_error.limit().unwrap();
    assert_eq!(
        page_limit.kind(),
        DocumentLimitKind::PageTreeObjectParseBytes
    );
    assert_eq!(page_limit.limit(), parse_cap);
    assert_eq!(page_limit.consumed(), parse_cap - 1);
    assert_eq!(page_limit.attempted(), 2);

    let outline_error = outline_failure(
        &document,
        &store,
        compact_outline_limits(1 << 20, parse_cap, 64 << 10),
    );
    let outline_limit = outline_error.limit().unwrap();
    assert_eq!(
        outline_limit.kind(),
        DocumentLimitKind::OutlineObjectParseBytes
    );
    assert_eq!(outline_limit.limit(), parse_cap);
    assert_eq!(outline_limit.consumed(), parse_cap - 1);
    assert_eq!(outline_limit.attempted(), 2);
}

#[test]
fn intrinsic_payload_cap_is_not_masked_by_tighter_service_children() {
    let fixture = primary_stream_services_fixture(0xdc, true, 1024);
    let store = supplied_store(&fixture);
    let baseline_document = acquired_document(&fixture);
    let mut baseline_job = baseline_document
        .open_object(baseline_document.root(), object_context())
        .unwrap();
    let baseline = match baseline_job.poll(&store, &NeverCancelled) {
        AcquiredObjectPoll::Ready(object) => object.stats(),
        other => panic!("baseline Catalog must resolve: {other:?}"),
    };
    assert!(baseline.payload_read_bytes() > 64);
    let decode = DecodeLimits::validate(DecodeLimitConfig {
        max_input_bytes: 64,
        ..DecodeLimitConfig::default()
    })
    .unwrap();
    let owner_limits = SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
        decode,
        ..SourceAcquiredDocumentLimitConfig::default()
    })
    .unwrap();
    let document = acquired_document_with(&fixture, owner_limits);

    for error in [
        page_failure(&document, &store, default_page_limits()),
        outline_failure(&document, &store, default_outline_limits()),
    ] {
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        assert!(
            error.limit().is_none(),
            "intrinsic Decode input bytes are not a page/outline aggregate"
        );
    }

    let service_cap = baseline.resolver().total_read_bytes() + 32;
    let page_error = page_failure(
        &document,
        &store,
        compact_page_limits(service_cap, 1 << 20, 64 << 10),
    );
    let page_limit = page_error.limit().unwrap();
    assert_eq!(
        page_limit.kind(),
        DocumentLimitKind::PageTreeObjectReadBytes
    );
    assert_eq!(page_limit.limit(), service_cap);
    assert_eq!(
        page_limit.consumed(),
        baseline.resolver().total_read_bytes()
    );
    assert_eq!(page_limit.attempted(), baseline.payload_read_bytes());

    let outline_error = outline_failure(
        &document,
        &store,
        compact_outline_limits(service_cap, 1 << 20, 64 << 10),
    );
    let outline_limit = outline_error.limit().unwrap();
    assert_eq!(
        outline_limit.kind(),
        DocumentLimitKind::OutlineObjectReadBytes
    );
    assert_eq!(outline_limit.limit(), service_cap);
    assert_eq!(
        outline_limit.consumed(),
        baseline.resolver().total_read_bytes()
    );
    assert_eq!(outline_limit.attempted(), baseline.payload_read_bytes());
}

#[test]
fn intrinsic_resolver_child_cap_is_not_promoted_to_a_service_aggregate() {
    let fixture = oversized_catalog_fixture(0xe8);
    let object = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: 16 << 20,
        initial_envelope_bytes: 256,
        max_envelope_bytes: 4 << 10,
        initial_boundary_bytes: 64,
        max_boundary_bytes: 256,
        max_stream_bytes: 1 << 20,
        max_total_read_bytes: (4 << 10) + 256,
        max_total_parse_bytes: (4 << 10) + 256,
    })
    .unwrap();
    let resolver = RevisionResolverLimits::from_object_limits(object).unwrap();
    let owner_limits = SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
        resolver,
        ..SourceAcquiredDocumentLimitConfig::default()
    })
    .unwrap();
    let document = acquired_document_with(&fixture, owner_limits);
    let store = supplied_store(&fixture);
    let service_cap = 16 << 10;
    assert!(service_cap < owner_limits.max_object_read_bytes());
    assert!(service_cap > resolver.max_total_object_read_bytes());

    for error in [
        page_failure(
            &document,
            &store,
            compact_page_limits(service_cap, service_cap, 64 << 10),
        ),
        outline_failure(
            &document,
            &store,
            compact_outline_limits(service_cap, service_cap, 64 << 10),
        ),
    ] {
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        assert!(
            error.limit().is_none(),
            "the service cap does not tighten the resolver/object intrinsic ceiling"
        );
        assert_eq!(
            error.object_error_code(),
            Some(pdf_rs_object::ObjectErrorCode::ResourceLimit)
        );
    }
}
