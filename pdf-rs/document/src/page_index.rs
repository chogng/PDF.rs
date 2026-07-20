use std::mem;
use std::sync::Arc;

use pdf_rs_bytes::SourceSnapshot;
use pdf_rs_syntax::ObjectRef;

use crate::{
    AttestedRevisionIndex, DocumentError, DocumentErrorCode, DocumentLimitKind, PageCount,
    PageTreeLimits, PageTreeStats, RevisionId, StrictCatalog,
};

const HARD_MAX_PAGES: u64 = 4_000_000;
const HARD_MAX_RETAINED_INDEX_BYTES: u64 = 512 * 1024 * 1024;

/// Validated admission limits for one immutable segmented page index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageIndexLimits {
    max_pages: u64,
    max_retained_index_bytes: u64,
}

impl PageIndexLimits {
    /// Validates nonzero page and retained-capacity ceilings against fixed hard limits.
    pub fn new(max_pages: u64, max_retained_index_bytes: u64) -> Result<Self, DocumentError> {
        if max_pages == 0
            || max_pages > HARD_MAX_PAGES
            || max_retained_index_bytes == 0
            || max_retained_index_bytes > HARD_MAX_RETAINED_INDEX_BYTES
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_pages,
            max_retained_index_bytes,
        })
    }

    /// Returns the maximum leaf identities admitted to one index.
    pub const fn max_pages(self) -> u64 {
        self.max_pages
    }

    /// Returns the allocator-reported segment and retained-child capacity ceiling.
    pub const fn max_retained_index_bytes(self) -> u64 {
        self.max_retained_index_bytes
    }
}

impl Default for PageIndexLimits {
    fn default() -> Self {
        Self::new(25_000, 4 * 1024 * 1024)
            .expect("built-in page-index limits satisfy hard ceilings")
    }
}

/// Kind of one validated page-index frontier segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageIndexSegmentKind {
    /// One exact leaf Page object covering a single logical index.
    Page,
    /// One Pages subtree carrying declared, partitioned, or complete Count evidence.
    Pages,
}

/// Strength of the retained proof for one page-index segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageSegmentEvidence {
    /// One exact Page dictionary identity has been validated.
    ExactPage,
    /// A Pages dictionary and its nonnegative Count were validated, but its child partition has
    /// not yet been classified.
    DeclaredCount,
    /// Every direct child was classified and its Page-or-Count contribution exactly partitioned
    /// the parent range.
    ValidatedPartition,
    /// Every descendant leaf in this subtree was validated and recomputed against Count.
    CompleteSubtree,
}

/// One retained direct child edge of a validated Pages dictionary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct PageIndexChild {
    reference: ObjectRef,
    edge_offset: u64,
}

impl PageIndexChild {
    pub(crate) const fn new(reference: ObjectRef, edge_offset: u64) -> Self {
        Self {
            reference,
            edge_offset,
        }
    }

    pub(crate) const fn reference(&self) -> ObjectRef {
        self.reference
    }

    pub(crate) const fn edge_offset(&self) -> u64 {
        self.edge_offset
    }
}

/// One discovered Page or Pages identity retained before or beside exact segment classification.
///
/// Discovery evidence is private to the document crate because an object reference alone does not
/// prove whether the target is a Page or Pages dictionary. Retaining the exact owning edge and
/// depth lets later lazy refinements distinguish active-ancestor cycles from duplicate reachability
/// without reopening unrelated subtrees.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageIndexNodeEvidence {
    reference: ObjectRef,
    parent: Option<ObjectRef>,
    edge_offset: u64,
    depth: u32,
}

impl PageIndexNodeEvidence {
    pub(crate) const fn new(
        reference: ObjectRef,
        parent: Option<ObjectRef>,
        edge_offset: u64,
        depth: u32,
    ) -> Self {
        Self {
            reference,
            parent,
            edge_offset,
            depth,
        }
    }

    pub(crate) const fn reference(&self) -> ObjectRef {
        self.reference
    }

    pub(crate) const fn parent(&self) -> Option<ObjectRef> {
        self.parent
    }

    pub(crate) const fn edge_offset(&self) -> u64 {
        self.edge_offset
    }

    pub(crate) const fn depth(&self) -> u32 {
        self.depth
    }
}

/// Immutable validated subtree range retained by a segmented page index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageSegmentSummary {
    start_index: u32,
    page_count: u32,
    object: ObjectRef,
    parent: Option<ObjectRef>,
    depth: u32,
    kind: PageIndexSegmentKind,
    evidence: PageSegmentEvidence,
    declared_count: u32,
    count_offset: Option<u64>,
    children: Option<Arc<Vec<PageIndexChild>>>,
}

impl PageSegmentSummary {
    /// Returns the first zero-based logical page covered by this segment.
    pub const fn start_index(&self) -> u32 {
        self.start_index
    }

    /// Returns the number of logical pages in this half-open segment.
    pub const fn page_count(&self) -> u32 {
        self.page_count
    }

    /// Returns the exclusive end of this segment's logical page range.
    pub const fn end_index(&self) -> u32 {
        self.start_index + self.page_count
    }

    /// Returns the exact Page or Pages object summarized by this segment.
    pub const fn object(&self) -> ObjectRef {
        self.object
    }

    /// Returns the exact parent Pages object, or `None` for the page-tree root.
    pub const fn parent(&self) -> Option<ObjectRef> {
        self.parent
    }

    /// Returns the one-based root-relative Page/Pages depth.
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// Returns whether this segment is one leaf Page or an expandable Pages subtree.
    pub const fn kind(&self) -> PageIndexSegmentKind {
        self.kind
    }

    /// Returns the retained proof strength for this exact segment.
    pub const fn evidence(&self) -> PageSegmentEvidence {
        self.evidence
    }

    /// Returns the dictionary Count value retained for this segment.
    ///
    /// Leaf Page segments use the implicit value one.
    pub const fn declared_count(&self) -> u32 {
        self.declared_count
    }

    /// Returns the recomputed leaf count only when the complete subtree was validated.
    pub const fn validated_count(&self) -> Option<u32> {
        match self.evidence {
            PageSegmentEvidence::ExactPage | PageSegmentEvidence::CompleteSubtree => {
                Some(self.page_count)
            }
            PageSegmentEvidence::DeclaredCount | PageSegmentEvidence::ValidatedPartition => None,
        }
    }

    /// Returns the locally checked direct-child contribution when available.
    pub const fn partitioned_count(&self) -> Option<u32> {
        match self.evidence {
            PageSegmentEvidence::ExactPage
            | PageSegmentEvidence::ValidatedPartition
            | PageSegmentEvidence::CompleteSubtree => Some(self.page_count),
            PageSegmentEvidence::DeclaredCount => None,
        }
    }

