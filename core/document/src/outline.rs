use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteSource, DataTicket, JobId, RequestPriority, ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_object::{ObjectLimitKind, ObjectStats, ObjectWorkCaps};
use pdf_rs_syntax::{Located, ObjectRef, SyntaxObject};

use crate::catalog::parse_strict_catalog;
use crate::dictionary::{
    collect_structural_fields, direct_dictionary, optional_non_null_field, reject_duplicate_field,
    required_field,
};
use crate::model::AttestedRevisionIndexOwner;
use crate::text_string::{TextStringMeasurement, decode_measured_text_string, measure_text_string};
use crate::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, AttestedRevisionIndex,
    DecodedTextString, DocumentCancellation, DocumentError, DocumentErrorCode, DocumentLimitKind,
    OpenAttestedObjectJob, OutlineLimits, SharedAttestedRevisionIndex, StrictCatalog,
};

const CANCELLATION_PROBE_INTERVAL: usize = 256;

/// Runtime identity, lower object checkpoints, and priority for one outline job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutlineJobContext {
    job: JobId,
    object_envelope_checkpoint: ResumeCheckpoint,
    object_boundary_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl OutlineJobContext {
    /// Creates a context whose two child object checkpoints remain runtime-owned.
    pub const fn new(
        job: JobId,
        object_envelope_checkpoint: ResumeCheckpoint,
        object_boundary_checkpoint: ResumeCheckpoint,
        priority: RequestPriority,
    ) -> Self {
        Self {
            job,
            object_envelope_checkpoint,
            object_boundary_checkpoint,
            priority,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the checkpoint used by child object-envelope reads.
    pub const fn object_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.object_envelope_checkpoint
    }

    /// Returns the checkpoint used by child stream-boundary reads.
    pub const fn object_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.object_boundary_checkpoint
    }

    /// Returns the scheduling priority copied to every child object request.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }
}

/// Public phase of one strict Catalog and document-outline job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutlinePhase {
    /// The trailer root is being reopened and validated as a strict Catalog.
    Catalog,
    /// The optional outline root and its reachable items are being traversed.
    Traversing,
    /// The complete reachable outline was validated.
    Ready,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Direct activation-target shape retained for a strict outline item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutlineTargetKind {
    /// The item has no activation target.
    None,
    /// The item contains one direct destination value.
    Destination,
    /// The item contains one direct action dictionary.
    Action,
}

/// One validated outline item in deterministic pre-order.
#[derive(Eq, PartialEq)]
pub struct OutlineItem {
    reference: ObjectRef,
    parent_index: Option<usize>,
    depth: u64,
    title: DecodedTextString,
    declared_count: Option<i64>,
    target_kind: OutlineTargetKind,
    direct_children: u64,
    visible_descendants_if_open: u64,
}

impl OutlineItem {
    /// Returns the exact-generation indirect object identity.
    pub const fn reference(&self) -> ObjectRef {
        self.reference
    }

    /// Returns the pre-order index of the parent item, or `None` for a top-level item.
    pub const fn parent_index(&self) -> Option<usize> {
        self.parent_index
    }

    /// Returns the one-based item depth below the outline root.
    pub const fn depth(&self) -> u64 {
        self.depth
    }

    /// Borrows the decoded Unicode title without exposing it through diagnostics.
    pub fn title(&self) -> &str {
        self.title.as_str()
    }

    /// Borrows the decoded title together with its encoding and capacity evidence.
    pub const fn decoded_title(&self) -> &DecodedTextString {
        &self.title
    }

    /// Returns the source `/Count`, including its open or closed sign.
    pub const fn declared_count(&self) -> Option<i64> {
        self.declared_count
    }

    /// Returns the normalized `/Count`, using zero when the entry was absent.
    pub const fn count(&self) -> i64 {
        match self.declared_count {
            Some(value) => value,
            None => 0,
        }
    }

    /// Returns the validated direct activation-target shape.
    pub const fn target_kind(&self) -> OutlineTargetKind {
        self.target_kind
    }

    /// Returns the number of items in this item's direct child sibling list.
    pub const fn direct_children(&self) -> u64 {
        self.direct_children
    }

    /// Returns descendants visible if this item is opened, respecting nested signs.
    pub const fn visible_descendants_if_open(&self) -> u64 {
        self.visible_descendants_if_open
    }

    /// Returns descendants currently visible below this item.
    ///
    /// A negative declared count closes the item, so none of its descendants are
    /// currently visible even though their validated if-open count is retained.
    pub const fn visible_descendants(&self) -> u64 {
        match self.declared_count {
            Some(count) if count > 0 => self.visible_descendants_if_open,
            Some(_) | None => 0,
        }
    }
}

impl fmt::Debug for OutlineItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutlineItem")
            .field("reference", &self.reference)
            .field("parent_index", &self.parent_index)
            .field("depth", &self.depth)
            .field("title", &"[REDACTED]")
            .field("declared_count", &self.declared_count)
            .field("target_kind", &self.target_kind)
            .field("direct_children", &self.direct_children)
            .field(
                "visible_descendants_if_open",
                &self.visible_descendants_if_open,
            )
            .finish()
    }
}

/// Deterministic traversal work and retained-capacity accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OutlineStats {
    objects_started: u64,
    items_started: u64,
    max_depth: u64,
    max_siblings_per_level: u64,
    object_read_bytes: u64,
    object_parse_bytes: u64,
    title_input_bytes: u64,
    title_utf8_bytes: u64,
    title_reserved_utf8_bytes: u64,
    reserved_working_bytes: u64,
    reserved_result_bytes: u64,
    peak_retained_bytes: u64,
}

impl OutlineStats {
    /// Returns proof-preserving child object jobs successfully started.
    pub const fn objects_started(self) -> u64 {
        self.objects_started
    }

    /// Returns outline-item object jobs successfully started.
    pub const fn items_started(self) -> u64 {
        self.items_started
    }

    /// Returns the greatest root-relative outline-item depth started.
    pub const fn max_depth(self) -> u64 {
        self.max_depth
    }

    /// Returns the greatest sibling position started at any one level.
    pub const fn max_siblings_per_level(self) -> u64 {
        self.max_siblings_per_level
    }

    /// Returns cumulative exact-read bytes charged by child object jobs.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }

    /// Returns cumulative parser-window bytes charged by child object jobs.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }

    /// Returns cumulative decoded source bytes across accepted titles.
    pub const fn title_input_bytes(self) -> u64 {
        self.title_input_bytes
    }

    /// Returns cumulative logical UTF-8 bytes across accepted titles.
    pub const fn title_utf8_bytes(self) -> u64 {
        self.title_utf8_bytes
    }

