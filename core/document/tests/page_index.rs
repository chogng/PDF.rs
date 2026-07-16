use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AttestRevisionJob, AttestedRevisionIndex, BuildPageIndexJob, CandidateRevisionIndex,
    DocumentError, DocumentErrorCode, DocumentLimitKind, DocumentLimits, LookupPageJob,
    NeverCancelled as DocumentNeverCancelled, PageHandle, PageIndex, PageIndexBuildPoll,
    PageIndexLimits, PageIndexSegmentKind, PageLookup, PageLookupPhase, PageLookupPoll,
    PageLookupStats, PageSegmentEvidence, PageSegmentSummary, PageTreeJobContext,
    PageTreeLimitConfig, PageTreeLimits, RevisionAttestationJobContext, RevisionAttestationLimits,
    RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(61);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

fn snapshot(len: u64, salt: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([salt; 32]),
            SourceRevision::new(u64::from(salt) + 1),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [salt ^ 0x9d; 32]),
    )
}

fn fixture(bodies: &[(u32, &[u8])], size: u32, salt: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut in_use = Vec::new();
    for &(number, body) in bodies {
        let offset = u64::try_from(bytes.len()).expect("fixture offset fits u64");
        in_use.push((number, offset));
        bytes.extend_from_slice(body);
    }
    let startxref = u64::try_from(bytes.len()).expect("fixture length fits u64");
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for number in 0..size {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else if let Some((_, offset)) = in_use.iter().find(|&&(entry, _)| entry == number) {
            format!("{offset:010} 00000 n \n")
        } else {
            "0000000000 00000 f \n".to_owned()
        };
        assert_eq!(row.len(), 20);
        bytes.extend_from_slice(row.as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
            .as_bytes(),
    );
    Fixture {
        snapshot: snapshot(
            u64::try_from(bytes.len()).expect("fixture length fits u64"),
            salt,
        ),
        bytes,
    }
}

fn two_subtree_fixture(salt: u8) -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 4 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>\nendobj\n",
            ),
            (4, b"4 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n"),
            (5, b"5 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n"),
            (
                6,
                b"6 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [7 0 R 8 0 R] /Count 2 >>\nendobj\n",
            ),
            (7, b"7 0 obj\n<< /Type /Page /Parent 6 0 R >>\nendobj\n"),
            (8, b"8 0 obj\n<< /Type /Page /Parent 6 0 R >>\nendobj\n"),
        ],
        9,
        salt,
    )
}

fn ancestor_cycle_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [2 0 R] /Count 1 >>\nendobj\n",
            ),
        ],
        4,
        0xc1,
    )
}

fn duplicate_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>\nendobj\n",
            ),
            (3, b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n"),
        ],
        4,
        0xc2,
    )
}

fn unrelated_deep_count_mismatch_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 4 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>\nendobj\n",
            ),
            (4, b"4 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n"),
            (5, b"5 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n"),
            (
                6,
                b"6 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [7 0 R] /Count 2 >>\nendobj\n",
            ),
            (
                7,
                b"7 0 obj\n<< /Type /Pages /Parent 6 0 R /Kids [8 0 R] /Count 2 >>\nendobj\n",
            ),
            (8, b"8 0 obj\n<< /Type /Page /Parent 7 0 R >>\nendobj\n"),
        ],
        9,
        0xc3,
    )
}

fn unrelated_deep_cycle_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 4 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>\nendobj\n",
            ),
            (4, b"4 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n"),
            (5, b"5 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n"),
            (
                6,
                b"6 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [7 0 R] /Count 2 >>\nendobj\n",
            ),
            (
                7,
                b"7 0 obj\n<< /Type /Pages /Parent 6 0 R /Kids [6 0 R] /Count 2 >>\nendobj\n",
            ),
        ],
        8,
        0xc4,
    )
}

fn seven_node_incremental_fixture(salt: u8) -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 3 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>\nendobj\n",
            ),
            (4, b"4 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n"),
            (5, b"5 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n"),
            (
                6,
                b"6 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [7 0 R] /Count 1 >>\nendobj\n",
            ),
            (
                7,
                b"7 0 obj\n<< /Type /Pages /Parent 6 0 R /Kids [8 0 R] /Count 1 >>\nendobj\n",
            ),
            (8, b"8 0 obj\n<< /Type /Page /Parent 7 0 R >>\nendobj\n"),
        ],
        9,
        salt,
    )
}

