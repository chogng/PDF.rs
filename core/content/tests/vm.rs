use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_content::{
    ContentLimitConfig, ContentLimitKind, ContentLimits, ContentUnsupportedKind,
    ContentVmErrorCode, ContentVmFailure, ContentVmLimitConfig, ContentVmLimitKind,
    ContentVmLimits, ContentVmPhase, ContentVmPoll, InterpretPageJob, OperatorFailurePolicy,
    OperatorKind,
};
use pdf_rs_document::{
    AcquiredPageContent, AttestRevisionJob, CandidateRevisionIndex, DocumentCancellation,
    DocumentErrorCode, DocumentLimitKind, DocumentLimits, NeverCancelled as DocumentNeverCancelled,
    PageContentJobContext, PageContentLimits, PageContentPoll, PageIndexBuildPoll, PageIndexLimits,
    PageLookupPoll, PageMaterializationJobContext, PageMaterializationLimits,
    PageMaterializationPoll, PagePropertyLookupLimitConfig, PagePropertyLookupLimits,
    PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits, RevisionAttestationJobContext,
    RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_scene::{
    Matrix, SceneErrorCode, SceneLimitConfig, SceneLimitKind, SceneLimits, SceneScalar,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
};

const REVISION_ID: RevisionId = RevisionId::new(86);
const CATALOG: &[u8] = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
const PAGE_ROOT: &[u8] = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";
const DEFAULT_RESOURCES: &[u8] = b"<< >>";
const PROPERTY_RESOURCES: &[u8] = b"<< /Properties << /P 7 0 R >> >>";

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

struct VmInput {
    acquired: AcquiredPageContent,
    store: RangeStore,
    snapshot: SourceSnapshot,
}

fn snapshot(len: u64, salt: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([salt; 32]),
            SourceRevision::new(u64::from(salt) + 1),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [salt ^ 0x5d; 32]),
    )
}