    /// Returns cumulative allocator-reported UTF-8 title capacity.
    pub const fn title_reserved_utf8_bytes(self) -> u64 {
        self.title_reserved_utf8_bytes
    }

    /// Returns historical allocator-reported capacity reserved for traversal state.
    pub const fn reserved_working_bytes(self) -> u64 {
        self.reserved_working_bytes
    }

    /// Returns historical item-vector and accepted-title capacity reserved by the job.
    ///
    /// A successful result retains this capacity. A failed job releases it before
    /// returning its terminal error while preserving the statistic as evidence.
    pub const fn reserved_result_bytes(self) -> u64 {
        self.reserved_result_bytes
    }

    /// Returns the historical sum of working and result capacity at peak coexistence.
    pub const fn reserved_bytes(self) -> u64 {
        self.reserved_working_bytes
            .saturating_add(self.reserved_result_bytes)
    }

    /// Returns the greatest allocator-reported capacity admitted by the job.
    ///
    /// This excludes a transient allocation that the job rejects before accepting
    /// the corresponding title into its result.
    pub const fn peak_retained_bytes(self) -> u64 {
        self.peak_retained_bytes
    }
}

/// Complete successful strict document outline.
#[derive(Eq, PartialEq)]
pub struct Outline {
    catalog: StrictCatalog,
    root: Option<ObjectRef>,
    root_count: Option<u64>,
    visible_items: u64,
    items: Vec<OutlineItem>,
    stats: OutlineStats,
}

impl Outline {
    /// Returns the validated source- and revision-bound Catalog summary.
    pub const fn catalog(&self) -> StrictCatalog {
        self.catalog
    }

    /// Returns the Catalog's outline-root reference, when present.
    pub const fn root(&self) -> Option<ObjectRef> {
        self.root
    }

    /// Returns the optional nonnegative outline-root `/Count`.
    pub const fn root_count(&self) -> Option<u64> {
        self.root_count
    }

    /// Returns top-level items plus descendants made visible by positive counts.
    pub const fn visible_items(&self) -> u64 {
        self.visible_items
    }

    /// Borrows every validated item in deterministic pre-order.
    pub fn items(&self) -> &[OutlineItem] {
        &self.items
    }

    /// Returns deterministic work and retained-capacity accounting.
    pub const fn stats(&self) -> OutlineStats {
        self.stats
    }
}

impl fmt::Debug for Outline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Outline")
            .field("catalog", &self.catalog)
            .field("root", &self.root)
            .field("root_count", &self.root_count)
            .field("visible_items", &self.visible_items)
            .field("item_count", &self.items.len())
            .field("items", &"[REDACTED]")
            .field("stats", &self.stats)
            .finish()
    }
}

/// Result of polling one strict Catalog and document-outline job.
pub enum OutlinePoll {
    /// The optional outline root and every reachable item were validated.
    Ready(Outline),
    /// The current proof-preserving object child requires exact source ranges.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the active child request.
        missing: SmallRanges,
        /// Child envelope or stream-boundary checkpoint to retain while waiting.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a stable terminal failure.
    Failed(DocumentError),
}

impl fmt::Debug for OutlinePoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(value) => formatter.debug_tuple("Ready").field(value).finish(),
            Self::Pending {
                ticket,
                missing,
                checkpoint,
            } => formatter
                .debug_struct("Pending")
                .field("ticket", ticket)
                .field("missing", missing)
                .field("checkpoint", checkpoint)
                .finish(),
            Self::Failed(error) => formatter.debug_tuple("Failed").field(error).finish(),
        }
    }
}

#[derive(Clone, Copy)]
enum JobState {
    Active,
    Ready,
    Failed,
}

#[derive(Clone, Copy)]
struct VisitItem {
    reference: ObjectRef,
    parent: ObjectRef,
    parent_index: Option<usize>,
    expected_prev: Option<ObjectRef>,
    last: ObjectRef,
    depth: u64,
    sibling_position: u64,
    edge_offset: u64,
}

#[derive(Clone, Copy)]
enum WorkItem {
    Root {
        reference: ObjectRef,
        edge_offset: u64,
    },
    Visit(VisitItem),
    Finish {
        item_index: usize,
        count_offset: u64,
    },
}

#[derive(Clone, Copy)]
enum CurrentTarget {
    Catalog,
    Root {
        reference: ObjectRef,
        edge_offset: u64,
    },
    Item(VisitItem),
}

struct ChildState {
    job: OpenAttestedObjectJob,
    accounted_stats: ObjectStats,
    work_caps: ObjectWorkCaps,
    reference: ObjectRef,
    offset: u64,
}

/// One-shot job that validates the strict Catalog and complete document outline.
///
/// The job performs no file, network, callback, or async-runtime I/O. It reopens
/// only objects covered by one [`AttestedRevisionIndex`], suspends on exact byte
/// requests, and never trusts outline counts to allocate. Result and traversal
/// storage are reserved from validated limits before the job is published.
pub struct ReadOutlineJob<'index> {
    index: AttestedRevisionIndexOwner<'index>,
    snapshot: SourceSnapshot,
    context: OutlineJobContext,
    limits: OutlineLimits,
    catalog: Option<StrictCatalog>,
    root: Option<ObjectRef>,
    root_count: Option<u64>,
    root_count_offset: Option<u64>,
    work: Vec<WorkItem>,
    seen_slots: Vec<u64>,
    seen_count: u64,
    active_items: Vec<ObjectRef>,
    items: Vec<OutlineItem>,
    current: Option<CurrentTarget>,
    child: Option<ChildState>,
    stats: OutlineStats,
    visible_items: u64,
    state: JobState,
    terminal_error: DocumentError,
}

