use std::env;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_rs_bytes::{
    ByteSlice, ByteSource, JobId, RangeStore, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId,
    SourceValidator, SourceValidatorKind,
};
use pdf_rs_content::{
    ContentFontLimits, ContentFontProfile, ContentGraphicsLimits, ContentImageLimitConfig,
    ContentImageLimits, ContentImageProfile, ContentLimits, ContentVmErrorCode, ContentVmFailure,
    ContentVmLimits, ContentVmPoll, InterpretPageJob,
};
use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_document::{
    AcquiredPageContent, FontResourceJobContext, FontResourceLimits, ImageXObjectJobContext,
    ImageXObjectLimits, ImageXObjectUnsupportedKind, NeverCancelled, OpenStrictBaseRevisionJob,
    PageContentJobContext, PageContentLimits, PageContentPoll, PageFontLookupLimits,
    PageIndexBuildPoll, PageIndexLimits, PageLookupPoll, PageMaterializationJobContext,
    PageMaterializationLimits, PageMaterializationPoll, PagePropertyLookupLimits,
    PageTreeJobContext, PageTreeLimits, PageXObjectLookupLimits, RevisionAttestationJobContext,
    RevisionAttestationLimits, RevisionId, SharedAttestedRevisionIndex, StrictBaseOpenContext,
    StrictBaseOpenError, StrictBaseOpenLimits, StrictBaseOpenPoll,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_raster::reference::{
    CanonicalPixelBuffer, ReferenceCapabilityDecision, ReferenceRasterCancellation,
    ReferenceRasterLimitConfig, ReferenceRasterLimits, ReferenceRenderConfig,
    ReferenceRenderErrorCode, ReferenceRenderJob, ReferenceRenderLimitKind, ReferenceRenderPhase,
    ReferenceRenderPoll,
};
use pdf_rs_scene::{GraphicsCommand, GraphicsSceneLimits};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{XrefJobContext, XrefLimits};

mod artifact;
mod fixture;
mod pending;

use artifact::{
    LimitEvidence, NormalizedOutcome, OutcomeInput, RenderEvidence, canonical_pixel, normalize,
    write_outputs,
};
use fixture::{Fixture, FixtureSpec, IMAGE_RGB, ImageSpec, PAGE_OBJECT_NUMBER, build_fixture};
use pending::{PendingEvent, complete_pending, source_changed_pending};

const PATH_CLIP_CONTENT: &[u8] = b"q 0 0 50 100 re W n 0 g 0 0 100 100 re f Q \
                                  1 0 0 rg 50 0 50 100 re f";
const STROKE_CONTENT: &[u8] = b"2 w [4 2] 0 d 5 5 90 90 re S 0.5 g 10 10 80 80 re B*";
const IMAGE_CONTENT: &[u8] = b"q 100 0 0 100 0 0 cm /Im0 Do Q";
const FONT_CONTENT: &[u8] = b"BT /F0 1000 Tf 0 0 Td (A) Tj ET";
const MIXED_CONTENT: &[u8] = b"0.8 g 0 0 100 100 re f \
                               q 50 0 0 100 0 0 cm /Im0 Do Q \
                               0 g BT /F0 500 Tf 0 0 Td (A) Tj ET";
const INVALID_CONTENT: &[u8] = b"q";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Case {
    ValidPathClip,
    ValidStroke,
    ValidImage,
    ValidFont,
    ValidMixed,
    UnsupportedInterpolatedImage,
    InvalidContentState,
    StrictInvalidXref,
    CancelFinalPublication,
    SourceChangeAfterPending,
    ImageDecodedOneLess,
    RasterOutputOneLess,
}

const CASES: [Case; 12] = [
    Case::ValidPathClip,
    Case::ValidStroke,
    Case::ValidImage,
    Case::ValidFont,
    Case::ValidMixed,
    Case::UnsupportedInterpolatedImage,
    Case::InvalidContentState,
    Case::StrictInvalidXref,
    Case::CancelFinalPublication,
    Case::SourceChangeAfterPending,
    Case::ImageDecodedOneLess,
    Case::RasterOutputOneLess,
];

impl Case {
    const fn id(self) -> &'static str {
        match self {
            Self::ValidPathClip => "raster/m3-reference/valid-path-clip",
            Self::ValidStroke => "raster/m3-reference/valid-stroke",
            Self::ValidImage => "raster/m3-reference/valid-image",
            Self::ValidFont => "raster/m3-reference/valid-font",
            Self::ValidMixed => "raster/m3-reference/valid-mixed",
            Self::UnsupportedInterpolatedImage => {
                "raster/m3-reference/producer-unsupported-interpolated-image"
            }
            Self::InvalidContentState => "raster/m3-reference/invalid-content-state",
            Self::StrictInvalidXref => "raster/m3-reference/strict-invalid-xref",
            Self::CancelFinalPublication => "raster/m3-reference/cancel-final-publication",
            Self::SourceChangeAfterPending => "raster/m3-reference/source-change-after-pending",
            Self::ImageDecodedOneLess => "raster/m3-reference/image-decoded-one-less",
            Self::RasterOutputOneLess => "raster/m3-reference/raster-output-one-less",
        }
    }

    const fn salt(self) -> u8 {
        match self {
            Self::ValidPathClip => 0xc1,
            Self::ValidStroke => 0xc2,
            Self::ValidImage => 0xc3,
            Self::ValidFont => 0xc4,
            Self::ValidMixed => 0xc5,
            Self::UnsupportedInterpolatedImage => 0xc6,
            Self::InvalidContentState => 0xc7,
            Self::StrictInvalidXref => 0xc8,
            Self::CancelFinalPublication => 0xc9,
            Self::SourceChangeAfterPending => 0xca,
            Self::ImageDecodedOneLess => 0xcb,
            Self::RasterOutputOneLess => 0xcc,
        }
    }

    const fn fixture_spec(self) -> FixtureSpec {
        match self {
            Self::ValidPathClip | Self::CancelFinalPublication | Self::RasterOutputOneLess => {
                FixtureSpec {
                    content: PATH_CLIP_CONTENT,
                    image: None,
                    font: false,
                    invalid_startxref: false,
                    salt: self.salt(),
                }
            }
            Self::ValidStroke => FixtureSpec {
                content: STROKE_CONTENT,
                image: None,
                font: false,
                invalid_startxref: false,
                salt: self.salt(),
            },
            Self::ValidImage | Self::SourceChangeAfterPending | Self::ImageDecodedOneLess => {
                FixtureSpec {
                    content: IMAGE_CONTENT,
                    image: Some(ImageSpec { interpolate: false }),
                    font: false,
                    invalid_startxref: false,
                    salt: self.salt(),
                }
            }
            Self::ValidFont => FixtureSpec {
                content: FONT_CONTENT,
                image: None,
                font: true,
                invalid_startxref: false,
                salt: self.salt(),
            },
            Self::ValidMixed => FixtureSpec {
                content: MIXED_CONTENT,
                image: Some(ImageSpec { interpolate: false }),
                font: true,
                invalid_startxref: false,
                salt: self.salt(),
            },
            Self::UnsupportedInterpolatedImage => FixtureSpec {
                content: IMAGE_CONTENT,
                image: Some(ImageSpec { interpolate: true }),
                font: false,
                invalid_startxref: false,
                salt: self.salt(),
            },
            Self::InvalidContentState => FixtureSpec {
                content: INVALID_CONTENT,
                image: None,
                font: false,
                invalid_startxref: false,
                salt: self.salt(),
            },
            Self::StrictInvalidXref => FixtureSpec {
                content: PATH_CLIP_CONTENT,
                image: None,
                font: false,
                invalid_startxref: true,
                salt: self.salt(),
            },
        }
    }

    const fn output_size(self) -> (u32, u32) {
        match self {
            Self::ValidPathClip
            | Self::ValidImage
            | Self::CancelFinalPublication
            | Self::RasterOutputOneLess => (2, 1),
            Self::ValidStroke
            | Self::ValidFont
            | Self::ValidMixed
            | Self::UnsupportedInterpolatedImage
            | Self::InvalidContentState
            | Self::StrictInvalidXref
            | Self::SourceChangeAfterPending
            | Self::ImageDecodedOneLess => (8, 8),
        }
    }
}

