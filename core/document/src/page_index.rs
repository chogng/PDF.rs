use std::mem;
use std::sync::Arc;

use pdf_rs_bytes::SourceSnapshot;
use pdf_rs_syntax::ObjectRef;

use crate::{
    AttestedRevisionIndex, DocumentError, DocumentErrorCode, DocumentLimitKind, PageCount,
    PageTreeStats, RevisionId, StrictCatalog,
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
    /// One validated Pages subtree that can be refined without rebuilding the whole tree.
    Pages,
}

/// One retained direct child edge of a validated Pages dictionary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

    pub(crate) const fn reference(self) -> ObjectRef {
        self.reference
    }

    pub(crate) const fn edge_offset(self) -> u64 {
        self.edge_offset
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

    /// Returns the dictionary Count value covered by the completed M1 page-tree proof.
    ///
    /// Leaf Page segments use the implicit value one.
    pub const fn declared_count(&self) -> u32 {
        self.declared_count
    }

    /// Returns the recomputed validated leaf count for this segment.
    pub const fn validated_count(&self) -> u32 {
        self.page_count
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
            declared_count: 1,
            count_offset: None,
            children: None,
        }
    }

    pub(crate) fn pages(
        start_index: u32,
        page_count: u32,
        object: ObjectRef,
        parent: Option<ObjectRef>,
        depth: u32,
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
            declared_count: page_count,
            count_offset,
            children: children.map(Arc::new),
        }
    }

    pub(crate) fn children(&self) -> Option<&[PageIndexChild]> {
        self.children.as_deref().map(Vec::as_slice)
    }
}

/// Source- and revision-bound identity of one validated logical Page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageHandle {
    catalog: StrictCatalog,
    page_count: u32,
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

    /// Returns the complete validated page count shared by sibling handles.
    pub const fn document_page_count(self) -> u32 {
        self.page_count
    }
}

/// Immutable source- and revision-bound logical page index.
///
/// Construction remains sealed behind a completed M1 page-tree proof. The initial frontier
/// contains only the validated root range; page lookup then returns refined immutable indices that
/// cache direct Kids and validated Count summaries along the requested paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageIndex {
    catalog: StrictCatalog,
    page_count: u32,
    segments: Arc<Vec<PageSegmentSummary>>,
    retained_index_bytes: u64,
    stats: PageTreeStats,
    limits: PageIndexLimits,
}

impl PageIndex {
    /// Returns the validated source- and revision-bound Catalog summary.
    pub const fn catalog(&self) -> StrictCatalog {
        self.catalog
    }

    /// Returns the complete validated leaf-page count.
    pub const fn len(&self) -> u32 {
        self.page_count
    }

    /// Reports whether the validated page tree contains no leaf pages.
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

    /// Reports whether every logical page has been refined to one exact leaf segment.
    pub fn is_complete(&self) -> bool {
        self.segments
            .iter()
            .filter(|segment| segment.kind() == PageIndexSegmentKind::Page)
            .count()
            == usize::try_from(self.page_count).unwrap_or(usize::MAX)
    }

    /// Returns allocator-reported retained segment and direct-child capacity.
    ///
    /// Fixed `Arc` and `Vec` headers are inline owner metadata and are not included.
    pub const fn retained_index_bytes(&self) -> u64 {
        self.retained_index_bytes
    }

    /// Returns the complete M1 traversal statistics that admitted this index.
    pub const fn stats(&self) -> PageTreeStats {
        self.stats
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
            order.stats,
            limits,
            root,
            Some(root_offset),
        )
    }

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
        if page_count > 0 {
            segments
                .try_reserve_exact(1)
                .map_err(|_| page_index_resource(limits, catalog.pages(), None, u64::MAX))?;
            segments.push(PageSegmentSummary::pages(
                0,
                page_count,
                catalog.pages(),
                None,
                1,
                None,
                None,
            ));
        }
        Self::from_segments(
            catalog,
            page_count,
            segments,
            count.stats(),
            limits,
            catalog.pages(),
            None,
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
        expanded: PageSegmentSummary,
        replacements: Vec<PageSegmentSummary>,
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
        let mut cursor = replaced.start_index();
        for replacement in &replacements {
            if replacement.page_count() == 0
                || replacement.start_index() != cursor
                || replacement.end_index() > replaced.end_index()
            {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(replaced.object()),
                    replaced.count_offset(),
                ));
            }
            cursor = replacement.end_index();
        }
        if cursor != replaced.end_index() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageTreeCountMismatch,
                Some(replaced.object()),
                replaced.count_offset(),
            ));
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
        segments.push(expanded);
        segments.extend(self.segments[segment_index + 1..].iter().cloned());
        segments.extend(replacements);
        Self::from_segments(
            self.catalog,
            self.page_count,
            segments,
            self.stats,
            self.limits,
            replaced.object(),
            replaced.count_offset(),
        )
    }

    pub(crate) const fn mint_handle(&self, index: u32, object: ObjectRef) -> PageHandle {
        PageHandle {
            catalog: self.catalog,
            page_count: self.page_count,
            index,
            object,
        }
    }

    fn from_segments(
        catalog: StrictCatalog,
        page_count: u32,
        segments: Vec<PageSegmentSummary>,
        stats: PageTreeStats,
        limits: PageIndexLimits,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<Self, DocumentError> {
        let retained_index_bytes = retained_page_index_bytes(&segments).ok_or_else(|| {
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
            retained_index_bytes,
            stats,
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

fn retained_page_index_bytes(segments: &Vec<PageSegmentSummary>) -> Option<u64> {
    let summaries = u64::try_from(segments.capacity())
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<PageSegmentSummary>()).ok()?)?;
    segments.iter().try_fold(summaries, |total, segment| {
        let child_bytes = segment.children.as_ref().map_or(Some(0), |children| {
            u64::try_from(children.capacity())
                .ok()?
                .checked_mul(u64::try_from(mem::size_of::<PageIndexChild>()).ok()?)
        })?;
        total.checked_add(child_bytes)
    })
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
        assert_eq!(index.stats(), count.stats());
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
        assert_eq!(root.validated_count(), 3);
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