fn cross_sibling_duplicate_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 >>\nendobj\n",
            ),
            (
                4,
                b"4 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 >>\nendobj\n",
            ),
            (5, b"5 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n"),
        ],
        6,
        0xc5,
    )
}

fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("test object reference is nonzero")
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let range = ByteRange::new(
        0,
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
    )
    .unwrap();
    store
        .supply(RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone()).unwrap())
        .unwrap();
    store
}

fn parsed_xref(fixture: &Fixture) -> XrefSection {
    let store = supplied_store(fixture);
    let mut job = OpenXrefJob::new(
        fixture.snapshot,
        XrefJobContext::new(
            JobId::new(6_101),
            ResumeCheckpoint::new(6_102),
            ResumeCheckpoint::new(6_103),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &XrefNeverCancelled) {
        XrefPoll::Ready(section) => section,
        XrefPoll::Pending { .. } => panic!("complete xref source must not suspend"),
        XrefPoll::Failed(error) => panic!("self-authored xref must parse: {error}"),
    }
}

fn ready_index(fixture: &Fixture) -> AttestedRevisionIndex {
    let candidate = CandidateRevisionIndex::from_xref(
        &parsed_xref(fixture),
        REVISION_ID,
        DocumentLimits::default(),
        &DocumentNeverCancelled,
    )
    .expect("self-authored xref yields a candidate");
    let store = supplied_store(fixture);
    let mut job = AttestRevisionJob::new(
        candidate,
        RevisionAttestationJobContext::new(
            JobId::new(6_201),
            ResumeCheckpoint::new(6_202),
            ResumeCheckpoint::new(6_203),
            ResumeCheckpoint::new(6_204),
            RequestPriority::Metadata,
        ),
        RevisionAttestationLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Ready(index) => index,
        RevisionAttestationPoll::Pending { .. } => {
            panic!("complete attestation source must not suspend")
        }
        RevisionAttestationPoll::Failed(error) => {
            panic!("self-authored revision must attest: {error}")
        }
    }
}

fn context(seed: u64) -> PageTreeJobContext {
    PageTreeJobContext::new(
        JobId::new(seed),
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        RequestPriority::VisiblePage,
    )
}

fn tree_limits_with_nodes(max_nodes: u64) -> PageTreeLimits {
    PageTreeLimits::validate(PageTreeLimitConfig {
        max_nodes,
        max_depth: 4,
        max_pages: 4,
        max_kids_per_node: 4,
        max_total_object_read_bytes: 1 << 20,
        max_total_object_parse_bytes: 1 << 20,
        max_retained_traversal_bytes: 8 << 10,
    })
    .expect("test page-tree limits validate")
}

fn tree_limits() -> PageTreeLimits {
    tree_limits_with_nodes(8)
}

fn index_limits() -> PageIndexLimits {
    PageIndexLimits::new(4, 16 << 10).expect("test page-index limits validate")
}

fn build_ready(authority: &AttestedRevisionIndex, source: &dyn ByteSource, seed: u64) -> PageIndex {
    build_ready_with_limits(authority, source, seed, tree_limits())
}

fn build_ready_with_limits(
    authority: &AttestedRevisionIndex,
    source: &dyn ByteSource,
    seed: u64,
    limits: PageTreeLimits,
) -> PageIndex {
    let mut job = authority
        .build_page_index(context(seed), limits, index_limits())
        .expect("valid page-index build job");
    match job.poll(source, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Ready(index) => index,
        PageIndexBuildPoll::Pending { .. } => panic!("complete source must not suspend"),
        PageIndexBuildPoll::Failed(error) => panic!("valid page index must build: {error}"),
    }
}

fn lookup_ready(
    authority: &AttestedRevisionIndex,
    index: &PageIndex,
    target: u32,
    source: &dyn ByteSource,
    seed: u64,
) -> (PageLookup, PageLookupStats) {
    lookup_ready_with_limits(authority, index, target, source, seed, tree_limits())
}

fn lookup_ready_with_limits(
    authority: &AttestedRevisionIndex,
    index: &PageIndex,
    target: u32,
    source: &dyn ByteSource,
    seed: u64,
    limits: PageTreeLimits,
) -> (PageLookup, PageLookupStats) {
    let mut job = authority
        .lookup_page(index, target, context(seed), limits)
        .expect("valid page lookup job");
    let result = match job.poll(source, &DocumentNeverCancelled) {
        PageLookupPoll::Ready(result) => result,
        PageLookupPoll::Pending { .. } => panic!("complete source must not suspend"),
        PageLookupPoll::Failed(error) => panic!("valid page lookup must succeed: {error}"),
    };
    (result, job.stats())
}

fn assert_segment(
    index: &PageIndex,
    object: ObjectRef,
    start_index: u32,
    page_count: u32,
    kind: PageIndexSegmentKind,
) -> &PageSegmentSummary {
    let segment = index
        .segments()
        .iter()
        .find(|segment| segment.object() == object)
        .unwrap_or_else(|| panic!("missing page-index segment for {object:?}"));
    assert_eq!(segment.start_index(), start_index);
    assert_eq!(segment.page_count(), page_count);
    assert_eq!(segment.end_index(), start_index + page_count);
    assert_eq!(segment.kind(), kind);
    segment
}

struct PanicSource(SourceSnapshot);

impl ByteSource for PanicSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("cached page lookup must not poll the byte source")
    }
}

