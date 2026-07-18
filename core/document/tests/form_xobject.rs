use std::sync::Arc;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AcquiredFormXObject, AttestRevisionJob, CandidateRevisionIndex, DocumentLimits,
    FormXObjectJobContext, FormXObjectPoll, FormXObjectUnsupportedKind, MaterializedPage,
    NeverCancelled as DocumentNeverCancelled, PageIndexBuildPoll, PageIndexLimits, PageLookupPoll,
    PageMaterializationJobContext, PageMaterializationLimits, PageMaterializationPoll,
    PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits, PageXObjectLookupLimits,
    PageXObjectLookupOutcome, PageXObjectReference, RevisionAttestationJobContext,
    RevisionAttestationLimits, RevisionAttestationPoll, RevisionId, SharedAttestedRevisionIndex,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(97);
const CATALOG: &[u8] = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
const ONE_PAGE_ROOT: &[u8] = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

struct Prepared {
    authority: SharedAttestedRevisionIndex,
    store: RangeStore,
    page: MaterializedPage,
}

fn snapshot(len: u64, salt: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([salt; 32]),
            SourceRevision::new(u64::from(salt) + 1),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [salt ^ 0x59; 32]),
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

fn stream_body(number: u32, dictionary: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut body = format!("{number} 0 obj\n<< ").into_bytes();
    body.extend_from_slice(dictionary);
    body.extend_from_slice(format!(" /Length {} >>\nstream\n", payload.len()).as_bytes());
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\nendstream\nendobj\n");
    body
}

fn zlib_stored(input: &[u8]) -> Vec<u8> {
    let mut output = vec![0x78, 0x01];
    let mut position = 0;
    while position < input.len() {
        let remaining = input.len() - position;
        let length = remaining.min(usize::from(u16::MAX));
        let final_block = position + length == input.len();
        output.push(u8::from(final_block));
        let length = u16::try_from(length).unwrap();
        output.extend_from_slice(&length.to_le_bytes());
        output.extend_from_slice(&(!length).to_le_bytes());
        output.extend_from_slice(&input[position..position + usize::from(length)]);
        position += usize::from(length);
    }
    let mut s1 = 1_u32;
    let mut s2 = 0_u32;
    for byte in input {
        s1 = (s1 + u32::from(*byte)) % 65_521;
        s2 = (s2 + s1) % 65_521;
    }
    output.extend_from_slice(&((s2 << 16) | s1).to_be_bytes());
    output
}

