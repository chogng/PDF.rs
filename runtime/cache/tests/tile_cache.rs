use std::sync::atomic::{AtomicUsize, Ordering};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_cache::{
    NativeTile, NeverCancelledTileCache, TileAdmission, TileCache, TileCacheAddress,
    TileCacheBinding, TileCacheCancellation, TileCacheErrorCode, TileCacheLimitConfig,
    TileCacheLimitKind, TileCacheLimits, TileCacheLookup, TileCacheMissReason, TileCacheOwnerId,
    TileCacheSessionId, TileOutcomeKind, TileRejectReason, TileRenderOutcome, TileRetentionClass,
};
use pdf_rs_policy::{
    AntialiasMode, CapabilityEvaluator, CapabilityProfile, DeviceRect, NativeBackend,
    OptionalContentIdentity, PolicyCancellation, PolicyLimits, QualityPolicy, RenderConfig,
    RenderConfigInput, RenderPlan, RenderPlanOutcome, RenderPlanRequest, RendererEpoch, ZoomRatio,
    create_render_plan,
};
use pdf_rs_scene::{
    CapabilityContext, CapabilityStatus, GraphicsCapability, GraphicsSceneBuilder,
    GraphicsSceneLimits, PageGeometry, PageRotation, SceneBinding, SceneRect, SceneScalar,
};
use pdf_rs_syntax::ObjectRef;

const OWNER: TileCacheOwnerId = TileCacheOwnerId::new(0x0a11_ce01);
const SESSION: TileCacheSessionId = TileCacheSessionId::new(0x5e55_10a1);

struct NeverPolicy;

