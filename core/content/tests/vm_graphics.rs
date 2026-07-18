use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[allow(dead_code)]
#[path = "../../font/tests/support/mod.rs"]
mod font_support;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_content::{
    ContentColorSpaceAcquisitionProfile, ContentColorSpaceJobContext,
    ContentExtGStateAcquisitionProfile, ContentExtGStateJobContext, ContentFontLimitConfig,
    ContentFontLimitKind, ContentFontLimits, ContentFontProfile, ContentFontStats, ContentFormPoll,
    ContentFormProfile, ContentGraphicsLimitConfig, ContentGraphicsLimitKind,
    ContentGraphicsLimits, ContentImageLimitConfig, ContentImageLimitKind, ContentImageLimits,
    ContentImageProfile, ContentLimits, ContentUnsupportedKind, ContentVmError, ContentVmErrorCode,
    ContentVmFailure, ContentVmLimitConfig, ContentVmLimitKind, ContentVmLimits, ContentVmPoll,
    InterpretFormJob, InterpretPageJob,
};
use pdf_rs_document::{
    AcquiredPageContent, AttestRevisionJob, CandidateRevisionIndex, DocumentCancellation,
    FontResourceJobContext, FontResourceLimits, FontResourceUnsupportedKind, FormXObjectJobContext,
    FormXObjectPoll, ImageXObjectJobContext, ImageXObjectLimits,
    NeverCancelled as DocumentNeverCancelled, PageColorSpaceLookupLimits, PageContentJobContext,
    PageContentLimits, PageContentPoll, PageExtGStateLookupLimits, PageFontLookupLimits,
    PageIndexBuildPoll, PageIndexLimits, PageLookupPoll, PageMaterializationJobContext,
    PageMaterializationLimits, PageMaterializationPoll, PagePropertyLookupLimits,
    PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits, PageXObjectLookupLimits,
    PageXObjectLookupOutcome, RevisionAttestationJobContext, RevisionAttestationLimits,
    RevisionAttestationPoll, RevisionId, SharedAttestedRevisionIndex,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_scene::{
    BlendMode, DashPatternBuilder, DeviceColor, FillRule, GlyphPainting, GraphicsCommand,
    GraphicsResource, GraphicsSceneLimitConfig, GraphicsSceneLimits, ImageColorSpace, LineCap,
    LineJoin, Matrix, PageGeometry, PageRotation as ScenePageRotation, PathResourceBuilder,
    PathSegment, SceneBinding, SceneRect, SceneScalar, SceneUnit, SceneVersion,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
};

const REVISION_ID: RevisionId = RevisionId::new(92);
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
    authority: SharedAttestedRevisionIndex,
    store: RangeStore,
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

fn fixture(content: &[u8], resources: &[u8], salt: u8) -> Fixture {
    fixture_with_objects(content, resources, &[], salt)
}

fn fixture_with_objects(
    content: &[u8],
    resources: &[u8],
    extra_objects: &[(u32, Vec<u8>)],
    salt: u8,
) -> Fixture {
    let mut page =
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources ".to_vec();
    page.extend_from_slice(resources);
    page.extend_from_slice(b" /Contents 4 0 R >>\nendobj\n");
    let mut stream = format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream\nendobj\n");

    let mut bodies = vec![
        (1_u32, CATALOG.to_vec()),
        (2, PAGE_ROOT.to_vec()),
        (3, page),
        (4, stream),
    ];
    bodies.extend(extra_objects.iter().cloned());
    let size = bodies
        .iter()
        .map(|(number, _)| number.saturating_add(1))
        .max()
        .unwrap_or(1)
        .max(8);
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
        snapshot: snapshot(
            u64::try_from(bytes.len()).expect("fixture length fits"),
            salt,
        ),
        bytes,
    }
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).expect("store");
    let range =
        ByteRange::new(0, u64::try_from(fixture.bytes.len()).expect("length")).expect("range");
    store
        .supply(
            RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone()).expect("response"),
        )
        .expect("supply");
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
    .expect("tree limits")
}

fn acquire(content: &[u8], salt: u8) -> VmInput {
    acquire_with_resources(content, DEFAULT_RESOURCES, salt)
}

fn acquire_with_resources(content: &[u8], resources: &[u8], salt: u8) -> VmInput {
    let fixture = fixture(content, resources, salt);
    acquire_fixture(fixture)
}

fn acquire_with_objects(
    content: &[u8],
    resources: &[u8],
    extra_objects: &[(u32, Vec<u8>)],
    salt: u8,
) -> VmInput {
    acquire_fixture(fixture_with_objects(
        content,
        resources,
        extra_objects,
        salt,
    ))
}

fn acquire_fixture(fixture: Fixture) -> VmInput {
    let store = supplied_store(&fixture);
    let mut xref = OpenXrefJob::new(
        fixture.snapshot,
        XrefJobContext::new(
            JobId::new(30_001),
            ResumeCheckpoint::new(30_002),
            ResumeCheckpoint::new(30_003),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .expect("xref job");
    let section = match xref.poll(&store, &XrefNeverCancelled) {
        XrefPoll::Ready(section) => section,
        outcome => panic!("strict xref must be ready: {outcome:?}"),
    };
    let candidate = CandidateRevisionIndex::from_xref(
        &section,
        REVISION_ID,
        Default::default(),
        &DocumentNeverCancelled,
    )
    .expect("candidate");
    let mut attest = AttestRevisionJob::new(
        candidate,
        RevisionAttestationJobContext::new(
            JobId::new(30_011),
            ResumeCheckpoint::new(30_012),
            ResumeCheckpoint::new(30_013),
            ResumeCheckpoint::new(30_014),
            RequestPriority::Metadata,
        ),
        RevisionAttestationLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .expect("attest job");
    let authority = match attest.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Ready(index) => index,
        outcome => panic!("strict revision must attest: {outcome:?}"),
    }
    .into_shared();
    let mut build = authority
        .build_page_index_owned(
            tree_context(30_021),
            tree_limits(),
            PageIndexLimits::new(4, 16 << 10).expect("index limits"),
        )
        .expect("index job");
    let cold = match build.poll(&store, &DocumentNeverCancelled) {
        PageIndexBuildPoll::Ready(index) => index,
        outcome => panic!("strict Page index must build: {outcome:?}"),
    };
    let mut lookup = authority
        .lookup_page_owned(&cold, 0, tree_context(30_031), tree_limits())
        .expect("lookup job");
    let lookup = match lookup.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Ready(lookup) => lookup,
        outcome => panic!("strict Page lookup must finish: {outcome:?}"),
    };
    let (index, handle) = lookup.into_parts();
    let mut materialize = authority
        .materialize_page_owned(
            &index,
            handle,
            materialization_context(30_041),
            PageMaterializationLimits::default(),
        )
        .expect("materialize job");
    let page = match materialize.poll(&store, &DocumentNeverCancelled) {
        PageMaterializationPoll::Ready(page) => page,
        outcome => panic!("strict Page materialization must finish: {outcome:?}"),
    };
    let mut content_job = authority
        .acquire_page_content_owned(
            &index,
            page,
            content_context(30_051),
            PageContentLimits::default(),
        )
        .expect("content job");
    let acquired = match content_job.poll(&store, &DocumentNeverCancelled) {
        PageContentPoll::Ready(content) => content,
        outcome => panic!("strict Page content acquisition must finish: {outcome:?}"),
    };
    VmInput {
        acquired,
        authority,
        store,
    }
}

fn graphics_job(
    content: &[u8],
    salt: u8,
    graphics_limits: ContentGraphicsLimits,
) -> (InterpretPageJob, RangeStore) {
    graphics_job_with_vm_limits(content, salt, ContentVmLimits::default(), graphics_limits)
}

fn graphics_job_with_vm_limits(
    content: &[u8],
    salt: u8,
    vm_limits: ContentVmLimits,
    graphics_limits: ContentGraphicsLimits,
) -> (InterpretPageJob, RangeStore) {
    graphics_job_with_resources_and_vm_limits(
        content,
        DEFAULT_RESOURCES,
        salt,
        vm_limits,
        graphics_limits,
    )
}

fn graphics_job_with_resources_and_vm_limits(
    content: &[u8],
    resources: &[u8],
    salt: u8,
    vm_limits: ContentVmLimits,
    graphics_limits: ContentGraphicsLimits,
) -> (InterpretPageJob, RangeStore) {
    let input = acquire_with_resources(content, resources, salt);
    (
        InterpretPageJob::new_graphics_v2(
            input.acquired,
            ContentLimits::default(),
            vm_limits,
            graphics_limits,
            PagePropertyLookupLimits::default(),
            GraphicsSceneLimits::default(),
        ),
        input.store,
    )
}

fn graphics_ready(content: &[u8], salt: u8) -> Arc<pdf_rs_content::InterpretedPage> {
    let (mut job, store) = graphics_job(content, salt, ContentGraphicsLimits::default());
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("graphics fixture must be ready: {outcome:?}"),
    }
}

#[test]
fn direct_marked_content_properties_are_pixel_neutral_in_graphics_profile() {
    let page = graphics_ready(
        b"/Span << /ActualText (replacement) /MCID 7 >> BDC 0 0 10 10 re f EMC",
        0x04,
    );

    assert!(page.property_uses().is_empty());
    assert_eq!(page.property_stats().lookups(), 0);
    assert_eq!(page.vm_stats().max_marked_content_depth(), 1);
    assert_eq!(
        page.scene()
            .graphics()
            .expect("graphics-v2 Scene")
            .commands()
            .len(),
        1
    );
}

fn image_object(number: u32, dictionary_entries: &[u8], decoded: &[u8]) -> Vec<u8> {
    let mut object = format!(
        "{number} 0 obj\n<< /Type /XObject /Subtype /Image /Width 2 /Height 1 \
         /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {} ",
        decoded.len()
    )
    .into_bytes();
    object.extend_from_slice(dictionary_entries);
    object.extend_from_slice(b" >>\nstream\n");
    object.extend_from_slice(decoded);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

fn packed_gray_image_object(
    number: u32,
    width: u32,
    height: u32,
    bits_per_component: u8,
    packed: &[u8],
) -> Vec<u8> {
    let mut object = format!(
        "{number} 0 obj\n<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
         /ColorSpace /DeviceGray /BitsPerComponent {bits_per_component} /Length {} >>\nstream\n",
        packed.len()
    )
    .into_bytes();
    object.extend_from_slice(packed);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

fn form_object(number: u32, dictionary_entries: &[u8], content: &[u8]) -> Vec<u8> {
    let mut object = format!(
        "{number} 0 obj\n<< /Type /XObject /Subtype /Form /Length {} ",
        content.len()
    )
    .into_bytes();
    object.extend_from_slice(dictionary_entries);
    object.extend_from_slice(b" >>\nstream\n");
    object.extend_from_slice(content);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

fn image_job(
    content: &[u8],
    resources: &[u8],
    extra_objects: &[(u32, Vec<u8>)],
    salt: u8,
    image_limits: ContentImageLimits,
) -> (InterpretPageJob, RangeStore) {
    image_job_with_vm_limits(
        content,
        resources,
        extra_objects,
        salt,
        image_limits,
        ContentVmLimits::default(),
    )
}

fn image_job_with_vm_limits(
    content: &[u8],
    resources: &[u8],
    extra_objects: &[(u32, Vec<u8>)],
    salt: u8,
    image_limits: ContentImageLimits,
    vm_limits: ContentVmLimits,
) -> (InterpretPageJob, RangeStore) {
    let input = acquire_with_objects(content, resources, extra_objects, salt);
    let profile = ContentImageProfile::new(
        input.authority,
        PageXObjectLookupLimits::default(),
        ImageXObjectJobContext::new(
            JobId::new(31_001),
            ResumeCheckpoint::new(31_002),
            ResumeCheckpoint::new(31_003),
            ResumeCheckpoint::new(31_004),
            RequestPriority::VisiblePage,
        ),
        ImageXObjectLimits::default(),
        image_limits,
    );
    (
        InterpretPageJob::new_graphics_v2_with_images(
            input.acquired,
            ContentLimits::default(),
            vm_limits,
            ContentGraphicsLimits::default(),
            PagePropertyLookupLimits::default(),
            profile,
            GraphicsSceneLimits::default(),
        ),
        input.store,
    )
}

fn default_image_job(content: &[u8], salt: u8) -> (InterpretPageJob, RangeStore) {
    image_job(
        content,
        b"<< /XObject << /Im0 5 0 R /Alias 5 0 R >> >>",
        &[(5, image_object(5, b"", &[10, 20, 30, 40, 50, 60]))],
        salt,
        ContentImageLimits::default(),
    )
}

fn indirect_object(number: u32, value: &[u8]) -> Vec<u8> {
    let mut object = format!("{number} 0 obj\n").into_bytes();
    object.extend_from_slice(value);
    object.extend_from_slice(b"\nendobj\n");
    object
}

fn font_program_object(number: u32, program: &[u8]) -> Vec<u8> {
    let mut object = format!(
        "{number} 0 obj\n<< /Length {} /Length1 {} >>\nstream\n",
        program.len(),
        program.len()
    )
    .into_bytes();
    object.extend_from_slice(program);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

fn font_widths(ascii_a: u32) -> String {
    font_widths_for_range(0x20, 0x7e, ascii_a)
}

fn font_widths_for_range(first: u8, last: u8, ascii_a: u32) -> String {
    (first..=last)
        .map(|byte| if byte == b'A' { ascii_a } else { 600 })
        .map(|width| width.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn embedded_font_objects(
    font_number: u32,
    descriptor_number: u32,
    program_number: u32,
    program: &[u8],
    ascii_a_width: u32,
) -> Vec<(u32, Vec<u8>)> {
    let font = format!(
        "<< /Type /Font /Subtype /TrueType /Encoding /WinAnsiEncoding \
         /FirstChar 32 /LastChar 126 /Widths [{}] /FontDescriptor {descriptor_number} 0 R >>",
        font_widths(ascii_a_width)
    );
    vec![
        (font_number, indirect_object(font_number, font.as_bytes())),
        (
            descriptor_number,
            indirect_object(
                descriptor_number,
                format!("<< /Type /FontDescriptor /FontFile2 {program_number} 0 R >>").as_bytes(),
            ),
        ),
        (program_number, font_program_object(program_number, program)),
    ]
}

fn complete_winansi_font_objects(
    font_number: u32,
    descriptor_number: u32,
    program_number: u32,
    program: &[u8],
    ascii_a_width: u32,
) -> Vec<(u32, Vec<u8>)> {
    let font = format!(
        "<< /Type /Font /Subtype /TrueType /Encoding /WinAnsiEncoding \
         /FirstChar 32 /LastChar 255 /Widths [{}] /FontDescriptor {descriptor_number} 0 R >>",
        font_widths_for_range(0x20, 0xff, ascii_a_width)
    );
    vec![
        (font_number, indirect_object(font_number, font.as_bytes())),
        (
            descriptor_number,
            indirect_object(
                descriptor_number,
                format!("<< /Type /FontDescriptor /FontFile2 {program_number} 0 R >>").as_bytes(),
            ),
        ),
        (program_number, font_program_object(program_number, program)),
    ]
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
        RequestPriority::VisiblePage,
    )
}

fn font_job_with_limits(
    content: &[u8],
    resources: &[u8],
    objects: &[(u32, Vec<u8>)],
    salt: u8,
    vm_limits: ContentVmLimits,
    font_limits: ContentFontLimits,
    scene_limits: GraphicsSceneLimits,
) -> (InterpretPageJob, RangeStore) {
    let input = acquire_with_objects(content, resources, objects, salt);
    let profile = ContentFontProfile::new(
        input.authority,
        PageFontLookupLimits::default(),
        font_context(32_001),
        FontResourceLimits::default(),
        font_limits,
    );
    (
        InterpretPageJob::new_graphics_v2_with_fonts(
            input.acquired,
            ContentLimits::default(),
            vm_limits,
            ContentGraphicsLimits::default(),
            PagePropertyLookupLimits::default(),
            profile,
            scene_limits,
        ),
        input.store,
    )
}

fn default_font_job(content: &[u8], salt: u8) -> (InterpretPageJob, RangeStore) {
    foundational_font_job(
        content,
        salt,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    )
}

fn foundational_font_job(
    content: &[u8],
    salt: u8,
    vm_limits: ContentVmLimits,
    font_limits: ContentFontLimits,
    scene_limits: GraphicsSceneLimits,
) -> (InterpretPageJob, RangeStore) {
    let objects = embedded_font_objects(5, 6, 7, &font_support::foundational_font(), 777);
    font_job_with_limits(
        content,
        b"<< /Font << /F0 5 0 R >> >>",
        &objects,
        salt,
        vm_limits,
        font_limits,
        scene_limits,
    )
}

fn font_ready(content: &[u8], salt: u8) -> Arc<pdf_rs_content::InterpretedPage> {
    let (mut job, store) = default_font_job(content, salt);
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("font fixture must be ready: {outcome:?}"),
    }
}

fn font_limits(mut update: impl FnMut(&mut ContentFontLimitConfig)) -> ContentFontLimits {
    let mut config = ContentFontLimitConfig::default();
    update(&mut config);
    ContentFontLimits::validate(config).expect("test font limits")
}

fn two_distinct_font_job(
    salt: u8,
    vm_limits: ContentVmLimits,
    font_limits: ContentFontLimits,
) -> (InterpretPageJob, RangeStore) {
    let first = font_support::foundational_font();
    let second =
        font_support::build_font(vec![Vec::new(), font_support::contour_glyph(&[true; 128])]);
    let mut objects = embedded_font_objects(5, 6, 7, &first, 777);
    objects.extend(embedded_font_objects(8, 9, 10, &second, 333));
    font_job_with_limits(
        b"BT /F0 10 Tf [(A) 100 200] TJ /Alias 10 Tf (A) Tj /F1 10 Tf (A) Tj ET",
        b"<< /Font << /F0 5 0 R /Alias 5 0 R /F1 8 0 R >> >>",
        &objects,
        salt,
        vm_limits,
        font_limits,
        GraphicsSceneLimits::default(),
    )
}

fn one_font_acquisition_stats(
    font_number: u32,
    descriptor_number: u32,
    program_number: u32,
    program: &[u8],
    width: u32,
    salt: u8,
) -> ContentFontStats {
    let objects = embedded_font_objects(
        font_number,
        descriptor_number,
        program_number,
        program,
        width,
    );
    let resources = format!("<< /Font << /F {font_number} 0 R >> >>");
    let (mut job, store) = font_job_with_limits(
        b"BT /F 10 Tf (A) Tj ET",
        resources.as_bytes(),
        &objects,
        salt,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page.font_stats(),
        outcome => panic!("single Font acquisition must publish: {outcome:?}"),
    }
}

fn three_unique_image_job(
    salt: u8,
    image_limits: ContentImageLimits,
) -> (InterpretPageJob, RangeStore) {
    image_job(
        b"/First Do /Second Do /Third Do",
        b"<< /XObject << /First 5 0 R /Second 6 0 R /Third 7 0 R >> >>",
        &[
            (5, image_object(5, b"", &[1, 2, 3, 4, 5, 6])),
            (6, image_object(6, b"", &[7, 8, 9, 10, 11, 12])),
            (7, image_object(7, b"", &[13, 14, 15, 16, 17, 18])),
        ],
        salt,
        image_limits,
    )
}

fn graphics_failure(content: &[u8], salt: u8, limits: ContentGraphicsLimits) -> ContentVmError {
    let (mut job, store) = graphics_job(content, salt, limits);
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => error,
        outcome => panic!("graphics fixture must fail in VM: {outcome:?}"),
    }
}

fn graphics_limits(
    mut update: impl FnMut(&mut ContentGraphicsLimitConfig),
) -> ContentGraphicsLimits {
    let mut config = ContentGraphicsLimitConfig::default();
    update(&mut config);
    ContentGraphicsLimits::validate(config).expect("test graphics limits")
}

fn vm_limits(mut update: impl FnMut(&mut ContentVmLimitConfig)) -> ContentVmLimits {
    let mut config = ContentVmLimitConfig::default();
    update(&mut config);
    ContentVmLimits::validate(config).expect("test VM limits")
}

fn dash_capacity(entries: usize) -> u64 {
    let mut builder = DashPatternBuilder::new();
    builder
        .try_reserve_exact(entries)
        .expect("test dash reserve");
    builder.retained_bytes().expect("test dash bytes")
}

fn path_capacity(slots: usize) -> u64 {
    let mut builder = PathResourceBuilder::new();
    builder.try_reserve_exact(slots).expect("test path reserve");
    builder.retained_bytes().expect("test path bytes")
}

fn dash_content(entries: usize, malformed_tail: bool) -> Vec<u8> {
    let mut content = b"[".to_vec();
    for index in 0..entries {
        if malformed_tail && index + 1 == entries {
            content.extend_from_slice(b".0000000001");
        } else {
            content.extend_from_slice(b"1");
        }
        content.push(b' ');
    }
    content.extend_from_slice(b"] 0 d");
    content
}

struct GuardedSnapshotSource {
    original: SourceSnapshot,
    replacement: SourceSnapshot,
    changed: AtomicBool,
    snapshot_calls: AtomicUsize,
}

struct CountingStoreSource<'source> {
    complete: &'source RangeStore,
    snapshot_calls: AtomicUsize,
}

struct ChangingStoreSource<'source> {
    complete: &'source RangeStore,
    replacement: SourceSnapshot,
    changed: AtomicBool,
    snapshot_calls: AtomicUsize,
}

impl ByteSource for ChangingStoreSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.snapshot_calls.fetch_add(1, Ordering::AcqRel);
        if self.changed.load(Ordering::Acquire) {
            self.replacement
        } else {
            self.complete.snapshot()
        }
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        self.complete.poll(request)
    }
}

