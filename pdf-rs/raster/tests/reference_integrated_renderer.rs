use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_raster::reference::{
    CanonicalPixelBuffer, ReferenceRasterCancellation, ReferenceRasterLimitConfig,
    ReferenceRasterLimits, ReferenceRenderConfig, ReferenceRenderErrorCode, ReferenceRenderJob,
    ReferenceRenderLimitKind, ReferenceRenderPhase, ReferenceRenderPoll, ReferenceRenderStats,
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

fn double_image_scene() -> Arc<Scene> {
    let image = ImageResource::new(
        resource_source(26),
        1,
        1,
        ImageColorSpace::DeviceRgb,
        8,
        false,
        vec![255, 0, 0],
    )
    .unwrap();
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    for index in 0..2 {
        builder
            .draw_image(
                image.clone(),
                Matrix::IDENTITY,
                SceneUnit::ONE,
                BlendMode::Normal,
                SceneBounds::Page,
                source(index),
            )
            .unwrap();
    }
    Arc::new(builder.finish().unwrap())
}

fn image_then_singular_image_scene() -> Arc<Scene> {
    let image = ImageResource::new(
        resource_source(28),
        1,
        1,
        ImageColorSpace::DeviceRgb,
        8,
        false,
        vec![255, 0, 0],
    )
    .unwrap();
    let singular = Matrix::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::ONE,
        SceneScalar::ZERO,
        SceneScalar::ZERO,
    ]);
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    for (index, transform) in [(0, Matrix::IDENTITY), (1, singular)] {
        builder
            .draw_image(
                image.clone(),
                transform,
                SceneUnit::ONE,
                BlendMode::Normal,
                SceneBounds::Page,
                source(index),
            )
            .unwrap();
    }
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
            SceneUnit::from_u16(32_768),
            BlendMode::Normal,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    builder
        .append_fill(
            rectangle("0", "0", "1", "1"),
            FillRule::Nonzero,
            red(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(1),
        )
        .unwrap();
    builder.end_group(SceneBounds::Page, source(2)).unwrap();
    Arc::new(builder.finish().unwrap())
}