impl PolicyCancellation for NeverPolicy {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct CancelAt {
    calls: AtomicUsize,
    cancel_at: usize,
}

impl CancelAt {
    const fn new(cancel_at: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            cancel_at,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TileCacheCancellation for CancelAt {
    fn is_cancelled(&self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_at
    }
}

#[derive(Clone, Copy)]
struct PlanSpec {
    stable_seed: u8,
    source_revision: u64,
    document_revision: u64,
    revision_startxref: u64,
    page_index: u32,
    page_object_number: u32,
    page_object_generation: u16,
    geometry_delta: i64,
    geometry_rotation: PageRotation,
    requirement_parameter: Option<u64>,
    clip_x: i32,
    clip_y: i32,
    clip_width: u32,
    clip_height: u32,
    zoom_numerator: u32,
    zoom_denominator: u32,
    device_scale_milli: u32,
    viewport_rotation: PageRotation,
    optional_content: u64,
    annotation_revision: u64,
    backend: NativeBackend,
    quality: QualityPolicy,
    antialias: AntialiasMode,
    tile_width: u32,
    tile_height: u32,
    renderer_epoch: u32,
}

impl Default for PlanSpec {
    fn default() -> Self {
        Self {
            stable_seed: 0x41,
            source_revision: 7,
            document_revision: 11,
            revision_startxref: 19,
            page_index: 3,
            page_object_number: 41,
            page_object_generation: 0,
            geometry_delta: 0,
            geometry_rotation: PageRotation::Degrees0,
            requirement_parameter: None,
            clip_x: 0,
            clip_y: 0,
            clip_width: 4,
            clip_height: 2,
            zoom_numerator: 3,
            zoom_denominator: 2,
            device_scale_milli: 2_000,
            viewport_rotation: PageRotation::Degrees0,
            optional_content: 5,
            annotation_revision: 9,
            backend: NativeBackend::FastCpu,
            quality: QualityPolicy::Full,
            antialias: AntialiasMode::Coverage4x4,
            tile_width: 2,
            tile_height: 2,
            renderer_epoch: 1,
        }
    }
}

fn plan(spec: PlanSpec) -> RenderPlan {
    let source = SourceIdentity::new(
        SourceStableId::new([spec.stable_seed; 32]),
        SourceRevision::new(spec.source_revision),
    );
    let binding = SceneBinding::new(
        source,
        spec.revision_startxref,
        spec.page_index,
        ObjectRef::new(spec.page_object_number, spec.page_object_generation).unwrap(),
    );
    let media = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::from_scaled(612_000_000_000 + spec.geometry_delta),
        SceneScalar::from_scaled(792_000_000_000 + spec.geometry_delta),
    ])
    .unwrap();
    let geometry = PageGeometry::new(media, media, spec.geometry_rotation);
    let mut builder =
        GraphicsSceneBuilder::new_v2(binding, geometry, GraphicsSceneLimits::default());
    if let Some(parameter) = spec.requirement_parameter {
        builder
            .add_requirement(
                GraphicsCapability::PathFill,
                parameter,
                CapabilityContext::Scene,
                Vec::new(),
                CapabilityStatus::Supported,
            )
            .unwrap();
    }
    let scene = builder.finish().unwrap();
    let decision = CapabilityEvaluator::new(
        CapabilityProfile::m3_reference_v1(),
        PolicyLimits::default(),
    )
    .evaluate(&scene, spec.document_revision, &NeverPolicy)
    .unwrap();
    let mut config_input = match spec.backend {
        NativeBackend::ReferenceCpu => RenderConfigInput::reference_cpu_full(),
        NativeBackend::FastCpu => RenderConfigInput::fast_cpu_full(),
    };
    config_input.quality = spec.quality;
    config_input.antialias = spec.antialias;
    config_input.tile_width = spec.tile_width;
    config_input.tile_height = spec.tile_height;
    config_input.tile_halo = 0;
    let config = RenderConfig::validate(config_input).unwrap();
    let request = RenderPlanRequest::new(
        1,
        DeviceRect::new(spec.clip_x, spec.clip_y, spec.clip_width, spec.clip_height).unwrap(),
        ZoomRatio::new(spec.zoom_numerator, spec.zoom_denominator).unwrap(),
        spec.device_scale_milli,
        spec.viewport_rotation,
        OptionalContentIdentity::new(spec.optional_content),
        spec.annotation_revision,
    )
    .unwrap();
    match create_render_plan(
        &scene,
        decision,
        config,
        request,
        RendererEpoch::new(spec.renderer_epoch).unwrap(),
        PolicyLimits::default(),
        &NeverPolicy,
    )
    .unwrap()
    {
        RenderPlanOutcome::Ready(plan) => plan,
        RenderPlanOutcome::NotPublishable(decision) => {
            panic!("test plan must be publishable: {:?}", decision.status())
        }
    }
}

fn address(plan: &RenderPlan, tile_index: usize) -> TileCacheAddress {
    TileCacheAddress::new(
        OWNER,
        SESSION,
        plan.tiles()[tile_index].content_key().clone(),
    )
}

fn binding(plan: &RenderPlan) -> TileCacheBinding {
    TileCacheBinding::from_content_key(OWNER, SESSION, plan.tiles()[0].content_key())
}

fn required_pixel_bytes(address: &TileCacheAddress) -> usize {
    let tile = address.content_key().tile();
    usize::try_from(
        u64::from(tile.width())
            .checked_mul(u64::from(tile.height()))
            .and_then(|pixels| pixels.checked_mul(4))
            .unwrap(),
    )
    .unwrap()
}

fn native_tile(address: &TileCacheAddress, fill: u8) -> NativeTile {
    NativeTile::try_new(
        address.content_key().clone(),
        vec![fill; required_pixel_bytes(address)],
    )
    .unwrap()
}

fn native_tile_with_capacity(address: &TileCacheAddress, fill: u8, capacity: usize) -> NativeTile {
    let required = required_pixel_bytes(address);
    assert!(capacity >= required);
    let mut pixels = Vec::with_capacity(capacity);
    pixels.resize(required, fill);
    let tile = NativeTile::try_new(address.content_key().clone(), pixels).unwrap();
    assert!(tile.pixel_capacity_bytes() >= u64::try_from(capacity).unwrap());
    tile
}

fn cache_limits(
    max_entries: u64,
    max_tile_pixel_bytes: u64,
    max_pixel_bytes: u64,
    max_resident_bytes: u64,
) -> TileCacheLimits {
    TileCacheLimits::validate(TileCacheLimitConfig {
        max_entries,
        max_tile_pixel_bytes,
        max_pixel_bytes,
        max_resident_bytes,
    })
    .unwrap()
}

fn admit(
    cache: &mut TileCache,
    address: &TileCacheAddress,
    tile: NativeTile,
    retention: TileRetentionClass,
) -> pdf_rs_cache::TileAdmitted {
    match cache
        .try_admit(
            address,
            TileRenderOutcome::Complete(tile),
            retention,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Admitted(admitted) => admitted,
        TileAdmission::Rejected(rejected) => {
            panic!("complete matching tile must enter: {:?}", rejected.reason())
        }
    }
}

fn lookup_miss(cache: &mut TileCache, address: &TileCacheAddress) -> TileCacheMissReason {
    match cache.lookup(address, &NeverCancelledTileCache).unwrap() {
        TileCacheLookup::Hit(_) => panic!("mutated address must not hit"),
        TileCacheLookup::Miss(reason) => reason,
    }
}

#[test]
fn exact_complete_key_hits_and_borrowed_pixels_remain_immutable() {
    let plan = plan(PlanSpec::default());
    assert_eq!(plan.tiles().len(), 2);
    let exact = address(&plan, 0);
    let other_tile = address(&plan, 1);
    let cache_binding = binding(&plan);
    assert_eq!(cache_binding.owner_id(), OWNER);
    assert_eq!(cache_binding.session_id(), SESSION);
    assert_eq!(cache_binding.source(), exact.content_key().source());
    assert_eq!(
        cache_binding.document_revision(),
        exact.content_key().document_revision()
    );
    assert_eq!(
        cache_binding.revision_startxref(),
        exact.content_key().revision_startxref()
    );
    assert_eq!(
        cache_binding.renderer_epoch(),
        exact.content_key().renderer_epoch()
    );
    assert_eq!(OWNER.value(), 0x0a11_ce01);
    assert_eq!(SESSION.value(), 0x5e55_10a1);

    let mut cache = TileCache::new(cache_binding, TileCacheLimits::default()).unwrap();
    let admitted = admit(
        &mut cache,
        &exact,
        native_tile(&exact, 0x9a),
        TileRetentionClass::ProtectedViewport,
    );
    assert!(!admitted.replaced());
    assert_eq!(admitted.evicted(), 0);

    match cache.lookup(&exact, &NeverCancelledTileCache).unwrap() {
        TileCacheLookup::Hit(tile) => {
            assert_eq!(tile.content_key(), exact.content_key());
            assert_eq!(tile.stride(), 8);
            assert_eq!(tile.pixel_bytes(), 16);
            assert_eq!(tile.pixels(), &[0x9a; 16]);
        }
        TileCacheLookup::Miss(reason) => panic!("exact complete key must hit: {reason:?}"),
    }
    assert_eq!(
        lookup_miss(&mut cache, &other_tile),
        TileCacheMissReason::NotFound
    );
    let stats = cache.stats();
    assert_eq!(stats.entries(), 1);
    assert_eq!(stats.protected_entries(), 1);
    assert_eq!(stats.recent_entries(), 0);
    assert_eq!(stats.hits(), 1);
    assert_eq!(stats.misses(), 1);
    assert_eq!(
        stats.resident_bytes(),
        stats.metadata_bytes() + stats.pixel_capacity_bytes()
    );
}

#[test]
fn every_owner_revision_epoch_and_policy_content_mutation_misses() {
    let baseline_spec = PlanSpec::default();
    let baseline = plan(baseline_spec);
    let exact = address(&baseline, 0);
    let mut cache = TileCache::new(binding(&baseline), TileCacheLimits::default()).unwrap();
    admit(
        &mut cache,
        &exact,
        native_tile(&exact, 1),
        TileRetentionClass::RecentUse,
    );

    let foreign_owner = TileCacheAddress::new(
        TileCacheOwnerId::new(OWNER.value() + 1),
        SESSION,
        exact.content_key().clone(),
    );
    assert_eq!(
        lookup_miss(&mut cache, &foreign_owner),
        TileCacheMissReason::ForeignOwner
    );
    let foreign_session = TileCacheAddress::new(
        OWNER,
        TileCacheSessionId::new(SESSION.value() + 1),
        exact.content_key().clone(),
    );
    assert_eq!(
        lookup_miss(&mut cache, &foreign_session),
        TileCacheMissReason::ForeignSession
    );

    let mut mutations: Vec<(&str, PlanSpec, TileCacheMissReason)> = Vec::new();
    macro_rules! mutation {
        ($name:literal, $field:ident, $value:expr, $reason:expr) => {{
            let mut spec = baseline_spec;
            spec.$field = $value;
            mutations.push(($name, spec, $reason));
        }};
    }
    mutation!(
        "source stable identity",
        stable_seed,
        baseline_spec.stable_seed + 1,
        TileCacheMissReason::SourceMismatch
    );
    mutation!(
        "source revision",
        source_revision,
        baseline_spec.source_revision + 1,
        TileCacheMissReason::SourceMismatch
    );
    mutation!(
        "document revision",
        document_revision,
        baseline_spec.document_revision + 1,
        TileCacheMissReason::StaleRevision
    );
    mutation!(
        "revision startxref",
        revision_startxref,
        baseline_spec.revision_startxref + 1,
        TileCacheMissReason::StaleRevision
    );
    mutation!(
        "page index",
        page_index,
        baseline_spec.page_index + 1,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "page object number",
        page_object_number,
        baseline_spec.page_object_number + 1,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "page object generation",
        page_object_generation,
        baseline_spec.page_object_generation + 1,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "Scene and capability decision identity",
        requirement_parameter,
        Some(0),
        TileCacheMissReason::NotFound
    );
    mutation!(
        "geometry identity",
        geometry_delta,
        1,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "geometry rotation",
        geometry_rotation,
        PageRotation::Degrees90,
        TileCacheMissReason::NotFound
    );
    mutation!("viewport clip x", clip_x, 1, TileCacheMissReason::NotFound);
    mutation!("viewport clip y", clip_y, 1, TileCacheMissReason::NotFound);
    mutation!(
        "viewport clip width",
        clip_width,
        3,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "viewport clip height",
        clip_height,
        1,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "zoom bucket",
        zoom_numerator,
        5,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "device scale",
        device_scale_milli,
        1_500,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "viewport rotation",
        viewport_rotation,
        PageRotation::Degrees90,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "optional content",
        optional_content,
        baseline_spec.optional_content + 1,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "annotation revision",
        annotation_revision,
        baseline_spec.annotation_revision + 1,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "quality",
        quality,
        QualityPolicy::Preview,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "Native backend",
        backend,
        NativeBackend::ReferenceCpu,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "render configuration",
        antialias,
        AntialiasMode::SingleSample,
        TileCacheMissReason::NotFound
    );
    mutation!(
        "renderer epoch",
        renderer_epoch,
        baseline_spec.renderer_epoch + 1,
        TileCacheMissReason::StaleRendererEpoch
    );

    for (name, spec, expected_reason) in mutations {
        let changed = plan(spec);
        let changed_address = address(&changed, 0);
        assert_ne!(
            exact.content_key(),
            changed_address.content_key(),
            "{name} must change the complete policy key"
        );
        assert_eq!(
            lookup_miss(&mut cache, &changed_address),
            expected_reason,
            "{name} must never reuse baseline pixels"
        );
    }

    let tile_coordinate = address(&baseline, 1);
    assert_ne!(
        exact.content_key().tile(),
        tile_coordinate.content_key().tile()
    );
    assert_eq!(
        lookup_miss(&mut cache, &tile_coordinate),
        TileCacheMissReason::NotFound
    );
    assert_eq!(cache.stats().entries(), 1);
}

#[test]
fn admission_rejects_foreign_stale_mismatched_and_non_success_outcomes() {
    let baseline = plan(PlanSpec::default());
    let exact = address(&baseline, 0);
    let mut cache = TileCache::new(binding(&baseline), TileCacheLimits::default()).unwrap();

    let foreign_owner = TileCacheAddress::new(
        TileCacheOwnerId::new(OWNER.value() + 1),
        SESSION,
        exact.content_key().clone(),
    );
    let rejected = match cache
        .try_admit(
            &foreign_owner,
            TileRenderOutcome::Complete(native_tile(&exact, 1)),
            TileRetentionClass::RecentUse,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Rejected(rejected) => rejected,
        TileAdmission::Admitted(_) => panic!("foreign owner must be rejected"),
    };
    assert_eq!(rejected.reason(), TileRejectReason::ForeignOwner);
    assert_eq!(rejected.into_outcome().kind(), TileOutcomeKind::Complete);

    let foreign_session = TileCacheAddress::new(
        OWNER,
        TileCacheSessionId::new(SESSION.value() + 1),
        exact.content_key().clone(),
    );
    let rejected = match cache
        .try_admit(
            &foreign_session,
            TileRenderOutcome::Complete(native_tile(&exact, 2)),
            TileRetentionClass::RecentUse,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Rejected(rejected) => rejected,
        TileAdmission::Admitted(_) => panic!("foreign session must be rejected"),
    };
    assert_eq!(rejected.reason(), TileRejectReason::ForeignSession);

    let changed_source = plan(PlanSpec {
        stable_seed: 0x42,
        ..PlanSpec::default()
    });
    let changed_source_address = address(&changed_source, 0);
    let rejected = match cache
        .try_admit(
            &changed_source_address,
            TileRenderOutcome::Complete(native_tile(&changed_source_address, 3)),
            TileRetentionClass::RecentUse,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Rejected(rejected) => rejected,
        TileAdmission::Admitted(_) => panic!("foreign source must be rejected"),
    };
    assert_eq!(rejected.reason(), TileRejectReason::SourceMismatch);

    let stale_revision = plan(PlanSpec {
        document_revision: 12,
        ..PlanSpec::default()
    });
    let stale_revision_address = address(&stale_revision, 0);
    let rejected = match cache
        .try_admit(
            &stale_revision_address,
            TileRenderOutcome::Complete(native_tile(&stale_revision_address, 4)),
            TileRetentionClass::RecentUse,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Rejected(rejected) => rejected,
        TileAdmission::Admitted(_) => panic!("stale revision must be rejected"),
    };
    assert_eq!(rejected.reason(), TileRejectReason::StaleRevision);

    let stale_epoch = plan(PlanSpec {
        renderer_epoch: 2,
        ..PlanSpec::default()
    });
    let stale_epoch_address = address(&stale_epoch, 0);
    let rejected = match cache
        .try_admit(
            &stale_epoch_address,
            TileRenderOutcome::Complete(native_tile(&stale_epoch_address, 5)),
            TileRetentionClass::RecentUse,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Rejected(rejected) => rejected,
        TileAdmission::Admitted(_) => panic!("stale renderer epoch must be rejected"),
    };
    assert_eq!(rejected.reason(), TileRejectReason::StaleRendererEpoch);

    let other_tile = address(&baseline, 1);
    let rejected = match cache
        .try_admit(
            &exact,
            TileRenderOutcome::Complete(native_tile(&other_tile, 6)),
            TileRetentionClass::RecentUse,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Rejected(rejected) => rejected,
        TileAdmission::Admitted(_) => panic!("mismatched complete key must be rejected"),
    };
    assert_eq!(rejected.reason(), TileRejectReason::ContentKeyMismatch);

    for (outcome, expected) in [
        (TileRenderOutcome::Incomplete, TileRejectReason::Incomplete),
        (
            TileRenderOutcome::Unsupported,
            TileRejectReason::Unsupported,
        ),
        (TileRenderOutcome::Cancelled, TileRejectReason::Cancelled),
        (TileRenderOutcome::Failed, TileRejectReason::Failed),
        (
            TileRenderOutcome::SourceChanged,
            TileRejectReason::SourceChanged,
        ),
    ] {
        let kind = outcome.kind();
        let rejected = match cache
            .try_admit(
                &exact,
                outcome,
                TileRetentionClass::ProtectedViewport,
                &NeverCancelledTileCache,
            )
            .unwrap()
        {
            TileAdmission::Rejected(rejected) => rejected,
            TileAdmission::Admitted(_) => panic!("{kind:?} must never enter the cache"),
        };
        assert_eq!(rejected.reason(), expected);
        assert_eq!(rejected.into_outcome().kind(), kind);
    }
    assert_eq!(cache.stats().entries(), 0);
    assert_eq!(cache.stats().rejections(), 11);
}

#[test]
fn pixel_extent_metadata_and_capacity_boundaries_are_charged_exactly() {
    let plan = plan(PlanSpec::default());
    let exact = address(&plan, 0);
    let required = required_pixel_bytes(&exact);
    assert_eq!(required, 16);
    for invalid_len in [required - 1, required + 1] {
        let error =
            NativeTile::try_new(exact.content_key().clone(), vec![0; invalid_len]).unwrap_err();
        assert_eq!(error.code(), TileCacheErrorCode::InvalidTile);
    }
    let oversized_capacity = native_tile_with_capacity(&exact, 7, 32);
    assert_eq!(oversized_capacity.pixel_bytes(), 16);
    assert!(oversized_capacity.pixel_capacity_bytes() >= 32);

    let broad_limits = cache_limits(1, 1024, 1024, 64 * 1024);
    let probe = TileCache::new(binding(&plan), broad_limits).unwrap();
    let metadata_bytes = probe.stats().metadata_bytes();
    assert!(metadata_bytes > 0);
    drop(probe);

    let metadata_error =
        TileCache::new(binding(&plan), cache_limits(1, 1, 1, metadata_bytes - 1)).unwrap_err();
    assert_eq!(metadata_error.code(), TileCacheErrorCode::ResourceLimit);
    let metadata_limit = metadata_error.limit().unwrap();
    assert_eq!(metadata_limit.kind(), TileCacheLimitKind::MetadataBytes);
    assert_eq!(metadata_limit.limit(), metadata_bytes - 1);
    assert_eq!(metadata_limit.attempted(), metadata_bytes);
    assert_eq!(metadata_limit.scope().owner_id(), OWNER);
    assert_eq!(metadata_limit.scope().session_id(), SESSION);

    let exact_pixels = u64::try_from(required).unwrap();
    let exact_limits = cache_limits(1, exact_pixels, exact_pixels, metadata_bytes + exact_pixels);
    let mut exact_cache = TileCache::new(binding(&plan), exact_limits).unwrap();
    admit(
        &mut exact_cache,
        &exact,
        native_tile(&exact, 8),
        TileRetentionClass::RecentUse,
    );
    assert_eq!(exact_cache.stats().pixel_capacity_bytes(), exact_pixels);
    assert_eq!(
        exact_cache.stats().resident_bytes(),
        metadata_bytes + exact_pixels
    );

    let mut tile_one_less = TileCache::new(
        binding(&plan),
        cache_limits(
            1,
            exact_pixels - 1,
            exact_pixels,
            metadata_bytes + exact_pixels,
        ),
    )
    .unwrap();
    let rejected = match tile_one_less
        .try_admit(
            &exact,
            TileRenderOutcome::Complete(native_tile(&exact, 9)),
            TileRetentionClass::RecentUse,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Rejected(rejected) => rejected,
        TileAdmission::Admitted(_) => panic!("one-less per-tile limit must reject"),
    };
    assert_eq!(rejected.reason(), TileRejectReason::TileTooLarge);
    let limit = rejected.limit().unwrap();
    assert_eq!(limit.kind(), TileCacheLimitKind::TilePixelBytes);
    assert_eq!(limit.limit(), exact_pixels - 1);
    assert_eq!(limit.attempted(), exact_pixels);

    let mut resident_one_less = TileCache::new(
        binding(&plan),
        cache_limits(
            1,
            exact_pixels,
            exact_pixels,
            metadata_bytes + exact_pixels - 1,
        ),
    )
    .unwrap();
    let rejected = match resident_one_less
        .try_admit(
            &exact,
            TileRenderOutcome::Complete(native_tile(&exact, 10)),
            TileRetentionClass::RecentUse,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Rejected(rejected) => rejected,
        TileAdmission::Admitted(_) => panic!("one-less resident limit must reject"),
    };
    assert_eq!(rejected.reason(), TileRejectReason::ResidentLimit);
    let limit = rejected.limit().unwrap();
    assert_eq!(limit.kind(), TileCacheLimitKind::ResidentBytes);
    assert_eq!(limit.limit(), metadata_bytes + exact_pixels - 1);
    assert_eq!(limit.consumed(), metadata_bytes);
    assert_eq!(limit.attempted(), exact_pixels);
    assert_eq!(rejected.into_outcome().kind(), TileOutcomeKind::Complete);
}

#[test]
fn eviction_prefers_recent_use_then_protected_and_replacement_changes_segment() {
    let plan = plan(PlanSpec {
        clip_width: 8,
        ..PlanSpec::default()
    });
    assert_eq!(plan.tiles().len(), 4);
    let keys: Vec<_> = (0..4).map(|index| address(&plan, index)).collect();
    let mut cache = TileCache::new(binding(&plan), cache_limits(2, 1024, 1024, 64 * 1024)).unwrap();

    admit(
        &mut cache,
        &keys[0],
        native_tile(&keys[0], 0),
        TileRetentionClass::ProtectedViewport,
    );
    admit(
        &mut cache,
        &keys[1],
        native_tile(&keys[1], 1),
        TileRetentionClass::RecentUse,
    );
    let admitted = admit(
        &mut cache,
        &keys[2],
        native_tile(&keys[2], 2),
        TileRetentionClass::RecentUse,
    );
    assert_eq!(admitted.evicted_recent(), 1);
    assert_eq!(admitted.evicted_protected(), 0);
    assert_eq!(
        lookup_miss(&mut cache, &keys[1]),
        TileCacheMissReason::NotFound
    );
    assert!(matches!(
        cache.lookup(&keys[0], &NeverCancelledTileCache).unwrap(),
        TileCacheLookup::Hit(_)
    ));

    let replacement = admit(
        &mut cache,
        &keys[2],
        native_tile(&keys[2], 0x22),
        TileRetentionClass::ProtectedViewport,
    );
    assert!(replacement.replaced());
    assert_eq!(replacement.evicted(), 0);
    assert_eq!(cache.stats().protected_entries(), 2);
    assert_eq!(cache.stats().recent_entries(), 0);

    let admitted = admit(
        &mut cache,
        &keys[3],
        native_tile(&keys[3], 3),
        TileRetentionClass::RecentUse,
    );
    assert_eq!(admitted.evicted_recent(), 0);
    assert_eq!(admitted.evicted_protected(), 1);
    assert_eq!(
        lookup_miss(&mut cache, &keys[0]),
        TileCacheMissReason::NotFound
    );
    for resident in [&keys[2], &keys[3]] {
        assert!(matches!(
            cache.lookup(resident, &NeverCancelledTileCache).unwrap(),
            TileCacheLookup::Hit(_)
        ));
    }
    let stats = cache.stats();
    assert_eq!(stats.entries(), 2);
    assert_eq!(stats.replacements(), 1);
    assert_eq!(stats.evictions(), 2);
}

#[test]
fn a_new_protected_viewport_demotes_the_previous_viewport_atomically() {
    let old_plan = plan(PlanSpec::default());
    let old = address(&old_plan, 0);
    let old_peer = address(&old_plan, 1);
    let new_plan = plan(PlanSpec {
        zoom_numerator: 5,
        ..PlanSpec::default()
    });
    let current = address(&new_plan, 0);
    let mut cache =
        TileCache::new(binding(&old_plan), cache_limits(2, 1024, 1024, 64 * 1024)).unwrap();
    admit(
        &mut cache,
        &old,
        native_tile(&old, 1),
        TileRetentionClass::ProtectedViewport,
    );
    admit(
        &mut cache,
        &current,
        native_tile(&current, 2),
        TileRetentionClass::ProtectedViewport,
    );
    assert_eq!(cache.stats().protected_entries(), 1);
    assert_eq!(cache.stats().recent_entries(), 1);

    let admitted = admit(
        &mut cache,
        &old_peer,
        native_tile(&old_peer, 3),
        TileRetentionClass::RecentUse,
    );
    assert_eq!(admitted.evicted_recent(), 1);
    assert_eq!(admitted.evicted_protected(), 0);
    assert_eq!(lookup_miss(&mut cache, &old), TileCacheMissReason::NotFound);
    for resident in [&current, &old_peer] {
        assert!(matches!(
            cache.lookup(resident, &NeverCancelledTileCache).unwrap(),
            TileCacheLookup::Hit(_)
        ));
    }
}

#[test]
fn pixel_pressure_evicts_multiple_recent_tiles_in_one_deterministic_plan() {
    let plan = plan(PlanSpec {
        clip_width: 8,
        ..PlanSpec::default()
    });
    let keys: Vec<_> = (0..4).map(|index| address(&plan, index)).collect();
    let probe = TileCache::new(binding(&plan), cache_limits(4, 64, 64, 64 * 1024)).unwrap();
    let metadata = probe.stats().metadata_bytes();
    drop(probe);
    let mut cache = TileCache::new(binding(&plan), cache_limits(4, 64, 64, metadata + 64)).unwrap();
    for (fill, key) in keys.iter().take(3).enumerate() {
        let tile = native_tile_with_capacity(key, u8::try_from(fill).unwrap(), 16);
        admit(&mut cache, key, tile, TileRetentionClass::RecentUse);
    }
    assert_eq!(cache.stats().pixel_capacity_bytes(), 48);

    let incoming = native_tile_with_capacity(&keys[3], 3, 40);
    let admitted = admit(
        &mut cache,
        &keys[3],
        incoming,
        TileRetentionClass::RecentUse,
    );
    assert_eq!(admitted.evicted_recent(), 2);
    assert_eq!(admitted.evicted_protected(), 0);
    assert_eq!(cache.stats().entries(), 2);
    assert_eq!(cache.stats().pixel_capacity_bytes(), 56);
    for victim in [&keys[0], &keys[1]] {
        assert_eq!(
            lookup_miss(&mut cache, victim),
            TileCacheMissReason::NotFound
        );
    }
    for resident in [&keys[2], &keys[3]] {
        assert!(matches!(
            cache.lookup(resident, &NeverCancelledTileCache).unwrap(),
            TileCacheLookup::Hit(_)
        ));
    }
}

#[test]
fn cancellation_during_long_lookup_and_admission_scans_is_atomic() {
    const RESIDENT: usize = 128;
    let plan = plan(PlanSpec {
        clip_width: 130,
        clip_height: 1,
        tile_width: 1,
        tile_height: 1,
        ..PlanSpec::default()
    });
    assert_eq!(plan.tiles().len(), 130);
    let mut cache = TileCache::new(
        binding(&plan),
        cache_limits(u64::try_from(RESIDENT).unwrap(), 64, 1024, 1024 * 1024),
    )
    .unwrap();
    for index in 0..RESIDENT {
        let key = address(&plan, index);
        admit(
            &mut cache,
            &key,
            native_tile(&key, u8::try_from(index % 255).unwrap()),
            TileRetentionClass::RecentUse,
        );
    }
    let before = cache.stats();
    let missing = address(&plan, RESIDENT);
    let lookup_cancellation = CancelAt::new(3);
    let error = cache.lookup(&missing, &lookup_cancellation).unwrap_err();
    assert_eq!(error.code(), TileCacheErrorCode::Cancelled);
    assert!(lookup_cancellation.calls() >= 3);
    assert_eq!(cache.stats(), before);

    let incoming = address(&plan, RESIDENT + 1);
    let admission_cancellation = CancelAt::new(3);
    let failure = cache
        .try_admit(
            &incoming,
            TileRenderOutcome::Complete(native_tile(&incoming, 0xee)),
            TileRetentionClass::ProtectedViewport,
            &admission_cancellation,
        )
        .unwrap_err();
    assert_eq!(failure.error().code(), TileCacheErrorCode::Cancelled);
    assert_eq!(failure.into_outcome().kind(), TileOutcomeKind::Complete);
    assert!(admission_cancellation.calls() >= 3);
    assert_eq!(cache.stats(), before);
}

#[test]
fn close_is_idempotent_and_new_renderer_epoch_cannot_reuse_old_pixels() {
    let epoch_one = plan(PlanSpec::default());
    let old = address(&epoch_one, 0);
    let mut cache = TileCache::new(binding(&epoch_one), TileCacheLimits::default()).unwrap();
    admit(
        &mut cache,
        &old,
        native_tile(&old, 0x71),
        TileRetentionClass::ProtectedViewport,
    );
    let before = cache.stats();
    let first = cache.close();
    assert!(!first.already_closed());
    assert_eq!(first.released_entries(), 1);
    assert_eq!(first.released_metadata_bytes(), before.metadata_bytes());
    assert_eq!(
        first.released_pixel_capacity_bytes(),
        before.pixel_capacity_bytes()
    );
    assert_eq!(first.current_entries(), 0);
    assert_eq!(first.current_metadata_bytes(), 0);
    assert_eq!(first.current_pixel_capacity_bytes(), 0);
    let closed = cache.stats();
    assert!(closed.is_closed());
    assert_eq!(closed.entries(), 0);
    assert_eq!(closed.metadata_bytes(), 0);
    assert_eq!(closed.pixel_capacity_bytes(), 0);
    assert_eq!(closed.resident_bytes(), 0);

    let second = cache.close();
    assert!(second.already_closed());
    assert_eq!(second.released_entries(), 0);
    assert_eq!(second.released_metadata_bytes(), 0);
    assert_eq!(second.released_pixel_capacity_bytes(), 0);
    assert_eq!(second.current_entries(), 0);
    assert_eq!(second.current_metadata_bytes(), 0);
    assert_eq!(second.current_pixel_capacity_bytes(), 0);
    assert_eq!(lookup_miss(&mut cache, &old), TileCacheMissReason::Closed);
    let rejected = match cache
        .try_admit(
            &old,
            TileRenderOutcome::Complete(native_tile(&old, 0x72)),
            TileRetentionClass::RecentUse,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Rejected(rejected) => rejected,
        TileAdmission::Admitted(_) => panic!("closed cache must not admit"),
    };
    assert_eq!(rejected.reason(), TileRejectReason::Closed);

    let epoch_two = plan(PlanSpec {
        renderer_epoch: 2,
        ..PlanSpec::default()
    });
    let current = address(&epoch_two, 0);
    let mut restarted = TileCache::new(binding(&epoch_two), TileCacheLimits::default()).unwrap();
    assert_eq!(
        lookup_miss(&mut restarted, &old),
        TileCacheMissReason::StaleRendererEpoch
    );
    let rejected = match restarted
        .try_admit(
            &old,
            TileRenderOutcome::Complete(native_tile(&old, 0x73)),
            TileRetentionClass::RecentUse,
            &NeverCancelledTileCache,
        )
        .unwrap()
    {
        TileAdmission::Rejected(rejected) => rejected,
        TileAdmission::Admitted(_) => panic!("old epoch must not enter restarted cache"),
    };
    assert_eq!(rejected.reason(), TileRejectReason::StaleRendererEpoch);
    admit(
        &mut restarted,
        &current,
        native_tile(&current, 0x74),
        TileRetentionClass::ProtectedViewport,
    );
    assert!(matches!(
        restarted
            .lookup(&current, &NeverCancelledTileCache)
            .unwrap(),
        TileCacheLookup::Hit(_)
    ));
}

#[test]
fn limits_reject_zero_inconsistent_and_hard_ceiling_profiles() {
    let defaults = TileCacheLimitConfig::default();
    let limits = TileCacheLimits::validate(defaults).unwrap();
    assert_eq!(limits.max_entries(), defaults.max_entries);
    assert_eq!(limits.max_tile_pixel_bytes(), defaults.max_tile_pixel_bytes);
    assert_eq!(limits.max_pixel_bytes(), defaults.max_pixel_bytes);
    assert_eq!(limits.max_resident_bytes(), defaults.max_resident_bytes);

    for config in [
        TileCacheLimitConfig {
            max_entries: 0,
            ..defaults
        },
        TileCacheLimitConfig {
            max_tile_pixel_bytes: 0,
            ..defaults
        },
        TileCacheLimitConfig {
            max_pixel_bytes: 0,
            ..defaults
        },
        TileCacheLimitConfig {
            max_resident_bytes: 0,
            ..defaults
        },
        TileCacheLimitConfig {
            max_tile_pixel_bytes: defaults.max_pixel_bytes + 1,
            ..defaults
        },
        TileCacheLimitConfig {
            max_pixel_bytes: defaults.max_resident_bytes + 1,
            ..defaults
        },
        TileCacheLimitConfig {
            max_entries: u64::MAX,
            ..defaults
        },
        TileCacheLimitConfig {
            max_tile_pixel_bytes: u64::MAX,
            max_pixel_bytes: u64::MAX,
            max_resident_bytes: u64::MAX,
            ..defaults
        },
    ] {
        assert_eq!(
            TileCacheLimits::validate(config).unwrap_err().code(),
            TileCacheErrorCode::InvalidLimits
        );
    }
}

#[test]
fn debug_output_redacts_native_pixel_storage_and_rejected_outcomes() {
    let plan = plan(PlanSpec::default());
    let exact = address(&plan, 0);
    let tile = native_tile(&exact, 0xa5);
    let tile_debug = format!("{tile:?}");
    assert!(tile_debug.contains("[REDACTED]"));
    assert!(!tile_debug.contains("165, 165"));

    let mut cache = TileCache::new(binding(&plan), TileCacheLimits::default()).unwrap();
    admit(
        &mut cache,
        &exact,
        tile,
        TileRetentionClass::ProtectedViewport,
    );
    let cache_debug = format!("{cache:?}");
    let hit_debug = format!(
        "{:?}",
        cache.lookup(&exact, &NeverCancelledTileCache).unwrap()
    );
    for debug in [&cache_debug, &hit_debug] {
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("165, 165"));
    }
}