struct Cancelled;

impl pdf_rs_document::DocumentCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

fn assert_zero_lookup_work(stats: PageLookupStats) {
    assert_eq!(stats.objects_started(), 0);
    assert_eq!(stats.nodes_classified(), 0);
    assert_eq!(stats.segments_refined(), 0);
    assert_eq!(stats.object_read_bytes(), 0);
    assert_eq!(stats.object_parse_bytes(), 0);
}

#[test]
fn cold_build_retains_only_root_evidence_then_lookups_upgrade_requested_subtrees() {
    let fixture = two_subtree_fixture(0xa1);
    let authority = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let initial = build_ready(&authority, &store, 6_301);

    assert_eq!(initial.catalog().snapshot(), fixture.snapshot);
    assert_eq!(initial.len(), 4);
    assert!(!initial.is_complete());
    assert_eq!(initial.segments().len(), 1);
    let build_stats = initial.stats();
    assert_eq!(build_stats.objects_started(), 2);
    assert_eq!(build_stats.nodes_started(), 1);
    assert_eq!(build_stats.exact_pages(), 0);
    assert_eq!(build_stats.max_depth(), 1);
    assert_eq!(build_stats.max_kids_per_node(), 2);
    assert!(build_stats.object_read_bytes() > 0);
    assert!(build_stats.object_parse_bytes() > 0);
    assert!(build_stats.peak_retained_traversal_bytes() > 0);
    assert!(!build_stats.has_complete_tree_proof());

    let root = assert_segment(&initial, object_ref(2), 0, 4, PageIndexSegmentKind::Pages);
    assert_eq!(root.parent(), None);
    assert_eq!(root.depth(), 1);
    assert_eq!(root.declared_count(), 4);
    assert_eq!(root.evidence(), PageSegmentEvidence::DeclaredCount);
    assert_eq!(root.validated_count(), None);
    assert_eq!(root.partitioned_count(), None);
    assert!(root.count_offset().is_some());
    assert_eq!(root.retained_kid_count(), Some(2));

    let (page_one, first_stats) = lookup_ready(&authority, &initial, 1, &store, 6_401);
    assert_eq!(first_stats.objects_started(), 4);
    assert_eq!(first_stats.nodes_classified(), 4);
    assert_eq!(first_stats.segments_refined(), 2);
    let page_one_handle: PageHandle = page_one.handle();
    assert_eq!(page_one_handle.index(), 1);
    assert_eq!(page_one_handle.object(), object_ref(5));
    assert_eq!(page_one_handle.snapshot(), fixture.snapshot);
    assert_eq!(page_one_handle.revision_id(), REVISION_ID);
    assert_eq!(page_one_handle.catalog_root(), object_ref(1));
    assert_eq!(page_one_handle.page_tree_root(), object_ref(2));
    assert_eq!(page_one_handle.document_page_count(), 4);
    assert_eq!(
        page_one_handle.document_page_count_evidence(),
        PageSegmentEvidence::ValidatedPartition
    );
    assert_eq!(page_one.page_index().page(1), Some(object_ref(5)));
    let (refined, returned_handle) = page_one.into_parts();
    assert_eq!(returned_handle, page_one_handle);
    refined.validate_handle(returned_handle).unwrap();
    let refined_root = assert_segment(&refined, object_ref(2), 0, 4, PageIndexSegmentKind::Pages);
    assert_eq!(
        refined_root.evidence(),
        PageSegmentEvidence::ValidatedPartition
    );
    assert_eq!(refined_root.validated_count(), None);
    assert_eq!(refined_root.partitioned_count(), Some(4));
    assert_eq!(refined_root.retained_kid_count(), Some(2));
    let selected_parent =
        assert_segment(&refined, object_ref(3), 0, 2, PageIndexSegmentKind::Pages);
    assert_eq!(selected_parent.declared_count(), 2);
    assert_eq!(
        selected_parent.evidence(),
        PageSegmentEvidence::CompleteSubtree
    );
    assert_eq!(selected_parent.validated_count(), Some(2));
    assert_eq!(selected_parent.retained_kid_count(), Some(2));
    assert_segment(&refined, object_ref(4), 0, 1, PageIndexSegmentKind::Page);
    assert_segment(&refined, object_ref(5), 1, 1, PageIndexSegmentKind::Page);
    let deferred = assert_segment(&refined, object_ref(6), 2, 2, PageIndexSegmentKind::Pages);
    assert_eq!(deferred.parent(), Some(object_ref(2)));
    assert_eq!(deferred.depth(), 2);
    assert_eq!(deferred.declared_count(), 2);
    assert_eq!(deferred.evidence(), PageSegmentEvidence::DeclaredCount);
    assert_eq!(deferred.validated_count(), None);
    assert_eq!(deferred.partitioned_count(), None);
    assert_eq!(deferred.retained_kid_count(), Some(2));

    let panic_source = PanicSource(fixture.snapshot);
    let (cached_page_zero, cached_stats) =
        lookup_ready(&authority, &refined, 0, &panic_source, 6_501);
    assert_eq!(cached_stats.objects_started(), 0);
    assert_eq!(cached_stats.nodes_classified(), 0);
    assert_eq!(cached_stats.segments_refined(), 0);
    assert_eq!(cached_stats.object_read_bytes(), 0);
    assert_eq!(cached_stats.object_parse_bytes(), 0);
    assert_eq!(cached_page_zero.handle().object(), object_ref(4));
    assert_eq!(cached_page_zero.page_index(), &refined);

    let (page_three, last_stats) = lookup_ready(&authority, &refined, 3, &store, 6_601);
    assert_eq!(last_stats.objects_started(), 2);
    assert_eq!(last_stats.nodes_classified(), 2);
    assert_eq!(last_stats.segments_refined(), 1);
    assert_eq!(page_three.handle().index(), 3);
    assert_eq!(page_three.handle().object(), object_ref(8));
    assert_eq!(
        page_three.handle().document_page_count_evidence(),
        PageSegmentEvidence::CompleteSubtree
    );
    let (complete, page_three_handle) = page_three.into_parts();
    assert!(complete.is_complete());
    for (reference, start) in [(object_ref(2), 0), (object_ref(3), 0), (object_ref(6), 2)] {
        let page_count = if reference == object_ref(2) { 4 } else { 2 };
        let segment = assert_segment(
            &complete,
            reference,
            start,
            page_count,
            PageIndexSegmentKind::Pages,
        );
        assert_eq!(segment.evidence(), PageSegmentEvidence::CompleteSubtree);
        assert_eq!(segment.validated_count(), Some(page_count));
        assert_eq!(segment.partitioned_count(), Some(page_count));
    }
    assert_segment(&complete, object_ref(4), 0, 1, PageIndexSegmentKind::Page);
    assert_segment(&complete, object_ref(5), 1, 1, PageIndexSegmentKind::Page);
    assert_segment(&complete, object_ref(7), 2, 1, PageIndexSegmentKind::Page);
    assert_segment(&complete, object_ref(8), 3, 1, PageIndexSegmentKind::Page);
    complete.validate_handle(page_one_handle).unwrap();
    complete.validate_handle(page_three_handle).unwrap();
}

