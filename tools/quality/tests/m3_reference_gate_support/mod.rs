use std::env;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_rs_bytes::{
    ByteSlice, ByteSource, JobId, RangeStore, RangeStoreLimitConfig, RangeStoreLimits, ReadPoll,
    ReadRequest, RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_content::{
    ContentFontLimitConfig, ContentFontLimits, ContentFontProfile, ContentGraphicsLimitConfig,
    ContentGraphicsLimits, ContentImageLimitConfig, ContentImageLimits, ContentImageProfile,
    ContentLimitConfig, ContentLimits, ContentVmErrorCode, ContentVmFailure, ContentVmLimitConfig,
    ContentVmLimits, ContentVmPoll, InterpretPageJob,
};
use pdf_rs_document::{
    AcquiredPageContent, DocumentLimitConfig, DocumentLimits, FontResourceJobContext,
    FontResourceLimitConfig, FontResourceLimits, ImageXObjectJobContext, ImageXObjectLimitConfig,
    ImageXObjectLimits, ImageXObjectUnsupportedKind, NeverCancelled, OpenStrictBaseRevisionJob,
    PageContentJobContext, PageContentLimitConfig, PageContentLimits, PageContentPoll,
    PageFontLookupLimitConfig, PageFontLookupLimits, PageIndexBuildPoll, PageIndexLimits,
    PageLookupPoll, PageMaterializationJobContext, PageMaterializationLimitConfig,
    PageMaterializationLimits, PageMaterializationPoll, PagePropertyLookupLimitConfig,
    PagePropertyLookupLimits, PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits,
    PageXObjectLookupLimitConfig, PageXObjectLookupLimits, RevisionAttestationJobContext,
    RevisionAttestationLimitConfig, RevisionAttestationLimits, RevisionId,
    SharedAttestedRevisionIndex, StrictBaseOpenContext, StrictBaseOpenError, StrictBaseOpenLimits,
    StrictBaseOpenPoll,
};
use pdf_rs_filters::{DecodeLimitConfig, DecodeLimits};
use pdf_rs_font::{FontLimitConfig, FontLimits};
use pdf_rs_object::{ObjectLimitConfig, ObjectLimits};
use pdf_rs_raster::reference::{
    CanonicalPixelBuffer, ReferenceCapabilityDecision, ReferenceRasterCancellation,
    ReferenceRasterLimitConfig, ReferenceRasterLimits, ReferenceRenderConfig,
    ReferenceRenderErrorCode, ReferenceRenderJob, ReferenceRenderLimitKind, ReferenceRenderPhase,
    ReferenceRenderPoll,
};
use pdf_rs_scene::{GraphicsCommand, GraphicsSceneLimitConfig, GraphicsSceneLimits};
use pdf_rs_syntax::{SyntaxLimitConfig, SyntaxLimits};
use pdf_rs_xref::{XrefJobContext, XrefLimitConfig, XrefLimits};

mod artifact;
mod fixture;
mod pending;
mod registry;

use artifact::{
    LimitEvidence, NormalizedOutcome, OutcomeInput, RenderEvidence, canonical_pixel, normalize,
    write_outputs,
};
use fixture::{FixtureSpec, IMAGE_RGB, ImageSpec, PAGE_OBJECT_NUMBER, build_fixture};
use pending::{PendingEvent, complete_pending, source_changed_pending};
use registry::{CaseContract, load_registry};

const PATH_CLIP_CONTENT: &[u8] = b"q 0 0 50 100 re W n 0 g 0 0 100 100 re f Q \
                                  1 0 0 rg 50 0 50 100 re f";
const STROKE_CONTENT: &[u8] = b"2 w [4 2] 0 d 5 5 90 90 re S 0.5 g 10 10 80 80 re B*";
const IMAGE_CONTENT: &[u8] = b"q 100 0 0 100 0 0 cm /Im0 Do Q";
const FONT_CONTENT: &[u8] = b"BT /F0 1000 Tf 0 0 Td (A) Tj ET";
const MIXED_CONTENT: &[u8] = b"0.8 g 0 0 100 100 re f \
                               q 50 0 0 100 0 0 cm /Im0 Do Q \
                               0 g BT /F0 500 Tf 0 0 Td (A) Tj ET";
const INVALID_CONTENT: &[u8] = b"q";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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

    fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "raster/m3-reference/valid-path-clip" => Self::ValidPathClip,
            "raster/m3-reference/valid-stroke" => Self::ValidStroke,
            "raster/m3-reference/valid-image" => Self::ValidImage,
            "raster/m3-reference/valid-font" => Self::ValidFont,
            "raster/m3-reference/valid-mixed" => Self::ValidMixed,
            "raster/m3-reference/producer-unsupported-interpolated-image" => {
                Self::UnsupportedInterpolatedImage
            }
            "raster/m3-reference/invalid-content-state" => Self::InvalidContentState,
            "raster/m3-reference/strict-invalid-xref" => Self::StrictInvalidXref,
            "raster/m3-reference/cancel-final-publication" => Self::CancelFinalPublication,
            "raster/m3-reference/source-change-after-pending" => Self::SourceChangeAfterPending,
            "raster/m3-reference/image-decoded-one-less" => Self::ImageDecodedOneLess,
            "raster/m3-reference/raster-output-one-less" => Self::RasterOutputOneLess,
            _ => return None,
        })
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
    let registry = load_registry();
    let mut outcomes = Vec::with_capacity(registry.len());
    for contract in &registry {
        let case = contract.case;
        let first_ids = RuntimeIds::for_case(case, 0);
        let second_ids = RuntimeIds::for_case(case, 1);
        assert_ne!(first_ids.open, second_ids.open);
        assert_ne!(first_ids.image, second_ids.image);
        assert_ne!(first_ids.font, second_ids.font);
        assert_ne!(first_ids.base, second_ids.base);
        let first = run_case(contract, 0);
        let second = run_case(contract, 1);
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

fn run_case(contract: &CaseContract, replay: u64) -> NormalizedOutcome {
    let case = contract.case;
    let regenerated = build_fixture(case.fixture_spec());
    assert_eq!(
        regenerated.bytes,
        contract.input,
        "case={} committed input differs from the deterministic fixture self-check",
        case.id()
    );
    match case {
        Case::ValidPathClip
        | Case::ValidStroke
        | Case::ValidImage
        | Case::ValidFont
        | Case::ValidMixed => run_ready_case(contract, replay),
        Case::UnsupportedInterpolatedImage => run_unsupported_case(contract, replay),
        Case::InvalidContentState => run_invalid_content_case(contract, replay),
        Case::StrictInvalidXref => run_invalid_strict_case(contract, replay),
        Case::CancelFinalPublication => run_cancel_case(contract, replay),
        Case::SourceChangeAfterPending => run_source_change_case(contract, replay),
        Case::ImageDecodedOneLess => run_image_one_less_case(contract, replay),
        Case::RasterOutputOneLess => run_raster_one_less_case(contract, replay),
    }
}

fn run_ready_case(contract: &CaseContract, replay: u64) -> NormalizedOutcome {
    let case = contract.case;
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(contract, replay, &mut pending)
        .unwrap_or_else(|error| panic!("case={} strict open must succeed: {error}", case.id()));
    let (mut vm, store, expected_jobs) = make_vm(acquired, contract);
    let page = match drive_vm(&mut vm, &store, contract, &expected_jobs, &mut pending) {
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
    let config = render_config(contract);
    let limits = raster_limits(contract);
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
    assert_ready_render_stats(case, &buffer);

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
        contract,
        outcome: "ready",
        stage: "reference-render",
        diagnostic_id: None,
        pending: &pending,
        scene: Some(scene_bytes),
        pixel: Some(canonical_pixel(&buffer)),
        render: Some(render),
        limit: None,
    })
}

