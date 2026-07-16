use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AcquirePageContentJob, AcquiredPageContent, AttestRevisionJob, AttestedRevisionIndex,
    CandidateRevisionIndex, DocumentCancellation, DocumentError, DocumentErrorCode,
    DocumentLimitKind, DocumentLimits, MaterializedPage, NeverCancelled as DocumentNeverCancelled,
    PageContentJobContext, PageContentLimitConfig, PageContentLimits, PageContentPhase,
    PageContentPoll, PageHandle, PageIndex, PageIndexBuildPoll, PageIndexLimits, PageLookupPoll,
    PageMaterializationJobContext, PageMaterializationLimits, PageMaterializationPoll,
    PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits, RevisionAttestationJobContext,
    RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_filters::{DecodeLimitConfig, DecodeLimits, StreamFilter};
use pdf_rs_object::{IndirectObjectValue, ObjectErrorCode, ObjectLimitKind, ObjectLimits};
use pdf_rs_syntax::{ObjectRef, SyntaxLimitKind, SyntaxLimits};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(81);
const CATALOG: &[u8] = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
const ONE_PAGE_ROOT: &[u8] = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";
const IDENTITY_CONTENT: &[u8] = b"q 2 0 0 2 10 20 cm Q";
const FLATE_DECODED: &[u8] = b"q 1 0 0 1 0 0 cm Q";
const FLATE_CONTENT: &[u8] = &[
    0x78, 0x9c, 0x2b, 0x54, 0x30, 0x54, 0x30, 0x00, 0x42, 0x08, 0x99, 0x9c, 0xab, 0x10, 0x08, 0x00,
    0x21, 0x82, 0x03, 0xb5,
];

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
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [salt ^ 0x8b; 32]),
    )
}

fn fixture(bodies: &[(u32, Vec<u8>)], size: u32, salt: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut in_use = Vec::new();
    for (number, body) in bodies {
        let offset = u64::try_from(bytes.len()).expect("fixture offset fits u64");
        in_use.push((*number, offset));
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

fn page_body(contents: &[u8]) -> Vec<u8> {
    let mut body =
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >>".to_vec();
    body.extend_from_slice(contents);
    body.extend_from_slice(b" >>\nendobj\n");
    body
}

fn stream_body(number: u32, payload: &[u8], filter: Option<&[u8]>) -> Vec<u8> {
    let mut body = format!("{number} 0 obj\n<< /Length {}", payload.len()).into_bytes();
    if let Some(filter) = filter {
        body.extend_from_slice(b" /Filter /");
        body.extend_from_slice(filter);
    }
    body.extend_from_slice(b" >>\nstream\n");
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\nendstream\nendobj\n");
    body
}

fn stream_body_with_filter_value(number: u32, payload: &[u8], filter: &[u8]) -> Vec<u8> {
    let mut body = format!("{number} 0 obj\n<< /Length {}", payload.len()).into_bytes();
    body.extend_from_slice(b" /Filter ");
    body.extend_from_slice(filter);
    body.extend_from_slice(b" >>\nstream\n");
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\nendstream\nendobj\n");
    body
}

fn ascii_hex(bytes: &[u8]) -> Vec<u8> {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = Vec::with_capacity(bytes.len() * 2 + 1);
    for byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)]);
        encoded.push(DIGITS[usize::from(byte & 0x0f)]);
    }
    encoded.push(b'>');
    encoded
}

fn one_page_fixture(contents: &[u8], extras: Vec<(u32, Vec<u8>)>, size: u32, salt: u8) -> Fixture {
    let mut bodies = vec![
        (1, CATALOG.to_vec()),
        (2, ONE_PAGE_ROOT.to_vec()),
        (3, page_body(contents)),
    ];
    bodies.extend(extras);
    fixture(&bodies, size, salt)
}

fn two_stream_fixture(salt: u8) -> Fixture {
    one_page_fixture(
        b" /Contents [4 0 R 5 0 R]",
        vec![
            (4, stream_body(4, IDENTITY_CONTENT, None)),
            (5, stream_body(5, FLATE_CONTENT, Some(b"FlateDecode"))),
        ],
        6,
        salt,
    )
}

fn alias_array_fixture() -> Fixture {
    one_page_fixture(
        b" /Contents 4 0 R",
        vec![
            (4, b"4 0 obj\n5 0 R\nendobj\n".to_vec()),
            (5, b"5 0 obj\n[6 0 R 7 0 R]\nendobj\n".to_vec()),
            (6, stream_body(6, b"BT /F1 12 Tf ET", None)),
            (7, stream_body(7, FLATE_CONTENT, Some(b"FlateDecode"))),
        ],
        8,
        0xe2,
    )
}

fn alias_stream_fixture() -> Fixture {
    one_page_fixture(
        b" /Contents 4 0 R",
        vec![
            (4, b"4 0 obj\n5 0 R\nendobj\n".to_vec()),
            (5, stream_body(5, IDENTITY_CONTENT, None)),
        ],
        6,
        0xe3,
    )
}

fn alias_cycle_fixture() -> Fixture {
    one_page_fixture(
        b" /Contents 4 0 R",
        vec![
            (4, b"4 0 obj\n5 0 R\nendobj\n".to_vec()),
            (5, b"5 0 obj\n4 0 R\nendobj\n".to_vec()),
        ],
        6,
        0xe4,
    )
}