#[test]
fn lookup_prioritizes_source_change_over_cancellation_and_replays_terminal_failures() {
    let fixture = two_subtree_fixture(0xa5);
    let authority = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let index = build_ready(&authority, &store, 6_651);

    let mut changed = authority
        .lookup_page(&index, 1, context(6_661), tree_limits())
        .unwrap();
    let wrong_snapshot = snapshot(
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
        0xf1,
    );
    let mismatch = match changed.poll(&PanicSource(wrong_snapshot), &Cancelled) {
        PageLookupPoll::Failed(error) => error,
        PageLookupPoll::Ready(_) => panic!("changed source must not publish a Page"),
        PageLookupPoll::Pending { .. } => {
            panic!("changed source must fail before byte acquisition")
        }
    };
    assert_eq!(mismatch.code(), DocumentErrorCode::SourceSnapshotMismatch);
    assert_eq!(changed.phase(), PageLookupPhase::Failed);
    assert_zero_lookup_work(changed.stats());
    match changed.poll(&PanicSource(fixture.snapshot), &DocumentNeverCancelled) {
        PageLookupPoll::Failed(repeated) => assert_eq!(repeated, mismatch),
        _ => panic!("terminal source-change failure must replay exactly"),
    }
    assert_zero_lookup_work(changed.stats());

    let mut cancelled = authority
        .lookup_page(&index, 1, context(6_671), tree_limits())
        .unwrap();
    let cancellation = match cancelled.poll(&PanicSource(fixture.snapshot), &Cancelled) {
        PageLookupPoll::Failed(error) => error,
        PageLookupPoll::Ready(_) => panic!("pre-work cancellation must not publish a Page"),
        PageLookupPoll::Pending { .. } => {
            panic!("pre-work cancellation must fail before byte acquisition")
        }
    };
    assert_eq!(cancellation.code(), DocumentErrorCode::Cancelled);
    assert_eq!(cancelled.phase(), PageLookupPhase::Failed);
    assert_zero_lookup_work(cancelled.stats());
    match cancelled.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Failed(repeated) => assert_eq!(repeated, cancellation),
        _ => panic!("terminal cancellation failure must replay exactly"),
    }
    assert_zero_lookup_work(cancelled.stats());
}