    /// Returns the source offset of the Pages Count value when the refining lookup reopened it.
    pub const fn count_offset(&self) -> Option<u64> {
        self.count_offset
    }

    /// Returns the retained direct Kids count when this Pages summary has been refined far enough.
    ///
    /// `None` means the segment is either a Page leaf or its Pages dictionary has not been
    /// reopened by a lookup yet.
    pub fn retained_kid_count(&self) -> Option<u32> {
        self.children
            .as_ref()
            .and_then(|children| u32::try_from(children.len()).ok())
    }

    pub(crate) fn page(start_index: u32, object: ObjectRef, parent: ObjectRef, depth: u32) -> Self {
        Self {
            start_index,
            page_count: 1,
            object,
            parent: Some(parent),
            depth,
            kind: PageIndexSegmentKind::Page,
            evidence: PageSegmentEvidence::ExactPage,
            declared_count: 1,
            count_offset: None,
            children: None,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the sealed segment constructor keeps every proof-bound range field explicit"
    )]
    pub(crate) fn pages(
        start_index: u32,
        page_count: u32,
        object: ObjectRef,
        parent: Option<ObjectRef>,
        depth: u32,
        evidence: PageSegmentEvidence,
        count_offset: Option<u64>,
        children: Option<Vec<PageIndexChild>>,
    ) -> Self {
        Self {
            start_index,
            page_count,
            object,
            parent,
            depth,
            kind: PageIndexSegmentKind::Pages,
            evidence,
            declared_count: page_count,
            count_offset,
            children: children.map(Arc::new),
        }
    }

    pub(crate) fn children(&self) -> Option<&[PageIndexChild]> {
        self.children.as_deref().map(Vec::as_slice)
    }
}

/// Deterministic work retained from construction of one immutable page index.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageIndexStats {
    pub(crate) objects_started: u64,
    pub(crate) nodes_started: u64,
    pub(crate) exact_pages: u64,
    pub(crate) max_depth: u64,
    pub(crate) max_kids_per_node: u64,
    pub(crate) object_read_bytes: u64,
    pub(crate) object_parse_bytes: u64,
    pub(crate) peak_retained_traversal_bytes: u64,
    pub(crate) complete_tree_proof: bool,
}

impl PageIndexStats {
    /// Returns proof-preserving object jobs started while constructing the index.
    pub const fn objects_started(self) -> u64 {
        self.objects_started
    }

    /// Returns Page or Pages dictionaries started while constructing the index.
    pub const fn nodes_started(self) -> u64 {
        self.nodes_started
    }

    /// Returns exact Page leaves validated during index construction.
    pub const fn exact_pages(self) -> u64 {
        self.exact_pages
    }

    /// Returns the greatest root-relative Page/Pages depth started during construction.
    pub const fn max_depth(self) -> u64 {
        self.max_depth
    }

    /// Returns the greatest direct Kids count observed during construction.
    pub const fn max_kids_per_node(self) -> u64 {
        self.max_kids_per_node
    }

    /// Returns cumulative exact-read bytes charged during construction.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }

    /// Returns cumulative parser-window bytes charged during construction.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }

    /// Returns peak allocator-reported traversal capacity during construction.
    pub const fn peak_retained_traversal_bytes(self) -> u64 {
        self.peak_retained_traversal_bytes
    }

    /// Reports whether construction included a complete descendant-and-Count proof.
    pub const fn has_complete_tree_proof(self) -> bool {
        self.complete_tree_proof
    }

    pub(crate) const fn from_complete_tree(stats: PageTreeStats) -> Self {
        Self {
            objects_started: stats.objects_started(),
            nodes_started: stats.nodes_started(),
            exact_pages: stats.pages(),
            max_depth: stats.max_depth(),
            max_kids_per_node: stats.max_kids_per_node(),
            object_read_bytes: stats.object_read_bytes(),
            object_parse_bytes: stats.object_parse_bytes(),
            peak_retained_traversal_bytes: stats.reserved_traversal_bytes(),
            complete_tree_proof: true,
        }
    }
}

/// Source- and revision-bound identity of one validated logical Page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageHandle {
    catalog: StrictCatalog,
    page_count: u32,
    page_count_evidence: PageSegmentEvidence,
    index: u32,
    object: ObjectRef,
}

impl PageHandle {
    /// Returns this Page's zero-based logical index.
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Returns the exact indirect Page object identity.
    pub const fn object(self) -> ObjectRef {
        self.object
    }

    /// Returns the immutable source snapshot covered by this handle.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.catalog.snapshot()
    }

    /// Returns the caller-assigned revision identity covered by this handle.
    pub const fn revision_id(self) -> RevisionId {
        self.catalog.revision_id()
    }

    /// Returns the revision `startxref` anchor covered by this handle.
    pub const fn revision_startxref(self) -> u64 {
        self.catalog.revision_startxref()
    }

    /// Returns the exact trailer Catalog object covered by this handle.
    pub const fn catalog_root(self) -> ObjectRef {
        self.catalog.root()
    }

    /// Returns the exact page-tree root covered by this handle.
    pub const fn page_tree_root(self) -> ObjectRef {
        self.catalog.pages()
    }

    /// Returns the root Pages Count retained by the index that minted this handle.
    pub const fn document_page_count(self) -> u32 {
        self.page_count
    }

    /// Returns the proof strength retained for the root Pages Count when this handle was minted.
    pub const fn document_page_count_evidence(self) -> PageSegmentEvidence {
        self.page_count_evidence
    }
}

/// Immutable source- and revision-bound logical page index.
///
/// Construction remains sealed behind either a completed M1 page-tree proof or an M2 cold
/// Catalog/root proof. A cold index treats unopened subtree Counts as explicit declared-range
/// evidence; lookup returns refined immutable indices that upgrade only requested paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageIndex {
    catalog: StrictCatalog,
    page_count: u32,
    segments: Arc<Vec<PageSegmentSummary>>,
    segment_order: Arc<Vec<usize>>,
    nodes: Arc<Vec<PageIndexNodeEvidence>>,
    retained_index_bytes: u64,
    stats: PageIndexStats,
    tree_limits: PageTreeLimits,
    limits: PageIndexLimits,
}

impl PageIndex {
    /// Returns the validated source- and revision-bound Catalog summary.
    pub const fn catalog(&self) -> StrictCatalog {
        self.catalog
    }

    /// Returns the root Pages Count defining this index's current logical range.
    ///
    /// Callers can inspect the root segment evidence to distinguish a cold declared range from a
    /// completely recomputed subtree.
    pub const fn len(&self) -> u32 {
        self.page_count
    }

    /// Reports whether the retained root Pages Count declares an empty logical range.
    pub const fn is_empty(&self) -> bool {
        self.page_count == 0
    }