fn run_unsupported_case(contract: &CaseContract, replay: u64) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(contract, replay, &mut pending)
        .unwrap_or_else(|error| panic!("unsupported fixture must strictly open: {error}"));
    let (mut vm, store, jobs) = make_vm(acquired, contract);
    // `/Interpolate true` is outside the registered basic Image producer profile. The proof-bound
    // Content layer therefore returns its own structured capability result before a Scene exists;
    // this case intentionally does not mislabel that strong producer boundary as a raster outcome.
    let unsupported = match drive_vm(&mut vm, &store, contract, &jobs, &mut pending) {
        VmTerminal::Unsupported(unsupported) => unsupported,
        outcome => panic!(
            "interpolated image must be rejected by the Native producer capability boundary: {}",
            vm_label(&outcome)
        ),
    };
    assert_eq!(
        Some(unsupported.diagnostic_id()),
        contract.terminal.diagnostic_id.as_deref()
    );
    assert_eq!(
        unsupported
            .image_xobject()
            .expect("producer capability retains lower Image reason")
            .kind(),
        ImageXObjectUnsupportedKind::Interpolation
    );
    assert_eq!(vm.image_stats().image_uses(), 0);
    normalize(OutcomeInput {
        contract,
        outcome: "unsupported",
        stage: "content-image",
        diagnostic_id: Some(unsupported.diagnostic_id()),
        pending: &pending,
        scene: None,
        pixel: None,
        render: None,
        limit: None,
    })
}

fn run_invalid_content_case(contract: &CaseContract, replay: u64) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(contract, replay, &mut pending)
        .unwrap_or_else(|error| panic!("invalid Content fixture must strictly open: {error}"));
    let (mut vm, store, jobs) = make_vm(acquired, contract);
    let failure = match drive_vm(&mut vm, &store, contract, &jobs, &mut pending) {
        VmTerminal::Failed(error) => error,
        outcome => panic!("unbalanced q must fail Content: {}", vm_label(&outcome)),
    };
    assert_eq!(
        Some(failure.diagnostic_id()),
        contract.terminal.diagnostic_id.as_deref()
    );
    normalize(OutcomeInput {
        contract,
        outcome: "failed",
        stage: "content-vm",
        diagnostic_id: Some(failure.diagnostic_id()),
        pending: &pending,
        scene: None,
        pixel: None,
        render: None,
        limit: None,
    })
}

