use std::fmt;
use std::mem::size_of;
use std::num::NonZeroU32;
use std::sync::Arc;

use pdf_rs_bytes::SourceIdentity;
pub use pdf_rs_protocol::RenderPlanId;
use pdf_rs_scene::{PageGeometry, PageRotation, Scene, SceneCanonicalObserver, SceneErrorCode};

use crate::canonical_hash::{CanonicalHasher, PreimageHasher};
use crate::capability::{CancellationWork, canonical_scene_upper_bound};
use crate::{
    CapabilityDecision, CapabilityDecisionHash, CapabilityStatus, CapabilitySubject, GeometryHash,
    NativeBackend, OptionalContentIdentity, OutputProfile, PlannedTileHash, PolicyCancellation,
    PolicyError, PolicyJobLimits, PolicyJobPoll, PolicyJobStats, PolicyLimitKind, PolicyLimits,
    PolicyPollBudget, QualityPolicy, RenderConfig, RenderConfigHash, RenderPlanHash, RendererEpoch,
    SceneHash, TileContentHash,
};

pub(crate) const RENDER_PLAN_SCHEMA_VERSION: u16 = 1;
const TILE_KEY_SCHEMA_VERSION: u16 = 1;
const PLANNED_TILE_SCHEMA_VERSION: u16 = 1;

