use std::fmt;
use std::mem;

use pdf_rs_bytes::{ByteSource, DataTicket, ResumeCheckpoint, SmallRanges, SourceSnapshot};
use pdf_rs_object::{ObjectLimitKind, ObjectStats, ObjectWorkCaps};
use pdf_rs_syntax::{Located, ObjectRef, SyntaxObject};

use crate::catalog::parse_strict_catalog;
use crate::dictionary::{
    StructuralFields, collect_structural_fields, direct_dictionary, optional_field,
    reject_duplicate_field, required_field,
};
use crate::model::AttestedRevisionIndexOwner;
use crate::page_index::{PageIndexChild, PageIndexNodeEvidence};
use crate::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, AttestedRevisionIndex,
    DocumentCancellation, DocumentError, DocumentErrorCode, DocumentLimitKind,
    LocallyRepairedRevisionIndex, OpenAttestedObjectJob, PageHandle, PageIndex, PageIndexLimits,
    PageIndexSegmentKind, PageIndexStats, PageSegmentEvidence, PageSegmentSummary,
    PageTreeJobContext, PageTreeLimits, PageTreePhase, SharedAttestedRevisionIndex,
    SharedLocallyRepairedRevisionIndex, StrictCatalog,
};

const CANCELLATION_PROBE_INTERVAL: usize = 256;

/// Result of polling one page-index construction job.
pub enum PageIndexBuildPoll {
    /// The Catalog and root Pages proof were admitted as an immutable lazy index.
    Ready(PageIndex),
    /// The active Catalog or root Pages child requires exact source ranges.
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

impl fmt::Debug for PageIndexBuildPoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(index) => formatter.debug_tuple("Ready").field(index).finish(),
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
enum BuildState {
    Active,
    Ready,
    Failed,
}

#[derive(Clone, Copy)]
enum BuildTarget {
    Catalog,
    RootPages {
        reference: ObjectRef,
        edge_offset: u64,
    },
}

struct BuildChildState {
    job: OpenAttestedObjectJob,
    accounted_stats: ObjectStats,
    work_caps: ObjectWorkCaps,
    reference: ObjectRef,
    offset: u64,
}

struct ParsedRootPages {
    page_count: u32,
    count_offset: u64,
    children: Vec<PageIndexChild>,
    nodes: Vec<PageIndexNodeEvidence>,
    retained_bytes: u64,
}

/// One-shot cold bootstrap for an immutable lazy page index.
///
/// Construction opens only the strict Catalog and its root Pages dictionary. It validates the
/// root shape, direct Kids identities, declared Count, immediate cycle/duplicate topology, and
/// configured budgets, then publishes those direct edges as provisional range evidence. No
/// descendant Page or Pages object is opened until a lookup requests its range.
pub struct BuildPageIndexJob<'index> {
    authority: AttestedRevisionIndexOwner<'index>,
    snapshot: SourceSnapshot,
    context: PageTreeJobContext,
    stats: PageIndexStats,
    tree_limits: PageTreeLimits,
    limits: PageIndexLimits,
    catalog: Option<StrictCatalog>,
    current: Option<BuildTarget>,
    child: Option<BuildChildState>,
    root: ObjectRef,
    root_offset: Option<u64>,
    state: BuildState,
    terminal_error: DocumentError,
}

