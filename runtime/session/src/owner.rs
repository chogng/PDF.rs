use std::fmt;
use std::mem;

use pdf_rs_cache::{
    ReadyAdmission, ReadyLookup, ReadyStore, ReadyStoreBinding, ReadyStoreKey, ReadyStoreLimits,
    ReadyStoreSessionId, ReadyStoreStats,
};
use pdf_rs_document::{DocumentCancellation, ResolvedReference};

use crate::{ReadySessionAdmissionError, ReadySessionError};

/// Public phase of the Ready-store owner slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadySessionPhase {
    /// The store exists and may serve or retain successful Ready values.
    Ready,
    /// The store and all session-only cache allocations have been dropped.
    Closed,
}

/// Current Ready-store resources owned exclusively by one session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadySessionResources {
    entries: u64,
    metadata_bytes: u64,
    value_heap_bytes: u64,
    resident_bytes: u64,
}

impl ReadySessionResources {
    const ZERO: Self = Self {
        entries: 0,
        metadata_bytes: 0,
        value_heap_bytes: 0,
        resident_bytes: 0,
    };

    fn from_stats(stats: ReadyStoreStats) -> Self {
        Self {
            entries: stats.entries(),
            metadata_bytes: stats.metadata_bytes(),
            value_heap_bytes: stats.value_heap_bytes(),
            resident_bytes: stats.resident_bytes(),
        }
    }

    /// Returns the number of currently retained successful values.
    pub const fn entries(self) -> u64 {
        self.entries
    }

    /// Returns current allocator-reported fixed store metadata bytes.
    pub const fn metadata_bytes(self) -> u64 {
        self.metadata_bytes
    }

    /// Returns current syntax and reference-path heap bytes retained by values.
    pub const fn value_heap_bytes(self) -> u64 {
        self.value_heap_bytes
    }

    /// Returns current metadata plus retained value heap bytes.
    pub const fn resident_bytes(self) -> u64 {
        self.resident_bytes
    }
}

/// Stable evidence returned after synchronous Ready-store close.
///
/// Released byte counts are allocator-capacity ownership accounting from the
/// dropped store. They are not a process RSS measurement or a guarantee about
/// when a platform allocator returns pages to the operating system.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadySessionCloseReport {
    session_id: ReadyStoreSessionId,
    released_entries: u64,
    released_metadata_bytes: u64,
    released_value_heap_bytes: u64,
    released_resident_bytes: u64,
    peak_resident_bytes: u64,
}

impl ReadySessionCloseReport {
    fn from_stats(session_id: ReadyStoreSessionId, stats: ReadyStoreStats) -> Self {
        debug_assert_eq!(
            stats.metadata_bytes().checked_add(stats.value_heap_bytes()),
            Some(stats.resident_bytes())
        );
        Self {
            session_id,
            released_entries: stats.entries(),
            released_metadata_bytes: stats.metadata_bytes(),
            released_value_heap_bytes: stats.value_heap_bytes(),
            released_resident_bytes: stats.resident_bytes(),
            peak_resident_bytes: stats.peak_resident_bytes(),
        }
    }

    /// Returns the opaque identity whose Ready store completed close.
    pub const fn session_id(self) -> ReadyStoreSessionId {
        self.session_id
    }

    /// Returns the number of successful values dropped during close.
    pub const fn released_entries(self) -> u64 {
        self.released_entries
    }

    /// Returns fixed metadata capacity bytes dropped during close.
    pub const fn released_metadata_bytes(self) -> u64 {
        self.released_metadata_bytes
    }

    /// Returns retained value heap bytes dropped during close.
    pub const fn released_value_heap_bytes(self) -> u64 {
        self.released_value_heap_bytes
    }

    /// Returns metadata plus retained value heap bytes dropped during close.
    pub const fn released_resident_bytes(self) -> u64 {
        self.released_resident_bytes
    }

    /// Returns the greatest store resident total observed before close.
    pub const fn peak_resident_bytes(self) -> u64 {
        self.peak_resident_bytes
    }
}

enum OwnerState<S> {
    Ready(S),
    Closed(ReadySessionCloseReport),
}

impl<S> OwnerState<S> {
    fn phase(&self) -> ReadySessionPhase {
        match self {
            Self::Ready(_) => ReadySessionPhase::Ready,
            Self::Closed(_) => ReadySessionPhase::Closed,
        }
    }

    fn close_and_drop(&mut self, proposed: ReadySessionCloseReport) -> ReadySessionCloseReport {
        if let Self::Closed(existing) = self {
            return *existing;
        }
        let previous = mem::replace(self, Self::Closed(proposed));
        let Self::Ready(value) = previous else {
            unreachable!("the closed state returned before replacement")
        };
        drop(value);
        proposed
    }
}

/// Exclusive Ready-store owner for one already-open document session.
///
/// The owner cannot be cloned and never exposes its store. Warm hits borrow from
/// `&mut self`, so Rust prevents close while such a value is still borrowed.
/// Callers must keep a borrowed hit within one synchronous actor turn and must
/// explicitly call [`Self::close`] before publishing a protocol `SessionClosed`
/// event. Dropping this owner without explicit close still releases the store as
/// a resource-safety fallback, but publishes no lifecycle event.
pub struct ReadySessionOwner {
    session_id: ReadyStoreSessionId,
    state: OwnerState<ReadyStore>,
}

