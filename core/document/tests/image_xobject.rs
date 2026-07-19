use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AcquiredImageXObject, AttestRevisionJob, CandidateRevisionIndex, DocumentCancellation,
    DocumentError, DocumentErrorCode, DocumentLimitKind, DocumentLimits, ImageXObjectColorSpace,
    ImageXObjectJobContext, ImageXObjectLimitConfig, ImageXObjectLimits, ImageXObjectPhase,
    ImageXObjectPoll, ImageXObjectUnsupportedKind, MaterializedPage,
    NeverCancelled as DocumentNeverCancelled, PageIndexBuildPoll, PageIndexLimits, PageLookupPoll,
    PageMaterializationJobContext, PageMaterializationLimits, PageMaterializationPoll,
    PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits, PageXObjectLookupLimitConfig,
    PageXObjectLookupLimits, PageXObjectLookupOutcome, PageXObjectReference,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
    SharedAttestedRevisionIndex,
};
use pdf_rs_filters::{DecodeLimitConfig, DecodeLimits};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(89);
const CATALOG: &[u8] = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
const ONE_PAGE_ROOT: &[u8] = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";
const RGB_2X2: &[u8] = &[0, 1, 2, 10, 20, 30, 40, 50, 60, 250, 251, 252];
const FLATE_RGB_2X3_DECODED: &[u8] = b"q 1 0 0 1 0 0 cm Q";
const FLATE_RGB_2X3: &[u8] = &[
    0x78, 0x9c, 0x2b, 0x54, 0x30, 0x54, 0x30, 0x00, 0x42, 0x08, 0x99, 0x9c, 0xab, 0x10, 0x08, 0x00,
    0x21, 0x82, 0x03, 0xb5,
];

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
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [salt ^ 0x53; 32]),
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

fn image_fixture(dictionary: &[u8], payload: &[u8], salt: u8) -> Fixture {
    resource_fixture(
        b"<< /XObject << /Im0 4 0 R >> >>",
        vec![(4, stream_body(4, dictionary, payload))],
        5,
        salt,
    )
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
        let chunk_len = remaining.min(usize::from(u16::MAX));
        let final_block = chunk_len == remaining;
        output.push(u8::from(final_block));
        let chunk_len = u16::try_from(chunk_len).unwrap();
        output.extend_from_slice(&chunk_len.to_le_bytes());
        output.extend_from_slice(&(!chunk_len).to_le_bytes());
        let end = position + usize::from(chunk_len);
        output.extend_from_slice(&input[position..end]);
        position = end;
    }
    let mut first = 1_u32;
    let mut second = 0_u32;
    for byte in input {
        first = (first + u32::from(*byte)) % 65_521;
        second = (second + first) % 65_521;
    }
    output.extend_from_slice(&((second << 16) | first).to_be_bytes());
    output
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
            JobId::new(12_101),
            ResumeCheckpoint::new(12_102),
            ResumeCheckpoint::new(12_103),
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
            JobId::new(12_201),
            ResumeCheckpoint::new(12_202),
            ResumeCheckpoint::new(12_203),
            ResumeCheckpoint::new(12_204),
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

fn image_context(seed: u64) -> ImageXObjectJobContext {
    ImageXObjectJobContext::new(
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

fn lookup_image(prepared: &Prepared) -> PageXObjectReference {
    let mut resolver = prepared
        .page
        .resources()
        .xobject_resolver(PageXObjectLookupLimits::default());
    match resolver
        .lookup_image_xobject(
            b"Im0",
            &PanicSource(prepared.store.snapshot()),
            &DocumentNeverCancelled,
        )
        .expect("valid Page XObject lookup")
    {
        PageXObjectLookupOutcome::Ready(proof) => proof,
        PageXObjectLookupOutcome::Unsupported(unsupported) => {
            panic!("registered indirect image reference expected: {unsupported:?}")
        }
    }
}

fn acquire_ready(
    prepared: &Prepared,
    limits: ImageXObjectLimits,
    seed: u64,
) -> Arc<AcquiredImageXObject> {
    let proof = lookup_image(prepared);
    let mut job = prepared
        .authority
        .acquire_image_xobject(proof, image_context(seed), limits)
        .expect("valid Image XObject job");
    match job.poll(&prepared.store, &DocumentNeverCancelled) {
        ImageXObjectPoll::Ready(image) => image,
        ImageXObjectPoll::Pending { .. } => panic!("complete image source must not suspend"),
        ImageXObjectPoll::Unsupported(unsupported) => {
            panic!("registered image must be supported: {unsupported:?}")
        }
        ImageXObjectPoll::Failed(error) => panic!("registered image must acquire: {error}"),
    }
}

fn acquire_failure(prepared: &Prepared, limits: ImageXObjectLimits, seed: u64) -> DocumentError {
    let proof = lookup_image(prepared);
    match prepared
        .authority
        .acquire_image_xobject(proof, image_context(seed), limits)
    {
        Ok(mut job) => match job.poll(&prepared.store, &DocumentNeverCancelled) {
            ImageXObjectPoll::Failed(error) => error,
            ImageXObjectPoll::Ready(_) => panic!("failing image must not publish"),
            ImageXObjectPoll::Pending { .. } => panic!("complete failure must not suspend"),
            ImageXObjectPoll::Unsupported(unsupported) => {
                panic!("resource or syntax failure expected, got {unsupported:?}")
            }
        },
        Err(error) => error,
    }
}

fn offset_of(bytes: &[u8], needle: &[u8]) -> u64 {
    let offset = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("fixture contains marker");
    u64::try_from(offset).expect("fixture offset fits u64")
}

struct PanicSource(SourceSnapshot);

impl ByteSource for PanicSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("no-I/O lookup or terminal replay must not poll")
    }
}