impl ByteSource for CountingStoreSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.snapshot_calls.fetch_add(1, Ordering::AcqRel);
        self.complete.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        self.complete.poll(request)
    }
}

struct CancelAtSnapshotCall<'source> {
    source: &'source CountingStoreSource<'source>,
    trigger: usize,
}

impl DocumentCancellation for CancelAtSnapshotCall<'_> {
    fn is_cancelled(&self) -> bool {
        self.source.snapshot_calls.load(Ordering::Acquire) >= self.trigger
    }
}

struct CancelDuringStore<'source> {
    source: &'source ChangingStoreSource<'source>,
    trigger_snapshot_call: usize,
    change_source: bool,
}

impl DocumentCancellation for CancelDuringStore<'_> {
    fn is_cancelled(&self) -> bool {
        if self.source.snapshot_calls.load(Ordering::Acquire) < self.trigger_snapshot_call {
            return false;
        }
        if self.change_source {
            self.source.changed.store(true, Ordering::Release);
        }
        true
    }
}

impl ByteSource for GuardedSnapshotSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.snapshot_calls.fetch_add(1, Ordering::AcqRel);
        if self.changed.load(Ordering::Acquire) {
            self.replacement
        } else {
            self.original
        }
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("sealed graphics VM must not reacquire content bytes")
    }
}

struct BlockPayloadAfter<'source> {
    complete: &'source RangeStore,
    missing: &'source RangeStore,
    checkpoint: ResumeCheckpoint,
    admitted_payload_polls: usize,
    payload_polls: AtomicUsize,
}

impl ByteSource for BlockPayloadAfter<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.complete.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        if request.checkpoint() == self.checkpoint {
            let poll = self.payload_polls.fetch_add(1, Ordering::AcqRel);
            if poll >= self.admitted_payload_polls {
                return self.missing.poll(request);
            }
        }
        self.complete.poll(request)
    }
}

struct StagedPayloadSource<'source> {
    complete: &'source RangeStore,
    missing: &'source RangeStore,
    checkpoint: ResumeCheckpoint,
    admitted_payload_polls: AtomicUsize,
}

impl StagedPayloadSource<'_> {
    fn admit_one(&self) {
        self.admitted_payload_polls.fetch_add(1, Ordering::AcqRel);
    }
}

impl ByteSource for StagedPayloadSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.complete.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        if request.checkpoint() == self.checkpoint
            && self
                .admitted_payload_polls
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_err()
        {
            return self.missing.poll(request);
        }
        self.complete.poll(request)
    }
}

struct AlwaysCancelled;

impl DocumentCancellation for AlwaysCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

struct CancelDuringDash<'a> {
    source: &'a GuardedSnapshotSource,
    trigger_snapshot_call: usize,
    change_source: bool,
}

impl DocumentCancellation for CancelDuringDash<'_> {
    fn is_cancelled(&self) -> bool {
        if self.source.snapshot_calls.load(Ordering::Acquire) < self.trigger_snapshot_call {
            return false;
        }
        if self.change_source {
            self.source.changed.store(true, Ordering::Release);
        }
        true
    }
}

#[test]
fn graphics_limit_profile_validates_and_round_trips_independent_dimensions() {
    let config = ContentGraphicsLimitConfig::default();
    let limits = ContentGraphicsLimits::validate(config).expect("default graphics limits");
    assert_eq!(limits.max_path_segments(), config.max_path_segments);
    assert_eq!(
        limits.max_path_retained_bytes(),
        config.max_path_retained_bytes
    );
    assert_eq!(limits.max_dash_entries(), config.max_dash_entries);
    assert_eq!(
        limits.max_dash_retained_bytes(),
        config.max_dash_retained_bytes
    );

    for mutate in [
        (|value: &mut ContentGraphicsLimitConfig| value.max_path_segments = 0)
            as fn(&mut ContentGraphicsLimitConfig),
        |value: &mut ContentGraphicsLimitConfig| value.max_path_retained_bytes = 0,
        |value: &mut ContentGraphicsLimitConfig| value.max_dash_entries = 0,
        |value: &mut ContentGraphicsLimitConfig| value.max_dash_retained_bytes = 0,
        |value: &mut ContentGraphicsLimitConfig| value.max_path_segments = u64::MAX,
        |value: &mut ContentGraphicsLimitConfig| value.max_path_retained_bytes = u64::MAX,
        |value: &mut ContentGraphicsLimitConfig| value.max_dash_entries = u32::MAX,
        |value: &mut ContentGraphicsLimitConfig| value.max_dash_retained_bytes = u64::MAX,
    ] {
        let mut invalid = config;
        mutate(&mut invalid);
        assert_eq!(
            ContentGraphicsLimits::validate(invalid)
                .expect_err("invalid graphics limit must fail")
                .code(),
            ContentVmErrorCode::InvalidLimits
        );
    }
}

#[test]
fn legacy_profile_validates_registered_graphics_operands_then_rejects_v2() {
    let input = acquire(b"/Bad 0 m", 0x21);
    let mut malformed = InterpretPageJob::new(
        input.acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        PagePropertyLookupLimits::default(),
        Default::default(),
    );
    match malformed.poll(&input.store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            assert_eq!(error.code(), ContentVmErrorCode::InvalidOperandType);
        }
        outcome => panic!("malformed operand must fail before profile rejection: {outcome:?}"),
    }

    let input = acquire(b"0 0 m", 0x22);
    let mut unsupported = InterpretPageJob::new(
        input.acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        PagePropertyLookupLimits::default(),
        Default::default(),
    );
    match unsupported.poll(&input.store, &DocumentNeverCancelled) {
        ContentVmPoll::Unsupported(value) => {
            assert_eq!(value.kind(), ContentUnsupportedKind::GraphicsV2Operator);
        }
        outcome => panic!("valid graphics operator must require explicit v2: {outcome:?}"),
    }
}

#[test]
fn image_xobjects_publish_ctm_sampling_paint_provenance_and_exact_cache_identity() {
    let (mut job, store) = default_image_job(b"q 2 0 0 3 4 5 cm /Im0 Do /Alias Do Q", 0x61);
    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("basic Image XObjects must publish: {outcome:?}"),
    };

    let graphics = page.scene().graphics().expect("graphics-v2 Scene");
    assert_eq!(graphics.commands().len(), 4);
    assert_eq!(graphics.resources().len(), 1);
    for (index, operator_index) in [(1, 2), (2, 3)] {
        let GraphicsCommand::DrawImage {
            image,
            transform,
            alpha,
            blend_mode,
        } = graphics.commands()[index].command()
        else {
            panic!("command {index} must draw the cached image");
        };
        assert_eq!(
            *transform,
            Matrix::new([
                SceneScalar::from_scaled(2_000_000_000),
                SceneScalar::ZERO,
                SceneScalar::ZERO,
                SceneScalar::from_scaled(3_000_000_000),
                SceneScalar::from_scaled(4_000_000_000),
                SceneScalar::from_scaled(5_000_000_000),
            ])
        );
        assert_eq!(*alpha, SceneUnit::ONE);
        assert_eq!(*blend_mode, BlendMode::Normal);
        assert_eq!(
            graphics.commands()[index].source().operator_index(),
            operator_index
        );
        assert_eq!(image.value(), 0);
    }

    let GraphicsResource::Image(image) = graphics.resources()[0].resource() else {
        panic!("the exact resource must be one image");
    };
    assert_eq!(image.width(), 2);
    assert_eq!(image.height(), 1);
    assert_eq!(image.color_space(), ImageColorSpace::DeviceRgb);
    assert_eq!(image.bits_per_component(), 8);
    assert!(!image.interpolate());
    assert_eq!(image.decoded(), [10, 20, 30, 40, 50, 60]);
    assert_eq!(image.source().object().number(), 5);
    assert_ne!(image.source().decode_context(), 0);

    assert_eq!(page.image_uses().len(), 2);
    assert_eq!(page.image_uses()[0].xobject().target().number(), 5);
    assert_eq!(page.image_uses()[1].xobject().target().number(), 5);
    assert_ne!(
        page.image_uses()[0].xobject().entry_key_offset(),
        page.image_uses()[1].xobject().entry_key_offset()
    );
    assert_eq!(
        page.image_uses()[0].resource_source(),
        page.image_uses()[1].resource_source()
    );
    assert_eq!(page.vm_stats().image_uses(), 2);
    assert_eq!(page.xobject_stats().lookups(), 2);
    assert_eq!(page.image_stats().image_uses(), 2);
    assert_eq!(page.image_stats().lookups(), 2);
    assert_eq!(page.image_stats().cache_hits(), 1);
    assert_eq!(page.image_stats().acquisitions(), 1);
    assert_eq!(page.image_stats().unique_images(), 1);
    assert!(page.image_stats().object_read_bytes() > 0);
    assert!(page.image_stats().object_parse_bytes() > 0);
    assert!(page.image_stats().metadata_entries() > 0);
    assert_eq!(page.image_stats().encoded_bytes(), 6);
    assert_eq!(page.image_stats().decoded_bytes(), 6);
    assert!(page.image_stats().cache_retained_bytes() > 0);
}

#[test]
fn packed_image_samples_normalize_to_eight_bit_scene_resources() {
    let objects = [(
        5,
        packed_gray_image_object(5, 8, 2, 1, &[0b1010_1010, 0b0101_0101]),
    )];
    let (mut job, store) = image_job(
        b"/Im0 Do",
        b"<< /XObject << /Im0 5 0 R >> >>",
        &objects,
        0x60,
        ContentImageLimits::default(),
    );
    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("packed image must normalize into the Scene: {outcome:?}"),
    };
    let graphics = page.scene().graphics().unwrap();
    let GraphicsResource::Image(image) = graphics.resources()[0].resource() else {
        panic!("packed image publishes one Scene image")
    };
    assert_eq!(image.bits_per_component(), 8);
    assert_eq!(
        image.decoded(),
        [
            255, 0, 255, 0, 255, 0, 255, 0, 0, 255, 0, 255, 0, 255, 0, 255
        ]
    );
    assert_eq!(page.image_stats().encoded_bytes(), 2);
    assert_eq!(page.image_stats().decoded_bytes(), 16);
}

#[test]
fn image_profile_context_and_lower_capability_outcomes_are_structured_and_atomic() {
    let input = acquire_with_resources(b"/Im0 Do", b"<< /XObject << /Im0 5 0 R >> >>", 0x62);
    let mut without_profile = InterpretPageJob::new_graphics_v2(
        input.acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
        PagePropertyLookupLimits::default(),
        GraphicsSceneLimits::default(),
    );
    match without_profile.poll(&input.store, &DocumentNeverCancelled) {
        ContentVmPoll::Unsupported(value) => {
            assert_eq!(value.kind(), ContentUnsupportedKind::ImageProfileRequired);
            assert!(value.image_xobject().is_none());
        }
        outcome => panic!("Do without proof authority must be unsupported: {outcome:?}"),
    }

    let (mut text_job, text_store) = default_image_job(b"BT /Im0 Do ET", 0x63);
    match text_job.poll(&text_store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            assert_eq!(error.code(), ContentVmErrorCode::InvalidOperatorContext);
            assert_eq!(text_job.image_stats().lookups(), 0);
            assert_eq!(text_job.image_stats().acquisition_polls(), 0);
        }
        outcome => panic!("Do inside a text object must fail before lookup: {outcome:?}"),
    }

    let resources = b"<< /XObject << /Good 5 0 R /Masked 6 0 R >> >>";
    let objects = [
        (5, image_object(5, b"", &[1, 2, 3, 4, 5, 6])),
        (
            6,
            image_object(6, b"/ImageMask true", &[7, 8, 9, 10, 11, 12]),
        ),
    ];
    let (mut unsupported, store) = image_job(
        b"/Good Do /Masked Do",
        resources,
        &objects,
        0x64,
        ContentImageLimits::default(),
    );
    let first = unsupported.poll(&store, &DocumentNeverCancelled);
    let lower = match first {
        ContentVmPoll::Unsupported(value) => {
            assert_eq!(value.kind(), ContentUnsupportedKind::ImageXObject);
            assert_eq!(value.source().page_operator_ordinal(), 1);
            value.image_xobject().expect("lower image reason")
        }
        outcome => panic!("masked image must suppress the complete Scene: {outcome:?}"),
    };
    assert_eq!(
        lower.kind(),
        pdf_rs_document::ImageXObjectUnsupportedKind::ImageMask
    );
    match unsupported.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Unsupported(value) => {
            assert_eq!(value.image_xobject().expect("replayed lower reason"), lower)
        }
        outcome => panic!("terminal unsupported outcome must replay: {outcome:?}"),
    }
}

#[test]
fn semantic_plan_failures_precede_every_image_lookup_and_acquisition() {
    let (mut underflow, store) = image_job(
        b"Q /Masked Do",
        b"<< /XObject << /Masked 6 0 R >> >>",
        &[(
            6,
            image_object(6, b"/ImageMask true", &[7, 8, 9, 10, 11, 12]),
        )],
        0x76,
        ContentImageLimits::default(),
    );
    match underflow.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            assert_eq!(error.code(), ContentVmErrorCode::InvalidGraphicsState);
            assert_eq!(error.source().expect("Q source").page_operator_ordinal(), 0);
        }
        outcome => panic!("Q underflow must precede the masked image: {outcome:?}"),
    }
    assert_eq!(underflow.image_stats().planning_operators(), 1);
    assert_eq!(underflow.image_stats().lookups(), 0);
    assert_eq!(underflow.image_stats().acquisition_polls(), 0);

    let (mut operator_limited, store) = image_job_with_vm_limits(
        b"q BT",
        b"<< >>",
        &[],
        0x77,
        ContentImageLimits::default(),
        vm_limits(|config| config.max_operators = 1),
    );
    match operator_limited.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.limit().expect("operator limit context");
            assert_eq!(limit.kind(), ContentVmLimitKind::Operators);
            assert_eq!(
                error.source().expect("BT source").page_operator_ordinal(),
                1
            );
        }
        outcome => panic!("BT operator admission must precede terminal imbalance: {outcome:?}"),
    }
    assert_eq!(operator_limited.image_stats().planning_operators(), 2);
    assert_eq!(operator_limited.image_stats().lookups(), 0);
    assert_eq!(operator_limited.image_stats().acquisition_polls(), 0);
}

#[test]
fn long_non_image_semantic_planning_is_single_pass_and_cancellable() {
    let mut content = Vec::new();
    for _ in 0..300 {
        content.extend_from_slice(b"q Q ");
    }
    let (mut job, store) = image_job(&content, b"<< >>", &[], 0x78, ContentImageLimits::default());
    let source = CountingStoreSource {
        complete: &store,
        snapshot_calls: AtomicUsize::new(0),
    };
    let cancellation = CancelAtSnapshotCall {
        source: &source,
        trigger: 20,
    };
    match job.poll(&source, &cancellation) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            assert_eq!(error.code(), ContentVmErrorCode::Cancelled);
        }
        outcome => panic!("long semantic planning must observe cancellation: {outcome:?}"),
    }
    assert_eq!(job.image_stats().scan_passes(), 1);
    assert!(job.image_stats().planning_operators() > 0);
    assert!(job.image_stats().planning_operators() < 600);
    assert_eq!(job.image_stats().lookups(), 0);
    assert_eq!(job.image_stats().acquisition_polls(), 0);
    assert_eq!(job.image_stats().execution_passes(), 0);
}

#[test]
fn aggregate_image_use_and_decoded_byte_limits_reject_exactly_before_publication() {
    let use_limits = ContentImageLimits::validate(ContentImageLimitConfig {
        max_image_uses: 1,
        ..ContentImageLimitConfig::default()
    })
    .expect("one-use limit");
    let (mut use_job, store) = image_job(
        b"/Im0 Do /Alias Do",
        b"<< /XObject << /Im0 5 0 R /Alias 5 0 R >> >>",
        &[(5, image_object(5, b"", &[1, 2, 3, 4, 5, 6]))],
        0x65,
        use_limits,
    );
    match use_job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.image_limit().expect("image-use context");
            assert_eq!(limit.kind(), ContentImageLimitKind::ImageUses);
            assert_eq!(limit.limit(), 1);
            assert_eq!(limit.consumed(), 1);
            assert_eq!(limit.attempted(), 1);
        }
        outcome => panic!("second image use must suppress publication: {outcome:?}"),
    }

    let decoded_limits = ContentImageLimits::validate(ContentImageLimitConfig {
        max_decoded_bytes: 5,
        ..ContentImageLimitConfig::default()
    })
    .expect("five-byte aggregate limit");
    let (mut decoded_job, store) = image_job(
        b"/Im0 Do",
        b"<< /XObject << /Im0 5 0 R >> >>",
        &[(5, image_object(5, b"", &[1, 2, 3, 4, 5, 6]))],
        0x66,
        decoded_limits,
    );
    match decoded_job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.image_limit().expect("decoded-byte context");
            assert_eq!(limit.kind(), ContentImageLimitKind::DecodedBytes);
            assert_eq!(limit.limit(), 5);
            assert_eq!(limit.consumed(), 0);
            assert_eq!(limit.attempted(), 6);
        }
        outcome => panic!("decoded aggregate one-less must suppress Scene: {outcome:?}"),
    }
}

#[test]
fn every_aggregate_image_budget_accepts_exact_and_rejects_one_less() {
    let (mut measured_job, measured_store) =
        three_unique_image_job(0x6b, ContentImageLimits::default());
    let measured = match measured_job.poll(&measured_store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page.image_stats(),
        outcome => panic!("measurement fixture must publish: {outcome:?}"),
    };
    assert_eq!(measured.image_uses(), 3);
    assert_eq!(measured.unique_images(), 3);
    assert_eq!(measured.decoded_bytes(), 18);
    assert_eq!(measured.planning_operators(), 3);
    assert_eq!(measured.cache_probes(), 3);
    assert_eq!(measured.acquisition_polls(), 3);
    assert!(measured.plan_retained_bytes() > 1);
    assert!(measured.cache_retained_bytes() > 1);

    let exact = ContentImageLimits::validate(ContentImageLimitConfig {
        max_image_uses: measured.image_uses(),
        max_unique_images: measured.unique_images(),
        max_decoded_bytes: measured.decoded_bytes(),
        max_planning_operators: measured.planning_operators(),
        max_cache_probes: measured.cache_probes(),
        max_plan_retained_bytes: measured.plan_retained_bytes(),
        max_cache_retained_bytes: measured.cache_retained_bytes(),
        max_acquisition_polls: measured.acquisition_polls(),
    })
    .expect("measured exact image limits validate");
    let (mut exact_job, exact_store) = three_unique_image_job(0x6c, exact);
    match exact_job.poll(&exact_store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => assert_eq!(page.image_stats(), measured),
        outcome => panic!("every exact aggregate image budget must publish: {outcome:?}"),
    }

    macro_rules! rejects_one_less {
        ($salt:expr, $field:ident, $measured:expr, $kind:expr) => {{
            let limits = ContentImageLimits::validate(ContentImageLimitConfig {
                $field: $measured - 1,
                ..ContentImageLimitConfig::default()
            })
            .expect("positive one-less image limit validates");
            let (mut job, store) = three_unique_image_job($salt, limits);
            match job.poll(&store, &DocumentNeverCancelled) {
                ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                    let limit = error.image_limit().expect("aggregate image limit context");
                    assert_eq!(limit.kind(), $kind);
                    assert_eq!(limit.limit(), $measured - 1);
                }
                outcome => panic!("one-less aggregate image budget must fail: {outcome:?}"),
            }
            job
        }};
    }

    let image_use_job = rejects_one_less!(
        0x6d,
        max_image_uses,
        measured.image_uses(),
        ContentImageLimitKind::ImageUses
    );
    assert_eq!(image_use_job.image_stats().lookups(), 0);
    assert_eq!(image_use_job.image_stats().acquisition_polls(), 0);

    let unique_job = rejects_one_less!(
        0x6e,
        max_unique_images,
        measured.unique_images(),
        ContentImageLimitKind::UniqueImages
    );
    assert_eq!(unique_job.image_stats().acquisitions(), 0);

    let decoded_job = rejects_one_less!(
        0x6f,
        max_decoded_bytes,
        measured.decoded_bytes(),
        ContentImageLimitKind::DecodedBytes
    );
    assert_eq!(decoded_job.image_stats().acquisitions(), 2);

    let planning_job = rejects_one_less!(
        0x70,
        max_planning_operators,
        measured.planning_operators(),
        ContentImageLimitKind::PlanningOperators
    );
    assert_eq!(planning_job.image_stats().lookups(), 0);

    let cache_probe_job = rejects_one_less!(
        0x71,
        max_cache_probes,
        measured.cache_probes(),
        ContentImageLimitKind::CacheProbes
    );
    assert_eq!(cache_probe_job.image_stats().cache_probes(), 2);
    assert_eq!(cache_probe_job.image_stats().acquisitions(), 0);
    assert_eq!(cache_probe_job.image_stats().execution_passes(), 0);

    let plan_job = rejects_one_less!(
        0x72,
        max_plan_retained_bytes,
        measured.plan_retained_bytes(),
        ContentImageLimitKind::PlanRetainedBytes
    );
    assert_eq!(plan_job.image_stats().lookups(), 0);

    let cache_job = rejects_one_less!(
        0x73,
        max_cache_retained_bytes,
        measured.cache_retained_bytes(),
        ContentImageLimitKind::CacheRetainedBytes
    );
    assert_eq!(cache_job.image_stats().acquisitions(), 0);

    let polls_job = rejects_one_less!(
        0x74,
        max_acquisition_polls,
        measured.acquisition_polls(),
        ContentImageLimitKind::AcquisitionPolls
    );
    assert_eq!(polls_job.image_stats().acquisitions(), 2);
    assert_eq!(polls_job.image_stats().execution_passes(), 0);
}

