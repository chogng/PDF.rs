use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{
    ByteRange, SmallRanges, SourceError, SourceErrorCode, SourceIdentity, SourceLimitKind,
    SourceSnapshot,
};

pub(crate) struct ResidentTracker {
    current: AtomicU64,
    max: u64,
}

impl ResidentTracker {
    pub(crate) const fn new(max: u64) -> Self {
        Self {
            current: AtomicU64::new(0),
            max,
        }
    }

    pub(crate) fn current(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    pub(crate) fn try_reserve(
        self: &Arc<Self>,
        bytes: u64,
    ) -> Result<ResidentReservation, SourceError> {
        let mut current = self.current.load(Ordering::Acquire);
        loop {
            let attempted = current.checked_add(bytes).ok_or_else(|| {
                SourceError::resource(SourceLimitKind::ResidentBytes, self.max, u64::MAX)
            })?;
            if attempted > self.max {
                return Err(SourceError::resource(
                    SourceLimitKind::ResidentBytes,
                    self.max,
                    attempted,
                ));
            }
            // AcqRel publishes each reservation before backing allocation or
            // adoption; Release on Drop makes reclamation visible to later
            // reservations without ordering unrelated document bytes.
            match self.current.compare_exchange_weak(
                current,
                attempted,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(ResidentReservation {
                        tracker: Arc::clone(self),
                        bytes,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn try_reserve_remaining(
        self: &Arc<Self>,
        minimum: u64,
    ) -> Result<ResidentReservation, SourceError> {
        let mut current = self.current.load(Ordering::Acquire);
        loop {
            let available = self
                .max
                .checked_sub(current)
                .ok_or_else(|| SourceError::for_code(SourceErrorCode::InternalState))?;
            if available < minimum {
                let attempted = current.saturating_add(minimum);
                return Err(SourceError::resource(
                    SourceLimitKind::ResidentBytes,
                    self.max,
                    attempted,
                ));
            }
            // Coalescing temporarily claims every remaining byte before asking
            // the allocator for a Vec. The allocator-reported capacity is then
            // validated and the unused reservation returned before adoption.
            match self.current.compare_exchange_weak(
                current,
                self.max,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(ResidentReservation {
                        tracker: Arc::clone(self),
                        bytes: available,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    fn release(&self, bytes: u64) {
        let previous = self.current.fetch_sub(bytes, Ordering::Release);
        debug_assert!(previous >= bytes, "resident byte accounting underflow");
    }
}

pub(crate) struct ResidentReservation {
    tracker: Arc<ResidentTracker>,
    bytes: u64,
}

impl ResidentReservation {
    pub(crate) fn reserved_bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn shrink_to(mut self, bytes: u64) -> Result<Self, SourceError> {
        let released = self
            .bytes
            .checked_sub(bytes)
            .ok_or_else(|| SourceError::for_code(SourceErrorCode::InternalState))?;
        self.bytes = bytes;
        if released != 0 {
            self.tracker.release(released);
        }
        Ok(self)
    }

    pub(crate) fn adopt_vec(self, bytes: Vec<u8>) -> Result<Arc<BackingBytes>, SourceError> {
        let actual = u64::try_from(bytes.capacity())
            .map_err(|_| SourceError::for_code(SourceErrorCode::InternalState))?;
        if actual != self.bytes {
            return Err(SourceError::for_code(SourceErrorCode::InternalState));
        }
        Ok(Arc::new(BackingBytes {
            bytes,
            _reservation: self,
        }))
    }
}

impl Drop for ResidentReservation {
    fn drop(&mut self) {
        self.tracker.release(self.bytes);
    }
}

pub(crate) struct BackingBytes {
    // Rust drops struct fields in declaration order. Keeping the reservation
    // after storage prevents resident accounting from being released before
    // the allocation itself is gone.
    bytes: Vec<u8>,
    _reservation: ResidentReservation,
}

impl BackingBytes {
    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
}

/// Opaque runtime job identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JobId(u64);

impl JobId {
    /// Creates a runtime job identity.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric value for protocol adaptation.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Opaque identifier for a retained parser resume checkpoint.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResumeCheckpoint(u64);

impl ResumeCheckpoint {
    /// Creates a resume-checkpoint identifier owned by the caller.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric value for runtime bookkeeping.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// One runtime requeue target retained by a pending data ticket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResumeSubscription {
    job: JobId,
    checkpoint: ResumeCheckpoint,
}

impl ResumeSubscription {
    /// Creates a job/checkpoint subscription for runtime bookkeeping.
    pub const fn new(job: JobId, checkpoint: ResumeCheckpoint) -> Self {
        Self { job, checkpoint }
    }

    /// Returns the subscribed job.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the retained resume checkpoint.
    pub const fn checkpoint(self) -> ResumeCheckpoint {
        self.checkpoint
    }
}

/// Opaque one-shot data-arrival ticket allocated by a Range store.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DataTicket {
    namespace: u64,
    value: u64,
}

impl DataTicket {
    pub(crate) const fn new(namespace: u64, value: u64) -> Self {
        Self { namespace, value }
    }

    /// Returns the store-local opaque numeric value for runtime bookkeeping.
    ///
    /// Equality also includes a private Range-store namespace, so values from
    /// different stores never identify the same ticket.
    pub const fn value(self) -> u64 {
        self.value
    }
}

/// Scheduling priority for one byte request.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RequestPriority {
    /// Bytes needed by the currently visible page.
    VisiblePage,
    /// Font or image bytes needed for the first viewport.
    FirstViewportResource,
    /// Bytes for an adjacent-page prediction.
    AdjacentPage,
    /// Catalog, outline, page-label, or search metadata.
    Metadata,
    /// Background prefetch with the lowest urgency.
    BackgroundPrefetch,
}

impl RequestPriority {
    /// Returns a stable descending-urgency rank; a larger value is more urgent.
    pub const fn rank(self) -> u8 {
        match self {
            Self::VisiblePage => 5,
            Self::FirstViewportResource => 4,
            Self::AdjacentPage => 3,
            Self::Metadata => 2,
            Self::BackgroundPrefetch => 1,
        }
    }
}

/// One exact snapshot-bound byte request from a resumable Native job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadRequest {
    range: ByteRange,
    priority: RequestPriority,
    job: JobId,
    checkpoint: ResumeCheckpoint,
}

impl ReadRequest {
    /// Creates a byte request whose checkpoint remains owned by the caller.
    pub const fn new(
        range: ByteRange,
        priority: RequestPriority,
        job: JobId,
        checkpoint: ResumeCheckpoint,
    ) -> Self {
        Self {
            range,
            priority,
            job,
            checkpoint,
        }
    }

    /// Returns the exact requested byte range.
    pub const fn range(self) -> ByteRange {
        self.range
    }

    /// Returns the scheduling priority.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }

    /// Returns the requesting job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the caller-owned resume checkpoint identity.
    pub const fn checkpoint(self) -> ResumeCheckpoint {
        self.checkpoint
    }
}

/// Stable owned byte slice carrying the immutable source identity and range.
#[derive(Clone)]
pub struct ByteSlice {
    identity: SourceIdentity,
    range: ByteRange,
    backing: Arc<BackingBytes>,
    backing_offset: usize,
}

impl PartialEq for ByteSlice {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.range == other.range
            && self.bytes() == other.bytes()
    }
}

impl Eq for ByteSlice {}

impl ByteSlice {
    pub(crate) fn new(
        identity: SourceIdentity,
        range: ByteRange,
        backing: Arc<BackingBytes>,
        backing_offset: usize,
    ) -> Result<Self, SourceError> {
        let range_len = usize::try_from(range.len())
            .map_err(|_| SourceError::for_code(SourceErrorCode::ResponseLengthMismatch))?;
        let backing_end = backing_offset
            .checked_add(range_len)
            .ok_or_else(|| SourceError::for_code(SourceErrorCode::ResponseLengthMismatch))?;
        if backing_end > backing.as_slice().len() {
            return Err(SourceError::for_code(
                SourceErrorCode::ResponseLengthMismatch,
            ));
        }
        Ok(Self {
            identity,
            range,
            backing,
            backing_offset,
        })
    }

    /// Returns the immutable source identity that owns these bytes.
    pub const fn identity(&self) -> SourceIdentity {
        self.identity
    }

    /// Returns the exact source range represented by the slice.
    pub const fn range(&self) -> ByteRange {
        self.range
    }

    /// Borrows the stable owned bytes.
    pub fn bytes(&self) -> &[u8] {
        let len = usize::try_from(self.range.len())
            .expect("ByteSlice construction proved its range length fits usize");
        &self.backing.as_slice()[self.backing_offset..self.backing_offset + len]
    }
}

impl fmt::Debug for ByteSlice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ByteSlice")
            .field("identity", &self.identity)
            .field("range", &self.range)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// Host-supplied response bytes with complete snapshot metadata observed at receipt.
///
/// If a store was created with an unknown total length, its first response with
/// a known length binds that observed value for all later validation without
/// mutating the public session snapshot.
pub struct RangeResponse {
    snapshot: SourceSnapshot,
    range: ByteRange,
    bytes: Vec<u8>,
}

impl RangeResponse {
    /// Validates that the response byte count exactly matches its declared range.
    pub fn new(
        snapshot: SourceSnapshot,
        range: ByteRange,
        bytes: Vec<u8>,
    ) -> Result<Self, SourceError> {
        let expected = usize::try_from(range.len())
            .map_err(|_| SourceError::for_code(SourceErrorCode::ResponseLengthMismatch))?;
        if bytes.len() != expected {
            return Err(SourceError::for_code(
                SourceErrorCode::ResponseLengthMismatch,
            ));
        }
        if snapshot
            .len()
            .is_some_and(|source_len| range.end_exclusive() > source_len)
        {
            return Err(SourceError::for_code(SourceErrorCode::ResponseOutOfBounds));
        }
        Ok(Self {
            snapshot,
            range,
            bytes,
        })
    }

    pub(crate) fn into_parts(self) -> (SourceSnapshot, ByteRange, Vec<u8>) {
        (self.snapshot, self.range, self.bytes)
    }
}

impl fmt::Debug for RangeResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RangeResponse")
            .field("snapshot", &self.snapshot)
            .field("range", &self.range)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// Result of polling a synchronous, resumable byte source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadPoll<T> {
    /// The complete requested value is owned and stable.
    Ready(T),
    /// Data is absent; the runtime must wait for the one-shot ticket and requeue later.
    Pending {
        /// Ticket whose terminal transition wakes, but does not directly resume, jobs.
        ticket: DataTicket,
        /// Canonical missing ranges associated with the ticket.
        missing: SmallRanges,
    },
    /// The exact request crosses or begins beyond a known immutable source end.
    EndOfFile,
    /// The request cannot safely continue.
    Failed(SourceError),
}

/// Snapshot-bound synchronous byte source used by resumable core jobs.
pub trait ByteSource: Send + Sync {
    /// Returns the immutable snapshot bound for this source instance.
    fn snapshot(&self) -> SourceSnapshot;

    /// Polls one exact range without performing file, network, or async-runtime I/O.
    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice>;
}
