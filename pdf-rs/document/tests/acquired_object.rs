use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    ResumeCheckpoint, SmallRanges, SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId,
    SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AcquiredObjectCoordinate, AcquiredObjectJobContext, AcquiredObjectPoll, AcquiredObjectValue,
    DocumentCancellation, DocumentError, DocumentErrorCode, DocumentLimitKind,
    EffectiveObjectLocator, NeverCancelSourceRevisionChain, NeverCancelled,
    OpenSourceRevisionChainJob, SourceAcquiredDocument, SourceAcquiredDocumentLimitConfig,
    SourceAcquiredDocumentLimits, SourceRevisionChainJobContext, SourceRevisionChainLimits,
    SourceRevisionChainPoll,
};
use pdf_rs_filters::{DecodeLimitConfig, DecodeLimits, FilterPlan};
use pdf_rs_object::{DecodedObject, ObjectLimits};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits, SyntaxObject};
use pdf_rs_xref::{RevisionLimits, XrefAnchorLimits, XrefLimits, XrefStreamLimits};

const CHAIN_JOB: JobId = JobId::new(41_001);
const CHAIN_TAIL: ResumeCheckpoint = ResumeCheckpoint::new(41_002);
const CHAIN_ANCHOR: ResumeCheckpoint = ResumeCheckpoint::new(41_003);
const CHAIN_TRADITIONAL: ResumeCheckpoint = ResumeCheckpoint::new(41_004);
const CHAIN_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(41_005);
const CHAIN_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(41_006);
const CHAIN_PAYLOAD: ResumeCheckpoint = ResumeCheckpoint::new(41_007);

const OBJECT_JOB: JobId = JobId::new(41_101);
const OBJECT_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(41_102);
const OBJECT_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(41_103);
const LENGTH_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(41_104);
const LENGTH_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(41_105);
const OBJECT_PAYLOAD: ResumeCheckpoint = ResumeCheckpoint::new(41_106);

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
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [tag ^ 0x5a; 32]),
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

fn object_stream_fixture(tag: u8, filtered: bool) -> Fixture {
    object_stream_fixture_with_padding(tag, filtered, 0)
}

fn object_stream_fixture_with_padding(tag: u8, filtered: bool, padding: usize) -> Fixture {
    object_stream_fixture_with_padding_and_terminal(tag, filtered, padding, 0)
}

fn object_stream_fixture_with_padding_and_terminal(
    tag: u8,
    filtered: bool,
    padding: usize,
    terminal_kind: u8,
) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let pages = push_object(&mut bytes, 4, b"<< /Type /Pages /Count 0 /Kids [] >>");
    let mut decoded = b"1 0 << /Type /Catalog /Pages 4 0 R >>".to_vec();
    decoded.resize(decoded.len() + padding, b' ');
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
            "2 0 obj\n<< /Type /ObjStm /N 1 /First 4{filter} /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let startxref = u64::try_from(bytes.len()).unwrap();
    let mut xref = Vec::new();
    append_stream_entry(&mut xref, 0, 0, u16::MAX);
    append_stream_entry(&mut xref, 2, 2, 0);
    append_stream_entry(&mut xref, 1, u32::try_from(object_stream).unwrap(), 0);
    append_stream_entry(&mut xref, terminal_kind, 0, 0);
    append_stream_entry(&mut xref, 1, u32::try_from(pages).unwrap(), 0);
    append_stream_entry(&mut xref, 1, u32::try_from(startxref).unwrap(), 0);
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 4 2] /Length {} >>\nstream\n",
            xref.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    fixture(bytes, tag)
}

fn traditional_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let root = push_object(&mut bytes, 1, b"<< /Type /Catalog >>");
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{root:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    fixture(bytes, tag)
}