/// Returns the canonical wire-visible identity for one source-bound page geometry.
///
/// The identity binds the exact Scene boxes and intrinsic rotation to the immutable
/// source revision and logical/indirect page identities. The final coordinate-space
/// tag identifies PDF points with a bottom-left origin.
pub fn page_geometry_identity(scene: &Scene) -> Result<[u8; 32], PolicyError> {
    let binding = scene.binding();
    let source = binding.source();
    let page_object = binding.page_object();
    let geometry = scene.geometry();
    let mut hasher = CanonicalHasher::new(b"wire-page-geometry-identity/v1");
    hasher.bytes(source.stable_id().digest().as_slice());
    hasher.u64(source.revision().value());
    hasher.u64(binding.revision_startxref());
    hasher.u32(binding.page_index());
    hasher.u32(page_object.number());
    hasher.u16(page_object.generation());
    for coordinate in geometry.media_box().coordinates() {
        hasher.i64(coordinate.scaled());
    }
    for coordinate in geometry.crop_box().coordinates() {
        hasher.i64(coordinate.scaled());
    }
    hasher.u16(geometry.rotation().degrees());
    hasher.u8(1);
    hasher.finish()
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RenderPlanJobPhase {
    Validate,
    Canonicalize,
    HashCanonical,
    Initialize,
    AllocateContent,
    BuildContent,
    AllocateManifest,
    BuildManifest,
    EncodeManifest,
    HashManifest,
    AllocateTiles,
    BuildTiles,
    Publish,
}

/// Owned render planning job with explicit tile and hash cursors.
pub struct RenderPlanJob {
    scene: Arc<Scene>,
    decision: Option<CapabilityDecision>,
    config: RenderConfig,
    request: RenderPlanRequest,
    renderer_epoch: RendererEpoch,
    limits: PolicyLimits,
    job_limits: PolicyJobLimits,
    stats: PolicyJobStats,
    phase: RenderPlanJobPhase,
    terminal: Option<Result<RenderPlanOutcome, PolicyError>>,
    result_taken: bool,
    canonical: Option<Vec<u8>>,
    canonical_offset: usize,
    canonical_hasher: Option<CanonicalHasher>,
    actual_subject: Option<CapabilitySubject>,
    viewport: Option<ViewportIdentity>,
    columns: u32,
    rows: u32,
    tile_count: u32,
    tile_index: u32,
    content_keys: Vec<TileContentKey>,
    manifest_regions: Vec<pdf_rs_protocol::SurfaceRegion>,
    manifest_hashes: Vec<pdf_rs_protocol::TileContentHash>,
    manifest_index: usize,
    manifest: Option<pdf_rs_protocol::RenderPlanManifest>,
    manifest_preimage: Option<Vec<u8>>,
    manifest_preimage_offset: usize,
    manifest_hasher: Option<PreimageHasher>,
    plan_hash: Option<RenderPlanHash>,
    tiles: Vec<PlannedTileIdentity>,
    planned_tile_index: usize,
}

impl RenderPlanJob {
    /// Creates one owned resumable planning job after admitting a conservative bound derived from
    /// the Scene's published retained capacity. The serializer observer also enforces the same
    /// limit against actual output before allocation.
    #[allow(
        clippy::too_many_arguments,
        reason = "the owned job binds every immutable planning identity and independent limit"
    )]
    pub fn new(
        scene: Arc<Scene>,
        decision: CapabilityDecision,
        config: RenderConfig,
        request: RenderPlanRequest,
        renderer_epoch: RendererEpoch,
        limits: PolicyLimits,
        job_limits: PolicyJobLimits,
    ) -> Result<Self, PolicyError> {
        let declared = canonical_scene_upper_bound(&scene);
        if decision.status() == CapabilityStatus::Supported
            && declared > job_limits.max_atomic_canonical_bytes()
        {
            return Err(PolicyError::resource(
                PolicyLimitKind::AtomicCanonicalBytes,
                job_limits.max_atomic_canonical_bytes(),
                0,
                declared,
            ));
        }
        if decision.status() == CapabilityStatus::Supported
            && declared > job_limits.max_retained_bytes()
        {
            return Err(PolicyError::resource(
                PolicyLimitKind::JobRetainedBytes,
                job_limits.max_retained_bytes(),
                0,
                declared,
            ));
        }
        Ok(Self::new_compatibility(
            scene,
            decision,
            config,
            request,
            renderer_epoch,
            limits,
            job_limits,
        ))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the synchronous compatibility path binds the same complete planning identity"
    )]
    fn new_compatibility(
        scene: Arc<Scene>,
        decision: CapabilityDecision,
        config: RenderConfig,
        request: RenderPlanRequest,
        renderer_epoch: RendererEpoch,
        limits: PolicyLimits,
        job_limits: PolicyJobLimits,
    ) -> Self {
        Self {
            scene,
            decision: Some(decision),
            config,
            request,
            renderer_epoch,
            limits,
            job_limits,
            stats: PolicyJobStats::default(),
            phase: RenderPlanJobPhase::Validate,
            terminal: None,
            result_taken: false,
            canonical: None,
            canonical_offset: 0,
            canonical_hasher: None,
            actual_subject: None,
            viewport: None,
            columns: 0,
            rows: 0,
            tile_count: 0,
            tile_index: 0,
            content_keys: Vec::new(),
            manifest_regions: Vec::new(),
            manifest_hashes: Vec::new(),
            manifest_index: 0,
            manifest: None,
            manifest_preimage: None,
            manifest_preimage_offset: 0,
            manifest_hasher: None,
            plan_hash: None,
            tiles: Vec::new(),
            planned_tile_index: 0,
        }
    }

    /// Returns deterministic work and job-owned capacity accounting.
    pub const fn stats(&self) -> PolicyJobStats {
        self.stats
    }

    /// Borrows the replayable terminal result.
    pub fn result(&self) -> Option<Result<&RenderPlanOutcome, PolicyError>> {
        self.terminal
            .as_ref()
            .map(|result| result.as_ref().map_err(|error| *error))
    }

    /// Moves the terminal result out without cloning a plan.
    pub fn take_result(&mut self) -> Option<Result<RenderPlanOutcome, PolicyError>> {
        let result = self.terminal.take();
        if result.is_some() {
            self.stats.clear_retained();
            self.result_taken = true;
        }
        result
    }

    /// Advances at most the validated nonzero work budget.
    pub fn poll(
        &mut self,
        budget: PolicyPollBudget,
        cancellation: &dyn PolicyCancellation,
    ) -> PolicyJobPoll {
        if self.terminal.is_some() || self.result_taken {
            return PolicyJobPoll::Ready;
        }
        for _ in 0..budget.work_units().get() {
            if cancellation.is_cancelled() {
                self.fail(PolicyError::cancelled());
                return PolicyJobPoll::Ready;
            }
            if let Err(error) = self.stats.charge_work() {
                self.fail(error);
                return PolicyJobPoll::Ready;
            }
            match self.step(cancellation) {
                Ok(true) => return PolicyJobPoll::Ready,
                Ok(false) => {}
                Err(error) => {
                    self.fail(error);
                    return PolicyJobPoll::Ready;
                }
            }
        }
        PolicyJobPoll::Pending
    }

    fn step(&mut self, cancellation: &dyn PolicyCancellation) -> Result<bool, PolicyError> {
        match self.phase {
            RenderPlanJobPhase::Validate => self.step_validate(),
            RenderPlanJobPhase::Canonicalize => self.step_canonicalize(cancellation),
            RenderPlanJobPhase::HashCanonical => self.step_hash_canonical(),
            RenderPlanJobPhase::Initialize => self.step_initialize(),
            RenderPlanJobPhase::AllocateContent => self.step_allocate_content(),
            RenderPlanJobPhase::BuildContent => self.step_build_content(),
            RenderPlanJobPhase::AllocateManifest => self.step_allocate_manifest(),
            RenderPlanJobPhase::BuildManifest => self.step_build_manifest(),
            RenderPlanJobPhase::EncodeManifest => self.step_encode_manifest(cancellation),
            RenderPlanJobPhase::HashManifest => self.step_hash_manifest(),
            RenderPlanJobPhase::AllocateTiles => self.step_allocate_tiles(),
            RenderPlanJobPhase::BuildTiles => self.step_build_tiles(),
            RenderPlanJobPhase::Publish => self.step_publish(),
        }
    }

    fn step_validate(&mut self) -> Result<bool, PolicyError> {
        let decision = self
            .decision
            .as_ref()
            .ok_or_else(PolicyError::identity_mismatch)?;
        if decision.status() != CapabilityStatus::Supported {
            let decision = self
                .decision
                .take()
                .ok_or_else(PolicyError::identity_mismatch)?;
            self.terminal = Some(Ok(RenderPlanOutcome::NotPublishable(decision)));
            return Ok(true);
        }
        self.phase = RenderPlanJobPhase::Canonicalize;
        Ok(false)
    }

    fn step_canonicalize(
        &mut self,
        cancellation: &dyn PolicyCancellation,
    ) -> Result<bool, PolicyError> {
        struct Observer<'a> {
            cancellation: &'a dyn PolicyCancellation,
            limit: u64,
            observed: u64,
            error: Option<PolicyError>,
        }
        impl SceneCanonicalObserver for Observer<'_> {
            fn observe(&mut self, fragment: &[u8]) -> bool {
                if self.cancellation.is_cancelled() {
                    self.error = Some(PolicyError::cancelled());
                    return false;
                }
                let Ok(additional) = u64::try_from(fragment.len()) else {
                    self.error = Some(PolicyError::numeric_overflow());
                    return false;
                };
                let Some(attempted) = self.observed.checked_add(additional) else {
                    self.error = Some(PolicyError::numeric_overflow());
                    return false;
                };
                if attempted > self.limit {
                    self.error = Some(PolicyError::resource(
                        PolicyLimitKind::AtomicCanonicalBytes,
                        self.limit,
                        self.observed,
                        additional,
                    ));
                    return false;
                }
                self.observed = attempted;
                true
            }
        }
        let mut observer = Observer {
            cancellation,
            limit: self.job_limits.max_atomic_canonical_bytes(),
            observed: 0,
            error: None,
        };
        let canonical = match self.scene.canonical_json_bytes_observed(&mut observer) {
            Ok(value) => value,
            Err(error) if error.code() == SceneErrorCode::CanonicalizationInterrupted => {
                return Err(observer
                    .error
                    .unwrap_or_else(PolicyError::scene_canonicalization));
            }
            Err(_) => return Err(PolicyError::scene_canonicalization()),
        };
        let retained = render_vec_capacity_bytes(&canonical)?;
        self.stats.charge_allocation(retained, self.job_limits)?;
        self.stats.set_atomic_canonical_bytes(
            u64::try_from(canonical.len()).map_err(|_| PolicyError::numeric_overflow())?,
        );
        let mut hasher = CanonicalHasher::new(b"scene/canonical-json/v1");
        hasher.u64(u64::try_from(canonical.len()).map_err(|_| PolicyError::numeric_overflow())?);
        self.canonical = Some(canonical);
        self.canonical_hasher = Some(hasher);
        self.phase = RenderPlanJobPhase::HashCanonical;
        Ok(false)
    }

    fn step_hash_canonical(&mut self) -> Result<bool, PolicyError> {
        let canonical = self
            .canonical
            .as_ref()
            .ok_or_else(PolicyError::identity_mismatch)?;
        if let Some(chunk) = canonical
            .get(self.canonical_offset..)
            .and_then(|remaining| remaining.chunks(4 * 1024).next())
        {
            self.canonical_hasher
                .as_mut()
                .ok_or_else(PolicyError::identity_mismatch)?
                .bytes(chunk);
            self.canonical_offset = self
                .canonical_offset
                .checked_add(chunk.len())
                .ok_or_else(PolicyError::numeric_overflow)?;
            return Ok(false);
        }
        let scene_hash = SceneHash::new(
            self.canonical_hasher
                .take()
                .ok_or_else(PolicyError::identity_mismatch)?
                .finish()?,
        );
        self.actual_subject = Some(CapabilitySubject::from_scene_hash(
            &self.scene,
            self.decision
                .as_ref()
                .ok_or_else(PolicyError::identity_mismatch)?
                .subject()
                .document_revision(),
            scene_hash,
        ));
        let released = render_vec_capacity_bytes(
            self.canonical
                .as_ref()
                .ok_or_else(PolicyError::identity_mismatch)?,
        )?;
        self.canonical = None;
        self.stats.release(released)?;
        self.phase = RenderPlanJobPhase::Initialize;
        Ok(false)
    }

    fn step_initialize(&mut self) -> Result<bool, PolicyError> {
        let decision = self
            .decision
            .as_ref()
            .ok_or_else(PolicyError::identity_mismatch)?;
        let actual_subject = self
            .actual_subject
            .ok_or_else(PolicyError::identity_mismatch)?;
        if actual_subject != decision.subject()
            || decision.missing_total() != 0
            || !decision.missing().is_empty()
            || decision.rejection_code().is_some()
        {
            return Err(PolicyError::identity_mismatch());
        }
        ensure_plan_dimensions(self.request.clip(), self.limits)?;
        let geometry_hash = hash_geometry(self.scene.geometry())?;
        self.viewport = Some(ViewportIdentity {
            generation: self.request.generation(),
            geometry_hash,
            clip: self.request.clip(),
            zoom: self.request.zoom(),
            device_scale_milli: self.request.device_scale_milli(),
            rotation: self.request.rotation(),
            optional_content: self.request.optional_content(),
            annotation_revision: self.request.annotation_revision(),
        });
        let (tile_width, tile_height) = self.config.tile_size();
        self.columns = ceil_div(self.request.clip().width(), tile_width)?;
        self.rows = ceil_div(self.request.clip().height(), tile_height)?;
        self.tile_count = self
            .columns
            .checked_mul(self.rows)
            .ok_or_else(PolicyError::numeric_overflow)?;
        if self.tile_count > self.limits.max_tiles() {
            return Err(PolicyError::resource(
                PolicyLimitKind::Tiles,
                u64::from(self.limits.max_tiles()),
                0,
                u64::from(self.tile_count),
            ));
        }
        self.phase = RenderPlanJobPhase::AllocateContent;
        Ok(false)
    }

    fn step_allocate_content(&mut self) -> Result<bool, PolicyError> {
        render_reserve_job_vec(
            &mut self.content_keys,
            usize::try_from(self.tile_count).map_err(|_| PolicyError::numeric_overflow())?,
            self.job_limits,
            &mut self.stats,
        )?;
        self.phase = RenderPlanJobPhase::BuildContent;
        Ok(false)
    }

    fn step_build_content(&mut self) -> Result<bool, PolicyError> {
        if self.tile_index == self.tile_count {
            self.phase = RenderPlanJobPhase::AllocateManifest;
            return Ok(false);
        }
        let subject = self
            .actual_subject
            .ok_or_else(PolicyError::identity_mismatch)?;
        let viewport = self.viewport.ok_or_else(PolicyError::identity_mismatch)?;
        let row = self.tile_index / self.columns;
        let column = self.tile_index % self.columns;
        let (tile_width, tile_height) = self.config.tile_size();
        let x_offset = column
            .checked_mul(tile_width)
            .ok_or_else(PolicyError::numeric_overflow)?;
        let y_offset = row
            .checked_mul(tile_height)
            .ok_or_else(PolicyError::numeric_overflow)?;
        let tile = DeviceRect::new(
            add_unsigned_i32(self.request.clip().x(), x_offset)?,
            add_unsigned_i32(self.request.clip().y(), y_offset)?,
            self.request
                .clip()
                .width()
                .checked_sub(x_offset)
                .ok_or_else(PolicyError::numeric_overflow)?
                .min(tile_width),
            self.request
                .clip()
                .height()
                .checked_sub(y_offset)
                .ok_or_else(PolicyError::numeric_overflow)?
                .min(tile_height),
        )?;
        let mut key = TileContentKey {
            source: subject.source(),
            document_revision: subject.document_revision(),
            revision_startxref: subject.revision_startxref(),
            page_index: subject.page_index(),
            page_object_number: subject.page_object_number(),
            page_object_generation: subject.page_object_generation(),
            scene_hash: subject.scene_hash(),
            decision_hash: self
                .decision
                .as_ref()
                .ok_or_else(PolicyError::identity_mismatch)?
                .hash(),
            geometry_hash: viewport.geometry_hash(),
            viewport_clip: self.request.clip(),
            zoom: self.request.zoom(),
            device_scale_milli: self.request.device_scale_milli(),
            rotation: self.request.rotation(),
            optional_content: self.request.optional_content(),
            annotation_revision: self.request.annotation_revision(),
            tile,
            quality: self.config.quality(),
            output_profile: self.config.output_profile(),
            render_config_hash: self.config.hash(),
            renderer_epoch: self.renderer_epoch,
            backend: self.config.backend(),
            hash: TileContentHash::new([0; 32]),
        };
        key.hash = TileContentHash::new(hash_tile_content(&key)?);
        self.content_keys.push(key);
        self.tile_index = self
            .tile_index
            .checked_add(1)
            .ok_or_else(PolicyError::numeric_overflow)?;
        Ok(false)
    }

    fn step_allocate_manifest(&mut self) -> Result<bool, PolicyError> {
        let capacity =
            usize::try_from(self.tile_count).map_err(|_| PolicyError::numeric_overflow())?;
        render_reserve_job_vec(
            &mut self.manifest_regions,
            capacity,
            self.job_limits,
            &mut self.stats,
        )?;
        render_reserve_job_vec(
            &mut self.manifest_hashes,
            capacity,
            self.job_limits,
            &mut self.stats,
        )?;
        self.phase = RenderPlanJobPhase::BuildManifest;
        Ok(false)
    }

    fn step_build_manifest(&mut self) -> Result<bool, PolicyError> {
        if let Some(tile) = self.content_keys.get(self.manifest_index) {
            let rectangle = tile.tile();
            self.manifest_regions.push(pdf_rs_protocol::SurfaceRegion {
                page_index: tile.page_index(),
                x: rectangle.x(),
                y: rectangle.y(),
                width: rectangle.width(),
                height: rectangle.height(),
                coordinate_space: pdf_rs_protocol::SurfaceCoordinateSpace::DevicePixelsTopLeft,
            });
            self.manifest_hashes
                .push(pdf_rs_protocol::TileContentHash::new(
                    tile.hash().into_digest(),
                ));
            self.manifest_index = self
                .manifest_index
                .checked_add(1)
                .ok_or_else(PolicyError::numeric_overflow)?;
            return Ok(false);
        }
        let decision = self
            .decision
            .as_ref()
            .ok_or_else(PolicyError::identity_mismatch)?;
        let subject = decision.subject();
        let viewport = self.viewport.ok_or_else(PolicyError::identity_mismatch)?;
        let clip = viewport.clip();
        let manifest = pdf_rs_protocol::RenderPlanManifest {
            plan_schema_version: RENDER_PLAN_SCHEMA_VERSION,
            document_revision: subject.document_revision(),
            render_config: pdf_rs_protocol::RenderConfigHash::new(self.config.hash().into_digest()),
            renderer_epoch: pdf_rs_protocol::RendererEpoch::new(self.renderer_epoch.value()),
            plan_id: RenderPlanId::new(self.request.generation()),
            generation: self.request.generation(),
            scene_hash: pdf_rs_protocol::SceneHash::new(subject.scene_hash().into_digest()),
            decision_hash: pdf_rs_protocol::CapabilityDecisionHash::new(
                decision.hash().into_digest(),
            ),
            geometry_hash: pdf_rs_protocol::GeometryHash::new(
                viewport.geometry_hash().into_digest(),
            ),
            viewport_clip: pdf_rs_protocol::SurfaceRegion {
                page_index: subject.page_index(),
                x: clip.x(),
                y: clip.y(),
                width: clip.width(),
                height: clip.height(),
                coordinate_space: pdf_rs_protocol::SurfaceCoordinateSpace::DevicePixelsTopLeft,
            },
            zoom_numerator: viewport.zoom().numerator(),
            zoom_denominator: viewport.zoom().denominator(),
            device_scale_milli: viewport.device_scale_milli(),
            rotation: protocol_page_rotation(viewport.rotation()),
            optional_content: viewport.optional_content().value(),
            annotation_revision: viewport.annotation_revision(),
            backend: match self.config.backend() {
                NativeBackend::ReferenceCpu => pdf_rs_protocol::NativeBackend::ReferenceCpu,
                NativeBackend::FastCpu => pdf_rs_protocol::NativeBackend::FastCpu,
            },
            output_profile: pdf_rs_protocol::OutputProfile::Srgb,
            quality: match self.config.quality() {
                QualityPolicy::Preview => pdf_rs_protocol::QualityPolicy::Preview,
                QualityPolicy::Full => pdf_rs_protocol::QualityPolicy::Full,
            },
            regions: std::mem::take(&mut self.manifest_regions),
            tile_content_hashes: std::mem::take(&mut self.manifest_hashes),
        };
        if !manifest.wire_invariants_valid() {
            return Err(PolicyError::identity_mismatch());
        }
        self.manifest = Some(manifest);
        self.phase = RenderPlanJobPhase::EncodeManifest;
        Ok(false)
    }

    fn step_encode_manifest(
        &mut self,
        cancellation: &dyn PolicyCancellation,
    ) -> Result<bool, PolicyError> {
        let estimated = u64::from(self.tile_count)
            .checked_mul(256)
            .and_then(|value| value.checked_add(4 * 1024))
            .ok_or_else(PolicyError::numeric_overflow)?;
        if estimated
            > self
                .job_limits
                .max_retained_bytes()
                .saturating_sub(self.stats.retained_bytes())
        {
            return Err(PolicyError::resource(
                PolicyLimitKind::JobRetainedBytes,
                self.job_limits.max_retained_bytes(),
                self.stats.retained_bytes(),
                estimated,
            ));
        }
        let mut work = CancellationWork::new(cancellation, self.limits.cancellation_interval())?;
        let preimage = crate::protocol_projection::render_plan_manifest_hash_preimage(
            self.manifest
                .as_ref()
                .ok_or_else(PolicyError::identity_mismatch)?,
            &mut work,
        )?;
        let retained = render_vec_capacity_bytes(&preimage)?;
        self.stats.charge_allocation(retained, self.job_limits)?;
        self.manifest_preimage = Some(preimage);
        self.manifest_hasher = Some(PreimageHasher::new());
        self.phase = RenderPlanJobPhase::HashManifest;
        Ok(false)
    }

    fn step_hash_manifest(&mut self) -> Result<bool, PolicyError> {
        let preimage = self
            .manifest_preimage
            .as_ref()
            .ok_or_else(PolicyError::identity_mismatch)?;
        if let Some(chunk) = preimage
            .get(self.manifest_preimage_offset..)
            .and_then(|remaining| remaining.chunks(4 * 1024).next())
        {
            self.manifest_hasher
                .as_mut()
                .ok_or_else(PolicyError::identity_mismatch)?
                .update(chunk)?;
            self.manifest_preimage_offset = self
                .manifest_preimage_offset
                .checked_add(chunk.len())
                .ok_or_else(PolicyError::numeric_overflow)?;
            return Ok(false);
        }
        self.plan_hash = Some(RenderPlanHash::new(
            self.manifest_hasher
                .take()
                .ok_or_else(PolicyError::identity_mismatch)?
                .finish()?,
        ));
        let released = render_vec_capacity_bytes(
            self.manifest_preimage
                .as_ref()
                .ok_or_else(PolicyError::identity_mismatch)?,
        )?;
        self.manifest_preimage = None;
        self.stats.release(released)?;
        self.phase = RenderPlanJobPhase::AllocateTiles;
        Ok(false)
    }

    fn step_allocate_tiles(&mut self) -> Result<bool, PolicyError> {
        render_reserve_job_vec(
            &mut self.tiles,
            usize::try_from(self.tile_count).map_err(|_| PolicyError::numeric_overflow())?,
            self.job_limits,
            &mut self.stats,
        )?;
        self.phase = RenderPlanJobPhase::BuildTiles;
        Ok(false)
    }

    fn step_build_tiles(&mut self) -> Result<bool, PolicyError> {
        let Some(content_key) = self.content_keys.get(self.planned_tile_index).cloned() else {
            let released = render_vec_capacity_bytes(&self.content_keys)?;
            self.content_keys = Vec::new();
            self.stats.release(released)?;
            self.phase = RenderPlanJobPhase::Publish;
            return Ok(false);
        };
        let ordinal =
            u32::try_from(self.planned_tile_index).map_err(|_| PolicyError::numeric_overflow())?;
        let id = RenderPlanId::new(self.request.generation());
        let plan_hash = self.plan_hash.ok_or_else(PolicyError::identity_mismatch)?;
        let hash = PlannedTileHash::new(hash_planned_tile(
            ordinal,
            &content_key,
            self.request.generation(),
            id,
            plan_hash,
        )?);
        self.tiles.push(PlannedTileIdentity {
            ordinal,
            content_key,
            generation: self.request.generation(),
            plan_id: id,
            plan_hash,
            hash,
        });
        self.planned_tile_index = self
            .planned_tile_index
            .checked_add(1)
            .ok_or_else(PolicyError::numeric_overflow)?;
        Ok(false)
    }

    fn step_publish(&mut self) -> Result<bool, PolicyError> {
        if self.tiles.len()
            != usize::try_from(self.tile_count).map_err(|_| PolicyError::numeric_overflow())?
        {
            return Err(PolicyError::identity_mismatch());
        }
        let plan = RenderPlan {
            schema_version: RENDER_PLAN_SCHEMA_VERSION,
            id: RenderPlanId::new(self.request.generation()),
            hash: self.plan_hash.ok_or_else(PolicyError::identity_mismatch)?,
            decision: self
                .decision
                .take()
                .ok_or_else(PolicyError::identity_mismatch)?,
            viewport: self.viewport.ok_or_else(PolicyError::identity_mismatch)?,
            config: self.config,
            renderer_epoch: self.renderer_epoch,
            manifest: self
                .manifest
                .take()
                .ok_or_else(PolicyError::identity_mismatch)?,
            tiles: std::mem::take(&mut self.tiles),
        };
        self.terminal = Some(Ok(RenderPlanOutcome::Ready(plan)));
        Ok(true)
    }

    fn fail(&mut self, error: PolicyError) {
        self.canonical = None;
        self.content_keys = Vec::new();
        self.manifest_regions = Vec::new();
        self.manifest_hashes = Vec::new();
        self.manifest = None;
        self.manifest_preimage = None;
        self.tiles = Vec::new();
        self.stats.clear_retained();
        self.terminal = Some(Err(error));
    }
}