#[test]
fn cache_comparisons_are_probe_bounded_and_cooperatively_cancellable() {
    let mut observed = None;
    for trigger in 1..128 {
        let (mut job, store) = three_unique_image_job(0x75, ContentImageLimits::default());
        let source = CountingStoreSource {
            complete: &store,
            snapshot_calls: AtomicUsize::new(0),
        };
        let cancellation = CancelAtSnapshotCall {
            source: &source,
            trigger,
        };
        let outcome = job.poll(&source, &cancellation);
        if matches!(
            outcome,
            ContentVmPoll::Failed(ContentVmFailure::Vm(error))
                if error.code() == ContentVmErrorCode::Cancelled
        ) && job.image_stats().cache_probes() > 0
            && job.image_stats().cache_probes() < 3
            && job.image_stats().acquisition_polls() == 0
        {
            observed = Some(job.image_stats());
            break;
        }
    }
    let stats = observed.expect("one deterministic guard boundary lies inside cache comparison");
    assert_eq!(stats.scan_passes(), 1);
    assert_eq!(stats.planning_operators(), 3);
    assert_eq!(stats.execution_passes(), 0);
}

#[test]
fn image_payload_pending_resumes_without_partial_publication() {
    let objects = [
        (5, image_object(5, b"", &[1, 2, 3, 4, 5, 6])),
        (6, image_object(6, b"", &[7, 8, 9, 10, 11, 12])),
    ];
    let resources = b"<< /XObject << /First 5 0 R /Second 6 0 R >> >>";
    let (mut job, store) = image_job(
        b"/First Do /Second Do",
        resources,
        &objects,
        0x67,
        ContentImageLimits::default(),
    );
    let missing = RangeStore::new(store.snapshot(), Default::default()).unwrap();
    let source = BlockPayloadAfter {
        complete: &store,
        missing: &missing,
        checkpoint: ResumeCheckpoint::new(31_004),
        admitted_payload_polls: 1,
        payload_polls: AtomicUsize::new(0),
    };
    match job.poll(&source, &DocumentNeverCancelled) {
        ContentVmPoll::Pending { checkpoint, .. } => {
            assert_eq!(checkpoint, ResumeCheckpoint::new(31_004));
        }
        outcome => panic!("second image payload must suspend: {outcome:?}"),
    }
    assert_eq!(job.image_stats().image_uses(), 0);
    assert_eq!(job.image_stats().acquisitions(), 1);
    assert_eq!(job.image_stats().acquisition_polls(), 2);
    assert!(job.image_stats().cache_retained_bytes() > 0);
    assert_eq!(
        job.image_stats().cache_retained_bytes(),
        job.image_stats().peak_cache_retained_bytes()
    );
    assert_eq!(job.image_stats().scan_passes(), 1);
    assert_eq!(job.image_stats().planning_operators(), 2);
    assert_eq!(job.image_stats().lookups(), 2);
    assert_eq!(job.image_stats().cache_probes(), 1);
    assert_eq!(job.image_stats().execution_passes(), 0);
    assert_eq!(job.xobject_stats().lookups(), 2);

    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("supplied second payload must resume atomically: {outcome:?}"),
    };
    assert_eq!(page.image_uses().len(), 2);
    assert_eq!(page.image_stats().image_uses(), 2);
    assert_eq!(page.image_stats().acquisitions(), 2);
    assert_eq!(page.image_stats().cache_hits(), 0);
    assert_eq!(page.image_stats().acquisition_polls(), 3);
    assert_eq!(page.image_stats().scan_passes(), 1);
    assert_eq!(page.image_stats().planning_operators(), 2);
    assert_eq!(page.image_stats().lookups(), 2);
    assert_eq!(page.image_stats().cache_probes(), 1);
    assert_eq!(page.image_stats().execution_passes(), 1);
    assert_eq!(page.xobject_stats().lookups(), 2);
}

#[test]
fn multiple_images_resume_from_the_exact_acquisition_cursor_without_replanning() {
    let objects = [
        (5, image_object(5, b"", &[1, 2, 3, 4, 5, 6])),
        (6, image_object(6, b"", &[7, 8, 9, 10, 11, 12])),
    ];
    let (mut job, store) = image_job(
        b"/First Do /Second Do",
        b"<< /XObject << /First 5 0 R /Second 6 0 R >> >>",
        &objects,
        0x6a,
        ContentImageLimits::default(),
    );
    let missing = RangeStore::new(store.snapshot(), Default::default()).unwrap();
    let source = StagedPayloadSource {
        complete: &store,
        missing: &missing,
        checkpoint: ResumeCheckpoint::new(31_004),
        admitted_payload_polls: AtomicUsize::new(0),
    };

    assert!(matches!(
        job.poll(&source, &DocumentNeverCancelled),
        ContentVmPoll::Pending { .. }
    ));
    assert_eq!(job.image_stats().scan_passes(), 1);
    assert_eq!(job.image_stats().planning_operators(), 2);
    assert_eq!(job.image_stats().lookups(), 2);
    assert_eq!(job.image_stats().acquisitions(), 0);
    assert_eq!(job.image_stats().acquisition_polls(), 1);
    assert_eq!(job.image_stats().execution_passes(), 0);

    source.admit_one();
    assert!(matches!(
        job.poll(&source, &DocumentNeverCancelled),
        ContentVmPoll::Pending { .. }
    ));
    assert_eq!(job.image_stats().scan_passes(), 1);
    assert_eq!(job.image_stats().planning_operators(), 2);
    assert_eq!(job.image_stats().lookups(), 2);
    assert_eq!(job.image_stats().acquisitions(), 1);
    assert_eq!(job.image_stats().acquisition_polls(), 3);
    assert_eq!(job.image_stats().execution_passes(), 0);

    source.admit_one();
    let page = match job.poll(&source, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("second staged payload must finish exactly once: {outcome:?}"),
    };
    assert_eq!(page.image_stats().scan_passes(), 1);
    assert_eq!(page.image_stats().planning_operators(), 2);
    assert_eq!(page.image_stats().lookups(), 2);
    assert_eq!(page.image_stats().cache_probes(), 1);
    assert_eq!(page.image_stats().acquisitions(), 2);
    assert_eq!(page.image_stats().acquisition_polls(), 4);
    assert_eq!(page.image_stats().execution_passes(), 1);
    assert_eq!(page.image_uses().len(), 2);
    assert_eq!(page.xobject_stats().lookups(), 2);
}

#[test]
fn early_runtime_failure_after_image_pending_preserves_unexecuted_stats() {
    let make_pending = |salt| {
        image_job(
            b"/First Do /Second Do",
            b"<< /XObject << /First 5 0 R /Second 6 0 R >> >>",
            &[
                (5, image_object(5, b"", &[1, 2, 3, 4, 5, 6])),
                (6, image_object(6, b"", &[7, 8, 9, 10, 11, 12])),
            ],
            salt,
            ContentImageLimits::default(),
        )
    };

    let (mut changed_job, changed_store) = make_pending(0x68);
    let changed_missing = RangeStore::new(changed_store.snapshot(), Default::default()).unwrap();
    let changed_source = BlockPayloadAfter {
        complete: &changed_store,
        missing: &changed_missing,
        checkpoint: ResumeCheckpoint::new(31_004),
        admitted_payload_polls: 1,
        payload_polls: AtomicUsize::new(0),
    };
    assert!(matches!(
        changed_job.poll(&changed_source, &DocumentNeverCancelled),
        ContentVmPoll::Pending { .. }
    ));
    assert_eq!(changed_job.image_stats().image_uses(), 0);
    let replacement = snapshot(
        changed_store.snapshot().len().expect("fixture length"),
        0x99,
    );
    let changed_guard = GuardedSnapshotSource {
        original: changed_store.snapshot(),
        replacement,
        changed: AtomicBool::new(true),
        snapshot_calls: AtomicUsize::new(0),
    };
    match changed_job.poll(&changed_guard, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            assert_eq!(error.code(), ContentVmErrorCode::SourceSnapshotMismatch);
        }
        outcome => panic!("source change after Pending must fail before replay: {outcome:?}"),
    }
    assert_eq!(changed_job.image_stats().image_uses(), 0);

    let (mut cancelled_job, cancelled_store) = make_pending(0x69);
    let cancelled_missing =
        RangeStore::new(cancelled_store.snapshot(), Default::default()).unwrap();
    let cancelled_source = BlockPayloadAfter {
        complete: &cancelled_store,
        missing: &cancelled_missing,
        checkpoint: ResumeCheckpoint::new(31_004),
        admitted_payload_polls: 1,
        payload_polls: AtomicUsize::new(0),
    };
    assert!(matches!(
        cancelled_job.poll(&cancelled_source, &DocumentNeverCancelled),
        ContentVmPoll::Pending { .. }
    ));
    match cancelled_job.poll(&cancelled_store, &AlwaysCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            assert_eq!(error.code(), ContentVmErrorCode::Cancelled);
        }
        outcome => panic!("cancellation after Pending must fail before replay: {outcome:?}"),
    }
    assert_eq!(cancelled_job.image_stats().image_uses(), 0);
}

#[test]
fn graphics_v2_publishes_line_color_clip_paint_and_exact_provenance() {
    let page = graphics_ready(
        b"q 2 w 1 J 2 j 10 M [3 1] 2 d \
          0 0 10 10 re W n \
          1 0 0 rg .5 G 1 1 8 8 re B* Q",
        0x23,
    );
    assert_eq!(page.scene().version(), SceneVersion::V2_0);
    assert!(page.scene().commands().is_empty());
    let graphics = page.scene().graphics().expect("v2 graphics");
    assert_eq!(graphics.commands().len(), 4);
    assert!(graphics.is_supported());
    assert!(matches!(
        graphics.commands()[0].command(),
        GraphicsCommand::Save
    ));
    assert!(matches!(
        graphics.commands()[1].command(),
        GraphicsCommand::Clip {
            rule: FillRule::Nonzero,
            ..
        }
    ));
    assert_eq!(graphics.commands()[1].source().operator_index(), 7);
    let GraphicsCommand::FillStroke {
        rule,
        fill,
        stroke,
        style,
        ..
    } = graphics.commands()[2].command()
    else {
        panic!("third command must fill and stroke");
    };
    assert_eq!(*rule, FillRule::EvenOdd);
    assert_eq!(
        fill.color(),
        DeviceColor::Rgb {
            red: SceneUnit::ONE,
            green: SceneUnit::ZERO,
            blue: SceneUnit::ZERO,
        }
    );
    assert_eq!(
        stroke.color(),
        DeviceColor::Gray(SceneUnit::from_u16(32_768))
    );
    assert_eq!(style.width(), SceneScalar::from_scaled(2_000_000_000));
    assert_eq!(style.cap(), LineCap::Round);
    assert_eq!(style.join(), LineJoin::Bevel);
    assert_eq!(
        style.miter_limit(),
        SceneScalar::from_scaled(10_000_000_000)
    );
    assert_eq!(
        style.dash().array(),
        [SceneScalar::from_scaled(3_000_000_000), SceneScalar::ONE]
    );
    assert_eq!(
        style.dash().phase(),
        SceneScalar::from_scaled(2_000_000_000)
    );
    assert!(matches!(
        graphics.commands()[3].command(),
        GraphicsCommand::Restore
    ));
    assert_eq!(page.final_ctm(), Matrix::IDENTITY);
}

#[test]
fn q_and_q_restore_complete_state_but_never_save_the_current_path() {
    let page = graphics_ready(b"2 w 1 G 0 0 m q 5 w 1 0 0 RG 10 0 l Q S", 0x24);
    let graphics = page.scene().graphics().expect("v2 graphics");
    assert_eq!(graphics.commands().len(), 3);
    let GraphicsCommand::Stroke {
        path, paint, style, ..
    } = graphics.commands()[2].command()
    else {
        panic!("final command must stroke");
    };
    assert_eq!(style.width(), SceneScalar::from_scaled(2_000_000_000));
    assert_eq!(paint.color(), DeviceColor::Gray(SceneUnit::ONE));
    let entry = graphics
        .resources()
        .iter()
        .find(|entry| entry.id() == *path)
        .expect("path resource");
    let GraphicsResource::Path(path) = entry.resource() else {
        panic!("stroke resource must be a path");
    };
    assert_eq!(path.segments().len(), 2);
    assert!(matches!(path.segments()[0], PathSegment::MoveTo(_)));
    assert!(matches!(path.segments()[1], PathSegment::LineTo(_)));
}

#[test]
fn q_and_q_restore_dash_line_color_alpha_blend_and_nested_clip_state() {
    let page = graphics_ready(
        b"2 w 1 J 2 j 11 M [3 1] 2 d .25 g 0 0 10 10 re W* n \
          q 5 w 2 J 1 j 20 M [7 2] 1 d 1 0 0 rg 0 0 5 5 re W n Q \
          0 0 1 1 re B",
        0x28,
    );
    let commands = page.scene().graphics().expect("v2 graphics").commands();
    assert_eq!(commands.len(), 5);
    assert!(matches!(
        commands[0].command(),
        GraphicsCommand::Clip {
            rule: FillRule::EvenOdd,
            ..
        }
    ));
    assert_eq!(commands[0].source().operator_index(), 7);
    assert!(matches!(commands[1].command(), GraphicsCommand::Save));
    assert!(matches!(
        commands[2].command(),
        GraphicsCommand::Clip {
            rule: FillRule::Nonzero,
            ..
        }
    ));
    assert_eq!(commands[2].source().operator_index(), 17);
    assert!(matches!(commands[3].command(), GraphicsCommand::Restore));

    let GraphicsCommand::FillStroke {
        fill,
        stroke,
        style,
        ..
    } = commands[4].command()
    else {
        panic!("post-restore command must fill and stroke");
    };
    assert_eq!(style.width(), SceneScalar::from_scaled(2_000_000_000));
    assert_eq!(style.cap(), LineCap::Round);
    assert_eq!(style.join(), LineJoin::Bevel);
    assert_eq!(
        style.miter_limit(),
        SceneScalar::from_scaled(11_000_000_000)
    );
    assert_eq!(
        style.dash().array(),
        [SceneScalar::from_scaled(3_000_000_000), SceneScalar::ONE]
    );
    assert_eq!(
        style.dash().phase(),
        SceneScalar::from_scaled(2_000_000_000)
    );
    assert_eq!(fill.color(), DeviceColor::Gray(SceneUnit::from_u16(16_384)));
    assert_eq!(stroke.color(), DeviceColor::Gray(SceneUnit::ZERO));
    assert_eq!(fill.alpha(), SceneUnit::ONE);
    assert_eq!(stroke.alpha(), SceneUnit::ONE);
    assert_eq!(fill.blend_mode(), BlendMode::Normal);
    assert_eq!(stroke.blend_mode(), BlendMode::Normal);
}

#[test]
fn every_direct_device_color_operator_reaches_the_matching_paint_channel() {
    let page = graphics_ready(
        b".25 g 0 0 1 1 re f \
          -1 2 .5 RG 0 0 m 1 0 l S \
          0 1 1 0 k 0 0 1 1 re f \
          1 0 0 0 K 0 0 m 1 0 l S",
        0x27,
    );
    let commands = page.scene().graphics().expect("v2 graphics").commands();
    let GraphicsCommand::Fill { paint, .. } = commands[0].command() else {
        panic!("first command must fill");
    };
    assert_eq!(
        paint.color(),
        DeviceColor::Gray(SceneUnit::from_u16(16_384))
    );
    let GraphicsCommand::Stroke { paint, .. } = commands[1].command() else {
        panic!("second command must stroke");
    };
    assert_eq!(
        paint.color(),
        DeviceColor::Rgb {
            red: SceneUnit::ZERO,
            green: SceneUnit::ONE,
            blue: SceneUnit::from_u16(32_768),
        }
    );
    let GraphicsCommand::Fill { paint, .. } = commands[2].command() else {
        panic!("third command must fill");
    };
    assert_eq!(
        paint.color(),
        DeviceColor::Cmyk {
            cyan: SceneUnit::ZERO,
            magenta: SceneUnit::ONE,
            yellow: SceneUnit::ONE,
            black: SceneUnit::ZERO,
        }
    );
    let GraphicsCommand::Stroke { paint, .. } = commands[3].command() else {
        panic!("fourth command must stroke");
    };
    assert_eq!(
        paint.color(),
        DeviceColor::Cmyk {
            cyan: SceneUnit::ONE,
            magenta: SceneUnit::ZERO,
            yellow: SceneUnit::ZERO,
            black: SceneUnit::ZERO,
        }
    );
}

#[test]
fn cubic_shorthands_and_every_paint_family_publish_exact_command_kinds() {
    let cubic = graphics_ready(b"0 0 m 1 2 3 4 5 6 c 7 8 9 10 v 11 12 13 14 y h S", 0x25);
    let graphics = cubic.scene().graphics().expect("v2 graphics");
    let GraphicsCommand::Stroke { path, .. } = graphics.commands()[0].command() else {
        panic!("cubic fixture must stroke");
    };
    let entry = graphics
        .resources()
        .iter()
        .find(|entry| entry.id() == *path)
        .expect("path resource");
    let GraphicsResource::Path(path) = entry.resource() else {
        panic!("resource must be path");
    };
    assert_eq!(path.segments().len(), 5);
    let PathSegment::CubicTo { control_1, .. } = path.segments()[2] else {
        panic!("v must produce cubic");
    };
    assert_eq!(
        control_1,
        pdf_rs_scene::ScenePoint::new(
            SceneScalar::from_scaled(5_000_000_000),
            SceneScalar::from_scaled(6_000_000_000)
        )
    );
    let PathSegment::CubicTo { control_2, end, .. } = path.segments()[3] else {
        panic!("y must produce cubic");
    };
    assert_eq!(control_2, end);

    let page = graphics_ready(
        b"0 0 1 1 re S 0 0 1 1 re s \
          0 0 1 1 re f 0 0 1 1 re F 0 0 1 1 re f* \
          0 0 1 1 re B 0 0 1 1 re B* \
          0 0 1 1 re b 0 0 1 1 re b* 0 0 1 1 re n",
        0x26,
    );
    let commands = page.scene().graphics().expect("v2 graphics").commands();
    assert_eq!(commands.len(), 9);
    assert!(matches!(
        commands[0].command(),
        GraphicsCommand::Stroke { .. }
    ));
    assert!(matches!(
        commands[1].command(),
        GraphicsCommand::Stroke { .. }
    ));
    assert!(matches!(
        commands[2].command(),
        GraphicsCommand::Fill {
            rule: FillRule::Nonzero,
            ..
        }
    ));
    assert!(matches!(
        commands[3].command(),
        GraphicsCommand::Fill {
            rule: FillRule::Nonzero,
            ..
        }
    ));
    assert!(matches!(
        commands[4].command(),
        GraphicsCommand::Fill {
            rule: FillRule::EvenOdd,
            ..
        }
    ));
    assert!(matches!(
        commands[5].command(),
        GraphicsCommand::FillStroke {
            rule: FillRule::Nonzero,
            ..
        }
    ));
    assert!(matches!(
        commands[6].command(),
        GraphicsCommand::FillStroke {
            rule: FillRule::EvenOdd,
            ..
        }
    ));
    assert!(matches!(
        commands[7].command(),
        GraphicsCommand::FillStroke { .. }
    ));
    assert!(matches!(
        commands[8].command(),
        GraphicsCommand::FillStroke { .. }
    ));
}

