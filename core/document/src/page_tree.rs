use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteSource, DataTicket, JobId, RequestPriority, ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_object::{ObjectLimitKind, ObjectStats, ObjectWorkCaps};
use pdf_rs_syntax::{Located, ObjectRef, SyntaxObject};

use crate::catalog::parse_strict_catalog;
use crate::dictionary::{
    StructuralFields, collect_structural_fields, direct_dictionary, optional_field,
    reject_duplicate_field, required_field,
};
use crate::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, AttestedRevisionIndex,
    DocumentCancellation, DocumentError, DocumentErrorCode, DocumentLimitKind,
    OpenAttestedObjectJob, PageTreeLimits, StrictCatalog,
};

const CANCELLATION_PROBE_INTERVAL: usize = 256;

/// Runtime identity, lower object checkpoints, and priority for one page-count job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageTreeJobContext {
    job: JobId,
    object_envelope_checkpoint: ResumeCheckpoint,
    object_boundary_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl PageTreeJobContext {
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

/// Public phase of one strict Catalog and page-tree count job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageTreePhase {
    /// The trailer root is being reopened and validated as a strict Catalog.
    Catalog,
    /// The Catalog is valid and its Page/Pages descendants are being traversed.
    Traversing,
    /// Every reachable node and declared subtree count was validated.
    Ready,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Deterministic traversal work and active-capacity accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageTreeStats {
    objects_started: u64,
    nodes_started: u64,
    pages: u64,
    max_depth: u64,
    max_kids_per_node: u64,
    object_read_bytes: u64,
    object_parse_bytes: u64,
    reserved_traversal_bytes: u64,
}

impl PageTreeStats {
    /// Returns proof-preserving child object jobs successfully started, including the Catalog.
    pub const fn objects_started(self) -> u64 {
        self.objects_started
    }

    /// Returns Page or Pages object jobs successfully started.
    pub const fn nodes_started(self) -> u64 {
        self.nodes_started
    }

    /// Returns validated leaf Page objects through the latest poll.
    pub const fn pages(self) -> u64 {
        self.pages
    }

    /// Returns the greatest root-relative Page/Pages depth started.
    pub const fn max_depth(self) -> u64 {
        self.max_depth
    }

    /// Returns the greatest direct Kids count observed on one valid Pages node.
    pub const fn max_kids_per_node(self) -> u64 {
        self.max_kids_per_node
    }

    /// Returns cumulative exact-read bytes charged by child object jobs.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }

    /// Returns cumulative parser-window bytes charged by child object jobs.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }

    /// Returns allocator-reported traversal capacity reserved while the job was active.
    ///
    /// This is historical accounting. The work stack, seen table, and active path are
    /// released before a terminal poll result is returned.
    pub const fn reserved_traversal_bytes(self) -> u64 {
        self.reserved_traversal_bytes
    }
}

/// Complete successful strict page-count result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageCount {
    catalog: StrictCatalog,
    page_count: u64,
    stats: PageTreeStats,
}

impl PageCount {
    /// Returns the validated source- and revision-bound Catalog summary.
    pub const fn catalog(self) -> StrictCatalog {
        self.catalog
    }

    /// Returns the number of validated leaf Page objects.
    pub const fn page_count(self) -> u64 {
        self.page_count
    }

    /// Returns deterministic work and traversal-capacity accounting.
    pub const fn stats(self) -> PageTreeStats {
        self.stats
    }
}

