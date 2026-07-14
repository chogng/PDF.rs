use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_syntax::ObjectRef;
use pdf_rs_xref::{XrefEntryKind, XrefSection};

use crate::model::{LogicalEntry, LogicalEntryState};
use crate::{
    CandidateRevisionIndex, DocumentError, DocumentErrorCode, DocumentIndexStats,
    DocumentLimitKind, DocumentLimits, PhysicalObjectInterval, RevisionId,
};

const CANCELLATION_INTERVAL: u64 = 256;
const ACCOUNTED_LOGICAL_ENTRY_BYTES: u64 = 32;
const ACCOUNTED_PHYSICAL_INTERVAL_BYTES: u64 = 64;

/// Cooperative cancellation probe supplied by the owning document runtime.
pub trait DocumentCancellation: Send + Sync {
    /// Reports whether index construction must stop at the next bounded probe.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation probe that never requests cancellation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NeverCancelled;

impl DocumentCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl DocumentCancellation for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

impl CandidateRevisionIndex {
    /// Builds an unauthenticated candidate interval index from one parsed xref section.
    ///
    /// Construction validates total, in-use, retained-entry-capacity, allocation, and sort budgets; checks
    /// cancellation in all long loops; rejects duplicate or out-of-revision physical offsets; and
    /// requires the trailer root to name an exact-generation in-use row. The result still does not
    /// attest that any offset begins a top-level indirect object.
    pub fn from_xref(
        section: &XrefSection,
        revision_id: RevisionId,
        limits: DocumentLimits,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<Self, DocumentError> {
        check_cancelled(cancellation)?;

        let entries = section.entries();
        let total_entries = u64::try_from(entries.len()).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::TotalEntries,
                limits.max_total_entries,
                0,
                u64::MAX,
                None,
            )
        })?;
        if total_entries > limits.max_total_entries {
            return Err(DocumentError::resource(
                DocumentLimitKind::TotalEntries,
                limits.max_total_entries,
                0,
                total_entries,
                None,
            ));
        }

        let mut in_use_entries = 0_u64;
        for (index, entry) in entries.iter().enumerate() {
            probe_loop(cancellation, index)?;
            if matches!(entry.kind(), XrefEntryKind::InUse { .. }) {
                in_use_entries = in_use_entries.checked_add(1).ok_or_else(|| {
                    DocumentError::resource(
                        DocumentLimitKind::InUseEntries,
                        limits.max_in_use_entries,
                        0,
                        u64::MAX,
                        None,
                    )
                })?;
            }
        }
        check_cancelled(cancellation)?;
        if in_use_entries > limits.max_in_use_entries {
            return Err(DocumentError::resource(
                DocumentLimitKind::InUseEntries,
                limits.max_in_use_entries,
                0,
                in_use_entries,
                None,
            ));
        }

