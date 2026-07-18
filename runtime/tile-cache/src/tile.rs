//! Complete immutable Native tile ownership and deterministic eviction.

use std::error::Error;
use std::fmt;
use std::mem;

use pdf_rs_bytes::SourceIdentity;
use pdf_rs_policy::{PixelFormat, RendererEpoch, TileContentKey};

const HARD_MAX_ENTRIES: u64 = 65_536;
const HARD_MAX_TILE_PIXEL_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_PIXEL_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_RESIDENT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const CANCELLATION_INTERVAL: usize = 64;

/// Opaque identity of the Worker or renderer owner that owns one tile cache.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TileCacheOwnerId(u64);

impl TileCacheOwnerId {
    /// Wraps one runtime-issued owner identity.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric identity.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Opaque identity of the document session that owns one tile cache.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TileCacheSessionId(u64);

impl TileCacheSessionId {
    /// Wraps one runtime-issued session identity.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric identity.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Complete owner and immutable document binding for one product tile cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileCacheBinding {
    owner_id: TileCacheOwnerId,
    session_id: TileCacheSessionId,
    source: SourceIdentity,
    document_revision: u64,
    revision_startxref: u64,
    renderer_epoch: RendererEpoch,
}

impl TileCacheBinding {
    /// Creates a binding from every cache-wide owner and document identity.
    pub const fn new(
        owner_id: TileCacheOwnerId,
        session_id: TileCacheSessionId,
        source: SourceIdentity,
        document_revision: u64,
        revision_startxref: u64,
        renderer_epoch: RendererEpoch,
    ) -> Self {
        Self {
            owner_id,
            session_id,
            source,
            document_revision,
            revision_startxref,
            renderer_epoch,
        }
    }

    /// Creates a binding from one authentic policy-produced tile content key.
    pub fn from_content_key(
        owner_id: TileCacheOwnerId,
        session_id: TileCacheSessionId,
        content_key: &TileContentKey,
    ) -> Self {
        Self::new(
            owner_id,
            session_id,
            content_key.source(),
            content_key.document_revision(),
            content_key.revision_startxref(),
            content_key.renderer_epoch(),
        )
    }

    /// Returns the Worker or renderer owner.
    pub const fn owner_id(self) -> TileCacheOwnerId {
        self.owner_id
    }

    /// Returns the owning document session.
    pub const fn session_id(self) -> TileCacheSessionId {
        self.session_id
    }

    /// Returns the complete immutable source identity.
    pub const fn source(self) -> SourceIdentity {
        self.source
    }

    /// Returns the product document revision.
    pub const fn document_revision(self) -> u64 {
        self.document_revision
    }

    /// Returns the exact xref anchor for the bound revision.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns the nonzero Native renderer epoch.
    pub const fn renderer_epoch(self) -> RendererEpoch {
        self.renderer_epoch
    }
}

/// Complete lookup or admission address for one product tile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TileCacheAddress {
    owner_id: TileCacheOwnerId,
    session_id: TileCacheSessionId,
    content_key: TileContentKey,
}

impl TileCacheAddress {
    /// Binds one authentic complete policy content identity to its runtime owners.
    pub const fn new(
        owner_id: TileCacheOwnerId,
        session_id: TileCacheSessionId,
        content_key: TileContentKey,
    ) -> Self {
        Self {
            owner_id,
            session_id,
            content_key,
        }
    }

    /// Returns the Worker or renderer owner.
    pub const fn owner_id(&self) -> TileCacheOwnerId {
        self.owner_id
    }

    /// Returns the owning document session.
    pub const fn session_id(&self) -> TileCacheSessionId {
        self.session_id
    }

    /// Borrows the complete policy-produced tile content key.
    pub const fn content_key(&self) -> &TileContentKey {
        &self.content_key
    }
}

/// Successful complete immutable Native tile pixels.
///
/// The constructor validates exact tightly packed RGBA8 extent. Pixel storage
/// remains private and can only be borrowed immutably after construction.
pub struct NativeTile {
    content_key: TileContentKey,
    stride: u32,
    pixels: Vec<u8>,
    pixel_capacity_bytes: u64,
}

impl NativeTile {
    /// Validates and seals tightly packed pixels for one complete policy key.
    pub fn try_new(content_key: TileContentKey, pixels: Vec<u8>) -> Result<Self, TileCacheError> {
        let bytes_per_pixel = match content_key.output_profile().format() {
            PixelFormat::Rgba8 => 4_u32,
        };
        let tile = content_key.tile();
        let stride = tile
            .width()
            .checked_mul(bytes_per_pixel)
            .ok_or_else(TileCacheError::invalid_tile)?;
        let required_bytes = u64::from(stride)
            .checked_mul(u64::from(tile.height()))
            .ok_or_else(TileCacheError::invalid_tile)?;
        let actual_bytes =
            u64::try_from(pixels.len()).map_err(|_| TileCacheError::invalid_tile())?;
        let pixel_capacity_bytes =
            u64::try_from(pixels.capacity()).map_err(|_| TileCacheError::invalid_tile())?;
        if actual_bytes != required_bytes || pixel_capacity_bytes < actual_bytes {
            return Err(TileCacheError::invalid_tile());
        }
        Ok(Self {
            content_key,
            stride,
            pixels,
            pixel_capacity_bytes,
        })
    }

    /// Borrows the complete generation-independent product identity.
    pub const fn content_key(&self) -> &TileContentKey {
        &self.content_key
    }

    /// Returns the validated tightly packed row stride.
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// Returns the exact initialized pixel byte length.
    pub fn pixel_bytes(&self) -> u64 {
        u64::try_from(self.pixels.len())
            .expect("validated Native tile pixel length always fits in u64")
    }