fn padded_empty_content_fixture(entries: usize, salt: u8) -> Fixture {
    let mut page =
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Pad ["
            .to_vec();
    for _ in 0..entries {
        page.extend_from_slice(b" null");
    }
    page.extend_from_slice(b" ] >>\nendobj\n");
    fixture(
        &[
            (1, CATALOG.to_vec()),
            (2, ONE_PAGE_ROOT.to_vec()),
            (3, page),
        ],
        4,
        salt,
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
            JobId::new(8_101),
            ResumeCheckpoint::new(8_102),
            ResumeCheckpoint::new(8_103),
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
            JobId::new(8_201),
            ResumeCheckpoint::new(8_202),
            ResumeCheckpoint::new(8_203),
            ResumeCheckpoint::new(8_204),
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
    index.validate_handle(handle).unwrap();
    Prepared {
        authority,
        store,
        index,
        handle,
    }
}

fn materialize_ready(prepared: &Prepared, seed: u64) -> MaterializedPage {
    let mut job = prepared
        .authority
        .materialize_page(
            &prepared.index,
            prepared.handle,
            materialization_context(seed),
            PageMaterializationLimits::default(),
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

fn acquire_ready(prepared: &Prepared, limits: PageContentLimits, seed: u64) -> AcquiredPageContent {
    let page = materialize_ready(prepared, seed);
    let mut job = prepared
        .authority
        .acquire_page_content(&prepared.index, page, content_context(seed + 20), limits)
        .expect("valid Page content acquisition job");
    match job.poll(&prepared.store, &DocumentNeverCancelled) {
        PageContentPoll::Ready(content) => content,
        PageContentPoll::Pending { .. } => {
            panic!("fully supplied content source must not suspend")
        }
        PageContentPoll::Failed(error) => panic!("valid Page content must acquire: {error}"),
    }
}

fn poll_failure(
    job: &mut AcquirePageContentJob<'_>,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
) -> DocumentError {
    match job.poll(source, cancellation) {
        PageContentPoll::Failed(error) => error,
        PageContentPoll::Ready(_) => panic!("failing input must not publish Page content"),
        PageContentPoll::Pending { .. } => panic!("fully supplied or pre-work failure suspended"),
    }
}

fn limits_with(kind: DocumentLimitKind, value: u64) -> PageContentLimits {
    let mut config = PageContentLimitConfig::default();
    match kind {
        DocumentLimitKind::PageContentStreams => config.max_streams = value,
        DocumentLimitKind::PageContentArrayEntries => config.max_array_entries = value,
        DocumentLimitKind::PageContentObjects => config.max_objects = value,
        DocumentLimitKind::PageContentReferenceEdges => config.max_reference_edges = value,
        DocumentLimitKind::PageContentAliasDepth => config.max_alias_depth = value,
        DocumentLimitKind::PageContentObjectReadBytes => {
            config.max_total_object_read_bytes = value;
        }
        DocumentLimitKind::PageContentObjectParseBytes => {
            config.max_total_object_parse_bytes = value;
        }
        DocumentLimitKind::PageContentEncodedBytes => config.max_total_encoded_bytes = value,
        DocumentLimitKind::PageContentDecodedBytes => config.max_total_decoded_bytes = value,
        DocumentLimitKind::PageContentDecodeFuel => config.max_total_decode_fuel = value,
        DocumentLimitKind::PageContentRetainedStateBytes => {
            config.max_retained_state_bytes = value;
        }
        _ => panic!("test helper accepts only Page content budgets"),
    }
    PageContentLimits::validate(config).expect("positive measured budget validates")
}

fn limits_with_decode(decode_limits: DecodeLimits) -> PageContentLimits {
    PageContentLimits::validate(PageContentLimitConfig {
        decode_limits,
        ..PageContentLimitConfig::default()
    })
    .expect("test per-stream decode limits validate")
}

fn failure_with_limits(prepared: &Prepared, limits: PageContentLimits, seed: u64) -> DocumentError {
    let page = materialize_ready(prepared, seed);
    match prepared.authority.acquire_page_content(
        &prepared.index,
        page,
        content_context(seed + 20),
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
        panic!("terminal or pre-work result must not poll the byte source")
    }
}

struct Cancelled;

impl DocumentCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

struct MutableSnapshotSource<'a> {
    store: &'a RangeStore,
    expected: SourceSnapshot,
    replacement: SourceSnapshot,
    changed: Arc<AtomicBool>,
    armed: Arc<AtomicBool>,
    arm_checkpoint: Option<ResumeCheckpoint>,
}

impl ByteSource for MutableSnapshotSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        if self.changed.load(Ordering::Acquire) {
            self.replacement
        } else {
            self.expected
        }
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        let checkpoint = request.checkpoint();
        let result = self.store.poll(request);
        if self.arm_checkpoint == Some(checkpoint) {
            self.armed.store(true, Ordering::Release);
        }
        result
    }
}

struct PayloadOnlyMissingSource<'a> {
    complete: &'a RangeStore,
    missing: &'a RangeStore,
    payload_checkpoint: ResumeCheckpoint,
    payload_polls: AtomicUsize,
}

impl PayloadOnlyMissingSource<'_> {
    fn payload_polls(&self) -> usize {
        self.payload_polls.load(Ordering::Acquire)
    }
}

impl ByteSource for PayloadOnlyMissingSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.complete.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        if request.checkpoint() == self.payload_checkpoint {
            let _ = self.payload_polls.fetch_add(1, Ordering::AcqRel);
            self.missing.poll(request)
        } else {
            self.complete.poll(request)
        }
    }
}

#[derive(Clone, Copy)]
enum PayloadReadyMode {
    ForeignIdentity,
    WrongRange,
}

struct PayloadReadySource<'a> {
    expected: &'a RangeStore,
    foreign: &'a RangeStore,
    payload_checkpoint: ResumeCheckpoint,
    armed: Arc<AtomicBool>,
    mode: PayloadReadyMode,
}

impl ByteSource for PayloadReadySource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.expected.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        if request.checkpoint() != self.payload_checkpoint {
            return self.expected.poll(request);
        }
        let result = match self.mode {
            PayloadReadyMode::ForeignIdentity => self.foreign.poll(request),
            PayloadReadyMode::WrongRange => {
                let shifted = ByteRange::new(request.range().start() + 1, request.range().len())
                    .expect("fixture payload has room for a shifted same-length range");
                self.expected.poll(ReadRequest::new(
                    shifted,
                    request.priority(),
                    request.job(),
                    request.checkpoint(),
                ))
            }
        };
        self.armed.store(true, Ordering::Release);
        result
    }
}

struct CancelWhenArmed {
    armed: Arc<AtomicBool>,
}

impl DocumentCancellation for CancelWhenArmed {
    fn is_cancelled(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }
}

struct CountCancellation {
    probes: AtomicUsize,
}

impl DocumentCancellation for CountCancellation {
    fn is_cancelled(&self) -> bool {
        let _ = self.probes.fetch_add(1, Ordering::AcqRel);
        false
    }
}

struct FlipOnProbeCancellation {
    probes: AtomicUsize,
    trigger: usize,
    changed: Arc<AtomicBool>,
    mutate_source: bool,
}

impl DocumentCancellation for FlipOnProbeCancellation {
    fn is_cancelled(&self) -> bool {
        let probe = self.probes.fetch_add(1, Ordering::AcqRel);
        if probe < self.trigger {
            return false;
        }
        if self.mutate_source {
            self.changed.store(true, Ordering::Release);
        }
        true
    }
}

struct FlipAfterArmCancellation {
    armed: Arc<AtomicBool>,
    probes: AtomicUsize,
    trigger: usize,
    changed: Arc<AtomicBool>,
    mutate_source: bool,
}

impl DocumentCancellation for FlipAfterArmCancellation {
    fn is_cancelled(&self) -> bool {
        if !self.armed.load(Ordering::Acquire) {
            return false;
        }
        let probe = self.probes.fetch_add(1, Ordering::AcqRel);
        if probe < self.trigger {
            return false;
        }
        if self.mutate_source {
            self.changed.store(true, Ordering::Release);
        }
        true
    }
}