struct Cancelled;

impl DocumentCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

struct MutableSnapshotSource {
    original: SourceSnapshot,
    changed: SourceSnapshot,
    use_changed: AtomicBool,
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
        panic!("Page XObject lookup must not poll")
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

struct PayloadOnlyMissingSource<'a> {
    complete: &'a RangeStore,
    missing: &'a RangeStore,
    payload_checkpoint: ResumeCheckpoint,
    payload_polls: AtomicUsize,
}

impl ByteSource for PayloadOnlyMissingSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.complete.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        if request.checkpoint() == self.payload_checkpoint {
            self.payload_polls.fetch_add(1, Ordering::SeqCst);
            self.missing.poll(request)
        } else {
            self.complete.poll(request)
        }
    }
}

#[test]
fn direct_lookup_and_identity_acquisition_preserve_proof_and_replay_ready() {
    let fixture = image_fixture(
        b"/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        RGB_2X2,
        0xd1,
    );
    let prepared = prepare(&fixture, 12_301);
    let mut resolver = prepared
        .page
        .resources()
        .xobject_resolver(PageXObjectLookupLimits::default());
    let proof = match resolver
        .lookup_image_xobject(
            b"Im0",
            &PanicSource(fixture.snapshot),
            &DocumentNeverCancelled,
        )
        .expect("direct XObject dictionary resolves")
    {
        PageXObjectLookupOutcome::Ready(proof) => proof,
        PageXObjectLookupOutcome::Unsupported(unsupported) => {
            panic!("indirect selected object is registered: {unsupported:?}")
        }
    };
    assert_eq!(proof.target(), object_ref(4));
    assert_eq!(proof.snapshot(), fixture.snapshot);
    assert_eq!(proof.revision_id(), REVISION_ID);
    assert_eq!(
        proof.revision_startxref(),
        prepared.authority.as_attested().startxref()
    );
    assert_eq!(proof.scope_defining_object(), object_ref(3));
    assert_eq!(proof.resource_dictionary_owner(), object_ref(3));
    assert_eq!(
        proof.xobject_key_offset(),
        offset_of(&fixture.bytes, b"/XObject")
    );
    assert_eq!(proof.entry_key_offset(), offset_of(&fixture.bytes, b"/Im0"));
    assert_eq!(
        proof.entry_value_offset(),
        offset_of(&fixture.bytes, b"/Im0 4 0 R") + 5
    );
    assert_eq!(resolver.stats().lookups(), 1);
    assert_eq!(resolver.stats().entry_visits(), 3);
    assert!(resolver.stats().index_bytes() > 0);

    let mut job = prepared
        .authority
        .acquire_image_xobject(proof, image_context(12_341), ImageXObjectLimits::default())
        .expect("valid image job");
    assert_eq!(job.phase(), ImageXObjectPhase::Object);
    let ready = match job.poll(&prepared.store, &DocumentNeverCancelled) {
        ImageXObjectPoll::Ready(image) => image,
        other => panic!("identity image must be ready, got {other:?}"),
    };
    assert_eq!(job.phase(), ImageXObjectPhase::Ready);
    assert_eq!(ready.proof(), proof);
    assert_eq!(ready.reference(), object_ref(4));
    assert_eq!(ready.width(), 2);
    assert_eq!(ready.height(), 2);
    assert_eq!(ready.color_space(), ImageXObjectColorSpace::DeviceRgb);
    assert_eq!(ready.components(), 3);
    assert_eq!(ready.bits_per_component(), 8);
    assert!(!ready.interpolate());
    assert_eq!(ready.stride_bytes(), 6);
    assert_eq!(ready.decoded_bytes(), RGB_2X2);
    assert!(ready.filter_plan().is_empty());
    assert_ne!(ready.decode_context(), 0);
    assert_eq!(ready.stats().encoded_bytes(), 12);
    assert_eq!(ready.stats().decoded_bytes(), 12);
    assert_eq!(ready.stats().metadata_entries(), 7);
    assert!(ready.stats().decode_fuel() > 0);
    assert!(ready.stats().retained_bytes() >= 12);

    let replay = match job.poll(&PanicSource(fixture.snapshot), &Cancelled) {
        ImageXObjectPoll::Ready(image) => image,
        other => panic!("terminal Ready must replay without runtime work: {other:?}"),
    };
    assert!(Arc::ptr_eq(&ready, &replay));
}

