use std::hint::black_box;
use std::num::NonZeroU32;
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_fast_raster::fast::{
    FastRasterCancellation, FastRasterErrorCode, FastRasterJob, FastRasterJobLimits,
    FastRasterJobPoll, FastRasterLimits, FastRasterOwnedJob, FastRasterPollBudget, NeverCancelled,
};
use pdf_rs_policy::{
    CapabilityEvaluator, CapabilityProfile, DeviceRect, NeverCancelled as PolicyNever,
    OptionalContentIdentity, PolicyJobLimits, PolicyLimits, RenderConfig, RenderConfigInput,
    RenderPlan, RenderPlanOutcome, RenderPlanRequest, RendererEpoch, ZoomRatio, create_render_plan,
};
use pdf_rs_scene::{
    BlendMode, CommandSource, DeviceColor, FillRule, GlyphOutline, GlyphUse,
    GraphicsResourceSource, GraphicsSceneBuilder, GraphicsSceneLimits, Matrix, PageGeometry,
    PageRotation, Paint, PathResource, PathSegment, Scene, SceneBinding, SceneBounds, ScenePoint,
    SceneRect, SceneScalar, SceneUnit,
};
use pdf_rs_syntax::ObjectRef;

const WIDTH: u32 = 306;
const HEIGHT: u32 = 396;
const SAMPLE_COUNT: usize = 21;
const TILE_COUNT: usize = 4;

#[test]
#[ignore = "captures local release-profile M4 Fast component measurements"]
fn captures_fixed_scope_fast_component_samples() {
    let scene = Arc::new(benchmark_scene());
    let plan = Arc::new(fast_plan(&scene));
    assert_eq!(plan.tiles().len(), TILE_COUNT);

    let mut first_tile_samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut full_job_samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut tile_poll_samples = Vec::with_capacity(SAMPLE_COUNT * TILE_COUNT);
    let mut stats = None;
    for _ in 0..SAMPLE_COUNT {
        let sample = owned_job_sample(Arc::clone(&scene), Arc::clone(&plan));
        first_tile_samples.push(sample.first_tile_ns);
        full_job_samples.push(sample.full_job_ns);
        tile_poll_samples.extend(sample.tile_poll_ns);
        assert_eq!(
            *stats.get_or_insert(sample.stats),
            sample.stats,
            "every owned job must publish identical accounting"
        );
    }

    let order = [0, 1, 2, 3];
    let mut cold_bin_samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut cold_fingerprint = None;
    for _ in 0..SAMPLE_COUNT {
        let start = Instant::now();
        let job = FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled)
            .expect("cold Fast job");
        let tiles = job
            .render_all(&order, &NeverCancelled)
            .expect("cold Fast render");
        cold_bin_samples.push(elapsed_ns(start));
        let observed = tile_fingerprint(&tiles);
        assert_eq!(*cold_fingerprint.get_or_insert(observed), observed);
        black_box(observed);
    }

    let reused_job =
        FastRasterJob::new(&scene, &plan, FastRasterLimits::default(), &NeverCancelled)
            .expect("reused-bin Fast job");
    black_box(
        reused_job
            .render_all(&order, &NeverCancelled)
            .expect("reused-bin warmup"),
    );
    let mut reused_bin_samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut reused_fingerprint = None;
    for _ in 0..SAMPLE_COUNT {
        let start = Instant::now();
        let tiles = reused_job
            .render_all(&order, &NeverCancelled)
            .expect("reused-bin Fast render");
        reused_bin_samples.push(elapsed_ns(start));
        let observed = tile_fingerprint(&tiles);
        assert_eq!(*reused_fingerprint.get_or_insert(observed), observed);
        black_box(observed);
    }
    assert_eq!(cold_fingerprint, reused_fingerprint);

    let mut cancellation_samples = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        cancellation_samples.push(cancellation_sample(Arc::clone(&scene), Arc::clone(&plan)));
    }

    print_samples("first_tile_ns", &first_tile_samples);
    print_samples("full_owned_job_ns", &full_job_samples);
    print_samples("tile_poll_ns", &tile_poll_samples);
    print_samples("cold_bins_full_render_ns", &cold_bin_samples);
    print_samples("reused_bins_full_render_ns", &reused_bin_samples);
    print_samples("cancellation_latency_ns", &cancellation_samples);
    let stats = stats.expect("owned samples publish accounting");
    println!(
        "m4-fast-component width={WIDTH} height={HEIGHT} tiles={} pixels={} retained_bytes={} peak_intermediate_bytes={} fuel={} cancellation_checks={}",
        stats.tiles(),
        stats.pixels(),
        stats.retained_bytes(),
        stats.peak_intermediate_bytes(),
        stats.fuel(),
        stats.cancellation_checks(),
    );
}