impl BuildPageIndexJob<'_> {
    /// Returns the immutable source snapshot covered by the cold bootstrap.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the runtime context copied to each proof-preserving object child.
    pub const fn context(&self) -> PageTreeJobContext {
        self.context
    }

    /// Returns the immutable index-admission limits.
    pub const fn index_limits(&self) -> PageIndexLimits {
        self.limits
    }

    /// Returns the structural limits enforced by construction and later refinements.
    pub const fn tree_limits(&self) -> PageTreeLimits {
        self.tree_limits
    }

    /// Returns deterministic cold-construction accounting through the latest poll.
    pub const fn stats(&self) -> PageIndexStats {
        self.stats
    }

    /// Returns the public page-tree phase projection.
    pub const fn phase(&self) -> PageTreePhase {
        match self.state {
            BuildState::Ready => PageTreePhase::Ready,
            BuildState::Failed => PageTreePhase::Failed,
            BuildState::Active if self.catalog.is_some() => PageTreePhase::Traversing,
            BuildState::Active => PageTreePhase::Catalog,
        }
    }

    /// Advances cold Catalog/root bootstrap without opening descendant page-tree nodes.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> PageIndexBuildPoll {
        if !matches!(self.state, BuildState::Active) {
            return PageIndexBuildPoll::Failed(self.terminal_error);
        }

        loop {
            if source.snapshot() != self.snapshot {
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
                        self.current = Some(BuildTarget::Catalog);
                    } else {
                        return self.fail(DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(self.root),
                            self.root_offset,
                        ));
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
                    return PageIndexBuildPoll::Pending {
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
                    if source.snapshot() != self.snapshot {
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
                    match target {
                        BuildTarget::Catalog => {
                            if let Err(error) = self.accept_catalog(object, cancellation) {
                                return self.fail(error);
                            }
                        }
                        BuildTarget::RootPages {
                            reference,
                            edge_offset,
                        } => match self.accept_root_pages(
                            reference,
                            edge_offset,
                            object,
                            cancellation,
                        ) {
                            Ok(index) => return self.finish_ready(index),
                            Err(error) => return self.fail(error),
                        },
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
            BuildTarget::Catalog => (self.root, None),
            BuildTarget::RootPages { reference, .. } => (reference, Some(1_u64)),
        };
        let attested = self.authority.as_attested();
        let attestation = attested.attestation(reference)?;
        let offset = attestation.xref_offset();
        if let Some(depth) = node_depth {
            if self.stats.nodes_started >= self.tree_limits.max_nodes() {
                return Err(DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeNodes,
                    self.tree_limits.max_nodes(),
                    self.stats.nodes_started,
                    1,
                    reference,
                    Some(offset),
                ));
            }
            if depth > self.tree_limits.max_depth() {
                return Err(DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeDepth,
                    self.tree_limits.max_depth(),
                    depth.saturating_sub(1),
                    1,
                    reference,
                    Some(offset),
                ));
            }
        }
        let read_remaining = self
            .tree_limits
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
                self.tree_limits.max_total_object_read_bytes(),
                self.stats.object_read_bytes,
                1,
                reference,
                Some(offset),
            ));
        }
        let parse_remaining = self
            .tree_limits
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
                self.tree_limits.max_total_object_parse_bytes(),
                self.stats.object_parse_bytes,
                1,
                reference,
                Some(offset),
            ));
        }
        let work_caps = ObjectWorkCaps::new(
            read_remaining.min(attested.object_limits().max_total_read_bytes()),
            parse_remaining.min(attested.object_limits().max_total_parse_bytes()),
        )
        .map_err(|error| DocumentError::from_object_access_constructor(error, reference, offset))?;
        let context = AttestedObjectJobContext::new(
            self.context.job(),
            self.context.object_envelope_checkpoint(),
            self.context.object_boundary_checkpoint(),
            self.context.priority(),
        );
        let job = attested.open_object(reference, context, work_caps)?;
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
        self.child = Some(BuildChildState {
            job,
            accounted_stats: ObjectStats::default(),
            work_caps,
            reference,
            offset,
        });
        Ok(())
    }

    fn account_child_stats(&mut self, child: &mut BuildChildState) -> Result<(), DocumentError> {
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
            .filter(|value| *value <= self.tree_limits.max_total_object_read_bytes())
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
            .filter(|value| *value <= self.tree_limits.max_total_object_parse_bytes())
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

    fn map_child_error(&self, error: DocumentError, child: &BuildChildState) -> DocumentError {
        if error.code() == DocumentErrorCode::ResourceLimit
            && let Some(lower) = error.object_error()
            && let Some(limit) = lower.limit()
        {
            match limit.kind() {
                ObjectLimitKind::TotalReadBytes
                    if child.work_caps.max_read_bytes()
                        < self
                            .authority
                            .as_attested()
                            .object_limits()
                            .max_total_read_bytes() =>
                {
                    return DocumentError::aggregate_object_resource(
                        DocumentLimitKind::PageTreeObjectReadBytes,
                        self.tree_limits.max_total_object_read_bytes(),
                        self.stats.object_read_bytes,
                        limit.attempted(),
                        lower,
                        child.reference,
                        child.offset,
                    );
                }
                ObjectLimitKind::TotalParseBytes
                    if child.work_caps.max_parse_bytes()
                        < self
                            .authority
                            .as_attested()
                            .object_limits()
                            .max_total_parse_bytes() =>
                {
                    return DocumentError::aggregate_object_resource(
                        DocumentLimitKind::PageTreeObjectParseBytes,
                        self.tree_limits.max_total_object_parse_bytes(),
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
        let parsed = parse_strict_catalog(self.authority.as_attested(), &object, cancellation)?;
        let catalog = parsed.summary();
        let pages = parsed.pages_entry();
        self.catalog = Some(catalog);
        self.current = Some(BuildTarget::RootPages {
            reference: pages.reference(),
            edge_offset: pages.value_offset(),
        });
        Ok(())
    }

    fn accept_root_pages(
        &mut self,
        reference: ObjectRef,
        edge_offset: u64,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<PageIndex, DocumentError> {
        if object.reference() != reference {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(object.reference()),
                Some(object.attestation().xref_offset()),
            ));
        }
        let catalog = self.catalog.ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(edge_offset),
            )
        })?;
        let offset = object.attestation().xref_offset();
        let dictionary = direct_dictionary(
            &object,
            self.snapshot,
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
        if !matches!(
            type_value.value(),
            SyntaxObject::Name(name) if name.bytes() == b"Pages"
        ) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeNode,
                Some(reference),
                Some(type_value.span().start()),
            ));
        }
        reject_duplicate_field(&fields, 1, reference)?;
        validate_parent(reference, None, optional_field(&fields, 1), offset)?;
        reject_duplicate_field(&fields, 2, reference)?;
        reject_duplicate_field(&fields, 3, reference)?;
        let parsed =
            self.parse_root_pages_fields(reference, edge_offset, offset, &fields, cancellation)?;
        self.stats.max_kids_per_node = self
            .stats
            .max_kids_per_node
            .max(u64::try_from(parsed.children.len()).unwrap_or(u64::MAX));
        self.stats.peak_retained_traversal_bytes = self
            .stats
            .peak_retained_traversal_bytes
            .max(parsed.retained_bytes);
        self.stats.complete_tree_proof = false;
        PageIndex::from_lazy_root(
            catalog,
            parsed.page_count,
            parsed.count_offset,
            parsed.children,
            parsed.nodes,
            self.stats,
            self.tree_limits,
            self.limits,
            Some(edge_offset),
        )
    }

    fn parse_root_pages_fields(
        &self,
        reference: ObjectRef,
        edge_offset: u64,
        object_offset: u64,
        fields: &StructuralFields<'_, 4>,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<ParsedRootPages, DocumentError> {
        let kids_value = required_field(
            fields,
            2,
            reference,
            object_offset,
            DocumentErrorCode::InvalidPageTreeNode,
        )?;
        let SyntaxObject::Array(kids) = kids_value.value() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeNode,
                Some(reference),
                Some(kids_value.span().start()),
            ));
        };
        let count_value = required_field(
            fields,
            3,
            reference,
            object_offset,
            DocumentErrorCode::InvalidPageTreeNode,
        )?;
        let Some(page_count) = count_value
            .value()
            .as_integer()
            .and_then(|value| u32::try_from(value).ok())
        else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeNode,
                Some(reference),
                Some(count_value.span().start()),
            ));
        };
        let page_count_u64 = u64::from(page_count);
        let max_pages = self.tree_limits.max_pages().min(self.limits.max_pages());
        if page_count_u64 > max_pages {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreePages,
                max_pages,
                0,
                page_count_u64,
                reference,
                Some(count_value.span().start()),
            ));
        }
        let child_count = kids.values().len();
        let child_count_u64 = u64::try_from(child_count).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(kids_value.span().start()),
            )
        })?;
        if child_count_u64 > self.tree_limits.max_kids_per_node() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeKids,
                self.tree_limits.max_kids_per_node(),
                0,
                child_count_u64,
                reference,
                Some(kids_value.span().start()),
            ));
        }
        let discovered = child_count_u64.checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(kids_value.span().start()),
            )
        })?;
        if discovered > self.tree_limits.max_nodes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeNodes,
                self.tree_limits.max_nodes(),
                1,
                child_count_u64,
                reference,
                Some(kids_value.span().start()),
            ));
        }
        if child_count > 0 && self.tree_limits.max_depth() < 2 {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeDepth,
                self.tree_limits.max_depth(),
                1,
                1,
                reference,
                Some(kids_value.span().start()),
            ));
        }
        let retained_bytes = root_bootstrap_bytes(child_count, child_count.saturating_add(1))
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(kids_value.span().start()),
                )
            })?;
        if retained_bytes > self.tree_limits.max_retained_traversal_bytes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                self.tree_limits.max_retained_traversal_bytes(),
                0,
                retained_bytes,
                reference,
                Some(kids_value.span().start()),
            ));
        }
        let mut children = Vec::new();
        children.try_reserve_exact(child_count).map_err(|_| {
            DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                self.tree_limits.max_retained_traversal_bytes(),
                0,
                retained_bytes,
                reference,
                Some(kids_value.span().start()),
            )
        })?;
        for (index, child) in kids.values().iter().enumerate() {
            if index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(reference),
                    Some(child.span().start()),
                ));
            }
            let Some(child_reference) = child.value().as_reference() else {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageTreeNode,
                    Some(reference),
                    Some(child.span().start()),
                ));
            };
            children.push(PageIndexChild::new(child_reference, child.span().start()));
        }
        for child in &children {
            if child.reference() == reference {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::PageTreeCycle,
                    Some(reference),
                    Some(child.edge_offset()),
                ));
            }
        }
        let mut duplicate_probe = children.clone();
        duplicate_probe.sort_unstable_by_key(|child| child.reference());
        if let Some(duplicate) = duplicate_probe
            .windows(2)
            .find(|pair| pair[0].reference() == pair[1].reference())
            .map(|pair| pair[0].reference())
        {
            let duplicate_offset = children
                .iter()
                .filter(|child| child.reference() == duplicate)
                .nth(1)
                .map(PageIndexChild::edge_offset);
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicatePageTreeNode,
                Some(duplicate),
                duplicate_offset,
            ));
        }
        for child in &children {
            self.authority
                .as_attested()
                .attestation(child.reference())?;
        }
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(child_count.saturating_add(1))
            .map_err(|_| {
                DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeTraversalBytes,
                    self.tree_limits.max_retained_traversal_bytes(),
                    0,
                    retained_bytes,
                    reference,
                    Some(kids_value.span().start()),
                )
            })?;
        nodes.push(PageIndexNodeEvidence::new(reference, None, edge_offset, 1));
        for child in &children {
            nodes.push(PageIndexNodeEvidence::new(
                child.reference(),
                Some(reference),
                child.edge_offset(),
                2,
            ));
        }
        Ok(ParsedRootPages {
            page_count,
            count_offset: count_value.span().start(),
            children,
            nodes,
            retained_bytes,
        })
    }

    fn finish_ready(&mut self, index: PageIndex) -> PageIndexBuildPoll {
        self.child = None;
        self.current = None;
        self.state = BuildState::Ready;
        self.terminal_error = DocumentError::for_code(
            DocumentErrorCode::JobAlreadyComplete,
            Some(self.root),
            self.root_offset,
        );
        PageIndexBuildPoll::Ready(index)
    }

    fn fail(&mut self, error: DocumentError) -> PageIndexBuildPoll {
        self.child = None;
        self.current = None;
        self.catalog = None;
        self.state = BuildState::Failed;
        self.terminal_error = error;
        PageIndexBuildPoll::Failed(error)
    }

    fn current_reference(&self) -> Option<ObjectRef> {
        match self.current {
            Some(BuildTarget::Catalog) => Some(self.root),
            Some(BuildTarget::RootPages { reference, .. }) => Some(reference),
            None => self.catalog.map(StrictCatalog::pages).or(Some(self.root)),
        }
    }

    fn current_offset(&self) -> Option<u64> {
        match self.current {
            Some(BuildTarget::Catalog) => self.root_offset,
            Some(BuildTarget::RootPages { edge_offset, .. }) => Some(edge_offset),
            None => self.root_offset,
        }
    }
}