fn run_invalid_strict_case(contract: &CaseContract, replay: u64) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let error = match acquire_pipeline(contract, replay, &mut pending) {
        Ok(_) => panic!("corrupt startxref must not publish strict authority"),
        Err(error) => error,
    };
    let diagnostic = strict_diagnostic(*error);
    assert_eq!(Some(diagnostic), contract.terminal.diagnostic_id.as_deref());
    normalize(OutcomeInput {
        contract,
        outcome: "failed",
        stage: "strict-open",
        diagnostic_id: Some(diagnostic),
        pending: &pending,
        scene: None,
        pixel: None,
        render: None,
        limit: None,
    })
}

fn run_cancel_case(contract: &CaseContract, replay: u64) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(contract, replay, &mut pending)
        .unwrap_or_else(|error| panic!("cancellation fixture must strictly open: {error}"));
    let (mut vm, store, jobs) = make_vm(acquired, contract);
    let page = match drive_vm(&mut vm, &store, contract, &jobs, &mut pending) {
        VmTerminal::Ready(page) => page,
        outcome => panic!(
            "cancellation fixture must publish Scene: {}",
            vm_label(&outcome)
        ),
    };
    let scene = page.scene_arc();
    let scene_bytes = scene.canonical_json_bytes().unwrap();
    let config = render_config(contract);
    let limits = raster_limits(contract);

    let measurement = RasterCancellation::never();
    let mut measured = ReferenceRenderJob::new(scene.clone(), config, limits);
    let baseline = match measured.poll(&measurement) {
        ReferenceRenderPoll::Ready(buffer) => buffer,
        outcome => panic!("cancellation measurement must render: {outcome:?}"),
    };
    assert_eq!(baseline.stats().cancellation_checks(), measurement.calls());
    let cancel_at = measurement.calls();
    drop(baseline);
    drop(measured);
    let cancellation = RasterCancellation::at(cancel_at);
    let mut renderer = ReferenceRenderJob::new(scene, config, limits);
    let failure = match renderer.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("final publication cancellation must fail: {outcome:?}"),
    };
    assert_eq!(failure.code(), ReferenceRenderErrorCode::Cancelled);
    assert_eq!(
        Some(failure.diagnostic_id()),
        contract.terminal.diagnostic_id.as_deref()
    );
    assert_eq!(
        renderer.stats().final_conversion_pixels(),
        u64::from(contract.width) * u64::from(contract.height)
    );
    assert_eq!(renderer.stats().retained_bytes(), 0);
    let calls = cancellation.calls();
    assert_eq!(
        renderer.poll(&cancellation),
        ReferenceRenderPoll::Failed(failure)
    );
    assert_eq!(cancellation.calls(), calls);
    normalize(OutcomeInput {
        contract,
        outcome: "cancelled",
        stage: "reference-publication",
        diagnostic_id: Some(failure.diagnostic_id()),
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

fn run_source_change_case(contract: &CaseContract, replay: u64) -> NormalizedOutcome {
    let case = contract.case;
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(contract, replay, &mut pending)
        .unwrap_or_else(|error| panic!("source-change fixture must strictly open: {error}"));
    let original = acquired.snapshot;
    let (mut vm, store, jobs) = make_vm(acquired, contract);
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
    assert_eq!(
        Some(failure.diagnostic_id()),
        contract.terminal.diagnostic_id.as_deref()
    );
    normalize(OutcomeInput {
        contract,
        outcome: "source-changed",
        stage: "content-vm-resume",
        diagnostic_id: Some(failure.diagnostic_id()),
        pending: &pending,
        scene: None,
        pixel: None,
        render: None,
        limit: None,
    })
}

fn run_image_one_less_case(contract: &CaseContract, replay: u64) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(contract, replay, &mut pending)
        .unwrap_or_else(|error| panic!("one-less image fixture must strictly open: {error}"));
    let (mut vm, store, jobs) = make_vm(acquired, contract);
    let failure = match drive_vm(&mut vm, &store, contract, &jobs, &mut pending) {
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
    assert_eq!(
        Some(failure.diagnostic_id()),
        contract.terminal.diagnostic_id.as_deref()
    );
    normalize(OutcomeInput {
        contract,
        outcome: "resource-limited",
        stage: "content-image",
        diagnostic_id: Some(failure.diagnostic_id()),
        pending: &pending,
        scene: None,
        pixel: None,
        render: None,
        limit: Some(limit),
    })
}

fn run_raster_one_less_case(contract: &CaseContract, replay: u64) -> NormalizedOutcome {
    let mut pending = Vec::new();
    let acquired = acquire_pipeline(contract, replay, &mut pending)
        .unwrap_or_else(|error| panic!("one-less raster fixture must strictly open: {error}"));
    let (mut vm, store, jobs) = make_vm(acquired, contract);
    let page = match drive_vm(&mut vm, &store, contract, &jobs, &mut pending) {
        VmTerminal::Ready(page) => page,
        outcome => panic!(
            "one-less raster fixture must publish Scene: {}",
            vm_label(&outcome)
        ),
    };
    let scene = page.scene_arc();
    let scene_bytes = scene.canonical_json_bytes().unwrap();
    let config = render_config(contract);
    let limits = raster_limits(contract);
    let cancellation = RasterCancellation::never();
    let mut renderer = ReferenceRenderJob::new(scene, config, limits);
    let failure = match renderer.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("one-less raster output bytes must fail: {outcome:?}"),
    };
    assert_eq!(failure.code(), ReferenceRenderErrorCode::ResourceLimit);
    let raster_limit = failure.limit().expect("raster failure retains limit");
    assert_eq!(raster_limit.kind(), ReferenceRenderLimitKind::OutputBytes);
    assert_eq!(raster_limit.limit(), contract.max_raster_output_bytes);
    assert_eq!(renderer.stats().retained_bytes(), 0);
    assert_eq!(
        Some(failure.diagnostic_id()),
        contract.terminal.diagnostic_id.as_deref()
    );
    normalize(OutcomeInput {
        contract,
        outcome: "resource-limited",
        stage: "reference-preflight",
        diagnostic_id: Some(failure.diagnostic_id()),
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
    contract: &CaseContract,
    replay: u64,
    pending: &mut Vec<PendingEvent>,
) -> Result<AcquiredPipeline, Box<StrictBaseOpenError>> {
    let case = contract.case;
    let snapshot = source_snapshot(contract);
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
        strict_limits(contract),
    )
    .expect("M3 strict-open contexts and validated profiles are compatible");
    let open_store = empty_store(snapshot, contract);
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
                &contract.input,
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
    let authority = authority.into_shared();

    let tree_limits = page_tree_limits(contract);
    let mut build = authority
        .build_page_index_owned(
            page_tree_context(ids.build, ids.base + 2_100),
            tree_limits,
            PageIndexLimits::default(),
        )
        .expect("strict authority mints an owned cold page-index job");
    let build_store = empty_store(snapshot, contract);
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
                &contract.input,
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
    let lookup_store = empty_store(snapshot, contract);
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
                &contract.input,
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
            materialization_limits(contract),
        )
        .expect("strict authority mints owned Page materialization");
    let materialize_store = empty_store(snapshot, contract);
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
                &contract.input,
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
            page_content_limits(contract),
        )
        .expect("strict authority mints owned Page-content acquisition");
    let content_store = empty_store(snapshot, contract);
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
                &contract.input,
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
    contract: &CaseContract,
) -> (InterpretPageJob, RangeStore, [JobId; 2]) {
    let ids = acquired.ids;
    let image_profile = ContentImageProfile::new(
        acquired.authority.clone(),
        page_xobject_lookup_limits(contract),
        ImageXObjectJobContext::new(
            ids.image,
            ResumeCheckpoint::new(ids.base + 6_101),
            ResumeCheckpoint::new(ids.base + 6_102),
            ResumeCheckpoint::new(ids.base + 6_103),
            RequestPriority::FirstViewportResource,
        ),
        image_xobject_limits(contract),
        content_image_limits(contract),
    );
    let font_profile = ContentFontProfile::new(
        acquired.authority,
        page_font_lookup_limits(contract),
        FontResourceJobContext::new(
            ids.font,
            ResumeCheckpoint::new(ids.base + 7_101),
            ResumeCheckpoint::new(ids.base + 7_102),
            ResumeCheckpoint::new(ids.base + 7_103),
            ResumeCheckpoint::new(ids.base + 7_104),
            ResumeCheckpoint::new(ids.base + 7_105),
            ResumeCheckpoint::new(ids.base + 7_106),
            ResumeCheckpoint::new(ids.base + 7_107),
            ResumeCheckpoint::new(ids.base + 7_108),
            ResumeCheckpoint::new(ids.base + 7_109),
            ResumeCheckpoint::new(ids.base + 7_110),
            ResumeCheckpoint::new(ids.base + 7_111),
            RequestPriority::FirstViewportResource,
        ),
        font_resource_limits(contract),
        content_font_limits(contract),
    );
    let vm = InterpretPageJob::new_graphics_v2_with_images_and_fonts(
        acquired.acquired,
        content_limits(contract),
        vm_limits(contract),
        content_graphics_limits(contract),
        property_limits(contract),
        image_profile,
        font_profile,
        graphics_scene_limits(contract),
    );
    (
        vm,
        empty_store(acquired.snapshot, contract),
        [ids.image, ids.font],
    )
}