#[test]
fn lookup_rejects_out_of_bounds_indices_and_handles_from_another_binding() {
    let fixture = two_subtree_fixture(0xa2);
    let authority = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let index = build_ready(&authority, &store, 6_701);

    let error = match authority.lookup_page(&index, 4, context(6_801), tree_limits()) {
        Err(error) => error,
        Ok(mut job) => match job.poll(&store, &DocumentNeverCancelled) {
            PageLookupPoll::Failed(error) => error,
            PageLookupPoll::Ready(_) => panic!("out-of-bounds lookup must not succeed"),
            PageLookupPoll::Pending { .. } => {
                panic!("out-of-bounds lookup must fail before source acquisition")
            }
        },
    };
    assert_eq!(error.code(), DocumentErrorCode::PageIndexOutOfBounds);
    assert_eq!(error.reference(), Some(object_ref(2)));

    let (lookup, _) = lookup_ready(&authority, &index, 0, &store, 6_901);
    let handle = lookup.handle();
    let other_fixture = two_subtree_fixture(0xa3);
    let other_authority = ready_index(&other_fixture);
    let other_store = supplied_store(&other_fixture);
    let other_index = build_ready(&other_authority, &other_store, 7_001);
    let stale = other_index
        .validate_handle(handle)
        .expect_err("a source-bound page handle must not cross immutable bindings");
    assert_eq!(stale.code(), DocumentErrorCode::StalePageHandle);
    assert_eq!(stale.reference(), Some(object_ref(4)));
}

#[test]
fn cold_build_rejects_a_direct_duplicate_kid_with_stable_terminal_replay() {
    let fixture = duplicate_fixture();
    let authority = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let mut job = authority
        .build_page_index(context(7_101), tree_limits(), index_limits())
        .expect("cold build validates the Catalog and root Pages dictionary");
    let failure = match job.poll(&store, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Failed(error) => error,
        PageIndexBuildPoll::Ready(_) => {
            panic!("a duplicate direct root Kid must not enter the lazy index")
        }
        PageIndexBuildPoll::Pending { .. } => panic!("complete failing source must not pend"),
    };
    assert_eq!(failure.code(), DocumentErrorCode::DuplicatePageTreeNode);
    assert_eq!(failure.reference(), Some(object_ref(3)));
    match job.poll(&store, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Failed(repeated) => assert_eq!(repeated, failure),
        _ => panic!("terminal cold-build failure must replay exactly"),
    }
}

