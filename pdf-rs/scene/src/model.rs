use std::fmt;
use std::sync::Arc;

use pdf_rs_bytes::SourceIdentity;
use pdf_rs_syntax::ObjectRef;

use crate::{GraphicsScene, SceneError, SceneErrorCode, SceneLimits, SceneScalar};

/// Observer for bounded canonical Scene serialization.
///
/// The serializer calls this before appending each bounded output fragment. Returning `false`
/// interrupts serialization with [`SceneErrorCode::CanonicalizationInterrupted`] and publishes no
/// canonical byte vector. Observers must not retain the borrowed fragment.
pub trait SceneCanonicalObserver {
    /// Returns whether serialization may append the next fragment.
    fn observe(&mut self, next_fragment: &[u8]) -> bool;
}

/// Version of the immutable Scene schema.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SceneVersion {
    major: u16,
    minor: u16,
}

impl SceneVersion {
    /// Initial incompatible/compatible Scene schema pair.
    pub const V1_0: Self = Self { major: 1, minor: 0 };
    /// Graphics-capable incompatible Scene schema generation.
    pub const V2_0: Self = Self { major: 2, minor: 0 };

    /// Returns the incompatible schema generation.
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the backwards-compatible schema revision.
    pub const fn minor(self) -> u16 {
        self.minor
    }
}

/// Runtime binding from one Scene to its immutable document source and page.
///
/// The source identity remains available for runtime cache and stale-result checks but is omitted
/// from canonical Scene output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneBinding {
    source: SourceIdentity,
    revision_startxref: u64,
    page_index: u32,
    page_object: ObjectRef,
}

impl SceneBinding {
    /// Creates one runtime Scene binding.
    pub const fn new(
        source: SourceIdentity,
        revision_startxref: u64,
        page_index: u32,
        page_object: ObjectRef,
    ) -> Self {
        Self {
            source,
            revision_startxref,
            page_index,
            page_object,
        }
    }

    /// Returns the immutable runtime source identity.
    pub const fn source(self) -> SourceIdentity {
        self.source
    }

    /// Returns the revision `startxref` anchor.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns the zero-based logical page index.
    pub const fn page_index(self) -> u32 {
        self.page_index
    }

    /// Returns the exact indirect Page object identity.
    pub const fn page_object(self) -> ObjectRef {
        self.page_object
    }
}

/// Positive-area rectangle in PDF user-space coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneRect {
    coordinates: [SceneScalar; 4],
}

impl SceneRect {
    /// Creates `[left, bottom, right, top]` with positive representable width and height.
    pub fn new(coordinates: [SceneScalar; 4]) -> Result<Self, SceneError> {
        if coordinates[2] <= coordinates[0] || coordinates[3] <= coordinates[1] {
            return Err(SceneError::for_code(SceneErrorCode::InvalidGeometry, None));
        }
        coordinates[2].checked_sub(coordinates[0])?;
        coordinates[3].checked_sub(coordinates[1])?;
        Ok(Self { coordinates })
    }

    /// Returns `[left, bottom, right, top]`.
    pub const fn coordinates(self) -> [SceneScalar; 4] {
        self.coordinates
    }

    /// Returns the checked rectangle width.
    pub fn width(self) -> Result<SceneScalar, SceneError> {
        self.coordinates[2].checked_sub(self.coordinates[0])
    }

    /// Returns the checked rectangle height.
    pub fn height(self) -> Result<SceneScalar, SceneError> {
        self.coordinates[3].checked_sub(self.coordinates[1])
    }
}

/// Canonical clockwise page rotation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PageRotation {
    /// No rotation.
    Degrees0,
    /// Clockwise quarter turn.
    Degrees90,
    /// Clockwise half turn.
    Degrees180,
    /// Clockwise three-quarter turn.
    Degrees270,
}

impl PageRotation {
    /// Returns the canonical nonnegative degree value.
    pub const fn degrees(self) -> u16 {
        match self {
            Self::Degrees0 => 0,
            Self::Degrees90 => 90,
            Self::Degrees180 => 180,
            Self::Degrees270 => 270,
        }
    }
}

/// Immutable page geometry retained by a Scene.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageGeometry {
    media_box: SceneRect,
    crop_box: SceneRect,
    rotation: PageRotation,
}