fn drive_vm(
    vm: &mut InterpretPageJob,
    store: &RangeStore,
    contract: &CaseContract,
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
                &contract.input,
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

fn empty_store(snapshot: SourceSnapshot, contract: &CaseContract) -> RangeStore {
    RangeStore::new(snapshot, range_limits(contract))
        .expect("manifest-owned Range store limits validate")
}

fn source_snapshot(contract: &CaseContract) -> SourceSnapshot {
    let salt = contract.case.salt();
    let len = u64::try_from(contract.input.len()).expect("case input length fits u64");
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([salt; 32]),
            SourceRevision::new(u64::from(salt)),
        ),
        Some(len),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [salt.wrapping_add(1); 32],
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

fn checked_product(left: u64, right: u64, label: &str) -> u64 {
    left.checked_mul(right)
        .unwrap_or_else(|| panic!("{label} fits u64"))
}

fn checked_sum(left: u64, right: u64, label: &str) -> u64 {
    left.checked_add(right)
        .unwrap_or_else(|| panic!("{label} fits u64"))
}

fn object_work_bytes(contract: &CaseContract) -> u64 {
    checked_product(
        contract.max_input_bytes,
        contract.max_objects,
        "M3 object work",
    )
}

fn range_limits(contract: &CaseContract) -> RangeStoreLimits {
    RangeStoreLimits::validate(RangeStoreLimitConfig {
        max_input_bytes: contract.max_input_bytes,
        max_read_bytes: contract.max_input_bytes,
        max_cached_bytes: contract.max_input_bytes,
        max_resident_bytes: checked_product(contract.max_input_bytes, 2, "M3 Range residency"),
        ..RangeStoreLimitConfig::default()
    })
    .expect("manifest-owned Range limits validate")
}

fn strict_limits(contract: &CaseContract) -> StrictBaseOpenLimits {
    StrictBaseOpenLimits::new(
        xref_limits(contract),
        document_limits(contract),
        attestation_limits(contract),
        object_limits(contract),
        syntax_limits(contract),
    )
}

fn xref_limits(contract: &CaseContract) -> XrefLimits {
    let entries = contract
        .max_objects
        .checked_add(1)
        .expect("M3 xref free row fits u64");
    let source_work = checked_product(contract.max_input_bytes, 2, "M3 xref source work");
    XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: contract.max_input_bytes,
        initial_tail_bytes: contract.max_input_bytes,
        max_tail_bytes: contract.max_input_bytes,
        initial_section_bytes: contract.max_input_bytes,
        max_section_bytes: contract.max_input_bytes,
        max_total_read_bytes: source_work,
        max_total_parse_bytes: source_work,
        max_subsections: entries,
        max_entries: entries,
    })
    .expect("manifest-owned xref limits validate")
}