#[test]
fn cross_sibling_descendant_duplicate_fails_during_root_refinement_and_replays() {
    let fixture = cross_sibling_duplicate_fixture();
    let authority = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let index = build_ready(&authority, &store, 7_111);
    let root = assert_segment(&index, object_ref(2), 0, 2, PageIndexSegmentKind::Pages);
    assert_eq!(root.evidence(), PageSegmentEvidence::DeclaredCount);

    let mut lookup = authority
        .lookup_page(&index, 0, context(7_112), tree_limits())
        .unwrap();
    let failure = match lookup.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Failed(error) => error,
        PageLookupPoll::Ready(_) => {
            panic!("a descendant shared across sibling subtrees must not publish a Page")
        }
        PageLookupPoll::Pending { .. } => panic!("complete failing source must not pend"),
    };
    assert_eq!(failure.code(), DocumentErrorCode::DuplicatePageTreeNode);
    assert_eq!(failure.reference(), Some(object_ref(5)));
    assert_eq!(lookup.stats().segments_refined(), 0);
    assert_eq!(lookup.phase(), PageLookupPhase::Failed);
    match lookup.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Failed(repeated) => assert_eq!(repeated, failure),
        _ => panic!("terminal cross-sibling duplicate failure must replay exactly"),
    }
}

#[test]
fn discovered_node_limit_is_global_across_incremental_refinements() {
    let exact_fixture = seven_node_incremental_fixture(0xc6);
    let exact_authority = ready_index(&exact_fixture);
    let exact_store = supplied_store(&exact_fixture);
    let exact_limits = tree_limits_with_nodes(7);
    let exact_initial =
        build_ready_with_limits(&exact_authority, &exact_store, 7_113, exact_limits);
    let (first, _) = lookup_ready_with_limits(
        &exact_authority,
        &exact_initial,
        0,
        &exact_store,
        7_114,
        exact_limits,
    );
    assert_eq!(first.handle().object(), object_ref(4));
    let (six_discovered, first_handle) = first.into_parts();
    six_discovered.validate_handle(first_handle).unwrap();
    let deferred = assert_segment(
        &six_discovered,
        object_ref(6),
        2,
        1,
        PageIndexSegmentKind::Pages,
    );
    assert_eq!(deferred.evidence(), PageSegmentEvidence::DeclaredCount);

    let (last, _) = lookup_ready_with_limits(
        &exact_authority,
        &six_discovered,
        2,
        &exact_store,
        7_115,
        exact_limits,
    );
    assert_eq!(last.handle().object(), object_ref(8));
    let exact_complete = last.page_index();
    assert!(exact_complete.is_complete());
    assert_eq!(
        assert_segment(
            exact_complete,
            object_ref(2),
            0,
            3,
            PageIndexSegmentKind::Pages,
        )
        .evidence(),
        PageSegmentEvidence::CompleteSubtree
    );

    let limited_fixture = seven_node_incremental_fixture(0xc7);
    let limited_authority = ready_index(&limited_fixture);
    let limited_store = supplied_store(&limited_fixture);
    let limited_limits = tree_limits_with_nodes(6);
    let limited_initial =
        build_ready_with_limits(&limited_authority, &limited_store, 7_116, limited_limits);
    let (first, _) = lookup_ready_with_limits(
        &limited_authority,
        &limited_initial,
        0,
        &limited_store,
        7_117,
        limited_limits,
    );
    let (six_discovered, _) = first.into_parts();

    let mut failing = limited_authority
        .lookup_page(&six_discovered, 2, context(7_118), limited_limits)
        .unwrap();
    let failure = match failing.poll(&limited_store, &DocumentNeverCancelled) {
        PageLookupPoll::Failed(error) => error,
        PageLookupPoll::Ready(_) => {
            panic!("discovering a seventh Page-tree node must exceed a six-node global limit")
        }
        PageLookupPoll::Pending { .. } => panic!("complete failing source must not pend"),
    };
    assert_eq!(failure.code(), DocumentErrorCode::ResourceLimit);
    let detail = failure
        .limit()
        .expect("global node exhaustion retains structured limit detail");
    assert_eq!(detail.kind(), DocumentLimitKind::PageTreeNodes);
    assert_eq!(detail.limit(), 6);
    assert_eq!(detail.consumed(), 6);
    assert_eq!(detail.attempted(), 1);
    match failing.poll(&limited_store, &DocumentNeverCancelled) {
        PageLookupPoll::Failed(repeated) => assert_eq!(repeated, failure),
        _ => panic!("terminal global-node-limit failure must replay exactly"),
    }

    let mut retried = limited_authority
        .lookup_page(&six_discovered, 2, context(7_119), limited_limits)
        .unwrap();
    match retried.poll(&limited_store, &DocumentNeverCancelled) {
        PageLookupPoll::Failed(repeated) => assert_eq!(repeated, failure),
        PageLookupPoll::Ready(_) => {
            panic!("a new lookup job must not reset the retained discovered-node budget")
        }
        PageLookupPoll::Pending { .. } => panic!("complete failing source must not pend"),
    }
}

