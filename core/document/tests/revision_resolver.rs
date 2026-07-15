use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    DocumentErrorCode, DocumentLimits, EffectiveObjectLocator,
    NeverCancelled as DocumentNeverCancelled, ResolveObjectJob, RevisionObjectIndex,
    RevisionResolverJobContext, RevisionResolverLimits, RevisionResolverPhase,
    RevisionResolverPoll,
};
use pdf_rs_object::{DeclaredStreamLength, NeverCancelled as ObjectNeverCancelled};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    HybridSupplement, NeverCancelled as XrefNeverCancelled, RevisionCandidate, RevisionEntry,
    RevisionLimits, compose_revision_chain,
};

const OBJECT_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(102);
const OBJECT_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(103);
const LENGTH_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(104);
const LENGTH_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(105);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    stream_offset: u64,
    length_offset: u64,
    root_offset: u64,
    startxref: u64,
}

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

fn stream_fixture() -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let stream_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Length 2 0 R >>\nstream\n");
    bytes.extend(std::iter::repeat_n(b'P', 8192));
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let length_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"2 0 obj\n8192\nendobj\n");
    let root_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"3 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let startxref = bytes.len() as u64;
    bytes.extend_from_slice(b"xref placeholder followed by immutable tail bytes\n");
    let snapshot = snapshot(bytes.len() as u64, 0x71);
    Fixture {
        bytes,
        snapshot,
        stream_offset,
        length_offset,
        root_offset,
        startxref,
    }
}

fn base_chain(fixture: &Fixture) -> pdf_rs_xref::RevisionChain {
    let candidate = RevisionCandidate::traditional(
        fixture.snapshot,
        fixture.startxref,
        4,
        ObjectRef::new(3, 0).unwrap(),
        None,
        vec![
            RevisionEntry::free(0, 0, 65_535),
            RevisionEntry::uncompressed(1, fixture.stream_offset, 0),
            RevisionEntry::uncompressed(2, fixture.length_offset, 0),
            RevisionEntry::uncompressed(3, fixture.root_offset, 0),
        ],
    );
    compose_revision_chain(
        vec![candidate],
        RevisionLimits::default(),
        &XrefNeverCancelled,
    )
    .expect("fixture revision chain is valid")
}

fn index(fixture: &Fixture) -> RevisionObjectIndex {
    RevisionObjectIndex::new(
        base_chain(fixture),
        DocumentLimits::default(),
        &DocumentNeverCancelled,
    )
    .expect("fixture object index is valid")
}

