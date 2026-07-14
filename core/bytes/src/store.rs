use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::source::{BackingBytes, ResidentTracker};
use crate::{
    ByteRange, ByteSlice, ByteSource, DataTicket, JobId, RangeResponse, ReadPoll, ReadRequest,
    ResumeSubscription, SmallRanges, SourceError, SourceErrorCategory, SourceErrorCode,
    SourceLimitKind, SourceSnapshot,
};

const HARD_MAX_INPUT_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_READ_BYTES: u64 = 16 * 1024 * 1024;
const HARD_MAX_CACHED_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_RESIDENT_BYTES: u64 = 128 * 1024 * 1024;
const HARD_MAX_SEGMENTS: usize = 4096;
const HARD_MAX_TICKETS: usize = 4096;
const HARD_MAX_SUBSCRIBERS_PER_TICKET: usize = 4096;
const HARD_MAX_TOTAL_SUBSCRIPTIONS: usize = 16_384;
const HARD_MAX_MISSING_RANGES: usize = 256;

/// Unvalidated Range-store resource-limit input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeStoreLimitConfig {
    /// Maximum immutable source length accepted by this store profile.
    pub max_input_bytes: u64,
    /// Maximum exact request or supplied response range.
    pub max_read_bytes: u64,
    /// Maximum unique source bytes retained after an operation.
    pub max_cached_bytes: u64,
    /// Maximum retained plus in-flight/coalescing bytes during an operation.
    pub max_resident_bytes: u64,
    /// Maximum disjoint cached backing segments.
    pub max_segments: usize,
    /// Maximum retained pending and terminal tickets.
    pub max_tickets: usize,
    /// Maximum job/checkpoint subscribers on one ticket.
    pub max_subscribers_per_ticket: usize,
    /// Maximum subscriptions retained across all tickets.
    pub max_total_subscriptions: usize,
    /// Maximum disjoint missing ranges emitted for one ticket.
    pub max_missing_ranges: usize,
}

impl Default for RangeStoreLimitConfig {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024 * 1024,
            max_read_bytes: 1024 * 1024,
            max_cached_bytes: 16 * 1024 * 1024,
            max_resident_bytes: 48 * 1024 * 1024,
            max_segments: 256,
            max_tickets: 256,
            max_subscribers_per_ticket: 256,
            max_total_subscriptions: 1024,
            max_missing_ranges: 64,
        }
    }
}

/// Validated deterministic per-store limits below fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeStoreLimits {
    max_input_bytes: u64,
    max_read_bytes: u64,
    max_cached_bytes: u64,
    max_resident_bytes: u64,
    max_segments: usize,
    max_tickets: usize,
    max_subscribers_per_ticket: usize,
    max_total_subscriptions: usize,
    max_missing_ranges: usize,
}

impl RangeStoreLimits {
    /// Validates a complete Range-store budget profile.
    pub fn validate(config: RangeStoreLimitConfig) -> Result<Self, SourceError> {
        if config.max_input_bytes == 0
            || config.max_input_bytes > HARD_MAX_INPUT_BYTES
            || config.max_read_bytes == 0
            || config.max_read_bytes > HARD_MAX_READ_BYTES
            || config.max_read_bytes > config.max_input_bytes
            || config.max_cached_bytes < config.max_read_bytes
            || config.max_cached_bytes > HARD_MAX_CACHED_BYTES
            || config.max_resident_bytes < config.max_cached_bytes
            || config.max_resident_bytes > HARD_MAX_RESIDENT_BYTES
            || config.max_segments == 0
            || config.max_segments > HARD_MAX_SEGMENTS
            || config.max_tickets == 0
            || config.max_tickets > HARD_MAX_TICKETS
            || config.max_subscribers_per_ticket == 0
            || config.max_subscribers_per_ticket > HARD_MAX_SUBSCRIBERS_PER_TICKET
            || config.max_total_subscriptions == 0
            || config.max_total_subscriptions > HARD_MAX_TOTAL_SUBSCRIPTIONS
            || config.max_missing_ranges == 0
            || config.max_missing_ranges > HARD_MAX_MISSING_RANGES
        {
            return Err(SourceError::for_code(SourceErrorCode::InvalidLimits));
        }
        Ok(Self {
            max_input_bytes: config.max_input_bytes,
            max_read_bytes: config.max_read_bytes,
            max_cached_bytes: config.max_cached_bytes,
            max_resident_bytes: config.max_resident_bytes,
            max_segments: config.max_segments,
            max_tickets: config.max_tickets,
            max_subscribers_per_ticket: config.max_subscribers_per_ticket,
            max_total_subscriptions: config.max_total_subscriptions,
            max_missing_ranges: config.max_missing_ranges,
        })
    }

    /// Returns the maximum immutable source length.
    pub const fn max_input_bytes(self) -> u64 {
        self.max_input_bytes
    }

    /// Returns the maximum exact read request size.
    pub const fn max_read_bytes(self) -> u64 {
        self.max_read_bytes
    }

    /// Returns the maximum total cached source bytes.
    pub const fn max_cached_bytes(self) -> u64 {
        self.max_cached_bytes
    }

    /// Returns the maximum retained plus in-flight/coalescing bytes.
    pub const fn max_resident_bytes(self) -> u64 {
        self.max_resident_bytes
    }

    /// Returns the maximum number of disjoint cached segments.
    pub const fn max_segments(self) -> usize {
        self.max_segments
    }

