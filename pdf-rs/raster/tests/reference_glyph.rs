#![allow(
    dead_code,
    reason = "the staged glyph harness shares complete geometry and coverage kernels"
)]

#[path = "reference_glyph_support/mod.rs"]
mod reference;

use std::{cell::Cell, mem::size_of};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_raster::reference::{NormalizedQ16, PremultipliedRgbaQ16, ReferenceSrgbQ16};
use pdf_rs_scene::{
    BlendMode, CommandSource, DeviceColor, FillRule, GlyphOutline, GlyphRun, GlyphUse,
    GraphicsCommand, GraphicsResourceEntry, GraphicsResourceSource, GraphicsSceneBuilder,
    GraphicsSceneLimits, Matrix, PageGeometry, PageRotation, Paint, PathResource, PathSegment,
    SceneBinding, SceneBounds, ScenePoint, SceneRect, SceneScalar, SceneUnit,
};
use pdf_rs_syntax::ObjectRef;

use reference::coverage::CoverageMask;
use reference::geometry::{GeometryCancellation, GeometryLimits, GeometryWork};
use reference::glyph::{
    GlyphCancellation, GlyphFailure, GlyphLimitKind, GlyphLimits, GlyphRaster, GlyphStats,
    paint_glyph_run, rasterize_glyph_run,
};

struct NeverCancel;

impl GlyphCancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl GeometryCancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct CancelAtCheck {
    checks: Cell<u64>,
    cancel_at: u64,
}

impl GlyphCancellation for CancelAtCheck {
    fn is_cancelled(&self) -> bool {
        let next = self.checks.get() + 1;
        self.checks.set(next);
        next >= self.cancel_at
    }
}

fn scalar(value: &str) -> SceneScalar {
    SceneScalar::from_decimal(value).unwrap()
}

fn point(x: &str, y: &str) -> ScenePoint {
    ScenePoint::new(scalar(x), scalar(y))
}

fn matrix(values: [&str; 6]) -> Matrix {
    Matrix::new(values.map(scalar))
}

fn geometry() -> PageGeometry {
    geometry_with_rotation(PageRotation::Degrees0)
}

fn geometry_with_rotation(rotation: PageRotation) -> PageGeometry {
    let bounds = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::ONE,
        SceneScalar::ONE,
    ])
    .unwrap();
    PageGeometry::new(bounds, bounds, rotation)
}

fn binding() -> SceneBinding {
    SceneBinding::new(
        SourceIdentity::new(SourceStableId::new([7; 32]), SourceRevision::new(9)),
        42,
        0,
        ObjectRef::new(3, 0).unwrap(),
    )
}

fn source() -> CommandSource {
    CommandSource::new(ObjectRef::new(4, 0).unwrap(), 0, 0, 2, 0).unwrap()
}

fn resource_source(glyph_id: u32) -> GraphicsResourceSource {
    GraphicsResourceSource::new(ObjectRef::new(9, 0).unwrap(), 42, u64::from(glyph_id))
}

fn square(size: &str) -> PathResource {
    PathResource::new(vec![
        PathSegment::MoveTo(point("0", "0")),
        PathSegment::LineTo(point(size, "0")),
        PathSegment::LineTo(point(size, size)),
        PathSegment::LineTo(point("0", size)),
        PathSegment::ClosePath,
    ])
    .unwrap()
}

fn triangle(size: &str) -> PathResource {
    PathResource::new(vec![
        PathSegment::MoveTo(point("0", "0")),
        PathSegment::LineTo(point(size, "0")),
        PathSegment::LineTo(point("0", size)),
        PathSegment::ClosePath,
    ])
    .unwrap()
}

fn octagon() -> PathResource {
    PathResource::new(vec![
        PathSegment::MoveTo(point("250", "0")),
        PathSegment::LineTo(point("750", "0")),
        PathSegment::LineTo(point("1000", "250")),
        PathSegment::LineTo(point("1000", "750")),
        PathSegment::LineTo(point("750", "1000")),
        PathSegment::LineTo(point("250", "1000")),
        PathSegment::LineTo(point("0", "750")),
        PathSegment::LineTo(point("0", "250")),
        PathSegment::ClosePath,
    ])
    .unwrap()
}