    /// Returns allocator-reported pixel capacity charged to the cache owner.
    pub const fn pixel_capacity_bytes(&self) -> u64 {
        self.pixel_capacity_bytes
    }

    /// Borrows immutable tightly packed pixel bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

impl fmt::Debug for NativeTile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeTile")
            .field("content_key", &self.content_key)
            .field("stride", &self.stride)
            .field("pixel_bytes", &self.pixel_bytes())
            .field("pixel_capacity_bytes", &self.pixel_capacity_bytes)
            .field("pixels", &"[REDACTED]")
            .finish()
    }
}

/// Terminal producer outcome presented to tile-cache admission.
pub enum TileRenderOutcome {
    /// One successful complete immutable Native tile.
    Complete(NativeTile),
    /// Rendering stopped before a complete atomic tile existed.
    Incomplete,
    /// Product capability policy did not support the requested tile.
    Unsupported,
    /// The producing render operation was cancelled.
    Cancelled,
    /// The producing render operation failed.
    Failed,
    /// The immutable source identity changed before completion.
    SourceChanged,
}

impl TileRenderOutcome {
    /// Returns the stable terminal outcome kind without exposing pixels.
    pub const fn kind(&self) -> TileOutcomeKind {
        match self {
            Self::Complete(_) => TileOutcomeKind::Complete,
            Self::Incomplete => TileOutcomeKind::Incomplete,
            Self::Unsupported => TileOutcomeKind::Unsupported,
            Self::Cancelled => TileOutcomeKind::Cancelled,
            Self::Failed => TileOutcomeKind::Failed,
            Self::SourceChanged => TileOutcomeKind::SourceChanged,
        }
    }
}

impl fmt::Debug for TileRenderOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Complete(_) => formatter
                .debug_tuple("Complete")
                .field(&"[REDACTED]")
                .finish(),
            Self::Incomplete => formatter.write_str("Incomplete"),
            Self::Unsupported => formatter.write_str("Unsupported"),
            Self::Cancelled => formatter.write_str("Cancelled"),
            Self::Failed => formatter.write_str("Failed"),
            Self::SourceChanged => formatter.write_str("SourceChanged"),
        }
    }
}

/// Stable kind of a producer terminal outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileOutcomeKind {
    /// Complete immutable Native pixels.
    Complete,
    /// No complete atomic tile.
    Incomplete,
    /// Product capability policy did not support the work.
    Unsupported,
    /// Producer cancellation.
    Cancelled,
    /// Producer failure.
    Failed,
    /// Immutable source changed.
    SourceChanged,
}

/// Admission segment controlling deterministic eviction preference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileRetentionClass {
    /// Tile belongs to the currently protected viewport.
    ProtectedViewport,
    /// Tile is retained only by bounded recent-use policy.
    RecentUse,
}

/// Cooperative cancellation observed by bounded tile-cache scans.
pub trait TileCacheCancellation {
    /// Reports whether the current operation must stop before publication.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation source that never abandons a tile-cache operation.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancelledTileCache;

impl TileCacheCancellation for NeverCancelledTileCache {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Unvalidated deterministic limits for one product tile cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileCacheLimitConfig {
    /// Maximum logical tile entries retained at once.
    pub max_entries: u64,
    /// Maximum allocator pixel capacity accepted for one tile.
    pub max_tile_pixel_bytes: u64,
    /// Maximum aggregate allocator pixel capacity.
    pub max_pixel_bytes: u64,
    /// Maximum fixed metadata plus aggregate pixel capacity.
    pub max_resident_bytes: u64,
}

impl Default for TileCacheLimitConfig {
    fn default() -> Self {
        Self {
            max_entries: 256,
            max_tile_pixel_bytes: 64 * 1024 * 1024,
            max_pixel_bytes: 256 * 1024 * 1024,
            max_resident_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Validated tile-cache limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileCacheLimits {
    max_entries: usize,
    max_tile_pixel_bytes: u64,
    max_pixel_bytes: u64,
    max_resident_bytes: u64,
}

impl TileCacheLimits {
    /// Validates a complete product tile-cache budget.
    pub fn validate(config: TileCacheLimitConfig) -> Result<Self, TileCacheError> {
        let max_entries = usize::try_from(config.max_entries).ok();
        if config.max_entries == 0
            || config.max_entries > HARD_MAX_ENTRIES
            || max_entries.is_none()
            || config.max_tile_pixel_bytes == 0
            || config.max_tile_pixel_bytes > HARD_MAX_TILE_PIXEL_BYTES
            || config.max_pixel_bytes == 0
            || config.max_pixel_bytes > HARD_MAX_PIXEL_BYTES
            || config.max_resident_bytes == 0
            || config.max_resident_bytes > HARD_MAX_RESIDENT_BYTES
            || config.max_tile_pixel_bytes > config.max_pixel_bytes
            || config.max_pixel_bytes > config.max_resident_bytes
        {
            return Err(TileCacheError::for_code(TileCacheErrorCode::InvalidLimits));
        }
        let Some(max_entries) = max_entries else {
            return Err(TileCacheError::for_code(TileCacheErrorCode::InvalidLimits));
        };
        Ok(Self {
            max_entries,
            max_tile_pixel_bytes: config.max_tile_pixel_bytes,
            max_pixel_bytes: config.max_pixel_bytes,
            max_resident_bytes: config.max_resident_bytes,
        })
    }

    /// Returns the maximum logical resident tile count.
    pub fn max_entries(self) -> u64 {
        u64::try_from(self.max_entries).expect("validated tile entry ceiling always fits in u64")
    }

    /// Returns the maximum allocator capacity accepted for one tile.
    pub const fn max_tile_pixel_bytes(self) -> u64 {
        self.max_tile_pixel_bytes
    }

    /// Returns the aggregate allocator pixel-capacity ceiling.
    pub const fn max_pixel_bytes(self) -> u64 {
        self.max_pixel_bytes
    }

    /// Returns the fixed metadata plus pixel-capacity ceiling.
    pub const fn max_resident_bytes(self) -> u64 {
        self.max_resident_bytes
    }
}

impl Default for TileCacheLimits {
    fn default() -> Self {
        Self::validate(TileCacheLimitConfig::default())
            .expect("built-in tile-cache limits satisfy hard ceilings")
    }
}

/// Resource owner charged by one tile-cache budget decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileCacheScope {
    owner_id: TileCacheOwnerId,
    session_id: TileCacheSessionId,
}

impl TileCacheScope {
    /// Returns the charged Worker or renderer owner.
    pub const fn owner_id(self) -> TileCacheOwnerId {
        self.owner_id
    }

    /// Returns the charged document session.
    pub const fn session_id(self) -> TileCacheSessionId {
        self.session_id
    }
}

/// Deterministic tile-cache budget dimension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileCacheLimitKind {
    /// Fixed preallocated entry metadata.
    MetadataBytes,
    /// Allocator pixel capacity of one incoming tile.
    TilePixelBytes,
    /// Aggregate allocator pixel capacity.
    PixelBytes,
    /// Fixed metadata plus aggregate allocator pixel capacity.
    ResidentBytes,
    /// Fallible fixed metadata allocation.
    Allocation,
}

/// Structured tile-cache resource-limit context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileCacheLimit {
    kind: TileCacheLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
    scope: TileCacheScope,
}

impl TileCacheLimit {
    /// Returns the rejected budget dimension.
    pub const fn kind(self) -> TileCacheLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the amount retained before the rejected operation.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the amount the rejected operation would add or allocate.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }

