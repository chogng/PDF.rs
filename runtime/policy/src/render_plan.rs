use pdf_rs_bytes::SourceIdentity;
pub use pdf_rs_protocol::RenderPlanId;
use pdf_rs_scene::{PageGeometry, PageRotation, Scene};

use crate::canonical_hash::CanonicalHasher;
use crate::capability::{CancellationWork, subject_for_scene};
use crate::{
    CapabilityDecision, CapabilityDecisionHash, CapabilityStatus, GeometryHash, NativeBackend,
    OptionalContentIdentity, OutputProfile, PlannedTileHash, PolicyCancellation, PolicyError,
    PolicyLimitKind, PolicyLimits, QualityPolicy, RenderConfig, RenderConfigHash, RenderPlanHash,
    RendererEpoch, SceneHash, TileContentHash,
};

pub(crate) const RENDER_PLAN_SCHEMA_VERSION: u16 = 1;
const TILE_KEY_SCHEMA_VERSION: u16 = 1;
const PLANNED_TILE_SCHEMA_VERSION: u16 = 1;

/// Canonical reduced positive zoom ratio.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ZoomRatio {
    numerator: u32,
    denominator: u32,
}

impl ZoomRatio {
    /// Creates a nonzero ratio and rejects noncanonical reducible forms.
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, PolicyError> {
        if numerator == 0 || denominator == 0 || gcd(numerator, denominator) != 1 {
            return Err(PolicyError::invalid_render_request());
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// Returns the reduced numerator.
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    /// Returns the reduced denominator.
    pub const fn denominator(self) -> u32 {
        self.denominator
    }
}

/// Checked device-pixel rectangle in top-left page space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl DeviceRect {
    /// Creates a positive-area rectangle whose exclusive ends remain representable as `i32`.
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Result<Self, PolicyError> {
        let width_signed =
            i32::try_from(width).map_err(|_| PolicyError::invalid_render_request())?;
        let height_signed =
            i32::try_from(height).map_err(|_| PolicyError::invalid_render_request())?;
        if width == 0
            || height == 0
            || x.checked_add(width_signed).is_none()
            || y.checked_add(height_signed).is_none()
        {
            return Err(PolicyError::invalid_render_request());
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    /// Returns the left coordinate.
    pub const fn x(self) -> i32 {
        self.x
    }

    /// Returns the top coordinate.
    pub const fn y(self) -> i32 {
        self.y
    }

    /// Returns the width.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Returns the height.
    pub const fn height(self) -> u32 {
        self.height
    }
}

/// Immutable generation-bound viewport identity retained by one RenderPlan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewportIdentity {
    generation: u64,
    geometry_hash: GeometryHash,
    clip: DeviceRect,
    zoom: ZoomRatio,
    device_scale_milli: u32,
    rotation: PageRotation,
    optional_content: OptionalContentIdentity,
    annotation_revision: u64,
}

impl ViewportIdentity {
    /// Returns the nonzero replacement generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns the complete page-geometry identity.
    pub const fn geometry_hash(self) -> GeometryHash {
        self.geometry_hash
    }

    /// Returns the requested device-pixel clip.
    pub const fn clip(self) -> DeviceRect {
        self.clip
    }

    /// Returns the canonical zoom.
    pub const fn zoom(self) -> ZoomRatio {
        self.zoom
    }

    /// Returns the integer milli-scale DPR.
    pub const fn device_scale_milli(self) -> u32 {
        self.device_scale_milli
    }

    /// Returns the user-requested canonical rotation.
    pub const fn rotation(self) -> PageRotation {
        self.rotation
    }

    /// Returns the optional-content configuration identity.
    pub const fn optional_content(self) -> OptionalContentIdentity {
        self.optional_content
    }

    /// Returns the annotation revision.
    pub const fn annotation_revision(self) -> u64 {
        self.annotation_revision
    }
}

/// Validated generation and viewport input for Native render planning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderPlanRequest {
    generation: u64,
    clip: DeviceRect,
    zoom: ZoomRatio,
    device_scale_milli: u32,
    rotation: PageRotation,
    optional_content: OptionalContentIdentity,
    annotation_revision: u64,
}

impl RenderPlanRequest {
    /// Creates a request with nonzero generation and integer milli-scale DPR.
    #[allow(
        clippy::too_many_arguments,
        reason = "every independent viewport identity must be explicit at the planning boundary"
    )]
    pub fn new(
        generation: u64,
        clip: DeviceRect,
        zoom: ZoomRatio,
        device_scale_milli: u32,
        rotation: PageRotation,
        optional_content: OptionalContentIdentity,
        annotation_revision: u64,
    ) -> Result<Self, PolicyError> {
        if generation == 0 || device_scale_milli == 0 {
            return Err(PolicyError::invalid_render_request());
        }
        Ok(Self {
            generation,
            clip,
            zoom,
            device_scale_milli,
            rotation,
            optional_content,
            annotation_revision,
        })
    }

    /// Returns the nonzero viewport generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns the device-pixel clip.
    pub const fn clip(self) -> DeviceRect {
        self.clip
    }

    /// Returns the canonical zoom ratio.
    pub const fn zoom(self) -> ZoomRatio {
        self.zoom
    }

    /// Returns the integer milli-scale DPR.
    pub const fn device_scale_milli(self) -> u32 {
        self.device_scale_milli
    }

    /// Returns the user-requested rotation.
    pub const fn rotation(self) -> PageRotation {
        self.rotation
    }

    /// Returns the optional-content identity.
    pub const fn optional_content(self) -> OptionalContentIdentity {
        self.optional_content
    }

    /// Returns the annotation revision.
    pub const fn annotation_revision(self) -> u64 {
        self.annotation_revision
    }
}

