use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex, DocumentCancellation,
    DocumentError, DocumentErrorCode, DocumentLimitKind, DocumentLimits, MaterializePageJob,
    MaterializedPage, NeverCancelled as DocumentNeverCancelled, PageHandle, PageIndex,
    PageIndexBuildPoll, PageIndexLimits, PageLookupPoll, PageMaterializationJobContext,
    PageMaterializationLimitConfig, PageMaterializationLimits, PageMaterializationPhase,
    PageMaterializationPoll, PageRotation, PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(71);
const CATALOG: &[u8] = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
const ONE_PAGE_ROOT: &[u8] = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

struct Prepared {
    authority: AttestedRevisionIndex,
    store: RangeStore,
    index: PageIndex,
    handle: PageHandle,
}

type PageValueFailureCase = (
    &'static [u8],
    &'static [(u32, &'static [u8])],
    u32,
    DocumentErrorCode,
);

fn snapshot(len: u64, salt: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([salt; 32]),
            SourceRevision::new(u64::from(salt) + 1),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [salt ^ 0x93; 32]),
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

fn one_page_fixture(page: &[u8], extras: &[(u32, &[u8])], size: u32, salt: u8) -> Fixture {
    let mut bodies = vec![(1, CATALOG), (2, ONE_PAGE_ROOT), (3, page)];
    bodies.extend_from_slice(extras);
    fixture(&bodies, size, salt)
}

fn direct_values_fixture() -> Fixture {
    one_page_fixture(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612.5 792] /CropBox [10 20 600 780] /Rotate -90 /Resources << /Font << /F1 4 0 R >> >> >>\nendobj\n",
        &[(4, b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n")],
        5,
        0xd1,
    )
}

fn inherited_defaults_fixture() -> Fixture {
    fixture(
        &[
            (1, CATALOG),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [-10.25 0 600 800] /Resources [ /MustNotMerge ] >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 /Resources << /Nearest true >> >>\nendobj\n",
            ),
            (4, b"4 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n"),
        ],
        5,
        0xd2,
    )
}

fn alias_fixture() -> Fixture {
    one_page_fixture(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox 4 0 R /CropBox 6 0 R /Rotate 8 0 R /Resources 10 0 R >>\nendobj\n",
        &[
            (4, b"4 0 obj\n5 0 R\nendobj\n"),
            (5, b"5 0 obj\n[0 0 200.125 300]\nendobj\n"),
            (6, b"6 0 obj\n[1 2 199 299]\nendobj\n"),
            (8, b"8 0 obj\n9 0 R\nendobj\n"),
            (9, b"9 0 obj\n-90\nendobj\n"),
            (10, b"10 0 obj\n11 0 R\nendobj\n"),
            (11, b"11 0 obj\n<< /ProcSet [/PDF] >>\nendobj\n"),
        ],
        12,
        0xd3,
    )
}

fn alias_cycle_fixture() -> Fixture {
    one_page_fixture(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox 4 0 R /Resources << >> >>\nendobj\n",
        &[
            (4, b"4 0 obj\n5 0 R\nendobj\n"),
            (5, b"5 0 obj\n4 0 R\nendobj\n"),
        ],
        6,
        0xd4,
    )
}

fn null_alias_falls_back_fixture() -> Fixture {
    fixture(
        &[
            (1, CATALOG),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 320 480] /Resources << /Root true >> >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox 4 0 R /Resources 5 0 R >>\nendobj\n",
            ),
            (4, b"4 0 obj\nnull\nendobj\n"),
            (5, b"5 0 obj\nnull\nendobj\n"),
        ],
        6,
        0xd5,
    )
}