fn document_limits(contract: &CaseContract) -> DocumentLimits {
    let entries = contract
        .max_objects
        .checked_add(1)
        .expect("M3 document free row fits u64");
    DocumentLimits::validate(DocumentLimitConfig {
        max_total_entries: entries,
        max_in_use_entries: contract.max_objects,
        max_logical_index_bytes: object_work_bytes(contract),
        max_sort_steps: checked_product(entries, entries, "M3 document sort work"),
    })
    .expect("manifest-owned document limits validate")
}

fn attestation_limits(contract: &CaseContract) -> RevisionAttestationLimits {
    let object_work = object_work_bytes(contract);
    RevisionAttestationLimits::validate(RevisionAttestationLimitConfig {
        max_source_bytes: contract.max_input_bytes,
        max_objects: contract.max_objects,
        scan_chunk_bytes: contract.max_input_bytes,
        max_trivia_bytes: contract.max_input_bytes,
        max_comment_bytes: contract.max_input_bytes,
        max_total_object_read_bytes: object_work,
        max_total_object_parse_bytes: object_work,
        max_retained_evidence_bytes: object_work,
    })
    .expect("manifest-owned attestation limits validate")
}

fn object_limits(contract: &CaseContract) -> ObjectLimits {
    let source_work = checked_product(contract.max_input_bytes, 2, "M3 object source work");
    ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: contract.max_input_bytes,
        initial_envelope_bytes: contract.max_input_bytes,
        max_envelope_bytes: contract.max_input_bytes,
        initial_boundary_bytes: contract.max_input_bytes,
        max_boundary_bytes: contract.max_input_bytes,
        max_stream_bytes: contract
            .max_stream_output_bytes
            .min(contract.max_input_bytes),
        max_total_read_bytes: source_work,
        max_total_parse_bytes: source_work,
    })
    .expect("manifest-owned object limits validate")
}

fn syntax_limits(contract: &CaseContract) -> SyntaxLimits {
    SyntaxLimits::validate(SyntaxLimitConfig {
        max_input_bytes: contract.max_input_bytes,
        max_token_bytes: contract.max_input_bytes,
        max_comment_bytes: contract.max_input_bytes,
        max_name_bytes: contract.max_input_bytes,
        max_string_source_bytes: contract.max_input_bytes,
        max_string_decoded_bytes: contract.max_stream_output_bytes,
        max_owned_bytes: contract
            .max_stream_output_bytes
            .max(contract.max_input_bytes),
        max_total_tokens: contract.operator_fuel.min(contract.decode_fuel),
        max_container_entries: contract.operator_fuel,
        max_container_bytes: object_work_bytes(contract),
        max_container_depth: u16::try_from(contract.max_resolve_depth)
            .expect("M3 resolve depth fits syntax type"),
    })
    .expect("manifest-owned syntax limits validate")
}

fn page_tree_limits(contract: &CaseContract) -> PageTreeLimits {
    let object_work = object_work_bytes(contract);
    PageTreeLimits::validate(PageTreeLimitConfig {
        max_nodes: contract.max_objects,
        max_depth: contract.max_resolve_depth,
        max_pages: 1,
        max_kids_per_node: contract.max_objects,
        max_total_object_read_bytes: object_work,
        max_total_object_parse_bytes: object_work,
        max_retained_traversal_bytes: object_work,
    })
    .expect("manifest-owned page-tree limits validate")
}