fn malformed_indirect_length_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut target_body = b"<< /Length 2 0 R /Meta [".to_vec();
    for index in 0..96 {
        target_body.extend_from_slice(
            format!("<< /K{index} (target-retained-value-{index:03}) >> ").as_bytes(),
        );
    }
    target_body.extend_from_slice(b"] >>\nstream\nDATA\nendstream");
    let target = push_object(&mut bytes, 1, &target_body);

    let mut malformed_length = b"[ ".to_vec();
    for index in 0..192 {
        malformed_length.extend_from_slice(
            format!("<< /K{index} (length-retained-value-{index:03}) >> ").as_bytes(),
        );
    }
    malformed_length.extend_from_slice(b"]");
    let length = push_object(&mut bytes, 2, &malformed_length);

    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(
        format!(
            "xref\n0 3\n0000000000 65535 f \n{target:010} 00000 n \n{length:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    fixture(bytes, tag)
}

fn compressed_length_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let pages = push_object(&mut bytes, 4, b"<< /Type /Pages /Count 0 /Kids [] >>");
    let root_value = b"<< /Type /Catalog /Pages 4 0 R >>";
    let second_offset = root_value.len() + 1;
    let header = format!("1 0 6 {second_offset} ");
    let mut payload = header.as_bytes().to_vec();
    payload.extend_from_slice(root_value);
    payload.extend_from_slice(b" 100");
    let object_stream = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /ObjStm /N 2 /First {} /Length 6 0 R >>\nstream\n",
            header.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let startxref = u64::try_from(bytes.len()).unwrap();
    let mut xref = Vec::new();
    append_stream_entry(&mut xref, 0, 0, u16::MAX);
    append_stream_entry(&mut xref, 2, 2, 0);
    append_stream_entry(&mut xref, 1, u32::try_from(object_stream).unwrap(), 0);
    append_stream_entry(&mut xref, 0, 0, 0);
    append_stream_entry(&mut xref, 1, u32::try_from(pages).unwrap(), 0);
    append_stream_entry(&mut xref, 1, u32::try_from(startxref).unwrap(), 0);
    append_stream_entry(&mut xref, 2, 2, 1);
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 7 /Root 1 0 R /W [1 4 2] /Length {} >>\nstream\n",
            xref.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    fixture(bytes, tag)
}

fn latest_uncompressed_indirect_length_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let pages = push_object(&mut bytes, 4, b"<< /Type /Pages /Count 0 /Kids [] >>");
    let old_length = push_object(&mut bytes, 6, b"1");
    let mut decoded = b"1 0 << /Type /Catalog /Pages 4 0 R >>".to_vec();
    decoded.resize(decoded.len() + 8 * 1024, b' ');
    let object_stream = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length 6 0 R >>\nstream\n");
    bytes.extend_from_slice(&decoded);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let base_startxref = u64::try_from(bytes.len()).unwrap();
    let mut xref = Vec::new();
    append_stream_entry(&mut xref, 0, 0, u16::MAX);
    append_stream_entry(&mut xref, 2, 2, 0);
    append_stream_entry(&mut xref, 1, u32::try_from(object_stream).unwrap(), 0);
    append_stream_entry(&mut xref, 0, 0, 0);
    append_stream_entry(&mut xref, 1, u32::try_from(pages).unwrap(), 0);
    append_stream_entry(&mut xref, 1, u32::try_from(base_startxref).unwrap(), 0);
    append_stream_entry(&mut xref, 1, u32::try_from(old_length).unwrap(), 0);
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 7 /Root 1 0 R /W [1 4 2] /Length {} >>\nstream\n",
            xref.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{base_startxref}\n%%EOF\n").as_bytes());

    let newest_length = push_object(&mut bytes, 6, decoded.len().to_string().as_bytes());
    let newest_startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"xref\n6 1\n");
    bytes.extend_from_slice(format!("{newest_length:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 7 /Prev {base_startxref} >>\nstartxref\n{newest_startxref}\n%%EOF\n"
        )
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

fn acquired_chain(fixture: &Fixture) -> pdf_rs_document::SourceAcquiredRevisionChain {
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
    match job.poll(&store, &NeverCancelSourceRevisionChain) {
        SourceRevisionChainPoll::Ready(chain) => chain,
        other => panic!("fully supplied chain did not complete: {other:?}"),
    }
}

fn document_with(
    fixture: &Fixture,
    limits: SourceAcquiredDocumentLimits,
) -> SourceAcquiredDocument {
    SourceAcquiredDocument::new(acquired_chain(fixture), limits, &NeverCancelled).unwrap()
}

struct CountingCancellation(AtomicUsize);

impl CountingCancellation {
    fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    fn calls(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }
}

impl DocumentCancellation for CountingCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.fetch_add(1, Ordering::AcqRel);
        false
    }
}

fn ready_object<'owner>(
    document: &'owner SourceAcquiredDocument,
    source: &dyn ByteSource,
    reference: ObjectRef,
) -> pdf_rs_document::AcquiredObject<'owner> {
    let mut job = document.open_object(reference, object_context()).unwrap();
    match job.poll(source, &NeverCancelled) {
        AcquiredObjectPoll::Ready(object) => object,
        other => panic!("fully supplied object did not complete: {other:?}"),
    }
}