    /// Returns the owner and session charged by the decision.
    pub const fn scope(self) -> TileCacheScope {
        self.scope
    }
}

/// Stable machine-readable tile-cache failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileCacheErrorCode {
    /// Limits are zero, inconsistent, or above fixed ceilings.
    InvalidLimits,
    /// Fixed entry metadata allocation failed.
    Allocation,
    /// A deterministic retained-resource budget was exhausted.
    ResourceLimit,
    /// The caller cancelled a cache operation before publication.
    Cancelled,
    /// Pixel extent, length, or capacity is inconsistent with its content key.
    InvalidTile,
    /// A checked cache invariant could not be maintained.
    InternalState,
}

/// Coarse tile-cache failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileCacheErrorCategory {
    /// Invalid caller-owned configuration or tile construction.
    Configuration,
    /// Deterministic resource exhaustion.
    Resource,
    /// Normal cooperative cancellation.
    Cancellation,
    /// Internal checked-state failure.
    Internal,
}

/// Stable recovery policy for a tile-cache failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileCacheRecoverability {
    /// Correct configuration or tile construction before retrying.
    CorrectConfiguration,
    /// Reduce retained work or select an approved larger budget.
    ReduceWorkload,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not approved.
    DoNotRetry,
}

/// Source-redacted tile-cache error with stable policy metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileCacheError {
    code: TileCacheErrorCode,
    category: TileCacheErrorCategory,
    recoverability: TileCacheRecoverability,
    diagnostic_id: &'static str,
    limit: Option<TileCacheLimit>,
}

impl TileCacheError {
    const fn for_code(code: TileCacheErrorCode) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            TileCacheErrorCode::InvalidLimits => (
                TileCacheErrorCategory::Configuration,
                TileCacheRecoverability::CorrectConfiguration,
                "RPE-CACHE-0101",
            ),
            TileCacheErrorCode::Allocation => (
                TileCacheErrorCategory::Resource,
                TileCacheRecoverability::ReduceWorkload,
                "RPE-CACHE-0102",
            ),
            TileCacheErrorCode::ResourceLimit => (
                TileCacheErrorCategory::Resource,
                TileCacheRecoverability::ReduceWorkload,
                "RPE-CACHE-0103",
            ),
            TileCacheErrorCode::Cancelled => (
                TileCacheErrorCategory::Cancellation,
                TileCacheRecoverability::AbandonOperation,
                "RPE-CACHE-0104",
            ),
            TileCacheErrorCode::InvalidTile => (
                TileCacheErrorCategory::Configuration,
                TileCacheRecoverability::CorrectConfiguration,
                "RPE-CACHE-0105",
            ),
            TileCacheErrorCode::InternalState => (
                TileCacheErrorCategory::Internal,
                TileCacheRecoverability::DoNotRetry,
                "RPE-CACHE-0106",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            limit: None,
        }
    }

    const fn invalid_tile() -> Self {
        Self::for_code(TileCacheErrorCode::InvalidTile)
    }

    const fn with_limit(
        kind: TileCacheLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        scope: TileCacheScope,
    ) -> Self {
        Self {
            limit: Some(TileCacheLimit {
                kind,
                limit,
                consumed,
                attempted,
                scope,
            }),
            ..Self::for_code(TileCacheErrorCode::ResourceLimit)
        }
    }

    const fn allocation(limit: u64, attempted: u64, scope: TileCacheScope) -> Self {
        Self {
            limit: Some(TileCacheLimit {
                kind: TileCacheLimitKind::Allocation,
                limit,
                consumed: 0,
                attempted,
                scope,
            }),
            ..Self::for_code(TileCacheErrorCode::Allocation)
        }
    }

    /// Returns the stable machine-readable code.
    pub const fn code(self) -> TileCacheErrorCode {
        self.code
    }

    /// Returns the coarse failure category.
    pub const fn category(self) -> TileCacheErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> TileCacheRecoverability {
        self.recoverability
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns structured deterministic limit context, when applicable.
    pub const fn limit(self) -> Option<TileCacheLimit> {
        self.limit
    }
}