/// Generation-independent complete content and cache identity for one product tile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TileContentKey {
    source: SourceIdentity,
    document_revision: u64,
    revision_startxref: u64,
    page_index: u32,
    page_object_number: u32,
    page_object_generation: u16,
    scene_hash: SceneHash,
    decision_hash: CapabilityDecisionHash,
    geometry_hash: GeometryHash,
    viewport_clip: DeviceRect,
    zoom: ZoomRatio,
    device_scale_milli: u32,
    rotation: PageRotation,
    optional_content: OptionalContentIdentity,
    annotation_revision: u64,
    tile: DeviceRect,
    quality: QualityPolicy,
    output_profile: OutputProfile,
    render_config_hash: RenderConfigHash,
    renderer_epoch: RendererEpoch,
    backend: NativeBackend,
    hash: TileContentHash,
}

impl TileContentKey {
    /// Returns the immutable source identity.
    pub const fn source(&self) -> SourceIdentity {
        self.source
    }

    /// Returns the product document revision.
    pub const fn document_revision(&self) -> u64 {
        self.document_revision
    }

    /// Returns the revision xref anchor.
    pub const fn revision_startxref(&self) -> u64 {
        self.revision_startxref
    }

    /// Returns the logical page index.
    pub const fn page_index(&self) -> u32 {
        self.page_index
    }

    /// Returns the Page object number.
    pub const fn page_object_number(&self) -> u32 {
        self.page_object_number
    }

    /// Returns the Page object generation.
    pub const fn page_object_generation(&self) -> u16 {
        self.page_object_generation
    }

    /// Returns the complete Scene identity.
    pub const fn scene_hash(&self) -> SceneHash {
        self.scene_hash
    }

    /// Returns the complete product capability-policy identity.
    pub const fn decision_hash(&self) -> CapabilityDecisionHash {
        self.decision_hash
    }

    /// Returns the complete page-geometry identity.
    pub const fn geometry_hash(&self) -> GeometryHash {
        self.geometry_hash
    }

    /// Returns the generation-independent viewport clip.
    pub const fn viewport_clip(&self) -> DeviceRect {
        self.viewport_clip
    }

    /// Returns the canonical zoom ratio.
    pub const fn zoom(&self) -> ZoomRatio {
        self.zoom
    }

    /// Returns the integer milli-scale DPR.
    pub const fn device_scale_milli(&self) -> u32 {
        self.device_scale_milli
    }

    /// Returns the user-requested rotation.
    pub const fn rotation(&self) -> PageRotation {
        self.rotation
    }

    /// Returns the optional-content identity.
    pub const fn optional_content(&self) -> OptionalContentIdentity {
        self.optional_content
    }

    /// Returns the annotation revision.
    pub const fn annotation_revision(&self) -> u64 {
        self.annotation_revision
    }

    /// Returns the tile rectangle.
    pub const fn tile(&self) -> DeviceRect {
        self.tile
    }

    /// Returns the quality policy.
    pub const fn quality(&self) -> QualityPolicy {
        self.quality
    }

    /// Returns the complete output profile.
    pub const fn output_profile(&self) -> OutputProfile {
        self.output_profile
    }

    /// Returns the render-configuration identity.
    pub const fn render_config_hash(&self) -> RenderConfigHash {
        self.render_config_hash
    }

    /// Returns the renderer epoch.
    pub const fn renderer_epoch(&self) -> RendererEpoch {
        self.renderer_epoch
    }

    /// Returns the selected Native backend.
    pub const fn backend(&self) -> NativeBackend {
        self.backend
    }

