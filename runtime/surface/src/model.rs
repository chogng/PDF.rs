use core::fmt;

use pdf_rs_policy::{AlphaMode as PolicyAlphaMode, PixelFormat as PolicyPixelFormat, RenderPlan};
use pdf_rs_protocol::{
    AlphaMode, PixelFormat, RenderPlanHash, SessionId, SurfaceId, SurfaceMetadata,
    SurfacePlanBinding, SurfaceRenderIdentity, SurfaceTransport, WorkerId,
};

use crate::error::error;
use crate::{SurfaceError, SurfaceErrorCode};

/// Nonzero identity of one Worker process epoch.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkerEpoch(u64);

impl WorkerEpoch {
    /// Creates a nonzero Worker epoch.
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the numeric epoch.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Opaque fake platform-handle identity.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FakeHandleId(u64);

impl FakeHandleId {
    /// Constructs an identity from an untrusted fake handle-table value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric identity for deterministic test adapters.
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for FakeHandleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FakeHandleId([REDACTED])")
    }
}

/// Fake platform-handle class presented by an untrusted import table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandleClass {
    /// Immutable shared-memory-like byte storage.
    SharedMemory,
    /// Deliberately wrong file-like handle used to test fail-closed class checks.
    File,
    /// Deliberately wrong socket-like handle used to test fail-closed class checks.
    Socket,
}

/// Fake platform-handle access rights.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandleAccess {
    /// Consumer may only read immutable published bytes.
    ReadOnly,
    /// Producer-private mutable access before publication.
    ReadWrite,
    /// Deliberately invalid executable access.
    Execute,
}

/// Public untrusted parts used to construct one fake imported handle descriptor.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct FakeHandleParts {
    /// Opaque handle-table identity.
    pub id: FakeHandleId,
    /// Sensitive one-shot transfer token.
    pub transfer_token: u64,
    /// Claimed platform-handle class.
    pub class: HandleClass,
    /// Claimed access rights.
    pub access: HandleAccess,
    /// Claimed accessible shared region bytes.
    pub region_length: u64,
    /// Claimed owning Worker.
    pub worker: WorkerId,
    /// Claimed owning Session.
    pub session: SessionId,
    /// Claimed Worker epoch.
    pub worker_epoch: WorkerEpoch,
    /// Claimed Surface identity.
    pub surface: SurfaceId,
    /// Claimed viewport generation.
    pub generation: u64,
}

impl fmt::Debug for FakeHandleParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeHandleParts")
            .field("id", &"[REDACTED]")
            .field("transfer_token", &"[REDACTED]")
            .field("class", &self.class)
            .field("access", &self.access)
            .field("region_length", &self.region_length)
            .field("worker", &self.worker)
            .field("session", &self.session)
            .field("worker_epoch", &self.worker_epoch)
            .field("surface", &self.surface)
            .field("generation", &self.generation)
            .finish()
    }
}

/// Fake out-of-band handle descriptor received at the consumer boundary.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct FakeHandleDescriptor {
    parts: FakeHandleParts,
}

impl FakeHandleDescriptor {
    /// Constructs a descriptor from untrusted transport fields.
    pub const fn from_parts(parts: FakeHandleParts) -> Self {
        Self { parts }
    }

    /// Returns all untrusted descriptor fields.
    pub const fn parts(self) -> FakeHandleParts {
        self.parts
    }
}

impl fmt::Debug for FakeHandleDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeHandleDescriptor")
            .field("id", &"[REDACTED]")
            .field("transfer_token", &"[REDACTED]")
            .field("class", &self.parts.class)
            .field("access", &self.parts.access)
            .field("region_length", &self.parts.region_length)
            .field("worker", &self.parts.worker)
            .field("session", &self.parts.session)
            .field("worker_epoch", &self.parts.worker_epoch)
            .field("surface", &self.parts.surface)
            .field("generation", &self.parts.generation)
            .finish()
    }
}

/// Exact immutable plan, region, generation, format, and alpha identity expected for one Surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfacePlanIdentity {
    generation: u64,
    binding: SurfacePlanBinding,
    format: PixelFormat,
    alpha: AlphaMode,
}

impl SurfacePlanIdentity {
    /// Creates an identity from an already accepted canonical protocol plan binding.
    pub const fn from_protocol(
        generation: u64,
        binding: SurfacePlanBinding,
        format: PixelFormat,
        alpha: AlphaMode,
    ) -> Self {
        Self {
            generation,
            binding,
            format,
            alpha,
        }
    }