impl fmt::Debug for BuildPageIndexJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuildPageIndexJob")
            .field("tree_limits", &self.tree_limits)
            .field("index_limits", &self.limits)
            .field("phase", &self.phase())
            .field("current", &"[REDACTED]")
            .field("child", &"[REDACTED]")
            .finish()
    }
}

/// Public phase of one immutable page-index lookup and refinement job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageLookupPhase {
    /// The narrowest cached segment covering the requested index is being selected.
    Selecting,
    /// An unresolved Pages frontier dictionary is being reopened.
    OpeningFrontier,
    /// Direct children of the selected Pages frontier are being classified.
    ClassifyingChildren,
    /// The exact Page identity and refined immutable index were published.
    Ready,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Deterministic object work and frontier-refinement accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageLookupStats {
    objects_started: u64,
    nodes_classified: u64,
    segments_refined: u64,
    max_depth: u64,
    max_kids_per_node: u64,
    object_read_bytes: u64,
    object_parse_bytes: u64,
    peak_retained_traversal_bytes: u64,
}

impl PageLookupStats {
    /// Returns proof-preserving Page or Pages object jobs successfully started.
    pub const fn objects_started(self) -> u64 {
        self.objects_started
    }

    /// Returns Page or Pages dictionaries accepted through the latest poll.
    pub const fn nodes_classified(self) -> u64 {
        self.nodes_classified
    }

    /// Returns Pages frontier segments atomically replaced by direct-child summaries.
    pub const fn segments_refined(self) -> u64 {
        self.segments_refined
    }

    /// Returns the greatest root-relative Page/Pages depth started.
    pub const fn max_depth(self) -> u64 {
        self.max_depth
    }

    /// Returns the greatest direct Kids count observed on one reopened Pages node.
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

    /// Returns peak allocator-reported capacity retained by the active frontier.
    pub const fn peak_retained_traversal_bytes(self) -> u64 {
        self.peak_retained_traversal_bytes
    }
}

/// Successful exact page lookup with the immutable refinement that made it reusable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageLookup {
    page_index: PageIndex,
    handle: PageHandle,
}

impl PageLookup {
    /// Borrows the refined immutable page index.
    pub const fn page_index(&self) -> &PageIndex {
        &self.page_index
    }

    /// Returns the exact source- and revision-bound Page handle.
    pub const fn handle(&self) -> PageHandle {
        self.handle
    }

    /// Consumes the result into its refined index and exact handle.
    pub fn into_parts(self) -> (PageIndex, PageHandle) {
        (self.page_index, self.handle)
    }
}

/// Result of polling one exact page lookup and refinement job.
#[allow(
    clippy::large_enum_variant,
    reason = "the ready value keeps the immutable proof-bound page index inline without an untracked allocation"
)]
pub enum PageLookupPoll {
    /// The requested Page and reusable refined immutable index are ready.
    Ready(PageLookup),
    /// The active Page or Pages object child requires exact source ranges.
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

impl fmt::Debug for PageLookupPoll {
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
enum LookupJobState {
    Active,
    Ready,
    Failed,
}

#[derive(Clone, Copy)]
enum CurrentTarget {
    Frontier {
        reference: ObjectRef,
        depth: u32,
    },
    Child {
        edge: PageIndexChild,
        depth: u32,
        start_index: u32,
    },
}

struct LookupChildState {
    job: OpenAttestedObjectJob,
    accounted_stats: ObjectStats,
    work_caps: ObjectWorkCaps,
    reference: ObjectRef,
    offset: u64,
}

struct FrontierState {
    segment_index: usize,
    segment: PageSegmentSummary,
    count_offset: Option<u64>,
    children_ready: bool,
    children: Vec<PageIndexChild>,
    next_child: usize,
    next_index: u32,
    replacements: Vec<PageSegmentSummary>,
    pending_nodes: Vec<PageIndexNodeEvidence>,
    retained_bytes: u64,
}

enum ParsedPageTreeNode {
    Page,
    Pages {
        page_count: u32,
        count_offset: u64,
        children: Vec<PageIndexChild>,
        retained_child_bytes: u64,
    },
}

/// One-shot exact logical-page lookup over an immutable segmented page index.
///
/// A lookup refines only Pages segments on the requested root-to-leaf frontier. Each refinement
/// classifies every direct child so its half-open page ranges form a checked partition, while
/// unrelated descendant subtrees remain unopened. Reusing the returned index makes an already
/// resolved Page a zero-object-job cache hit.
pub struct LookupPageJob<'index> {
    authority: AttestedRevisionIndexOwner<'index>,
    snapshot: SourceSnapshot,
    context: PageTreeJobContext,
    limits: PageTreeLimits,
    target_index: u32,
    page_index: Option<PageIndex>,
    frontier: Option<FrontierState>,
    current: Option<CurrentTarget>,
    child: Option<LookupChildState>,
    stats: PageLookupStats,
    state: LookupJobState,
    terminal_error: DocumentError,
}

impl LookupPageJob<'_> {
    /// Returns the immutable source snapshot covered by the proof and page index.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns runtime identity, child checkpoints, and scheduling priority.
    pub const fn context(&self) -> PageTreeJobContext {
        self.context
    }