fn fixture(content: Option<&[u8]>, resources: &[u8], salt: u8) -> Fixture {
    let mut page =
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources ".to_vec();
    page.extend_from_slice(resources);
    if content.is_some() {
        page.extend_from_slice(b" /Contents 4 0 R");
    }
    page.extend_from_slice(b" >>\nendobj\n");

    let mut bodies = vec![
        (1_u32, CATALOG.to_vec()),
        (2, PAGE_ROOT.to_vec()),
        (3, page),
    ];
    if let Some(content) = content {
        let mut stream = format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes();
        stream.extend_from_slice(content);
        stream.extend_from_slice(b"\nendstream\nendobj\n");
        bodies.push((4, stream));
    }

    let size = 9_u32;
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in bodies {
        offsets.push((
            number,
            u64::try_from(bytes.len()).expect("fixture offset fits u64"),
        ));
        bytes.extend_from_slice(&body);
    }
    let startxref = u64::try_from(bytes.len()).expect("fixture offset fits u64");
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for number in 0..size {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else if let Some((_, offset)) = offsets.iter().find(|(entry, _)| *entry == number) {
            format!("{offset:010} 00000 n \n")
        } else {
            "0000000000 00000 f \n".to_owned()
        };
        bytes.extend_from_slice(row.as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
            .as_bytes(),
    );
    Fixture {
        snapshot: snapshot(u64::try_from(bytes.len()).unwrap(), salt),
        bytes,
    }
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(fixture.bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone()).unwrap())
        .unwrap();
    store
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

fn content_context(seed: u64) -> PageContentJobContext {
    PageContentJobContext::new(
        JobId::new(seed),
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        ResumeCheckpoint::new(seed + 3),
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
    .unwrap()
}

fn acquire(content: Option<&[u8]>, resources: &[u8], salt: u8) -> VmInput {
    let fixture = fixture(content, resources, salt);
    let store = supplied_store(&fixture);
    let mut xref = OpenXrefJob::new(
        fixture.snapshot,
        XrefJobContext::new(
            JobId::new(20_001),
            ResumeCheckpoint::new(20_002),
            ResumeCheckpoint::new(20_003),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let section = match xref.poll(&store, &XrefNeverCancelled) {
        XrefPoll::Ready(section) => section,
        outcome => panic!("strict xref must be ready: {outcome:?}"),
    };
    let candidate = CandidateRevisionIndex::from_xref(
        &section,
        REVISION_ID,
        DocumentLimits::default(),
        &DocumentNeverCancelled,
    )
    .unwrap();
    let mut attest = AttestRevisionJob::new(
        candidate,
        RevisionAttestationJobContext::new(
            JobId::new(20_011),
            ResumeCheckpoint::new(20_012),
            ResumeCheckpoint::new(20_013),
            ResumeCheckpoint::new(20_014),
            RequestPriority::Metadata,
        ),
        RevisionAttestationLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    let authority = match attest.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Ready(index) => index,
        outcome => panic!("strict revision must attest: {outcome:?}"),
    };
    let mut build = authority
        .build_page_index(
            tree_context(20_021),
            tree_limits(),
            PageIndexLimits::new(4, 16 << 10).unwrap(),
        )
        .unwrap();
    let cold = match build.poll(&store, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Ready(index) => index,
        outcome => panic!("strict Page index must build: {outcome:?}"),
    };
    let mut lookup = authority
        .lookup_page(&cold, 0, tree_context(20_031), tree_limits())
        .unwrap();
    let lookup = match lookup.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Ready(lookup) => lookup,
        outcome => panic!("strict Page lookup must finish: {outcome:?}"),
    };
    let (index, handle) = lookup.into_parts();
    let mut materialize = authority
        .materialize_page(
            &index,
            handle,
            materialization_context(20_041),
            PageMaterializationLimits::default(),
        )
        .unwrap();
    let page = match materialize.poll(&store, &DocumentNeverCancelled) {
        PageMaterializationPoll::Ready(page) => page,
        outcome => panic!("strict Page materialization must finish: {outcome:?}"),
    };
    let mut content_job = authority
        .acquire_page_content(
            &index,
            page,
            content_context(20_051),
            PageContentLimits::default(),
        )
        .unwrap();
    let acquired = match content_job.poll(&store, &DocumentNeverCancelled) {
        PageContentPoll::Ready(content) => content,
        outcome => panic!("strict Page content acquisition must finish: {outcome:?}"),
    };
    VmInput {
        acquired,
        store,
        snapshot: fixture.snapshot,
    }
}

fn job_with(
    input: VmInput,
    scan_limits: ContentLimits,
    vm_limits: ContentVmLimits,
    property_limits: PagePropertyLookupLimits,
    scene_limits: SceneLimits,
) -> (InterpretPageJob, RangeStore, SourceSnapshot) {
    (
        InterpretPageJob::new(
            input.acquired,
            scan_limits,
            vm_limits,
            property_limits,
            scene_limits,
        ),
        input.store,
        input.snapshot,
    )
}

fn default_job(
    content: Option<&[u8]>,
    resources: &[u8],
    salt: u8,
) -> (InterpretPageJob, RangeStore, SourceSnapshot) {
    job_with(
        acquire(content, resources, salt),
        ContentLimits::default(),
        ContentVmLimits::default(),
        PagePropertyLookupLimits::default(),
        SceneLimits::default(),
    )
}

fn ready(
    content: Option<&[u8]>,
    resources: &[u8],
    salt: u8,
) -> Arc<pdf_rs_content::InterpretedPage> {
    let (mut job, store, _) = default_job(content, resources, salt);
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("fixture must interpret: {outcome:?}"),
    }
}

fn vm_failure(content: &[u8], resources: &[u8], salt: u8) -> pdf_rs_content::ContentVmError {
    let (mut job, store, _) = default_job(Some(content), resources, salt);
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => error,
        outcome => panic!("fixture must fail in VM: {outcome:?}"),
    }
}

fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).unwrap()
}

