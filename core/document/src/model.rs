use std::fmt;

use pdf_rs_bytes::SourceSnapshot;
use pdf_rs_object::IndirectObjectTarget;
use pdf_rs_syntax::ObjectRef;

use crate::{DocumentError, DocumentErrorCode};

/// Stable caller-assigned identity of one candidate PDF revision.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RevisionId(u32);

impl RevisionId {
    /// Creates a candidate revision identity.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the caller-assigned numeric identity.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Candidate physical byte interval derived solely from unauthenticated xref metadata.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PhysicalObjectInterval {
    pub(crate) revision_id: RevisionId,
    pub(crate) reference: ObjectRef,
    pub(crate) xref_offset: u64,
    pub(crate) object_upper_bound: u64,
    pub(crate) logical_slot: u32,
}

impl PhysicalObjectInterval {
    /// Returns the candidate revision that supplied this interval.
    pub const fn revision_id(self) -> RevisionId {
        self.revision_id
    }

    /// Returns the exact object identity claimed by the candidate xref row.
    pub const fn reference(self) -> ObjectRef {
        self.reference
    }

    /// Returns the candidate xref offset at the interval start.
    pub const fn xref_offset(self) -> u64 {
        self.xref_offset
    }

    /// Returns the exclusive bound at the next in-use offset or revision `startxref`.
    pub const fn object_upper_bound(self) -> u64 {
        self.object_upper_bound
    }

    /// Returns the candidate physical interval length.
    pub const fn len(self) -> u64 {
        self.object_upper_bound - self.xref_offset
    }

    /// Reports whether this candidate physical interval contains no bytes.
    pub const fn is_empty(self) -> bool {
        self.xref_offset == self.object_upper_bound
    }
}

impl fmt::Debug for PhysicalObjectInterval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PhysicalObjectInterval")
            .field("revision_id", &self.revision_id)
            .field("reference", &self.reference)
            .field("xref_offset", &self.xref_offset)
            .field("object_upper_bound", &self.object_upper_bound)
            .finish()
    }
}

/// Deterministic work and allocator-reported entry-capacity accounting for one candidate index.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DocumentIndexStats {
    pub(crate) total_entries: u64,
    pub(crate) in_use_entries: u64,
    pub(crate) logical_index_bytes: u64,
    pub(crate) sort_steps: u64,
}

impl DocumentIndexStats {
    /// Returns all xref rows retained in the logical index.
    pub const fn total_entries(self) -> u64 {
        self.total_entries
    }

    /// Returns in-use rows retained in the physical interval index.
    pub const fn in_use_entries(self) -> u64 {
        self.in_use_entries
    }

    /// Returns conservatively accounted allocator capacity for retained index entries.
    pub const fn logical_index_bytes(self) -> u64 {
        self.logical_index_bytes
    }

    /// Returns physical-offset sort comparisons and swaps.
    pub const fn sort_steps(self) -> u64 {
        self.sort_steps
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LogicalEntryState {
    Free,
    InUse { physical_index: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LogicalEntry {
    pub(crate) object_number: u32,
    pub(crate) generation: u16,
    pub(crate) state: LogicalEntryState,
}

/// Xref-derived candidate revision index that has not attested object-header context.
///
/// Physical intervals close the cross-object upper-bound gap for framing. They do not prove that
/// an offset is at top level rather than embedded in a comment, string, or stream payload.
pub struct CandidateRevisionIndex {
    pub(crate) snapshot: SourceSnapshot,
    pub(crate) revision_id: RevisionId,
    pub(crate) startxref: u64,
    pub(crate) root: ObjectRef,
    pub(crate) logical_entries: Vec<LogicalEntry>,
    pub(crate) physical_intervals: Vec<PhysicalObjectInterval>,
    pub(crate) stats: DocumentIndexStats,
}

impl CandidateRevisionIndex {
    /// Returns the immutable source snapshot that supplied the candidate xref section.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the caller-assigned candidate revision identity.
    pub const fn revision_id(&self) -> RevisionId {
        self.revision_id
    }

    /// Returns the candidate revision's xref-table offset.
    pub const fn startxref(&self) -> u64 {
        self.startxref
    }

    /// Returns the exact-generation in-use trailer root claimed by the candidate revision.
    pub const fn root(&self) -> ObjectRef {
        self.root
    }

    /// Returns deterministic construction and allocator-capacity accounting.
    pub const fn stats(&self) -> DocumentIndexStats {
        self.stats
    }

    /// Returns all candidate physical intervals in strictly increasing offset order.
    pub fn physical_intervals(&self) -> &[PhysicalObjectInterval] {
        &self.physical_intervals
    }

    /// Looks up an exact object identity while distinguishing missing, free, and generation states.
    pub fn interval(&self, reference: ObjectRef) -> Result<&PhysicalObjectInterval, DocumentError> {
        let logical = self
            .logical_entries
            .binary_search_by_key(&reference.number(), |entry| entry.object_number)
            .ok()
            .map(|index| &self.logical_entries[index])
            .ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::MissingObject, Some(reference), None)
            })?;
        if logical.generation != reference.generation() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::GenerationMismatch,
                Some(reference),
                None,
            ));
        }
        let physical_index = match logical.state {
            LogicalEntryState::Free => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::FreeObject,
                    Some(reference),
                    None,
                ));
            }
            LogicalEntryState::InUse { physical_index } => physical_index,
        };
        self.physical_intervals
            .get(physical_index as usize)
            .ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), None)
            })
    }

    /// Forms an explicitly unattested five-field object target from one candidate interval.
    ///
    /// The target is suitable only for the next object-header attestation phase. It must not be
    /// interpreted as proof that the xref offset is a top-level indirect object.
    pub fn unattested_target(
        &self,
        reference: ObjectRef,
    ) -> Result<IndirectObjectTarget, DocumentError> {
        let interval = self.interval(reference)?;
        IndirectObjectTarget::new(
            self.snapshot,
            reference,
            interval.xref_offset,
            interval.object_upper_bound,
            self.startxref,
        )
        .map_err(|error| DocumentError::from_object(error, reference, interval.xref_offset))
    }
}

impl fmt::Debug for CandidateRevisionIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CandidateRevisionIndex")
            .field("snapshot", &self.snapshot)
            .field("revision_id", &self.revision_id)
            .field("startxref", &self.startxref)
            .field("root", &self.root)
            .field("stats", &self.stats)
            .field("logical_entries", &"[REDACTED]")
            .field("physical_intervals", &"[REDACTED]")
            .finish()
    }
}