fn transient_page_syntax_fixture() -> Fixture {
    let mut page =
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 320 480] /Noise (".to_vec();
    page.extend(std::iter::repeat_n(b'a', 4_096));
    page.extend_from_slice(b") >>\nendobj\n");
    fixture(
        &[
            (1, CATALOG),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources << >> >>\nendobj\n",
            ),
            (3, &page),
        ],
        4,
        0xd6,
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
            JobId::new(7_101),
            ResumeCheckpoint::new(7_102),
            ResumeCheckpoint::new(7_103),
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
            JobId::new(7_201),
            ResumeCheckpoint::new(7_202),
            ResumeCheckpoint::new(7_203),
            ResumeCheckpoint::new(7_204),
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

fn tree_context(seed: u64) -> PageTreeJobContext {
    PageTreeJobContext::new(
        JobId::new(seed),
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        RequestPriority::VisiblePage,
    )
}

fn materialization_context(seed: u64) -> PageMaterializationJobContext {
    PageMaterializationJobContext::new(
        JobId::new(seed),
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        RequestPriority::VisiblePage,
    )
}

fn tree_limits() -> PageTreeLimits {
    PageTreeLimits::validate(PageTreeLimitConfig {
        max_nodes: 8,
        max_depth: 4,
        max_pages: 4,
        max_kids_per_node: 4,
        max_total_object_read_bytes: 1 << 20,
        max_total_object_parse_bytes: 1 << 20,
        max_retained_traversal_bytes: 8 << 10,
    })
    .expect("test page-tree limits validate")
}

fn index_limits() -> PageIndexLimits {
    PageIndexLimits::new(4, 16 << 10).expect("test page-index limits validate")
}

fn prepare(fixture: &Fixture, seed: u64) -> Prepared {
    let authority = ready_index(fixture);
    let store = supplied_store(fixture);
    let mut build = authority
        .build_page_index(tree_context(seed), tree_limits(), index_limits())
        .expect("valid page-index build job");
    let cold = match build.poll(&store, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Ready(index) => index,
        PageIndexBuildPoll::Pending { .. } => panic!("complete source must not suspend"),
        PageIndexBuildPoll::Failed(error) => panic!("valid page index must build: {error}"),
    };
    let mut lookup = authority
        .lookup_page(&cold, 0, tree_context(seed + 10), tree_limits())
        .expect("valid page lookup job");
    let lookup = match lookup.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Ready(lookup) => lookup,
        PageLookupPoll::Pending { .. } => panic!("complete source must not suspend"),
        PageLookupPoll::Failed(error) => panic!("valid page lookup must succeed: {error}"),
    };
    let (index, handle) = lookup.into_parts();
    index
        .validate_handle(handle)
        .expect("refined index validates its exact Page handle");
    Prepared {
        authority,
        store,
        index,
        handle,
    }
}

fn materialize_ready(
    prepared: &Prepared,
    limits: PageMaterializationLimits,
    seed: u64,
) -> MaterializedPage {
    let mut job = prepared
        .authority
        .materialize_page(
            &prepared.index,
            prepared.handle,
            materialization_context(seed),
            limits,
        )
        .expect("valid materialization job");
    match job.poll(&prepared.store, &DocumentNeverCancelled) {
        PageMaterializationPoll::Ready(page) => page,
        PageMaterializationPoll::Pending { .. } => {
            panic!("fully supplied materialization source must not suspend")
        }
        PageMaterializationPoll::Failed(error) => {
            panic!("valid page materialization must succeed: {error}")
        }
    }
}

fn poll_failure(
    job: &mut MaterializePageJob<'_>,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
) -> DocumentError {
    match job.poll(source, cancellation) {
        PageMaterializationPoll::Failed(error) => error,
        PageMaterializationPoll::Ready(_) => panic!("failing input must not publish a Page"),
        PageMaterializationPoll::Pending { .. } => {
            panic!("fully supplied or pre-work failure must not suspend")
        }
    }
}

fn limits_with(kind: DocumentLimitKind, value: u64) -> PageMaterializationLimits {
    let mut config = PageMaterializationLimitConfig::default();
    match kind {
        DocumentLimitKind::PageMaterializationAncestors => config.max_ancestor_depth = value,
        DocumentLimitKind::PageMaterializationObjects => config.max_objects = value,
        DocumentLimitKind::PageMaterializationReferenceEdges => {
            config.max_reference_edges = value;
        }
        DocumentLimitKind::PageMaterializationObjectReadBytes => {
            config.max_total_object_read_bytes = value;
        }
        DocumentLimitKind::PageMaterializationObjectParseBytes => {
            config.max_total_object_parse_bytes = value;
        }
        DocumentLimitKind::PageMaterializationStateBytes => {
            config.max_retained_state_bytes = value;
        }
        _ => panic!("test helper accepts only page-materialization budgets"),
    }
    PageMaterializationLimits::validate(config).expect("positive measured budget validates")
}