#[test]
fn operand_type_context_conversion_and_path_state_fail_before_mutation() {
    for (case, (content, code)) in [
        (
            b"BT /Bad 0 m".as_slice(),
            ContentVmErrorCode::InvalidOperandType,
        ),
        (
            b"BT 0 0 m".as_slice(),
            ContentVmErrorCode::InvalidOperatorContext,
        ),
        (
            b"-1 w".as_slice(),
            ContentVmErrorCode::InvalidGraphicsParameter,
        ),
        (b"1 1 l".as_slice(), ContentVmErrorCode::InvalidPathState),
        (
            b"[0 0] 0 d".as_slice(),
            ContentVmErrorCode::InvalidGraphicsParameter,
        ),
        (
            b"3 J".as_slice(),
            ContentVmErrorCode::InvalidGraphicsParameter,
        ),
        (
            b"-1 j".as_slice(),
            ContentVmErrorCode::InvalidGraphicsParameter,
        ),
        (
            b".5 M".as_slice(),
            ContentVmErrorCode::InvalidGraphicsParameter,
        ),
        (
            b"[-1 2] 0 d".as_slice(),
            ContentVmErrorCode::InvalidGraphicsParameter,
        ),
        (
            b"[1 2] -1 d".as_slice(),
            ContentVmErrorCode::InvalidGraphicsParameter,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            graphics_failure(
                content,
                0x30 + u8::try_from(case).expect("case fits"),
                ContentGraphicsLimits::default()
            )
            .code(),
            code,
            "case {case}"
        );
    }
}

#[test]
fn every_graphics_budget_accepts_exact_and_rejects_one_less() {
    let exact_segments = graphics_limits(|config| config.max_path_segments = 5);
    let page = {
        let (mut job, store) = graphics_job(b"0 0 1 1 re f", 0x40, exact_segments);
        match job.poll(&store, &DocumentNeverCancelled) {
            ContentVmPoll::Ready(page) => page,
            outcome => panic!("exact segment budget must pass: {outcome:?}"),
        }
    };
    assert_eq!(
        page.scene().graphics().expect("graphics").resources().len(),
        1
    );

    let segment_error = graphics_failure(
        b"0 0 1 1 re f",
        0x41,
        graphics_limits(|config| config.max_path_segments = 4),
    );
    let segment_limit = segment_error.graphics_limit().expect("segment context");
    assert_eq!(segment_limit.kind(), ContentGraphicsLimitKind::PathSegments);
    assert_eq!(segment_limit.limit(), 4);
    assert_eq!(segment_limit.consumed(), 0);
    assert_eq!(segment_limit.attempted(), 5);

    let path_bytes = u64::try_from(std::mem::size_of::<PathSegment>() * 5).expect("path bytes fit");
    let retained_error = graphics_failure(
        b"0 0 1 1 re f",
        0x42,
        graphics_limits(|config| config.max_path_retained_bytes = path_bytes - 1),
    );
    assert_eq!(
        retained_error
            .graphics_limit()
            .expect("retained context")
            .kind(),
        ContentGraphicsLimitKind::PathRetainedBytes
    );

    let dash_error = graphics_failure(
        b"[1 2] 0 d",
        0x43,
        graphics_limits(|config| config.max_dash_entries = 1),
    );
    let dash_limit = dash_error.graphics_limit().expect("dash context");
    assert_eq!(dash_limit.kind(), ContentGraphicsLimitKind::DashEntries);
    assert_eq!(dash_limit.limit(), 1);
    assert_eq!(dash_limit.attempted(), 2);

    let mut dash_builder = DashPatternBuilder::new();
    dash_builder.try_reserve_exact(2).expect("dash reserve");
    let dash_bytes = dash_builder.retained_bytes().expect("dash bytes");
    let exact_dash_retained = graphics_limits(|config| config.max_dash_retained_bytes = dash_bytes);
    let (mut exact_job, exact_store) = graphics_job(b"[1 2] 0 d", 0x44, exact_dash_retained);
    assert!(matches!(
        exact_job.poll(&exact_store, &DocumentNeverCancelled),
        ContentVmPoll::Ready(_)
    ));

    let retained_error = graphics_failure(
        b"[1 2] 0 d",
        0x45,
        graphics_limits(|config| config.max_dash_retained_bytes = dash_bytes - 1),
    );
    let retained_limit = retained_error
        .graphics_limit()
        .expect("dash retained context");
    assert_eq!(
        retained_limit.kind(),
        ContentGraphicsLimitKind::DashRetainedBytes
    );
    assert_eq!(retained_limit.limit(), dash_bytes - 1);
    assert_eq!(retained_limit.attempted(), dash_bytes);
}

#[test]
fn nested_distinct_dash_payloads_are_aggregate_charged_to_graphics_and_vm_retention() {
    let content = b"[1] 0 d q [2 3] 0 d q [4 5 6] 0 d Q Q";
    let aggregate_dash = dash_capacity(1) + dash_capacity(2) + dash_capacity(3);
    let exact_graphics = graphics_limits(|config| config.max_dash_retained_bytes = aggregate_dash);
    let (mut exact_graphics_job, exact_graphics_store) =
        graphics_job(content, 0x46, exact_graphics);
    assert!(matches!(
        exact_graphics_job.poll(&exact_graphics_store, &DocumentNeverCancelled),
        ContentVmPoll::Ready(_)
    ));
    assert!(
        exact_graphics_job.vm_stats().peak_retained_bytes()
            >= exact_graphics_job
                .scan_stats()
                .retained_bytes()
                .saturating_add(aggregate_dash)
    );

    let graphics_error = graphics_failure(
        content,
        0x47,
        graphics_limits(|config| config.max_dash_retained_bytes = aggregate_dash - 1),
    );
    let graphics_limit = graphics_error
        .graphics_limit()
        .expect("aggregate dash failure");
    assert_eq!(
        graphics_limit.kind(),
        ContentGraphicsLimitKind::DashRetainedBytes
    );
    assert_eq!(graphics_limit.limit(), aggregate_dash - 1);
    assert!(graphics_limit.consumed() > 0);

    let measured_peak = exact_graphics_job.vm_stats().peak_retained_bytes();
    let (mut exact_vm_job, exact_vm_store) = graphics_job_with_vm_limits(
        content,
        0x48,
        vm_limits(|config| config.max_retained_bytes = measured_peak),
        ContentGraphicsLimits::default(),
    );
    assert!(matches!(
        exact_vm_job.poll(&exact_vm_store, &DocumentNeverCancelled),
        ContentVmPoll::Ready(_)
    ));

    let (mut tight_vm_job, tight_vm_store) = graphics_job_with_vm_limits(
        content,
        0x49,
        vm_limits(|config| config.max_retained_bytes = measured_peak - 1),
        ContentGraphicsLimits::default(),
    );
    match tight_vm_job.poll(&tight_vm_store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.limit().expect("VM retained failure");
            assert_eq!(limit.kind(), ContentVmLimitKind::RetainedBytes);
            assert_eq!(limit.limit(), measured_peak - 1);
        }
        outcome => panic!("one-less aggregate VM retention must fail: {outcome:?}"),
    }
}

#[test]
fn completed_path_actions_remain_aggregate_vm_charged_after_handoff() {
    let content = b"0 0 m 1 0 l S 0 1 m 1 1 l f 0 2 m 1 2 l W n";
    let (mut baseline, baseline_store) = graphics_job_with_vm_limits(
        content,
        0x79,
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
    );
    assert!(matches!(
        baseline.poll(&baseline_store, &DocumentNeverCancelled),
        ContentVmPoll::Ready(_)
    ));
    let measured_peak = baseline.vm_stats().peak_retained_bytes();
    let aggregate_path_bytes = path_capacity(4) * 3;
    assert!(
        measured_peak
            >= baseline
                .scan_stats()
                .retained_bytes()
                .saturating_add(aggregate_path_bytes),
        "three completed nonempty paths must remain retained together"
    );

    let (mut exact, exact_store) = graphics_job_with_vm_limits(
        content,
        0x7a,
        vm_limits(|config| config.max_retained_bytes = measured_peak),
        ContentGraphicsLimits::default(),
    );
    let exact_outcome = exact.poll(&exact_store, &DocumentNeverCancelled);
    assert!(
        matches!(exact_outcome, ContentVmPoll::Ready(_)),
        "exact action-path retention must publish: {exact_outcome:?}"
    );

    let (mut tight, tight_store) = graphics_job_with_vm_limits(
        content,
        0x7b,
        vm_limits(|config| config.max_retained_bytes = measured_peak - 1),
        ContentGraphicsLimits::default(),
    );
    match tight.poll(&tight_store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.limit().expect("action-path retained context");
            assert_eq!(limit.kind(), ContentVmLimitKind::RetainedBytes);
            assert_eq!(limit.limit(), measured_peak - 1);
        }
        outcome => panic!("one-less action-path retention must fail: {outcome:?}"),
    }
}

#[test]
fn image_plan_retention_deduplicates_shared_paths_and_dashes_across_pending() {
    let pending_plan_retained =
        |content: &[u8], salt: u8, image_limits: ContentImageLimits| -> u64 {
            let (mut job, store) = image_job(
                content,
                b"<< /XObject << /Im0 5 0 R >> >>",
                &[(5, image_object(5, b"", &[1, 2, 3, 4, 5, 6]))],
                salt,
                image_limits,
            );
            let missing = RangeStore::new(store.snapshot(), Default::default()).unwrap();
            let source = BlockPayloadAfter {
                complete: &store,
                missing: &missing,
                checkpoint: ResumeCheckpoint::new(31_004),
                admitted_payload_polls: 0,
                payload_polls: AtomicUsize::new(0),
            };
            assert!(matches!(
                job.poll(&source, &DocumentNeverCancelled),
                ContentVmPoll::Pending { .. }
            ));
            let first = job.image_stats();
            assert!(first.plan_retained_bytes() > 0);
            assert_eq!(
                first.plan_retained_bytes(),
                first.peak_plan_retained_bytes()
            );
            assert_eq!(first.execution_passes(), 0);
            assert!(matches!(
                job.poll(&source, &DocumentNeverCancelled),
                ContentVmPoll::Pending { .. }
            ));
            let second = job.image_stats();
            assert_eq!(second.plan_retained_bytes(), first.plan_retained_bytes());
            assert_eq!(
                second.peak_plan_retained_bytes(),
                first.peak_plan_retained_bytes()
            );
            assert_eq!(second.scan_passes(), first.scan_passes());
            assert_eq!(second.planning_operators(), first.planning_operators());
            assert_eq!(second.lookups(), first.lookups());
            assert_eq!(second.execution_passes(), 0);
            first.plan_retained_bytes()
        };

    let single = pending_plan_retained(
        b"[1 2] 0 d 0 0 m 1 0 l S /Im0 Do",
        0x7c,
        ContentImageLimits::default(),
    );
    let shared_dash = pending_plan_retained(
        b"[1 2] 0 d 0 0 m 1 0 l S 0 1 m 1 1 l S /Im0 Do",
        0x7d,
        ContentImageLimits::default(),
    );
    let distinct_dash = pending_plan_retained(
        b"[1 2] 0 d 0 0 m 1 0 l S [3 4 5] 0 d 0 1 m 1 1 l S /Im0 Do",
        0x7e,
        ContentImageLimits::default(),
    );
    assert_eq!(shared_dash - single, path_capacity(4));
    assert_eq!(distinct_dash - shared_dash, dash_capacity(3));

    let fill_stroke = pending_plan_retained(
        b"[1 2] 0 d 0 0 1 1 re B /Im0 Do",
        0x7f,
        ContentImageLimits::default(),
    );
    let fill_stroke_clip = pending_plan_retained(
        b"[1 2] 0 d 0 0 1 1 re W B /Im0 Do",
        0x80,
        ContentImageLimits::default(),
    );
    assert_eq!(fill_stroke_clip, fill_stroke);

    let exact_limits = ContentImageLimits::validate(ContentImageLimitConfig {
        max_plan_retained_bytes: distinct_dash,
        ..ContentImageLimitConfig::default()
    })
    .expect("exact image plan limit");
    assert_eq!(
        pending_plan_retained(
            b"[1 2] 0 d 0 0 m 1 0 l S [3 4 5] 0 d 0 1 m 1 1 l S /Im0 Do",
            0x81,
            exact_limits,
        ),
        distinct_dash
    );

    let tight_limits = ContentImageLimits::validate(ContentImageLimitConfig {
        max_plan_retained_bytes: distinct_dash - 1,
        ..ContentImageLimitConfig::default()
    })
    .expect("one-less image plan limit");
    let (mut tight, tight_store) = image_job(
        b"[1 2] 0 d 0 0 m 1 0 l S [3 4 5] 0 d 0 1 m 1 1 l S /Im0 Do",
        b"<< /XObject << /Im0 5 0 R >> >>",
        &[(5, image_object(5, b"", &[1, 2, 3, 4, 5, 6]))],
        0x82,
        tight_limits,
    );
    match tight.poll(&tight_store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.image_limit().expect("image plan retained context");
            assert_eq!(limit.kind(), ContentImageLimitKind::PlanRetainedBytes);
            assert_eq!(limit.limit(), distinct_dash - 1);
        }
        outcome => panic!("one-less image plan retention must fail: {outcome:?}"),
    }
    assert_eq!(tight.image_stats().lookups(), 0);
    assert_eq!(tight.image_stats().acquisition_polls(), 0);
    assert_eq!(tight.xobject_stats().lookups(), 0);
}

#[test]
fn vm_retention_aggregates_live_path_dash_property_and_saved_state_exactly() {
    let content = b"[1 2 3] 0 d 0 0 m 1 0 l /Tag /P BDC q Q EMC";
    let (mut baseline, baseline_store) = graphics_job_with_resources_and_vm_limits(
        content,
        PROPERTY_RESOURCES,
        0x62,
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
    );
    let page = match baseline.poll(&baseline_store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("aggregate baseline must be ready: {outcome:?}"),
    };
    assert_eq!(page.property_uses().len(), 1);
    let exact_retained = baseline.vm_stats().peak_retained_bytes();
    assert!(
        exact_retained
            > baseline
                .scan_stats()
                .retained_bytes()
                .saturating_add(dash_capacity(3))
    );

    let (mut exact, exact_store) = graphics_job_with_resources_and_vm_limits(
        content,
        PROPERTY_RESOURCES,
        0x63,
        vm_limits(|config| config.max_retained_bytes = exact_retained),
        ContentGraphicsLimits::default(),
    );
    let exact_outcome = exact.poll(&exact_store, &DocumentNeverCancelled);
    assert!(
        matches!(exact_outcome, ContentVmPoll::Ready(_)),
        "exact aggregate retention must be ready: {exact_outcome:?}"
    );

    let (mut tight, tight_store) = graphics_job_with_resources_and_vm_limits(
        content,
        PROPERTY_RESOURCES,
        0x64,
        vm_limits(|config| config.max_retained_bytes = exact_retained - 1),
        ContentGraphicsLimits::default(),
    );
    match tight.poll(&tight_store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.limit().expect("aggregate retained context");
            assert_eq!(limit.kind(), ContentVmLimitKind::RetainedBytes);
            assert_eq!(limit.limit(), exact_retained - 1);
        }
        outcome => panic!("one-less aggregate retention must fail: {outcome:?}"),
    }
}

#[test]
fn dash_entries_and_complete_fuel_are_admitted_before_numeric_conversion() {
    let content = dash_content(300, false);
    let (mut baseline, baseline_store) =
        graphics_job(&content, 0x4a, ContentGraphicsLimits::default());
    assert!(matches!(
        baseline.poll(&baseline_store, &DocumentNeverCancelled),
        ContentVmPoll::Ready(_)
    ));
    let exact_fuel = baseline.vm_stats().fuel();
    assert!(exact_fuel > 1);

    let (mut exact, exact_store) = graphics_job_with_vm_limits(
        &content,
        0x4b,
        vm_limits(|config| config.max_fuel = exact_fuel),
        ContentGraphicsLimits::default(),
    );
    assert!(matches!(
        exact.poll(&exact_store, &DocumentNeverCancelled),
        ContentVmPoll::Ready(_)
    ));

    for (salt, candidate) in [(0x4c, content), (0x4d, dash_content(300, true))] {
        let (mut tight, tight_store) = graphics_job_with_vm_limits(
            &candidate,
            salt,
            vm_limits(|config| config.max_fuel = exact_fuel - 1),
            ContentGraphicsLimits::default(),
        );
        match tight.poll(&tight_store, &DocumentNeverCancelled) {
            ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                let limit = error.limit().expect("fuel context");
                assert_eq!(limit.kind(), ContentVmLimitKind::Fuel);
                assert_eq!(limit.limit(), exact_fuel - 1);
            }
            outcome => panic!("fuel must reject before dash conversion: {outcome:?}"),
        }
    }

    let malformed = dash_content(300, true);
    let error = graphics_failure(
        &malformed,
        0x4e,
        graphics_limits(|config| config.max_dash_entries = 299),
    );
    let limit = error.graphics_limit().expect("dash-entry context");
    assert_eq!(limit.kind(), ContentGraphicsLimitKind::DashEntries);
    assert_eq!(limit.limit(), 299);
    assert_eq!(limit.attempted(), 300);

    let expected_dash_bytes = dash_capacity(300);
    let retained_error = graphics_failure(
        &malformed,
        0x65,
        graphics_limits(|config| {
            config.max_dash_retained_bytes = expected_dash_bytes - 1;
        }),
    );
    let retained_limit = retained_error
        .graphics_limit()
        .expect("dash retained context");
    assert_eq!(
        retained_limit.kind(),
        ContentGraphicsLimitKind::DashRetainedBytes
    );
    assert_eq!(retained_limit.limit(), expected_dash_bytes - 1);
    assert_eq!(retained_limit.attempted(), expected_dash_bytes);

    let (mut malformed_baseline, malformed_store) =
        graphics_job(&malformed, 0x66, ContentGraphicsLimits::default());
    assert!(matches!(
        malformed_baseline.poll(&malformed_store, &DocumentNeverCancelled),
        ContentVmPoll::Failed(ContentVmFailure::Vm(_))
    ));
    let vm_retained_before_candidate = malformed_baseline.scan_stats().retained_bytes();
    let (mut tight_retained, tight_retained_store) = graphics_job_with_vm_limits(
        &malformed,
        0x67,
        vm_limits(|config| {
            config.max_retained_bytes = vm_retained_before_candidate
                .saturating_add(expected_dash_bytes)
                .saturating_sub(1);
        }),
        ContentGraphicsLimits::default(),
    );
    match tight_retained.poll(&tight_retained_store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.limit().expect("VM retained context");
            assert_eq!(limit.kind(), ContentVmLimitKind::RetainedBytes);
            assert_eq!(
                limit.limit(),
                vm_retained_before_candidate + expected_dash_bytes - 1
            );
        }
        outcome => panic!("VM retention must reject before dash conversion: {outcome:?}"),
    }
}

#[test]
fn long_dash_conversion_probes_cancellation_and_prioritizes_source_change() {
    let content = dash_content(512, false);
    for (case, (salt, change_source, expected)) in [
        (0, (0x4f, false, ContentVmErrorCode::Cancelled)),
        (1, (0x50, true, ContentVmErrorCode::SourceSnapshotMismatch)),
    ] {
        let input = acquire(&content, salt);
        let original = input.acquired.handle().snapshot();
        let replacement = snapshot(
            original.len().expect("fixture length"),
            salt.wrapping_add(0x40),
        );
        let source = GuardedSnapshotSource {
            original,
            replacement,
            changed: AtomicBool::new(false),
            snapshot_calls: AtomicUsize::new(0),
        };
        let cancellation = CancelDuringDash {
            source: &source,
            trigger_snapshot_call: 9,
            change_source,
        };
        let mut job = InterpretPageJob::new_graphics_v2(
            input.acquired,
            ContentLimits::default(),
            ContentVmLimits::default(),
            ContentGraphicsLimits::default(),
            PagePropertyLookupLimits::default(),
            GraphicsSceneLimits::default(),
        );
        match job.poll(&source, &cancellation) {
            ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                assert_eq!(error.code(), expected, "case {case}");
            }
            outcome => panic!("dash probe must terminate case {case}: {outcome:?}"),
        }
        assert!(
            source.snapshot_calls.load(Ordering::Acquire) >= 10,
            "case {case} must reach the post-256-entry guard"
        );
        assert!(
            job.vm_stats().peak_retained_bytes()
                >= job
                    .scan_stats()
                    .retained_bytes()
                    .saturating_add(dash_capacity(512)),
            "case {case} must report the admitted candidate allocation"
        );
    }
}