impl ReadySessionOwner {
    /// Creates the unique owner and precharges fixed Ready-store metadata.
    pub fn new(
        binding: ReadyStoreBinding,
        limits: ReadyStoreLimits,
    ) -> Result<Self, ReadySessionError> {
        let session_id = binding.session_id();
        let store = ReadyStore::new(binding, limits)
            .map_err(|error| ReadySessionError::from_ready_store(session_id, error))?;
        Ok(Self {
            session_id,
            state: OwnerState::Ready(store),
        })
    }

    /// Returns the opaque identity retained across Ready and Closed phases.
    pub const fn session_id(&self) -> ReadyStoreSessionId {
        self.session_id
    }

    /// Returns the current public phase.
    pub fn phase(&self) -> ReadySessionPhase {
        self.state.phase()
    }

    /// Returns the complete active store binding or `SessionClosed`.
    pub fn binding(&self) -> Result<ReadyStoreBinding, ReadySessionError> {
        match &self.state {
            OwnerState::Ready(store) => Ok(store.binding()),
            OwnerState::Closed(_) => Err(ReadySessionError::session_closed(self.session_id)),
        }
    }

    /// Returns active store accounting or `SessionClosed`.
    pub fn stats(&self) -> Result<ReadyStoreStats, ReadySessionError> {
        match &self.state {
            OwnerState::Ready(store) => Ok(store.stats()),
            OwnerState::Closed(_) => Err(ReadySessionError::session_closed(self.session_id)),
        }
    }

    /// Returns current owned resources, which are all zero after close.
    pub fn resources(&self) -> ReadySessionResources {
        match &self.state {
            OwnerState::Ready(store) => ReadySessionResources::from_stats(store.stats()),
            OwnerState::Closed(_) => ReadySessionResources::ZERO,
        }
    }

    /// Looks up one complete key, rejecting Closed before cancellation or key checks.
    pub fn lookup(
        &mut self,
        key: ReadyStoreKey,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<ReadyLookup<'_>, ReadySessionError> {
        match &mut self.state {
            OwnerState::Ready(store) => store
                .lookup(key, cancellation)
                .map_err(|error| ReadySessionError::from_ready_store(self.session_id, error)),
            OwnerState::Closed(_) => Err(ReadySessionError::session_closed(self.session_id)),
        }
    }

    /// Attempts to retain a value, returning it on lifecycle or cache failure.
    ///
    /// Normal cache-policy rejection remains a successful [`ReadyAdmission`]
    /// outcome and also returns the value through its rejection variant.
    pub fn try_admit(
        &mut self,
        key: ReadyStoreKey,
        value: ResolvedReference,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<ReadyAdmission, ReadySessionAdmissionError> {
        match &mut self.state {
            OwnerState::Ready(store) => {
                store.try_admit(key, value, cancellation).map_err(|lower| {
                    let error = lower.error();
                    let value = lower.into_value();
                    ReadySessionAdmissionError::new(
                        ReadySessionError::from_ready_store(self.session_id, error),
                        value,
                    )
                })
            }
            OwnerState::Closed(_) => Err(ReadySessionAdmissionError::new(
                ReadySessionError::session_closed(self.session_id),
                value,
            )),
        }
    }

    /// Drops the complete Ready store and returns an idempotent close report.
    ///
    /// The first call captures final accounting, removes the only store from the
    /// owner, explicitly drops it, and only then returns. Later calls return the
    /// exact saved report without changing state or counters.
    pub fn close(&mut self) -> ReadySessionCloseReport {
        let report = match &self.state {
            OwnerState::Ready(store) => {
                ReadySessionCloseReport::from_stats(self.session_id, store.stats())
            }
            OwnerState::Closed(report) => *report,
        };
        self.state.close_and_drop(report)
    }

    /// Returns the stable report only after close completed.
    pub fn close_report(&self) -> Option<ReadySessionCloseReport> {
        match &self.state {
            OwnerState::Ready(_) => None,
            OwnerState::Closed(report) => Some(*report),
        }
    }
}

impl fmt::Debug for ReadySessionOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadySessionOwner")
            .field("session_id", &self.session_id)
            .field("phase", &self.phase())
            .field("resources", &self.resources())
            .field("close_report", &self.close_report())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    struct DropSpy(Rc<Cell<u64>>);

    impl Drop for DropSpy {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    fn report() -> ReadySessionCloseReport {
        ReadySessionCloseReport {
            session_id: ReadyStoreSessionId::new(17),
            released_entries: 1,
            released_metadata_bytes: 2,
            released_value_heap_bytes: 3,
            released_resident_bytes: 5,
            peak_resident_bytes: 8,
        }
    }

    #[test]
    fn state_close_drops_the_unique_value_once_before_returning() {
        let drops = Rc::new(Cell::new(0));
        let mut state = OwnerState::Ready(DropSpy(Rc::clone(&drops)));

        let first = state.close_and_drop(report());
        assert_eq!(drops.get(), 1);
        assert_eq!(state.phase(), ReadySessionPhase::Closed);

        let second = state.close_and_drop(ReadySessionCloseReport {
            released_entries: 99,
            ..report()
        });
        assert_eq!(drops.get(), 1);
        assert_eq!(second, first);
    }
}