        let (requested_logical_bytes, requested_physical_bytes, requested_index_bytes) =
            accounted_index_bytes(total_entries, in_use_entries).ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::LogicalIndexBytes,
                    limits.max_logical_index_bytes,
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        if requested_index_bytes > limits.max_logical_index_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::LogicalIndexBytes,
                limits.max_logical_index_bytes,
                0,
                requested_index_bytes,
                None,
            ));
        }

        let mut logical_entries = Vec::new();
        logical_entries
            .try_reserve_exact(entries.len())
            .map_err(|_| {
                DocumentError::resource(
                    DocumentLimitKind::Allocation,
                    limits.max_logical_index_bytes,
                    0,
                    requested_logical_bytes,
                    None,
                )
            })?;
        let logical_capacity = u64::try_from(logical_entries.capacity()).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::Allocation,
                limits.max_logical_index_bytes,
                0,
                requested_logical_bytes,
                None,
            )
        })?;
        let logical_entry_bytes = logical_capacity
            .checked_mul(ACCOUNTED_LOGICAL_ENTRY_BYTES)
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::LogicalIndexBytes,
                    limits.max_logical_index_bytes,
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        if logical_entry_bytes > limits.max_logical_index_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::LogicalIndexBytes,
                limits.max_logical_index_bytes,
                0,
                logical_entry_bytes,
                None,
            ));
        }
        let physical_capacity = usize::try_from(in_use_entries).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::Allocation,
                limits.max_logical_index_bytes,
                logical_entry_bytes,
                requested_physical_bytes,
                None,
            )
        })?;
        let mut physical_intervals = Vec::new();
        physical_intervals
            .try_reserve_exact(physical_capacity)
            .map_err(|_| {
                DocumentError::resource(
                    DocumentLimitKind::Allocation,
                    limits.max_logical_index_bytes,
                    logical_entry_bytes,
                    requested_physical_bytes,
                    None,
                )
            })?;
        let retained_physical_capacity =
            u64::try_from(physical_intervals.capacity()).map_err(|_| {
                DocumentError::resource(
                    DocumentLimitKind::Allocation,
                    limits.max_logical_index_bytes,
                    logical_entry_bytes,
                    requested_physical_bytes,
                    None,
                )
            })?;
        let physical_interval_bytes = retained_physical_capacity
            .checked_mul(ACCOUNTED_PHYSICAL_INTERVAL_BYTES)
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::LogicalIndexBytes,
                    limits.max_logical_index_bytes,
                    logical_entry_bytes,
                    u64::MAX,
                    None,
                )
            })?;
        let logical_index_bytes = logical_entry_bytes
            .checked_add(physical_interval_bytes)
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::LogicalIndexBytes,
                    limits.max_logical_index_bytes,
                    logical_entry_bytes,
                    u64::MAX,
                    None,
                )
            })?;
        if logical_index_bytes > limits.max_logical_index_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::LogicalIndexBytes,
                limits.max_logical_index_bytes,
                logical_entry_bytes,
                physical_interval_bytes,
                None,
            ));
        }

        for (logical_slot, entry) in entries.iter().enumerate() {
            probe_loop(cancellation, logical_slot)?;
            let state = match entry.kind() {
                XrefEntryKind::Free { .. } => LogicalEntryState::Free,
                XrefEntryKind::InUse { offset } => {
                    if offset >= section.startxref() {
                        let reference =
                            ObjectRef::new(entry.object_number(), entry.generation()).ok();
                        return Err(DocumentError::for_code(
                            DocumentErrorCode::InvalidPhysicalOffset,
                            reference,
                            Some(offset),
                        ));
                    }
                    let reference = ObjectRef::new(entry.object_number(), entry.generation())
                        .map_err(|_| {
                            DocumentError::for_code(
                                DocumentErrorCode::InvalidXrefEntry,
                                None,
                                Some(offset),
                            )
                        })?;
                    let logical_slot = u32::try_from(logical_slot).map_err(|_| {
                        DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(reference),
                            Some(offset),
                        )
                    })?;
                    physical_intervals.push(PhysicalObjectInterval {
                        revision_id,
                        reference,
                        xref_offset: offset,
                        object_upper_bound: section.startxref(),
                        logical_slot,
                    });
                    LogicalEntryState::InUse {
                        physical_index: u32::MAX,
                    }
                }
            };
            logical_entries.push(LogicalEntry {
                object_number: entry.object_number(),
                generation: entry.generation(),
                state,
            });
        }
        check_cancelled(cancellation)?;
        validate_root(&logical_entries, section.root())?;

        let mut meter = SortMeter::new(limits.max_sort_steps);
        cancellable_heapsort(&mut physical_intervals, &mut meter, cancellation)?;

        for index in 0..physical_intervals.len() {
            probe_loop(cancellation, index)?;
            if index > 0
                && physical_intervals[index - 1].xref_offset
                    == physical_intervals[index].xref_offset
            {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::DuplicatePhysicalOffset,
                    Some(physical_intervals[index].reference),
                    Some(physical_intervals[index].xref_offset),
                ));
            }
            let upper_bound = physical_intervals
                .get(index + 1)
                .map_or(section.startxref(), |next| next.xref_offset);
            physical_intervals[index].object_upper_bound = upper_bound;
            let physical_index = u32::try_from(index).map_err(|_| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(physical_intervals[index].reference),
                    Some(physical_intervals[index].xref_offset),
                )
            })?;
            let logical_slot = physical_intervals[index].logical_slot as usize;
            let logical = logical_entries.get_mut(logical_slot).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(physical_intervals[index].reference),
                    Some(physical_intervals[index].xref_offset),
                )
            })?;
            logical.state = LogicalEntryState::InUse { physical_index };
        }
        check_cancelled(cancellation)?;

        Ok(Self {
            snapshot: section.snapshot(),
            revision_id,
            startxref: section.startxref(),
            root: section.root(),
            logical_entries,
            physical_intervals,
            stats: DocumentIndexStats {
                total_entries,
                in_use_entries,
                logical_index_bytes,
                sort_steps: meter.steps,
            },
        })
    }
}

