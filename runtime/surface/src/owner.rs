use core::fmt;
use std::collections::BTreeMap;

use pdf_rs_protocol::{
    AlphaMode, CompatibleHandshake, ENDPOINT_CAPABILITY_SHARED_MEMORY, EndpointCapabilities,
    EndpointRole, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    PixelFormat, ProtocolError, ProtocolErrorCode, ProtocolHello, ProtocolValidator, RendererEpoch,
    SCHEMA_HASH, SessionId, SurfaceId, SurfaceMetadata, SurfaceOwner as WireSurfaceOwner,
    SurfaceTransport, SurfaceValidationContext, WorkerId,
};

use crate::error::error;
use crate::{
    AcquiredSurface, AllocatedSurface, FakeHandleDescriptor, FakeHandleId, FakeHandleParts,
    HandleAccess, HandleClass, ImportedSurface, LifecycleReport, PublishedSurface, ReleaseOutcome,
    RetireReason, SurfaceAccess, SurfaceAllocation, SurfaceConsumerContext, SurfaceError,
    SurfaceErrorCode, SurfaceLimits, SurfacePlanIdentity, SurfaceReleaseReport,
    SurfaceResourceReport, SurfaceTransfer, WorkerEpoch,
};

const INITIAL_SECRET: u64 = 0xa5a5_5a5a_d3c3_b4b4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionState {
    Active { generation: u64 },
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransferState {
    Available,
    Transferred { token: u64 },
    Imported { token: u64 },
}

struct PublishedState {
    metadata: SurfaceMetadata,
    transport: SurfaceTransport,
    lease_deadline: u64,
    transfer: TransferState,
}

// Keeping canonical publication metadata inline avoids a second allocation during the atomic
// private-to-published transition. The number of entries is independently hard-bounded.
#[allow(clippy::large_enum_variant)]
enum LiveState {
    Private { complete: bool },
    Published(PublishedState),
}

struct SurfaceEntry {
    allocation: SurfaceAllocation,
    handle: FakeHandleId,
    layout_bytes: u64,
    bytes: Vec<u8>,
    access: SurfaceAccess,
    state: LiveState,
}

#[derive(Clone, Copy)]
struct HandleRecord {
    surface: SurfaceId,
    worker: WorkerId,
    session: SessionId,
    worker_epoch: WorkerEpoch,
    generation: u64,
    class: HandleClass,
    access: HandleAccess,
    region_length: u64,
    transfer_token: Option<u64>,
    imported: bool,
}

#[derive(Clone, Copy)]
struct RetiredRecord {
    access: SurfaceAccess,
    reason: RetireReason,
}

#[derive(Clone, Copy)]
struct Layout {
    bytes: u64,
    offset: usize,
    end: usize,
    region_length: usize,
}

/// Pure Worker/session Surface owner with a bounded fake shared-memory handle table.
pub struct SurfaceOwner {
    worker: WorkerId,
    worker_epoch: WorkerEpoch,
    renderer_epoch: RendererEpoch,
    limits: SurfaceLimits,
    validator: ProtocolValidator,
    handshake: CompatibleHandshake,
    now: u64,
    next_surface_id: u64,
    next_handle_id: u64,
    next_secret: u64,
    issued_surface_ids: u64,
    sessions: BTreeMap<SessionId, SessionState>,
    surfaces: BTreeMap<SurfaceId, SurfaceEntry>,
    handles: BTreeMap<FakeHandleId, HandleRecord>,
    retired: BTreeMap<SurfaceId, RetiredRecord>,
}

impl SurfaceOwner {
    /// Creates an empty owner for one exact nonzero Worker and renderer epoch.
    pub fn new(
        worker: WorkerId,
        worker_epoch: WorkerEpoch,
        renderer_epoch: RendererEpoch,
        limits: SurfaceLimits,
    ) -> Result<Self, SurfaceError> {
        if worker.value() == 0 || worker_epoch.value() == 0 || renderer_epoch.value() == 0 {
            return Err(error(SurfaceErrorCode::InvalidWorker));
        }
        let validator = ProtocolValidator::new(limits.protocol());
        let handshake = shared_memory_handshake(validator)?;
        Ok(Self {
            worker,
            worker_epoch,
            renderer_epoch,
            limits,
            validator,
            handshake,
            now: 0,
            next_surface_id: 1,
            next_handle_id: 1,
            next_secret: INITIAL_SECRET,
            issued_surface_ids: 0,
            sessions: BTreeMap::new(),
            surfaces: BTreeMap::new(),
            handles: BTreeMap::new(),
            retired: BTreeMap::new(),
        })
    }

    /// Returns the current Worker identity.
    pub const fn worker(&self) -> WorkerId {
        self.worker
    }

    /// Returns the current Worker epoch.
    pub const fn worker_epoch(&self) -> WorkerEpoch {
        self.worker_epoch
    }

    /// Returns the current renderer epoch.
    pub const fn renderer_epoch(&self) -> RendererEpoch {
        self.renderer_epoch
    }

    /// Returns the current virtual tick.
    pub const fn virtual_tick(&self) -> u64 {
        self.now
    }

    /// Opens one nonzero Session at one nonzero initial viewport generation.
    ///
    /// Repeating the exact active binding is idempotent. Session IDs are never reused in one
    /// Worker epoch, including after close.
    pub fn open_session(
        &mut self,
        session: SessionId,
        generation: u64,
    ) -> Result<(), SurfaceError> {
        if session.value() == 0 {
            return Err(error(SurfaceErrorCode::InvalidSession));
        }
        if generation == 0 {
            return Err(error(SurfaceErrorCode::InvalidGeneration));
        }
        match self.sessions.get(&session).copied() {
            Some(SessionState::Active {
                generation: current,
            }) if current == generation => return Ok(()),
            Some(SessionState::Active { .. } | SessionState::Closed) => {
                return Err(error(SurfaceErrorCode::InvalidSession));
            }
            None => {}
        }
        if self.sessions.len() >= self.limits.max_sessions_per_epoch() {
            return Err(error(SurfaceErrorCode::CapacityExceeded));
        }
        self.sessions
            .insert(session, SessionState::Active { generation });
        Ok(())
    }

    /// Allocates zero-initialized producer-private mutable storage and one private fake handle.
    ///
    /// Every identity, layout, byte, live-Surface, handle, and per-epoch ID charge is validated
    /// before owner state changes.
    pub fn allocate(
        &mut self,
        allocation: SurfaceAllocation,
    ) -> Result<AllocatedSurface, SurfaceError> {
        let layout = self.validate_allocation(&allocation)?;
        if self.surfaces.len() >= self.limits.max_live_surfaces()
            || self.handles.len() >= self.limits.max_handles()
            || self.issued_surface_ids >= self.limits.max_surface_ids_per_epoch()
        {
            return Err(error(SurfaceErrorCode::CapacityExceeded));
        }
        let retained = self
            .current_resources()
            .retained_bytes()
            .checked_add(allocation.region_length)
            .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;
        if retained > self.limits.max_total_bytes() {
            return Err(error(SurfaceErrorCode::CapacityExceeded));
        }

        let surface_value = self.next_surface_id;
        let handle_value = self.next_handle_id;
        let lease_token = self.next_secret;
        let next_surface_id = surface_value
            .checked_add(1)
            .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;
        let next_handle_id = handle_value
            .checked_add(1)
            .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;
        let next_secret = lease_token
            .checked_add(1)
            .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;
        let issued_surface_ids = self
            .issued_surface_ids
            .checked_add(1)
            .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;

        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(layout.region_length)
            .map_err(|_| error(SurfaceErrorCode::CapacityExceeded))?;
        bytes.resize(layout.region_length, 0);

        let surface = SurfaceId::new(surface_value);
        let handle = FakeHandleId::new(handle_value);
        let access = SurfaceAccess::new(
            self.worker,
            allocation.session,
            self.worker_epoch,
            surface,
            lease_token,
        );
        let handle_record = HandleRecord {
            surface,
            worker: self.worker,
            session: allocation.session,
            worker_epoch: self.worker_epoch,
            generation: allocation.plan.generation(),
            class: HandleClass::SharedMemory,
            access: HandleAccess::ReadWrite,
            region_length: allocation.region_length,
            transfer_token: None,
            imported: false,
        };
        let entry = SurfaceEntry {
            allocation,
            handle,
            layout_bytes: layout.bytes,
            bytes,
            access,
            state: LiveState::Private { complete: false },
        };
        self.handles.insert(handle, handle_record);
        self.surfaces.insert(surface, entry);
        self.next_surface_id = next_surface_id;
        self.next_handle_id = next_handle_id;
        self.next_secret = next_secret;
        self.issued_surface_ids = issued_surface_ids;
        Ok(AllocatedSurface::new(
            access,
            layout.bytes,
            u64::try_from(layout.region_length).expect("validated region length originated as u64"),
        ))
    }

    /// Replaces the exact private pixel range and marks it complete.
    ///
    /// The full canonical `stride * height` range must be supplied at once. Padding outside the
    /// declared pixel range remains private zero-initialized storage.
    pub fn write_private_pixels(
        &mut self,
        access: SurfaceAccess,
        pixels: &[u8],
    ) -> Result<(), SurfaceError> {
        self.validate_owner_access(access)?;
        let entry = self
            .surfaces
            .get_mut(&access.surface())
            .ok_or_else(|| error(SurfaceErrorCode::UnknownSurface))?;
        validate_entry_access(entry, access)?;
        if !matches!(&entry.state, LiveState::Private { .. }) {
            return Err(error(SurfaceErrorCode::InvalidState));
        }
        let layout = layout_for(&entry.allocation, self.limits)?;
        if pixels.len() != layout.end - layout.offset {
            return Err(error(SurfaceErrorCode::InvalidLayout));
        }
        entry.bytes[layout.offset..layout.end].copy_from_slice(pixels);
        entry.state = LiveState::Private { complete: true };
        Ok(())
    }

    /// Atomically publishes one complete private Surface as immutable read-only shared memory.
    pub fn publish(&mut self, access: SurfaceAccess) -> Result<PublishedSurface, SurfaceError> {
        self.validate_owner_access(access)?;
        let (metadata, transport, handle, deadline) = {
            let entry = self
                .surfaces
                .get(&access.surface())
                .ok_or_else(|| error(SurfaceErrorCode::UnknownSurface))?;
            validate_entry_access(entry, access)?;
            match &entry.state {
                LiveState::Private { complete: true } => {}
                LiveState::Private { complete: false } => {
                    return Err(error(SurfaceErrorCode::IncompleteSurface));
                }
                LiveState::Published(_) => return Err(error(SurfaceErrorCode::InvalidState)),
            }
            self.validate_allocation(&entry.allocation)?;
            let metadata = metadata_for(entry, self.worker);
            let transport = SurfaceTransport::SharedMemory {
                slot: 0,
                region_length: entry.allocation.region_length,
            };
            self.validate_canonical_surface(
                &metadata,
                &transport,
                &entry.allocation.plan,
                entry.allocation.worker,
                entry.allocation.session,
                entry.allocation.region_length,
            )?;
            let deadline = self
                .now
                .checked_add(self.limits.lease_ticks())
                .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;
            (metadata, transport, entry.handle, deadline)
        };

        let handle_record = self
            .handles
            .get_mut(&handle)
            .ok_or_else(|| error(SurfaceErrorCode::InvalidHandle))?;
        if handle_record.access != HandleAccess::ReadWrite
            || handle_record.class != HandleClass::SharedMemory
        {
            return Err(error(SurfaceErrorCode::InvalidHandle));
        }
        handle_record.access = HandleAccess::ReadOnly;
        let entry = self
            .surfaces
            .get_mut(&access.surface())
            .expect("validated live Surface remains present");
        entry.state = LiveState::Published(PublishedState {
            metadata: metadata.clone(),
            transport: transport.clone(),
            lease_deadline: deadline,
            transfer: TransferState::Available,
        });
        Ok(PublishedSurface::new(access, metadata, transport))
    }

    /// Creates the single permitted out-of-band transfer for one published Surface.
    pub fn transfer(&mut self, access: SurfaceAccess) -> Result<SurfaceTransfer, SurfaceError> {
        self.validate_owner_access(access)?;
        let (handle, metadata, transport) = {
            let entry = self
                .surfaces
                .get(&access.surface())
                .ok_or_else(|| error(SurfaceErrorCode::UnknownSurface))?;
            validate_entry_access(entry, access)?;
            let LiveState::Published(published) = &entry.state else {
                return Err(error(SurfaceErrorCode::InvalidState));
            };
            if published.transfer != TransferState::Available {
                return Err(error(SurfaceErrorCode::TransferConsumed));
            }
            (
                entry.handle,
                published.metadata.clone(),
                published.transport.clone(),
            )
        };
        let transfer_token = self.next_secret;
        let next_secret = transfer_token
            .checked_add(1)
            .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;
        let handle_record = self
            .handles
            .get(&handle)
            .copied()
            .ok_or_else(|| error(SurfaceErrorCode::InvalidHandle))?;
        if handle_record.access != HandleAccess::ReadOnly
            || handle_record.class != HandleClass::SharedMemory
            || handle_record.transfer_token.is_some()
        {
            return Err(error(SurfaceErrorCode::InvalidHandle));
        }
        let descriptor = FakeHandleDescriptor::from_parts(FakeHandleParts {
            id: handle,
            transfer_token,
            class: handle_record.class,
            access: handle_record.access,
            region_length: handle_record.region_length,
            worker: handle_record.worker,
            session: handle_record.session,
            worker_epoch: handle_record.worker_epoch,
            surface: handle_record.surface,
            generation: handle_record.generation,
        });

        let entry = self
            .surfaces
            .get_mut(&access.surface())
            .expect("validated live Surface remains present");
        let LiveState::Published(published) = &mut entry.state else {
            unreachable!("validated published Surface changed without an intervening borrow")
        };
        published.transfer = TransferState::Transferred {
            token: transfer_token,
        };
        let record = self
            .handles
            .get_mut(&handle)
            .expect("validated fake handle remains present");
        record.transfer_token = Some(transfer_token);
        self.next_secret = next_secret;
        Ok(SurfaceTransfer {
            metadata,
            transport,
            handle: descriptor,
        })
    }

    /// Independently validates and consumes one transferred fake shared-memory handle.
    ///
    /// Failed validation never consumes the one-shot transfer.
    pub fn import(
        &mut self,
        transfer: SurfaceTransfer,
        context: &SurfaceConsumerContext,
    ) -> Result<ImportedSurface, SurfaceError> {
        self.validate_consumer_context(context)?;
        let surface = transfer.metadata.id;
        let parts = transfer.handle.parts();
        let (
            expected_access,
            expected_handle,
            expected_token,
            region_length,
            expected_metadata,
            expected_plan,
        ) = {
            let entry = self
                .surfaces
                .get(&surface)
                .ok_or_else(|| error(SurfaceErrorCode::UnknownSurface))?;
            let LiveState::Published(published) = &entry.state else {
                return Err(error(SurfaceErrorCode::InvalidState));
            };
            let TransferState::Transferred { token } = published.transfer else {
                return Err(error(SurfaceErrorCode::TransferConsumed));
            };
            (
                entry.access,
                entry.handle,
                token,
                entry.allocation.region_length,
                published.metadata.clone(),
                entry.allocation.plan.clone(),
            )
        };
        let handle_record = self
            .handles
            .get(&expected_handle)
            .copied()
            .ok_or_else(|| error(SurfaceErrorCode::InvalidHandle))?;

        if parts.class != HandleClass::SharedMemory {
            return Err(error(SurfaceErrorCode::InvalidHandleClass));
        }
        if parts.access != HandleAccess::ReadOnly {
            return Err(error(SurfaceErrorCode::InvalidHandleAccess));
        }
        if parts.id != expected_handle
            || parts.transfer_token == 0
            || parts.transfer_token != expected_token
            || parts.region_length != region_length
            || parts.worker != context.worker
            || parts.session != context.session
            || parts.worker_epoch != context.worker_epoch
            || parts.surface != surface
            || parts.generation != context.plan.generation()
            || handle_record.surface != parts.surface
            || handle_record.worker != parts.worker
            || handle_record.session != parts.session
            || handle_record.worker_epoch != parts.worker_epoch
            || handle_record.generation != parts.generation
            || handle_record.class != parts.class
            || handle_record.access != parts.access
            || handle_record.region_length != parts.region_length
            || handle_record.transfer_token != Some(parts.transfer_token)
            || handle_record.imported
        {
            return Err(error(SurfaceErrorCode::InvalidHandle));
        }
        if transfer.metadata.lease_token != expected_access.lease_token() {
            return Err(error(SurfaceErrorCode::InvalidLease));
        }
        if transfer.metadata.owner.worker != context.worker
            || transfer.metadata.owner.session != context.session
        {
            return Err(error(SurfaceErrorCode::InvalidOwner));
        }
        if transfer.metadata.generation != context.plan.generation() {
            return Err(error(SurfaceErrorCode::InvalidGeneration));
        }
        if context.plan != expected_plan {
            return Err(error(SurfaceErrorCode::InvalidPlan));
        }
        if transfer.metadata.format != context.plan.format()
            || transfer.metadata.alpha != context.plan.alpha()
        {
            return Err(error(SurfaceErrorCode::InvalidLayout));
        }
        if transfer.metadata.region != expected_metadata.region
            || transfer.metadata.width != expected_metadata.width
            || transfer.metadata.height != expected_metadata.height
            || transfer.metadata.stride != expected_metadata.stride
            || transfer.metadata.byte_offset != expected_metadata.byte_offset
            || transfer.metadata.byte_length != expected_metadata.byte_length
        {
            return Err(error(SurfaceErrorCode::InvalidLayout));
        }
        match &transfer.transport {
            SurfaceTransport::SharedMemory {
                slot: 0,
                region_length: declared,
            } if *declared == region_length => {}
            _ => return Err(error(SurfaceErrorCode::InvalidHandle)),
        }
        self.validate_canonical_surface(
            &transfer.metadata,
            &transfer.transport,
            &context.plan,
            context.worker,
            context.session,
            handle_record.region_length,
        )?;

        let entry = self
            .surfaces
            .get_mut(&surface)
            .expect("validated live Surface remains present");
        let LiveState::Published(published) = &mut entry.state else {
            unreachable!("validated published Surface changed without an intervening borrow")
        };
        published.transfer = TransferState::Imported {
            token: expected_token,
        };
        self.handles
            .get_mut(&expected_handle)
            .expect("validated fake handle remains present")
            .imported = true;
        Ok(ImportedSurface::new(
            expected_access,
            expected_handle,
            expected_token,
        ))
    }

    /// Acquires an immutable borrowed pixel view from one exact imported Surface.
    pub fn acquire<'a>(
        &'a self,
        imported: ImportedSurface,
        context: &SurfaceConsumerContext,
    ) -> Result<AcquiredSurface<'a>, SurfaceError> {
        self.validate_consumer_context(context)?;
        self.validate_owner_access(imported.access())?;
        let entry = self
            .surfaces
            .get(&imported.access().surface())
            .ok_or_else(|| error(SurfaceErrorCode::UnknownSurface))?;
        validate_entry_access(entry, imported.access())?;
        let LiveState::Published(published) = &entry.state else {
            return Err(error(SurfaceErrorCode::InvalidState));
        };
        if published.transfer
            != (TransferState::Imported {
                token: imported.transfer_token(),
            })
            || entry.handle != imported.handle()
        {
            return Err(error(SurfaceErrorCode::InvalidHandle));
        }
        let record = self
            .handles
            .get(&entry.handle)
            .ok_or_else(|| error(SurfaceErrorCode::InvalidHandle))?;
        if !record.imported
            || record.transfer_token != Some(imported.transfer_token())
            || record.access != HandleAccess::ReadOnly
            || record.class != HandleClass::SharedMemory
        {
            return Err(error(SurfaceErrorCode::InvalidHandle));
        }
        if published.metadata.format != context.plan.format()
            || published.metadata.alpha != context.plan.alpha()
        {
            return Err(error(SurfaceErrorCode::InvalidLayout));
        }
        self.validate_canonical_surface(
            &published.metadata,
            &published.transport,
            &context.plan,
            context.worker,
            context.session,
            record.region_length,
        )?;
        let layout = layout_for(&entry.allocation, self.limits)?;
        Ok(AcquiredSurface::new(
            &published.metadata,
            &entry.bytes[layout.offset..layout.end],
        ))
    }

    /// Drops one private Surface for cancellation, failure, or stale completion.
    pub fn discard_private(
        &mut self,
        access: SurfaceAccess,
        reason: RetireReason,
    ) -> Result<SurfaceReleaseReport, SurfaceError> {
        if !matches!(
            reason,
            RetireReason::Cancelled | RetireReason::Failed | RetireReason::StaleGeneration
        ) {
            return Err(error(SurfaceErrorCode::InvalidState));
        }
        self.validate_owner_access(access)?;
        let entry = self
            .surfaces
            .get(&access.surface())
            .ok_or_else(|| error(SurfaceErrorCode::UnknownSurface))?;
        validate_entry_access(entry, access)?;
        if !matches!(&entry.state, LiveState::Private { .. }) {
            return Err(error(SurfaceErrorCode::InvalidState));
        }
        Ok(self.retire_surface(access.surface(), reason))
    }

    /// Releases a live Surface or acknowledges the exact already-retired lease idempotently.
    pub fn release(&mut self, access: SurfaceAccess) -> Result<ReleaseOutcome, SurfaceError> {
        self.validate_owner_access(access)?;
        if let Some(entry) = self.surfaces.get(&access.surface()) {
            validate_entry_access(entry, access)?;
            let report = self.retire_surface(access.surface(), RetireReason::ReleasedByHost);
            return Ok(ReleaseOutcome::Released(report));
        }
        let retired = self
            .retired
            .get(&access.surface())
            .ok_or_else(|| error(SurfaceErrorCode::UnknownSurface))?;
        if retired.access != access {
            return Err(if retired.access.lease_token() != access.lease_token() {
                error(SurfaceErrorCode::InvalidLease)
            } else {
                error(SurfaceErrorCode::InvalidOwner)
            });
        }
        Ok(ReleaseOutcome::AlreadyRetired(retired.reason))
    }

    /// Advances only the deterministic virtual clock and reclaims leases at the exact deadline.
    pub fn advance_clock(&mut self, ticks: u64) -> Result<LifecycleReport, SurfaceError> {
        let now = self
            .now
            .checked_add(ticks)
            .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;
        self.now = now;
        let expired = self
            .surfaces
            .iter()
            .filter_map(|(surface, entry)| match &entry.state {
                LiveState::Published(published) if published.lease_deadline <= now => {
                    Some(*surface)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut released = SurfaceReleaseReport::default();
        for surface in expired {
            released.merge(self.retire_surface(surface, RetireReason::LeaseExpired));
        }
        Ok(LifecycleReport::new(released, self.current_resources()))
    }

    /// Replaces a Session viewport generation and drops every older private or published Surface.
    pub fn replace_generation(
        &mut self,
        session: SessionId,
        generation: u64,
    ) -> Result<LifecycleReport, SurfaceError> {
        let current = match self.sessions.get(&session).copied() {
            Some(SessionState::Active { generation }) => generation,
            Some(SessionState::Closed) | None => {
                return Err(error(SurfaceErrorCode::InvalidSession));
            }
        };
        if generation == 0 || generation <= current {
            return Err(error(SurfaceErrorCode::InvalidGeneration));
        }
        let stale = self
            .surfaces
            .iter()
            .filter_map(|(surface, entry)| {
                (entry.allocation.session == session
                    && entry.allocation.plan.generation() < generation)
                    .then_some((*surface, matches!(&entry.state, LiveState::Private { .. })))
            })
            .collect::<Vec<_>>();
        let mut released = SurfaceReleaseReport::default();
        for (surface, private) in stale {
            let reason = if private {
                RetireReason::StaleGeneration
            } else {
                RetireReason::GenerationReplaced
            };
            released.merge(self.retire_surface(surface, reason));
        }
        self.sessions
            .insert(session, SessionState::Active { generation });
        Ok(LifecycleReport::new(released, self.current_resources()))
    }

    /// Closes one Session, invalidates all of its storage, and returns exact zero-repeat evidence.
    pub fn close_session(&mut self, session: SessionId) -> Result<LifecycleReport, SurfaceError> {
        match self.sessions.get(&session).copied() {
            Some(SessionState::Closed) => {
                return Ok(LifecycleReport::new(
                    SurfaceReleaseReport::default(),
                    self.current_resources(),
                ));
            }
            Some(SessionState::Active { .. }) => {}
            None => return Err(error(SurfaceErrorCode::InvalidSession)),
        }
        self.sessions.insert(session, SessionState::Closed);
        let owned = self
            .surfaces
            .iter()
            .filter_map(|(surface, entry)| {
                (entry.allocation.session == session).then_some(*surface)
            })
            .collect::<Vec<_>>();
        let mut released = SurfaceReleaseReport::default();
        for surface in owned {
            released.merge(self.retire_surface(surface, RetireReason::SessionClosed));
        }
        Ok(LifecycleReport::new(released, self.current_resources()))
    }

    /// Invalidates the complete old Worker epoch and starts a distinct empty epoch.
    pub fn restart(
        &mut self,
        worker: WorkerId,
        worker_epoch: WorkerEpoch,
        renderer_epoch: RendererEpoch,
    ) -> Result<LifecycleReport, SurfaceError> {
        if worker.value() == 0
            || worker == self.worker
            || worker_epoch.value() <= self.worker_epoch.value()
            || renderer_epoch.value() == 0
            || renderer_epoch == self.renderer_epoch
        {
            return Err(error(SurfaceErrorCode::InvalidWorker));
        }
        let mut released = SurfaceReleaseReport::default();
        let surfaces = self.surfaces.keys().copied().collect::<Vec<_>>();
        for surface in surfaces {
            released.merge(self.retire_surface(surface, RetireReason::SessionClosed));
        }
        self.surfaces.clear();
        self.handles.clear();
        self.retired.clear();
        self.sessions.clear();
        self.worker = worker;
        self.worker_epoch = worker_epoch;
        self.renderer_epoch = renderer_epoch;
        self.now = 0;
        self.next_surface_id = 1;
        self.next_handle_id = 1;
        self.next_secret = INITIAL_SECRET;
        self.issued_surface_ids = 0;
        Ok(LifecycleReport::new(released, self.current_resources()))
    }

    /// Returns exact current resources without exposing bytes, leases, or handles.
    pub fn current_resources(&self) -> SurfaceResourceReport {
        let active_sessions = self
            .sessions
            .values()
            .filter(|state| matches!(state, SessionState::Active { .. }))
            .count();
        let mut private_surfaces = 0;
        let mut published_surfaces = 0;
        let mut imported_surfaces = 0;
        let mut retained_bytes = 0_u64;
        for entry in self.surfaces.values() {
            retained_bytes = retained_bytes
                .checked_add(entry.allocation.region_length)
                .expect("aggregate live bytes were precharged under a validated bound");
            match &entry.state {
                LiveState::Private { .. } => private_surfaces += 1,
                LiveState::Published(published) => {
                    if matches!(published.transfer, TransferState::Imported { .. }) {
                        imported_surfaces += 1;
                    } else {
                        published_surfaces += 1;
                    }
                }
            }
        }
        SurfaceResourceReport::new(
            active_sessions,
            private_surfaces,
            published_surfaces,
            imported_surfaces,
            self.handles.len(),
            retained_bytes,
        )
    }

    fn validate_allocation(&self, allocation: &SurfaceAllocation) -> Result<Layout, SurfaceError> {
        if allocation.worker != self.worker || allocation.worker_epoch != self.worker_epoch {
            return Err(error(SurfaceErrorCode::InvalidWorker));
        }
        let generation = match self.sessions.get(&allocation.session).copied() {
            Some(SessionState::Active { generation }) => generation,
            Some(SessionState::Closed) | None => {
                return Err(error(SurfaceErrorCode::InvalidSession));
            }
        };
        if allocation.plan.generation() == 0 || allocation.plan.generation() != generation {
            return Err(error(SurfaceErrorCode::InvalidGeneration));
        }
        validate_plan(&allocation.plan, self.renderer_epoch)?;
        if allocation.width != allocation.plan.binding().region().width
            || allocation.height != allocation.plan.binding().region().height
            || allocation.format != allocation.plan.format()
            || allocation.alpha != allocation.plan.alpha()
        {
            return Err(error(SurfaceErrorCode::InvalidLayout));
        }
        layout_for(allocation, self.limits)
    }

    fn validate_consumer_context(
        &self,
        context: &SurfaceConsumerContext,
    ) -> Result<(), SurfaceError> {
        if context.worker != self.worker || context.worker_epoch != self.worker_epoch {
            return Err(error(SurfaceErrorCode::InvalidWorker));
        }
        let generation = match self.sessions.get(&context.session).copied() {
            Some(SessionState::Active { generation }) => generation,
            Some(SessionState::Closed) | None => {
                return Err(error(SurfaceErrorCode::InvalidSession));
            }
        };
        if context.plan.generation() != generation {
            return Err(error(SurfaceErrorCode::InvalidGeneration));
        }
        validate_plan(&context.plan, self.renderer_epoch)
    }

    fn validate_owner_access(&self, access: SurfaceAccess) -> Result<(), SurfaceError> {
        if access.worker() != self.worker || access.worker_epoch() != self.worker_epoch {
            return Err(error(SurfaceErrorCode::InvalidWorker));
        }
        if access.session().value() == 0 || access.surface().value() == 0 {
            return Err(error(SurfaceErrorCode::InvalidOwner));
        }
        if access.lease_token() == 0 {
            return Err(error(SurfaceErrorCode::InvalidLease));
        }
        Ok(())
    }

    fn validate_canonical_surface(
        &self,
        metadata: &SurfaceMetadata,
        transport: &SurfaceTransport,
        plan: &SurfacePlanIdentity,
        worker: WorkerId,
        session: SessionId,
        region_length: u64,
    ) -> Result<(), SurfaceError> {
        let context = SurfaceValidationContext::new(
            worker,
            session,
            plan.generation(),
            plan.binding().clone(),
            self.handshake,
            1,
        )
        .with_shared_memory(0, region_length);
        self.validator
            .validate_surface(metadata, transport, &context)
            .map(|_| ())
            .map_err(map_protocol_error)
    }

    fn retire_surface(&mut self, surface: SurfaceId, reason: RetireReason) -> SurfaceReleaseReport {
        let entry = self
            .surfaces
            .remove(&surface)
            .expect("retirement is called only for a validated live Surface");
        let imported = matches!(
            &entry.state,
            LiveState::Published(PublishedState {
                transfer: TransferState::Imported { .. },
                ..
            })
        );
        let report = match entry.state {
            LiveState::Private { .. } => {
                SurfaceReleaseReport::private(entry.allocation.region_length)
            }
            LiveState::Published(_) => {
                SurfaceReleaseReport::published(entry.allocation.region_length, imported)
            }
        };
        self.handles
            .remove(&entry.handle)
            .expect("every live Surface owns exactly one fake handle");
        self.retired.insert(
            surface,
            RetiredRecord {
                access: entry.access,
                reason,
            },
        );
        report
    }
}

impl fmt::Debug for SurfaceOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceOwner")
            .field("worker", &self.worker)
            .field("worker_epoch", &self.worker_epoch)
            .field("renderer_epoch", &self.renderer_epoch)
            .field("virtual_tick", &self.now)
            .field("resources", &self.current_resources())
            .field("retired_records", &self.retired.len())
            .field("pixel_storage", &"[REDACTED]")
            .field("handle_table", &"[REDACTED]")
            .finish()
    }
}