fn form_fixture(
    dictionary: &[u8],
    payload: &[u8],
    mut extras: Vec<(u32, Vec<u8>)>,
    size: u32,
    salt: u8,
) -> Fixture {
    let page = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Resources << /XObject << /Fm0 4 0 R >> >> >>\nendobj\n"
        .to_vec();
    let mut bodies = vec![
        (1, CATALOG.to_vec()),
        (2, ONE_PAGE_ROOT.to_vec()),
        (3, page),
        (4, stream_body(4, dictionary, payload)),
    ];
    bodies.append(&mut extras);
    fixture(bodies, size, salt)
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
            JobId::new(13_101),
            ResumeCheckpoint::new(13_102),
            ResumeCheckpoint::new(13_103),
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

fn ready_index(fixture: &Fixture) -> pdf_rs_document::AttestedRevisionIndex {
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
            JobId::new(13_201),
            ResumeCheckpoint::new(13_202),
            ResumeCheckpoint::new(13_203),
            ResumeCheckpoint::new(13_204),
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

fn form_context(seed: u64) -> FormXObjectJobContext {
    FormXObjectJobContext::new(
        JobId::new(seed),
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        ResumeCheckpoint::new(seed + 3),
        ResumeCheckpoint::new(seed + 4),
        ResumeCheckpoint::new(seed + 5),
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
    .expect("test Page-tree limits validate")
}

fn prepare(fixture: &Fixture, seed: u64) -> Prepared {
    let authority = ready_index(fixture);
    let store = supplied_store(fixture);
    let mut build = authority
        .build_page_index(
            tree_context(seed),
            tree_limits(),
            PageIndexLimits::new(4, 16 << 10).expect("test Page-index limits validate"),
        )
        .expect("valid Page-index build job");
    let cold = match build.poll(&store, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Ready(index) => index,
        PageIndexBuildPoll::Pending { .. } => panic!("complete source must not suspend"),
        PageIndexBuildPoll::Failed(error) => panic!("valid Page index must build: {error}"),
    };
    let mut lookup = authority
        .lookup_page(&cold, 0, tree_context(seed + 10), tree_limits())
        .expect("valid Page lookup job");
    let lookup = match lookup.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Ready(lookup) => lookup,
        PageLookupPoll::Pending { .. } => panic!("complete source must not suspend"),
        PageLookupPoll::Failed(error) => panic!("valid Page lookup must succeed: {error}"),
    };
    let (index, handle) = lookup.into_parts();
    let mut materialize = authority
        .materialize_page(
            &index,
            handle,
            materialization_context(seed + 20),
            PageMaterializationLimits::default(),
        )
        .expect("valid materialization job");
    let page = match materialize.poll(&store, &DocumentNeverCancelled) {
        PageMaterializationPoll::Ready(page) => page,
        PageMaterializationPoll::Pending { .. } => {
            panic!("complete materialization source must not suspend")
        }
        PageMaterializationPoll::Failed(error) => {
            panic!("valid Page materialization must succeed: {error}")
        }
    };
    Prepared {
        authority: authority.into_shared(),
        store,
        page,
    }
}

fn lookup_xobject(prepared: &Prepared, name: &[u8]) -> PageXObjectReference {
    let mut resolver = prepared
        .page
        .resources()
        .xobject_resolver(PageXObjectLookupLimits::default());
    match resolver
        .lookup_image_xobject(
            name,
            &PanicSource(prepared.store.snapshot()),
            &DocumentNeverCancelled,
        )
        .expect("valid Page XObject lookup")
    {
        PageXObjectLookupOutcome::Ready(proof) => proof,
        PageXObjectLookupOutcome::Unsupported(unsupported) => {
            panic!("registered indirect XObject expected: {unsupported:?}")
        }
    }
}

fn acquire(prepared: &Prepared, seed: u64) -> FormXObjectPoll {
    let proof = lookup_xobject(prepared, b"Fm0");
    let mut job = prepared
        .authority
        .acquire_form_xobject(proof, form_context(seed))
        .expect("valid Form XObject job");
    job.poll(&prepared.store, &DocumentNeverCancelled)
}

fn acquire_ready(prepared: &Prepared, seed: u64) -> Arc<AcquiredFormXObject> {
    match acquire(prepared, seed) {
        FormXObjectPoll::Ready(form) => form,
        FormXObjectPoll::Pending { .. } => panic!("complete Form source must not suspend"),
        FormXObjectPoll::Unsupported(unsupported) => {
            panic!("registered Form must be supported: {unsupported:?}")
        }
        FormXObjectPoll::Failed(error) => panic!("registered Form must acquire: {error}"),
    }
}

struct PanicSource(SourceSnapshot);

impl ByteSource for PanicSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("no-I/O lookup must not poll")
    }
}

#[test]
fn identity_form_retains_geometry_payload_and_its_own_resource_scope() {
    let payload = b"q 1 0 0 1 8 9 cm /Nested Do Q";
    let nested = stream_body(
        5,
        b"/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << >>",
        b"",
    );
    let fixture = form_fixture(
        b"/Type /XObject /Subtype /Form /BBox [-1.25 0 100.5 200] \
          /Matrix [1 0 0 1 3.5 -4] \
          /Resources << /XObject << /Nested 5 0 R >> >> \
          /Group << /Type /Group /S /Transparency /CS /DeviceRGB >>",
        payload,
        vec![(5, nested)],
        6,
        71,
    );
    let prepared = prepare(&fixture, 13_301);
    let form = acquire_ready(&prepared, 13_401);

    assert_eq!(form.reference(), ObjectRef::new(4, 0).unwrap());
    assert_eq!(form.content_bytes(), payload);
    assert_eq!(
        form.bbox()
            .coordinates()
            .map(pdf_rs_document::PageCoordinate::scaled),
        [-1_250_000_000, 0, 100_500_000_000, 200_000_000_000]
    );
    assert_eq!(
        form.matrix().map(pdf_rs_document::PageCoordinate::scaled),
        [
            1_000_000_000,
            0,
            0,
            1_000_000_000,
            3_500_000_000,
            -4_000_000_000
        ]
    );
    assert!(form.simple_transparency_group());
    assert_eq!(form.resources().defining_object(), form.reference());
    assert_eq!(
        form.resources().ancestor_lookup_chain(),
        &[form.reference()]
    );
    assert_eq!(form.stats().metadata_entries(), 7);
    assert_eq!(
        form.stats().encoded_bytes(),
        u64::try_from(payload.len()).unwrap()
    );

    let mut nested_resolver = form
        .resources()
        .xobject_resolver(PageXObjectLookupLimits::default());
    let nested_proof = match nested_resolver
        .lookup_image_xobject(
            b"Nested",
            &PanicSource(prepared.store.snapshot()),
            &DocumentNeverCancelled,
        )
        .expect("Form-owned resource lookup succeeds")
    {
        PageXObjectLookupOutcome::Ready(proof) => proof,
        PageXObjectLookupOutcome::Unsupported(unsupported) => {
            panic!("indirect nested Form reference expected: {unsupported:?}")
        }
    };
    assert_eq!(nested_proof.target(), ObjectRef::new(5, 0).unwrap());
    assert_eq!(nested_proof.scope_defining_object(), form.reference());
}