fn materialization_limits(contract: &CaseContract) -> PageMaterializationLimits {
    let object_work = object_work_bytes(contract);
    PageMaterializationLimits::validate(PageMaterializationLimitConfig {
        max_ancestor_depth: contract.max_resolve_depth,
        max_objects: contract.max_objects,
        max_reference_edges: contract.max_objects,
        max_total_object_read_bytes: object_work,
        max_total_object_parse_bytes: object_work,
        max_retained_state_bytes: object_work,
    })
    .expect("manifest-owned materialization limits validate")
}

fn stream_decode_limits(contract: &CaseContract) -> DecodeLimits {
    DecodeLimits::validate(DecodeLimitConfig {
        max_input_bytes: contract.max_input_bytes,
        max_filters: u16::try_from(contract.max_objects).expect("M3 filter count fits u16"),
        max_layer_output_bytes: contract.max_stream_output_bytes,
        max_total_output_bytes: contract.max_stream_output_bytes,
        max_final_output_bytes: contract.max_stream_output_bytes,
        max_retained_capacity_bytes: contract.max_stream_output_bytes,
        max_fuel: contract.decode_fuel,
        cancellation_check_interval_fuel: contract.decode_fuel.min(256),
    })
    .expect("manifest-owned stream decoder limits validate")
}

fn page_content_limits(contract: &CaseContract) -> PageContentLimits {
    PageContentLimits::validate(PageContentLimitConfig {
        max_streams: contract.max_objects,
        max_array_entries: contract.max_objects,
        max_objects: contract.max_objects,
        max_reference_edges: contract.max_objects,
        max_alias_depth: contract.max_resolve_depth,
        max_total_object_read_bytes: object_work_bytes(contract),
        max_total_object_parse_bytes: object_work_bytes(contract),
        max_total_encoded_bytes: contract.max_input_bytes,
        // Content-stream decode and Image aggregate decode are separate ownership domains.
        // The one-less image case intentionally keeps 4 KiB here and applies its 5-byte
        // max_total_decode_bytes only in ContentImageLimits below.
        max_total_decoded_bytes: contract.max_stream_output_bytes,
        max_total_decode_fuel: contract.decode_fuel,
        max_retained_state_bytes: object_work_bytes(contract),
        decode_limits: stream_decode_limits(contract),
    })
    .expect("manifest-owned Page content limits validate")
}

fn content_limits(contract: &CaseContract) -> ContentLimits {
    ContentLimits::validate(ContentLimitConfig {
        max_streams: u32::try_from(contract.max_objects).expect("M3 stream count fits u32"),
        max_total_decoded_bytes: contract.max_stream_output_bytes,
        max_tokens: contract.operator_fuel,
        max_token_bytes: contract.max_stream_output_bytes,
        max_operands_per_operator: u32::try_from(contract.max_objects)
            .expect("M3 operand count fits u32"),
        max_nesting_depth: u16::try_from(contract.max_resolve_depth)
            .expect("M3 Content nesting fits u16"),
        max_operators: contract.operator_fuel,
        max_fuel: contract.operator_fuel,
        max_retained_bytes: object_work_bytes(contract),
    })
    .expect("manifest-owned Content scanner limits validate")
}

fn vm_limits(contract: &CaseContract) -> ContentVmLimits {
    ContentVmLimits::validate(ContentVmLimitConfig {
        max_operators: contract.operator_fuel,
        max_fuel: contract.operator_fuel,
        max_graphics_state_depth: u32::try_from(contract.max_group_depth)
            .expect("M3 graphics-state depth fits u32"),
        max_compatibility_depth: u32::try_from(contract.max_group_depth)
            .expect("M3 compatibility depth fits u32"),
        max_marked_content_depth: u32::try_from(contract.max_group_depth)
            .expect("M3 marked-content depth fits u32"),
        max_property_uses: contract.max_scene_commands,
        max_retained_bytes: object_work_bytes(contract),
    })
    .expect("manifest-owned Content VM limits validate")
}

fn property_limits(contract: &CaseContract) -> PagePropertyLookupLimits {
    PagePropertyLookupLimits::validate(PagePropertyLookupLimitConfig {
        max_lookups: contract.max_scene_commands,
        max_entry_visits: checked_product(
            contract.max_objects,
            contract.max_resolve_depth,
            "M3 property entry visits",
        ),
    })
    .expect("manifest-owned Page property limits validate")
}

fn resource_lookup_entry_visits(contract: &CaseContract) -> u64 {
    checked_product(
        checked_product(
            contract.max_objects,
            contract.max_resolve_depth,
            "M3 resource lookup object-depth work",
        ),
        contract.max_scene_commands,
        "M3 resource lookup command work",
    )
}

fn page_xobject_lookup_limits(contract: &CaseContract) -> PageXObjectLookupLimits {
    PageXObjectLookupLimits::validate(PageXObjectLookupLimitConfig {
        max_lookups: contract.max_scene_commands,
        max_entry_visits: resource_lookup_entry_visits(contract),
        ..PageXObjectLookupLimitConfig::default()
    })
    .expect("manifest-owned Page XObject lookup limits validate")
}

fn page_font_lookup_limits(contract: &CaseContract) -> PageFontLookupLimits {
    PageFontLookupLimits::validate(PageFontLookupLimitConfig {
        max_lookups: contract.max_scene_commands,
        max_entry_visits: resource_lookup_entry_visits(contract),
    })
    .expect("manifest-owned Page Font lookup limits validate")
}

