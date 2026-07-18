//! Fixed post-parse Fast CPU holdout owned by the non-product quality package.

use std::sync::Arc;

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_fast_raster::fast::{FastRasterJob, FastRasterLimits, FastTileSet, NeverCancelled};
use pdf_rs_policy::{
    CapabilityEvaluator, CapabilityProfile, DeviceRect, NeverCancelled as PolicyNever,
    OptionalContentIdentity, PolicyLimits, RenderConfig, RenderConfigInput, RenderPlan,
    RenderPlanOutcome, RenderPlanRequest, RendererEpoch, ZoomRatio, create_render_plan,
};
use pdf_rs_raster::reference::{
    ReferenceRasterCancellation, ReferenceRasterLimits, ReferenceRenderConfig, ReferenceRenderJob,
    ReferenceRenderPoll,
};
use pdf_rs_scene::{
    BlendMode, CommandSource, DeviceColor, FillRule, GlyphOutline, GlyphUse,
    GraphicsResourceSource, GraphicsSceneBuilder, GraphicsSceneLimits, ImageColorSpace,
    ImageResource, Matrix, PageGeometry, PageRotation, Paint, PathResource, PathSegment, Scene,
    SceneBinding, SceneBounds, ScenePoint, SceneRect, SceneScalar, SceneUnit,
};
use pdf_rs_syntax::ObjectRef;

const PAGE_EDGE: u32 = 16;
const HOLDOUT_PAGES: u32 = 1_000;
const FAMILY_COUNT: u32 = 4;
const EXPECTED_COHORT_SHA256: &str =
    "cef88fcfcaa0d31cc74d9958e3185a9216cc7b638a3240ca7e3813a01b26ae42";
const EXPECTED_PIXEL_SHA256: &str =
    "f15710d0d7c0ef3d92c52b65a520576506a406a94789df04201d8e19ab363b9c";
const EXPECTED_TOTAL_FAST_FUEL: u64 = 23_741_984;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Family {
    Fill,
    Clip,
    Image,
    Glyph,
}

impl Family {
    const fn for_seed(seed: u32) -> Self {
        match seed % FAMILY_COUNT {
            0 => Self::Fill,
            1 => Self::Clip,
            2 => Self::Image,
            _ => Self::Glyph,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Clip => "clip",
            Self::Image => "image",
            Self::Glyph => "glyph",
        }
    }
}

#[test]
fn fixed_post_parse_holdout_qualifies_one_thousand_fast_pages() {
    let mut descriptors = Vec::new();
    let mut product_pixels = Vec::new();
    let mut family_counts = [0_u32; FAMILY_COUNT as usize];
    let mut maximum_channel_delta = 0_u8;
    let mut changed_channels = 0_u64;
    let mut pages_with_differences = [0_u32; FAMILY_COUNT as usize];
    let mut family_maximum_delta = [0_u8; FAMILY_COUNT as usize];
    let mut worst_seed = 0_u32;
    let mut total_fast_tiles = 0_u64;
    let mut total_fast_fuel = 0_u64;

    for seed in 0..HOLDOUT_PAGES {
        let family = Family::for_seed(seed);
        family_counts[usize::try_from(seed % FAMILY_COUNT).unwrap()] += 1;
        descriptors.extend_from_slice(format!("{seed}:{}\n", family.label()).as_bytes());

        let scene = holdout_scene(seed, family);
        let plan = fast_plan(&scene);
        let order = [0, 1, 2, 3];
        let fast = FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled)
            .expect("eligible holdout page admits Fast CPU")
            .render_all(&order, &NeverCancelled)
            .expect("eligible holdout page publishes every Fast tile");
        assert_eq!(fast.tiles().len(), 4, "seed {seed} published partial tiles");
        assert_eq!(fast.stats().pixels(), u64::from(PAGE_EDGE * PAGE_EDGE));
        total_fast_tiles += fast.stats().tiles();
        total_fast_fuel = total_fast_fuel
            .checked_add(fast.stats().fuel())
            .expect("aggregate deterministic fuel fits u64");

        let fast_pixels = compose(&fast);
        let reference_pixels = reference_pixels(&scene);
        assert_eq!(fast_pixels.len(), reference_pixels.len());
        let mut page_maximum_delta = 0_u8;
        for (fast, reference) in fast_pixels.iter().zip(&reference_pixels) {
            let delta = fast.abs_diff(*reference);
            page_maximum_delta = page_maximum_delta.max(delta);
            changed_channels += u64::from(delta != 0);
        }
        let family_index = usize::try_from(seed % FAMILY_COUNT).unwrap();
        if page_maximum_delta != 0 {
            pages_with_differences[family_index] += 1;
        }
        family_maximum_delta[family_index] =
            family_maximum_delta[family_index].max(page_maximum_delta);
        if page_maximum_delta > maximum_channel_delta {
            maximum_channel_delta = page_maximum_delta;
            worst_seed = seed;
        }
        product_pixels.extend_from_slice(&fast_pixels);
    }

    assert_eq!(family_counts, [250, 250, 250, 250]);
    assert_eq!(total_fast_tiles, 4_000);
    assert_eq!(pages_with_differences, [0, 0, 0, 0]);
    assert_eq!(family_maximum_delta, [0, 0, 0, 0]);
    assert_eq!(
        maximum_channel_delta, 0,
        "seed {worst_seed}: integer-aligned holdout pages must match reviewed Reference pixels exactly"
    );
    assert_eq!(changed_channels, 0);
    assert_eq!(total_fast_fuel, EXPECTED_TOTAL_FAST_FUEL);

    let cohort_hash = hex_digest(&sha256(&descriptors).expect("cohort hash"));
    let pixel_hash = hex_digest(&sha256(&product_pixels).expect("pixel hash"));
    assert_eq!(cohort_hash, EXPECTED_COHORT_SHA256);
    assert_eq!(pixel_hash, EXPECTED_PIXEL_SHA256);
}