impl ReadOutlineJob<'_> {
    /// Returns the immutable source snapshot covered by the owning attested index.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns runtime identity, child checkpoints, and scheduling priority.
    pub const fn context(&self) -> OutlineJobContext {
        self.context
    }

    /// Returns the complete validated outline limits.
    pub const fn limits(&self) -> OutlineLimits {
        self.limits
    }

    /// Returns deterministic work and retained-capacity accounting through the latest poll.
    pub const fn stats(&self) -> OutlineStats {
        self.stats
    }

    /// Returns the public job phase.
    pub const fn phase(&self) -> OutlinePhase {
        match self.state {
            JobState::Ready => OutlinePhase::Ready,
            JobState::Failed => OutlinePhase::Failed,
            JobState::Active if self.catalog.is_some() => OutlinePhase::Traversing,
            JobState::Active => OutlinePhase::Catalog,
        }
    }

    /// Advances the job without performing host I/O or resuming inside a callback.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> OutlinePoll {
        if !matches!(self.state, JobState::Active) {
            return OutlinePoll::Failed(self.terminal_error);
        }

        loop {
            if source.snapshot() != self.index.snapshot() {
                return self.fail(DocumentError::for_code(
                    DocumentErrorCode::SourceSnapshotMismatch,
                    self.current_reference(),
                    self.current_offset(),
                ));
            }
            if cancellation.is_cancelled() {
                return self.fail(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    self.current_reference(),
                    self.current_offset(),
                ));
            }

            if self.child.is_none() {
                if self.current.is_none() {
                    if self.catalog.is_none() {
                        self.current = Some(CurrentTarget::Catalog);
                    } else {
                        match self.work.pop() {
                            Some(WorkItem::Root {
                                reference,
                                edge_offset,
                            }) => {
                                self.current = Some(CurrentTarget::Root {
                                    reference,
                                    edge_offset,
                                });
                            }
                            Some(WorkItem::Visit(visit)) => {
                                self.current = Some(CurrentTarget::Item(visit));
                            }
                            Some(WorkItem::Finish {
                                item_index,
                                count_offset,
                            }) => {
                                if let Err(error) = self.finish_item(item_index, count_offset) {
                                    return self.fail(error);
                                }
                                continue;
                            }
                            None => return self.finish_ready(),
                        }
                    }
                }

                if let Err(error) = self.start_child() {
                    return self.fail(error);
                }
            }

            let Some(mut child) = self.child.take() else {
                return self.fail(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    self.current_reference(),
                    self.current_offset(),
                ));
            };
            let outcome = child.job.poll(source, cancellation);
            if let Err(error) = self.account_child_stats(&mut child) {
                return self.fail(error);
            }

            match outcome {
                AttestedObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    self.child = Some(child);
                    return OutlinePoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    };
                }
                AttestedObjectPoll::Failed(error) => {
                    let mapped = self.map_child_error(error, &child);
                    return self.fail(mapped);
                }
                AttestedObjectPoll::Ready(object) => {
                    if source.snapshot() != self.index.snapshot() {
                        return self.fail(DocumentError::for_code(
                            DocumentErrorCode::SourceSnapshotMismatch,
                            Some(child.reference),
                            Some(child.offset),
                        ));
                    }
                    if cancellation.is_cancelled() {
                        return self.fail(DocumentError::for_code(
                            DocumentErrorCode::Cancelled,
                            Some(child.reference),
                            Some(child.offset),
                        ));
                    }

                    let Some(target) = self.current.take() else {
                        return self.fail(DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(child.reference),
                            Some(child.offset),
                        ));
                    };
                    let result = match target {
                        CurrentTarget::Catalog => self.accept_catalog(object, cancellation),
                        CurrentTarget::Root { reference, .. } => {
                            self.accept_root(reference, object, cancellation)
                        }
                        CurrentTarget::Item(visit) => self.accept_item(visit, object, cancellation),
                    };
                    if let Err(error) = result {
                        return self.fail(error);
                    }
                }
            }
        }
    }

    fn start_child(&mut self) -> Result<(), DocumentError> {
        let target = self.current.ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_reference(),
                self.current_offset(),
            )
        })?;
        let (reference, item) = match target {
            CurrentTarget::Catalog => (self.index.root(), None),
            CurrentTarget::Root { reference, .. } => (reference, None),
            CurrentTarget::Item(visit) => (visit.reference, Some(visit)),
        };
        let attestation = self.index.attestation(reference)?;
        let offset = attestation.xref_offset();

        if let Some(visit) = item {
            if self.stats.items_started >= self.limits.max_items() {
                return Err(DocumentError::outline_resource(
                    DocumentLimitKind::OutlineItems,
                    self.limits.max_items(),
                    self.stats.items_started,
                    1,
                    reference,
                    Some(offset),
                ));
            }
            if visit.depth > self.limits.max_depth() {
                return Err(DocumentError::outline_resource(
                    DocumentLimitKind::OutlineDepth,
                    self.limits.max_depth(),
                    visit.depth.saturating_sub(1),
                    1,
                    reference,
                    Some(offset),
                ));
            }
            if visit.sibling_position > self.limits.max_siblings_per_level() {
                return Err(DocumentError::outline_resource(
                    DocumentLimitKind::OutlineSiblings,
                    self.limits.max_siblings_per_level(),
                    visit.sibling_position.saturating_sub(1),
                    1,
                    reference,
                    Some(offset),
                ));
            }
        }

        let read_remaining = self
            .limits
            .max_total_object_read_bytes()
            .checked_sub(self.stats.object_read_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if read_remaining == 0 {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineObjectReadBytes,
                self.limits.max_total_object_read_bytes(),
                self.stats.object_read_bytes,
                1,
                reference,
                Some(offset),
            ));
        }
        let parse_remaining = self
            .limits
            .max_total_object_parse_bytes()
            .checked_sub(self.stats.object_parse_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if parse_remaining == 0 {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineObjectParseBytes,
                self.limits.max_total_object_parse_bytes(),
                self.stats.object_parse_bytes,
                1,
                reference,
                Some(offset),
            ));
        }

        let work_caps = ObjectWorkCaps::new(
            read_remaining.min(self.index.object_limits().max_total_read_bytes()),
            parse_remaining.min(self.index.object_limits().max_total_parse_bytes()),
        )
        .map_err(|error| DocumentError::from_object_access_constructor(error, reference, offset))?;
        let context = AttestedObjectJobContext::new(
            self.context.job,
            self.context.object_envelope_checkpoint,
            self.context.object_boundary_checkpoint,
            self.context.priority,
        );
        let job = self.index.open_object(reference, context, work_caps)?;

        self.stats.objects_started =
            self.stats.objects_started.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if let Some(visit) = item {
            self.stats.items_started =
                self.stats.items_started.checked_add(1).ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(offset),
                    )
                })?;
            self.stats.max_depth = self.stats.max_depth.max(visit.depth);
            self.stats.max_siblings_per_level = self
                .stats
                .max_siblings_per_level
                .max(visit.sibling_position);
        }
        self.child = Some(ChildState {
            job,
            accounted_stats: ObjectStats::default(),
            work_caps,
            reference,
            offset,
        });
        Ok(())
    }

    fn account_child_stats(&mut self, child: &mut ChildState) -> Result<(), DocumentError> {
        let current = child.job.stats();
        let read_delta = current
            .read_bytes()
            .checked_sub(child.accounted_stats.read_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.offset),
                )
            })?;
        let parse_delta = current
            .parse_bytes()
            .checked_sub(child.accounted_stats.parse_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.offset),
                )
            })?;
        self.stats.object_read_bytes = self
            .stats
            .object_read_bytes
            .checked_add(read_delta)
            .filter(|value| *value <= self.limits.max_total_object_read_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.offset),
                )
            })?;
        self.stats.object_parse_bytes = self
            .stats
            .object_parse_bytes
            .checked_add(parse_delta)
            .filter(|value| *value <= self.limits.max_total_object_parse_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.offset),
                )
            })?;
        child.accounted_stats = current;
        Ok(())
    }

    fn map_child_error(&self, error: DocumentError, child: &ChildState) -> DocumentError {
        if error.code() == DocumentErrorCode::ResourceLimit
            && let Some(lower) = error.object_error()
            && let Some(limit) = lower.limit()
        {
            match limit.kind() {
                ObjectLimitKind::TotalReadBytes
                    if child.work_caps.max_read_bytes()
                        < self.index.object_limits().max_total_read_bytes() =>
                {
                    return DocumentError::aggregate_object_resource(
                        DocumentLimitKind::OutlineObjectReadBytes,
                        self.limits.max_total_object_read_bytes(),
                        self.stats.object_read_bytes,
                        limit.attempted(),
                        lower,
                        child.reference,
                        child.offset,
                    );
                }
                ObjectLimitKind::TotalParseBytes
                    if child.work_caps.max_parse_bytes()
                        < self.index.object_limits().max_total_parse_bytes() =>
                {
                    return DocumentError::aggregate_object_resource(
                        DocumentLimitKind::OutlineObjectParseBytes,
                        self.limits.max_total_object_parse_bytes(),
                        self.stats.object_parse_bytes,
                        limit.attempted(),
                        lower,
                        child.reference,
                        child.offset,
                    );
                }
                ObjectLimitKind::SourceBytes
                | ObjectLimitKind::EnvelopeBytes
                | ObjectLimitKind::BoundaryBytes
                | ObjectLimitKind::StreamBytes
                | ObjectLimitKind::TotalReadBytes
                | ObjectLimitKind::TotalParseBytes
                | ObjectLimitKind::RepairScanBytes
                | ObjectLimitKind::RepairHeaderCandidates
                | ObjectLimitKind::RepairBoundaryCandidates => {}
            }
        }
        error
    }

    fn accept_catalog(
        &mut self,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let parsed = parse_strict_catalog(&self.index, &object, cancellation)?;
        let catalog = parsed.summary();
        let outlines = parsed.outlines_entry(cancellation)?;
        self.catalog = Some(catalog);
        if let Some(entry) = outlines {
            let reference = entry.reference();
            self.root = Some(reference);
            self.push_work(WorkItem::Root {
                reference,
                edge_offset: entry.value_offset(),
            })?;
        }
        Ok(())
    }

    fn accept_root(
        &mut self,
        expected: ObjectRef,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let reference = object.reference();
        let offset = object.attestation().xref_offset();
        if reference != expected || self.root != Some(reference) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(offset),
            ));
        }
        let dictionary = direct_dictionary(
            &object,
            self.index.snapshot(),
            DocumentErrorCode::InvalidOutlineDictionary,
        )?;
        let fields = collect_structural_fields(
            dictionary,
            [
                b"Type".as_slice(),
                b"First".as_slice(),
                b"Last".as_slice(),
                b"Count".as_slice(),
            ],
            reference,
            cancellation,
        )?;
        for field in 0..4 {
            reject_duplicate_field(&fields, field, reference)?;
        }

        if let Some(value) = optional_non_null_field(&fields, 0) {
            match value.value() {
                SyntaxObject::Name(name) if name.bytes() == b"Outlines" => {}
                SyntaxObject::Reference(_) => {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::UnsupportedOutlineRepresentation,
                        Some(reference),
                        Some(value.span().start()),
                    ));
                }
                _ => {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::InvalidOutlineDictionary,
                        Some(reference),
                        Some(value.span().start()),
                    ));
                }
            }
        }

        let first = optional_non_null_field(&fields, 1);
        let last = optional_non_null_field(&fields, 2);
        let pair = outline_reference_pair(
            first,
            last,
            reference,
            offset,
            DocumentErrorCode::InvalidOutlineDictionary,
        )?;

        self.root_count = match optional_non_null_field(&fields, 3) {
            None => None,
            Some(value) => match value.value() {
                SyntaxObject::Reference(_) => {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::UnsupportedOutlineRepresentation,
                        Some(reference),
                        Some(value.span().start()),
                    ));
                }
                SyntaxObject::Integer(count) if *count >= 0 => {
                    self.root_count_offset = Some(value.span().start());
                    Some(u64::try_from(*count).map_err(|_| {
                        DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(reference),
                            Some(value.span().start()),
                        )
                    })?)
                }
                _ => {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::InvalidOutlineDictionary,
                        Some(reference),
                        Some(value.span().start()),
                    ));
                }
            },
        };

        if let Some((first, last)) = pair {
            self.schedule_item(
                VisitItem {
                    reference: first.reference,
                    parent: reference,
                    parent_index: None,
                    expected_prev: None,
                    last: last.reference,
                    depth: 1,
                    sibling_position: 1,
                    edge_offset: first.offset,
                },
                cancellation,
            )?;
        }
        Ok(())
    }

    fn accept_item(
        &mut self,
        visit: VisitItem,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let reference = object.reference();
        let object_offset = object.attestation().xref_offset();
        if reference != visit.reference {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(object_offset),
            ));
        }
        let dictionary = direct_dictionary(
            &object,
            self.index.snapshot(),
            DocumentErrorCode::InvalidOutlineItem,
        )?;
        let fields = collect_structural_fields(
            dictionary,
            [
                b"Title".as_slice(),
                b"Parent".as_slice(),
                b"Prev".as_slice(),
                b"Next".as_slice(),
                b"First".as_slice(),
                b"Last".as_slice(),
                b"Count".as_slice(),
                b"Dest".as_slice(),
                b"A".as_slice(),
            ],
            reference,
            cancellation,
        )?;
        for field in 0..9 {
            reject_duplicate_field(&fields, field, reference)?;
        }

        let title_value = required_field(
            &fields,
            0,
            reference,
            object_offset,
            DocumentErrorCode::InvalidOutlineTitle,
        )?;
        let title_string = match title_value.value() {
            SyntaxObject::String(value) => value,
            SyntaxObject::Reference(_) => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::UnsupportedOutlineRepresentation,
                    Some(reference),
                    Some(title_value.span().start()),
                ));
            }
            _ => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidOutlineTitle,
                    Some(reference),
                    Some(title_value.span().start()),
                ));
            }
        };

        let parent_value = required_field(
            &fields,
            1,
            reference,
            object_offset,
            DocumentErrorCode::OutlineParentMismatch,
        )?;
        if parent_value.value().as_reference() != Some(visit.parent) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::OutlineParentMismatch,
                Some(reference),
                Some(parent_value.span().start()),
            ));
        }
        validate_prev(
            reference,
            visit.expected_prev,
            optional_non_null_field(&fields, 2),
            object_offset,
        )?;

        let next_value = optional_non_null_field(&fields, 3);
        let next = if reference == visit.last {
            if let Some(value) = next_value {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::OutlineSiblingMismatch,
                    Some(reference),
                    Some(value.span().start()),
                ));
            }
            None
        } else {
            let value = next_value.ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::OutlineSiblingMismatch,
                    Some(reference),
                    Some(object_offset),
                )
            })?;
            let next = value.value().as_reference().ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::OutlineSiblingMismatch,
                    Some(reference),
                    Some(value.span().start()),
                )
            })?;
            Some((next, value.span().start()))
        };

        let children = outline_reference_pair(
            optional_non_null_field(&fields, 4),
            optional_non_null_field(&fields, 5),
            reference,
            object_offset,
            DocumentErrorCode::InvalidOutlineItem,
        )?;
        let (declared_count, count_offset) = match optional_non_null_field(&fields, 6) {
            None => (None, object_offset),
            Some(value) => match value.value() {
                SyntaxObject::Integer(count) => (Some(*count), value.span().start()),
                SyntaxObject::Reference(_) => {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::UnsupportedOutlineRepresentation,
                        Some(reference),
                        Some(value.span().start()),
                    ));
                }
                _ => {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::InvalidOutlineItem,
                        Some(reference),
                        Some(value.span().start()),
                    ));
                }
            },
        };
        let target_kind = validate_target(
            reference,
            optional_non_null_field(&fields, 7),
            optional_non_null_field(&fields, 8),
        )?;

        let measurement =
            measure_text_string(title_string, self.limits.title_limits(), cancellation).map_err(
                |error| {
                    DocumentError::from_outline_text(error, reference, title_value.span().start())
                },
            )?;
        self.preflight_title(measurement, reference, title_value.span().start())?;
        let title = decode_measured_text_string(
            title_string,
            self.limits.title_limits(),
            measurement,
            cancellation,
        )
        .map_err(|error| {
            DocumentError::from_outline_text(error, reference, title_value.span().start())
        })?;
        self.account_title(&title, reference, title_value.span().start())?;

        if self.items.len() >= self.items.capacity()
            || self.active_items.len() >= self.active_items.capacity()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(object_offset),
            ));
        }
        let item_index = self.items.len();
        if visit
            .parent_index
            .is_some_and(|parent| parent >= item_index)
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(object_offset),
            ));
        }
        self.items.push(OutlineItem {
            reference,
            parent_index: visit.parent_index,
            depth: visit.depth,
            title,
            declared_count,
            target_kind,
            direct_children: 0,
            visible_descendants_if_open: 0,
        });
        self.active_items.push(reference);
        if let Some((next, edge_offset)) = next {
            let sibling_position = visit.sibling_position.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(edge_offset),
                )
            })?;
            self.schedule_item(
                VisitItem {
                    reference: next,
                    parent: visit.parent,
                    parent_index: visit.parent_index,
                    expected_prev: Some(reference),
                    last: visit.last,
                    depth: visit.depth,
                    sibling_position,
                    edge_offset,
                },
                cancellation,
            )?;
        }
        self.push_work(WorkItem::Finish {
            item_index,
            count_offset,
        })?;
        if let Some((first, last)) = children {
            let depth = visit.depth.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(first.offset),
                )
            })?;
            self.schedule_item(
                VisitItem {
                    reference: first.reference,
                    parent: reference,
                    parent_index: Some(item_index),
                    expected_prev: None,
                    last: last.reference,
                    depth,
                    sibling_position: 1,
                    edge_offset: first.offset,
                },
                cancellation,
            )?;
        }
        Ok(())
    }

    fn preflight_title(
        &self,
        measurement: TextStringMeasurement,
        reference: ObjectRef,
        offset: u64,
    ) -> Result<(), DocumentError> {
        let next_input = self
            .stats
            .title_input_bytes
            .checked_add(measurement.input_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if next_input > self.limits.max_total_title_input_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineTotalTitleInputBytes,
                self.limits.max_total_title_input_bytes(),
                self.stats.title_input_bytes,
                measurement.input_bytes(),
                reference,
                Some(offset),
            ));
        }

        let next_utf8 = self
            .stats
            .title_utf8_bytes
            .checked_add(measurement.utf8_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if next_utf8 > self.limits.max_total_title_utf8_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineTotalTitleUtf8Bytes,
                self.limits.max_total_title_utf8_bytes(),
                self.stats.title_utf8_bytes,
                measurement.utf8_bytes(),
                reference,
                Some(offset),
            ));
        }

        let requested_result = self
            .stats
            .reserved_result_bytes
            .checked_add(measurement.utf8_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        let requested_retained = self
            .stats
            .reserved_working_bytes
            .checked_add(requested_result)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if requested_retained > self.limits.max_retained_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineRetainedBytes,
                self.limits.max_retained_bytes(),
                self.stats.reserved_bytes(),
                measurement.utf8_bytes(),
                reference,
                Some(offset),
            ));
        }
        Ok(())
    }

    fn account_title(
        &mut self,
        title: &DecodedTextString,
        reference: ObjectRef,
        offset: u64,
    ) -> Result<(), DocumentError> {
        let next_input = self
            .stats
            .title_input_bytes
            .checked_add(title.input_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if next_input > self.limits.max_total_title_input_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineTotalTitleInputBytes,
                self.limits.max_total_title_input_bytes(),
                self.stats.title_input_bytes,
                title.input_bytes(),
                reference,
                Some(offset),
            ));
        }
        let next_utf8 = self
            .stats
            .title_utf8_bytes
            .checked_add(title.utf8_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if next_utf8 > self.limits.max_total_title_utf8_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineTotalTitleUtf8Bytes,
                self.limits.max_total_title_utf8_bytes(),
                self.stats.title_utf8_bytes,
                title.utf8_bytes(),
                reference,
                Some(offset),
            ));
        }
        let next_reserved_utf8 = self
            .stats
            .title_reserved_utf8_bytes
            .checked_add(title.reserved_utf8_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if next_reserved_utf8 > self.limits.max_total_title_utf8_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineTotalTitleUtf8Bytes,
                self.limits.max_total_title_utf8_bytes(),
                self.stats.title_reserved_utf8_bytes,
                title.reserved_utf8_bytes(),
                reference,
                Some(offset),
            ));
        }
        let next_result = self
            .stats
            .reserved_result_bytes
            .checked_add(title.reserved_utf8_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        let next_retained = self
            .stats
            .reserved_working_bytes
            .checked_add(next_result)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if next_retained > self.limits.max_retained_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineRetainedBytes,
                self.limits.max_retained_bytes(),
                self.stats.reserved_bytes(),
                title.reserved_utf8_bytes(),
                reference,
                Some(offset),
            ));
        }

        self.stats.title_input_bytes = next_input;
        self.stats.title_utf8_bytes = next_utf8;
        self.stats.title_reserved_utf8_bytes = next_reserved_utf8;
        self.stats.reserved_result_bytes = next_result;
        self.stats.peak_retained_bytes = self.stats.peak_retained_bytes.max(next_retained);
        Ok(())
    }

    fn finish_item(&mut self, item_index: usize, count_offset: u64) -> Result<(), DocumentError> {
        let Some(item) = self.items.get(item_index) else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                None,
                Some(count_offset),
            ));
        };
        let reference = item.reference;
        let parent_index = item.parent_index;
        let declared_count = item.declared_count;
        let direct_children = item.direct_children;
        let visible_descendants = item.visible_descendants_if_open;
        let Some(active) = self.active_items.pop() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(count_offset),
            ));
        };
        if active != reference {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(count_offset),
            ));
        }

        let count_matches = if direct_children == 0 {
            declared_count.is_none_or(|count| count == 0)
        } else {
            declared_count
                .is_some_and(|count| count != 0 && count.unsigned_abs() == visible_descendants)
        };
        if !count_matches {
            return Err(DocumentError::for_code(
                DocumentErrorCode::OutlineCountMismatch,
                Some(reference),
                Some(count_offset),
            ));
        }

        let visible_contribution = 1_u64
            .checked_add(if declared_count.is_some_and(|count| count > 0) {
                visible_descendants
            } else {
                0
            })
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(count_offset),
                )
            })?;
        match parent_index {
            Some(parent_index) => {
                let Some(parent) = self.items.get_mut(parent_index) else {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(count_offset),
                    ));
                };
                parent.direct_children =
                    parent.direct_children.checked_add(1).ok_or_else(|| {
                        DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(reference),
                            Some(count_offset),
                        )
                    })?;
                parent.visible_descendants_if_open = parent
                    .visible_descendants_if_open
                    .checked_add(visible_contribution)
                    .ok_or_else(|| {
                        DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(reference),
                            Some(count_offset),
                        )
                    })?;
            }
            None => {
                self.visible_items = self
                    .visible_items
                    .checked_add(visible_contribution)
                    .ok_or_else(|| {
                        DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(reference),
                            Some(count_offset),
                        )
                    })?;
            }
        }
        Ok(())
    }

    fn schedule_item(
        &mut self,
        visit: VisitItem,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        if self.active_contains(visit.reference, cancellation)? {
            return Err(DocumentError::for_code(
                DocumentErrorCode::OutlineCycle,
                Some(visit.reference),
                Some(visit.edge_offset),
            ));
        }
        if self.seen_contains(visit.reference, cancellation)? {
            let code = if self.prior_in_sibling_chain(
                visit.reference,
                visit.parent_index,
                cancellation,
            )? {
                DocumentErrorCode::OutlineCycle
            } else {
                DocumentErrorCode::DuplicateOutlineItem
            };
            return Err(DocumentError::for_code(
                code,
                Some(visit.reference),
                Some(visit.edge_offset),
            ));
        }
        if visit.depth > self.limits.max_depth() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineDepth,
                self.limits.max_depth(),
                visit.depth.saturating_sub(1),
                1,
                visit.reference,
                Some(visit.edge_offset),
            ));
        }
        if visit.sibling_position > self.limits.max_siblings_per_level() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineSiblings,
                self.limits.max_siblings_per_level(),
                visit.sibling_position.saturating_sub(1),
                1,
                visit.reference,
                Some(visit.edge_offset),
            ));
        }
        if self.seen_count >= self.limits.max_items() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineItems,
                self.limits.max_items(),
                self.seen_count,
                1,
                visit.reference,
                Some(visit.edge_offset),
            ));
        }
        if !self.insert_seen(visit.reference, cancellation)? {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(visit.reference),
                Some(visit.edge_offset),
            ));
        }
        self.push_work(WorkItem::Visit(visit))
    }

    fn active_contains(
        &self,
        reference: ObjectRef,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<bool, DocumentError> {
        if self.root == Some(reference) {
            return Ok(true);
        }
        for (index, active) in self.active_items.iter().enumerate() {
            if index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(reference),
                    None,
                ));
            }
            if *active == reference {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn prior_in_sibling_chain(
        &self,
        reference: ObjectRef,
        parent_index: Option<usize>,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<bool, DocumentError> {
        for (index, item) in self.items.iter().enumerate() {
            if index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(reference),
                    None,
                ));
            }
            if item.reference == reference && item.parent_index == parent_index {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn seen_contains(
        &self,
        reference: ObjectRef,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<bool, DocumentError> {
        if self.seen_slots.is_empty() || !self.seen_slots.len().is_power_of_two() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                None,
            ));
        }
        let key = encode_reference(reference);
        let mask = self.seen_slots.len() - 1;
        let mut slot = reference_slot(key, mask);
        for probe in 0..self.seen_slots.len() {
            if probe % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(reference),
                    None,
                ));
            }
            match self.seen_slots[slot] {
                0 => return Ok(false),
                existing if existing == key => return Ok(true),
                _ => slot = (slot + 1) & mask,
            }
        }
        Err(DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(reference),
            None,
        ))
    }

    fn insert_seen(
        &mut self,
        reference: ObjectRef,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<bool, DocumentError> {
        if self.seen_slots.is_empty() || !self.seen_slots.len().is_power_of_two() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                None,
            ));
        }
        let key = encode_reference(reference);
        let mask = self.seen_slots.len() - 1;
        let mut slot = reference_slot(key, mask);
        for probe in 0..self.seen_slots.len() {
            if probe % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(reference),
                    None,
                ));
            }
            match self.seen_slots[slot] {
                0 => {
                    self.seen_slots[slot] = key;
                    self.seen_count = self.seen_count.checked_add(1).ok_or_else(|| {
                        DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(reference),
                            None,
                        )
                    })?;
                    return Ok(true);
                }
                existing if existing == key => return Ok(false),
                _ => slot = (slot + 1) & mask,
            }
        }
        Err(DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(reference),
            None,
        ))
    }

    fn push_work(&mut self, item: WorkItem) -> Result<(), DocumentError> {
        if self.work.len() >= self.work.capacity() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_reference(),
                self.current_offset(),
            ));
        }
        self.work.push(item);
        Ok(())
    }

    fn finish_ready(&mut self) -> OutlinePoll {
        if !self.active_items.is_empty() || self.current.is_some() || self.child.is_some() {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_reference(),
                self.current_offset(),
            ));
        }
        let Some(catalog) = self.catalog.take() else {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.index.root()),
                self.root_offset(),
            ));
        };
        if self.items.is_empty() {
            if self.root_count.is_some() {
                return self.fail(DocumentError::for_code(
                    DocumentErrorCode::OutlineCountMismatch,
                    self.root,
                    self.root_count_offset
                        .or_else(|| self.outline_root_offset()),
                ));
            }
        } else if self.root_count != Some(self.visible_items) {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::OutlineCountMismatch,
                self.root,
                self.root_count_offset
                    .or_else(|| self.outline_root_offset()),
            ));
        }

        let root = self.root;
        let root_count = self.root_count;
        let visible_items = self.visible_items;
        let stats = self.stats;
        self.release_working();
        let items = mem::take(&mut self.items);
        self.state = JobState::Ready;
        self.terminal_error = DocumentError::for_code(
            DocumentErrorCode::JobAlreadyComplete,
            Some(self.index.root()),
            self.root_offset(),
        );
        OutlinePoll::Ready(Outline {
            catalog,
            root,
            root_count,
            visible_items,
            items,
            stats,
        })
    }

    fn fail(&mut self, error: DocumentError) -> OutlinePoll {
        self.child = None;
        self.current = None;
        self.catalog = None;
        self.release_working();
        self.items = Vec::new();
        self.state = JobState::Failed;
        self.terminal_error = error;
        OutlinePoll::Failed(error)
    }

    fn release_working(&mut self) {
        self.work = Vec::new();
        self.seen_slots = Vec::new();
        self.seen_count = 0;
        self.active_items = Vec::new();
    }

    fn current_reference(&self) -> Option<ObjectRef> {
        match self.current {
            Some(CurrentTarget::Catalog) => Some(self.index.root()),
            Some(CurrentTarget::Root { reference, .. }) => Some(reference),
            Some(CurrentTarget::Item(visit)) => Some(visit.reference),
            None => None,
        }
    }

    fn current_offset(&self) -> Option<u64> {
        match self.current {
            Some(CurrentTarget::Root { edge_offset, .. }) => Some(edge_offset),
            Some(CurrentTarget::Item(visit)) => Some(visit.edge_offset),
            _ => self
                .current_reference()
                .and_then(|reference| self.index.attestation(reference).ok())
                .map(crate::ObjectAttestation::xref_offset),
        }
    }

    fn root_offset(&self) -> Option<u64> {
        self.index
            .attestation(self.index.root())
            .ok()
            .map(crate::ObjectAttestation::xref_offset)
    }

    fn outline_root_offset(&self) -> Option<u64> {
        self.root
            .and_then(|reference| self.index.attestation(reference).ok())
            .map(crate::ObjectAttestation::xref_offset)
    }
}