#[derive(Clone, Copy)]
struct RuntimeIds {
    open: JobId,
    build: JobId,
    lookup: JobId,
    materialize: JobId,
    content: JobId,
    image: JobId,
    font: JobId,
    base: u64,
}

impl RuntimeIds {
    fn for_case(case: Case, replay: u64) -> Self {
        let base = replay
            .checked_mul(100_000_000)
            .and_then(|value| value.checked_add(u64::from(case.salt()) * 100_000))
            .expect("M3 replay runtime namespace fits u64");
        Self {
            open: JobId::new(base + 1_000),
            build: JobId::new(base + 2_000),
            lookup: JobId::new(base + 3_000),
            materialize: JobId::new(base + 4_000),
            content: JobId::new(base + 5_000),
            image: JobId::new(base + 6_000),
            font: JobId::new(base + 7_000),
            base,
        }
    }
}

struct AcquiredPipeline {
    authority: SharedAttestedRevisionIndex,
    acquired: AcquiredPageContent,
    snapshot: SourceSnapshot,
    ids: RuntimeIds,
}

enum VmTerminal {
    Ready(Arc<pdf_rs_content::InterpretedPage>),
    Unsupported(pdf_rs_content::ContentUnsupported),
    Failed(ContentVmFailure),
}

/// Runs the strict full-Native M3 Reference integration gate.
pub(super) fn run_gate() {
    let mut outcomes = Vec::with_capacity(CASES.len());
    for case in CASES {
        let first_ids = RuntimeIds::for_case(case, 0);
        let second_ids = RuntimeIds::for_case(case, 1);
        assert_ne!(first_ids.open, second_ids.open);
        assert_ne!(first_ids.image, second_ids.image);
        assert_ne!(first_ids.font, second_ids.font);
        assert_ne!(first_ids.base, second_ids.base);
        let first = run_case(case, 0);
        let second = run_case(case, 1);
        assert_eq!(
            first,
            second,
            "case={} must normalize identically across two fresh strict pipelines",
            case.id()
        );
        outcomes.push(first);
    }

    if let Some(output) = env::var_os("PDF_RS_M3_REFERENCE_GATE_OUTPUT") {
        write_outputs(Path::new(&output), &outcomes);
    }
}

