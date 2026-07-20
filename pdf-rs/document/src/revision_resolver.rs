use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteSource, DataTicket, JobId, RequestPriority, ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_object::{
    DeclaredStreamLength, IndirectObject, IndirectObjectTarget, ObjectCancellation,
    ObjectEnvelopePoll, ObjectError, ObjectJobContext, ObjectLimitKind, ObjectLimits, ObjectPoll,
    ObjectStats, ObjectStream, ObjectStreamEntry, ObjectWorkCaps, OpenObjectEnvelopeJob,
    OpenStreamBoundaryJob, ResolvedStreamLength, StreamEnvelope,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{ResolvedXrefEntry, RevisionChain, RevisionEntryKind};

use crate::{
    DocumentCancellation, DocumentError, DocumentErrorCode, DocumentLimitKind, DocumentLimits,
};

const CANCELLATION_INTERVAL: usize = 256;
const ACCOUNTED_ANCHOR_BYTES: u64 = 8;

/// Effective uncompressed definition and its derived physical framing interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UncompressedObjectLocator {
    provenance: ResolvedXrefEntry,
    generation: u16,
    offset: u64,
    object_upper_bound: u64,
}

impl UncompressedObjectLocator {
    /// Returns the revision and primary-or-hybrid layer that supplied this definition.
    pub const fn provenance(self) -> ResolvedXrefEntry {
        self.provenance
    }

    /// Returns the effective generation number.
    pub const fn generation(self) -> u16 {
        self.generation
    }

    /// Returns the exact physical offset claimed by the winning xref entry.
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns the nearest greater geometry-validated xref-claimed object or section anchor.
    pub const fn object_upper_bound(self) -> u64 {
        self.object_upper_bound
    }
}

/// Effective compressed definition using generation-zero decoded object-stream coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompressedObjectLocator {
    provenance: ResolvedXrefEntry,
    object_stream: u32,
    index: u32,
}

impl CompressedObjectLocator {
    /// Returns the revision and primary-or-hybrid layer that supplied this definition.
    pub const fn provenance(self) -> ResolvedXrefEntry {
        self.provenance
    }

    /// Returns the object number of the containing object stream.
    pub const fn object_stream(self) -> u32 {
        self.object_stream
    }

    /// Returns the zero-based entry index inside the decoded object stream.
    pub const fn index(self) -> u32 {
        self.index
    }
}

/// Latest-wins definition for one object number in a composed revision chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveObjectLocator {
    /// An unknown xref-stream row type interpreted as null and hiding older definitions.
    Null {
        /// Winning entry provenance.
        provenance: ResolvedXrefEntry,
        /// Encoded future row type.
        encoded_type: u64,
    },
    /// A free row that hides every older definition.
    Free {
        /// Winning entry provenance.
        provenance: ResolvedXrefEntry,
        /// Next object number in the free chain.
        next_free: u32,
        /// Free-entry generation.
        generation: u16,
    },
    /// An uncompressed object with a derived physical interval.
    Uncompressed(UncompressedObjectLocator),
    /// A generation-zero object inside an object stream.
    Compressed(CompressedObjectLocator),
}

impl EffectiveObjectLocator {
    /// Returns the winning revision and layer for this definition.
    pub const fn provenance(self) -> ResolvedXrefEntry {
        match self {
            Self::Null { provenance, .. } | Self::Free { provenance, .. } => provenance,
            Self::Uncompressed(locator) => locator.provenance,
            Self::Compressed(locator) => locator.provenance,
        }
    }
}

/// Bounded construction and retained-capacity evidence for a revision object index.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RevisionObjectIndexStats {
    entries: u64,
    uncompressed_entries: u64,
    unique_anchors: u64,
    retained_anchor_bytes: u64,
    sort_steps: u64,
}

impl RevisionObjectIndexStats {
    /// Returns all primary and hybrid entries represented by the chain.
    pub const fn entries(self) -> u64 {
        self.entries
    }

    /// Returns all uncompressed definitions inspected across revisions.
    pub const fn uncompressed_entries(self) -> u64 {
        self.uncompressed_entries
    }

    /// Returns unique physical object and xref anchors retained by the index.
    pub const fn unique_anchors(self) -> u64 {
        self.unique_anchors
    }

    /// Returns allocator-reported retained anchor capacity in bytes.
    pub const fn retained_anchor_bytes(self) -> u64 {
        self.retained_anchor_bytes
    }

    /// Returns comparisons and swaps charged by cancellable sorting.
    pub const fn sort_steps(self) -> u64 {
        self.sort_steps
    }
}

/// Source-bound latest-wins object lookup plus derived physical anchors.
///
/// The index retains a validated [`RevisionChain`] and adds only bounded physical
/// anchor geometry. It does not claim that xref offsets are top-level-attested,
/// decode object streams, acquire revision sections, or implement repair.
pub struct RevisionObjectIndex {
    chain: RevisionChain,
    anchors: Vec<u64>,
    stats: RevisionObjectIndexStats,
}