    /// Returns the maximum number of retained pending and terminal tickets.
    pub const fn max_tickets(self) -> usize {
        self.max_tickets
    }

    /// Returns the maximum number of job/checkpoint subscribers per ticket.
    pub const fn max_subscribers_per_ticket(self) -> usize {
        self.max_subscribers_per_ticket
    }

    /// Returns the maximum number of subscriptions retained across all tickets.
    pub const fn max_total_subscriptions(self) -> usize {
        self.max_total_subscriptions
    }

    /// Returns the maximum number of disjoint ranges in one pending result.
    pub const fn max_missing_ranges(self) -> usize {
        self.max_missing_ranges
    }
}

impl Default for RangeStoreLimits {
    fn default() -> Self {
        Self::validate(RangeStoreLimitConfig::default())
            .expect("built-in Range-store limits satisfy hard ceilings")
    }
}

/// Observable retained state of one data ticket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TicketStatus {
    /// The ticket still waits for all originally missing ranges.
    Pending {
        /// Number of distinct job/checkpoint subscribers.
        subscriber_count: usize,
    },
    /// Re-polling can complete with cached bytes or a newly observed EOF.
    Ready,
    /// The host marked the data operation failed.
    Failed(SourceError),
    /// The bound immutable source snapshot no longer matches the host response.
    SourceChanged,
    /// Every subscriber left before the ticket completed.
    Abandoned,
}

/// Result of injecting host-validated bytes or source metadata into a Range store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupplyOutcome {
    cached_bytes: u64,
    ready_tickets: Vec<DataTicket>,
}

impl SupplyOutcome {
    /// Returns the total unique cached bytes after insertion.
    pub const fn cached_bytes(&self) -> u64 {
        self.cached_bytes
    }

    /// Returns tickets that became ready during this update.
    ///
    /// The caller may enqueue their jobs after this method returns. The Range
    /// store never resumes parser work inline while holding its state lock.
    pub fn ready_tickets(&self) -> &[DataTicket] {
        &self.ready_tickets
    }
}

#[derive(Clone)]
struct Segment {
    start: u64,
    end_exclusive: u64,
    backing: Arc<BackingBytes>,
}

impl Segment {
    fn len(&self) -> u64 {
        self.end_exclusive - self.start
    }