impl fmt::Debug for RenderPlanJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderPlanJob")
            .field("request", &self.request)
            .field("renderer_epoch", &self.renderer_epoch)
            .field("job_limits", &self.job_limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase)
            .field("terminal", &self.terminal.as_ref().map(Result::is_ok))
            .field("scene", &"[REDACTED]")
            .field("working_bytes", &"[REDACTED]")
            .finish()
    }
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
    let scene = Arc::new(scene.clone());
    let job_limits =
        PolicyJobLimits::synchronous_compatibility(canonical_scene_upper_bound(&scene), u64::MAX);
    let mut job = RenderPlanJob::new_compatibility(
        scene,
        decision,
        config,
        request,
        renderer_epoch,
        limits,
        job_limits,
    );
    let budget = PolicyPollBudget::new(
        NonZeroU32::new(4_096).expect("fixed synchronous poll budget is nonzero"),
    )?;
    loop {
        match job.poll(budget, cancellation) {
            PolicyJobPoll::Pending => {}
            PolicyJobPoll::Ready => {
                return job
                    .take_result()
                    .ok_or_else(PolicyError::identity_mismatch)?;
            }
        }
    }
}

fn render_vec_capacity_bytes<T>(values: &Vec<T>) -> Result<u64, PolicyError> {
    u64::try_from(values.capacity())
        .ok()
        .and_then(|count| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(PolicyError::numeric_overflow)
}

fn render_reserve_job_vec<T>(
    values: &mut Vec<T>,
    capacity: usize,
    limits: PolicyJobLimits,
    stats: &mut PolicyJobStats,
) -> Result<(), PolicyError> {
    if capacity == 0 {
        return Ok(());
    }
    let requested = u64::try_from(capacity)
        .ok()
        .and_then(|count| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(PolicyError::numeric_overflow)?;
    if requested
        > limits
            .max_retained_bytes()
            .saturating_sub(stats.retained_bytes())
    {
        return Err(PolicyError::resource(
            PolicyLimitKind::JobRetainedBytes,
            limits.max_retained_bytes(),
            stats.retained_bytes(),
            requested,
        ));
    }
    values
        .try_reserve_exact(capacity)
        .map_err(|_| PolicyError::allocation())?;
    stats.charge_allocation(render_vec_capacity_bytes(values)?, limits)
}

const fn protocol_page_rotation(rotation: PageRotation) -> pdf_rs_protocol::PageRotation {
    match rotation {
        PageRotation::Degrees0 => pdf_rs_protocol::PageRotation::Degrees0,
        PageRotation::Degrees90 => pdf_rs_protocol::PageRotation::Degrees90,
        PageRotation::Degrees180 => pdf_rs_protocol::PageRotation::Degrees180,
        PageRotation::Degrees270 => pdf_rs_protocol::PageRotation::Degrees270,
    }
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

#[cfg(test)]
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