impl RevisionObjectIndex {
    /// Builds a bounded physical-anchor index over one already-composed chain.
    pub fn new(
        chain: RevisionChain,
        limits: DocumentLimits,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<Self, DocumentError> {
        check_cancelled(cancellation)?;
        let entries = chain.stats().entries();
        if entries > limits.max_total_entries() {
            return Err(DocumentError::resource(
                DocumentLimitKind::TotalEntries,
                limits.max_total_entries(),
                0,
                entries,
                None,
            ));
        }

        let mut uncompressed_entries = 0_u64;
        for (revision_index, revision) in chain.revisions().iter().enumerate() {
            probe(cancellation, revision_index)?;
            uncompressed_entries = count_uncompressed(
                uncompressed_entries,
                revision.primary_entries(),
                limits,
                cancellation,
            )?;
            if let Some(supplement) = revision.hybrid_supplement() {
                uncompressed_entries = count_uncompressed(
                    uncompressed_entries,
                    supplement.entries(),
                    limits,
                    cancellation,
                )?;
            }
        }
        check_cancelled(cancellation)?;

        let section_count = u64::from(chain.stats().sections());
        let max_anchors = limits.max_logical_index_bytes() / ACCOUNTED_ANCHOR_BYTES;
        let requested_anchors =
            uncompressed_entries
                .checked_add(section_count)
                .ok_or_else(|| {
                    DocumentError::resource(
                        DocumentLimitKind::RevisionResolverAnchors,
                        max_anchors,
                        0,
                        u64::MAX,
                        None,
                    )
                })?;
        if requested_anchors > max_anchors {
            return Err(DocumentError::resource(
                DocumentLimitKind::RevisionResolverAnchors,
                max_anchors,
                0,
                requested_anchors,
                None,
            ));
        }
        let requested_bytes = requested_anchors
            .checked_mul(ACCOUNTED_ANCHOR_BYTES)
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::RevisionResolverIndexBytes,
                    limits.max_logical_index_bytes(),
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        if requested_bytes > limits.max_logical_index_bytes() {
            return Err(DocumentError::resource(
                DocumentLimitKind::RevisionResolverIndexBytes,
                limits.max_logical_index_bytes(),
                0,
                requested_bytes,
                None,
            ));
        }
        let capacity = usize::try_from(requested_anchors).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::RevisionResolverAnchors,
                max_anchors,
                0,
                requested_anchors,
                None,
            )
        })?;
        let mut anchors = Vec::new();
        anchors.try_reserve_exact(capacity).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::Allocation,
                limits.max_logical_index_bytes(),
                0,
                requested_bytes,
                None,
            )
        })?;

        for (revision_index, revision) in chain.revisions().iter().enumerate() {
            probe(cancellation, revision_index)?;
            anchors.push(revision.startxref());
            append_offsets(&mut anchors, revision.primary_entries(), cancellation)?;
            if let Some(supplement) = revision.hybrid_supplement() {
                anchors.push(supplement.startxref());
                append_offsets(&mut anchors, supplement.entries(), cancellation)?;
            }
        }
        check_cancelled(cancellation)?;

        let retained_anchor_bytes = u64::try_from(anchors.capacity())
            .ok()
            .and_then(|capacity| capacity.checked_mul(ACCOUNTED_ANCHOR_BYTES))
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::RevisionResolverIndexBytes,
                    limits.max_logical_index_bytes(),
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        if retained_anchor_bytes > limits.max_logical_index_bytes() {
            return Err(DocumentError::resource(
                DocumentLimitKind::RevisionResolverIndexBytes,
                limits.max_logical_index_bytes(),
                0,
                retained_anchor_bytes,
                None,
            ));
        }

        let mut meter = SortMeter::new(limits.max_sort_steps());
        cancellable_heapsort(&mut anchors, &mut meter, cancellation)?;
        cancellable_dedup(&mut anchors, cancellation)?;
        let unique_anchors = u64::try_from(anchors.len())
            .map_err(|_| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))?;
        Ok(Self {
            chain,
            anchors,
            stats: RevisionObjectIndexStats {
                entries,
                uncompressed_entries,
                unique_anchors,
                retained_anchor_bytes,
                sort_steps: meter.steps,
            },
        })
    }

    /// Returns the immutable source snapshot shared by the chain and all locators.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.chain.snapshot()
    }

    /// Returns the newest effective trailer root.
    pub const fn root(&self) -> ObjectRef {
        self.chain.root()
    }

    /// Borrows the validated newest-to-oldest revision chain.
    pub const fn chain(&self) -> &RevisionChain {
        &self.chain
    }

    /// Returns construction work and retained-capacity evidence.
    pub const fn stats(&self) -> RevisionObjectIndexStats {
        self.stats
    }

    /// Looks up the winning definition without falling back through free, null, or mismatch states.
    pub fn locator(&self, object_number: u32) -> Option<EffectiveObjectLocator> {
        let provenance = self.chain.entry(object_number)?;
        let entry = provenance.entry();
        Some(match entry.kind() {
            RevisionEntryKind::Null { encoded_type } => EffectiveObjectLocator::Null {
                provenance,
                encoded_type,
            },
            RevisionEntryKind::Free {
                next_free,
                generation,
            } => EffectiveObjectLocator::Free {
                provenance,
                next_free,
                generation,
            },
            RevisionEntryKind::Uncompressed { offset, generation } => {
                let successor = self.anchors.partition_point(|anchor| *anchor <= offset);
                let object_upper_bound = self
                    .anchors
                    .get(successor)
                    .copied()
                    .or_else(|| self.snapshot().len())
                    .unwrap_or(offset);
                EffectiveObjectLocator::Uncompressed(UncompressedObjectLocator {
                    provenance,
                    generation,
                    offset,
                    object_upper_bound,
                })
            }
            RevisionEntryKind::Compressed {
                object_stream,
                index,
            } => EffectiveObjectLocator::Compressed(CompressedObjectLocator {
                provenance,
                object_stream,
                index,
            }),
        })
    }

    /// Binds one effective compressed row to its validated latest-wins object-stream container.
    ///
    /// The supplied stream must already have crossed the `pdf-rs/object` framing and unfiltered
    /// payload boundary. This method rechecks the effective container definition, exact physical
    /// framing interval, decoded index, and embedded object number before exposing a borrowed value.
    pub fn resolve_compressed<'stream>(
        &self,
        reference: ObjectRef,
        stream: &'stream ObjectStream,
    ) -> Result<ResolvedCompressedObject<'stream>, DocumentError> {
        if reference.generation() != 0 {
            return Err(DocumentError::for_code(
                DocumentErrorCode::GenerationMismatch,
                Some(reference),
                None,
            ));
        }
        let locator = match self.locator(reference.number()) {
            Some(EffectiveObjectLocator::Compressed(locator)) => locator,
            Some(EffectiveObjectLocator::Null { .. }) => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::NullObject,
                    Some(reference),
                    None,
                ));
            }
            Some(EffectiveObjectLocator::Free { .. }) => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::FreeObject,
                    Some(reference),
                    None,
                ));
            }
            Some(EffectiveObjectLocator::Uncompressed(locator)) => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::NotCompressedObject,
                    Some(reference),
                    Some(locator.offset),
                ));
            }
            None => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::MissingObject,
                    Some(reference),
                    None,
                ));
            }
        };
        if stream.snapshot() != self.snapshot() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(reference),
                Some(locator.provenance.revision_startxref()),
            ));
        }
        if stream.container().number() != locator.object_stream
            || stream.container().generation() != 0
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidObjectStreamContainer,
                Some(reference),
                Some(locator.provenance.revision_startxref()),
            ));
        }
        let container_locator = match self.locator(locator.object_stream) {
            Some(EffectiveObjectLocator::Uncompressed(locator)) => locator,
            _ => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidObjectStreamContainer,
                    Some(reference),
                    Some(stream.container_offset()),
                ));
            }
        };
        if container_locator.generation != 0
            || container_locator.offset != stream.container_offset()
            || container_locator.object_upper_bound != stream.container_upper_bound()
            || container_locator.provenance.revision_startxref() != stream.revision_startxref()
            || container_locator.offset >= container_locator.provenance.revision_startxref()
            || stream.encoded_payload_span().start() < container_locator.offset
            || stream.encoded_payload_span().end_exclusive() > container_locator.object_upper_bound
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidObjectStreamContainer,
                Some(reference),
                Some(stream.container_offset()),
            ));
        }
        let entry = stream.entry(locator.index).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::CompressedObjectMismatch,
                Some(reference),
                Some(stream.encoded_payload_span().start()),
            )
        })?;
        if entry.index() != locator.index || entry.object_number() != reference.number() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::CompressedObjectMismatch,
                Some(reference),
                Some(stream.encoded_payload_span().start()),
            ));
        }
        Ok(ResolvedCompressedObject {
            locator,
            container_locator,
            stream,
            entry,
        })
    }

    fn target(
        &self,
        reference: ObjectRef,
    ) -> Result<(UncompressedObjectLocator, IndirectObjectTarget), DocumentError> {
        let locator = self.locator(reference.number()).ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::MissingObject, Some(reference), None)
        })?;
        let uncompressed = match locator {
            EffectiveObjectLocator::Null { .. } => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::NullObject,
                    Some(reference),
                    None,
                ));
            }
            EffectiveObjectLocator::Free { .. } => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::FreeObject,
                    Some(reference),
                    None,
                ));
            }
            EffectiveObjectLocator::Compressed(locator) => {
                if reference.generation() != 0 {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::GenerationMismatch,
                        Some(reference),
                        None,
                    ));
                }
                return Err(DocumentError::for_code(
                    DocumentErrorCode::UnsupportedCompressedObject,
                    Some(reference),
                    Some(locator.provenance().revision_startxref()),
                ));
            }
            EffectiveObjectLocator::Uncompressed(locator) => locator,
        };
        if uncompressed.generation != reference.generation() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::GenerationMismatch,
                Some(reference),
                Some(uncompressed.offset),
            ));
        }
        let revision_startxref = uncompressed.provenance.revision_startxref();
        if uncompressed.offset >= revision_startxref {
            return Err(DocumentError::for_code(
                DocumentErrorCode::UnsupportedXrefStreamContainer,
                Some(reference),
                Some(uncompressed.offset),
            ));
        }
        let target = IndirectObjectTarget::new(
            self.snapshot(),
            reference,
            uncompressed.offset,
            uncompressed.object_upper_bound,
            revision_startxref,
        )
        .map_err(|error| {
            DocumentError::from_revision_resolver_object(
                error,
                reference,
                uncompressed.offset,
                true,
            )
        })?;
        Ok((uncompressed, target))
    }
}