fn content_graphics_limits(contract: &CaseContract) -> ContentGraphicsLimits {
    ContentGraphicsLimits::validate(ContentGraphicsLimitConfig {
        max_path_segments: contract.max_path_segments,
        max_dash_entries: u32::try_from(contract.max_path_segments)
            .expect("M3 dash-entry ceiling fits u32"),
        ..ContentGraphicsLimitConfig::default()
    })
    .expect("manifest-owned Content graphics limits validate")
}

fn image_xobject_limits(contract: &CaseContract) -> ImageXObjectLimits {
    ImageXObjectLimits::validate(ImageXObjectLimitConfig {
        max_pixels: contract.max_image_pixels,
        max_encoded_bytes: contract.max_stream_output_bytes,
        max_decoded_bytes: contract.max_stream_output_bytes,
        max_decode_fuel: contract.decode_fuel,
        decode_limits: stream_decode_limits(contract),
        ..ImageXObjectLimitConfig::default()
    })
    .expect("manifest-owned Image XObject limits validate")
}

fn content_image_limits(contract: &CaseContract) -> ContentImageLimits {
    ContentImageLimits::validate(ContentImageLimitConfig {
        max_image_uses: contract.max_scene_commands,
        max_unique_images: contract.max_scene_commands,
        max_decoded_bytes: contract.max_total_decode_bytes,
        max_planning_operators: contract.operator_fuel,
        max_cache_probes: checked_product(
            contract.max_objects,
            contract.max_scene_commands,
            "M3 image cache probes",
        ),
        max_acquisition_polls: contract.operator_fuel,
        ..ContentImageLimitConfig::default()
    })
    .expect("manifest-owned aggregate Image limits validate")
}

fn font_parser_retained_bytes(contract: &CaseContract) -> u64 {
    checked_product(
        checked_product(
            contract.max_stream_output_bytes,
            contract.max_objects,
            "M3 font stream-object retention",
        ),
        contract.max_group_depth,
        "M3 font depth retention",
    )
}

fn font_resource_retained_bytes(contract: &CaseContract) -> u64 {
    let decoder_state = checked_product(
        contract.max_stream_output_bytes,
        2,
        "M3 Font resource decoder retention",
    );
    checked_sum(
        checked_sum(
            font_parser_retained_bytes(contract),
            object_work_bytes(contract),
            "M3 Font resource parser-object retention",
        ),
        decoder_state,
        "M3 Font resource aggregate retention",
    )
}

fn font_parser_limits(contract: &CaseContract) -> FontLimits {
    let aggregate_shape = checked_product(
        contract.max_path_segments,
        contract.max_scene_commands,
        "M3 aggregate font shape",
    );
    FontLimits::validate(FontLimitConfig {
        max_input_bytes: contract.max_stream_output_bytes,
        max_tables: u16::try_from(contract.max_objects).expect("M3 font table count fits u16"),
        max_glyphs: u32::try_from(contract.max_scene_commands)
            .expect("M3 font glyph count fits u32"),
        max_cmap_segments: u32::try_from(contract.max_scene_commands)
            .expect("M3 cmap segment count fits u32"),
        max_glyph_data_bytes: contract.max_stream_output_bytes,
        max_glyph_bytes: contract.max_stream_output_bytes,
        max_glyph_contours: u32::try_from(contract.max_path_segments)
            .expect("M3 glyph contour count fits u32"),
        max_total_contours: aggregate_shape,
        max_glyph_points: u32::try_from(contract.max_path_segments)
            .expect("M3 glyph point count fits u32"),
        max_total_points: aggregate_shape,
        max_components: aggregate_shape,
        max_component_depth: u16::try_from(contract.max_group_depth)
            .expect("M3 compound-glyph depth fits u16"),
        max_path_segments: aggregate_shape,
        max_retained_bytes: font_parser_retained_bytes(contract),
        max_fuel: contract.operator_fuel,
        cancellation_check_interval_fuel: contract.operator_fuel.min(256),
    })
    .expect("manifest-owned TrueType parser limits validate")
}

fn font_resource_limits(contract: &CaseContract) -> FontResourceLimits {
    let width_entries = checked_product(
        contract.max_objects,
        contract.max_scene_commands,
        "M3 simple-font Widths entries",
    );
    FontResourceLimits::validate(FontResourceLimitConfig {
        max_polls: contract.operator_fuel,
        max_objects: contract.max_objects,
        max_reference_edges: contract.max_objects.min(contract.max_resolve_depth),
        max_metadata_entries: resource_lookup_entry_visits(contract),
        max_widths: width_entries,
        max_object_read_bytes: object_work_bytes(contract),
        max_object_parse_bytes: object_work_bytes(contract),
        // FontFile2 is a stream-owned decode. It deliberately does not inherit the aggregate
        // image budget, so image-decoded-one-less still fails at the Content Image boundary.
        max_encoded_bytes: contract.max_stream_output_bytes,
        max_decoded_bytes: contract.max_stream_output_bytes,
        max_decode_fuel: contract.decode_fuel,
        // Font acquisition keeps PDF objects and decoder capacity live while reserving the
        // complete lower TrueType-parser budget, so its ceiling owns all three domains.
        max_retained_bytes: font_resource_retained_bytes(contract),
        decode_limits: stream_decode_limits(contract),
        font_limits: font_parser_limits(contract),
    })
    .expect("manifest-owned embedded Font resource limits validate")
}