fn run_case(case: Case, replay: u64) -> NormalizedOutcome {
    let fixture = build_fixture(case.fixture_spec());
    assert_eq!(fixture.salt, case.salt());
    match case {
        Case::ValidPathClip
        | Case::ValidStroke
        | Case::ValidImage
        | Case::ValidFont
        | Case::ValidMixed => run_ready_case(case, replay, &fixture),
        Case::UnsupportedInterpolatedImage => run_unsupported_case(case, replay, &fixture),
        Case::InvalidContentState => run_invalid_content_case(case, replay, &fixture),
        Case::StrictInvalidXref => run_invalid_strict_case(case, replay, &fixture),
        Case::CancelFinalPublication => run_cancel_case(case, replay, &fixture),
        Case::SourceChangeAfterPending => run_source_change_case(case, replay, &fixture),
        Case::ImageDecodedOneLess => run_image_one_less_case(case, replay, &fixture),
        Case::RasterOutputOneLess => run_raster_one_less_case(case, replay, &fixture),
    }
}

fn run_ready_case(case: Case, replay: u64, fixture: &Fixture) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(case, replay, fixture, &mut pending)
        .unwrap_or_else(|error| panic!("case={} strict open must succeed: {error}", case.id()));
    let image_limits = if case == Case::ValidImage {
        content_image_limits(IMAGE_RGB.len() as u64)
    } else {
        ContentImageLimits::default()
    };
    let (mut vm, store, expected_jobs) = make_vm(acquired, image_limits);
    let page = match drive_vm(&mut vm, &store, fixture, &expected_jobs, &mut pending) {
        VmTerminal::Ready(page) => page,
        VmTerminal::Unsupported(error) => {
            panic!(
                "case={} Content unexpectedly unsupported: {error}",
                case.id()
            )
        }
        VmTerminal::Failed(error) => {
            panic!("case={} Content unexpectedly failed: {error}", case.id())
        }
    };
    assert_ready_content(case, &page);

    let scene = page.scene_arc();
    let scene_bytes = scene
        .canonical_json_bytes()
        .expect("bounded integrated Scene canonicalizes");
    let (width, height) = case.output_size();
    let config =
        ReferenceRenderConfig::opaque_srgb(width, height).expect("positive output configuration");
    let limits = if case == Case::ValidPathClip {
        raster_output_limits(u64::from(width) * u64::from(height) * 4)
    } else {
        ReferenceRasterLimits::default()
    };
    let cancellation = RasterCancellation::never();
    let mut renderer = ReferenceRenderJob::new(scene, config, limits);
    let buffer = match renderer.poll(&cancellation) {
        ReferenceRenderPoll::Ready(buffer) => buffer,
        outcome => panic!(
            "case={} Reference render must be Ready: {outcome:?}",
            case.id()
        ),
    };
    assert_eq!(renderer.phase(), ReferenceRenderPhase::Ready);
    assert_eq!(
        buffer.capability_decision(),
        ReferenceCapabilityDecision::Supported
    );
    assert_eq!(buffer.binding(), page.scene().binding());
    assert_eq!(buffer.config(), config);
    assert_eq!(buffer.limits(), limits);
    assert_ready_pixels(case, &buffer);

    let cancellation_calls = cancellation.calls();
    let replay = match renderer.poll(&cancellation) {
        ReferenceRenderPoll::Ready(buffer) => buffer,
        outcome => panic!("case={} Ready render must replay: {outcome:?}", case.id()),
    };
    assert!(Arc::ptr_eq(&buffer, &replay));
    assert_eq!(cancellation.calls(), cancellation_calls);

    let render = RenderEvidence {
        binding: buffer.binding(),
        config: buffer.config(),
        identity: buffer.identity(),
        limits: buffer.limits(),
        stats: buffer.stats(),
        phase: "ready",
    };
    normalize(OutcomeInput {
        case_id: case.id(),
        outcome: "ready",
        stage: "reference-render",
        diagnostic_id: None,
        input: &fixture.bytes,
        pending: &pending,
        scene: Some(scene_bytes),
        pixel: Some(canonical_pixel(&buffer)),
        render: Some(render),
        limit: None,
    })
}

fn run_unsupported_case(case: Case, replay: u64, fixture: &Fixture) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(case, replay, fixture, &mut pending)
        .unwrap_or_else(|error| panic!("unsupported fixture must strictly open: {error}"));
    let (mut vm, store, jobs) = make_vm(acquired, ContentImageLimits::default());
    // `/Interpolate true` is outside the registered basic Image producer profile. The proof-bound
    // Content layer therefore returns its own structured capability result before a Scene exists;
    // this case intentionally does not mislabel that strong producer boundary as a raster outcome.
    let unsupported = match drive_vm(&mut vm, &store, fixture, &jobs, &mut pending) {
        VmTerminal::Unsupported(unsupported) => unsupported,
        outcome => panic!(
            "interpolated image must be rejected by the Native producer capability boundary: {}",
            vm_label(&outcome)
        ),
    };
    assert_eq!(unsupported.diagnostic_id(), "RPE-CONTENT-UNSUPPORTED-0009");
    assert_eq!(
        unsupported
            .image_xobject()
            .expect("producer capability retains lower Image reason")
            .kind(),
        ImageXObjectUnsupportedKind::Interpolation
    );
    assert_eq!(vm.image_stats().image_uses(), 0);
    normalize(OutcomeInput {
        case_id: case.id(),
        outcome: "unsupported",
        stage: "content-image",
        diagnostic_id: Some(unsupported.diagnostic_id()),
        input: &fixture.bytes,
        pending: &pending,
        scene: None,
        pixel: None,
        render: None,
        limit: None,
    })
}