impl fmt::Debug for ReadOutlineJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadOutlineJob")
            .field("snapshot", &self.index.snapshot())
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .field("root", &self.root)
            .field("current", &"[REDACTED]")
            .field("work", &"[REDACTED]")
            .field("seen", &"[REDACTED]")
            .field("active_path", &"[REDACTED]")
            .field("items", &"[REDACTED]")
            .field("child", &"[REDACTED]")
            .finish()
    }
}

impl AttestedRevisionIndex {
    /// Creates a bounded one-shot job that validates and retains the document outline.
    pub fn read_outline(
        &self,
        context: OutlineJobContext,
        limits: OutlineLimits,
    ) -> Result<ReadOutlineJob<'_>, DocumentError> {
        read_outline_with_owner(AttestedRevisionIndexOwner::Borrowed(self), context, limits)
    }
}

impl SharedAttestedRevisionIndex {
    /// Creates an outline job that owns a clone of this proof handle.
    pub fn read_outline_owned(
        &self,
        context: OutlineJobContext,
        limits: OutlineLimits,
    ) -> Result<ReadOutlineJob<'static>, DocumentError> {
        read_outline_with_owner(
            AttestedRevisionIndexOwner::Shared(self.clone()),
            context,
            limits,
        )
    }
}

fn read_outline_with_owner<'index>(
    index: AttestedRevisionIndexOwner<'index>,
    context: OutlineJobContext,
    limits: OutlineLimits,
) -> Result<ReadOutlineJob<'index>, DocumentError> {
    let attested = index.as_attested();
    let snapshot = attested.snapshot();
    let root = attested.root();
    let root_offset = attested.attestation(root)?.xref_offset();
    if context.object_envelope_checkpoint == context.object_boundary_checkpoint {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidOutlineJobContext,
            Some(root),
            Some(root_offset),
        ));
    }

    let item_capacity = usize::try_from(limits.max_items()).map_err(|_| {
        DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(root),
            Some(root_offset),
        )
    })?;
    let work_capacity = limits
        .max_items()
        .checked_mul(2)
        .and_then(|value| value.checked_add(1))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(root_offset),
            )
        })?;
    let seen_capacity = limits
        .max_items()
        .checked_mul(2)
        .and_then(u64::checked_next_power_of_two)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(root_offset),
            )
        })?;
    let active_capacity = usize::try_from(limits.max_depth()).map_err(|_| {
        DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(root),
            Some(root_offset),
        )
    })?;
    let requested_total = validate_retained_plan(
        limits,
        root,
        root_offset,
        [item_capacity, work_capacity, seen_capacity, active_capacity],
    )?;
    let allocation_error = |attempted| {
        DocumentError::outline_resource(
            DocumentLimitKind::OutlineRetainedBytes,
            limits.max_retained_bytes(),
            0,
            attempted,
            root,
            Some(root_offset),
        )
    };
    let mut items = Vec::new();
    items
        .try_reserve_exact(item_capacity)
        .map_err(|_| allocation_error(requested_total))?;
    let items_plan = validate_retained_plan(
        limits,
        root,
        root_offset,
        [
            items.capacity(),
            work_capacity,
            seen_capacity,
            active_capacity,
        ],
    )?;
    let mut work = Vec::new();
    work.try_reserve_exact(work_capacity)
        .map_err(|_| allocation_error(items_plan))?;
    let work_plan = validate_retained_plan(
        limits,
        root,
        root_offset,
        [
            items.capacity(),
            work.capacity(),
            seen_capacity,
            active_capacity,
        ],
    )?;
    let mut seen = Vec::new();
    seen.try_reserve_exact(seen_capacity)
        .map_err(|_| allocation_error(work_plan))?;
    let seen_plan = validate_retained_plan(
        limits,
        root,
        root_offset,
        [
            items.capacity(),
            work.capacity(),
            seen.capacity(),
            active_capacity,
        ],
    )?;
    seen.resize(seen_capacity, 0_u64);
    let mut active = Vec::new();
    active
        .try_reserve_exact(active_capacity)
        .map_err(|_| allocation_error(seen_plan))?;
    let reserved_total = validate_retained_plan(
        limits,
        root,
        root_offset,
        [
            items.capacity(),
            work.capacity(),
            seen.capacity(),
            active.capacity(),
        ],
    )?;

    let reserved_working = working_bytes(work.capacity(), seen.capacity(), active.capacity())
        .ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(root_offset),
            )
        })?;
    let reserved_result = result_bytes(items.capacity()).ok_or_else(|| {
        DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(root),
            Some(root_offset),
        )
    })?;
    Ok(ReadOutlineJob {
        index,
        snapshot,
        context,
        limits,
        catalog: None,
        root: None,
        root_count: None,
        root_count_offset: None,
        work,
        seen_slots: seen,
        seen_count: 0,
        active_items: active,
        items,
        current: None,
        child: None,
        stats: OutlineStats {
            reserved_working_bytes: reserved_working,
            reserved_result_bytes: reserved_result,
            peak_retained_bytes: reserved_total,
            ..OutlineStats::default()
        },
        visible_items: 0,
        state: JobState::Active,
        terminal_error: DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(root),
            Some(root_offset),
        ),
    })
}