impl PageGeometry {
    /// Creates page geometry from already validated boxes and canonical rotation.
    pub const fn new(media_box: SceneRect, crop_box: SceneRect, rotation: PageRotation) -> Self {
        Self {
            media_box,
            crop_box,
            rotation,
        }
    }

    /// Returns the inherited MediaBox.
    pub const fn media_box(self) -> SceneRect {
        self.media_box
    }

    /// Returns the inherited CropBox.
    pub const fn crop_box(self) -> SceneRect {
        self.crop_box
    }

    /// Returns the canonical clockwise rotation.
    pub const fn rotation(self) -> PageRotation {
        self.rotation
    }
}

/// Stable Scene resource identifier assigned by first canonical command use.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceId(u32);

impl ResourceId {
    pub(crate) const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the zero-based stable resource identifier.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Resource kind supported by the Scene v1 foundation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SceneResourceKind {
    /// A Page resource dictionary entry used by marked-content properties.
    MarkedContentProperties,
}

/// One stable source object admitted to the Scene resource table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneResource {
    id: ResourceId,
    kind: SceneResourceKind,
    object: ObjectRef,
}

impl SceneResource {
    pub(crate) const fn marked_content_properties(id: ResourceId, object: ObjectRef) -> Self {
        Self {
            id,
            kind: SceneResourceKind::MarkedContentProperties,
            object,
        }
    }

    /// Returns the stable Scene-local identifier.
    pub const fn id(self) -> ResourceId {
        self.id
    }

    /// Returns the semantic resource kind.
    pub const fn kind(self) -> SceneResourceKind {
        self.kind
    }

    /// Returns the exact defining PDF object identity.
    pub const fn object(self) -> ObjectRef {
        self.object
    }
}

/// Bounded decoded PDF name retained by one Scene command.
#[derive(Clone, Eq, PartialEq)]
pub struct SceneName {
    bytes: Vec<u8>,
}

impl SceneName {
    pub(crate) fn copy_from(
        bytes: &[u8],
        max_bytes: u32,
        command_index: Option<u32>,
    ) -> Result<Self, SceneError> {
        let attempted = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if attempted > u64::from(max_bytes) {
            return Err(SceneError::resource(
                crate::SceneLimitKind::NameBytes,
                u64::from(max_bytes),
                0,
                attempted,
                command_index,
            ));
        }
        let mut owned = Vec::new();
        owned.try_reserve_exact(bytes.len()).map_err(|_| {
            SceneError::resource(
                crate::SceneLimitKind::Allocation,
                u64::from(max_bytes),
                0,
                attempted,
                command_index,
            )
        })?;
        owned.extend_from_slice(bytes);
        Ok(Self { bytes: owned })
    }

    /// Borrows the decoded PDF name bytes without assuming UTF-8.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, SceneError> {
        u64::try_from(self.bytes.capacity())
            .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))
    }
}

impl fmt::Debug for SceneName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SceneName")
            .field("len", &self.bytes.len())
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// Semantic Scene command kind.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SceneCommandKind {
    /// Enter one marked-content sequence.
    BeginMarkedContent,
    /// Leave the most recently entered marked-content sequence.
    EndMarkedContent,
}

/// One minimal semantic Scene v1 command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneCommand {
    kind: SceneCommandKind,
    tag: Option<SceneName>,
    properties: Option<ResourceId>,
}

impl SceneCommand {
    pub(crate) const fn begin(tag: SceneName, properties: Option<ResourceId>) -> Self {
        Self {
            kind: SceneCommandKind::BeginMarkedContent,
            tag: Some(tag),
            properties,
        }
    }

    pub(crate) const fn end() -> Self {
        Self {
            kind: SceneCommandKind::EndMarkedContent,
            tag: None,
            properties: None,
        }
    }

    /// Returns the semantic command kind.
    pub const fn kind(&self) -> SceneCommandKind {
        self.kind
    }

    /// Returns the marked-content tag for a begin command.
    pub fn tag(&self) -> Option<&SceneName> {
        self.tag.as_ref()
    }

    /// Returns the optional marked-content properties resource.
    pub const fn properties(&self) -> Option<ResourceId> {
        self.properties
    }
}