#[test]
fn lookup_rejects_tree_limits_different_from_the_index_before_work() {
    let fixture = seven_node_incremental_fixture(0xc8);
    let authority = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let index_limits = tree_limits_with_nodes(7);
    let index = build_ready_with_limits(&authority, &store, 7_120, index_limits);

    let error = match authority.lookup_page(&index, 0, context(7_121), tree_limits_with_nodes(8)) {
        Err(error) => error,
        Ok(_) => panic!("lookup limits must match the immutable index before a job is created"),
    };
    assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
    assert_eq!(error.reference(), Some(object_ref(2)));
}

#[test]
fn ancestor_cycle_is_delayed_to_lookup_and_replays_the_stable_page_tree_error() {
    let fixture = ancestor_cycle_fixture();
    let authority = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let index = build_ready(&authority, &store, 7_121);
    let root = assert_segment(&index, object_ref(2), 0, 1, PageIndexSegmentKind::Pages);
    assert_eq!(root.evidence(), PageSegmentEvidence::DeclaredCount);
    assert_eq!(root.retained_kid_count(), Some(1));

    let mut lookup = authority
        .lookup_page(&index, 0, context(7_131), tree_limits())
        .unwrap();
    let failure = match lookup.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Failed(error) => error,
        PageLookupPoll::Ready(_) => panic!("an ancestor cycle must not publish a Page"),
        PageLookupPoll::Pending { .. } => panic!("complete failing source must not pend"),
    };
    assert_eq!(failure.code(), DocumentErrorCode::PageTreeCycle);
    assert_eq!(failure.reference(), Some(object_ref(2)));
    assert_eq!(lookup.phase(), PageLookupPhase::Failed);
    match lookup.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Failed(repeated) => assert_eq!(repeated, failure),
        _ => panic!("terminal ancestor-cycle failure must replay exactly"),
    }
}

#[test]
fn unrelated_deep_count_and_cycle_errors_wait_until_their_declared_range_is_requested() {
    let cases = [
        (
            unrelated_deep_count_mismatch_fixture(),
            DocumentErrorCode::PageTreeCountMismatch,
            object_ref(7),
        ),
        (
            unrelated_deep_cycle_fixture(),
            DocumentErrorCode::PageTreeCycle,
            object_ref(6),
        ),
    ];

    for (case_index, (fixture, expected_code, expected_reference)) in cases.into_iter().enumerate()
    {
        let authority = ready_index(&fixture);
        let store = supplied_store(&fixture);
        let seed = 7_151 + u64::try_from(case_index).unwrap() * 20;
        let initial = build_ready(&authority, &store, seed);
        assert_eq!(initial.stats().objects_started(), 2);
        assert!(!initial.stats().has_complete_tree_proof());

        let (safe_lookup, safe_stats) = lookup_ready(&authority, &initial, 0, &store, seed + 3);
        assert_eq!(safe_lookup.handle().object(), object_ref(4));
        assert_eq!(safe_stats.objects_started(), 4);
        let (safe_index, safe_handle) = safe_lookup.into_parts();
        safe_index.validate_handle(safe_handle).unwrap();
        let deferred = assert_segment(
            &safe_index,
            object_ref(6),
            2,
            2,
            PageIndexSegmentKind::Pages,
        );
        assert_eq!(deferred.evidence(), PageSegmentEvidence::DeclaredCount);
        assert_eq!(deferred.validated_count(), None);

        let mut failing = authority
            .lookup_page(&safe_index, 3, context(seed + 6), tree_limits())
            .unwrap();
        let failure = match failing.poll(&store, &DocumentNeverCancelled) {
            PageLookupPoll::Failed(error) => error,
            PageLookupPoll::Ready(_) => {
                panic!("requesting the malformed deferred range must not publish a Page")
            }
            PageLookupPoll::Pending { .. } => panic!("complete failing source must not pend"),
        };
        assert_eq!(failure.code(), expected_code);
        assert_eq!(failure.reference(), Some(expected_reference));
        match failing.poll(&store, &DocumentNeverCancelled) {
            PageLookupPoll::Failed(repeated) => assert_eq!(repeated, failure),
            _ => panic!("terminal deferred-range failure must replay exactly"),
        }
    }
}

