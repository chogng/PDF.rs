use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_content::{
    ContentGraphicsLimitConfig, ContentGraphicsLimitKind, ContentGraphicsLimits, ContentLimits,
    ContentUnsupportedKind, ContentVmError, ContentVmErrorCode, ContentVmFailure,
    ContentVmLimitConfig, ContentVmLimitKind, ContentVmLimits, ContentVmPoll, InterpretPageJob,
};
use pdf_rs_document::{
    AcquiredPageContent, AttestRevisionJob, CandidateRevisionIndex, DocumentCancellation,
    NeverCancelled as DocumentNeverCancelled, PageContentJobContext, PageContentLimits,
    PageContentPoll, PageIndexBuildPoll, PageIndexLimits, PageLookupPoll,
    PageMaterializationJobContext, PageMaterializationLimits, PageMaterializationPoll,
    PagePropertyLookupLimits, PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_scene::{
    BlendMode, DashPatternBuilder, DeviceColor, FillRule, GraphicsCommand, GraphicsResource,
    GraphicsSceneLimitConfig, GraphicsSceneLimits, LineCap, LineJoin, Matrix, PathSegment,
    SceneScalar, SceneUnit, SceneVersion,
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
    let mut page =
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources ".to_vec();
    page.extend_from_slice(resources);
    page.extend_from_slice(b" /Contents 4 0 R >>\nendobj\n");
    let mut stream = format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream\nendobj\n");

    let bodies = [
        (1_u32, CATALOG.to_vec()),
        (2, PAGE_ROOT.to_vec()),
        (3, page),
        (4, stream),
    ];
    let size = 8_u32;
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
    };
    let mut build = authority
        .build_page_index(
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
        .lookup_page(&cold, 0, tree_context(30_031), tree_limits())
        .expect("lookup job");
    let lookup = match lookup.poll(&store, &DocumentNeverCancelled) {
        PageLookupPoll::Ready(lookup) => lookup,
        outcome => panic!("strict Page lookup must finish: {outcome:?}"),
    };
    let (index, handle) = lookup.into_parts();
    let mut materialize = authority
        .materialize_page(
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
        .acquire_page_content(
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
    VmInput { acquired, store }
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
    assert!(matches!(
        exact.poll(&exact_store, &DocumentNeverCancelled),
        ContentVmPoll::Ready(_)
    ));

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