fn failure_with_limits(
    prepared: &Prepared,
    limits: PageMaterializationLimits,
    seed: u64,
) -> DocumentError {
    match prepared.authority.materialize_page(
        &prepared.index,
        prepared.handle,
        materialization_context(seed),
        limits,
    ) {
        Ok(mut job) => poll_failure(&mut job, &prepared.store, &DocumentNeverCancelled),
        Err(error) => error,
    }
}

struct PanicSource(SourceSnapshot);

impl ByteSource for PanicSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("pre-work rejection must not poll the byte source")
    }
}

struct Cancelled;

impl DocumentCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[test]
fn direct_page_values_materialize_without_opening_unused_ancestors() {
    let prepared = prepare(&direct_values_fixture(), 7_301);
    let page = materialize_ready(&prepared, PageMaterializationLimits::default(), 7_321);

    assert_eq!(page.handle(), prepared.handle);
    assert_eq!(page.boxes().media_box().left().scaled(), 0);
    assert_eq!(page.boxes().media_box().right().scaled(), 612_500_000_000);
    assert_eq!(page.boxes().crop_box().left().scaled(), 10_000_000_000);
    assert!(!page.boxes().crop_box_defaults_to_media_box());
    assert_eq!(
        page.boxes().media_box_provenance().defining_object(),
        object_ref(3)
    );
    assert_eq!(
        page.boxes().crop_box_provenance().defining_object(),
        object_ref(3)
    );
    assert_eq!(page.rotation(), PageRotation::Degrees270);
    assert_eq!(
        page.rotation_provenance()
            .expect("explicit Rotate retains provenance")
            .defining_object(),
        object_ref(3)
    );
    assert_eq!(page.resources().defining_object(), object_ref(3));
    assert_eq!(page.resources().ancestor_lookup_chain(), &[object_ref(3)]);
    assert_eq!(page.resources().resource_object(), None);
    assert_eq!(page.stats().objects_started(), 1);
    assert_eq!(page.stats().ancestors_opened(), 1);
    assert_eq!(page.stats().reference_edges(), 0);
    assert!(page.stats().object_read_bytes() > 0);
    assert!(page.stats().object_parse_bytes() > 0);
    assert!(page.stats().retained_state_bytes() > 0);
}

#[test]
fn inherited_values_apply_defaults_and_stop_at_the_nearest_resources_scope() {
    let prepared = prepare(&inherited_defaults_fixture(), 7_401);
    let page = materialize_ready(&prepared, PageMaterializationLimits::default(), 7_421);

    assert_eq!(page.boxes().media_box().left().scaled(), -10_250_000_000);
    assert_eq!(
        page.boxes().media_box_provenance().defining_object(),
        object_ref(2)
    );
    assert_eq!(page.boxes().crop_box(), page.boxes().media_box());
    assert!(page.boxes().crop_box_defaults_to_media_box());
    assert_eq!(
        page.boxes().crop_box_provenance().defining_object(),
        object_ref(2)
    );
    assert_eq!(page.rotation(), PageRotation::Degrees0);
    assert!(page.rotation_defaults_to_zero());
    assert_eq!(page.rotation_provenance(), None);

    assert_eq!(page.resources().defining_object(), object_ref(3));
    assert_eq!(
        page.resources().ancestor_lookup_chain(),
        &[object_ref(4), object_ref(3)]
    );
    assert_eq!(page.resources().resource_object(), None);
    assert_eq!(page.resources().resource_alias_chain(), &[]);
    assert_eq!(page.stats().objects_started(), 3);
    assert_eq!(page.stats().ancestors_opened(), 3);
}