fn validate_entry_access(entry: &SurfaceEntry, access: SurfaceAccess) -> Result<(), SurfaceError> {
    if entry.access.worker() != access.worker()
        || entry.access.session() != access.session()
        || entry.access.worker_epoch() != access.worker_epoch()
        || entry.access.surface() != access.surface()
    {
        return Err(error(SurfaceErrorCode::InvalidOwner));
    }
    if entry.access.lease_token() != access.lease_token() {
        return Err(error(SurfaceErrorCode::InvalidLease));
    }
    Ok(())
}

fn validate_plan(
    plan: &SurfacePlanIdentity,
    renderer_epoch: RendererEpoch,
) -> Result<(), SurfaceError> {
    let render = plan.binding().render();
    if plan.generation() == 0
        || plan.binding().region().width == 0
        || plan.binding().region().height == 0
        || render.renderer_epoch().value() == 0
        || render.renderer_epoch() != renderer_epoch
        || render.plan_id().value() == 0
        || digest_is_zero(render.render_config().digest())
        || digest_is_zero(render.plan_hash().digest())
        || digest_is_zero(render.scene_hash().digest())
        || digest_is_zero(render.decision_hash().digest())
    {
        return Err(error(SurfaceErrorCode::InvalidPlan));
    }
    match plan.format() {
        PixelFormat::Rgba8 => {}
    }
    match plan.alpha() {
        AlphaMode::Straight | AlphaMode::Premultiplied => {}
    }
    Ok(())
}