fn failure(outcome: AcquiredObjectPoll<'_>) -> DocumentError {
    match outcome {
        AcquiredObjectPoll::Failed(error) => error,
        other => panic!("expected failure, got {other:?}"),
    }
}

fn supply_missing(store: &RangeStore, fixture: &Fixture, missing: &SmallRanges, reverse: bool) {
    let ranges: Vec<_> = if reverse {
        missing.as_slice().iter().rev().copied().collect()
    } else {
        missing.as_slice().to_vec()
    };
    for range in ranges {
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

#[test]
fn resolves_direct_and_unfiltered_compressed_values_without_lending_the_chain() {
    let fixture = object_stream_fixture(0xb1, false);
    let store = supplied_store(&fixture);
    let document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    assert_eq!(document.root(), ObjectRef::new(1, 0).unwrap());
    assert!(!document.acquisition().proofs().is_empty());
    assert!(document.stats().owner_retained_bound_bytes() > 0);

    let direct = ready_object(&document, &store, ObjectRef::new(4, 0).unwrap());
    let AcquiredObjectValue::Uncompressed(value) = direct.value().unwrap() else {
        panic!("page root must remain a physical top-level value");
    };
    assert!(matches!(value.value(), SyntaxObject::Dictionary(_)));
    assert!(matches!(
        direct.coordinate().unwrap(),
        AcquiredObjectCoordinate::Physical(_)
    ));

    let compressed = ready_object(&document, &store, ObjectRef::new(1, 0).unwrap());
    let AcquiredObjectValue::Compressed(value) = compressed.value().unwrap() else {
        panic!("catalog must retain decoded object-stream coordinates");
    };
    let DecodedObject::Dictionary(dictionary) = value.value() else {
        panic!("compressed catalog must be a dictionary");
    };
    assert!(matches!(
        dictionary.get(b"Type").unwrap().value(),
        DecodedObject::Name(name) if name.bytes() == b"Catalog"
    ));
    assert_eq!(
        dictionary.get(b"Pages").unwrap().value().as_reference(),
        Some(ObjectRef::new(4, 0).unwrap())
    );
    assert!(matches!(
        compressed.coordinate().unwrap(),
        AcquiredObjectCoordinate::Decoded { container, .. }
            if container == ObjectRef::new(2, 0).unwrap()
    ));
    assert_eq!(compressed.stats().decode_output_bytes(), 0);
    assert!(compressed.stats().object_stream().is_some());
}

#[test]
fn filtered_object_stream_retains_sealed_decode_and_revalidates_exact_entry() {
    let fixture = object_stream_fixture(0xb2, true);
    let store = supplied_store(&fixture);
    let document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let object = ready_object(&document, &store, ObjectRef::new(1, 0).unwrap());
    assert!(object.stats().decode_output_bytes() > 0);
    assert!(object.stats().decode_fuel() > 0);
    assert!(object.stats().retained_proof_bytes() > 0);
    assert!(
        object.stats().admitted_retained_bound_bytes() >= object.stats().retained_proof_bytes()
    );
    let AcquiredObjectValue::Compressed(value) = object.value().unwrap() else {
        panic!("filtered catalog must remain compressed");
    };
    assert!(matches!(value.value(), DecodedObject::Dictionary(_)));
    assert!(
        object.value().is_ok(),
        "every publication revalidates binding"
    );
}

#[test]
fn sparse_reverse_supply_resumes_through_payload_and_replays_one_pending_ticket() {
    let fixture = object_stream_fixture_with_padding(0xb3, true, 8 * 1024);
    let document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    let (first_ticket, first_missing, first_checkpoint, before) =
        match job.poll(&store, &NeverCancelled) {
            AcquiredObjectPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => (ticket, missing, checkpoint, job.stats()),
            other => panic!("empty store must pend: {other:?}"),
        };
    match job.poll(&store, &NeverCancelled) {
        AcquiredObjectPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(ticket, first_ticket);
            assert_eq!(missing, first_missing);
            assert_eq!(checkpoint, first_checkpoint);
            assert_eq!(job.stats(), before);
        }
        other => panic!("unchanged source must replay pending: {other:?}"),
    }
    supply_missing(&store, &fixture, &first_missing, true);

    let mut checkpoints = vec![first_checkpoint];
    let mut ready = false;
    for _ in 0..32 {
        match job.poll(&store, &NeverCancelled) {
            AcquiredObjectPoll::Pending {
                missing,
                checkpoint,
                ..
            } => {
                checkpoints.push(checkpoint);
                supply_missing(&store, &fixture, &missing, true);
            }
            AcquiredObjectPoll::Ready(object) => {
                assert!(object.value().is_ok());
                ready = true;
                break;
            }
            AcquiredObjectPoll::Failed(error) => panic!("sparse resolution failed: {error:?}"),
        }
    }
    assert!(ready);
    assert!(checkpoints.contains(&OBJECT_PAYLOAD));
    assert!(checkpoints.contains(&OBJECT_ENVELOPE));
    assert!(checkpoints.contains(&OBJECT_BOUNDARY));
}

#[test]
fn cancellation_source_change_and_terminal_replay_are_stable() {
    let fixture = object_stream_fixture(0xb4, false);
    let document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let empty = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let cancellation = AtomicBool::new(false);
    let mut cancelled = document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    assert!(matches!(
        cancelled.poll(&empty, &cancellation),
        AcquiredObjectPoll::Pending { .. }
    ));
    cancellation.store(true, Ordering::Release);
    let error = failure(cancelled.poll(&empty, &cancellation));
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
    assert_eq!(failure(cancelled.poll(&empty, &NeverCancelled)), error);

    struct SnapshotOnly(SourceSnapshot);
    impl ByteSource for SnapshotOnly {
        fn snapshot(&self) -> SourceSnapshot {
            self.0
        }

        fn poll(&self, _: ReadRequest) -> ReadPoll<ByteSlice> {
            panic!("snapshot mismatch must precede lower polling")
        }
    }
    let changed = SnapshotOnly(snapshot(fixture.snapshot.len().unwrap(), 0xe4));
    let mut mismatch = document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    assert_eq!(
        failure(mismatch.poll(&changed, &NeverCancelled)).code(),
        DocumentErrorCode::SourceSnapshotMismatch
    );

    let full = supplied_store(&fixture);
    let mut complete = document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    assert!(matches!(
        complete.poll(&full, &NeverCancelled),
        AcquiredObjectPoll::Ready(_)
    ));
    assert_eq!(
        failure(complete.poll(&changed, &cancellation)).code(),
        DocumentErrorCode::JobAlreadyComplete
    );
}

#[test]
fn object_aggregate_limits_accept_measured_exact_and_reject_one_less() {
    let fixture = object_stream_fixture(0xb5, true);
    let store = supplied_store(&fixture);
    let measured_document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let measured = ready_object(&measured_document, &store, ObjectRef::new(1, 0).unwrap()).stats();
    assert!(measured.total_read_bytes() > 0);
    assert!(measured.total_parse_bytes() > 0);
    assert!(measured.retained_proof_bytes() > 0);
    assert!(measured.admitted_retained_bound_bytes() >= measured.retained_proof_bytes());

    let exact = SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
        max_object_read_bytes: measured.total_read_bytes(),
        max_object_parse_bytes: measured.total_parse_bytes(),
        max_object_retained_bytes: measured.admitted_retained_bound_bytes(),
        ..SourceAcquiredDocumentLimitConfig::default()
    })
    .unwrap();
    let exact_document = document_with(&fixture, exact);
    assert!(
        ready_object(&exact_document, &store, ObjectRef::new(1, 0).unwrap())
            .value()
            .is_ok()
    );

    for (config, expected) in [
        (
            SourceAcquiredDocumentLimitConfig {
                max_object_read_bytes: measured.total_read_bytes() - 1,
                max_object_parse_bytes: measured.total_parse_bytes(),
                max_object_retained_bytes: measured.admitted_retained_bound_bytes(),
                ..SourceAcquiredDocumentLimitConfig::default()
            },
            DocumentLimitKind::AcquiredObjectReadBytes,
        ),
        (
            SourceAcquiredDocumentLimitConfig {
                max_object_read_bytes: measured.total_read_bytes(),
                max_object_parse_bytes: measured.total_parse_bytes() - 1,
                max_object_retained_bytes: measured.admitted_retained_bound_bytes(),
                ..SourceAcquiredDocumentLimitConfig::default()
            },
            DocumentLimitKind::AcquiredObjectParseBytes,
        ),
    ] {
        let limits = SourceAcquiredDocumentLimits::validate(config).unwrap();
        let document = document_with(&fixture, limits);
        let mut job = document
            .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
            .unwrap();
        let error = failure(job.poll(&store, &NeverCancelled));
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), expected);
    }

    let one_less = SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
        max_object_read_bytes: measured.total_read_bytes(),
        max_object_parse_bytes: measured.total_parse_bytes(),
        max_object_retained_bytes: measured.admitted_retained_bound_bytes() - 1,
        ..SourceAcquiredDocumentLimitConfig::default()
    })
    .unwrap();
    let document = document_with(&fixture, one_less);
    let error = document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    let limit = error.limit().unwrap();
    assert_eq!(limit.kind(), DocumentLimitKind::AcquiredObjectRetainedBytes);
    assert_eq!(limit.consumed(), 0, "no lower child work was admitted");
    assert_eq!(limit.attempted(), measured.admitted_retained_bound_bytes());
}

