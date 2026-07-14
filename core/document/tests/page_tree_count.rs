use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex, CountPagesJob, DocumentError,
    DocumentErrorCategory, DocumentErrorCode, DocumentLimitKind, DocumentLimits,
    NeverCancelled as DocumentNeverCancelled, PageCount, PageCountPoll, PageTreeJobContext,
    PageTreeLimitConfig, PageTreeLimits, PageTreePhase, RevisionAttestationJobContext,
    RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::{ObjectErrorCode, ObjectLimitKind, ObjectLimits};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(29);
const ATTEST_JOB: JobId = JobId::new(1_701);
const ATTEST_SCAN: ResumeCheckpoint = ResumeCheckpoint::new(1_702);
const ATTEST_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(1_703);
const ATTEST_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(1_704);
const PAGE_TREE_JOB: JobId = JobId::new(1_801);
const PAGE_TREE_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(1_802);
const PAGE_TREE_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(1_803);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

fn snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0xa1; 32]), SourceRevision::new(41)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xd7; 32]),
    )
}

fn fixture(bodies: &[(u32, &[u8])], size: u32) -> Fixture {
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
    let source = snapshot(u64::try_from(bytes.len()).expect("fixture length fits u64"));
    Fixture {
        bytes,
        snapshot: source,
    }
}

fn single_page_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            ),
            (3, b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n"),
        ],
        4,
    )
}

fn empty_page_tree_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
            ),
        ],
        3,
    )
}

fn nested_three_page_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 3 >>\nendobj\n",
            ),
            (3, b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n"),
            (
                4,
                b"4 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R 6 0 R] /Count 2 >>\nendobj\n",
            ),
            (5, b"5 0 obj\n<< /Type /Page /Parent 4 0 R >>\nendobj\n"),
            (6, b"6 0 obj\n<< /Type /Page /Parent 4 0 R >>\nendobj\n"),
        ],
        7,
    )
}

fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("test object reference is nonzero")
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    supply_range(
        &store,
        fixture,
        ByteRange::new(
            0,
            u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
        )
        .unwrap(),
    );
    store
}

fn supply_range(store: &RangeStore, fixture: &Fixture, range: ByteRange) {
    let start = usize::try_from(range.start()).expect("fixture offset fits usize");
    let end = usize::try_from(range.end_exclusive()).expect("fixture offset fits usize");
    store
        .supply(
            RangeResponse::new(fixture.snapshot, range, fixture.bytes[start..end].to_vec())
                .expect("fixture response matches its exact range"),
        )
        .expect("fixture range fits store limits");
}