    /// Returns the complete generation-independent typed digest.
    pub const fn hash(&self) -> TileContentHash {
        self.hash
    }
}

/// One generation-bound tile identity used for stale-result suppression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedTileIdentity {
    ordinal: u32,
    content_key: TileContentKey,
    generation: u64,
    plan_id: RenderPlanId,
    plan_hash: RenderPlanHash,
    hash: PlannedTileHash,
}

impl PlannedTileIdentity {
    /// Returns the canonical row-major tile ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Borrows the generation-independent content key.
    pub const fn content_key(&self) -> &TileContentKey {
        &self.content_key
    }

    /// Returns the explicit viewport generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the owning plan ID.
    pub const fn plan_id(&self) -> RenderPlanId {
        self.plan_id
    }

    /// Returns the owning complete plan hash.
    pub const fn plan_hash(&self) -> RenderPlanHash {
        self.plan_hash
    }

    /// Returns the complete generation-bound tile hash.
    pub const fn hash(&self) -> PlannedTileHash {
        self.hash
    }
}

/// Complete immutable backend-neutral Native RenderPlan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderPlan {
    schema_version: u16,
    id: RenderPlanId,
    hash: RenderPlanHash,
    decision: CapabilityDecision,
    viewport: ViewportIdentity,
    config: RenderConfig,
    renderer_epoch: RendererEpoch,
    manifest: pdf_rs_protocol::RenderPlanManifest,
    tiles: Vec<PlannedTileIdentity>,
}

impl RenderPlan {
    /// Returns the RenderPlan schema version.
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Returns the nonzero deterministic plan ID.
    pub const fn id(&self) -> RenderPlanId {
        self.id
    }

    /// Returns the complete plan digest.
    pub const fn hash(&self) -> RenderPlanHash {
        self.hash
    }

    /// Returns the exact Supported decision consumed by this plan.
    pub const fn decision(&self) -> &CapabilityDecision {
        &self.decision
    }

    /// Returns the generation-bound viewport identity.
    pub const fn viewport(&self) -> ViewportIdentity {
        self.viewport
    }

    /// Returns the complete Native render configuration.
    pub const fn config(&self) -> RenderConfig {
        self.config
    }

    /// Returns the renderer epoch.
    pub const fn renderer_epoch(&self) -> RendererEpoch {
        self.renderer_epoch
    }

    /// Borrows the exact generated manifest whose canonical payload is bound by [`Self::hash`].
    pub const fn protocol_manifest(&self) -> &pdf_rs_protocol::RenderPlanManifest {
        &self.manifest
    }

    /// Borrows tiles in canonical row-major order.
    pub fn tiles(&self) -> &[PlannedTileIdentity] {
        &self.tiles
    }

    /// Reports whether a Native retry changes configuration identity or renderer epoch.
    ///
    /// Both possible backends are represented by [`NativeBackend`], so this API cannot select an
    /// external renderer.
    pub fn retry_has_distinct_identity(
        &self,
        config: RenderConfig,
        renderer_epoch: RendererEpoch,
    ) -> bool {
        config.hash() != self.config.hash() || renderer_epoch != self.renderer_epoch
    }
}

/// Planning outcome that preserves Unsupported and Rejected decisions without partial tiles.
#[allow(
    clippy::large_enum_variant,
    reason = "complete bounded proof-bearing plans and decisions remain inline so a terminal outcome needs no second infallible allocation"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderPlanOutcome {
    /// One complete publishable Native plan.
    Ready(RenderPlan),
    /// Exact nonpublishable product decision, with no tile set.
    NotPublishable(CapabilityDecision),
}