#[test]
fn parent_lent_envelope_and_decoder_caps_report_exact_acquired_aggregates() {
    let direct_fixture = traditional_fixture(0xbd);
    let direct_limits = SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
        max_object_read_bytes: 1,
        ..SourceAcquiredDocumentLimitConfig::default()
    })
    .unwrap();
    let direct_document = document_with(&direct_fixture, direct_limits);
    let direct_store = supplied_store(&direct_fixture);
    let mut direct = direct_document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    let error = failure(direct.poll(&direct_store, &NeverCancelled));
    let detail = error.limit().unwrap();
    assert_eq!(detail.kind(), DocumentLimitKind::AcquiredObjectReadBytes);
    assert_eq!(detail.limit(), 1);
    assert_eq!(detail.consumed(), 0);
    let EffectiveObjectLocator::Uncompressed(locator) = direct_document.locator(1).unwrap() else {
        panic!("traditional Catalog must be uncompressed");
    };
    let expected_attempt = direct_limits
        .resolver()
        .object()
        .initial_envelope_bytes()
        .min(
            locator
                .object_upper_bound()
                .checked_sub(locator.offset())
                .unwrap()
                + 1,
        );
    assert_eq!(detail.attempted(), expected_attempt);
    assert_eq!(direct.stats().total_read_bytes(), 0);

    let filtered_fixture = object_stream_fixture_with_padding(0xbe, true, 8 * 1024);
    let filtered_store = supplied_store(&filtered_fixture);
    let baseline_document =
        document_with(&filtered_fixture, SourceAcquiredDocumentLimits::default());
    let baseline = ready_object(
        &baseline_document,
        &filtered_store,
        ObjectRef::new(1, 0).unwrap(),
    )
    .stats();
    let resolver_parse = baseline.resolver().total_parse_bytes();
    let decoder_parent_cap = resolver_parse + 32;
    assert!(baseline.decode_output_bytes() > 32);
    let decoder_limits =
        SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
            max_object_parse_bytes: decoder_parent_cap,
            ..SourceAcquiredDocumentLimitConfig::default()
        })
        .unwrap();
    let decoder_document = document_with(&filtered_fixture, decoder_limits);
    let mut decoder = decoder_document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    let error = failure(decoder.poll(&filtered_store, &NeverCancelled));
    let detail = error.limit().unwrap();
    assert_eq!(detail.kind(), DocumentLimitKind::AcquiredObjectParseBytes);
    assert_eq!(detail.limit(), decoder_parent_cap);
    assert_eq!(detail.consumed(), decoder_parent_cap - 1);
    assert_eq!(
        detail.attempted(),
        2,
        "one rejected decoder byte plus one reserved semantic byte"
    );
    assert_eq!(decoder.stats().decode_output_bytes(), 0);
    assert_eq!(decoder.stats().total_parse_bytes(), resolver_parse);
}