#[test]
fn one_level_indirect_device_color_space_is_proof_bound_and_decoded() {
    let fixture = resource_fixture(
        b"<< /XObject << /Im0 4 0 R >> >>",
        vec![
            (
                4,
                stream_body(
                    4,
                    b"/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace 5 0 R /BitsPerComponent 8",
                    RGB_2X2,
                ),
            ),
            (5, b"5 0 obj\n/DeviceRGB\nendobj\n".to_vec()),
        ],
        6,
        0xd2,
    );
    let prepared = prepare(&fixture, 12_381);
    let color_space_offset = offset_of(&fixture.bytes, b"5 0 obj");
    let partial = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let prefix = ByteRange::new(0, color_space_offset).unwrap();
    partial
        .supply(
            RangeResponse::new(
                fixture.snapshot,
                prefix,
                fixture.bytes[..usize::try_from(color_space_offset).unwrap()].to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    let mut job = prepared
        .authority
        .acquire_image_xobject(
            lookup_image(&prepared),
            image_context(12_391),
            ImageXObjectLimits::default(),
        )
        .expect("valid indirect ColorSpace image job");
    match job.poll(&partial, &DocumentNeverCancelled) {
        ImageXObjectPoll::Pending { checkpoint, .. } => assert!(
            checkpoint == job.context().object_envelope_checkpoint()
                || checkpoint == job.context().object_boundary_checkpoint()
        ),
        other => panic!("missing indirect ColorSpace object must suspend: {other:?}"),
    }
    assert_eq!(job.phase(), ImageXObjectPhase::ColorSpace);
    let image = match job.poll(&prepared.store, &DocumentNeverCancelled) {
        ImageXObjectPoll::Ready(image) => image,
        other => panic!("supplied ColorSpace object must resume to Ready: {other:?}"),
    };

    assert_eq!(image.reference(), object_ref(4));
    assert_eq!(
        image
            .color_space_object()
            .expect("indirect ColorSpace proof remains owned")
            .reference(),
        object_ref(5)
    );
    assert_eq!(image.color_space(), ImageXObjectColorSpace::DeviceRgb);
    assert_eq!(image.decoded_bytes(), RGB_2X2);
    assert_eq!(image.stats().metadata_entries(), 15);
    assert!(image.stats().retained_bytes() >= u64::try_from(RGB_2X2.len()).unwrap());
}

#[test]
fn indexed_icc_alternate_and_flate_lookup_publish_bounded_palette_evidence() {
    let palette = [255, 0, 0, 0, 255, 0];
    let encoded_palette = zlib_stored(&palette);
    let fixture = resource_fixture(
        b"<< /XObject << /Im0 4 0 R >> >>",
        vec![
            (
                4,
                stream_body(
                    4,
                    b"/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace 5 0 R /BitsPerComponent 8 /Decode [0 255]",
                    &[0, 1],
                ),
            ),
            (5, b"5 0 obj\n[/Indexed 6 0 R 1 7 0 R]\nendobj\n".to_vec()),
            (6, b"6 0 obj\n[/ICCBased 8 0 R]\nendobj\n".to_vec()),
            (
                7,
                stream_body(7, b"/Filter /FlateDecode", &encoded_palette),
            ),
            (8, stream_body(8, b"/N 3", b"ignored-profile-payload")),
        ],
        9,
        0xd3,
    );
    let prepared = prepare(&fixture, 12_421);
    let image = acquire_ready(&prepared, ImageXObjectLimits::default(), 12_431);

    assert_eq!(image.color_space(), ImageXObjectColorSpace::IndexedRgb);
    assert_eq!(image.components(), 3);
    assert_eq!(image.source_components(), 1);
    assert_eq!(image.indexed_high_value(), Some(1));
    assert_eq!(image.indexed_lookup_bytes(), Some(palette.as_slice()));
    assert_eq!(image.decoded_bytes(), &[0, 1]);
    assert_eq!(
        image
            .color_space_object()
            .expect("Indexed definition proof")
            .reference(),
        object_ref(5)
    );
    assert_eq!(
        image
            .color_space_base_object()
            .expect("Indexed base proof")
            .reference(),
        object_ref(6)
    );
    assert_eq!(
        image
            .icc_profile_object()
            .expect("ICC alternate proof")
            .reference(),
        object_ref(8)
    );
    assert_eq!(
        image
            .indexed_lookup_object()
            .expect("lookup stream proof")
            .reference(),
        object_ref(7)
    );
    assert_eq!(
        image.stats().encoded_bytes(),
        u64::try_from(encoded_palette.len() + 2).unwrap()
    );
}

#[test]
fn flate_default_decode_and_all_direct_device_color_spaces_are_registered() {
    let flate = image_fixture(
        b"/Type /XObject /Subtype /Image /Width 2 /Height 3 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Decode [0.0 1.0 0 1 0e2 0.1e1] /Filter /FlateDecode /DecodeParms << /Predictor 1 /Colors 3 /BitsPerComponent 8 /Columns 2 >>",
        FLATE_RGB_2X3,
        0xd2,
    );
    let prepared = prepare(&flate, 12_401);
    let image = acquire_ready(&prepared, ImageXObjectLimits::default(), 12_441);
    assert_eq!(image.decoded_bytes(), FLATE_RGB_2X3_DECODED);
    assert_eq!(
        image.filter_plan().filters(),
        &[pdf_rs_filters::StreamFilter::FlateDecode]
    );
    assert_eq!(image.width(), 2);
    assert_eq!(image.height(), 3);
    assert_eq!(image.stride_bytes(), 6);

    for (salt, color, payload, expected) in [
        (
            0xd3,
            b"DeviceGray".as_slice(),
            vec![7, 9],
            ImageXObjectColorSpace::DeviceGray,
        ),
        (
            0xd4,
            b"DeviceCMYK".as_slice(),
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            ImageXObjectColorSpace::DeviceCmyk,
        ),
    ] {
        let mut dictionary =
            b"/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /".to_vec();
        dictionary.extend_from_slice(color);
        dictionary.extend_from_slice(b" /BitsPerComponent 8");
        let fixture = image_fixture(&dictionary, &payload, salt);
        let prepared = prepare(&fixture, 12_500 + u64::from(salt));
        let image = acquire_ready(
            &prepared,
            ImageXObjectLimits::default(),
            12_700 + u64::from(salt),
        );
        assert_eq!(image.color_space(), expected);
        assert_eq!(image.decoded_bytes(), payload);
    }
}

#[test]
fn one_bit_gray_png_predictor_preserves_packed_rows_and_metadata() {
    let predicted = [0, 0b1010_1010, 0, 0b0101_0101];
    let payload = zlib_stored(&predicted);
    let fixture = image_fixture(
        b"/Type /XObject /Subtype /Image /Width 8 /Height 2 /ColorSpace /DeviceGray \
          /BitsPerComponent 1 /Filter /FlateDecode /DecodeParms \
          << /Predictor 10 /Colors 1 /BitsPerComponent 1 /Columns 8 >>",
        &payload,
        0xd5,
    );
    let prepared = prepare(&fixture, 12_501);
    let image = acquire_ready(&prepared, ImageXObjectLimits::default(), 12_541);

    assert_eq!(image.bits_per_component(), 1);
    assert_eq!(image.stride_bytes(), 1);
    assert_eq!(image.decoded_bytes(), &[0b1010_1010, 0b0101_0101]);
    assert_eq!(image.stats().decoded_bytes(), 2);
    assert_eq!(
        image.filter_plan().filters(),
        &[pdf_rs_filters::StreamFilter::FlateDecode]
    );
}

#[test]
fn payload_pending_uses_only_the_payload_checkpoint_and_resumes() {
    let fixture = image_fixture(
        b"/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        RGB_2X2,
        0xd5,
    );
    let prepared = prepare(&fixture, 12_801);
    let context = image_context(12_841);
    let missing = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let source = PayloadOnlyMissingSource {
        complete: &prepared.store,
        missing: &missing,
        payload_checkpoint: context.payload_checkpoint(),
        payload_polls: AtomicUsize::new(0),
    };
    let mut job = prepared
        .authority
        .acquire_image_xobject(
            lookup_image(&prepared),
            context,
            ImageXObjectLimits::default(),
        )
        .expect("valid image job");
    match job.poll(&source, &DocumentNeverCancelled) {
        ImageXObjectPoll::Pending { checkpoint, .. } => {
            assert_eq!(checkpoint, context.payload_checkpoint());
        }
        other => panic!("missing payload must suspend: {other:?}"),
    }
    assert_eq!(job.phase(), ImageXObjectPhase::Payload);
    assert_eq!(source.payload_polls.load(Ordering::SeqCst), 1);
    match job.poll(&prepared.store, &DocumentNeverCancelled) {
        ImageXObjectPoll::Ready(image) => assert_eq!(image.decoded_bytes(), RGB_2X2),
        other => panic!("supplied payload must resume to Ready: {other:?}"),
    }
}

#[test]
fn unsupported_image_profiles_are_typed_and_never_publish() {
    let cases: Vec<(&[u8], &[u8], ImageXObjectUnsupportedKind)> = vec![
        (
            b"/Type /XObject /Subtype /Form",
            b"x",
            ImageXObjectUnsupportedKind::NonImageXObject,
        ),
        (
            b"/Type /XObject /Subtype /Image /ImageMask true",
            b"x",
            ImageXObjectUnsupportedKind::ImageMask,
        ),
        (
            b"/Type /XObject /Subtype /Image /Mask [0 1]",
            b"x",
            ImageXObjectUnsupportedKind::ExplicitMask,
        ),
        (
            b"/Type /XObject /Subtype /Image /SMask << >>",
            b"x",
            ImageXObjectUnsupportedKind::SoftMask,
        ),
        (
            b"/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /CalRGB /BitsPerComponent 8",
            b"x",
            ImageXObjectUnsupportedKind::UnsupportedColorSpace,
        ),
        (
            b"/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 3",
            b"x",
            ImageXObjectUnsupportedKind::UnsupportedBitsPerComponent,
        ),
        (
            b"/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0]",
            b"x",
            ImageXObjectUnsupportedKind::UnsupportedDecodeArray,
        ),
        (
            b"/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Interpolate true",
            b"x",
            ImageXObjectUnsupportedKind::Interpolation,
        ),
        (
            b"/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /ASCIIHexDecode",
            b"78>",
            ImageXObjectUnsupportedKind::UnsupportedFilter,
        ),
        (
            b"/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /DecodeParms << /Predictor 9 >>",
            FLATE_RGB_2X3,
            ImageXObjectUnsupportedKind::UnsupportedDecodeParameters,
        ),
        (
            b"/Type /XObject /Subtype /Image /Width 2 /Height 3 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /DecodeParms [<< /Predictor 1 >> << /Predictor 1 >>]",
            FLATE_RGB_2X3,
            ImageXObjectUnsupportedKind::UnsupportedDecodeParameters,
        ),
    ];
    for (index, (dictionary, payload, expected)) in cases.into_iter().enumerate() {
        let fixture = image_fixture(
            dictionary,
            payload,
            0xe0_u8.wrapping_add(u8::try_from(index).unwrap()),
        );
        let prepared = prepare(&fixture, 13_001 + u64::try_from(index).unwrap() * 100);
        let mut job = prepared
            .authority
            .acquire_image_xobject(
                lookup_image(&prepared),
                image_context(14_001 + u64::try_from(index).unwrap() * 10),
                ImageXObjectLimits::default(),
            )
            .expect("valid unsupported-profile job");
        let unsupported = match job.poll(&prepared.store, &DocumentNeverCancelled) {
            ImageXObjectPoll::Unsupported(unsupported) => unsupported,
            other => panic!("profile must be typed Unsupported: {other:?}"),
        };
        assert_eq!(unsupported.kind(), expected);
        assert_eq!(unsupported.reference(), object_ref(4));
        assert!(
            unsupported
                .diagnostic_id()
                .starts_with("RPE-DOCUMENT-XOBJECT-")
        );
        assert_eq!(job.phase(), ImageXObjectPhase::Unsupported);
    }

    let alias = resource_fixture(
        b"<< /XObject << /Im0 4 0 R >> >>",
        vec![(4, b"4 0 obj\n5 0 R\nendobj\n".to_vec())],
        5,
        0xf1,
    );
    let prepared = prepare(&alias, 15_001);
    let mut job = prepared
        .authority
        .acquire_image_xobject(
            lookup_image(&prepared),
            image_context(15_041),
            ImageXObjectLimits::default(),
        )
        .expect("valid alias image job");
    match job.poll(&prepared.store, &DocumentNeverCancelled) {
        ImageXObjectPoll::Unsupported(unsupported) => {
            assert_eq!(
                unsupported.kind(),
                ImageXObjectUnsupportedKind::XObjectAlias
            );
        }
        other => panic!("whole-object alias must be typed Unsupported: {other:?}"),
    }
}

#[test]
fn malformed_images_and_decoded_geometry_fail_without_publication() {
    let mut trailing_flate = FLATE_RGB_2X3.to_vec();
    trailing_flate.push(b'x');
    let cases: Vec<(&[u8], &[u8], DocumentErrorCode)> = vec![
        (
            b"/Type /XObject /Subtype /Image /Width 1 /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
            b"x",
            DocumentErrorCode::DuplicateStructuralKey,
        ),
        (
            b"/Type /XObject /Subtype /Image /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
            b"x",
            DocumentErrorCode::InvalidImageXObject,
        ),
        (
            b"/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
            b"xyz",
            DocumentErrorCode::InvalidImageXObject,
        ),
        (
            b"/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode",
            b"not-zlib",
            DocumentErrorCode::ImageXObjectDecodeFailure,
        ),
        (
            b"/Type /XObject /Subtype /Image /Width 2 /Height 3 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
            trailing_flate.as_slice(),
            DocumentErrorCode::ImageXObjectDecodeFailure,
        ),
    ];
    for (index, (dictionary, payload, expected)) in cases.into_iter().enumerate() {
        let fixture = image_fixture(
            dictionary,
            payload,
            0xa0_u8.wrapping_add(u8::try_from(index).unwrap()),
        );
        let prepared = prepare(&fixture, 16_001 + u64::try_from(index).unwrap() * 100);
        let error = acquire_failure(
            &prepared,
            ImageXObjectLimits::default(),
            16_041 + u64::try_from(index).unwrap() * 100,
        );
        assert_eq!(error.code(), expected);
    }
}

#[test]
fn lookup_and_acquisition_limits_fail_at_the_registered_boundary() {
    let fixture = image_fixture(
        b"/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        RGB_2X2,
        0xa5,
    );
    let prepared = prepare(&fixture, 17_001);
    let baseline = acquire_ready(&prepared, ImageXObjectLimits::default(), 17_041);
    let stats = baseline.stats();
    let mut resolver = prepared.page.resources().xobject_resolver(
        PageXObjectLookupLimits::validate(PageXObjectLookupLimitConfig {
            max_lookups: 1,
            max_entry_visits: 1,
            ..PageXObjectLookupLimitConfig::default()
        })
        .unwrap(),
    );
    let error = resolver
        .lookup_image_xobject(
            b"Im0",
            &PanicSource(fixture.snapshot),
            &DocumentNeverCancelled,
        )
        .expect_err("one outer visit leaves no inner-entry budget");
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().expect("limit evidence").kind(),
        DocumentLimitKind::PageXObjectEntryVisits
    );

    let exact_config = ImageXObjectLimitConfig {
        max_width: 2,
        max_height: 2,
        max_pixels: 4,
        max_stride_bytes: 6,
        max_metadata_entries: stats.metadata_entries(),
        max_object_read_bytes: stats.object_read_bytes(),
        max_object_parse_bytes: stats.object_parse_bytes(),
        max_encoded_bytes: stats.encoded_bytes(),
        max_decoded_bytes: stats.decoded_bytes(),
        max_decode_fuel: stats.decode_fuel(),
        max_retained_bytes: stats.peak_retained_bytes(),
        decode_limits: DecodeLimits::default(),
    };
    let exact_limits =
        ImageXObjectLimits::validate(exact_config).expect("measured exact limits validate");
    let exact = acquire_ready(&prepared, exact_limits, 17_081);
    assert_eq!(exact.decoded_bytes(), baseline.decoded_bytes());
    assert_eq!(exact.stats(), stats);

    for (kind, config) in [
        (
            DocumentLimitKind::ImageXObjectWidth,
            ImageXObjectLimitConfig {
                max_width: 1,
                ..exact_config
            },
        ),
        (
            DocumentLimitKind::ImageXObjectHeight,
            ImageXObjectLimitConfig {
                max_height: 1,
                ..exact_config
            },
        ),
        (
            DocumentLimitKind::ImageXObjectPixels,
            ImageXObjectLimitConfig {
                max_pixels: 3,
                ..exact_config
            },
        ),
        (
            DocumentLimitKind::ImageXObjectStrideBytes,
            ImageXObjectLimitConfig {
                max_stride_bytes: 5,
                ..exact_config
            },
        ),
        (
            DocumentLimitKind::ImageXObjectMetadataEntries,
            ImageXObjectLimitConfig {
                max_metadata_entries: stats.metadata_entries() - 1,
                ..exact_config
            },
        ),
        (
            DocumentLimitKind::ImageXObjectObjectReadBytes,
            ImageXObjectLimitConfig {
                max_object_read_bytes: stats.object_read_bytes() - 1,
                ..exact_config
            },
        ),
        (
            DocumentLimitKind::ImageXObjectObjectParseBytes,
            ImageXObjectLimitConfig {
                max_object_parse_bytes: stats.object_parse_bytes() - 1,
                ..exact_config
            },
        ),
        (
            DocumentLimitKind::ImageXObjectEncodedBytes,
            ImageXObjectLimitConfig {
                max_encoded_bytes: stats.encoded_bytes() - 1,
                ..exact_config
            },
        ),
        (
            DocumentLimitKind::ImageXObjectDecodedBytes,
            ImageXObjectLimitConfig {
                max_decoded_bytes: stats.decoded_bytes() - 1,
                ..exact_config
            },
        ),
        (
            DocumentLimitKind::ImageXObjectDecodeFuel,
            ImageXObjectLimitConfig {
                max_decode_fuel: stats.decode_fuel() - 1,
                ..exact_config
            },
        ),
        (
            DocumentLimitKind::ImageXObjectRetainedBytes,
            ImageXObjectLimitConfig {
                max_retained_bytes: stats.peak_retained_bytes() - 1,
                ..exact_config
            },
        ),
    ] {
        let limits = ImageXObjectLimits::validate(config).expect("positive test limit validates");
        let error = acquire_failure(&prepared, limits, 17_101 + kind as u64);
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        assert_eq!(error.limit().expect("limit evidence").kind(), kind);
    }
}

#[test]
fn large_xobject_dictionary_is_indexed_once_for_many_distinct_lookups() {
    const XOBJECTS: usize = 747;
    let mut resources = b"<< /XObject << ".to_vec();
    for index in (0..XOBJECTS).rev() {
        resources.extend_from_slice(format!("/Fm{index} 4 0 R ").as_bytes());
    }
    resources.extend_from_slice(b">> >>");
    let fixture = resource_fixture(
        &resources,
        vec![(
            4,
            stream_body(4, b"/Type /XObject /Subtype /Form /BBox [0 0 1 1]", b""),
        )],
        5,
        0xb7,
    );
    let prepared = prepare(&fixture, 17_101);
    let mut resolver = prepared
        .page
        .resources()
        .xobject_resolver(PageXObjectLookupLimits::default());
    for index in 0..XOBJECTS {
        let name = format!("Fm{index}");
        match resolver
            .lookup_image_xobject(
                name.as_bytes(),
                &PanicSource(fixture.snapshot),
                &DocumentNeverCancelled,
            )
            .expect("indexed XObject lookup")
        {
            PageXObjectLookupOutcome::Ready(proof) => assert_eq!(proof.target(), object_ref(4)),
            PageXObjectLookupOutcome::Unsupported(value) => {
                panic!("indirect XObject reference must be ready: {value:?}")
            }
        }
    }
    assert_eq!(resolver.stats().lookups(), XOBJECTS as u64);
    assert!(resolver.stats().entry_visits() < 10_000);
    assert!(resolver.stats().index_bytes() > 0);

    let mut low = prepared.page.resources().xobject_resolver(
        PageXObjectLookupLimits::validate(PageXObjectLookupLimitConfig {
            max_index_bytes: 1,
            ..PageXObjectLookupLimitConfig::default()
        })
        .unwrap(),
    );
    let error = low
        .lookup_image_xobject(
            b"Fm0",
            &PanicSource(fixture.snapshot),
            &DocumentNeverCancelled,
        )
        .expect_err("one byte cannot retain the lookup index");
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().expect("index-byte limit evidence").kind(),
        DocumentLimitKind::PageXObjectIndexBytes
    );
}