    /// Returns the complete validated lookup limits.
    pub const fn limits(&self) -> PageTreeLimits {
        self.limits
    }

    /// Returns the requested zero-based logical page index.
    pub const fn target_index(&self) -> u32 {
        self.target_index
    }

    /// Returns deterministic work and frontier-capacity accounting through the latest poll.
    pub const fn stats(&self) -> PageLookupStats {
        self.stats
    }

    /// Returns the public resumable lookup phase.
    pub const fn phase(&self) -> PageLookupPhase {
        match self.state {
            LookupJobState::Ready => PageLookupPhase::Ready,
            LookupJobState::Failed => PageLookupPhase::Failed,
            LookupJobState::Active if self.child.is_some() => match self.current {
                Some(CurrentTarget::Frontier { .. }) => PageLookupPhase::OpeningFrontier,
                Some(CurrentTarget::Child { .. }) => PageLookupPhase::ClassifyingChildren,
                None => PageLookupPhase::Selecting,
            },
            LookupJobState::Active if self.frontier.is_some() => {
                PageLookupPhase::ClassifyingChildren
            }
            LookupJobState::Active => PageLookupPhase::Selecting,
        }
    }

    /// Advances the lookup without performing host I/O or resuming inside a callback.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> PageLookupPoll {
        if !matches!(self.state, LookupJobState::Active) {
            return PageLookupPoll::Failed(self.terminal_error);
        }