fn identity_group_scene() -> Arc<Scene> {
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
    builder
        .append_fill(
            rectangle("0", "0", "1", "1"),
            FillRule::Nonzero,
            red(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(1),
        )
        .unwrap();
    builder.end_group(SceneBounds::Page, source(2)).unwrap();
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

fn double_glyph_scene() -> Arc<Scene> {
    let outline =
        GlyphOutline::new(resource_source(27), 10, 1, rectangle("0", "0", "1", "1")).unwrap();
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    for index in 0..2 {
        builder
            .draw_glyph_run(
                vec![GlyphUse::new(outline.clone(), Matrix::IDENTITY, 65)],
                black(),
                SceneBounds::Page,
                source(index),
            )
            .unwrap();
    }
    Arc::new(builder.finish().unwrap())
}

fn glyph_then_empty_glyph_scene() -> Arc<Scene> {
    let visible =
        GlyphOutline::new(resource_source(29), 11, 1, rectangle("0", "0", "1", "1")).unwrap();
    let empty = GlyphOutline::new(
        resource_source(30),
        12,
        1,
        PathResource::new(Vec::new()).unwrap(),
    )
    .unwrap();
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    for (index, outline) in [(0, visible), (1, empty)] {
        builder
            .draw_glyph_run(
                vec![GlyphUse::new(outline, Matrix::IDENTITY, 65)],
                black(),
                SceneBounds::Page,
                source(index),
            )
            .unwrap();
    }
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

fn empty_scene() -> Arc<Scene> {
    Arc::new(
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default())
            .finish()
            .unwrap(),
    )
}

fn fill_only_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder
        .append_fill(
            rectangle("0", "0", "1", "1"),
            FillRule::Nonzero,
            black(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn double_fill_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    for index in 0..2 {
        builder
            .append_fill(
                rectangle("0", "0", "1", "1"),
                FillRule::Nonzero,
                black(),
                Matrix::IDENTITY,
                SceneBounds::Page,
                source(index),
            )
            .unwrap();
    }
    Arc::new(builder.finish().unwrap())
}

fn fill_then_empty_fill_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    for (index, path) in [
        (0, rectangle("0", "0", "1", "1")),
        (1, PathResource::new(Vec::new()).unwrap()),
    ] {
        builder
            .append_fill(
                path,
                FillRule::Nonzero,
                black(),
                Matrix::IDENTITY,
                SceneBounds::Page,
                source(index),
            )
            .unwrap();
    }
    Arc::new(builder.finish().unwrap())
}

fn clip_only_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder
        .append_clip(
            rectangle("0", "0", "0.5", "1"),
            FillRule::Nonzero,
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn double_clip_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    for (index, maximum_x) in [(0, "0.75"), (1, "0.5")] {
        builder
            .append_clip(
                rectangle("0", "0", maximum_x, "1"),
                FillRule::Nonzero,
                Matrix::IDENTITY,
                SceneBounds::Page,
                source(index),
            )
            .unwrap();
    }
    Arc::new(builder.finish().unwrap())
}

fn repeated_glyph_scene() -> Arc<Scene> {
    let outline =
        GlyphOutline::new(resource_source(24), 9, 1, rectangle("0", "0", "1", "1")).unwrap();
    let glyphs = (0..300)
        .map(|_| GlyphUse::new(outline.clone(), Matrix::IDENTITY, 65))
        .collect();
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    builder
        .draw_glyph_run(glyphs, black(), SceneBounds::Page, source(0))
        .unwrap();
    Arc::new(builder.finish().unwrap())
}

fn dense_dependency_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    let mut previous = Vec::new();
    for _ in 0..25 {
        let id = builder
            .add_requirement(
                GraphicsCapability::PathFill,
                0,
                CapabilityContext::Scene,
                previous.clone(),
                CapabilityStatus::Supported,
            )
            .unwrap();
        previous.push(id);
    }
    Arc::new(builder.finish().unwrap())
}

fn huge_outer_command_scene() -> Arc<Scene> {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), unit_geometry(), GraphicsSceneLimits::default());
    for index in 0..150 {
        builder
            .append_save(SceneBounds::Page, source(index))
            .unwrap();
    }
    for index in 150..300 {
        builder
            .append_restore(SceneBounds::Page, source(index))
            .unwrap();
    }
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

fn failed_limit_stats(
    scene: Arc<Scene>,
    width: u32,
    height: u32,
    config: ReferenceRasterLimitConfig,
    expected_kind: ReferenceRenderLimitKind,
) -> (ReferenceRenderStats, u64, u64) {
    let cancellation = Cancellation::never();
    let released = Arc::downgrade(&scene);
    let mut job = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(width, height).unwrap(),
        ReferenceRasterLimits::validate(config).unwrap(),
    );
    let failure = match job.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("{expected_kind:?} exhaustion must fail: {outcome:?}"),
    };
    let limit = failure
        .limit()
        .expect("aggregate exhaustion must retain context");
    assert_eq!(limit.kind(), expected_kind);
    let stats = job.stats();
    assert_eq!(stats.cancellation_checks(), cancellation.calls());
    assert_eq!(stats.coverage_bytes(), 0);
    assert_eq!(stats.retained_bytes(), 0);
    assert!(released.upgrade().is_none());

    let frozen_calls = cancellation.calls();
    assert_eq!(
        job.poll(&cancellation),
        ReferenceRenderPoll::Failed(failure)
    );
    assert_eq!(job.stats(), stats);
    assert_eq!(cancellation.calls(), frozen_calls);
    (stats, limit.consumed(), limit.attempted())
}