#[test]
fn identity_and_flate_streams_preserve_order_geometry_and_decode_proof() {
    let prepared = prepare(&two_stream_fixture(0xe1), 8_301);
    let content = acquire_ready(&prepared, PageContentLimits::default(), 8_321);

    assert_eq!(content.handle(), prepared.handle);
    assert_eq!(content.page().handle(), prepared.handle);
    assert_eq!(content.page().resources().defining_object(), object_ref(3));
    assert_eq!(content.len(), 2);
    assert_eq!(content.streams()[0].stream_index(), 0);
    assert_eq!(content.streams()[0].reference(), object_ref(4));
    assert_eq!(content.streams()[0].decoded_bytes(), IDENTITY_CONTENT);
    assert!(content.streams()[0].filter_plan().is_empty());
    assert_eq!(content.streams()[1].stream_index(), 1);
    assert_eq!(content.streams()[1].reference(), object_ref(5));
    assert_eq!(content.streams()[1].decoded_bytes(), FLATE_DECODED);
    assert_eq!(
        content.streams()[1].filter_plan().filters(),
        &[StreamFilter::FlateDecode]
    );
    for stream in content.streams() {
        let IndirectObjectValue::Stream(framed) = stream.object().value() else {
            panic!("acquired content retains its framed stream object");
        };
        assert_eq!(stream.dictionary_span(), framed.dictionary().span());
        assert_eq!(stream.data_span(), framed.data_span());
        let decoded = stream
            .decoded()
            .expect("non-empty fixture uses foundational decoder");
        assert_eq!(decoded.attestation().owner(), stream.object().reference());
        assert_eq!(
            decoded.attestation().dictionary_span(),
            stream.dictionary_span()
        );
        assert_eq!(decoded.attestation().encoded_span(), stream.data_span());
        assert_eq!(decoded.attestation().snapshot(), prepared.store.snapshot());
        assert_eq!(
            decoded.attestation().source_identity(),
            prepared.store.snapshot().identity()
        );
        assert_eq!(
            decoded.attestation().encoded().identity(),
            prepared.store.snapshot().identity()
        );
        assert_eq!(
            decoded.attestation().encoded().range(),
            ByteRange::new(framed.data_span().start(), framed.data_span().len()).unwrap()
        );
    }
    assert_eq!(content.stats().streams(), 2);
    assert_eq!(content.stats().array_entries(), 2);
    assert_eq!(content.stats().objects_started(), 3);
    assert_eq!(content.stats().reference_edges(), 2);
    assert_eq!(
        content.stats().encoded_bytes(),
        u64::try_from(IDENTITY_CONTENT.len() + FLATE_CONTENT.len()).unwrap()
    );
    assert_eq!(
        content.stats().decoded_bytes(),
        u64::try_from(IDENTITY_CONTENT.len() + FLATE_DECODED.len()).unwrap()
    );
    assert!(content.stats().decode_fuel() > content.stats().decoded_bytes());
    assert!(content.stats().retained_state_bytes() > 0);
}

#[test]
fn zero_length_identity_stream_has_an_explicit_proof_and_filtered_empty_fails() {
    let empty_fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(4, stream_body(4, b"", None))],
        5,
        0xee,
    );
    let prepared = prepare(&empty_fixture, 8_351);
    let content = acquire_ready(&prepared, PageContentLimits::default(), 8_371);
    assert_eq!(content.len(), 1);
    let stream = &content.streams()[0];
    assert!(stream.decoded().is_none());
    assert!(stream.decoded_bytes().is_empty());
    assert!(stream.data_span().is_empty());
    assert!(stream.filter_plan().is_empty());
    let proof = stream
        .decode()
        .empty_identity()
        .expect("empty identity proof is explicit");
    assert_eq!(proof.owner(), object_ref(4));
    assert_eq!(proof.encoded_span(), stream.data_span());
    assert_eq!(proof.fuel_consumed(), 1);
    assert_eq!(content.stats().encoded_bytes(), 0);
    assert_eq!(content.stats().decoded_bytes(), 0);
    assert_eq!(content.stats().decode_fuel(), 1);

    let filtered_fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(4, stream_body(4, b"", Some(b"FlateDecode")))],
        5,
        0xef,
    );
    let filtered = prepare(&filtered_fixture, 8_381);
    let page = materialize_ready(&filtered, 8_391);
    let mut job = filtered
        .authority
        .acquire_page_content(
            &filtered.index,
            page,
            content_context(8_395),
            PageContentLimits::default(),
        )
        .unwrap();
    assert_eq!(
        poll_failure(&mut job, &filtered.store, &DocumentNeverCancelled).code(),
        DocumentErrorCode::PageContentDecodeFailure
    );
}

#[test]
fn exhausted_decoded_budget_still_accepts_a_known_zero_output_stream() {
    let success_fixture = one_page_fixture(
        b" /Contents [4 0 R 5 0 R]",
        vec![
            (4, stream_body(4, IDENTITY_CONTENT, None)),
            (5, stream_body(5, b"", None)),
        ],
        6,
        0xd6,
    );
    let config = PageContentLimitConfig {
        max_total_decoded_bytes: u64::try_from(IDENTITY_CONTENT.len()).unwrap(),
        ..PageContentLimitConfig::default()
    };
    let exact_limits = PageContentLimits::validate(config).unwrap();
    let success = prepare(&success_fixture, 8_376);
    let content = acquire_ready(&success, exact_limits, 8_378);
    assert_eq!(content.len(), 2);
    assert_eq!(
        content.stats().decoded_bytes(),
        u64::try_from(IDENTITY_CONTENT.len()).unwrap()
    );
    assert!(content.streams()[1].decode().empty_identity().is_some());

    let nonzero_fixture = one_page_fixture(
        b" /Contents [4 0 R 5 0 R]",
        vec![
            (4, stream_body(4, IDENTITY_CONTENT, None)),
            (5, stream_body(5, b"x", None)),
        ],
        6,
        0xd7,
    );
    let failure = prepare(&nonzero_fixture, 8_382);
    let error = failure_with_limits(&failure, exact_limits, 8_384);
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().expect("decoded aggregate detail").kind(),
        DocumentLimitKind::PageContentDecodedBytes
    );
}

#[test]
fn decoded_aggregate_caps_only_final_output_not_layer_or_cumulative_output() {
    let encoded = ascii_hex(FLATE_CONTENT);
    let fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(
            4,
            stream_body_with_filter_value(4, &encoded, b"[/ASCIIHexDecode /FlateDecode]"),
        )],
        5,
        0xda,
    );
    let prepared = prepare(&fixture, 8_386);
    let limits = PageContentLimits::validate(PageContentLimitConfig {
        max_total_decoded_bytes: u64::try_from(FLATE_DECODED.len()).unwrap(),
        ..PageContentLimitConfig::default()
    })
    .unwrap();
    let content = acquire_ready(&prepared, limits, 8_388);
    let decoded = content.streams()[0].decoded().unwrap();
    assert_eq!(decoded.bytes(), FLATE_DECODED);
    assert!(
        decoded.attestation().cumulative_output_bytes()
            > u64::try_from(FLATE_DECODED.len()).unwrap()
    );
    assert_eq!(
        content.stats().decoded_bytes(),
        u64::try_from(FLATE_DECODED.len()).unwrap()
    );
}