        loop {
            if source.snapshot() != self.snapshot {
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

            if self.frontier.is_none() {
                match self.select_frontier(cancellation) {
                    Ok(Some((object, index))) => return self.finish_ready(object, index),
                    Ok(None) => {}
                    Err(error) => return self.fail(error),
                }
            }

            if self.child.is_none() {
                if self.current.is_none() {
                    match self.schedule_next_target() {
                        Ok(true) => {}
                        Ok(false) => {
                            if let Err(error) = self.finish_frontier_refinement() {
                                return self.fail(error);
                            }
                            continue;
                        }
                        Err(error) => return self.fail(error),
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
                    return PageLookupPoll::Pending {
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
                    if source.snapshot() != self.snapshot {
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
                        CurrentTarget::Frontier { reference, depth } => {
                            self.accept_frontier(reference, depth, object, cancellation)
                        }
                        CurrentTarget::Child {
                            edge,
                            depth,
                            start_index,
                        } => self.accept_child(edge, depth, start_index, object, cancellation),
                    };
                    if let Err(error) = result {
                        return self.fail(error);
                    }
                }
            }
        }
    }

    fn select_frontier(
        &mut self,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Option<(ObjectRef, u32)>, DocumentError> {
        if cancellation.is_cancelled() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                None,
                None,
            ));
        }
        let page_index = self
            .page_index
            .as_ref()
            .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))?;
        let (segment_index, segment) = page_index
            .segment_containing(self.target_index)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::PageIndexOutOfBounds,
                    Some(page_index.catalog().pages()),
                    None,
                )
            })?;
        if segment.kind() == PageIndexSegmentKind::Page {
            if segment.start_index() != self.target_index || segment.page_count() != 1 {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(segment.object()),
                    segment.count_offset(),
                ));
            }
            return Ok(Some((segment.object(), self.target_index)));
        }
        if u64::from(segment.depth()) > self.limits.max_depth() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeDepth,
                self.limits.max_depth(),
                u64::from(segment.depth().saturating_sub(1)),
                1,
                segment.object(),
                segment.count_offset(),
            ));
        }
        if u64::from(segment.page_count()) > self.limits.max_pages() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreePages,
                self.limits.max_pages(),
                0,
                u64::from(segment.page_count()),
                segment.object(),
                segment.count_offset(),
            ));
        }

        let mut frontier = FrontierState {
            segment_index,
            segment: segment.clone(),
            count_offset: segment.count_offset(),
            children_ready: false,
            children: Vec::new(),
            next_child: 0,
            next_index: segment.start_index(),
            replacements: Vec::new(),
            pending_nodes: Vec::new(),
            retained_bytes: 0,
        };
        if let Some(children) = segment.children() {
            self.reserve_frontier_storage(&mut frontier, children.len(), segment.object(), None)?;
            frontier.children.extend_from_slice(children);
            frontier.children_ready = true;
        }
        self.stats.peak_retained_traversal_bytes = self
            .stats
            .peak_retained_traversal_bytes
            .max(frontier.retained_bytes);
        self.frontier = Some(frontier);
        Ok(None)
    }

    fn reserve_frontier_storage(
        &self,
        frontier: &mut FrontierState,
        child_count: usize,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        let requested = retained_frontier_bytes(child_count, child_count).ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
        })?;
        if requested > self.limits.max_retained_traversal_bytes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                self.limits.max_retained_traversal_bytes(),
                0,
                requested,
                reference,
                offset,
            ));
        }
        frontier
            .children
            .try_reserve_exact(child_count)
            .map_err(|_| {
                DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeTraversalBytes,
                    self.limits.max_retained_traversal_bytes(),
                    0,
                    requested,
                    reference,
                    offset,
                )
            })?;
        frontier
            .replacements
            .try_reserve_exact(child_count)
            .map_err(|_| {
                DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeTraversalBytes,
                    self.limits.max_retained_traversal_bytes(),
                    0,
                    requested,
                    reference,
                    offset,
                )
            })?;
        frontier.retained_bytes = retained_frontier_bytes(
            frontier.children.capacity(),
            frontier.replacements.capacity(),
        )
        .ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
        })?;
        if frontier.retained_bytes > self.limits.max_retained_traversal_bytes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                self.limits.max_retained_traversal_bytes(),
                0,
                frontier.retained_bytes,
                reference,
                offset,
            ));
        }
        Ok(())
    }

    fn schedule_next_target(&mut self) -> Result<bool, DocumentError> {
        let Some(frontier) = self.frontier.as_mut() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                None,
                None,
            ));
        };
        if !frontier.children_ready {
            self.current = Some(CurrentTarget::Frontier {
                reference: frontier.segment.object(),
                depth: frontier.segment.depth(),
            });
            return Ok(true);
        }
        if frontier.next_child >= frontier.children.len() {
            return Ok(false);
        }
        let edge = frontier.children[frontier.next_child];
        let depth = frontier.segment.depth().checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(frontier.segment.object()),
                Some(edge.edge_offset()),
            )
        })?;
        let page_index = self.page_index.as_ref().ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(edge.reference()),
                Some(edge.edge_offset()),
            )
        })?;
        if !page_index.node_matches(
            edge.reference(),
            Some(frontier.segment.object()),
            edge.edge_offset(),
            depth,
        ) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::AttestedObjectEvidenceMismatch,
                Some(edge.reference()),
                Some(edge.edge_offset()),
            ));
        }
        self.current = Some(CurrentTarget::Child {
            edge,
            depth,
            start_index: frontier.next_index,
        });
        Ok(true)
    }

    fn start_child(&mut self) -> Result<(), DocumentError> {
        let target = self.current.ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_reference(),
                self.current_offset(),
            )
        })?;
        let (reference, depth) = match target {
            CurrentTarget::Frontier { reference, depth } => (reference, depth),
            CurrentTarget::Child { edge, depth, .. } => (edge.reference(), depth),
        };
        let attestation = self.authority.attestation(reference)?;
        let offset = attestation.xref_offset();
        if self.stats.objects_started >= self.limits.max_nodes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeNodes,
                self.limits.max_nodes(),
                self.stats.objects_started,
                1,
                reference,
                Some(offset),
            ));
        }
        if u64::from(depth) > self.limits.max_depth() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeDepth,
                self.limits.max_depth(),
                u64::from(depth.saturating_sub(1)),
                1,
                reference,
                Some(offset),
            ));
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
            read_remaining.min(self.authority.object_limits().max_total_read_bytes()),
            parse_remaining.min(self.authority.object_limits().max_total_parse_bytes()),
        )
        .map_err(|error| DocumentError::from_object_access_constructor(error, reference, offset))?;
        let context = AttestedObjectJobContext::new(
            self.context.job(),
            self.context.object_envelope_checkpoint(),
            self.context.object_boundary_checkpoint(),
            self.context.priority(),
        );
        let job = self.authority.open_object(reference, context, work_caps)?;
        self.stats.objects_started =
            self.stats.objects_started.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        self.stats.max_depth = self.stats.max_depth.max(u64::from(depth));
        self.child = Some(LookupChildState {
            job,
            accounted_stats: ObjectStats::default(),
            work_caps,
            reference,
            offset,
        });
        Ok(())
    }

    fn account_child_stats(&mut self, child: &mut LookupChildState) -> Result<(), DocumentError> {
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

    fn map_child_error(&self, error: DocumentError, child: &LookupChildState) -> DocumentError {
        if error.code() == DocumentErrorCode::ResourceLimit
            && let Some(lower) = error.object_error()
            && let Some(limit) = lower.limit()
        {
            match limit.kind() {
                ObjectLimitKind::TotalReadBytes
                    if child.work_caps.max_read_bytes()
                        < self.authority.object_limits().max_total_read_bytes() =>
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
                        < self.authority.object_limits().max_total_parse_bytes() =>
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
                | ObjectLimitKind::TotalParseBytes
                | ObjectLimitKind::RepairScanBytes
                | ObjectLimitKind::RepairHeaderCandidates
                | ObjectLimitKind::RepairBoundaryCandidates => {}
            }
        }
        error
    }

    fn accept_frontier(
        &mut self,
        reference: ObjectRef,
        depth: u32,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        if object.reference() != reference {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(object.reference()),
                Some(object.attestation().xref_offset()),
            ));
        }
        let (expected_parent, expected_count) = {
            let frontier = self.frontier.as_ref().ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), None)
            })?;
            if frontier.segment.object() != reference || frontier.segment.depth() != depth {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    None,
                ));
            }
            (frontier.segment.parent(), frontier.segment.page_count())
        };
        let parsed =
            self.parse_page_tree_node(object, expected_parent, depth, true, cancellation)?;
        let ParsedPageTreeNode::Pages {
            page_count,
            count_offset,
            children,
            retained_child_bytes,
        } = parsed
        else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeNode,
                Some(reference),
                self.current_offset(),
            ));
        };
        if page_count != expected_count {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageTreeCountMismatch,
                Some(reference),
                Some(count_offset),
            ));
        }
        let child_count = children.len();
        let Some(frontier) = self.frontier.as_mut() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(count_offset),
            ));
        };
        reserve_replacement_storage(
            self.limits,
            &mut self.stats,
            frontier,
            child_count,
            retained_child_bytes,
            reference,
            Some(count_offset),
        )?;
        self.discover_children(reference, depth, &children, cancellation)?;
        let Some(frontier) = self.frontier.as_mut() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(count_offset),
            ));
        };
        frontier.children = children;
        frontier.count_offset = Some(count_offset);
        frontier.children_ready = true;
        self.stats.nodes_classified =
            self.stats.nodes_classified.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(count_offset),
                )
            })?;
        self.stats.max_kids_per_node = self
            .stats
            .max_kids_per_node
            .max(u64::try_from(child_count).unwrap_or(u64::MAX));
        Ok(())
    }

    fn accept_child(
        &mut self,
        edge: PageIndexChild,
        depth: u32,
        start_index: u32,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        if object.reference() != edge.reference() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(object.reference()),
                Some(object.attestation().xref_offset()),
            ));
        }
        let parent = self
            .frontier
            .as_ref()
            .map(|frontier| frontier.segment.object())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(edge.reference()),
                    Some(edge.edge_offset()),
                )
            })?;
        let parsed = self.parse_page_tree_node(object, Some(parent), depth, false, cancellation)?;
        if let ParsedPageTreeNode::Pages { children, .. } = &parsed {
            self.discover_children(edge.reference(), depth, children, cancellation)?;
        }
        let complete_tree_proof = self
            .page_index
            .as_ref()
            .is_some_and(|page_index| page_index.stats().has_complete_tree_proof());
        let (summary, contribution, nested_child_bytes, count_offset) = match parsed {
            ParsedPageTreeNode::Page => (
                Some(PageSegmentSummary::page(
                    start_index,
                    edge.reference(),
                    parent,
                    depth,
                )),
                1,
                0,
                None,
            ),
            ParsedPageTreeNode::Pages {
                page_count,
                count_offset,
                children,
                retained_child_bytes,
            } => {
                self.stats.max_kids_per_node = self
                    .stats
                    .max_kids_per_node
                    .max(u64::try_from(children.len()).unwrap_or(u64::MAX));
                (
                    Some(PageSegmentSummary::pages(
                        start_index,
                        page_count,
                        edge.reference(),
                        Some(parent),
                        depth,
                        if complete_tree_proof {
                            PageSegmentEvidence::CompleteSubtree
                        } else {
                            PageSegmentEvidence::DeclaredCount
                        },
                        Some(count_offset),
                        Some(children),
                    )),
                    page_count,
                    retained_child_bytes,
                    Some(count_offset),
                )
            }
        };
        let Some(frontier) = self.frontier.as_mut() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(edge.reference()),
                Some(edge.edge_offset()),
            ));
        };
        if frontier.next_child >= frontier.children.len()
            || frontier.children[frontier.next_child] != edge
            || frontier.next_index != start_index
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(edge.reference()),
                Some(edge.edge_offset()),
            ));
        }
        let next_index = frontier
            .next_index
            .checked_add(contribution)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(edge.reference()),
                    count_offset.or(Some(edge.edge_offset())),
                )
            })?;
        if next_index > frontier.segment.end_index() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageTreeCountMismatch,
                Some(frontier.segment.object()),
                frontier.count_offset.or(frontier.segment.count_offset()),
            ));
        }
        frontier.retained_bytes = frontier
            .retained_bytes
            .checked_add(nested_child_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(edge.reference()),
                    count_offset.or(Some(edge.edge_offset())),
                )
            })?;
        if frontier.retained_bytes > self.limits.max_retained_traversal_bytes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                self.limits.max_retained_traversal_bytes(),
                frontier.retained_bytes.saturating_sub(nested_child_bytes),
                nested_child_bytes,
                edge.reference(),
                count_offset.or(Some(edge.edge_offset())),
            ));
        }
        if let Some(summary) = summary {
            frontier.replacements.push(summary);
        }
        frontier.next_index = next_index;
        frontier.next_child += 1;
        self.stats.nodes_classified =
            self.stats.nodes_classified.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(edge.reference()),
                    count_offset.or(Some(edge.edge_offset())),
                )
            })?;
        self.stats.peak_retained_traversal_bytes = self
            .stats
            .peak_retained_traversal_bytes
            .max(frontier.retained_bytes);
        Ok(())
    }

    fn discover_children(
        &mut self,
        parent: ObjectRef,
        parent_depth: u32,
        children: &[PageIndexChild],
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        if children.is_empty() {
            return Ok(());
        }
        let child_depth = parent_depth.checked_add(1).ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(parent), None)
        })?;
        {
            let Some(frontier) = self.frontier.as_mut() else {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(parent),
                    None,
                ));
            };
            let before =
                retained_node_bytes(frontier.pending_nodes.capacity()).ok_or_else(|| {
                    DocumentError::for_code(DocumentErrorCode::InternalState, Some(parent), None)
                })?;
            frontier
                .pending_nodes
                .try_reserve_exact(children.len())
                .map_err(|_| {
                    DocumentError::page_tree_resource(
                        DocumentLimitKind::PageTreeTraversalBytes,
                        self.limits.max_retained_traversal_bytes(),
                        frontier.retained_bytes,
                        u64::MAX,
                        parent,
                        children.first().map(|child| child.edge_offset()),
                    )
                })?;
            let after =
                retained_node_bytes(frontier.pending_nodes.capacity()).ok_or_else(|| {
                    DocumentError::for_code(DocumentErrorCode::InternalState, Some(parent), None)
                })?;
            let added = after.checked_sub(before).ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(parent), None)
            })?;
            frontier.retained_bytes =
                frontier.retained_bytes.checked_add(added).ok_or_else(|| {
                    DocumentError::for_code(DocumentErrorCode::InternalState, Some(parent), None)
                })?;
            if frontier.retained_bytes > self.limits.max_retained_traversal_bytes() {
                return Err(DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeTraversalBytes,
                    self.limits.max_retained_traversal_bytes(),
                    frontier.retained_bytes.saturating_sub(added),
                    added,
                    parent,
                    children.first().map(|child| child.edge_offset()),
                ));
            }
        }

        for (index, child) in children.iter().enumerate() {
            if index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(parent),
                    Some(child.edge_offset()),
                ));
            }
            let node = {
                let page_index = self.page_index.as_ref().ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(parent),
                        Some(child.edge_offset()),
                    )
                })?;
                let pending = self
                    .frontier
                    .as_ref()
                    .map(|frontier| frontier.pending_nodes.as_slice())
                    .ok_or_else(|| {
                        DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(parent),
                            Some(child.edge_offset()),
                        )
                    })?;
                page_index.validate_new_node(
                    parent,
                    child.reference(),
                    child.edge_offset(),
                    child_depth,
                    pending,
                )?
            };
            let Some(frontier) = self.frontier.as_mut() else {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(parent),
                    Some(child.edge_offset()),
                ));
            };
            frontier.pending_nodes.push(node);
        }
        for child in children {
            self.authority.attestation(child.reference())?;
        }
        let retained = self
            .frontier
            .as_ref()
            .map(|frontier| frontier.retained_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(parent), None)
            })?;
        self.stats.peak_retained_traversal_bytes =
            self.stats.peak_retained_traversal_bytes.max(retained);
        Ok(())
    }

    fn parse_page_tree_node(
        &self,
        object: AttestedObject,
        expected_parent: Option<ObjectRef>,
        depth: u32,
        opening_frontier: bool,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<ParsedPageTreeNode, DocumentError> {
        let reference = object.reference();
        let offset = object.attestation().xref_offset();
        let dictionary = direct_dictionary(
            &object,
            self.snapshot,
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
        validate_parent(
            reference,
            expected_parent,
            optional_field(&fields, 1),
            offset,
        )?;
        if is_page {
            if expected_parent.is_none() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageTreeNode,
                    Some(reference),
                    Some(type_value.span().start()),
                ));
            }
            return Ok(ParsedPageTreeNode::Page);
        }

        reject_duplicate_field(&fields, 2, reference)?;
        reject_duplicate_field(&fields, 3, reference)?;
        self.parse_pages_node(
            reference,
            depth,
            offset,
            &fields,
            opening_frontier,
            cancellation,
        )
    }

    fn parse_pages_node(
        &self,
        reference: ObjectRef,
        depth: u32,
        object_offset: u64,
        fields: &StructuralFields<'_, 4>,
        opening_frontier: bool,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<ParsedPageTreeNode, DocumentError> {
        let kids_value = required_field(
            fields,
            2,
            reference,
            object_offset,
            DocumentErrorCode::InvalidPageTreeNode,
        )?;
        let SyntaxObject::Array(kids) = kids_value.value() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeNode,
                Some(reference),
                Some(kids_value.span().start()),
            ));
        };
        let count_value = required_field(
            fields,
            3,
            reference,
            object_offset,
            DocumentErrorCode::InvalidPageTreeNode,
        )?;
        let Some(page_count) = count_value
            .value()
            .as_integer()
            .and_then(|value| u32::try_from(value).ok())
        else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeNode,
                Some(reference),
                Some(count_value.span().start()),
            ));
        };
        if u64::from(page_count) > self.limits.max_pages() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreePages,
                self.limits.max_pages(),
                0,
                u64::from(page_count),
                reference,
                Some(count_value.span().start()),
            ));
        }

        let child_count = kids.values().len();
        let child_count_u64 = u64::try_from(child_count).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(kids_value.span().start()),
            )
        })?;
        if child_count_u64 > self.limits.max_kids_per_node() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeKids,
                self.limits.max_kids_per_node(),
                0,
                child_count_u64,
                reference,
                Some(kids_value.span().start()),
            ));
        }
        let child_depth = depth.checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(kids_value.span().start()),
            )
        })?;
        if child_count > 0 && u64::from(child_depth) > self.limits.max_depth() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeDepth,
                self.limits.max_depth(),
                u64::from(depth),
                1,
                reference,
                Some(kids_value.span().start()),
            ));
        }
        let retained_before = self
            .frontier
            .as_ref()
            .map_or(0, |frontier| frontier.retained_bytes);
        let attempted_bytes = if opening_frontier {
            retained_frontier_bytes(child_count, child_count)
        } else {
            retained_children_bytes(child_count)
        }
        .ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(kids_value.span().start()),
            )
        })?;
        let requested_work_bytes =
            retained_before
                .checked_add(attempted_bytes)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(kids_value.span().start()),
                    )
                })?;
        if requested_work_bytes > self.limits.max_retained_traversal_bytes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                self.limits.max_retained_traversal_bytes(),
                retained_before,
                attempted_bytes,
                reference,
                Some(kids_value.span().start()),
            ));
        }
        let mut children = Vec::new();
        children.try_reserve_exact(child_count).map_err(|_| {
            DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                self.limits.max_retained_traversal_bytes(),
                retained_before,
                attempted_bytes,
                reference,
                Some(kids_value.span().start()),
            )
        })?;
        for (index, child) in kids.values().iter().enumerate() {
            if index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(reference),
                    Some(child.span().start()),
                ));
            }
            let Some(child_reference) = child.value().as_reference() else {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageTreeNode,
                    Some(reference),
                    Some(child.span().start()),
                ));
            };
            children.push(PageIndexChild::new(child_reference, child.span().start()));
        }
        let retained_child_bytes =
            retained_children_bytes(children.capacity()).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(kids_value.span().start()),
                )
            })?;
        Ok(ParsedPageTreeNode::Pages {
            page_count,
            count_offset: count_value.span().start(),
            children,
            retained_child_bytes,
        })
    }

    fn finish_frontier_refinement(&mut self) -> Result<(), DocumentError> {
        let Some(frontier) = self.frontier.take() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                None,
                None,
            ));
        };
        if !frontier.children_ready
            || frontier.next_child != frontier.children.len()
            || frontier.next_index != frontier.segment.end_index()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageTreeCountMismatch,
                Some(frontier.segment.object()),
                frontier.count_offset.or(frontier.segment.count_offset()),
            ));
        }
        let page_index = self.page_index.take().ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(frontier.segment.object()),
                frontier.count_offset.or(frontier.segment.count_offset()),
            )
        })?;
        let expanded = PageSegmentSummary::pages(
            frontier.segment.start_index(),
            frontier.segment.page_count(),
            frontier.segment.object(),
            frontier.segment.parent(),
            frontier.segment.depth(),
            if frontier.segment.evidence() == PageSegmentEvidence::CompleteSubtree {
                PageSegmentEvidence::CompleteSubtree
            } else {
                PageSegmentEvidence::ValidatedPartition
            },
            frontier.count_offset,
            Some(frontier.children),
        );
        let refined = page_index.refine(
            frontier.segment_index,
            expanded,
            frontier.replacements,
            frontier.pending_nodes,
        )?;
        self.page_index = Some(refined);
        self.stats.segments_refined =
            self.stats.segments_refined.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(frontier.segment.object()),
                    frontier.count_offset.or(frontier.segment.count_offset()),
                )
            })?;
        Ok(())
    }

    fn finish_ready(&mut self, object: ObjectRef, index: u32) -> PageLookupPoll {
        if self.frontier.is_some() || self.current.is_some() || self.child.is_some() {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(object),
                None,
            ));
        }
        let Some(page_index) = self.page_index.take() else {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(object),
                None,
            ));
        };
        let handle = page_index.mint_handle(index, object);
        self.state = LookupJobState::Ready;
        self.terminal_error =
            DocumentError::for_code(DocumentErrorCode::JobAlreadyComplete, Some(object), None);
        PageLookupPoll::Ready(PageLookup { page_index, handle })
    }

    fn fail(&mut self, error: DocumentError) -> PageLookupPoll {
        self.child = None;
        self.current = None;
        self.frontier = None;
        self.page_index = None;
        self.state = LookupJobState::Failed;
        self.terminal_error = error;
        PageLookupPoll::Failed(error)
    }

    fn current_reference(&self) -> Option<ObjectRef> {
        match self.current {
            Some(CurrentTarget::Frontier { reference, .. }) => Some(reference),
            Some(CurrentTarget::Child { edge, .. }) => Some(edge.reference()),
            None => self
                .frontier
                .as_ref()
                .map(|frontier| frontier.segment.object())
                .or_else(|| {
                    self.page_index
                        .as_ref()
                        .map(|page_index| page_index.catalog().pages())
                }),
        }
    }

    fn current_offset(&self) -> Option<u64> {
        match self.current {
            Some(CurrentTarget::Child { edge, .. }) => Some(edge.edge_offset()),
            Some(CurrentTarget::Frontier { reference, .. }) => self
                .authority
                .attestation(reference)
                .ok()
                .map(crate::ObjectAttestation::xref_offset),
            None => self
                .current_reference()
                .and_then(|reference| self.authority.attestation(reference).ok())
                .map(crate::ObjectAttestation::xref_offset),
        }
    }
}