fn find_mid_child_cancellation(
    scene: SceneFactory,
    width: u32,
    height: u32,
    predicate: impl Fn(ReferenceRenderStats, ReferenceRenderStats) -> bool,
) -> ReferenceRenderStats {
    let measurement = Cancellation::never();
    let baseline = ready(
        scene(),
        width,
        height,
        ReferenceRasterLimits::default(),
        &measurement,
    );
    let baseline_stats = baseline.stats();

    for cancel_at in 2..=measurement.calls() {
        let cancellation = Cancellation::at(cancel_at);
        let scene = scene();
        let released = Arc::downgrade(&scene);
        let mut job = ReferenceRenderJob::new(
            scene,
            ReferenceRenderConfig::opaque_srgb(width, height).unwrap(),
            ReferenceRasterLimits::default(),
        );
        let failure = match job.poll(&cancellation) {
            ReferenceRenderPoll::Failed(error)
                if error.code() == ReferenceRenderErrorCode::Cancelled =>
            {
                error
            }
            _ => continue,
        };
        assert_eq!(job.stats().cancellation_checks(), cancellation.calls());
        assert!(released.upgrade().is_none());
        if !predicate(job.stats(), baseline_stats) {
            continue;
        }

        let frozen_stats = job.stats();
        let frozen_calls = cancellation.calls();
        assert_eq!(
            job.poll(&cancellation),
            ReferenceRenderPoll::Failed(failure)
        );
        assert_eq!(job.stats(), frozen_stats);
        assert_eq!(cancellation.calls(), frozen_calls);
        return frozen_stats;
    }
    panic!("no deterministic child cancellation point matched the requested phase")
}

