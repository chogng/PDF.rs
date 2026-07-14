use std::fmt;
use std::mem;

use pdf_rs_document::{DocumentCancellation, ReferenceChainLimits, ResolvedReference};
use pdf_rs_syntax::ObjectRef;

use crate::{
    ReadyStoreAdmissionError, ReadyStoreBinding, ReadyStoreError, ReadyStoreErrorCode,
    ReadyStoreKey, ReadyStoreLimit, ReadyStoreLimitKind, ReadyStoreLimits, ReadyStoreScope,
};

const CANCELLATION_INTERVAL: usize = 256;

/// Reason a complete Ready lookup did not hit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadyMissReason {
    /// The key belongs to another complete store binding or cache epoch.
    BindingMismatch,
    /// No exact root-and-resolution-profile entry is resident.
    NotFound,
}

/// Borrowed result of one cancellation-aware Ready lookup.
pub enum ReadyLookup<'store> {
    /// The exact immutable proof-bearing value is resident.
    Hit(&'store ResolvedReference),
    /// The complete key did not identify a resident value.
    Miss(ReadyMissReason),
}

impl fmt::Debug for ReadyLookup<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hit(_) => formatter.debug_tuple("Hit").field(&"[REDACTED]").finish(),
            Self::Miss(reason) => formatter.debug_tuple("Miss").field(reason).finish(),
        }
    }
}

/// Policy reason a successful value was returned instead of retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadyRejectReason {
    /// The key belongs to another store or the value's source/profile binding differs.
    BindingMismatch,
    /// The key root is not the value's requested root.
    RootMismatch,
    /// The key's cold-path limits differ from the value's producing limits.
    ResolutionProfileMismatch,
    /// The complete value-owned footprint exceeds the per-value ceiling.
    ValueTooLarge,
    /// Metadata plus this value's retained heap cannot fit the owner ceiling.
    ResidentLimit,
}

/// Rejected admission that returns the move-only proof-bearing value.
pub struct ReadyRejected {
    reason: ReadyRejectReason,
    limit: Option<ReadyStoreLimit>,
    value: ResolvedReference,
}

impl ReadyRejected {
    /// Returns the stable policy reason.
    pub const fn reason(&self) -> ReadyRejectReason {
        self.reason
    }

    /// Returns structured budget context for a size-policy rejection.
    pub const fn limit(&self) -> Option<ReadyStoreLimit> {
        self.limit
    }

    /// Returns the successful value to its caller without cloning.
    pub fn into_value(self) -> ResolvedReference {
        self.value
    }
}

impl fmt::Debug for ReadyRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyRejected")
            .field("reason", &self.reason)
            .field("limit", &self.limit)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// Metadata returned after a value becomes resident.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyAdmitted {
    replaced: bool,
    evicted: u64,
}

impl ReadyAdmitted {
    /// Reports whether an older value with the exact key was replaced.
    pub const fn replaced(self) -> bool {
        self.replaced
    }

    /// Returns the number of other deterministic LRU victims removed.
    pub const fn evicted(self) -> u64 {
        self.evicted
    }
}

/// Policy outcome of attempting to retain one successful Ready value.
#[derive(Debug)]
pub enum ReadyAdmission {
    /// The owner retained the value.
    Admitted(ReadyAdmitted),
    /// Policy declined retention and returned the successful value.
    Rejected(ReadyRejected),
}

/// Current and cumulative accounting for one session Ready store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyStoreStats {
    entries: u64,
    hits: u64,
    misses: u64,
    admissions: u64,
    replacements: u64,
    rejections: u64,
    evictions: u64,
    metadata_bytes: u64,
    value_heap_bytes: u64,
    resident_bytes: u64,
    peak_resident_bytes: u64,
}

impl ReadyStoreStats {
    /// Returns the current logical resident entry count.
    pub const fn entries(self) -> u64 {
        self.entries
    }

    /// Returns cumulative exact-key borrowed hits.
    pub const fn hits(self) -> u64 {
        self.hits
    }

    /// Returns cumulative binding or exact-key misses.
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

    /// Returns cumulative capacity or resident-pressure evictions.
    pub const fn evictions(self) -> u64 {
        self.evictions
    }