#[test]
fn intrinsic_stream_limits_preserve_lower_dimensions_and_values() {
    let input_fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(4, stream_body(4, b"abcde", None))],
        5,
        0xdb,
    );
    let input = prepare(&input_fixture, 8_410);
    let input_error = failure_with_limits(
        &input,
        limits_with_decode(
            DecodeLimits::validate(DecodeLimitConfig {
                max_input_bytes: 4,
                ..DecodeLimitConfig::default()
            })
            .unwrap(),
        ),
        8_412,
    );
    let input_limit = input_error.limit().expect("intrinsic input limit detail");
    assert_eq!(
        input_limit.kind(),
        DocumentLimitKind::PageContentStreamInputBytes
    );
    assert_eq!(input_limit.limit(), 4);
    assert_eq!(input_limit.consumed(), 0);
    assert_eq!(input_limit.attempted(), 5);

    let filters_fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(
            4,
            stream_body_with_filter_value(4, b"00>", b"[/ASCIIHexDecode /FlateDecode]"),
        )],
        5,
        0xdc,
    );
    let filters = prepare(&filters_fixture, 8_420);
    let filters_error = failure_with_limits(
        &filters,
        limits_with_decode(
            DecodeLimits::validate(DecodeLimitConfig {
                max_filters: 1,
                ..DecodeLimitConfig::default()
            })
            .unwrap(),
        ),
        8_422,
    );
    let filters_limit = filters_error
        .limit()
        .expect("intrinsic filter-count limit detail");
    assert_eq!(
        filters_limit.kind(),
        DocumentLimitKind::PageContentStreamFilters
    );
    assert_eq!(filters_limit.limit(), 1);
    assert_eq!(filters_limit.consumed(), 0);
    assert_eq!(filters_limit.attempted(), 2);

    for (case, (decode_limits, kind)) in [
        (
            DecodeLimits::validate(DecodeLimitConfig {
                max_layer_output_bytes: 4,
                max_final_output_bytes: 4,
                ..DecodeLimitConfig::default()
            })
            .unwrap(),
            DocumentLimitKind::PageContentStreamLayerOutputBytes,
        ),
        (
            DecodeLimits::validate(DecodeLimitConfig {
                max_total_output_bytes: 4,
                max_final_output_bytes: 4,
                ..DecodeLimitConfig::default()
            })
            .unwrap(),
            DocumentLimitKind::PageContentStreamTotalOutputBytes,
        ),
        (
            DecodeLimits::validate(DecodeLimitConfig {
                max_final_output_bytes: 4,
                ..DecodeLimitConfig::default()
            })
            .unwrap(),
            DocumentLimitKind::PageContentStreamFinalOutputBytes,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = one_page_fixture(
            b" /Contents 4 0 R",
            vec![(4, stream_body(4, b"abcde", None))],
            5,
            0xb0 + u8::try_from(case).unwrap(),
        );
        let prepared = prepare(&fixture, 8_430 + u64::try_from(case).unwrap() * 10);
        let error = failure_with_limits(
            &prepared,
            limits_with_decode(decode_limits),
            8_432 + u64::try_from(case).unwrap() * 10,
        );
        let limit = error.limit().expect("intrinsic output limit detail");
        assert_eq!(limit.kind(), kind);
        assert_eq!(limit.limit(), 4);
        assert_eq!(limit.consumed(), 4);
        assert_eq!(limit.attempted(), 5);
    }

    let fuel_fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(4, stream_body(4, b"x", None))],
        5,
        0xdd,
    );
    let fuel = prepare(&fuel_fixture, 8_470);
    let fuel_error = failure_with_limits(
        &fuel,
        limits_with_decode(
            DecodeLimits::validate(DecodeLimitConfig {
                max_fuel: 1,
                cancellation_check_interval_fuel: 1,
                ..DecodeLimitConfig::default()
            })
            .unwrap(),
        ),
        8_472,
    );
    let fuel_limit = fuel_error.limit().expect("intrinsic fuel limit detail");
    assert_eq!(
        fuel_limit.kind(),
        DocumentLimitKind::PageContentStreamDecodeFuel
    );
    assert_eq!(fuel_limit.limit(), 1);
    assert_eq!(fuel_limit.consumed(), 1);
    assert_eq!(fuel_limit.attempted(), 2);

    let retained_fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(
            4,
            stream_body_with_filter_value(4, b"34313E>", b"[/ASCIIHexDecode /ASCIIHexDecode]"),
        )],
        5,
        0xde,
    );
    let retained = prepare(&retained_fixture, 8_480);
    let retained_error = failure_with_limits(
        &retained,
        limits_with_decode(
            DecodeLimits::validate(DecodeLimitConfig {
                max_final_output_bytes: 4,
                max_retained_capacity_bytes: 4,
                ..DecodeLimitConfig::default()
            })
            .unwrap(),
        ),
        8_482,
    );
    let retained_limit = retained_error
        .limit()
        .expect("intrinsic retained-capacity limit detail");
    assert_eq!(
        retained_limit.kind(),
        DocumentLimitKind::PageContentStreamRetainedBytes
    );
    assert_eq!(retained_limit.limit(), 4);
    assert_eq!(retained_limit.consumed(), 4);
    assert_eq!(retained_limit.attempted(), 5);
}

#[test]
fn payload_input_preflight_selects_the_tighter_owner_without_polling_payload() {
    let fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(4, stream_body(4, b"abcde", None))],
        5,
        0xb8,
    );
    let prepared = prepare(&fixture, 8_484);
    let missing = RangeStore::new(fixture.snapshot, Default::default()).unwrap();

    let baseline_context = content_context(8_486);
    let baseline_page = materialize_ready(&prepared, 8_485);
    let mut baseline = prepared
        .authority
        .acquire_page_content(
            &prepared.index,
            baseline_page,
            baseline_context,
            PageContentLimits::default(),
        )
        .unwrap();
    let baseline_source = PayloadOnlyMissingSource {
        complete: &prepared.store,
        missing: &missing,
        payload_checkpoint: baseline_context.payload_checkpoint(),
        payload_polls: AtomicUsize::new(0),
    };
    match baseline.poll(&baseline_source, &DocumentNeverCancelled) {
        PageContentPoll::Pending { checkpoint, .. } => {
            assert_eq!(checkpoint, baseline_context.payload_checkpoint())
        }
        outcome => panic!("missing payload must suspend at its checkpoint, got {outcome:?}"),
    }
    assert_eq!(baseline_source.payload_polls(), 1);

    for (case, (parent_limit, intrinsic_limit, kind)) in [
        (4, 5, DocumentLimitKind::PageContentEncodedBytes),
        (5, 4, DocumentLimitKind::PageContentStreamInputBytes),
        (4, 4, DocumentLimitKind::PageContentStreamInputBytes),
    ]
    .into_iter()
    .enumerate()
    {
        let decode_limits = DecodeLimits::validate(DecodeLimitConfig {
            max_input_bytes: intrinsic_limit,
            ..DecodeLimitConfig::default()
        })
        .unwrap();
        let limits = PageContentLimits::validate(PageContentLimitConfig {
            max_total_encoded_bytes: parent_limit,
            decode_limits,
            ..PageContentLimitConfig::default()
        })
        .unwrap();
        let context = content_context(8_490 + u64::try_from(case).unwrap() * 10);
        let page = materialize_ready(&prepared, 8_489 + u64::try_from(case).unwrap() * 10);
        let mut job = prepared
            .authority
            .acquire_page_content(&prepared.index, page, context, limits)
            .unwrap();
        let source = PayloadOnlyMissingSource {
            complete: &prepared.store,
            missing: &missing,
            payload_checkpoint: context.payload_checkpoint(),
            payload_polls: AtomicUsize::new(0),
        };
        let error = poll_failure(&mut job, &source, &DocumentNeverCancelled);
        let detail = error.limit().expect("payload input preflight limit detail");
        assert_eq!(detail.kind(), kind, "preflight precedence case {case}");
        assert_eq!(
            detail.limit(),
            if kind == DocumentLimitKind::PageContentEncodedBytes {
                parent_limit
            } else {
                intrinsic_limit
            }
        );
        assert_eq!(detail.consumed(), 0);
        assert_eq!(detail.attempted(), 5);
        assert_eq!(
            source.payload_polls(),
            0,
            "preflight case {case} must reject before payload polling"
        );
    }
}