/// Decoded-coordinate provenance for one Scene command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSource {
    object: ObjectRef,
    stream_index: u32,
    decoded_start: u64,
    decoded_length: u64,
    operator_index: u32,
}

impl CommandSource {
    /// Creates source provenance after checking the decoded exclusive end.
    pub fn new(
        object: ObjectRef,
        stream_index: u32,
        decoded_start: u64,
        decoded_length: u64,
        operator_index: u32,
    ) -> Result<Self, SceneError> {
        decoded_start
            .checked_add(decoded_length)
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?;
        Ok(Self {
            object,
            stream_index,
            decoded_start,
            decoded_length,
            operator_index,
        })
    }

    /// Returns the content-stream object containing the operator.
    pub const fn object(self) -> ObjectRef {
        self.object
    }

    /// Returns the zero-based stream ordinal in Page Contents order.
    pub const fn stream_index(self) -> u32 {
        self.stream_index
    }

    /// Returns the decoded-relative operator start.
    pub const fn decoded_start(self) -> u64 {
        self.decoded_start
    }

    /// Returns the decoded-relative operator length.
    pub const fn decoded_length(self) -> u64 {
        self.decoded_length
    }

    /// Returns the zero-based operator ordinal across the interpreted page.
    pub const fn operator_index(self) -> u32 {
        self.operator_index
    }
}

/// Semantic feature observed in one Scene.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SceneFeature {
    /// The Scene contains semantic marked-content commands.
    MarkedContent,
    /// A marked-content command refers to a properties resource.
    MarkedContentProperties,
}

/// Page-level capability decision represented by the Scene.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityDecision {
    /// Every semantic feature in this Scene is supported by the producing profile.
    Supported,
    /// At least one declared Scene capability is outside the producing profile.
    Unsupported,
}

/// Immutable deterministic feature report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureReport {
    decision: CapabilityDecision,
    tags: Arc<Vec<SceneFeature>>,
}

impl FeatureReport {
    pub(crate) fn supported(tags: Vec<SceneFeature>) -> Self {
        Self {
            decision: CapabilityDecision::Supported,
            tags: Arc::new(tags),
        }
    }

    pub(crate) fn with_decision(decision: CapabilityDecision, tags: Vec<SceneFeature>) -> Self {
        Self {
            decision,
            tags: Arc::new(tags),
        }
    }

    /// Returns the page-level capability decision.
    pub const fn decision(&self) -> CapabilityDecision {
        self.decision
    }

    /// Returns sorted semantic feature tags.
    pub fn tags(&self) -> &[SceneFeature] {
        &self.tags
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, SceneError> {
        let capacity = u64::try_from(self.tags.capacity())
            .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        let width = u64::try_from(std::mem::size_of::<SceneFeature>())
            .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        capacity
            .checked_mul(width)
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))
    }
}

/// Deterministic Scene construction and ownership accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SceneStats {
    commands: u32,
    resources: u32,
    max_marked_content_depth: u32,
    retained_bytes: u64,
    resource_index_work: u64,
}

impl SceneStats {
    pub(crate) const fn new(
        commands: u32,
        resources: u32,
        max_marked_content_depth: u32,
        retained_bytes: u64,
        resource_index_work: u64,
    ) -> Self {
        Self {
            commands,
            resources,
            max_marked_content_depth,
            retained_bytes,
            resource_index_work,
        }
    }

    /// Returns the semantic command count.
    pub const fn commands(self) -> u32 {
        self.commands
    }

    /// Returns the stable resource count.
    pub const fn resources(self) -> u32 {
        self.resources
    }

    /// Returns the deepest marked-content nesting reached.
    pub const fn max_marked_content_depth(self) -> u32 {
        self.max_marked_content_depth
    }

    /// Returns allocator-reported retained vector and scalar-buffer capacity.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    /// Returns charged resource-index comparison bounds and insertion shifts.
    pub const fn resource_index_work(self) -> u64 {
        self.resource_index_work
    }
}