fn curved_arch(size: &str) -> PathResource {
    PathResource::new(vec![
        PathSegment::MoveTo(point("0", "0")),
        PathSegment::CubicTo {
            control_1: point("0", size),
            control_2: point(size, size),
            end: point(size, "0"),
        },
        PathSegment::ClosePath,
    ])
    .unwrap()
}

fn empty_path() -> PathResource {
    PathResource::new(Vec::new()).unwrap()
}

fn black() -> Paint {
    Paint::new(
        DeviceColor::Gray(SceneUnit::ZERO),
        SceneUnit::ONE,
        BlendMode::Normal,
    )
}

fn white() -> PremultipliedRgbaQ16 {
    ReferenceSrgbQ16::gray(NormalizedQ16::ONE).with_constant_alpha(NormalizedQ16::ONE)
}

fn gray() -> PremultipliedRgbaQ16 {
    ReferenceSrgbQ16::gray(NormalizedQ16::from_bits(32_768).unwrap())
        .with_constant_alpha(NormalizedQ16::ONE)
}

fn build_run(
    glyphs: Vec<(GlyphOutline, Matrix, u32)>,
    paint: Paint,
) -> (GlyphRun, Vec<GraphicsResourceEntry>) {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), geometry(), GraphicsSceneLimits::default());
    builder
        .draw_glyph_run(
            glyphs
                .into_iter()
                .map(|(outline, transform, code)| GlyphUse::new(outline, transform, code))
                .collect(),
            paint,
            SceneBounds::Page,
            source(),
        )
        .unwrap();
    let scene = builder.finish().unwrap();
    let graphics = scene.graphics().unwrap();
    let run = match graphics.commands()[0].command() {
        GraphicsCommand::DrawGlyphRun(run) => run.clone(),
        command => panic!("unexpected command: {command:?}"),
    };
    (run, graphics.resources().to_vec())
}

fn outline(glyph_id: u32, units_per_em: u16, path: PathResource) -> GlyphOutline {
    GlyphOutline::new(resource_source(glyph_id), glyph_id, units_per_em, path).unwrap()
}

fn raster(
    run: &GlyphRun,
    resources: &[GraphicsResourceEntry],
    width: u32,
    height: u32,
    backdrop: &[PremultipliedRgbaQ16],
) -> Result<GlyphRaster, GlyphFailure> {
    rasterize_glyph_run(
        run,
        resources,
        geometry(),
        width,
        height,
        backdrop,
        None,
        GlyphLimits::default(),
        &NeverCancel,
    )
}

fn rgba(raster: &GlyphRaster) -> Vec<[u8; 4]> {
    raster
        .pixels()
        .iter()
        .copied()
        .map(PremultipliedRgbaQ16::to_straight_rgba8)
        .collect()
}

#[test]
fn one_em_square_uses_font_units_then_glyph_and_page_transforms() {
    let (run, resources) = build_run(
        vec![(outline(1, 1_000, square("1000")), Matrix::IDENTITY, 65)],
        black(),
    );
    let result = raster(&run, &resources, 1, 1, &[white()]).unwrap();
    assert_eq!(GlyphRaster::PROFILE, "reference-glyph-v1");
    assert_eq!((result.width(), result.height()), (1, 1));
    assert_eq!(
        result.pixel(0, 0).unwrap().to_straight_rgba8(),
        [0, 0, 0, 255]
    );
    assert_eq!(rgba(&result), vec![[0, 0, 0, 255]]);

    let half_width = matrix(["0.5", "0", "0", "1", "0", "0"]);
    let (run, resources) = build_run(
        vec![(outline(2, 1_000, square("1000")), half_width, 66)],
        black(),
    );
    assert_eq!(
        rgba(&raster(&run, &resources, 1, 1, &[white()]).unwrap()),
        vec![[128, 128, 128, 255]]
    );

    let (run, resources) = build_run(
        vec![(outline(3, 2_000, square("1000")), Matrix::IDENTITY, 67)],
        black(),
    );
    assert_eq!(
        rgba(&raster(&run, &resources, 1, 1, &[white()]).unwrap()),
        vec![[191, 191, 191, 255]]
    );
}