#[test]
fn parent_lent_object_stream_syntax_cap_reports_exact_acquired_aggregate() {
    let fixture = object_stream_fixture_with_padding(0xc0, false, 1024);
    let store = supplied_store(&fixture);
    let baseline_document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let baseline = ready_object(&baseline_document, &store, ObjectRef::new(1, 0).unwrap()).stats();
    let resolver_parse = baseline.resolver().total_parse_bytes();
    let semantic_parse = baseline.object_stream().unwrap().syntax_input_bytes();
    assert!(semantic_parse > 1);

    let exact_cap = resolver_parse + semantic_parse;
    let exact_limits = SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
        max_object_parse_bytes: exact_cap,
        ..SourceAcquiredDocumentLimitConfig::default()
    })
    .unwrap();
    let exact_document = document_with(&fixture, exact_limits);
    assert!(
        ready_object(&exact_document, &store, ObjectRef::new(1, 0).unwrap())
            .value()
            .is_ok()
    );

    let one_less_cap = exact_cap - 1;
    let one_less_limits =
        SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
            max_object_parse_bytes: one_less_cap,
            ..SourceAcquiredDocumentLimitConfig::default()
        })
        .unwrap();
    let one_less_document = document_with(&fixture, one_less_limits);
    let mut one_less = one_less_document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    let error = failure(one_less.poll(&store, &NeverCancelled));
    let detail = error.limit().unwrap();
    assert_eq!(detail.kind(), DocumentLimitKind::AcquiredObjectParseBytes);
    assert_eq!(detail.limit(), one_less_cap);
    assert_eq!(detail.consumed(), resolver_parse);
    assert_eq!(detail.attempted(), semantic_parse);
    assert_eq!(one_less.stats().decode_output_bytes(), 0);
    assert!(one_less.stats().object_stream().is_none());
}