#[test]
fn empty_identity_requires_one_nonzero_lower_retained_permit_without_counting_it_as_heap() {
    let fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(4, stream_body(4, b"", None))],
        5,
        0xdf,
    );
    let baseline_prepared = prepare(&fixture, 8_490);
    let baseline = acquire_ready(&baseline_prepared, PageContentLimits::default(), 8_492);
    let measured_heap = baseline.stats().peak_retained_state_bytes();
    let exact = measured_heap.checked_add(1).unwrap();

    let exact_prepared = prepare(&fixture, 8_494);
    let content = acquire_ready(
        &exact_prepared,
        limits_with(DocumentLimitKind::PageContentRetainedStateBytes, exact),
        8_496,
    );
    assert_eq!(content.stats().peak_retained_state_bytes(), measured_heap);

    let tight_prepared = prepare(&fixture, 8_498);
    let error = failure_with_limits(
        &tight_prepared,
        limits_with(DocumentLimitKind::PageContentRetainedStateBytes, exact - 1),
        8_500,
    );
    let limit = error.limit().expect("empty retained permit limit detail");
    assert_eq!(
        limit.kind(),
        DocumentLimitKind::PageContentRetainedStateBytes
    );
    assert_eq!(limit.limit(), exact - 1);
    assert_eq!(limit.attempted(), 1);
}

#[test]
fn unsupported_filter_and_malformed_flate_have_distinct_page_content_codes() {
    for (filter, payload, code, salt) in [
        (
            b"LZWDecode".as_slice(),
            b"arbitrary".as_slice(),
            DocumentErrorCode::UnsupportedPageContentFilter,
            0xd8,
        ),
        (
            b"FlateDecode".as_slice(),
            b"not-a-zlib-stream".as_slice(),
            DocumentErrorCode::PageContentDecodeFailure,
            0xd9,
        ),
    ] {
        let fixture = one_page_fixture(
            b" /Contents 4 0 R",
            vec![(4, stream_body(4, payload, Some(filter)))],
            5,
            salt,
        );
        let prepared = prepare(&fixture, 8_397 + u64::from(salt));
        let page = materialize_ready(&prepared, 8_399 + u64::from(salt));
        let mut job = prepared
            .authority
            .acquire_page_content(
                &prepared.index,
                page,
                content_context(8_401 + u64::from(salt)),
                PageContentLimits::default(),
            )
            .unwrap();
        assert_eq!(
            poll_failure(&mut job, &prepared.store, &DocumentNeverCancelled).code(),
            code
        );
    }
}

#[test]
fn absent_null_and_empty_array_publish_deterministic_empty_content() {
    for (contents, salt) in [
        (b"".as_slice(), 0xe5),
        (b" /Contents null".as_slice(), 0xe6),
        (b" /Contents []".as_slice(), 0xe7),
    ] {
        let fixture = one_page_fixture(contents, Vec::new(), 4, salt);
        let prepared = prepare(&fixture, 8_401 + u64::from(salt));
        let content = acquire_ready(
            &prepared,
            PageContentLimits::default(),
            8_421 + u64::from(salt),
        );
        assert!(content.is_empty());
        assert_eq!(content.stats().streams(), 0);
        assert_eq!(content.stats().objects_started(), 1);
        assert_eq!(content.stats().encoded_bytes(), 0);
        assert_eq!(content.stats().decoded_bytes(), 0);
    }
}

#[test]
fn whole_object_aliases_resolve_to_one_stream_or_an_ordered_array() {
    let stream_prepared = prepare(&alias_stream_fixture(), 8_501);
    let stream = acquire_ready(&stream_prepared, PageContentLimits::default(), 8_521);
    assert_eq!(stream.len(), 1);
    assert_eq!(stream.streams()[0].reference(), object_ref(5));
    assert_eq!(stream.streams()[0].decoded_bytes(), IDENTITY_CONTENT);
    assert_eq!(stream.stats().objects_started(), 3);
    assert_eq!(stream.stats().reference_edges(), 2);
    assert_eq!(stream.stats().max_alias_depth(), 2);

    let array_prepared = prepare(&alias_array_fixture(), 8_551);
    let array = acquire_ready(&array_prepared, PageContentLimits::default(), 8_571);
    assert_eq!(
        array
            .streams()
            .iter()
            .map(|stream| stream.reference())
            .collect::<Vec<_>>(),
        vec![object_ref(6), object_ref(7)]
    );
    assert_eq!(array.stats().objects_started(), 5);
    assert_eq!(array.stats().reference_edges(), 4);
    assert_eq!(array.stats().max_alias_depth(), 2);
}

#[test]
fn alias_depth_budget_bounds_cycle_scans_and_accepts_the_exact_chain() {
    let fixture = alias_stream_fixture();
    let baseline_prepared = prepare(&fixture, 8_581);
    let baseline = acquire_ready(&baseline_prepared, PageContentLimits::default(), 8_591);
    assert_eq!(baseline.stats().max_alias_depth(), 2);

    let exact_prepared = prepare(&fixture, 8_593);
    let exact = acquire_ready(
        &exact_prepared,
        limits_with(DocumentLimitKind::PageContentAliasDepth, 2),
        8_595,
    );
    assert_eq!(exact.stats().max_alias_depth(), 2);

    let tight_prepared = prepare(&fixture, 8_597);
    let error = failure_with_limits(
        &tight_prepared,
        limits_with(DocumentLimitKind::PageContentAliasDepth, 1),
        8_599,
    );
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().expect("alias-depth detail").kind(),
        DocumentLimitKind::PageContentAliasDepth
    );

    let cycle_prepared = prepare(&alias_cycle_fixture(), 8_600);
    let cycle_page = materialize_ready(&cycle_prepared, 8_602);
    let mut cycle = cycle_prepared
        .authority
        .acquire_page_content(
            &cycle_prepared.index,
            cycle_page,
            content_context(8_604),
            limits_with(DocumentLimitKind::PageContentAliasDepth, 3),
        )
        .unwrap();
    assert_eq!(
        poll_failure(&mut cycle, &cycle_prepared.store, &DocumentNeverCancelled).code(),
        DocumentErrorCode::PageContentAliasCycle
    );
}

