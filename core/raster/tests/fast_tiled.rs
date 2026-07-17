use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_policy::{
    AntialiasMode, CapabilityEvaluator, CapabilityProfile, DeviceRect, OptionalContentIdentity,
    PolicyCancellation, PolicyLimits, RenderConfig, RenderConfigInput, RenderPlan,
    RenderPlanOutcome, RenderPlanRequest, RendererEpoch, ZoomRatio, create_render_plan,
};
use pdf_rs_raster::fast::{
    FastRasterCancellation, FastRasterErrorCode, FastRasterJob, FastRasterLimitConfig,
    FastRasterLimitKind, FastRasterLimits, NeverCancelled,
};
use pdf_rs_raster::reference::{
    ReferenceRasterCancellation, ReferenceRasterLimits, ReferenceRenderConfig, ReferenceRenderJob,
    ReferenceRenderPoll,
};
use pdf_rs_scene::{
    BlendMode, CommandSource, DashPattern, DeviceColor, FillRule, GlyphOutline, GlyphUse,
    GraphicsResourceSource, GraphicsSceneBuilder, GraphicsSceneLimits, ImageColorSpace,
    ImageResource, LineCap, LineJoin, LineStyle, Matrix, PageGeometry, PageRotation, Paint,
    PathResource, PathSegment, Scene, SceneBinding, SceneBounds, ScenePoint, SceneRect,
    SceneScalar, SceneUnit,
};
use pdf_rs_syntax::ObjectRef;

const PAGE_WIDTH: u32 = 16;
const PAGE_HEIGHT: u32 = 16;