/// Creates a complete immutable Native plan or preserves a nonpublishable decision.
#[allow(
    clippy::too_many_arguments,
    reason = "planning binds Scene, decision, viewport, config, epoch, limits, and cancellation independently"
)]
pub fn create_render_plan(
    scene: &Scene,
    decision: CapabilityDecision,
    config: RenderConfig,
    request: RenderPlanRequest,
    renderer_epoch: RendererEpoch,
    limits: PolicyLimits,
    cancellation: &dyn PolicyCancellation,
) -> Result<RenderPlanOutcome, PolicyError> {
    if decision.status() != CapabilityStatus::Supported {
        return Ok(RenderPlanOutcome::NotPublishable(decision));
    }
    let mut work = CancellationWork::new(cancellation, limits.cancellation_interval())?;
    let actual_subject =
        subject_for_scene(scene, decision.subject().document_revision(), &mut work)?;
    if actual_subject != decision.subject()
        || decision.missing_total() != 0
        || !decision.missing().is_empty()
        || decision.rejection_code().is_some()
    {
        return Err(PolicyError::identity_mismatch());
    }

    ensure_plan_dimensions(request.clip(), limits)?;
    let geometry_hash = hash_geometry(scene.geometry())?;
    let viewport = ViewportIdentity {
        generation: request.generation(),
        geometry_hash,
        clip: request.clip(),
        zoom: request.zoom(),
        device_scale_milli: request.device_scale_milli(),
        rotation: request.rotation(),
        optional_content: request.optional_content(),
        annotation_revision: request.annotation_revision(),
    };

    let (tile_width, tile_height) = config.tile_size();
    let columns = ceil_div(request.clip().width(), tile_width)?;
    let rows = ceil_div(request.clip().height(), tile_height)?;
    let tile_count = columns
        .checked_mul(rows)
        .ok_or_else(PolicyError::numeric_overflow)?;
    if tile_count > limits.max_tiles() {
        return Err(PolicyError::resource(
            PolicyLimitKind::Tiles,
            u64::from(limits.max_tiles()),
            0,
            u64::from(tile_count),
        ));
    }

    let tile_capacity = usize::try_from(tile_count).map_err(|_| PolicyError::numeric_overflow())?;
    let mut content_keys = Vec::new();
    content_keys
        .try_reserve_exact(tile_capacity)
        .map_err(|_| PolicyError::allocation())?;
    for row in 0..rows {
        let y_offset = row
            .checked_mul(tile_height)
            .ok_or_else(PolicyError::numeric_overflow)?;
        let y = add_unsigned_i32(request.clip().y(), y_offset)?;
        let remaining_height = request
            .clip()
            .height()
            .checked_sub(y_offset)
            .ok_or_else(PolicyError::numeric_overflow)?;
        let height = remaining_height.min(tile_height);
        for column in 0..columns {
            let x_offset = column
                .checked_mul(tile_width)
                .ok_or_else(PolicyError::numeric_overflow)?;
            let x = add_unsigned_i32(request.clip().x(), x_offset)?;
            let remaining_width = request
                .clip()
                .width()
                .checked_sub(x_offset)
                .ok_or_else(PolicyError::numeric_overflow)?;
            let width = remaining_width.min(tile_width);
            let tile = DeviceRect::new(x, y, width, height)?;
            let mut key = TileContentKey {
                source: actual_subject.source(),
                document_revision: actual_subject.document_revision(),
                revision_startxref: actual_subject.revision_startxref(),
                page_index: actual_subject.page_index(),
                page_object_number: actual_subject.page_object_number(),
                page_object_generation: actual_subject.page_object_generation(),
                scene_hash: actual_subject.scene_hash(),
                decision_hash: decision.hash(),
                geometry_hash,
                viewport_clip: request.clip(),
                zoom: request.zoom(),
                device_scale_milli: request.device_scale_milli(),
                rotation: request.rotation(),
                optional_content: request.optional_content(),
                annotation_revision: request.annotation_revision(),
                tile,
                quality: config.quality(),
                output_profile: config.output_profile(),
                render_config_hash: config.hash(),
                renderer_epoch,
                backend: config.backend(),
                hash: TileContentHash::new([0; 32]),
            };
            key.hash = TileContentHash::new(hash_tile_content(&key)?);
            content_keys.push(key);
            work.step()?;
        }
    }
    if content_keys.len() != tile_capacity {
        return Err(PolicyError::identity_mismatch());
    }

    let id = RenderPlanId::new(request.generation());
    let manifest = crate::protocol_projection::render_plan_manifest(
        &decision,
        viewport,
        config,
        renderer_epoch,
        id,
        &content_keys,
        &mut work,
    )?;
    let plan_hash = RenderPlanHash::new(hash_render_plan_manifest(&manifest, &mut work)?);
    let mut tiles = Vec::new();
    tiles
        .try_reserve_exact(tile_capacity)
        .map_err(|_| PolicyError::allocation())?;
    for (ordinal, content_key) in content_keys.into_iter().enumerate() {
        let ordinal = u32::try_from(ordinal).map_err(|_| PolicyError::numeric_overflow())?;
        let hash = PlannedTileHash::new(hash_planned_tile(
            ordinal,
            &content_key,
            request.generation(),
            id,
            plan_hash,
        )?);
        tiles.push(PlannedTileIdentity {
            ordinal,
            content_key,
            generation: request.generation(),
            plan_id: id,
            plan_hash,
            hash,
        });
        work.step()?;
    }
    work.check()?;
    Ok(RenderPlanOutcome::Ready(RenderPlan {
        schema_version: RENDER_PLAN_SCHEMA_VERSION,
        id,
        hash: plan_hash,
        decision,
        viewport,
        config,
        renderer_epoch,
        manifest,
        tiles,
    }))
}