impl fmt::Display for TileCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)
    }
}

impl Error for TileCacheError {}

/// Reason a complete tile-cache lookup did not hit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileCacheMissReason {
    /// The cache is closed and owns no current resources.
    Closed,
    /// The address names a different Worker or renderer owner.
    ForeignOwner,
    /// The address names a different document session.
    ForeignSession,
    /// The immutable source identity differs.
    SourceMismatch,
    /// The product document revision or xref anchor differs.
    StaleRevision,
    /// The Native renderer epoch differs.
    StaleRendererEpoch,
    /// No exact complete content key is resident.
    NotFound,
}

/// Borrowed result of one cancellation-aware tile lookup.
pub enum TileCacheLookup<'cache> {
    /// The exact immutable complete Native tile is resident.
    Hit(&'cache NativeTile),
    /// The address cannot identify a current resident tile.
    Miss(TileCacheMissReason),
}

impl fmt::Debug for TileCacheLookup<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hit(_) => formatter.debug_tuple("Hit").field(&"[REDACTED]").finish(),
            Self::Miss(reason) => formatter.debug_tuple("Miss").field(reason).finish(),
        }
    }
}

/// Policy reason a terminal producer outcome was not retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileRejectReason {
    /// The cache is closed.
    Closed,
    /// The address names a different Worker or renderer owner.
    ForeignOwner,
    /// The address names a different document session.
    ForeignSession,
    /// The immutable source identity differs.
    SourceMismatch,
    /// The product document revision or xref anchor differs.
    StaleRevision,
    /// The Native renderer epoch differs.
    StaleRendererEpoch,
    /// Complete pixels carry a different policy content key than the address.
    ContentKeyMismatch,
    /// Rendering did not produce a complete atomic tile.
    Incomplete,
    /// Capability policy did not support the work.
    Unsupported,
    /// The producer was cancelled.
    Cancelled,
    /// The producer failed.
    Failed,
    /// The immutable source changed.
    SourceChanged,
    /// One tile's allocator capacity exceeds the per-tile ceiling.
    TileTooLarge,
    /// One tile can never fit the aggregate pixel-capacity ceiling.
    PixelLimit,
    /// Fixed metadata plus one tile can never fit the resident ceiling.
    ResidentLimit,
}

/// Rejected admission that returns the complete terminal producer outcome.
pub struct TileRejected {
    reason: TileRejectReason,
    limit: Option<TileCacheLimit>,
    outcome: TileRenderOutcome,
}

impl TileRejected {
    /// Returns the stable policy rejection reason.
    pub const fn reason(&self) -> TileRejectReason {
        self.reason
    }

    /// Returns structured budget context for a size-policy rejection.
    pub const fn limit(&self) -> Option<TileCacheLimit> {
        self.limit
    }

    /// Returns the terminal producer outcome to its caller.
    pub fn into_outcome(self) -> TileRenderOutcome {
        self.outcome
    }
}

impl fmt::Debug for TileRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TileRejected")
            .field("reason", &self.reason)
            .field("limit", &self.limit)
            .field("outcome", &"[REDACTED]")
            .finish()
    }
}

/// Admission failure that returns ownership of the producer outcome.
pub struct TileCacheAdmissionError {
    error: TileCacheError,
    outcome: TileRenderOutcome,
}

impl TileCacheAdmissionError {
    /// Returns the stable tile-cache failure.
    pub const fn error(&self) -> TileCacheError {
        self.error
    }

    /// Returns the terminal producer outcome to its caller.
    pub fn into_outcome(self) -> TileRenderOutcome {
        self.outcome
    }
}

impl fmt::Debug for TileCacheAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TileCacheAdmissionError")
            .field("error", &self.error)
            .field("outcome", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for TileCacheAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for TileCacheAdmissionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

/// Metadata returned after a complete Native tile becomes resident.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileAdmitted {
    replaced: bool,
    evicted_recent: u64,
    evicted_protected: u64,
}

impl TileAdmitted {
    /// Reports whether an older exact-key tile was replaced.
    pub const fn replaced(self) -> bool {
        self.replaced
    }

    /// Returns recent-use victims removed before publication.
    pub const fn evicted_recent(self) -> u64 {
        self.evicted_recent
    }

    /// Returns protected-viewport victims removed only after recent-use exhaustion.
    pub const fn evicted_protected(self) -> u64 {
        self.evicted_protected
    }

    /// Returns all deterministic pressure victims.
    pub const fn evicted(self) -> u64 {
        self.evicted_recent + self.evicted_protected
    }
}

/// Policy outcome of attempting to retain one terminal producer result.
#[derive(Debug)]
pub enum TileAdmission {
    /// A complete immutable Native tile became resident.
    Admitted(TileAdmitted),
    /// Policy declined retention and returned the producer outcome.
    Rejected(TileRejected),
}

/// Current and cumulative accounting for one product tile cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileCacheStats {
    closed: bool,
    entries: u64,
    protected_entries: u64,
    recent_entries: u64,
    hits: u64,
    misses: u64,
    admissions: u64,
    replacements: u64,
    rejections: u64,
    evictions: u64,
    metadata_bytes: u64,
    pixel_capacity_bytes: u64,
    resident_bytes: u64,
    peak_resident_bytes: u64,
}

impl TileCacheStats {
    /// Reports whether close has released every current resource.
    pub const fn is_closed(self) -> bool {
        self.closed
    }

    /// Returns the current logical tile count.
    pub const fn entries(self) -> u64 {
        self.entries
    }

    /// Returns current protected-viewport tiles.
    pub const fn protected_entries(self) -> u64 {
        self.protected_entries
    }