#[test]
fn cycles_duplicate_contents_and_wrong_shapes_have_specific_terminal_codes() {
    let cycle_prepared = prepare(&alias_cycle_fixture(), 8_601);
    let page = materialize_ready(&cycle_prepared, 8_621);
    let mut cycle = cycle_prepared
        .authority
        .acquire_page_content(
            &cycle_prepared.index,
            page,
            content_context(8_641),
            PageContentLimits::default(),
        )
        .unwrap();
    let cycle_error = poll_failure(&mut cycle, &cycle_prepared.store, &DocumentNeverCancelled);
    assert_eq!(cycle_error.code(), DocumentErrorCode::PageContentAliasCycle);
    assert_eq!(cycle.phase(), PageContentPhase::Failed);
    assert_eq!(
        poll_failure(
            &mut cycle,
            &PanicSource(cycle_prepared.store.snapshot()),
            &DocumentNeverCancelled
        ),
        cycle_error
    );

    let cases = [
        (
            b" /Contents 4 0 R /Contents 5 0 R".as_slice(),
            vec![
                (4, stream_body(4, b"a", None)),
                (5, stream_body(5, b"b", None)),
            ],
            6,
            DocumentErrorCode::DuplicatePageContents,
        ),
        (
            b" /Contents 7".as_slice(),
            Vec::new(),
            4,
            DocumentErrorCode::InvalidPageContents,
        ),
        (
            b" /Contents << /Unexpected true >>".as_slice(),
            Vec::new(),
            4,
            DocumentErrorCode::InvalidPageContents,
        ),
        (
            b" /Contents 4 0 R".as_slice(),
            vec![(4, b"4 0 obj\n<< /Unexpected true >>\nendobj\n".to_vec())],
            5,
            DocumentErrorCode::InvalidPageContents,
        ),
        (
            b" /Contents [[4 0 R]]".as_slice(),
            vec![(4, stream_body(4, b"a", None))],
            5,
            DocumentErrorCode::UnsupportedPageContentsRepresentation,
        ),
        (
            b" /Contents [4 0 R]".as_slice(),
            vec![(4, b"4 0 obj\n5 0 R\nendobj\n".to_vec())],
            6,
            DocumentErrorCode::UnsupportedPageContentsRepresentation,
        ),
        (
            b" /Contents [4 0 R]".as_slice(),
            vec![(4, b"4 0 obj\n42\nendobj\n".to_vec())],
            5,
            DocumentErrorCode::InvalidPageContents,
        ),
    ];
    for (case, (contents, extras, size, code)) in cases.into_iter().enumerate() {
        let fixture = one_page_fixture(contents, extras, size, 0xf0 + u8::try_from(case).unwrap());
        let prepared = prepare(&fixture, 8_701 + u64::try_from(case).unwrap() * 100);
        let page = materialize_ready(&prepared, 8_721 + u64::try_from(case).unwrap() * 100);
        let mut job = prepared
            .authority
            .acquire_page_content(
                &prepared.index,
                page,
                content_context(8_741 + u64::try_from(case).unwrap() * 100),
                PageContentLimits::default(),
            )
            .unwrap();
        let error = poll_failure(&mut job, &prepared.store, &DocumentNeverCancelled);
        assert_eq!(error.code(), code, "failure case {case}");
    }
}

#[test]
fn child_object_retained_capacity_is_lent_before_syntax_allocation() {
    let fixture = padded_empty_content_fixture(512, 0xc6);
    let baseline_prepared = prepare(&fixture, 9_101);
    let baseline = acquire_ready(&baseline_prepared, PageContentLimits::default(), 9_121);
    let exact = baseline.stats().peak_retained_state_bytes();
    assert!(exact > 1);
    assert!(baseline.is_empty());

    let exact_prepared = prepare(&fixture, 9_141);
    let exact_content = acquire_ready(
        &exact_prepared,
        limits_with(DocumentLimitKind::PageContentRetainedStateBytes, exact),
        9_161,
    );
    assert_eq!(exact_content.stats().peak_retained_state_bytes(), exact);

    let tight_prepared = prepare(&fixture, 9_181);
    let page = materialize_ready(&tight_prepared, 9_201);
    let mut job = tight_prepared
        .authority
        .acquire_page_content(
            &tight_prepared.index,
            page,
            content_context(9_221),
            limits_with(DocumentLimitKind::PageContentRetainedStateBytes, exact - 1),
        )
        .unwrap();
    let error = poll_failure(&mut job, &tight_prepared.store, &DocumentNeverCancelled);
    let aggregate = error.limit().expect("Page retained aggregate detail");
    assert_eq!(
        aggregate.kind(),
        DocumentLimitKind::PageContentRetainedStateBytes
    );
    assert_eq!(aggregate.limit(), exact - 1);
    assert!(aggregate.consumed() + aggregate.attempted() > aggregate.limit());
    let lower = error
        .object_error()
        .expect("aggregate retained failure preserves the object error");
    assert_eq!(lower.code(), ObjectErrorCode::SyntaxFailure);
    let syntax = lower
        .syntax_error()
        .and_then(|error| error.limit())
        .expect("object error preserves the pre-allocation syntax limit");
    assert_eq!(syntax.kind(), SyntaxLimitKind::RetainedBytes);
    assert_eq!(aggregate.attempted(), syntax.attempted());
    assert_eq!(
        aggregate.consumed(),
        (exact - 1 - syntax.limit()) + syntax.consumed()
    );
    assert_eq!(job.stats().objects_started(), 1);
    assert_eq!(job.stats().streams(), 0);
}