/// Result of polling one strict Catalog and page-tree count job.
pub enum PageCountPoll {
    /// The complete reachable page tree was validated and counted.
    Ready(PageCount),
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

impl fmt::Debug for PageCountPoll {
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
struct VisitNode {
    reference: ObjectRef,
    parent: Option<ObjectRef>,
    depth: u64,
    edge_offset: u64,
}

#[derive(Clone, Copy)]
enum WorkItem {
    Visit(VisitNode),
    Finish {
        reference: ObjectRef,
        declared_pages: u64,
        pages_before: u64,
        count_offset: u64,
    },
}

#[derive(Clone, Copy)]
enum CurrentTarget {
    Catalog,
    Node(VisitNode),
}

struct ChildState {
    job: OpenAttestedObjectJob,
    accounted_stats: ObjectStats,
    work_caps: ObjectWorkCaps,
    reference: ObjectRef,
    offset: u64,
}

/// One-shot job that validates the strict Catalog and counts its complete page tree.
///
/// The job performs no file, network, callback, or async-runtime I/O. It reopens
/// only objects covered by one [`AttestedRevisionIndex`], suspends on exact byte
/// requests, and never trusts `/Count` or `/Kids` to allocate. Traversal storage
/// is reserved from validated limits before the job is published.
pub struct CountPagesJob<'index> {
    index: &'index AttestedRevisionIndex,
    context: PageTreeJobContext,
    limits: PageTreeLimits,
    catalog: Option<StrictCatalog>,
    work: Vec<WorkItem>,
    seen_slots: Vec<u64>,
    seen_count: u64,
    active_pages_nodes: Vec<ObjectRef>,
    current: Option<CurrentTarget>,
    child: Option<ChildState>,
    stats: PageTreeStats,
    state: JobState,
    terminal_error: DocumentError,
}

impl CountPagesJob<'_> {
    /// Returns the immutable source snapshot covered by the owning attested index.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.index.snapshot()
    }

    /// Returns runtime identity, child checkpoints, and scheduling priority.
    pub const fn context(&self) -> PageTreeJobContext {
        self.context
    }

    /// Returns the complete validated page-tree limits.
    pub const fn limits(&self) -> PageTreeLimits {
        self.limits
    }

    /// Returns deterministic work and active-capacity accounting through the latest poll.
    pub const fn stats(&self) -> PageTreeStats {
        self.stats
    }

    /// Returns the public job phase.
    pub const fn phase(&self) -> PageTreePhase {
        match self.state {
            JobState::Ready => PageTreePhase::Ready,
            JobState::Failed => PageTreePhase::Failed,
            JobState::Active if self.catalog.is_some() => PageTreePhase::Traversing,
            JobState::Active => PageTreePhase::Catalog,
        }
    }

    /// Advances the job without performing host I/O or resuming inside a callback.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> PageCountPoll {
        if !matches!(self.state, JobState::Active) {
            return PageCountPoll::Failed(self.terminal_error);
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
                            Some(WorkItem::Visit(visit)) => {
                                self.current = Some(CurrentTarget::Node(visit));
                            }
                            Some(WorkItem::Finish {
                                reference,
                                declared_pages,
                                pages_before,
                                count_offset,
                            }) => {
                                if let Err(error) = self.finish_pages_node(
                                    reference,
                                    declared_pages,
                                    pages_before,
                                    count_offset,
                                ) {
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
                    return PageCountPoll::Pending {
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
                        CurrentTarget::Node(visit) => {
                            self.accept_page_tree_node(visit, object, cancellation)
                        }
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
        let (reference, node_depth) = match target {
            CurrentTarget::Catalog => (self.index.root(), None),
            CurrentTarget::Node(visit) => (visit.reference, Some(visit.depth)),
        };
        let attestation = self.index.attestation(reference)?;
        let offset = attestation.xref_offset();

        if let Some(depth) = node_depth {
            if self.stats.nodes_started >= self.limits.max_nodes() {
                return Err(DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeNodes,
                    self.limits.max_nodes(),
                    self.stats.nodes_started,
                    1,
                    reference,
                    Some(offset),
                ));
            }
            if depth > self.limits.max_depth() {
                return Err(DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeDepth,
                    self.limits.max_depth(),
                    depth.saturating_sub(1),
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
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeObjectReadBytes,
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
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeObjectParseBytes,
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
        if let Some(depth) = node_depth {
            self.stats.nodes_started =
                self.stats.nodes_started.checked_add(1).ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(offset),
                    )
                })?;
            self.stats.max_depth = self.stats.max_depth.max(depth);
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
                        DocumentLimitKind::PageTreeObjectReadBytes,
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
                        DocumentLimitKind::PageTreeObjectParseBytes,
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
                | ObjectLimitKind::TotalParseBytes => {}
            }
        }
        error
    }

    fn accept_catalog(
        &mut self,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let parsed = parse_strict_catalog(self.index, &object, cancellation)?;
        let catalog = parsed.summary();
        let pages_entry = parsed.pages_entry();
        let pages = pages_entry.reference();
        self.catalog = Some(catalog);
        if !self.insert_seen(pages, cancellation)? {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(pages),
                Some(pages_entry.value_offset()),
            ));
        }
        self.push_work(WorkItem::Visit(VisitNode {
            reference: pages,
            parent: None,
            depth: 1,
            edge_offset: pages_entry.value_offset(),
        }))?;
        Ok(())
    }

    fn accept_page_tree_node(
        &mut self,
        visit: VisitNode,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let reference = object.reference();
        let offset = object.attestation().xref_offset();
        if reference != visit.reference {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(offset),
            ));
        }
        let dictionary = direct_dictionary(
            &object,
            self.index.snapshot(),
            DocumentErrorCode::InvalidPageTreeNode,
        )?;
        let fields = collect_structural_fields(
            dictionary,
            [
                b"Type".as_slice(),
                b"Parent".as_slice(),
                b"Kids".as_slice(),
                b"Count".as_slice(),
            ],
            reference,
            cancellation,
        )?;
        reject_duplicate_field(&fields, 0, reference)?;
        let type_value = required_field(
            &fields,
            0,
            reference,
            offset,
            DocumentErrorCode::InvalidPageTreeNode,
        )?;
        let is_page = match type_value.value() {
            SyntaxObject::Name(name) if name.bytes() == b"Page" => true,
            SyntaxObject::Name(name) if name.bytes() == b"Pages" => false,
            _ => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageTreeNode,
                    Some(reference),
                    Some(type_value.span().start()),
                ));
            }
        };
        reject_duplicate_field(&fields, 1, reference)?;
        let parent_value = optional_field(&fields, 1);
        validate_parent(visit, parent_value, offset)?;

        if is_page {
            if visit.parent.is_none() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageTreeNode,
                    Some(reference),
                    Some(type_value.span().start()),
                ));
            }
            if self.stats.pages >= self.limits.max_pages() {
                return Err(DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreePages,
                    self.limits.max_pages(),
                    self.stats.pages,
                    1,
                    reference,
                    Some(offset),
                ));
            }
            self.stats.pages = self.stats.pages.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
            Ok(())
        } else {
            reject_duplicate_field(&fields, 2, reference)?;
            reject_duplicate_field(&fields, 3, reference)?;
            self.accept_pages_node(visit, &fields, cancellation)
        }
    }

    fn accept_pages_node(
        &mut self,
        visit: VisitNode,
        fields: &StructuralFields<'_, 4>,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let offset = self
            .index
            .attestation(visit.reference)
            .map(crate::ObjectAttestation::xref_offset)
            .unwrap_or(visit.edge_offset);
        let kids_value = required_field(
            fields,
            2,
            visit.reference,
            offset,
            DocumentErrorCode::InvalidPageTreeNode,
        )?;
        let SyntaxObject::Array(kids) = kids_value.value() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeNode,
                Some(visit.reference),
                Some(kids_value.span().start()),
            ));
        };
        let count_value = required_field(
            fields,
            3,
            visit.reference,
            offset,
            DocumentErrorCode::InvalidPageTreeNode,
        )?;
        let Some(declared_pages) = count_value
            .value()
            .as_integer()
            .and_then(|value| u64::try_from(value).ok())
        else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeNode,
                Some(visit.reference),
                Some(count_value.span().start()),
            ));
        };

        let kids_count = u64::try_from(kids.values().len()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(visit.reference),
                Some(kids_value.span().start()),
            )
        })?;
        if kids_count > self.limits.max_kids_per_node() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeKids,
                self.limits.max_kids_per_node(),
                0,
                kids_count,
                visit.reference,
                Some(kids_value.span().start()),
            ));
        }
        let scheduled = self.seen_count.checked_add(kids_count).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(visit.reference),
                Some(kids_value.span().start()),
            )
        })?;
        if scheduled > self.limits.max_nodes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeNodes,
                self.limits.max_nodes(),
                self.seen_count,
                kids_count,
                visit.reference,
                Some(kids_value.span().start()),
            ));
        }
        let child_depth = visit.depth.checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(visit.reference),
                Some(offset),
            )
        })?;
        if kids_count > 0 && child_depth > self.limits.max_depth() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeDepth,
                self.limits.max_depth(),
                visit.depth,
                1,
                visit.reference,
                Some(kids_value.span().start()),
            ));
        }

        let additional_work = kids.values().len().checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(visit.reference),
                Some(kids_value.span().start()),
            )
        })?;
        if self
            .work
            .len()
            .checked_add(additional_work)
            .is_none_or(|needed| needed > self.work.capacity())
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(visit.reference),
                Some(kids_value.span().start()),
            ));
        }
        if self.active_pages_nodes.len() >= self.active_pages_nodes.capacity() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(visit.reference),
                Some(offset),
            ));
        }
        self.active_pages_nodes.push(visit.reference);

        for (index, child) in kids.values().iter().enumerate() {
            if index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(visit.reference),
                    Some(child.span().start()),
                ));
            }
            let Some(reference) = child.value().as_reference() else {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageTreeNode,
                    Some(visit.reference),
                    Some(child.span().start()),
                ));
            };
            if self.active_contains(reference, cancellation)? {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::PageTreeCycle,
                    Some(reference),
                    Some(child.span().start()),
                ));
            }
            if !self.insert_seen(reference, cancellation)? {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::DuplicatePageTreeNode,
                    Some(reference),
                    Some(child.span().start()),
                ));
            }
        }

        self.stats.max_kids_per_node = self.stats.max_kids_per_node.max(kids_count);
        self.push_work(WorkItem::Finish {
            reference: visit.reference,
            declared_pages,
            pages_before: self.stats.pages,
            count_offset: count_value.span().start(),
        })?;
        for child in kids.values().iter().rev() {
            let reference = child
                .value()
                .as_reference()
                .expect("validated Kids entries remain exact references");
            self.push_work(WorkItem::Visit(VisitNode {
                reference,
                parent: Some(visit.reference),
                depth: child_depth,
                edge_offset: child.span().start(),
            }))?;
        }
        Ok(())
    }

    fn finish_pages_node(
        &mut self,
        reference: ObjectRef,
        declared_pages: u64,
        pages_before: u64,
        count_offset: u64,
    ) -> Result<(), DocumentError> {
        let actual = self.stats.pages.checked_sub(pages_before).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(count_offset),
            )
        })?;
        let Some(active) = self.active_pages_nodes.pop() else {
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
        if actual != declared_pages {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageTreeCountMismatch,
                Some(reference),
                Some(count_offset),
            ));
        }
        Ok(())
    }

    fn active_contains(
        &self,
        reference: ObjectRef,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<bool, DocumentError> {
        for (index, active) in self.active_pages_nodes.iter().enumerate() {
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
        let mut slot = usize::try_from(mix_reference(key))
            .map(|value| value & mask)
            .unwrap_or_else(|_| {
                let folded = mix_reference(key) ^ (mix_reference(key) >> 32);
                usize::try_from(folded & u64::try_from(mask).unwrap_or(u64::MAX)).unwrap_or(0)
            });
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

    fn finish_ready(&mut self) -> PageCountPoll {
        if !self.active_pages_nodes.is_empty() || self.current.is_some() || self.child.is_some() {
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
        let page_count = self.stats.pages;
        let stats = self.stats;
        self.release_traversal();
        self.state = JobState::Ready;
        self.terminal_error = DocumentError::for_code(
            DocumentErrorCode::JobAlreadyComplete,
            Some(self.index.root()),
            self.root_offset(),
        );
        PageCountPoll::Ready(PageCount {
            catalog,
            page_count,
            stats,
        })
    }

    fn fail(&mut self, error: DocumentError) -> PageCountPoll {
        self.child = None;
        self.current = None;
        self.catalog = None;
        self.release_traversal();
        self.state = JobState::Failed;
        self.terminal_error = error;
        PageCountPoll::Failed(error)
    }

    fn release_traversal(&mut self) {
        self.work = Vec::new();
        self.seen_slots = Vec::new();
        self.seen_count = 0;
        self.active_pages_nodes = Vec::new();
    }

    fn current_reference(&self) -> Option<ObjectRef> {
        match self.current {
            Some(CurrentTarget::Catalog) => Some(self.index.root()),
            Some(CurrentTarget::Node(visit)) => Some(visit.reference),
            None => None,
        }
    }

    fn current_offset(&self) -> Option<u64> {
        self.current_reference()
            .and_then(|reference| self.index.attestation(reference).ok())
            .map(crate::ObjectAttestation::xref_offset)
    }

    fn root_offset(&self) -> Option<u64> {
        self.index
            .attestation(self.index.root())
            .ok()
            .map(crate::ObjectAttestation::xref_offset)
    }
}

impl fmt::Debug for CountPagesJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CountPagesJob")
            .field("snapshot", &self.index.snapshot())
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .field("current", &"[REDACTED]")
            .field("work", &"[REDACTED]")
            .field("seen", &"[REDACTED]")
            .field("active_path", &"[REDACTED]")
            .field("child", &"[REDACTED]")
            .finish()
    }
}

impl AttestedRevisionIndex {
    /// Creates a bounded one-shot job that validates the Catalog and counts the page tree.
    pub fn count_pages(
        &self,
        context: PageTreeJobContext,
        limits: PageTreeLimits,
    ) -> Result<CountPagesJob<'_>, DocumentError> {
        let root = self.root();
        let root_offset = self.attestation(root)?.xref_offset();
        if context.object_envelope_checkpoint == context.object_boundary_checkpoint {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeJobContext,
                Some(root),
                Some(root_offset),
            ));
        }

        let work_items = usize::try_from(limits.effective_work_items()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(root_offset),
            )
        })?;
        let seen_slots = limits
            .effective_seen_references()
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
        let active_depth = usize::try_from(limits.max_depth()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(root_offset),
            )
        })?;
        let requested_bytes =
            traversal_bytes(work_items, seen_slots, active_depth).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(root),
                    Some(root_offset),
                )
            })?;
        if requested_bytes > limits.max_retained_traversal_bytes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                0,
                requested_bytes,
                root,
                Some(root_offset),
            ));
        }

        let mut work = Vec::new();
        work.try_reserve_exact(work_items).map_err(|_| {
            DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                0,
                requested_bytes,
                root,
                Some(root_offset),
            )
        })?;
        let mut seen = Vec::new();
        seen.try_reserve_exact(seen_slots).map_err(|_| {
            DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                0,
                requested_bytes,
                root,
                Some(root_offset),
            )
        })?;
        seen.resize(seen_slots, 0_u64);
        let mut active = Vec::new();
        active.try_reserve_exact(active_depth).map_err(|_| {
            DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                0,
                requested_bytes,
                root,
                Some(root_offset),
            )
        })?;

        let reserved_bytes = traversal_bytes(work.capacity(), seen.capacity(), active.capacity())
            .ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(root_offset),
            )
        })?;
        if reserved_bytes > limits.max_retained_traversal_bytes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                0,
                reserved_bytes,
                root,
                Some(root_offset),
            ));
        }

        Ok(CountPagesJob {
            index: self,
            context,
            limits,
            catalog: None,
            work,
            seen_slots: seen,
            seen_count: 0,
            active_pages_nodes: active,
            current: None,
            child: None,
            stats: PageTreeStats {
                reserved_traversal_bytes: reserved_bytes,
                ..PageTreeStats::default()
            },
            state: JobState::Active,
            terminal_error: DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(root_offset),
            ),
        })
    }
}