    /// Returns current recent-use tiles.
    pub const fn recent_entries(self) -> u64 {
        self.recent_entries
    }

    /// Returns cumulative exact-key hits.
    pub const fn hits(self) -> u64 {
        self.hits
    }

    /// Returns cumulative misses.
    pub const fn misses(self) -> u64 {
        self.misses
    }

    /// Returns cumulative successful admissions.
    pub const fn admissions(self) -> u64 {
        self.admissions
    }

    /// Returns cumulative exact-key replacements.
    pub const fn replacements(self) -> u64 {
        self.replacements
    }

    /// Returns cumulative policy rejections.
    pub const fn rejections(self) -> u64 {
        self.rejections
    }

    /// Returns cumulative pressure evictions.
    pub const fn evictions(self) -> u64 {
        self.evictions
    }

    /// Returns allocator-reported fixed entry metadata capacity.
    pub const fn metadata_bytes(self) -> u64 {
        self.metadata_bytes
    }

    /// Returns allocator-reported pixel capacity of current tiles.
    pub const fn pixel_capacity_bytes(self) -> u64 {
        self.pixel_capacity_bytes
    }

    /// Returns current fixed metadata plus pixel capacity.
    pub const fn resident_bytes(self) -> u64 {
        self.resident_bytes
    }

    /// Returns the greatest resident total observed after publication.
    pub const fn peak_resident_bytes(self) -> u64 {
        self.peak_resident_bytes
    }
}

/// Synchronous close evidence for current tile-cache resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileCacheCloseReport {
    already_closed: bool,
    released_entries: u64,
    released_metadata_bytes: u64,
    released_pixel_capacity_bytes: u64,
    current_entries: u64,
    current_metadata_bytes: u64,
    current_pixel_capacity_bytes: u64,
}

impl TileCacheCloseReport {
    /// Reports whether the cache was already closed on entry.
    pub const fn already_closed(self) -> bool {
        self.already_closed
    }

    /// Returns entries released by this close call.
    pub const fn released_entries(self) -> u64 {
        self.released_entries
    }

    /// Returns fixed metadata bytes released by this close call.
    pub const fn released_metadata_bytes(self) -> u64 {
        self.released_metadata_bytes
    }

    /// Returns pixel capacity released by this close call.
    pub const fn released_pixel_capacity_bytes(self) -> u64 {
        self.released_pixel_capacity_bytes
    }

    /// Returns current entries after close.
    pub const fn current_entries(self) -> u64 {
        self.current_entries
    }

    /// Returns current fixed metadata bytes after close.
    pub const fn current_metadata_bytes(self) -> u64 {
        self.current_metadata_bytes
    }

    /// Returns current pixel capacity after close.
    pub const fn current_pixel_capacity_bytes(self) -> u64 {
        self.current_pixel_capacity_bytes
    }
}

struct TileEntry {
    retention: TileRetentionClass,
    tile: NativeTile,
}

/// Single-writer bounded cache for successful immutable Native product tiles.
///
/// The fixed entry vector is ordered from least to most recent use within each
/// retention segment. Eviction consumes recent-use entries before protected
/// viewport entries and has no wall-clock, random, or hash-table dependency.
pub struct TileCache {
    binding: TileCacheBinding,
    limits: TileCacheLimits,
    entries: Vec<TileEntry>,
    metadata_bytes: u64,
    pixel_capacity_bytes: u64,
    hits: u64,
    misses: u64,
    admissions: u64,
    replacements: u64,
    rejections: u64,
    evictions: u64,
    peak_resident_bytes: u64,
    closed: bool,
}