fn holdout_scene(seed: u32, family: Family) -> Scene {
    let mut builder = builder(seed);
    match family {
        Family::Fill => append_fill_family(&mut builder, seed),
        Family::Clip => append_clip_family(&mut builder, seed),
        Family::Image => append_image_family(&mut builder, seed),
        Family::Glyph => append_glyph_family(&mut builder, seed),
    }
    builder.finish().expect("holdout Scene finishes")
}

fn append_fill_family(builder: &mut GraphicsSceneBuilder, seed: u32) {
    let left = i64::from(1 + (seed * 3) % 8);
    let bottom = i64::from(1 + (seed * 5) % 8);
    let width = i64::from(2 + (seed * 7) % 6);
    let height = i64::from(2 + (seed * 11) % 6);
    append_fill(
        builder,
        rectangle(
            left,
            bottom,
            (left + width).min(15),
            (bottom + height).min(15),
        ),
        paint(seed),
        0,
    );
    append_fill(
        builder,
        rectangle(0, 0, i64::from(1 + seed % 8), i64::from(1 + seed % 8)),
        paint(seed.rotate_left(7)),
        1,
    );
}

fn append_clip_family(builder: &mut GraphicsSceneBuilder, seed: u32) {
    builder
        .append_save(SceneBounds::Page, source(seed, 0))
        .expect("save");
    let edge = i64::from(4 + seed % 9);
    let clip = rectangle(1, 1, edge, edge);
    builder
        .append_clip(
            clip.clone(),
            if seed.is_multiple_of(2) {
                FillRule::Nonzero
            } else {
                FillRule::EvenOdd
            },
            Matrix::IDENTITY,
            path_bounds(&clip),
            source(seed, 1),
        )
        .expect("clip");
    append_fill(builder, rectangle(0, 0, 16, 16), paint(seed), 2);
    builder
        .append_restore(SceneBounds::Page, source(seed, 3))
        .expect("restore");
}

fn append_image_family(builder: &mut GraphicsSceneBuilder, seed: u32) {
    let image = ImageResource::new(
        GraphicsResourceSource::new(
            ObjectRef::new(10_000 + seed, 0).expect("image object"),
            41,
            u64::from(seed),
        ),
        2,
        2,
        ImageColorSpace::DeviceRgb,
        8,
        false,
        vec![
            byte(seed),
            byte(seed.rotate_left(3)),
            byte(seed.rotate_left(7)),
            byte(seed.wrapping_add(31)),
            byte(seed.wrapping_add(67)),
            byte(seed.wrapping_add(101)),
            byte(seed.wrapping_add(137)),
            byte(seed.wrapping_add(173)),
            byte(seed.wrapping_add(211)),
            byte(seed.wrapping_add(239)),
            byte(seed.wrapping_add(17)),
            byte(seed.wrapping_add(83)),
        ],
    )
    .expect("bounded image");
    builder
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
            source(seed, 0),
        )
        .expect("image command");
}

fn append_glyph_family(builder: &mut GraphicsSceneBuilder, seed: u32) {
    let outline = GlyphOutline::new(
        GraphicsResourceSource::new(
            ObjectRef::new(20_000 + seed, 0).expect("font object"),
            41,
            u64::from(seed),
        ),
        seed,
        1_000,
        rectangle(0, 0, 1_000, 1_000),
    )
    .expect("bounded glyph");
    let x = i64::from(seed % 8);
    let y = i64::from((seed / 8) % 8);
    let glyphs = [
        (x, y, 3_i64),
        ((x + 5) % 12, (y + 3) % 12, 4_i64),
        ((x + 9) % 13, (y + 7) % 13, 2_i64),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (x, y, edge))| {
        GlyphUse::new(
            outline.clone(),
            Matrix::new([
                scalar(edge),
                SceneScalar::ZERO,
                SceneScalar::ZERO,
                scalar(edge),
                scalar(x),
                scalar(y),
            ]),
            u32::try_from(index).expect("glyph index"),
        )
    })
    .collect::<Vec<_>>();
    builder
        .draw_glyph_run(glyphs, paint(seed), SceneBounds::Page, source(seed, 0))
        .expect("glyph run");
}

