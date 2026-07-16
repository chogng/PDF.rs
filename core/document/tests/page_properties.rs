use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex, DocumentCancellation,
    DocumentErrorCode, DocumentLimitKind, DocumentLimits, MaterializedPage,
    NeverCancelled as DocumentNeverCancelled, PageHandle, PageIndex, PageIndexBuildPoll,
    PageIndexLimits, PageLookupPoll, PageMaterializationJobContext, PageMaterializationLimitConfig,
    PageMaterializationLimits, PageMaterializationPoll, PagePropertyLookupLimitConfig,
    PagePropertyLookupLimits, PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(83);
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

fn snapshot(len: u64, salt: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([salt; 32]),
            SourceRevision::new(u64::from(salt) + 1),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [salt ^ 0xa7; 32]),
    )
}

fn fixture(bodies: Vec<(u32, Vec<u8>)>, size: u32, salt: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut in_use = Vec::new();
    for (number, body) in bodies {
        let offset = u64::try_from(bytes.len()).expect("fixture offset fits u64");
        in_use.push((number, offset));
        bytes.extend_from_slice(&body);
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

fn resource_fixture(
    resources: &[u8],
    mut extras: Vec<(u32, Vec<u8>)>,
    size: u32,
    salt: u8,
) -> Fixture {
    let mut page =
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources ".to_vec();
    page.extend_from_slice(resources);
    page.extend_from_slice(b" >>\nendobj\n");
    let mut bodies = vec![
        (1, CATALOG.to_vec()),
        (2, ONE_PAGE_ROOT.to_vec()),
        (3, page),
    ];
    bodies.append(&mut extras);
    fixture(bodies, size, salt)
}

fn indirect_resource_fixture(resource_dictionary: &[u8], salt: u8) -> Fixture {
    let mut resource_object = b"10 0 obj\n".to_vec();
    resource_object.extend_from_slice(resource_dictionary);
    resource_object.extend_from_slice(b"\nendobj\n");
    resource_fixture(b"10 0 R", vec![(10, resource_object)], 11, salt)
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
            JobId::new(10_101),
            ResumeCheckpoint::new(10_102),
            ResumeCheckpoint::new(10_103),
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
            JobId::new(10_201),
            ResumeCheckpoint::new(10_202),
            ResumeCheckpoint::new(10_203),
            ResumeCheckpoint::new(10_204),
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

fn materialization_limits() -> PageMaterializationLimits {
    PageMaterializationLimits::validate(PageMaterializationLimitConfig {
        max_ancestor_depth: 8,
        max_objects: 16,
        max_reference_edges: 8,
        max_total_object_read_bytes: 1 << 20,
        max_total_object_parse_bytes: 1 << 20,
        max_retained_state_bytes: 1 << 20,
    })
    .expect("test materialization limits validate")
}

fn prepare(fixture: &Fixture, seed: u64) -> Prepared {
    let authority = ready_index(fixture);
    let store = supplied_store(fixture);
    let mut build = authority
        .build_page_index(
            tree_context(seed),
            tree_limits(),
            PageIndexLimits::new(4, 16 << 10).expect("test page-index limits validate"),
        )
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
    Prepared {
        authority,
        store,
        index,
        handle,
    }
}

fn materialize(prepared: &Prepared, seed: u64) -> MaterializedPage {
    let mut job = prepared
        .authority
        .materialize_page(
            &prepared.index,
            prepared.handle,
            materialization_context(seed),
            materialization_limits(),
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

fn property_limits(max_lookups: u64, max_entry_visits: u64) -> PagePropertyLookupLimits {
    PagePropertyLookupLimits::validate(PagePropertyLookupLimitConfig {
        max_lookups,
        max_entry_visits,
    })
    .expect("positive test property limits validate")
}

fn offset_of(bytes: &[u8], needle: &[u8]) -> u64 {
    let index = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("fixture contains expected marker");
    u64::try_from(index).expect("fixture offset fits u64")
}

fn offsets_of(bytes: &[u8], needle: &[u8]) -> Vec<u64> {
    bytes
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(index, _)| u64::try_from(index).expect("fixture offset fits u64"))
        .collect()
}

struct PanicSource(SourceSnapshot);

impl ByteSource for PanicSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("page property lookup must not poll the byte source")
    }
}

struct AlwaysCancelled;

impl DocumentCancellation for AlwaysCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

struct CancelAtProbe {
    probes: AtomicUsize,
    cancel_at: usize,
}

impl CancelAtProbe {
    const fn new(cancel_at: usize) -> Self {
        Self {
            probes: AtomicUsize::new(0),
            cancel_at,
        }
    }
}

impl DocumentCancellation for CancelAtProbe {
    fn is_cancelled(&self) -> bool {
        self.probes.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_at
    }
}

struct MutableSnapshotSource {
    original: SourceSnapshot,
    changed: SourceSnapshot,
    use_changed: AtomicBool,
}

impl MutableSnapshotSource {
    fn new(original: SourceSnapshot, changed: SourceSnapshot) -> Self {
        Self {
            original,
            changed,
            use_changed: AtomicBool::new(false),
        }
    }
}

impl ByteSource for MutableSnapshotSource {
    fn snapshot(&self) -> SourceSnapshot {
        if self.use_changed.load(Ordering::SeqCst) {
            self.changed
        } else {
            self.original
        }
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("page property lookup must not poll the byte source")
    }
}

struct FlipSourceAndCancelAtProbe<'source> {
    source: &'source MutableSnapshotSource,
    probes: AtomicUsize,
    flip_at: usize,
}

impl DocumentCancellation for FlipSourceAndCancelAtProbe<'_> {
    fn is_cancelled(&self) -> bool {
        let probe = self.probes.fetch_add(1, Ordering::SeqCst) + 1;
        if probe >= self.flip_at {
            self.source.use_changed.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }
}

#[test]
fn direct_property_lookup_retains_exact_provenance_without_polling_or_name_copy() {
    let fixture = resource_fixture(
        b"<< /Font << /F1 6 0 R >> /Properties << /Other 8 0 R /Secret 7 0 R >> >>",
        Vec::new(),
        9,
        0xe1,
    );
    let prepared = prepare(&fixture, 10_301);
    let page = materialize(&prepared, 10_321);
    let mut resolver = page
        .resources()
        .property_resolver(PagePropertyLookupLimits::default());
    let proof = resolver
        .lookup_marked_content_property(
            b"Secret",
            &PanicSource(fixture.snapshot),
            &DocumentNeverCancelled,
        )
        .expect("direct property reference resolves");

    assert_eq!(proof.target(), object_ref(7));
    assert_eq!(proof.snapshot(), fixture.snapshot);
    assert_eq!(proof.revision_id(), REVISION_ID);
    assert_eq!(proof.revision_startxref(), prepared.authority.startxref());
    assert_eq!(proof.scope_defining_object(), object_ref(3));
    assert_eq!(
        proof.scope_defining_value_offset(),
        offset_of(&fixture.bytes, b"/Resources <<") + 11
    );
    assert_eq!(proof.resource_dictionary_owner(), object_ref(3));
    assert_eq!(
        proof.properties_key_offset(),
        offset_of(&fixture.bytes, b"/Properties")
    );
    assert_eq!(
        proof.properties_value_offset(),
        offset_of(&fixture.bytes, b"/Properties <<") + 12
    );
    assert_eq!(
        proof.property_key_offset(),
        offset_of(&fixture.bytes, b"/Secret")
    );
    assert_eq!(
        proof.property_value_offset(),
        offset_of(&fixture.bytes, b"/Secret 7 0 R") + 8
    );
    assert_eq!(resolver.stats().lookups(), 1);
    assert_eq!(resolver.stats().entry_visits(), 4);

    let debug = format!("{proof:?}");
    assert!(debug.contains("[NOT RETAINED]"));
    assert!(!debug.contains("Secret"));
}

#[test]
fn indirect_resource_scope_names_its_terminal_dictionary_owner() {
    let fixture = indirect_resource_fixture(b"<< /Properties << /P 7 0 R >> >>", 0xe2);
    let prepared = prepare(&fixture, 10_401);
    let page = materialize(&prepared, 10_421);
    let mut resolver = page
        .resources()
        .property_resolver(PagePropertyLookupLimits::default());
    let proof = resolver
        .lookup_marked_content_property(
            b"P",
            &PanicSource(fixture.snapshot),
            &DocumentNeverCancelled,
        )
        .expect("indirect Resources terminal dictionary resolves");

    assert_eq!(proof.target(), object_ref(7));
    assert_eq!(proof.scope_defining_object(), object_ref(3));
    assert_eq!(proof.resource_dictionary_owner(), object_ref(10));
    assert_eq!(
        proof.scope_defining_value_offset(),
        offset_of(&fixture.bytes, b"/Resources 10 0 R") + 11
    );
    assert_eq!(resolver.stats().lookups(), 1);
    assert_eq!(resolver.stats().entry_visits(), 2);
}

#[test]
fn invalid_and_unsupported_property_shapes_are_stable_and_accounted() {
    let cases: &[(&[u8], DocumentErrorCode, &str)] = &[
        (
            b"<< >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties null >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties [] >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties 12 >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties true >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties 1.5 >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties /Named >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties (text) >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties 8 0 R >>",
            DocumentErrorCode::UnsupportedIndirectPageProperties,
            "RPE-DOCUMENT-0076",
        ),
        (
            b"<< /Properties << /Other 7 0 R >> >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties << /P null >> >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties << /P [] >> >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties << /P 12 >> >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties << /P /Named >> >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties << /P true >> >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties << /P 1.5 >> >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties << /P (text) >> >>",
            DocumentErrorCode::InvalidPagePropertyResource,
            "RPE-DOCUMENT-0075",
        ),
        (
            b"<< /Properties << /P << /MCID 0 >> >> >>",
            DocumentErrorCode::UnsupportedDirectPagePropertyDictionary,
            "RPE-DOCUMENT-0077",
        ),
    ];

    for (index, &(resources, expected_code, diagnostic_id)) in cases.iter().enumerate() {
        let fixture = resource_fixture(
            resources,
            Vec::new(),
            9,
            0xf0_u8.wrapping_add(u8::try_from(index).expect("case index fits u8")),
        );
        let prepared = prepare(
            &fixture,
            10_501 + u64::try_from(index).expect("case index fits u64") * 40,
        );
        let page = materialize(
            &prepared,
            10_521 + u64::try_from(index).expect("case index fits u64") * 40,
        );
        let mut resolver = page
            .resources()
            .property_resolver(PagePropertyLookupLimits::default());
        let error = resolver
            .lookup_marked_content_property(
                b"P",
                &PanicSource(fixture.snapshot),
                &DocumentNeverCancelled,
            )
            .expect_err("unsupported or invalid property shape must fail");
        assert_eq!(error.code(), expected_code, "case {index}");
        assert_eq!(error.diagnostic_id(), diagnostic_id, "case {index}");
        assert_eq!(resolver.stats().lookups(), 1, "case {index}");
        assert!(resolver.stats().entry_visits() >= 1 || resources == b"<< >>");
    }
}

#[test]
fn duplicate_relevant_keys_fail_but_unrelated_duplicates_are_ignored() {
    let outer = resource_fixture(
        b"<< /Properties << /P 7 0 R >> /Noise true /Properties << /P 8 0 R >> >>",
        Vec::new(),
        9,
        0xc1,
    );
    let prepared = prepare(&outer, 11_101);
    let page = materialize(&prepared, 11_121);
    let mut resolver = page
        .resources()
        .property_resolver(PagePropertyLookupLimits::default());
    let error = resolver
        .lookup_marked_content_property(b"P", &PanicSource(outer.snapshot), &DocumentNeverCancelled)
        .expect_err("duplicate Properties must fail");
    assert_eq!(error.code(), DocumentErrorCode::DuplicateStructuralKey);
    assert_eq!(
        error.offset(),
        Some(offsets_of(&outer.bytes, b"/Properties")[1])
    );
    assert_eq!(resolver.stats().entry_visits(), 3);

    let inner = resource_fixture(
        b"<< /Properties << /P 7 0 R /Other 8 0 R /P 9 0 R >> >>",
        Vec::new(),
        10,
        0xc2,
    );
    let prepared = prepare(&inner, 11_201);
    let page = materialize(&prepared, 11_221);
    let mut resolver = page
        .resources()
        .property_resolver(PagePropertyLookupLimits::default());
    let error = resolver
        .lookup_marked_content_property(b"P", &PanicSource(inner.snapshot), &DocumentNeverCancelled)
        .expect_err("duplicate requested property must fail");
    assert_eq!(error.code(), DocumentErrorCode::DuplicateStructuralKey);
    assert_eq!(error.offset(), Some(offsets_of(&inner.bytes, b"/P ")[1]));
    assert_eq!(resolver.stats().entry_visits(), 4);

    let unrelated = resource_fixture(
        b"<< /Font 1 /Font 2 /Properties << /Other 8 0 R /Other 9 0 R /P 7 0 R >> >>",
        Vec::new(),
        10,
        0xc3,
    );
    let prepared = prepare(&unrelated, 11_301);
    let page = materialize(&prepared, 11_321);
    let mut resolver = page
        .resources()
        .property_resolver(PagePropertyLookupLimits::default());
    let proof = resolver
        .lookup_marked_content_property(
            b"P",
            &PanicSource(unrelated.snapshot),
            &DocumentNeverCancelled,
        )
        .expect("unrelated duplicate keys do not affect the selected structure");
    assert_eq!(proof.target(), object_ref(7));
    assert_eq!(resolver.stats().entry_visits(), 6);
}

#[test]
fn exact_and_one_less_lookup_and_entry_visit_limits_are_independent() {
    let fixture = resource_fixture(
        b"<< /Properties << /P 7 0 R /Q 8 0 R >> >>",
        Vec::new(),
        9,
        0xc4,
    );
    let prepared = prepare(&fixture, 11_401);
    let page = materialize(&prepared, 11_421);
    let source = PanicSource(fixture.snapshot);

    let mut exact = page.resources().property_resolver(property_limits(2, 6));
    assert_eq!(
        exact
            .lookup_marked_content_property(b"P", &source, &DocumentNeverCancelled)
            .expect("first exact-budget lookup succeeds")
            .target(),
        object_ref(7)
    );
    assert_eq!(
        exact
            .lookup_marked_content_property(b"Q", &source, &DocumentNeverCancelled)
            .expect("second exact-budget lookup succeeds")
            .target(),
        object_ref(8)
    );
    assert_eq!(exact.stats().lookups(), 2);
    assert_eq!(exact.stats().entry_visits(), 6);

    let mut one_less_lookup = page.resources().property_resolver(property_limits(1, 6));
    one_less_lookup
        .lookup_marked_content_property(b"P", &source, &DocumentNeverCancelled)
        .expect("first lookup consumes the exact one-lookup budget");
    let error = one_less_lookup
        .lookup_marked_content_property(b"Q", &source, &DocumentNeverCancelled)
        .expect_err("one-less lookup budget must fail before scanning");
    let limit = error
        .limit()
        .expect("lookup exhaustion retains limit evidence");
    assert_eq!(limit.kind(), DocumentLimitKind::PagePropertyLookups);
    assert_eq!(limit.limit(), 1);
    assert_eq!(limit.consumed(), 1);
    assert_eq!(limit.attempted(), 1);
    assert_eq!(one_less_lookup.stats().lookups(), 1);
    assert_eq!(one_less_lookup.stats().entry_visits(), 3);

    let mut exact_visits = page.resources().property_resolver(property_limits(1, 3));
    exact_visits
        .lookup_marked_content_property(b"P", &source, &DocumentNeverCancelled)
        .expect("three fixed visits are sufficient");
    assert_eq!(exact_visits.stats().entry_visits(), 3);

    let mut one_less_visits = page.resources().property_resolver(property_limits(1, 2));
    let error = one_less_visits
        .lookup_marked_content_property(b"P", &source, &DocumentNeverCancelled)
        .expect_err("one-less entry visit budget must fail");
    let limit = error
        .limit()
        .expect("entry exhaustion retains exact limit evidence");
    assert_eq!(limit.kind(), DocumentLimitKind::PagePropertyEntryVisits);
    assert_eq!(limit.limit(), 2);
    assert_eq!(limit.consumed(), 2);
    assert_eq!(limit.attempted(), 1);
    assert_eq!(one_less_visits.stats().lookups(), 1);
    assert_eq!(one_less_visits.stats().entry_visits(), 2);
}

#[test]
fn runtime_precedence_is_source_then_cancellation_then_resource_then_semantic() {
    let fixture = resource_fixture(b"<< /Noise true /Other false >>", Vec::new(), 4, 0xc5);
    let prepared = prepare(&fixture, 11_501);
    let page = materialize(&prepared, 11_521);
    let changed = snapshot(
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
        0x3a,
    );

    let mut initial_tie = page
        .resources()
        .property_resolver(PagePropertyLookupLimits::default());
    let error = initial_tie
        .lookup_marked_content_property(b"P", &PanicSource(changed), &AlwaysCancelled)
        .expect_err("source mismatch outranks initial cancellation");
    assert_eq!(error.code(), DocumentErrorCode::SourceSnapshotMismatch);
    assert_eq!(initial_tie.stats().lookups(), 0);

    let mut initial_cancel = page
        .resources()
        .property_resolver(PagePropertyLookupLimits::default());
    let error = initial_cancel
        .lookup_marked_content_property(b"P", &PanicSource(fixture.snapshot), &AlwaysCancelled)
        .expect_err("matching source reports initial cancellation");
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
    assert_eq!(initial_cancel.stats().lookups(), 0);

    let mut semantic = page
        .resources()
        .property_resolver(PagePropertyLookupLimits::default());
    let error = semantic
        .lookup_marked_content_property(
            b"P",
            &PanicSource(fixture.snapshot),
            &CancelAtProbe::new(2),
        )
        .expect_err("cancellation observed after scanning outranks missing Properties");
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
    assert_eq!(semantic.stats().lookups(), 1);
    assert_eq!(semantic.stats().entry_visits(), 2);

    let mut resource = page.resources().property_resolver(property_limits(1, 1));
    let error = resource
        .lookup_marked_content_property(
            b"P",
            &PanicSource(fixture.snapshot),
            &CancelAtProbe::new(2),
        )
        .expect_err("cancellation observed while reporting a limit outranks the limit");
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
    assert_eq!(resource.stats().entry_visits(), 1);

    let source = MutableSnapshotSource::new(fixture.snapshot, changed);
    let cancellation = FlipSourceAndCancelAtProbe {
        source: &source,
        probes: AtomicUsize::new(0),
        flip_at: 2,
    };
    let mut source_tie = page.resources().property_resolver(property_limits(1, 1));
    let error = source_tie
        .lookup_marked_content_property(b"P", &source, &cancellation)
        .expect_err("source mutation during cancellation probe outranks cancellation and limit");
    assert_eq!(error.code(), DocumentErrorCode::SourceSnapshotMismatch);
    assert_eq!(source_tie.stats().entry_visits(), 1);
}

#[test]
fn resource_exhaustion_precedes_a_later_duplicate_semantic_failure() {
    let fixture = resource_fixture(
        b"<< /Properties << /P 7 0 R /P 8 0 R >> >>",
        Vec::new(),
        9,
        0xc6,
    );
    let prepared = prepare(&fixture, 11_601);
    let page = materialize(&prepared, 11_621);
    let mut resolver = page.resources().property_resolver(property_limits(1, 2));
    let error = resolver
        .lookup_marked_content_property(
            b"P",
            &PanicSource(fixture.snapshot),
            &DocumentNeverCancelled,
        )
        .expect_err("entry budget rejects before duplicate policy can complete");
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(
        error
            .limit()
            .expect("resource failure retains limit")
            .kind(),
        DocumentLimitKind::PagePropertyEntryVisits
    );
    assert_eq!(resolver.stats().entry_visits(), 2);
}

#[test]
fn long_scans_probe_cancellation_at_the_fixed_256_visit_granularity() {
    let mut resources = b"<<".to_vec();
    for index in 0..257 {
        resources.extend_from_slice(format!(" /N{index} null").as_bytes());
    }
    resources.extend_from_slice(b" /Properties << /P 7 0 R >> >>");
    let fixture = resource_fixture(&resources, Vec::new(), 9, 0xc7);
    let prepared = prepare(&fixture, 11_701);
    let page = materialize(&prepared, 11_721);
    let cancellation = CancelAtProbe::new(2);
    let mut resolver = page
        .resources()
        .property_resolver(PagePropertyLookupLimits::default());
    let error = resolver
        .lookup_marked_content_property(b"P", &PanicSource(fixture.snapshot), &cancellation)
        .expect_err("second cancellation probe stops the long scan");
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
    assert_eq!(resolver.stats().lookups(), 1);
    assert_eq!(resolver.stats().entry_visits(), 256);
}