#[test]
fn same_size_gray_soft_mask_is_proof_bound_decoded_and_retained() {
    let main = zlib_stored(&[10, 20, 30, 40, 50, 60]);
    let mask = zlib_stored(&[0, 255]);
    let fixture = resource_fixture(
        b"<< /XObject << /Im0 4 0 R >> >>",
        vec![
            (
                4,
                stream_body(
                    4,
                    b"/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 5 0 R /Filter [/FlateDecode] /DecodeParms [<< /Colors 3 /BitsPerComponent 4 /Columns 2 >>]",
                    &main,
                ),
            ),
            (
                5,
                stream_body(
                    5,
                    b"/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [0 1] /Filter /FlateDecode /DecodeParms << /Predictor 1 /Colors 1 /BitsPerComponent 8 /Columns 2 >>",
                    &mask,
                ),
            ),
        ],
        6,
        0xf2,
    );
    let prepared = prepare(&fixture, 15_101);
    let mut job = prepared
        .authority
        .acquire_image_xobject(
            lookup_image(&prepared),
            image_context(15_141),
            ImageXObjectLimits::default(),
        )
        .expect("valid soft-mask image job");
    let image = match job.poll(&prepared.store, &DocumentNeverCancelled) {
        ImageXObjectPoll::Ready(image) => image,
        other => panic!("soft-mask image must be ready: {other:?}"),
    };
    assert_eq!(job.phase(), ImageXObjectPhase::Ready);
    assert_eq!(image.decoded_bytes(), &[10, 20, 30, 40, 50, 60]);
    assert_eq!(image.soft_mask_decoded_bytes(), Some([0, 255].as_slice()));
    assert_eq!(image.soft_mask_stride_bytes(), Some(2));
    assert_eq!(
        image
            .soft_mask_object()
            .expect("proof-bound soft-mask object")
            .reference(),
        object_ref(5)
    );
    assert_eq!(image.stats().decoded_bytes(), 8);
    assert!(image.stats().retained_bytes() >= 8);
}