fn run_invalid_content_case(case: Case, replay: u64, fixture: &Fixture) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(case, replay, fixture, &mut pending)
        .unwrap_or_else(|error| panic!("invalid Content fixture must strictly open: {error}"));
    let (mut vm, store, jobs) = make_vm(acquired, ContentImageLimits::default());
    let failure = match drive_vm(&mut vm, &store, fixture, &jobs, &mut pending) {
        VmTerminal::Failed(error) => error,
        outcome => panic!("unbalanced q must fail Content: {}", vm_label(&outcome)),
    };
    assert_eq!(failure.diagnostic_id(), "RPE-CONTENT-VM-0007");
    normalize(OutcomeInput {
        case_id: case.id(),
        outcome: "failed",
        stage: "content-vm",
        diagnostic_id: Some(failure.diagnostic_id()),
        input: &fixture.bytes,
        pending: &pending,
        scene: None,
        pixel: None,
        render: None,
        limit: None,
    })
}

fn run_invalid_strict_case(case: Case, replay: u64, fixture: &Fixture) -> NormalizedOutcome {
    assert_ne!(fixture.startxref, fixture.advertised_startxref);
    let mut pending = Vec::new();
    let error = match acquire_pipeline(case, replay, fixture, &mut pending) {
        Ok(_) => panic!("corrupt startxref must not publish strict authority"),
        Err(error) => error,
    };
    let diagnostic = strict_diagnostic(*error);
    assert_eq!(diagnostic, "RPE-XREF-0011");
    normalize(OutcomeInput {
        case_id: case.id(),
        outcome: "failed",
        stage: "strict-open",
        diagnostic_id: Some(diagnostic),
        input: &fixture.bytes,
        pending: &pending,
        scene: None,
        pixel: None,
        render: None,
        limit: None,
    })
}

fn run_cancel_case(case: Case, replay: u64, fixture: &Fixture) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(case, replay, fixture, &mut pending)
        .unwrap_or_else(|error| panic!("cancellation fixture must strictly open: {error}"));
    let (mut vm, store, jobs) = make_vm(acquired, ContentImageLimits::default());
    let page = match drive_vm(&mut vm, &store, fixture, &jobs, &mut pending) {
        VmTerminal::Ready(page) => page,
        outcome => panic!(
            "cancellation fixture must publish Scene: {}",
            vm_label(&outcome)
        ),
    };
    let scene = page.scene_arc();
    let scene_bytes = scene.canonical_json_bytes().unwrap();
    let (width, height) = case.output_size();
    let config = ReferenceRenderConfig::opaque_srgb(width, height).unwrap();
    let limits = ReferenceRasterLimits::default();

    let measurement = RasterCancellation::never();
    let mut measured = ReferenceRenderJob::new(scene.clone(), config, limits);
    let baseline = match measured.poll(&measurement) {
        ReferenceRenderPoll::Ready(buffer) => buffer,
        outcome => panic!("cancellation measurement must render: {outcome:?}"),
    };
    assert_eq!(baseline.stats().cancellation_checks(), measurement.calls());
    let cancellation = RasterCancellation::at(measurement.calls());
    let mut renderer = ReferenceRenderJob::new(scene, config, limits);
    let failure = match renderer.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("final publication cancellation must fail: {outcome:?}"),
    };
    assert_eq!(failure.code(), ReferenceRenderErrorCode::Cancelled);
    assert_eq!(failure.diagnostic_id(), "RPE-RASTER-0004");
    assert_eq!(
        renderer.stats().final_conversion_pixels(),
        u64::from(width) * u64::from(height)
    );
    assert_eq!(renderer.stats().retained_bytes(), 0);
    let calls = cancellation.calls();
    assert_eq!(
        renderer.poll(&cancellation),
        ReferenceRenderPoll::Failed(failure)
    );
    assert_eq!(cancellation.calls(), calls);
    normalize(OutcomeInput {
        case_id: case.id(),
        outcome: "cancelled",
        stage: "reference-publication",
        diagnostic_id: Some(failure.diagnostic_id()),
        input: &fixture.bytes,
        pending: &pending,
        scene: Some(scene_bytes),
        pixel: None,
        render: Some(RenderEvidence {
            binding: page.scene().binding(),
            config,
            identity: renderer.identity(),
            limits: renderer.limits(),
            stats: renderer.stats(),
            phase: "failed",
        }),
        limit: None,
    })
}

