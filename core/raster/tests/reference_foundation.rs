use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_raster::reference::{
    AlphaMode, PixelFormat, PixelOrigin, ReferenceOutputProfile, ReferencePixelBufferVersion,
    ReferenceRasterCancellation, ReferenceRasterLimitConfig, ReferenceRasterLimits,
    ReferenceRenderConfig, ReferenceRenderErrorCode, ReferenceRenderJob, ReferenceRenderLimitKind,
    ReferenceRenderPhase, ReferenceRenderPoll,
};
use pdf_rs_scene::{
    CommandSource, PageGeometry, PageRotation, Scene, SceneBinding, SceneBuilder, SceneLimitConfig,
    SceneLimits, SceneRect, SceneScalar,
};
use pdf_rs_syntax::ObjectRef;

fn source(salt: u8) -> SourceIdentity {
    SourceIdentity::new(
        SourceStableId::new([salt; 32]),
        SourceRevision::new(u64::from(salt) + 1),
    )
}

fn binding(salt: u8) -> SceneBinding {
    SceneBinding::new(source(salt), 42, 0, ObjectRef::new(3, 0).unwrap())
}

fn geometry() -> PageGeometry {
    let bounds = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::from_decimal("100").unwrap(),
        SceneScalar::from_decimal("200").unwrap(),
    ])
    .unwrap();
    PageGeometry::new(bounds, bounds, PageRotation::Degrees0)
}

fn empty_scene(salt: u8) -> Arc<Scene> {
    Arc::new(
        SceneBuilder::new(binding(salt), geometry(), SceneLimits::default())
            .finish()
            .unwrap(),
    )
}

fn marked_scene(salt: u8) -> Arc<Scene> {
    let source = |index| {
        CommandSource::new(
            ObjectRef::new(4, 0).unwrap(),
            0,
            u64::from(index) * 4,
            3,
            index,
        )
        .unwrap()
    };
    let mut builder = SceneBuilder::new(
        binding(salt),
        geometry(),
        SceneLimits::validate(SceneLimitConfig::default()).unwrap(),
    );
    builder
        .begin_marked_content(b"Artifact", None, source(0))
        .unwrap();
    builder.end_marked_content(source(1)).unwrap();
    Arc::new(builder.finish().unwrap())
}

fn limits(mut update: impl FnMut(&mut ReferenceRasterLimitConfig)) -> ReferenceRasterLimits {
    let mut config = ReferenceRasterLimitConfig::default();
    update(&mut config);
    ReferenceRasterLimits::validate(config).unwrap()
}

struct Cancellation {
    cancel_at: u64,
    calls: AtomicU64,
}

impl Cancellation {
    fn never() -> Self {
        Self {
            cancel_at: u64::MAX,
            calls: AtomicU64::new(0),
        }
    }