#[test]
fn matte_soft_mask_is_typed_unsupported_instead_of_rendered_with_wrong_colors() {
    let fixture = resource_fixture(
        b"<< /XObject << /Im0 4 0 R >> >>",
        vec![
            (
                4,
                stream_body(
                    4,
                    b"/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 5 0 R",
                    &[10, 20, 30, 40, 50, 60],
                ),
            ),
            (
                5,
                stream_body(
                    5,
                    b"/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Matte [0 0 0]",
                    &[0, 255],
                ),
            ),
        ],
        6,
        0xf3,
    );
    let prepared = prepare(&fixture, 15_201);
    let mut job = prepared
        .authority
        .acquire_image_xobject(
            lookup_image(&prepared),
            image_context(15_241),
            ImageXObjectLimits::default(),
        )
        .expect("valid soft-mask image job");
    match job.poll(&prepared.store, &DocumentNeverCancelled) {
        ImageXObjectPoll::Unsupported(unsupported) => {
            assert_eq!(unsupported.kind(), ImageXObjectUnsupportedKind::SoftMask);
            assert_eq!(unsupported.reference(), object_ref(5));
            assert_eq!(unsupported.offset(), offset_of(&fixture.bytes, b"[0 0 0]"));
        }
        other => panic!("Matte soft mask must not publish wrong pixels: {other:?}"),
    }
}

