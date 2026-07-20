use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    DocumentErrorCode, DocumentLimits, NeverCancelled as DocumentNeverCancelled, ResolveObjectJob,
    RevisionObjectIndex, RevisionResolverJobContext, RevisionResolverLimits, RevisionResolverPoll,
};
use pdf_rs_object::{
    IndirectObjectValue, NeverCancelled as ObjectNeverCancelled, ObjectStream, ObjectStreamLimits,
    parse_unfiltered_object_stream,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, RevisionCandidate, RevisionEntry, RevisionLimits,
    compose_revision_chain,
};

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    container_offset: u64,
    root_offset: u64,
    startxref: u64,
}

fn snapshot(len: u64, marker: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([marker; 32]),
            SourceRevision::new(u64::from(marker)),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [marker ^ 0xa5; 32]),
    )
}

fn reference(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).unwrap()
}

fn build_fixture(marker: u8) -> Fixture {
    let first_object = b"<< /Kind /Compressed /Next 11 0 R >>";
    let second_object = b"[10 0 R 99]";
    let header = format!("10 0 11 {} ", first_object.len() + 1);
    let mut payload = header.as_bytes().to_vec();
    payload.extend_from_slice(first_object);
    payload.push(b' ');
    payload.extend_from_slice(second_object);

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let container_offset = bytes.len() as u64;
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /ObjStm /N 2 /First {} /Length {} >>\nstream\n",
            header.len(),
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let root_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let startxref = bytes.len() as u64;
    bytes.extend_from_slice(b"xref placeholder\n");
    let snapshot = snapshot(bytes.len() as u64, marker);
    Fixture {
        bytes,
        snapshot,
        container_offset,
        root_offset,
        startxref,
    }
}

fn entries(fixture: &Fixture, compressed_index: u32, container_free: bool) -> Vec<RevisionEntry> {
    (0_u32..12)
        .map(|number| match number {
            0 => RevisionEntry::free(0, 0, 65_535),
            1 => RevisionEntry::uncompressed(1, fixture.root_offset, 0),
            5 if container_free => RevisionEntry::free(5, 0, 0),
            5 => RevisionEntry::uncompressed(5, fixture.container_offset, 0),
            10 => RevisionEntry::compressed(10, 5, compressed_index),
            11 => RevisionEntry::compressed(11, 5, 1),
            _ => RevisionEntry::free(number, 0, 0),
        })
        .collect()
}

fn index(fixture: &Fixture, compressed_index: u32, container_free: bool) -> RevisionObjectIndex {
    index_from_candidates(vec![RevisionCandidate::traditional(
        fixture.snapshot,
        fixture.startxref,
        12,
        reference(1),
        None,
        entries(fixture, compressed_index, container_free),
    )])
}

fn index_from_candidates(candidates: Vec<RevisionCandidate>) -> RevisionObjectIndex {
    let chain =
        compose_revision_chain(candidates, RevisionLimits::default(), &XrefNeverCancelled).unwrap();
    RevisionObjectIndex::new(chain, DocumentLimits::default(), &DocumentNeverCancelled).unwrap()
}

fn context() -> RevisionResolverJobContext {
    RevisionResolverJobContext::new(
        JobId::new(801),
        ResumeCheckpoint::new(802),
        ResumeCheckpoint::new(803),
        ResumeCheckpoint::new(804),
        ResumeCheckpoint::new(805),
        RequestPriority::VisiblePage,
    )
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let range = ByteRange::new(0, fixture.bytes.len() as u64).unwrap();
    store
        .supply(RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone()).unwrap())
        .unwrap();
    store
}

fn payload_slice(store: &RangeStore, resolved: &pdf_rs_document::ResolvedObject) -> ByteSlice {
    let IndirectObjectValue::Stream(stream) = resolved.object().value() else {
        panic!("resolved container must be a stream")
    };
    match store.poll(ReadRequest::new(
        ByteRange::new(stream.data_span().start(), stream.data_span().len()).unwrap(),
        RequestPriority::VisiblePage,
        JobId::new(806),
        ResumeCheckpoint::new(807),
    )) {
        ReadPoll::Ready(bytes) => bytes,
        other => panic!("supplied object-stream payload must be ready: {other:?}"),
    }
}