#[test]
fn intrinsic_encoded_payload_cap_is_not_relabelled_as_an_acquired_aggregate() {
    let fixture = object_stream_fixture_with_padding(0xbf, true, 1024);
    let store = supplied_store(&fixture);
    let baseline_document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let baseline = ready_object(&baseline_document, &store, ObjectRef::new(1, 0).unwrap()).stats();
    assert!(baseline.payload_read_bytes() > 64);
    let decode = DecodeLimits::validate(DecodeLimitConfig {
        max_input_bytes: 64,
        ..DecodeLimitConfig::default()
    })
    .unwrap();
    let limits = SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
        decode,
        ..SourceAcquiredDocumentLimitConfig::default()
    })
    .unwrap();
    let document = document_with(&fixture, limits);
    let mut job = document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    let error = failure(job.poll(&store, &NeverCancelled));
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert!(
        error.limit().is_none(),
        "the intrinsic Decode input profile is not the acquired aggregate"
    );
    assert!(job.stats().resolver().total_read_bytes() > 0);
    assert_eq!(job.stats().payload_read_bytes(), 0);

    let aggregate_cap = baseline.resolver().total_read_bytes() + 32;
    let aggregate_limits =
        SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
            decode,
            max_object_read_bytes: aggregate_cap,
            ..SourceAcquiredDocumentLimitConfig::default()
        })
        .unwrap();
    let aggregate_document = document_with(&fixture, aggregate_limits);
    let mut aggregate = aggregate_document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    let error = failure(aggregate.poll(&store, &NeverCancelled));
    let detail = error.limit().unwrap();
    assert_eq!(detail.kind(), DocumentLimitKind::AcquiredObjectReadBytes);
    assert_eq!(detail.limit(), aggregate_cap);
    assert_eq!(detail.consumed(), baseline.resolver().total_read_bytes());
    assert_eq!(detail.attempted(), baseline.payload_read_bytes());
}

#[test]
fn indirect_length_retained_peak_is_admitted_before_either_child_polls() {
    let fixture = malformed_indirect_length_fixture(0xb9);
    let store = supplied_store(&fixture);
    let measured_document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let admitted = measured_document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap()
        .stats()
        .admitted_retained_bound_bytes();

    let exact_limits = SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
        max_object_retained_bytes: admitted,
        ..SourceAcquiredDocumentLimitConfig::default()
    })
    .unwrap();
    let exact_document = document_with(&fixture, exact_limits);
    let mut exact = exact_document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    assert_eq!(exact.stats().resolver_peak_retained_bytes(), 0);
    let error = failure(exact.poll(&store, &NeverCancelled));
    assert_eq!(error.code(), DocumentErrorCode::InvalidIndirectLength);
    let stats = exact.stats();
    let target = stats.resolver().object().retained_heap_bytes();
    let length = stats.resolver().length_dependency().retained_heap_bytes();
    assert!(target > 0);
    assert!(
        length > target,
        "the malformed Length child is intentionally large"
    );
    assert_eq!(
        stats.resolver_peak_retained_bytes(),
        target + length,
        "the target envelope and malformed Length syntax are simultaneously accounted"
    );
    assert!(stats.resolver_peak_retained_bytes() <= admitted);

    let one_less_limits =
        SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
            max_object_retained_bytes: admitted - 1,
            ..SourceAcquiredDocumentLimitConfig::default()
        })
        .unwrap();
    let one_less_document = document_with(&fixture, one_less_limits);
    let error = one_less_document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap_err();
    let limit = error.limit().unwrap();
    assert_eq!(limit.kind(), DocumentLimitKind::AcquiredObjectRetainedBytes);
    assert_eq!(
        limit.consumed(),
        0,
        "neither resolver child was created or polled"
    );
    assert_eq!(limit.attempted(), admitted);
}