    fn at(check: u64) -> Self {
        Self {
            cancel_at: check,
            calls: AtomicU64::new(0),
        }
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ReferenceRasterCancellation for Cancellation {
    fn is_cancelled(&self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_at
    }
}

fn render(
    scene: Arc<Scene>,
    config: ReferenceRenderConfig,
    limits: ReferenceRasterLimits,
) -> Arc<pdf_rs_raster::reference::CanonicalPixelBuffer> {
    let cancellation = Cancellation::never();
    let mut job = ReferenceRenderJob::new(scene, config, limits);
    match job.poll(&cancellation) {
        ReferenceRenderPoll::Ready(buffer) => buffer,
        outcome => panic!("expected complete Reference pixels: {outcome:?}"),
    }
}

#[test]
fn exact_white_pixel_contract_and_multiline_stride_are_stable() {
    let one = render(
        empty_scene(1),
        ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
        ReferenceRasterLimits::default(),
    );
    assert_eq!(one.version(), ReferencePixelBufferVersion::V1);
    assert_eq!(
        one.config().profile(),
        ReferenceOutputProfile::OpaqueSrgbStraightRgba8V1
    );
    assert_eq!(one.config().profile().origin(), PixelOrigin::TopLeft);
    assert_eq!(one.config().profile().pixel_format(), PixelFormat::Rgba8);
    assert_eq!(one.config().profile().alpha_mode(), AlphaMode::Straight);
    assert_eq!(one.config().profile().label(), "sRGB-reference-v1");
    assert_eq!(one.width(), 1);
    assert_eq!(one.height(), 1);
    assert_eq!(one.stride_bytes(), 4);
    assert_eq!(one.rgba(), &[255, 255, 255, 255]);
    assert_eq!(one.stats().commands(), 0);
    assert_eq!(one.stats().pixels(), 1);
    assert_eq!(one.stats().fuel(), 1);
    assert!(one.stats().retained_bytes() >= 4);
    assert_eq!(one.stats().cancellation_checks(), 3);

    let multi = render(
        empty_scene(1),
        ReferenceRenderConfig::opaque_srgb(2, 2).unwrap(),
        ReferenceRasterLimits::default(),
    );
    assert_eq!(multi.stride_bytes(), 8);
    assert_eq!(multi.rgba(), &[255; 16]);
    assert_eq!(multi.stats().pixels(), 4);
    assert_eq!(multi.stats().fuel(), 4);
}

#[test]
fn marked_content_is_explicitly_non_painting_and_binding_remains_runtime_exact() {
    let config = ReferenceRenderConfig::opaque_srgb(2, 1).unwrap();
    let empty = render(empty_scene(7), config, ReferenceRasterLimits::default());
    let marked = render(marked_scene(7), config, ReferenceRasterLimits::default());
    assert_eq!(empty.rgba(), marked.rgba());
    assert_eq!(marked.stats().commands(), 2);
    assert_eq!(marked.stats().fuel(), 4);

    let other_source = render(empty_scene(91), config, ReferenceRasterLimits::default());
    assert_eq!(empty.rgba(), other_source.rgba());
    assert_ne!(empty.binding().source(), other_source.binding().source());
    assert_eq!(empty.binding(), binding(7));
    assert_eq!(other_source.binding(), binding(91));
}

#[test]
fn repeat_runs_and_successful_terminal_replay_are_exact() {
    let config = ReferenceRenderConfig::opaque_srgb(3, 2).unwrap();
    let first = render(marked_scene(4), config, ReferenceRasterLimits::default());
    let second = render(marked_scene(4), config, ReferenceRasterLimits::default());
    assert_eq!(first, second);

    let initial = Cancellation::never();
    let scene = marked_scene(4);
    let retained_scene = Arc::clone(&scene);
    let mut job = ReferenceRenderJob::new(scene, config, ReferenceRasterLimits::default());
    assert_eq!(Arc::strong_count(&retained_scene), 2);
    let first_poll = match job.poll(&initial) {
        ReferenceRenderPoll::Ready(buffer) => buffer,
        outcome => panic!("first poll must publish: {outcome:?}"),
    };
    assert_eq!(Arc::strong_count(&retained_scene), 1);
    assert_eq!(job.phase(), ReferenceRenderPhase::Ready);
    let replay_cancellation = Cancellation::at(1);
    let replay = match job.poll(&replay_cancellation) {
        ReferenceRenderPoll::Ready(buffer) => buffer,
        outcome => panic!("terminal replay must remain Ready: {outcome:?}"),
    };
    assert!(Arc::ptr_eq(&first_poll, &replay));
    assert_eq!(Arc::strong_count(&retained_scene), 1);
    assert_eq!(replay_cancellation.calls(), 0);
}

#[test]
fn cancellation_before_allocation_during_work_and_before_publication_is_terminal() {
    let config = ReferenceRenderConfig::opaque_srgb(1, 1).unwrap();
    let pre_scene = empty_scene(1);
    let retained_pre_scene = Arc::clone(&pre_scene);
    let mut pre = ReferenceRenderJob::new(pre_scene, config, ReferenceRasterLimits::default());
    assert_eq!(Arc::strong_count(&retained_pre_scene), 2);
    assert!(matches!(
        pre.poll(&Cancellation::at(1)),
        ReferenceRenderPoll::Failed(error)
            if error.code() == ReferenceRenderErrorCode::Cancelled
    ));
    assert_eq!(Arc::strong_count(&retained_pre_scene), 1);

    let mut before_allocation = ReferenceRenderJob::new(
        empty_scene(1),
        ReferenceRenderConfig::opaque_srgb(16_384, 4_096).unwrap(),
        ReferenceRasterLimits::default(),
    );
    let before_allocation_cancellation = Cancellation::at(2);
    assert!(matches!(
        before_allocation.poll(&before_allocation_cancellation),
        ReferenceRenderPoll::Failed(error)
            if error.code() == ReferenceRenderErrorCode::Cancelled
    ));
    assert_eq!(before_allocation_cancellation.calls(), 2);

    let mut interval = ReferenceRenderJob::new(
        empty_scene(1),
        ReferenceRenderConfig::opaque_srgb(257, 1).unwrap(),
        ReferenceRasterLimits::default(),
    );
    assert!(matches!(
        interval.poll(&Cancellation::at(3)),
        ReferenceRenderPoll::Failed(error)
            if error.code() == ReferenceRenderErrorCode::Cancelled
    ));

    let mut final_check =
        ReferenceRenderJob::new(empty_scene(1), config, ReferenceRasterLimits::default());
    assert!(matches!(
        final_check.poll(&Cancellation::at(3)),
        ReferenceRenderPoll::Failed(error)
            if error.code() == ReferenceRenderErrorCode::Cancelled
    ));
    assert_eq!(final_check.phase(), ReferenceRenderPhase::Failed);
    let replay_cancellation = Cancellation::never();
    assert!(matches!(
        final_check.poll(&replay_cancellation),
        ReferenceRenderPoll::Failed(error)
            if error.code() == ReferenceRenderErrorCode::Cancelled
    ));
    assert_eq!(replay_cancellation.calls(), 0);
}

#[test]
fn configuration_and_limit_profiles_fail_closed() {
    assert_eq!(
        ReferenceRenderConfig::opaque_srgb(0, 1).unwrap_err().code(),
        ReferenceRenderErrorCode::InvalidConfig
    );
    let invalid = ReferenceRasterLimitConfig {
        max_pixels: 0,
        ..ReferenceRasterLimitConfig::default()
    };
    assert_eq!(
        ReferenceRasterLimits::validate(invalid).unwrap_err().code(),
        ReferenceRenderErrorCode::InvalidLimits
    );
    let invalid = ReferenceRasterLimitConfig {
        max_output_bytes: u64::MAX,
        ..ReferenceRasterLimitConfig::default()
    };
    assert_eq!(
        ReferenceRasterLimits::validate(invalid).unwrap_err().code(),
        ReferenceRenderErrorCode::InvalidLimits
    );
    let invalid = ReferenceRasterLimitConfig {
        max_requirements: 0,
        ..ReferenceRasterLimitConfig::default()
    };
    assert_eq!(
        ReferenceRasterLimits::validate(invalid).unwrap_err().code(),
        ReferenceRenderErrorCode::InvalidLimits
    );

    let huge = ReferenceRenderConfig::opaque_srgb(u32::MAX, u32::MAX).unwrap();
    let mut job = ReferenceRenderJob::new(empty_scene(1), huge, ReferenceRasterLimits::default());
    assert!(matches!(
        job.poll(&Cancellation::never()),
        ReferenceRenderPoll::Failed(error)
            if error.limit().is_some_and(|value| value.kind() == ReferenceRenderLimitKind::Width)
    ));
}

#[test]
fn every_semantic_budget_rejects_one_less_than_required_work() {
    let config = ReferenceRenderConfig::opaque_srgb(2, 2).unwrap();
    for (limits, expected_kind) in [
        (
            limits(|value| value.max_width = 1),
            ReferenceRenderLimitKind::Width,
        ),
        (
            limits(|value| value.max_height = 1),
            ReferenceRenderLimitKind::Height,
        ),
        (
            limits(|value| value.max_pixels = 3),
            ReferenceRenderLimitKind::Pixels,
        ),
        (
            limits(|value| value.max_stride_bytes = 7),
            ReferenceRenderLimitKind::StrideBytes,
        ),
        (
            limits(|value| value.max_output_bytes = 15),
            ReferenceRenderLimitKind::OutputBytes,
        ),
        (
            limits(|value| value.max_fuel = 3),
            ReferenceRenderLimitKind::Fuel,
        ),
        (
            limits(|value| value.max_retained_bytes = 15),
            ReferenceRenderLimitKind::RetainedBytes,
        ),
    ] {
        let mut job = ReferenceRenderJob::new(empty_scene(2), config, limits);
        match job.poll(&Cancellation::never()) {
            ReferenceRenderPoll::Failed(error) => {
                assert_eq!(error.code(), ReferenceRenderErrorCode::ResourceLimit);
                assert_eq!(error.limit().unwrap().kind(), expected_kind);
            }
            outcome => panic!("one-less budget must fail: {outcome:?}"),
        }
    }

    let mut commands = ReferenceRenderJob::new(
        marked_scene(2),
        config,
        limits(|value| value.max_commands = 1),
    );
    match commands.poll(&Cancellation::never()) {
        ReferenceRenderPoll::Failed(error) => {
            assert_eq!(
                error.limit().unwrap().kind(),
                ReferenceRenderLimitKind::Commands
            );
        }
        outcome => panic!("one-less command budget must fail: {outcome:?}"),
    }
}

#[test]
fn exact_measured_profile_is_admitted_and_one_less_retention_is_rejected() {
    let config = ReferenceRenderConfig::opaque_srgb(3, 1).unwrap();
    let measured = render(empty_scene(5), config, ReferenceRasterLimits::default())
        .stats()
        .retained_bytes();
    let exact = render(
        empty_scene(5),
        config,
        limits(|value| {
            value.max_width = 3;
            value.max_height = 1;
            value.max_pixels = 3;
            value.max_stride_bytes = 12;
            value.max_output_bytes = 12;
            value.max_commands = 1;
            value.max_fuel = 3;
            value.max_retained_bytes = measured;
        }),
    );
    assert_eq!(exact.stats().retained_bytes(), measured);
    assert_eq!(exact.stats().requirements(), 0);
    assert_eq!(exact.stats().pixels(), 3);
    assert_eq!(exact.stats().fuel(), 3);

    let marked_measured = render(marked_scene(5), config, ReferenceRasterLimits::default())
        .stats()
        .retained_bytes();
    let marked_exact = render(
        marked_scene(5),
        config,
        limits(|value| {
            value.max_width = 3;
            value.max_height = 1;
            value.max_pixels = 3;
            value.max_stride_bytes = 12;
            value.max_output_bytes = 12;
            value.max_commands = 2;
            value.max_fuel = 5;
            value.max_retained_bytes = marked_measured;
        }),
    );
    assert_eq!(marked_exact.stats().commands(), 2);
    assert_eq!(marked_exact.stats().requirements(), 0);
    assert_eq!(marked_exact.stats().fuel(), 5);

    let mut one_less = ReferenceRenderJob::new(
        empty_scene(5),
        config,
        limits(|value| value.max_retained_bytes = measured - 1),
    );
    assert!(matches!(
        one_less.poll(&Cancellation::never()),
        ReferenceRenderPoll::Failed(error)
            if error.limit().is_some_and(|value| value.kind() == ReferenceRenderLimitKind::RetainedBytes)
    ));
}

#[test]
fn public_debug_output_redacts_pixel_bytes() {
    let buffer = render(
        empty_scene(8),
        ReferenceRenderConfig::opaque_srgb(1, 1).unwrap(),
        ReferenceRasterLimits::default(),
    );
    let debug = format!("{buffer:?}");
    assert!(debug.contains("rgba_bytes: 4"));
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("255, 255, 255, 255"));

    let mut job = ReferenceRenderJob::new(
        empty_scene(8),
        ReferenceRenderConfig::opaque_srgb(2, 2).unwrap(),
        limits(|value| value.max_pixels = 3),
    );
    let error = match job.poll(&Cancellation::never()) {
        ReferenceRenderPoll::Failed(error) => error,
        outcome => panic!("tight pixel budget must fail: {outcome:?}"),
    };
    assert!(!format!("{error:?}").contains("255"));
    assert_eq!(error.diagnostic_id(), "RPE-RASTER-0005");
}