impl fmt::Debug for LookupPageJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LookupPageJob")
            .field("snapshot", &self.snapshot)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("target_index", &self.target_index)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .field("page_index", &"[REDACTED]")
            .field("frontier", &"[REDACTED]")
            .field("current", &"[REDACTED]")
            .field("child", &"[REDACTED]")
            .finish()
    }
}

impl AttestedRevisionIndex {
    /// Cold-builds an immutable segmented page index while borrowing this strict proof.
    pub fn build_page_index(
        &self,
        context: PageTreeJobContext,
        tree_limits: PageTreeLimits,
        index_limits: PageIndexLimits,
    ) -> Result<BuildPageIndexJob<'_>, DocumentError> {
        build_page_index_with_owner(
            AttestedRevisionIndexOwner::Borrowed(self),
            context,
            tree_limits,
            index_limits,
        )
    }

    /// Creates a borrowed exact-page lookup that owns an immutable index clone.
    pub fn lookup_page(
        &self,
        page_index: &PageIndex,
        target_index: u32,
        context: PageTreeJobContext,
        limits: PageTreeLimits,
    ) -> Result<LookupPageJob<'_>, DocumentError> {
        lookup_page_with_owner(
            AttestedRevisionIndexOwner::Borrowed(self),
            page_index,
            target_index,
            context,
            limits,
        )
    }
}