fn context() -> RevisionResolverJobContext {
    RevisionResolverJobContext::new(
        JobId::new(101),
        OBJECT_ENVELOPE,
        OBJECT_BOUNDARY,
        LENGTH_ENVELOPE,
        LENGTH_BOUNDARY,
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

fn supply_missing(store: &RangeStore, fixture: &Fixture, ranges: &[ByteRange]) {
    for range in ranges {
        let start = range.start() as usize;
        let end = range.end_exclusive() as usize;
        store
            .supply(
                RangeResponse::new(fixture.snapshot, *range, fixture.bytes[start..end].to_vec())
                    .unwrap(),
            )
            .unwrap();
    }
}

#[test]
fn index_derives_nearest_physical_successors_and_retains_provenance() {
    let fixture = stream_fixture();
    let index = index(&fixture);

    let EffectiveObjectLocator::Uncompressed(stream) = index.locator(1).unwrap() else {
        panic!("object 1 must be uncompressed");
    };
    assert_eq!(stream.offset(), fixture.stream_offset);
    assert_eq!(stream.object_upper_bound(), fixture.length_offset);
    assert_eq!(stream.provenance().revision().ordinal(), 0);
    assert_eq!(
        stream.provenance().origin(),
        pdf_rs_xref::RevisionEntryOrigin::Primary
    );

    let EffectiveObjectLocator::Uncompressed(length) = index.locator(2).unwrap() else {
        panic!("object 2 must be uncompressed");
    };
    assert_eq!(length.object_upper_bound(), fixture.root_offset);
    assert_eq!(index.stats().entries(), 4);
    assert_eq!(index.stats().uncompressed_entries(), 3);
    assert_eq!(index.stats().unique_anchors(), 4);
    assert!(index.stats().retained_anchor_bytes() >= 4 * 8);
}

#[test]
fn indirect_length_resolves_effective_integer_and_frames_exact_stream() {
    let fixture = stream_fixture();
    let index = index(&fixture);
    let store = supplied_store(&fixture);
    let mut job = ResolveObjectJob::new(
        &index,
        ObjectRef::new(1, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();

    let resolved = match job.poll(&store, &ObjectNeverCancelled) {
        RevisionResolverPoll::Ready(resolved) => resolved,
        other => panic!("fully supplied stream must resolve, got {other:?}"),
    };
    assert_eq!(job.phase(), RevisionResolverPhase::Complete);
    assert_eq!(resolved.locator().offset(), fixture.stream_offset);
    let pdf_rs_object::IndirectObjectValue::Stream(stream) = resolved.object().value() else {
        panic!("object 1 must frame as a stream");
    };
    assert_eq!(stream.data_span().len(), 8192);
    assert_eq!(stream.length_claim().value(), 8192);
    assert!(stream.length_claim().resolved_value_span().is_some());
    assert!(matches!(
        stream.length_claim().declaration(),
        DeclaredStreamLength::Indirect { reference, .. }
            if reference == ObjectRef::new(2, 0).unwrap()
    ));
    assert!(job.stats().object().read_bytes() > 0);
    assert!(job.stats().length_dependency().read_bytes() > 0);
    assert_eq!(
        job.stats().total_read_bytes(),
        job.stats().object().read_bytes() + job.stats().length_dependency().read_bytes()
    );
    assert!(job.stats().total_read_bytes() <= job.limits().max_total_object_read_bytes());
    assert!(job.stats().total_parse_bytes() <= job.limits().max_total_object_parse_bytes());
}

#[test]
fn sparse_resolution_resumes_across_object_length_and_boundary_checkpoints() {
    let fixture = stream_fixture();
    let index = index(&fixture);
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = ResolveObjectJob::new(
        &index,
        ObjectRef::new(1, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let mut checkpoints = Vec::new();

    let resolved = loop {
        match job.poll(&store, &ObjectNeverCancelled) {
            RevisionResolverPoll::Ready(resolved) => break resolved,
            RevisionResolverPoll::Pending {
                missing,
                checkpoint,
                ..
            } => {
                checkpoints.push(checkpoint);
                supply_missing(&store, &fixture, missing.as_slice());
            }
            RevisionResolverPoll::Failed(error) => panic!("sparse resolution failed: {error:?}"),
        }
        assert!(
            checkpoints.len() < 12,
            "resolver must make bounded progress"
        );
    };

    assert_eq!(
        checkpoints,
        vec![OBJECT_ENVELOPE, LENGTH_ENVELOPE, OBJECT_BOUNDARY]
    );
    assert_eq!(
        resolved.object().object_span().start(),
        fixture.stream_offset
    );
    assert!(store.cached_bytes().unwrap() < fixture.bytes.len() as u64);
    assert!(store.cached_bytes().unwrap() < 5000);
}

#[test]
fn source_change_and_cancellation_are_terminal_and_stable() {
    let fixture = stream_fixture();
    let index = index(&fixture);
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut changed = ResolveObjectJob::new(
        &index,
        ObjectRef::new(3, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    store.signal_source_changed().unwrap();
    let first = match changed.poll(&store, &ObjectNeverCancelled) {
        RevisionResolverPoll::Failed(error) => error,
        other => panic!("source change must fail, got {other:?}"),
    };
    assert_eq!(first.code(), DocumentErrorCode::SourceSnapshotMismatch);
    let repeated = match changed.poll(&store, &ObjectNeverCancelled) {
        RevisionResolverPoll::Failed(error) => error,
        other => panic!("terminal source failure must remain stable, got {other:?}"),
    };
    assert_eq!(repeated, first);

    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut cancelled = ResolveObjectJob::new(
        &index,
        ObjectRef::new(3, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let flag = AtomicBool::new(true);
    let error = match cancelled.poll(&store, &flag) {
        RevisionResolverPoll::Failed(error) => error,
        other => panic!("cancelled resolver must fail, got {other:?}"),
    };
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
    assert!(flag.load(Ordering::Acquire));
}

#[test]
fn newest_free_null_compressed_and_generation_states_never_fall_back() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let old_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n11\nendobj\n");
    let root_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"2 0 obj\n<<>>\nendobj\n");
    let base_startxref = bytes.len() as u64;
    bytes.extend_from_slice(b"old xref bytes\n");
    let new_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 1 obj\n22\nendobj\n");
    let newest_startxref = bytes.len() as u64;
    bytes.extend_from_slice(b"new xref bytes and tail\n");
    let source = snapshot(bytes.len() as u64, 0x72);
    let root = ObjectRef::new(2, 0).unwrap();
    let base = RevisionCandidate::traditional(
        source,
        base_startxref,
        4,
        root,
        None,
        vec![
            RevisionEntry::free(0, 0, 65_535),
            RevisionEntry::uncompressed(1, old_offset, 0),
            RevisionEntry::uncompressed(2, root_offset, 0),
            RevisionEntry::free(3, 0, 0),
        ],
    );

    let make_index = |entry: RevisionEntry| {
        let update = RevisionCandidate::traditional(
            source,
            newest_startxref,
            4,
            root,
            Some(base_startxref),
            vec![entry],
        );
        let chain = compose_revision_chain(
            vec![update, base.clone()],
            RevisionLimits::default(),
            &XrefNeverCancelled,
        )
        .unwrap();
        RevisionObjectIndex::new(chain, DocumentLimits::default(), &DocumentNeverCancelled).unwrap()
    };

    let free = make_index(RevisionEntry::free(1, 0, 1));
    assert!(matches!(
        free.locator(1),
        Some(EffectiveObjectLocator::Free { generation: 1, .. })
    ));
    let error = ResolveObjectJob::new(
        &free,
        ObjectRef::new(1, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::FreeObject);

    let null = make_index(RevisionEntry::null(1, 7));
    let error = ResolveObjectJob::new(
        &null,
        ObjectRef::new(1, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::NullObject);

    let compressed = make_index(RevisionEntry::compressed(1, 3, 0));
    let error = ResolveObjectJob::new(
        &compressed,
        ObjectRef::new(1, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::UnsupportedCompressedObject);

    let updated = make_index(RevisionEntry::uncompressed(1, new_offset, 1));
    let error = ResolveObjectJob::new(
        &updated,
        ObjectRef::new(1, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::GenerationMismatch);
    let EffectiveObjectLocator::Uncompressed(locator) = updated.locator(1).unwrap() else {
        panic!("newest generation must remain the effective locator");
    };
    assert_eq!(locator.offset(), new_offset);
    assert_eq!(locator.object_upper_bound(), newest_startxref);
}

#[test]
fn primary_xref_stream_self_entry_keeps_its_distinct_unsupported_boundary() {
    let fixture = stream_fixture();
    let newest_startxref = fixture.startxref + 5;
    assert!(newest_startxref < fixture.snapshot.len().unwrap());
    let root = ObjectRef::new(3, 0).unwrap();
    let base = RevisionCandidate::traditional(
        fixture.snapshot,
        fixture.startxref,
        4,
        root,
        None,
        vec![
            RevisionEntry::free(0, 0, 65_535),
            RevisionEntry::uncompressed(1, fixture.stream_offset, 0),
            RevisionEntry::uncompressed(2, fixture.length_offset, 0),
            RevisionEntry::uncompressed(3, fixture.root_offset, 0),
        ],
    );
    let newest = RevisionCandidate::xref_stream(
        fixture.snapshot,
        newest_startxref,
        ObjectRef::new(4, 0).unwrap(),
        5,
        root,
        Some(fixture.startxref),
        vec![RevisionEntry::uncompressed(4, newest_startxref, 0)],
    );
    let chain = compose_revision_chain(
        vec![newest, base],
        RevisionLimits::default(),
        &XrefNeverCancelled,
    )
    .unwrap();
    let index = RevisionObjectIndex::new(chain, DocumentLimits::default(), &DocumentNeverCancelled)
        .unwrap();
    let error = ResolveObjectJob::new(
        &index,
        ObjectRef::new(4, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap_err();
    assert_eq!(
        error.code(),
        DocumentErrorCode::UnsupportedXrefStreamContainer
    );
    assert_eq!(error.offset(), Some(newest_startxref));
}

#[test]
fn hybrid_update_resolves_supplement_length_and_preserves_revision_local_bounds() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let old_target_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n11\nendobj\n");
    let old_length_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"2 0 obj\n1\nendobj\n");
    let root_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"3 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let base_startxref = bytes.len() as u64;
    bytes.extend_from_slice(b"base xref bytes\n");

    let new_target_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Length 2 0 R >>\nstream\nDATA\nendstream\nendobj\n");
    let new_length_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"2 0 obj\n4\nendobj\n");
    let supplement_startxref = bytes.len() as u64;
    bytes.extend_from_slice(b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n");
    let newest_startxref = bytes.len() as u64;
    bytes.extend_from_slice(b"new traditional xref bytes and immutable tail\n");

    let source = snapshot(bytes.len() as u64, 0x74);
    let root = ObjectRef::new(3, 0).unwrap();
    let base = RevisionCandidate::traditional(
        source,
        base_startxref,
        5,
        root,
        None,
        vec![
            RevisionEntry::free(0, 0, 65_535),
            RevisionEntry::uncompressed(1, old_target_offset, 0),
            RevisionEntry::uncompressed(2, old_length_offset, 0),
            RevisionEntry::uncompressed(3, root_offset, 0),
            RevisionEntry::free(4, 0, 0),
        ],
    );
    let supplement = HybridSupplement::new(
        source,
        supplement_startxref,
        ObjectRef::new(4, 0).unwrap(),
        5,
        Some(base_startxref),
        vec![
            RevisionEntry::uncompressed(2, new_length_offset, 0),
            RevisionEntry::uncompressed(4, supplement_startxref, 0),
        ],
    );
    let update = RevisionCandidate::traditional(
        source,
        newest_startxref,
        5,
        root,
        Some(base_startxref),
        vec![RevisionEntry::uncompressed(1, new_target_offset, 0)],
    )
    .with_xref_stream(supplement_startxref)
    .with_hybrid_supplement(supplement);
    let chain = compose_revision_chain(
        vec![update, base],
        RevisionLimits::default(),
        &XrefNeverCancelled,
    )
    .unwrap();
    let index = RevisionObjectIndex::new(chain, DocumentLimits::default(), &DocumentNeverCancelled)
        .unwrap();

    let EffectiveObjectLocator::Uncompressed(target) = index.locator(1).unwrap() else {
        panic!("hybrid primary target must be uncompressed");
    };
    assert_eq!(target.provenance().revision().ordinal(), 1);
    assert_eq!(
        target.provenance().origin(),
        pdf_rs_xref::RevisionEntryOrigin::Primary
    );
    assert_eq!(target.object_upper_bound(), new_length_offset);

    let EffectiveObjectLocator::Uncompressed(length) = index.locator(2).unwrap() else {
        panic!("hybrid supplement length must be uncompressed");
    };
    assert_eq!(length.provenance().revision().ordinal(), 1);
    assert_eq!(
        length.provenance().origin(),
        pdf_rs_xref::RevisionEntryOrigin::HybridSupplement
    );
    assert_eq!(length.object_upper_bound(), supplement_startxref);

    let EffectiveObjectLocator::Uncompressed(container) = index.locator(4).unwrap() else {
        panic!("hybrid xref-stream container must be uncompressed");
    };
    assert_eq!(
        container.provenance().origin(),
        pdf_rs_xref::RevisionEntryOrigin::HybridSupplement
    );
    assert_eq!(container.offset(), supplement_startxref);
    assert_eq!(container.object_upper_bound(), newest_startxref);

    let EffectiveObjectLocator::Uncompressed(older_root) = index.locator(3).unwrap() else {
        panic!("older root must remain effective and uncompressed");
    };
    assert_eq!(older_root.provenance().revision().ordinal(), 0);
    assert_eq!(older_root.object_upper_bound(), base_startxref);

    let store = RangeStore::new(source, Default::default()).unwrap();
    store
        .supply(
            RangeResponse::new(
                source,
                ByteRange::new(0, bytes.len() as u64).unwrap(),
                bytes,
            )
            .unwrap(),
        )
        .unwrap();
    let mut target_job = ResolveObjectJob::new(
        &index,
        ObjectRef::new(1, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let resolved = match target_job.poll(&store, &ObjectNeverCancelled) {
        RevisionResolverPoll::Ready(resolved) => resolved,
        other => panic!("hybrid target must resolve, got {other:?}"),
    };
    let pdf_rs_object::IndirectObjectValue::Stream(stream) = resolved.object().value() else {
        panic!("hybrid target must frame as a stream");
    };
    assert_eq!(stream.data_span().len(), 4);
    assert_eq!(stream.length_claim().value(), 4);
    assert!(stream.length_claim().resolved_value_span().is_some());

    let mut container_job = ResolveObjectJob::new(
        &index,
        ObjectRef::new(4, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let container = match container_job.poll(&store, &ObjectNeverCancelled) {
        RevisionResolverPoll::Ready(resolved) => resolved,
        other => panic!("hybrid container must resolve, got {other:?}"),
    };
    assert_eq!(container.object().xref_offset(), supplement_startxref);
    assert_eq!(container.object().object_upper_bound(), newest_startxref);
}

#[test]
fn self_referential_length_is_rejected_before_boundary_framing() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let stream_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Length 1 0 R >>\nstream\nX\nendstream\nendobj\n");
    let root_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"2 0 obj\n<<>>\nendobj\n");
    let startxref = bytes.len() as u64;
    bytes.extend_from_slice(b"xref tail\n");
    let source = snapshot(bytes.len() as u64, 0x73);
    let chain = compose_revision_chain(
        vec![RevisionCandidate::traditional(
            source,
            startxref,
            3,
            ObjectRef::new(2, 0).unwrap(),
            None,
            vec![
                RevisionEntry::free(0, 0, 65_535),
                RevisionEntry::uncompressed(1, stream_offset, 0),
                RevisionEntry::uncompressed(2, root_offset, 0),
            ],
        )],
        RevisionLimits::default(),
        &XrefNeverCancelled,
    )
    .unwrap();
    let index = RevisionObjectIndex::new(chain, DocumentLimits::default(), &DocumentNeverCancelled)
        .unwrap();
    let store = RangeStore::new(source, Default::default()).unwrap();
    store
        .supply(
            RangeResponse::new(
                source,
                ByteRange::new(0, bytes.len() as u64).unwrap(),
                bytes,
            )
            .unwrap(),
        )
        .unwrap();
    let mut job = ResolveObjectJob::new(
        &index,
        ObjectRef::new(1, 0).unwrap(),
        context(),
        RevisionResolverLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let error = match job.poll(&store, &ObjectNeverCancelled) {
        RevisionResolverPoll::Failed(error) => error,
        other => panic!("self length must fail, got {other:?}"),
    };
    assert_eq!(error.code(), DocumentErrorCode::IndirectLengthCycle);
    assert_eq!(job.phase(), RevisionResolverPhase::Failed);
}