fn parsed_stream(fixture: &Fixture, index: &RevisionObjectIndex) -> ObjectStream {
    let store = supplied_store(fixture);
    let mut job = ResolveObjectJob::new(
        index,
        reference(5),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let resolved = match job.poll(&store, &ObjectNeverCancelled) {
        RevisionResolverPoll::Ready(resolved) => resolved,
        other => panic!("container must resolve from its effective offset: {other:?}"),
    };
    let payload = payload_slice(&store, &resolved);
    parse_unfiltered_object_stream(
        resolved.object(),
        &payload,
        ObjectStreamLimits::default(),
        &ObjectNeverCancelled,
    )
    .unwrap()
}

#[test]
fn compressed_xref_row_binds_exact_container_index_and_object_number() {
    let fixture = build_fixture(0x61);
    let index = index(&fixture, 0, false);
    let stream = parsed_stream(&fixture, &index);

    let resolved = index.resolve_compressed(reference(10), &stream).unwrap();
    assert_eq!(resolved.locator().object_stream(), 5);
    assert_eq!(resolved.locator().index(), 0);
    assert_eq!(
        resolved.container_locator().offset(),
        fixture.container_offset
    );
    assert_eq!(resolved.entry().object_number(), 10);
    assert_eq!(resolved.entry().index(), 0);
    assert!(resolved.entry().value().value().as_dictionary().is_some());
    assert_eq!(resolved.stream().container(), reference(5));

    let second = index.resolve_compressed(reference(11), &stream).unwrap();
    assert_eq!(second.entry().index(), 1);
    assert_eq!(second.entry().object_number(), 11);
}

#[test]
fn index_number_generation_container_and_snapshot_mismatches_are_rejected() {
    let fixture = build_fixture(0x62);
    let valid_index = index(&fixture, 0, false);
    let stream = parsed_stream(&fixture, &valid_index);

    let wrong_index = index(&fixture, 1, false);
    assert_eq!(
        wrong_index
            .resolve_compressed(reference(10), &stream)
            .unwrap_err()
            .code(),
        DocumentErrorCode::CompressedObjectMismatch
    );

    let missing_container = index(&fixture, 0, true);
    assert_eq!(
        missing_container
            .resolve_compressed(reference(10), &stream)
            .unwrap_err()
            .code(),
        DocumentErrorCode::InvalidObjectStreamContainer
    );

    assert_eq!(
        valid_index
            .resolve_compressed(ObjectRef::new(10, 1).unwrap(), &stream)
            .unwrap_err()
            .code(),
        DocumentErrorCode::GenerationMismatch
    );
    assert_eq!(
        valid_index
            .resolve_compressed(reference(1), &stream)
            .unwrap_err()
            .code(),
        DocumentErrorCode::NotCompressedObject
    );

    let foreign_fixture = build_fixture(0x63);
    let foreign_index = index(&foreign_fixture, 0, false);
    let foreign_stream = parsed_stream(&foreign_fixture, &foreign_index);
    assert_eq!(
        valid_index
            .resolve_compressed(reference(10), &foreign_stream)
            .unwrap_err()
            .code(),
        DocumentErrorCode::SourceSnapshotMismatch
    );
}

#[test]
fn newest_revision_cannot_reuse_a_stale_object_stream_container() {
    let mut fixture = build_fixture(0x64);
    let old_startxref = fixture.startxref;
    let replacement_offset = fixture.bytes.len() as u64;
    fixture.bytes.extend_from_slice(b"5 0 obj\nnull\nendobj\n");
    let new_startxref = fixture.bytes.len() as u64;
    fixture.bytes.extend_from_slice(b"xref update\n");
    fixture.snapshot = snapshot(fixture.bytes.len() as u64, 0x64);

    let base_index = index(&fixture, 0, false);
    let stale_stream = parsed_stream(&fixture, &base_index);
    let newest = RevisionCandidate::traditional(
        fixture.snapshot,
        new_startxref,
        12,
        reference(1),
        Some(old_startxref),
        vec![RevisionEntry::uncompressed(5, replacement_offset, 0)],
    );
    let base = RevisionCandidate::traditional(
        fixture.snapshot,
        old_startxref,
        12,
        reference(1),
        None,
        entries(&fixture, 0, false),
    );
    let updated = index_from_candidates(vec![newest, base]);

    assert_eq!(
        updated
            .resolve_compressed(reference(10), &stale_stream)
            .unwrap_err()
            .code(),
        DocumentErrorCode::InvalidObjectStreamContainer
    );
}

#[test]
fn newest_free_or_null_container_definition_masks_older_stream() {
    let fixture = build_fixture(0x65);
    let base_index = index(&fixture, 0, false);
    let stale_stream = parsed_stream(&fixture, &base_index);
    let new_startxref = fixture.bytes.len() as u64 - 1;

    for masked in [RevisionEntry::free(5, 0, 0), RevisionEntry::null(5, 9)] {
        let newest = RevisionCandidate::traditional(
            fixture.snapshot,
            new_startxref,
            12,
            reference(1),
            Some(fixture.startxref),
            vec![masked],
        );
        let base = RevisionCandidate::traditional(
            fixture.snapshot,
            fixture.startxref,
            12,
            reference(1),
            None,
            entries(&fixture, 0, false),
        );
        let updated = index_from_candidates(vec![newest, base]);
        assert_eq!(
            updated
                .resolve_compressed(reference(10), &stale_stream)
                .unwrap_err()
                .code(),
            DocumentErrorCode::InvalidObjectStreamContainer
        );
    }
}

#[test]
fn object_stream_container_cannot_itself_be_compressed() {
    let fixture = build_fixture(0x66);
    let valid_index = index(&fixture, 0, false);
    let stream = parsed_stream(&fixture, &valid_index);
    let mut nested_entries = entries(&fixture, 0, false);
    nested_entries[5] = RevisionEntry::compressed(5, 6, 0);
    nested_entries[6] = RevisionEntry::uncompressed(6, fixture.container_offset, 0);
    let nested_index = index_from_candidates(vec![RevisionCandidate::traditional(
        fixture.snapshot,
        fixture.startxref,
        12,
        reference(1),
        None,
        nested_entries,
    )]);

    assert_eq!(
        nested_index
            .resolve_compressed(reference(10), &stream)
            .unwrap_err()
            .code(),
        DocumentErrorCode::InvalidObjectStreamContainer
    );
}