fn content_font_limits(contract: &CaseContract) -> ContentFontLimits {
    let aggregate_shape = checked_product(
        contract.max_path_segments,
        contract.max_scene_commands,
        "M3 aggregate Content Font outline shape",
    );
    let resource_retained = checked_product(
        font_resource_retained_bytes(contract),
        contract.max_objects,
        "M3 aggregate Content Font resource retention",
    );
    let glyph_retained = checked_product(
        checked_product(
            contract.max_stream_output_bytes,
            contract.max_path_segments,
            "M3 Content Font glyph-shape retention",
        ),
        contract.max_group_depth,
        "M3 Content Font expanded-glyph retention",
    );
    let plan_retained = checked_product(
        contract.max_stream_output_bytes,
        contract.max_scene_commands,
        "M3 Content Font plan retention",
    );
    let cache_retained = checked_product(
        contract.max_stream_output_bytes,
        contract.max_objects,
        "M3 Content Font cache retention",
    );
    ContentFontLimits::validate(ContentFontLimitConfig {
        max_font_uses: contract.max_scene_commands,
        max_unique_fonts: contract.max_objects,
        max_resource_retained_bytes: resource_retained,
        max_glyphs: contract.max_scene_commands,
        max_outline_segments: aggregate_shape,
        max_glyph_retained_bytes: glyph_retained,
        max_text_bytes: contract.max_stream_output_bytes,
        max_text_adjustments: contract.max_scene_commands,
        max_planning_operators: contract.operator_fuel,
        max_cache_probes: resource_lookup_entry_visits(contract),
        max_plan_retained_bytes: plan_retained,
        max_cache_retained_bytes: cache_retained,
        max_acquisition_polls: contract.operator_fuel,
    })
    .expect("manifest-owned aggregate Content Font limits validate")
}

fn graphics_scene_limits(contract: &CaseContract) -> GraphicsSceneLimits {
    GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_commands: u32::try_from(contract.max_scene_commands)
            .expect("M3 Scene command ceiling fits u32"),
        max_path_segments: contract.max_path_segments,
        max_image_bytes: contract.max_total_decode_bytes,
        max_state_depth: u32::try_from(contract.max_group_depth)
            .expect("M3 Scene state depth fits u32"),
        max_group_depth: u32::try_from(contract.max_group_depth)
            .expect("M3 Scene group depth fits u32"),
        ..GraphicsSceneLimitConfig::default()
    })
    .expect("manifest-owned graphics Scene limits validate")
}

fn render_config(contract: &CaseContract) -> ReferenceRenderConfig {
    ReferenceRenderConfig::opaque_srgb(contract.width, contract.height)
        .expect("positive manifest-owned output configuration")
}

fn raster_limits(contract: &CaseContract) -> ReferenceRasterLimits {
    let stride = u64::from(contract.width)
        .checked_mul(4)
        .expect("M3 output stride fits u64");
    ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
        max_width: contract.width,
        max_height: contract.height,
        max_pixels: contract.max_image_pixels,
        max_stride_bytes: stride,
        max_output_bytes: contract.max_raster_output_bytes,
        max_commands: contract.max_scene_commands,
        max_image_source_pixels: contract.max_image_pixels,
        max_image_decoded_bytes: contract.max_total_decode_bytes,
        max_clip_depth: u32::try_from(contract.max_group_depth)
            .expect("M3 Reference clip depth fits u32"),
        ..ReferenceRasterLimitConfig::default()
    })
    .expect("manifest-owned Reference output limits validate")
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

fn assert_ready_render_stats(case: Case, buffer: &CanonicalPixelBuffer) {
    assert_eq!(
        buffer.rgba().len(),
        usize::try_from(u64::from(buffer.width()) * u64::from(buffer.height()) * 4).unwrap()
    );
    match case {
        Case::ValidPathClip => {
            assert!(buffer.stats().geometry_segments() > 0);
            assert!(buffer.stats().clip_bytes() > 0);
        }
        Case::ValidStroke => {
            assert!(buffer.stats().stroke_runs() > 0);
            assert!(buffer.stats().stroke_primitives() > 0);
            assert!(buffer.stats().dash_chunks() > 0);
        }
        Case::ValidImage => {
            assert_eq!(buffer.stats().image_commands(), 1);
            assert_eq!(buffer.stats().image_decoded_bytes(), IMAGE_RGB.len() as u64);
        }
        Case::ValidFont => {
            assert_eq!(buffer.stats().glyph_runs(), 1);
            assert_eq!(buffer.stats().glyphs(), 1);
            assert!(buffer.stats().glyph_outline_segments() > 0);
        }
        Case::ValidMixed => {
            assert_eq!(buffer.stats().image_commands(), 1);
            assert_eq!(buffer.stats().glyph_runs(), 1);
            assert_eq!(buffer.stats().commands(), 5);
        }
        _ => panic!("only successful cases have Ready pixel assertions"),
    }
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