fn ensure_plan_dimensions(clip: DeviceRect, limits: PolicyLimits) -> Result<(), PolicyError> {
    for dimension in [clip.width(), clip.height()] {
        if dimension > limits.max_output_dimension() {
            return Err(PolicyError::resource(
                PolicyLimitKind::OutputDimension,
                u64::from(limits.max_output_dimension()),
                0,
                u64::from(dimension),
            ));
        }
    }
    let pixels = u64::from(clip.width())
        .checked_mul(u64::from(clip.height()))
        .ok_or_else(PolicyError::numeric_overflow)?;
    if pixels > limits.max_output_pixels() {
        return Err(PolicyError::resource(
            PolicyLimitKind::OutputPixels,
            limits.max_output_pixels(),
            0,
            pixels,
        ));
    }
    Ok(())
}

fn ceil_div(value: u32, divisor: u32) -> Result<u32, PolicyError> {
    let quotient = value / divisor;
    quotient
        .checked_add(u32::from(!value.is_multiple_of(divisor)))
        .ok_or_else(PolicyError::numeric_overflow)
}

fn add_unsigned_i32(base: i32, offset: u32) -> Result<i32, PolicyError> {
    base.checked_add(i32::try_from(offset).map_err(|_| PolicyError::numeric_overflow())?)
        .ok_or_else(PolicyError::numeric_overflow)
}

fn hash_geometry(geometry: PageGeometry) -> Result<GeometryHash, PolicyError> {
    let mut hasher = CanonicalHasher::new(b"page-geometry/v1");
    for coordinate in geometry.media_box().coordinates() {
        hasher.i64(coordinate.scaled());
    }
    for coordinate in geometry.crop_box().coordinates() {
        hasher.i64(coordinate.scaled());
    }
    hasher.u16(geometry.rotation().degrees());
    Ok(GeometryHash::new(hasher.finish()?))
}

fn hash_tile_content(key: &TileContentKey) -> Result<[u8; 32], PolicyError> {
    let mut hasher = CanonicalHasher::new(b"tile-content-key/v1");
    hasher.u16(TILE_KEY_SCHEMA_VERSION);
    hasher.bytes(key.source.stable_id().digest().as_slice());
    hasher.u64(key.source.revision().value());
    hasher.u64(key.document_revision);
    hasher.u64(key.revision_startxref);
    hasher.u32(key.page_index);
    hasher.u32(key.page_object_number);
    hasher.u16(key.page_object_generation);
    hasher.bytes(key.scene_hash.digest());
    hasher.bytes(key.decision_hash.digest());
    hasher.bytes(key.geometry_hash.digest());
    hash_rect(&mut hasher, key.viewport_clip);
    hash_zoom(&mut hasher, key.zoom);
    hasher.u32(key.device_scale_milli);
    hasher.u16(key.rotation.degrees());
    hasher.u64(key.optional_content.value());
    hasher.u64(key.annotation_revision);
    hash_rect(&mut hasher, key.tile);
    hasher.u8(key.quality as u8);
    hash_output_profile(&mut hasher, key.output_profile);
    hasher.bytes(key.render_config_hash.digest());
    hasher.u32(key.renderer_epoch.value());
    hasher.u8(key.backend as u8);
    hasher.finish()
}

fn hash_render_plan_manifest(
    manifest: &pdf_rs_protocol::RenderPlanManifest,
    work: &mut CancellationWork<'_>,
) -> Result<[u8; 32], PolicyError> {
    work.check()?;
    let preimage = crate::protocol_projection::render_plan_manifest_hash_preimage(manifest, work)?;
    crate::canonical_hash::hash_preimage_observed(&preimage, || work.step())
}

fn hash_planned_tile(
    ordinal: u32,
    key: &TileContentKey,
    generation: u64,
    plan_id: RenderPlanId,
    plan_hash: RenderPlanHash,
) -> Result<[u8; 32], PolicyError> {
    let mut hasher = CanonicalHasher::new(b"planned-tile-identity/v1");
    hasher.u16(PLANNED_TILE_SCHEMA_VERSION);
    hasher.u32(ordinal);
    hasher.bytes(key.hash().digest());
    hasher.u64(generation);
    hasher.u64(plan_id.value());
    hasher.bytes(plan_hash.digest());
    hasher.finish()
}

fn hash_rect(hasher: &mut CanonicalHasher, rect: DeviceRect) {
    hasher.i32(rect.x());
    hasher.i32(rect.y());
    hasher.u32(rect.width());
    hasher.u32(rect.height());
}