impl fmt::Debug for RevisionObjectIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RevisionObjectIndex")
            .field("snapshot", &self.snapshot())
            .field("root", &self.root())
            .field("stats", &self.stats)
            .field("anchors", &"[REDACTED]")
            .finish()
    }
}

/// Runtime job identity and distinct checkpoints for target and length-dependency framing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionResolverJobContext {
    job: JobId,
    object_envelope_checkpoint: ResumeCheckpoint,
    object_boundary_checkpoint: ResumeCheckpoint,
    length_envelope_checkpoint: ResumeCheckpoint,
    length_boundary_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl RevisionResolverJobContext {
    /// Creates a resolver context; job construction requires all four checkpoints to differ.
    pub const fn new(
        job: JobId,
        object_envelope_checkpoint: ResumeCheckpoint,
        object_boundary_checkpoint: ResumeCheckpoint,
        length_envelope_checkpoint: ResumeCheckpoint,
        length_boundary_checkpoint: ResumeCheckpoint,
        priority: RequestPriority,
    ) -> Self {
        Self {
            job,
            object_envelope_checkpoint,
            object_boundary_checkpoint,
            length_envelope_checkpoint,
            length_boundary_checkpoint,
            priority,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the target-object envelope checkpoint.
    pub const fn object_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.object_envelope_checkpoint
    }

    /// Returns the target-object stream-boundary checkpoint.
    pub const fn object_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.object_boundary_checkpoint
    }

    /// Returns the indirect-length object envelope checkpoint.
    pub const fn length_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.length_envelope_checkpoint
    }

    /// Returns the reserved indirect-length boundary checkpoint.
    pub const fn length_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.length_boundary_checkpoint
    }

    /// Returns the scheduling priority copied to every child read.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }

    const fn object_context(self) -> ObjectJobContext {
        ObjectJobContext::new(
            self.job,
            self.object_envelope_checkpoint,
            self.object_boundary_checkpoint,
            self.priority,
        )
    }

    const fn length_context(self) -> ObjectJobContext {
        ObjectJobContext::new(
            self.job,
            self.length_envelope_checkpoint,
            self.length_boundary_checkpoint,
            self.priority,
        )
    }

    fn is_valid(self) -> bool {
        let checkpoints = [
            self.object_envelope_checkpoint,
            self.object_boundary_checkpoint,
            self.length_envelope_checkpoint,
            self.length_boundary_checkpoint,
        ];
        checkpoints
            .iter()
            .enumerate()
            .all(|(index, checkpoint)| !checkpoints[index + 1..].contains(checkpoint))
    }
}