#[test]
fn whole_value_aliases_preserve_complete_geometry_rotation_and_resource_provenance() {
    let prepared = prepare(&alias_fixture(), 7_501);
    let page = materialize_ready(&prepared, PageMaterializationLimits::default(), 7_521);

    assert_eq!(page.boxes().media_box().right().scaled(), 200_125_000_000);
    assert_eq!(
        page.boxes().media_box_provenance().defining_object(),
        object_ref(3)
    );
    assert_eq!(
        page.boxes().media_box_provenance().alias_chain(),
        &[object_ref(4), object_ref(5)]
    );
    assert_eq!(
        page.boxes().crop_box_provenance().alias_chain(),
        &[object_ref(6)]
    );
    assert_eq!(page.rotation(), PageRotation::Degrees270);
    assert_eq!(
        page.rotation_provenance()
            .expect("indirect Rotate retains provenance")
            .alias_chain(),
        &[object_ref(8), object_ref(9)]
    );
    assert_eq!(page.resources().defining_object(), object_ref(3));
    assert_eq!(page.resources().resource_object(), Some(object_ref(10)));
    assert_eq!(
        page.resources().terminal_resource_object(),
        Some(object_ref(11))
    );
    assert_eq!(
        page.resources().resource_alias_chain(),
        &[object_ref(10), object_ref(11)]
    );
    assert_eq!(page.resources().ancestor_lookup_chain(), &[object_ref(3)]);
    assert_eq!(page.stats().objects_started(), 8);
    assert_eq!(page.stats().ancestors_opened(), 1);
    assert_eq!(page.stats().reference_edges(), 7);
    assert_eq!(page.stats().max_alias_depth(), 2);
}

#[test]
fn alias_cycles_fail_with_stable_terminal_replay() {
    let prepared = prepare(&alias_cycle_fixture(), 7_601);
    let mut job = prepared
        .authority
        .materialize_page(
            &prepared.index,
            prepared.handle,
            materialization_context(7_621),
            PageMaterializationLimits::default(),
        )
        .unwrap();
    let failure = poll_failure(&mut job, &prepared.store, &DocumentNeverCancelled);
    assert_eq!(failure.code(), DocumentErrorCode::PageValueAliasCycle);
    assert_eq!(failure.reference(), Some(object_ref(4)));
    assert_eq!(job.phase(), PageMaterializationPhase::Failed);
    assert_eq!(job.stats().objects_started(), 3);
    assert_eq!(job.stats().reference_edges(), 2);

    let repeated = poll_failure(&mut job, &prepared.store, &DocumentNeverCancelled);
    assert_eq!(repeated, failure);
    assert_eq!(job.stats().objects_started(), 3);
    assert_eq!(job.stats().reference_edges(), 2);
}

#[test]
fn indirect_null_values_resume_inheritance_at_the_parent_scope() {
    let prepared = prepare(&null_alias_falls_back_fixture(), 7_651);
    let page = materialize_ready(&prepared, PageMaterializationLimits::default(), 7_671);

    assert_eq!(
        page.boxes().media_box_provenance().defining_object(),
        object_ref(2)
    );
    assert_eq!(page.resources().defining_object(), object_ref(2));
    assert_eq!(
        page.resources().ancestor_lookup_chain(),
        &[object_ref(3), object_ref(2)]
    );
    assert_eq!(page.stats().reference_edges(), 2);
    assert_eq!(page.stats().ancestors_opened(), 2);
}

#[test]
fn missing_and_invalid_page_values_report_specific_stable_codes() {
    let cases: &[PageValueFailureCase] = &[
        (
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << >> >>\nendobj\n",
            &[],
            4,
            DocumentErrorCode::MissingPageMediaBox,
        ),
        (
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
            &[],
            4,
            DocumentErrorCode::MissingPageResources,
        ),
        (
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 0 100] /Resources << >> >>\nendobj\n",
            &[],
            4,
            DocumentErrorCode::InvalidPageBox,
        ),
        (
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100.0000000001 100] /Resources << >> >>\nendobj\n",
            &[],
            4,
            DocumentErrorCode::InvalidPageBox,
        ),
        (
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Rotate 45 /Resources << >> >>\nendobj\n",
            &[],
            4,
            DocumentErrorCode::InvalidPageRotation,
        ),
        (
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources [] >>\nendobj\n",
            &[],
            4,
            DocumentErrorCode::InvalidPageResources,
        ),
        (
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /MediaBox [0 0 200 200] /Resources << >> >>\nendobj\n",
            &[],
            4,
            DocumentErrorCode::DuplicateStructuralKey,
        ),
        (
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources 4 0 R >>\nendobj\n",
            &[(4, b"4 0 obj\n17\nendobj\n")],
            5,
            DocumentErrorCode::InvalidPageResources,
        ),
        (
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 4 0 R 100] /Resources << >> >>\nendobj\n",
            &[(4, b"4 0 obj\n50\nendobj\n")],
            5,
            DocumentErrorCode::UnsupportedPageValueRepresentation,
        ),
    ];

    for (case_index, &(page, extras, size, expected)) in cases.iter().enumerate() {
        let salt = 0xe0 + u8::try_from(case_index).unwrap();
        let fixture = one_page_fixture(page, extras, size, salt);
        let seed = 7_701 + u64::try_from(case_index).unwrap() * 30;
        let prepared = prepare(&fixture, seed);
        let mut job = prepared
            .authority
            .materialize_page(
                &prepared.index,
                prepared.handle,
                materialization_context(seed + 20),
                PageMaterializationLimits::default(),
            )
            .unwrap();
        let failure = poll_failure(&mut job, &prepared.store, &DocumentNeverCancelled);
        assert_eq!(failure.code(), expected, "case {case_index}");
        let repeated = poll_failure(&mut job, &prepared.store, &DocumentNeverCancelled);
        assert_eq!(repeated, failure, "case {case_index}");
    }
}