    /// Returns actual allocator-reported entry-vector capacity bytes.
    pub const fn metadata_bytes(self) -> u64 {
        self.metadata_bytes
    }

    /// Returns syntax and reference-path heap bytes retained by current values.
    pub const fn value_heap_bytes(self) -> u64 {
        self.value_heap_bytes
    }

    /// Returns current metadata plus retained value heap bytes.
    pub const fn resident_bytes(self) -> u64 {
        self.resident_bytes
    }

    /// Returns the greatest current resident total observed after publication.
    pub const fn peak_resident_bytes(self) -> u64 {
        self.peak_resident_bytes
    }
}

struct Entry {
    root: ObjectRef,
    resolution_limits: ReferenceChainLimits,
    heap_bytes: u64,
    value: ResolvedReference,
}

impl Entry {
    fn matches(&self, key: ReadyStoreKey) -> bool {
        self.root == key.root() && self.resolution_limits == key.resolution_limits()
    }
}

/// Single-writer, session-scoped store for immutable resolved Ready values.
///
/// Construction preallocates fixed entry capacity and charges its actual
/// allocator-reported bytes. Lookup returns a borrow, so a hit cannot be
/// extracted or cloned while the store retains ownership. Entries remain in
/// least-to-most-recently-used order, eliminating clocks and overflow.
pub struct ReadyStore {
    binding: ReadyStoreBinding,
    limits: ReadyStoreLimits,
    entries: Vec<Entry>,
    metadata_bytes: u64,
    value_heap_bytes: u64,
    hits: u64,
    misses: u64,
    admissions: u64,
    replacements: u64,
    rejections: u64,
    evictions: u64,
    peak_resident_bytes: u64,
}