    fn end_exclusive(&self) -> u64 {
        self.end_exclusive
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TicketPhase {
    Pending,
    Ready,
    Failed(SourceError),
    SourceChanged,
    Abandoned,
}

struct TicketRecord {
    ticket: DataTicket,
    missing: SmallRanges,
    subscribers: Vec<ResumeSubscription>,
    phase: TicketPhase,
}

struct StoreState {
    segments: Vec<Segment>,
    cached_bytes: u64,
    tickets: Vec<TicketRecord>,
    next_ticket: u64,
    observed_len: Option<u64>,
    subscription_count: usize,
}

/// Bounded in-memory Range store for an immutable source snapshot.
///
/// Responses are supplied by a host after file or network I/O. The store owns
/// stable copies, canonicalizes overlapping and adjacent segments, and turns
/// data arrival into terminal tickets without invoking parser callbacks.
pub struct RangeStore {
    snapshot: SourceSnapshot,
    limits: RangeStoreLimits,
    resident: Arc<ResidentTracker>,
    source_changed: AtomicBool,
    state: Mutex<StoreState>,
}

impl RangeStore {
    /// Creates an empty store and reserves only the configured bounded metadata.
    pub fn new(snapshot: SourceSnapshot, limits: RangeStoreLimits) -> Result<Self, SourceError> {
        if let Some(input_bytes) = snapshot.len()
            && input_bytes > limits.max_input_bytes
        {
            return Err(SourceError::resource(
                SourceLimitKind::InputBytes,
                limits.max_input_bytes,
                input_bytes,
            ));
        }
        let mut segments = Vec::new();
        segments
            .try_reserve_exact(limits.max_segments)
            .map_err(|_| {
                SourceError::resource(
                    SourceLimitKind::Allocation,
                    usize_to_u64(limits.max_segments),
                    usize_to_u64(limits.max_segments),
                )
            })?;
        let mut tickets = Vec::new();
        tickets.try_reserve_exact(limits.max_tickets).map_err(|_| {
            SourceError::resource(
                SourceLimitKind::Allocation,
                usize_to_u64(limits.max_tickets),
                usize_to_u64(limits.max_tickets),
            )
        })?;
        Ok(Self {
            snapshot,
            limits,
            resident: Arc::new(ResidentTracker::new(limits.max_resident_bytes)),
            source_changed: AtomicBool::new(false),
            state: Mutex::new(StoreState {
                segments,
                cached_bytes: 0,
                tickets,
                next_ticket: 1,
                observed_len: snapshot.len(),
                subscription_count: 0,
            }),
        })
    }

    /// Returns the deterministic limits fixed for this store.
    pub const fn limits(&self) -> RangeStoreLimits {
        self.limits
    }

    /// Returns the total unique cached bytes.
    pub fn cached_bytes(&self) -> Result<u64, SourceError> {
        Ok(self.lock_state()?.cached_bytes)
    }

    /// Returns charged backing, queued response, and coalescing capacity.
    pub fn resident_bytes(&self) -> u64 {
        self.resident.current()
    }

    /// Observes complete source metadata without supplying response bytes.
    ///
    /// This binds a previously unknown total length, including zero, and wakes
    /// pending exact reads that now resolve as EOF. A missing observed length
    /// is an idempotent no-op only while this store also remains unbound.
    pub fn observe_snapshot(
        &self,
        observed_snapshot: SourceSnapshot,
    ) -> Result<SupplyOutcome, SourceError> {
        if self.source_change_precedes_commit() {
            let mut state = self.lock_state()?;
            transition_source_changed(&mut state);
            return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        if observed_snapshot.identity() != self.snapshot.identity()
            || observed_snapshot.validator() != self.snapshot.validator()
        {
            self.source_changed.swap(true, Ordering::AcqRel);
            let mut state = self.lock_state()?;
            transition_source_changed(&mut state);
            return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        let mut state = self.lock_state()?;
        if self.synchronize_source_change(&mut state) {
            return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        let bind_observed_len = match (state.observed_len, observed_snapshot.len()) {
            (Some(expected), Some(observed)) if expected == observed => None,
            (Some(_), _) => {
                self.poison_source(&mut state);
                return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
            }
            (None, Some(observed)) => {
                if observed > self.limits.max_input_bytes {
                    return Err(SourceError::resource(
                        SourceLimitKind::InputBytes,
                        self.limits.max_input_bytes,
                        observed,
                    ));
                }
                if state
                    .segments
                    .last()
                    .is_some_and(|segment| segment.end_exclusive() > observed)
                {
                    self.poison_source(&mut state);
                    return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
                }
                Some(observed)
            }
            (None, None) => None,
        };

        let mut ready_tickets = Vec::new();
        ready_tickets
            .try_reserve_exact(state.tickets.len())
            .map_err(|_| {
                SourceError::resource(
                    SourceLimitKind::Allocation,
                    usize_to_u64(self.limits.max_tickets),
                    usize_to_u64(state.tickets.len()),
                )
            })?;
        if self.source_change_precedes_commit() {
            transition_source_changed(&mut state);
            return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        if let Some(observed) = bind_observed_len {
            state.observed_len = Some(observed);
            complete_tickets_at_eof(&mut state, observed, &mut ready_tickets);
        }
        Ok(SupplyOutcome {
            cached_bytes: state.cached_bytes,
            ready_tickets,
        })
    }

    /// Inserts one complete host response and reports newly ready tickets.
    ///
    /// A snapshot mismatch or conflicting overlap atomically poisons the store:
    /// every pending ticket becomes [`TicketStatus::SourceChanged`] and all
    /// later polls fail with [`SourceErrorCode::SourceChanged`].
    pub fn supply(&self, response: RangeResponse) -> Result<SupplyOutcome, SourceError> {
        let (observed_snapshot, range, response_bytes) = response.into_parts();
        if self.source_change_precedes_commit() {
            drop(response_bytes);
            let mut state = self.lock_state()?;
            transition_source_changed(&mut state);
            return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        if observed_snapshot.identity() != self.snapshot.identity()
            || observed_snapshot.validator() != self.snapshot.validator()
        {
            drop(response_bytes);
            self.source_changed.swap(true, Ordering::AcqRel);
            let mut state = self.lock_state()?;
            transition_source_changed(&mut state);
            return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        if range.len() > self.limits.max_read_bytes {
            let error = SourceError::resource(
                SourceLimitKind::ReadBytes,
                self.limits.max_read_bytes,
                range.len(),
            );
            drop(response_bytes);
            self.reject_if_observed_metadata_conflicts(observed_snapshot)?;
            return Err(error);
        }
        if range.end_exclusive() > self.limits.max_input_bytes {
            let error = SourceError::resource(
                SourceLimitKind::InputBytes,
                self.limits.max_input_bytes,
                range.end_exclusive(),
            );
            drop(response_bytes);
            self.reject_if_observed_metadata_conflicts(observed_snapshot)?;
            return Err(error);
        }
        // Response backing becomes Range-store in-flight state at this point.
        // Reserve before waiting on the state mutex so concurrent queued
        // supplies share the same hard resident ceiling.
        let response_capacity = match u64::try_from(response_bytes.capacity()) {
            Ok(capacity) => capacity,
            Err(_) => {
                drop(response_bytes);
                self.reject_if_observed_metadata_conflicts(observed_snapshot)?;
                return Err(SourceError::resource(
                    SourceLimitKind::Allocation,
                    self.limits.max_resident_bytes,
                    u64::MAX,
                ));
            }
        };
        let response_reservation = match self.resident.try_reserve(response_capacity) {
            Ok(reservation) => reservation,
            Err(error) => {
                drop(response_bytes);
                self.reject_if_observed_metadata_conflicts(observed_snapshot)?;
                return Err(error);
            }
        };
        let response_backing = response_reservation.adopt_vec(response_bytes)?;

        let mut state = self.lock_state()?;
        if self.synchronize_source_change(&mut state) {
            return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        let bind_observed_len = match (state.observed_len, observed_snapshot.len()) {
            (Some(expected), Some(observed)) if expected == observed => None,
            (Some(_), _) => {
                self.poison_source(&mut state);
                return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
            }
            (None, Some(observed)) => {
                if observed > self.limits.max_input_bytes {
                    return Err(SourceError::resource(
                        SourceLimitKind::InputBytes,
                        self.limits.max_input_bytes,
                        observed,
                    ));
                }
                if state
                    .segments
                    .last()
                    .is_some_and(|segment| segment.end_exclusive() > observed)
                {
                    self.poison_source(&mut state);
                    return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
                }
                Some(observed)
            }
            (None, None) => None,
        };

        let mut ready_tickets = Vec::new();
        ready_tickets
            .try_reserve_exact(state.tickets.len())
            .map_err(|_| {
                SourceError::resource(
                    SourceLimitKind::Allocation,
                    usize_to_u64(self.limits.max_tickets),
                    usize_to_u64(state.tickets.len()),
                )
            })?;

        for segment in &state.segments {
            if ranges_overlap(
                segment.start,
                segment.end_exclusive(),
                range.start(),
                range.end_exclusive(),
            ) && overlap_conflicts(segment, range, response_backing.as_slice())?
            {
                self.poison_source(&mut state);
                return Err(SourceError::for_code(SourceErrorCode::ConflictingBytes));
            }
        }

        if range_is_covered(&state.segments, range) {
            if self.source_change_precedes_commit() {
                transition_source_changed(&mut state);
                return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
            }
            if let Some(observed) = bind_observed_len {
                state.observed_len = Some(observed);
                complete_tickets_at_eof(&mut state, observed, &mut ready_tickets);
            }
            return Ok(SupplyOutcome {
                cached_bytes: state.cached_bytes,
                ready_tickets,
            });
        }

        let mut affected = Vec::new();
        affected
            .try_reserve_exact(state.segments.len())
            .map_err(|_| {
                SourceError::resource(
                    SourceLimitKind::Allocation,
                    usize_to_u64(self.limits.max_segments),
                    usize_to_u64(state.segments.len()),
                )
            })?;
        let mut union_start = range.start();
        let mut union_end = range.end_exclusive();
        let mut removed_bytes = 0_u64;
        for (index, segment) in state.segments.iter().enumerate() {
            if segment.start <= range.end_exclusive() && range.start() <= segment.end_exclusive() {
                affected.push(index);
                union_start = union_start.min(segment.start);
                union_end = union_end.max(segment.end_exclusive());
                removed_bytes = removed_bytes.checked_add(segment.len()).ok_or_else(|| {
                    SourceError::resource(
                        SourceLimitKind::CachedBytes,
                        self.limits.max_cached_bytes,
                        u64::MAX,
                    )
                })?;
            }
        }

        let merged_len_u64 = union_end
            .checked_sub(union_start)
            .ok_or_else(|| SourceError::for_code(SourceErrorCode::InternalState))?;
        let cached_without_affected = state
            .cached_bytes
            .checked_sub(removed_bytes)
            .ok_or_else(|| SourceError::for_code(SourceErrorCode::InternalState))?;
        let new_cached_bytes = cached_without_affected
            .checked_add(merged_len_u64)
            .ok_or_else(|| {
                SourceError::resource(
                    SourceLimitKind::CachedBytes,
                    self.limits.max_cached_bytes,
                    u64::MAX,
                )
            })?;
        if new_cached_bytes > self.limits.max_cached_bytes {
            return Err(SourceError::resource(
                SourceLimitKind::CachedBytes,
                self.limits.max_cached_bytes,
                new_cached_bytes,
            ));
        }
        let new_segment_count = state
            .segments
            .len()
            .checked_sub(affected.len())
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| {
                SourceError::resource(
                    SourceLimitKind::Segments,
                    usize_to_u64(self.limits.max_segments),
                    u64::MAX,
                )
            })?;
        if new_segment_count > self.limits.max_segments {
            return Err(SourceError::resource(
                SourceLimitKind::Segments,
                usize_to_u64(self.limits.max_segments),
                usize_to_u64(new_segment_count),
            ));
        }

        let backing = if affected.is_empty() {
            response_backing
        } else {
            let merged_reservation = self.resident.try_reserve_remaining(merged_len_u64)?;
            let merged_len = usize::try_from(merged_len_u64).map_err(|_| {
                SourceError::resource(
                    SourceLimitKind::Allocation,
                    self.limits.max_resident_bytes,
                    merged_len_u64,
                )
            })?;
            let mut merged_bytes = Vec::new();
            merged_bytes.try_reserve_exact(merged_len).map_err(|_| {
                SourceError::resource(
                    SourceLimitKind::Allocation,
                    self.limits.max_resident_bytes,
                    merged_len_u64,
                )
            })?;
            let merged_capacity = u64::try_from(merged_bytes.capacity()).map_err(|_| {
                SourceError::resource(
                    SourceLimitKind::Allocation,
                    self.limits.max_resident_bytes,
                    u64::MAX,
                )
            })?;
            if merged_capacity > merged_reservation.reserved_bytes() {
                let resident_before_reservation = self
                    .limits
                    .max_resident_bytes
                    .checked_sub(merged_reservation.reserved_bytes())
                    .ok_or_else(|| SourceError::for_code(SourceErrorCode::InternalState))?;
                let attempted = resident_before_reservation.saturating_add(merged_capacity);
                return Err(SourceError::resource(
                    SourceLimitKind::ResidentBytes,
                    self.limits.max_resident_bytes,
                    attempted,
                ));
            }
            merged_bytes.resize(merged_len, 0);
            for index in &affected {
                copy_into_union(
                    union_start,
                    &mut merged_bytes,
                    state.segments[*index].start,
                    state.segments[*index].backing.as_slice(),
                )?;
            }
            copy_into_union(
                union_start,
                &mut merged_bytes,
                range.start(),
                response_backing.as_slice(),
            )?;
            drop(response_backing);
            merged_reservation
                .shrink_to(merged_capacity)?
                .adopt_vec(merged_bytes)?
        };

        let mut prospective_segments = Vec::new();
        prospective_segments
            .try_reserve_exact(self.limits.max_segments)
            .map_err(|_| {
                SourceError::resource(
                    SourceLimitKind::Allocation,
                    usize_to_u64(self.limits.max_segments),
                    usize_to_u64(new_segment_count),
                )
            })?;
        for (index, segment) in state.segments.iter().enumerate() {
            if affected.binary_search(&index).is_err() {
                prospective_segments.push(segment.clone());
            }
        }
        let insertion = prospective_segments
            .binary_search_by_key(&union_start, |segment| segment.start)
            .unwrap_or_else(|index| index);
        prospective_segments.insert(
            insertion,
            Segment {
                start: union_start,
                end_exclusive: union_end,
                backing,
            },
        );

        let mut ticket_updates = Vec::new();
        ticket_updates
            .try_reserve_exact(state.tickets.len())
            .map_err(|_| {
                SourceError::resource(
                    SourceLimitKind::Allocation,
                    usize_to_u64(self.limits.max_tickets),
                    usize_to_u64(state.tickets.len()),
                )
            })?;
        for (index, record) in state.tickets.iter().enumerate() {
            if record.phase == TicketPhase::Pending {
                let remaining = remaining_missing_ranges(
                    &prospective_segments,
                    record.missing.as_slice(),
                    self.limits.max_missing_ranges,
                )?;
                let resolves_at_eof = bind_observed_len.is_some_and(|observed| {
                    remaining
                        .as_ref()
                        .is_some_and(|missing| missing_crosses_eof(missing, observed))
                });
                ticket_updates.push((index, if resolves_at_eof { None } else { remaining }));
            }
        }

        if self.source_change_precedes_commit() {
            transition_source_changed(&mut state);
            return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        state.segments = prospective_segments;
        state.cached_bytes = new_cached_bytes;
        if let Some(observed) = bind_observed_len {
            state.observed_len = Some(observed);
        }
        for (index, remaining) in ticket_updates {
            let record = &mut state.tickets[index];
            if let Some(missing) = remaining {
                record.missing = missing;
            } else {
                record.phase = TicketPhase::Ready;
                ready_tickets.push(record.ticket);
            }
        }

        Ok(SupplyOutcome {
            cached_bytes: state.cached_bytes,
            ready_tickets,
        })
    }

    /// Marks one pending ticket failed without resuming its subscribers inline.
    pub fn fail_ticket(&self, ticket: DataTicket, error: SourceError) -> Result<(), SourceError> {
        let mut state = self.lock_state()?;
        self.synchronize_source_change(&mut state);
        find_ticket_mut(&mut state, ticket)?;
        if error.category() == SourceErrorCategory::Integrity {
            // Snapshot integrity is session-wide even when this ticket already
            // committed another terminal state. Poison future work without
            // rewriting that first terminal state.
            self.poison_source(&mut state);
            return Ok(());
        }
        {
            let record = find_ticket_mut(&mut state, ticket)?;
            if record.phase != TicketPhase::Pending {
                return Err(SourceError::for_code(
                    SourceErrorCode::TicketAlreadyTerminal,
                ));
            }
        }
        let record = find_ticket_mut(&mut state, ticket)?;
        record.phase = TicketPhase::Failed(error);
        Ok(())
    }

    /// Atomically invalidates this snapshot and every pending ticket.
    ///
    /// Hosts call this when a validator changes before a response body is
    /// available. Repeated calls are idempotent.
    pub fn signal_source_changed(&self) -> Result<(), SourceError> {
        self.source_changed.swap(true, Ordering::AcqRel);
        let mut state = self.lock_state()?;
        transition_source_changed(&mut state);
        Ok(())
    }

    /// Removes every subscription for one job from a pending ticket.
    ///
    /// Other jobs remain subscribed. The ticket becomes abandoned only after
    /// its final subscriber leaves.
    pub fn unsubscribe(&self, ticket: DataTicket, job: JobId) -> Result<(), SourceError> {
        let mut state = self.lock_state()?;
        self.synchronize_source_change(&mut state);
        let removed = {
            let record = find_ticket_mut(&mut state, ticket)?;
            if record.phase != TicketPhase::Pending {
                return Err(SourceError::for_code(
                    SourceErrorCode::TicketAlreadyTerminal,
                ));
            }
            let before = record.subscribers.len();
            record
                .subscribers
                .retain(|subscriber| subscriber.job() != job);
            let removed = before - record.subscribers.len();
            if record.subscribers.is_empty() {
                record.phase = TicketPhase::Abandoned;
            }
            removed
        };
        state.subscription_count = state
            .subscription_count
            .checked_sub(removed)
            .ok_or_else(|| SourceError::for_code(SourceErrorCode::InternalState))?;
        Ok(())
    }

    /// Moves retained job/checkpoint subscriptions out of one terminal ticket.
    ///
    /// Runtime calls this after observing a terminal ticket and then requeues or
    /// terminates those jobs outside the store lock. A second call returns an
    /// empty vector and never repeats a wake target.
    pub fn take_subscriptions(
        &self,
        ticket: DataTicket,
    ) -> Result<Vec<ResumeSubscription>, SourceError> {
        let mut state = self.lock_state()?;
        self.synchronize_source_change(&mut state);
        let subscribers = {
            let record = find_ticket_mut(&mut state, ticket)?;
            if record.phase == TicketPhase::Pending {
                return Err(SourceError::for_code(SourceErrorCode::TicketNotTerminal));
            }
            std::mem::take(&mut record.subscribers)
        };
        state.subscription_count = state
            .subscription_count
            .checked_sub(subscribers.len())
            .ok_or_else(|| SourceError::for_code(SourceErrorCode::InternalState))?;
        Ok(subscribers)
    }

    /// Returns the retained state of one ticket.
    pub fn ticket_status(&self, ticket: DataTicket) -> Result<TicketStatus, SourceError> {
        let mut state = self.lock_state()?;
        self.synchronize_source_change(&mut state);
        let record = state
            .tickets
            .iter()
            .find(|record| record.ticket == ticket)
            .ok_or_else(|| SourceError::for_code(SourceErrorCode::UnknownTicket))?;
        Ok(match record.phase {
            TicketPhase::Pending => TicketStatus::Pending {
                subscriber_count: record.subscribers.len(),
            },
            TicketPhase::Ready => TicketStatus::Ready,
            TicketPhase::Failed(error) => TicketStatus::Failed(error),
            TicketPhase::SourceChanged => TicketStatus::SourceChanged,
            TicketPhase::Abandoned => TicketStatus::Abandoned,
        })
    }

    /// Releases one retained terminal ticket record.
    pub fn release_ticket(&self, ticket: DataTicket) -> Result<(), SourceError> {
        let mut state = self.lock_state()?;
        self.synchronize_source_change(&mut state);
        let index = state
            .tickets
            .iter()
            .position(|record| record.ticket == ticket)
            .ok_or_else(|| SourceError::for_code(SourceErrorCode::UnknownTicket))?;
        if state.tickets[index].phase == TicketPhase::Pending {
            return Err(SourceError::for_code(SourceErrorCode::TicketNotTerminal));
        }
        if !state.tickets[index].subscribers.is_empty() {
            return Err(SourceError::for_code(
                SourceErrorCode::SubscriptionsNotTaken,
            ));
        }
        let removed = state.tickets.remove(index).subscribers.len();
        state.subscription_count = state
            .subscription_count
            .checked_sub(removed)
            .ok_or_else(|| SourceError::for_code(SourceErrorCode::InternalState))?;
        Ok(())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, StoreState>, SourceError> {
        self.state
            .lock()
            .map_err(|_| SourceError::for_code(SourceErrorCode::InternalState))
    }

    fn reject_if_observed_metadata_conflicts(
        &self,
        observed_snapshot: SourceSnapshot,
    ) -> Result<(), SourceError> {
        let mut state = self.lock_state()?;
        if self.synchronize_source_change(&mut state) {
            return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        let conflicts = match (state.observed_len, observed_snapshot.len()) {
            (Some(expected), Some(observed)) => expected != observed,
            (Some(_), None) => true,
            (None, Some(observed)) => state
                .segments
                .last()
                .is_some_and(|segment| segment.end_exclusive() > observed),
            (None, None) => false,
        };
        if conflicts {
            self.poison_source(&mut state);
            return Err(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        Ok(())
    }

    fn source_change_precedes_commit(&self) -> bool {
        // This no-op RMW is the linearization point between a successful data
        // commit and signal_source_changed(). A preceding swap(true) makes the
        // comparison fail; a succeeding swap is ordered after this commit.
        self.source_changed
            .compare_exchange(false, false, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    }

    fn synchronize_source_change(&self, state: &mut StoreState) -> bool {
        let changed = self.source_change_precedes_commit();
        if changed {
            transition_source_changed(state);
        }
        changed
    }

    fn poison_source(&self, state: &mut StoreState) {
        self.source_changed.swap(true, Ordering::AcqRel);
        transition_source_changed(state);
    }
}

impl ByteSource for RangeStore {
    fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        let mut state = match self.lock_state() {
            Ok(state) => state,
            Err(error) => return ReadPoll::Failed(error),
        };
        if self.synchronize_source_change(&mut state) {
            return ReadPoll::Failed(SourceError::for_code(SourceErrorCode::SourceChanged));
        }
        if request.range().len() > self.limits.max_read_bytes {
            return ReadPoll::Failed(SourceError::resource(
                SourceLimitKind::ReadBytes,
                self.limits.max_read_bytes,
                request.range().len(),
            ));
        }
        if state
            .observed_len
            .is_some_and(|source_len| request.range().end_exclusive() > source_len)
        {
            return ReadPoll::EndOfFile;
        }
        if request.range().end_exclusive() > self.limits.max_input_bytes {
            return ReadPoll::Failed(SourceError::resource(
                SourceLimitKind::InputBytes,
                self.limits.max_input_bytes,
                request.range().end_exclusive(),
            ));
        }
        if range_is_covered(&state.segments, request.range()) {
            return match borrow_range(&state.segments, self.snapshot.identity(), request.range()) {
                Ok(bytes) => ReadPoll::Ready(bytes),
                Err(error) => ReadPoll::Failed(error),
            };
        }

        let missing = match missing_ranges(
            &state.segments,
            request.range(),
            self.limits.max_missing_ranges,
        ) {
            Ok(missing) => missing,
            Err(error) => return ReadPoll::Failed(error),
        };
        let subscriber = ResumeSubscription::new(request.job(), request.checkpoint());
        if let Some(index) = state
            .tickets
            .iter()
            .position(|record| record.phase == TicketPhase::Pending && record.missing == missing)
        {
            let record = &state.tickets[index];
            if let Some(existing) = record
                .subscribers
                .iter()
                .find(|existing| existing.job() == request.job())
            {
                if existing.checkpoint() != request.checkpoint() {
                    return ReadPoll::Failed(SourceError::for_code(
                        SourceErrorCode::CheckpointConflict,
                    ));
                }
                return ReadPoll::Pending {
                    ticket: record.ticket,
                    missing,
                };
            }
            if state.subscription_count >= self.limits.max_total_subscriptions {
                return ReadPoll::Failed(SourceError::resource(
                    SourceLimitKind::TotalSubscriptions,
                    usize_to_u64(self.limits.max_total_subscriptions),
                    usize_to_u64(state.subscription_count + 1),
                ));
            }
            let ticket = state.tickets[index].ticket;
            let inserted = if !state.tickets[index].subscribers.contains(&subscriber) {
                let record = &mut state.tickets[index];
                if record.subscribers.len() >= self.limits.max_subscribers_per_ticket {
                    return ReadPoll::Failed(SourceError::resource(
                        SourceLimitKind::TicketSubscribers,
                        usize_to_u64(self.limits.max_subscribers_per_ticket),
                        usize_to_u64(record.subscribers.len() + 1),
                    ));
                }
                if record.subscribers.try_reserve(1).is_err() {
                    return ReadPoll::Failed(SourceError::resource(
                        SourceLimitKind::Allocation,
                        usize_to_u64(self.limits.max_subscribers_per_ticket),
                        usize_to_u64(record.subscribers.len() + 1),
                    ));
                }
                record.subscribers.push(subscriber);
                true
            } else {
                false
            };
            if inserted {
                state.subscription_count += 1;
            }
            return ReadPoll::Pending { ticket, missing };
        }
        if state.tickets.len() >= self.limits.max_tickets {
            return ReadPoll::Failed(SourceError::resource(
                SourceLimitKind::Tickets,
                usize_to_u64(self.limits.max_tickets),
                usize_to_u64(state.tickets.len() + 1),
            ));
        }
        if state.subscription_count >= self.limits.max_total_subscriptions {
            return ReadPoll::Failed(SourceError::resource(
                SourceLimitKind::TotalSubscriptions,
                usize_to_u64(self.limits.max_total_subscriptions),
                usize_to_u64(state.subscription_count + 1),
            ));
        }
        let ticket = DataTicket(state.next_ticket);
        state.next_ticket = match state.next_ticket.checked_add(1) {
            Some(next) => next,
            None => {
                return ReadPoll::Failed(SourceError::resource(
                    SourceLimitKind::Tickets,
                    usize_to_u64(self.limits.max_tickets),
                    u64::MAX,
                ));
            }
        };
        let mut subscribers = Vec::new();
        if subscribers.try_reserve_exact(1).is_err() {
            return ReadPoll::Failed(SourceError::resource(SourceLimitKind::Allocation, 1, 1));
        }
        subscribers.push(subscriber);
        state.tickets.push(TicketRecord {
            ticket,
            missing: missing.clone(),
            subscribers,
            phase: TicketPhase::Pending,
        });
        state.subscription_count += 1;
        ReadPoll::Pending { ticket, missing }
    }
}

impl fmt::Debug for RangeStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RangeStore")
            .field("snapshot", &self.snapshot)
            .field("limits", &self.limits)
            .field("state", &"[REDACTED]")
            .finish()
    }
}

fn find_ticket_mut(
    state: &mut StoreState,
    ticket: DataTicket,
) -> Result<&mut TicketRecord, SourceError> {
    state
        .tickets
        .iter_mut()
        .find(|record| record.ticket == ticket)
        .ok_or_else(|| SourceError::for_code(SourceErrorCode::UnknownTicket))
}

fn transition_source_changed(state: &mut StoreState) {
    for record in &mut state.tickets {
        if record.phase == TicketPhase::Pending {
            record.phase = TicketPhase::SourceChanged;
        }
    }
}

fn missing_crosses_eof(missing: &SmallRanges, observed_len: u64) -> bool {
    missing
        .as_slice()
        .last()
        .is_some_and(|range| range.end_exclusive() > observed_len)
}

fn complete_tickets_at_eof(
    state: &mut StoreState,
    observed_len: u64,
    ready_tickets: &mut Vec<DataTicket>,
) {
    for record in &mut state.tickets {
        if record.phase == TicketPhase::Pending
            && missing_crosses_eof(&record.missing, observed_len)
        {
            record.phase = TicketPhase::Ready;
            ready_tickets.push(record.ticket);
        }
    }
}

fn ranges_overlap(left_start: u64, left_end: u64, right_start: u64, right_end: u64) -> bool {
    left_start < right_end && right_start < left_end
}

fn overlap_conflicts(
    segment: &Segment,
    response_range: ByteRange,
    response_bytes: &[u8],
) -> Result<bool, SourceError> {
    let overlap_start = segment.start.max(response_range.start());
    let overlap_end = segment.end_exclusive().min(response_range.end_exclusive());
    if overlap_start >= overlap_end {
        return Ok(false);
    }
    let segment_start = usize::try_from(overlap_start - segment.start)
        .map_err(|_| SourceError::for_code(SourceErrorCode::ResourceLimit))?;
    let response_start = usize::try_from(overlap_start - response_range.start())
        .map_err(|_| SourceError::for_code(SourceErrorCode::ResourceLimit))?;
    let overlap_len = usize::try_from(overlap_end - overlap_start)
        .map_err(|_| SourceError::for_code(SourceErrorCode::ResourceLimit))?;
    let segment_end = segment_start
        .checked_add(overlap_len)
        .ok_or_else(|| SourceError::for_code(SourceErrorCode::ResourceLimit))?;
    let response_end = response_start
        .checked_add(overlap_len)
        .ok_or_else(|| SourceError::for_code(SourceErrorCode::ResourceLimit))?;
    Ok(segment.backing.as_slice()[segment_start..segment_end]
        != response_bytes[response_start..response_end])
}

fn copy_into_union(
    union_start: u64,
    union: &mut [u8],
    source_start: u64,
    source: &[u8],
) -> Result<(), SourceError> {
    let offset = usize::try_from(source_start - union_start)
        .map_err(|_| SourceError::for_code(SourceErrorCode::ResourceLimit))?;
    let end = offset
        .checked_add(source.len())
        .ok_or_else(|| SourceError::for_code(SourceErrorCode::ResourceLimit))?;
    let destination = union
        .get_mut(offset..end)
        .ok_or_else(|| SourceError::for_code(SourceErrorCode::InternalState))?;
    destination.copy_from_slice(source);
    Ok(())
}

fn range_is_covered(segments: &[Segment], range: ByteRange) -> bool {
    let mut cursor = range.start();
    for segment in segments {
        if segment.end_exclusive() <= cursor {
            continue;
        }
        if segment.start > cursor {
            return false;
        }
        cursor = cursor.max(segment.end_exclusive());
        if cursor >= range.end_exclusive() {
            return true;
        }
    }
    false
}

fn borrow_range(
    segments: &[Segment],
    identity: crate::SourceIdentity,
    range: ByteRange,
) -> Result<ByteSlice, SourceError> {
    for segment in segments {
        if segment.start <= range.start() && segment.end_exclusive() >= range.end_exclusive() {
            let backing_offset = usize::try_from(range.start() - segment.start)
                .map_err(|_| SourceError::for_code(SourceErrorCode::InternalState))?;
            return ByteSlice::new(
                identity,
                range,
                Arc::clone(&segment.backing),
                backing_offset,
            );
        }
    }
    Err(SourceError::for_code(SourceErrorCode::InternalState))
}

fn missing_ranges(
    segments: &[Segment],
    requested: ByteRange,
    max_ranges: usize,
) -> Result<SmallRanges, SourceError> {
    let mut ranges = missing_range_storage(max_ranges)?;
    append_uncovered_ranges(segments, requested, &mut ranges, max_ranges)?;
    if ranges.is_empty() {
        return Err(SourceError::for_code(SourceErrorCode::InternalState));
    }
    SmallRanges::try_from_canonical(ranges, max_ranges)
}

fn remaining_missing_ranges(
    segments: &[Segment],
    requested: &[ByteRange],
    max_ranges: usize,
) -> Result<Option<SmallRanges>, SourceError> {
    let mut ranges = missing_range_storage(max_ranges)?;
    for range in requested {
        append_uncovered_ranges(segments, *range, &mut ranges, max_ranges)?;
    }
    if ranges.is_empty() {
        Ok(None)
    } else {
        SmallRanges::try_from_canonical(ranges, max_ranges).map(Some)
    }
}

fn missing_range_storage(max_ranges: usize) -> Result<Vec<ByteRange>, SourceError> {
    let mut ranges = Vec::new();
    ranges.try_reserve_exact(max_ranges).map_err(|_| {
        SourceError::resource(
            SourceLimitKind::Allocation,
            usize_to_u64(max_ranges),
            usize_to_u64(max_ranges),
        )
    })?;
    Ok(ranges)
}

fn append_uncovered_ranges(
    segments: &[Segment],
    requested: ByteRange,
    ranges: &mut Vec<ByteRange>,
    max_ranges: usize,
) -> Result<(), SourceError> {
    let mut cursor = requested.start();
    for segment in segments {
        if segment.end_exclusive() <= cursor {
            continue;
        }
        if segment.start >= requested.end_exclusive() {
            break;
        }
        if segment.start > cursor {
            if ranges.len() >= max_ranges {
                return Err(SourceError::resource(
                    SourceLimitKind::MissingRanges,
                    usize_to_u64(max_ranges),
                    usize_to_u64(ranges.len() + 1),
                ));
            }
            ranges.push(ByteRange::new(cursor, segment.start - cursor)?);
        }
        cursor = cursor.max(segment.end_exclusive());
        if cursor >= requested.end_exclusive() {
            break;
        }
    }
    if cursor < requested.end_exclusive() {
        if ranges.len() >= max_ranges {
            return Err(SourceError::resource(
                SourceLimitKind::MissingRanges,
                usize_to_u64(max_ranges),
                usize_to_u64(ranges.len() + 1),
            ));
        }
        ranges.push(ByteRange::new(cursor, requested.end_exclusive() - cursor)?);
    }
    Ok(())
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).expect("validated Range-store counts fit within fixed u64 ceilings")
}