#[test]
fn ancestor_depth_budget_accepts_exact_depth_and_rejects_one_less_before_work() {
    let prepared = prepare(&inherited_defaults_fixture(), 7_951);
    let exact = materialize_ready(
        &prepared,
        limits_with(DocumentLimitKind::PageMaterializationAncestors, 3),
        7_971,
    );
    assert_eq!(exact.stats().ancestors_opened(), 3);

    let error = failure_with_limits(
        &prepared,
        limits_with(DocumentLimitKind::PageMaterializationAncestors, 2),
        7_981,
    );
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    let detail = error.limit().expect("depth failure retains limit detail");
    assert_eq!(
        detail.kind(),
        DocumentLimitKind::PageMaterializationAncestors
    );
    assert_eq!(detail.limit(), 2);
    assert_eq!(detail.consumed(), 0);
    assert_eq!(detail.attempted(), 3);
}

#[test]
fn aggregate_materialization_budgets_accept_exact_work_and_reject_one_less() {
    let prepared = prepare(&alias_fixture(), 8_001);
    let baseline = materialize_ready(&prepared, PageMaterializationLimits::default(), 8_021);
    let stats = baseline.stats();
    let cases = [
        (
            DocumentLimitKind::PageMaterializationObjects,
            stats.objects_started(),
        ),
        (
            DocumentLimitKind::PageMaterializationReferenceEdges,
            stats.reference_edges(),
        ),
        (
            DocumentLimitKind::PageMaterializationObjectReadBytes,
            stats.object_read_bytes(),
        ),
        (
            DocumentLimitKind::PageMaterializationObjectParseBytes,
            stats.object_parse_bytes(),
        ),
        (
            DocumentLimitKind::PageMaterializationStateBytes,
            stats.peak_retained_state_bytes(),
        ),
    ];

    for (case_index, (kind, measured)) in cases.into_iter().enumerate() {
        assert!(measured > 1, "{kind:?} baseline must support one-less");
        let seed = 8_101 + u64::try_from(case_index).unwrap() * 20;
        let exact = materialize_ready(&prepared, limits_with(kind, measured), seed);
        let exact_stats = exact.stats();
        let exact_value = match kind {
            DocumentLimitKind::PageMaterializationObjects => exact_stats.objects_started(),
            DocumentLimitKind::PageMaterializationReferenceEdges => exact_stats.reference_edges(),
            DocumentLimitKind::PageMaterializationObjectReadBytes => {
                exact_stats.object_read_bytes()
            }
            DocumentLimitKind::PageMaterializationObjectParseBytes => {
                exact_stats.object_parse_bytes()
            }
            DocumentLimitKind::PageMaterializationStateBytes => {
                exact_stats.peak_retained_state_bytes()
            }
            _ => unreachable!("cases contain only aggregate materialization budgets"),
        };
        assert_eq!(exact_value, measured, "{kind:?} exact budget drifted");

        let error = failure_with_limits(&prepared, limits_with(kind, measured - 1), seed + 10);
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit, "{kind:?}");
        let detail = error.limit().expect("budget failure retains limit detail");
        assert_eq!(detail.kind(), kind);
        assert_eq!(detail.limit(), measured - 1);
        assert!(detail.attempted() > 0);
    }
}

