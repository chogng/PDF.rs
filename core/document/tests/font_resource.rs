#[allow(dead_code)]
#[path = "../../font/tests/support/mod.rs"]
mod font_support;

use std::sync::Arc;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AcquiredFontResource, AttestRevisionJob, CandidateRevisionIndex, DocumentCancellation,
    DocumentError, DocumentErrorCode, DocumentLimitKind, DocumentLimits, FontResourceJobContext,
    FontResourceLimitConfig, FontResourceLimits, FontResourcePhase, FontResourcePoll,
    FontResourceUnsupportedKind, MaterializedPage, NeverCancelled as DocumentNeverCancelled,
    PageFontLookupLimitConfig, PageFontLookupLimits, PageFontLookupOutcome, PageFontReference,
    PageIndexBuildPoll, PageIndexLimits, PageLookupPoll, PageMaterializationJobContext,
    PageMaterializationLimits, PageMaterializationPoll, PageTreeJobContext, PageTreeLimitConfig,
    PageTreeLimits, RevisionAttestationJobContext, RevisionAttestationLimits,
    RevisionAttestationPoll, RevisionId, SharedAttestedRevisionIndex,
};
use pdf_rs_filters::{DecodeLimitConfig, DecodeLimits};
use pdf_rs_font::{
    FontLimit, FontLimitConfig, FontLimitKind, FontLimits, FontParseOutcome, FontProfile,
    FontProgram, FontUnsupportedKind, NeverCancelled as FontNeverCancelled, parse_truetype,
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

fn direct_object(number: u32, value: &[u8]) -> Vec<u8> {
    let mut body = format!("{number} 0 obj\n").into_bytes();
    body.extend_from_slice(value);
    body.extend_from_slice(b"\nendobj\n");
    body
}

fn stream_body(number: u32, dictionary: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut body = format!("{number} 0 obj\n<< ").into_bytes();
    body.extend_from_slice(dictionary);
    body.extend_from_slice(format!(" /Length {} >>\nstream\n", payload.len()).as_bytes());
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\nendstream\nendobj\n");
    body
}

fn widths(first: u8, last: u8, ascii_a: u32) -> String {
    (first..=last)
        .map(|code| if code == b'A' { ascii_a } else { 600 })
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn cff_index(items: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(items.len() as u16).to_be_bytes());
    if items.is_empty() {
        return bytes;
    }
    bytes.push(1);
    let mut offset = 1_u8;
    bytes.push(offset);
    for item in items {
        offset = offset.checked_add(item.len() as u8).unwrap();
        bytes.push(offset);
    }
    for item in items {
        bytes.extend_from_slice(item);
    }
    bytes
}

fn cff_dict_integer(bytes: &mut Vec<u8>, value: usize) {
    bytes.push(29);
    bytes.extend_from_slice(&(value as i32).to_be_bytes());
}

fn type2_integer(bytes: &mut Vec<u8>, value: i16) {
    match value {
        -107..=107 => bytes.push((value + 139) as u8),
        108..=1_131 => {
            let adjusted = value - 108;
            bytes.push((247 + adjusted / 256) as u8);
            bytes.push((adjusted % 256) as u8);
        }
        -1_131..=-108 => {
            let adjusted = -value - 108;
            bytes.push((251 + adjusted / 256) as u8);
            bytes.push((adjusted % 256) as u8);
        }
        _ => {
            bytes.push(28);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn foundational_cff() -> Vec<u8> {
    let name = cff_index(&[b"DocumentCff".to_vec()]);
    let strings = cff_index(&[]);
    let global_subrs = cff_index(&[]);
    let charset = vec![0, 0, 34, 0, 200];

    let notdef = vec![14];
    let mut letter_a = Vec::new();
    type2_integer(&mut letter_a, 500);
    type2_integer(&mut letter_a, 100);
    type2_integer(&mut letter_a, 100);
    letter_a.push(21);
    for value in [300, 700, 300, -700, -600, 0] {
        type2_integer(&mut letter_a, value);
    }
    letter_a.extend_from_slice(&[5, 14]);
    let mut aacute = Vec::new();
    type2_integer(&mut aacute, 500);
    type2_integer(&mut aacute, 100);
    type2_integer(&mut aacute, 100);
    aacute.push(21);
    for value in [300, 700, 300, -700, -600, 0] {
        type2_integer(&mut aacute, value);
    }
    aacute.extend_from_slice(&[5, 14]);
    let charstrings = cff_index(&[notdef, letter_a, aacute]);

    let top_len = 12_usize;
    let top_index_len = 2 + 1 + 2 + top_len;
    let prefix = 4 + name.len() + top_index_len + strings.len() + global_subrs.len();
    let charset_offset = prefix;
    let charstrings_offset = charset_offset + charset.len();
    let mut top = Vec::new();
    cff_dict_integer(&mut top, charset_offset);
    top.push(15);
    cff_dict_integer(&mut top, charstrings_offset);
    top.push(17);
    assert_eq!(top.len(), top_len);

    let mut bytes = vec![1, 0, 4, 4];
    bytes.extend_from_slice(&name);
    bytes.extend_from_slice(&cff_index(&[top]));
    bytes.extend_from_slice(&strings);
    bytes.extend_from_slice(&global_subrs);
    bytes.extend_from_slice(&charset);
    bytes.extend_from_slice(&charstrings);
    bytes
}

fn font_dictionary(descriptor: &str) -> Vec<u8> {
    format!(
        "<< /Type /Font /Subtype /TrueType /Encoding /WinAnsiEncoding \
         /FirstChar 32 /LastChar 126 /Widths [{}] /FontDescriptor {descriptor} >>",
        widths(32, 126, 777)
    )
    .into_bytes()
}

fn valid_font_fixture(program: &[u8], flate: bool, direct_descriptor: bool, salt: u8) -> Fixture {
    let encoded = if flate {
        zlib_stored(program)
    } else {
        program.to_vec()
    };
    let descriptor = if direct_descriptor {
        "<< /Type /FontDescriptor /FontFile2 6 0 R >>"
    } else {
        "5 0 R"
    };
    let font = direct_object(4, &font_dictionary(descriptor));
    let mut extras = vec![(4, font)];
    if !direct_descriptor {
        extras.push((
            5,
            direct_object(5, b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
        ));
    }
    let program_dictionary = if flate {
        format!(
            "/Length1 {} /Filter /FlateDecode /DecodeParms << >>",
            program.len()
        )
    } else {
        format!("/Length1 {}", program.len())
    };
    extras.push((6, stream_body(6, program_dictionary.as_bytes(), &encoded)));
    resource_fixture(b"<< /Font << /F0 4 0 R >> >>", extras, 7, salt)
}

fn identity_h_cidfont_fixture(program: &[u8], salt: u8) -> Fixture {
    resource_fixture(
        b"<< /Font << /F0 4 0 R >> >>",
        vec![
            (
                4,
                direct_object(
                    4,
                    b"<< /Type /Font /Subtype /Type0 /Encoding /Identity-H \
                       /DescendantFonts [5 0 R] >>",
                ),
            ),
            (
                5,
                direct_object(
                    5,
                    b"<< /Type /Font /Subtype /CIDFontType2 /CIDToGIDMap /Identity \
                       /DW 1000 /W [1 [777 778] 3 3 779] /FontDescriptor 6 0 R >>",
                ),
            ),
            (
                6,
                direct_object(6, b"<< /Type /FontDescriptor /FontFile2 7 0 R >>"),
            ),
            (
                7,
                stream_body(7, format!("/Length1 {}", program.len()).as_bytes(), program),
            ),
        ],
        8,
        salt,
    )
}

fn declared_length_font_fixture(
    program: &[u8],
    flate: bool,
    declared_length: usize,
    salt: u8,
) -> Fixture {
    let encoded = if flate {
        zlib_stored(program)
    } else {
        program.to_vec()
    };
    let dictionary = if flate {
        format!("/Length1 {declared_length} /Filter /FlateDecode")
    } else {
        format!("/Length1 {declared_length}")
    };
    custom_font_fixture(
        &font_dictionary("5 0 R"),
        Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
        Some(stream_body(6, dictionary.as_bytes(), &encoded)),
        salt,
    )
}

fn custom_font_fixture(
    font_value: &[u8],
    descriptor_value: Option<&[u8]>,
    program_body: Option<Vec<u8>>,
    salt: u8,
) -> Fixture {
    let mut extras = vec![(4, direct_object(4, font_value))];
    if let Some(value) = descriptor_value {
        extras.push((5, direct_object(5, value)));
    }
    if let Some(body) = program_body {
        extras.push((6, body));
    }
    resource_fixture(b"<< /Font << /F0 4 0 R >> >>", extras, 7, salt)
}

fn zlib_stored(input: &[u8]) -> Vec<u8> {
    let mut output = vec![0x78, 0x01];
    if input.is_empty() {
        output.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    } else {
        let mut position = 0;
        while position < input.len() {
            let remaining = input.len() - position;
            let length = remaining.min(usize::from(u16::MAX));
            let final_block = position + length == input.len();
            output.push(u8::from(final_block));
            let length = u16::try_from(length).expect("stored block fits u16");
            output.extend_from_slice(&length.to_le_bytes());
            output.extend_from_slice(&(!length).to_le_bytes());
            output.extend_from_slice(&input[position..position + usize::from(length)]);
            position += usize::from(length);
        }
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

fn two_contour_glyph() -> Vec<u8> {
    let mut glyph = Vec::new();
    for value in [2_i16, 0, 0, 100, 100] {
        glyph.extend_from_slice(&value.to_be_bytes());
    }
    glyph.extend_from_slice(&2_u16.to_be_bytes());
    glyph.extend_from_slice(&5_u16.to_be_bytes());
    glyph.extend_from_slice(&0_u16.to_be_bytes());
    glyph.extend_from_slice(&[0x01; 6]);
    for value in [0_i16, 100, -100, 10, 80, -80, 0, 0, 100, -90, 0, 80] {
        glyph.extend_from_slice(&value.to_be_bytes());
    }
    glyph
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
            JobId::new(18_101),
            ResumeCheckpoint::new(18_102),
            ResumeCheckpoint::new(18_103),
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
            JobId::new(18_201),
            ResumeCheckpoint::new(18_202),
            ResumeCheckpoint::new(18_203),
            ResumeCheckpoint::new(18_204),
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

fn font_context(seed: u64) -> FontResourceJobContext {
    FontResourceJobContext::new(
        JobId::new(seed),
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        ResumeCheckpoint::new(seed + 3),
        ResumeCheckpoint::new(seed + 4),
        ResumeCheckpoint::new(seed + 5),
        ResumeCheckpoint::new(seed + 6),
        ResumeCheckpoint::new(seed + 7),
        ResumeCheckpoint::new(seed + 8),
        ResumeCheckpoint::new(seed + 9),
        ResumeCheckpoint::new(seed + 10),
        ResumeCheckpoint::new(seed + 11),
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

fn lookup_font(prepared: &Prepared) -> PageFontReference {
    let mut resolver = prepared
        .page
        .resources()
        .font_resolver(PageFontLookupLimits::default());
    match resolver
        .lookup_font(
            b"F0",
            &PanicSource(prepared.store.snapshot()),
            &DocumentNeverCancelled,
        )
        .expect("valid Page Font lookup")
    {
        PageFontLookupOutcome::Ready(proof) => proof,
        PageFontLookupOutcome::Unsupported(value) => {
            panic!("registered indirect font expected: {value:?}")
        }
    }
}

fn acquire_ready(
    prepared: &Prepared,
    limits: FontResourceLimits,
    seed: u64,
) -> Arc<AcquiredFontResource> {
    let mut job = prepared
        .authority
        .acquire_font_resource(lookup_font(prepared), font_context(seed), limits)
        .expect("valid Font resource job");
    match job.poll(&prepared.store, &DocumentNeverCancelled) {
        FontResourcePoll::Ready(font) => font,
        FontResourcePoll::Pending { .. } => panic!("complete font source must not suspend"),
        FontResourcePoll::Unsupported(value) => {
            panic!("registered font must be supported: {value:?}")
        }
        FontResourcePoll::Failed(error) => panic!("registered font must acquire: {error:?}"),
    }
}

fn acquire_terminal(
    prepared: &Prepared,
    limits: FontResourceLimits,
    seed: u64,
) -> FontResourcePoll {
    let mut job = prepared
        .authority
        .acquire_font_resource(lookup_font(prepared), font_context(seed), limits)
        .expect("valid Font resource job");
    job.poll(&prepared.store, &DocumentNeverCancelled)
}

fn acquire_failure(prepared: &Prepared, limits: FontResourceLimits, seed: u64) -> DocumentError {
    match acquire_terminal(prepared, limits, seed) {
        FontResourcePoll::Failed(error) => error,
        other => panic!("structured Font failure expected, got {other:?}"),
    }
}

fn acquire_unsupported(prepared: &Prepared, seed: u64) -> pdf_rs_document::FontResourceUnsupported {
    match acquire_terminal(prepared, FontResourceLimits::default(), seed) {
        FontResourcePoll::Unsupported(value) => value,
        other => panic!("typed Font capability expected, got {other:?}"),
    }
}

fn offset_of(bytes: &[u8], needle: &[u8]) -> u64 {
    let offset = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("fixture contains marker");
    u64::try_from(offset).expect("fixture offset fits u64")
}

fn lower_font_limit(program: &[u8], limits: FontLimits) -> FontLimit {
    match parse_truetype(
        program,
        FontProfile::SimpleTrueTypeWinAnsiV1,
        limits,
        &FontNeverCancelled,
    )
    .into_outcome()
    {
        FontParseOutcome::Failed(error) => error.limit().expect("lower failure is a limit"),
        outcome => panic!("lower parser limit failure expected, got {outcome:?}"),
    }
}

fn exact_font_config(program: &[u8], font: &AcquiredFontResource) -> FontLimitConfig {
    let stats = font.font().stats();
    let largest_glyph = (0..font.font().glyph_count())
        .map(|glyph| font_support::glyph_range(program, glyph).len() as u64)
        .max()
        .expect("fixture has glyphs");
    FontLimitConfig {
        max_input_bytes: program.len() as u64,
        max_tables: 7,
        max_glyphs: u32::from(font.font().glyph_count()),
        max_cmap_segments: stats.cmap_segments() as u32,
        max_glyph_data_bytes: stats.glyph_data_bytes(),
        max_glyph_bytes: largest_glyph,
        max_glyph_contours: 1,
        max_total_contours: stats.source_contours(),
        max_glyph_points: 3,
        max_total_points: stats.source_points(),
        max_components: stats.components(),
        max_component_depth: 1,
        max_path_segments: stats.path_segments(),
        max_retained_bytes: stats.peak_retained_bytes(),
        max_fuel: stats.fuel(),
        cancellation_check_interval_fuel: 1,
    }
}

fn retained_font_prefix(font: &AcquiredFontResource) -> u64 {
    let object_bytes = font
        .font_object()
        .syntax_heap_bytes()
        .checked_add(
            font.descriptor_object()
                .map_or(0, |object| object.syntax_heap_bytes()),
        )
        .and_then(|value| value.checked_add(font.program_object().syntax_heap_bytes()))
        .expect("fixture object accounting fits");
    object_bytes
        .checked_add(
            font.decoded_program()
                .attestation()
                .plan_retained_heap_bytes(),
        )
        .and_then(|value| {
            value.checked_add(
                font.decoded_program()
                    .attestation()
                    .peak_retained_capacity_bytes(),
            )
        })
        .expect("fixture retained accounting fits")
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

struct CheckpointMissingSource<'a> {
    complete: &'a RangeStore,
    missing: &'a RangeStore,
    blocked: ResumeCheckpoint,
}

impl ByteSource for CheckpointMissingSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.complete.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        if request.checkpoint() == self.blocked {
            self.missing.poll(request)
        } else {
            self.complete.poll(request)
        }
    }
}

#[test]
fn direct_lookup_and_identity_acquisition_preserve_pdf_metrics_proof_and_replay() {
    let program = font_support::foundational_font();
    let fixture = valid_font_fixture(&program, false, false, 0xb1);
    let prepared = prepare(&fixture, 18_301);
    let mut resolver = prepared
        .page
        .resources()
        .font_resolver(PageFontLookupLimits::default());
    let proof = match resolver
        .lookup_font(
            b"F0",
            &PanicSource(fixture.snapshot),
            &DocumentNeverCancelled,
        )
        .expect("direct Font dictionary resolves")
    {
        PageFontLookupOutcome::Ready(proof) => proof,
        PageFontLookupOutcome::Unsupported(value) => {
            panic!("indirect selected Font is registered: {value:?}")
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
    assert_eq!(proof.font_key_offset(), offset_of(&fixture.bytes, b"/Font"));
    assert_eq!(proof.entry_key_offset(), offset_of(&fixture.bytes, b"/F0"));
    assert_eq!(resolver.stats().lookups(), 1);
    assert_eq!(resolver.stats().entry_visits(), 2);

    let mut job = prepared
        .authority
        .acquire_font_resource(proof, font_context(18_341), FontResourceLimits::default())
        .expect("valid Font job");
    assert_eq!(job.phase(), FontResourcePhase::Font);
    let ready = match job.poll(&prepared.store, &DocumentNeverCancelled) {
        FontResourcePoll::Ready(font) => font,
        other => panic!("identity font must be ready, got {other:?}"),
    };
    assert_eq!(job.phase(), FontResourcePhase::Ready);
    assert_eq!(ready.proof(), proof);
    assert_eq!(ready.reference(), object_ref(4));
    assert_eq!(
        ready.descriptor_object().map(|value| value.reference()),
        Some(object_ref(5))
    );
    assert_eq!(ready.program_object().reference(), object_ref(6));
    assert_eq!(ready.first_char(), Some(32));
    assert_eq!(ready.last_char(), Some(126));
    assert_eq!(ready.pdf_width_for_winansi(b'A'), Some(777));
    assert_eq!(ready.pdf_width_for_winansi(0x1f), None);
    let glyph = ready.font().glyph_id_for_winansi(b'A').unwrap();
    assert_eq!(ready.font().advance_width(glyph), Some(501));
    assert_eq!(ready.decoded_program().bytes(), program);
    assert!(
        ready
            .decoded_program()
            .attestation()
            .filter_plan()
            .is_empty()
    );
    assert_eq!(ready.stats().objects(), 3);
    assert_eq!(ready.stats().reference_edges(), 2);
    assert_eq!(ready.stats().metadata_entries(), 11);
    assert_eq!(ready.stats().widths(), 95);
    assert_eq!(ready.stats().encoded_bytes(), program.len() as u64);
    assert_eq!(ready.stats().decoded_bytes(), program.len() as u64);
    assert!(ready.stats().decode_fuel() > 0);
    assert_eq!(ready.stats().font(), ready.font().stats());
    assert_eq!(ready.stats().font().input_bytes(), program.len() as u64);
    assert!(ready.stats().retained_bytes() > 0);
    assert!(ready.stats().peak_retained_bytes() >= ready.stats().retained_bytes());

    let replay = match job.poll(&PanicSource(fixture.snapshot), &Cancelled) {
        FontResourcePoll::Ready(font) => font,
        other => panic!("terminal Ready must replay without runtime work: {other:?}"),
    };
    assert!(Arc::ptr_eq(&ready, &replay));
}

#[test]
fn identity_h_cidfonttype2_acquisition_preserves_two_byte_codes_widths_and_proofs() {
    let program = font_support::foundational_font();
    let fixture = identity_h_cidfont_fixture(&program, 0xe7);
    let prepared = prepare(&fixture, 18_451);
    let context = font_context(18_491);
    let missing = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let source = CheckpointMissingSource {
        complete: &prepared.store,
        missing: &missing,
        blocked: context.descendant_envelope_checkpoint(),
    };
    let mut job = prepared
        .authority
        .acquire_font_resource(
            lookup_font(&prepared),
            context,
            FontResourceLimits::default(),
        )
        .unwrap();
    match job.poll(&source, &DocumentNeverCancelled) {
        FontResourcePoll::Pending { checkpoint, .. } => {
            assert_eq!(checkpoint, context.descendant_envelope_checkpoint())
        }
        other => panic!("Type0 descendant checkpoint must suspend: {other:?}"),
    }
    let ready = match job.poll(&prepared.store, &DocumentNeverCancelled) {
        FontResourcePoll::Ready(font) => font,
        other => panic!("Identity-H CIDFontType2 must resume to Ready: {other:?}"),
    };

    assert!(ready.uses_identity_h());
    assert_eq!(ready.first_char(), None);
    assert_eq!(ready.last_char(), None);
    assert_eq!(
        ready.descendant_object().map(|object| object.reference()),
        Some(object_ref(5))
    );
    assert_eq!(
        ready.descriptor_object().map(|object| object.reference()),
        Some(object_ref(6))
    );
    assert_eq!(ready.program_object().reference(), object_ref(7));
    assert_eq!(ready.font().profile(), FontProfile::CidFontType2IdentityV1);
    assert_eq!(ready.character_code_count(&[0, 1, 0, 2, 0, 3]), Some(3));
    assert_eq!(ready.character_code_count(&[0, 1, 0]), None);
    let mut cursor = 0;
    assert_eq!(
        ready.decode_next_character_code(&[0, 1, 0, 2], &mut cursor),
        Some(1)
    );
    assert_eq!(
        ready.decode_next_character_code(&[0, 1, 0, 2], &mut cursor),
        Some(2)
    );
    assert_eq!(cursor, 4);
    assert_eq!(ready.pdf_width_for_character_code(1), Some(777));
    assert_eq!(ready.pdf_width_for_character_code(2), Some(778));
    assert_eq!(ready.pdf_width_for_character_code(3), Some(779));
    assert_eq!(ready.pdf_width_for_character_code(4), Some(1_000));
    assert_eq!(ready.glyph_id_for_character_code(1).unwrap().get(), 1);
    assert_eq!(ready.glyph_id_for_character_code(3).unwrap().get(), 3);
    assert_eq!(ready.glyph_id_for_character_code(4), None);
    assert_eq!(ready.stats().objects(), 4);
    assert_eq!(ready.stats().reference_edges(), 3);
}

#[test]
fn type1c_fontfile3_acquisition_maps_standard_and_difference_glyph_names() {
    let program = foundational_cff();
    let font = format!(
        "<< /Type /Font /Subtype /Type1 \
         /Encoding << /BaseEncoding /StandardEncoding /Differences [65 /A 97 /aacute] >> \
         /FirstChar 32 /LastChar 126 /Widths [{}] /FontDescriptor 5 0 R >>",
        widths(32, 126, 777)
    );
    let fixture = custom_font_fixture(
        font.as_bytes(),
        Some(b"<< /Type /FontDescriptor /FontFile3 6 0 R >>"),
        Some(stream_body(6, b"/Subtype /Type1C", &program)),
        0xe1,
    );
    let prepared = prepare(&fixture, 18_501);
    let ready = acquire_ready(&prepared, FontResourceLimits::default(), 18_541);

    let FontProgram::Type1C(cff) = ready.font() else {
        panic!("Type1C program expected");
    };
    assert_eq!(cff.glyph_count(), 3);
    assert_eq!(ready.glyph_id_for_code(b'A').unwrap().get(), 1);
    assert_eq!(ready.glyph_id_for_code(b'a').unwrap().get(), 2);
    assert_eq!(ready.pdf_width_for_winansi(b'A'), Some(777));
    assert_eq!(ready.decoded_program().bytes(), program);
    assert_eq!(ready.font().profile(), FontProfile::SimpleType1CStandardV1);
}

#[test]
fn indirect_winansi_type1_encoding_admits_all_simple_codes_and_retains_its_proof() {
    let program = foundational_cff();
    let font = format!(
        "<< /Type /Font /Subtype /Type1 /Encoding 5 0 R \
         /FirstChar 0 /LastChar 255 /Widths [{}] /FontDescriptor 6 0 R >>",
        widths(0, 255, 777)
    );
    let fixture = resource_fixture(
        b"<< /Font << /F0 4 0 R >> >>",
        vec![
            (4, direct_object(4, font.as_bytes())),
            (
                5,
                direct_object(
                    5,
                    b"<< /BaseEncoding /WinAnsiEncoding \
                       /Differences [25 /A 65 /A 97 /aacute] >>",
                ),
            ),
            (
                6,
                direct_object(6, b"<< /Type /FontDescriptor /FontFile3 7 0 R >>"),
            ),
            (7, stream_body(7, b"/Subtype /Type1C", &program)),
        ],
        8,
        0xe5,
    );
    let prepared = prepare(&fixture, 18_601);
    let context = font_context(18_641);
    let missing = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let source = CheckpointMissingSource {
        complete: &prepared.store,
        missing: &missing,
        blocked: context.encoding_envelope_checkpoint(),
    };
    let mut job = prepared
        .authority
        .acquire_font_resource(
            lookup_font(&prepared),
            context,
            FontResourceLimits::default(),
        )
        .unwrap();
    match job.poll(&source, &DocumentNeverCancelled) {
        FontResourcePoll::Pending { checkpoint, .. } => {
            assert_eq!(checkpoint, context.encoding_envelope_checkpoint())
        }
        other => panic!("indirect Encoding checkpoint must suspend: {other:?}"),
    }
    let ready = match job.poll(&prepared.store, &DocumentNeverCancelled) {
        FontResourcePoll::Ready(font) => font,
        other => panic!("indirect Encoding must resume to Ready: {other:?}"),
    };

    assert_eq!(
        ready.encoding_object().map(|object| object.reference()),
        Some(object_ref(5))
    );
    assert_eq!(ready.first_char(), Some(0));
    assert_eq!(ready.last_char(), Some(255));
    assert_eq!(ready.pdf_width_for_code(25), Some(600));
    assert_eq!(ready.glyph_id_for_code(25).unwrap().get(), 1);
    assert_eq!(ready.glyph_id_for_code(b'A').unwrap().get(), 1);
    assert_eq!(ready.glyph_id_for_code(b'a').unwrap().get(), 2);
    assert_eq!(ready.stats().objects(), 4);
    assert_eq!(ready.stats().reference_edges(), 3);
}

#[test]
fn complete_winansi_acquisition_retains_extended_pdf_widths_and_glyph_mapping() {
    let program = font_support::foundational_font();
    let font = format!(
        "<< /Type /Font /Subtype /TrueType /Encoding /WinAnsiEncoding \
         /FirstChar 32 /LastChar 255 /Widths [{}] /FontDescriptor 5 0 R >>",
        widths(32, 255, 777)
    );
    let fixture = custom_font_fixture(
        font.as_bytes(),
        Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
        Some(stream_body(
            6,
            format!("/Length1 {}", program.len()).as_bytes(),
            &program,
        )),
        0xe4,
    );
    let prepared = prepare(&fixture, 18_351);
    let ready = acquire_ready(&prepared, FontResourceLimits::default(), 18_361);

    assert_eq!(ready.font().profile(), FontProfile::SimpleTrueTypeWinAnsiV1);
    assert_eq!(ready.pdf_width_for_winansi(0x80), Some(600));
    assert_eq!(ready.pdf_width_for_winansi(0xff), Some(600));
    assert_eq!(
        ready.font().glyph_id_for_winansi(0x80),
        Some(pdf_rs_font::GlyphId::new(0))
    );
    assert_eq!(ready.stats().widths(), 224);
}

#[test]
fn direct_descriptor_and_single_flate_program_are_registered() {
    let program = font_support::foundational_font();
    let fixture = valid_font_fixture(&program, true, true, 0xb2);
    let prepared = prepare(&fixture, 18_401);
    let ready = acquire_ready(&prepared, FontResourceLimits::default(), 18_441);
    assert!(ready.descriptor_object().is_none());
    assert_eq!(ready.stats().objects(), 2);
    assert_eq!(ready.stats().reference_edges(), 1);
    assert_eq!(ready.decoded_program().bytes(), program);
    assert_eq!(
        ready
            .decoded_program()
            .attestation()
            .filter_plan()
            .filters(),
        &[pdf_rs_filters::StreamFilter::FlateDecode]
    );
}

#[test]
fn lookup_profiles_are_bounded_and_direct_or_indirect_categories_are_typed() {
    let direct = resource_fixture(
        b"<< /Font << /F0 << /Type /Font >> >> >>",
        Vec::new(),
        4,
        0xb3,
    );
    let prepared = prepare(&direct, 18_501);
    let mut resolver = prepared
        .page
        .resources()
        .font_resolver(PageFontLookupLimits::default());
    match resolver
        .lookup_font(
            b"F0",
            &PanicSource(direct.snapshot),
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        PageFontLookupOutcome::Unsupported(value) => {
            assert_eq!(value.kind(), FontResourceUnsupportedKind::DirectFont);
            assert_eq!(value.reference(), object_ref(3));
        }
        outcome => panic!("direct selected Font must be typed Unsupported: {outcome:?}"),
    }

    let indirect = resource_fixture(b"<< /Font 7 0 R >>", Vec::new(), 4, 0xb4);
    let prepared = prepare(&indirect, 18_601);
    let mut resolver = prepared
        .page
        .resources()
        .font_resolver(PageFontLookupLimits::default());
    match resolver
        .lookup_font(
            b"F0",
            &PanicSource(indirect.snapshot),
            &DocumentNeverCancelled,
        )
        .unwrap()
    {
        PageFontLookupOutcome::Unsupported(value) => {
            assert_eq!(
                value.kind(),
                FontResourceUnsupportedKind::IndirectFontDictionary
            );
            assert_eq!(value.reference(), object_ref(7));
        }
        outcome => panic!("indirect Font category must be typed Unsupported: {outcome:?}"),
    }

    let program = font_support::foundational_font();
    let fixture = valid_font_fixture(&program, false, false, 0xb5);
    let prepared = prepare(&fixture, 18_701);
    let mut resolver = prepared.page.resources().font_resolver(
        PageFontLookupLimits::validate(PageFontLookupLimitConfig {
            max_lookups: 1,
            max_entry_visits: 1,
        })
        .unwrap(),
    );
    let error = resolver
        .lookup_font(
            b"F0",
            &PanicSource(fixture.snapshot),
            &DocumentNeverCancelled,
        )
        .expect_err("outer entry leaves no selected-entry visit budget");
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        pdf_rs_document::DocumentLimitKind::PageFontEntryVisits
    );
}

#[test]
fn unsupported_pdf_and_truetype_capabilities_are_typed_before_publication() {
    let font_alias = custom_font_fixture(b"5 0 R", None, None, 0xca);
    let prepared = prepare(&font_alias, 18_741);
    assert_eq!(
        acquire_unsupported(&prepared, 18_781).kind(),
        FontResourceUnsupportedKind::FontAlias
    );

    let non_truetype = custom_font_fixture(b"<< /Type /Font /Subtype /Type3 >>", None, None, 0xb6);
    let prepared = prepare(&non_truetype, 18_801);
    assert_eq!(
        acquire_unsupported(&prepared, 18_841).kind(),
        FontResourceUnsupportedKind::NonTrueType
    );

    let encoding = custom_font_fixture(
        b"<< /Type /Font /Subtype /TrueType /Encoding /MacRomanEncoding >>",
        None,
        None,
        0xb7,
    );
    let prepared = prepare(&encoding, 18_901);
    assert_eq!(
        acquire_unsupported(&prepared, 18_941).kind(),
        FontResourceUnsupportedKind::UnsupportedEncoding
    );

    let mut real_widths = widths(32, 126, 777);
    real_widths.replace_range(..3, "1.5");
    let font = format!(
        "<< /Type /Font /Subtype /TrueType /Encoding /WinAnsiEncoding \
         /FirstChar 32 /LastChar 126 /Widths [{real_widths}] /FontDescriptor 5 0 R >>"
    );
    let widths_fixture = custom_font_fixture(font.as_bytes(), None, None, 0xb8);
    let prepared = prepare(&widths_fixture, 19_001);
    assert_eq!(
        acquire_unsupported(&prepared, 19_041).kind(),
        FontResourceUnsupportedKind::UnsupportedWidths
    );

    let missing_program = custom_font_fixture(
        format!(
            "<< /Type /Font /Subtype /TrueType /Encoding /WinAnsiEncoding \
             /FirstChar 32 /LastChar 126 /Widths [{}] >>",
            widths(32, 126, 777)
        )
        .as_bytes(),
        None,
        None,
        0xb9,
    );
    let prepared = prepare(&missing_program, 19_101);
    assert_eq!(
        acquire_unsupported(&prepared, 19_141).kind(),
        FontResourceUnsupportedKind::MissingEmbeddedProgram
    );

    let program = font_support::foundational_font();
    let descriptor_alias = custom_font_fixture(
        &font_dictionary("5 0 R"),
        Some(b"6 0 R"),
        Some(stream_body(
            6,
            format!("/Length1 {}", program.len()).as_bytes(),
            &program,
        )),
        0xcb,
    );
    let prepared = prepare(&descriptor_alias, 19_161);
    assert_eq!(
        acquire_unsupported(&prepared, 19_181).kind(),
        FontResourceUnsupportedKind::FontDescriptorAlias
    );

    let program_alias = custom_font_fixture(
        &font_dictionary("5 0 R"),
        Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
        Some(direct_object(6, b"5 0 R")),
        0xcc,
    );
    let prepared = prepare(&program_alias, 19_181);
    assert_eq!(
        acquire_unsupported(&prepared, 19_191).kind(),
        FontResourceUnsupportedKind::FontFileAlias
    );

    let unsupported_filter = custom_font_fixture(
        &font_dictionary("5 0 R"),
        Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
        Some(stream_body(
            6,
            format!("/Length1 {} /Filter /ASCIIHexDecode", program.len()).as_bytes(),
            b"00>",
        )),
        0xba,
    );
    let prepared = prepare(&unsupported_filter, 19_201);
    assert_eq!(
        acquire_unsupported(&prepared, 19_241).kind(),
        FontResourceUnsupportedKind::UnsupportedFilter
    );

    let unsupported_parameters = custom_font_fixture(
        &font_dictionary("5 0 R"),
        Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
        Some(stream_body(
            6,
            format!(
                "/Length1 {} /Filter /FlateDecode /DecodeParms << /Predictor 2 >>",
                program.len()
            )
            .as_bytes(),
            &zlib_stored(&program),
        )),
        0xbb,
    );
    let prepared = prepare(&unsupported_parameters, 19_301);
    assert_eq!(
        acquire_unsupported(&prepared, 19_341).kind(),
        FontResourceUnsupportedKind::UnsupportedDecodeParameters
    );

    let mut unsupported_program = program;
    unsupported_program[..4].copy_from_slice(b"OTTO");
    let lower = valid_font_fixture(&unsupported_program, false, false, 0xbc);
    let prepared = prepare(&lower, 19_401);
    let unsupported = acquire_unsupported(&prepared, 19_441);
    assert_eq!(
        unsupported.kind(),
        FontResourceUnsupportedKind::TrueTypeProgram
    );
    assert_eq!(
        unsupported.font_unsupported().map(|value| value.kind()),
        Some(FontUnsupportedKind::SfntFlavor)
    );
    assert!(
        unsupported
            .diagnostic_id()
            .starts_with("RPE-DOCUMENT-FONT-")
    );
}

#[test]
fn malformed_pdf_metadata_decode_and_font_programs_fail_without_publication() {
    let program = font_support::foundational_font();
    let mut trailing_flate = zlib_stored(&program);
    trailing_flate.push(0);
    let cases = vec![
        (
            custom_font_fixture(
                b"<< /Type /Font /Subtype /TrueType /Subtype /TrueType >>",
                None,
                None,
                0xbd,
            ),
            DocumentErrorCode::DuplicateStructuralKey,
        ),
        (
            custom_font_fixture(
                b"<< /Type /Font /Subtype /TrueType /Encoding /WinAnsiEncoding \
                  /FirstChar 256 /LastChar 256 /Widths [600] /FontDescriptor 5 0 R >>",
                None,
                None,
                0xd2,
            ),
            DocumentErrorCode::InvalidFontResource,
        ),
        (
            custom_font_fixture(
                format!(
                    "<< /Type /Font /Subtype /TrueType /Encoding /WinAnsiEncoding \
                     /FirstChar 32 /LastChar 126 /Widths [{}] /FontDescriptor 5 0 R >>",
                    widths(32, 125, 777)
                )
                .as_bytes(),
                None,
                None,
                0xbe,
            ),
            DocumentErrorCode::InvalidFontResource,
        ),
        (
            custom_font_fixture(
                &font_dictionary("5 0 R"),
                Some(b"<< /Type /FontDescriptor /FontFile 6 0 R /FontFile2 6 0 R >>"),
                None,
                0xd3,
            ),
            DocumentErrorCode::InvalidFontResource,
        ),
        (
            custom_font_fixture(
                &font_dictionary("5 0 R"),
                Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R /FontFile3 6 0 R >>"),
                None,
                0xd4,
            ),
            DocumentErrorCode::InvalidFontResource,
        ),
        (
            custom_font_fixture(
                &font_dictionary("5 0 R"),
                Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
                Some(stream_body(6, b"", &program)),
                0xbf,
            ),
            DocumentErrorCode::InvalidFontResource,
        ),
        (
            custom_font_fixture(
                &font_dictionary("5 0 R"),
                Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
                Some(stream_body(
                    6,
                    format!("/Length1 {}", program.len() + 1).as_bytes(),
                    &program,
                )),
                0xc0,
            ),
            DocumentErrorCode::InvalidFontResource,
        ),
        (
            custom_font_fixture(
                &font_dictionary("5 0 R"),
                Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
                Some(stream_body(
                    6,
                    b"/Length1 12",
                    &[0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                )),
                0xc1,
            ),
            DocumentErrorCode::FontProgramFailure,
        ),
        (
            custom_font_fixture(
                &font_dictionary("5 0 R"),
                Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
                Some(stream_body(
                    6,
                    format!("/Length1 {} /Filter /FlateDecode", program.len()).as_bytes(),
                    b"not-zlib",
                )),
                0xc2,
            ),
            DocumentErrorCode::FontResourceDecodeFailure,
        ),
        (
            custom_font_fixture(
                &font_dictionary("5 0 R"),
                Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
                Some(stream_body(
                    6,
                    format!("/Length1 {} /Filter /FlateDecode", program.len()).as_bytes(),
                    &trailing_flate,
                )),
                0xd7,
            ),
            DocumentErrorCode::FontResourceDecodeFailure,
        ),
    ];
    for (index, (fixture, expected)) in cases.into_iter().enumerate() {
        let seed = 19_501 + u64::try_from(index).unwrap() * 100;
        let prepared = prepare(&fixture, seed);
        let error = acquire_failure(&prepared, FontResourceLimits::default(), seed + 40);
        assert_eq!(error.code(), expected, "case {index}");
    }
}

#[test]
fn low_length1_identity_and_flate_commit_decode_stats_before_stable_failure() {
    let program = font_support::foundational_font();
    for (index, flate) in [false, true].into_iter().enumerate() {
        let index = u64::try_from(index).unwrap();
        let baseline_fixture = declared_length_font_fixture(
            &program,
            flate,
            program.len(),
            0xde + u8::try_from(index).unwrap(),
        );
        let baseline_prepared = prepare(&baseline_fixture, 20_001 + index * 100);
        let baseline = acquire_ready(
            &baseline_prepared,
            FontResourceLimits::default(),
            20_041 + index * 100,
        );
        let expected_fuel = baseline.decoded_program().attestation().fuel_consumed();
        let expected_decode_peak = retained_font_prefix(&baseline);
        assert!(expected_fuel > 0);

        let low_fixture = declared_length_font_fixture(
            &program,
            flate,
            program.len() - 1,
            0xe0 + u8::try_from(index).unwrap(),
        );
        let low_prepared = prepare(&low_fixture, 20_201 + index * 100);
        let decode_only_font_limits = FontLimits::validate(FontLimitConfig {
            max_retained_bytes: 1,
            ..FontLimitConfig::default()
        })
        .unwrap();
        let low_limits = FontResourceLimits::validate(FontResourceLimitConfig {
            font_limits: decode_only_font_limits,
            ..FontResourceLimitConfig::default()
        })
        .unwrap();
        let mut job = low_prepared
            .authority
            .acquire_font_resource(
                lookup_font(&low_prepared),
                font_context(20_241 + index * 100),
                low_limits,
            )
            .unwrap();
        let first_error = match job.poll(&low_prepared.store, &DocumentNeverCancelled) {
            FontResourcePoll::Failed(error) => error,
            other => panic!("low Length1 must fail without publication: {other:?}"),
        };
        assert_eq!(first_error.code(), DocumentErrorCode::InvalidFontResource);
        assert_eq!(job.phase(), FontResourcePhase::Failed);
        let failed_stats = job.stats();
        assert_eq!(failed_stats.decoded_bytes(), program.len() as u64);
        assert_eq!(failed_stats.decode_fuel(), expected_fuel);
        assert!(failed_stats.peak_retained_bytes() >= expected_decode_peak);
        assert_eq!(failed_stats.font().input_bytes(), 0);
        assert_eq!(failed_stats.retained_bytes(), 0);

        let replay_error =
            match job.poll(&PanicSource(low_fixture.snapshot), &DocumentNeverCancelled) {
                FontResourcePoll::Failed(error) => error,
                other => panic!("low Length1 terminal failure must replay: {other:?}"),
            };
        assert_eq!(replay_error, first_error);
        assert_eq!(job.stats(), failed_stats);
    }
}

#[test]
fn every_reachable_object_and_payload_checkpoint_suspends_exactly_and_resumes() {
    let program = font_support::foundational_font();
    let fixture = valid_font_fixture(&program, false, false, 0xc3);
    let prepared = prepare(&fixture, 20_201);
    let context = font_context(20_241);
    let checkpoints = [
        context.font_envelope_checkpoint(),
        context.descriptor_envelope_checkpoint(),
        context.program_envelope_checkpoint(),
        context.payload_checkpoint(),
    ];
    for blocked in checkpoints {
        let missing = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
        let source = CheckpointMissingSource {
            complete: &prepared.store,
            missing: &missing,
            blocked,
        };
        let mut job = prepared
            .authority
            .acquire_font_resource(
                lookup_font(&prepared),
                context,
                FontResourceLimits::default(),
            )
            .expect("valid staged Font job");
        match job.poll(&source, &DocumentNeverCancelled) {
            FontResourcePoll::Pending { checkpoint, .. } => assert_eq!(checkpoint, blocked),
            other => panic!("blocked checkpoint {blocked:?} must suspend: {other:?}"),
        }
        match job.poll(&prepared.store, &DocumentNeverCancelled) {
            FontResourcePoll::Ready(font) => {
                assert_eq!(font.decoded_program().bytes(), program);
                assert_eq!(font.stats().polls(), 2);
            }
            other => panic!("completed source must resume Font acquisition: {other:?}"),
        }
    }

    let mut padded = program.clone();
    padded.resize(5_000, 0);
    let fixture = valid_font_fixture(&padded, false, false, 0xc9);
    let prepared = prepare(&fixture, 20_501);
    let context = font_context(20_541);
    let missing = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let source = CheckpointMissingSource {
        complete: &prepared.store,
        missing: &missing,
        blocked: context.program_boundary_checkpoint(),
    };
    let mut job = prepared
        .authority
        .acquire_font_resource(
            lookup_font(&prepared),
            context,
            FontResourceLimits::default(),
        )
        .unwrap();
    match job.poll(&source, &DocumentNeverCancelled) {
        FontResourcePoll::Pending { checkpoint, .. } => {
            assert_eq!(checkpoint, context.program_boundary_checkpoint())
        }
        other => panic!("large FontFile2 must suspend at its boundary checkpoint: {other:?}"),
    }
    match job.poll(&prepared.store, &DocumentNeverCancelled) {
        FontResourcePoll::Ready(font) => assert_eq!(font.decoded_program().bytes(), padded),
        FontResourcePoll::Failed(error) => panic!(
            "large FontFile2 boundary must resume: {error:?}, limit={:?}",
            error.limit()
        ),
        other => panic!("large FontFile2 boundary must resume: {other:?}"),
    }
}

#[test]
fn malformed_large_font_and_descriptor_objects_suspend_at_boundary_checkpoints() {
    let large_payload = vec![0_u8; 5_000];
    let cases = [
        (
            resource_fixture(
                b"<< /Font << /F0 4 0 R >> >>",
                vec![(4, stream_body(4, b"", &large_payload))],
                5,
                0xd8,
            ),
            ChildBoundary::Font,
        ),
        (
            resource_fixture(
                b"<< /Font << /F0 4 0 R >> >>",
                vec![
                    (4, direct_object(4, &font_dictionary("5 0 R"))),
                    (5, stream_body(5, b"", &large_payload)),
                ],
                6,
                0xd9,
            ),
            ChildBoundary::Descriptor,
        ),
    ];
    for (index, (fixture, boundary)) in cases.into_iter().enumerate() {
        let index = u64::try_from(index).unwrap();
        let prepared = prepare(&fixture, 20_801 + index * 100);
        let context = font_context(20_841 + index * 100);
        let blocked = match boundary {
            ChildBoundary::Font => context.font_boundary_checkpoint(),
            ChildBoundary::Descriptor => context.descriptor_boundary_checkpoint(),
        };
        let missing = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
        let source = CheckpointMissingSource {
            complete: &prepared.store,
            missing: &missing,
            blocked,
        };
        let mut job = prepared
            .authority
            .acquire_font_resource(
                lookup_font(&prepared),
                context,
                FontResourceLimits::default(),
            )
            .unwrap();
        match job.poll(&source, &DocumentNeverCancelled) {
            FontResourcePoll::Pending { checkpoint, .. } => assert_eq!(checkpoint, blocked),
            other => panic!("malformed large object must suspend at {blocked:?}: {other:?}"),
        }
        match job.poll(&prepared.store, &DocumentNeverCancelled) {
            FontResourcePoll::Failed(error) => {
                assert_eq!(error.code(), DocumentErrorCode::InvalidFontResource)
            }
            other => panic!("resumed malformed large object must fail: {other:?}"),
        }
    }
}

#[derive(Clone, Copy)]
enum ChildBoundary {
    Font,
    Descriptor,
}

#[test]
fn constructor_runtime_priority_and_failed_terminal_replay_are_stable() {
    let program = font_support::foundational_font();
    let fixture = valid_font_fixture(&program, false, false, 0xc4);
    let prepared = prepare(&fixture, 21_001);
    let proof = lookup_font(&prepared);
    let duplicate = FontResourceJobContext::new(
        JobId::new(21_041),
        ResumeCheckpoint::new(21_042),
        ResumeCheckpoint::new(21_042),
        ResumeCheckpoint::new(21_043),
        ResumeCheckpoint::new(21_044),
        ResumeCheckpoint::new(21_045),
        ResumeCheckpoint::new(21_046),
        ResumeCheckpoint::new(21_047),
        ResumeCheckpoint::new(21_048),
        ResumeCheckpoint::new(21_049),
        ResumeCheckpoint::new(21_050),
        ResumeCheckpoint::new(21_051),
        RequestPriority::VisiblePage,
    );
    let error = prepared
        .authority
        .acquire_font_resource(proof, duplicate, FontResourceLimits::default())
        .expect_err("all Font checkpoints must be pairwise distinct");
    assert_eq!(
        error.code(),
        DocumentErrorCode::InvalidFontResourceJobContext
    );

    let mut cancelled = prepared
        .authority
        .acquire_font_resource(proof, font_context(21_101), FontResourceLimits::default())
        .unwrap();
    let first = match cancelled.poll(&PanicSource(fixture.snapshot), &Cancelled) {
        FontResourcePoll::Failed(error) => error,
        other => panic!("pre-I/O cancellation must fail: {other:?}"),
    };
    assert_eq!(first.code(), DocumentErrorCode::Cancelled);
    let replay = match cancelled.poll(&PanicSource(fixture.snapshot), &Cancelled) {
        FontResourcePoll::Failed(error) => error,
        other => panic!("terminal failure must replay: {other:?}"),
    };
    assert_eq!(first, replay);

    let changed = snapshot(fixture.snapshot.len().unwrap(), 0x44);
    let mut changed_job = prepared
        .authority
        .acquire_font_resource(proof, font_context(21_201), FontResourceLimits::default())
        .unwrap();
    match changed_job.poll(&PanicSource(changed), &Cancelled) {
        FontResourcePoll::Failed(error) => {
            assert_eq!(error.code(), DocumentErrorCode::SourceSnapshotMismatch)
        }
        other => panic!("source change must precede simultaneous cancellation: {other:?}"),
    }

    let foreign_fixture = valid_font_fixture(&program, false, false, 0xc5);
    let foreign = prepare(&foreign_fixture, 21_301);
    let error = foreign
        .authority
        .acquire_font_resource(proof, font_context(21_341), FontResourceLimits::default())
        .expect_err("lookup proof cannot cross source authorities");
    assert_eq!(
        error.code(),
        DocumentErrorCode::AttestedObjectEvidenceMismatch
    );
}

#[test]
fn indirect_resources_retain_the_terminal_dictionary_owner() {
    let program = font_support::foundational_font();
    let extras = vec![
        (4, direct_object(4, &font_dictionary("5 0 R"))),
        (
            5,
            direct_object(5, b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
        ),
        (
            6,
            stream_body(
                6,
                format!("/Length1 {}", program.len()).as_bytes(),
                &program,
            ),
        ),
        (7, direct_object(7, b"<< /Font << /F0 4 0 R >> >>")),
    ];
    let fixture = resource_fixture(b"7 0 R", extras, 8, 0xcd);
    let prepared = prepare(&fixture, 21_501);
    let proof = lookup_font(&prepared);
    assert_eq!(proof.scope_defining_object(), object_ref(3));
    assert_eq!(proof.resource_dictionary_owner(), object_ref(7));
    assert_eq!(proof.target(), object_ref(4));
    let font = acquire_ready(&prepared, FontResourceLimits::default(), 21_541);
    assert_eq!(font.proof(), proof);
    assert_eq!(font.decoded_program().bytes(), program);
}

#[test]
fn exact_aggregate_limits_pass_and_one_less_boundaries_are_typed() {
    let program = font_support::foundational_font();
    let fixture = valid_font_fixture(&program, false, false, 0xc6);
    let prepared = prepare(&fixture, 22_001);
    let baseline = acquire_ready(&prepared, FontResourceLimits::default(), 22_041);
    let stats = baseline.stats();
    let exact = FontResourceLimitConfig {
        max_polls: stats.polls(),
        max_objects: stats.objects(),
        max_reference_edges: stats.reference_edges(),
        max_metadata_entries: stats.metadata_entries(),
        max_widths: stats.widths(),
        max_object_read_bytes: stats.object_read_bytes(),
        max_object_parse_bytes: stats.object_parse_bytes(),
        max_encoded_bytes: stats.encoded_bytes(),
        max_decoded_bytes: stats.decoded_bytes(),
        max_decode_fuel: stats.decode_fuel(),
        max_retained_bytes: stats.peak_retained_bytes(),
        decode_limits: DecodeLimits::default(),
        font_limits: FontLimits::default(),
    };
    let exact_limits = FontResourceLimits::validate(exact).expect("measured limits validate");
    let exact_font = acquire_ready(&prepared, exact_limits, 22_081);
    assert_eq!(exact_font.stats(), stats);

    for (index, (kind, config)) in [
        (
            DocumentLimitKind::FontResourceObjects,
            FontResourceLimitConfig {
                max_objects: stats.objects() - 1,
                ..exact
            },
        ),
        (
            DocumentLimitKind::FontResourceReferenceEdges,
            FontResourceLimitConfig {
                max_reference_edges: stats.reference_edges() - 1,
                ..exact
            },
        ),
        (
            DocumentLimitKind::FontResourceMetadataEntries,
            FontResourceLimitConfig {
                max_metadata_entries: stats.metadata_entries() - 1,
                ..exact
            },
        ),
        (
            DocumentLimitKind::FontResourceObjectReadBytes,
            FontResourceLimitConfig {
                max_object_read_bytes: stats.object_read_bytes() - 1,
                ..exact
            },
        ),
        (
            DocumentLimitKind::FontResourceObjectParseBytes,
            FontResourceLimitConfig {
                max_object_parse_bytes: stats.object_parse_bytes() - 1,
                ..exact
            },
        ),
        (
            DocumentLimitKind::FontResourceEncodedBytes,
            FontResourceLimitConfig {
                max_encoded_bytes: stats.encoded_bytes() - 1,
                ..exact
            },
        ),
        (
            DocumentLimitKind::FontResourceDecodedBytes,
            FontResourceLimitConfig {
                max_decoded_bytes: stats.decoded_bytes() - 1,
                ..exact
            },
        ),
        (
            DocumentLimitKind::FontResourceDecodeFuel,
            FontResourceLimitConfig {
                max_decode_fuel: stats.decode_fuel() - 1,
                ..exact
            },
        ),
        (
            DocumentLimitKind::FontResourceRetainedBytes,
            FontResourceLimitConfig {
                max_retained_bytes: stats.peak_retained_bytes() - 1,
                ..exact
            },
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let limits = FontResourceLimits::validate(config).expect("positive one-less limit");
        let error = acquire_failure(
            &prepared,
            limits,
            22_101 + u64::try_from(index).unwrap() * 20,
        );
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        let limit = error.limit().unwrap();
        assert_eq!(limit.kind(), kind);
        let (expected_limit, expected_consumed, expected_attempted) = match kind {
            DocumentLimitKind::FontResourceObjects => (stats.objects() - 1, stats.objects() - 1, 1),
            DocumentLimitKind::FontResourceReferenceEdges => {
                (stats.reference_edges() - 1, stats.reference_edges() - 1, 1)
            }
            DocumentLimitKind::FontResourceMetadataEntries => (
                stats.metadata_entries() - 1,
                stats.metadata_entries() - 1,
                1,
            ),
            DocumentLimitKind::FontResourceObjectReadBytes => (
                stats.object_read_bytes() - 1,
                stats.object_read_bytes() - 18,
                18,
            ),
            DocumentLimitKind::FontResourceObjectParseBytes => (
                stats.object_parse_bytes() - 1,
                stats.object_parse_bytes() - 18,
                18,
            ),
            DocumentLimitKind::FontResourceEncodedBytes => {
                (stats.encoded_bytes() - 1, 0, stats.encoded_bytes())
            }
            DocumentLimitKind::FontResourceDecodedBytes => {
                (stats.decoded_bytes() - 1, 0, stats.decoded_bytes())
            }
            DocumentLimitKind::FontResourceDecodeFuel => (
                stats.decode_fuel() - 1,
                stats.decode_fuel() - 1,
                stats.decode_fuel(),
            ),
            DocumentLimitKind::FontResourceRetainedBytes => (
                stats.peak_retained_bytes() - 1,
                stats.peak_retained_bytes() - stats.decoded_bytes(),
                stats.decoded_bytes(),
            ),
            _ => unreachable!("selected aggregate limit kind"),
        };
        assert_eq!(limit.limit(), expected_limit);
        assert_eq!(limit.consumed(), expected_consumed);
        assert_eq!(limit.attempted(), expected_attempted);
    }

    let lower_limits = FontLimits::validate(FontLimitConfig {
        max_fuel: stats.font().fuel() - 1,
        ..FontLimitConfig::default()
    })
    .unwrap();
    let error = acquire_failure(
        &prepared,
        FontResourceLimits::validate(FontResourceLimitConfig {
            font_limits: lower_limits,
            ..exact
        })
        .unwrap(),
        22_401,
    );
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        DocumentLimitKind::FontResourceParserWork
    );
}

#[test]
fn lower_font_limit_dimensions_preserve_exact_typed_evidence() {
    let program = font_support::foundational_font();
    let fixture = valid_font_fixture(&program, false, false, 0xda);
    let prepared = prepare(&fixture, 22_421);
    let baseline = acquire_ready(&prepared, FontResourceLimits::default(), 22_441);
    let exact = exact_font_config(&program, &baseline);
    let retained_prefix = retained_font_prefix(&baseline);
    let cases = [
        (
            FontLimitKind::InputBytes,
            DocumentLimitKind::FontResourceDecodedBytes,
            FontLimitConfig {
                max_input_bytes: exact.max_input_bytes - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::Tables,
            DocumentLimitKind::FontResourceTables,
            FontLimitConfig {
                max_tables: exact.max_tables - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::Glyphs,
            DocumentLimitKind::FontResourceGlyphs,
            FontLimitConfig {
                max_glyphs: exact.max_glyphs - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::CmapSegments,
            DocumentLimitKind::FontResourceCmapSegments,
            FontLimitConfig {
                max_cmap_segments: exact.max_cmap_segments - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::GlyphDataBytes,
            DocumentLimitKind::FontResourceGlyphDataBytes,
            FontLimitConfig {
                max_glyph_data_bytes: exact.max_glyph_data_bytes - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::GlyphBytes,
            DocumentLimitKind::FontResourceGlyphBytes,
            FontLimitConfig {
                max_glyph_bytes: exact.max_glyph_bytes - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::TotalContours,
            DocumentLimitKind::FontResourceTotalContours,
            FontLimitConfig {
                max_total_contours: exact.max_total_contours - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::GlyphPoints,
            DocumentLimitKind::FontResourceGlyphPoints,
            FontLimitConfig {
                max_glyph_points: exact.max_glyph_points - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::TotalPoints,
            DocumentLimitKind::FontResourceTotalPoints,
            FontLimitConfig {
                max_total_points: exact.max_total_points - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::Components,
            DocumentLimitKind::FontResourceComponents,
            FontLimitConfig {
                max_components: exact.max_components - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::PathSegments,
            DocumentLimitKind::FontResourcePathSegments,
            FontLimitConfig {
                max_path_segments: exact.max_path_segments - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::RetainedBytes,
            DocumentLimitKind::FontResourceRetainedBytes,
            FontLimitConfig {
                max_retained_bytes: exact.max_retained_bytes - 1,
                ..exact
            },
        ),
        (
            FontLimitKind::Fuel,
            DocumentLimitKind::FontResourceParserWork,
            FontLimitConfig {
                max_fuel: exact.max_fuel - 1,
                ..exact
            },
        ),
    ];
    for (index, (lower_kind, document_kind, config)) in cases.into_iter().enumerate() {
        let lower_limits = FontLimits::validate(config).expect("one-less lower limits validate");
        let lower = lower_font_limit(&program, lower_limits);
        assert_eq!(lower.kind(), lower_kind);
        let limits = FontResourceLimits::validate(FontResourceLimitConfig {
            font_limits: lower_limits,
            ..FontResourceLimitConfig::default()
        })
        .unwrap();
        let error = acquire_failure(
            &prepared,
            limits,
            22_461 + u64::try_from(index).unwrap() * 20,
        );
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        let mapped = error.limit().expect("mapped document limit evidence");
        assert_eq!(mapped.kind(), document_kind);
        let prefix = if matches!(lower_kind, FontLimitKind::RetainedBytes) {
            retained_prefix
        } else {
            0
        };
        assert_eq!(mapped.limit(), prefix + lower.limit());
        assert_eq!(mapped.consumed(), prefix + lower.consumed());
        assert_eq!(mapped.attempted(), lower.attempted());
    }

    let nested = font_support::build_font(vec![
        Vec::new(),
        font_support::triangle_glyph(),
        font_support::compound_glyph(&[(1, 0, 0)]),
        font_support::compound_glyph(&[(2, 0, 0)]),
    ]);
    let lower_limits = FontLimits::validate(FontLimitConfig {
        max_component_depth: 1,
        ..FontLimitConfig::default()
    })
    .unwrap();
    let lower = lower_font_limit(&nested, lower_limits);
    assert_eq!(lower.kind(), FontLimitKind::ComponentDepth);
    let nested_fixture = valid_font_fixture(&nested, false, false, 0xdb);
    let nested_prepared = prepare(&nested_fixture, 22_721);
    let error = acquire_failure(
        &nested_prepared,
        FontResourceLimits::validate(FontResourceLimitConfig {
            font_limits: lower_limits,
            ..FontResourceLimitConfig::default()
        })
        .unwrap(),
        22_761,
    );
    let mapped = error.limit().expect("mapped component-depth evidence");
    assert_eq!(mapped.kind(), DocumentLimitKind::FontResourceComponentDepth);
    assert_eq!(mapped.limit(), lower.limit());
    assert_eq!(mapped.consumed(), lower.consumed());
    assert_eq!(mapped.attempted(), lower.attempted());

    let two_contours = font_support::build_font(vec![Vec::new(), two_contour_glyph()]);
    let lower_limits = FontLimits::validate(FontLimitConfig {
        max_glyph_contours: 1,
        ..FontLimitConfig::default()
    })
    .unwrap();
    let lower = lower_font_limit(&two_contours, lower_limits);
    assert_eq!(lower.kind(), FontLimitKind::GlyphContours);
    let contour_fixture = valid_font_fixture(&two_contours, false, false, 0xdd);
    let contour_prepared = prepare(&contour_fixture, 22_901);
    let error = acquire_failure(
        &contour_prepared,
        FontResourceLimits::validate(FontResourceLimitConfig {
            font_limits: lower_limits,
            ..FontResourceLimitConfig::default()
        })
        .unwrap(),
        22_941,
    );
    let mapped = error.limit().expect("mapped glyph-contour evidence");
    assert_eq!(mapped.kind(), DocumentLimitKind::FontResourceGlyphContours);
    assert_eq!(mapped.limit(), lower.limit());
    assert_eq!(mapped.consumed(), lower.consumed());
    assert_eq!(mapped.attempted(), lower.attempted());
}

#[test]
fn child_syntax_retained_limit_includes_previously_retained_font_object() {
    let program = font_support::foundational_font();
    let fixture = valid_font_fixture(&program, false, false, 0xdc);
    let prepared = prepare(&fixture, 22_801);
    let baseline = acquire_ready(&prepared, FontResourceLimits::default(), 22_841);
    let retained_prefix = baseline.font_object().syntax_heap_bytes();
    let aggregate_limit = retained_prefix + 1;
    let error = acquire_failure(
        &prepared,
        FontResourceLimits::validate(FontResourceLimitConfig {
            max_retained_bytes: aggregate_limit,
            ..FontResourceLimitConfig::default()
        })
        .unwrap(),
        22_881,
    );
    assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
    let limit = error.limit().expect("mapped child syntax retained limit");
    assert_eq!(limit.kind(), DocumentLimitKind::FontResourceRetainedBytes);
    assert_eq!(limit.limit(), aggregate_limit);
    assert_eq!(limit.consumed(), retained_prefix);
    assert_eq!(limit.attempted(), 704);
}

#[test]
fn widths_and_poll_limits_fail_at_independent_valid_minimums() {
    let program = font_support::foundational_font();
    let wider_font = format!(
        "<< /Type /Font /Subtype /TrueType /Encoding /WinAnsiEncoding \
         /FirstChar 32 /LastChar 127 /Widths [{}] /FontDescriptor 5 0 R >>",
        widths(32, 127, 777)
    );
    let fixture = custom_font_fixture(
        wider_font.as_bytes(),
        Some(b"<< /Type /FontDescriptor /FontFile2 6 0 R >>"),
        Some(stream_body(
            6,
            format!("/Length1 {}", program.len()).as_bytes(),
            &program,
        )),
        0xc7,
    );
    let prepared = prepare(&fixture, 22_501);
    let error = acquire_failure(
        &prepared,
        FontResourceLimits::validate(FontResourceLimitConfig {
            max_widths: 95,
            ..FontResourceLimitConfig::default()
        })
        .unwrap(),
        22_541,
    );
    assert_eq!(
        error.limit().unwrap().kind(),
        DocumentLimitKind::FontResourceWidths
    );

    let normal = valid_font_fixture(&program, false, false, 0xce);
    let prepared = prepare(&normal, 22_571);
    let lower_decode = DecodeLimits::validate(DecodeLimitConfig {
        max_layer_output_bytes: program.len() as u64 - 1,
        max_final_output_bytes: program.len() as u64 - 1,
        ..DecodeLimitConfig::default()
    })
    .unwrap();
    let error = acquire_failure(
        &prepared,
        FontResourceLimits::validate(FontResourceLimitConfig {
            decode_limits: lower_decode,
            ..FontResourceLimitConfig::default()
        })
        .unwrap(),
        22_591,
    );
    assert_eq!(
        error.limit().unwrap().kind(),
        DocumentLimitKind::FontResourceDecodedBytes
    );

    let normal = valid_font_fixture(&program, false, false, 0xc8);
    let prepared = prepare(&normal, 22_601);
    let context = font_context(22_641);
    let missing = RangeStore::new(normal.snapshot, Default::default()).unwrap();
    let source = CheckpointMissingSource {
        complete: &prepared.store,
        missing: &missing,
        blocked: context.payload_checkpoint(),
    };
    let limits = FontResourceLimits::validate(FontResourceLimitConfig {
        max_polls: 1,
        ..FontResourceLimitConfig::default()
    })
    .unwrap();
    let mut job = prepared
        .authority
        .acquire_font_resource(lookup_font(&prepared), context, limits)
        .unwrap();
    assert!(matches!(
        job.poll(&source, &DocumentNeverCancelled),
        FontResourcePoll::Pending { .. }
    ));
    match job.poll(&prepared.store, &DocumentNeverCancelled) {
        FontResourcePoll::Failed(error) => {
            assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
            assert_eq!(
                error.limit().unwrap().kind(),
                DocumentLimitKind::FontResourcePolls
            );
        }
        other => panic!("second admitted poll must exceed the exact limit: {other:?}"),
    }
}