struct OwnedJobSample {
    first_tile_ns: u64,
    full_job_ns: u64,
    tile_poll_ns: Vec<u64>,
    stats: pdf_rs_fast_raster::fast::FastRasterStats,
}

fn owned_job_sample(scene: Arc<Scene>, plan: Arc<RenderPlan>) -> OwnedJobSample {
    let mut job = FastRasterOwnedJob::new(
        scene,
        plan,
        FastRasterLimits::default(),
        PolicyJobLimits::default(),
        FastRasterJobLimits::default(),
    )
    .expect("owned Fast job");
    let budget =
        FastRasterPollBudget::new(NonZeroU32::new(4_096).expect("nonzero")).expect("poll budget");
    let full_start = Instant::now();
    let mut first_tile_ns = None;
    let mut tile_poll_ns = Vec::with_capacity(TILE_COUNT);
    loop {
        let before = job.completed_tiles();
        let poll_start = Instant::now();
        let poll = job.poll(budget, &NeverCancelled);
        let poll_elapsed = elapsed_ns(poll_start);
        let after = job.completed_tiles();
        if after > before {
            assert_eq!(after, before + 1, "one poll publishes at most one tile");
            first_tile_ns.get_or_insert_with(|| elapsed_ns(full_start));
            tile_poll_ns.push(poll_elapsed);
        }
        if poll == FastRasterJobPoll::Ready {
            break;
        }
    }
    let full_job_ns = elapsed_ns(full_start);
    let tiles = job
        .take_result()
        .expect("terminal owned result")
        .expect("owned Fast render");
    assert_eq!(tile_poll_ns.len(), TILE_COUNT);
    assert_eq!(tiles.tiles().len(), TILE_COUNT);
    black_box(tile_fingerprint(&tiles));
    OwnedJobSample {
        first_tile_ns: first_tile_ns.expect("first tile completes"),
        full_job_ns,
        tile_poll_ns,
        stats: tiles.stats(),
    }
}

fn cancellation_sample(scene: Arc<Scene>, plan: Arc<RenderPlan>) -> u64 {
    let cancellation = Arc::new(AtomicCancellation::default());
    let barrier = Arc::new(Barrier::new(2));
    let child_cancellation = Arc::clone(&cancellation);
    let child_barrier = Arc::clone(&barrier);
    let worker = thread::spawn(move || {
        let job = FastRasterJob::new(
            &scene,
            &plan,
            FastRasterLimits::default(),
            child_cancellation.as_ref(),
        )
        .expect("cancellation Fast job");
        child_barrier.wait();
        job.render_all(&[0, 1, 2, 3], child_cancellation.as_ref())
    });
    barrier.wait();
    thread::sleep(Duration::from_millis(1));
    let start = Instant::now();
    cancellation.cancelled.store(true, Ordering::Release);
    let error = worker
        .join()
        .expect("cancellation worker does not panic")
        .expect_err("in-flight render observes cancellation");
    assert_eq!(error.code(), FastRasterErrorCode::Cancelled);
    elapsed_ns(start)
}

#[derive(Default)]
struct AtomicCancellation {
    cancelled: AtomicBool,
}