#[test]
fn strict_pipeline_executes_state_and_publishes_only_marked_content_scene_commands() {
    let page = ready(
        Some(
            b"q 2 0 0 2 10 20 cm Q BT ET BX ignored EX \
              /Outer BMC /Inner /P BDC EMC EMC",
        ),
        PROPERTY_RESOURCES,
        0x81,
    );
    assert_eq!(page.final_ctm(), Matrix::IDENTITY);
    assert_eq!(page.scene().commands().len(), 4);
    assert_eq!(page.scene().resources().len(), 1);
    assert_eq!(page.scene().resources()[0].object(), object_ref(7));
    assert_eq!(page.scene().commands()[0].tag().unwrap().bytes(), b"Outer");
    assert_eq!(page.scene().commands()[1].tag().unwrap().bytes(), b"Inner");
    assert_eq!(page.scene().provenance()[0].operator_index(), 8);
    assert_eq!(page.scene().provenance()[1].operator_index(), 9);
    assert_eq!(page.property_uses().len(), 1);
    assert_eq!(page.property_uses()[0].property().target(), object_ref(7));
    assert_eq!(page.property_uses()[0].source().page_operator_ordinal(), 9);
    assert_eq!(page.vm_stats().operators(), 12);
    assert_eq!(page.vm_stats().max_graphics_state_depth(), 1);
    assert_eq!(page.vm_stats().max_compatibility_depth(), 1);
    assert_eq!(page.vm_stats().max_marked_content_depth(), 2);
    assert_eq!(page.property_stats().lookups(), 1);
}

#[test]
fn empty_page_content_ready_replays_and_owned_result_survives_source_inputs() {
    let (mut job, store, snapshot) = default_job(None, DEFAULT_RESOURCES, 0x82);
    assert_eq!(job.phase(), ContentVmPhase::Pending);
    let first = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("empty acquired Page must publish an empty Scene: {outcome:?}"),
    };
    assert_eq!(job.phase(), ContentVmPhase::Ready);
    assert!(first.scene().commands().is_empty());
    assert!(first.scene().resources().is_empty());
    assert_eq!(first.final_ctm(), Matrix::IDENTITY);
    let replay = match job.poll(&PanicSource(snapshot), &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("terminal Ready must replay: {outcome:?}"),
    };
    assert!(Arc::ptr_eq(&first, &replay));
    drop(job);
    drop(store);
    assert!(first.acquired_content().is_empty());
    assert_eq!(first.page().handle().snapshot(), snapshot);
}

#[test]
fn matrix_composition_matches_one_equivalent_operator() {
    let composed = ready(
        Some(b"2 0 0 2 10 20 cm 1 0 0 1 3 4 cm"),
        DEFAULT_RESOURCES,
        0x83,
    );
    let equivalent = ready(Some(b"2 0 0 2 16 28 cm"), DEFAULT_RESOURCES, 0x84);
    assert_eq!(composed.final_ctm(), equivalent.final_ctm());
    assert_eq!(
        composed.final_ctm(),
        Matrix::new([
            SceneScalar::from_scaled(2_000_000_000),
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            SceneScalar::from_scaled(2_000_000_000),
            SceneScalar::from_scaled(16_000_000_000),
            SceneScalar::from_scaled(28_000_000_000),
        ])
    );
}