#[test]
fn retained_state_budget_includes_transient_ancestor_syntax() {
    let prepared = prepare(&transient_page_syntax_fixture(), 8_251);
    let baseline = materialize_ready(&prepared, PageMaterializationLimits::default(), 8_271);
    assert!(
        baseline.stats().peak_retained_state_bytes() > baseline.stats().retained_state_bytes(),
        "the large Page-only syntax must affect peak state without entering the published value"
    );

    let error = failure_with_limits(
        &prepared,
        limits_with(
            DocumentLimitKind::PageMaterializationStateBytes,
            baseline.stats().retained_state_bytes(),
        ),
        8_281,
    );
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().expect("state failure retains detail").kind(),
        DocumentLimitKind::PageMaterializationStateBytes
    );
}

#[test]
fn source_mismatch_precedes_cancellation_and_terminal_failures_replay_without_work() {
    let prepared = prepare(&direct_values_fixture(), 8_301);
    let mut changed = prepared
        .authority
        .materialize_page(
            &prepared.index,
            prepared.handle,
            materialization_context(8_321),
            PageMaterializationLimits::default(),
        )
        .unwrap();
    let wrong = snapshot(
        prepared
            .store
            .snapshot()
            .len()
            .expect("fixture source has a known length"),
        0xf1,
    );
    let mismatch = poll_failure(&mut changed, &PanicSource(wrong), &Cancelled);
    assert_eq!(mismatch.code(), DocumentErrorCode::SourceSnapshotMismatch);
    assert_eq!(changed.phase(), PageMaterializationPhase::Failed);
    assert_eq!(changed.stats().objects_started(), 0);
    assert_eq!(changed.stats().object_read_bytes(), 0);
    assert_eq!(changed.stats().object_parse_bytes(), 0);
    let repeated = poll_failure(&mut changed, &prepared.store, &DocumentNeverCancelled);
    assert_eq!(repeated, mismatch);
    assert_eq!(changed.stats().objects_started(), 0);

    let mut cancelled = prepared
        .authority
        .materialize_page(
            &prepared.index,
            prepared.handle,
            materialization_context(8_331),
            PageMaterializationLimits::default(),
        )
        .unwrap();
    let cancellation = poll_failure(
        &mut cancelled,
        &PanicSource(prepared.store.snapshot()),
        &Cancelled,
    );
    assert_eq!(cancellation.code(), DocumentErrorCode::Cancelled);
    assert_eq!(cancelled.phase(), PageMaterializationPhase::Failed);
    assert_eq!(cancelled.stats().objects_started(), 0);
    let repeated = poll_failure(&mut cancelled, &prepared.store, &DocumentNeverCancelled);
    assert_eq!(repeated, cancellation);
    assert_eq!(cancelled.stats().objects_started(), 0);
}

#[test]
fn pending_object_reads_replay_without_double_charging_then_resume() {
    let fixture = direct_values_fixture();
    let prepared = prepare(&fixture, 8_351);
    let sparse = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = prepared
        .authority
        .materialize_page(
            &prepared.index,
            prepared.handle,
            materialization_context(8_371),
            PageMaterializationLimits::default(),
        )
        .unwrap();

    let first = match job.poll(&sparse, &DocumentNeverCancelled) {
        PageMaterializationPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => (ticket, missing, checkpoint),
        PageMaterializationPoll::Ready(_) => panic!("empty source must suspend"),
        PageMaterializationPoll::Failed(error) => panic!("pending materialization failed: {error}"),
    };
    let charged = job.stats();
    let second = match job.poll(&sparse, &DocumentNeverCancelled) {
        PageMaterializationPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => (ticket, missing, checkpoint),
        PageMaterializationPoll::Ready(_) => panic!("unchanged empty source must remain pending"),
        PageMaterializationPoll::Failed(error) => {
            panic!("replayed pending materialization failed: {error}")
        }
    };
    assert_eq!(second, first);
    assert_eq!(job.stats(), charged);

    let full = ByteRange::new(
        0,
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
    )
    .unwrap();
    sparse
        .supply(RangeResponse::new(fixture.snapshot, full, fixture.bytes).unwrap())
        .unwrap();
    match job.poll(&sparse, &DocumentNeverCancelled) {
        PageMaterializationPoll::Ready(page) => {
            assert_eq!(page.handle(), prepared.handle);
            assert_eq!(page.rotation(), PageRotation::Degrees270);
        }
        PageMaterializationPoll::Pending { .. } => {
            panic!("complete resumed source must publish without another suspension")
        }
        PageMaterializationPoll::Failed(error) => {
            panic!("resumed materialization failed: {error}")
        }
    }
}