#[test]
fn exact_measured_budgets_succeed_and_one_less_fails_in_each_dimension() {
    let fixture = two_stream_fixture(0xe8);
    let baseline_prepared = prepare(&fixture, 9_301);
    let baseline = acquire_ready(&baseline_prepared, PageContentLimits::default(), 9_321);
    let stats = baseline.stats();
    let encoded_exact = u64::try_from(IDENTITY_CONTENT.len() + FLATE_CONTENT.len()).unwrap();
    let decoded_exact = u64::try_from(IDENTITY_CONTENT.len() + FLATE_DECODED.len()).unwrap();
    let measured = [
        (DocumentLimitKind::PageContentStreams, 2, Some((0, 2))),
        (DocumentLimitKind::PageContentArrayEntries, 2, Some((0, 2))),
        (DocumentLimitKind::PageContentObjects, 3, Some((2, 1))),
        (
            DocumentLimitKind::PageContentReferenceEdges,
            2,
            Some((1, 1)),
        ),
        (
            DocumentLimitKind::PageContentObjectReadBytes,
            stats.object_read_bytes(),
            None,
        ),
        (
            DocumentLimitKind::PageContentObjectParseBytes,
            stats.object_parse_bytes(),
            None,
        ),
        (
            DocumentLimitKind::PageContentEncodedBytes,
            encoded_exact,
            Some((
                u64::try_from(IDENTITY_CONTENT.len()).unwrap(),
                u64::try_from(FLATE_CONTENT.len()).unwrap(),
            )),
        ),
        (
            DocumentLimitKind::PageContentDecodedBytes,
            decoded_exact,
            Some((decoded_exact - 1, 1)),
        ),
        (
            DocumentLimitKind::PageContentDecodeFuel,
            stats.decode_fuel(),
            None,
        ),
        (
            DocumentLimitKind::PageContentRetainedStateBytes,
            stats.peak_retained_state_bytes(),
            None,
        ),
    ];

    for (case, (kind, exact, fixed_rejection)) in measured.into_iter().enumerate() {
        assert!(exact > 1, "{kind:?} must exercise a one-less runtime limit");
        let exact_prepared = prepare(&fixture, 9_501 + u64::try_from(case).unwrap() * 100);
        let exact_content = acquire_ready(
            &exact_prepared,
            limits_with(kind, exact),
            9_521 + u64::try_from(case).unwrap() * 100,
        );
        let exact_stats = exact_content.stats();
        let observed_exact = match kind {
            DocumentLimitKind::PageContentStreams => exact_stats.streams(),
            DocumentLimitKind::PageContentArrayEntries => exact_stats.array_entries(),
            DocumentLimitKind::PageContentObjects => exact_stats.objects_started(),
            DocumentLimitKind::PageContentReferenceEdges => exact_stats.reference_edges(),
            DocumentLimitKind::PageContentObjectReadBytes => exact_stats.object_read_bytes(),
            DocumentLimitKind::PageContentObjectParseBytes => exact_stats.object_parse_bytes(),
            DocumentLimitKind::PageContentEncodedBytes => exact_stats.encoded_bytes(),
            DocumentLimitKind::PageContentDecodedBytes => exact_stats.decoded_bytes(),
            DocumentLimitKind::PageContentDecodeFuel => exact_stats.decode_fuel(),
            DocumentLimitKind::PageContentRetainedStateBytes => {
                exact_stats.peak_retained_state_bytes()
            }
            _ => unreachable!("measured list contains only Page content aggregate limits"),
        };
        assert_eq!(observed_exact, exact, "exact {kind:?}");

        let tight_prepared = prepare(&fixture, 11_001 + u64::try_from(case).unwrap() * 100);
        let error = failure_with_limits(
            &tight_prepared,
            limits_with(kind, exact - 1),
            11_021 + u64::try_from(case).unwrap() * 100,
        );
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit, "{kind:?}");
        let detail = error.limit().expect("aggregate limit detail");
        assert_eq!(detail.kind(), kind, "{kind:?}");
        assert_eq!(detail.limit(), exact - 1, "{kind:?}");
        assert!(detail.attempted() > 0, "{kind:?}");
        assert!(detail.consumed() <= detail.limit(), "{kind:?}");
        assert!(
            detail.attempted() > detail.limit().saturating_sub(detail.consumed()),
            "{kind:?} evidence must prove the rejected work exceeds the limit"
        );
        if let Some((consumed, attempted)) = fixed_rejection {
            assert_eq!(detail.consumed(), consumed, "{kind:?}");
            assert_eq!(detail.attempted(), attempted, "{kind:?}");
        }
        if matches!(
            kind,
            DocumentLimitKind::PageContentObjectReadBytes
                | DocumentLimitKind::PageContentObjectParseBytes
        ) {
            let lower = error
                .object_error()
                .expect("object aggregate preserves its lower failure");
            let lower_detail = lower.limit().expect("lower object limit detail");
            assert_eq!(
                lower_detail.kind(),
                if kind == DocumentLimitKind::PageContentObjectReadBytes {
                    ObjectLimitKind::TotalReadBytes
                } else {
                    ObjectLimitKind::TotalParseBytes
                }
            );
            assert_eq!(detail.attempted(), lower_detail.attempted());
            assert!(
                lower_detail.attempted()
                    > lower_detail.limit().saturating_sub(lower_detail.consumed()),
                "{kind:?} lower evidence must independently prove the over-limit work"
            );
        }
    }
}

#[test]
fn pending_replays_then_resumes_without_duplicate_work_or_publication() {
    let fixture = two_stream_fixture(0xe9);
    let prepared = prepare(&fixture, 12_301);
    let page = materialize_ready(&prepared, 12_321);
    let mut job = prepared
        .authority
        .acquire_page_content(
            &prepared.index,
            page,
            content_context(12_341),
            PageContentLimits::default(),
        )
        .unwrap();
    let empty = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let first = match job.poll(&empty, &DocumentNeverCancelled) {
        PageContentPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => (ticket, missing, checkpoint),
        outcome => panic!("empty source must suspend, got {outcome:?}"),
    };
    let before = job.stats();
    let repeated = match job.poll(&empty, &DocumentNeverCancelled) {
        PageContentPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => (ticket, missing, checkpoint),
        outcome => panic!("unchanged empty source must replay Pending, got {outcome:?}"),
    };
    assert_eq!(repeated, first);
    assert_eq!(job.stats(), before);

    let range = ByteRange::new(0, u64::try_from(fixture.bytes.len()).unwrap()).unwrap();
    empty
        .supply(RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone()).unwrap())
        .unwrap();
    let content = match job.poll(&empty, &DocumentNeverCancelled) {
        PageContentPoll::Ready(content) => content,
        outcome => panic!("supplied source must finish, got {outcome:?}"),
    };
    assert_eq!(content.len(), 2);
    let terminal_stats = job.stats();
    match job.poll(&PanicSource(fixture.snapshot), &DocumentNeverCancelled) {
        PageContentPoll::Failed(error) => {
            assert_eq!(error.code(), DocumentErrorCode::JobAlreadyComplete)
        }
        outcome => panic!("Ready replay must be terminal, got {outcome:?}"),
    }
    assert_eq!(job.stats(), terminal_stats);
}

#[test]
fn source_mismatch_precedes_cancellation_and_failed_replay_reads_nothing() {
    let fixture = two_stream_fixture(0xea);
    let prepared = prepare(&fixture, 12_601);
    let page = materialize_ready(&prepared, 12_621);
    let mut job = prepared
        .authority
        .acquire_page_content(
            &prepared.index,
            page,
            content_context(12_641),
            PageContentLimits::default(),
        )
        .unwrap();
    let wrong = snapshot(u64::try_from(fixture.bytes.len()).unwrap(), 0xeb);
    let failure = poll_failure(&mut job, &PanicSource(wrong), &Cancelled);
    assert_eq!(failure.code(), DocumentErrorCode::SourceSnapshotMismatch);
    assert_eq!(job.phase(), PageContentPhase::Failed);
    assert_eq!(
        poll_failure(
            &mut job,
            &PanicSource(fixture.snapshot),
            &DocumentNeverCancelled
        ),
        failure
    );
}

#[test]
fn payload_ready_identity_precedes_cancellation_but_wrong_range_follows_it() {
    let fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(4, stream_body(4, IDENTITY_CONTENT, None))],
        5,
        0xce,
    );
    let prepared = prepare(&fixture, 12_605);
    let foreign_snapshot = snapshot(u64::try_from(fixture.bytes.len()).unwrap(), 0xcf);
    let foreign = RangeStore::new(foreign_snapshot, Default::default()).unwrap();
    let full_range = ByteRange::new(0, u64::try_from(fixture.bytes.len()).unwrap()).unwrap();
    foreign
        .supply(RangeResponse::new(foreign_snapshot, full_range, fixture.bytes.clone()).unwrap())
        .unwrap();

    for (case, (mode, cancelled, expected)) in [
        (
            PayloadReadyMode::ForeignIdentity,
            true,
            DocumentErrorCode::SourceSnapshotMismatch,
        ),
        (
            PayloadReadyMode::WrongRange,
            true,
            DocumentErrorCode::Cancelled,
        ),
        (
            PayloadReadyMode::WrongRange,
            false,
            DocumentErrorCode::InternalState,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let context = content_context(12_607 + u64::try_from(case).unwrap() * 10);
        let page = materialize_ready(&prepared, 12_606 + u64::try_from(case).unwrap() * 10);
        let mut job = prepared
            .authority
            .acquire_page_content(&prepared.index, page, context, PageContentLimits::default())
            .unwrap();
        let armed = Arc::new(AtomicBool::new(false));
        let source = PayloadReadySource {
            expected: &prepared.store,
            foreign: &foreign,
            payload_checkpoint: context.payload_checkpoint(),
            armed: Arc::clone(&armed),
            mode,
        };
        let cancellation = CancelWhenArmed {
            armed: Arc::clone(&armed),
        };
        let cancellation: &dyn DocumentCancellation = if cancelled {
            &cancellation
        } else {
            &DocumentNeverCancelled
        };
        let error = poll_failure(&mut job, &source, cancellation);
        assert!(armed.load(Ordering::Acquire));
        assert_eq!(
            error.code(),
            expected,
            "payload Ready precedence case {case}"
        );
    }
}