impl SharedAttestedRevisionIndex {
    /// Cold-builds an index in a job that owns a clone of this strict proof handle.
    pub fn build_page_index_owned(
        &self,
        context: PageTreeJobContext,
        tree_limits: PageTreeLimits,
        index_limits: PageIndexLimits,
    ) -> Result<BuildPageIndexJob<'static>, DocumentError> {
        build_page_index_with_owner(
            AttestedRevisionIndexOwner::Shared(self.clone()),
            context,
            tree_limits,
            index_limits,
        )
    }

    /// Creates an exact-page lookup that owns clones of the proof handle and immutable index.
    pub fn lookup_page_owned(
        &self,
        page_index: &PageIndex,
        target_index: u32,
        context: PageTreeJobContext,
        limits: PageTreeLimits,
    ) -> Result<LookupPageJob<'static>, DocumentError> {
        lookup_page_with_owner(
            AttestedRevisionIndexOwner::Shared(self.clone()),
            page_index,
            target_index,
            context,
            limits,
        )
    }
}

impl LocallyRepairedRevisionIndex {
    /// Cold-builds an immutable segmented page index while borrowing this repaired proof.
    pub fn build_page_index(
        &self,
        context: PageTreeJobContext,
        tree_limits: PageTreeLimits,
        index_limits: PageIndexLimits,
    ) -> Result<BuildPageIndexJob<'_>, DocumentError> {
        build_page_index_with_owner(
            AttestedRevisionIndexOwner::RepairedBorrowed(self),
            context,
            tree_limits,
            index_limits,
        )
    }

    /// Creates a borrowed exact-page lookup that retains the repaired proof typestate.
    pub fn lookup_page(
        &self,
        page_index: &PageIndex,
        target_index: u32,
        context: PageTreeJobContext,
        limits: PageTreeLimits,
    ) -> Result<LookupPageJob<'_>, DocumentError> {
        lookup_page_with_owner(
            AttestedRevisionIndexOwner::RepairedBorrowed(self),
            page_index,
            target_index,
            context,
            limits,
        )
    }
}