/// Resolver-wide child profile and checked aggregate work ceiling.
///
/// One resolution can frame at most the requested object and one indirect
/// `/Length` dependency. The aggregate ceilings are therefore exactly twice
/// the contained per-object ceilings; no child receives a larger scope than
/// half of its resolver parent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionResolverLimits {
    object: ObjectLimits,
    max_total_object_read_bytes: u64,
    max_total_object_parse_bytes: u64,
}

impl RevisionResolverLimits {
    /// Derives the exact two-child aggregate profile from validated object limits.
    pub fn from_object_limits(object: ObjectLimits) -> Result<Self, DocumentError> {
        let max_total_object_read_bytes = object
            .max_total_read_bytes()
            .checked_mul(2)
            .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InvalidLimits, None, None))?;
        let max_total_object_parse_bytes = object
            .max_total_parse_bytes()
            .checked_mul(2)
            .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InvalidLimits, None, None))?;
        Ok(Self {
            object,
            max_total_object_read_bytes,
            max_total_object_parse_bytes,
        })
    }

    /// Returns the validated per-child object-framing profile.
    pub const fn object(self) -> ObjectLimits {
        self.object
    }

    /// Returns the resolver-wide cumulative exact-read ceiling.
    pub const fn max_total_object_read_bytes(self) -> u64 {
        self.max_total_object_read_bytes
    }

    /// Returns the resolver-wide cumulative parser-window ceiling.
    pub const fn max_total_object_parse_bytes(self) -> u64 {
        self.max_total_object_parse_bytes
    }
}

impl Default for RevisionResolverLimits {
    fn default() -> Self {
        Self::from_object_limits(ObjectLimits::default())
            .expect("built-in object limits produce a valid two-child resolver profile")
    }
}

/// Parent-supplied cumulative caps across the target and optional indirect-Length child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionResolverWorkCaps {
    max_read_bytes: u64,
    max_parse_bytes: u64,
}

impl RevisionResolverWorkCaps {
    /// Validates positive caps no larger than the resolver profile.
    pub fn new(
        max_read_bytes: u64,
        max_parse_bytes: u64,
        limits: RevisionResolverLimits,
    ) -> Result<Self, DocumentError> {
        if max_read_bytes == 0
            || max_parse_bytes == 0
            || max_read_bytes > limits.max_total_object_read_bytes()
            || max_parse_bytes > limits.max_total_object_parse_bytes()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_read_bytes,
            max_parse_bytes,
        })
    }

    /// Uses the complete aggregate work profile.
    pub const fn from_limits(limits: RevisionResolverLimits) -> Self {
        Self {
            max_read_bytes: limits.max_total_object_read_bytes(),
            max_parse_bytes: limits.max_total_object_parse_bytes(),
        }
    }

    /// Returns the cumulative exact-read cap.
    pub const fn max_read_bytes(self) -> u64 {
        self.max_read_bytes
    }

    /// Returns the cumulative parser-window cap.
    pub const fn max_parse_bytes(self) -> u64 {
        self.max_parse_bytes
    }
}

/// Coarse resumable phase of one revision-aware object-resolution job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionResolverPhase {
    /// Framing the effective target object's envelope.
    ObjectEnvelope,
    /// Framing the effective indirect `/Length` object.
    LengthEnvelope,
    /// Framing the target stream at the resolved exact payload end.
    ObjectBoundary,
    /// The resolved object was returned.
    Complete,
    /// The job reached a terminal structured failure.
    Failed,
}

/// Child object work retained by the resolver for deterministic accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RevisionResolverStats {
    object: ObjectStats,
    length_dependency: ObjectStats,
}

impl RevisionResolverStats {
    /// Returns target-object framing work through the latest poll.
    pub const fn object(self) -> ObjectStats {
        self.object
    }

    /// Returns indirect-length object framing work, or zero when no dependency was opened.
    pub const fn length_dependency(self) -> ObjectStats {
        self.length_dependency
    }

    /// Returns checked aggregate child read bytes.
    pub const fn total_read_bytes(self) -> u64 {
        self.object.read_bytes() + self.length_dependency.read_bytes()
    }

    /// Returns checked aggregate child parser-window bytes.
    pub const fn total_parse_bytes(self) -> u64 {
        self.object.parse_bytes() + self.length_dependency.parse_bytes()
    }
}

/// Effective revision evidence paired with one exactly framed uncompressed object.
///
/// The wrapper exposes the lower object only by shared borrow so the effective
/// locator cannot be discarded while retaining the parsed value.
pub struct ResolvedObject {
    locator: UncompressedObjectLocator,
    object: IndirectObject,
}

/// Effective compressed xref evidence paired with one decoded object-stream entry.
///
/// The wrapper borrows both the complete object-stream proof and its exact entry so neither the
/// latest-wins locator nor the physical container evidence can be discarded independently.
pub struct ResolvedCompressedObject<'stream> {
    locator: CompressedObjectLocator,
    container_locator: UncompressedObjectLocator,
    stream: &'stream ObjectStream,
    entry: &'stream ObjectStreamEntry,
}

impl<'stream> ResolvedCompressedObject<'stream> {
    /// Returns the effective compressed xref definition.
    pub const fn locator(&self) -> CompressedObjectLocator {
        self.locator
    }

    /// Returns the effective uncompressed definition of the object-stream container.
    pub const fn container_locator(&self) -> UncompressedObjectLocator {
        self.container_locator
    }

    /// Borrows the complete validated object stream.
    pub const fn stream(&self) -> &'stream ObjectStream {
        self.stream
    }

    /// Borrows the exact decoded entry selected by the compressed xref row.
    pub const fn entry(&self) -> &'stream ObjectStreamEntry {
        self.entry
    }
}

impl fmt::Debug for ResolvedCompressedObject<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedCompressedObject")
            .field("locator", &self.locator)
            .field("container_locator", &self.container_locator)
            .field("stream", &self.stream)
            .field("entry", &self.entry)
            .finish()
    }
}