    /// Returns the exact Page object when this immutable refinement has resolved that index.
    pub fn page(&self, index: u32) -> Option<ObjectRef> {
        self.segment_containing(index)
            .filter(|(_, segment)| segment.kind() == PageIndexSegmentKind::Page)
            .map(|(_, segment)| segment.object())
    }

    /// Returns every retained validated segment summary.
    ///
    /// Expanded Pages summaries remain present beside their more-specific descendants so subtree
    /// range and Count evidence are not discarded during refinement.
    pub fn segments(&self) -> &[PageSegmentSummary] {
        &self.segments
    }

    /// Reports whether every logical page is exact and every retained Pages summary has a
    /// complete descendant proof.
    pub fn is_complete(&self) -> bool {
        let exact_pages = self
            .segments
            .iter()
            .filter(|segment| {
                segment.kind() == PageIndexSegmentKind::Page
                    && segment.evidence() == PageSegmentEvidence::ExactPage
                    && segment.page_count() == 1
            })
            .count();
        exact_pages == usize::try_from(self.page_count).unwrap_or(usize::MAX)
            && self.segments.iter().all(|segment| {
                segment.kind() != PageIndexSegmentKind::Pages
                    || segment.evidence() == PageSegmentEvidence::CompleteSubtree
            })
    }

    /// Returns allocator-reported retained segment, child-edge, and discovered-node capacity.
    ///
    /// Fixed `Arc` and `Vec` headers are inline owner metadata and are not included.
    pub const fn retained_index_bytes(&self) -> u64 {
        self.retained_index_bytes
    }

    /// Returns deterministic work retained from construction of this index.
    pub const fn stats(&self) -> PageIndexStats {
        self.stats
    }

    /// Returns the structural and per-lookup work limits bound to this immutable index.
    pub const fn tree_limits(&self) -> PageTreeLimits {
        self.tree_limits
    }

    /// Returns one discovered-node identity from the reference-ordered retained table.
    pub(crate) fn node(&self, reference: ObjectRef) -> Option<PageIndexNodeEvidence> {
        self.nodes
            .binary_search_by_key(&reference, PageIndexNodeEvidence::reference)
            .ok()
            .map(|index| self.nodes[index])
    }

    /// Returns the number of globally discovered Page or Pages identities.
    pub(crate) fn discovered_node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Reports whether one exact discovered-node edge is already retained.
    pub(crate) fn node_matches(
        &self,
        reference: ObjectRef,
        parent: Option<ObjectRef>,
        edge_offset: u64,
        depth: u32,
    ) -> bool {
        self.node(reference).is_some_and(|node| {
            node.reference() == reference
                && node.parent() == parent
                && node.edge_offset() == edge_offset
                && node.depth() == depth
        })
    }