impl TileCache {
    /// Preallocates and charges fixed entry metadata for one complete binding.
    pub fn new(binding: TileCacheBinding, limits: TileCacheLimits) -> Result<Self, TileCacheError> {
        let scope = scope(binding);
        let entry_bytes = u64::try_from(mem::size_of::<TileEntry>())
            .map_err(|_| TileCacheError::allocation(limits.max_resident_bytes, u64::MAX, scope))?;
        let estimated_metadata = u64::try_from(limits.max_entries)
            .ok()
            .and_then(|count| count.checked_mul(entry_bytes))
            .ok_or_else(|| {
                TileCacheError::allocation(limits.max_resident_bytes, u64::MAX, scope)
            })?;
        if estimated_metadata > limits.max_resident_bytes {
            return Err(TileCacheError::with_limit(
                TileCacheLimitKind::MetadataBytes,
                limits.max_resident_bytes,
                0,
                estimated_metadata,
                scope,
            ));
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(limits.max_entries).map_err(|_| {
            TileCacheError::allocation(limits.max_resident_bytes, estimated_metadata, scope)
        })?;
        let metadata_bytes = u64::try_from(entries.capacity())
            .ok()
            .and_then(|capacity| capacity.checked_mul(entry_bytes))
            .ok_or_else(|| {
                TileCacheError::allocation(limits.max_resident_bytes, u64::MAX, scope)
            })?;
        if metadata_bytes > limits.max_resident_bytes {
            return Err(TileCacheError::with_limit(
                TileCacheLimitKind::MetadataBytes,
                limits.max_resident_bytes,
                0,
                metadata_bytes,
                scope,
            ));
        }
        Ok(Self {
            binding,
            limits,
            entries,
            metadata_bytes,
            pixel_capacity_bytes: 0,
            hits: 0,
            misses: 0,
            admissions: 0,
            replacements: 0,
            rejections: 0,
            evictions: 0,
            peak_resident_bytes: metadata_bytes,
            closed: false,
        })
    }

    /// Returns the complete immutable owner, source, revision, and epoch binding.
    pub const fn binding(&self) -> TileCacheBinding {
        self.binding
    }

    /// Returns the validated owner limits.
    pub const fn limits(&self) -> TileCacheLimits {
        self.limits
    }

    /// Returns current and cumulative accounting.
    pub fn stats(&self) -> TileCacheStats {
        let entries = u64::try_from(self.entries.len())
            .expect("validated tile entry length always fits in u64");
        let protected_entries = u64::try_from(
            self.entries
                .iter()
                .filter(|entry| entry.retention == TileRetentionClass::ProtectedViewport)
                .count(),
        )
        .expect("protected entry count always fits in u64");
        let recent_entries = entries
            .checked_sub(protected_entries)
            .expect("protected entries are a subset of all entries");
        let resident_bytes = self
            .metadata_bytes
            .checked_add(self.pixel_capacity_bytes)
            .expect("current resident components remain beneath the validated ceiling");
        TileCacheStats {
            closed: self.closed,
            entries,
            protected_entries,
            recent_entries,
            hits: self.hits,
            misses: self.misses,
            admissions: self.admissions,
            replacements: self.replacements,
            rejections: self.rejections,
            evictions: self.evictions,
            metadata_bytes: self.metadata_bytes,
            pixel_capacity_bytes: self.pixel_capacity_bytes,
            resident_bytes,
            peak_resident_bytes: self.peak_resident_bytes,
        }
    }

    /// Looks up an exact complete content identity and publishes only an immutable borrow.
    pub fn lookup(
        &mut self,
        address: &TileCacheAddress,
        cancellation: &(dyn TileCacheCancellation + '_),
    ) -> Result<TileCacheLookup<'_>, TileCacheError> {
        check_cancelled(cancellation)?;
        if let Some(reason) = self.address_miss_reason(address) {
            self.misses = checked_increment(self.misses)?;
            return Ok(TileCacheLookup::Miss(reason));
        }
        let Some(index) = self.find_index(address.content_key(), cancellation)? else {
            self.misses = checked_increment(self.misses)?;
            return Ok(TileCacheLookup::Miss(TileCacheMissReason::NotFound));
        };
        let next_hits = checked_increment(self.hits)?;
        check_cancelled(cancellation)?;
        let entry = self.entries.remove(index);
        self.entries.push(entry);
        self.hits = next_hits;
        Ok(TileCacheLookup::Hit(
            &self
                .entries
                .last()
                .expect("an exact hit moves one resident entry to the LRU tail")
                .tile,
        ))
    }

    /// Attempts to retain one terminal producer outcome.
    ///
    /// Only a complete immutable Native tile under the exact address can enter.
    /// Policy rejection and cache-operation failure both return ownership of the
    /// terminal outcome.
    pub fn try_admit(
        &mut self,
        address: &TileCacheAddress,
        outcome: TileRenderOutcome,
        retention: TileRetentionClass,
        cancellation: &(dyn TileCacheCancellation + '_),
    ) -> Result<TileAdmission, TileCacheAdmissionError> {
        if let Err(error) = check_cancelled(cancellation) {
            return admission_failure(error, outcome);
        }
        if let Some(reason) = self.address_miss_reason(address) {
            return self.reject(reject_for_miss(reason), None, outcome);
        }
        let tile = match outcome {
            TileRenderOutcome::Complete(tile) => tile,
            other => return self.reject(reject_for_outcome(other.kind()), None, other),
        };
        if tile.content_key() != address.content_key() {
            return self.reject(
                TileRejectReason::ContentKeyMismatch,
                None,
                TileRenderOutcome::Complete(tile),
            );
        }

        let incoming_pixels = tile.pixel_capacity_bytes();
        if incoming_pixels > self.limits.max_tile_pixel_bytes {
            let limit = self.limit(
                TileCacheLimitKind::TilePixelBytes,
                self.limits.max_tile_pixel_bytes,
                0,
                incoming_pixels,
            );
            return self.reject(
                TileRejectReason::TileTooLarge,
                Some(limit),
                TileRenderOutcome::Complete(tile),
            );
        }
        if incoming_pixels > self.limits.max_pixel_bytes {
            let limit = self.limit(
                TileCacheLimitKind::PixelBytes,
                self.limits.max_pixel_bytes,
                self.pixel_capacity_bytes,
                incoming_pixels,
            );
            return self.reject(
                TileRejectReason::PixelLimit,
                Some(limit),
                TileRenderOutcome::Complete(tile),
            );
        }
        let Some(max_resident_pixels) = self
            .limits
            .max_resident_bytes
            .checked_sub(self.metadata_bytes)
        else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        if incoming_pixels > max_resident_pixels {
            let Some(consumed) = self.metadata_bytes.checked_add(self.pixel_capacity_bytes) else {
                return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
            };
            let limit = self.limit(
                TileCacheLimitKind::ResidentBytes,
                self.limits.max_resident_bytes,
                consumed,
                incoming_pixels,
            );
            return self.reject(
                TileRejectReason::ResidentLimit,
                Some(limit),
                TileRenderOutcome::Complete(tile),
            );
        }

        let replacement_index = match self.find_index(address.content_key(), cancellation) {
            Ok(index) => index,
            Err(error) => {
                return admission_failure(error, TileRenderOutcome::Complete(tile));
            }
        };
        let replaced = replacement_index.is_some();
        let replacement_pixels = replacement_index
            .map(|index| self.entries[index].tile.pixel_capacity_bytes())
            .unwrap_or(0);
        let Some(mut remaining_len) = self.entries.len().checked_sub(usize::from(replaced)) else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        let Some(mut remaining_pixels) = self.pixel_capacity_bytes.checked_sub(replacement_pixels)
        else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        let mut recent_victims = 0_usize;
        let mut protected_victims = 0_usize;

        for victim_class in [
            TileRetentionClass::RecentUse,
            TileRetentionClass::ProtectedViewport,
        ] {
            for (index, entry) in self.entries.iter().enumerate() {
                if let Err(error) = probe_scan(cancellation, index) {
                    return admission_failure(error, TileRenderOutcome::Complete(tile));
                }
                if admission_fits(
                    remaining_len,
                    remaining_pixels,
                    self.limits,
                    incoming_pixels,
                    max_resident_pixels,
                ) {
                    break;
                }
                let effective_retention =
                    effective_retention(entry, retention, address.content_key());
                if replacement_index == Some(index) || effective_retention != victim_class {
                    continue;
                }
                let Some(next_len) = remaining_len.checked_sub(1) else {
                    return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
                };
                let Some(next_pixels) =
                    remaining_pixels.checked_sub(entry.tile.pixel_capacity_bytes())
                else {
                    return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
                };
                let victim_counter = match victim_class {
                    TileRetentionClass::RecentUse => &mut recent_victims,
                    TileRetentionClass::ProtectedViewport => &mut protected_victims,
                };
                let Some(next_victims) = victim_counter.checked_add(1) else {
                    return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
                };
                remaining_len = next_len;
                remaining_pixels = next_pixels;
                *victim_counter = next_victims;
            }
        }

        if let Err(error) = check_cancelled(cancellation) {
            return admission_failure(error, TileRenderOutcome::Complete(tile));
        }
        if !admission_fits(
            remaining_len,
            remaining_pixels,
            self.limits,
            incoming_pixels,
            max_resident_pixels,
        ) {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        }

        let Some(new_pixels) = remaining_pixels.checked_add(incoming_pixels) else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        let Some(final_len) = remaining_len.checked_add(1) else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        let Ok(evicted_recent) = u64::try_from(recent_victims) else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        let Ok(evicted_protected) = u64::try_from(protected_victims) else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        let Some(evicted) = evicted_recent.checked_add(evicted_protected) else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        let Some(next_admissions) = self.admissions.checked_add(1) else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        let next_replacements = if replaced {
            let Some(next) = self.replacements.checked_add(1) else {
                return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
            };
            next
        } else {
            self.replacements
        };
        let Some(next_evictions) = self.evictions.checked_add(evicted) else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        let Some(resident) = self.metadata_bytes.checked_add(new_pixels) else {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        };
        if final_len > self.limits.max_entries
            || final_len > self.entries.capacity()
            || new_pixels > self.limits.max_pixel_bytes
            || resident > self.limits.max_resident_bytes
        {
            return admission_failure(internal_state(), TileRenderOutcome::Complete(tile));
        }
        if let Err(error) = check_cancelled(cancellation) {
            return admission_failure(error, TileRenderOutcome::Complete(tile));
        }

        let mut replacement_pending = replaced;
        let mut recent_pending = recent_victims;
        let mut protected_pending = protected_victims;
        self.entries.retain_mut(|entry| {
            if replacement_pending && entry.tile.content_key() == address.content_key() {
                replacement_pending = false;
                return false;
            }
            entry.retention = effective_retention(entry, retention, address.content_key());
            match entry.retention {
                TileRetentionClass::RecentUse if recent_pending > 0 => {
                    recent_pending -= 1;
                    false
                }
                TileRetentionClass::ProtectedViewport if protected_pending > 0 => {
                    protected_pending -= 1;
                    false
                }
                _ => true,
            }
        });
        debug_assert!(!replacement_pending);
        debug_assert_eq!(recent_pending, 0);
        debug_assert_eq!(protected_pending, 0);
        debug_assert_eq!(self.entries.len(), remaining_len);
        self.entries.push(TileEntry { retention, tile });
        self.pixel_capacity_bytes = new_pixels;
        self.admissions = next_admissions;
        self.replacements = next_replacements;
        self.evictions = next_evictions;
        self.peak_resident_bytes = self.peak_resident_bytes.max(resident);
        Ok(TileAdmission::Admitted(TileAdmitted {
            replaced,
            evicted_recent,
            evicted_protected,
        }))
    }

    /// Releases current tiles and fixed metadata and permanently closes the cache.
    pub fn close(&mut self) -> TileCacheCloseReport {
        let already_closed = self.closed;
        let released_entries =
            u64::try_from(self.entries.len()).expect("validated tile entry length fits in u64");
        let released_metadata_bytes = self.metadata_bytes;
        let released_pixel_capacity_bytes = self.pixel_capacity_bytes;
        self.entries = Vec::new();
        self.metadata_bytes = 0;
        self.pixel_capacity_bytes = 0;
        self.closed = true;
        TileCacheCloseReport {
            already_closed,
            released_entries,
            released_metadata_bytes,
            released_pixel_capacity_bytes,
            current_entries: 0,
            current_metadata_bytes: 0,
            current_pixel_capacity_bytes: 0,
        }
    }

    fn address_miss_reason(&self, address: &TileCacheAddress) -> Option<TileCacheMissReason> {
        if self.closed {
            Some(TileCacheMissReason::Closed)
        } else if address.owner_id() != self.binding.owner_id() {
            Some(TileCacheMissReason::ForeignOwner)
        } else if address.session_id() != self.binding.session_id() {
            Some(TileCacheMissReason::ForeignSession)
        } else if address.content_key().source() != self.binding.source() {
            Some(TileCacheMissReason::SourceMismatch)
        } else if address.content_key().document_revision() != self.binding.document_revision()
            || address.content_key().revision_startxref() != self.binding.revision_startxref()
        {
            Some(TileCacheMissReason::StaleRevision)
        } else if address.content_key().renderer_epoch() != self.binding.renderer_epoch() {
            Some(TileCacheMissReason::StaleRendererEpoch)
        } else {
            None
        }
    }

    fn find_index(
        &self,
        key: &TileContentKey,
        cancellation: &(dyn TileCacheCancellation + '_),
    ) -> Result<Option<usize>, TileCacheError> {
        for (index, entry) in self.entries.iter().enumerate() {
            probe_scan(cancellation, index)?;
            if entry.tile.content_key() == key {
                return Ok(Some(index));
            }
        }
        check_cancelled(cancellation)?;
        Ok(None)
    }

    fn reject(
        &mut self,
        reason: TileRejectReason,
        limit: Option<TileCacheLimit>,
        outcome: TileRenderOutcome,
    ) -> Result<TileAdmission, TileCacheAdmissionError> {
        let Some(rejections) = self.rejections.checked_add(1) else {
            return admission_failure(internal_state(), outcome);
        };
        self.rejections = rejections;
        Ok(TileAdmission::Rejected(TileRejected {
            reason,
            limit,
            outcome,
        }))
    }

    fn limit(
        &self,
        kind: TileCacheLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    ) -> TileCacheLimit {
        TileCacheLimit {
            kind,
            limit,
            consumed,
            attempted,
            scope: scope(self.binding),
        }
    }
}

impl fmt::Debug for TileCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TileCache")
            .field("binding", &self.binding)
            .field("limits", &self.limits)
            .field("stats", &self.stats())
            .field("entries", &"[REDACTED]")
            .finish()
    }
}

fn scope(binding: TileCacheBinding) -> TileCacheScope {
    TileCacheScope {
        owner_id: binding.owner_id(),
        session_id: binding.session_id(),
    }
}

fn effective_retention(
    entry: &TileEntry,
    incoming_retention: TileRetentionClass,
    incoming_key: &TileContentKey,
) -> TileRetentionClass {
    if incoming_retention == TileRetentionClass::ProtectedViewport
        && entry.retention == TileRetentionClass::ProtectedViewport
        && !same_viewport(entry.tile.content_key(), incoming_key)
    {
        TileRetentionClass::RecentUse
    } else {
        entry.retention
    }
}

fn same_viewport(left: &TileContentKey, right: &TileContentKey) -> bool {
    left.source() == right.source()
        && left.document_revision() == right.document_revision()
        && left.revision_startxref() == right.revision_startxref()
        && left.page_index() == right.page_index()
        && left.page_object_number() == right.page_object_number()
        && left.page_object_generation() == right.page_object_generation()
        && left.scene_hash() == right.scene_hash()
        && left.decision_hash() == right.decision_hash()
        && left.geometry_hash() == right.geometry_hash()
        && left.viewport_clip() == right.viewport_clip()
        && left.zoom() == right.zoom()
        && left.device_scale_milli() == right.device_scale_milli()
        && left.rotation() == right.rotation()
        && left.optional_content() == right.optional_content()
        && left.annotation_revision() == right.annotation_revision()
        && left.quality() == right.quality()
        && left.output_profile() == right.output_profile()
        && left.render_config_hash() == right.render_config_hash()
        && left.renderer_epoch() == right.renderer_epoch()
        && left.backend() == right.backend()
}

fn reject_for_miss(reason: TileCacheMissReason) -> TileRejectReason {
    match reason {
        TileCacheMissReason::Closed => TileRejectReason::Closed,
        TileCacheMissReason::ForeignOwner => TileRejectReason::ForeignOwner,
        TileCacheMissReason::ForeignSession => TileRejectReason::ForeignSession,
        TileCacheMissReason::SourceMismatch => TileRejectReason::SourceMismatch,
        TileCacheMissReason::StaleRevision => TileRejectReason::StaleRevision,
        TileCacheMissReason::StaleRendererEpoch => TileRejectReason::StaleRendererEpoch,
        TileCacheMissReason::NotFound => {
            unreachable!("binding validation never reports exact-key absence")
        }
    }
}

fn reject_for_outcome(kind: TileOutcomeKind) -> TileRejectReason {
    match kind {
        TileOutcomeKind::Complete => {
            unreachable!("complete outcomes continue through admission validation")
        }
        TileOutcomeKind::Incomplete => TileRejectReason::Incomplete,
        TileOutcomeKind::Unsupported => TileRejectReason::Unsupported,
        TileOutcomeKind::Cancelled => TileRejectReason::Cancelled,
        TileOutcomeKind::Failed => TileRejectReason::Failed,
        TileOutcomeKind::SourceChanged => TileRejectReason::SourceChanged,
    }
}

fn admission_fits(
    resident_entries: usize,
    resident_pixels: u64,
    limits: TileCacheLimits,
    incoming_pixels: u64,
    max_resident_pixels: u64,
) -> bool {
    resident_entries < limits.max_entries
        && resident_pixels
            .checked_add(incoming_pixels)
            .is_some_and(|pixels| pixels <= limits.max_pixel_bytes && pixels <= max_resident_pixels)
}

fn admission_failure(
    error: TileCacheError,
    outcome: TileRenderOutcome,
) -> Result<TileAdmission, TileCacheAdmissionError> {
    Err(TileCacheAdmissionError { error, outcome })
}

fn check_cancelled(cancellation: &(dyn TileCacheCancellation + '_)) -> Result<(), TileCacheError> {
    if cancellation.is_cancelled() {
        return Err(TileCacheError::for_code(TileCacheErrorCode::Cancelled));
    }
    Ok(())
}

fn probe_scan(
    cancellation: &(dyn TileCacheCancellation + '_),
    index: usize,
) -> Result<(), TileCacheError> {
    if index.is_multiple_of(CANCELLATION_INTERVAL) {
        check_cancelled(cancellation)?;
    }
    Ok(())
}

fn checked_increment(value: u64) -> Result<u64, TileCacheError> {
    value.checked_add(1).ok_or_else(internal_state)
}

fn internal_state() -> TileCacheError {
    TileCacheError::for_code(TileCacheErrorCode::InternalState)
}