impl ResolvedObject {
    /// Returns the effective xref definition and physical interval used for framing.
    pub const fn locator(&self) -> UncompressedObjectLocator {
        self.locator
    }

    /// Borrows the header-validated framed object.
    pub const fn object(&self) -> &IndirectObject {
        &self.object
    }

    pub(crate) fn into_parts(self) -> (UncompressedObjectLocator, IndirectObject) {
        (self.locator, self.object)
    }
}

impl fmt::Debug for ResolvedObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedObject")
            .field("locator", &self.locator)
            .field("object", &self.object)
            .finish()
    }
}

/// Result of polling one revision-aware uncompressed-object resolver.
#[allow(
    clippy::large_enum_variant,
    reason = "the one-shot resolved proof stays inline so retained ownership remains explicit"
)]
#[derive(Debug)]
pub enum RevisionResolverPoll {
    /// One exact effective uncompressed object was framed.
    Ready(ResolvedObject),
    /// Required bytes are absent and the runtime must wait for the ticket.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the requested window.
        missing: SmallRanges,
        /// Exact child checkpoint that must be retained while requeueing.
        checkpoint: ResumeCheckpoint,
    },
    /// The resolver reached a terminal structured failure.
    Failed(DocumentError),
}

enum ResolverState {
    ObjectEnvelope {
        locator: UncompressedObjectLocator,
        job: OpenObjectEnvelopeJob,
    },
    LengthEnvelope {
        locator: UncompressedObjectLocator,
        envelope: StreamEnvelope,
        job: OpenObjectEnvelopeJob,
        work_caps: ObjectWorkCaps,
    },
    ObjectBoundary {
        locator: UncompressedObjectLocator,
        job: OpenStreamBoundaryJob,
        work_caps: ObjectWorkCaps,
    },
    Complete,
    Failed(DocumentError),
}

/// One-shot resolver for effective uncompressed objects and uncompressed indirect stream lengths.
///
/// Compressed targets remain an explicit unsupported terminal state on this source-facing job;
/// callers first resolve and decode the object-stream container, then bind its proof through
/// [`RevisionObjectIndex::resolve_compressed`].
pub struct ResolveObjectJob<'index> {
    index: &'index RevisionObjectIndex,
    reference: ObjectRef,
    context: RevisionResolverJobContext,
    limits: RevisionResolverLimits,
    work_caps: RevisionResolverWorkCaps,
    target_work_caps: ObjectWorkCaps,
    syntax_limits: SyntaxLimits,
    stats: RevisionResolverStats,
    state: ResolverState,
}

impl<'index> ResolveObjectJob<'index> {
    /// Binds one exact reference to its latest effective uncompressed definition.
    pub fn new(
        index: &'index RevisionObjectIndex,
        reference: ObjectRef,
        context: RevisionResolverJobContext,
        limits: RevisionResolverLimits,
        syntax_limits: SyntaxLimits,
    ) -> Result<Self, DocumentError> {
        Self::new_with_work_caps(
            index,
            reference,
            context,
            limits,
            syntax_limits,
            RevisionResolverWorkCaps::from_limits(limits),
        )
    }

