use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_raster::reference::{
    CanonicalPixelBuffer, ReferenceRasterCancellation, ReferenceRasterLimitConfig,
    ReferenceRasterLimits, ReferenceRenderConfig, ReferenceRenderErrorCode, ReferenceRenderJob,
    ReferenceRenderLimitKind, ReferenceRenderPhase, ReferenceRenderPoll,
    ReferenceRenderUnsupportedKind,
};
use pdf_rs_scene::{
    BlendMode, CapabilityContext, CapabilityStatus, CommandSource, DashPattern, DeviceColor,
    FillRule, GlyphOutline, GlyphUse, GraphicsCapability, GraphicsResourceSource,
    GraphicsSceneBuilder, GraphicsSceneLimits, ImageColorSpace, ImageResource, LineCap, LineJoin,
    LineStyle, Matrix, PageGeometry, PageRotation, Paint, PathResource, PathSegment, Scene,
    SceneBinding, SceneBounds, ScenePoint, SceneRect, SceneScalar, SceneUnit,
};
use pdf_rs_syntax::ObjectRef;

type SceneFactory = fn() -> Arc<Scene>;
type UnsupportedCase = (SceneFactory, GraphicsCapability, &'static str);
type WorkingCase = (SceneFactory, u32, u32, &'static str);

struct Cancellation {
    calls: AtomicU64,
    cancel_at: Option<u64>,
}

impl Cancellation {
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

impl ReferenceRasterCancellation for Cancellation {
    fn is_cancelled(&self) -> bool {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.cancel_at.is_some_and(|cancel_at| call >= cancel_at)
    }
}

fn binding() -> SceneBinding {
    SceneBinding::new(
        SourceIdentity::new(SourceStableId::new([0x73; 32]), SourceRevision::new(5)),
        80,
        0,
        ObjectRef::new(3, 0).unwrap(),
    )
}

fn scalar(value: &str) -> SceneScalar {
    SceneScalar::from_decimal(value).unwrap()
}

fn unit_geometry() -> PageGeometry {
    let bounds = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::ONE,
        SceneScalar::ONE,
    ])
    .unwrap();
    PageGeometry::new(bounds, bounds, PageRotation::Degrees0)
}

fn source(index: u32) -> CommandSource {
    CommandSource::new(
        ObjectRef::new(4, 0).unwrap(),
        0,
        u64::from(index) * 4,
        1,
        index,
    )
    .unwrap()
}

fn resource_source(object: u32) -> GraphicsResourceSource {
    GraphicsResourceSource::new(ObjectRef::new(object, 0).unwrap(), 80, u64::from(object))
}

fn point(x: &str, y: &str) -> ScenePoint {
    ScenePoint::new(scalar(x), scalar(y))
}

fn rectangle(left: &str, bottom: &str, right: &str, top: &str) -> PathResource {
    PathResource::new(vec![
        PathSegment::MoveTo(point(left, bottom)),
        PathSegment::LineTo(point(right, bottom)),
        PathSegment::LineTo(point(right, top)),
        PathSegment::LineTo(point(left, top)),
        PathSegment::ClosePath,
    ])
    .unwrap()
}

fn line(start: ScenePoint, end: ScenePoint) -> PathResource {
    PathResource::new(vec![PathSegment::MoveTo(start), PathSegment::LineTo(end)]).unwrap()
}

fn black() -> Paint {
    Paint::new(
        DeviceColor::Gray(SceneUnit::ZERO),
        SceneUnit::ONE,
        BlendMode::Normal,
    )
}

fn red() -> Paint {
    Paint::new(
        DeviceColor::Rgb {
            red: SceneUnit::ONE,
            green: SceneUnit::ZERO,
            blue: SceneUnit::ZERO,
        },
        SceneUnit::ONE,
        BlendMode::Normal,
    )
}

fn clipped_fill_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder.append_save(SceneBounds::Page, source(0)).unwrap();
    builder
        .append_clip(
            rectangle("0", "0", "0.5", "1"),
            FillRule::Nonzero,
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(1),
        )
        .unwrap();
    builder
        .append_fill(
            rectangle("0", "0", "1", "1"),
            FillRule::Nonzero,
            black(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(2),
        )
        .unwrap();
    builder
        .append_restore(SceneBounds::Page, source(3))
        .unwrap();
    builder
        .append_fill(
            rectangle("0.5", "0", "1", "1"),
            FillRule::Nonzero,
            red(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(4),
        )
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn image_scene() -> Arc<Scene> {
    let image = ImageResource::new(
        resource_source(20),
        2,
        1,
        ImageColorSpace::DeviceRgb,
        8,
        false,
        vec![255, 0, 0, 0, 0, 255],
    )
    .unwrap();
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder
        .draw_image(
            image,
            Matrix::IDENTITY,
            SceneUnit::ONE,
            BlendMode::Normal,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn interpolated_image_scene() -> Arc<Scene> {
    let image = ImageResource::new(
        resource_source(22),
        1,
        1,
        ImageColorSpace::DeviceGray,
        8,
        true,
        vec![0],
    )
    .unwrap();
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder
        .draw_image(
            image,
            Matrix::IDENTITY,
            SceneUnit::ONE,
            BlendMode::Normal,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn group_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder
        .begin_group(
            SceneUnit::ONE,
            BlendMode::Normal,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    builder.end_group(SceneBounds::Page, source(1)).unwrap();
    Arc::new(builder.finish().unwrap())
}

fn glyph_scene() -> Arc<Scene> {
    let outline =
        GlyphOutline::new(resource_source(21), 7, 1, rectangle("0", "0", "1", "1")).unwrap();
    let glyphs = vec![
        GlyphUse::new(outline.clone(), Matrix::IDENTITY, 65),
        GlyphUse::new(outline, Matrix::IDENTITY, 65),
    ];
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder
        .draw_glyph_run(glyphs, black(), SceneBounds::Page, source(0))
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn empty_glyph_scene() -> Arc<Scene> {
    let outline = GlyphOutline::new(
        resource_source(23),
        8,
        1,
        PathResource::new(Vec::new()).unwrap(),
    )
    .unwrap();
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder
        .draw_glyph_run(
            vec![GlyphUse::new(outline, Matrix::IDENTITY, 32)],
            black(),
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn stroke_scene() -> Arc<Scene> {
    let dashed = LineStyle::new(
        scalar("0.25"),
        LineCap::Butt,
        LineJoin::Miter,
        scalar("10"),
        DashPattern::new(vec![scalar("0.25"), scalar("0.25")], SceneScalar::ZERO).unwrap(),
        Matrix::IDENTITY,
    )
    .unwrap();
    let solid = LineStyle::new(
        scalar("0.125"),
        LineCap::Square,
        LineJoin::Bevel,
        scalar("10"),
        DashPattern::new(Vec::new(), SceneScalar::ZERO).unwrap(),
        Matrix::IDENTITY,
    )
    .unwrap();
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder
        .append_stroke(
            line(point("0", "0.5"), point("1", "0.5")),
            black(),
            dashed,
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    builder
        .append_fill_stroke(
            rectangle("0.25", "0.25", "0.75", "0.75"),
            FillRule::EvenOdd,
            red(),
            black(),
            solid,
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(1),
        )
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn dependency_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    let color = builder
        .add_requirement(
            GraphicsCapability::DeviceColor,
            1,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Supported,
        )
        .unwrap();
    let fill = builder
        .add_requirement(
            GraphicsCapability::PathFill,
            0,
            CapabilityContext::Scene,
            vec![color],
            CapabilityStatus::Supported,
        )
        .unwrap();
    builder
        .add_requirement(
            GraphicsCapability::Clip,
            0,
            CapabilityContext::Scene,
            vec![color, fill],
            CapabilityStatus::Supported,
        )
        .unwrap();
    builder
        .add_requirement(
            GraphicsCapability::Blend,
            0,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Supported,
        )
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn nested_save_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder.append_save(SceneBounds::Page, source(0)).unwrap();
    builder.append_save(SceneBounds::Page, source(1)).unwrap();
    builder
        .append_restore(SceneBounds::Page, source(2))
        .unwrap();
    builder
        .append_restore(SceneBounds::Page, source(3))
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn ready(
    scene: Arc<Scene>,
    width: u32,
    height: u32,
    limits: ReferenceRasterLimits,
    cancellation: &Cancellation,
) -> Arc<CanonicalPixelBuffer> {
    let mut job = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(width, height).unwrap(),
        limits,
    );
    match job.poll(cancellation) {
        ReferenceRenderPoll::Ready(buffer) => buffer,
        outcome => panic!("integrated render must succeed: {outcome:?}"),
    }
}

#[test]
fn save_clip_fill_restore_and_source_order_publish_literal_pixels_and_replay() {
    let cancellation = Cancellation::never();
    let scene = clipped_fill_scene();
    let released = Arc::downgrade(&scene);
    let mut job = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(2, 1).unwrap(),
        ReferenceRasterLimits::default(),
    );
    let first = match job.poll(&cancellation) {
        ReferenceRenderPoll::Ready(buffer) => buffer,
        outcome => panic!("path command stream must render: {outcome:?}"),
    };
    assert_eq!(first.rgba(), &[0, 0, 0, 255, 255, 0, 0, 255]);
    assert_eq!(first.stats().commands(), 5);
    assert!(first.stats().geometry_segments() > 0);
    assert!(first.stats().geometry_edges() > 0);
    assert!(first.stats().geometry_samples() > 0);
    assert!(first.stats().peak_coverage_bytes() > 0);
    assert_eq!(first.stats().clip_depth(), 0);
    assert!(first.stats().clip_bytes() > 0);
    assert!(first.stats().peak_clip_bytes() >= first.stats().clip_bytes());
    assert_eq!(first.stats().final_conversion_pixels(), 2);
    assert_eq!(job.phase(), ReferenceRenderPhase::Ready);
    assert!(released.upgrade().is_none());

    let calls = cancellation.calls();
    let replay = match job.poll(&cancellation) {
        ReferenceRenderPoll::Ready(buffer) => buffer,
        outcome => panic!("ready result must replay: {outcome:?}"),
    };
    assert!(Arc::ptr_eq(&first, &replay));
    assert_eq!(cancellation.calls(), calls);
}

#[test]
fn mounted_image_and_glyph_kernels_publish_literal_pixels_and_complete_stats() {
    let image = ready(
        image_scene(),
        2,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert_eq!(image.rgba(), &[255, 0, 0, 255, 0, 0, 255, 255]);
    assert_eq!(image.stats().image_commands(), 1);
    assert_eq!(image.stats().image_source_pixels(), 2);
    assert_eq!(image.stats().image_stride_bytes(), 6);
    assert_eq!(image.stats().image_decoded_bytes(), 6);
    assert_eq!(image.stats().image_samples(), 128);
    assert_eq!(image.stats().image_conversions(), 128);

    let glyph = ready(
        glyph_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert_eq!(glyph.rgba(), &[0, 0, 0, 255]);
    assert_eq!(glyph.stats().glyph_runs(), 1);
    assert_eq!(glyph.stats().glyphs(), 2);
    assert_eq!(glyph.stats().glyph_resource_lookups(), 2);
    assert_eq!(glyph.stats().glyph_outline_segments(), 10);
    assert!(glyph.stats().glyph_samples() > 0);
    assert_eq!(glyph.stats().glyph_composites(), 64);
}

#[test]
fn mounted_stroke_and_fill_stroke_dispatch_report_all_stroke_dimensions() {
    let output = ready(
        stroke_scene(),
        4,
        4,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert!(
        output
            .rgba()
            .chunks_exact(4)
            .any(|pixel| pixel != [255, 255, 255, 255])
    );
    assert_eq!(output.stats().commands(), 2);
    assert!(output.stats().dash_chunks() > 0);
    assert!(output.stats().stroke_runs() > 0);
    assert!(output.stats().stroke_primitives() > 0);
    assert!(output.stats().geometry_segments() > 0);
    assert!(output.stats().peak_geometry_bytes() > 0);
}

#[test]
fn group_and_interpolated_image_are_unsupported_before_surface_allocation_and_replay() {
    let cases: [UnsupportedCase; 2] = [
        (group_scene, GraphicsCapability::IsolatedGroup, "group"),
        (
            interpolated_image_scene,
            GraphicsCapability::Image,
            "interpolated-image",
        ),
    ];
    for (scene, capability, label) in cases {
        let scene = scene();
        let released = Arc::downgrade(&scene);
        let cancellation = Cancellation::never();
        let mut job = ReferenceRenderJob::new(
            scene,
            ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
            ReferenceRasterLimits::default(),
        );
        let unsupported = match job.poll(&cancellation) {
            ReferenceRenderPoll::Unsupported(value) => value,
            outcome => panic!("{label} must be structured unsupported: {outcome:?}"),
        };
        assert_eq!(
            unsupported.kind(),
            ReferenceRenderUnsupportedKind::VisibleGraphicsRequirement
        );
        assert_eq!(unsupported.capability(), Some(capability));
        assert_eq!(
            unsupported.producer_status(),
            Some(CapabilityStatus::Supported)
        );
        assert_eq!(job.stats().surface_bytes(), 0);
        assert_eq!(job.stats().peak_working_bytes(), 0);
        assert!(released.upgrade().is_none());

        let calls = cancellation.calls();
        assert_eq!(
            job.poll(&cancellation),
            ReferenceRenderPoll::Unsupported(unsupported)
        );
        assert_eq!(cancellation.calls(), calls);
    }
}

#[test]
fn public_scene_construction_rejects_invalid_context_dependencies_and_resource_identity() {
    let mut invalid_context =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    assert!(
        invalid_context
            .add_requirement(
                GraphicsCapability::PathFill,
                0,
                CapabilityContext::Command(0),
                Vec::new(),
                CapabilityStatus::Supported,
            )
            .is_err()
    );

    let mut invalid_dependencies =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    let first = invalid_dependencies
        .add_requirement(
            GraphicsCapability::DeviceColor,
            1,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Supported,
        )
        .unwrap();
    assert!(
        invalid_dependencies
            .add_requirement(
                GraphicsCapability::PathFill,
                0,
                CapabilityContext::Scene,
                vec![first, first],
                CapabilityStatus::Supported,
            )
            .is_err()
    );

    let shared_identity = resource_source(30);
    let first_image = ImageResource::new(
        shared_identity,
        1,
        1,
        ImageColorSpace::DeviceGray,
        8,
        false,
        vec![0],
    )
    .unwrap();
    let conflicting_image = ImageResource::new(
        shared_identity,
        1,
        1,
        ImageColorSpace::DeviceGray,
        8,
        false,
        vec![255],
    )
    .unwrap();
    let mut invalid_resource =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    invalid_resource
        .draw_image(
            first_image,
            Matrix::IDENTITY,
            SceneUnit::ONE,
            BlendMode::Normal,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    assert!(
        invalid_resource
            .draw_image(
                conflicting_image,
                Matrix::IDENTITY,
                SceneUnit::ONE,
                BlendMode::Normal,
                SceneBounds::Page,
                source(1),
            )
            .is_err()
    );
}

#[test]
fn aggregate_requirement_dependency_and_resource_profiles_have_exact_one_less_boundaries() {
    let requirements = ready(
        dependency_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert_eq!(requirements.stats().requirements(), 4);
    assert_eq!(requirements.stats().dependencies(), 3);

    for (kind, config) in [
        (
            ReferenceRenderLimitKind::Requirements,
            ReferenceRasterLimitConfig {
                max_requirements: 3,
                ..ReferenceRasterLimitConfig::default()
            },
        ),
        (
            ReferenceRenderLimitKind::Dependencies,
            ReferenceRasterLimitConfig {
                max_dependencies: 2,
                ..ReferenceRasterLimitConfig::default()
            },
        ),
    ] {
        let mut job = ReferenceRenderJob::new(
            dependency_scene(),
            ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
            ReferenceRasterLimits::validate(config).unwrap(),
        );
        match job.poll(&Cancellation::never()) {
            ReferenceRenderPoll::Failed(error) => {
                assert_eq!(error.limit().unwrap().kind(), kind)
            }
            outcome => panic!("one-less {kind:?} must fail: {outcome:?}"),
        }
    }

    let resources = ready(
        clipped_fill_scene(),
        2,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert!(resources.stats().resources() > 1);
    let exact_count = resources.stats().resources();
    for (limit, should_succeed) in [(exact_count, true), (exact_count - 1, false)] {
        let limits = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_resources: limit,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap();
        let mut job = ReferenceRenderJob::new(
            clipped_fill_scene(),
            ReferenceRenderConfig::opaque_srgb(2, 1).unwrap(),
            limits,
        );
        match (should_succeed, job.poll(&Cancellation::never())) {
            (true, ReferenceRenderPoll::Ready(_)) => {}
            (false, ReferenceRenderPoll::Failed(error)) => assert_eq!(
                error.limit().unwrap().kind(),
                ReferenceRenderLimitKind::Resources
            ),
            (_, outcome) => panic!("resource boundary mismatch: {outcome:?}"),
        }
    }
}

#[test]
fn combined_working_memory_is_admitted_exactly_and_rejected_one_byte_short() {
    let cases: [WorkingCase; 4] = [
        (clipped_fill_scene, 2, 1, "path-clip"),
        (image_scene, 2, 1, "image"),
        (glyph_scene, 1, 1, "glyph"),
        (empty_glyph_scene, 1, 1, "empty-glyph"),
    ];
    for (scene, width, height, label) in cases {
        let baseline = ready(
            scene(),
            width,
            height,
            ReferenceRasterLimits::default(),
            &Cancellation::never(),
        );
        let peak = baseline.stats().peak_working_bytes();
        assert!(peak > 1, "{label} must retain measurable private work");
        let exact = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_peak_working_bytes: peak,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap();
        let exact_output = ready(scene(), width, height, exact, &Cancellation::never());
        assert_eq!(
            exact_output.rgba(),
            baseline.rgba(),
            "{label} exact profile"
        );
        assert_eq!(exact_output.stats().peak_working_bytes(), peak);

        let one_less = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_peak_working_bytes: peak - 1,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap();
        let mut job = ReferenceRenderJob::new(
            scene(),
            ReferenceRenderConfig::opaque_srgb(width, height).unwrap(),
            one_less,
        );
        match job.poll(&Cancellation::never()) {
            ReferenceRenderPoll::Failed(error) => assert_eq!(
                error.limit().unwrap().kind(),
                ReferenceRenderLimitKind::PeakWorkingBytes,
                "{label} one-less profile"
            ),
            outcome => panic!("{label} one-less working budget must fail: {outcome:?}"),
        }
    }
}

#[test]
fn integrated_image_dimensions_have_exact_and_one_less_aggregate_limits() {
    let baseline = ready(
        image_scene(),
        2,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    let stats = baseline.stats();
    let exact_config = ReferenceRasterLimitConfig {
        max_image_source_pixels: stats.image_source_pixels(),
        max_image_stride_bytes: stats.image_stride_bytes(),
        max_image_decoded_bytes: stats.image_decoded_bytes(),
        max_image_samples: stats.image_samples(),
        max_image_conversions: stats.image_conversions(),
        ..ReferenceRasterLimitConfig::default()
    };
    ready(
        image_scene(),
        2,
        1,
        ReferenceRasterLimits::validate(exact_config).unwrap(),
        &Cancellation::never(),
    );
    for (kind, config) in [
        (
            ReferenceRenderLimitKind::ImageSourcePixels,
            ReferenceRasterLimitConfig {
                max_image_source_pixels: stats.image_source_pixels() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::ImageStrideBytes,
            ReferenceRasterLimitConfig {
                max_image_stride_bytes: stats.image_stride_bytes() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::ImageDecodedBytes,
            ReferenceRasterLimitConfig {
                max_image_decoded_bytes: stats.image_decoded_bytes() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::ImageSamples,
            ReferenceRasterLimitConfig {
                max_image_samples: stats.image_samples() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::ImageConversions,
            ReferenceRasterLimitConfig {
                max_image_conversions: stats.image_conversions() - 1,
                ..exact_config
            },
        ),
    ] {
        let mut job = ReferenceRenderJob::new(
            image_scene(),
            ReferenceRenderConfig::opaque_srgb(2, 1).unwrap(),
            ReferenceRasterLimits::validate(config).unwrap(),
        );
        match job.poll(&Cancellation::never()) {
            ReferenceRenderPoll::Failed(error) => {
                assert_eq!(error.limit().unwrap().kind(), kind)
            }
            outcome => panic!("one-less {kind:?} must fail: {outcome:?}"),
        }
    }
}

#[test]
fn integrated_glyph_dimensions_have_exact_and_one_less_aggregate_limits() {
    let baseline = ready(
        glyph_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    let stats = baseline.stats();
    let exact_config = ReferenceRasterLimitConfig {
        max_glyphs: stats.glyphs(),
        max_glyph_resource_lookups: stats.glyph_resource_lookups(),
        max_glyph_outline_segments: stats.glyph_outline_segments(),
        max_geometry_samples: stats.geometry_samples(),
        max_glyph_samples: stats.glyph_samples(),
        max_glyph_composites: stats.glyph_composites(),
        max_fuel: stats.fuel(),
        ..ReferenceRasterLimitConfig::default()
    };
    ready(
        glyph_scene(),
        1,
        1,
        ReferenceRasterLimits::validate(exact_config).unwrap(),
        &Cancellation::never(),
    );
    for (kind, config) in [
        (
            ReferenceRenderLimitKind::Glyphs,
            ReferenceRasterLimitConfig {
                max_glyphs: stats.glyphs() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::GlyphResourceLookups,
            ReferenceRasterLimitConfig {
                max_glyph_resource_lookups: stats.glyph_resource_lookups() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::GlyphOutlineSegments,
            ReferenceRasterLimitConfig {
                max_glyph_outline_segments: stats.glyph_outline_segments() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::GeometrySamples,
            ReferenceRasterLimitConfig {
                max_geometry_samples: stats.geometry_samples() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::GlyphSamples,
            ReferenceRasterLimitConfig {
                max_glyph_samples: stats.glyph_samples() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::GlyphComposites,
            ReferenceRasterLimitConfig {
                max_glyph_composites: stats.glyph_composites() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::Fuel,
            ReferenceRasterLimitConfig {
                max_fuel: stats.fuel() - 1,
                ..exact_config
            },
        ),
    ] {
        let mut job = ReferenceRenderJob::new(
            glyph_scene(),
            ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
            ReferenceRasterLimits::validate(config).unwrap(),
        );
        match job.poll(&Cancellation::never()) {
            ReferenceRenderPoll::Failed(error) => {
                assert_eq!(error.limit().unwrap().kind(), kind)
            }
            outcome => panic!("one-less {kind:?} must fail: {outcome:?}"),
        }
    }
}

#[test]
fn output_surface_clip_coverage_geometry_and_fuel_profiles_are_exact_and_one_less() {
    let baseline = ready(
        clipped_fill_scene(),
        2,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    let stats = baseline.stats();
    assert_eq!(stats.final_conversion_pixels(), stats.pixels());
    let exact_config = ReferenceRasterLimitConfig {
        max_output_bytes: u64::try_from(baseline.rgba().len()).unwrap(),
        max_geometry_segments: stats.geometry_segments(),
        max_geometry_edges: stats.geometry_edges(),
        max_geometry_samples: stats.geometry_samples(),
        max_coverage_bytes: stats.peak_coverage_bytes(),
        max_geometry_bytes: stats.peak_geometry_bytes(),
        max_clip_bytes: stats.peak_clip_bytes(),
        max_fuel: stats.fuel(),
        max_surface_bytes: stats.surface_bytes(),
        max_peak_working_bytes: stats.peak_working_bytes(),
        max_retained_bytes: stats.retained_bytes(),
        ..ReferenceRasterLimitConfig::default()
    };
    let exact = ready(
        clipped_fill_scene(),
        2,
        1,
        ReferenceRasterLimits::validate(exact_config).unwrap(),
        &Cancellation::never(),
    );
    assert_eq!(exact.rgba(), baseline.rgba());
    assert_eq!(exact.stats(), stats);

    let cases = [
        (
            ReferenceRenderLimitKind::OutputBytes,
            ReferenceRasterLimitConfig {
                max_output_bytes: exact_config.max_output_bytes - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::GeometrySegments,
            ReferenceRasterLimitConfig {
                max_geometry_segments: stats.geometry_segments() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::GeometryEdges,
            ReferenceRasterLimitConfig {
                max_geometry_edges: stats.geometry_edges() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::GeometrySamples,
            ReferenceRasterLimitConfig {
                max_geometry_samples: stats.geometry_samples() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::CoverageBytes,
            ReferenceRasterLimitConfig {
                max_coverage_bytes: stats.peak_coverage_bytes() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::GeometryBytes,
            ReferenceRasterLimitConfig {
                max_geometry_bytes: stats.peak_geometry_bytes() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::ClipBytes,
            ReferenceRasterLimitConfig {
                max_clip_bytes: stats.peak_clip_bytes() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::Fuel,
            ReferenceRasterLimitConfig {
                max_fuel: stats.fuel() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::SurfaceBytes,
            ReferenceRasterLimitConfig {
                max_surface_bytes: stats.surface_bytes() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::PeakWorkingBytes,
            ReferenceRasterLimitConfig {
                max_peak_working_bytes: stats.peak_working_bytes() - 1,
                ..exact_config
            },
        ),
        (
            ReferenceRenderLimitKind::RetainedBytes,
            ReferenceRasterLimitConfig {
                max_retained_bytes: stats.retained_bytes() - 1,
                ..exact_config
            },
        ),
    ];
    for (kind, config) in cases {
        let mut job = ReferenceRenderJob::new(
            clipped_fill_scene(),
            ReferenceRenderConfig::opaque_srgb(2, 1).unwrap(),
            ReferenceRasterLimits::validate(config).unwrap(),
        );
        match job.poll(&Cancellation::never()) {
            ReferenceRenderPoll::Failed(error) => {
                assert_eq!(error.limit().unwrap().kind(), kind)
            }
            outcome => panic!("one-less {kind:?} must fail: {outcome:?}"),
        }
    }

    let exact_depth = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
        max_clip_depth: 2,
        ..ReferenceRasterLimitConfig::default()
    })
    .unwrap();
    ready(
        nested_save_scene(),
        1,
        1,
        exact_depth,
        &Cancellation::never(),
    );
    let one_less_depth = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
        max_clip_depth: 1,
        ..ReferenceRasterLimitConfig::default()
    })
    .unwrap();
    let mut depth_job = ReferenceRenderJob::new(
        nested_save_scene(),
        ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
        one_less_depth,
    );
    match depth_job.poll(&Cancellation::never()) {
        ReferenceRenderPoll::Failed(error) => assert_eq!(
            error.limit().unwrap().kind(),
            ReferenceRenderLimitKind::ClipDepth
        ),
        outcome => panic!("one-less clip depth must fail: {outcome:?}"),
    }
}

#[test]
fn late_output_limit_after_image_paint_is_atomic_releases_scene_and_replays() {
    let baseline = ready(
        image_scene(),
        2,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    let peak = baseline.stats().peak_working_bytes();
    let limits = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
        max_peak_working_bytes: peak - 1,
        ..ReferenceRasterLimitConfig::default()
    })
    .unwrap();
    let scene = image_scene();
    let released = Arc::downgrade(&scene);
    let cancellation = Cancellation::never();
    let mut job = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(2, 1).unwrap(),
        limits,
    );
    let failure = match job.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("post-paint output peak must fail atomically: {outcome:?}"),
    };
    assert_eq!(
        failure.limit().unwrap().kind(),
        ReferenceRenderLimitKind::PeakWorkingBytes
    );
    assert_eq!(job.stats().image_commands(), 1);
    assert_eq!(job.stats().final_conversion_pixels(), 0);
    assert!(released.upgrade().is_none());

    let calls = cancellation.calls();
    assert_eq!(
        job.poll(&cancellation),
        ReferenceRenderPoll::Failed(failure)
    );
    assert_eq!(cancellation.calls(), calls);
}

#[test]
fn cancellation_at_the_final_publication_probe_is_atomic_and_terminal() {
    let measurement = Cancellation::never();
    let baseline = ready(
        clipped_fill_scene(),
        2,
        1,
        ReferenceRasterLimits::default(),
        &measurement,
    );
    assert_eq!(baseline.stats().cancellation_checks(), measurement.calls());
    let cancellation = Cancellation::at(measurement.calls());
    let scene = clipped_fill_scene();
    let released = Arc::downgrade(&scene);
    let mut job = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(2, 1).unwrap(),
        ReferenceRasterLimits::default(),
    );
    match job.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => {
            assert_eq!(error.code(), ReferenceRenderErrorCode::Cancelled)
        }
        outcome => panic!("final publication cancellation must fail atomically: {outcome:?}"),
    }
    assert_eq!(job.phase(), ReferenceRenderPhase::Failed);
    assert_eq!(job.stats().final_conversion_pixels(), 2);
    assert!(released.upgrade().is_none());

    let calls = cancellation.calls();
    let terminal = job.poll(&cancellation);
    assert!(matches!(
        terminal,
        ReferenceRenderPoll::Failed(error) if error.code() == ReferenceRenderErrorCode::Cancelled
    ));
    assert_eq!(cancellation.calls(), calls);
}