fn run_source_change_case(case: Case, replay: u64, fixture: &Fixture) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(case, replay, fixture, &mut pending)
        .unwrap_or_else(|error| panic!("source-change fixture must strictly open: {error}"));
    let original = acquired.snapshot;
    let (mut vm, store, jobs) = make_vm(acquired, ContentImageLimits::default());
    match vm.poll(&store, &NeverCancelled) {
        ContentVmPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => source_changed_pending(
            "content-vm-source-change",
            &store,
            &jobs,
            ticket,
            &missing,
            checkpoint,
            &mut pending,
        ),
        outcome => panic!("fresh resource VM store must suspend before source change: {outcome:?}"),
    }
    let changed = ChangedSnapshotNoPoll {
        replacement: replacement_snapshot(original, case.salt()),
    };
    let failure = match vm.poll(&changed, &NeverCancelled) {
        ContentVmPoll::Failed(error) => error,
        outcome => panic!("changed snapshot must fail before source polling: {outcome:?}"),
    };
    match failure {
        ContentVmFailure::Vm(error) => {
            assert_eq!(error.code(), ContentVmErrorCode::SourceSnapshotMismatch)
        }
        other => panic!("source mismatch must remain a VM failure: {other:?}"),
    }
    assert_eq!(vm.image_stats().image_uses(), 0);
    assert_eq!(vm.font_stats().font_uses(), 0);
    normalize(OutcomeInput {
        case_id: case.id(),
        outcome: "source-changed",
        stage: "content-vm-resume",
        diagnostic_id: Some(failure.diagnostic_id()),
        input: &fixture.bytes,
        pending: &pending,
        scene: None,
        pixel: None,
        render: None,
        limit: None,
    })
}

fn run_image_one_less_case(case: Case, replay: u64, fixture: &Fixture) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(case, replay, fixture, &mut pending)
        .unwrap_or_else(|error| panic!("one-less image fixture must strictly open: {error}"));
    let one_less = u64::try_from(IMAGE_RGB.len()).unwrap() - 1;
    let (mut vm, store, jobs) = make_vm(acquired, content_image_limits(one_less));
    let failure = match drive_vm(&mut vm, &store, fixture, &jobs, &mut pending) {
        VmTerminal::Failed(error) => error,
        outcome => panic!(
            "one-less decoded image bytes must fail: {}",
            vm_label(&outcome)
        ),
    };
    let limit = match failure {
        ContentVmFailure::Vm(error) => {
            assert_eq!(error.code(), ContentVmErrorCode::ResourceLimit);
            let limit = error
                .image_limit()
                .expect("decoded image failure retains image limit");
            assert_eq!(
                limit.kind(),
                pdf_rs_content::ContentImageLimitKind::DecodedBytes
            );
            LimitEvidence {
                kind: "content-image-decoded-bytes",
                limit: limit.limit(),
                consumed: limit.consumed(),
                attempted: limit.attempted(),
            }
        }
        other => panic!("one-less decoded image bytes must be VM resource failure: {other:?}"),
    };
    assert_eq!(vm.image_stats().image_uses(), 0);
    normalize(OutcomeInput {
        case_id: case.id(),
        outcome: "resource-limited",
        stage: "content-image",
        diagnostic_id: Some(failure.diagnostic_id()),
        input: &fixture.bytes,
        pending: &pending,
        scene: None,
        pixel: None,
        render: None,
        limit: Some(limit),
    })
}

fn run_raster_one_less_case(case: Case, replay: u64, fixture: &Fixture) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(case, replay, fixture, &mut pending)
        .unwrap_or_else(|error| panic!("one-less raster fixture must strictly open: {error}"));
    let (mut vm, store, jobs) = make_vm(acquired, ContentImageLimits::default());
    let page = match drive_vm(&mut vm, &store, fixture, &jobs, &mut pending) {
        VmTerminal::Ready(page) => page,
        outcome => panic!(
            "one-less raster fixture must publish Scene: {}",
            vm_label(&outcome)
        ),
    };
    let scene = page.scene_arc();
    let scene_bytes = scene.canonical_json_bytes().unwrap();
    let (width, height) = case.output_size();
    let output_bytes = u64::from(width) * u64::from(height) * 4;
    let config = ReferenceRenderConfig::opaque_srgb(width, height).unwrap();
    let limits = raster_output_limits(output_bytes - 1);
    let cancellation = RasterCancellation::never();
    let mut renderer = ReferenceRenderJob::new(scene, config, limits);
    let failure = match renderer.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("one-less raster output bytes must fail: {outcome:?}"),
    };
    assert_eq!(failure.code(), ReferenceRenderErrorCode::ResourceLimit);
    let raster_limit = failure.limit().expect("raster failure retains limit");
    assert_eq!(raster_limit.kind(), ReferenceRenderLimitKind::OutputBytes);
    assert_eq!(renderer.stats().retained_bytes(), 0);
    normalize(OutcomeInput {
        case_id: case.id(),
        outcome: "resource-limited",
        stage: "reference-preflight",
        diagnostic_id: Some(failure.diagnostic_id()),
        input: &fixture.bytes,
        pending: &pending,
        scene: Some(scene_bytes),
        pixel: None,
        render: Some(RenderEvidence {
            binding: page.scene().binding(),
            config,
            identity: renderer.identity(),
            limits: renderer.limits(),
            stats: renderer.stats(),
            phase: "failed",
        }),
        limit: Some(LimitEvidence {
            kind: "reference-output-bytes",
            limit: raster_limit.limit(),
            consumed: raster_limit.consumed(),
            attempted: raster_limit.attempted(),
        }),
    })
}