    /// Binds one exact reference under parent-supplied aggregate target/dependency work caps.
    pub fn new_with_work_caps(
        index: &'index RevisionObjectIndex,
        reference: ObjectRef,
        context: RevisionResolverJobContext,
        limits: RevisionResolverLimits,
        syntax_limits: SyntaxLimits,
        work_caps: RevisionResolverWorkCaps,
    ) -> Result<Self, DocumentError> {
        if !context.is_valid() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidRevisionResolverJobContext,
                Some(reference),
                None,
            ));
        }
        if work_caps.max_read_bytes > limits.max_total_object_read_bytes
            || work_caps.max_parse_bytes > limits.max_total_object_parse_bytes
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                Some(reference),
                None,
            ));
        }
        let (locator, target) = index.target(reference)?;
        let object_caps = ObjectWorkCaps::new(
            work_caps
                .max_read_bytes
                .min(limits.object().max_total_read_bytes()),
            work_caps
                .max_parse_bytes
                .min(limits.object().max_total_parse_bytes()),
        )
        .map_err(|error| {
            DocumentError::from_revision_resolver_object(error, reference, locator.offset, true)
        })?;
        let job = OpenObjectEnvelopeJob::new_with_work_caps(
            target,
            context.object_context(),
            limits.object(),
            syntax_limits,
            object_caps,
        )
        .map_err(|error| {
            DocumentError::from_revision_resolver_object(error, reference, locator.offset, true)
        })?;
        Ok(Self {
            index,
            reference,
            context,
            limits,
            work_caps,
            target_work_caps: object_caps,
            syntax_limits,
            stats: RevisionResolverStats::default(),
            state: ResolverState::ObjectEnvelope { locator, job },
        })
    }

    /// Returns the immutable snapshot selected by the revision index.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.index.snapshot()
    }

    /// Returns the exact reference requested by the caller.
    pub const fn reference(&self) -> ObjectRef {
        self.reference
    }

    /// Returns the resolver job context.
    pub const fn context(&self) -> RevisionResolverJobContext {
        self.context
    }

    /// Returns the per-child and resolver-wide aggregate work profile.
    pub const fn limits(&self) -> RevisionResolverLimits {
        self.limits
    }

    /// Returns parent-supplied cumulative target/dependency work caps.
    pub const fn work_caps(&self) -> RevisionResolverWorkCaps {
        self.work_caps
    }

    /// Returns child framing work through the latest poll.
    pub const fn stats(&self) -> RevisionResolverStats {
        self.stats
    }

    /// Returns the current coarse resumable phase.
    pub const fn phase(&self) -> RevisionResolverPhase {
        match self.state {
            ResolverState::ObjectEnvelope { .. } => RevisionResolverPhase::ObjectEnvelope,
            ResolverState::LengthEnvelope { .. } => RevisionResolverPhase::LengthEnvelope,
            ResolverState::ObjectBoundary { .. } => RevisionResolverPhase::ObjectBoundary,
            ResolverState::Complete => RevisionResolverPhase::Complete,
            ResolverState::Failed(_) => RevisionResolverPhase::Failed,
        }
    }

    /// Advances resolution without performing host I/O itself.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn ObjectCancellation + '_),
    ) -> RevisionResolverPoll {
        loop {
            let state = mem::replace(&mut self.state, ResolverState::Complete);
            match state {
                ResolverState::ObjectEnvelope { locator, mut job } => {
                    let poll = job.poll(source, cancellation);
                    self.stats.object = job.stats();
                    match poll {
                        ObjectEnvelopePoll::Direct(object) => {
                            return self.ready(locator, object);
                        }
                        ObjectEnvelopePoll::Stream(envelope) => match envelope.declared_length() {
                            DeclaredStreamLength::Direct { .. } => {
                                let claim = match envelope.direct_length_claim() {
                                    Ok(claim) => claim,
                                    Err(error) => {
                                        return self.fail_object(error, locator.offset, false);
                                    }
                                };
                                let boundary = match OpenStreamBoundaryJob::new(envelope, claim) {
                                    Ok(job) => job,
                                    Err(error) => {
                                        return self.fail_object(error, locator.offset, true);
                                    }
                                };
                                self.state = ResolverState::ObjectBoundary {
                                    locator,
                                    job: boundary,
                                    work_caps: self.target_work_caps,
                                };
                            }
                            DeclaredStreamLength::Indirect { reference, .. } => {
                                if reference == self.reference {
                                    return self.fail(DocumentError::for_code(
                                        DocumentErrorCode::IndirectLengthCycle,
                                        Some(reference),
                                        Some(locator.offset),
                                    ));
                                }
                                let (length_locator, target) = match self.index.target(reference) {
                                    Ok(target) => target,
                                    Err(error) => return self.fail(error),
                                };
                                let read_remaining = match self
                                    .work_caps
                                    .max_read_bytes
                                    .checked_sub(self.stats.object.read_bytes())
                                {
                                    Some(value) if value > 0 => value,
                                    _ => {
                                        return self.fail(DocumentError::resource(
                                            DocumentLimitKind::RevisionResolverObjectReadBytes,
                                            self.work_caps.max_read_bytes,
                                            self.stats.object.read_bytes(),
                                            1,
                                            Some(length_locator.offset),
                                        ));
                                    }
                                };
                                let parse_remaining = match self
                                    .work_caps
                                    .max_parse_bytes
                                    .checked_sub(self.stats.object.parse_bytes())
                                {
                                    Some(value) if value > 0 => value,
                                    _ => {
                                        return self.fail(DocumentError::resource(
                                            DocumentLimitKind::RevisionResolverObjectParseBytes,
                                            self.work_caps.max_parse_bytes,
                                            self.stats.object.parse_bytes(),
                                            1,
                                            Some(length_locator.offset),
                                        ));
                                    }
                                };
                                let length_caps = match ObjectWorkCaps::new(
                                    read_remaining.min(self.limits.object().max_total_read_bytes()),
                                    parse_remaining
                                        .min(self.limits.object().max_total_parse_bytes()),
                                ) {
                                    Ok(caps) => caps,
                                    Err(error) => {
                                        return self.fail_object(
                                            error,
                                            length_locator.offset,
                                            true,
                                        );
                                    }
                                };
                                let length_job = match OpenObjectEnvelopeJob::new_with_work_caps(
                                    target,
                                    self.context.length_context(),
                                    self.limits.object(),
                                    self.syntax_limits,
                                    length_caps,
                                ) {
                                    Ok(job) => job,
                                    Err(error) => {
                                        return self.fail_object(
                                            error,
                                            length_locator.offset,
                                            true,
                                        );
                                    }
                                };
                                self.state = ResolverState::LengthEnvelope {
                                    locator,
                                    envelope,
                                    job: length_job,
                                    work_caps: length_caps,
                                };
                            }
                        },
                        ObjectEnvelopePoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = ResolverState::ObjectEnvelope { locator, job };
                            return RevisionResolverPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        ObjectEnvelopePoll::Failed(error) => {
                            return self.fail_child_object(
                                error,
                                locator.offset,
                                false,
                                ObjectStats::default(),
                                self.target_work_caps,
                            );
                        }
                    }
                }
                ResolverState::LengthEnvelope {
                    locator,
                    envelope,
                    mut job,
                    work_caps: length_work_caps,
                } => {
                    let length_offset = job.target().xref_offset();
                    let poll = job.poll(source, cancellation);
                    self.stats.length_dependency = job.stats();
                    match poll {
                        ObjectEnvelopePoll::Direct(object) => {
                            let resolution =
                                match ResolvedStreamLength::from_uncompressed_object(&object) {
                                    Ok(resolution) => resolution,
                                    Err(error) => {
                                        return self.fail_object(error, length_offset, false);
                                    }
                                };
                            let claim = match envelope.resolved_length_claim(resolution) {
                                Ok(claim) => claim,
                                Err(error) => {
                                    return self.fail_object(error, length_offset, false);
                                }
                            };
                            let continuation_caps =
                                match self.boundary_continuation_caps(locator.offset) {
                                    Ok(caps) => caps,
                                    Err(error) => return self.fail(error),
                                };
                            let boundary = match OpenStreamBoundaryJob::new_with_work_caps(
                                envelope,
                                claim,
                                continuation_caps,
                            ) {
                                Ok(job) => job,
                                Err(error) => {
                                    return self.fail_object(error, locator.offset, true);
                                }
                            };
                            self.state = ResolverState::ObjectBoundary {
                                locator,
                                job: boundary,
                                work_caps: continuation_caps,
                            };
                        }
                        ObjectEnvelopePoll::Stream(_) => {
                            return self.fail(DocumentError::for_code(
                                DocumentErrorCode::InvalidIndirectLength,
                                Some(job.target().reference()),
                                Some(length_offset),
                            ));
                        }
                        ObjectEnvelopePoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = ResolverState::LengthEnvelope {
                                locator,
                                envelope,
                                job,
                                work_caps: length_work_caps,
                            };
                            return RevisionResolverPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        ObjectEnvelopePoll::Failed(error) => {
                            return self.fail_child_object(
                                error,
                                length_offset,
                                false,
                                self.stats.object,
                                length_work_caps,
                            );
                        }
                    }
                }
                ResolverState::ObjectBoundary {
                    locator,
                    mut job,
                    work_caps: boundary_work_caps,
                } => {
                    let poll = job.poll(source, cancellation);
                    self.stats.object = job.stats();
                    match poll {
                        ObjectPoll::Ready(object) => return self.ready(locator, object),
                        ObjectPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = ResolverState::ObjectBoundary {
                                locator,
                                job,
                                work_caps: boundary_work_caps,
                            };
                            return RevisionResolverPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        ObjectPoll::Failed(error) => {
                            return self.fail_child_object(
                                error,
                                locator.offset,
                                false,
                                self.stats.length_dependency,
                                boundary_work_caps,
                            );
                        }
                    }
                }
                ResolverState::Complete => {
                    self.state = ResolverState::Complete;
                    return RevisionResolverPoll::Failed(DocumentError::for_code(
                        DocumentErrorCode::JobAlreadyComplete,
                        Some(self.reference),
                        None,
                    ));
                }
                ResolverState::Failed(error) => {
                    self.state = ResolverState::Failed(error);
                    return RevisionResolverPoll::Failed(error);
                }
            }
        }
    }

    fn ready(
        &mut self,
        locator: UncompressedObjectLocator,
        object: IndirectObject,
    ) -> RevisionResolverPoll {
        self.state = ResolverState::Complete;
        RevisionResolverPoll::Ready(ResolvedObject { locator, object })
    }

    fn boundary_continuation_caps(&self, offset: u64) -> Result<ObjectWorkCaps, DocumentError> {
        let read_cap = self
            .work_caps
            .max_read_bytes
            .checked_sub(self.stats.length_dependency.read_bytes())
            .filter(|cap| *cap >= self.stats.object.read_bytes())
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::RevisionResolverObjectReadBytes,
                    self.work_caps.max_read_bytes,
                    self.stats.total_read_bytes(),
                    1,
                    Some(offset),
                )
            })?
            .min(self.target_work_caps.max_read_bytes());
        let parse_cap = self
            .work_caps
            .max_parse_bytes
            .checked_sub(self.stats.length_dependency.parse_bytes())
            .filter(|cap| *cap >= self.stats.object.parse_bytes())
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::RevisionResolverObjectParseBytes,
                    self.work_caps.max_parse_bytes,
                    self.stats.total_parse_bytes(),
                    1,
                    Some(offset),
                )
            })?
            .min(self.target_work_caps.max_parse_bytes());
        ObjectWorkCaps::new(read_cap, parse_cap).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.reference),
                Some(offset),
            )
        })
    }

    fn fail_object(
        &mut self,
        error: pdf_rs_object::ObjectError,
        offset: u64,
        constructor: bool,
    ) -> RevisionResolverPoll {
        self.fail(DocumentError::from_revision_resolver_object(
            error,
            self.reference,
            offset,
            constructor,
        ))
    }

    fn fail_child_object(
        &mut self,
        error: ObjectError,
        offset: u64,
        constructor: bool,
        aggregate_before: ObjectStats,
        child_caps: ObjectWorkCaps,
    ) -> RevisionResolverPoll {
        let aggregate = error.limit().and_then(|limit| match limit.kind() {
            ObjectLimitKind::TotalReadBytes
                if child_caps.max_read_bytes() < self.limits.object().max_total_read_bytes() =>
            {
                Some((
                    DocumentLimitKind::RevisionResolverObjectReadBytes,
                    self.work_caps.max_read_bytes,
                    aggregate_before.read_bytes(),
                    limit,
                ))
            }
            ObjectLimitKind::TotalParseBytes
                if child_caps.max_parse_bytes() < self.limits.object().max_total_parse_bytes() =>
            {
                Some((
                    DocumentLimitKind::RevisionResolverObjectParseBytes,
                    self.work_caps.max_parse_bytes,
                    aggregate_before.parse_bytes(),
                    limit,
                ))
            }
            ObjectLimitKind::SourceBytes
            | ObjectLimitKind::EnvelopeBytes
            | ObjectLimitKind::BoundaryBytes
            | ObjectLimitKind::StreamBytes
            | ObjectLimitKind::TotalReadBytes
            | ObjectLimitKind::TotalParseBytes
            | ObjectLimitKind::RepairScanBytes
            | ObjectLimitKind::RepairHeaderCandidates
            | ObjectLimitKind::RepairBoundaryCandidates => None,
        });
        if let Some((kind, ceiling, before, limit)) = aggregate {
            let Some(consumed) = before.checked_add(limit.consumed()) else {
                return self.fail(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(self.reference),
                    Some(offset),
                ));
            };
            return self.fail(DocumentError::aggregate_object_resource(
                kind,
                ceiling,
                consumed,
                limit.attempted(),
                error,
                self.reference,
                offset,
            ));
        }
        self.fail_object(error, offset, constructor)
    }

    fn fail(&mut self, error: DocumentError) -> RevisionResolverPoll {
        self.state = ResolverState::Failed(error);
        RevisionResolverPoll::Failed(error)
    }
}