    /// Validates the bounded shape of one globally unique newly discovered child.
    pub(crate) fn validate_new_node_shape(
        &self,
        parent: PageIndexNodeEvidence,
        child: ObjectRef,
        edge_offset: u64,
        depth: u32,
        pending_count: usize,
    ) -> Result<PageIndexNodeEvidence, DocumentError> {
        let expected_depth = parent.depth().checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(child),
                Some(edge_offset),
            )
        })?;
        if depth != expected_depth {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(child),
                Some(edge_offset),
            ));
        }

        let discovered = self
            .nodes
            .len()
            .checked_add(pending_count)
            .and_then(|count| count.checked_add(1))
            .and_then(|count| u64::try_from(count).ok())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child),
                    Some(edge_offset),
                )
            })?;
        if discovered > self.tree_limits.max_nodes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeNodes,
                self.tree_limits.max_nodes(),
                discovered.saturating_sub(1),
                1,
                child,
                Some(edge_offset),
            ));
        }
        if u64::from(depth) > self.tree_limits.max_depth() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeDepth,
                self.tree_limits.max_depth(),
                u64::from(depth.saturating_sub(1)),
                1,
                child,
                Some(edge_offset),
            ));
        }

        Ok(PageIndexNodeEvidence::new(
            child,
            Some(parent.reference()),
            edge_offset,
            depth,
        ))
    }

    /// Validates that a handle belongs to this source, revision, Catalog, and logical order.
    pub fn validate_handle(&self, handle: PageHandle) -> Result<(), DocumentError> {
        let resolved_mismatch = self
            .page(handle.index)
            .is_some_and(|object| object != handle.object);
        if handle.catalog != self.catalog
            || handle.page_count != self.page_count
            || handle.index >= self.page_count
            || resolved_mismatch
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::StalePageHandle,
                Some(handle.object),
                None,
            ));
        }
        Ok(())
    }

    /// Builds the exact Page-to-root inheritance chain for materialization-owned traversal.
    pub(crate) fn inheritance_chain(
        &self,
        handle: PageHandle,
        max_depth: u64,
        max_retained_bytes: u64,
    ) -> Result<Vec<ObjectRef>, DocumentError> {
        self.validate_handle(handle)?;
        if self.page(handle.index) != Some(handle.object) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::StalePageHandle,
                Some(handle.object),
                None,
            ));
        }
        let leaf = self.node(handle.object).ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(handle.object), None)
        })?;
        let required_depth = u64::from(leaf.depth());
        if required_depth > max_depth {
            return Err(DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationAncestors,
                max_depth,
                0,
                required_depth,
                handle.object,
                Some(leaf.edge_offset()),
            ));
        }
        let required_capacity = usize::try_from(required_depth).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(handle.object),
                Some(leaf.edge_offset()),
            )
        })?;
        let required_bytes = u64::try_from(required_capacity)
            .ok()
            .and_then(|capacity| {
                capacity.checked_mul(u64::try_from(mem::size_of::<ObjectRef>()).ok()?)
            })
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(handle.object),
                    Some(leaf.edge_offset()),
                )
            })?;
        if required_bytes > max_retained_bytes {
            return Err(DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationStateBytes,
                max_retained_bytes,
                0,
                required_bytes,
                handle.object,
                Some(leaf.edge_offset()),
            ));
        }
        let mut chain = Vec::new();
        chain.try_reserve_exact(required_capacity).map_err(|_| {
            DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationStateBytes,
                max_retained_bytes,
                0,
                required_bytes,
                handle.object,
                Some(leaf.edge_offset()),
            )
        })?;
        let retained_bytes = u64::try_from(chain.capacity())
            .ok()
            .and_then(|capacity| {
                capacity.checked_mul(u64::try_from(mem::size_of::<ObjectRef>()).ok()?)
            })
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(handle.object),
                    Some(leaf.edge_offset()),
                )
            })?;
        if retained_bytes > max_retained_bytes {
            return Err(DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationStateBytes,
                max_retained_bytes,
                0,
                retained_bytes,
                handle.object,
                Some(leaf.edge_offset()),
            ));
        }

        let mut current = Some(handle.object);
        let mut expected_depth = leaf.depth();
        while let Some(reference) = current {
            let node = self.node(reference).ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), None)
            })?;
            if node.depth() != expected_depth {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(node.edge_offset()),
                ));
            }
            chain.push(reference);
            match node.parent() {
                Some(parent) => {
                    expected_depth = expected_depth.checked_sub(1).ok_or_else(|| {
                        DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(reference),
                            Some(node.edge_offset()),
                        )
                    })?;
                    current = Some(parent);
                }
                None if reference == self.catalog.pages() && expected_depth == 1 => {
                    current = None;
                }
                None => {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(node.edge_offset()),
                    ));
                }
            }
        }
        if chain.len() != required_capacity {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(handle.object),
                Some(leaf.edge_offset()),
            ));
        }
        Ok(chain)
    }

    #[allow(
        dead_code,
        reason = "sealed dense-order admission remains the M2-01 compatibility boundary"
    )]
    pub(crate) fn admit(
        authority: &AttestedRevisionIndex,
        order: ValidatedPageOrder,
        limits: PageIndexLimits,
    ) -> Result<Self, DocumentError> {
        let root = authority.root();
        let root_offset = authority.attestation(root)?.xref_offset();
        let catalog = order.catalog;
        if catalog.snapshot() != authority.snapshot()
            || catalog.revision_id() != authority.revision_id()
            || catalog.revision_startxref() != authority.startxref()
            || catalog.root() != root
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::AttestedObjectEvidenceMismatch,
                Some(root),
                Some(root_offset),
            ));
        }

        let page_count = u64::try_from(order.pages.len()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(root_offset),
            )
        })?;
        if page_count > limits.max_pages() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreePages,
                limits.max_pages(),
                0,
                page_count,
                root,
                Some(root_offset),
            ));
        }
        if order.stats.pages() != page_count {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(root_offset),
            ));
        }

        for reference in &order.pages {
            authority.attestation(*reference)?;
        }

        let page_count = u32::try_from(page_count).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(root_offset),
            )
        })?;
        let mut segments = Vec::new();
        segments
            .try_reserve_exact(order.pages.len())
            .map_err(|_| page_index_resource(limits, root, Some(root_offset), u64::MAX))?;
        for (index, reference) in order.pages.into_iter().enumerate() {
            let start_index = u32::try_from(index).map_err(|_| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(root_offset),
                )
            })?;
            segments.push(PageSegmentSummary::page(
                start_index,
                reference,
                catalog.pages(),
                2,
            ));
        }
        Self::from_segments(
            catalog,
            page_count,
            segments,
            Vec::new(),
            PageIndexStats::from_complete_tree(order.stats),
            PageTreeLimits::default(),
            limits,
            root,
            Some(root_offset),
        )
    }

    #[allow(
        dead_code,
        reason = "complete M1 scalar-proof admission remains available beside cold lazy bootstrap"
    )]
    pub(crate) fn from_page_count(
        count: PageCount,
        limits: PageIndexLimits,
    ) -> Result<Self, DocumentError> {
        let catalog = count.catalog();
        let page_count = u32::try_from(count.page_count()).map_err(|_| {
            DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreePages,
                limits.max_pages(),
                0,
                count.page_count(),
                catalog.pages(),
                None,
            )
        })?;
        if u64::from(page_count) > limits.max_pages() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreePages,
                limits.max_pages(),
                0,
                u64::from(page_count),
                catalog.pages(),
                None,
            ));
        }

        let mut segments = Vec::new();
        segments
            .try_reserve_exact(1)
            .map_err(|_| page_index_resource(limits, catalog.pages(), None, u64::MAX))?;
        segments.push(PageSegmentSummary::pages(
            0,
            page_count,
            catalog.pages(),
            None,
            1,
            PageSegmentEvidence::CompleteSubtree,
            None,
            None,
        ));
        Self::from_segments(
            catalog,
            page_count,
            segments,
            Vec::new(),
            PageIndexStats::from_complete_tree(count.stats()),
            PageTreeLimits::default(),
            limits,
            catalog.pages(),
            None,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "cold admission keeps every source, range, limit, and retained-proof input explicit"
    )]
    pub(crate) fn from_lazy_root(
        catalog: StrictCatalog,
        page_count: u32,
        count_offset: u64,
        children: Vec<PageIndexChild>,
        mut nodes: Vec<PageIndexNodeEvidence>,
        mut stats: PageIndexStats,
        tree_limits: PageTreeLimits,
        index_limits: PageIndexLimits,
        root_offset: Option<u64>,
    ) -> Result<Self, DocumentError> {
        let root = catalog.pages();
        let page_count_u64 = u64::from(page_count);
        let max_pages = tree_limits.max_pages().min(index_limits.max_pages());
        if page_count_u64 > max_pages {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreePages,
                max_pages,
                0,
                page_count_u64,
                root,
                Some(count_offset),
            ));
        }
        if tree_limits.max_depth() < 1 {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeDepth,
                tree_limits.max_depth(),
                0,
                1,
                root,
                root_offset,
            ));
        }
        let child_count = u64::try_from(children.len()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(count_offset),
            )
        })?;
        if child_count > tree_limits.max_kids_per_node() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeKids,
                tree_limits.max_kids_per_node(),
                0,
                child_count,
                root,
                Some(count_offset),
            ));
        }
        if child_count > 0 && tree_limits.max_depth() < 2 {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeDepth,
                tree_limits.max_depth(),
                1,
                1,
                root,
                Some(count_offset),
            ));
        }
        let expected_nodes = children.len().checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(count_offset),
            )
        })?;
        if nodes.len() != expected_nodes {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(count_offset),
            ));
        }
        let discovered = u64::try_from(nodes.len()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(count_offset),
            )
        })?;
        if discovered > tree_limits.max_nodes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeNodes,
                tree_limits.max_nodes(),
                0,
                discovered,
                root,
                Some(count_offset),
            ));
        }
        let Some(root_node) = nodes.first() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                root_offset,
            ));
        };
        if root_node.reference() != root || root_node.parent().is_some() || root_node.depth() != 1 {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                root_offset,
            ));
        }
        for (child, node) in children.iter().zip(nodes.iter().skip(1)) {
            if child.reference() == root {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::PageTreeCycle,
                    Some(root),
                    Some(child.edge_offset()),
                ));
            }
            if node.reference() != child.reference()
                || node.parent() != Some(root)
                || node.edge_offset() != child.edge_offset()
                || node.depth() != 2
            {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference()),
                    Some(child.edge_offset()),
                ));
            }
        }
        nodes.sort_unstable_by_key(PageIndexNodeEvidence::reference);
        let complete_tree_proof = page_count == 0 && children.is_empty();

        let mut segments = Vec::new();
        segments
            .try_reserve_exact(1)
            .map_err(|_| page_index_resource(index_limits, root, root_offset, u64::MAX))?;
        segments.push(PageSegmentSummary::pages(
            0,
            page_count,
            root,
            None,
            1,
            PageSegmentEvidence::DeclaredCount,
            Some(count_offset),
            Some(children),
        ));
        stats.complete_tree_proof = complete_tree_proof;
        Self::from_segments(
            catalog,
            page_count,
            segments,
            nodes,
            stats,
            tree_limits,
            index_limits,
            root,
            root_offset.or(Some(count_offset)),
        )
    }

    pub(crate) fn binding_matches(&self, authority: &AttestedRevisionIndex) -> bool {
        self.catalog.snapshot() == authority.snapshot()
            && self.catalog.revision_id() == authority.revision_id()
            && self.catalog.revision_startxref() == authority.startxref()
            && self.catalog.root() == authority.root()
    }

    pub(crate) fn segment_containing(&self, index: u32) -> Option<(usize, &PageSegmentSummary)> {
        if index >= self.page_count {
            return None;
        }
        self.segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| index >= segment.start_index() && index < segment.end_index())
            .max_by_key(|(_, segment)| {
                (
                    segment.depth(),
                    u8::from(segment.kind() == PageIndexSegmentKind::Page),
                )
            })
    }

    pub(crate) fn refine(
        &self,
        segment_index: usize,
        mut expanded: PageSegmentSummary,
        replacements: Vec<PageSegmentSummary>,
        new_nodes: Vec<PageIndexNodeEvidence>,
    ) -> Result<Self, DocumentError> {
        let Some(replaced) = self.segments.get(segment_index) else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.catalog.pages()),
                None,
            ));
        };
        if replaced.kind() != PageIndexSegmentKind::Pages
            || expanded.kind() != PageIndexSegmentKind::Pages
            || expanded.object() != replaced.object()
            || expanded.parent() != replaced.parent()
            || expanded.depth() != replaced.depth()
            || expanded.start_index() != replaced.start_index()
            || expanded.page_count() != replaced.page_count()
            || expanded.children().is_none()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(replaced.object()),
                replaced.count_offset(),
            ));
        }
        let expanded_children = expanded
            .children()
            .expect("validated expanded Pages summary retains direct children");
        if replacements.len() != expanded_children.len() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(replaced.object()),
                expanded.count_offset().or(replaced.count_offset()),
            ));
        }
        let child_depth = replaced.depth().checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(replaced.object()),
                expanded.count_offset().or(replaced.count_offset()),
            )
        })?;
        let mut cursor = replaced.start_index();
        for (edge, replacement) in expanded_children.iter().zip(&replacements) {
            let end_index = replacement
                .start_index()
                .checked_add(replacement.page_count())
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(replacement.object()),
                        replacement.count_offset().or(Some(edge.edge_offset())),
                    )
                })?;
            let shape_valid = match replacement.kind() {
                PageIndexSegmentKind::Page => {
                    replacement.page_count() == 1
                        && replacement.evidence() == PageSegmentEvidence::ExactPage
                        && replacement.children().is_none()
                        && replacement.count_offset().is_none()
                }
                PageIndexSegmentKind::Pages => replacement.children().is_some(),
            };
            if !shape_valid
                || replacement.object() != edge.reference()
                || replacement.parent() != Some(replaced.object())
                || replacement.depth() != child_depth
                || replacement.start_index() != cursor
                || end_index > replaced.end_index()
            {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(replacement.object()),
                    replacement.count_offset().or(Some(edge.edge_offset())),
                ));
            }
            cursor = end_index;
        }
        if cursor != replaced.end_index() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageTreeCountMismatch,
                Some(replaced.object()),
                expanded.count_offset().or(replaced.count_offset()),
            ));
        }

        let mut validated_new_nodes = new_nodes;
        validated_new_nodes.sort_unstable_by_key(PageIndexNodeEvidence::reference);
        if let Some(duplicate) = validated_new_nodes
            .windows(2)
            .find(|pair| pair[0].reference() == pair[1].reference())
            .map(|pair| pair[1])
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicatePageTreeNode,
                Some(duplicate.reference()),
                Some(duplicate.edge_offset()),
            ));
        }
        let discovered = self
            .nodes
            .len()
            .checked_add(validated_new_nodes.len())
            .and_then(|count| u64::try_from(count).ok())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(replaced.object()),
                    expanded.count_offset().or(replaced.count_offset()),
                )
            })?;
        if discovered > self.tree_limits.max_nodes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeNodes,
                self.tree_limits.max_nodes(),
                u64::try_from(self.nodes.len()).unwrap_or(u64::MAX),
                u64::try_from(validated_new_nodes.len()).unwrap_or(u64::MAX),
                replaced.object(),
                expanded.count_offset().or(replaced.count_offset()),
            ));
        }
        for node in &validated_new_nodes {
            if self.node(node.reference()).is_some() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::DuplicatePageTreeNode,
                    Some(node.reference()),
                    Some(node.edge_offset()),
                ));
            }
            let parent = node.parent().ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(node.reference()),
                    Some(node.edge_offset()),
                )
            })?;
            let parent_node = self.node(parent).or_else(|| {
                validated_new_nodes
                    .binary_search_by_key(&parent, PageIndexNodeEvidence::reference)
                    .ok()
                    .map(|index| validated_new_nodes[index])
            });
            let Some(parent_node) = parent_node else {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(parent),
                    Some(node.edge_offset()),
                ));
            };
            if parent_node
                .depth()
                .checked_add(1)
                .is_none_or(|depth| depth != node.depth())
                || u64::from(node.depth()) > self.tree_limits.max_depth()
            {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(node.reference()),
                    Some(node.edge_offset()),
                ));
            }
        }
        let node_len = self
            .nodes
            .len()
            .checked_add(validated_new_nodes.len())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(replaced.object()),
                    expanded.count_offset().or(replaced.count_offset()),
                )
            })?;
        let mut nodes = Vec::new();
        nodes.try_reserve_exact(node_len).map_err(|_| {
            page_index_resource(
                self.limits,
                replaced.object(),
                expanded.count_offset().or(replaced.count_offset()),
                u64::MAX,
            )
        })?;
        merge_sorted_nodes(&mut nodes, &self.nodes, &validated_new_nodes);
        for (edge, replacement) in expanded_children.iter().zip(&replacements) {
            if !node_matches_in(
                &nodes,
                replacement.object(),
                Some(replaced.object()),
                edge.edge_offset(),
                child_depth,
            ) {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(replacement.object()),
                    Some(edge.edge_offset()),
                ));
            }
        }

        let new_len = self
            .segments
            .len()
            .checked_add(replacements.len())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(replaced.object()),
                    replaced.count_offset(),
                )
            })?;
        let mut segments = Vec::new();
        segments.try_reserve_exact(new_len).map_err(|_| {
            page_index_resource(
                self.limits,
                replaced.object(),
                replaced.count_offset(),
                u64::MAX,
            )
        })?;
        segments.extend(self.segments[..segment_index].iter().cloned());
        expanded.evidence = PageSegmentEvidence::ValidatedPartition;
        segments.push(expanded);
        segments.extend(self.segments[segment_index + 1..].iter().cloned());
        segments.extend(replacements);
        Self::from_segments(
            self.catalog,
            self.page_count,
            segments,
            nodes,
            self.stats,
            self.tree_limits,
            self.limits,
            replaced.object(),
            replaced.count_offset(),
        )
    }

    pub(crate) fn mint_handle(&self, index: u32, object: ObjectRef) -> PageHandle {
        let page_count_evidence = self
            .segments
            .iter()
            .find(|segment| {
                segment.object() == self.catalog.pages()
                    && segment.parent().is_none()
                    && segment.kind() == PageIndexSegmentKind::Pages
            })
            .map_or(
                PageSegmentEvidence::CompleteSubtree,
                PageSegmentSummary::evidence,
            );
        PageHandle {
            catalog: self.catalog,
            page_count: self.page_count,
            page_count_evidence,
            index,
            object,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "sealed admission keeps retained proof, bound limits, and diagnostic anchor explicit"
    )]
    fn from_segments(
        catalog: StrictCatalog,
        page_count: u32,
        mut segments: Vec<PageSegmentSummary>,
        nodes: Vec<PageIndexNodeEvidence>,
        stats: PageIndexStats,
        tree_limits: PageTreeLimits,
        limits: PageIndexLimits,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<Self, DocumentError> {
        let discovered = u64::try_from(nodes.len()).map_err(|_| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
        })?;
        if discovered > tree_limits.max_nodes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeNodes,
                tree_limits.max_nodes(),
                0,
                discovered,
                reference,
                offset,
            ));
        }
        if nodes
            .windows(2)
            .any(|pair| pair[0].reference() >= pair[1].reference())
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                offset,
            ));
        }
        let segment_order = build_segment_order(&segments, limits, reference, offset)?;
        recompute_segment_evidence(&mut segments, &segment_order)?;
        let retained_index_bytes = retained_page_index_bytes(&segments, &segment_order, &nodes)
            .ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
            })?;
        if retained_index_bytes > limits.max_retained_index_bytes() {
            return Err(page_index_resource(
                limits,
                reference,
                offset,
                retained_index_bytes,
            ));
        }
        Ok(Self {
            catalog,
            page_count,
            segments: Arc::new(segments),
            segment_order: Arc::new(segment_order),
            nodes: Arc::new(nodes),
            retained_index_bytes,
            stats,
            tree_limits,
            limits,
        })
    }
}