#[derive(Clone, Copy)]
struct LocatedReference {
    reference: ObjectRef,
    offset: u64,
}

fn outline_reference_pair(
    first: Option<&Located<SyntaxObject>>,
    last: Option<&Located<SyntaxObject>>,
    owner: ObjectRef,
    owner_offset: u64,
    invalid_code: DocumentErrorCode,
) -> Result<Option<(LocatedReference, LocatedReference)>, DocumentError> {
    match (first, last) {
        (None, None) => Ok(None),
        (Some(first), Some(last)) => {
            let first_reference = first.value().as_reference().ok_or_else(|| {
                DocumentError::for_code(invalid_code, Some(owner), Some(first.span().start()))
            })?;
            let last_reference = last.value().as_reference().ok_or_else(|| {
                DocumentError::for_code(invalid_code, Some(owner), Some(last.span().start()))
            })?;
            Ok(Some((
                LocatedReference {
                    reference: first_reference,
                    offset: first.span().start(),
                },
                LocatedReference {
                    reference: last_reference,
                    offset: last.span().start(),
                },
            )))
        }
        (Some(value), None) | (None, Some(value)) => Err(DocumentError::for_code(
            invalid_code,
            Some(owner),
            Some(value.span().start().max(owner_offset)),
        )),
    }
}