#[test]
fn constructor_rejects_authority_handle_and_context_mismatches_before_source_work() {
    let first = prepare(&direct_values_fixture(), 8_401);
    let mut cold_build = first
        .authority
        .build_page_index(tree_context(8_431), tree_limits(), index_limits())
        .expect("same authority can rebuild a cold page index");
    let cold_index = match cold_build.poll(&first.store, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Ready(index) => index,
        PageIndexBuildPoll::Pending { .. } => panic!("resident cold build must not suspend"),
        PageIndexBuildPoll::Failed(error) => panic!("cold page index rebuild failed: {error}"),
    };
    let unresolved_handle = match first.authority.materialize_page(
        &cold_index,
        first.handle,
        materialization_context(8_441),
        PageMaterializationLimits::default(),
    ) {
        Err(error) => error,
        Ok(_) => panic!("a cold index must not materialize a handle minted by a refinement"),
    };
    assert_eq!(unresolved_handle.code(), DocumentErrorCode::StalePageHandle);

    let other_fixture = one_page_fixture(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 20] /Resources << >> >>\nendobj\n",
        &[],
        4,
        0xf2,
    );
    let other = prepare(&other_fixture, 8_451);

    let authority_mismatch = match other.authority.materialize_page(
        &first.index,
        first.handle,
        materialization_context(8_471),
        PageMaterializationLimits::default(),
    ) {
        Err(error) => error,
        Ok(_) => panic!("an unrelated attested authority must not borrow another PageIndex"),
    };
    assert_eq!(
        authority_mismatch.code(),
        DocumentErrorCode::AttestedObjectEvidenceMismatch
    );

    let stale_handle = match first.authority.materialize_page(
        &first.index,
        other.handle,
        materialization_context(8_481),
        PageMaterializationLimits::default(),
    ) {
        Err(error) => error,
        Ok(_) => panic!("a PageHandle from another immutable binding must be stale"),
    };
    assert_eq!(stale_handle.code(), DocumentErrorCode::StalePageHandle);

    let invalid_context = PageMaterializationJobContext::new(
        JobId::new(8_491),
        ResumeCheckpoint::new(8_492),
        ResumeCheckpoint::new(8_492),
        RequestPriority::VisiblePage,
    );
    let context_error = match first.authority.materialize_page(
        &first.index,
        first.handle,
        invalid_context,
        PageMaterializationLimits::default(),
    ) {
        Err(error) => error,
        Ok(_) => panic!("identical child checkpoints must not create a job"),
    };
    assert_eq!(
        context_error.code(),
        DocumentErrorCode::InvalidPageMaterializationJobContext
    );
}

#[test]
fn owned_shared_materialization_job_retains_the_attested_authority() {
    let Prepared {
        authority,
        store,
        index,
        handle,
    } = prepare(&direct_values_fixture(), 8_501);
    let shared = authority.into_shared();
    let mut job: MaterializePageJob<'static> = shared
        .materialize_page_owned(
            &index,
            handle,
            materialization_context(8_521),
            PageMaterializationLimits::default(),
        )
        .unwrap();
    drop(shared);

    match job.poll(&store, &DocumentNeverCancelled) {
        PageMaterializationPoll::Ready(page) => {
            assert_eq!(page.handle(), handle);
            assert_eq!(page.rotation(), PageRotation::Degrees270);
        }
        PageMaterializationPoll::Pending { .. } => {
            panic!("resident owned materialization must not suspend")
        }
        PageMaterializationPoll::Failed(error) => {
            panic!("owned materialization failed: {error}")
        }
    }
}

#[allow(clippy::result_large_err, dead_code)]
fn repaired_materialization_api_is_available(
    repaired: &pdf_rs_document::LocallyRepairedRevisionIndex,
    shared: &pdf_rs_document::SharedLocallyRepairedRevisionIndex,
    index: &PageIndex,
    handle: PageHandle,
) -> Result<(), DocumentError> {
    let _borrowed = repaired.materialize_page(
        index,
        handle,
        materialization_context(8_601),
        PageMaterializationLimits::default(),
    )?;
    let _owned = shared.materialize_page_owned(
        index,
        handle,
        materialization_context(8_611),
        PageMaterializationLimits::default(),
    )?;
    Ok(())
}