/// Sealed ordered leaf identities emitted only after a page-tree traversal succeeds.
#[allow(
    dead_code,
    reason = "sealed dense-order admission remains available beside lazy M2 refinement"
)]
pub(crate) struct ValidatedPageOrder {
    catalog: StrictCatalog,
    pages: Vec<ObjectRef>,
    stats: PageTreeStats,
}

impl ValidatedPageOrder {
    /// Packages a completed traversal result for bounded page-index admission.
    #[allow(
        dead_code,
        reason = "sealed dense-order admission remains available beside lazy M2 refinement"
    )]
    pub(crate) const fn new(
        catalog: StrictCatalog,
        pages: Vec<ObjectRef>,
        stats: PageTreeStats,
    ) -> Self {
        Self {
            catalog,
            pages,
            stats,
        }
    }
}

fn merge_sorted_nodes(
    destination: &mut Vec<PageIndexNodeEvidence>,
    published: &[PageIndexNodeEvidence],
    pending: &[PageIndexNodeEvidence],
) {
    let mut published_index = 0;
    let mut pending_index = 0;
    while published_index < published.len() && pending_index < pending.len() {
        if published[published_index].reference() < pending[pending_index].reference() {
            destination.push(published[published_index]);
            published_index += 1;
        } else {
            destination.push(pending[pending_index]);
            pending_index += 1;
        }
    }
    destination.extend_from_slice(&published[published_index..]);
    destination.extend_from_slice(&pending[pending_index..]);
}