impl FastRasterCancellation for AtomicCancellation {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

fn benchmark_scene() -> Scene {
    let source_identity =
        SourceIdentity::new(SourceStableId::new([0x4d; 32]), SourceRevision::new(1));
    let binding = SceneBinding::new(
        source_identity,
        83,
        0,
        ObjectRef::new(501, 0).expect("page object"),
    );
    let page = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        scalar(i64::from(WIDTH)),
        scalar(i64::from(HEIGHT)),
    ])
    .expect("page box");
    let mut builder = GraphicsSceneBuilder::new_v2(
        binding,
        PageGeometry::new(page, page, PageRotation::Degrees0),
        GraphicsSceneLimits::default(),
    );
    append_fill(
        &mut builder,
        rectangle(0, 0, i64::from(WIDTH), i64::from(HEIGHT)),
        paint(238, 241, 247),
        0,
    );
    append_fill(
        &mut builder,
        rectangle(24, 236, 282, 372),
        paint(28, 33, 48),
        1,
    );
    append_fill(
        &mut builder,
        rectangle(24, 24, 282, 212),
        paint(255, 255, 255),
        2,
    );

    let outline = GlyphOutline::new(
        GraphicsResourceSource::new(ObjectRef::new(601, 0).expect("font object"), 83, 0),
        65,
        1_000,
        rectangle(0, 0, 1_000, 1_000),
    )
    .expect("glyph outline");
    let glyphs = [
        (42, 306, 42),
        (96, 306, 42),
        (150, 306, 42),
        (42, 158, 24),
        (78, 158, 24),
        (114, 158, 24),
        (150, 158, 24),
        (186, 158, 24),
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
        .draw_glyph_run(glyphs, paint(37, 99, 235), SceneBounds::Page, source(3))
        .expect("glyph run");
    builder.finish().expect("benchmark Scene")
}

fn append_fill(builder: &mut GraphicsSceneBuilder, path: PathResource, paint: Paint, index: u32) {
    builder
        .append_fill(
            path,
            FillRule::Nonzero,
            paint,
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(index),
        )
        .expect("benchmark fill");
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

fn paint(red: u8, green: u8, blue: u8) -> Paint {
    Paint::new(
        DeviceColor::Rgb {
            red: SceneUnit::from_u16(u16::from(red) * 257),
            green: SceneUnit::from_u16(u16::from(green) * 257),
            blue: SceneUnit::from_u16(u16::from(blue) * 257),
        },
        SceneUnit::ONE,
        BlendMode::Normal,
    )
}

fn point(x: i64, y: i64) -> ScenePoint {
    ScenePoint::new(scalar(x), scalar(y))
}

fn scalar(value: i64) -> SceneScalar {
    SceneScalar::from_scaled(value * SceneScalar::ONE.scaled())
}

fn source(index: u32) -> CommandSource {
    CommandSource::new(
        ObjectRef::new(701, 0).expect("content object"),
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
    .expect("benchmark capability decision");
    let config =
        RenderConfig::validate(RenderConfigInput::fast_cpu_full()).expect("Fast render config");
    let request = RenderPlanRequest::new(
        1,
        DeviceRect::new(0, 0, WIDTH, HEIGHT).expect("device page"),
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
        RendererEpoch::new(13).expect("renderer epoch"),
        PolicyLimits::default(),
        &PolicyNever,
    )
    .expect("render plan")
    {
        RenderPlanOutcome::Ready(plan) => plan,
        RenderPlanOutcome::NotPublishable(decision) => {
            panic!(
                "benchmark unexpectedly unsupported: {:?}",
                decision.status()
            )
        }
    }
}

fn tile_fingerprint(tiles: &pdf_rs_fast_raster::fast::FastTileSet) -> u64 {
    tiles
        .tiles()
        .iter()
        .flat_map(|tile| tile.pixels())
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            hash.wrapping_mul(0x0000_0100_0000_01b3) ^ u64::from(*byte)
        })
}

fn elapsed_ns(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).expect("elapsed duration fits u64")
}

fn print_samples(metric: &str, samples: &[u64]) {
    assert!(!samples.is_empty());
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];
    let p95 = sorted[(sorted.len() * 95).div_ceil(100) - 1];
    let p99 = sorted[(sorted.len() * 99).div_ceil(100) - 1];
    println!(
        "m4-fast-component metric={metric} median_ns={median} p95_ns={p95} p99_ns={p99} raw_samples_ns={samples:?}"
    );
}
