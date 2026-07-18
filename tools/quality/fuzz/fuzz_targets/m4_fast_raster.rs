#![no_main]

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
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
const MAX_FUZZ_INPUT: usize = 256;

fuzz_target!(|data: &[u8]| {
    if data.len() <= MAX_FUZZ_INPUT {
        exercise_fast_raster(data);
    }
});

fn exercise_fast_raster(data: &[u8]) {
    let scene = generated_scene(data);
    let plan = fast_plan(&scene);
    let order = tile_order(byte(data, 31));
    let fast = FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled)
        .expect("generated scene remains Fast-eligible")
        .render_all(&order, &NeverCancelled)
        .expect("generated scene renders atomically");
    assert_eq!(fast.tiles().len(), 4);
    assert_eq!(fast.stats().tiles(), 4);
    assert_eq!(fast.stats().pixels(), u64::from(PAGE_EDGE * PAGE_EDGE));
    assert_eq!(compose(&fast), reference_pixels(&scene));
}

fn generated_scene(data: &[u8]) -> Scene {
    let mut builder = builder(data);
    match byte(data, 0) % 4 {
        0 => append_fill_family(&mut builder, data),
        1 => append_clip_family(&mut builder, data),
        2 => append_image_family(&mut builder, data),
        _ => append_glyph_family(&mut builder, data),
    }
    builder.finish().expect("bounded generated scene")
}

fn append_fill_family(builder: &mut GraphicsSceneBuilder, data: &[u8]) {
    builder
        .append_fill(
            rectangle(data, 1),
            FillRule::Nonzero,
            paint(data, 9),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(0),
        )
        .expect("bounded fill");
}

fn append_clip_family(builder: &mut GraphicsSceneBuilder, data: &[u8]) {
    builder
        .append_save(SceneBounds::Page, source(0))
        .expect("bounded save");
    builder
        .append_clip(
            rectangle(data, 1),
            if byte(data, 8) & 1 == 0 {
                FillRule::Nonzero
            } else {
                FillRule::EvenOdd
            },
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(1),
        )
        .expect("bounded clip");
    builder
        .append_fill(
            fixed_rectangle(0, 0, 16, 16),
            FillRule::Nonzero,
            paint(data, 9),
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(2),
        )
        .expect("bounded clipped fill");
    builder
        .append_restore(SceneBounds::Page, source(3))
        .expect("bounded restore");
}

fn append_image_family(builder: &mut GraphicsSceneBuilder, data: &[u8]) {
    let decoded = (0..12)
        .map(|index| byte(data, 1 + index))
        .collect::<Vec<_>>();
    let image = ImageResource::new(
        GraphicsResourceSource::new(
            ObjectRef::new(601, 0).expect("image object"),
            97,
            u64::from(byte(data, 30)),
        ),
        2,
        2,
        ImageColorSpace::DeviceRgb,
        8,
        false,
        decoded,
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
            source(0),
        )
        .expect("bounded image command");
}

fn append_glyph_family(builder: &mut GraphicsSceneBuilder, data: &[u8]) {
    let outline = GlyphOutline::new(
        GraphicsResourceSource::new(
            ObjectRef::new(701, 0).expect("font object"),
            97,
            u64::from(byte(data, 30)),
        ),
        u32::from(byte(data, 1)),
        1_000,
        fixed_rectangle(0, 0, 1_000, 1_000),
    )
    .expect("bounded glyph outline");
    let edge = i64::from(1 + byte(data, 2) % 8);
    let limit = u8::try_from(17 - edge).expect("positive glyph position limit");
    let x = i64::from(byte(data, 3) % limit);
    let y = i64::from(byte(data, 4) % limit);
    let glyph = GlyphUse::new(
        outline,
        Matrix::new([
            scalar(edge),
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            scalar(edge),
            scalar(x),
            scalar(y),
        ]),
        u32::from(byte(data, 5)),
    );
    builder
        .draw_glyph_run(vec![glyph], paint(data, 9), SceneBounds::Page, source(0))
        .expect("bounded glyph run");
}