#[test]
fn every_stack_rejects_underflow_and_terminal_imbalance_in_stable_order() {
    for (case, (content, code)) in [
        (b"Q".as_slice(), ContentVmErrorCode::InvalidGraphicsState),
        (b"q".as_slice(), ContentVmErrorCode::InvalidGraphicsState),
        (b"ET".as_slice(), ContentVmErrorCode::InvalidTextObject),
        (b"BT".as_slice(), ContentVmErrorCode::InvalidTextObject),
        (
            b"EX".as_slice(),
            ContentVmErrorCode::InvalidCompatibilityState,
        ),
        (
            b"BX".as_slice(),
            ContentVmErrorCode::InvalidCompatibilityState,
        ),
        (
            b"EMC".as_slice(),
            ContentVmErrorCode::InvalidMarkedContentState,
        ),
        (
            b"/A BMC".as_slice(),
            ContentVmErrorCode::InvalidMarkedContentState,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            vm_failure(
                content,
                DEFAULT_RESOURCES,
                0x90 + u8::try_from(case).unwrap()
            )
            .code(),
            code,
            "stack case {case}"
        );
    }
    assert_eq!(
        vm_failure(b"q BT", DEFAULT_RESOURCES, 0x99).code(),
        ContentVmErrorCode::InvalidGraphicsState
    );
    assert_eq!(
        vm_failure(b"BT BT", DEFAULT_RESOURCES, 0x9a).code(),
        ContentVmErrorCode::InvalidTextObject
    );
}

#[test]
fn every_operator_shape_validates_count_and_type_before_state_or_unsupported_policy() {
    for (case, token) in [b"q".as_slice(), b"Q", b"BT", b"ET", b"BX", b"EX", b"EMC"]
        .into_iter()
        .enumerate()
    {
        let mut content = b"1 ".to_vec();
        content.extend_from_slice(token);
        assert_eq!(
            vm_failure(
                &content,
                DEFAULT_RESOURCES,
                0xa0 + u8::try_from(case).unwrap()
            )
            .code(),
            ContentVmErrorCode::InvalidOperandCount
        );
    }
    for (case, (content, code)) in [
        (
            b"1 0 0 1 0 cm".as_slice(),
            ContentVmErrorCode::InvalidOperandCount,
        ),
        (
            b"/A 0 0 1 0 0 cm".as_slice(),
            ContentVmErrorCode::InvalidOperandType,
        ),
        (b"MP".as_slice(), ContentVmErrorCode::InvalidOperandCount),
        (b"1 MP".as_slice(), ContentVmErrorCode::InvalidOperandType),
        (b"/A DP".as_slice(), ContentVmErrorCode::InvalidOperandCount),
        (
            b"1 /P DP".as_slice(),
            ContentVmErrorCode::InvalidOperandType,
        ),
        (
            b"/A 1 DP".as_slice(),
            ContentVmErrorCode::InvalidOperandType,
        ),
        (b"BMC".as_slice(), ContentVmErrorCode::InvalidOperandCount),
        (b"1 BMC".as_slice(), ContentVmErrorCode::InvalidOperandType),
        (
            b"/A BDC".as_slice(),
            ContentVmErrorCode::InvalidOperandCount,
        ),
        (
            b"1 /P BDC".as_slice(),
            ContentVmErrorCode::InvalidOperandType,
        ),
        (
            b"/A 1 BDC".as_slice(),
            ContentVmErrorCode::InvalidOperandType,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            vm_failure(
                content,
                PROPERTY_RESOURCES,
                0xb0 + u8::try_from(case).unwrap()
            )
            .code(),
            code,
            "operand case {case}"
        );
    }
}

#[test]
fn exact_number_conversion_distinguishes_precision_and_overflow() {
    assert_eq!(
        vm_failure(b"0.0000000001 0 0 1 0 0 cm", DEFAULT_RESOURCES, 0xc0).code(),
        ContentVmErrorCode::NumericPrecision
    );
    assert_eq!(
        vm_failure(b"9223372037 0 0 1 0 0 cm", DEFAULT_RESOURCES, 0xc1).code(),
        ContentVmErrorCode::NumericOverflow
    );
}

