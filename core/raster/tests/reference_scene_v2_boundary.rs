use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_raster::reference::{
    ReferenceRasterCancellation, ReferenceRasterLimitConfig, ReferenceRasterLimits,
    ReferenceRenderConfig, ReferenceRenderJob, ReferenceRenderLimitKind, ReferenceRenderPhase,
    ReferenceRenderPoll, ReferenceRenderUnsupportedKind,
};
use pdf_rs_scene::{
    BlendMode, CapabilityContext, CapabilityStatus, CommandSource, DeviceColor, FillRule,
    GraphicsCapability, GraphicsSceneBuilder, GraphicsSceneLimits, Matrix, PageGeometry,
    PageRotation, Paint, PathResource, PathSegment, Scene, SceneBinding, SceneBounds, ScenePoint,
    SceneRect, SceneScalar, SceneUnit,
};
use pdf_rs_syntax::ObjectRef;

struct Cancellation(AtomicU64);

impl Cancellation {
    fn never() -> Self {
        Self(AtomicU64::new(0))
    }

    fn calls(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl ReferenceRasterCancellation for Cancellation {
    fn is_cancelled(&self) -> bool {
        self.0.fetch_add(1, Ordering::SeqCst);
        false
    }
}

fn binding() -> SceneBinding {
    SceneBinding::new(
        SourceIdentity::new(SourceStableId::new([0x31; 32]), SourceRevision::new(7)),
        91,
        0,
        ObjectRef::new(3, 0).unwrap(),
    )
}

fn geometry() -> PageGeometry {
    let bounds = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::from_decimal("10").unwrap(),
        SceneScalar::from_decimal("10").unwrap(),
    ])
    .unwrap();
    PageGeometry::new(bounds, bounds, PageRotation::Degrees0)
}

fn source(index: u32) -> CommandSource {
    CommandSource::new(
        ObjectRef::new(4, 0).unwrap(),
        0,
        u64::from(index) * 2,
        1,
        index,
    )
    .unwrap()
}

fn triangle() -> PathResource {
    PathResource::new(vec![
        PathSegment::MoveTo(ScenePoint::new(SceneScalar::ZERO, SceneScalar::ZERO)),
        PathSegment::LineTo(ScenePoint::new(SceneScalar::ONE, SceneScalar::ZERO)),
        PathSegment::LineTo(ScenePoint::new(SceneScalar::ZERO, SceneScalar::ONE)),
        PathSegment::ClosePath,
    ])
    .unwrap()
}

fn visible_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), geometry(), GraphicsSceneLimits::default());
    builder
        .append_fill(
            triangle(),
            FillRule::Nonzero,
            Paint::new(
                DeviceColor::Gray(SceneUnit::ZERO),
                SceneUnit::ONE,
                BlendMode::Normal,
            ),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

#[test]
fn supported_visible_v2_command_is_not_silently_rendered_as_a_white_page() {
    let scene = visible_scene();
    let released = Arc::downgrade(&scene);
    let cancellation = Cancellation::never();
    let mut job = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(2, 2).unwrap(),
        ReferenceRasterLimits::default(),
    );
    let unsupported = match job.poll(&cancellation) {
        ReferenceRenderPoll::Unsupported(value) => value,
        outcome => panic!("visible v2 must fail closed: {outcome:?}"),
    };
    assert_eq!(
        unsupported.kind(),
        ReferenceRenderUnsupportedKind::VisibleGraphicsCommand
    );
    assert_eq!(unsupported.index(), 0);
    assert_eq!(unsupported.diagnostic_id(), "RPE-RASTER-0008");
    assert_eq!(job.phase(), ReferenceRenderPhase::Unsupported);
    assert!(
        released.upgrade().is_none(),
        "terminal unsupported output must release the source Scene"
    );

    let replay_calls = cancellation.calls();
    assert_eq!(
        job.poll(&cancellation),
        ReferenceRenderPoll::Unsupported(unsupported)
    );
    assert_eq!(cancellation.calls(), replay_calls);

    let limits = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
        max_fuel: 3,
        ..ReferenceRasterLimitConfig::default()
    })
    .unwrap();
    let mut pixel_fuel_one_less = ReferenceRenderJob::new(
        visible_scene(),
        ReferenceRenderConfig::opaque_srgb(16, 16).unwrap(),
        limits,
    );
    assert!(matches!(
        pixel_fuel_one_less.poll(&Cancellation::never()),
        ReferenceRenderPoll::Unsupported(value)
            if value.kind() == ReferenceRenderUnsupportedKind::VisibleGraphicsCommand
    ));
}