#[test]
fn lower_decoder_fuel_reports_effective_limit_and_retention_adds_image_prefix() {
    let fixture = image_fixture(
        b"/Type /XObject /Subtype /Image /Width 2 /Height 3 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
        FLATE_RGB_2X3,
        0xaa,
    );
    let prepared = prepare(&fixture, 17_701);
    let baseline = acquire_ready(&prepared, ImageXObjectLimits::default(), 17_741);
    let baseline_fuel = baseline.stats().decode_fuel();
    assert!(baseline_fuel > 1);

    let fuel_config = DecodeLimitConfig {
        max_fuel: baseline_fuel - 1,
        cancellation_check_interval_fuel: 1,
        ..DecodeLimitConfig::default()
    };
    let lower_fuel = DecodeLimits::validate(fuel_config).expect("lower fuel profile validates");
    let fuel_limits = ImageXObjectLimits::validate(ImageXObjectLimitConfig {
        max_decode_fuel: baseline_fuel.saturating_mul(2),
        decode_limits: lower_fuel,
        ..ImageXObjectLimitConfig::default()
    })
    .expect("image profile with lower foundational fuel validates");
    let error = acquire_failure(&prepared, fuel_limits, 17_781);
    let limit = error.limit().expect("effective fuel evidence");
    assert_eq!(limit.kind(), DocumentLimitKind::ImageXObjectDecodeFuel);
    assert_eq!(limit.limit(), baseline_fuel - 1);

    let decoded_bytes =
        u64::try_from(FLATE_RGB_2X3_DECODED.len()).expect("fixture length fits u64");
    let encoded_bytes = u64::try_from(FLATE_RGB_2X3.len()).expect("fixture length fits u64");
    let lower_retained = DecodeLimits::validate(DecodeLimitConfig {
        max_input_bytes: encoded_bytes,
        max_filters: 1,
        max_layer_output_bytes: decoded_bytes,
        max_total_output_bytes: decoded_bytes,
        max_final_output_bytes: decoded_bytes,
        max_retained_capacity_bytes: decoded_bytes,
        max_fuel: DecodeLimitConfig::default().max_fuel,
        cancellation_check_interval_fuel: 1,
    })
    .expect("minimum decoded-capacity profile validates");
    let retained_limits = ImageXObjectLimits::validate(ImageXObjectLimitConfig {
        decode_limits: lower_retained,
        ..ImageXObjectLimitConfig::default()
    })
    .expect("image profile with lower foundational retention validates");
    let retained = acquire_ready(&prepared, retained_limits, 17_821);
    assert!(retained.stats().retained_bytes() > decoded_bytes);
    assert!(retained.stats().retained_bytes() <= retained_limits.max_retained_bytes());
}