fn validate_prev(
    reference: ObjectRef,
    expected: Option<ObjectRef>,
    observed: Option<&Located<SyntaxObject>>,
    object_offset: u64,
) -> Result<(), DocumentError> {
    match (expected, observed) {
        (None, None) => Ok(()),
        (Some(expected), Some(observed)) if observed.value().as_reference() == Some(expected) => {
            Ok(())
        }
        (_, Some(observed)) => Err(DocumentError::for_code(
            DocumentErrorCode::OutlineSiblingMismatch,
            Some(reference),
            Some(observed.span().start()),
        )),
        (_, None) => Err(DocumentError::for_code(
            DocumentErrorCode::OutlineSiblingMismatch,
            Some(reference),
            Some(object_offset),
        )),
    }
}

fn validate_target(
    reference: ObjectRef,
    destination: Option<&Located<SyntaxObject>>,
    action: Option<&Located<SyntaxObject>>,
) -> Result<OutlineTargetKind, DocumentError> {
    if destination.is_some() && action.is_some() {
        let offset = action
            .map(|value| value.span().start())
            .or_else(|| destination.map(|value| value.span().start()));
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidOutlineTarget,
            Some(reference),
            offset,
        ));
    }
    if let Some(value) = destination {
        return match value.value() {
            SyntaxObject::Array(_) | SyntaxObject::Name(_) | SyntaxObject::String(_) => {
                Ok(OutlineTargetKind::Destination)
            }
            SyntaxObject::Reference(_) => Err(DocumentError::for_code(
                DocumentErrorCode::UnsupportedOutlineRepresentation,
                Some(reference),
                Some(value.span().start()),
            )),
            _ => Err(DocumentError::for_code(
                DocumentErrorCode::InvalidOutlineTarget,
                Some(reference),
                Some(value.span().start()),
            )),
        };
    }
    if let Some(value) = action {
        return match value.value() {
            SyntaxObject::Dictionary(_) => Ok(OutlineTargetKind::Action),
            SyntaxObject::Reference(_) => Err(DocumentError::for_code(
                DocumentErrorCode::UnsupportedOutlineRepresentation,
                Some(reference),
                Some(value.span().start()),
            )),
            _ => Err(DocumentError::for_code(
                DocumentErrorCode::InvalidOutlineTarget,
                Some(reference),
                Some(value.span().start()),
            )),
        };
    }
    Ok(OutlineTargetKind::None)
}