#[test]
fn requirement_count_has_exact_stats_and_one_less_preallocation_rejection() {
    let mut supported =
        GraphicsSceneBuilder::new_v2(binding(), geometry(), GraphicsSceneLimits::default());
    supported
        .add_requirement(
            GraphicsCapability::DeviceColor,
            1,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Supported,
        )
        .unwrap();
    let supported = Arc::new(supported.finish().unwrap());
    let limits = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
        max_requirements: 1,
        max_fuel: 2,
        ..ReferenceRasterLimitConfig::default()
    })
    .unwrap();
    let mut exact = ReferenceRenderJob::new(
        Arc::clone(&supported),
        ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
        limits,
    );
    match exact.poll(&Cancellation::never()) {
        ReferenceRenderPoll::Ready(buffer) => {
            assert_eq!(buffer.stats().requirements(), 1);
            assert_eq!(buffer.stats().fuel(), 2);
        }
        outcome => panic!("exact requirement profile must render: {outcome:?}"),
    }

    let visible = visible_scene();
    assert_eq!(visible.graphics().unwrap().requirements().len(), 2);
    let released = Arc::downgrade(&visible);
    let limits = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
        max_requirements: 1,
        ..ReferenceRasterLimitConfig::default()
    })
    .unwrap();
    let mut one_less = ReferenceRenderJob::new(
        visible,
        ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
        limits,
    );
    match one_less.poll(&Cancellation::never()) {
        ReferenceRenderPoll::Failed(error) => assert_eq!(
            error.limit().unwrap().kind(),
            ReferenceRenderLimitKind::Requirements
        ),
        outcome => panic!("one-less requirement budget must fail: {outcome:?}"),
    }
    assert!(
        released.upgrade().is_none(),
        "preallocation limit failure must release the source Scene"
    );
}

#[test]
fn unsupported_capability_precedes_pixel_allocation_and_visible_command_dispatch() {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), geometry(), GraphicsSceneLimits::default());
    builder
        .add_requirement(
            GraphicsCapability::Image,
            8,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Unsupported,
        )
        .unwrap();
    let scene = Arc::new(builder.finish().unwrap());
    let mut job = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
        ReferenceRasterLimits::default(),
    );
    let unsupported = match job.poll(&Cancellation::never()) {
        ReferenceRenderPoll::Unsupported(value) => value,
        outcome => panic!("unsupported capability must be terminal: {outcome:?}"),
    };
    assert_eq!(
        unsupported.kind(),
        ReferenceRenderUnsupportedKind::VisibleGraphicsRequirement
    );
    assert_eq!(unsupported.index(), 0);
    assert_eq!(unsupported.diagnostic_id(), "RPE-RASTER-0007");
}

#[test]
fn unsupported_color_alpha_blend_and_group_requirements_remain_structured() {
    for capability in [
        GraphicsCapability::DeviceColor,
        GraphicsCapability::ConstantAlpha,
        GraphicsCapability::Blend,
        GraphicsCapability::IsolatedGroup,
    ] {
        let mut builder =
            GraphicsSceneBuilder::new_v2(binding(), geometry(), GraphicsSceneLimits::default());
        builder
            .add_requirement(
                capability,
                u64::MAX,
                CapabilityContext::Scene,
                Vec::new(),
                CapabilityStatus::Unsupported,
            )
            .unwrap();
        let scene = Arc::new(builder.finish().unwrap());
        let released = Arc::downgrade(&scene);
        let mut job = ReferenceRenderJob::new(
            scene,
            ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
            ReferenceRasterLimits::default(),
        );
        let unsupported = match job.poll(&Cancellation::never()) {
            ReferenceRenderPoll::Unsupported(value) => value,
            outcome => panic!("{capability:?} must remain structured unsupported: {outcome:?}"),
        };
        assert_eq!(
            unsupported.kind(),
            ReferenceRenderUnsupportedKind::VisibleGraphicsRequirement
        );
        assert_eq!(unsupported.index(), 0);
        assert_eq!(unsupported.diagnostic_id(), "RPE-RASTER-0007");
        assert!(
            released.upgrade().is_none(),
            "unsupported color-family requirement must release the source Scene"
        );
    }
}

#[test]
fn empty_v2_scene_remains_an_explicit_nonpainting_white_result() {
    let scene = Arc::new(
        GraphicsSceneBuilder::new_v2(binding(), geometry(), GraphicsSceneLimits::default())
            .finish()
            .unwrap(),
    );
    let mut job = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
        ReferenceRasterLimits::default(),
    );
    match job.poll(&Cancellation::never()) {
        ReferenceRenderPoll::Ready(buffer) => assert_eq!(buffer.rgba(), &[255, 255, 255, 255]),
        outcome => panic!("empty v2 is explicitly nonpainting: {outcome:?}"),
    }
}