#[test]
fn cold_build_prioritizes_source_change_over_cancellation_and_replays_without_work() {
    let fixture = two_subtree_fixture(0xa6);
    let authority = ready_index(&fixture);
    let store = supplied_store(&fixture);

    let mut changed = authority
        .build_page_index(context(7_191), tree_limits(), index_limits())
        .unwrap();
    let wrong_snapshot = snapshot(
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
        0xf2,
    );
    let mismatch = match changed.poll(&PanicSource(wrong_snapshot), &Cancelled) {
        PageIndexBuildPoll::Failed(error) => error,
        PageIndexBuildPoll::Ready(_) => panic!("changed source must not publish an index"),
        PageIndexBuildPoll::Pending { .. } => {
            panic!("changed source must fail before byte acquisition")
        }
    };
    assert_eq!(mismatch.code(), DocumentErrorCode::SourceSnapshotMismatch);
    assert_eq!(changed.stats().objects_started(), 0);
    assert_eq!(changed.stats().object_read_bytes(), 0);
    assert_eq!(changed.stats().object_parse_bytes(), 0);
    match changed.poll(&store, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Failed(repeated) => assert_eq!(repeated, mismatch),
        _ => panic!("terminal cold-build source failure must replay exactly"),
    }

    let mut cancelled = authority
        .build_page_index(context(7_201), tree_limits(), index_limits())
        .unwrap();
    let cancellation = match cancelled.poll(&PanicSource(fixture.snapshot), &Cancelled) {
        PageIndexBuildPoll::Failed(error) => error,
        PageIndexBuildPoll::Ready(_) => panic!("cancelled build must not publish an index"),
        PageIndexBuildPoll::Pending { .. } => {
            panic!("pre-work cancellation must fail before byte acquisition")
        }
    };
    assert_eq!(cancellation.code(), DocumentErrorCode::Cancelled);
    assert_eq!(cancelled.stats().objects_started(), 0);
    assert_eq!(cancelled.stats().object_read_bytes(), 0);
    assert_eq!(cancelled.stats().object_parse_bytes(), 0);
    match cancelled.poll(&store, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Failed(repeated) => assert_eq!(repeated, cancellation),
        _ => panic!("terminal cold-build cancellation must replay exactly"),
    }
}

#[test]
fn owned_shared_jobs_retain_the_attested_proof_for_build_and_lookup() {
    let fixture = two_subtree_fixture(0xa4);
    let store = supplied_store(&fixture);
    let shared = ready_index(&fixture).into_shared();
    let mut build: BuildPageIndexJob<'static> = shared
        .build_page_index_owned(context(7_201), tree_limits(), index_limits())
        .unwrap();
    drop(shared);
    let index = match build.poll(&store, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Ready(index) => index,
        PageIndexBuildPoll::Pending { .. } => panic!("resident owned build must not suspend"),
        PageIndexBuildPoll::Failed(error) => panic!("owned build failed: {error}"),
    };

    let shared = ready_index(&fixture).into_shared();
    let mut lookup: LookupPageJob<'static> = shared
        .lookup_page_owned(&index, 2, context(7_301), tree_limits())
        .unwrap();
    drop(shared);
    match lookup.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Ready(result) => {
            assert_eq!(result.handle().index(), 2);
            assert_eq!(result.handle().object(), object_ref(7));
        }
        PageLookupPoll::Pending { .. } => panic!("resident owned lookup must not suspend"),
        PageLookupPoll::Failed(error) => panic!("owned lookup failed: {error}"),
    }
}

#[allow(clippy::result_large_err, dead_code)]
fn repaired_page_index_api_is_available(
    repaired: &pdf_rs_document::LocallyRepairedRevisionIndex,
    shared: &pdf_rs_document::SharedLocallyRepairedRevisionIndex,
    index: &PageIndex,
) -> Result<(), DocumentError> {
    let _borrowed_build =
        repaired.build_page_index(context(7_401), tree_limits(), index_limits())?;
    let _owned_build =
        shared.build_page_index_owned(context(7_501), tree_limits(), index_limits())?;
    let _borrowed_lookup = repaired.lookup_page(index, 0, context(7_601), tree_limits())?;
    let _owned_lookup = shared.lookup_page_owned(index, 0, context(7_701), tree_limits())?;
    Ok(())
}

#[allow(dead_code)]
fn page_segment_summary_is_public(_: &PageSegmentSummary) {}