fn acquire_pipeline(
    case: Case,
    replay: u64,
    fixture: &Fixture,
    pending: &mut Vec<PendingEvent>,
) -> Result<AcquiredPipeline, Box<StrictBaseOpenError>> {
    let snapshot = source_snapshot(fixture);
    let ids = RuntimeIds::for_case(case, replay);
    let open_context = StrictBaseOpenContext::new(
        XrefJobContext::new(
            ids.open,
            ResumeCheckpoint::new(ids.base + 1_001),
            ResumeCheckpoint::new(ids.base + 1_002),
        ),
        RevisionAttestationJobContext::new(
            ids.open,
            ResumeCheckpoint::new(ids.base + 1_003),
            ResumeCheckpoint::new(ids.base + 1_004),
            ResumeCheckpoint::new(ids.base + 1_005),
            RequestPriority::VisiblePage,
        ),
    );
    let mut open = OpenStrictBaseRevisionJob::new(
        snapshot,
        RevisionId::new(u32::from(case.salt())),
        open_context,
        StrictBaseOpenLimits::new(
            XrefLimits::default(),
            pdf_rs_document::DocumentLimits::default(),
            RevisionAttestationLimits::default(),
            ObjectLimits::default(),
            SyntaxLimits::default(),
        ),
    )
    .expect("M3 strict-open contexts and validated profiles are compatible");
    let open_store = empty_store(snapshot);
    let authority = loop {
        match open.poll(&open_store, &NeverCancelled) {
            StrictBaseOpenPoll::Ready(authority) => break authority,
            StrictBaseOpenPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                "strict-open",
                &open_store,
                snapshot,
                &fixture.bytes,
                &[ids.open],
                ticket,
                &missing,
                checkpoint,
                pending,
            ),
            StrictBaseOpenPoll::Failed(error) => return Err(Box::new(error)),
        }
    };
    assert_eq!(authority.snapshot(), snapshot);
    assert_eq!(authority.startxref(), fixture.startxref);
    let authority = authority.into_shared();

    let tree_limits = PageTreeLimits::default();
    let mut build = authority
        .build_page_index_owned(
            page_tree_context(ids.build, ids.base + 2_100),
            tree_limits,
            PageIndexLimits::default(),
        )
        .expect("strict authority mints an owned cold page-index job");
    let build_store = empty_store(snapshot);
    let cold_index = loop {
        match build.poll(&build_store, &NeverCancelled) {
            PageIndexBuildPoll::Ready(index) => break index,
            PageIndexBuildPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                "page-index",
                &build_store,
                snapshot,
                &fixture.bytes,
                &[ids.build],
                ticket,
                &missing,
                checkpoint,
                pending,
            ),
            PageIndexBuildPoll::Failed(error) => {
                panic!("case={} page index must build: {error}", case.id())
            }
        }
    };

    let mut lookup = authority
        .lookup_page_owned(
            &cold_index,
            0,
            page_tree_context(ids.lookup, ids.base + 3_100),
            tree_limits,
        )
        .expect("strict authority mints an owned exact-page lookup");
    let lookup_store = empty_store(snapshot);
    let lookup = loop {
        match lookup.poll(&lookup_store, &NeverCancelled) {
            PageLookupPoll::Ready(lookup) => break lookup,
            PageLookupPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                "page-lookup",
                &lookup_store,
                snapshot,
                &fixture.bytes,
                &[ids.lookup],
                ticket,
                &missing,
                checkpoint,
                pending,
            ),
            PageLookupPoll::Failed(error) => {
                panic!("case={} page zero must resolve: {error}", case.id())
            }
        }
    };
    let (page_index, handle) = lookup.into_parts();
    assert_eq!(handle.index(), 0);
    assert_eq!(handle.object().number(), PAGE_OBJECT_NUMBER);
    page_index
        .validate_handle(handle)
        .expect("refined Page index validates its exact handle");

    let mut materialize = authority
        .materialize_page_owned(
            &page_index,
            handle,
            PageMaterializationJobContext::new(
                ids.materialize,
                ResumeCheckpoint::new(ids.base + 4_101),
                ResumeCheckpoint::new(ids.base + 4_102),
                RequestPriority::VisiblePage,
            ),
            PageMaterializationLimits::default(),
        )
        .expect("strict authority mints owned Page materialization");
    let materialize_store = empty_store(snapshot);
    let page = loop {
        match materialize.poll(&materialize_store, &NeverCancelled) {
            PageMaterializationPoll::Ready(page) => break page,
            PageMaterializationPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                "page-materialization",
                &materialize_store,
                snapshot,
                &fixture.bytes,
                &[ids.materialize],
                ticket,
                &missing,
                checkpoint,
                pending,
            ),
            PageMaterializationPoll::Failed(error) => {
                panic!(
                    "case={} Page materialization must succeed: {error}",
                    case.id()
                )
            }
        }
    };

    let mut content = authority
        .acquire_page_content_owned(
            &page_index,
            page,
            PageContentJobContext::new(
                ids.content,
                ResumeCheckpoint::new(ids.base + 5_101),
                ResumeCheckpoint::new(ids.base + 5_102),
                ResumeCheckpoint::new(ids.base + 5_103),
                RequestPriority::VisiblePage,
            ),
            PageContentLimits::default(),
        )
        .expect("strict authority mints owned Page-content acquisition");
    let content_store = empty_store(snapshot);
    let acquired = loop {
        match content.poll(&content_store, &NeverCancelled) {
            PageContentPoll::Ready(acquired) => break acquired,
            PageContentPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                "page-content",
                &content_store,
                snapshot,
                &fixture.bytes,
                &[ids.content],
                ticket,
                &missing,
                checkpoint,
                pending,
            ),
            PageContentPoll::Failed(error) => {
                panic!("case={} Page content must acquire: {error}", case.id())
            }
        }
    };
    assert_eq!(acquired.handle(), handle);
    assert_eq!(acquired.len(), 1);

    Ok(AcquiredPipeline {
        authority,
        acquired,
        snapshot,
        ids,
    })
}