#[test]
fn bounds_bins_preserve_source_order_and_skip_disjoint_tiles() {
    let mut scene_builder = builder();
    append_fill(&mut scene_builder, rectangle(0, 0, 6, 16), red(), 0);
    append_fill(&mut scene_builder, rectangle(10, 0, 16, 16), blue(), 1);
    let scene = scene_builder.finish().unwrap();
    let render_plan = plan(&scene, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let job = FastRasterJob::new(
        &scene,
        &render_plan,
        FastRasterLimits::default(),
        &NeverCancelled,
    )
    .unwrap();

    assert_eq!(job.bins().bins()[0], [0]);
    assert_eq!(job.bins().bins()[1], [1]);
    assert_eq!(job.bins().bins()[2], [0]);
    assert_eq!(job.bins().bins()[3], [1]);
    assert_eq!(job.bins().entries(), 4);

    let mut overpaint = builder();
    append_fill(&mut overpaint, rectangle(0, 0, 16, 16), red(), 0);
    append_fill(&mut overpaint, rectangle(0, 0, 16, 16), blue(), 1);
    let overpaint = overpaint.finish().unwrap();
    let overpaint_plan = plan(&overpaint, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let overpaint_job = FastRasterJob::new(
        &overpaint,
        &overpaint_plan,
        FastRasterLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    for bin in overpaint_job.bins().bins() {
        assert_eq!(bin, &[0, 1]);
    }
}

#[test]
fn whole_page_tiles_and_tile_order_are_metamorphic() {
    let scene = layered_scene();
    let whole_plan = plan(
        &scene,
        config(PAGE_WIDTH, PAGE_HEIGHT, 1),
        PAGE_WIDTH,
        PAGE_HEIGHT,
    );
    let whole_job = FastRasterJob::new(
        &scene,
        &whole_plan,
        FastRasterLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    let whole = whole_job.render_all(&[0], &NeverCancelled).unwrap();

    let tiled_plan = plan(&scene, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let tiled_job = FastRasterJob::new(
        &scene,
        &tiled_plan,
        FastRasterLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    let row_major = tiled_job
        .render_all(&[0, 1, 2, 3], &NeverCancelled)
        .unwrap();
    let reverse = tiled_job
        .render_all(&[3, 2, 1, 0], &NeverCancelled)
        .unwrap();

    assert_eq!(compose(&whole), compose(&row_major));
    assert_eq!(compose(&row_major), compose(&reverse));
    assert_eq!(row_major.stats(), reverse.stats());
    assert_eq!(row_major.plan_hash(), tiled_plan.hash());
}

#[test]
fn exact_rectangle_pixels_match_independently_enumerated_expectation() {
    let mut builder = builder();
    append_fill(&mut builder, rectangle(0, 8, 8, 16), red(), 0);
    let scene = builder.finish().unwrap();
    let plan = plan(&scene, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let job =
        FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled).unwrap();
    let pixels = compose(&job.render_all(&[0, 1, 2, 3], &NeverCancelled).unwrap());

    let mut expected = vec![255_u8; usize::try_from(PAGE_WIDTH * PAGE_HEIGHT * 4).unwrap()];
    for y in 0..8_usize {
        for x in 0..8_usize {
            let index = (y * usize::try_from(PAGE_WIDTH).unwrap() + x) * 4;
            expected[index..index + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    assert_eq!(pixels, expected);
}

#[test]
fn clip_masks_and_nearest_image_sampling_use_independent_scalar_kernels() {
    let mut clipped = builder();
    clipped.append_save(SceneBounds::Page, source(0)).unwrap();
    let clip_path = rectangle(0, 0, 8, 16);
    clipped
        .append_clip(
            clip_path.clone(),
            FillRule::Nonzero,
            Matrix::IDENTITY,
            bounds(0, 0, 8, 16),
            source(1),
        )
        .unwrap();
    append_fill_with_source(&mut clipped, rectangle(0, 0, 16, 16), red(), 2);
    clipped
        .append_restore(SceneBounds::Page, source(3))
        .unwrap();
    let clipped = clipped.finish().unwrap();
    let clipped_plan = plan(&clipped, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let clipped_pixels = compose(
        &FastRasterJob::new(
            &clipped,
            &clipped_plan,
            FastRasterLimits::default(),
            &NeverCancelled,
        )
        .unwrap()
        .render_all(&[0, 1, 2, 3], &NeverCancelled)
        .unwrap(),
    );
    assert_eq!(pixel(&clipped_pixels, 2, 2), [255, 0, 0, 255]);
    assert_eq!(pixel(&clipped_pixels, 12, 2), [255, 255, 255, 255]);

    let mut image_builder = builder();
    let image = ImageResource::new(
        GraphicsResourceSource::new(ObjectRef::new(60, 0).unwrap(), 19, 0),
        2,
        1,
        ImageColorSpace::DeviceRgb,
        8,
        false,
        vec![255, 0, 0, 0, 255, 0],
    )
    .unwrap();
    image_builder
        .draw_image(
            image,
            Matrix::new([
                scalar(16),
                SceneScalar::ZERO,
                SceneScalar::ZERO,
                scalar(16),
                SceneScalar::ZERO,
                SceneScalar::ZERO,
            ]),
            SceneUnit::ONE,
            BlendMode::Normal,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    let image_scene = image_builder.finish().unwrap();
    let image_plan = plan(&image_scene, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let image_pixels = compose(
        &FastRasterJob::new(
            &image_scene,
            &image_plan,
            FastRasterLimits::default(),
            &NeverCancelled,
        )
        .unwrap()
        .render_all(&[0, 1, 2, 3], &NeverCancelled)
        .unwrap(),
    );
    assert_eq!(pixel(&image_pixels, 2, 8), [255, 0, 0, 255]);
    assert_eq!(pixel(&image_pixels, 13, 8), [0, 255, 0, 255]);
}

#[test]
fn stroke_and_outline_glyph_are_mounted_in_the_fast_dispatch() {
    let mut stroke_builder = builder();
    let line = PathResource::new(vec![
        PathSegment::MoveTo(point(2, 8)),
        PathSegment::LineTo(point(14, 8)),
    ])
    .unwrap();
    let style = LineStyle::new(
        scalar(2),
        LineCap::Butt,
        LineJoin::Bevel,
        scalar(10),
        DashPattern::new(Vec::new(), SceneScalar::ZERO).unwrap(),
        Matrix::IDENTITY,
    )
    .unwrap();
    stroke_builder
        .append_stroke(
            line,
            red(),
            style,
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    let stroke_scene = stroke_builder.finish().unwrap();
    let stroke_plan = plan(&stroke_scene, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let stroke_pixels = compose(
        &FastRasterJob::new(
            &stroke_scene,
            &stroke_plan,
            FastRasterLimits::default(),
            &NeverCancelled,
        )
        .unwrap()
        .render_all(&[0, 1, 2, 3], &NeverCancelled)
        .unwrap(),
    );
    assert_eq!(pixel(&stroke_pixels, 8, 8), [255, 0, 0, 255]);
    assert_eq!(pixel(&stroke_pixels, 8, 4), [255, 255, 255, 255]);

    let mut glyph_builder = builder();
    let outline = GlyphOutline::new(
        GraphicsResourceSource::new(ObjectRef::new(61, 0).unwrap(), 19, 0),
        1,
        1_000,
        rectangle(0, 0, 1_000, 1_000),
    )
    .unwrap();
    let glyph_transform = Matrix::new([
        scalar(8),
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        scalar(8),
        SceneScalar::ZERO,
        scalar(8),
    ]);
    glyph_builder
        .draw_glyph_run(
            vec![GlyphUse::new(outline, glyph_transform, 65)],
            blue(),
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    let glyph_scene = glyph_builder.finish().unwrap();
    let glyph_plan = plan(&glyph_scene, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let glyph_pixels = compose(
        &FastRasterJob::new(
            &glyph_scene,
            &glyph_plan,
            FastRasterLimits::default(),
            &NeverCancelled,
        )
        .unwrap()
        .render_all(&[0, 1, 2, 3], &NeverCancelled)
        .unwrap(),
    );
    assert_eq!(pixel(&glyph_pixels, 2, 2), [0, 0, 255, 255]);
    assert_eq!(pixel(&glyph_pixels, 12, 2), [255, 255, 255, 255]);
}

#[test]
fn registered_stroke_semantics_match_reviewed_reference_pixels() {
    let mut scene_builder = builder();
    let dashed = PathResource::new(vec![
        PathSegment::MoveTo(point(2, 13)),
        PathSegment::LineTo(point(14, 13)),
    ])
    .unwrap();
    scene_builder
        .append_stroke(
            dashed,
            red(),
            LineStyle::new(
                scalar(2),
                LineCap::Butt,
                LineJoin::Bevel,
                scalar(10),
                DashPattern::new(vec![scalar(2), scalar(2)], SceneScalar::ZERO).unwrap(),
                Matrix::IDENTITY,
            )
            .unwrap(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();

    let joined = PathResource::new(vec![
        PathSegment::MoveTo(point(2, 2)),
        PathSegment::LineTo(point(6, 6)),
        PathSegment::LineTo(point(10, 2)),
    ])
    .unwrap();
    scene_builder
        .append_stroke(
            joined,
            blue(),
            LineStyle::new(
                scalar(2),
                LineCap::Square,
                LineJoin::Miter,
                scalar(4),
                DashPattern::new(Vec::new(), SceneScalar::ZERO).unwrap(),
                Matrix::IDENTITY,
            )
            .unwrap(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(1),
        )
        .unwrap();

    let scale_x_two = Matrix::new([
        scalar(2),
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::ONE,
        SceneScalar::ZERO,
        SceneScalar::ZERO,
    ]);
    let transformed = PathResource::new(vec![
        PathSegment::MoveTo(point(6, 7)),
        PathSegment::LineTo(point(6, 11)),
    ])
    .unwrap();
    scene_builder
        .append_stroke(
            transformed,
            red(),
            LineStyle::new(
                scalar(2),
                LineCap::Round,
                LineJoin::Round,
                scalar(10),
                DashPattern::new(Vec::new(), SceneScalar::ZERO).unwrap(),
                scale_x_two,
            )
            .unwrap(),
            scale_x_two,
            SceneBounds::Page,
            source(2),
        )
        .unwrap();

    let bevel_fallback = PathResource::new(vec![
        PathSegment::MoveTo(point(1, 8)),
        PathSegment::LineTo(point(3, 10)),
        PathSegment::LineTo(point(5, 8)),
    ])
    .unwrap();
    scene_builder
        .append_stroke(
            bevel_fallback,
            blue(),
            LineStyle::new(
                scalar(2),
                LineCap::Butt,
                LineJoin::Miter,
                SceneScalar::ONE,
                DashPattern::new(Vec::new(), SceneScalar::ZERO).unwrap(),
                Matrix::IDENTITY,
            )
            .unwrap(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(3),
        )
        .unwrap();

    let hairline = PathResource::new(vec![
        PathSegment::MoveTo(point(7, 15)),
        PathSegment::LineTo(point(11, 15)),
    ])
    .unwrap();
    scene_builder
        .append_stroke(
            hairline,
            blue(),
            LineStyle::new(
                SceneScalar::ZERO,
                LineCap::Round,
                LineJoin::Round,
                scalar(10),
                DashPattern::new(Vec::new(), SceneScalar::ZERO).unwrap(),
                Matrix::IDENTITY,
            )
            .unwrap(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(4),
        )
        .unwrap();

    let scene = scene_builder.finish().unwrap();
    let plan = plan(
        &scene,
        config(PAGE_WIDTH, PAGE_HEIGHT, 1),
        PAGE_WIDTH,
        PAGE_HEIGHT,
    );
    let fast = compose(
        &FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled)
            .unwrap()
            .render_all(&[0], &NeverCancelled)
            .unwrap(),
    );
    let reference = reference_pixels(&scene);
    let maximum_channel_delta = fast
        .iter()
        .zip(&reference)
        .map(|(fast, reference)| fast.abs_diff(*reference))
        .max()
        .unwrap();
    assert!(
        maximum_channel_delta <= 16,
        "registered dash, cap, join, miter, or nonuniform stroke-transform semantics diverged: \
         maximum 4x4-to-8x8 channel delta was {maximum_channel_delta}"
    );
}

#[test]
fn advanced_stroke_joins_dashes_and_noncommuting_transforms_match_reference() {
    let mut join_builder = builder();
    for (index, (offset, join)) in [(0_i64, LineJoin::Round), (6_i64, LineJoin::Bevel)]
        .into_iter()
        .enumerate()
    {
        let path = PathResource::new(vec![
            PathSegment::MoveTo(point(1 + offset, 2)),
            PathSegment::LineTo(point(3 + offset, 5)),
            PathSegment::LineTo(point(5 + offset, 2)),
        ])
        .unwrap();
        join_builder
            .append_stroke(
                path,
                if index == 0 { red() } else { blue() },
                LineStyle::new(
                    scalar(2),
                    LineCap::Butt,
                    join,
                    scalar(10),
                    DashPattern::new(Vec::new(), SceneScalar::ZERO).unwrap(),
                    Matrix::IDENTITY,
                )
                .unwrap(),
                Matrix::IDENTITY,
                SceneBounds::Page,
                source(u32::try_from(index).unwrap()),
            )
            .unwrap();
    }
    let closed_dashed = PathResource::new(vec![
        PathSegment::MoveTo(point(1, 9)),
        PathSegment::LineTo(point(7, 9)),
        PathSegment::LineTo(point(7, 13)),
        PathSegment::LineTo(point(1, 13)),
        PathSegment::ClosePath,
    ])
    .unwrap();
    join_builder
        .append_stroke(
            closed_dashed,
            red(),
            LineStyle::new(
                scalar(1),
                LineCap::Square,
                LineJoin::Miter,
                scalar(10),
                DashPattern::new(vec![scalar(2)], scalar(1)).unwrap(),
                Matrix::IDENTITY,
            )
            .unwrap(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(2),
        )
        .unwrap();
    assert_fast_matches_reference(
        &join_builder.finish().unwrap(),
        16,
        "round/bevel joins or odd phased closed dash",
    );

    let stroke_to_page = Matrix::new([
        scalar(2),
        SceneScalar::from_scaled(500_000_000),
        SceneScalar::from_scaled(250_000_000),
        SceneScalar::ONE,
        scalar(1),
        SceneScalar::from_scaled(500_000_000),
    ]);
    let path_to_page = Matrix::new([
        SceneScalar::ONE,
        SceneScalar::ZERO,
        SceneScalar::from_scaled(250_000_000),
        SceneScalar::ONE,
        scalar(2),
        scalar(1),
    ]);
    assert_ne!(
        stroke_to_page.checked_multiply(path_to_page).unwrap(),
        path_to_page.checked_multiply(stroke_to_page).unwrap()
    );
    let mut transform_builder = builder();
    transform_builder
        .append_stroke(
            PathResource::new(vec![
                PathSegment::MoveTo(point(1, 6)),
                PathSegment::LineTo(point(4, 6)),
                PathSegment::LineTo(point(4, 9)),
            ])
            .unwrap(),
            blue(),
            LineStyle::new(
                scalar(1),
                LineCap::Round,
                LineJoin::Round,
                scalar(10),
                DashPattern::new(
                    vec![scalar(1), scalar(1)],
                    SceneScalar::from_scaled(500_000_000),
                )
                .unwrap(),
                stroke_to_page,
            )
            .unwrap(),
            path_to_page,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    assert_fast_matches_reference(
        &transform_builder.finish().unwrap(),
        24,
        "noncommuting path/stroke transform",
    );
}

#[test]
fn long_path_inner_loops_observe_cancellation_without_publication() {
    let mut segments = Vec::new();
    segments.push(PathSegment::MoveTo(point(1, 1)));
    for index in 0..512 {
        let x = 1 + i64::from(index % 14);
        let y = if index % 2 == 0 { 1 } else { 15 };
        segments.push(PathSegment::LineTo(point(x, y)));
    }
    let path = PathResource::new(segments).unwrap();
    let mut scene_builder = builder();
    scene_builder
        .append_fill(
            path,
            FillRule::Nonzero,
            red(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    let scene = scene_builder.finish().unwrap();
    let config = RenderConfig::validate(RenderConfigInput {
        tile_width: PAGE_WIDTH,
        tile_height: PAGE_HEIGHT,
        tile_halo: 1,
        cancellation_interval: 4_096,
        ..RenderConfigInput::fast_cpu_full()
    })
    .unwrap();
    let plan = plan(&scene, config, PAGE_WIDTH, PAGE_HEIGHT);
    let job =
        FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled).unwrap();
    let cancellation = CancelAfter::new(1);
    assert_eq!(
        job.render_all(&[0], &cancellation).unwrap_err().code(),
        FastRasterErrorCode::Cancelled
    );
    assert_eq!(cancellation.calls(), 2);
}

#[test]
fn many_subpaths_are_fallibly_accounted_at_the_intermediate_boundary() {
    let mut segments = Vec::new();
    for index in 0..64_i64 {
        let left = index % 8 * 2;
        let bottom = index / 8 * 2;
        segments.extend([
            PathSegment::MoveTo(point(left, bottom)),
            PathSegment::LineTo(point(left + 1, bottom)),
            PathSegment::LineTo(point(left + 1, bottom + 1)),
            PathSegment::ClosePath,
        ]);
    }
    let path = PathResource::new(segments).unwrap();
    let mut scene_builder = builder();
    scene_builder
        .append_fill(
            path,
            FillRule::Nonzero,
            red(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    let scene = scene_builder.finish().unwrap();
    let plan = plan(
        &scene,
        config(PAGE_WIDTH, PAGE_HEIGHT, 1),
        PAGE_WIDTH,
        PAGE_HEIGHT,
    );
    let baseline = FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled)
        .unwrap()
        .render_all(&[0], &NeverCancelled)
        .unwrap()
        .stats()
        .peak_intermediate_bytes();
    FastRasterJob::new(
        &scene,
        &plan,
        limits_with(FastRasterLimitKind::IntermediateBytes, baseline),
        &NeverCancelled,
    )
    .unwrap()
    .render_all(&[0], &NeverCancelled)
    .unwrap();
    let error = FastRasterJob::new(
        &scene,
        &plan,
        limits_with(FastRasterLimitKind::IntermediateBytes, baseline - 1),
        &NeverCancelled,
    )
    .and_then(|job| job.render_all(&[0], &NeverCancelled))
    .unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        FastRasterLimitKind::IntermediateBytes
    );
}

#[test]
fn deep_clip_stack_uses_cached_payload_and_accounts_transient_growth() {
    let mut scene_builder = builder();
    for index in 0..32_u32 {
        scene_builder
            .append_save(SceneBounds::Page, source(index))
            .unwrap();
    }
    append_fill_with_source(&mut scene_builder, rectangle(2, 2, 14, 14), red(), 32);
    for index in 0..32_u32 {
        scene_builder
            .append_restore(SceneBounds::Page, source(33 + index))
            .unwrap();
    }
    let scene = scene_builder.finish().unwrap();
    let plan = plan(
        &scene,
        config(PAGE_WIDTH, PAGE_HEIGHT, 1),
        PAGE_WIDTH,
        PAGE_HEIGHT,
    );
    let baseline = FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled)
        .unwrap()
        .render_all(&[0], &NeverCancelled)
        .unwrap()
        .stats();
    let exact = limits_with(
        FastRasterLimitKind::IntermediateBytes,
        baseline.peak_intermediate_bytes(),
    );
    FastRasterJob::new(&scene, &plan, exact, &NeverCancelled)
        .unwrap()
        .render_all(&[0], &NeverCancelled)
        .unwrap();
    let one_less = limits_with(
        FastRasterLimitKind::IntermediateBytes,
        baseline.peak_intermediate_bytes() - 1,
    );
    let error = FastRasterJob::new(&scene, &plan, one_less, &NeverCancelled)
        .and_then(|job| job.render_all(&[0], &NeverCancelled))
        .unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        FastRasterLimitKind::IntermediateBytes
    );
}

#[test]
fn every_fast_resource_dimension_has_exact_and_one_less_boundaries() {
    let scene = layered_scene();
    let plan = plan(&scene, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let baseline_job =
        FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled).unwrap();
    let baseline = baseline_job
        .render_all(&[0, 1, 2, 3], &NeverCancelled)
        .unwrap()
        .stats();

    for (kind, exact) in [
        (FastRasterLimitKind::Pixels, baseline.pixels()),
        (
            FastRasterLimitKind::Commands,
            baseline.commands_considered(),
        ),
        (FastRasterLimitKind::BinEntries, baseline.bin_entries()),
        (
            FastRasterLimitKind::RetainedBytes,
            baseline.retained_bytes(),
        ),
        (
            FastRasterLimitKind::IntermediateBytes,
            baseline.peak_intermediate_bytes(),
        ),
        (FastRasterLimitKind::Fuel, baseline.fuel()),
    ] {
        let exact_limits = limits_with(kind, exact);
        let exact_job = FastRasterJob::new(&scene, &plan, exact_limits, &NeverCancelled).unwrap();
        exact_job
            .render_all(&[0, 1, 2, 3], &NeverCancelled)
            .unwrap();

        let one_less_limits = limits_with(kind, exact - 1);
        let result = FastRasterJob::new(&scene, &plan, one_less_limits, &NeverCancelled)
            .and_then(|job| job.render_all(&[0, 1, 2, 3], &NeverCancelled));
        let error = result.unwrap_err();
        assert_eq!(error.code(), FastRasterErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), kind);
    }

    let interval = u64::from(plan.config().input().cancellation_interval);
    FastRasterJob::new(
        &scene,
        &plan,
        limits_with(FastRasterLimitKind::CancellationInterval, interval),
        &NeverCancelled,
    )
    .unwrap();
    let error = FastRasterJob::new(
        &scene,
        &plan,
        limits_with(FastRasterLimitKind::CancellationInterval, interval - 1),
        &NeverCancelled,
    )
    .err()
    .unwrap();
    assert_eq!(
        error.limit().unwrap().kind(),
        FastRasterLimitKind::CancellationInterval
    );
}

#[test]
fn cancellation_and_malformed_permutations_never_publish_partial_sets() {
    let scene = layered_scene();
    let plan = plan(&scene, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let initial_cancel = CancelAfter::new(0);
    assert_eq!(
        FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &initial_cancel,)
            .err()
            .unwrap()
            .code(),
        FastRasterErrorCode::Cancelled
    );
    let job =
        FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled).unwrap();

    let cancelled = CancelAfter::new(2);
    let error = job.render_all(&[0, 1, 2, 3], &cancelled).unwrap_err();
    assert_eq!(error.code(), FastRasterErrorCode::Cancelled);
    assert!(cancelled.calls() >= 3);

    for invalid in [&[0, 1, 2][..], &[0, 1, 2, 2][..], &[0, 1, 2, 4][..]] {
        assert_eq!(
            job.render_all(invalid, &NeverCancelled).unwrap_err().code(),
            FastRasterErrorCode::IdentityMismatch
        );
    }
}

#[test]
fn complete_render_config_identity_is_retained_and_unimplemented_profiles_fail_closed() {
    let scene = layered_scene();
    let first_plan = plan(&scene, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let second_plan = plan(&scene, config(8, 8, 2), PAGE_WIDTH, PAGE_HEIGHT);
    let first = FastRasterJob::new(
        &scene,
        &first_plan,
        FastRasterLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    let second = FastRasterJob::new(
        &scene,
        &second_plan,
        FastRasterLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    assert_ne!(
        first.identity().render_config_hash(),
        second.identity().render_config_hash()
    );
    assert_eq!(
        first.identity().render_config_hash(),
        first_plan.config().hash()
    );
    assert_eq!(first.identity().clipping_profile(), "fast-clip-mask-4x4-v1");

    let unsupported = RenderConfig::validate(RenderConfigInput {
        antialias: AntialiasMode::Coverage8x8,
        ..RenderConfigInput::fast_cpu_full()
    })
    .unwrap();
    let unsupported_plan = plan(&scene, unsupported, PAGE_WIDTH, PAGE_HEIGHT);
    assert_eq!(
        FastRasterJob::new(
            &scene,
            &unsupported_plan,
            FastRasterLimits::default(),
            &NeverCancelled,
        )
        .err()
        .unwrap()
        .code(),
        FastRasterErrorCode::InvalidRenderConfig
    );

    let mut red_builder = builder();
    append_fill(&mut red_builder, rectangle(0, 0, 8, 8), red(), 0);
    let red_scene = red_builder.finish().unwrap();
    let red_plan = plan(&red_scene, config(8, 8, 1), PAGE_WIDTH, PAGE_HEIGHT);
    let mut blue_builder = builder();
    append_fill(&mut blue_builder, rectangle(0, 0, 8, 8), blue(), 0);
    let same_binding_different_scene = blue_builder.finish().unwrap();
    assert_eq!(
        FastRasterJob::new(
            &same_binding_different_scene,
            &red_plan,
            FastRasterLimits::default(),
            &NeverCancelled,
        )
        .err()
        .unwrap()
        .code(),
        FastRasterErrorCode::IdentityMismatch,
        "the renderer recomputes the complete Scene-bound decision rather than trusting cardinality"
    );
}

fn layered_scene() -> Scene {
    let mut builder = builder();
    append_fill(&mut builder, rectangle(0, 0, 12, 16), red(), 0);
    append_fill(&mut builder, rectangle(4, 4, 16, 12), blue(), 1);
    builder.finish().unwrap()
}

fn builder() -> GraphicsSceneBuilder {
    let source = SourceIdentity::new(SourceStableId::new([7; 32]), SourceRevision::new(11));
    let binding = SceneBinding::new(source, 19, 3, ObjectRef::new(41, 0).unwrap());
    let page = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        scalar(i64::from(PAGE_WIDTH)),
        scalar(i64::from(PAGE_HEIGHT)),
    ])
    .unwrap();
    GraphicsSceneBuilder::new_v2(
        binding,
        PageGeometry::new(page, page, PageRotation::Degrees0),
        GraphicsSceneLimits::default(),
    )
}

fn append_fill(builder: &mut GraphicsSceneBuilder, path: PathResource, paint: Paint, index: u32) {
    let bounds = path_bounds(&path);
    builder
        .append_fill(
            path,
            FillRule::Nonzero,
            paint,
            Matrix::IDENTITY,
            bounds,
            source(index),
        )
        .unwrap();
}

fn append_fill_with_source(
    builder: &mut GraphicsSceneBuilder,
    path: PathResource,
    paint: Paint,
    index: u32,
) {
    append_fill(builder, path, paint, index);
}

fn rectangle(left: i64, bottom: i64, right: i64, top: i64) -> PathResource {
    PathResource::new(vec![
        PathSegment::MoveTo(point(left, bottom)),
        PathSegment::LineTo(point(right, bottom)),
        PathSegment::LineTo(point(right, top)),
        PathSegment::LineTo(point(left, top)),
        PathSegment::ClosePath,
    ])
    .unwrap()
}

fn path_bounds(path: &PathResource) -> SceneBounds {
    let mut minimum = (i64::MAX, i64::MAX);
    let mut maximum = (i64::MIN, i64::MIN);
    for segment in path.segments() {
        for point in match *segment {
            PathSegment::MoveTo(point) | PathSegment::LineTo(point) => vec![point],
            PathSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => vec![control_1, control_2, end],
            PathSegment::ClosePath => Vec::new(),
        } {
            minimum.0 = minimum.0.min(point.x().scaled());
            minimum.1 = minimum.1.min(point.y().scaled());
            maximum.0 = maximum.0.max(point.x().scaled());
            maximum.1 = maximum.1.max(point.y().scaled());
        }
    }
    SceneBounds::finite(
        ScenePoint::new(
            SceneScalar::from_scaled(minimum.0),
            SceneScalar::from_scaled(minimum.1),
        ),
        ScenePoint::new(
            SceneScalar::from_scaled(maximum.0),
            SceneScalar::from_scaled(maximum.1),
        ),
    )
    .unwrap()
}

fn bounds(left: i64, bottom: i64, right: i64, top: i64) -> SceneBounds {
    SceneBounds::finite(point(left, bottom), point(right, top)).unwrap()
}

fn point(x: i64, y: i64) -> ScenePoint {
    ScenePoint::new(scalar(x), scalar(y))
}

fn scalar(value: i64) -> SceneScalar {
    SceneScalar::from_scaled(value * 1_000_000_000)
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

fn blue() -> Paint {
    Paint::new(
        DeviceColor::Rgb {
            red: SceneUnit::ZERO,
            green: SceneUnit::ZERO,
            blue: SceneUnit::ONE,
        },
        SceneUnit::ONE,
        BlendMode::Normal,
    )
}

fn source(index: u32) -> CommandSource {
    CommandSource::new(
        ObjectRef::new(50, 0).unwrap(),
        0,
        u64::from(index) * 4,
        3,
        index,
    )
    .unwrap()
}

fn config(tile_width: u32, tile_height: u32, halo: u16) -> RenderConfig {
    RenderConfig::validate(RenderConfigInput {
        tile_width,
        tile_height,
        tile_halo: halo,
        cancellation_interval: 64,
        ..RenderConfigInput::fast_cpu_full()
    })
    .unwrap()
}

fn plan(scene: &Scene, config: RenderConfig, width: u32, height: u32) -> RenderPlan {
    let decision = CapabilityEvaluator::new(
        CapabilityProfile::m3_reference_v1(),
        PolicyLimits::default(),
    )
    .evaluate(scene, 23, &PolicyNever)
    .unwrap();
    let request = RenderPlanRequest::new(
        41,
        DeviceRect::new(0, 0, width, height).unwrap(),
        ZoomRatio::new(1, 1).unwrap(),
        1_000,
        PageRotation::Degrees0,
        OptionalContentIdentity::new(5),
        9,
    )
    .unwrap();
    match create_render_plan(
        scene,
        decision,
        config,
        request,
        RendererEpoch::new(7).unwrap(),
        PolicyLimits::default(),
        &PolicyNever,
    )
    .unwrap()
    {
        RenderPlanOutcome::Ready(plan) => plan,
        RenderPlanOutcome::NotPublishable(decision) => {
            panic!("expected supported fixture, got {:?}", decision.status())
        }
    }
}

fn compose(set: &pdf_rs_raster::fast::FastTileSet) -> Vec<u8> {
    let mut page = vec![0_u8; usize::try_from(PAGE_WIDTH * PAGE_HEIGHT * 4).unwrap()];
    for tile in set.tiles() {
        let rect = tile.identity().content_key().tile();
        for row in 0..rect.height() {
            let source_start = usize::try_from(u64::from(row) * u64::from(tile.stride())).unwrap();
            let row_bytes = usize::try_from(rect.width() * 4).unwrap();
            let target_start = usize::try_from(
                (u64::from(u32::try_from(rect.y()).unwrap() + row) * u64::from(PAGE_WIDTH)
                    + u64::from(u32::try_from(rect.x()).unwrap()))
                    * 4,
            )
            .unwrap();
            page[target_start..target_start + row_bytes]
                .copy_from_slice(&tile.pixels()[source_start..source_start + row_bytes]);
        }
    }
    page
}

fn reference_pixels(scene: &Scene) -> Vec<u8> {
    let mut job = ReferenceRenderJob::new(
        Arc::new(scene.clone()),
        ReferenceRenderConfig::opaque_srgb(PAGE_WIDTH, PAGE_HEIGHT).unwrap(),
        ReferenceRasterLimits::default(),
    );
    match job.poll(&ReferenceNeverCancelled) {
        ReferenceRenderPoll::Ready(buffer) => buffer.rgba().to_vec(),
        outcome => panic!("reviewed Reference expectation must render: {outcome:?}"),
    }
}

fn assert_fast_matches_reference(scene: &Scene, maximum_delta: u8, label: &str) {
    let plan = plan(
        scene,
        config(PAGE_WIDTH, PAGE_HEIGHT, 1),
        PAGE_WIDTH,
        PAGE_HEIGHT,
    );
    let fast = compose(
        &FastRasterJob::new(scene, &plan, FastRasterLimits::default(), &NeverCancelled)
            .unwrap()
            .render_all(&[0], &NeverCancelled)
            .unwrap(),
    );
    let reference = reference_pixels(scene);
    let observed = fast
        .iter()
        .zip(&reference)
        .map(|(fast, reference)| fast.abs_diff(*reference))
        .max()
        .unwrap();
    assert!(
        observed <= maximum_delta,
        "{label} exceeded the reviewed 4x4-to-8x8 differential: {observed} > {maximum_delta}"
    );
}

struct ReferenceNeverCancelled;

impl ReferenceRasterCancellation for ReferenceNeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

fn pixel(page: &[u8], x: u32, y: u32) -> [u8; 4] {
    let index = usize::try_from((u64::from(y) * u64::from(PAGE_WIDTH) + u64::from(x)) * 4).unwrap();
    page[index..index + 4].try_into().unwrap()
}

fn limits_with(kind: FastRasterLimitKind, value: u64) -> FastRasterLimits {
    let mut config = FastRasterLimitConfig::default();
    match kind {
        FastRasterLimitKind::Pixels => config.max_pixels = value,
        FastRasterLimitKind::Commands => config.max_commands = value,
        FastRasterLimitKind::BinEntries => config.max_bin_entries = value,
        FastRasterLimitKind::RetainedBytes => config.max_retained_bytes = value,
        FastRasterLimitKind::IntermediateBytes => config.max_intermediate_bytes = value,
        FastRasterLimitKind::Fuel => config.max_fuel = value,
        FastRasterLimitKind::CancellationInterval => config.max_cancellation_interval = value,
    }
    FastRasterLimits::validate(config).unwrap()
}

struct PolicyNever;

impl PolicyCancellation for PolicyNever {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct CancelAfter {
    calls: AtomicUsize,
    allowed: usize,
}

impl CancelAfter {
    const fn new(allowed: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            allowed,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl FastRasterCancellation for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst) >= self.allowed
    }
}