fn node_matches_in(
    nodes: &[PageIndexNodeEvidence],
    reference: ObjectRef,
    parent: Option<ObjectRef>,
    edge_offset: u64,
    depth: u32,
) -> bool {
    nodes
        .binary_search_by_key(&reference, PageIndexNodeEvidence::reference)
        .ok()
        .is_some_and(|index| {
            let node = nodes[index];
            node.reference() == reference
                && node.parent() == parent
                && node.edge_offset() == edge_offset
                && node.depth() == depth
        })
}

fn build_segment_order(
    segments: &[PageSegmentSummary],
    limits: PageIndexLimits,
    reference: ObjectRef,
    offset: Option<u64>,
) -> Result<Vec<usize>, DocumentError> {
    let mut order = Vec::new();
    order
        .try_reserve_exact(segments.len())
        .map_err(|_| page_index_resource(limits, reference, offset, u64::MAX))?;
    order.extend(0..segments.len());
    order.sort_unstable_by_key(|index| segments[*index].object());
    if order
        .windows(2)
        .any(|pair| segments[pair[0]].object() == segments[pair[1]].object())
    {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(reference),
            offset,
        ));
    }
    Ok(order)
}

fn segment_index_by_reference(
    segments: &[PageSegmentSummary],
    order: &[usize],
    reference: ObjectRef,
) -> Option<usize> {
    order
        .binary_search_by_key(&reference, |index| segments[*index].object())
        .ok()
        .map(|index| order[index])
}