#[test]
fn analytic_triangle_and_two_by_two_translation_are_literal() {
    let (triangle_run, triangle_resources) = build_run(
        vec![(outline(4, 1_000, triangle("1000")), Matrix::IDENTITY, 68)],
        black(),
    );
    assert_eq!(
        rgba(&raster(&triangle_run, &triangle_resources, 1, 1, &[white()]).unwrap()),
        vec![[143, 143, 143, 255]]
    );

    let half = matrix(["0.5", "0", "0", "0.5", "0.5", "0.5"]);
    let (run, resources) = build_run(vec![(outline(5, 1_000, square("1000")), half, 69)], black());
    assert_eq!(
        rgba(&raster(&run, &resources, 2, 2, &[white(); 4]).unwrap()),
        vec![
            [255, 255, 255, 255],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [255, 255, 255, 255],
        ]
    );
}

#[test]
fn page_rotations_apply_after_font_unit_and_glyph_transforms() {
    let lower_left_quarter = matrix(["0.5", "0", "0", "0.5", "0", "0"]);
    let (run, resources) = build_run(
        vec![(outline(13, 1_000, square("1000")), lower_left_quarter, 76)],
        black(),
    );
    for (rotation, expected) in [
        (
            PageRotation::Degrees0,
            vec![
                [255, 255, 255, 255],
                [255, 255, 255, 255],
                [0, 0, 0, 255],
                [255, 255, 255, 255],
            ],
        ),
        (
            PageRotation::Degrees90,
            vec![
                [0, 0, 0, 255],
                [255, 255, 255, 255],
                [255, 255, 255, 255],
                [255, 255, 255, 255],
            ],
        ),
        (
            PageRotation::Degrees180,
            vec![
                [255, 255, 255, 255],
                [0, 0, 0, 255],
                [255, 255, 255, 255],
                [255, 255, 255, 255],
            ],
        ),
        (
            PageRotation::Degrees270,
            vec![
                [255, 255, 255, 255],
                [255, 255, 255, 255],
                [255, 255, 255, 255],
                [0, 0, 0, 255],
            ],
        ),
    ] {
        let raster = rasterize_glyph_run(
            &run,
            &resources,
            geometry_with_rotation(rotation),
            2,
            2,
            &[white(); 4],
            None,
            GlyphLimits::default(),
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(rgba(&raster), expected, "{rotation:?}");
    }
}

#[test]
fn run_outlines_union_before_one_alpha_and_blend_application() {
    let half_alpha = Paint::new(
        DeviceColor::Gray(SceneUnit::ZERO),
        SceneUnit::from_u16(32_768),
        BlendMode::Normal,
    );
    let glyph = outline(6, 1_000, square("1000"));
    let (single, single_resources) =
        build_run(vec![(glyph.clone(), Matrix::IDENTITY, 70)], half_alpha);
    let (overlap, overlap_resources) = build_run(
        vec![
            (glyph.clone(), Matrix::IDENTITY, 70),
            (glyph, Matrix::IDENTITY, 70),
        ],
        half_alpha,
    );
    let single = raster(&single, &single_resources, 1, 1, &[white()]).unwrap();
    let overlap = raster(&overlap, &overlap_resources, 1, 1, &[white()]).unwrap();
    assert_eq!(rgba(&overlap), rgba(&single));
    assert_eq!(rgba(&overlap), vec![[127, 127, 127, 255]]);

    let red_multiply = Paint::new(
        DeviceColor::Rgb {
            red: SceneUnit::ONE,
            green: SceneUnit::ZERO,
            blue: SceneUnit::ZERO,
        },
        SceneUnit::ONE,
        BlendMode::Multiply,
    );
    let (run, resources) = build_run(
        vec![(outline(7, 1_000, square("1000")), Matrix::IDENTITY, 71)],
        red_multiply,
    );
    assert_eq!(
        rgba(&raster(&run, &resources, 1, 1, &[gray()]).unwrap()),
        vec![[128, 0, 0, 255]]
    );
}

#[test]
fn clip_uses_the_same_sample_mask_and_empty_outline_is_a_no_op() {
    let (run, resources) = build_run(
        vec![(outline(8, 1_000, square("1000")), Matrix::IDENTITY, 72)],
        black(),
    );
    let mut geometry_work = GeometryWork::new(GeometryLimits::default(), &NeverCancel).unwrap();
    let mut clip = CoverageMask::empty(1, 1, &mut geometry_work).unwrap();
    let mut left_half = 0_u64;
    for sample_y in 0..8 {
        for sample_x in 0..4 {
            left_half |= 1_u64 << (sample_y * 8 + sample_x);
        }
    }
    clip.set_sample_mask(0, 0, left_half).unwrap();
    let clipped = rasterize_glyph_run(
        &run,
        &resources,
        geometry(),
        1,
        1,
        &[white()],
        Some(&clip),
        GlyphLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(rgba(&clipped), vec![[128, 128, 128, 255]]);

    let (empty, resources) = build_run(
        vec![(outline(9, 1_000, empty_path()), Matrix::IDENTITY, 32)],
        black(),
    );
    let empty = raster(&empty, &resources, 1, 1, &[white()]).unwrap();
    assert_eq!(rgba(&empty), vec![[255, 255, 255, 255]]);
    assert_eq!(empty.stats().samples(), 0);
    assert_eq!(empty.stats().composites(), 0);
}

#[test]
fn invalid_resource_and_shape_inputs_fail_structurally() {
    let (run, resources) = build_run(
        vec![(outline(10, 1_000, square("1000")), Matrix::IDENTITY, 73)],
        black(),
    );
    assert_eq!(
        raster(&run, &[], 1, 1, &[white()]),
        Err(GlyphFailure::InvalidResource { resource: 0 })
    );
    let mut path_builder =
        GraphicsSceneBuilder::new_v2(binding(), geometry(), GraphicsSceneLimits::default());
    path_builder
        .append_fill(
            square("1"),
            FillRule::Nonzero,
            black(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(),
        )
        .unwrap();
    let path_scene = path_builder.finish().unwrap();
    let path_resources = path_scene.graphics().unwrap().resources();
    assert_eq!(
        raster(&run, path_resources, 1, 1, &[white()]),
        Err(GlyphFailure::InvalidResource { resource: 0 })
    );
    assert_eq!(
        raster(&run, &resources, 2, 1, &[white()]),
        Err(GlyphFailure::InvalidGlyph)
    );

    let invalid_limits = GlyphLimits {
        max_glyphs: 0,
        ..GlyphLimits::default()
    };
    assert_eq!(
        rasterize_glyph_run(
            &run,
            &resources,
            geometry(),
            1,
            1,
            &[white()],
            None,
            invalid_limits,
            &NeverCancel,
        ),
        Err(GlyphFailure::InvalidGlyph)
    );
    let invalid_recursion = GlyphLimits {
        max_curve_recursion: 33,
        ..GlyphLimits::default()
    };
    assert_eq!(
        rasterize_glyph_run(
            &run,
            &resources,
            geometry(),
            1,
            1,
            &[white()],
            None,
            invalid_recursion,
            &NeverCancel,
        ),
        Err(GlyphFailure::InvalidGlyph)
    );
}

#[test]
fn nonzero_glyph_resource_identifier_resolves_in_mixed_first_use_order() {
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding(), geometry(), GraphicsSceneLimits::default());
    builder
        .append_fill(
            square("1"),
            FillRule::Nonzero,
            black(),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(),
        )
        .unwrap();
    builder
        .draw_glyph_run(
            vec![GlyphUse::new(
                outline(16, 1_000, square("1000")),
                Matrix::IDENTITY,
                79,
            )],
            black(),
            SceneBounds::Page,
            source(),
        )
        .unwrap();
    let scene = builder.finish().unwrap();
    let graphics = scene.graphics().unwrap();
    let run = match graphics.commands()[1].command() {
        GraphicsCommand::DrawGlyphRun(run) => run,
        command => panic!("unexpected command: {command:?}"),
    };
    assert_eq!(run.glyphs()[0].outline().value(), 1);
    let result = raster(run, graphics.resources(), 1, 1, &[white()]).unwrap();
    assert_eq!(rgba(&result), vec![[0, 0, 0, 255]]);
    assert_eq!(result.stats().resource_lookups(), 1);
}

#[test]
fn glyph_curve_recursion_is_bounded_and_singular_transforms_are_no_ops() {
    let (curved, resources) = build_run(
        vec![(
            outline(14, 1_000, curved_arch("1000")),
            Matrix::IDENTITY,
            77,
        )],
        black(),
    );
    let tight = GlyphLimits {
        max_curve_recursion: 1,
        ..GlyphLimits::default()
    };
    assert!(matches!(
        rasterize_glyph_run(
            &curved,
            &resources,
            geometry(),
            1,
            1,
            &[white()],
            None,
            tight,
            &NeverCancel,
        ),
        Err(GlyphFailure::Limit {
            kind: GlyphLimitKind::CurveRecursion,
            limit: 1,
            ..
        })
    ));

    let singular = matrix(["0", "0", "0", "1", "0", "0"]);
    let (run, resources) = build_run(
        vec![(outline(15, 1_000, square("1000")), singular, 78)],
        black(),
    );
    let result = raster(&run, &resources, 1, 1, &[white()]).unwrap();
    assert_eq!(rgba(&result), vec![[255, 255, 255, 255]]);
    assert_eq!(result.stats().samples(), 0);
    assert_eq!(result.stats().composites(), 0);
}

#[test]
fn every_glyph_budget_has_an_exact_and_one_less_boundary() {
    let (run, resources) = build_run(
        vec![
            (outline(11, 1_000, square("1000")), Matrix::IDENTITY, 74),
            (outline(12, 1_000, square("1000")), Matrix::IDENTITY, 75),
        ],
        black(),
    );
    let backdrop = [white(); 4];
    let baseline = raster(&run, &resources, 2, 2, &backdrop).unwrap();
    let stats = baseline.stats();
    let exact = GlyphLimits {
        max_glyphs: stats.glyphs(),
        max_resource_lookups: stats.resource_lookups(),
        max_outline_segments: stats.outline_segments(),
        max_flattened_segments: stats.flattened_segments(),
        max_edges: stats.edges(),
        max_samples: stats.samples(),
        max_coverage_bytes: stats.coverage_bytes(),
        max_output_pixels: stats.output_pixels(),
        max_composites: stats.composites(),
        max_geometry_bytes: stats.peak_geometry_bytes(),
        max_retained_bytes: stats.retained_bytes(),
        max_geometry_fuel: stats.geometry_fuel(),
        max_fuel: stats.fuel(),
        max_curve_recursion: GlyphLimits::default().max_curve_recursion,
    };
    let exact_result = rasterize_glyph_run(
        &run,
        &resources,
        geometry(),
        2,
        2,
        &backdrop,
        None,
        exact,
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(exact_result, baseline);

    for (kind, tight) in [
        (
            GlyphLimitKind::Glyphs,
            GlyphLimits {
                max_glyphs: stats.glyphs() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::ResourceLookups,
            GlyphLimits {
                max_resource_lookups: stats.resource_lookups() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::OutlineSegments,
            GlyphLimits {
                max_outline_segments: stats.outline_segments() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::FlattenedSegments,
            GlyphLimits {
                max_flattened_segments: stats.flattened_segments() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::Edges,
            GlyphLimits {
                max_edges: stats.edges() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::Samples,
            GlyphLimits {
                max_samples: stats.samples() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::CoverageBytes,
            GlyphLimits {
                max_coverage_bytes: stats.coverage_bytes() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::OutputPixels,
            GlyphLimits {
                max_output_pixels: stats.output_pixels() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::Composites,
            GlyphLimits {
                max_composites: stats.composites() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::GeometryBytes,
            GlyphLimits {
                max_geometry_bytes: stats.peak_geometry_bytes() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::RetainedBytes,
            GlyphLimits {
                max_retained_bytes: stats.retained_bytes() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::GeometryFuel,
            GlyphLimits {
                max_geometry_fuel: stats.geometry_fuel() - 1,
                ..exact
            },
        ),
        (
            GlyphLimitKind::Fuel,
            GlyphLimits {
                max_fuel: stats.fuel() - 1,
                ..exact
            },
        ),
    ] {
        let error = rasterize_glyph_run(
            &run,
            &resources,
            geometry(),
            2,
            2,
            &backdrop,
            None,
            tight,
            &NeverCancel,
        )
        .unwrap_err();
        assert!(
            matches!(error, GlyphFailure::Limit { kind: actual, .. } if actual == kind),
            "{kind:?} produced {error:?}"
        );
    }
}

#[test]
fn aggregate_retention_counts_coverage_plus_transient_geometry() {
    let (run, resources) = build_run(
        vec![(outline(17, 1_000, octagon()), Matrix::IDENTITY, 80)],
        black(),
    );
    let backdrop = [white(); 4];
    let baseline = raster(&run, &resources, 2, 2, &backdrop).unwrap();
    let stats = baseline.stats();
    assert!(stats.peak_geometry_bytes() > stats.geometry_bytes());
    let coverage_geometry_peak = stats
        .coverage_bytes()
        .checked_add(stats.peak_geometry_bytes())
        .unwrap();
    let coverage_pixel_peak = stats
        .coverage_bytes()
        .checked_add(
            stats
                .output_pixels()
                .checked_mul(u64::try_from(size_of::<PremultipliedRgbaQ16>()).unwrap())
                .unwrap(),
        )
        .unwrap();
    assert!(coverage_geometry_peak > coverage_pixel_peak);
    assert_eq!(
        stats.retained_bytes(),
        coverage_geometry_peak.max(coverage_pixel_peak)
    );
    assert_eq!(
        stats.peak_working_bytes(),
        coverage_geometry_peak.max(coverage_pixel_peak)
    );

    let exact = GlyphLimits {
        max_retained_bytes: stats.retained_bytes(),
        ..GlyphLimits::default()
    };
    assert_eq!(
        rasterize_glyph_run(
            &run,
            &resources,
            geometry(),
            2,
            2,
            &backdrop,
            None,
            exact,
            &NeverCancel,
        )
        .unwrap(),
        baseline
    );

    let one_less_retained = stats.retained_bytes() - 1;
    let retained_error = rasterize_glyph_run(
        &run,
        &resources,
        geometry(),
        2,
        2,
        &backdrop,
        None,
        GlyphLimits {
            max_retained_bytes: one_less_retained,
            ..GlyphLimits::default()
        },
        &NeverCancel,
    )
    .unwrap_err();
    assert!(matches!(
        retained_error,
        GlyphFailure::Limit {
            kind: GlyphLimitKind::RetainedBytes,
            limit,
            consumed,
            attempted,
        } if limit == one_less_retained
            && consumed >= stats.coverage_bytes()
            && consumed + attempted > limit
    ));

    let independent_geometry_error = rasterize_glyph_run(
        &run,
        &resources,
        geometry(),
        2,
        2,
        &backdrop,
        None,
        GlyphLimits {
            max_geometry_bytes: stats.peak_geometry_bytes() - 1,
            ..GlyphLimits::default()
        },
        &NeverCancel,
    )
    .unwrap_err();
    assert!(matches!(
        independent_geometry_error,
        GlyphFailure::Limit {
            kind: GlyphLimitKind::GeometryBytes,
            ..
        }
    ));
}

#[test]
fn standalone_peak_working_tracks_geometry_and_actual_pixel_stages() {
    let (geometry_run, geometry_resources) = build_run(
        vec![(outline(24, 1_000, octagon()), Matrix::IDENTITY, 81)],
        black(),
    );
    let backdrop = [white(); 4];
    let geometry_stats = raster(&geometry_run, &geometry_resources, 2, 2, &backdrop)
        .unwrap()
        .stats();
    let geometry_stage = geometry_stats
        .coverage_bytes()
        .checked_add(geometry_stats.peak_geometry_bytes())
        .unwrap();
    assert_eq!(geometry_stats.peak_working_bytes(), geometry_stage);

    let (pixel_run, pixel_resources) = build_run(
        vec![(outline(25, 1_000, empty_path()), Matrix::IDENTITY, 82)],
        black(),
    );
    let pixel_stats = raster(&pixel_run, &pixel_resources, 2, 2, &backdrop)
        .unwrap()
        .stats();
    let pixel_geometry_stage = pixel_stats
        .coverage_bytes()
        .checked_add(pixel_stats.peak_geometry_bytes())
        .unwrap();
    assert!(pixel_stats.retained_bytes() > pixel_geometry_stage);
    assert_eq!(
        pixel_stats.peak_working_bytes(),
        pixel_stats.retained_bytes()
    );
}

#[test]
fn cancellation_is_observed_before_allocations_during_geometry_and_before_return() {
    let (run, resources) = build_run(
        vec![(outline(12, 1_000, square("1000")), Matrix::IDENTITY, 75)],
        black(),
    );
    let baseline = raster(&run, &resources, 1, 1, &[white()]).unwrap();
    assert!(baseline.stats().cancellation_checks() >= 5);
    for cancel_at in 1..=baseline.stats().cancellation_checks() {
        let cancellation = CancelAtCheck {
            checks: Cell::new(0),
            cancel_at,
        };
        assert_eq!(
            rasterize_glyph_run(
                &run,
                &resources,
                geometry(),
                1,
                1,
                &[white()],
                None,
                GlyphLimits::default(),
                &cancellation,
            ),
            Err(GlyphFailure::Cancelled),
            "cancellation check {cancel_at}"
        );
    }
}

#[test]
fn mounted_one_less_composite_guard_precedes_surface_mutation_and_counter_commit() {
    let (run, resources) = build_run(
        vec![(outline(12, 1_000, square("1000")), Matrix::IDENTITY, 75)],
        black(),
    );
    let mut baseline_pixels = [white()];
    let mut baseline = GlyphStats::default();
    paint_glyph_run(
        &run,
        &resources,
        geometry(),
        1,
        1,
        &mut baseline_pixels,
        None,
        GlyphLimits::default(),
        &NeverCancel,
        &mut baseline,
    )
    .unwrap();
    assert!(baseline.composites() > 0);

    let mut pixels = [white()];
    let mut stats = GlyphStats::default();
    let limit = baseline.composites() - 1;
    let failure = paint_glyph_run(
        &run,
        &resources,
        geometry(),
        1,
        1,
        &mut pixels,
        None,
        GlyphLimits {
            max_composites: limit,
            ..GlyphLimits::default()
        },
        &NeverCancel,
        &mut stats,
    )
    .unwrap_err();
    assert_eq!(
        failure,
        GlyphFailure::Limit {
            kind: GlyphLimitKind::Composites,
            limit,
            consumed: 0,
            attempted: baseline.composites(),
        }
    );
    assert_eq!(stats.composites(), 0);
    assert_eq!(pixels, [white()]);
}

#[test]
fn mounted_zero_remaining_outline_rejects_before_surface_or_completed_counter_mutation() {
    let (run, resources) = build_run(
        vec![(outline(26, 1_000, square("1000")), Matrix::IDENTITY, 83)],
        black(),
    );
    let limits = GlyphLimits {
        max_outline_segments: 0,
        ..GlyphLimits::default()
    };
    let mut pixels = [white()];
    let mut stats = GlyphStats::default();
    let failure = paint_glyph_run(
        &run,
        &resources,
        geometry(),
        1,
        1,
        &mut pixels,
        None,
        limits,
        &NeverCancel,
        &mut stats,
    )
    .unwrap_err();
    assert_eq!(
        failure,
        GlyphFailure::Limit {
            kind: GlyphLimitKind::OutlineSegments,
            limit: 0,
            consumed: 0,
            attempted: 5,
        }
    );
    assert_eq!(pixels, [white()]);
    assert_eq!(stats.glyphs(), 0);
    assert_eq!(stats.resource_lookups(), 0);
    assert_eq!(stats.outline_segments(), 0);
    assert_eq!(stats.coverage_bytes(), 0);
    assert_eq!(stats.fuel(), 1);
    assert_eq!(stats.cancellation_checks(), 1);

    assert_eq!(
        rasterize_glyph_run(
            &run,
            &resources,
            geometry(),
            1,
            1,
            &[white()],
            None,
            limits,
            &NeverCancel,
        ),
        Err(GlyphFailure::InvalidGlyph),
        "the standalone constructor retains its strict nonzero limit contract"
    );
}
