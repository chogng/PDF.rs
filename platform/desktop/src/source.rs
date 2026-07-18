use std::collections::BTreeMap;
use std::sync::Arc;

use pdf_rs_protocol::{ByteRange, DataTicket, SessionId, SourceDescriptor};
use pdf_rs_surface::WorkerEpoch;

use crate::{
    CapabilityClass, CapabilityRights, DesktopCapability, DesktopCapabilityTable, DesktopIpcError,
    DesktopIpcErrorCode, DesktopIpcLimits, error::error,
};

/// One immutable Host-owned local source snapshot.
#[derive(Clone)]
pub struct HostSourceSnapshot {
    descriptor: SourceDescriptor,
    bytes: Arc<[u8]>,
}

impl HostSourceSnapshot {
    /// Creates one bounded snapshot without exposing its original path or file descriptor.
    pub fn new(
        descriptor: SourceDescriptor,
        bytes: Arc<[u8]>,
        limits: DesktopIpcLimits,
    ) -> Result<Self, DesktopIpcError> {
        if bytes.is_empty() || bytes.len() > limits.max_source_bytes() {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        if descriptor.length
            != Some(u64::try_from(bytes.len()).map_err(|_| error(DesktopIpcErrorCode::Source))?)
        {
            return Err(error(DesktopIpcErrorCode::Source));
        }
        Ok(Self { descriptor, bytes })
    }

    /// Returns the immutable source descriptor for canonical ticket validation.
    pub const fn descriptor(&self) -> &SourceDescriptor {
        &self.descriptor
    }
    /// Returns the bounded snapshot byte length without exposing backing ownership.
    pub fn byte_length(&self) -> usize {
        self.bytes.len()
    }

    fn checked_range(&self, range: &ByteRange) -> Result<(usize, usize), DesktopIpcError> {
        let start = usize::try_from(range.start).map_err(|_| error(DesktopIpcErrorCode::Source))?;
        let length = usize::try_from(range.len).map_err(|_| error(DesktopIpcErrorCode::Source))?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| error(DesktopIpcErrorCode::Source))?;
        if self.bytes.get(start..end).is_none() {
            return Err(error(DesktopIpcErrorCode::Source));
        }
        Ok((start, end))
    }
}

/// One exact bounded immutable segment granted for a pending `NeedData` ticket.
#[derive(Clone, Eq, PartialEq)]
pub struct SourceSegment {
    /// Exact ticket that owns this segment.
    pub ticket: DataTicket,
    /// Exact requested range.
    pub range: ByteRange,
    /// Authenticated, epoch-bound Host capability descriptor.
    pub capability: DesktopCapability,
    snapshot: Arc<[u8]>,
    start: usize,
    end: usize,
}

impl SourceSegment {
    /// Borrows bytes for a fresh unlinked segment capability.
    ///
    /// The child receives only that exact read-only shared segment, never the
    /// Host source path or its original file descriptor.
    pub fn bytes(&self) -> &[u8] {
        &self.snapshot[self.start..self.end]
    }
}

impl core::fmt::Debug for SourceSegment {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SourceSegment")
            .field("ticket", &self.ticket)
            .field("range", &self.range)
            .field("capability", &self.capability)
            .field("byte_length", &(self.end - self.start))
            .finish()
    }
}

/// Host-only exact ticket and immutable-range bridge.
pub struct HostRangeBridge {
    snapshot: HostSourceSnapshot,
    limits: DesktopIpcLimits,
    outstanding: BTreeMap<DataTicket, (SessionId, WorkerEpoch, Vec<ByteRange>)>,
    outstanding_ranges: usize,
    outstanding_range_bytes: u64,
    next_capability: u64,
}

impl HostRangeBridge {
    /// Creates an empty bounded ticket bridge for one immutable snapshot.
    pub fn new(snapshot: HostSourceSnapshot, limits: DesktopIpcLimits) -> Self {
        Self {
            snapshot,
            limits,
            outstanding: BTreeMap::new(),
            outstanding_ranges: 0,
            outstanding_range_bytes: 0,
            next_capability: 1,
        }
    }