fn assert_failed_component_peaks(stats: ReferenceRenderStats, label: &str) {
    for (component, bytes) in [
        ("coverage", stats.peak_coverage_bytes()),
        ("geometry", stats.peak_geometry_bytes()),
        ("clip", stats.peak_clip_bytes()),
    ] {
        let lower_bound = stats.surface_bytes().checked_add(bytes).unwrap();
        assert!(
            stats.peak_working_bytes() >= lower_bound,
            "{label} must retain its {component} child peak in aggregate working stats"
        );
    }
    assert_eq!(stats.coverage_bytes(), 0, "{label}");
    assert_eq!(stats.retained_bytes(), 0, "{label}");
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
    assert_eq!(first.stats().coverage_bytes(), 0);
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
fn isolated_group_composites_offscreen_while_interpolated_image_stays_unsupported() {
    let identity = ready(
        identity_group_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert_eq!(identity.rgba(), &[255, 0, 0, 255]);

    let group = ready(
        group_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert_eq!(group.rgba(), &[255, 127, 127, 255]);
    assert!(
        group.stats().peak_working_bytes() > group.stats().surface_bytes(),
        "offscreen group storage must contribute to peak working bytes"
    );
    assert!(
        group.stats().peak_working_bytes() > identity.stats().peak_working_bytes(),
        "identity groups must avoid a full-size offscreen surface"
    );

    let scene = interpolated_image_scene();
    let released = Arc::downgrade(&scene);
    let cancellation = Cancellation::never();
    let mut job = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
        ReferenceRasterLimits::default(),
    );
    let unsupported = match job.poll(&cancellation) {
        ReferenceRenderPoll::Unsupported(value) => value,
        outcome => panic!("interpolated image must be structured unsupported: {outcome:?}"),
    };
    assert_eq!(
        unsupported.kind(),
        ReferenceRenderUnsupportedKind::VisibleGraphicsRequirement
    );
    assert_eq!(unsupported.capability(), Some(GraphicsCapability::Image));
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
fn nested_preflight_admits_aggregate_lengths_before_bounded_traversal() {
    let dense = ready(
        dense_dependency_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert_eq!(dense.stats().dependencies(), 300);
    ready(
        dense_dependency_scene(),
        1,
        1,
        ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_dependencies: 300,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap(),
        &Cancellation::never(),
    );

    let scene = dense_dependency_scene();
    let released = Arc::downgrade(&scene);
    let cancellation = Cancellation::never();
    let mut job = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
        ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_dependencies: 299,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap(),
    );
    let failure = match job.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("one-less nested dependency admission must fail: {outcome:?}"),
    };
    assert_eq!(
        failure.limit().unwrap().kind(),
        ReferenceRenderLimitKind::Dependencies
    );
    assert_eq!(job.stats().dependencies(), 0);
    assert_eq!(job.stats().surface_bytes(), 0);
    assert_eq!(job.stats().cancellation_checks(), cancellation.calls());
    assert!(released.upgrade().is_none());
    let frozen_calls = cancellation.calls();
    assert_eq!(
        job.poll(&cancellation),
        ReferenceRenderPoll::Failed(failure)
    );
    assert_eq!(cancellation.calls(), frozen_calls);

    let repeated = ready(
        repeated_glyph_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert_eq!(repeated.stats().glyphs(), 300);
    assert_eq!(repeated.stats().glyph_resource_lookups(), 300);
    for (kind, limits) in [
        (
            ReferenceRenderLimitKind::Glyphs,
            ReferenceRasterLimitConfig {
                max_glyphs: 299,
                ..ReferenceRasterLimitConfig::default()
            },
        ),
        (
            ReferenceRenderLimitKind::GlyphResourceLookups,
            ReferenceRasterLimitConfig {
                max_glyph_resource_lookups: 299,
                ..ReferenceRasterLimitConfig::default()
            },
        ),
    ] {
        let cancellation = Cancellation::never();
        let mut job = ReferenceRenderJob::new(
            repeated_glyph_scene(),
            ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
            ReferenceRasterLimits::validate(limits).unwrap(),
        );
        match job.poll(&cancellation) {
            ReferenceRenderPoll::Failed(error) => {
                assert_eq!(error.limit().unwrap().kind(), kind);
                assert_eq!(job.stats().glyphs(), 0);
                assert_eq!(job.stats().glyph_resource_lookups(), 0);
                assert_eq!(job.stats().surface_bytes(), 0);
                assert_eq!(job.stats().cancellation_checks(), cancellation.calls());
            }
            outcome => panic!("one-less repeated-glyph admission must fail: {outcome:?}"),
        }
    }
}

#[test]
fn exhausted_aggregate_child_limits_reject_without_partial_merge_or_surface_mutation() {
    let (image_source, consumed, attempted) = failed_limit_stats(
        double_image_scene(),
        1,
        1,
        ReferenceRasterLimitConfig {
            max_image_source_pixels: 1,
            ..ReferenceRasterLimitConfig::default()
        },
        ReferenceRenderLimitKind::ImageSourcePixels,
    );
    assert_eq!((consumed, attempted), (1, 1));
    assert_eq!(image_source.image_commands(), 2);
    assert_eq!(image_source.image_source_pixels(), 1);
    assert_eq!(image_source.image_decoded_bytes(), 3);
    assert_eq!(image_source.image_samples(), 64);
    assert_eq!(image_source.image_conversions(), 64);

    let (image_samples, consumed, attempted) = failed_limit_stats(
        double_image_scene(),
        1,
        1,
        ReferenceRasterLimitConfig {
            max_image_samples: 64,
            ..ReferenceRasterLimitConfig::default()
        },
        ReferenceRenderLimitKind::ImageSamples,
    );
    assert_eq!((consumed, attempted), (64, 64));
    assert_eq!(image_samples.image_commands(), 2);
    assert_eq!(image_samples.image_source_pixels(), 2);
    assert_eq!(image_samples.image_decoded_bytes(), 6);
    assert_eq!(image_samples.image_samples(), 64);
    assert_eq!(image_samples.image_conversions(), 64);
    assert_eq!(image_samples.fuel(), image_source.fuel());
    assert_eq!(
        image_samples.cancellation_checks(),
        image_source.cancellation_checks()
    );
    assert_eq!(
        image_samples.peak_coverage_bytes(),
        image_source.peak_coverage_bytes()
    );

    let first_fill = ready(
        fill_only_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    )
    .stats();
    let (geometry_segments, consumed, attempted) = failed_limit_stats(
        double_fill_scene(),
        1,
        1,
        ReferenceRasterLimitConfig {
            max_geometry_segments: first_fill.geometry_segments(),
            ..ReferenceRasterLimitConfig::default()
        },
        ReferenceRenderLimitKind::GeometrySegments,
    );
    assert_eq!((consumed, attempted), (first_fill.geometry_segments(), 1));
    assert_eq!(
        geometry_segments.geometry_segments(),
        first_fill.geometry_segments()
    );
    assert_eq!(
        geometry_segments.geometry_edges(),
        first_fill.geometry_edges()
    );
    assert_eq!(
        geometry_segments.geometry_samples(),
        first_fill.geometry_samples()
    );

    let (geometry_samples, consumed, attempted) = failed_limit_stats(
        double_fill_scene(),
        1,
        1,
        ReferenceRasterLimitConfig {
            max_geometry_samples: first_fill.geometry_samples(),
            ..ReferenceRasterLimitConfig::default()
        },
        ReferenceRenderLimitKind::GeometrySamples,
    );
    assert_eq!(
        (consumed, attempted),
        (first_fill.geometry_samples(), first_fill.geometry_samples())
    );
    assert_eq!(
        geometry_samples.geometry_segments(),
        first_fill.geometry_segments() * 2
    );
    assert_eq!(
        geometry_samples.geometry_edges(),
        first_fill.geometry_edges() * 2
    );
    assert_eq!(
        geometry_samples.geometry_samples(),
        first_fill.geometry_samples()
    );
    assert_eq!(
        geometry_samples.peak_coverage_bytes(),
        first_fill.peak_coverage_bytes()
    );
    assert!(geometry_samples.fuel() > geometry_segments.fuel());

    let (glyph_outline, consumed, attempted) = failed_limit_stats(
        double_glyph_scene(),
        1,
        1,
        ReferenceRasterLimitConfig {
            max_glyph_outline_segments: 5,
            ..ReferenceRasterLimitConfig::default()
        },
        ReferenceRenderLimitKind::GlyphOutlineSegments,
    );
    assert_eq!((consumed, attempted), (5, 5));
    assert_eq!(glyph_outline.glyph_runs(), 2);
    assert_eq!(glyph_outline.glyphs(), 1);
    assert_eq!(glyph_outline.glyph_resource_lookups(), 1);
    assert_eq!(glyph_outline.glyph_outline_segments(), 5);
    assert_eq!(glyph_outline.glyph_samples(), 64);
    assert_eq!(glyph_outline.glyph_composites(), 64);
    assert!(glyph_outline.fuel() > 0);
    assert!(glyph_outline.peak_coverage_bytes() > 0);
}

#[test]
fn exact_zero_remaining_dimensions_still_admit_zero_work_children() {
    let first_fill = ready(
        fill_only_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    )
    .stats();
    let fill = ready(
        fill_then_empty_fill_scene(),
        1,
        1,
        ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_geometry_segments: first_fill.geometry_segments(),
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap(),
        &Cancellation::never(),
    );
    assert_eq!(
        fill.stats().geometry_segments(),
        first_fill.geometry_segments()
    );

    let image = ready(
        image_then_singular_image_scene(),
        1,
        1,
        ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_image_samples: 64,
            max_image_conversions: 64,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap(),
        &Cancellation::never(),
    );
    assert_eq!(image.stats().image_commands(), 2);
    assert_eq!(image.stats().image_samples(), 64);
    assert_eq!(image.stats().image_conversions(), 64);

    let glyph = ready(
        glyph_then_empty_glyph_scene(),
        1,
        1,
        ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_glyph_outline_segments: 5,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap(),
        &Cancellation::never(),
    );
    assert_eq!(glyph.stats().glyph_runs(), 2);
    assert_eq!(glyph.stats().glyph_outline_segments(), 5);
}

#[test]
fn large_outer_and_nested_preflight_scans_are_fuelled_cancellable_and_terminal() {
    for (scene, label) in [
        (dense_dependency_scene as SceneFactory, "dependency edges"),
        (repeated_glyph_scene as SceneFactory, "glyph lookups"),
        (huge_outer_command_scene as SceneFactory, "outer commands"),
    ] {
        let cancellation = Cancellation::at(2);
        let scene = scene();
        let released = Arc::downgrade(&scene);
        let mut job = ReferenceRenderJob::new(
            scene,
            ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
            ReferenceRasterLimits::default(),
        );
        let failure = match job.poll(&cancellation) {
            ReferenceRenderPoll::Failed(error)
                if error.code() == ReferenceRenderErrorCode::Cancelled =>
            {
                error
            }
            outcome => panic!("{label} scan must cancel at its fixed fuel boundary: {outcome:?}"),
        };
        assert_eq!(cancellation.calls(), 2, "{label}");
        assert_eq!(job.stats().cancellation_checks(), cancellation.calls());
        assert_eq!(job.stats().fuel(), 256, "{label}");
        assert_eq!(job.stats().surface_bytes(), 0, "{label}");
        assert!(released.upgrade().is_none(), "{label}");

        let frozen_stats = job.stats();
        let frozen_calls = cancellation.calls();
        assert_eq!(
            job.poll(&cancellation),
            ReferenceRenderPoll::Failed(failure)
        );
        assert_eq!(job.stats(), frozen_stats);
        assert_eq!(cancellation.calls(), frozen_calls);
    }
}

#[test]
fn white_surface_initialization_has_exact_fuel_midpoint_cancellation_and_replay() {
    let measurement = Cancellation::never();
    let baseline = ready(
        empty_scene(),
        16,
        16,
        ReferenceRasterLimits::default(),
        &measurement,
    );
    assert_eq!(baseline.stats().fuel(), 512);
    assert_eq!(baseline.stats().final_conversion_pixels(), 256);
    assert_eq!(baseline.stats().cancellation_checks(), measurement.calls());

    let exact = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
        max_fuel: 512,
        ..ReferenceRasterLimitConfig::default()
    })
    .unwrap();
    assert_eq!(
        ready(empty_scene(), 16, 16, exact, &Cancellation::never()).stats(),
        baseline.stats()
    );

    let scene = empty_scene();
    let released = Arc::downgrade(&scene);
    let cancellation = Cancellation::never();
    let mut one_less = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(16, 16).unwrap(),
        ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_fuel: 511,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap(),
    );
    let failure = match one_less.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("one-less initialization-plus-conversion fuel must fail: {outcome:?}"),
    };
    assert_eq!(
        failure.limit().unwrap().kind(),
        ReferenceRenderLimitKind::Fuel
    );
    assert_eq!(cancellation.calls(), 0);
    assert_eq!(one_less.stats().surface_bytes(), 0);
    assert!(released.upgrade().is_none());

    let scene = empty_scene();
    let released = Arc::downgrade(&scene);
    let cancellation = Cancellation::at(3);
    let mut mid_init = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(16, 16).unwrap(),
        ReferenceRasterLimits::default(),
    );
    let failure = match mid_init.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error)
            if error.code() == ReferenceRenderErrorCode::Cancelled =>
        {
            error
        }
        outcome => panic!("surface initialization must cancel in its first chunk: {outcome:?}"),
    };
    assert_eq!(mid_init.stats().fuel(), 256);
    assert_eq!(mid_init.stats().final_conversion_pixels(), 0);
    assert!(mid_init.stats().surface_bytes() > 0);
    assert_eq!(mid_init.stats().retained_bytes(), 0);
    assert_eq!(mid_init.stats().cancellation_checks(), cancellation.calls());
    assert!(released.upgrade().is_none());
    let frozen_stats = mid_init.stats();
    let frozen_calls = cancellation.calls();
    assert_eq!(
        mid_init.poll(&cancellation),
        ReferenceRenderPoll::Failed(failure)
    );
    assert_eq!(mid_init.stats(), frozen_stats);
    assert_eq!(cancellation.calls(), frozen_calls);

    let scene = empty_scene();
    let released = Arc::downgrade(&scene);
    let cancellation = Cancellation::at(4);
    let mut mid_conversion = ReferenceRenderJob::new(
        scene,
        ReferenceRenderConfig::opaque_srgb(16, 16).unwrap(),
        ReferenceRasterLimits::default(),
    );
    let failure = match mid_conversion.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error)
            if error.code() == ReferenceRenderErrorCode::Cancelled =>
        {
            error
        }
        outcome => panic!("final conversion must cancel before its 256th pixel: {outcome:?}"),
    };
    assert_eq!(mid_conversion.stats().fuel(), 512);
    assert_eq!(mid_conversion.stats().final_conversion_pixels(), 255);
    assert!(mid_conversion.stats().surface_bytes() > 0);
    assert_eq!(mid_conversion.stats().retained_bytes(), 0);
    assert_eq!(
        mid_conversion.stats().cancellation_checks(),
        cancellation.calls()
    );
    assert!(released.upgrade().is_none());
    let frozen_stats = mid_conversion.stats();
    let frozen_calls = cancellation.calls();
    assert_eq!(
        mid_conversion.poll(&cancellation),
        ReferenceRenderPoll::Failed(failure)
    );
    assert_eq!(mid_conversion.stats(), frozen_stats);
    assert_eq!(cancellation.calls(), frozen_calls);
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
fn every_mounted_child_merges_mid_cancellation_progress_and_freezes_replay() {
    let fill = find_mid_child_cancellation(fill_only_scene, 8, 8, |stats, baseline| {
        stats.geometry_segments() > 0
            && stats.geometry_samples() > 0
            && stats.geometry_samples() < baseline.geometry_samples()
            && stats.final_conversion_pixels() == 0
    });
    assert_failed_component_peaks(fill, "fill cancellation");

    let clip = find_mid_child_cancellation(clip_only_scene, 8, 8, |stats, baseline| {
        stats.geometry_segments() > 0
            && stats.geometry_samples() > 0
            && stats.geometry_samples() < baseline.geometry_samples()
            && stats.clip_bytes() == 0
    });
    assert_failed_component_peaks(clip, "clip cancellation");

    let clip_replacement =
        find_mid_child_cancellation(double_clip_scene, 16, 16, |stats, _baseline| {
            stats.clip_bytes() > 0 && stats.peak_clip_bytes() > stats.clip_bytes()
        });
    assert_failed_component_peaks(clip_replacement, "clip replacement cancellation");

    let image = find_mid_child_cancellation(image_scene, 8, 8, |stats, baseline| {
        stats.image_commands() == 1
            && stats.image_samples() > 0
            && stats.image_samples() < baseline.image_samples()
    });
    assert!(image.image_source_pixels() > 0);
    assert!(image.image_decoded_bytes() > 0);
    assert_eq!(image.coverage_bytes(), 0);

    let glyph = find_mid_child_cancellation(glyph_scene, 8, 8, |stats, baseline| {
        stats.glyph_runs() == 1
            && stats.glyph_resource_lookups() > 0
            && stats.geometry_samples() > 0
            && stats.geometry_samples() < baseline.geometry_samples()
    });
    assert!(glyph.glyph_outline_segments() > 0);
    assert_failed_component_peaks(glyph, "glyph cancellation");
}

