use std::collections::BTreeMap;

use pdf_rs_protocol::SessionId;
use pdf_rs_surface::WorkerEpoch;

use crate::{DesktopIpcError, DesktopIpcErrorCode, DesktopIpcLimits, error::error};

/// Capability kind carried by the separate desktop OOB table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CapabilityClass {
    /// Immutable Host-owned source segment readable only by the child.
    SourceSegment = 1,
    /// Immutable shared pixel region readable only by the Host consumer.
    SurfaceRegion = 2,
}

/// Minimum rights granted to a desktop shared-memory capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CapabilityRights {
    /// The capability grants read-only mapping only.
    ReadOnly = 1,
}

/// One exact OOB descriptor; native FD ownership is kept out of Debug output.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct DesktopCapability {
    id: u64,
    class: CapabilityClass,
    rights: CapabilityRights,
    owner: SessionId,
    worker_epoch: WorkerEpoch,
    byte_length: u64,
}

impl DesktopCapability {
    /// Creates a fully bound nonzero immutable capability descriptor.
    pub fn new(
        id: u64,
        class: CapabilityClass,
        rights: CapabilityRights,
        owner: SessionId,
        worker_epoch: WorkerEpoch,
        byte_length: u64,
    ) -> Result<Self, DesktopIpcError> {
        if id == 0 || owner.value() == 0 || byte_length == 0 {
            return Err(error(DesktopIpcErrorCode::Capability));
        }
        Ok(Self {
            id,
            class,
            rights,
            owner,
            worker_epoch,
            byte_length,
        })
    }

    /// Returns the opaque descriptor identity.
    pub const fn id(self) -> u64 {
        self.id
    }
    /// Returns the bounded shared-memory kind.
    pub const fn class(self) -> CapabilityClass {
        self.class
    }
    /// Returns its exact minimum rights.
    pub const fn rights(self) -> CapabilityRights {
        self.rights
    }
    /// Returns the owner session.
    pub const fn owner(self) -> SessionId {
        self.owner
    }
    /// Returns the exact child Worker epoch.
    pub const fn worker_epoch(self) -> WorkerEpoch {
        self.worker_epoch
    }
    /// Returns the granted extent.
    pub const fn byte_length(self) -> u64 {
        self.byte_length
    }
}

impl core::fmt::Debug for DesktopCapability {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("DesktopCapability")
            .field("class", &self.class)
            .field("rights", &self.rights)
            .field("owner", &"[REDACTED]")
            .field("worker_epoch", &self.worker_epoch)
            .field("byte_length", &self.byte_length)
            .field("id", &"[REDACTED]")
            .finish()
    }
}

/// Host-owned bounded capability ledger. Restart or disconnect revokes every entry.
pub struct DesktopCapabilityTable {
    limits: DesktopIpcLimits,
    entries: BTreeMap<u64, CapabilityEntry>,
    retained_bytes: u64,
}

struct CapabilityEntry {
    descriptor: DesktopCapability,
    state: CapabilityState,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CapabilityState {
    Live,
    Consumed,
    Revoked,
}

impl DesktopCapabilityTable {
    /// Creates an empty bounded table.
    pub fn new(limits: DesktopIpcLimits) -> Self {
        Self {
            limits,
            entries: BTreeMap::new(),
            retained_bytes: 0,
        }
    }

    /// Inserts one never-reused capability after exact epoch/owner validation.
    pub fn insert(&mut self, capability: DesktopCapability) -> Result<(), DesktopIpcError> {
        self.can_insert_batch(core::slice::from_ref(&capability))?;
        let retained_bytes = self
            .retained_bytes
            .checked_add(capability.byte_length())
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        self.entries.insert(
            capability.id(),
            CapabilityEntry {
                descriptor: capability,
                state: CapabilityState::Live,
            },
        );
        self.retained_bytes = retained_bytes;
        Ok(())
    }

    /// Checks a whole capability batch before any source ticket state changes.
    ///
    /// This is deliberately separate from insertion so a `ProvideData` grant
    /// cannot leave a partially admitted ticket behind when an aggregate bound
    /// is reached.
    pub fn can_insert_batch(
        &self,
        descriptors: &[DesktopCapability],
    ) -> Result<(), DesktopIpcError> {
        let resulting_count = self
            .entries
            .len()
            .checked_add(descriptors.len())
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        if resulting_count > self.limits.max_capabilities() {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        let mut retained = self.retained_bytes;
        for (index, descriptor) in descriptors.iter().enumerate() {
            if self.entries.contains_key(&descriptor.id())
                || descriptors[..index]
                    .iter()
                    .any(|prior| prior.id() == descriptor.id())
            {
                return Err(error(DesktopIpcErrorCode::Capability));
            }
            retained = retained
                .checked_add(descriptor.byte_length())
                .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        }
        if retained
            > u64::try_from(self.limits.max_capability_bytes())
                .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?
        {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        Ok(())
    }

    /// Validates descriptor identity, class, rights, owner, epoch, and exact extent.
    pub fn validate(
        &self,
        descriptor: DesktopCapability,
        expected_class: CapabilityClass,
        owner: SessionId,
        epoch: WorkerEpoch,
    ) -> Result<(), DesktopIpcError> {
        let Some(actual) = self.entries.get(&descriptor.id()) else {
            return Err(error(DesktopIpcErrorCode::Capability));
        };
        if actual.state != CapabilityState::Live
            || actual.descriptor != descriptor
            || descriptor.class() != expected_class
            || descriptor.rights() != CapabilityRights::ReadOnly
            || descriptor.owner() != owner
            || descriptor.worker_epoch() != epoch
        {
            return Err(error(DesktopIpcErrorCode::Capability));
        }
        Ok(())
    }

    /// Consumes one exact capability once after successful native descriptor import.
    pub fn consume(&mut self, id: u64) -> Result<(), DesktopIpcError> {
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or_else(|| error(DesktopIpcErrorCode::Capability))?;
        if entry.state != CapabilityState::Live {
            return Err(error(DesktopIpcErrorCode::Capability));
        }
        entry.state = CapabilityState::Consumed;
        Ok(())
    }

    /// Releases backing accounting after a consumer terminal release.
    pub fn release(&mut self, id: u64) -> Result<(), DesktopIpcError> {
        let entry = self
            .entries
            .remove(&id)
            .ok_or_else(|| error(DesktopIpcErrorCode::Capability))?;
        if entry.state == CapabilityState::Revoked {
            return Err(error(DesktopIpcErrorCode::Capability));
        }
        self.retained_bytes = self
            .retained_bytes
            .checked_sub(entry.descriptor.byte_length())
            .ok_or_else(|| error(DesktopIpcErrorCode::Capability))?;
        Ok(())
    }

    /// Revokes every capability on disconnect, crash, close, or restart.
    pub fn revoke_all(&mut self) {
        self.entries.clear();
        self.retained_bytes = 0;
    }
    /// Returns exact live capability count for shutdown evidence.
    pub fn live_count(&self) -> usize {
        self.entries.len()
    }
    /// Returns aggregate Host-owned bytes still represented by live capabilities.
    pub const fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }
}