#[test]
fn xobject_entry_limit_rechecks_source_and_cancellation_precedence() {
    let fixture = image_fixture(
        b"/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        RGB_2X2,
        0xa9,
    );
    let prepared = prepare(&fixture, 17_501);
    let changed = snapshot(
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
        0x79,
    );
    let source = MutableSnapshotSource {
        original: fixture.snapshot,
        changed,
        use_changed: AtomicBool::new(false),
    };
    let cancellation = FlipSourceAndCancelAtProbe {
        source: &source,
        probes: AtomicUsize::new(0),
        flip_at: 2,
    };
    let limits = PageXObjectLookupLimits::validate(PageXObjectLookupLimitConfig {
        max_lookups: 1,
        max_entry_visits: 1,
        ..PageXObjectLookupLimitConfig::default()
    })
    .expect("one-entry lookup limit");
    let mut resolver = prepared.page.resources().xobject_resolver(limits);
    let error = resolver
        .lookup_image_xobject(b"Im0", &source, &cancellation)
        .expect_err("runtime change while reporting the inner-entry limit must win");
    assert_eq!(error.code(), DocumentErrorCode::SourceSnapshotMismatch);
    assert_eq!(resolver.stats().lookups(), 1);
    assert_eq!(resolver.stats().entry_visits(), 1);
}