#[test]
fn equivalent_rectangle_and_matrix_formulations_have_equal_page_geometry() {
    let transformed = graphics_ready(b"2 0 0 2 10 20 cm 0 0 5 5 re f", 0x50);
    let direct = graphics_ready(b"10 20 10 10 re f", 0x51);
    let explicit = graphics_ready(b"10 20 m 20 20 l 20 30 l 10 30 l h f", 0x52);
    assert_eq!(
        normalized_fill_path(&transformed),
        normalized_fill_path(&direct)
    );
    assert_eq!(
        normalized_fill_path(&direct),
        normalized_fill_path(&explicit)
    );
}

#[test]
fn noncommuting_matrix_sequences_match_only_their_exact_composed_form() {
    let translated_then_scaled =
        graphics_ready(b"1 0 0 1 10 0 cm 2 0 0 2 0 0 cm 0 0 m 1 0 l S", 0x54);
    let translated_direct = graphics_ready(b"2 0 0 2 10 0 cm 0 0 m 1 0 l S", 0x55);
    let scaled_then_translated =
        graphics_ready(b"2 0 0 2 0 0 cm 1 0 0 1 10 0 cm 0 0 m 1 0 l S", 0x56);
    let scaled_direct = graphics_ready(b"2 0 0 2 20 0 cm 0 0 m 1 0 l S", 0x57);

    assert_eq!(
        normalized_stroke_path(&translated_then_scaled),
        normalized_stroke_path(&translated_direct)
    );
    assert_eq!(
        normalized_stroke_path(&scaled_then_translated),
        normalized_stroke_path(&scaled_direct)
    );
    assert_ne!(
        normalized_stroke_path(&translated_then_scaled),
        normalized_stroke_path(&scaled_then_translated)
    );
}

#[test]
fn current_path_applies_each_construction_time_ctm_without_retroactive_changes() {
    let page = graphics_ready(b"0 0 m 2 0 0 2 0 0 cm 1 0 l S", 0x53);
    let graphics = page.scene().graphics().expect("v2 graphics");
    let GraphicsCommand::Stroke {
        path,
        transform,
        style,
        ..
    } = graphics.commands()[0].command()
    else {
        panic!("fixture must stroke");
    };
    assert_eq!(*transform, Matrix::IDENTITY);
    assert_eq!(
        style.stroke_transform(),
        Matrix::new([
            SceneScalar::from_scaled(2_000_000_000),
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            SceneScalar::from_scaled(2_000_000_000),
            SceneScalar::ZERO,
            SceneScalar::ZERO,
        ])
    );
    let entry = graphics
        .resources()
        .iter()
        .find(|entry| entry.id() == *path)
        .expect("path resource");
    let GraphicsResource::Path(path) = entry.resource() else {
        panic!("resource must be path");
    };
    assert_eq!(
        path.segments(),
        [
            PathSegment::MoveTo(pdf_rs_scene::ScenePoint::new(
                SceneScalar::ZERO,
                SceneScalar::ZERO
            )),
            PathSegment::LineTo(pdf_rs_scene::ScenePoint::new(
                SceneScalar::from_scaled(2_000_000_000),
                SceneScalar::ZERO
            ))
        ]
    );
}

fn normalized_fill_path(page: &pdf_rs_content::InterpretedPage) -> Vec<PathSegment> {
    let graphics = page.scene().graphics().expect("v2 graphics");
    let GraphicsCommand::Fill {
        path, transform, ..
    } = graphics.commands()[0].command()
    else {
        panic!("fixture must fill");
    };
    let entry = graphics
        .resources()
        .iter()
        .find(|entry| entry.id() == *path)
        .expect("path resource");
    let GraphicsResource::Path(path) = entry.resource() else {
        panic!("resource must be path");
    };
    path.segments()
        .iter()
        .map(|segment| match *segment {
            PathSegment::MoveTo(point) => PathSegment::MoveTo(
                transform
                    .checked_transform_point(point)
                    .expect("fixture transform"),
            ),
            PathSegment::LineTo(point) => PathSegment::LineTo(
                transform
                    .checked_transform_point(point)
                    .expect("fixture transform"),
            ),
            PathSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => PathSegment::CubicTo {
                control_1: transform
                    .checked_transform_point(control_1)
                    .expect("fixture transform"),
                control_2: transform
                    .checked_transform_point(control_2)
                    .expect("fixture transform"),
                end: transform
                    .checked_transform_point(end)
                    .expect("fixture transform"),
            },
            PathSegment::ClosePath => PathSegment::ClosePath,
        })
        .collect()
}

fn normalized_stroke_path(page: &pdf_rs_content::InterpretedPage) -> Vec<PathSegment> {
    let graphics = page.scene().graphics().expect("v2 graphics");
    let GraphicsCommand::Stroke {
        path, transform, ..
    } = graphics.commands()[0].command()
    else {
        panic!("fixture must stroke");
    };
    let entry = graphics
        .resources()
        .iter()
        .find(|entry| entry.id() == *path)
        .expect("path resource");
    let GraphicsResource::Path(path) = entry.resource() else {
        panic!("resource must be path");
    };
    path.segments()
        .iter()
        .map(|segment| match *segment {
            PathSegment::MoveTo(point) => PathSegment::MoveTo(
                transform
                    .checked_transform_point(point)
                    .expect("fixture transform"),
            ),
            PathSegment::LineTo(point) => PathSegment::LineTo(
                transform
                    .checked_transform_point(point)
                    .expect("fixture transform"),
            ),
            PathSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => PathSegment::CubicTo {
                control_1: transform
                    .checked_transform_point(control_1)
                    .expect("fixture transform"),
                control_2: transform
                    .checked_transform_point(control_2)
                    .expect("fixture transform"),
                end: transform
                    .checked_transform_point(end)
                    .expect("fixture transform"),
            },
            PathSegment::ClosePath => PathSegment::ClosePath,
        })
        .collect()
}

#[test]
fn explicit_scene_limits_remain_independent_from_content_graphics_limits() {
    let input = acquire(b"0 0 1 1 re f 2 2 1 1 re f", 0x60);
    let scene_limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_commands: 1,
        ..GraphicsSceneLimitConfig::default()
    })
    .expect("scene limits");
    let mut job = InterpretPageJob::new_graphics_v2(
        input.acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
        PagePropertyLookupLimits::default(),
        scene_limits,
    );
    match job.poll(&input.store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Scene(error)) => {
            assert_eq!(error.code(), pdf_rs_scene::SceneErrorCode::ResourceLimit);
        }
        outcome => panic!("Scene command budget must remain independently enforced: {outcome:?}"),
    }
}

#[test]
fn failed_scene_append_still_reports_implicit_close_path_peak_before_handoff() {
    let input = acquire(b"0 0 m 1 0 l 2 0 l 3 0 l s", 0x61);
    let scene_limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_path_segments: 4,
        ..GraphicsSceneLimitConfig::default()
    })
    .expect("scene limits");
    let mut job = InterpretPageJob::new_graphics_v2(
        input.acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
        PagePropertyLookupLimits::default(),
        scene_limits,
    );
    match job.poll(&input.store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Scene(error)) => {
            assert_eq!(error.code(), pdf_rs_scene::SceneErrorCode::ResourceLimit);
        }
        outcome => panic!("Scene path budget must reject implicit close: {outcome:?}"),
    }
    let minimum_path_bytes =
        u64::try_from(std::mem::size_of::<PathSegment>() * 8).expect("path bytes");
    assert!(
        job.vm_stats().peak_retained_bytes()
            >= job
                .scan_stats()
                .retained_bytes()
                .saturating_add(minimum_path_bytes)
    );
}

#[test]
fn embedded_text_uses_pdf_widths_tj_and_exact_noncommuting_text_matrices() {
    let page = font_ready(
        b"2 3 4 5 6 7 cm BT /F0 10 Tf 1 Tc 50 Tz 1 Ts 1 2 3 4 5 6 Tm [(A) 100 (A)] TJ ET",
        0x81,
    );
    let graphics = page.scene().graphics().expect("graphics-v2 scene");
    assert_eq!(graphics.commands().len(), 1);
    let GraphicsCommand::DrawGlyphRun(run) = graphics.commands()[0].command() else {
        panic!("text must publish one glyph run");
    };
    assert_eq!(run.glyphs().len(), 2);
    assert_eq!(
        run.glyphs()[0].transform(),
        Matrix::new([
            SceneScalar::from_scaled(50_000_000_000),
            SceneScalar::from_scaled(65_000_000_000),
            SceneScalar::from_scaled(220_000_000_000),
            SceneScalar::from_scaled(290_000_000_000),
            SceneScalar::from_scaled(62_000_000_000),
            SceneScalar::from_scaled(81_000_000_000),
        ])
    );
    // PDF Widths gives 777, not the fixture hmtx advance 501:
    // ((777/1000*10)+Tc 1)*50% - (TJ 100/1000*10*50%) = 3.885. The second
    // transform is CTM x (Tm x Translate(3.885, 0)) x the text-render matrix.
    assert_eq!(
        run.glyphs()[1].transform(),
        Matrix::new([
            SceneScalar::from_scaled(50_000_000_000),
            SceneScalar::from_scaled(65_000_000_000),
            SceneScalar::from_scaled(220_000_000_000),
            SceneScalar::from_scaled(290_000_000_000),
            SceneScalar::from_scaled(100_850_000_000),
            SceneScalar::from_scaled(131_505_000_000),
        ])
    );
    assert_eq!(page.font_uses().len(), 1);
    assert_eq!(page.font_stats().font_uses(), 1);
    assert_eq!(page.font_stats().lookups(), 1);
    assert_eq!(page.font_stats().acquisitions(), 1);
    assert_eq!(page.font_stats().glyphs(), 2);
    assert!(page.font_stats().outline_segments() > 0);
    assert!(page.font_stats().object_read_bytes() > 0);
    assert!(page.font_stats().metadata_entries() > 0);
    assert_eq!(page.font_stats().widths(), 95);
    assert!(page.font_stats().font_input_bytes() > 0);
    assert!(page.font_stats().font_tables_visited() > 0);
    assert!(page.font_stats().font_path_segments() > 0);
    let shared = page.scene_arc();
    assert!(std::ptr::eq(page.scene(), shared.as_ref()));

    let outline_id = run.glyphs()[0].outline();
    let entry = graphics
        .resources()
        .iter()
        .find(|entry| entry.id() == outline_id)
        .expect("glyph outline resource");
    let GraphicsResource::GlyphOutline(outline) = entry.resource() else {
        panic!("run must reference a glyph outline");
    };
    assert_eq!(outline.units_per_em(), 1_000);
    assert_eq!(outline.source(), page.font_uses()[0].resource_source());
    assert_eq!(graphics.resources().len(), 1, "repeated A is interned once");
}

#[test]
fn q_restores_complete_text_parameters_but_not_text_matrices() {
    let first = font_support::foundational_font();
    let second = font_support::build_font(vec![Vec::new(), font_support::quadratic_glyph()]);
    let mut objects = embedded_font_objects(5, 6, 7, &first, 777);
    objects.extend(embedded_font_objects(8, 9, 10, &second, 333));
    let (mut job, store) = font_job_with_limits(
        b"BT /F0 10 Tf 1 Tc 2 Tw 50 Tz 7 TL 0 Tr 3 Ts 1 0 0 1 4 5 Tm \
          q /F1 20 Tf 10 Tc 20 Tw 200 Tz 30 TL 0 Tr 40 Ts 2 0 Td Q \
          ( A) Tj ET BT ( A) Tj T* (A) Tj ET",
        b"<< /Font << /F0 5 0 R /F1 8 0 R >> >>",
        &objects,
        0x82,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("q/Q text fixture must be ready: {outcome:?}"),
    };
    assert_eq!(page.font_uses().len(), 2);
    assert_eq!(page.font_uses()[0].font().target().number(), 5);
    assert_eq!(page.font_uses()[1].font().target().number(), 8);
    let graphics = page.scene().graphics().unwrap();
    assert!(matches!(
        graphics.commands()[0].command(),
        GraphicsCommand::Save
    ));
    assert!(matches!(
        graphics.commands()[1].command(),
        GraphicsCommand::Restore
    ));
    let GraphicsCommand::DrawGlyphRun(run) = graphics.commands()[2].command() else {
        panic!("restored text must draw");
    };
    assert_eq!(run.glyphs().len(), 2);
    assert_eq!(
        run.glyphs()[0].transform(),
        Matrix::new([
            SceneScalar::from_scaled(5_000_000_000),
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            SceneScalar::from_scaled(10_000_000_000),
            SceneScalar::from_scaled(6_000_000_000),
            SceneScalar::from_scaled(8_000_000_000),
        ])
    );
    assert_eq!(
        run.glyphs()[1].transform().components()[4],
        SceneScalar::from_scaled(10_500_000_000),
        "restored Tc/Tw/Tz must advance the leading space by 4.5"
    );
    let outline = graphics
        .resources()
        .iter()
        .find(|entry| entry.id() == run.glyphs()[0].outline())
        .unwrap();
    let GraphicsResource::GlyphOutline(outline) = outline.resource() else {
        panic!("glyph resource");
    };
    assert_eq!(outline.source().object().number(), 5);

    let GraphicsCommand::DrawGlyphRun(second_bt) = graphics.commands()[3].command() else {
        panic!("second BT must retain parameters")
    };
    assert_eq!(second_bt.glyphs().len(), 2);
    assert_eq!(
        second_bt.glyphs()[0].transform(),
        Matrix::new([
            SceneScalar::from_scaled(5_000_000_000),
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            SceneScalar::from_scaled(10_000_000_000),
            SceneScalar::ZERO,
            SceneScalar::from_scaled(3_000_000_000),
        ])
    );
    assert_eq!(
        second_bt.glyphs()[1].transform().components()[4],
        SceneScalar::from_scaled(4_500_000_000)
    );
    let GraphicsCommand::DrawGlyphRun(next_line) = graphics.commands()[4].command() else {
        panic!("T* must draw the next line")
    };
    assert_eq!(
        next_line.glyphs()[0].transform().components()[5],
        SceneScalar::from_scaled(-4_000_000_000),
        "second BT preserves leading 7 and rise 3 while resetting line matrices"
    );

    let (mut missing_job, missing_store) = default_font_job(b"BT q /F0 10 Tf Q (A) Tj ET", 0x83);
    match missing_job.poll(&missing_store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            assert_eq!(error.code(), ContentVmErrorCode::InvalidTextObject);
        }
        outcome => panic!("restoring no font selection must fail: {outcome:?}"),
    }
    assert_eq!(missing_job.font_stats().lookups(), 0);
    assert_eq!(missing_job.font_stats().acquisitions(), 0);
}

#[test]
fn text_parameters_set_before_bt_apply_inside_the_text_object() {
    let page = font_ready(b"/F0 10 Tf 1 Tc BT (AA) Tj ET", 0x84);
    let graphics = page.scene().graphics().expect("graphics-v2 scene");
    let GraphicsCommand::DrawGlyphRun(run) = graphics.commands()[0].command() else {
        panic!("text must publish one glyph run");
    };
    assert_eq!(run.glyphs().len(), 2);
    assert_eq!(
        run.glyphs()[0].transform().components()[4],
        SceneScalar::ZERO
    );
    assert_eq!(
        run.glyphs()[1].transform().components()[4],
        SceneScalar::from_scaled(8_770_000_000),
        "Tf and Tc set before BT must remain active for text showing"
    );
}

#[test]
fn bt_resets_only_text_matrices_and_quadratics_are_canonical_cubics() {
    let program = font_support::build_font(vec![Vec::new(), font_support::quadratic_glyph()]);
    let objects = embedded_font_objects(5, 6, 7, &program, 500);
    let (mut job, store) = font_job_with_limits(
        b"BT /F0 10 Tf 1 0 0 1 7 9 Tm ET BT (A) Tj ET",
        b"<< /Font << /F0 5 0 R >> >>",
        &objects,
        0x84,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("second BT reuses font but resets matrices: {outcome:?}"),
    };
    let graphics = page.scene().graphics().unwrap();
    let GraphicsCommand::DrawGlyphRun(run) = graphics.commands()[0].command() else {
        panic!("glyph run");
    };
    assert_eq!(
        run.glyphs()[0].transform().components()[4],
        SceneScalar::ZERO
    );
    assert_eq!(
        run.glyphs()[0].transform().components()[5],
        SceneScalar::ZERO
    );
    let resource = graphics
        .resources()
        .iter()
        .find(|entry| entry.id() == run.glyphs()[0].outline())
        .unwrap();
    let GraphicsResource::GlyphOutline(outline) = resource.resource() else {
        panic!("outline");
    };
    assert!(
        outline
            .outline()
            .segments()
            .iter()
            .any(|segment| matches!(segment, PathSegment::CubicTo { .. }))
    );
}

#[test]
fn text_render_mode_distinguishes_malformed_supported_and_clipping_modes() {
    for (value, expected) in [
        (-1, ContentVmErrorCode::InvalidGraphicsParameter),
        (8, ContentVmErrorCode::InvalidGraphicsParameter),
    ] {
        let content = format!("BT {value} Tr ET");
        let (mut job, store) =
            default_font_job(content.as_bytes(), 0x85_u8.wrapping_add(value as u8));
        match job.poll(&store, &DocumentNeverCancelled) {
            ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                assert_eq!(error.code(), expected)
            }
            outcome => panic!("out-of-range Tr must be malformed: {outcome:?}"),
        }
        assert_eq!(job.font_stats().lookups(), 0);
    }
    for value in 1..=3 {
        let content = format!("BT {value} Tr ET");
        let (mut job, store) = default_font_job(content.as_bytes(), 0x87 + value as u8);
        match job.poll(&store, &DocumentNeverCancelled) {
            ContentVmPoll::Ready(_) => {}
            outcome => panic!("registered non-clipping Tr {value} must execute: {outcome:?}"),
        }
        assert_eq!(job.font_stats().lookups(), 0);
    }
    for value in 4..=7 {
        let content = format!("BT {value} Tr ET");
        let (mut job, store) = default_font_job(content.as_bytes(), 0x87 + value as u8);
        match job.poll(&store, &DocumentNeverCancelled) {
            ContentVmPoll::Unsupported(error) => {
                assert_eq!(error.kind(), ContentUnsupportedKind::TextRenderMode);
            }
            outcome => panic!("text-clipping Tr {value} must be unsupported: {outcome:?}"),
        }
        assert_eq!(job.font_stats().lookups(), 0);
    }
}

#[test]
fn text_fill_stroke_stroke_and_invisible_modes_publish_exact_glyph_painting() {
    let page = font_ready(
        b"2 w 1 0 0 RG 0 0 1 rg BT /F0 10 Tf 2 Tr (A) Tj 3 Tr (A) Tj 1 Tr (A) Tj ET",
        0x8f,
    );
    let graphics = page.scene().graphics().expect("graphics-v2 page");
    assert_eq!(
        graphics.commands().len(),
        2,
        "invisible text advances without publishing a visible command"
    );

    let GraphicsCommand::DrawGlyphRun(fill_stroke) = graphics.commands()[0].command() else {
        panic!("mode 2 must publish a glyph run");
    };
    let GlyphPainting::FillStroke {
        fill,
        stroke,
        style,
    } = fill_stroke.painting()
    else {
        panic!("mode 2 must retain fill-then-stroke semantics");
    };
    assert_eq!(
        fill.color(),
        DeviceColor::Rgb {
            red: SceneUnit::ZERO,
            green: SceneUnit::ZERO,
            blue: SceneUnit::ONE,
        }
    );
    assert_eq!(
        stroke.color(),
        DeviceColor::Rgb {
            red: SceneUnit::ONE,
            green: SceneUnit::ZERO,
            blue: SceneUnit::ZERO,
        }
    );
    assert_eq!(style.width(), SceneScalar::from_scaled(2_000_000_000));

    let GraphicsCommand::DrawGlyphRun(stroke_only) = graphics.commands()[1].command() else {
        panic!("mode 1 must publish a glyph run");
    };
    assert!(matches!(
        stroke_only.painting(),
        GlyphPainting::Stroke { paint, style }
            if paint.color()
                == DeviceColor::Rgb {
                    red: SceneUnit::ONE,
                    green: SceneUnit::ZERO,
                    blue: SceneUnit::ZERO,
                }
                && style.width() == SceneScalar::from_scaled(2_000_000_000)
    ));
}