fn hash_zoom(hasher: &mut CanonicalHasher, zoom: ZoomRatio) {
    hasher.u32(zoom.numerator());
    hasher.u32(zoom.denominator());
}

fn hash_output_profile(hasher: &mut CanonicalHasher, profile: OutputProfile) {
    hasher.u32(profile.id());
    hasher.u8(profile.color() as u8);
    hasher.u8(profile.format() as u8);
    hasher.u8(profile.alpha() as u8);
}

const fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
    use pdf_rs_scene::PageRotation;

    use super::{
        CancellationWork, DeviceRect, PolicyLimits, RenderPlanHash, TileContentKey, ZoomRatio, gcd,
        hash_planned_tile, hash_render_plan_manifest, hash_tile_content,
    };
    use crate::{
        CapabilityDecisionHash, GeometryHash, NativeBackend, NeverCancelled,
        OptionalContentIdentity, OutputProfile, PlannedTileHash, QualityPolicy, RenderConfigHash,
        RendererEpoch, SceneHash, TileContentHash,
    };
    use pdf_rs_protocol as wire;

    #[test]
    fn zoom_and_rect_require_canonical_checked_forms() {
        assert_eq!(gcd(12, 8), 4);
        assert!(ZoomRatio::new(3, 2).is_ok());
        assert!(ZoomRatio::new(6, 4).is_err());
        assert!(ZoomRatio::new(0, 1).is_err());
        assert!(DeviceRect::new(-10, -20, 30, 40).is_ok());
        assert!(DeviceRect::new(i32::MAX, 0, 1, 1).is_err());
    }

    #[test]
    fn never_cancelled_is_usable_as_a_public_default_token() {
        let token = crate::NeverCancelled;
        assert!(!crate::PolicyCancellation::is_cancelled(&token));
    }

    #[test]
    fn every_tile_content_identity_input_changes_the_digest() {
        let base = TileContentKey {
            source: SourceIdentity::new(SourceStableId::new([1; 32]), SourceRevision::new(2)),
            document_revision: 3,
            revision_startxref: 4,
            page_index: 5,
            page_object_number: 6,
            page_object_generation: 7,
            scene_hash: SceneHash::new([8; 32]),
            decision_hash: CapabilityDecisionHash::new([19; 32]),
            geometry_hash: GeometryHash::new([9; 32]),
            viewport_clip: DeviceRect::new(-10, 20, 640, 480).unwrap(),
            zoom: ZoomRatio::new(3, 2).unwrap(),
            device_scale_milli: 2_000,
            rotation: PageRotation::Degrees0,
            optional_content: OptionalContentIdentity::new(11),
            annotation_revision: 12,
            tile: DeviceRect::new(-10, 20, 256, 256).unwrap(),
            quality: QualityPolicy::Full,
            output_profile: OutputProfile::SRGB_RGBA8_STRAIGHT,
            render_config_hash: RenderConfigHash::new([13; 32]),
            renderer_epoch: RendererEpoch::new(14).unwrap(),
            backend: NativeBackend::FastCpu,
            hash: TileContentHash::new([0; 32]),
        };
        let expected = hash_tile_content(&base).unwrap();
        let mut variants = Vec::new();

        let mut changed = base.clone();
        changed.source = SourceIdentity::new(SourceStableId::new([2; 32]), SourceRevision::new(2));
        variants.push(changed);
        let mut changed = base.clone();
        changed.source = SourceIdentity::new(SourceStableId::new([1; 32]), SourceRevision::new(3));
        variants.push(changed);

        macro_rules! push_incremented {
            ($field:ident) => {{
                let mut changed = base.clone();
                changed.$field += 1;
                variants.push(changed);
            }};
        }
        push_incremented!(document_revision);
        push_incremented!(revision_startxref);
        push_incremented!(page_index);
        push_incremented!(page_object_number);
        push_incremented!(page_object_generation);

        let mut changed = base.clone();
        changed.scene_hash = SceneHash::new([15; 32]);
        variants.push(changed);
        let mut changed = base.clone();
        changed.decision_hash = CapabilityDecisionHash::new([20; 32]);
        variants.push(changed);
        let mut changed = base.clone();
        changed.geometry_hash = GeometryHash::new([16; 32]);
        variants.push(changed);

        for mutate in [
            |rect: &mut DeviceRect| rect.x += 1,
            |rect: &mut DeviceRect| rect.y += 1,
            |rect: &mut DeviceRect| rect.width += 1,
            |rect: &mut DeviceRect| rect.height += 1,
        ] {
            let mut changed = base.clone();
            mutate(&mut changed.viewport_clip);
            variants.push(changed);
        }

        let mut changed = base.clone();
        changed.zoom.numerator += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.zoom.denominator += 1;
        variants.push(changed);
        push_incremented!(device_scale_milli);

        let mut changed = base.clone();
        changed.rotation = PageRotation::Degrees90;
        variants.push(changed);
        let mut changed = base.clone();
        changed.optional_content = OptionalContentIdentity::new(12);
        variants.push(changed);
        push_incremented!(annotation_revision);

        for mutate in [
            |rect: &mut DeviceRect| rect.x += 1,
            |rect: &mut DeviceRect| rect.y += 1,
            |rect: &mut DeviceRect| rect.width += 1,
            |rect: &mut DeviceRect| rect.height += 1,
        ] {
            let mut changed = base.clone();
            mutate(&mut changed.tile);
            variants.push(changed);
        }

        let mut changed = base.clone();
        changed.quality = QualityPolicy::Preview;
        variants.push(changed);
        let mut changed = base.clone();
        changed.output_profile = OutputProfile::hash_test_variant();
        variants.push(changed);
        let mut changed = base.clone();
        changed.render_config_hash = RenderConfigHash::new([17; 32]);
        variants.push(changed);
        let mut changed = base.clone();
        changed.renderer_epoch = RendererEpoch::new(15).unwrap();
        variants.push(changed);
        let mut changed = base.clone();
        changed.backend = NativeBackend::ReferenceCpu;
        variants.push(changed);

        for variant in variants {
            assert_ne!(hash_tile_content(&variant).unwrap(), expected);
        }

        let mut derived_output_only = base;
        derived_output_only.hash = TileContentHash::new([18; 32]);
        assert_eq!(
            hash_tile_content(&derived_output_only).unwrap(),
            expected,
            "the cached derived digest is intentionally not self-referential"
        );
    }

    #[test]
    fn every_planned_tile_identity_input_changes_the_digest() {
        let mut key = TileContentKey {
            source: SourceIdentity::new(SourceStableId::new([1; 32]), SourceRevision::new(2)),
            document_revision: 3,
            revision_startxref: 4,
            page_index: 5,
            page_object_number: 6,
            page_object_generation: 7,
            scene_hash: SceneHash::new([8; 32]),
            decision_hash: CapabilityDecisionHash::new([9; 32]),
            geometry_hash: GeometryHash::new([10; 32]),
            viewport_clip: DeviceRect::new(0, 0, 256, 256).unwrap(),
            zoom: ZoomRatio::new(3, 2).unwrap(),
            device_scale_milli: 2_000,
            rotation: PageRotation::Degrees0,
            optional_content: OptionalContentIdentity::new(11),
            annotation_revision: 12,
            tile: DeviceRect::new(0, 0, 256, 256).unwrap(),
            quality: QualityPolicy::Full,
            output_profile: OutputProfile::SRGB_RGBA8_STRAIGHT,
            render_config_hash: RenderConfigHash::new([13; 32]),
            renderer_epoch: RendererEpoch::new(14).unwrap(),
            backend: NativeBackend::FastCpu,
            hash: TileContentHash::new([15; 32]),
        };
        let plan_hash = RenderPlanHash::new([16; 32]);
        let expected =
            hash_planned_tile(0, &key, 17, wire::RenderPlanId::new(18), plan_hash).unwrap();

        assert_ne!(
            hash_planned_tile(1, &key, 17, wire::RenderPlanId::new(18), plan_hash,).unwrap(),
            expected,
        );

        key.hash = TileContentHash::new([19; 32]);
        assert_ne!(
            hash_planned_tile(0, &key, 17, wire::RenderPlanId::new(18), plan_hash,).unwrap(),
            expected,
        );
        key.hash = TileContentHash::new([15; 32]);

        assert_ne!(
            hash_planned_tile(0, &key, 18, wire::RenderPlanId::new(18), plan_hash,).unwrap(),
            expected,
        );
        assert_ne!(
            hash_planned_tile(0, &key, 17, wire::RenderPlanId::new(19), plan_hash,).unwrap(),
            expected,
        );
        assert_ne!(
            hash_planned_tile(
                0,
                &key,
                17,
                wire::RenderPlanId::new(18),
                RenderPlanHash::new([20; 32]),
            )
            .unwrap(),
            expected,
        );

        let derived_hash = PlannedTileHash::new(expected);
        assert!(!derived_hash.is_zero());
    }

    #[test]
    fn every_mutable_render_plan_manifest_field_changes_the_plan_hash() {
        let payload = hash_kat_payload("RenderPlanManifest");
        let manifest = wire::decode_render_plan_manifest_payload(
            &payload,
            wire::PayloadCodecLimits::protocol_default(),
        )
        .unwrap();
        let expected = manifest_hash(&manifest);
        let mut variants = Vec::new();

        let mut changed = manifest.clone();
        changed.plan_schema_version += 1;
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.document_revision += 1;
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.render_config = wire::RenderConfigHash::new([0x68; 32]);
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.renderer_epoch = wire::RendererEpoch::new(changed.renderer_epoch.value() + 1);
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.plan_id = wire::RenderPlanId::new(changed.plan_id.value() + 1);
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.generation += 1;
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.scene_hash = wire::SceneHash::new([0x78; 32]);
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.decision_hash = wire::CapabilityDecisionHash::new([0x8f; 32]);
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.geometry_hash = wire::GeometryHash::new([0x89; 32]);
        variants.push(changed);

        for mutate in [
            |region: &mut wire::SurfaceRegion| region.page_index += 1,
            |region: &mut wire::SurfaceRegion| region.x += 1,
            |region: &mut wire::SurfaceRegion| region.y += 1,
            |region: &mut wire::SurfaceRegion| region.width += 1,
            |region: &mut wire::SurfaceRegion| region.height += 1,
        ] {
            let mut changed = manifest.clone();
            mutate(&mut changed.viewport_clip);
            variants.push(changed);
        }

        let mut changed = manifest.clone();
        changed.zoom_numerator += 1;
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.zoom_denominator += 1;
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.device_scale_milli += 1;
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.rotation = match changed.rotation {
            wire::PageRotation::Degrees0 => wire::PageRotation::Degrees90,
            wire::PageRotation::Degrees90 => wire::PageRotation::Degrees180,
            wire::PageRotation::Degrees180 => wire::PageRotation::Degrees270,
            wire::PageRotation::Degrees270 => wire::PageRotation::Degrees0,
        };
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.optional_content += 1;
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.annotation_revision += 1;
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.backend = match changed.backend {
            wire::NativeBackend::ReferenceCpu => wire::NativeBackend::FastCpu,
            wire::NativeBackend::FastCpu => wire::NativeBackend::ReferenceCpu,
        };
        variants.push(changed);

        let mut changed = manifest.clone();
        changed.quality = match changed.quality {
            wire::QualityPolicy::Preview => wire::QualityPolicy::Full,
            wire::QualityPolicy::Full => wire::QualityPolicy::Preview,
        };
        variants.push(changed);

        for mutate in [
            |region: &mut wire::SurfaceRegion| region.page_index += 1,
            |region: &mut wire::SurfaceRegion| region.x += 1,
            |region: &mut wire::SurfaceRegion| region.y += 1,
            |region: &mut wire::SurfaceRegion| region.width += 1,
            |region: &mut wire::SurfaceRegion| region.height += 1,
        ] {
            let mut changed = manifest.clone();
            mutate(&mut changed.regions[0]);
            variants.push(changed);
        }

        let mut changed = manifest.clone();
        changed.tile_content_hashes[0] = wire::TileContentHash::new([0x9a; 32]);
        variants.push(changed);

        let mut with_second_region = manifest.clone();
        let mut second_region = with_second_region.regions[0].clone();
        second_region.page_index += 1;
        with_second_region.regions.push(second_region);
        with_second_region
            .tile_content_hashes
            .push(wire::TileContentHash::new([0x9b; 32]));
        variants.push(with_second_region.clone());
        with_second_region.regions.swap(0, 1);
        variants.push(with_second_region);

        for variant in variants {
            assert_ne!(manifest_hash(&variant), expected);
        }
    }

    fn manifest_hash(manifest: &wire::RenderPlanManifest) -> RenderPlanHash {
        let mut work = CancellationWork::new(
            &NeverCancelled,
            PolicyLimits::default().cancellation_interval(),
        )
        .unwrap();
        RenderPlanHash::new(hash_render_plan_manifest(manifest, &mut work).unwrap())
    }

    fn hash_kat_payload(type_name: &str) -> Vec<u8> {
        let vectors = include_str!("../../../protocol/generated/payload-codec-vectors.json");
        let hash_section = vectors.split_once("\"hash_known_answers\":").unwrap().1;
        let marker = format!("{{\"type\":\"{type_name}\"");
        let entry = hash_section.split_once(&marker).unwrap().1;
        let entry = entry.split_once('}').unwrap().0;
        let payload = entry
            .split_once("\"payload_hex\":\"")
            .unwrap()
            .1
            .split_once('"')
            .unwrap()
            .0;
        payload
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| (hex_digit(pair[0]) << 4) | hex_digit(pair[1]))
            .collect()
    }

    fn hex_digit(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("generated KAT contains non-hex byte"),
        }
    }
}