fn layout_for(
    allocation: &SurfaceAllocation,
    limits: SurfaceLimits,
) -> Result<Layout, SurfaceError> {
    let protocol = limits.protocol();
    if allocation.width == 0
        || allocation.height == 0
        || allocation.width > protocol.max_surface_dimension()
        || allocation.height > protocol.max_surface_dimension()
    {
        return Err(error(SurfaceErrorCode::InvalidLayout));
    }
    let bytes_per_pixel = match allocation.format {
        PixelFormat::Rgba8 => 4_u64,
    };
    let minimum_stride = u64::from(allocation.width)
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;
    let stride = u64::from(allocation.stride);
    if stride < minimum_stride
        || !stride.is_multiple_of(bytes_per_pixel)
        || stride > protocol.max_surface_stride_bytes()
    {
        return Err(error(SurfaceErrorCode::InvalidLayout));
    }
    let bytes = stride
        .checked_mul(u64::from(allocation.height))
        .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;
    if bytes == 0 || bytes > protocol.max_surface_bytes() {
        return Err(error(SurfaceErrorCode::InvalidLayout));
    }
    let end = allocation
        .byte_offset
        .checked_add(bytes)
        .ok_or_else(|| error(SurfaceErrorCode::NumericOverflow))?;
    if allocation.region_length == 0
        || allocation.region_length > protocol.max_surface_bytes()
        || end > allocation.region_length
    {
        return Err(error(SurfaceErrorCode::InvalidLayout));
    }
    let region_length = usize::try_from(allocation.region_length)
        .map_err(|_| error(SurfaceErrorCode::CapacityExceeded))?;
    let offset = usize::try_from(allocation.byte_offset)
        .map_err(|_| error(SurfaceErrorCode::CapacityExceeded))?;
    let end = usize::try_from(end).map_err(|_| error(SurfaceErrorCode::CapacityExceeded))?;
    Ok(Layout {
        bytes,
        offset,
        end,
        region_length,
    })
}