#[test]
fn td_next_line_quotes_empty_adjustments_and_winansi_boundaries_are_exact() {
    let page = font_ready(
        b"BT /F0 10 Tf 2 TL 3 -4 TD (A) Tj T* (A) Tj (A) ' 1 2 (A) \" \
          () Tj [] TJ [100] TJ (A) Tj ET",
        0x91,
    );
    let graphics = page.scene().graphics().unwrap();
    assert_eq!(graphics.commands().len(), 5);
    assert_eq!(
        graphics
            .commands()
            .iter()
            .map(|record| record.source().operator_index())
            .collect::<Vec<_>>(),
        [4, 6, 7, 8, 12],
        "empty strings and empty/adjustment-only TJ arrays publish no command"
    );
    let positions = graphics
        .commands()
        .iter()
        .map(|record| {
            let GraphicsCommand::DrawGlyphRun(run) = record.command() else {
                panic!("text fixture emits glyph runs")
            };
            let transform = run.glyphs()[0].transform().components();
            (transform[4], transform[5])
        })
        .collect::<Vec<_>>();
    assert_eq!(
        positions,
        [
            (
                SceneScalar::from_scaled(3_000_000_000),
                SceneScalar::from_scaled(-4_000_000_000)
            ),
            (
                SceneScalar::from_scaled(3_000_000_000),
                SceneScalar::from_scaled(-8_000_000_000)
            ),
            (
                SceneScalar::from_scaled(3_000_000_000),
                SceneScalar::from_scaled(-12_000_000_000)
            ),
            (
                SceneScalar::from_scaled(3_000_000_000),
                SceneScalar::from_scaled(-16_000_000_000)
            ),
            (
                SceneScalar::from_scaled(11_770_000_000),
                SceneScalar::from_scaled(-16_000_000_000)
            ),
        ]
    );
    assert_eq!(page.font_stats().text_bytes(), 5);
    assert_eq!(page.font_stats().text_adjustments(), 1);
    assert_eq!(page.font_stats().glyphs(), 5);

    let ascii = font_ready(b"BT /F0 10 Tf ( ~) Tj ET", 0x92);
    let GraphicsCommand::DrawGlyphRun(run) =
        ascii.scene().graphics().unwrap().commands()[0].command()
    else {
        panic!("ASCII endpoints draw")
    };
    assert_eq!(
        run.glyphs()
            .iter()
            .map(|glyph| glyph.character_code())
            .collect::<Vec<_>>(),
        [0x20, 0x7e]
    );

    let mut control = b"BT /F0 10 Tf (".to_vec();
    control.push(0x1f);
    control.extend_from_slice(b") Tj ET");
    let (mut job, store) = default_font_job(&control, 0x93);
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Unsupported(error) => {
            assert_eq!(error.kind(), ContentUnsupportedKind::TextEncoding)
        }
        outcome => panic!("control byte must be unsupported before lookup: {outcome:?}"),
    }
    assert_eq!(job.font_stats().lookups(), 0);
    assert_eq!(job.font_stats().acquisitions(), 0);

    let mut extended = b"BT /F0 10 Tf (".to_vec();
    extended.push(0x80);
    extended.extend_from_slice(b") Tj ET");
    let objects = complete_winansi_font_objects(5, 6, 7, &font_support::foundational_font(), 777);
    let (mut job, store) = font_job_with_limits(
        &extended,
        b"<< /Font << /F0 5 0 R >> >>",
        &objects,
        0x94,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("extended WinAnsi byte must render: {outcome:?}"),
    };
    let GraphicsCommand::DrawGlyphRun(run) =
        page.scene().graphics().unwrap().commands()[0].command()
    else {
        panic!("extended WinAnsi emits one glyph run")
    };
    assert_eq!(run.glyphs()[0].character_code(), 0x80);
}

#[test]
fn font_payload_pending_does_not_replan_lookup_or_publish_partial_scene() {
    let (mut job, store) = default_font_job(b"BT /F0 10 Tf (A) Tj ET", 0x88);
    let missing = RangeStore::new(store.snapshot(), Default::default()).unwrap();
    let source = BlockPayloadAfter {
        complete: &store,
        missing: &missing,
        checkpoint: ResumeCheckpoint::new(32_008),
        admitted_payload_polls: 0,
        payload_polls: AtomicUsize::new(0),
    };
    match job.poll(&source, &DocumentNeverCancelled) {
        ContentVmPoll::Pending { checkpoint, .. } => {
            assert_eq!(checkpoint, ResumeCheckpoint::new(32_008));
        }
        outcome => panic!("font payload must suspend: {outcome:?}"),
    }
    let planned = job.font_stats();
    assert_eq!(planned.lookups(), 1);
    assert_eq!(planned.acquisitions(), 0);
    assert_eq!(planned.execution_passes(), 0);
    assert_eq!(planned.font_uses(), 0);

    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("complete source resumes one atomic publication: {outcome:?}"),
    };
    assert_eq!(page.font_stats().lookups(), 1);
    assert_eq!(
        page.font_stats().planning_operators(),
        planned.planning_operators()
    );
    assert_eq!(page.font_stats().acquisitions(), 1);
    assert_eq!(page.font_stats().execution_passes(), 1);
    assert_eq!(page.font_uses().len(), 1);
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(replay) => assert!(Arc::ptr_eq(&page, &replay)),
        outcome => panic!("ready must replay: {outcome:?}"),
    }
}

#[test]
fn every_font_checkpoint_resumes_the_same_plan_lookup_and_terminal_arc() {
    let mut program = font_support::foundational_font();
    program.resize(5_000, 0);
    for (offset, checkpoint_value) in (32_002_u64..=32_008).enumerate() {
        let mut objects = embedded_font_objects(5, 6, 7, &program, 777);
        let resumes_ready = match checkpoint_value {
            32_003 => {
                objects = vec![(5, font_program_object(5, &vec![0; 5_000]))];
                false
            }
            32_005 => {
                let descriptor = objects.iter_mut().find(|(number, _)| *number == 6).unwrap();
                descriptor.1 = font_program_object(6, &vec![0; 5_000]);
                false
            }
            _ => true,
        };
        let (mut job, store) = font_job_with_limits(
            b"BT /F0 10 Tf (A) Tj ET",
            b"<< /Font << /F0 5 0 R >> >>",
            &objects,
            0xb2_u8.wrapping_add(offset as u8),
            ContentVmLimits::default(),
            ContentFontLimits::default(),
            GraphicsSceneLimits::default(),
        );
        let missing = RangeStore::new(store.snapshot(), Default::default()).unwrap();
        let checkpoint = ResumeCheckpoint::new(checkpoint_value);
        let source = BlockPayloadAfter {
            complete: &store,
            missing: &missing,
            checkpoint,
            admitted_payload_polls: 0,
            payload_polls: AtomicUsize::new(0),
        };
        match job.poll(&source, &DocumentNeverCancelled) {
            ContentVmPoll::Pending {
                checkpoint: actual, ..
            } => assert_eq!(actual, checkpoint),
            outcome => panic!("Font checkpoint {checkpoint_value} must suspend: {outcome:?}"),
        }
        let pending = job.font_stats();
        assert_eq!(pending.lookups(), 1);
        assert_eq!(pending.acquisitions(), 0);
        assert_eq!(pending.execution_passes(), 0);
        assert_eq!(job.font_lookup_stats().lookups(), 1);

        if resumes_ready {
            let page = match job.poll(&store, &DocumentNeverCancelled) {
                ContentVmPoll::Ready(page) => page,
                outcome => panic!("Font checkpoint {checkpoint_value} must resume: {outcome:?}"),
            };
            assert_eq!(
                page.font_stats().planning_operators(),
                pending.planning_operators()
            );
            assert_eq!(page.font_stats().lookups(), 1);
            assert_eq!(page.font_stats().acquisitions(), 1);
            assert_eq!(page.font_stats().execution_passes(), 1);
            assert_eq!(page.font_lookup_stats().lookups(), 1);
            match job.poll(&missing, &AlwaysCancelled) {
                ContentVmPoll::Ready(replay) => assert!(Arc::ptr_eq(&page, &replay)),
                outcome => panic!("terminal Font replay must do no source work: {outcome:?}"),
            }
        } else {
            assert!(matches!(
                job.poll(&store, &DocumentNeverCancelled),
                ContentVmPoll::Failed(ContentVmFailure::Document(_))
            ));
            let failed = job.font_stats();
            assert_eq!(failed.planning_operators(), pending.planning_operators());
            assert_eq!(failed.lookups(), 1);
            assert_eq!(failed.acquisitions(), 0);
            assert_eq!(failed.execution_passes(), 0);
            assert!(matches!(
                job.poll(&missing, &AlwaysCancelled),
                ContentVmPoll::Failed(ContentVmFailure::Document(_))
            ));
            assert_eq!(job.font_stats(), failed);
        }
    }
}

#[test]
fn two_font_alias_cache_aggregates_every_lower_stat_and_resource_atomically() {
    let first_program = font_support::foundational_font();
    let second_program =
        font_support::build_font(vec![Vec::new(), font_support::contour_glyph(&[true; 128])]);
    let first = one_font_acquisition_stats(5, 6, 7, &first_program, 777, 0xa0);
    let second = one_font_acquisition_stats(8, 9, 10, &second_program, 333, 0xa1);

    let (mut job, store) = two_distinct_font_job(
        0xa2,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
    );
    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("two distinct Fonts and one alias must publish: {outcome:?}"),
    };
    let stats = page.font_stats();
    assert_eq!(stats.font_uses(), 3);
    assert_eq!(stats.lookups(), 3);
    assert_eq!(stats.cache_hits(), 1);
    assert_eq!(stats.acquisitions(), 2);
    assert_eq!(stats.unique_fonts(), 2);
    assert_eq!(
        page.font_uses()
            .iter()
            .map(|usage| usage.font().target().number())
            .collect::<Vec<_>>(),
        [5, 5, 8]
    );
    assert_eq!(page.scene().graphics().unwrap().resources().len(), 2);

    macro_rules! assert_sum {
        ($($getter:ident),+ $(,)?) => {$ (
            assert_eq!(
                stats.$getter(),
                first.$getter().checked_add(second.$getter()).unwrap(),
                concat!("aggregate ", stringify!($getter))
            );
        )+ };
    }
    assert_sum!(
        resource_polls,
        resource_objects,
        resource_reference_edges,
        object_read_bytes,
        object_parse_bytes,
        metadata_entries,
        widths,
        encoded_bytes,
        decoded_bytes,
        decode_fuel,
        resource_retained_bytes,
        font_input_bytes,
        font_tables_visited,
        font_glyph_descriptions,
        font_cmap_segments,
        font_glyph_data_bytes,
        font_source_contours,
        font_source_points,
        font_components,
        font_path_segments,
        font_fuel,
        font_retained_bytes,
    );
    assert_eq!(
        stats.peak_font_retained_bytes(),
        first
            .peak_font_retained_bytes()
            .max(second.peak_font_retained_bytes())
    );
    assert_eq!(
        stats.peak_acquisition_retained_bytes(),
        first
            .peak_acquisition_retained_bytes()
            .max(second.peak_acquisition_retained_bytes())
    );
}

#[test]
fn every_content_font_budget_accepts_exact_and_rejects_one_less_with_failure_peaks() {
    let first_program = font_support::foundational_font();
    let second_program =
        font_support::build_font(vec![Vec::new(), font_support::contour_glyph(&[true; 128])]);
    let first_resource = one_font_acquisition_stats(5, 6, 7, &first_program, 777, 0xc0);
    let second_resource = one_font_acquisition_stats(8, 9, 10, &second_program, 333, 0xc1);
    assert!(
        second_resource.peak_acquisition_retained_bytes()
            > first_resource.peak_acquisition_retained_bytes()
    );
    assert!(second_resource.peak_font_retained_bytes() > first_resource.peak_font_retained_bytes());
    let (mut measured_job, measured_store) = two_distinct_font_job(
        0xa3,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
    );
    let measured = match measured_job.poll(&measured_store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page.font_stats(),
        outcome => panic!("Font budget measurement must publish: {outcome:?}"),
    };
    for value in [
        measured.font_uses(),
        measured.unique_fonts(),
        measured.resource_retained_bytes(),
        measured.glyphs(),
        measured.outline_segments(),
        measured.peak_glyph_retained_bytes(),
        measured.text_bytes(),
        measured.text_adjustments(),
        measured.planning_operators(),
        measured.cache_probes(),
        measured.peak_plan_retained_bytes(),
        measured.peak_cache_retained_bytes(),
        measured.acquisition_polls(),
    ] {
        assert!(value > 1, "one-less runtime limit must remain valid");
    }
    let exact = ContentFontLimits::validate(ContentFontLimitConfig {
        max_font_uses: measured.font_uses(),
        max_unique_fonts: measured.unique_fonts(),
        max_resource_retained_bytes: measured.resource_retained_bytes(),
        max_glyphs: measured.glyphs(),
        max_outline_segments: measured.outline_segments(),
        max_glyph_retained_bytes: measured.peak_glyph_retained_bytes(),
        max_text_bytes: measured.text_bytes(),
        max_text_adjustments: measured.text_adjustments(),
        max_planning_operators: measured.planning_operators(),
        max_cache_probes: measured.cache_probes(),
        max_plan_retained_bytes: measured.peak_plan_retained_bytes(),
        max_cache_retained_bytes: measured.peak_cache_retained_bytes(),
        max_acquisition_polls: measured.acquisition_polls(),
    })
    .unwrap();
    let (mut exact_job, exact_store) =
        two_distinct_font_job(0xa4, ContentVmLimits::default(), exact);
    match exact_job.poll(&exact_store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => assert_eq!(page.font_stats(), measured),
        outcome => panic!("all exact Content Font budgets must publish: {outcome:?}"),
    }

    macro_rules! reject_one_less {
        ($salt:expr, $field:ident, $value:expr, $kind:expr) => {{
            let limits = font_limits(|config| config.$field = $value - 1);
            let (mut job, store) = two_distinct_font_job($salt, ContentVmLimits::default(), limits);
            match job.poll(&store, &DocumentNeverCancelled) {
                ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                    let limit = error.font_limit().expect("Content Font limit context");
                    assert_eq!(limit.kind(), $kind);
                    assert_eq!(limit.limit(), $value - 1);
                }
                outcome => panic!("one-less Content Font budget must fail: {outcome:?}"),
            }
            job
        }};
    }

    let uses = reject_one_less!(
        0xa5,
        max_font_uses,
        measured.font_uses(),
        ContentFontLimitKind::FontUses
    );
    assert_eq!(uses.font_stats().lookups(), 0);
    let unique = reject_one_less!(
        0xa6,
        max_unique_fonts,
        measured.unique_fonts(),
        ContentFontLimitKind::UniqueFonts
    );
    assert_eq!(unique.font_stats().acquisitions(), 0);
    let resource = reject_one_less!(
        0xa7,
        max_resource_retained_bytes,
        measured.resource_retained_bytes(),
        ContentFontLimitKind::ResourceRetainedBytes
    );
    assert_eq!(resource.font_stats().acquisitions(), 1);
    assert_eq!(resource.font_stats().unique_fonts(), 1);
    macro_rules! assert_first_resource_sum {
        ($($getter:ident),+ $(,)?) => {$ (
            assert_eq!(
                resource.font_stats().$getter(),
                first_resource.$getter(),
                concat!("failed second resource must not publish ", stringify!($getter))
            );
        )+ };
    }
    assert_first_resource_sum!(
        resource_polls,
        resource_objects,
        resource_reference_edges,
        object_read_bytes,
        object_parse_bytes,
        metadata_entries,
        widths,
        encoded_bytes,
        decoded_bytes,
        decode_fuel,
        resource_retained_bytes,
        font_input_bytes,
        font_tables_visited,
        font_glyph_descriptions,
        font_cmap_segments,
        font_glyph_data_bytes,
        font_source_contours,
        font_source_points,
        font_components,
        font_path_segments,
        font_fuel,
        font_retained_bytes,
    );
    assert_eq!(
        resource.font_stats().peak_acquisition_retained_bytes(),
        second_resource.peak_acquisition_retained_bytes(),
        "failed acquired resource contributes its lower acquisition peak"
    );
    assert_eq!(
        resource.font_stats().peak_font_retained_bytes(),
        second_resource.peak_font_retained_bytes(),
        "failed acquired resource contributes its lower parser peak"
    );
    let before_replay = resource.font_stats();
    let mut resource = resource;
    let replay_store = RangeStore::new(measured_store.snapshot(), Default::default()).unwrap();
    assert!(matches!(
        resource.poll(&replay_store, &AlwaysCancelled),
        ContentVmPoll::Failed(_)
    ));
    assert_eq!(resource.font_stats(), before_replay);

    let glyphs = reject_one_less!(
        0xa8,
        max_glyphs,
        measured.glyphs(),
        ContentFontLimitKind::Glyphs
    );
    assert_eq!(glyphs.font_stats().glyphs(), 2);
    let segments = reject_one_less!(
        0xa9,
        max_outline_segments,
        measured.outline_segments(),
        ContentFontLimitKind::OutlineSegments
    );
    assert!(segments.font_stats().outline_segments() < measured.outline_segments());
    let glyph_retained = reject_one_less!(
        0xaa,
        max_glyph_retained_bytes,
        measured.peak_glyph_retained_bytes(),
        ContentFontLimitKind::GlyphRetainedBytes
    );
    assert!(glyph_retained.font_stats().peak_glyph_retained_bytes() > 0);
    assert!(
        glyph_retained.font_stats().peak_glyph_retained_bytes()
            < measured.peak_glyph_retained_bytes(),
        "the rejected second candidate cannot erase the first published failure peak"
    );
    let text = reject_one_less!(
        0xab,
        max_text_bytes,
        measured.text_bytes(),
        ContentFontLimitKind::TextBytes
    );
    assert_eq!(text.font_stats().lookups(), 0);
    let adjustments = reject_one_less!(
        0xac,
        max_text_adjustments,
        measured.text_adjustments(),
        ContentFontLimitKind::TextAdjustments
    );
    assert_eq!(adjustments.font_stats().lookups(), 0);
    let planning = reject_one_less!(
        0xad,
        max_planning_operators,
        measured.planning_operators(),
        ContentFontLimitKind::PlanningOperators
    );
    assert_eq!(planning.font_stats().lookups(), 0);
    let probes = reject_one_less!(
        0xae,
        max_cache_probes,
        measured.cache_probes(),
        ContentFontLimitKind::CacheProbes
    );
    assert_eq!(probes.font_stats().acquisitions(), 0);
    let plan = reject_one_less!(
        0xaf,
        max_plan_retained_bytes,
        measured.peak_plan_retained_bytes(),
        ContentFontLimitKind::PlanRetainedBytes
    );
    assert_eq!(plan.font_stats().lookups(), 0);
    let cache = reject_one_less!(
        0xb0,
        max_cache_retained_bytes,
        measured.peak_cache_retained_bytes(),
        ContentFontLimitKind::CacheRetainedBytes
    );
    assert_eq!(cache.font_stats().acquisitions(), 0);
    let polls = reject_one_less!(
        0xb1,
        max_acquisition_polls,
        measured.acquisition_polls(),
        ContentFontLimitKind::AcquisitionPolls
    );
    assert_eq!(polls.font_stats().acquisitions(), 1);
    assert_eq!(polls.font_stats().execution_passes(), 0);
}