#[test]
fn locator_terminal_errors_precede_tight_retained_admission() {
    let tight = SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
        max_object_retained_bytes: 1,
        ..SourceAcquiredDocumentLimitConfig::default()
    })
    .unwrap();
    let fixture = object_stream_fixture(0xba, false);
    let document = document_with(&fixture, tight);
    for (reference, expected) in [
        (
            ObjectRef::new(99, 0).unwrap(),
            DocumentErrorCode::MissingObject,
        ),
        (ObjectRef::new(3, 0).unwrap(), DocumentErrorCode::FreeObject),
        (
            ObjectRef::new(4, 1).unwrap(),
            DocumentErrorCode::GenerationMismatch,
        ),
    ] {
        let error = document
            .open_object(reference, object_context())
            .unwrap_err();
        assert_eq!(error.code(), expected);
        assert!(error.limit().is_none());
    }
    let retained = document
        .open_object(ObjectRef::new(4, 0).unwrap(), object_context())
        .unwrap_err();
    assert_eq!(
        retained.limit().unwrap().kind(),
        DocumentLimitKind::AcquiredObjectRetainedBytes
    );

    let null_fixture = object_stream_fixture_with_padding_and_terminal(0xbb, false, 0, 3);
    let null_document = document_with(&null_fixture, tight);
    let error = null_document
        .open_object(ObjectRef::new(3, 0).unwrap(), object_context())
        .unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::NullObject);
    assert!(error.limit().is_none());
}

#[test]
fn retained_admission_precedes_owner_index_and_direct_object_work() {
    let fixture = object_stream_fixture(0xb8, true);
    let measured = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let limits = measured.limits();
    let owner_bound = measured.stats().owner_retained_bound_bytes();
    assert!(owner_bound > measured.stats().source_proof_retained_bound_bytes());
    assert!(
        measured.stats().resolver_anchor_retained_bound_bytes()
            >= measured.stats().resolver_anchor_retained_bytes()
    );

    let exact_owner = SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
        max_owner_retained_bytes: owner_bound,
        ..SourceAcquiredDocumentLimitConfig::default()
    })
    .unwrap();
    let exact_cancellation = CountingCancellation::new();
    let exact =
        SourceAcquiredDocument::new(acquired_chain(&fixture), exact_owner, &exact_cancellation)
            .unwrap();
    assert_eq!(exact.stats().owner_retained_bound_bytes(), owner_bound);
    assert!(
        exact_cancellation.calls() > 1,
        "index construction probed cancellation"
    );

    let one_less_owner =
        SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
            max_owner_retained_bytes: owner_bound - 1,
            ..SourceAcquiredDocumentLimitConfig::default()
        })
        .unwrap();
    let rejected_cancellation = CountingCancellation::new();
    let error = SourceAcquiredDocument::new(
        acquired_chain(&fixture),
        one_less_owner,
        &rejected_cancellation,
    )
    .unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    let limit = error.limit().unwrap();
    assert_eq!(
        limit.kind(),
        DocumentLimitKind::AcquiredDocumentRetainedBytes
    );
    assert_eq!(limit.consumed(), 0);
    assert_eq!(limit.attempted(), owner_bound);
    assert_eq!(
        rejected_cancellation.calls(),
        1,
        "aggregate admission must reject before clone/index cancellation work"
    );

    let direct_reference = ObjectRef::new(4, 0).unwrap();
    let direct_bound = measured
        .open_object(direct_reference, object_context())
        .unwrap()
        .stats()
        .admitted_retained_bound_bytes();
    let compressed_bound = measured
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap()
        .stats()
        .admitted_retained_bound_bytes();
    assert!(
        direct_bound < compressed_bound,
        "admission is locator-specific"
    );
    let syntax = limits.object_stream().syntax();
    assert_eq!(
        direct_bound,
        2 * (syntax.max_owned_bytes() + syntax.max_container_bytes())
    );
    let expected_compressed = direct_bound
        + FilterPlan::retained_heap_upper_bound(limits.decode().max_filters()).unwrap()
        + limits.decode().max_retained_capacity_bytes()
        + limits.object_stream().max_working_bytes()
        + limits.object_stream().max_retained_entry_bytes()
        + limits.object_stream().max_retained_value_bytes();
    assert_eq!(
        compressed_bound, expected_compressed,
        "compressed admission covers framing, canonical plan, decoder output, parser workspace, and both semantic capacities"
    );
    assert!(
        compressed_bound <= limits.max_object_retained_bytes(),
        "the default branch aggregate fits its parent ceiling"
    );
    assert!(compressed_bound < 512 * 1024 * 1024);

    let direct_one_less =
        SourceAcquiredDocumentLimits::validate(SourceAcquiredDocumentLimitConfig {
            max_object_retained_bytes: direct_bound - 1,
            ..SourceAcquiredDocumentLimitConfig::default()
        })
        .unwrap();
    let direct_document = document_with(&fixture, direct_one_less);
    let error = direct_document
        .open_object(direct_reference, object_context())
        .unwrap_err();
    let limit = error.limit().unwrap();
    assert_eq!(limit.kind(), DocumentLimitKind::AcquiredObjectRetainedBytes);
    assert_eq!(limit.consumed(), 0, "direct framing never started");
    assert_eq!(limit.attempted(), direct_bound);
}