fn make_vm(
    acquired: AcquiredPipeline,
    image_limits: ContentImageLimits,
) -> (InterpretPageJob, RangeStore, [JobId; 2]) {
    let ids = acquired.ids;
    let image_profile = ContentImageProfile::new(
        acquired.authority.clone(),
        PageXObjectLookupLimits::default(),
        ImageXObjectJobContext::new(
            ids.image,
            ResumeCheckpoint::new(ids.base + 6_101),
            ResumeCheckpoint::new(ids.base + 6_102),
            ResumeCheckpoint::new(ids.base + 6_103),
            RequestPriority::FirstViewportResource,
        ),
        ImageXObjectLimits::default(),
        image_limits,
    );
    let font_profile = ContentFontProfile::new(
        acquired.authority,
        PageFontLookupLimits::default(),
        FontResourceJobContext::new(
            ids.font,
            ResumeCheckpoint::new(ids.base + 7_101),
            ResumeCheckpoint::new(ids.base + 7_102),
            ResumeCheckpoint::new(ids.base + 7_103),
            ResumeCheckpoint::new(ids.base + 7_104),
            ResumeCheckpoint::new(ids.base + 7_105),
            ResumeCheckpoint::new(ids.base + 7_106),
            ResumeCheckpoint::new(ids.base + 7_107),
            RequestPriority::FirstViewportResource,
        ),
        FontResourceLimits::default(),
        ContentFontLimits::default(),
    );
    let vm = InterpretPageJob::new_graphics_v2_with_images_and_fonts(
        acquired.acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
        PagePropertyLookupLimits::default(),
        image_profile,
        font_profile,
        GraphicsSceneLimits::default(),
    );
    (vm, empty_store(acquired.snapshot), [ids.image, ids.font])
}

fn drive_vm(
    vm: &mut InterpretPageJob,
    store: &RangeStore,
    fixture: &Fixture,
    expected_jobs: &[JobId],
    pending: &mut Vec<PendingEvent>,
) -> VmTerminal {
    loop {
        match vm.poll(store, &NeverCancelled) {
            ContentVmPoll::Ready(page) => return VmTerminal::Ready(page),
            ContentVmPoll::Unsupported(error) => return VmTerminal::Unsupported(error),
            ContentVmPoll::Failed(error) => return VmTerminal::Failed(error),
            ContentVmPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                "content-vm",
                store,
                store.snapshot(),
                &fixture.bytes,
                expected_jobs,
                ticket,
                &missing,
                checkpoint,
                pending,
            ),
        }
    }
}

fn page_tree_context(job: JobId, seed: u64) -> PageTreeJobContext {
    PageTreeJobContext::new(
        job,
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        RequestPriority::VisiblePage,
    )
}

fn empty_store(snapshot: SourceSnapshot) -> RangeStore {
    RangeStore::new(snapshot, Default::default()).expect("default Range store limits validate")
}

fn source_snapshot(fixture: &Fixture) -> SourceSnapshot {
    let len = u64::try_from(fixture.bytes.len()).expect("fixture length fits u64");
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([fixture.salt; 32]),
            SourceRevision::new(u64::from(fixture.salt)),
        ),
        Some(len),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [fixture.salt.wrapping_add(1); 32],
        ),
    )
}

fn replacement_snapshot(original: SourceSnapshot, salt: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([salt ^ 0xff; 32]),
            SourceRevision::new(u64::from(salt) + 1),
        ),
        original.len(),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [salt.wrapping_add(2); 32],
        ),
    )
}

fn content_image_limits(max_decoded_bytes: u64) -> ContentImageLimits {
    ContentImageLimits::validate(ContentImageLimitConfig {
        max_decoded_bytes,
        ..ContentImageLimitConfig::default()
    })
    .expect("positive case-owned image limits validate")
}

fn raster_output_limits(max_output_bytes: u64) -> ReferenceRasterLimits {
    ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
        max_output_bytes,
        ..ReferenceRasterLimitConfig::default()
    })
    .expect("positive case-owned Reference output limits validate")
}

fn strict_diagnostic(error: StrictBaseOpenError) -> &'static str {
    match error {
        StrictBaseOpenError::Xref(error) => error.diagnostic_id(),
        StrictBaseOpenError::Document(error) => error.diagnostic_id(),
    }
}

fn vm_label(outcome: &VmTerminal) -> &'static str {
    match outcome {
        VmTerminal::Ready(_) => "Ready",
        VmTerminal::Unsupported(_) => "Unsupported",
        VmTerminal::Failed(_) => "Failed",
    }
}