#[test]
fn correct_source_cancellation_precedes_document_work() {
    let fixture = two_stream_fixture(0xc1);
    let prepared = prepare(&fixture, 12_651);
    let page = materialize_ready(&prepared, 12_671);
    let mut job = prepared
        .authority
        .acquire_page_content(
            &prepared.index,
            page,
            content_context(12_691),
            PageContentLimits::default(),
        )
        .unwrap();
    let error = poll_failure(&mut job, &prepared.store, &Cancelled);
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
    assert_eq!(job.stats(), Default::default());
}

#[test]
fn source_mutation_during_filter_metadata_or_decode_precedes_lower_cancellation() {
    let fixture = one_page_fixture(
        b" /Contents 4 0 R",
        vec![(4, stream_body(4, b"x", None))],
        5,
        0xc2,
    );
    let prepared = prepare(&fixture, 12_701);
    let wrong = snapshot(u64::try_from(fixture.bytes.len()).unwrap(), 0xc3);
    for (case, (trigger, mutate_source, expected)) in [
        (1, false, DocumentErrorCode::Cancelled),
        (1, true, DocumentErrorCode::SourceSnapshotMismatch),
        (12, true, DocumentErrorCode::SourceSnapshotMismatch),
    ]
    .into_iter()
    .enumerate()
    {
        let context = content_context(12_721 + u64::try_from(case).unwrap() * 10);
        let page = materialize_ready(&prepared, 12_723 + u64::try_from(case).unwrap() * 10);
        let mut job = prepared
            .authority
            .acquire_page_content(&prepared.index, page, context, PageContentLimits::default())
            .unwrap();
        let changed = Arc::new(AtomicBool::new(false));
        let armed = Arc::new(AtomicBool::new(false));
        let source = MutableSnapshotSource {
            store: &prepared.store,
            expected: fixture.snapshot,
            replacement: wrong,
            changed: Arc::clone(&changed),
            armed: Arc::clone(&armed),
            arm_checkpoint: Some(context.payload_checkpoint()),
        };
        let cancellation = FlipAfterArmCancellation {
            armed,
            probes: AtomicUsize::new(0),
            trigger,
            changed,
            mutate_source,
        };
        let error = poll_failure(&mut job, &source, &cancellation);
        assert_eq!(error.code(), expected, "runtime precedence case {case}");
        assert!(
            cancellation.probes.load(Ordering::Acquire) > trigger,
            "the requested metadata/decode probe must have been reached"
        );
    }
}

#[test]
fn accept_page_semantic_fallback_rechecks_source_then_cancellation() {
    let fixture = one_page_fixture(
        b" /Contents 4 0 R /Contents 5 0 R",
        vec![
            (4, stream_body(4, b"a", None)),
            (5, stream_body(5, b"b", None)),
        ],
        6,
        0xc4,
    );
    let prepared = prepare(&fixture, 12_801);

    let page = materialize_ready(&prepared, 12_821);
    let mut baseline = prepared
        .authority
        .acquire_page_content(
            &prepared.index,
            page,
            content_context(12_841),
            PageContentLimits::default(),
        )
        .unwrap();
    let counter = CountCancellation {
        probes: AtomicUsize::new(0),
    };
    let baseline_error = poll_failure(&mut baseline, &prepared.store, &counter);
    assert_eq!(
        baseline_error.code(),
        DocumentErrorCode::DuplicatePageContents
    );
    let probes = counter.probes.load(Ordering::Acquire);
    assert!(probes > 0);

    let wrong = snapshot(u64::try_from(fixture.bytes.len()).unwrap(), 0xc5);
    for (case, (mutate_source, expected)) in [
        (false, DocumentErrorCode::Cancelled),
        (true, DocumentErrorCode::SourceSnapshotMismatch),
    ]
    .into_iter()
    .enumerate()
    {
        let page = materialize_ready(&prepared, 12_861 + u64::try_from(case).unwrap() * 10);
        let mut job = prepared
            .authority
            .acquire_page_content(
                &prepared.index,
                page,
                content_context(12_863 + u64::try_from(case).unwrap() * 10),
                PageContentLimits::default(),
            )
            .unwrap();
        let changed = Arc::new(AtomicBool::new(false));
        let source = MutableSnapshotSource {
            store: &prepared.store,
            expected: fixture.snapshot,
            replacement: wrong,
            changed: Arc::clone(&changed),
            armed: Arc::new(AtomicBool::new(false)),
            arm_checkpoint: None,
        };
        let cancellation = FlipOnProbeCancellation {
            probes: AtomicUsize::new(0),
            trigger: probes - 1,
            changed,
            mutate_source,
        };
        let error = poll_failure(&mut job, &source, &cancellation);
        assert_eq!(error.code(), expected, "accept fallback case {case}");
    }
}

#[test]
fn owned_job_retains_authority_and_materialized_page_after_handles_drop() {
    let fixture = two_stream_fixture(0xec);
    let Prepared {
        authority,
        store,
        index,
        handle,
    } = prepare(&fixture, 12_901);
    let temporary = Prepared {
        authority,
        store,
        index,
        handle,
    };
    let page = materialize_ready(&temporary, 12_921);
    let Prepared {
        authority,
        store,
        index,
        handle: _,
    } = temporary;
    let shared = authority.into_shared();
    let mut job = shared
        .acquire_page_content_owned(
            &index,
            page,
            content_context(12_941),
            PageContentLimits::default(),
        )
        .unwrap();
    drop(shared);
    let content = match job.poll(&store, &DocumentNeverCancelled) {
        PageContentPoll::Ready(content) => content,
        outcome => panic!("owned job must finish after source handle drop: {outcome:?}"),
    };
    assert_eq!(content.handle(), handle);
    assert_eq!(content.page().resources().defining_object(), object_ref(3));
}

#[test]
fn invalid_context_is_rejected_before_any_source_read() {
    let fixture = two_stream_fixture(0xed);
    let prepared = prepare(&fixture, 13_201);
    let page = materialize_ready(&prepared, 13_221);
    let repeated = ResumeCheckpoint::new(13_242);
    let error = match prepared.authority.acquire_page_content(
        &prepared.index,
        page,
        PageContentJobContext::new(
            JobId::new(13_241),
            repeated,
            repeated,
            ResumeCheckpoint::new(13_243),
            RequestPriority::VisiblePage,
        ),
        PageContentLimits::default(),
    ) {
        Ok(_) => panic!("duplicate checkpoints must fail construction"),
        Err(error) => error,
    };
    assert_eq!(
        error.code(),
        DocumentErrorCode::InvalidPageContentJobContext
    );
}