#[test]
fn mounted_child_one_less_failures_retain_consumed_stats_and_exact_error_context() {
    for (scene, label) in [
        (fill_only_scene as SceneFactory, "fill"),
        (clip_only_scene as SceneFactory, "clip"),
    ] {
        let baseline = ready(
            scene(),
            8,
            8,
            ReferenceRasterLimits::default(),
            &Cancellation::never(),
        );
        let limit = baseline.stats().geometry_samples() - 1;
        let cancellation = Cancellation::never();
        let mut job = ReferenceRenderJob::new(
            scene(),
            ReferenceRenderConfig::opaque_srgb(8, 8).unwrap(),
            ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
                max_geometry_samples: limit,
                ..ReferenceRasterLimitConfig::default()
            })
            .unwrap(),
        );
        let failure = match job.poll(&cancellation) {
            ReferenceRenderPoll::Failed(error) => error,
            outcome => panic!("{label} one-less child budget must fail: {outcome:?}"),
        };
        let context = failure.limit().unwrap();
        assert_eq!(context.kind(), ReferenceRenderLimitKind::GeometrySamples);
        assert_eq!(context.consumed(), job.stats().geometry_samples());
        assert!(job.stats().geometry_segments() > 0, "{label}");
        assert!(job.stats().geometry_edges() > 0, "{label}");
        assert_eq!(job.stats().coverage_bytes(), 0, "{label}");
        assert_failed_component_peaks(job.stats(), label);
        assert_eq!(job.stats().cancellation_checks(), cancellation.calls());
        let frozen_stats = job.stats();
        let frozen_calls = cancellation.calls();
        assert_eq!(
            job.poll(&cancellation),
            ReferenceRenderPoll::Failed(failure)
        );
        assert_eq!(job.stats(), frozen_stats);
        assert_eq!(cancellation.calls(), frozen_calls);
    }

    let image_baseline = ready(
        image_scene(),
        2,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    let cancellation = Cancellation::never();
    let mut image_job = ReferenceRenderJob::new(
        image_scene(),
        ReferenceRenderConfig::opaque_srgb(2, 1).unwrap(),
        ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_image_samples: image_baseline.stats().image_samples() - 1,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap(),
    );
    let image_failure = match image_job.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("image one-less child budget must fail: {outcome:?}"),
    };
    assert_eq!(image_job.stats().image_commands(), 1);
    assert_eq!(image_job.stats().image_source_pixels(), 2);
    assert_eq!(image_job.stats().image_decoded_bytes(), 6);
    assert_eq!(image_job.stats().image_samples(), 0);
    assert_eq!(
        image_failure.limit().unwrap().consumed(),
        image_job.stats().image_samples()
    );
    assert_eq!(
        image_job.stats().cancellation_checks(),
        cancellation.calls()
    );

    let glyph_baseline = ready(
        glyph_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    let cancellation = Cancellation::never();
    let mut glyph_job = ReferenceRenderJob::new(
        glyph_scene(),
        ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
        ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_fuel: glyph_baseline.stats().fuel() - 1,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap(),
    );
    let glyph_failure = match glyph_job.poll(&cancellation) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("glyph one-less aggregate fuel must fail: {outcome:?}"),
    };
    let context = glyph_failure.limit().unwrap();
    assert_eq!(context.kind(), ReferenceRenderLimitKind::Fuel);
    assert_eq!(context.consumed(), glyph_job.stats().fuel() + 1);
    assert!(context.consumed() + context.attempted() > context.limit());
    assert!(glyph_job.stats().glyph_resource_lookups() > 0);
    assert!(glyph_job.stats().geometry_samples() > 0);
    assert_failed_component_peaks(glyph_job.stats(), "glyph one-less fuel");
    assert_eq!(
        glyph_job.stats().cancellation_checks(),
        cancellation.calls()
    );
}

#[test]
fn transient_coverage_is_zero_after_clip_image_and_glyph_completion() {
    let clip = ready(
        clip_only_scene(),
        2,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert_eq!(clip.stats().coverage_bytes(), 0);
    assert!(clip.stats().peak_coverage_bytes() > 0);
    assert!(clip.stats().clip_bytes() > 0);
    assert!(clip.stats().peak_clip_bytes() >= clip.stats().clip_bytes());

    let image = ready(
        image_scene(),
        2,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert_eq!(image.stats().coverage_bytes(), 0);

    let glyph = ready(
        glyph_scene(),
        1,
        1,
        ReferenceRasterLimits::default(),
        &Cancellation::never(),
    );
    assert_eq!(glyph.stats().coverage_bytes(), 0);
    assert!(glyph.stats().peak_coverage_bytes() > 0);
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