#[test]
fn flate_form_with_indirect_resources_retains_both_proofs_and_decoded_content() {
    let payload = b"q 1 0 0 1 8 9 cm Q";
    let encoded = zlib_stored(payload);
    let fixture = form_fixture(
        b"/Type /XObject /Subtype /Form /BBox [0 10 10 0] /Resources 5 0 R \
          /Filter /FlateDecode",
        &encoded,
        vec![(5, b"5 0 obj\n<< >>\nendobj\n".to_vec())],
        6,
        72,
    );
    let prepared = prepare(&fixture, 13_501);
    let form = acquire_ready(&prepared, 13_601);

    assert_eq!(form.content_bytes(), payload);
    assert_eq!(
        form.bbox()
            .coordinates()
            .map(pdf_rs_document::PageCoordinate::scaled),
        [0, 0, 10_000_000_000, 10_000_000_000]
    );
    assert_eq!(
        form.resources().resource_object(),
        ObjectRef::new(5, 0).ok()
    );
    assert_eq!(
        form.form_object().map(|object| object.reference()),
        ObjectRef::new(4, 0).ok()
    );
    assert_eq!(form.stats().encoded_bytes(), encoded.len() as u64);
    assert_eq!(form.stats().decoded_bytes(), payload.len() as u64);
    assert!(form.stats().decode_fuel() > 0);
}

#[test]
fn unknown_form_filter_is_a_typed_unsupported_capability() {
    let fixture = form_fixture(
        b"/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << >> \
          /Filter /LZWDecode",
        b"encoded",
        vec![],
        5,
        75,
    );
    let prepared = prepare(&fixture, 14_101);
    let unsupported = match acquire(&prepared, 14_201) {
        FormXObjectPoll::Unsupported(unsupported) => unsupported,
        other => panic!("unknown Form filter must be typed unsupported, got {other:?}"),
    };
    assert_eq!(
        unsupported.kind(),
        FormXObjectUnsupportedKind::UnsupportedFilter
    );
}

#[test]
fn missing_direct_resources_is_a_typed_unsupported_capability() {
    let fixture = form_fixture(
        b"/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        b"q Q",
        vec![],
        5,
        73,
    );
    let prepared = prepare(&fixture, 13_701);
    let unsupported = match acquire(&prepared, 13_801) {
        FormXObjectPoll::Unsupported(unsupported) => unsupported,
        other => panic!("resource-less Form must be typed unsupported, got {other:?}"),
    };
    assert_eq!(
        unsupported.kind(),
        FormXObjectUnsupportedKind::UnsupportedResources
    );
}

#[test]
fn unrepresentable_bbox_coordinate_fails_before_payload_publication() {
    let fixture = form_fixture(
        b"/Type /XObject /Subtype /Form /BBox [0 0 10.0000000001 10] /Resources << >>",
        b"q Q",
        vec![],
        5,
        74,
    );
    let prepared = prepare(&fixture, 13_901);
    let error = match acquire(&prepared, 14_001) {
        FormXObjectPoll::Failed(error) => error,
        other => panic!("noncanonical geometry must fail, got {other:?}"),
    };
    assert_eq!(
        error.code(),
        pdf_rs_document::DocumentErrorCode::InvalidFormXObject
    );
}