fn recompute_segment_evidence(
    segments: &mut [PageSegmentSummary],
    segment_order: &[usize],
) -> Result<(), DocumentError> {
    if segment_order.len() != segments.len() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InternalState,
            None,
            None,
        ));
    }
    for segment in segments.iter() {
        if segment.kind() == PageIndexSegmentKind::Page
            && (segment.page_count() != 1
                || segment.evidence() != PageSegmentEvidence::ExactPage
                || segment.children().is_some()
                || segment.count_offset().is_some())
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(segment.object()),
                segment.count_offset(),
            ));
        }
    }

    for index in (0..segments.len()).rev() {
        if segments[index].kind() != PageIndexSegmentKind::Pages {
            continue;
        }
        let Some(children) = segments[index].children.clone() else {
            continue;
        };
        let object = segments[index].object();
        let count_offset = segments[index].count_offset();
        if children.is_empty() {
            if segments[index].page_count() != 0 {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::PageTreeCountMismatch,
                    Some(object),
                    count_offset,
                ));
            }
            segments[index].evidence = PageSegmentEvidence::CompleteSubtree;
            continue;
        }

        let child_depth = segments[index].depth().checked_add(1).ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(object), count_offset)
        })?;
        let mut cursor = segments[index].start_index();
        let end_index = segments[index].end_index();
        let mut found = 0_usize;
        let mut complete = true;
        for edge in children.iter() {
            let Some(child_index) =
                segment_index_by_reference(segments, segment_order, edge.reference())
            else {
                continue;
            };
            let child = &segments[child_index];
            if child_index <= index
                || child.parent() != Some(object)
                || child.depth() != child_depth
                || child.start_index() != cursor
                || child.end_index() > end_index
            {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(edge.reference()),
                    Some(edge.edge_offset()),
                ));
            }
            cursor = child.end_index();
            found = found.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(object),
                    count_offset,
                )
            })?;
            complete &= matches!(
                child.evidence(),
                PageSegmentEvidence::ExactPage | PageSegmentEvidence::CompleteSubtree
            );
        }
        if found == 0 {
            segments[index].evidence = PageSegmentEvidence::DeclaredCount;
            continue;
        }
        if found != children.len() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(object),
                count_offset,
            ));
        }
        if cursor != end_index {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageTreeCountMismatch,
                Some(object),
                count_offset,
            ));
        }
        segments[index].evidence = if complete {
            PageSegmentEvidence::CompleteSubtree
        } else {
            PageSegmentEvidence::ValidatedPartition
        };
    }
    Ok(())
}

fn retained_page_index_bytes(
    segments: &Vec<PageSegmentSummary>,
    segment_order: &Vec<usize>,
    nodes: &Vec<PageIndexNodeEvidence>,
) -> Option<u64> {
    let summaries = u64::try_from(segments.capacity())
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<PageSegmentSummary>()).ok()?)?;
    let discovered = u64::try_from(nodes.capacity())
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<PageIndexNodeEvidence>()).ok()?)?;
    let segment_lookup = u64::try_from(segment_order.capacity())
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<usize>()).ok()?)?;
    segments.iter().try_fold(
        summaries
            .checked_add(segment_lookup)?
            .checked_add(discovered)?,
        |total, segment| {
            let child_bytes = segment.children.as_ref().map_or(Some(0), |children| {
                u64::try_from(children.capacity())
                    .ok()?
                    .checked_mul(u64::try_from(mem::size_of::<PageIndexChild>()).ok()?)
            })?;
            total.checked_add(child_bytes)
        },
    )
}

fn page_index_resource(
    limits: PageIndexLimits,
    reference: ObjectRef,
    offset: Option<u64>,
    attempted: u64,
) -> DocumentError {
    DocumentError::page_tree_resource(
        DocumentLimitKind::PageIndexBytes,
        limits.max_retained_index_bytes(),
        0,
        attempted,
        reference,
        offset,
    )
}

#[cfg(test)]
mod tests {
    use pdf_rs_bytes::{
        ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint,
        SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
        SourceValidatorKind,
    };
    use pdf_rs_object::ObjectLimits;
    use pdf_rs_syntax::SyntaxLimits;
    use pdf_rs_xref::{
        NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
        XrefSection,
    };

    use super::*;
    use crate::{
        AttestRevisionJob, CandidateRevisionIndex, DocumentLimits,
        NeverCancelled as DocumentNeverCancelled, PageCount, PageCountPoll, PageTreeJobContext,
        PageTreeLimitConfig, PageTreeLimits, RevisionAttestationJobContext,
        RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
    };

    const REVISION_ID: RevisionId = RevisionId::new(51);

    struct Fixture {
        bytes: Vec<u8>,
        snapshot: SourceSnapshot,
    }