#[test]
fn unsupported_policy_matches_operator_table_and_bx_ignores_only_unknown_operators() {
    assert_eq!(
        OperatorKind::MarkedContentPoint.spec().failure_policy(),
        OperatorFailurePolicy::ValidateThenUnsupported
    );
    assert_eq!(
        OperatorKind::MarkedContentPointProperties
            .spec()
            .failure_policy(),
        OperatorFailurePolicy::ValidateThenUnsupported
    );
    for (case, (content, kind)) in [
        (
            b"/A MP".as_slice(),
            ContentUnsupportedKind::MarkedContentPoint,
        ),
        (
            b"/A /P DP".as_slice(),
            ContentUnsupportedKind::MarkedContentPointProperties,
        ),
        (
            b"unknown".as_slice(),
            ContentUnsupportedKind::UnknownOperator,
        ),
        (
            b"BX /A MP EX".as_slice(),
            ContentUnsupportedKind::MarkedContentPoint,
        ),
        (
            b"/A << /Secret 1 >> BDC".as_slice(),
            ContentUnsupportedKind::DirectContentPropertyDictionary,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut job, store, snapshot) = default_job(
            Some(content),
            PROPERTY_RESOURCES,
            0xd0 + u8::try_from(case).unwrap(),
        );
        let first = match job.poll(&store, &DocumentNeverCancelled) {
            ContentVmPoll::Unsupported(value) => value,
            outcome => panic!("unsupported case must not publish a Scene: {outcome:?}"),
        };
        assert_eq!(first.kind(), kind);
        assert_eq!(job.phase(), ContentVmPhase::Unsupported);
        match job.poll(&PanicSource(snapshot), &DocumentNeverCancelled) {
            ContentVmPoll::Unsupported(replay) => assert_eq!(replay, first),
            outcome => panic!("Unsupported must replay: {outcome:?}"),
        }
    }
    let ignored = ready(Some(b"BX unknown EX"), DEFAULT_RESOURCES, 0xd8);
    assert!(ignored.scene().commands().is_empty());
}

#[test]
fn property_shapes_map_to_document_failure_or_structured_unsupported() {
    for (case, (resources, expected)) in [
        (
            b"<< /Properties 8 0 R >>".as_slice(),
            ContentUnsupportedKind::IndirectPageProperties,
        ),
        (
            b"<< /Properties << /P << /K 1 >> >> >>".as_slice(),
            ContentUnsupportedKind::DirectPagePropertyDictionary,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut job, store, _) = default_job(
            Some(b"/A /P BDC"),
            resources,
            0xe0 + u8::try_from(case).unwrap(),
        );
        match job.poll(&store, &DocumentNeverCancelled) {
            ContentVmPoll::Unsupported(value) => assert_eq!(value.kind(), expected),
            outcome => panic!("unsupported resource shape expected: {outcome:?}"),
        }
    }

    for (case, (resources, code)) in [
        (
            b"<< /Font << >> >>".as_slice(),
            DocumentErrorCode::InvalidPagePropertyResource,
        ),
        (
            b"<< /Properties << >> >>".as_slice(),
            DocumentErrorCode::InvalidPagePropertyResource,
        ),
        (
            b"<< /Properties << /P 7 >> >>".as_slice(),
            DocumentErrorCode::InvalidPagePropertyResource,
        ),
        (
            b"<< /Properties << /P 7 0 R /P 8 0 R >> >>".as_slice(),
            DocumentErrorCode::DuplicateStructuralKey,
        ),
        (
            b"<< /Properties << /P 7 0 R >> /Properties << /P 8 0 R >> >>".as_slice(),
            DocumentErrorCode::DuplicateStructuralKey,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut job, store, _) = default_job(
            Some(b"/A /P BDC"),
            resources,
            0xe4 + u8::try_from(case).unwrap(),
        );
        match job.poll(&store, &DocumentNeverCancelled) {
            ContentVmPoll::Failed(ContentVmFailure::Document(error)) => {
                assert_eq!(error.code(), code)
            }
            outcome => panic!("invalid resource shape expected: {outcome:?}"),
        }
    }
}