#[test]
fn tf_names_and_nested_tj_strings_have_exact_byte_fuel_boundaries() {
    let text = vec![b'A'; 2_048];
    let mut tj_content = b"BT /F0 10 Tf [(".to_vec();
    tj_content.extend_from_slice(&text);
    tj_content.extend_from_slice(b")] TJ ET");
    let (mut measured_tj, store) = default_font_job(&tj_content, 0xc2);
    let tj_total_fuel = match measured_tj.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page.vm_stats().fuel(),
        outcome => panic!("large nested TJ string must publish: {outcome:?}"),
    };
    let tj_through_show = tj_total_fuel - 1;

    let (mut exact_tj, store) = foundational_font_job(
        &tj_content,
        0xc3,
        vm_limits(|config| config.max_fuel = tj_through_show),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    match exact_tj.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.limit().expect("ET fuel limit");
            assert_eq!(limit.kind(), ContentVmLimitKind::Fuel);
            assert_eq!(limit.consumed(), tj_through_show);
            assert_eq!(limit.attempted(), 1);
        }
        outcome => panic!("exact TJ byte fuel must reach the following ET: {outcome:?}"),
    }
    assert_eq!(exact_tj.font_stats().text_bytes(), 2_048);
    assert_eq!(exact_tj.font_stats().lookups(), 0);

    let (mut short_tj, store) = default_font_job(b"BT /F0 10 Tf [(A)] TJ ET", 0xc4);
    let short_tj_fuel = match short_tj.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page.vm_stats().fuel(),
        outcome => panic!("short TJ must publish: {outcome:?}"),
    };
    assert_eq!(tj_total_fuel - short_tj_fuel, 2_047);

    let (mut one_less_tj, store) = foundational_font_job(
        &tj_content,
        0xc5,
        vm_limits(|config| config.max_fuel = tj_through_show - 1),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    match one_less_tj.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.limit().expect("TJ byte fuel limit");
            assert_eq!(limit.kind(), ContentVmLimitKind::Fuel);
            assert_eq!(limit.limit(), tj_through_show - 1);
            assert_eq!(limit.attempted(), 2_048);
        }
        outcome => panic!("one-less nested TJ byte fuel must reject: {outcome:?}"),
    }
    assert_eq!(one_less_tj.font_stats().text_bytes(), 0);
    assert_eq!(one_less_tj.font_stats().lookups(), 0);

    let long_name = vec![b'N'; 2_048];
    let mut tf_content = b"BT /".to_vec();
    tf_content.extend_from_slice(&long_name);
    tf_content.extend_from_slice(b" 10 Tf ET");
    let resources = {
        let mut value = b"<< /Font << /".to_vec();
        value.extend_from_slice(&long_name);
        value.extend_from_slice(b" 5 0 R >> >>");
        value
    };
    let objects = embedded_font_objects(5, 6, 7, &font_support::foundational_font(), 777);
    let (mut measured_tf, store) = font_job_with_limits(
        &tf_content,
        &resources,
        &objects,
        0xc6,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    let tf_total_fuel = match measured_tf.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page.vm_stats().fuel(),
        outcome => panic!("large Tf name must publish: {outcome:?}"),
    };
    let tf_through_select = tf_total_fuel - 1;
    let mut short_resources = b"<< /Font << /F 5 0 R >> >>".to_vec();
    let (mut short_tf, store) = font_job_with_limits(
        b"BT /F 10 Tf ET",
        &short_resources,
        &objects,
        0xc7,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    let short_tf_fuel = match short_tf.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page.vm_stats().fuel(),
        outcome => panic!("short Tf name must publish: {outcome:?}"),
    };
    assert_eq!(tf_total_fuel - short_tf_fuel, 2_047);
    short_resources.clear();

    for (salt, limit, exact) in [
        (0xc8, tf_through_select, true),
        (0xc9, tf_through_select - 1, false),
    ] {
        let (mut job, store) = font_job_with_limits(
            &tf_content,
            &resources,
            &objects,
            salt,
            vm_limits(|config| config.max_fuel = limit),
            ContentFontLimits::default(),
            GraphicsSceneLimits::default(),
        );
        match job.poll(&store, &DocumentNeverCancelled) {
            ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                let fuel = error.limit().expect("Tf/ET fuel limit");
                assert_eq!(fuel.kind(), ContentVmLimitKind::Fuel);
                if exact {
                    assert_eq!(fuel.consumed(), tf_through_select);
                    assert_eq!(fuel.attempted(), 1);
                } else {
                    assert_eq!(fuel.limit(), tf_through_select - 1);
                    assert_eq!(fuel.consumed(), 1);
                    assert_eq!(fuel.attempted(), 2_051);
                }
            }
            outcome => panic!("Tf byte-fuel boundary must reject deterministically: {outcome:?}"),
        }
        assert_eq!(job.font_stats().lookups(), 0);
    }
}

#[test]
fn combined_text_plan_font_use_and_saved_parameter_stack_retention_is_exact() {
    let mut deep = b"BT /F0 10 Tf ".to_vec();
    for _ in 0..64 {
        deep.extend_from_slice(b"q ");
    }
    for _ in 0..64 {
        deep.extend_from_slice(b"Q ");
    }
    deep.extend_from_slice(b"ET");

    let flat = b"BT /F0 10 Tf ET".to_vec();
    let (mut flat_job, store) = default_font_job(&flat, 0xca);
    let flat_peak = match flat_job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page.vm_stats().peak_retained_bytes(),
        outcome => panic!("flat text must publish: {outcome:?}"),
    };

    let (mut measured, store) = default_font_job(&deep, 0xcb);
    let missing = RangeStore::new(store.snapshot(), Default::default()).unwrap();
    let blocker = BlockPayloadAfter {
        complete: &store,
        missing: &missing,
        checkpoint: ResumeCheckpoint::new(32_008),
        admitted_payload_polls: 0,
        payload_polls: AtomicUsize::new(0),
    };
    assert!(matches!(
        measured.poll(&blocker, &DocumentNeverCancelled),
        ContentVmPoll::Pending { .. }
    ));
    let plan_only_peak = measured.vm_stats().peak_retained_bytes();
    let peak = match measured.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => {
            assert_eq!(page.vm_stats().max_graphics_state_depth(), 64);
            assert_eq!(page.font_stats().glyphs(), 0);
            page.vm_stats().peak_retained_bytes()
        }
        outcome => panic!("deep saved text stack must publish: {outcome:?}"),
    };
    assert!(
        peak > flat_peak,
        "saved text parameters remain live with the text plan"
    );
    assert_eq!(
        peak, plan_only_peak,
        "planning and materialization independently reserve the same measured saved-state peak"
    );

    let (mut exact, store) = foundational_font_job(
        &deep,
        0xcc,
        vm_limits(|config| config.max_retained_bytes = peak),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    match exact.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => assert_eq!(page.vm_stats().peak_retained_bytes(), peak),
        outcome => panic!("exact combined VM retained budget must publish: {outcome:?}"),
    }

    let (mut one_less, store) = foundational_font_job(
        &deep,
        0xcd,
        vm_limits(|config| config.max_retained_bytes = peak - 1),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    match one_less.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
            let limit = error.limit().expect("combined retained limit");
            assert_eq!(limit.kind(), ContentVmLimitKind::RetainedBytes);
            assert_eq!(limit.limit(), peak - 1);
        }
        outcome => panic!("one-less combined VM retained budget must fail: {outcome:?}"),
    }
    assert_eq!(one_less.font_stats().acquisitions(), 0);
    assert_eq!(one_less.font_stats().execution_passes(), 0);
    assert!(one_less.vm_stats().peak_retained_bytes() < peak);

    let scene_limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_state_depth: 63,
        ..GraphicsSceneLimitConfig::default()
    })
    .unwrap();
    let (mut scene_failed, store) = foundational_font_job(
        &deep,
        0xe3,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        scene_limits,
    );
    assert!(matches!(
        scene_failed.poll(&store, &DocumentNeverCancelled),
        ContentVmPoll::Failed(ContentVmFailure::Scene(_))
    ));
    assert_eq!(scene_failed.font_stats().acquisitions(), 1);
    assert_eq!(scene_failed.font_stats().execution_passes(), 1);
    assert_eq!(
        scene_failed.vm_stats().peak_retained_bytes(),
        peak,
        "a later Scene failure preserves actual materialization font-use and saved-stack capacity"
    );
}

#[test]
fn known_text_glyph_outline_and_scene_limits_precede_expensive_work_but_keep_failure_peaks() {
    let mut malformed = b"BT /F0 10 Tf (".to_vec();
    malformed.extend(std::iter::repeat_n(b'A', 2_048));
    malformed.push(0x7f);
    malformed.extend_from_slice(b") Tj ET");
    let (mut text_limited, store) = foundational_font_job(
        &malformed,
        0xce,
        ContentVmLimits::default(),
        font_limits(|config| config.max_text_bytes = 2_048),
        GraphicsSceneLimits::default(),
    );
    match text_limited.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => assert_eq!(
            error.font_limit().expect("text limit").kind(),
            ContentFontLimitKind::TextBytes
        ),
        outcome => panic!("known text count must reject before encoding validation: {outcome:?}"),
    }
    assert_eq!(text_limited.font_stats().lookups(), 0);
    assert_eq!(text_limited.font_stats().acquisitions(), 0);

    let mut huge_text = b"BT /F0 10 Tf (".to_vec();
    huge_text.extend(std::iter::repeat_n(b'A', 2_048));
    huge_text.extend_from_slice(b") Tj ET");
    let (mut glyph_limited, store) = foundational_font_job(
        &huge_text,
        0xcf,
        ContentVmLimits::default(),
        font_limits(|config| config.max_glyphs = 1),
        GraphicsSceneLimits::default(),
    );
    match glyph_limited.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => assert_eq!(
            error.font_limit().expect("glyph limit").kind(),
            ContentFontLimitKind::Glyphs
        ),
        outcome => panic!("known glyph count must reject before mapping: {outcome:?}"),
    }
    assert_eq!(glyph_limited.font_stats().acquisitions(), 1);
    assert_eq!(glyph_limited.font_stats().execution_passes(), 1);
    assert_eq!(glyph_limited.font_stats().glyphs(), 0);
    assert_eq!(glyph_limited.font_stats().peak_glyph_retained_bytes(), 0);

    let large_program = font_support::build_font(vec![
        Vec::new(),
        font_support::contour_glyph(&[true; 1_024]),
    ]);
    let large_objects = embedded_font_objects(5, 6, 7, &large_program, 777);
    let (mut measured_outline, store) = font_job_with_limits(
        b"BT /F0 10 Tf (A) Tj ET",
        b"<< /Font << /F0 5 0 R >> >>",
        &large_objects,
        0xd0,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    let outline_segments = match measured_outline.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page.font_stats().outline_segments(),
        outcome => panic!("large outline measurement must publish: {outcome:?}"),
    };
    let (mut outline_limited, store) = font_job_with_limits(
        b"BT /F0 10 Tf (A) Tj ET",
        b"<< /Font << /F0 5 0 R >> >>",
        &large_objects,
        0xd1,
        ContentVmLimits::default(),
        font_limits(|config| config.max_outline_segments = outline_segments - 1),
        GraphicsSceneLimits::default(),
    );
    match outline_limited.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => assert_eq!(
            error.font_limit().expect("outline limit").kind(),
            ContentFontLimitKind::OutlineSegments
        ),
        outcome => panic!("known outline count must reject before copying: {outcome:?}"),
    }
    assert_eq!(outline_limited.font_stats().outline_segments(), 0);
    assert_eq!(outline_limited.font_stats().peak_glyph_retained_bytes(), 0);

    let scene_limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_glyphs: 1,
        ..GraphicsSceneLimitConfig::default()
    })
    .unwrap();
    let (mut scene_failed, store) = foundational_font_job(
        b"BT /F0 10 Tf (AA) Tj ET",
        0xd2,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        scene_limits,
    );
    assert!(matches!(
        scene_failed.poll(&store, &DocumentNeverCancelled),
        ContentVmPoll::Failed(ContentVmFailure::Scene(_))
    ));
    assert_eq!(scene_failed.font_stats().glyphs(), 0);
    assert!(scene_failed.font_stats().peak_glyph_retained_bytes() > 0);
    assert!(
        scene_failed.vm_stats().peak_retained_bytes()
            >= scene_failed
                .scan_stats()
                .retained_bytes()
                .saturating_add(scene_failed.font_stats().peak_glyph_retained_bytes())
    );

    let unsupported_objects = embedded_font_objects(5, 6, 7, &[0; 512], 777);
    let (mut unsupported_font, store) = font_job_with_limits(
        b"BT /F0 10 Tf (A) Tj ET",
        b"<< /Font << /F0 5 0 R >> >>",
        &unsupported_objects,
        0xd3,
        ContentVmLimits::default(),
        ContentFontLimits::default(),
        GraphicsSceneLimits::default(),
    );
    match unsupported_font.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Unsupported(error) => {
            assert_eq!(error.kind(), ContentUnsupportedKind::FontResource);
            let lower = error.font_resource().expect("lower Font capability");
            assert_eq!(lower.kind(), FontResourceUnsupportedKind::TrueTypeProgram);
            assert_eq!(
                lower
                    .font_unsupported()
                    .expect("lower sfnt capability")
                    .kind(),
                pdf_rs_font::FontUnsupportedKind::SfntFlavor
            );
        }
        outcome => {
            panic!("unsupported sfnt must terminate after plan/cache allocation: {outcome:?}")
        }
    }
    assert!(unsupported_font.font_stats().peak_plan_retained_bytes() > 0);
    assert!(unsupported_font.font_stats().peak_cache_retained_bytes() > 0);
    assert_eq!(unsupported_font.font_stats().acquisitions(), 0);
}

#[test]
fn huge_tf_and_tj_copy_guards_publish_vm_peaks_and_prioritize_source_change() {
    let objects = embedded_font_objects(5, 6, 7, &font_support::foundational_font(), 777);
    let large_text = vec![b'A'; 2_048];
    let mut tj_content = b"BT /F0 10 Tf (".to_vec();
    tj_content.extend_from_slice(&large_text);
    tj_content.extend_from_slice(b") Tj ET");

    let large_name = vec![b'N'; 2_048];
    let mut tf_content = b"BT /".to_vec();
    tf_content.extend_from_slice(&large_name);
    tf_content.extend_from_slice(b" 10 Tf ET");
    let mut tf_resources = b"<< /Font << /".to_vec();
    tf_resources.extend_from_slice(&large_name);
    tf_resources.extend_from_slice(b" 5 0 R >> >>");

    for (fixture_index, (label, content, resources, copied_bytes)) in [
        (
            "Tj",
            tj_content,
            b"<< /Font << /F0 5 0 R >> >>".to_vec(),
            2_048_u64,
        ),
        ("Tf", tf_content, tf_resources, 2_048_u64),
    ]
    .into_iter()
    .enumerate()
    {
        let expected_ordinal = if label == "Tj" { 2 } else { 1 };
        for (case_index, (change_source, expected)) in [
            (false, ContentVmErrorCode::Cancelled),
            (true, ContentVmErrorCode::SourceSnapshotMismatch),
        ]
        .into_iter()
        .enumerate()
        {
            let mut observed = false;
            for trigger in 1..256 {
                let salt = 0xd4_u8.wrapping_add((fixture_index * 2 + case_index) as u8);
                let (mut job, store) = font_job_with_limits(
                    &content,
                    &resources,
                    &objects,
                    salt,
                    ContentVmLimits::default(),
                    ContentFontLimits::default(),
                    GraphicsSceneLimits::default(),
                );
                let original = store.snapshot();
                let source = ChangingStoreSource {
                    complete: &store,
                    replacement: snapshot(
                        original.len().expect("fixture length"),
                        salt.wrapping_add(0x40),
                    ),
                    changed: AtomicBool::new(false),
                    snapshot_calls: AtomicUsize::new(0),
                };
                let cancellation = CancelDuringStore {
                    source: &source,
                    trigger_snapshot_call: trigger,
                    change_source,
                };
                let outcome = job.poll(&source, &cancellation);
                let copied_peak = job.vm_stats().peak_retained_bytes()
                    >= job
                        .scan_stats()
                        .retained_bytes()
                        .saturating_add(copied_bytes);
                if matches!(
                    outcome,
                    ContentVmPoll::Failed(ContentVmFailure::Vm(error))
                        if error.code() == expected
                            && error.source().is_some_and(|source| {
                                source.page_operator_ordinal() == expected_ordinal
                            })
                ) && copied_peak
                    && job.font_stats().lookups() == 0
                    && job.font_stats().acquisitions() == 0
                {
                    let terminal_vm = job.vm_stats();
                    let terminal_font = job.font_stats();
                    let missing = RangeStore::new(original, Default::default()).unwrap();
                    match job.poll(&missing, &AlwaysCancelled) {
                        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                            assert_eq!(error.code(), expected, "terminal {label} replay")
                        }
                        replay => panic!("terminal {label} replay must do no work: {replay:?}"),
                    }
                    assert_eq!(job.vm_stats(), terminal_vm);
                    assert_eq!(job.font_stats(), terminal_font);
                    observed = true;
                    break;
                }
            }
            assert!(
                observed,
                "large {label} copy must expose a guarded post-allocation {expected:?} boundary"
            );
        }
    }
}

#[test]
fn huge_adjustment_only_tj_guards_execution_and_prioritizes_source_change() {
    let mut large = b"BT /F0 10 Tf [".to_vec();
    for _ in 0..1_024 {
        large.extend_from_slice(b"0 ");
    }
    large.extend_from_slice(b"] TJ ET");

    let resume_baseline = {
        let (mut job, store) = default_font_job(b"BT /F0 10 Tf [0] TJ ET", 0xd8);
        let missing = RangeStore::new(store.snapshot(), Default::default()).unwrap();
        let blocker = BlockPayloadAfter {
            complete: &store,
            missing: &missing,
            checkpoint: ResumeCheckpoint::new(32_008),
            admitted_payload_polls: 0,
            payload_polls: AtomicUsize::new(0),
        };
        assert!(matches!(
            job.poll(&blocker, &DocumentNeverCancelled),
            ContentVmPoll::Pending { .. }
        ));
        let source = CountingStoreSource {
            complete: &store,
            snapshot_calls: AtomicUsize::new(0),
        };
        assert!(matches!(
            job.poll(&source, &DocumentNeverCancelled),
            ContentVmPoll::Ready(_)
        ));
        source.snapshot_calls.load(Ordering::Acquire)
    };

    for (case_index, (change_source, expected)) in [
        (false, ContentVmErrorCode::Cancelled),
        (true, ContentVmErrorCode::SourceSnapshotMismatch),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut job, store) = default_font_job(&large, 0xd9 + case_index as u8);
        let original = store.snapshot();
        let missing = RangeStore::new(original, Default::default()).unwrap();
        let blocker = BlockPayloadAfter {
            complete: &store,
            missing: &missing,
            checkpoint: ResumeCheckpoint::new(32_008),
            admitted_payload_polls: 0,
            payload_polls: AtomicUsize::new(0),
        };
        assert!(matches!(
            job.poll(&blocker, &DocumentNeverCancelled),
            ContentVmPoll::Pending { .. }
        ));
        let source = ChangingStoreSource {
            complete: &store,
            replacement: snapshot(
                original.len().expect("fixture length"),
                0xe8 + case_index as u8,
            ),
            changed: AtomicBool::new(false),
            snapshot_calls: AtomicUsize::new(0),
        };
        let cancellation = CancelDuringStore {
            source: &source,
            trigger_snapshot_call: resume_baseline + 2,
            change_source,
        };
        match job.poll(&source, &cancellation) {
            ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                assert_eq!(error.code(), expected);
                assert_eq!(
                    error
                        .source()
                        .expect("TJ guard source")
                        .page_operator_ordinal(),
                    2
                );
            }
            outcome => panic!("large adjustment-only TJ must stop in execution: {outcome:?}"),
        }
        assert_eq!(job.font_stats().text_adjustments(), 1_024);
        assert_eq!(job.font_stats().acquisitions(), 1);
        assert_eq!(job.font_stats().execution_passes(), 1);
        assert_eq!(job.font_stats().glyphs(), 0);
        let terminal = job.font_stats();
        match job.poll(&missing, &AlwaysCancelled) {
            ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                assert_eq!(error.code(), expected)
            }
            outcome => panic!("terminal adjustment TJ must replay: {outcome:?}"),
        }
        assert_eq!(job.font_stats(), terminal);
    }
}