impl fmt::Debug for ResolveObjectJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolveObjectJob")
            .field("snapshot", &self.snapshot())
            .field("reference", &self.reference)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("work_caps", &self.work_caps)
            .field("target_work_caps", &self.target_work_caps)
            .field("syntax_limits", &self.syntax_limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .finish()
    }
}

fn count_uncompressed(
    consumed: u64,
    entries: &[pdf_rs_xref::RevisionEntry],
    limits: DocumentLimits,
    cancellation: &dyn DocumentCancellation,
) -> Result<u64, DocumentError> {
    let mut count = 0_u64;
    for (index, entry) in entries.iter().enumerate() {
        probe(cancellation, index)?;
        if matches!(entry.kind(), RevisionEntryKind::Uncompressed { .. }) {
            count = count.checked_add(1).ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::RevisionResolverAnchors,
                    limits.max_in_use_entries(),
                    consumed,
                    u64::MAX,
                    None,
                )
            })?;
        }
    }
    check_cancelled(cancellation)?;
    let total = consumed.checked_add(count).ok_or_else(|| {
        DocumentError::resource(
            DocumentLimitKind::RevisionResolverAnchors,
            limits.max_in_use_entries(),
            consumed,
            u64::MAX,
            None,
        )
    })?;
    if total > limits.max_in_use_entries() {
        return Err(DocumentError::resource(
            DocumentLimitKind::RevisionResolverAnchors,
            limits.max_in_use_entries(),
            consumed,
            count,
            None,
        ));
    }
    Ok(total)
}