fn builder(data: &[u8]) -> GraphicsSceneBuilder {
    let source_identity = SourceIdentity::new(
        SourceStableId::new([byte(data, 29); 32]),
        SourceRevision::new(u64::try_from(data.len()).expect("bounded input length")),
    );
    let binding = SceneBinding::new(
        source_identity,
        97,
        u32::from(byte(data, 30)),
        ObjectRef::new(501, 0).expect("page object"),
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

fn rectangle(data: &[u8], offset: usize) -> PathResource {
    let left = i64::from(byte(data, offset) % 15);
    let bottom = i64::from(byte(data, offset + 1) % 15);
    let width_limit = u8::try_from(16 - left).expect("positive width limit");
    let height_limit = u8::try_from(16 - bottom).expect("positive height limit");
    let width = i64::from(1 + byte(data, offset + 2) % width_limit);
    let height = i64::from(1 + byte(data, offset + 3) % height_limit);
    fixed_rectangle(left, bottom, left + width, bottom + height)
}

fn fixed_rectangle(left: i64, bottom: i64, right: i64, top: i64) -> PathResource {
    PathResource::new(vec![
        PathSegment::MoveTo(point(left, bottom)),
        PathSegment::LineTo(point(right, bottom)),
        PathSegment::LineTo(point(right, top)),
        PathSegment::LineTo(point(left, top)),
        PathSegment::ClosePath,
    ])
    .expect("rectangle path")
}

fn paint(data: &[u8], offset: usize) -> Paint {
    Paint::new(
        DeviceColor::Rgb {
            red: unit(byte(data, offset)),
            green: unit(byte(data, offset + 1)),
            blue: unit(byte(data, offset + 2)),
        },
        SceneUnit::ONE,
        BlendMode::Normal,
    )
}

fn unit(value: u8) -> SceneUnit {
    SceneUnit::from_u16(u16::from(value) * 257)
}

fn point(x: i64, y: i64) -> ScenePoint {
    ScenePoint::new(scalar(x), scalar(y))
}

fn scalar(value: i64) -> SceneScalar {
    SceneScalar::from_scaled(value * SceneScalar::ONE.scaled())
}

fn source(index: u32) -> CommandSource {
    CommandSource::new(
        ObjectRef::new(801, 0).expect("content object"),
        0,
        u64::from(index) * 4,
        3,
        index,
    )
    .expect("command source")
}

fn byte(data: &[u8], index: usize) -> u8 {
    data.get(index)
        .copied()
        .unwrap_or_else(|| u8::try_from((index * 73 + 29) % 256).expect("fallback byte"))
}

fn tile_order(selector: u8) -> [u32; 4] {
    const ORDERS: [[u32; 4]; 8] = [
        [0, 1, 2, 3],
        [3, 2, 1, 0],
        [0, 2, 1, 3],
        [1, 3, 0, 2],
        [2, 0, 3, 1],
        [3, 1, 2, 0],
        [1, 0, 3, 2],
        [2, 3, 0, 1],
    ];
    ORDERS[usize::from(selector % 8)]
}

fn fast_plan(scene: &Scene) -> RenderPlan {
    let decision = CapabilityEvaluator::new(
        CapabilityProfile::m3_reference_v1(),
        PolicyLimits::default(),
    )
    .evaluate(scene, 1, &PolicyNever)
    .expect("generated capability decision");
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
        RendererEpoch::new(11).expect("renderer epoch"),
        PolicyLimits::default(),
        &PolicyNever,
    )
    .expect("render plan")
    {
        RenderPlanOutcome::Ready(plan) => plan,
        RenderPlanOutcome::NotPublishable(decision) => {
            panic!(
                "generated page unexpectedly unsupported: {:?}",
                decision.status()
            )
        }
    }
}

fn compose(set: &FastTileSet) -> Vec<u8> {
    let mut page = vec![0_u8; usize::try_from(PAGE_EDGE * PAGE_EDGE * 4).expect("page bytes")];
    for tile in set.tiles() {
        let rect = tile.identity().content_key().tile();
        for row in 0..rect.height() {
            let source_start =
                usize::try_from(u64::from(row) * u64::from(tile.stride())).expect("source offset");
            let row_bytes = usize::try_from(rect.width() * 4).expect("row bytes");
            let target_start = usize::try_from(
                (u64::from(u32::try_from(rect.y()).expect("nonnegative tile y") + row)
                    * u64::from(PAGE_EDGE)
                    + u64::from(u32::try_from(rect.x()).expect("nonnegative tile x")))
                    * 4,
            )
            .expect("target offset");
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
        outcome => panic!("generated Reference terminal: {outcome:?}"),
    }
}

struct ReferenceNever;

impl ReferenceRasterCancellation for ReferenceNever {
    fn is_cancelled(&self) -> bool {
        false
    }
}