#[test]
fn huge_outline_guards_after_allocation_and_prioritizes_source_change() {
    let program = font_support::build_font(vec![
        Vec::new(),
        font_support::contour_glyph(&[true; 1_024]),
    ]);
    let objects = embedded_font_objects(5, 6, 7, &program, 777);
    let resources = b"<< /Font << /F0 5 0 R >> >>";

    let resume_without_show = {
        let (mut job, store) = font_job_with_limits(
            b"BT /F0 10 Tf ET",
            resources,
            &objects,
            0xdb,
            ContentVmLimits::default(),
            ContentFontLimits::default(),
            GraphicsSceneLimits::default(),
        );
        let missing = RangeStore::new(store.snapshot(), Default::default()).unwrap();
        let blocker = BlockPayloadAfter {
            complete: &store,
            missing: &missing,
            checkpoint: ResumeCheckpoint::new(32_008),
            admitted_payload_polls: 0,
            payload_polls: AtomicUsize::new(0),
        };
        assert!(matches!(
            job.poll(&blocker, &DocumentNeverCancelled),
            ContentVmPoll::Pending { .. }
        ));
        let source = CountingStoreSource {
            complete: &store,
            snapshot_calls: AtomicUsize::new(0),
        };
        assert!(matches!(
            job.poll(&source, &DocumentNeverCancelled),
            ContentVmPoll::Ready(_)
        ));
        source.snapshot_calls.load(Ordering::Acquire)
    };
    let measured_peak = {
        let (mut job, store) = font_job_with_limits(
            b"BT /F0 10 Tf (A) Tj ET",
            resources,
            &objects,
            0xdc,
            ContentVmLimits::default(),
            ContentFontLimits::default(),
            GraphicsSceneLimits::default(),
        );
        match job.poll(&store, &DocumentNeverCancelled) {
            ContentVmPoll::Ready(page) => page.font_stats().peak_glyph_retained_bytes(),
            outcome => panic!("large outline measurement must publish: {outcome:?}"),
        }
    };

    for (case_index, (change_source, expected)) in [
        (false, ContentVmErrorCode::Cancelled),
        (true, ContentVmErrorCode::SourceSnapshotMismatch),
    ]
    .into_iter()
    .enumerate()
    {
        let mut observed = false;
        for offset in (4..64).step_by(2) {
            let (mut job, store) = font_job_with_limits(
                b"BT /F0 10 Tf (A) Tj ET",
                resources,
                &objects,
                0xdd + case_index as u8,
                ContentVmLimits::default(),
                ContentFontLimits::default(),
                GraphicsSceneLimits::default(),
            );
            let original = store.snapshot();
            let missing = RangeStore::new(original, Default::default()).unwrap();
            let blocker = BlockPayloadAfter {
                complete: &store,
                missing: &missing,
                checkpoint: ResumeCheckpoint::new(32_008),
                admitted_payload_polls: 0,
                payload_polls: AtomicUsize::new(0),
            };
            assert!(matches!(
                job.poll(&blocker, &DocumentNeverCancelled),
                ContentVmPoll::Pending { .. }
            ));
            let source = ChangingStoreSource {
                complete: &store,
                replacement: snapshot(
                    original.len().expect("fixture length"),
                    0xed + case_index as u8,
                ),
                changed: AtomicBool::new(false),
                snapshot_calls: AtomicUsize::new(0),
            };
            let cancellation = CancelDuringStore {
                source: &source,
                trigger_snapshot_call: resume_without_show + offset,
                change_source,
            };
            let outcome = job.poll(&source, &cancellation);
            if matches!(
                outcome,
                ContentVmPoll::Failed(ContentVmFailure::Vm(error))
                    if error.code() == expected
                        && error.source().is_some_and(|source| source.page_operator_ordinal() == 2)
            ) && job.font_stats().execution_passes() == 1
                && job.font_stats().peak_glyph_retained_bytes() == measured_peak
                && job.font_stats().glyphs() == 0
            {
                assert!(
                    job.vm_stats().peak_retained_bytes()
                        >= job
                            .scan_stats()
                            .retained_bytes()
                            .saturating_add(measured_peak)
                );
                let terminal_vm = job.vm_stats();
                let terminal_font = job.font_stats();
                match job.poll(&missing, &AlwaysCancelled) {
                    ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                        assert_eq!(error.code(), expected)
                    }
                    replay => panic!("terminal outline failure must replay: {replay:?}"),
                }
                assert_eq!(job.vm_stats(), terminal_vm);
                assert_eq!(job.font_stats(), terminal_font);
                observed = true;
                break;
            }
        }
        assert!(
            observed,
            "large outline must expose guarded {expected:?} after reserve"
        );
    }
}

#[test]
fn long_non_font_prefix_is_guarded_again_before_tf_lookup() {
    let mut content = Vec::new();
    for _ in 0..300 {
        content.extend_from_slice(b"q Q ");
    }
    content.extend_from_slice(b"BT /F0 10 Tf (A) Tj ET");

    let planning_end_calls = {
        let (mut job, store) = foundational_font_job(
            &content,
            0xdf,
            ContentVmLimits::default(),
            font_limits(|config| config.max_plan_retained_bytes = 1),
            GraphicsSceneLimits::default(),
        );
        let source = CountingStoreSource {
            complete: &store,
            snapshot_calls: AtomicUsize::new(0),
        };
        assert!(matches!(
            job.poll(&source, &DocumentNeverCancelled),
            ContentVmPoll::Failed(ContentVmFailure::Vm(_))
        ));
        assert_eq!(job.font_stats().planning_operators(), 604);
        source.snapshot_calls.load(Ordering::Acquire)
    };

    for (case_index, (change_source, expected)) in [
        (false, ContentVmErrorCode::Cancelled),
        (true, ContentVmErrorCode::SourceSnapshotMismatch),
    ]
    .into_iter()
    .enumerate()
    {
        let mut observed = false;
        for trigger in planning_end_calls + 500..planning_end_calls + 540 {
            let (mut job, store) = default_font_job(&content, 0xe1 + case_index as u8);
            let original = store.snapshot();
            let source = ChangingStoreSource {
                complete: &store,
                replacement: snapshot(
                    original.len().expect("fixture length"),
                    0xf1 + case_index as u8,
                ),
                changed: AtomicBool::new(false),
                snapshot_calls: AtomicUsize::new(0),
            };
            let cancellation = CancelDuringStore {
                source: &source,
                trigger_snapshot_call: trigger,
                change_source,
            };
            let outcome = job.poll(&source, &cancellation);
            if matches!(
                outcome,
                ContentVmPoll::Failed(ContentVmFailure::Vm(error))
                    if error.code() == expected
                        && error.source().is_some_and(|source| {
                            (256..600).contains(&source.page_operator_ordinal())
                        })
            ) && job.font_stats().planning_operators() == 604
                && job.font_stats().lookups() == 0
                && job.font_stats().acquisition_polls() == 0
            {
                let terminal_vm = job.vm_stats();
                let terminal_font = job.font_stats();
                let missing = RangeStore::new(original, Default::default()).unwrap();
                match job.poll(&missing, &AlwaysCancelled) {
                    ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => {
                        assert_eq!(error.code(), expected);
                        assert!(error.source().is_some_and(|source| {
                            (256..600).contains(&source.page_operator_ordinal())
                        }));
                    }
                    replay => panic!("terminal non-font scan failure must replay: {replay:?}"),
                }
                assert_eq!(job.vm_stats(), terminal_vm);
                assert_eq!(job.font_stats(), terminal_font);
                observed = true;
                break;
            }
        }
        assert!(
            observed,
            "second Font scan must guard a non-font prefix before Tf ({expected:?})"
        );
    }
}

#[test]
fn text_without_font_profile_is_exact_unsupported_and_terminal() {
    let (mut job, store) = graphics_job(b"BT 1 Tc ET", 0xf3, ContentGraphicsLimits::default());
    match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Unsupported(error) => {
            assert_eq!(error.kind(), ContentUnsupportedKind::FontProfileRequired);
            assert_eq!(error.source().page_operator_ordinal(), 1);
            assert!(error.font_resource().is_none());
        }
        outcome => {
            panic!("registered text semantics require an explicit Font profile: {outcome:?}")
        }
    }
    let terminal_vm = job.vm_stats();
    let missing = RangeStore::new(store.snapshot(), Default::default()).unwrap();
    match job.poll(&missing, &AlwaysCancelled) {
        ContentVmPoll::Unsupported(error) => {
            assert_eq!(error.kind(), ContentUnsupportedKind::FontProfileRequired);
            assert_eq!(error.source().page_operator_ordinal(), 1);
        }
        outcome => panic!("FontProfileRequired must replay without source work: {outcome:?}"),
    }
    assert_eq!(job.vm_stats(), terminal_vm);
    assert_eq!(job.font_stats(), ContentFontStats::default());
}

#[test]
fn one_page_publishes_images_and_embedded_text_through_combined_profiles() {
    let mut objects = vec![(5, image_object(5, b"", &[10, 20, 30, 40, 50, 60]))];
    objects.extend(embedded_font_objects(
        8,
        9,
        10,
        &font_support::foundational_font(),
        777,
    ));
    let input = acquire_with_objects(
        b"/Im0 Do BT /F0 10 Tf (A) Tj ET",
        b"<< /XObject << /Im0 5 0 R >> /Font << /F0 8 0 R >> >>",
        &objects,
        0xf4,
    );
    let VmInput {
        acquired,
        authority,
        store,
    } = input;
    let image_profile = ContentImageProfile::new(
        authority.clone(),
        PageXObjectLookupLimits::default(),
        ImageXObjectJobContext::new(
            JobId::new(33_001),
            ResumeCheckpoint::new(33_002),
            ResumeCheckpoint::new(33_003),
            ResumeCheckpoint::new(33_004),
            RequestPriority::VisiblePage,
        ),
        ImageXObjectLimits::default(),
        ContentImageLimits::default(),
    );
    let font_profile = ContentFontProfile::new(
        authority,
        PageFontLookupLimits::default(),
        font_context(34_001),
        FontResourceLimits::default(),
        ContentFontLimits::default(),
    );
    let mut job = InterpretPageJob::new_graphics_v2_with_images_and_fonts(
        acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
        PagePropertyLookupLimits::default(),
        image_profile,
        font_profile,
        GraphicsSceneLimits::default(),
    );
    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        outcome => panic!("combined Image and Font profiles must publish one page: {outcome:?}"),
    };
    assert_eq!(page.image_stats().acquisitions(), 1);
    assert_eq!(page.font_stats().acquisitions(), 1);
    assert_eq!(page.image_uses().len(), 1);
    assert_eq!(page.font_uses().len(), 1);
    let graphics = page.scene().graphics().unwrap();
    assert_eq!(graphics.commands().len(), 2);
    assert!(matches!(
        graphics.commands()[0].command(),
        GraphicsCommand::DrawImage { .. }
    ));
    assert!(matches!(
        graphics.commands()[1].command(),
        GraphicsCommand::DrawGlyphRun(_)
    ));
    assert_eq!(graphics.resources().len(), 2);
}

#[test]
fn form_interpreter_uses_form_resources_matrix_and_caller_page_coordinates() {
    let input = acquire_with_objects(
        b"",
        b"<< /XObject << /Fm0 5 0 R >> >>",
        &[
            (
                5,
                form_object(
                    5,
                    b"/BBox [0 0 10 10] /Matrix [2 0 0 3 4 5] \
                      /Resources << /ExtGState << /Fade 6 0 R >> \
                      /ColorSpace << /CS0 7 0 R >> >>",
                    b"/Fade gs /CS0 cs 1 0 0 scn 0 0 10 10 re f",
                ),
            ),
            (6, b"6 0 obj\n<< /ca 0.5 >>\nendobj\n".to_vec()),
            (7, b"7 0 obj\n[/ICCBased 8 0 R]\nendobj\n".to_vec()),
            (
                8,
                b"8 0 obj\n<< /N 3 /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec(),
            ),
        ],
        0xf5,
    );
    let VmInput {
        acquired,
        authority,
        store,
    } = input;
    let proof = {
        let mut resolver = acquired
            .page()
            .resources()
            .xobject_resolver(PageXObjectLookupLimits::default());
        match resolver
            .lookup_image_xobject(b"Fm0", &store, &DocumentNeverCancelled)
            .expect("Form resource lookup")
        {
            PageXObjectLookupOutcome::Ready(proof) => proof,
            PageXObjectLookupOutcome::Unsupported(unsupported) => {
                panic!("indirect Form proof expected: {unsupported:?}")
            }
        }
    };
    let mut acquisition = authority
        .acquire_form_xobject(
            proof,
            FormXObjectJobContext::new(
                JobId::new(35_001),
                ResumeCheckpoint::new(35_002),
                ResumeCheckpoint::new(35_003),
                ResumeCheckpoint::new(35_004),
                RequestPriority::VisiblePage,
            ),
        )
        .expect("Form acquisition job");
    let form = match acquisition.poll(&store, &DocumentNeverCancelled) {
        FormXObjectPoll::Ready(form) => form,
        outcome => panic!("identity Form must acquire: {outcome:?}"),
    };

    let handle = acquired.handle();
    let binding = SceneBinding::new(
        handle.snapshot().identity(),
        handle.revision_startxref(),
        handle.index(),
        handle.object(),
    );
    let boxes = acquired.page().boxes();
    let media = SceneRect::new(
        boxes
            .media_box()
            .coordinates()
            .map(|value| SceneScalar::from_scaled(value.scaled())),
    )
    .expect("page media box");
    let crop = SceneRect::new(
        boxes
            .crop_box()
            .coordinates()
            .map(|value| SceneScalar::from_scaled(value.scaled())),
    )
    .expect("page crop box");
    let geometry = PageGeometry::new(media, crop, ScenePageRotation::Degrees0);
    let image_profile = ContentImageProfile::new(
        authority.clone(),
        PageXObjectLookupLimits::default(),
        ImageXObjectJobContext::new(
            JobId::new(35_101),
            ResumeCheckpoint::new(35_102),
            ResumeCheckpoint::new(35_103),
            ResumeCheckpoint::new(35_104),
            RequestPriority::VisiblePage,
        ),
        ImageXObjectLimits::default(),
        ContentImageLimits::default(),
    );
    let font_profile = ContentFontProfile::new(
        authority.clone(),
        PageFontLookupLimits::default(),
        font_context(35_201),
        FontResourceLimits::default(),
        ContentFontLimits::default(),
    );
    let invocation = Matrix::new([
        SceneScalar::ONE,
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::ONE,
        SceneScalar::from_scaled(10_000_000_000),
        SceneScalar::from_scaled(20_000_000_000),
    ]);
    let mut job = InterpretFormJob::new_graphics_v2_with_images_and_fonts(
        form,
        binding,
        geometry,
        invocation,
        ContentLimits::default(),
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
        PagePropertyLookupLimits::default(),
        image_profile,
        font_profile,
        GraphicsSceneLimits::default(),
    )
    .expect("representable invocation and Form matrices")
    .with_dynamic_ext_gstates(ContentExtGStateAcquisitionProfile::new(
        authority.clone(),
        PageExtGStateLookupLimits::default(),
        ContentExtGStateJobContext::new(
            JobId::new(35_301),
            ResumeCheckpoint::new(35_302),
            RequestPriority::VisiblePage,
        ),
    ))
    .with_dynamic_color_spaces(ContentColorSpaceAcquisitionProfile::new(
        authority,
        PageColorSpaceLookupLimits::default(),
        ContentColorSpaceJobContext::new(
            JobId::new(35_401),
            ResumeCheckpoint::new(35_402),
            RequestPriority::VisiblePage,
        ),
    ));
    let interpreted = match job.poll(&store, &DocumentNeverCancelled) {
        ContentFormPoll::Ready(form) => form,
        outcome => panic!("path-only Form must interpret: {outcome:?}"),
    };
    let graphics = interpreted.scene().graphics().expect("graphics-v2 Form");
    assert!(matches!(
        graphics.commands().first().map(|record| record.command()),
        Some(GraphicsCommand::Save)
    ));
    assert!(matches!(
        graphics.commands().get(1).map(|record| record.command()),
        Some(GraphicsCommand::Clip { .. })
    ));
    assert!(matches!(
        graphics.commands().last().map(|record| record.command()),
        Some(GraphicsCommand::Restore)
    ));
    assert_eq!(
        graphics.commands().iter().find_map(|record| {
            let GraphicsCommand::Fill { paint, .. } = record.command() else {
                return None;
            };
            Some(paint.alpha())
        }),
        Some(SceneUnit::from_u16(32_768))
    );
    assert!(matches!(
        graphics.commands().iter().find_map(|record| {
            let GraphicsCommand::Fill { paint, .. } = record.command() else {
                return None;
            };
            Some(paint.color())
        }),
        Some(DeviceColor::Rgb {
            red: SceneUnit::ONE,
            green: SceneUnit::ZERO,
            blue: SceneUnit::ZERO,
        })
    ));
    let Some((path_id, transform)) = graphics.commands().iter().find_map(|record| {
        let GraphicsCommand::Fill {
            path, transform, ..
        } = record.command()
        else {
            return None;
        };
        Some((*path, *transform))
    }) else {
        panic!("Form rectangle must become one fill");
    };
    assert_eq!(
        transform.components().map(SceneScalar::scaled),
        Matrix::IDENTITY.components().map(SceneScalar::scaled)
    );
    let GraphicsResource::Path(path) = graphics
        .resources()
        .iter()
        .find(|entry| entry.id() == path_id)
        .expect("fill path resource")
        .resource()
    else {
        panic!("Form fill must retain its transformed path");
    };
    let points = path
        .segments()
        .iter()
        .filter_map(|segment| match segment {
            PathSegment::MoveTo(point) | PathSegment::LineTo(point) => {
                Some([point.x().scaled(), point.y().scaled()])
            }
            PathSegment::CubicTo { .. } | PathSegment::ClosePath => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        points,
        [
            [14_000_000_000, 25_000_000_000],
            [34_000_000_000, 25_000_000_000],
            [34_000_000_000, 55_000_000_000],
            [14_000_000_000, 55_000_000_000],
        ]
    );
    assert_eq!(
        interpreted.acquired_form().resources().defining_object(),
        proof.target()
    );
}

#[test]
fn page_do_recursively_classifies_forms_and_imports_their_scenes() {
    let outer = form_object(
        5,
        b"/BBox [0 0 20 20] /Matrix [1 0 0 1 5 6] \
          /Resources << /XObject << /Nested 6 0 R >> >> \
          /Group << /Type /Group /S /Transparency /CS /DeviceRGB >>",
        b"q 1 0 0 1 7 8 cm /Nested Do Q",
    );
    let nested = form_object(
        6,
        b"/BBox [0 0 10 10] /Matrix [3 0 0 2 1 2] /Resources << >>",
        b"0 0 10 10 re 0 0 1 rg f",
    );
    let input = acquire_with_objects(
        b"2 0 0 2 3 4 cm /Fm0 Do",
        b"<< /XObject << /Fm0 5 0 R >> >>",
        &[(5, outer), (6, nested)],
        0xf6,
    );
    let VmInput {
        acquired,
        authority,
        store,
    } = input;
    let image_profile = ContentImageProfile::new(
        authority.clone(),
        PageXObjectLookupLimits::default(),
        ImageXObjectJobContext::new(
            JobId::new(36_001),
            ResumeCheckpoint::new(36_002),
            ResumeCheckpoint::new(36_003),
            ResumeCheckpoint::new(36_004),
            RequestPriority::VisiblePage,
        ),
        ImageXObjectLimits::default(),
        ContentImageLimits::default(),
    );
    let font_profile = ContentFontProfile::new(
        authority.clone(),
        PageFontLookupLimits::default(),
        font_context(36_101),
        FontResourceLimits::default(),
        ContentFontLimits::default(),
    );
    let form_profile = ContentFormProfile::new(
        authority,
        FormXObjectJobContext::new(
            JobId::new(36_201),
            ResumeCheckpoint::new(36_202),
            ResumeCheckpoint::new(36_203),
            ResumeCheckpoint::new(36_204),
            RequestPriority::VisiblePage,
        ),
        4,
        ContentLimits::default(),
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
        PagePropertyLookupLimits::default(),
        image_profile.clone(),
        font_profile.clone(),
        GraphicsSceneLimits::default(),
    )
    .expect("compatible bounded Form profile");
    let mut job = InterpretPageJob::new_graphics_v2_with_images_and_fonts(
        acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
        PagePropertyLookupLimits::default(),
        image_profile,
        font_profile,
        GraphicsSceneLimits::default(),
    )
    .with_forms(form_profile)
    .expect("image-capable Page job accepts Form recursion");
    let page = match job.poll(&store, &DocumentNeverCancelled) {
        ContentVmPoll::Ready(page) => page,
        ContentVmPoll::Failed(ContentVmFailure::Scene(error)) => panic!(
            "nested Form Scene failed at {:?} with {:?}",
            error.command_index(),
            error.code()
        ),
        ContentVmPoll::Failed(ContentVmFailure::Vm(error)) => panic!(
            "nested Form VM failed at {:?} with {:?}",
            error.source().map(|source| source.page_operator_ordinal()),
            error.code()
        ),
        outcome => panic!("nested Forms must publish through Page Do: {outcome:?}"),
    };

    assert!(page.image_uses().is_empty());
    assert_eq!(page.form_uses().len(), 1);
    assert_eq!(page.form_uses()[0].form().form_uses().len(), 1);
    assert_eq!(
        page.form_uses()[0].form().form_uses()[0].xobject().target(),
        pdf_rs_syntax::ObjectRef::new(6, 0).unwrap()
    );
    let graphics = page.scene().graphics().expect("graphics-v2 Page");
    assert_eq!(
        graphics
            .commands()
            .iter()
            .filter(|record| matches!(record.command(), GraphicsCommand::Clip { .. }))
            .count(),
        2
    );
    assert!(
        graphics
            .commands()
            .iter()
            .any(|record| matches!(record.command(), GraphicsCommand::BeginIsolatedGroup { .. }))
    );
    let fill_path = graphics
        .commands()
        .iter()
        .find_map(|record| match record.command() {
            GraphicsCommand::Fill { path, .. } => Some(*path),
            _ => None,
        })
        .expect("nested Form fill");
    let GraphicsResource::Path(path) = graphics
        .resources()
        .iter()
        .find(|entry| entry.id() == fill_path)
        .expect("nested Form path resource")
        .resource()
    else {
        panic!("nested fill resource must be a path");
    };
    let first = match path.segments().first() {
        Some(PathSegment::MoveTo(point)) => *point,
        other => panic!("nested rectangle starts with MoveTo, got {other:?}"),
    };
    assert_eq!(
        [first.x().scaled(), first.y().scaled()],
        [29_000_000_000, 36_000_000_000]
    );
}