fn assert_ready_content(case: Case, page: &pdf_rs_content::InterpretedPage) {
    let graphics = page
        .scene()
        .graphics()
        .expect("combined M3 Content profile publishes Scene v2");
    assert!(graphics.is_supported());
    match case {
        Case::ValidPathClip => {
            assert_eq!(page.image_uses().len(), 0);
            assert_eq!(page.font_uses().len(), 0);
            assert_eq!(graphics.commands().len(), 5);
            assert!(matches!(
                graphics.commands()[1].command(),
                GraphicsCommand::Clip { .. }
            ));
        }
        Case::ValidStroke => {
            assert_eq!(page.image_uses().len(), 0);
            assert_eq!(page.font_uses().len(), 0);
            assert!(graphics.commands().iter().any(|record| matches!(
                record.command(),
                GraphicsCommand::Stroke { .. } | GraphicsCommand::FillStroke { .. }
            )));
        }
        Case::ValidImage => {
            assert_eq!(page.image_uses().len(), 1);
            assert_eq!(page.font_uses().len(), 0);
            assert_eq!(page.image_stats().acquisitions(), 1);
        }
        Case::ValidFont => {
            assert_eq!(page.image_uses().len(), 0);
            assert_eq!(page.font_uses().len(), 1);
            assert_eq!(page.font_stats().acquisitions(), 1);
        }
        Case::ValidMixed => {
            assert_eq!(page.image_uses().len(), 1);
            assert_eq!(page.font_uses().len(), 1);
            assert_eq!(page.image_stats().acquisitions(), 1);
            assert_eq!(page.font_stats().acquisitions(), 1);
            assert_eq!(graphics.commands().len(), 5);
            assert!(matches!(
                graphics.commands()[0].command(),
                GraphicsCommand::Fill { .. }
            ));
            assert!(matches!(
                graphics.commands()[1].command(),
                GraphicsCommand::Save
            ));
            assert!(matches!(
                graphics.commands()[2].command(),
                GraphicsCommand::DrawImage { .. }
            ));
            assert!(matches!(
                graphics.commands()[3].command(),
                GraphicsCommand::Restore
            ));
            assert!(matches!(
                graphics.commands()[4].command(),
                GraphicsCommand::DrawGlyphRun(_)
            ));
        }
        _ => panic!("only successful cases have Ready Content assertions"),
    }
}

fn assert_ready_pixels(case: Case, buffer: &CanonicalPixelBuffer) {
    assert_eq!(
        buffer.rgba().len(),
        usize::try_from(u64::from(buffer.width()) * u64::from(buffer.height()) * 4).unwrap()
    );
    match case {
        Case::ValidPathClip => {
            assert_eq!(
                buffer.rgba(),
                &[0, 0, 0, 255, 255, 0, 0, 255],
                "clip/fill/source order has a literal two-pixel authority"
            );
            assert!(buffer.stats().geometry_segments() > 0);
            assert!(buffer.stats().clip_bytes() > 0);
        }
        Case::ValidStroke => {
            assert!(has_non_white_pixel(buffer.rgba()));
            assert!(buffer.stats().stroke_runs() > 0);
            assert!(buffer.stats().stroke_primitives() > 0);
            assert!(buffer.stats().dash_chunks() > 0);
        }
        Case::ValidImage => {
            assert_eq!(
                buffer.rgba(),
                &[255, 0, 0, 255, 0, 0, 255, 255],
                "basic image sampling has a literal red/blue authority"
            );
            assert_eq!(buffer.stats().image_commands(), 1);
            assert_eq!(buffer.stats().image_decoded_bytes(), IMAGE_RGB.len() as u64);
        }
        Case::ValidFont => {
            assert!(has_non_white_pixel(buffer.rgba()));
            assert_eq!(buffer.stats().glyph_runs(), 1);
            assert_eq!(buffer.stats().glyphs(), 1);
            assert!(buffer.stats().glyph_outline_segments() > 0);
        }
        Case::ValidMixed => {
            assert!(has_non_white_pixel(buffer.rgba()));
            assert_eq!(buffer.stats().image_commands(), 1);
            assert_eq!(buffer.stats().glyph_runs(), 1);
            assert_eq!(buffer.stats().commands(), 5);
            let rgba_sha256 = hex_digest(
                &sha256(buffer.rgba()).expect("bounded mixed RGBA output fits SHA-256 framing"),
            );
            // This frozen digest is an implementation-bound Reference regression for the
            // overlapping path -> image -> glyph order. It is not independent O0/O1 authority.
            assert_eq!(
                rgba_sha256,
                "05c2256f5ef14fc8c0733f273a2827846bf0b854bbaec5027e0278ca7f864a1e"
            );
        }
        _ => panic!("only successful cases have Ready pixel assertions"),
    }
}

fn has_non_white_pixel(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4)
        .any(|pixel| pixel != [255, 255, 255, 255])
}

struct ChangedSnapshotNoPoll {
    replacement: SourceSnapshot,
}

impl ByteSource for ChangedSnapshotNoPoll {
    fn snapshot(&self) -> SourceSnapshot {
        self.replacement
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("source snapshot guard must reject before polling changed bytes")
    }
}

struct RasterCancellation {
    calls: AtomicU64,
    cancel_at: Option<u64>,
}

impl RasterCancellation {
    fn never() -> Self {
        Self {
            calls: AtomicU64::new(0),
            cancel_at: None,
        }
    }

    fn at(call: u64) -> Self {
        Self {
            calls: AtomicU64::new(0),
            cancel_at: Some(call),
        }
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ReferenceRasterCancellation for RasterCancellation {
    fn is_cancelled(&self) -> bool {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.cancel_at.is_some_and(|cancel_at| call >= cancel_at)
    }
}