const fn accounted_index_bytes(total_entries: u64, in_use_entries: u64) -> Option<(u64, u64, u64)> {
    let logical = total_entries.checked_mul(ACCOUNTED_LOGICAL_ENTRY_BYTES);
    let physical = in_use_entries.checked_mul(ACCOUNTED_PHYSICAL_INTERVAL_BYTES);
    match (logical, physical) {
        (Some(logical), Some(physical)) => match logical.checked_add(physical) {
            Some(total) => Some((logical, physical, total)),
            None => None,
        },
        _ => None,
    }
}

fn validate_root(entries: &[LogicalEntry], root: ObjectRef) -> Result<(), DocumentError> {
    let valid = entries
        .binary_search_by_key(&root.number(), |entry| entry.object_number)
        .ok()
        .map(|index| entries[index])
        .is_some_and(|entry| {
            entry.generation == root.generation()
                && matches!(entry.state, LogicalEntryState::InUse { .. })
        });
    if !valid {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidTrailerRoot,
            Some(root),
            None,
        ));
    }
    Ok(())
}

fn check_cancelled(cancellation: &(dyn DocumentCancellation + '_)) -> Result<(), DocumentError> {
    if cancellation.is_cancelled() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::Cancelled,
            None,
            None,
        ));
    }
    Ok(())
}