fn working_bytes(work: usize, seen: usize, active: usize) -> Option<u64> {
    let work = capacity_bytes::<WorkItem>(work)?;
    let seen = capacity_bytes::<u64>(seen)?;
    let active = capacity_bytes::<ObjectRef>(active)?;
    work.checked_add(seen)?.checked_add(active)
}

fn result_bytes(items: usize) -> Option<u64> {
    capacity_bytes::<OutlineItem>(items)
}

fn retained_bytes(capacities: [usize; 4]) -> Option<u64> {
    let [items, work, seen, active] = capacities;
    result_bytes(items)?.checked_add(working_bytes(work, seen, active)?)
}

fn validate_retained_plan(
    limits: OutlineLimits,
    root: ObjectRef,
    root_offset: u64,
    capacities: [usize; 4],
) -> Result<u64, DocumentError> {
    let attempted = retained_bytes(capacities).ok_or_else(|| {
        DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(root),
            Some(root_offset),
        )
    })?;
    if attempted > limits.max_retained_bytes() {
        return Err(DocumentError::outline_resource(
            DocumentLimitKind::OutlineRetainedBytes,
            limits.max_retained_bytes(),
            0,
            attempted,
            root,
            Some(root_offset),
        ));
    }
    Ok(attempted)
}

fn capacity_bytes<T>(capacity: usize) -> Option<u64> {
    u64::try_from(capacity)
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<T>()).ok()?)
}

fn encode_reference(reference: ObjectRef) -> u64 {
    (u64::from(reference.number()) << 16) | u64::from(reference.generation())
}

fn reference_slot(key: u64, mask: usize) -> usize {
    usize::try_from(mix_reference(key))
        .map(|value| value & mask)
        .unwrap_or_else(|_| {
            let folded = mix_reference(key) ^ (mix_reference(key) >> 32);
            usize::try_from(folded & u64::try_from(mask).unwrap_or(u64::MAX)).unwrap_or(0)
        })
}

fn mix_reference(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