#[test]
fn successful_bdc_uses_only_acquired_bytes_and_no_io_on_first_poll() {
    let input = acquire(Some(b"/A /P BDC EMC"), PROPERTY_RESOURCES, 0xe9);
    let snapshot = input.snapshot;
    let mut job = InterpretPageJob::new(
        input.acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        PagePropertyLookupLimits::default(),
        SceneLimits::default(),
    );
    drop(input.store);
    match job.poll(&PanicSource(snapshot), &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => {
            assert_eq!(page.scene().commands().len(), 2);
            assert_eq!(page.property_uses().len(), 1);
        }
        outcome => panic!("sealed BDC interpretation must not request source bytes: {outcome:?}"),
    }
}

#[test]
fn repeated_property_use_interns_one_scene_resource_and_retains_both_proofs() {
    let page = ready(
        Some(b"/A /P BDC EMC /B /P BDC EMC"),
        PROPERTY_RESOURCES,
        0xea,
    );
    assert_eq!(page.scene().resources().len(), 1);
    assert_eq!(page.scene().commands().len(), 4);
    assert_eq!(page.property_uses().len(), 2);
    assert_eq!(page.property_stats().lookups(), 2);
    assert_eq!(page.property_uses()[0].property().target(), object_ref(7));
    assert_eq!(page.property_uses()[1].property().target(), object_ref(7));
}

struct PanicSource(SourceSnapshot);

impl ByteSource for PanicSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("sealed VM must not poll already-acquired content")
    }
}

struct AlwaysCancelled;

impl DocumentCancellation for AlwaysCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

struct MutableSource {
    original: SourceSnapshot,
    replacement: SourceSnapshot,
    changed: AtomicBool,
}

impl ByteSource for MutableSource {
    fn snapshot(&self) -> SourceSnapshot {
        if self.changed.load(Ordering::Acquire) {
            self.replacement
        } else {
            self.original
        }
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("sealed VM must not poll already-acquired content")
    }
}

struct FlipAndCancel<'a> {
    source: &'a MutableSource,
    probes: AtomicUsize,
    flip_at: usize,
}

impl DocumentCancellation for FlipAndCancel<'_> {
    fn is_cancelled(&self) -> bool {
        let probe = self.probes.fetch_add(1, Ordering::AcqRel) + 1;
        if probe >= self.flip_at {
            self.source.changed.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }
}

#[test]
fn source_change_precedes_cancellation_before_work_and_during_scanning() {
    let input = acquire(Some(b"q Q"), DEFAULT_RESOURCES, 0xeb);
    let wrong = snapshot(input.snapshot.len().unwrap(), 0xec);
    let mut job = InterpretPageJob::new(
        input.acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        PagePropertyLookupLimits::default(),
        SceneLimits::default(),
    );
    match job.poll(&PanicSource(wrong), &AlwaysCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            assert_eq!(error.code(), ContentVmErrorCode::SourceSnapshotMismatch)
        }
        outcome => panic!("foreign source must win: {outcome:?}"),
    }

    let input = acquire(Some(b"q Q"), DEFAULT_RESOURCES, 0xed);
    let replacement = snapshot(input.snapshot.len().unwrap(), 0xee);
    let source = MutableSource {
        original: input.snapshot,
        replacement,
        changed: AtomicBool::new(false),
    };
    let cancellation = FlipAndCancel {
        source: &source,
        probes: AtomicUsize::new(0),
        flip_at: 2,
    };
    let mut job = InterpretPageJob::new(
        input.acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        PagePropertyLookupLimits::default(),
        SceneLimits::default(),
    );
    match job.poll(&source, &cancellation) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            assert_eq!(error.code(), ContentVmErrorCode::SourceSnapshotMismatch)
        }
        outcome => panic!("mutation during cancellation must win: {outcome:?}"),
    }
    assert!(cancellation.probes.load(Ordering::Acquire) >= 2);
}