/// Immutable, source-bound, backend-neutral Scene v1.
#[derive(Clone, Eq, PartialEq)]
pub struct Scene {
    version: SceneVersion,
    binding: SceneBinding,
    geometry: PageGeometry,
    commands: Arc<Vec<SceneCommand>>,
    resources: Arc<Vec<SceneResource>>,
    features: FeatureReport,
    provenance: Arc<Vec<CommandSource>>,
    limits: SceneLimits,
    stats: SceneStats,
    graphics: Option<GraphicsScene>,
}

impl Scene {
    #[allow(
        clippy::too_many_arguments,
        reason = "the immutable Scene publication boundary validates and stores each semantic section explicitly"
    )]
    pub(crate) fn new(
        binding: SceneBinding,
        geometry: PageGeometry,
        commands: Vec<SceneCommand>,
        resources: Vec<SceneResource>,
        features: FeatureReport,
        provenance: Vec<CommandSource>,
        limits: SceneLimits,
        stats: SceneStats,
    ) -> Self {
        Self {
            version: SceneVersion::V1_0,
            binding,
            geometry,
            commands: Arc::new(commands),
            resources: Arc::new(resources),
            features,
            provenance: Arc::new(provenance),
            limits,
            stats,
            graphics: None,
        }
    }

    pub(crate) fn new_graphics(
        binding: SceneBinding,
        geometry: PageGeometry,
        graphics: GraphicsScene,
    ) -> Self {
        let decision = if graphics.is_supported() {
            CapabilityDecision::Supported
        } else {
            CapabilityDecision::Unsupported
        };
        let stats = SceneStats::new(
            u32::try_from(graphics.commands().len())
                .expect("graphics command count is bounded by a u32 limit"),
            u32::try_from(graphics.resources().len())
                .expect("graphics resource count is bounded by a u32 limit"),
            0,
            graphics.stats().retained_bytes(),
            graphics.stats().resource_index_work(),
        );
        Self {
            version: SceneVersion::V2_0,
            binding,
            geometry,
            commands: Arc::new(Vec::new()),
            resources: Arc::new(Vec::new()),
            features: FeatureReport::with_decision(decision, Vec::new()),
            provenance: Arc::new(Vec::new()),
            limits: SceneLimits::default(),
            stats,
            graphics: Some(graphics),
        }
    }

    /// Returns the Scene schema version.
    pub const fn version(&self) -> SceneVersion {
        self.version
    }

    /// Returns the runtime source/page binding.
    pub const fn binding(&self) -> SceneBinding {
        self.binding
    }

    /// Returns immutable page geometry.
    pub const fn geometry(&self) -> PageGeometry {
        self.geometry
    }

    /// Returns semantic commands in execution order.
    pub fn commands(&self) -> &[SceneCommand] {
        &self.commands
    }

    /// Returns stable resources in identifier order.
    pub fn resources(&self) -> &[SceneResource] {
        &self.resources
    }

    /// Returns the deterministic feature report.
    pub const fn features(&self) -> &FeatureReport {
        &self.features
    }

    /// Returns command provenance paired by command index.
    pub fn provenance(&self) -> &[CommandSource] {
        &self.provenance
    }

    /// Returns the legacy Scene-v1 limit profile.
    ///
    /// Scene v2 callers use [`GraphicsScene::limits`] through [`Scene::graphics`].
    pub const fn limits(&self) -> SceneLimits {
        self.limits
    }

    /// Returns common deterministic construction and retained-capacity accounting.
    ///
    /// Scene v2 reports graphics command/resource counts, retained bytes, and index work here;
    /// graphics-specific dimensions remain available through [`GraphicsScene::stats`].
    pub const fn stats(&self) -> SceneStats {
        self.stats
    }

    /// Borrows Scene v2 graphics semantics, or returns `None` for legacy Scene v1.
    pub const fn graphics(&self) -> Option<&GraphicsScene> {
        self.graphics.as_ref()
    }
}

impl fmt::Debug for Scene {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Scene")
            .field("version", &self.version)
            .field("page_index", &self.binding.page_index)
            .field("page_object", &self.binding.page_object)
            .field("geometry", &self.geometry)
            .field("command_count", &self.commands.len())
            .field("resource_count", &self.resources.len())
            .field("features", &self.features)
            .field("stats", &self.stats)
            .field("has_graphics", &self.graphics.is_some())
            .field("content", &"[REDACTED]")
            .finish()
    }
}