fn traversal_bytes(work: usize, seen: usize, active: usize) -> Option<u64> {
    let work = u64::try_from(work)
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<WorkItem>()).ok()?)?;
    let seen = u64::try_from(seen)
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<u64>()).ok()?)?;
    let active = u64::try_from(active)
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<ObjectRef>()).ok()?)?;
    work.checked_add(seen)?.checked_add(active)
}

fn encode_reference(reference: ObjectRef) -> u64 {
    (u64::from(reference.number()) << 16) | u64::from(reference.generation())
}

fn mix_reference(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn validate_parent(
    visit: VisitNode,
    parent: Option<&Located<SyntaxObject>>,
    object_offset: u64,
) -> Result<(), DocumentError> {
    match (visit.parent, parent) {
        (None, None) => Ok(()),
        (Some(expected), Some(observed)) if observed.value().as_reference() == Some(expected) => {
            Ok(())
        }
        (_, Some(observed)) => Err(DocumentError::for_code(
            DocumentErrorCode::PageTreeParentMismatch,
            Some(visit.reference),
            Some(observed.span().start()),
        )),
        (_, None) => Err(DocumentError::for_code(
            DocumentErrorCode::PageTreeParentMismatch,
            Some(visit.reference),
            Some(object_offset),
        )),
    }
}