fn probe_loop(
    cancellation: &(dyn DocumentCancellation + '_),
    index: usize,
) -> Result<(), DocumentError> {
    if index.is_multiple_of(CANCELLATION_INTERVAL as usize) {
        check_cancelled(cancellation)?;
    }
    Ok(())
}

struct SortMeter {
    limit: u64,
    steps: u64,
    steps_since_probe: u64,
}

impl SortMeter {
    const fn new(limit: u64) -> Self {
        Self {
            limit,
            steps: 0,
            steps_since_probe: 0,
        }
    }

    fn step(
        &mut self,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<(), DocumentError> {
        if self.steps >= self.limit {
            return Err(DocumentError::resource(
                DocumentLimitKind::SortSteps,
                self.limit,
                self.steps,
                1,
                None,
            ));
        }
        self.steps += 1;
        self.steps_since_probe += 1;
        if self.steps_since_probe == CANCELLATION_INTERVAL {
            check_cancelled(cancellation)?;
            self.steps_since_probe = 0;
        }
        Ok(())
    }

    fn finish(
        &mut self,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<(), DocumentError> {
        check_cancelled(cancellation)?;
        self.steps_since_probe = 0;
        Ok(())
    }
}

fn cancellable_heapsort(
    values: &mut [PhysicalObjectInterval],
    meter: &mut SortMeter,
    cancellation: &(dyn DocumentCancellation + '_),
) -> Result<(), DocumentError> {
    if values.len() < 2 {
        return meter.finish(cancellation);
    }

    for root in (0..(values.len() / 2)).rev() {
        sift_down(values, root, values.len(), meter, cancellation)?;
    }
    for end in (1..values.len()).rev() {
        meter.step(cancellation)?;
        values.swap(0, end);
        sift_down(values, 0, end, meter, cancellation)?;
    }
    meter.finish(cancellation)
}

fn sift_down(
    values: &mut [PhysicalObjectInterval],
    mut root: usize,
    end: usize,
    meter: &mut SortMeter,
    cancellation: &(dyn DocumentCancellation + '_),
) -> Result<(), DocumentError> {
    loop {
        let Some(left) = root.checked_mul(2).and_then(|value| value.checked_add(1)) else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                None,
                None,
            ));
        };
        if left >= end {
            return Ok(());
        }
        let mut greatest = root;
        meter.step(cancellation)?;
        if values[greatest].xref_offset < values[left].xref_offset {
            greatest = left;
        }
        let right = left + 1;
        if right < end {
            meter.step(cancellation)?;
            if values[greatest].xref_offset < values[right].xref_offset {
                greatest = right;
            }
        }
        if greatest == root {
            return Ok(());
        }
        meter.step(cancellation)?;
        values.swap(root, greatest);
        root = greatest;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    struct CancelOnProbe {
        probes: AtomicU64,
        cancel_at: u64,
    }

    impl DocumentCancellation for CancelOnProbe {
        fn is_cancelled(&self) -> bool {
            self.probes.fetch_add(1, Ordering::Relaxed) + 1 >= self.cancel_at
        }
    }

    fn interval(number: u32, offset: u64) -> PhysicalObjectInterval {
        PhysicalObjectInterval {
            revision_id: RevisionId::new(7),
            reference: ObjectRef::new(number, 0).unwrap(),
            xref_offset: offset,
            object_upper_bound: 999,
            logical_slot: number,
        }
    }

    #[test]
    fn heapsort_orders_physical_offsets_and_charges_work() {
        let mut values = [interval(1, 90), interval(2, 10), interval(3, 50)];
        let mut meter = SortMeter::new(100);
        cancellable_heapsort(&mut values, &mut meter, &NeverCancelled).unwrap();
        assert_eq!(values.map(|value| value.xref_offset), [10_u64, 50, 90]);
        assert!(meter.steps > 0);
    }

    #[test]
    fn heapsort_enforces_exact_step_budget() {
        let mut values = [interval(1, 90), interval(2, 10), interval(3, 50)];
        let mut meter = SortMeter::new(1);
        let error = cancellable_heapsort(&mut values, &mut meter, &NeverCancelled).unwrap_err();
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), DocumentLimitKind::SortSteps);
        assert_eq!(error.limit().unwrap().consumed(), 1);
    }

    #[test]
    fn heapsort_checks_cancellation_after_at_most_256_steps() {
        let mut values = (1..=300)
            .rev()
            .map(|number| interval(number, u64::from(number)))
            .collect::<Vec<_>>();
        let cancellation = CancelOnProbe {
            probes: AtomicU64::new(0),
            cancel_at: 1,
        };
        let mut meter = SortMeter::new(100_000);
        let error = cancellable_heapsort(&mut values, &mut meter, &cancellation).unwrap_err();
        assert_eq!(error.code(), DocumentErrorCode::Cancelled);
        assert!(meter.steps <= CANCELLATION_INTERVAL);
    }

    #[test]
    fn accounting_is_checked_and_conservative() {
        assert_eq!(accounted_index_bytes(2, 1), Some((64, 64, 128)));
        assert_eq!(accounted_index_bytes(u64::MAX, 1), None);
        assert!(
            usize::try_from(ACCOUNTED_LOGICAL_ENTRY_BYTES).unwrap()
                >= std::mem::size_of::<LogicalEntry>()
        );
        assert!(
            usize::try_from(ACCOUNTED_PHYSICAL_INTERVAL_BYTES).unwrap()
                >= std::mem::size_of::<PhysicalObjectInterval>()
        );
    }
}