impl ReadyStore {
    /// Allocates and charges fixed metadata capacity for one complete session binding.
    pub fn new(
        binding: ReadyStoreBinding,
        limits: ReadyStoreLimits,
    ) -> Result<Self, ReadyStoreError> {
        let scope = ReadyStoreScope::Session(binding.session_id());
        let entry_bytes = u64::try_from(mem::size_of::<Entry>())
            .map_err(|_| ReadyStoreError::allocation(limits.max_resident_bytes, u64::MAX, scope))?;
        let estimated_metadata = u64::try_from(limits.max_entries)
            .ok()
            .and_then(|count| count.checked_mul(entry_bytes))
            .ok_or_else(|| {
                ReadyStoreError::allocation(limits.max_resident_bytes, u64::MAX, scope)
            })?;
        if estimated_metadata > limits.max_resident_bytes {
            return Err(ReadyStoreError::resource(
                ReadyStoreLimitKind::ResidentBytes,
                limits.max_resident_bytes,
                0,
                estimated_metadata,
                scope,
                None,
            ));
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(limits.max_entries).map_err(|_| {
            ReadyStoreError::allocation(limits.max_resident_bytes, estimated_metadata, scope)
        })?;
        let metadata_bytes = u64::try_from(entries.capacity())
            .ok()
            .and_then(|capacity| capacity.checked_mul(entry_bytes))
            .ok_or_else(|| {
                ReadyStoreError::allocation(limits.max_resident_bytes, u64::MAX, scope)
            })?;
        if metadata_bytes > limits.max_resident_bytes {
            return Err(ReadyStoreError::resource(
                ReadyStoreLimitKind::ResidentBytes,
                limits.max_resident_bytes,
                0,
                metadata_bytes,
                scope,
                None,
            ));
        }
        Ok(Self {
            binding,
            limits,
            entries,
            metadata_bytes,
            value_heap_bytes: 0,
            hits: 0,
            misses: 0,
            admissions: 0,
            replacements: 0,
            rejections: 0,
            evictions: 0,
            peak_resident_bytes: metadata_bytes,
        })
    }

    /// Returns the complete immutable session binding.
    pub const fn binding(&self) -> ReadyStoreBinding {
        self.binding
    }

    /// Returns the validated owner limits.
    pub const fn limits(&self) -> ReadyStoreLimits {
        self.limits
    }

    /// Returns current and cumulative accounting.
    pub fn stats(&self) -> ReadyStoreStats {
        let entries = u64::try_from(self.entries.len())
            .expect("entry length remains beneath the validated u64 cache limit");
        let resident_bytes = self
            .metadata_bytes
            .checked_add(self.value_heap_bytes)
            .expect("current resident components remain beneath the validated ceiling");
        ReadyStoreStats {
            entries,
            hits: self.hits,
            misses: self.misses,
            admissions: self.admissions,
            replacements: self.replacements,
            rejections: self.rejections,
            evictions: self.evictions,
            metadata_bytes: self.metadata_bytes,
            value_heap_bytes: self.value_heap_bytes,
            resident_bytes,
            peak_resident_bytes: self.peak_resident_bytes,
        }
    }

    /// Looks up one complete key, checking cancellation before publishing a warm hit.
    pub fn lookup(
        &mut self,
        key: ReadyStoreKey,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<ReadyLookup<'_>, ReadyStoreError> {
        check_cancelled(cancellation)?;
        if key.binding() != self.binding {
            self.misses = checked_increment(self.misses)?;
            return Ok(ReadyLookup::Miss(ReadyMissReason::BindingMismatch));
        }
        let Some(index) = self.find_entry_index(key, cancellation)? else {
            self.misses = checked_increment(self.misses)?;
            return Ok(ReadyLookup::Miss(ReadyMissReason::NotFound));
        };
        let next_hits = checked_increment(self.hits)?;
        check_cancelled(cancellation)?;
        let entry = self.entries.remove(index);
        self.entries.push(entry);
        self.hits = next_hits;
        Ok(ReadyLookup::Hit(
            &self
                .entries
                .last()
                .expect("a hit moves one resident entry to the LRU tail")
                .value,
        ))
    }

    /// Attempts to retain one successful value under an exact complete key.
    ///
    /// Policy rejection and internal failure both return ownership of the value.
    /// Cancellation is checked throughout bounded planning and immediately
    /// before the no-fail commit changes resident contents or counters.
    pub fn try_admit(
        &mut self,
        key: ReadyStoreKey,
        value: ResolvedReference,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<ReadyAdmission, ReadyStoreAdmissionError> {
        if let Err(error) = check_cancelled(cancellation) {
            return admission_failure(error, value);
        }
        if key.binding() != self.binding || !value_matches_binding(&value, self.binding) {
            return self.reject(ReadyRejectReason::BindingMismatch, None, value);
        }
        if key.root() != value.root() {
            return self.reject(ReadyRejectReason::RootMismatch, None, value);
        }
        if key.resolution_limits() != value.limits() {
            return self.reject(ReadyRejectReason::ResolutionProfileMismatch, None, value);
        }
        let footprint = match value.try_resident_footprint() {
            Ok(footprint) => footprint,
            Err(error) => {
                return Err(ReadyStoreAdmissionError::new(
                    ReadyStoreError::from_footprint(error),
                    value,
                ));
            }
        };
        if footprint.total_bytes() > self.limits.max_value_bytes {
            let limit = self.limit(
                ReadyStoreLimitKind::ValueBytes,
                self.limits.max_value_bytes,
                0,
                footprint.total_bytes(),
                Some(key.root()),
            );
            return self.reject(ReadyRejectReason::ValueTooLarge, Some(limit), value);
        }
        let Some(heap_bytes) = footprint
            .syntax_heap_bytes()
            .checked_add(footprint.chain_capacity_bytes())
        else {
            return Err(ReadyStoreAdmissionError::new(
                ReadyStoreError::for_code(ReadyStoreErrorCode::InternalState),
                value,
            ));
        };
        let Some(max_heap) = self
            .limits
            .max_resident_bytes
            .checked_sub(self.metadata_bytes)
        else {
            return Err(ReadyStoreAdmissionError::new(
                ReadyStoreError::for_code(ReadyStoreErrorCode::InternalState),
                value,
            ));
        };
        if heap_bytes > max_heap {
            let Some(consumed) = self.metadata_bytes.checked_add(self.value_heap_bytes) else {
                return admission_failure(internal_state(), value);
            };
            let limit = self.limit(
                ReadyStoreLimitKind::ResidentBytes,
                self.limits.max_resident_bytes,
                consumed,
                heap_bytes,
                Some(key.root()),
            );
            return self.reject(ReadyRejectReason::ResidentLimit, Some(limit), value);
        }

        let replacement_index = match self.find_entry_index(key, cancellation) {
            Ok(index) => index,
            Err(error) => return admission_failure(error, value),
        };
        let replaced = replacement_index.is_some();
        let replacement_heap = replacement_index
            .map(|index| self.entries[index].heap_bytes)
            .unwrap_or(0);
        let Some(mut remaining_len) = self.entries.len().checked_sub(usize::from(replaced)) else {
            return admission_failure(internal_state(), value);
        };
        let Some(mut remaining_heap) = self.value_heap_bytes.checked_sub(replacement_heap) else {
            return admission_failure(internal_state(), value);
        };
        let mut victim_count = 0_usize;
        if !admission_fits(
            remaining_len,
            remaining_heap,
            self.limits.max_entries,
            heap_bytes,
            max_heap,
        ) {
            for (index, entry) in self.entries.iter().enumerate() {
                if let Err(error) = probe_scan(cancellation, index) {
                    return admission_failure(error, value);
                }
                if replacement_index == Some(index) {
                    continue;
                }
                if admission_fits(
                    remaining_len,
                    remaining_heap,
                    self.limits.max_entries,
                    heap_bytes,
                    max_heap,
                ) {
                    break;
                }
                let Some(next_len) = remaining_len.checked_sub(1) else {
                    return admission_failure(internal_state(), value);
                };
                let Some(next_heap) = remaining_heap.checked_sub(entry.heap_bytes) else {
                    return admission_failure(internal_state(), value);
                };
                let Some(next_victims) = victim_count.checked_add(1) else {
                    return admission_failure(internal_state(), value);
                };
                remaining_len = next_len;
                remaining_heap = next_heap;
                victim_count = next_victims;
            }
        }
        if let Err(error) = check_cancelled(cancellation) {
            return admission_failure(error, value);
        }
        if !admission_fits(
            remaining_len,
            remaining_heap,
            self.limits.max_entries,
            heap_bytes,
            max_heap,
        ) {
            return admission_failure(internal_state(), value);
        }

        let Some(new_heap_bytes) = remaining_heap.checked_add(heap_bytes) else {
            return admission_failure(internal_state(), value);
        };
        let Some(final_len) = remaining_len.checked_add(1) else {
            return admission_failure(internal_state(), value);
        };
        let Ok(evicted) = u64::try_from(victim_count) else {
            return admission_failure(internal_state(), value);
        };
        let Some(next_admissions) = self.admissions.checked_add(1) else {
            return admission_failure(internal_state(), value);
        };
        let next_replacements = if replaced {
            let Some(next) = self.replacements.checked_add(1) else {
                return admission_failure(internal_state(), value);
            };
            next
        } else {
            self.replacements
        };
        let Some(next_evictions) = self.evictions.checked_add(evicted) else {
            return admission_failure(internal_state(), value);
        };
        let Some(resident) = self.metadata_bytes.checked_add(new_heap_bytes) else {
            return admission_failure(internal_state(), value);
        };
        if final_len > self.limits.max_entries
            || final_len > self.entries.capacity()
            || resident > self.limits.max_resident_bytes
        {
            return admission_failure(internal_state(), value);
        }
        if let Err(error) = check_cancelled(cancellation) {
            return admission_failure(error, value);
        }

        // Planning skipped the first exact replacement and selected the first
        // `victim_count` other LRU entries. Retain removes that same exact set
        // in one linear commit, independent of where the replacement appears.
        let mut replacement_pending = replaced;
        let mut victims_pending = victim_count;
        self.entries.retain(|entry| {
            if replacement_pending && entry.matches(key) {
                replacement_pending = false;
                return false;
            }
            if victims_pending > 0 {
                victims_pending -= 1;
                return false;
            }
            true
        });
        debug_assert!(!replacement_pending);
        debug_assert_eq!(victims_pending, 0);
        debug_assert_eq!(self.entries.len(), remaining_len);
        self.entries.push(Entry {
            root: key.root(),
            resolution_limits: key.resolution_limits(),
            heap_bytes,
            value,
        });
        self.value_heap_bytes = new_heap_bytes;
        self.admissions = next_admissions;
        self.replacements = next_replacements;
        self.evictions = next_evictions;
        self.peak_resident_bytes = self.peak_resident_bytes.max(resident);
        Ok(ReadyAdmission::Admitted(ReadyAdmitted {
            replaced,
            evicted,
        }))
    }

    /// Drops every Ready value while retaining the precharged metadata capacity.
    pub fn clear(&mut self) -> u64 {
        let removed = u64::try_from(self.entries.len())
            .expect("entry length remains beneath the validated u64 cache limit");
        self.entries.clear();
        self.value_heap_bytes = 0;
        removed
    }

    fn reject(
        &mut self,
        reason: ReadyRejectReason,
        limit: Option<ReadyStoreLimit>,
        value: ResolvedReference,
    ) -> Result<ReadyAdmission, ReadyStoreAdmissionError> {
        let Some(rejections) = self.rejections.checked_add(1) else {
            return Err(ReadyStoreAdmissionError::new(
                ReadyStoreError::for_code(ReadyStoreErrorCode::InternalState),
                value,
            ));
        };
        self.rejections = rejections;
        Ok(ReadyAdmission::Rejected(ReadyRejected {
            reason,
            limit,
            value,
        }))
    }

    fn find_entry_index(
        &self,
        key: ReadyStoreKey,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<Option<usize>, ReadyStoreError> {
        for (index, entry) in self.entries.iter().enumerate() {
            probe_scan(cancellation, index)?;
            if entry.matches(key) {
                return Ok(Some(index));
            }
        }
        check_cancelled(cancellation)?;
        Ok(None)
    }

    fn limit(
        &self,
        kind: ReadyStoreLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        reference: Option<ObjectRef>,
    ) -> ReadyStoreLimit {
        ReadyStoreLimit::new(
            kind,
            limit,
            consumed,
            attempted,
            ReadyStoreScope::Session(self.binding.session_id()),
            reference,
        )
    }
}

impl fmt::Debug for ReadyStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyStore")
            .field("binding", &self.binding)
            .field("limits", &self.limits)
            .field("stats", &self.stats())
            .field("entries", &"[REDACTED]")
            .finish()
    }
}

fn value_matches_binding(value: &ResolvedReference, binding: ReadyStoreBinding) -> bool {
    let object = value.object();
    object.snapshot() == binding.snapshot()
        && object.revision_id() == binding.revision_id()
        && object.revision_startxref() == binding.revision_startxref()
        && object.object_limits() == binding.object_limits()
        && object.syntax_limits() == binding.syntax_limits()
}

fn admission_failure(
    error: ReadyStoreError,
    value: ResolvedReference,
) -> Result<ReadyAdmission, ReadyStoreAdmissionError> {
    Err(ReadyStoreAdmissionError::new(error, value))
}

fn admission_fits(
    resident_entries: usize,
    resident_heap: u64,
    max_entries: usize,
    incoming_heap: u64,
    max_heap: u64,
) -> bool {
    resident_entries < max_entries
        && resident_heap
            .checked_add(incoming_heap)
            .is_some_and(|bytes| bytes <= max_heap)
}

fn check_cancelled(cancellation: &(dyn DocumentCancellation + '_)) -> Result<(), ReadyStoreError> {
    if cancellation.is_cancelled() {
        return Err(ReadyStoreError::for_code(ReadyStoreErrorCode::Cancelled));
    }
    Ok(())
}

fn probe_scan(
    cancellation: &(dyn DocumentCancellation + '_),
    index: usize,
) -> Result<(), ReadyStoreError> {
    if index.is_multiple_of(CANCELLATION_INTERVAL) {
        check_cancelled(cancellation)?;
    }
    Ok(())
}

fn internal_state() -> ReadyStoreError {
    ReadyStoreError::for_code(ReadyStoreErrorCode::InternalState)
}

fn checked_increment(value: u64) -> Result<u64, ReadyStoreError> {
    value
        .checked_add(1)
        .ok_or_else(|| ReadyStoreError::for_code(ReadyStoreErrorCode::InternalState))
}