fn builder(seed: u32) -> GraphicsSceneBuilder {
    let source_identity =
        SourceIdentity::new(SourceStableId::new([0x4d; 32]), SourceRevision::new(1));
    let binding = SceneBinding::new(
        source_identity,
        41,
        seed,
        ObjectRef::new(seed + 1, 0).expect("page object"),
    );
    let page = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        scalar(i64::from(PAGE_EDGE)),
        scalar(i64::from(PAGE_EDGE)),
    ])
    .expect("page box");
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
            source(0, index),
        )
        .expect("fill command");
}

fn rectangle(left: i64, bottom: i64, right: i64, top: i64) -> PathResource {
    PathResource::new(vec![
        PathSegment::MoveTo(point(left, bottom)),
        PathSegment::LineTo(point(right, bottom)),
        PathSegment::LineTo(point(right, top)),
        PathSegment::LineTo(point(left, top)),
        PathSegment::ClosePath,
    ])
    .expect("rectangle path")
}

fn path_bounds(path: &PathResource) -> SceneBounds {
    let mut minimum = (i64::MAX, i64::MAX);
    let mut maximum = (i64::MIN, i64::MIN);
    for segment in path.segments() {
        let points = match *segment {
            PathSegment::MoveTo(point) | PathSegment::LineTo(point) => [Some(point), None, None],
            PathSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => [Some(control_1), Some(control_2), Some(end)],
            PathSegment::ClosePath => [None, None, None],
        };
        for point in points.into_iter().flatten() {
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
    .expect("finite path bounds")
}

fn paint(seed: u32) -> Paint {
    Paint::new(
        DeviceColor::Rgb {
            red: unit(seed),
            green: unit(seed.rotate_left(9)),
            blue: unit(seed.rotate_left(17)),
        },
        SceneUnit::ONE,
        BlendMode::Normal,
    )
}

fn byte(value: u32) -> u8 {
    u8::try_from((value.wrapping_mul(73).wrapping_add(29)) % 256).expect("byte")
}

fn unit(value: u32) -> SceneUnit {
    SceneUnit::from_u16(u16::from(byte(value)) * 257)
}

fn point(x: i64, y: i64) -> ScenePoint {
    ScenePoint::new(scalar(x), scalar(y))
}

fn scalar(value: i64) -> SceneScalar {
    SceneScalar::from_scaled(value * SceneScalar::ONE.scaled())
}

fn source(seed: u32, index: u32) -> CommandSource {
    CommandSource::new(
        ObjectRef::new(30_000 + seed, 0).expect("content object"),
        0,
        u64::from(index) * 4,
        3,
        index,
    )
    .expect("command source")
}

fn fast_plan(scene: &Scene) -> RenderPlan {
    let decision = CapabilityEvaluator::new(
        CapabilityProfile::m3_reference_v1(),
        PolicyLimits::default(),
    )
    .evaluate(scene, 1, &PolicyNever)
    .expect("holdout capability decision");
    let config = RenderConfig::validate(RenderConfigInput {
        tile_width: 8,
        tile_height: 8,
        tile_halo: 1,
        cancellation_interval: 64,
        ..RenderConfigInput::fast_cpu_full()
    })
    .expect("Fast config");
    let request = RenderPlanRequest::new(
        1,
        DeviceRect::new(0, 0, PAGE_EDGE, PAGE_EDGE).expect("device page"),
        ZoomRatio::new(1, 1).expect("unit zoom"),
        1_000,
        PageRotation::Degrees0,
        OptionalContentIdentity::new(1),
        1,
    )
    .expect("plan request");
    match create_render_plan(
        scene,
        decision,
        config,
        request,
        RendererEpoch::new(7).expect("renderer epoch"),
        PolicyLimits::default(),
        &PolicyNever,
    )
    .expect("render plan")
    {
        RenderPlanOutcome::Ready(plan) => plan,
        RenderPlanOutcome::NotPublishable(decision) => {
            panic!(
                "holdout page unexpectedly unsupported: {:?}",
                decision.status()
            )
        }
    }
}

fn compose(set: &FastTileSet) -> Vec<u8> {
    let mut page = vec![0_u8; usize::try_from(PAGE_EDGE * PAGE_EDGE * 4).unwrap()];
    for tile in set.tiles() {
        let rect = tile.identity().content_key().tile();
        for row in 0..rect.height() {
            let source_start = usize::try_from(u64::from(row) * u64::from(tile.stride())).unwrap();
            let row_bytes = usize::try_from(rect.width() * 4).unwrap();
            let target_start = usize::try_from(
                (u64::from(u32::try_from(rect.y()).unwrap() + row) * u64::from(PAGE_EDGE)
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
        ReferenceRenderConfig::opaque_srgb(PAGE_EDGE, PAGE_EDGE).expect("Reference config"),
        ReferenceRasterLimits::default(),
    );
    match job.poll(&ReferenceNever) {
        ReferenceRenderPoll::Ready(buffer) => buffer.rgba().to_vec(),
        outcome => panic!("eligible holdout Reference terminal: {outcome:?}"),
    }
}

struct ReferenceNever;

impl ReferenceRasterCancellation for ReferenceNever {
    fn is_cancelled(&self) -> bool {
        false
    }
}