#[test]
fn traditional_acquisition_remains_a_valid_direct_resolver_owner() {
    let fixture = traditional_fixture(0xb6);
    let store = supplied_store(&fixture);
    let document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let object = ready_object(&document, &store, ObjectRef::new(1, 0).unwrap());
    assert!(matches!(
        object.value().unwrap(),
        AcquiredObjectValue::Uncompressed(value)
            if matches!(value.value(), SyntaxObject::Dictionary(_))
    ));
}

#[test]
fn compressed_indirect_length_dependency_is_explicitly_unsupported() {
    let fixture = compressed_length_fixture(0xb7);
    let store = supplied_store(&fixture);
    let document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let mut job = document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    let error = failure(job.poll(&store, &NeverCancelled));
    assert_eq!(error.code(), DocumentErrorCode::UnsupportedCompressedObject);
    assert_eq!(error.reference(), Some(ObjectRef::new(6, 0).unwrap()));
}

#[test]
fn latest_uncompressed_indirect_length_frames_and_decodes_object_stream_e2e() {
    let fixture = latest_uncompressed_indirect_length_fixture(0xbc);
    let document = document_with(&fixture, SourceAcquiredDocumentLimits::default());
    let length = document.locator(6).unwrap();
    assert_eq!(
        length.provenance().revision().ordinal(),
        1,
        "the incremental uncompressed Length must win over the wrong base value"
    );

    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = document
        .open_object(ObjectRef::new(1, 0).unwrap(), object_context())
        .unwrap();
    let mut checkpoints = Vec::new();
    let object = loop {
        match job.poll(&store, &NeverCancelled) {
            AcquiredObjectPoll::Pending {
                missing,
                checkpoint,
                ..
            } => {
                checkpoints.push(checkpoint);
                supply_missing(&store, &fixture, &missing, true);
            }
            AcquiredObjectPoll::Ready(object) => break object,
            AcquiredObjectPoll::Failed(error) => {
                panic!("effective indirect-Length acquisition failed: {error:?}")
            }
        }
        assert!(checkpoints.len() < 16, "bounded child phases must progress");
    };

    assert!(checkpoints.contains(&OBJECT_ENVELOPE));
    assert!(checkpoints.contains(&LENGTH_ENVELOPE));
    assert!(checkpoints.contains(&OBJECT_BOUNDARY));
    assert!(checkpoints.contains(&OBJECT_PAYLOAD));
    let stats = object.stats();
    assert!(stats.resolver().object().boundary_attempts() > 0);
    assert!(stats.resolver().length_dependency().read_bytes() > 0);
    assert!(stats.resolver().length_dependency().parse_bytes() > 0);
    assert!(
        stats.payload_read_bytes() > 1,
        "the stale base Length was one"
    );
    assert_eq!(
        stats.total_read_bytes(),
        stats.resolver().total_read_bytes() + stats.payload_read_bytes()
    );
    assert!(stats.object_stream().is_some());
    let AcquiredObjectValue::Compressed(value) = object.value().unwrap() else {
        panic!("Catalog must remain bound to decoded object-stream coordinates");
    };
    let DecodedObject::Dictionary(dictionary) = value.value() else {
        panic!("decoded Catalog must be a dictionary");
    };
    assert!(matches!(
        dictionary.get(b"Type").unwrap().value(),
        DecodedObject::Name(name) if name.bytes() == b"Catalog"
    ));
}