#[test]
fn indirect_and_direct_xobject_resource_shapes_are_typed_at_lookup() {
    let indirect = resource_fixture(
        b"<< /XObject 10 0 R >>",
        vec![(10, b"10 0 obj\n<< /Im0 4 0 R >>\nendobj\n".to_vec())],
        11,
        0xa6,
    );
    let prepared = prepare(&indirect, 18_001);
    let mut resolver = prepared
        .page
        .resources()
        .xobject_resolver(PageXObjectLookupLimits::default());
    match resolver
        .lookup_image_xobject(
            b"Im0",
            &PanicSource(indirect.snapshot),
            &DocumentNeverCancelled,
        )
        .expect("indirect category is a typed capability outcome")
    {
        PageXObjectLookupOutcome::Unsupported(unsupported) => {
            assert_eq!(
                unsupported.kind(),
                ImageXObjectUnsupportedKind::IndirectXObjectDictionary
            );
            assert_eq!(unsupported.reference(), object_ref(10));
        }
        PageXObjectLookupOutcome::Ready(_) => panic!("indirect category is outside phase 1"),
    }

    let direct = resource_fixture(
        b"<< /XObject << /Im0 << /Type /XObject /Subtype /Image >> >> >>",
        Vec::new(),
        4,
        0xa7,
    );
    let prepared = prepare(&direct, 18_101);
    let mut resolver = prepared
        .page
        .resources()
        .xobject_resolver(PageXObjectLookupLimits::default());
    match resolver
        .lookup_image_xobject(
            b"Im0",
            &PanicSource(direct.snapshot),
            &DocumentNeverCancelled,
        )
        .expect("direct selected XObject is a typed capability outcome")
    {
        PageXObjectLookupOutcome::Unsupported(unsupported) => assert_eq!(
            unsupported.kind(),
            ImageXObjectUnsupportedKind::DirectXObject
        ),
        PageXObjectLookupOutcome::Ready(_) => panic!("direct XObject is outside phase 1"),
    }
}

#[test]
fn runtime_integrity_cancellation_and_context_checks_precede_work() {
    let fixture = image_fixture(
        b"/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        RGB_2X2,
        0xa8,
    );
    let prepared = prepare(&fixture, 19_001);
    let proof = lookup_image(&prepared);
    let context = image_context(19_041);
    let invalid_context = ImageXObjectJobContext::new(
        context.job(),
        context.object_envelope_checkpoint(),
        context.object_envelope_checkpoint(),
        context.payload_checkpoint(),
        context.priority(),
    );
    let error = prepared
        .authority
        .acquire_image_xobject(proof, invalid_context, ImageXObjectLimits::default())
        .expect_err("duplicate checkpoints are invalid");
    assert_eq!(
        error.code(),
        DocumentErrorCode::InvalidImageXObjectJobContext
    );

    let mut cancelled = prepared
        .authority
        .acquire_image_xobject(proof, context, ImageXObjectLimits::default())
        .expect("valid image job");
    match cancelled.poll(&PanicSource(fixture.snapshot), &Cancelled) {
        ImageXObjectPoll::Failed(error) => assert_eq!(error.code(), DocumentErrorCode::Cancelled),
        other => panic!("cancellation must fail before I/O: {other:?}"),
    }

    let changed = snapshot(
        u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
        0x77,
    );
    let mut changed_job = prepared
        .authority
        .acquire_image_xobject(proof, image_context(19_101), ImageXObjectLimits::default())
        .expect("valid image job");
    match changed_job.poll(&PanicSource(changed), &DocumentNeverCancelled) {
        ImageXObjectPoll::Failed(error) => {
            assert_eq!(error.code(), DocumentErrorCode::SourceSnapshotMismatch);
        }
        other => panic!("changed source must fail before I/O: {other:?}"),
    }
}