#[test]
fn cancellation_only_has_stable_vm_failure_and_terminal_replay() {
    let input = acquire(Some(b"q Q"), DEFAULT_RESOURCES, 0xef);
    let snapshot = input.snapshot;
    let mut job = InterpretPageJob::new(
        input.acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        PagePropertyLookupLimits::default(),
        SceneLimits::default(),
    );
    let first = match job.poll(&PanicSource(snapshot), &AlwaysCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            assert_eq!(error.code(), ContentVmErrorCode::Cancelled);
            assert_eq!(error.diagnostic_id(), "RPE-CONTENT-VM-0011");
            ContentVmFailure::Vm(error)
        }
        outcome => panic!("correct-source cancellation must fail in VM: {outcome:?}"),
    };
    assert_eq!(job.phase(), ContentVmPhase::Failed);
    match job.poll(&PanicSource(snapshot), &DocumentNeverCancelled) {
        ContentVmPoll::Failed(replay) => assert_eq!(replay, first),
        outcome => panic!("Cancelled failure must replay: {outcome:?}"),
    }
}

fn limits_with(kind: ContentVmLimitKind, value: u64) -> ContentVmLimits {
    let mut config = ContentVmLimitConfig::default();
    match kind {
        ContentVmLimitKind::Operators => config.max_operators = value,
        ContentVmLimitKind::Fuel => config.max_fuel = value,
        ContentVmLimitKind::GraphicsStateDepth => {
            config.max_graphics_state_depth = u32::try_from(value).unwrap();
        }
        ContentVmLimitKind::CompatibilityDepth => {
            config.max_compatibility_depth = u32::try_from(value).unwrap();
        }
        ContentVmLimitKind::MarkedContentDepth => {
            config.max_marked_content_depth = u32::try_from(value).unwrap();
        }
        ContentVmLimitKind::PropertyUses => config.max_property_uses = value,
        ContentVmLimitKind::RetainedBytes => config.max_retained_bytes = value,
        ContentVmLimitKind::Allocation => panic!("allocation failure cannot be forced portably"),
    }
    ContentVmLimits::validate(config).unwrap()
}

#[test]
fn every_vm_budget_accepts_exact_measured_work_and_rejects_one_less() {
    let content = b"q q Q Q BX BX EX EX /A BMC /B /P BDC /C /P BDC EMC EMC EMC";
    let baseline = ready(Some(content), PROPERTY_RESOURCES, 0xf0);
    let stats = baseline.vm_stats();
    let measured = [
        (ContentVmLimitKind::Operators, stats.operators()),
        (ContentVmLimitKind::Fuel, stats.fuel()),
        (
            ContentVmLimitKind::GraphicsStateDepth,
            u64::from(stats.max_graphics_state_depth()),
        ),
        (
            ContentVmLimitKind::CompatibilityDepth,
            u64::from(stats.max_compatibility_depth()),
        ),
        (
            ContentVmLimitKind::MarkedContentDepth,
            u64::from(stats.max_marked_content_depth()),
        ),
        (ContentVmLimitKind::PropertyUses, stats.property_uses()),
        (
            ContentVmLimitKind::RetainedBytes,
            stats.peak_retained_bytes(),
        ),
    ];
    for (case, (kind, exact)) in measured.into_iter().enumerate() {
        assert!(exact > 1, "{kind:?} fixture must support one-less");
        let exact_input = acquire(
            Some(content),
            PROPERTY_RESOURCES,
            0x20 + u8::try_from(case).unwrap(),
        );
        let (mut exact_job, exact_store, _) = job_with(
            exact_input,
            ContentLimits::default(),
            limits_with(kind, exact),
            PagePropertyLookupLimits::default(),
            SceneLimits::default(),
        );
        assert!(
            matches!(
                exact_job.poll(&exact_store, &DocumentNeverCancelled),
                ContentVmPoll::Ready(_)
            ),
            "exact {kind:?}"
        );

        let tight_input = acquire(
            Some(content),
            PROPERTY_RESOURCES,
            0x30 + u8::try_from(case).unwrap(),
        );
        let (mut tight_job, tight_store, _) = job_with(
            tight_input,
            ContentLimits::default(),
            limits_with(kind, exact - 1),
            PagePropertyLookupLimits::default(),
            SceneLimits::default(),
        );
        match tight_job.poll(&tight_store, &DocumentNeverCancelled) {
            ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                assert_eq!(error.code(), ContentVmErrorCode::ResourceLimit);
                let limit = error.limit().expect("VM resource failure has detail");
                assert_eq!(limit.kind(), kind);
                assert_eq!(limit.limit(), exact - 1);
                assert!(limit.attempted() > 0);
                assert!(limit.consumed() <= limit.limit());
                assert!(
                    limit.attempted() > limit.limit().saturating_sub(limit.consumed()),
                    "{kind:?} evidence proves over-limit work"
                );
            }
            outcome => panic!("one-less {kind:?} must fail: {outcome:?}"),
        }
    }
}