fn append_offsets(
    anchors: &mut Vec<u64>,
    entries: &[pdf_rs_xref::RevisionEntry],
    cancellation: &dyn DocumentCancellation,
) -> Result<(), DocumentError> {
    for (index, entry) in entries.iter().enumerate() {
        probe(cancellation, index)?;
        if let RevisionEntryKind::Uncompressed { offset, .. } = entry.kind() {
            anchors.push(offset);
        }
    }
    Ok(())
}

fn check_cancelled(cancellation: &dyn DocumentCancellation) -> Result<(), DocumentError> {
    if cancellation.is_cancelled() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::Cancelled,
            None,
            None,
        ));
    }
    Ok(())
}

fn probe(cancellation: &dyn DocumentCancellation, index: usize) -> Result<(), DocumentError> {
    if index.is_multiple_of(CANCELLATION_INTERVAL) {
        check_cancelled(cancellation)?;
    }
    Ok(())
}

struct SortMeter {
    limit: u64,
    steps: u64,
    since_probe: usize,
}

impl SortMeter {
    const fn new(limit: u64) -> Self {
        Self {
            limit,
            steps: 0,
            since_probe: 0,
        }
    }

    fn step(&mut self, cancellation: &dyn DocumentCancellation) -> Result<(), DocumentError> {
        if self.steps >= self.limit {
            return Err(DocumentError::resource(
                DocumentLimitKind::RevisionResolverSortSteps,
                self.limit,
                self.steps,
                1,
                None,
            ));
        }
        self.steps += 1;
        self.since_probe += 1;
        if self.since_probe == CANCELLATION_INTERVAL {
            check_cancelled(cancellation)?;
            self.since_probe = 0;
        }
        Ok(())
    }
}

fn cancellable_heapsort(
    values: &mut [u64],
    meter: &mut SortMeter,
    cancellation: &dyn DocumentCancellation,
) -> Result<(), DocumentError> {
    if values.len() < 2 {
        return check_cancelled(cancellation);
    }
    for root in (0..values.len() / 2).rev() {
        sift_down(values, root, values.len(), meter, cancellation)?;
    }
    for end in (1..values.len()).rev() {
        meter.step(cancellation)?;
        values.swap(0, end);
        sift_down(values, 0, end, meter, cancellation)?;
    }
    check_cancelled(cancellation)
}

fn cancellable_dedup(
    values: &mut Vec<u64>,
    cancellation: &dyn DocumentCancellation,
) -> Result<(), DocumentError> {
    check_cancelled(cancellation)?;
    if values.len() < 2 {
        return Ok(());
    }
    let mut write = 1_usize;
    for read in 1..values.len() {
        probe(cancellation, read)?;
        if values[read] != values[write - 1] {
            values[write] = values[read];
            write += 1;
        }
    }
    values.truncate(write);
    check_cancelled(cancellation)
}

fn sift_down(
    values: &mut [u64],
    mut root: usize,
    end: usize,
    meter: &mut SortMeter,
    cancellation: &dyn DocumentCancellation,
) -> Result<(), DocumentError> {
    loop {
        let left = root
            .checked_mul(2)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))?;
        if left >= end {
            return Ok(());
        }
        let mut greatest = root;
        meter.step(cancellation)?;
        if values[greatest] < values[left] {
            greatest = left;
        }
        let right = left + 1;
        if right < end {
            meter.step(cancellation)?;
            if values[greatest] < values[right] {
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

    use pdf_rs_xref::RevisionEntry;

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

    #[test]
    fn resolver_context_requires_four_distinct_checkpoints() {
        let context = RevisionResolverJobContext::new(
            JobId::new(1),
            ResumeCheckpoint::new(2),
            ResumeCheckpoint::new(3),
            ResumeCheckpoint::new(2),
            ResumeCheckpoint::new(4),
            RequestPriority::VisiblePage,
        );
        assert!(!context.is_valid());
    }

    #[test]
    fn resolver_limits_make_each_child_no_more_than_half_the_parent() {
        let object = ObjectLimits::default();
        let limits = RevisionResolverLimits::from_object_limits(object).unwrap();
        assert_eq!(limits.object(), object);
        assert_eq!(
            limits.max_total_object_read_bytes(),
            object.max_total_read_bytes() * 2
        );
        assert_eq!(
            limits.max_total_object_parse_bytes(),
            object.max_total_parse_bytes() * 2
        );
    }

    #[test]
    fn entry_count_and_dedup_probe_cancellation_within_256_steps() {
        let entries = (1..=600)
            .map(|number| RevisionEntry::uncompressed(number, u64::from(number), 0))
            .collect::<Vec<_>>();
        let count_cancel = CancelOnProbe {
            probes: AtomicU64::new(0),
            cancel_at: 2,
        };
        let error =
            count_uncompressed(0, &entries, DocumentLimits::default(), &count_cancel).unwrap_err();
        assert_eq!(error.code(), DocumentErrorCode::Cancelled);
        assert_eq!(count_cancel.probes.load(Ordering::Relaxed), 2);

        let mut values = (0..600).map(|value| value / 2).collect::<Vec<_>>();
        let dedup_cancel = CancelOnProbe {
            probes: AtomicU64::new(0),
            cancel_at: 2,
        };
        let error = cancellable_dedup(&mut values, &dedup_cancel).unwrap_err();
        assert_eq!(error.code(), DocumentErrorCode::Cancelled);
        assert_eq!(dedup_cancel.probes.load(Ordering::Relaxed), 2);
    }
}