fn metadata_for(entry: &SurfaceEntry, worker: WorkerId) -> SurfaceMetadata {
    let render = entry.allocation.plan.binding().render();
    SurfaceMetadata {
        id: entry.access.surface(),
        lease_token: entry.access.lease_token(),
        owner: WireSurfaceOwner {
            worker,
            session: entry.allocation.session,
        },
        generation: entry.allocation.plan.generation(),
        region: entry.allocation.plan.binding().region().clone(),
        width: entry.allocation.width,
        height: entry.allocation.height,
        stride: entry.allocation.stride,
        format: entry.allocation.format,
        alpha: entry.allocation.alpha,
        byte_offset: entry.allocation.byte_offset,
        byte_length: entry.layout_bytes,
        render_config: render.render_config(),
        renderer_epoch: render.renderer_epoch(),
        plan_id: render.plan_id(),
        plan_hash: render.plan_hash(),
        scene_hash: render.scene_hash(),
        decision_hash: render.decision_hash(),
        backend: render.backend(),
    }
}

fn digest_is_zero(digest: &[u8; 32]) -> bool {
    digest.iter().all(|byte| *byte == 0)
}

fn shared_memory_handshake(
    validator: ProtocolValidator,
) -> Result<CompatibleHandshake, SurfaceError> {
    let limits = validator.limits();
    let hello = |role| ProtocolHello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        schema_hash: SCHEMA_HASH,
        endpoint_role: role,
        capabilities: EndpointCapabilities {
            supported: ENDPOINT_CAPABILITY_SHARED_MEMORY,
            mandatory: ENDPOINT_CAPABILITY_SHARED_MEMORY,
        },
        max_message_bytes: limits.max_payload_bytes().min(MAX_MESSAGE_BYTES),
        max_transfer_slots: limits.max_transfer_slots().min(MAX_TRANSFER_SLOTS),
    };
    validator
        .validate_handshake(&hello(EndpointRole::Host), &hello(EndpointRole::Engine))
        .map_err(map_protocol_error)
}

fn map_protocol_error(protocol: ProtocolError) -> SurfaceError {
    let code = match protocol.code() {
        ProtocolErrorCode::NumericOverflow => SurfaceErrorCode::NumericOverflow,
        ProtocolErrorCode::InvalidSurfaceOwner => SurfaceErrorCode::InvalidOwner,
        ProtocolErrorCode::InvalidSurfaceLease => SurfaceErrorCode::InvalidLease,
        ProtocolErrorCode::InvalidSurfaceEpoch => SurfaceErrorCode::InvalidGeneration,
        ProtocolErrorCode::InvalidSurfacePlan | ProtocolErrorCode::InvalidSurfaceRegion => {
            SurfaceErrorCode::InvalidPlan
        }
        ProtocolErrorCode::InvalidSurfaceLayout | ProtocolErrorCode::InvalidSurfaceRange => {
            SurfaceErrorCode::InvalidLayout
        }
        ProtocolErrorCode::InvalidSurfaceSlot
        | ProtocolErrorCode::InvalidSharedFence
        | ProtocolErrorCode::MissingEndpointCapability => SurfaceErrorCode::InvalidHandle,
        _ => SurfaceErrorCode::InvalidLayout,
    };
    error(code)
}