    fn snapshot(len: u64) -> SourceSnapshot {
        SourceSnapshot::new(
            SourceIdentity::new(SourceStableId::new([0xb2; 32]), SourceRevision::new(8)),
            Some(len),
            SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x5d; 32]),
        )
    }

    fn nested_fixture() -> Fixture {
        let bodies: &[(u32, &[u8])] = &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 3 >>\nendobj\n",
            ),
            (3, b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n"),
            (
                4,
                b"4 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R 6 0 R] /Count 2 >>\nendobj\n",
            ),
            (5, b"5 0 obj\n<< /Type /Page /Parent 4 0 R >>\nendobj\n"),
            (6, b"6 0 obj\n<< /Type /Page /Parent 4 0 R >>\nendobj\n"),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for &(number, body) in bodies {
            offsets.push((number, bytes.len()));
            bytes.extend_from_slice(body);
        }
        let startxref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        for number in 1..7 {
            let offset = offsets
                .iter()
                .find(|(candidate, _)| *candidate == number)
                .map(|(_, offset)| *offset)
                .expect("every fixture object has one xref row");
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
                .as_bytes(),
        );
        Fixture {
            snapshot: snapshot(u64::try_from(bytes.len()).unwrap()),
            bytes,
        }
    }

    fn store(fixture: &Fixture) -> RangeStore {
        let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
        let range = ByteRange::new(0, u64::try_from(fixture.bytes.len()).unwrap()).unwrap();
        store
            .supply(RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone()).unwrap())
            .unwrap();
        store
    }

    fn xref(fixture: &Fixture) -> XrefSection {
        let source = store(fixture);
        let mut job = OpenXrefJob::new(
            fixture.snapshot,
            XrefJobContext::new(
                JobId::new(5_001),
                ResumeCheckpoint::new(5_002),
                ResumeCheckpoint::new(5_003),
            ),
            XrefLimits::default(),
            SyntaxLimits::default(),
        )
        .unwrap();
        match job.poll(&source, &XrefNeverCancelled) {
            XrefPoll::Ready(value) => value,
            XrefPoll::Pending { .. } => panic!("resident fixture must not suspend"),
            XrefPoll::Failed(error) => panic!("fixture xref failed: {error}"),
        }
    }

    fn authority(fixture: &Fixture) -> AttestedRevisionIndex {
        let source = store(fixture);
        let candidate = CandidateRevisionIndex::from_xref(
            &xref(fixture),
            REVISION_ID,
            DocumentLimits::default(),
            &DocumentNeverCancelled,
        )
        .unwrap();
        let mut job = AttestRevisionJob::new(
            candidate,
            RevisionAttestationJobContext::new(
                JobId::new(5_101),
                ResumeCheckpoint::new(5_102),
                ResumeCheckpoint::new(5_103),
                ResumeCheckpoint::new(5_104),
                RequestPriority::Metadata,
            ),
            RevisionAttestationLimits::default(),
            ObjectLimits::default(),
            SyntaxLimits::default(),
        )
        .unwrap();
        match job.poll(&source, &DocumentNeverCancelled) {
            RevisionAttestationPoll::Ready(value) => value,
            RevisionAttestationPoll::Pending { .. } => panic!("resident fixture must not suspend"),
            RevisionAttestationPoll::Failed(error) => panic!("fixture attestation failed: {error}"),
        }
    }

    fn page_count(authority: &AttestedRevisionIndex, fixture: &Fixture) -> PageCount {
        let source = store(fixture);
        let limits = PageTreeLimits::validate(PageTreeLimitConfig {
            max_nodes: 8,
            max_depth: 4,
            max_pages: 4,
            max_kids_per_node: 4,
            max_total_object_read_bytes: 1 << 20,
            max_total_object_parse_bytes: 1 << 20,
            max_retained_traversal_bytes: 4 << 10,
        })
        .unwrap();
        let mut job = authority
            .count_pages(
                PageTreeJobContext::new(
                    JobId::new(5_201),
                    ResumeCheckpoint::new(5_202),
                    ResumeCheckpoint::new(5_203),
                    RequestPriority::VisiblePage,
                ),
                limits,
            )
            .unwrap();
        match job.poll(&source, &DocumentNeverCancelled) {
            PageCountPoll::Ready(value) => value,
            PageCountPoll::Pending { .. } => panic!("resident fixture must not suspend"),
            PageCountPoll::Failed(error) => panic!("fixture page count failed: {error}"),
        }
    }

    fn object_ref(number: u32) -> ObjectRef {
        ObjectRef::new(number, 0).unwrap()
    }

    #[test]
    fn sealed_order_admission_retains_identity_order_stats_and_capacity() {
        let fixture = nested_fixture();
        let authority = authority(&fixture);
        let count = page_count(&authority, &fixture);
        let order = ValidatedPageOrder::new(
            count.catalog(),
            vec![object_ref(3), object_ref(5), object_ref(6)],
            count.stats(),
        );
        let index = PageIndex::admit(&authority, order, PageIndexLimits::new(3, 1024).unwrap())
            .expect("validated order must admit");

        assert_eq!(index.catalog(), count.catalog());
        assert_eq!(index.len(), 3);
        assert!(!index.is_empty());
        assert!(index.is_complete());
        assert_eq!(index.segments().len(), 3);
        assert_eq!(
            index
                .segments()
                .iter()
                .map(PageSegmentSummary::object)
                .collect::<Vec<_>>(),
            [object_ref(3), object_ref(5), object_ref(6)]
        );
        assert_eq!(index.page(0), Some(object_ref(3)));
        assert_eq!(index.page(2), Some(object_ref(6)));
        assert_eq!(index.page(3), None);
        let handle = index.mint_handle(1, object_ref(5));
        assert_eq!(handle.index(), 1);
        assert_eq!(handle.object(), object_ref(5));
        assert_eq!(handle.snapshot(), fixture.snapshot);
        index.validate_handle(handle).unwrap();
        assert_eq!(
            index.stats(),
            PageIndexStats::from_complete_tree(count.stats())
        );
        assert!(index.retained_index_bytes() > 0);
        assert_eq!(index.clone(), index);
    }

    #[test]
    fn page_count_admission_starts_with_one_validated_root_segment() {
        let fixture = nested_fixture();
        let authority = authority(&fixture);
        let count = page_count(&authority, &fixture);
        let index = PageIndex::from_page_count(count, PageIndexLimits::new(3, 1024).unwrap())
            .expect("completed page-count proof must admit a root frontier");

        assert_eq!(index.len(), 3);
        assert!(!index.is_complete());
        assert_eq!(index.page(0), None);
        assert_eq!(index.segments().len(), 1);
        let root = &index.segments()[0];
        assert_eq!(root.kind(), PageIndexSegmentKind::Pages);
        assert_eq!(root.object(), object_ref(2));
        assert_eq!(root.parent(), None);
        assert_eq!(root.start_index(), 0);
        assert_eq!(root.end_index(), 3);
        assert_eq!(root.declared_count(), 3);
        assert_eq!(root.evidence(), PageSegmentEvidence::CompleteSubtree);
        assert_eq!(root.validated_count(), Some(3));
        assert_eq!(root.retained_kid_count(), None);
    }

    #[test]
    fn admission_rejects_count_capacity_and_authority_mismatches() {
        let fixture = nested_fixture();
        let authority = authority(&fixture);
        let count = page_count(&authority, &fixture);

        let short = ValidatedPageOrder::new(
            count.catalog(),
            vec![object_ref(3), object_ref(5)],
            count.stats(),
        );
        assert_eq!(
            PageIndex::admit(&authority, short, PageIndexLimits::new(3, 1024).unwrap())
                .unwrap_err()
                .code(),
            DocumentErrorCode::InternalState
        );

        let over_pages = ValidatedPageOrder::new(
            count.catalog(),
            vec![object_ref(3), object_ref(5), object_ref(6)],
            count.stats(),
        );
        let error = PageIndex::admit(
            &authority,
            over_pages,
            PageIndexLimits::new(2, 1024).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        assert_eq!(
            error.limit().expect("page limit detail").kind(),
            DocumentLimitKind::PageTreePages
        );

        let tight_bytes = ValidatedPageOrder::new(
            count.catalog(),
            vec![object_ref(3), object_ref(5), object_ref(6)],
            count.stats(),
        );
        let error = PageIndex::admit(&authority, tight_bytes, PageIndexLimits::new(3, 1).unwrap())
            .unwrap_err();
        assert_eq!(
            error.limit().expect("byte limit detail").kind(),
            DocumentLimitKind::PageIndexBytes
        );

        let missing = ValidatedPageOrder::new(
            count.catalog(),
            vec![object_ref(3), object_ref(5), object_ref(7)],
            count.stats(),
        );
        assert_eq!(
            PageIndex::admit(&authority, missing, PageIndexLimits::new(3, 1024).unwrap())
                .unwrap_err()
                .code(),
            DocumentErrorCode::MissingObject
        );
    }

    #[test]
    fn limit_validation_is_nonzero_and_hard_bounded() {
        let defaults = PageIndexLimits::default();
        assert!(defaults.max_pages() > 0);
        assert!(defaults.max_retained_index_bytes() > 0);
        assert_eq!(
            PageIndexLimits::new(0, 1).unwrap_err().code(),
            DocumentErrorCode::InvalidLimits
        );
        assert_eq!(
            PageIndexLimits::new(1, 0).unwrap_err().code(),
            DocumentErrorCode::InvalidLimits
        );
        assert!(PageIndexLimits::new(HARD_MAX_PAGES, HARD_MAX_RETAINED_INDEX_BYTES).is_ok());
        assert_eq!(
            PageIndexLimits::new(HARD_MAX_PAGES + 1, 1)
                .unwrap_err()
                .code(),
            DocumentErrorCode::InvalidLimits
        );
    }
}