    /// Projects one canonical policy RenderPlan tile without forking its wire identities.
    pub fn from_render_plan(plan: &RenderPlan, tile_ordinal: usize) -> Result<Self, SurfaceError> {
        let manifest = plan.protocol_manifest();
        let region = manifest
            .regions
            .get(tile_ordinal)
            .ok_or_else(|| error(SurfaceErrorCode::InvalidPlan))?
            .clone();
        if plan.tiles().get(tile_ordinal).is_none()
            || manifest.regions.len() != manifest.tile_content_hashes.len()
        {
            return Err(error(SurfaceErrorCode::InvalidPlan));
        }
        let render = SurfaceRenderIdentity::new(
            manifest.render_config,
            manifest.renderer_epoch,
            manifest.plan_id,
            RenderPlanHash::new(plan.hash().into_digest()),
            manifest.scene_hash,
            manifest.decision_hash,
            manifest.backend,
        );
        let output = plan.config().output_profile();
        let format = match output.format() {
            PolicyPixelFormat::Rgba8 => PixelFormat::Rgba8,
        };
        let alpha = match output.alpha() {
            PolicyAlphaMode::Straight => AlphaMode::Straight,
            PolicyAlphaMode::Premultiplied => AlphaMode::Premultiplied,
        };
        Ok(Self::from_protocol(
            manifest.generation,
            SurfacePlanBinding::new(region, render),
            format,
            alpha,
        ))
    }

    /// Returns the nonzero viewport generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Borrows the exact canonical Surface plan binding.
    pub const fn binding(&self) -> &SurfacePlanBinding {
        &self.binding
    }

    /// Returns the exact output pixel format.
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// Returns the exact output alpha representation.
    pub const fn alpha(&self) -> AlphaMode {
        self.alpha
    }
}

/// Complete producer allocation request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfaceAllocation {
    /// Claimed producing Worker.
    pub worker: WorkerId,
    /// Claimed owning Session.
    pub session: SessionId,
    /// Claimed producing Worker epoch.
    pub worker_epoch: WorkerEpoch,
    /// Exact immutable policy/protocol plan identity.
    pub plan: SurfacePlanIdentity,
    /// Actual pixel width.
    pub width: u32,
    /// Actual pixel height.
    pub height: u32,
    /// Actual row stride in bytes.
    pub stride: u32,
    /// Actual pixel format.
    pub format: PixelFormat,
    /// Actual alpha representation.
    pub alpha: AlphaMode,
    /// Pixel-range offset in the shared region.
    pub byte_offset: u64,
    /// Complete allocated shared region extent.
    pub region_length: u64,
}

/// Exact consumer context used to independently validate an imported Surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfaceConsumerContext {
    /// Expected Worker owner.
    pub worker: WorkerId,
    /// Expected Session owner.
    pub session: SessionId,
    /// Expected current Worker epoch.
    pub worker_epoch: WorkerEpoch,
    /// Expected immutable plan and output identity.
    pub plan: SurfacePlanIdentity,
}

/// Exact owner and sensitive lease proof used for lifecycle operations.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct SurfaceAccess {
    worker: WorkerId,
    session: SessionId,
    worker_epoch: WorkerEpoch,
    surface: SurfaceId,
    lease_token: u64,
}

impl SurfaceAccess {
    /// Constructs an access proof from untrusted operation fields.
    pub const fn new(
        worker: WorkerId,
        session: SessionId,
        worker_epoch: WorkerEpoch,
        surface: SurfaceId,
        lease_token: u64,
    ) -> Self {
        Self {
            worker,
            session,
            worker_epoch,
            surface,
            lease_token,
        }
    }

    /// Returns the Worker owner.
    pub const fn worker(self) -> WorkerId {
        self.worker
    }

    /// Returns the Session owner.
    pub const fn session(self) -> SessionId {
        self.session
    }

    /// Returns the Worker epoch.
    pub const fn worker_epoch(self) -> WorkerEpoch {
        self.worker_epoch
    }

    /// Returns the Surface identity.
    pub const fn surface(self) -> SurfaceId {
        self.surface
    }

    pub(crate) const fn lease_token(self) -> u64 {
        self.lease_token
    }
}

impl fmt::Debug for SurfaceAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceAccess")
            .field("worker", &self.worker)
            .field("session", &self.session)
            .field("worker_epoch", &self.worker_epoch)
            .field("surface", &self.surface)
            .field("lease_token", &"[REDACTED]")
            .finish()
    }
}

/// Result of allocating one producer-private mutable Surface region.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct AllocatedSurface {
    access: SurfaceAccess,
    layout_bytes: u64,
    region_length: u64,
}

impl AllocatedSurface {
    pub(crate) const fn new(access: SurfaceAccess, layout_bytes: u64, region_length: u64) -> Self {
        Self {
            access,
            layout_bytes,
            region_length,
        }
    }

    /// Returns the exact access proof.
    pub const fn access(self) -> SurfaceAccess {
        self.access
    }