#[test]
fn scanner_document_and_scene_failures_are_preserved_without_vm_remapping() {
    let scan_limits = ContentLimits::validate(ContentLimitConfig {
        max_tokens: 1,
        ..ContentLimitConfig::default()
    })
    .unwrap();
    let input = acquire(Some(b"q Q"), DEFAULT_RESOURCES, 0x41);
    let (mut job, store, snapshot) = job_with(
        input,
        scan_limits,
        ContentVmLimits::default(),
        PagePropertyLookupLimits::default(),
        SceneLimits::default(),
    );
    let first = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Content(error)) => {
            assert_eq!(error.limit().unwrap().kind(), ContentLimitKind::Tokens);
            ContentVmFailure::Content(error)
        }
        outcome => panic!("scanner limit must remain Content failure: {outcome:?}"),
    };
    assert_eq!(first.diagnostic_id(), "RPE-CONTENT-0012");
    match job.poll(&PanicSource(snapshot), &DocumentNeverCancelled) {
        ContentVmPoll::Failed(replay) => assert_eq!(replay, first),
        outcome => panic!("failure must replay: {outcome:?}"),
    }

    let property_limits = PagePropertyLookupLimits::validate(PagePropertyLookupLimitConfig {
        max_lookups: 1,
        max_entry_visits: 1,
    })
    .unwrap();
    let input = acquire(Some(b"/A /P BDC"), PROPERTY_RESOURCES, 0x42);
    let (mut job, store, _) = job_with(
        input,
        ContentLimits::default(),
        ContentVmLimits::default(),
        property_limits,
        SceneLimits::default(),
    );
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Document(error)) => {
            assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
            assert_eq!(
                error.limit().unwrap().kind(),
                DocumentLimitKind::PagePropertyEntryVisits
            );
        }
        outcome => panic!("property limit must remain Document failure: {outcome:?}"),
    }

    let scene_limits = SceneLimits::validate(SceneLimitConfig {
        max_commands: 1,
        ..SceneLimitConfig::default()
    })
    .unwrap();
    let input = acquire(Some(b"/A BMC EMC"), DEFAULT_RESOURCES, 0x43);
    let (mut job, store, _) = job_with(
        input,
        ContentLimits::default(),
        ContentVmLimits::default(),
        PagePropertyLookupLimits::default(),
        scene_limits,
    );
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Scene(error)) => {
            assert_eq!(error.code(), SceneErrorCode::ResourceLimit);
            assert_eq!(error.limit().unwrap().kind(), SceneLimitKind::Commands);
        }
        outcome => panic!("Scene limit must remain Scene failure: {outcome:?}"),
    }
}

#[test]
fn public_debug_output_redacts_content_names_numbers_and_scene_state() {
    let sentinel = b"987654321 0 0 1 0 0 cm /TopSecret BMC EMC";
    let (mut job, store, _) = default_job(Some(sentinel), DEFAULT_RESOURCES, 0x44);
    let pending_debug = format!("{job:?}");
    assert!(pending_debug.contains("[REDACTED]"));
    assert!(!pending_debug.contains("TopSecret"));
    assert!(!pending_debug.contains("987654321"));
    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("sentinel fixture must be valid: {outcome:?}"),
    };
    let debug = format!("{page:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("TopSecret"));
    assert!(!debug.contains("987654321"));
}