impl SharedLocallyRepairedRevisionIndex {
    /// Cold-builds an index in a job that owns a clone of this repaired proof handle.
    pub fn build_page_index_owned(
        &self,
        context: PageTreeJobContext,
        tree_limits: PageTreeLimits,
        index_limits: PageIndexLimits,
    ) -> Result<BuildPageIndexJob<'static>, DocumentError> {
        build_page_index_with_owner(
            AttestedRevisionIndexOwner::RepairedShared(self.clone()),
            context,
            tree_limits,
            index_limits,
        )
    }

    /// Creates an exact-page lookup that owns clones of the repaired proof and immutable index.
    pub fn lookup_page_owned(
        &self,
        page_index: &PageIndex,
        target_index: u32,
        context: PageTreeJobContext,
        limits: PageTreeLimits,
    ) -> Result<LookupPageJob<'static>, DocumentError> {
        lookup_page_with_owner(
            AttestedRevisionIndexOwner::RepairedShared(self.clone()),
            page_index,
            target_index,
            context,
            limits,
        )
    }
}

fn build_page_index_with_owner<'index>(
    authority: AttestedRevisionIndexOwner<'index>,
    context: PageTreeJobContext,
    tree_limits: PageTreeLimits,
    limits: PageIndexLimits,
) -> Result<BuildPageIndexJob<'index>, DocumentError> {
    let attested = authority.as_attested();
    let snapshot = attested.snapshot();
    let root = attested.root();
    let root_offset = attested.attestation(root)?.xref_offset();
    if context.object_envelope_checkpoint() == context.object_boundary_checkpoint() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidPageTreeJobContext,
            Some(root),
            Some(root_offset),
        ));
    }
    Ok(BuildPageIndexJob {
        authority,
        snapshot,
        context,
        stats: PageIndexStats::default(),
        tree_limits,
        limits,
        catalog: None,
        current: None,
        child: None,
        root,
        root_offset: Some(root_offset),
        state: BuildState::Active,
        terminal_error: DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(root),
            Some(root_offset),
        ),
    })
}

fn lookup_page_with_owner<'index>(
    authority: AttestedRevisionIndexOwner<'index>,
    page_index: &PageIndex,
    target_index: u32,
    context: PageTreeJobContext,
    limits: PageTreeLimits,
) -> Result<LookupPageJob<'index>, DocumentError> {
    let attested = authority.as_attested();
    let root = attested.root();
    let root_offset = attested.attestation(root)?.xref_offset();
    if context.object_envelope_checkpoint() == context.object_boundary_checkpoint() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidPageTreeJobContext,
            Some(root),
            Some(root_offset),
        ));
    }
    if !page_index.binding_matches(attested) {
        return Err(DocumentError::for_code(
            DocumentErrorCode::AttestedObjectEvidenceMismatch,
            Some(root),
            Some(root_offset),
        ));
    }
    if limits != page_index.tree_limits() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidLimits,
            Some(page_index.catalog().pages()),
            None,
        ));
    }
    if target_index >= page_index.len() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::PageIndexOutOfBounds,
            Some(page_index.catalog().pages()),
            None,
        ));
    }
    Ok(LookupPageJob {
        snapshot: attested.snapshot(),
        authority,
        context,
        limits,
        target_index,
        page_index: Some(page_index.clone()),
        frontier: None,
        current: None,
        child: None,
        stats: PageLookupStats::default(),
        state: LookupJobState::Active,
        terminal_error: DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(root),
            Some(root_offset),
        ),
    })
}

fn validate_parent(
    reference: ObjectRef,
    expected: Option<ObjectRef>,
    parent_value: Option<&Located<SyntaxObject>>,
    object_offset: u64,
) -> Result<(), DocumentError> {
    match (expected, parent_value) {
        (None, None) => Ok(()),
        (Some(expected), Some(value)) if value.value().as_reference() == Some(expected) => Ok(()),
        (_, Some(value)) => Err(DocumentError::for_code(
            DocumentErrorCode::PageTreeParentMismatch,
            Some(reference),
            Some(value.span().start()),
        )),
        (Some(_), None) => Err(DocumentError::for_code(
            DocumentErrorCode::PageTreeParentMismatch,
            Some(reference),
            Some(object_offset),
        )),
    }
}

fn reserve_replacement_storage(
    limits: PageTreeLimits,
    stats: &mut PageLookupStats,
    frontier: &mut FrontierState,
    child_count: usize,
    retained_child_bytes: u64,
    reference: ObjectRef,
    offset: Option<u64>,
) -> Result<(), DocumentError> {
    frontier
        .replacements
        .try_reserve_exact(child_count)
        .map_err(|_| {
            DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                frontier.retained_bytes,
                u64::MAX,
                reference,
                offset,
            )
        })?;
    let fixed = retained_frontier_bytes(
        frontier.children.capacity(),
        frontier.replacements.capacity(),
    )
    .ok_or_else(|| {
        DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
    })?;
    frontier.retained_bytes = fixed.checked_add(retained_child_bytes).ok_or_else(|| {
        DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
    })?;
    if frontier.retained_bytes > limits.max_retained_traversal_bytes() {
        return Err(DocumentError::page_tree_resource(
            DocumentLimitKind::PageTreeTraversalBytes,
            limits.max_retained_traversal_bytes(),
            0,
            frontier.retained_bytes,
            reference,
            offset,
        ));
    }
    stats.peak_retained_traversal_bytes = stats
        .peak_retained_traversal_bytes
        .max(frontier.retained_bytes);
    Ok(())
}

fn retained_children_bytes(children: usize) -> Option<u64> {
    u64::try_from(children)
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<PageIndexChild>()).ok()?)
}

fn retained_node_bytes(nodes: usize) -> Option<u64> {
    u64::try_from(nodes)
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<PageIndexNodeEvidence>()).ok()?)
}

fn retained_frontier_bytes(children: usize, replacements: usize) -> Option<u64> {
    let children = retained_children_bytes(children)?;
    let replacements = u64::try_from(replacements)
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<PageSegmentSummary>()).ok()?)?;
    children.checked_add(replacements)
}

fn root_bootstrap_bytes(children: usize, nodes: usize) -> Option<u64> {
    let retained_children = retained_children_bytes(children)?;
    let duplicate_probe = retained_children_bytes(children)?;
    let retained_nodes = retained_node_bytes(nodes)?;
    retained_children
        .checked_add(duplicate_probe)?
        .checked_add(retained_nodes)
}