fn parsed_xref(fixture: &Fixture) -> XrefSection {
    let store = supplied_store(fixture);
    let mut job = OpenXrefJob::new(
        fixture.snapshot,
        XrefJobContext::new(
            JobId::new(1_601),
            ResumeCheckpoint::new(1_602),
            ResumeCheckpoint::new(1_603),
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

fn candidate(fixture: &Fixture) -> CandidateRevisionIndex {
    CandidateRevisionIndex::from_xref(
        &parsed_xref(fixture),
        REVISION_ID,
        DocumentLimits::default(),
        &DocumentNeverCancelled,
    )
    .expect("self-authored xref yields a candidate")
}

fn ready_index(fixture: &Fixture) -> AttestedRevisionIndex {
    let store = supplied_store(fixture);
    let mut job = AttestRevisionJob::new(
        candidate(fixture),
        RevisionAttestationJobContext::new(
            ATTEST_JOB,
            ATTEST_SCAN,
            ATTEST_ENVELOPE,
            ATTEST_BOUNDARY,
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

fn context() -> PageTreeJobContext {
    PageTreeJobContext::new(
        PAGE_TREE_JOB,
        PAGE_TREE_ENVELOPE,
        PAGE_TREE_BOUNDARY,
        RequestPriority::VisiblePage,
    )
}

fn limits_with(
    max_nodes: u64,
    max_depth: u64,
    max_pages: u64,
    max_kids_per_node: u64,
    max_read: u64,
    max_parse: u64,
    max_traversal: u64,
) -> PageTreeLimits {
    PageTreeLimits::validate(PageTreeLimitConfig {
        max_nodes,
        max_depth,
        max_pages,
        max_kids_per_node,
        max_total_object_read_bytes: max_read,
        max_total_object_parse_bytes: max_parse,
        max_retained_traversal_bytes: max_traversal,
    })
    .expect("test page-tree limits validate")
}

fn compact_limits() -> PageTreeLimits {
    limits_with(8, 4, 4, 4, 1 << 20, 1 << 20, 4 << 10)
}

fn poll_ready(job: &mut CountPagesJob<'_>, source: &dyn ByteSource) -> PageCount {
    match job.poll(source, &DocumentNeverCancelled) {
        PageCountPoll::Ready(count) => count,
        PageCountPoll::Pending { .. } => panic!("complete source must not suspend"),
        PageCountPoll::Failed(error) => panic!("valid page tree must count: {error}"),
    }
}

fn poll_failure(
    job: &mut CountPagesJob<'_>,
    source: &dyn ByteSource,
    cancellation: &dyn pdf_rs_document::DocumentCancellation,
) -> DocumentError {
    let failure = match job.poll(source, cancellation) {
        PageCountPoll::Failed(error) => error,
        PageCountPoll::Ready(_) => panic!("expected failure, got Ready"),
        PageCountPoll::Pending { .. } => panic!("complete or failing source must not pend"),
    };
    assert_eq!(job.phase(), PageTreePhase::Failed);
    match job.poll(source, cancellation) {
        PageCountPoll::Failed(repeated) => assert_eq!(repeated, failure),
        _ => panic!("terminal failure must replay the same error"),
    }
    failure
}

fn fixture_failure(fixture: &Fixture) -> DocumentError {
    let index = ready_index(fixture);
    let store = supplied_store(fixture);
    let mut job = index.count_pages(context(), compact_limits()).unwrap();
    poll_failure(&mut job, &store, &DocumentNeverCancelled)
}

struct PanicSource(SourceSnapshot);

impl ByteSource for PanicSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("source poll must not run")
    }
}

#[test]
fn valid_single_and_nested_trees_return_bound_catalog_counts_stats_and_stable_terminal_phase() {
    for (fixture, expected_pages, expected_nodes, expected_depth, expected_kids) in [
        (empty_page_tree_fixture(), 0, 1, 1, 0),
        (single_page_fixture(), 1, 2, 2, 1),
        (nested_three_page_fixture(), 3, 5, 3, 2),
    ] {
        let index = ready_index(&fixture);
        let store = supplied_store(&fixture);
        let limits = compact_limits();
        let mut job = index.count_pages(context(), limits).unwrap();

        assert_eq!(job.snapshot(), fixture.snapshot);
        assert_eq!(job.context(), context());
        assert_eq!(job.limits(), limits);
        assert_eq!(job.phase(), PageTreePhase::Catalog);
        assert_eq!(job.stats().objects_started(), 0);
        assert!(job.stats().reserved_traversal_bytes() > 0);

        let count = poll_ready(&mut job, &store);
        assert_eq!(job.phase(), PageTreePhase::Ready);
        assert_eq!(count.page_count(), expected_pages);
        assert_eq!(count.catalog().snapshot(), fixture.snapshot);
        assert_eq!(count.catalog().revision_id(), REVISION_ID);
        assert_eq!(count.catalog().revision_startxref(), index.startxref());
        assert_eq!(count.catalog().root(), object_ref(1));
        assert_eq!(count.catalog().pages(), object_ref(2));

        let stats = count.stats();
        assert_eq!(stats, job.stats());
        assert_eq!(stats.objects_started(), expected_nodes + 1);
        assert_eq!(stats.nodes_started(), expected_nodes);
        assert_eq!(stats.pages(), expected_pages);
        assert_eq!(stats.max_depth(), expected_depth);
        assert_eq!(stats.max_kids_per_node(), expected_kids);
        assert!(stats.object_read_bytes() > 0);
        assert!(stats.object_parse_bytes() > 0);
        assert!(stats.reserved_traversal_bytes() > 0);

        match job.poll(&store, &DocumentNeverCancelled) {
            PageCountPoll::Failed(error) => {
                assert_eq!(error.code(), DocumentErrorCode::JobAlreadyComplete)
            }
            _ => panic!("a completed one-shot job must reject repoll"),
        }
        assert_eq!(job.phase(), PageTreePhase::Ready);
        assert_eq!(job.stats(), stats);
    }
}

#[test]
fn duplicate_structural_keys_and_root_parent_have_distinct_strict_failures() {
    let duplicate_keys = [
        fixture(
            &[
                (
                    1,
                    b"1 0 obj\n<< /Type /Catalog /Type /Catalog /Pages 2 0 R >>\nendobj\n",
                ),
                (
                    2,
                    b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
                ),
            ],
            3,
        ),
        fixture(
            &[
                (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                (
                    2,
                    b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 /Count 0 >>\nendobj\n",
                ),
            ],
            3,
        ),
    ];
    for fixture in duplicate_keys {
        let failure = fixture_failure(&fixture);
        assert_eq!(failure.code(), DocumentErrorCode::DuplicateStructuralKey);
        assert_eq!(failure.category(), DocumentErrorCategory::Syntax);
    }

    let root_parent = fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Parent 1 0 R /Kids [] /Count 0 >>\nendobj\n",
            ),
        ],
        3,
    );
    let failure = fixture_failure(&root_parent);
    assert_eq!(failure.code(), DocumentErrorCode::PageTreeParentMismatch);
    assert_eq!(failure.reference(), Some(object_ref(2)));
}

#[test]
fn catalog_and_page_or_pages_dictionary_shapes_are_strict() {
    let invalid_catalogs = [
        fixture(&[(1, b"1 0 obj\n42\nendobj\n")], 2),
        fixture(
            &[
                (1, b"1 0 obj\n<< /Type /Pages /Pages 2 0 R >>\nendobj\n"),
                (
                    2,
                    b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
                ),
            ],
            3,
        ),
        fixture(
            &[(1, b"1 0 obj\n<< /Type /Catalog /Pages 2 >>\nendobj\n")],
            2,
        ),
        fixture(&[(1, b"1 0 obj\n<< /Pages 2 0 R >>\nendobj\n")], 2),
    ];
    for fixture in invalid_catalogs {
        let failure = fixture_failure(&fixture);
        assert_eq!(failure.code(), DocumentErrorCode::InvalidCatalog);
        assert_eq!(failure.category(), DocumentErrorCategory::Syntax);
    }

    let invalid_nodes = [
        fixture(
            &[
                (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                (2, b"2 0 obj\n<< /Type /Page >>\nendobj\n"),
            ],
            3,
        ),
        fixture(
            &[
                (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                (2, b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n"),
            ],
            3,
        ),
        fixture(
            &[
                (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                (
                    2,
                    b"2 0 obj\n<< /Type /Pages /Kids [3] /Count 1 >>\nendobj\n",
                ),
            ],
            3,
        ),
        fixture(
            &[
                (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                (
                    2,
                    b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
                ),
                (3, b"3 0 obj\n<< /Type /Widget /Parent 2 0 R >>\nendobj\n"),
            ],
            4,
        ),
        fixture(
            &[
                (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                (
                    2,
                    b"2 0 obj\n<< /Type /Pages /Kids [] /Count -1 >>\nendobj\n",
                ),
            ],
            3,
        ),
    ];
    for fixture in invalid_nodes {
        let failure = fixture_failure(&fixture);
        assert_eq!(failure.code(), DocumentErrorCode::InvalidPageTreeNode);
        assert_eq!(failure.category(), DocumentErrorCategory::Syntax);
    }
}

#[test]
fn cycles_duplicate_kids_counts_and_parent_links_have_distinct_stable_errors() {
    let cases = [
        (
            fixture(
                &[
                    (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                    (
                        2,
                        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 0 >>\nendobj\n",
                    ),
                    (
                        3,
                        b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [2 0 R] /Count 0 >>\nendobj\n",
                    ),
                ],
                4,
            ),
            DocumentErrorCode::PageTreeCycle,
            object_ref(2),
        ),
        (
            fixture(
                &[
                    (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                    (
                        2,
                        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>\nendobj\n",
                    ),
                    (
                        3,
                        b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n",
                    ),
                ],
                4,
            ),
            DocumentErrorCode::DuplicatePageTreeNode,
            object_ref(3),
        ),
        (
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
            ),
            DocumentErrorCode::DuplicatePageTreeNode,
            object_ref(5),
        ),
        (
            fixture(
                &[
                    (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                    (
                        2,
                        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 2 >>\nendobj\n",
                    ),
                    (
                        3,
                        b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n",
                    ),
                ],
                4,
            ),
            DocumentErrorCode::PageTreeCountMismatch,
            object_ref(2),
        ),
        (
            fixture(
                &[
                    (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                    (
                        2,
                        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
                    ),
                    (3, b"3 0 obj\n<< /Type /Page >>\nendobj\n"),
                ],
                4,
            ),
            DocumentErrorCode::PageTreeParentMismatch,
            object_ref(3),
        ),
        (
            fixture(
                &[
                    (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
                    (
                        2,
                        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
                    ),
                    (
                        3,
                        b"3 0 obj\n<< /Type /Page /Parent 4 0 R >>\nendobj\n",
                    ),
                    (
                        4,
                        b"4 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
                    ),
                ],
                5,
            ),
            DocumentErrorCode::PageTreeParentMismatch,
            object_ref(3),
        ),
    ];

    for (fixture, expected, reference) in cases {
        let failure = fixture_failure(&fixture);
        assert_eq!(failure.code(), expected);
        assert_eq!(failure.reference(), Some(reference));
        assert_eq!(failure.category(), DocumentErrorCategory::Syntax);
    }
}

#[test]
fn pending_replays_ticket_checkpoint_missing_ranges_and_charges_without_duplication_then_cancels() {
    let fixture = nested_three_page_fixture();
    let index = ready_index(&fixture);
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = index.count_pages(context(), compact_limits()).unwrap();

    let (ticket, missing, checkpoint) = match job.poll(&store, &DocumentNeverCancelled) {
        PageCountPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => (ticket, missing, checkpoint),
        _ => panic!("empty page-tree source must suspend on the Catalog envelope"),
    };
    assert_eq!(checkpoint, PAGE_TREE_ENVELOPE);
    assert_eq!(job.phase(), PageTreePhase::Catalog);
    let charged = job.stats();
    assert_eq!(charged.objects_started(), 1);
    assert!(charged.object_read_bytes() > 0);
    assert_eq!(charged.object_parse_bytes(), 0);

    match job.poll(&store, &DocumentNeverCancelled) {
        PageCountPoll::Pending {
            ticket: repeated_ticket,
            missing: repeated_missing,
            checkpoint: repeated_checkpoint,
        } => {
            assert_eq!(repeated_ticket, ticket);
            assert_eq!(repeated_missing, missing);
            assert_eq!(repeated_checkpoint, checkpoint);
        }
        _ => panic!("unchanged source must replay Pending"),
    }
    assert_eq!(job.stats(), charged);

    for range in missing.as_slice() {
        supply_range(&store, &fixture, *range);
    }
    let (node_ticket, node_missing, node_checkpoint) = loop {
        match job.poll(&store, &DocumentNeverCancelled) {
            PageCountPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } if job.phase() == PageTreePhase::Traversing => {
                break (ticket, missing, checkpoint);
            }
            PageCountPoll::Pending { missing, .. } => {
                for range in missing.as_slice() {
                    supply_range(&store, &fixture, *range);
                }
            }
            PageCountPoll::Ready(_) => panic!("partial source must suspend while traversing"),
            PageCountPoll::Failed(error) => {
                panic!("supplying Catalog ranges must enter traversal: {error}")
            }
        }
    };
    assert_eq!(node_checkpoint, PAGE_TREE_ENVELOPE);
    let traversal_charged = job.stats();
    assert!(traversal_charged.objects_started() >= 2);
    assert!(traversal_charged.nodes_started() >= 1);
    assert!(traversal_charged.object_parse_bytes() > 0);
    match job.poll(&store, &DocumentNeverCancelled) {
        PageCountPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(ticket, node_ticket);
            assert_eq!(missing, node_missing);
            assert_eq!(checkpoint, node_checkpoint);
        }
        _ => panic!("unchanged node source must replay Pending"),
    }
    assert_eq!(job.stats(), traversal_charged);

    let cancelled = AtomicBool::new(true);
    let failure = poll_failure(&mut job, &PanicSource(fixture.snapshot), &cancelled);
    assert_eq!(failure.code(), DocumentErrorCode::Cancelled);
    assert_eq!(failure.category(), DocumentErrorCategory::Cancellation);
    assert_eq!(job.stats(), traversal_charged);

    let mut before_read = index.count_pages(context(), compact_limits()).unwrap();
    let failure = poll_failure(&mut before_read, &PanicSource(fixture.snapshot), &cancelled);
    assert_eq!(failure.code(), DocumentErrorCode::Cancelled);
    assert_eq!(before_read.stats().objects_started(), 0);
    assert_eq!(before_read.stats().object_read_bytes(), 0);
}

fn exact_shape_limits(read: u64, parse: u64, traversal: u64) -> PageTreeLimits {
    limits_with(5, 3, 3, 2, read, parse, traversal)
}

fn assert_limit_failure(
    fixture: &Fixture,
    limits: PageTreeLimits,
    expected: DocumentLimitKind,
) -> DocumentError {
    let index = ready_index(fixture);
    let store = supplied_store(fixture);
    let mut job = index.count_pages(context(), limits).unwrap();
    let failure = poll_failure(&mut job, &store, &DocumentNeverCancelled);
    assert_eq!(failure.code(), DocumentErrorCode::ResourceLimit);
    let detail = failure
        .limit()
        .expect("resource failure retains limit detail");
    assert_eq!(detail.kind(), expected);
    assert!(detail.consumed() <= detail.limit());
    assert!(detail.attempted() > 0);
    failure
}

#[test]
fn node_depth_page_and_kids_limits_accept_exact_shape_and_reject_one_less() {
    let fixture = nested_three_page_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let exact = exact_shape_limits(1 << 20, 1 << 20, 4 << 10);
    let mut job = index.count_pages(context(), exact).unwrap();
    let count = poll_ready(&mut job, &store);
    assert_eq!(count.page_count(), 3);
    assert_eq!(count.stats().nodes_started(), 5);
    assert_eq!(count.stats().max_depth(), 3);
    assert_eq!(count.stats().max_kids_per_node(), 2);

    for (limits, kind, ceiling) in [
        (
            limits_with(4, 3, 3, 2, 1 << 20, 1 << 20, 4 << 10),
            DocumentLimitKind::PageTreeNodes,
            4,
        ),
        (
            limits_with(5, 2, 3, 2, 1 << 20, 1 << 20, 4 << 10),
            DocumentLimitKind::PageTreeDepth,
            2,
        ),
        (
            limits_with(5, 3, 2, 2, 1 << 20, 1 << 20, 4 << 10),
            DocumentLimitKind::PageTreePages,
            2,
        ),
        (
            limits_with(5, 3, 3, 1, 1 << 20, 1 << 20, 4 << 10),
            DocumentLimitKind::PageTreeKids,
            1,
        ),
    ] {
        let failure = assert_limit_failure(&fixture, limits, kind);
        assert_eq!(failure.limit().unwrap().limit(), ceiling);
    }
}

#[test]
fn aggregate_read_and_parse_limits_accept_exact_measured_work_and_reject_one_less() {
    let fixture = nested_three_page_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let generous = exact_shape_limits(1 << 20, 1 << 20, 4 << 10);
    let mut baseline = index.count_pages(context(), generous).unwrap();
    let baseline = poll_ready(&mut baseline, &store).stats();
    let read = baseline.object_read_bytes();
    let parse = baseline.object_parse_bytes();
    assert!(read > 1);
    assert!(parse > 1);

    let mut exact = index
        .count_pages(context(), exact_shape_limits(read, parse, 4 << 10))
        .unwrap();
    let exact_count = poll_ready(&mut exact, &store);
    assert_eq!(exact_count.stats().object_read_bytes(), read);
    assert_eq!(exact_count.stats().object_parse_bytes(), parse);

    let read_failure = assert_limit_failure(
        &fixture,
        exact_shape_limits(read - 1, parse, 4 << 10),
        DocumentLimitKind::PageTreeObjectReadBytes,
    );
    let lower = read_failure
        .object_error()
        .expect("aggregate read failure retains lower object error");
    assert_eq!(lower.code(), ObjectErrorCode::ResourceLimit);
    assert_eq!(
        lower.limit().expect("lower read limit detail").kind(),
        ObjectLimitKind::TotalReadBytes
    );

    let parse_failure = assert_limit_failure(
        &fixture,
        exact_shape_limits(read, parse - 1, 4 << 10),
        DocumentLimitKind::PageTreeObjectParseBytes,
    );
    let lower = parse_failure
        .object_error()
        .expect("aggregate parse failure retains lower object error");
    assert_eq!(lower.code(), ObjectErrorCode::ResourceLimit);
    assert_eq!(
        lower.limit().expect("lower parse limit detail").kind(),
        ObjectLimitKind::TotalParseBytes
    );
}

#[test]
fn traversal_capacity_limit_accepts_exact_reserved_bytes_and_rejects_one_less_at_construction() {
    let fixture = nested_three_page_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let initial_limits = exact_shape_limits(1 << 20, 1 << 20, 4 << 10);
    let mut initial = index.count_pages(context(), initial_limits).unwrap();
    let reserved = poll_ready(&mut initial, &store)
        .stats()
        .reserved_traversal_bytes();
    assert!(reserved > 1);

    let exact_limits = exact_shape_limits(1 << 20, 1 << 20, reserved);
    let mut exact = index.count_pages(context(), exact_limits).unwrap();
    assert_eq!(exact.stats().reserved_traversal_bytes(), reserved);
    assert_eq!(poll_ready(&mut exact, &store).page_count(), 3);

    let error = match index.count_pages(
        context(),
        exact_shape_limits(1 << 20, 1 << 20, reserved - 1),
    ) {
        Ok(_) => panic!("one byte below reserved capacity must reject construction"),
        Err(error) => error,
    };
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    let detail = error
        .limit()
        .expect("construction failure retains limit detail");
    assert_eq!(detail.kind(), DocumentLimitKind::PageTreeTraversalBytes);
    assert_eq!(detail.limit(), reserved - 1);
    assert_eq!(detail.consumed(), 0);
    assert_eq!(detail.attempted(), reserved);
}

#[test]
fn equal_child_checkpoints_are_rejected_before_a_job_is_published() {
    let fixture = single_page_fixture();
    let index = ready_index(&fixture);
    let invalid = PageTreeJobContext::new(
        PAGE_TREE_JOB,
        PAGE_TREE_ENVELOPE,
        PAGE_TREE_ENVELOPE,
        RequestPriority::VisiblePage,
    );
    let error = match index.count_pages(invalid, compact_limits()) {
        Ok(_) => panic!("equal child checkpoints must reject construction"),
        Err(error) => error,
    };
    assert_eq!(error.code(), DocumentErrorCode::InvalidPageTreeJobContext);
    assert_eq!(error.category(), DocumentErrorCategory::Configuration);
    assert_eq!(error.reference(), Some(object_ref(1)));
}