    /// Returns exact initialized pixel bytes required before publication.
    pub const fn layout_bytes(self) -> u64 {
        self.layout_bytes
    }

    /// Returns the complete retained shared-region extent.
    pub const fn region_length(self) -> u64 {
        self.region_length
    }
}

impl fmt::Debug for AllocatedSurface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AllocatedSurface")
            .field("access", &self.access)
            .field("layout_bytes", &self.layout_bytes)
            .field("region_length", &self.region_length)
            .finish()
    }
}

/// Immutable canonical publication produced by an atomic state transition.
#[derive(Clone, Eq, PartialEq)]
pub struct PublishedSurface {
    access: SurfaceAccess,
    metadata: SurfaceMetadata,
    transport: SurfaceTransport,
}

impl PublishedSurface {
    pub(crate) const fn new(
        access: SurfaceAccess,
        metadata: SurfaceMetadata,
        transport: SurfaceTransport,
    ) -> Self {
        Self {
            access,
            metadata,
            transport,
        }
    }

    /// Returns the exact lifecycle access proof.
    pub const fn access(&self) -> SurfaceAccess {
        self.access
    }

    /// Borrows canonical immutable Surface metadata.
    pub const fn metadata(&self) -> &SurfaceMetadata {
        &self.metadata
    }

    /// Borrows the canonical shared-memory transport.
    pub const fn transport(&self) -> &SurfaceTransport {
        &self.transport
    }
}

impl fmt::Debug for PublishedSurface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublishedSurface")
            .field("access", &self.access)
            .field("metadata", &self.metadata)
            .field("transport", &"[REDACTED]")
            .finish()
    }
}

/// Untrusted wire metadata plus one fake out-of-band transferred handle.
#[derive(Clone, Eq, PartialEq)]
pub struct SurfaceTransfer {
    /// Canonical-looking metadata that the consumer must revalidate.
    pub metadata: SurfaceMetadata,
    /// Canonical-looking transport that the consumer must revalidate.
    pub transport: SurfaceTransport,
    /// Untrusted fake platform-handle descriptor.
    pub handle: FakeHandleDescriptor,
}

impl fmt::Debug for SurfaceTransfer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceTransfer")
            .field("metadata", &self.metadata)
            .field("transport", &"[REDACTED]")
            .field("handle", &self.handle)
            .finish()
    }
}

/// Opaque proof that one transferred handle was imported exactly once.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ImportedSurface {
    access: SurfaceAccess,
    handle: FakeHandleId,
    transfer_token: u64,
}

impl ImportedSurface {
    pub(crate) const fn new(
        access: SurfaceAccess,
        handle: FakeHandleId,
        transfer_token: u64,
    ) -> Self {
        Self {
            access,
            handle,
            transfer_token,
        }
    }

    /// Returns the exact lifecycle access proof.
    pub const fn access(self) -> SurfaceAccess {
        self.access
    }

    pub(crate) const fn handle(self) -> FakeHandleId {
        self.handle
    }

    pub(crate) const fn transfer_token(self) -> u64 {
        self.transfer_token
    }
}

impl fmt::Debug for ImportedSurface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportedSurface")
            .field("access", &self.access)
            .field("handle", &"[REDACTED]")
            .field("transfer_token", &"[REDACTED]")
            .finish()
    }
}

/// Immutable borrowed consumer view that cannot outlive the owner table.
pub struct AcquiredSurface<'a> {
    metadata: &'a SurfaceMetadata,
    bytes: &'a [u8],
}

impl<'a> AcquiredSurface<'a> {
    pub(crate) const fn new(metadata: &'a SurfaceMetadata, bytes: &'a [u8]) -> Self {
        Self { metadata, bytes }
    }

    /// Borrows canonical immutable metadata.
    pub const fn metadata(&self) -> &SurfaceMetadata {
        self.metadata
    }

    /// Borrows the exact immutable pixel byte range.
    pub const fn bytes(&self) -> &[u8] {
        self.bytes
    }
}

impl fmt::Debug for AcquiredSurface<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquiredSurface")
            .field("metadata", &self.metadata)
            .field("bytes", &format_args!("[BYTES:{}]", self.bytes.len()))
            .finish()
    }
}

/// Reason private or published storage became terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetireReason {
    /// Host explicitly released the Surface.
    ReleasedByHost,
    /// The deterministic virtual lease expired.
    LeaseExpired,
    /// A newer viewport generation replaced the Surface.
    GenerationReplaced,
    /// The owning Session closed.
    SessionClosed,
    /// Producer cancelled incomplete private work.
    Cancelled,
    /// Producer failed incomplete private work.
    Failed,
    /// Producer completion was stale for the active generation.
    StaleGeneration,
}