    /// Registers one exact nonempty ticket request before any Host bytes are copied.
    pub fn register(
        &mut self,
        ticket: DataTicket,
        session: SessionId,
        epoch: WorkerEpoch,
        ranges: Vec<ByteRange>,
    ) -> Result<(), DesktopIpcError> {
        if ticket.value() == 0
            || session.value() == 0
            || ranges.is_empty()
            || self.outstanding.contains_key(&ticket)
            || self.outstanding.len() >= self.limits.max_capabilities()
            || ranges.len() > self.limits.max_capabilities()
        {
            return Err(error(DesktopIpcErrorCode::Source));
        }
        let mut requested_bytes = 0_u64;
        for range in &ranges {
            self.snapshot.checked_range(range)?;
            requested_bytes = requested_bytes
                .checked_add(range.len)
                .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        }
        let max_outstanding_bytes = u64::try_from(
            self.snapshot
                .byte_length()
                .min(self.limits.max_source_bytes()),
        )
        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        if requested_bytes > max_outstanding_bytes {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        let new_range_count = self
            .outstanding_ranges
            .checked_add(ranges.len())
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        let new_range_bytes = self
            .outstanding_range_bytes
            .checked_add(requested_bytes)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        if new_range_count > self.limits.max_capabilities()
            || new_range_bytes > max_outstanding_bytes
        {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        self.outstanding.insert(ticket, (session, epoch, ranges));
        self.outstanding_ranges = new_range_count;
        self.outstanding_range_bytes = new_range_bytes;
        Ok(())
    }

    /// Produces exact immutable source capabilities once and consumes the ticket.
    pub fn provide(
        &mut self,
        ticket: DataTicket,
        capabilities: &mut DesktopCapabilityTable,
    ) -> Result<Vec<SourceSegment>, DesktopIpcError> {
        let (session, epoch, ranges) = self
            .outstanding
            .get(&ticket)
            .cloned()
            .ok_or_else(|| error(DesktopIpcErrorCode::Source))?;
        let mut segments = Vec::new();
        segments
            .try_reserve_exact(ranges.len())
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        let mut staged = Vec::new();
        staged
            .try_reserve_exact(ranges.len())
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        let mut next = self.next_capability;
        for range in ranges {
            let (start, end) = self.snapshot.checked_range(&range)?;
            let id = next;
            next = id
                .checked_add(1)
                .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
            let capability = DesktopCapability::new(
                id,
                CapabilityClass::SourceSegment,
                CapabilityRights::ReadOnly,
                session,
                epoch,
                u64::try_from(end - start).map_err(|_| error(DesktopIpcErrorCode::Source))?,
            )?;
            staged.push((range, capability, start, end));
        }
        let mut descriptors = Vec::new();
        descriptors
            .try_reserve_exact(staged.len())
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        for (_, capability, _, _) in &staged {
            descriptors.push(*capability);
        }
        capabilities.can_insert_batch(&descriptors)?;
        for capability in descriptors {
            capabilities.insert(capability)?;
        }
        for (range, capability, start, end) in staged {
            segments.push(SourceSegment {
                ticket,
                range,
                capability,
                snapshot: Arc::clone(&self.snapshot.bytes),
                start,
                end,
            });
        }
        self.outstanding.remove(&ticket);
        self.outstanding_ranges = self
            .outstanding_ranges
            .checked_sub(segments.len())
            .ok_or_else(|| error(DesktopIpcErrorCode::Source))?;
        let released_bytes = segments
            .iter()
            .try_fold(0_u64, |total, segment| {
                total.checked_add(u64::try_from(segment.bytes().len()).ok()?)
            })
            .ok_or_else(|| error(DesktopIpcErrorCode::Source))?;
        self.outstanding_range_bytes = self
            .outstanding_range_bytes
            .checked_sub(released_bytes)
            .ok_or_else(|| error(DesktopIpcErrorCode::Source))?;
        self.next_capability = next;
        Ok(segments)
    }

    /// Invalidates all outstanding tickets after source change, close, disconnect, or restart.
    pub fn invalidate(&mut self) {
        self.outstanding.clear();
        self.outstanding_ranges = 0;
        self.outstanding_range_bytes = 0;
    }
    /// Returns outstanding ticket count for zero-resource shutdown evidence.
    pub fn outstanding(&self) -> usize {
        self.outstanding.len()
    }
}
