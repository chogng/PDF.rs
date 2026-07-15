use std::fmt;
use std::mem;

use pdf_rs_bytes::{ByteSource, DataTicket, ResumeCheckpoint, SmallRanges, SourceSnapshot};
use pdf_rs_object::{DecodedArray, DecodedDictionary, DecodedLocatedObject, DecodedObject};
use pdf_rs_syntax::{Located, ObjectRef, PdfArray, PdfDictionary, PdfString, SyntaxObject};

use crate::text_string::{TextStringMeasurement, decode_measured_text_string, measure_text_string};
use crate::{
    AcquiredObject, AcquiredObjectJobContext, AcquiredObjectPoll, AcquiredObjectStats,
    AcquiredObjectValue, AcquiredObjectWorkCaps, DecodedTextString, DocumentCancellation,
    DocumentError, DocumentErrorCode, DocumentLimitKind, EffectiveObjectLocator,
    OpenAcquiredObjectJob, OutlineLimits, OutlinePhase, OutlineTargetKind, PageTreeLimits,
    PageTreePhase, SourceAcquiredDocument,
};

const CANCELLATION_INTERVAL: usize = 256;

#[derive(Clone, Copy)]
enum ValueView<'value> {
    Physical(&'value Located<SyntaxObject>),
    Decoded(&'value DecodedLocatedObject),
}

impl<'value> ValueView<'value> {
    fn from_acquired(value: AcquiredObjectValue<'value>) -> Self {
        match value {
            AcquiredObjectValue::Uncompressed(value) => Self::Physical(value),
            AcquiredObjectValue::Compressed(value) => Self::Decoded(value),
        }
    }

    fn is_null(self) -> bool {
        match self {
            Self::Physical(value) => matches!(value.value(), SyntaxObject::Null),
            Self::Decoded(value) => matches!(value.value(), DecodedObject::Null),
        }
    }

    fn as_reference(self) -> Option<ObjectRef> {
        match self {
            Self::Physical(value) => value.value().as_reference(),
            Self::Decoded(value) => value.value().as_reference(),
        }
    }

    fn as_integer(self) -> Option<i64> {
        match self {
            Self::Physical(value) => value.value().as_integer(),
            Self::Decoded(value) => value.value().as_integer(),
        }
    }

    fn as_name(self) -> Option<&'value [u8]> {
        match self {
            Self::Physical(value) => match value.value() {
                SyntaxObject::Name(name) => Some(name.bytes()),
                _ => None,
            },
            Self::Decoded(value) => match value.value() {
                DecodedObject::Name(name) => Some(name.bytes()),
                _ => None,
            },
        }
    }

    fn as_string(self) -> Option<&'value PdfString> {
        match self {
            Self::Physical(value) => match value.value() {
                SyntaxObject::String(value) => Some(value),
                _ => None,
            },
            Self::Decoded(value) => match value.value() {
                DecodedObject::String(value) => Some(value),
                _ => None,
            },
        }
    }

    fn as_dictionary(self) -> Option<DictionaryView<'value>> {
        match self {
            Self::Physical(value) => value.value().as_dictionary().map(DictionaryView::Physical),
            Self::Decoded(value) => value.value().as_dictionary().map(DictionaryView::Decoded),
        }
    }

    fn as_array(self) -> Option<ArrayView<'value>> {
        match self {
            Self::Physical(value) => match value.value() {
                SyntaxObject::Array(value) => Some(ArrayView::Physical(value)),
                _ => None,
            },
            Self::Decoded(value) => match value.value() {
                DecodedObject::Array(value) => Some(ArrayView::Decoded(value)),
                _ => None,
            },
        }
    }

    fn is_direct_destination(self) -> bool {
        match self {
            Self::Physical(value) => matches!(
                value.value(),
                SyntaxObject::Array(_) | SyntaxObject::Name(_) | SyntaxObject::String(_)
            ),
            Self::Decoded(value) => matches!(
                value.value(),
                DecodedObject::Array(_) | DecodedObject::Name(_) | DecodedObject::String(_)
            ),
        }
    }
}

#[derive(Clone, Copy)]
enum DictionaryView<'dictionary> {
    Physical(&'dictionary PdfDictionary),
    Decoded(&'dictionary DecodedDictionary),
}

#[derive(Clone, Copy)]
enum ArrayView<'array> {
    Physical(&'array PdfArray),
    Decoded(&'array DecodedArray),
}

impl<'array> ArrayView<'array> {
    fn len(self) -> usize {
        match self {
            Self::Physical(array) => array.values().len(),
            Self::Decoded(array) => array.values().len(),
        }
    }

    fn value(self, index: usize) -> Option<ValueView<'array>> {
        match self {
            Self::Physical(array) => array.values().get(index).map(ValueView::Physical),
            Self::Decoded(array) => array.values().get(index).map(ValueView::Decoded),
        }
    }
}

struct Fields<'dictionary, const N: usize> {
    values: [Option<ValueView<'dictionary>>; N],
    duplicates: [bool; N],
}

fn collect_fields<'dictionary, const N: usize>(
    dictionary: DictionaryView<'dictionary>,
    keys: [&[u8]; N],
    reference: ObjectRef,
    source_offset: u64,
    cancellation: &dyn DocumentCancellation,
) -> Result<Fields<'dictionary, N>, DocumentError> {
    let mut fields = Fields {
        values: [None; N],
        duplicates: [false; N],
    };
    match dictionary {
        DictionaryView::Physical(dictionary) => {
            for (entry_index, entry) in dictionary.entries().iter().enumerate() {
                probe(cancellation, entry_index, reference, source_offset)?;
                record_field(
                    &mut fields,
                    keys,
                    entry.key().value().bytes(),
                    ValueView::Physical(entry.value()),
                );
            }
        }
        DictionaryView::Decoded(dictionary) => {
            for (entry_index, entry) in dictionary.entries().iter().enumerate() {
                probe(cancellation, entry_index, reference, source_offset)?;
                record_field(
                    &mut fields,
                    keys,
                    entry.key().bytes(),
                    ValueView::Decoded(entry.value()),
                );
            }
        }
    }
    Ok(fields)
}

fn record_field<'dictionary, const N: usize>(
    fields: &mut Fields<'dictionary, N>,
    keys: [&[u8]; N],
    key: &[u8],
    value: ValueView<'dictionary>,
) {
    for (index, expected) in keys.iter().enumerate() {
        if key != *expected {
            continue;
        }
        if fields.values[index].is_some() {
            fields.duplicates[index] = true;
        } else {
            fields.values[index] = Some(value);
        }
        break;
    }
}

fn reject_duplicate<const N: usize>(
    fields: &Fields<'_, N>,
    index: usize,
    reference: ObjectRef,
    source_offset: u64,
) -> Result<(), DocumentError> {
    if fields.duplicates.get(index).copied().unwrap_or(true) {
        return Err(DocumentError::for_code(
            DocumentErrorCode::DuplicateStructuralKey,
            Some(reference),
            Some(source_offset),
        ));
    }
    Ok(())
}

fn required<'dictionary, const N: usize>(
    fields: &Fields<'dictionary, N>,
    index: usize,
    reference: ObjectRef,
    source_offset: u64,
    code: DocumentErrorCode,
) -> Result<ValueView<'dictionary>, DocumentError> {
    fields
        .values
        .get(index)
        .copied()
        .flatten()
        .ok_or_else(|| DocumentError::for_code(code, Some(reference), Some(source_offset)))
}

fn optional_non_null<'dictionary, const N: usize>(
    fields: &Fields<'dictionary, N>,
    index: usize,
) -> Option<ValueView<'dictionary>> {
    fields
        .values
        .get(index)
        .copied()
        .flatten()
        .filter(|value| !value.is_null())
}

/// Source-bound Catalog summary produced without top-level-attestation casting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquiredCatalog {
    snapshot: SourceSnapshot,
    root: ObjectRef,
    pages: ObjectRef,
    root_revision_startxref: u64,
}

impl AcquiredCatalog {
    /// Returns the immutable acquired-chain snapshot.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the effective trailer root.
    pub const fn root(self) -> ObjectRef {
        self.root
    }

    /// Returns the exact page-tree root reference.
    pub const fn pages(self) -> ObjectRef {
        self.pages
    }

    /// Returns the winning revision anchor for the Catalog definition.
    pub const fn root_revision_startxref(self) -> u64 {
        self.root_revision_startxref
    }
}

struct ParsedCatalog {
    catalog: AcquiredCatalog,
    outlines: Option<ObjectRef>,
}

fn parse_catalog(
    owner: &SourceAcquiredDocument,
    object: &AcquiredObject<'_>,
    cancellation: &dyn DocumentCancellation,
) -> Result<ParsedCatalog, DocumentError> {
    let reference = object.reference();
    let source_offset = owner.object_source_offset(reference).ok_or_else(|| {
        DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), None)
    })?;
    if reference != owner.root() || object.snapshot() != owner.snapshot() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(reference),
            Some(source_offset),
        ));
    }
    let value = ValueView::from_acquired(object.value()?);
    let dictionary = value.as_dictionary().ok_or_else(|| {
        DocumentError::for_code(
            DocumentErrorCode::InvalidCatalog,
            Some(reference),
            Some(source_offset),
        )
    })?;
    let fields = collect_fields(
        dictionary,
        [
            b"Type".as_slice(),
            b"Pages".as_slice(),
            b"Outlines".as_slice(),
        ],
        reference,
        source_offset,
        cancellation,
    )?;
    for index in 0..3 {
        reject_duplicate(&fields, index, reference, source_offset)?;
    }
    let type_value = required(
        &fields,
        0,
        reference,
        source_offset,
        DocumentErrorCode::InvalidCatalog,
    )?;
    if type_value.as_name() != Some(b"Catalog".as_slice()) {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidCatalog,
            Some(reference),
            Some(source_offset),
        ));
    }
    let pages = required(
        &fields,
        1,
        reference,
        source_offset,
        DocumentErrorCode::InvalidCatalog,
    )?
    .as_reference()
    .ok_or_else(|| {
        DocumentError::for_code(
            DocumentErrorCode::InvalidCatalog,
            Some(reference),
            Some(source_offset),
        )
    })?;
    let outlines = match optional_non_null(&fields, 2) {
        Some(value) => Some(value.as_reference().ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InvalidOutlineDictionary,
                Some(reference),
                Some(source_offset),
            )
        })?),
        None => None,
    };
    let root_revision_startxref = owner
        .locator(reference.number())
        .map(EffectiveObjectLocator::provenance)
        .map(|provenance| provenance.revision_startxref())
        .ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(source_offset),
            )
        })?;
    Ok(ParsedCatalog {
        catalog: AcquiredCatalog {
            snapshot: owner.snapshot(),
            root: reference,
            pages,
            root_revision_startxref,
        },
        outlines,
    })
}

/// Deterministic acquired-chain page traversal work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AcquiredPageTreeStats {
    objects_started: u64,
    nodes_started: u64,
    pages: u64,
    max_depth: u64,
    max_kids_per_node: u64,
    object_read_bytes: u64,
    object_parse_bytes: u64,
    reserved_traversal_bytes: u64,
}

impl AcquiredPageTreeStats {
    /// Returns all acquired object children started, including the Catalog.
    pub const fn objects_started(self) -> u64 {
        self.objects_started
    }
    /// Returns all Page or Pages nodes started.
    pub const fn nodes_started(self) -> u64 {
        self.nodes_started
    }
    /// Returns validated Page leaves.
    pub const fn pages(self) -> u64 {
        self.pages
    }
    /// Returns the greatest page-tree depth started.
    pub const fn max_depth(self) -> u64 {
        self.max_depth
    }
    /// Returns the greatest direct Kids count.
    pub const fn max_kids_per_node(self) -> u64 {
        self.max_kids_per_node
    }
    /// Returns cumulative acquired-object exact reads.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }
    /// Returns cumulative framing, decode, and semantic parser work.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }
    /// Returns allocator-reported traversal capacity.
    pub const fn reserved_traversal_bytes(self) -> u64 {
        self.reserved_traversal_bytes
    }
}

/// Successful acquired-chain Catalog and page-count service result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquiredPageCount {
    catalog: AcquiredCatalog,
    page_count: u64,
    stats: AcquiredPageTreeStats,
}

impl AcquiredPageCount {
    /// Returns the acquired-chain Catalog summary.
    pub const fn catalog(self) -> AcquiredCatalog {
        self.catalog
    }
    /// Returns validated leaf Page count.
    pub const fn page_count(self) -> u64 {
        self.page_count
    }
    /// Returns deterministic traversal accounting.
    pub const fn stats(self) -> AcquiredPageTreeStats {
        self.stats
    }
}

/// Poll result for an acquired-chain page-count service job.
pub enum AcquiredPageCountPoll {
    /// Complete validated page count.
    Ready(AcquiredPageCount),
    /// The active object resolver requires exact source ranges.
    Pending {
        /// One-shot data-arrival ticket.
        ticket: DataTicket,
        /// Canonical exact ranges still missing.
        missing: SmallRanges,
        /// Exact lower checkpoint retained while waiting.
        checkpoint: ResumeCheckpoint,
    },
    /// Stable structured failure.
    Failed(DocumentError),
}

impl fmt::Debug for AcquiredPageCountPoll {
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
struct VisitPageNode {
    reference: ObjectRef,
    parent: Option<ObjectRef>,
    depth: u64,
    edge_source_offset: u64,
}

#[derive(Clone, Copy)]
enum PageWork {
    Visit(VisitPageNode),
    Finish {
        reference: ObjectRef,
        declared_pages: u64,
        pages_before: u64,
        source_offset: u64,
    },
}

#[derive(Clone, Copy)]
enum PageTarget {
    Catalog,
    Node(VisitPageNode),
}

struct PageChild<'owner> {
    job: OpenAcquiredObjectJob<'owner>,
    accounted: AcquiredObjectStats,
    reference: ObjectRef,
    source_offset: u64,
}

#[derive(Clone, Copy)]
enum ServiceState {
    Active,
    Ready,
    Failed,
}

/// One-shot acquired-chain Catalog and page-tree traversal.
pub struct CountAcquiredPagesJob<'owner> {
    owner: &'owner SourceAcquiredDocument,
    context: AcquiredObjectJobContext,
    limits: PageTreeLimits,
    catalog: Option<AcquiredCatalog>,
    work: Vec<PageWork>,
    seen_slots: Vec<u64>,
    seen_count: u64,
    active: Vec<ObjectRef>,
    current: Option<PageTarget>,
    child: Option<PageChild<'owner>>,
    stats: AcquiredPageTreeStats,
    state: ServiceState,
    terminal_error: DocumentError,
}

impl SourceAcquiredDocument {
    /// Creates a page-count job that borrows this complete source-acquired proof owner.
    pub fn count_acquired_pages(
        &self,
        context: AcquiredObjectJobContext,
        limits: PageTreeLimits,
    ) -> Result<CountAcquiredPagesJob<'_>, DocumentError> {
        CountAcquiredPagesJob::new(self, context, limits)
    }
}

impl<'owner> CountAcquiredPagesJob<'owner> {
    fn new(
        owner: &'owner SourceAcquiredDocument,
        context: AcquiredObjectJobContext,
        limits: PageTreeLimits,
    ) -> Result<Self, DocumentError> {
        let root = owner.root();
        let source_offset = owner.classify_object_target(root)?.source_offset();
        if !acquired_context_valid(context) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidRevisionResolverJobContext,
                Some(root),
                Some(source_offset),
            ));
        }
        let work_capacity = usize::try_from(limits.effective_work_items()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(source_offset),
            )
        })?;
        let seen_capacity = limits
            .effective_seen_references()
            .checked_mul(2)
            .and_then(u64::checked_next_power_of_two)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(root),
                    Some(source_offset),
                )
            })?;
        let active_capacity = usize::try_from(limits.max_depth()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(source_offset),
            )
        })?;
        let requested = page_traversal_bytes(work_capacity, seen_capacity, active_capacity)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(root),
                    Some(source_offset),
                )
            })?;
        if requested > limits.max_retained_traversal_bytes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                0,
                requested,
                root,
                Some(source_offset),
            ));
        }
        let mut work = Vec::new();
        work.try_reserve_exact(work_capacity).map_err(|_| {
            DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                0,
                requested,
                root,
                Some(source_offset),
            )
        })?;
        let mut seen_slots = Vec::new();
        seen_slots.try_reserve_exact(seen_capacity).map_err(|_| {
            DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                0,
                requested,
                root,
                Some(source_offset),
            )
        })?;
        seen_slots.resize(seen_capacity, 0);
        let mut active = Vec::new();
        active.try_reserve_exact(active_capacity).map_err(|_| {
            DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                0,
                requested,
                root,
                Some(source_offset),
            )
        })?;
        let reserved =
            page_traversal_bytes(work.capacity(), seen_slots.capacity(), active.capacity())
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(root),
                        Some(source_offset),
                    )
                })?;
        if reserved > limits.max_retained_traversal_bytes() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeTraversalBytes,
                limits.max_retained_traversal_bytes(),
                0,
                reserved,
                root,
                Some(source_offset),
            ));
        }
        Ok(Self {
            owner,
            context,
            limits,
            catalog: None,
            work,
            seen_slots,
            seen_count: 0,
            active,
            current: None,
            child: None,
            stats: AcquiredPageTreeStats {
                reserved_traversal_bytes: reserved,
                ..Default::default()
            },
            state: ServiceState::Active,
            terminal_error: DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(source_offset),
            ),
        })
    }

    /// Returns the immutable acquired-chain snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.owner.snapshot()
    }
    /// Returns runtime context shared by sequential object children.
    pub const fn context(&self) -> AcquiredObjectJobContext {
        self.context
    }
    /// Returns page-tree limits.
    pub const fn limits(&self) -> PageTreeLimits {
        self.limits
    }
    /// Returns deterministic traversal accounting.
    pub const fn stats(&self) -> AcquiredPageTreeStats {
        self.stats
    }
    /// Returns public traversal phase.
    pub const fn phase(&self) -> PageTreePhase {
        match self.state {
            ServiceState::Ready => PageTreePhase::Ready,
            ServiceState::Failed => PageTreePhase::Failed,
            ServiceState::Active if self.catalog.is_some() => PageTreePhase::Traversing,
            ServiceState::Active => PageTreePhase::Catalog,
        }
    }

    /// Advances page counting without host I/O inside the job.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> AcquiredPageCountPoll {
        if !matches!(self.state, ServiceState::Active) {
            return AcquiredPageCountPoll::Failed(self.terminal_error);
        }
        loop {
            if source.snapshot() != self.owner.snapshot() {
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
                        self.current = Some(PageTarget::Catalog);
                    } else {
                        match self.work.pop() {
                            Some(PageWork::Visit(visit)) => {
                                self.current = Some(PageTarget::Node(visit))
                            }
                            Some(PageWork::Finish {
                                reference,
                                declared_pages,
                                pages_before,
                                source_offset,
                            }) => {
                                if let Err(error) = self.finish_pages(
                                    reference,
                                    declared_pages,
                                    pages_before,
                                    source_offset,
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
            if let Err(error) = self.account_child(&mut child) {
                return self.fail(error);
            }
            match outcome {
                AcquiredObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    self.child = Some(child);
                    return AcquiredPageCountPoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    };
                }
                AcquiredObjectPoll::Failed(error) => {
                    return self.fail(self.map_child_error(error, &child));
                }
                AcquiredObjectPoll::Ready(object) => {
                    let Some(target) = self.current.take() else {
                        return self.fail(DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(child.reference),
                            Some(child.source_offset),
                        ));
                    };
                    let result = match target {
                        PageTarget::Catalog => self.accept_catalog(&object, cancellation),
                        PageTarget::Node(visit) => self.accept_node(visit, &object, cancellation),
                    };
                    if let Err(error) = result {
                        return self.fail(error);
                    }
                }
            }
        }
    }

    fn start_child(&mut self) -> Result<(), DocumentError> {
        let target = self
            .current
            .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))?;
        let (reference, depth) = match target {
            PageTarget::Catalog => (self.owner.root(), None),
            PageTarget::Node(visit) => (visit.reference, Some(visit.depth)),
        };
        let source_offset = self
            .owner
            .classify_object_target(reference)?
            .source_offset();
        if let Some(depth) = depth {
            if self.stats.nodes_started >= self.limits.max_nodes() {
                return Err(DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeNodes,
                    self.limits.max_nodes(),
                    self.stats.nodes_started,
                    1,
                    reference,
                    Some(source_offset),
                ));
            }
            if depth > self.limits.max_depth() {
                return Err(DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeDepth,
                    self.limits.max_depth(),
                    depth.saturating_sub(1),
                    1,
                    reference,
                    Some(source_offset),
                ));
            }
        }
        let read_remaining = self
            .limits
            .max_total_object_read_bytes()
            .checked_sub(self.stats.object_read_bytes)
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeObjectReadBytes,
                    self.limits.max_total_object_read_bytes(),
                    self.stats.object_read_bytes,
                    1,
                    reference,
                    Some(source_offset),
                )
            })?;
        let parse_remaining = self
            .limits
            .max_total_object_parse_bytes()
            .checked_sub(self.stats.object_parse_bytes)
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeObjectParseBytes,
                    self.limits.max_total_object_parse_bytes(),
                    self.stats.object_parse_bytes,
                    1,
                    reference,
                    Some(source_offset),
                )
            })?;
        let caps = AcquiredObjectWorkCaps::new(
            read_remaining.min(self.owner.limits().max_object_read_bytes()),
            parse_remaining.min(self.owner.limits().max_object_parse_bytes()),
            self.owner.limits(),
        )?;
        let job = self
            .owner
            .open_object_with_work_caps(reference, self.context, caps)?;
        self.stats.objects_started =
            self.stats.objects_started.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(source_offset),
                )
            })?;
        if let Some(depth) = depth {
            self.stats.nodes_started =
                self.stats.nodes_started.checked_add(1).ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(source_offset),
                    )
                })?;
            self.stats.max_depth = self.stats.max_depth.max(depth);
        }
        self.child = Some(PageChild {
            job,
            accounted: AcquiredObjectStats::default(),
            reference,
            source_offset,
        });
        Ok(())
    }

    fn account_child(&mut self, child: &mut PageChild<'owner>) -> Result<(), DocumentError> {
        let current = child.job.stats();
        let read_delta = current
            .total_read_bytes()
            .checked_sub(child.accounted.total_read_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.source_offset),
                )
            })?;
        let parse_delta = current
            .total_parse_bytes()
            .checked_sub(child.accounted.total_parse_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.source_offset),
                )
            })?;
        self.stats.object_read_bytes = self
            .stats
            .object_read_bytes
            .checked_add(read_delta)
            .filter(|value| *value <= self.limits.max_total_object_read_bytes())
            .ok_or_else(|| {
                DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeObjectReadBytes,
                    self.limits.max_total_object_read_bytes(),
                    self.stats.object_read_bytes,
                    read_delta,
                    child.reference,
                    Some(child.source_offset),
                )
            })?;
        self.stats.object_parse_bytes = self
            .stats
            .object_parse_bytes
            .checked_add(parse_delta)
            .filter(|value| *value <= self.limits.max_total_object_parse_bytes())
            .ok_or_else(|| {
                DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreeObjectParseBytes,
                    self.limits.max_total_object_parse_bytes(),
                    self.stats.object_parse_bytes,
                    parse_delta,
                    child.reference,
                    Some(child.source_offset),
                )
            })?;
        child.accounted = current;
        Ok(())
    }

    fn map_child_error(&self, error: DocumentError, child: &PageChild<'_>) -> DocumentError {
        if error.code() == DocumentErrorCode::ResourceLimit
            && let Some(limit) = error.limit()
        {
            let kind =
                match limit.kind() {
                    DocumentLimitKind::AcquiredObjectReadBytes
                        if child.job.work_caps().max_read_bytes()
                            < self.owner.limits().max_object_read_bytes() =>
                    {
                        Some(DocumentLimitKind::PageTreeObjectReadBytes)
                    }
                    DocumentLimitKind::RevisionResolverObjectReadBytes
                        if child.job.work_caps().max_read_bytes()
                            < self.owner.limits().max_object_read_bytes().min(
                                self.owner.limits().resolver().max_total_object_read_bytes(),
                            ) =>
                    {
                        Some(DocumentLimitKind::PageTreeObjectReadBytes)
                    }
                    DocumentLimitKind::AcquiredObjectParseBytes
                        if child.job.work_caps().max_parse_bytes()
                            < self.owner.limits().max_object_parse_bytes() =>
                    {
                        Some(DocumentLimitKind::PageTreeObjectParseBytes)
                    }
                    DocumentLimitKind::RevisionResolverObjectParseBytes
                        if child.job.work_caps().max_parse_bytes()
                            < self.owner.limits().max_object_parse_bytes().min(
                                self.owner
                                    .limits()
                                    .resolver()
                                    .max_total_object_parse_bytes(),
                            ) =>
                    {
                        Some(DocumentLimitKind::PageTreeObjectParseBytes)
                    }
                    _ => None,
                };
            if let Some(kind) = kind {
                let (ceiling, service_consumed, child_accounted) =
                    if kind == DocumentLimitKind::PageTreeObjectReadBytes {
                        (
                            self.limits.max_total_object_read_bytes(),
                            self.stats.object_read_bytes,
                            child.accounted.total_read_bytes(),
                        )
                    } else {
                        (
                            self.limits.max_total_object_parse_bytes(),
                            self.stats.object_parse_bytes,
                            child.accounted.total_parse_bytes(),
                        )
                    };
                let consumed = match service_consumed
                    .checked_sub(child_accounted)
                    .and_then(|before_child| before_child.checked_add(limit.consumed()))
                {
                    Some(consumed) => consumed,
                    None => {
                        return DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(child.reference),
                            Some(child.source_offset),
                        );
                    }
                };
                return DocumentError::page_tree_resource(
                    kind,
                    ceiling,
                    consumed,
                    limit.attempted(),
                    child.reference,
                    Some(child.source_offset),
                );
            }
        }
        error
    }

    fn accept_catalog(
        &mut self,
        object: &AcquiredObject<'_>,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let parsed = parse_catalog(self.owner, object, cancellation)?;
        let pages = parsed.catalog.pages();
        let offset = self.owner.object_source_offset(pages).unwrap_or_else(|| {
            self.owner
                .object_source_offset(self.owner.root())
                .unwrap_or(0)
        });
        if !self.insert_seen(pages, cancellation)? {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(pages),
                Some(offset),
            ));
        }
        self.catalog = Some(parsed.catalog);
        self.push_page_work(PageWork::Visit(VisitPageNode {
            reference: pages,
            parent: None,
            depth: 1,
            edge_source_offset: offset,
        }))
    }

    fn accept_node(
        &mut self,
        visit: VisitPageNode,
        object: &AcquiredObject<'_>,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let reference = object.reference();
        let source_offset = self
            .owner
            .object_source_offset(reference)
            .unwrap_or(visit.edge_source_offset);
        if reference != visit.reference {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(source_offset),
            ));
        }
        let dictionary = ValueView::from_acquired(object.value()?)
            .as_dictionary()
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InvalidPageTreeNode,
                    Some(reference),
                    Some(source_offset),
                )
            })?;
        let fields = collect_fields(
            dictionary,
            [
                b"Type".as_slice(),
                b"Parent".as_slice(),
                b"Kids".as_slice(),
                b"Count".as_slice(),
            ],
            reference,
            source_offset,
            cancellation,
        )?;
        reject_duplicate(&fields, 0, reference, source_offset)?;
        let kind = required(
            &fields,
            0,
            reference,
            source_offset,
            DocumentErrorCode::InvalidPageTreeNode,
        )?;
        let is_page = match kind.as_name() {
            Some(b"Page") => true,
            Some(b"Pages") => false,
            _ => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageTreeNode,
                    Some(reference),
                    Some(source_offset),
                ));
            }
        };
        reject_duplicate(&fields, 1, reference, source_offset)?;
        let parent = match fields.values[1] {
            None => None,
            Some(value) if value.is_null() => None,
            Some(value) => Some(value.as_reference().ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InvalidPageTreeNode,
                    Some(reference),
                    Some(source_offset),
                )
            })?),
        };
        if parent != visit.parent {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageTreeParentMismatch,
                Some(reference),
                Some(source_offset),
            ));
        }
        if is_page {
            if visit.parent.is_none() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageTreeNode,
                    Some(reference),
                    Some(source_offset),
                ));
            }
            if self.stats.pages >= self.limits.max_pages() {
                return Err(DocumentError::page_tree_resource(
                    DocumentLimitKind::PageTreePages,
                    self.limits.max_pages(),
                    self.stats.pages,
                    1,
                    reference,
                    Some(source_offset),
                ));
            }
            self.stats.pages += 1;
            return Ok(());
        }
        reject_duplicate(&fields, 2, reference, source_offset)?;
        reject_duplicate(&fields, 3, reference, source_offset)?;
        let kids = required(
            &fields,
            2,
            reference,
            source_offset,
            DocumentErrorCode::InvalidPageTreeNode,
        )?
        .as_array()
        .ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeNode,
                Some(reference),
                Some(source_offset),
            )
        })?;
        let declared_pages = required(
            &fields,
            3,
            reference,
            source_offset,
            DocumentErrorCode::InvalidPageTreeNode,
        )?
        .as_integer()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InvalidPageTreeNode,
                Some(reference),
                Some(source_offset),
            )
        })?;
        let kids_count = u64::try_from(kids.len()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(source_offset),
            )
        })?;
        if kids_count > self.limits.max_kids_per_node() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeKids,
                self.limits.max_kids_per_node(),
                0,
                kids_count,
                reference,
                Some(source_offset),
            ));
        }
        if self
            .seen_count
            .checked_add(kids_count)
            .is_none_or(|value| value > self.limits.max_nodes())
        {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeNodes,
                self.limits.max_nodes(),
                self.seen_count,
                kids_count,
                reference,
                Some(source_offset),
            ));
        }
        let child_depth = visit.depth.checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(source_offset),
            )
        })?;
        if kids_count > 0 && child_depth > self.limits.max_depth() {
            return Err(DocumentError::page_tree_resource(
                DocumentLimitKind::PageTreeDepth,
                self.limits.max_depth(),
                visit.depth,
                1,
                reference,
                Some(source_offset),
            ));
        }
        if self.active.len() >= self.active.capacity() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(source_offset),
            ));
        }
        self.active.push(reference);
        for index in 0..kids.len() {
            probe(cancellation, index, reference, source_offset)?;
            let child = kids
                .value(index)
                .and_then(ValueView::as_reference)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InvalidPageTreeNode,
                        Some(reference),
                        Some(source_offset),
                    )
                })?;
            if self.active_contains(child, cancellation)? {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::PageTreeCycle,
                    Some(child),
                    Some(source_offset),
                ));
            }
            if !self.insert_seen(child, cancellation)? {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::DuplicatePageTreeNode,
                    Some(child),
                    Some(source_offset),
                ));
            }
        }
        self.stats.max_kids_per_node = self.stats.max_kids_per_node.max(kids_count);
        self.push_page_work(PageWork::Finish {
            reference,
            declared_pages,
            pages_before: self.stats.pages,
            source_offset,
        })?;
        for index in (0..kids.len()).rev() {
            probe(cancellation, index, reference, source_offset)?;
            let child = kids
                .value(index)
                .and_then(ValueView::as_reference)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(source_offset),
                    )
                })?;
            let offset = self
                .owner
                .object_source_offset(child)
                .unwrap_or(source_offset);
            self.push_page_work(PageWork::Visit(VisitPageNode {
                reference: child,
                parent: Some(reference),
                depth: child_depth,
                edge_source_offset: offset,
            }))?;
        }
        Ok(())
    }

    fn finish_pages(
        &mut self,
        reference: ObjectRef,
        declared: u64,
        before: u64,
        offset: u64,
    ) -> Result<(), DocumentError> {
        let actual = self.stats.pages.checked_sub(before).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(offset),
            )
        })?;
        if self.active.pop() != Some(reference) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(offset),
            ));
        }
        if actual != declared {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageTreeCountMismatch,
                Some(reference),
                Some(offset),
            ));
        }
        Ok(())
    }

    fn active_contains(
        &self,
        reference: ObjectRef,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<bool, DocumentError> {
        for (index, active) in self.active.iter().enumerate() {
            probe(
                cancellation,
                index,
                reference,
                self.owner.object_source_offset(reference).unwrap_or(0),
            )?;
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
        insert_seen(
            &mut self.seen_slots,
            &mut self.seen_count,
            reference,
            cancellation,
            self.owner.object_source_offset(reference).unwrap_or(0),
        )
    }

    fn push_page_work(&mut self, work: PageWork) -> Result<(), DocumentError> {
        if self.work.len() >= self.work.capacity() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_reference(),
                self.current_offset(),
            ));
        }
        self.work.push(work);
        Ok(())
    }

    fn finish_ready(&mut self) -> AcquiredPageCountPoll {
        if !self.active.is_empty() || self.current.is_some() || self.child.is_some() {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_reference(),
                self.current_offset(),
            ));
        }
        let Some(catalog) = self.catalog.take() else {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.owner.root()),
                self.owner.object_source_offset(self.owner.root()),
            ));
        };
        let result = AcquiredPageCount {
            catalog,
            page_count: self.stats.pages,
            stats: self.stats,
        };
        self.release();
        self.state = ServiceState::Ready;
        self.terminal_error = DocumentError::for_code(
            DocumentErrorCode::JobAlreadyComplete,
            Some(self.owner.root()),
            self.owner.object_source_offset(self.owner.root()),
        );
        AcquiredPageCountPoll::Ready(result)
    }

    fn fail(&mut self, error: DocumentError) -> AcquiredPageCountPoll {
        self.child = None;
        self.current = None;
        self.catalog = None;
        self.release();
        self.state = ServiceState::Failed;
        self.terminal_error = error;
        AcquiredPageCountPoll::Failed(error)
    }

    fn release(&mut self) {
        self.work = Vec::new();
        self.seen_slots = Vec::new();
        self.seen_count = 0;
        self.active = Vec::new();
    }
    fn current_reference(&self) -> Option<ObjectRef> {
        match self.current {
            Some(PageTarget::Catalog) => Some(self.owner.root()),
            Some(PageTarget::Node(visit)) => Some(visit.reference),
            None => None,
        }
    }
    fn current_offset(&self) -> Option<u64> {
        self.current_reference()
            .and_then(|reference| self.owner.object_source_offset(reference))
    }
}

impl fmt::Debug for CountAcquiredPagesJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CountAcquiredPagesJob")
            .field("snapshot", &self.snapshot())
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .field("work", &"[REDACTED]")
            .field("child", &"[REDACTED]")
            .finish()
    }
}

/// One validated acquired-chain outline item in deterministic pre-order.
#[derive(Eq, PartialEq)]
pub struct AcquiredOutlineItem {
    reference: ObjectRef,
    parent_index: Option<usize>,
    depth: u64,
    title: DecodedTextString,
    declared_count: Option<i64>,
    target_kind: OutlineTargetKind,
    direct_children: u64,
    visible_descendants_if_open: u64,
}

impl AcquiredOutlineItem {
    /// Returns the exact-generation indirect object identity.
    pub const fn reference(&self) -> ObjectRef {
        self.reference
    }

    /// Returns the pre-order index of the parent item, or `None` at the root level.
    pub const fn parent_index(&self) -> Option<usize> {
        self.parent_index
    }

    /// Returns the one-based depth below the outline root.
    pub const fn depth(&self) -> u64 {
        self.depth
    }

    /// Borrows the decoded Unicode title.
    pub fn title(&self) -> &str {
        self.title.as_str()
    }

    /// Borrows title encoding and allocator-capacity evidence.
    pub const fn decoded_title(&self) -> &DecodedTextString {
        &self.title
    }

    /// Returns the source `/Count`, including its open or closed sign.
    pub const fn declared_count(&self) -> Option<i64> {
        self.declared_count
    }

    /// Returns the normalized `/Count`, using zero when absent.
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

    /// Returns direct child items.
    pub const fn direct_children(&self) -> u64 {
        self.direct_children
    }

    /// Returns descendants visible if this item is opened.
    pub const fn visible_descendants_if_open(&self) -> u64 {
        self.visible_descendants_if_open
    }

    /// Returns descendants visible under the item's current `/Count` sign.
    pub const fn visible_descendants(&self) -> u64 {
        match self.declared_count {
            Some(count) if count > 0 => self.visible_descendants_if_open,
            Some(_) | None => 0,
        }
    }
}

impl fmt::Debug for AcquiredOutlineItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquiredOutlineItem")
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

/// Deterministic acquired-outline traversal and retained-capacity accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AcquiredOutlineStats {
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

impl AcquiredOutlineStats {
    /// Returns all acquired object children started, including Catalog and outline root.
    pub const fn objects_started(self) -> u64 {
        self.objects_started
    }
    /// Returns outline-item object children started.
    pub const fn items_started(self) -> u64 {
        self.items_started
    }
    /// Returns the greatest root-relative item depth started.
    pub const fn max_depth(self) -> u64 {
        self.max_depth
    }
    /// Returns the greatest sibling position started at any level.
    pub const fn max_siblings_per_level(self) -> u64 {
        self.max_siblings_per_level
    }
    /// Returns cumulative acquired-object exact reads.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }
    /// Returns cumulative framing, decode, and semantic parser work.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }
    /// Returns cumulative decoded PDF-string bytes.
    pub const fn title_input_bytes(self) -> u64 {
        self.title_input_bytes
    }
    /// Returns cumulative logical UTF-8 title bytes.
    pub const fn title_utf8_bytes(self) -> u64 {
        self.title_utf8_bytes
    }
    /// Returns cumulative allocator-reported title capacity.
    pub const fn title_reserved_utf8_bytes(self) -> u64 {
        self.title_reserved_utf8_bytes
    }
    /// Returns allocator-reported traversal capacity.
    pub const fn reserved_working_bytes(self) -> u64 {
        self.reserved_working_bytes
    }
    /// Returns item-vector and accepted-title capacity.
    pub const fn reserved_result_bytes(self) -> u64 {
        self.reserved_result_bytes
    }
    /// Returns working plus result capacity.
    pub const fn reserved_bytes(self) -> u64 {
        self.reserved_working_bytes
            .saturating_add(self.reserved_result_bytes)
    }
    /// Returns greatest allocator-reported retained capacity admitted.
    pub const fn peak_retained_bytes(self) -> u64 {
        self.peak_retained_bytes
    }
}

/// Complete successful Catalog and outline service over one acquired revision chain.
#[derive(Eq, PartialEq)]
pub struct AcquiredOutline {
    catalog: AcquiredCatalog,
    root: Option<ObjectRef>,
    root_count: Option<u64>,
    visible_items: u64,
    items: Vec<AcquiredOutlineItem>,
    stats: AcquiredOutlineStats,
}

impl AcquiredOutline {
    /// Returns the source-bound Catalog summary.
    pub const fn catalog(&self) -> AcquiredCatalog {
        self.catalog
    }
    /// Returns the optional outline-root reference.
    pub const fn root(&self) -> Option<ObjectRef> {
        self.root
    }
    /// Returns the optional nonnegative outline-root `/Count`.
    pub const fn root_count(&self) -> Option<u64> {
        self.root_count
    }
    /// Returns top-level items plus descendants visible under positive counts.
    pub const fn visible_items(&self) -> u64 {
        self.visible_items
    }
    /// Borrows every validated item in deterministic pre-order.
    pub fn items(&self) -> &[AcquiredOutlineItem] {
        &self.items
    }
    /// Returns deterministic traversal accounting.
    pub const fn stats(&self) -> AcquiredOutlineStats {
        self.stats
    }
}

impl fmt::Debug for AcquiredOutline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquiredOutline")
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

/// Poll result for an acquired-chain outline service job.
#[allow(clippy::large_enum_variant)]
pub enum AcquiredOutlinePoll {
    /// The optional outline root and every reachable item were validated.
    Ready(AcquiredOutline),
    /// The active object resolver requires exact source ranges.
    Pending {
        /// One-shot data-arrival ticket.
        ticket: DataTicket,
        /// Canonical exact ranges still missing.
        missing: SmallRanges,
        /// Exact lower checkpoint retained while waiting.
        checkpoint: ResumeCheckpoint,
    },
    /// Stable structured failure.
    Failed(DocumentError),
}

impl fmt::Debug for AcquiredOutlinePoll {
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
struct VisitOutlineItem {
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
enum OutlineWork {
    Root {
        reference: ObjectRef,
        edge_offset: u64,
    },
    Visit(VisitOutlineItem),
    Finish {
        item_index: usize,
        count_offset: u64,
    },
}

#[derive(Clone, Copy)]
enum OutlineTarget {
    Catalog,
    Root {
        reference: ObjectRef,
        edge_offset: u64,
    },
    Item(VisitOutlineItem),
}

struct OutlineChild<'owner> {
    job: OpenAcquiredObjectJob<'owner>,
    accounted: AcquiredObjectStats,
    reference: ObjectRef,
    source_offset: u64,
}

/// One-shot acquired-chain Catalog and complete outline traversal.
pub struct ReadAcquiredOutlineJob<'owner> {
    owner: &'owner SourceAcquiredDocument,
    context: AcquiredObjectJobContext,
    limits: OutlineLimits,
    catalog: Option<AcquiredCatalog>,
    root: Option<ObjectRef>,
    root_count: Option<u64>,
    root_count_offset: Option<u64>,
    work: Vec<OutlineWork>,
    seen_slots: Vec<u64>,
    seen_count: u64,
    active_items: Vec<ObjectRef>,
    items: Vec<AcquiredOutlineItem>,
    current: Option<OutlineTarget>,
    child: Option<OutlineChild<'owner>>,
    stats: AcquiredOutlineStats,
    visible_items: u64,
    state: ServiceState,
    terminal_error: DocumentError,
}

impl SourceAcquiredDocument {
    /// Creates an outline job that borrows this complete source-acquired proof owner.
    pub fn read_acquired_outline(
        &self,
        context: AcquiredObjectJobContext,
        limits: OutlineLimits,
    ) -> Result<ReadAcquiredOutlineJob<'_>, DocumentError> {
        ReadAcquiredOutlineJob::new(self, context, limits)
    }
}

impl<'owner> ReadAcquiredOutlineJob<'owner> {
    fn new(
        owner: &'owner SourceAcquiredDocument,
        context: AcquiredObjectJobContext,
        limits: OutlineLimits,
    ) -> Result<Self, DocumentError> {
        let root = owner.root();
        let source_offset = owner.classify_object_target(root)?.source_offset();
        if !acquired_context_valid(context) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidRevisionResolverJobContext,
                Some(root),
                Some(source_offset),
            ));
        }
        let item_capacity = usize::try_from(limits.max_items()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(source_offset),
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
                    Some(source_offset),
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
                    Some(source_offset),
                )
            })?;
        let active_capacity = usize::try_from(limits.max_depth()).map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(source_offset),
            )
        })?;
        let capacities = [item_capacity, work_capacity, seen_capacity, active_capacity];
        let requested = validate_outline_retained_plan(limits, root, source_offset, capacities)?;
        let allocation_error = || {
            DocumentError::outline_resource(
                DocumentLimitKind::OutlineRetainedBytes,
                limits.max_retained_bytes(),
                0,
                requested,
                root,
                Some(source_offset),
            )
        };
        let mut items = Vec::new();
        items
            .try_reserve_exact(item_capacity)
            .map_err(|_| allocation_error())?;
        let mut work = Vec::new();
        work.try_reserve_exact(work_capacity)
            .map_err(|_| allocation_error())?;
        let mut seen_slots = Vec::new();
        seen_slots
            .try_reserve_exact(seen_capacity)
            .map_err(|_| allocation_error())?;
        seen_slots.resize(seen_capacity, 0);
        let mut active_items = Vec::new();
        active_items
            .try_reserve_exact(active_capacity)
            .map_err(|_| allocation_error())?;
        let actual_capacities = [
            items.capacity(),
            work.capacity(),
            seen_slots.capacity(),
            active_items.capacity(),
        ];
        let reserved =
            validate_outline_retained_plan(limits, root, source_offset, actual_capacities)?;
        let reserved_working = outline_working_bytes(
            work.capacity(),
            seen_slots.capacity(),
            active_items.capacity(),
        )
        .ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(source_offset),
            )
        })?;
        let reserved_result = outline_result_bytes(items.capacity()).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(source_offset),
            )
        })?;
        Ok(Self {
            owner,
            context,
            limits,
            catalog: None,
            root: None,
            root_count: None,
            root_count_offset: None,
            work,
            seen_slots,
            seen_count: 0,
            active_items,
            items,
            current: None,
            child: None,
            stats: AcquiredOutlineStats {
                reserved_working_bytes: reserved_working,
                reserved_result_bytes: reserved_result,
                peak_retained_bytes: reserved,
                ..Default::default()
            },
            visible_items: 0,
            state: ServiceState::Active,
            terminal_error: DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(source_offset),
            ),
        })
    }

    /// Returns the immutable acquired-chain snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.owner.snapshot()
    }
    /// Returns runtime context shared by sequential acquired-object children.
    pub const fn context(&self) -> AcquiredObjectJobContext {
        self.context
    }
    /// Returns complete outline limits.
    pub const fn limits(&self) -> OutlineLimits {
        self.limits
    }
    /// Returns deterministic work accounting through the latest poll.
    pub const fn stats(&self) -> AcquiredOutlineStats {
        self.stats
    }
    /// Returns the public outline phase.
    pub const fn phase(&self) -> OutlinePhase {
        match self.state {
            ServiceState::Ready => OutlinePhase::Ready,
            ServiceState::Failed => OutlinePhase::Failed,
            ServiceState::Active if self.catalog.is_some() => OutlinePhase::Traversing,
            ServiceState::Active => OutlinePhase::Catalog,
        }
    }

    /// Advances outline traversal without host I/O inside the job.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> AcquiredOutlinePoll {
        if !matches!(self.state, ServiceState::Active) {
            return AcquiredOutlinePoll::Failed(self.terminal_error);
        }
        loop {
            if source.snapshot() != self.owner.snapshot() {
                return self.fail_outline(DocumentError::for_code(
                    DocumentErrorCode::SourceSnapshotMismatch,
                    self.current_outline_reference(),
                    self.current_outline_offset(),
                ));
            }
            if cancellation.is_cancelled() {
                return self.fail_outline(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    self.current_outline_reference(),
                    self.current_outline_offset(),
                ));
            }
            if self.child.is_none() {
                if self.current.is_none() {
                    if self.catalog.is_none() {
                        self.current = Some(OutlineTarget::Catalog);
                    } else {
                        match self.work.pop() {
                            Some(OutlineWork::Root {
                                reference,
                                edge_offset,
                            }) => {
                                self.current = Some(OutlineTarget::Root {
                                    reference,
                                    edge_offset,
                                });
                            }
                            Some(OutlineWork::Visit(visit)) => {
                                self.current = Some(OutlineTarget::Item(visit));
                            }
                            Some(OutlineWork::Finish {
                                item_index,
                                count_offset,
                            }) => {
                                if let Err(error) =
                                    self.finish_outline_item(item_index, count_offset)
                                {
                                    return self.fail_outline(error);
                                }
                                continue;
                            }
                            None => return self.finish_outline_ready(),
                        }
                    }
                }
                if let Err(error) = self.start_outline_child() {
                    return self.fail_outline(error);
                }
            }
            let Some(mut child) = self.child.take() else {
                return self.fail_outline(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    self.current_outline_reference(),
                    self.current_outline_offset(),
                ));
            };
            let outcome = child.job.poll(source, cancellation);
            if let Err(error) = self.account_outline_child(&mut child) {
                return self.fail_outline(error);
            }
            match outcome {
                AcquiredObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    self.child = Some(child);
                    return AcquiredOutlinePoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    };
                }
                AcquiredObjectPoll::Failed(error) => {
                    let mapped = self.map_outline_child_error(error, &child);
                    return self.fail_outline(mapped);
                }
                AcquiredObjectPoll::Ready(object) => {
                    let Some(target) = self.current.take() else {
                        return self.fail_outline(DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(child.reference),
                            Some(child.source_offset),
                        ));
                    };
                    let result = match target {
                        OutlineTarget::Catalog => {
                            self.accept_outline_catalog(&object, cancellation)
                        }
                        OutlineTarget::Root { reference, .. } => {
                            self.accept_outline_root(reference, &object, cancellation)
                        }
                        OutlineTarget::Item(visit) => {
                            self.accept_outline_item(visit, &object, cancellation)
                        }
                    };
                    if let Err(error) = result {
                        return self.fail_outline(error);
                    }
                }
            }
        }
    }

    fn start_outline_child(&mut self) -> Result<(), DocumentError> {
        let target = self.current.ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_outline_reference(),
                self.current_outline_offset(),
            )
        })?;
        let (reference, visit) = match target {
            OutlineTarget::Catalog => (self.owner.root(), None),
            OutlineTarget::Root { reference, .. } => (reference, None),
            OutlineTarget::Item(visit) => (visit.reference, Some(visit)),
        };
        let source_offset = self
            .owner
            .classify_object_target(reference)?
            .source_offset();
        if let Some(visit) = visit {
            if self.stats.items_started >= self.limits.max_items() {
                return Err(DocumentError::outline_resource(
                    DocumentLimitKind::OutlineItems,
                    self.limits.max_items(),
                    self.stats.items_started,
                    1,
                    reference,
                    Some(source_offset),
                ));
            }
            if visit.depth > self.limits.max_depth() {
                return Err(DocumentError::outline_resource(
                    DocumentLimitKind::OutlineDepth,
                    self.limits.max_depth(),
                    visit.depth.saturating_sub(1),
                    1,
                    reference,
                    Some(source_offset),
                ));
            }
            if visit.sibling_position > self.limits.max_siblings_per_level() {
                return Err(DocumentError::outline_resource(
                    DocumentLimitKind::OutlineSiblings,
                    self.limits.max_siblings_per_level(),
                    visit.sibling_position.saturating_sub(1),
                    1,
                    reference,
                    Some(source_offset),
                ));
            }
        }
        let read_remaining = self
            .limits
            .max_total_object_read_bytes()
            .checked_sub(self.stats.object_read_bytes)
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                DocumentError::outline_resource(
                    DocumentLimitKind::OutlineObjectReadBytes,
                    self.limits.max_total_object_read_bytes(),
                    self.stats.object_read_bytes,
                    1,
                    reference,
                    Some(source_offset),
                )
            })?;
        let parse_remaining = self
            .limits
            .max_total_object_parse_bytes()
            .checked_sub(self.stats.object_parse_bytes)
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                DocumentError::outline_resource(
                    DocumentLimitKind::OutlineObjectParseBytes,
                    self.limits.max_total_object_parse_bytes(),
                    self.stats.object_parse_bytes,
                    1,
                    reference,
                    Some(source_offset),
                )
            })?;
        let caps = AcquiredObjectWorkCaps::new(
            read_remaining.min(self.owner.limits().max_object_read_bytes()),
            parse_remaining.min(self.owner.limits().max_object_parse_bytes()),
            self.owner.limits(),
        )?;
        let job = self
            .owner
            .open_object_with_work_caps(reference, self.context, caps)?;
        self.stats.objects_started =
            self.stats.objects_started.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(source_offset),
                )
            })?;
        if let Some(visit) = visit {
            self.stats.items_started =
                self.stats.items_started.checked_add(1).ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(source_offset),
                    )
                })?;
            self.stats.max_depth = self.stats.max_depth.max(visit.depth);
            self.stats.max_siblings_per_level = self
                .stats
                .max_siblings_per_level
                .max(visit.sibling_position);
        }
        self.child = Some(OutlineChild {
            job,
            accounted: AcquiredObjectStats::default(),
            reference,
            source_offset,
        });
        Ok(())
    }

    fn account_outline_child(
        &mut self,
        child: &mut OutlineChild<'owner>,
    ) -> Result<(), DocumentError> {
        let current = child.job.stats();
        let read_delta = current
            .total_read_bytes()
            .checked_sub(child.accounted.total_read_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.source_offset),
                )
            })?;
        let parse_delta = current
            .total_parse_bytes()
            .checked_sub(child.accounted.total_parse_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.source_offset),
                )
            })?;
        self.stats.object_read_bytes = self
            .stats
            .object_read_bytes
            .checked_add(read_delta)
            .filter(|value| *value <= self.limits.max_total_object_read_bytes())
            .ok_or_else(|| {
                DocumentError::outline_resource(
                    DocumentLimitKind::OutlineObjectReadBytes,
                    self.limits.max_total_object_read_bytes(),
                    self.stats.object_read_bytes,
                    read_delta,
                    child.reference,
                    Some(child.source_offset),
                )
            })?;
        self.stats.object_parse_bytes = self
            .stats
            .object_parse_bytes
            .checked_add(parse_delta)
            .filter(|value| *value <= self.limits.max_total_object_parse_bytes())
            .ok_or_else(|| {
                DocumentError::outline_resource(
                    DocumentLimitKind::OutlineObjectParseBytes,
                    self.limits.max_total_object_parse_bytes(),
                    self.stats.object_parse_bytes,
                    parse_delta,
                    child.reference,
                    Some(child.source_offset),
                )
            })?;
        child.accounted = current;
        Ok(())
    }

    fn map_outline_child_error(
        &self,
        error: DocumentError,
        child: &OutlineChild<'_>,
    ) -> DocumentError {
        if error.code() == DocumentErrorCode::ResourceLimit
            && let Some(limit) = error.limit()
        {
            let kind =
                match limit.kind() {
                    DocumentLimitKind::AcquiredObjectReadBytes
                        if child.job.work_caps().max_read_bytes()
                            < self.owner.limits().max_object_read_bytes() =>
                    {
                        Some(DocumentLimitKind::OutlineObjectReadBytes)
                    }
                    DocumentLimitKind::RevisionResolverObjectReadBytes
                        if child.job.work_caps().max_read_bytes()
                            < self.owner.limits().max_object_read_bytes().min(
                                self.owner.limits().resolver().max_total_object_read_bytes(),
                            ) =>
                    {
                        Some(DocumentLimitKind::OutlineObjectReadBytes)
                    }
                    DocumentLimitKind::AcquiredObjectParseBytes
                        if child.job.work_caps().max_parse_bytes()
                            < self.owner.limits().max_object_parse_bytes() =>
                    {
                        Some(DocumentLimitKind::OutlineObjectParseBytes)
                    }
                    DocumentLimitKind::RevisionResolverObjectParseBytes
                        if child.job.work_caps().max_parse_bytes()
                            < self.owner.limits().max_object_parse_bytes().min(
                                self.owner
                                    .limits()
                                    .resolver()
                                    .max_total_object_parse_bytes(),
                            ) =>
                    {
                        Some(DocumentLimitKind::OutlineObjectParseBytes)
                    }
                    _ => None,
                };
            if let Some(kind) = kind {
                let (ceiling, service_consumed, child_accounted) =
                    if kind == DocumentLimitKind::OutlineObjectReadBytes {
                        (
                            self.limits.max_total_object_read_bytes(),
                            self.stats.object_read_bytes,
                            child.accounted.total_read_bytes(),
                        )
                    } else {
                        (
                            self.limits.max_total_object_parse_bytes(),
                            self.stats.object_parse_bytes,
                            child.accounted.total_parse_bytes(),
                        )
                    };
                let consumed = match service_consumed
                    .checked_sub(child_accounted)
                    .and_then(|before_child| before_child.checked_add(limit.consumed()))
                {
                    Some(consumed) => consumed,
                    None => {
                        return DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(child.reference),
                            Some(child.source_offset),
                        );
                    }
                };
                return DocumentError::outline_resource(
                    kind,
                    ceiling,
                    consumed,
                    limit.attempted(),
                    child.reference,
                    Some(child.source_offset),
                );
            }
        }
        error
    }

    fn accept_outline_catalog(
        &mut self,
        object: &AcquiredObject<'_>,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let parsed = parse_catalog(self.owner, object, cancellation)?;
        self.catalog = Some(parsed.catalog);
        if let Some(reference) = parsed.outlines {
            self.root = Some(reference);
            let edge_offset = self
                .owner
                .object_source_offset(reference)
                .or_else(|| self.owner.object_source_offset(self.owner.root()))
                .unwrap_or(0);
            self.push_outline_work(OutlineWork::Root {
                reference,
                edge_offset,
            })?;
        }
        Ok(())
    }

    fn accept_outline_root(
        &mut self,
        expected: ObjectRef,
        object: &AcquiredObject<'_>,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let reference = object.reference();
        let source_offset = self
            .owner
            .object_source_offset(reference)
            .unwrap_or_else(|| self.current_outline_offset().unwrap_or(0));
        if reference != expected || self.root != Some(reference) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(source_offset),
            ));
        }
        let dictionary = ValueView::from_acquired(object.value()?)
            .as_dictionary()
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InvalidOutlineDictionary,
                    Some(reference),
                    Some(source_offset),
                )
            })?;
        let fields = collect_fields(
            dictionary,
            [
                b"Type".as_slice(),
                b"First".as_slice(),
                b"Last".as_slice(),
                b"Count".as_slice(),
            ],
            reference,
            source_offset,
            cancellation,
        )?;
        for index in 0..4 {
            reject_duplicate(&fields, index, reference, source_offset)?;
        }
        if let Some(value) = optional_non_null(&fields, 0) {
            if value.as_name() == Some(b"Outlines".as_slice()) {
                // Valid direct outline-root type.
            } else if value.as_reference().is_some() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::UnsupportedOutlineRepresentation,
                    Some(reference),
                    Some(source_offset),
                ));
            } else {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidOutlineDictionary,
                    Some(reference),
                    Some(source_offset),
                ));
            }
        }
        let pair = outline_reference_pair_view(
            optional_non_null(&fields, 1),
            optional_non_null(&fields, 2),
            reference,
            source_offset,
            DocumentErrorCode::InvalidOutlineDictionary,
        )?;
        self.root_count = match optional_non_null(&fields, 3) {
            None => None,
            Some(value) if value.as_reference().is_some() => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::UnsupportedOutlineRepresentation,
                    Some(reference),
                    Some(source_offset),
                ));
            }
            Some(value) => {
                let count = value
                    .as_integer()
                    .filter(|value| *value >= 0)
                    .and_then(|value| u64::try_from(value).ok())
                    .ok_or_else(|| {
                        DocumentError::for_code(
                            DocumentErrorCode::InvalidOutlineDictionary,
                            Some(reference),
                            Some(source_offset),
                        )
                    })?;
                self.root_count_offset = Some(source_offset);
                Some(count)
            }
        };
        if let Some((first, last)) = pair {
            let edge_offset = self
                .owner
                .object_source_offset(first)
                .unwrap_or(source_offset);
            self.schedule_outline_item(
                VisitOutlineItem {
                    reference: first,
                    parent: reference,
                    parent_index: None,
                    expected_prev: None,
                    last,
                    depth: 1,
                    sibling_position: 1,
                    edge_offset,
                },
                cancellation,
            )?;
        }
        Ok(())
    }

    fn accept_outline_item(
        &mut self,
        visit: VisitOutlineItem,
        object: &AcquiredObject<'_>,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        let reference = object.reference();
        let source_offset = self
            .owner
            .object_source_offset(reference)
            .unwrap_or(visit.edge_offset);
        if reference != visit.reference {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(source_offset),
            ));
        }
        let dictionary = ValueView::from_acquired(object.value()?)
            .as_dictionary()
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InvalidOutlineItem,
                    Some(reference),
                    Some(source_offset),
                )
            })?;
        let fields = collect_fields(
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
            source_offset,
            cancellation,
        )?;
        for index in 0..9 {
            reject_duplicate(&fields, index, reference, source_offset)?;
        }
        let title_value = required(
            &fields,
            0,
            reference,
            source_offset,
            DocumentErrorCode::InvalidOutlineTitle,
        )?;
        let title_string = match title_value.as_string() {
            Some(value) => value,
            None if title_value.as_reference().is_some() => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::UnsupportedOutlineRepresentation,
                    Some(reference),
                    Some(source_offset),
                ));
            }
            None => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidOutlineTitle,
                    Some(reference),
                    Some(source_offset),
                ));
            }
        };
        let parent = required(
            &fields,
            1,
            reference,
            source_offset,
            DocumentErrorCode::OutlineParentMismatch,
        )?
        .as_reference();
        if parent != Some(visit.parent) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::OutlineParentMismatch,
                Some(reference),
                Some(source_offset),
            ));
        }
        validate_outline_prev(
            reference,
            visit.expected_prev,
            optional_non_null(&fields, 2),
            source_offset,
        )?;
        let next_value = optional_non_null(&fields, 3);
        let next = if reference == visit.last {
            if next_value.is_some() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::OutlineSiblingMismatch,
                    Some(reference),
                    Some(source_offset),
                ));
            }
            None
        } else {
            let next = next_value
                .and_then(ValueView::as_reference)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::OutlineSiblingMismatch,
                        Some(reference),
                        Some(source_offset),
                    )
                })?;
            Some(next)
        };
        let children = outline_reference_pair_view(
            optional_non_null(&fields, 4),
            optional_non_null(&fields, 5),
            reference,
            source_offset,
            DocumentErrorCode::InvalidOutlineItem,
        )?;
        let (declared_count, count_offset) = match optional_non_null(&fields, 6) {
            None => (None, source_offset),
            Some(value) if value.as_reference().is_some() => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::UnsupportedOutlineRepresentation,
                    Some(reference),
                    Some(source_offset),
                ));
            }
            Some(value) => (
                Some(value.as_integer().ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InvalidOutlineItem,
                        Some(reference),
                        Some(source_offset),
                    )
                })?),
                source_offset,
            ),
        };
        let target_kind = validate_outline_target(
            reference,
            optional_non_null(&fields, 7),
            optional_non_null(&fields, 8),
            source_offset,
        )?;
        let measurement =
            measure_text_string(title_string, self.limits.title_limits(), cancellation).map_err(
                |error| DocumentError::from_outline_text(error, reference, source_offset),
            )?;
        self.preflight_outline_title(measurement, reference, source_offset)?;
        let title = decode_measured_text_string(
            title_string,
            self.limits.title_limits(),
            measurement,
            cancellation,
        )
        .map_err(|error| DocumentError::from_outline_text(error, reference, source_offset))?;
        self.account_outline_title(&title, reference, source_offset)?;
        if self.items.len() >= self.items.capacity()
            || self.active_items.len() >= self.active_items.capacity()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(source_offset),
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
                Some(source_offset),
            ));
        }
        self.items.push(AcquiredOutlineItem {
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
        if let Some(next) = next {
            let sibling_position = visit.sibling_position.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(source_offset),
                )
            })?;
            let edge_offset = self
                .owner
                .object_source_offset(next)
                .unwrap_or(source_offset);
            self.schedule_outline_item(
                VisitOutlineItem {
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
        self.push_outline_work(OutlineWork::Finish {
            item_index,
            count_offset,
        })?;
        if let Some((first, last)) = children {
            let depth = visit.depth.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(source_offset),
                )
            })?;
            let edge_offset = self
                .owner
                .object_source_offset(first)
                .unwrap_or(source_offset);
            self.schedule_outline_item(
                VisitOutlineItem {
                    reference: first,
                    parent: reference,
                    parent_index: Some(item_index),
                    expected_prev: None,
                    last,
                    depth,
                    sibling_position: 1,
                    edge_offset,
                },
                cancellation,
            )?;
        }
        Ok(())
    }

    fn preflight_outline_title(
        &self,
        measurement: TextStringMeasurement,
        reference: ObjectRef,
        source_offset: u64,
    ) -> Result<(), DocumentError> {
        let next_input = self
            .stats
            .title_input_bytes
            .checked_add(measurement.input_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(source_offset),
                )
            })?;
        if next_input > self.limits.max_total_title_input_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineTotalTitleInputBytes,
                self.limits.max_total_title_input_bytes(),
                self.stats.title_input_bytes,
                measurement.input_bytes(),
                reference,
                Some(source_offset),
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
                    Some(source_offset),
                )
            })?;
        if next_utf8 > self.limits.max_total_title_utf8_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineTotalTitleUtf8Bytes,
                self.limits.max_total_title_utf8_bytes(),
                self.stats.title_utf8_bytes,
                measurement.utf8_bytes(),
                reference,
                Some(source_offset),
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
                    Some(source_offset),
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
                    Some(source_offset),
                )
            })?;
        if requested_retained > self.limits.max_retained_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineRetainedBytes,
                self.limits.max_retained_bytes(),
                self.stats.reserved_bytes(),
                measurement.utf8_bytes(),
                reference,
                Some(source_offset),
            ));
        }
        Ok(())
    }

    fn account_outline_title(
        &mut self,
        title: &DecodedTextString,
        reference: ObjectRef,
        source_offset: u64,
    ) -> Result<(), DocumentError> {
        let next_input = self
            .stats
            .title_input_bytes
            .checked_add(title.input_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(source_offset),
                )
            })?;
        if next_input > self.limits.max_total_title_input_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineTotalTitleInputBytes,
                self.limits.max_total_title_input_bytes(),
                self.stats.title_input_bytes,
                title.input_bytes(),
                reference,
                Some(source_offset),
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
                    Some(source_offset),
                )
            })?;
        if next_utf8 > self.limits.max_total_title_utf8_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineTotalTitleUtf8Bytes,
                self.limits.max_total_title_utf8_bytes(),
                self.stats.title_utf8_bytes,
                title.utf8_bytes(),
                reference,
                Some(source_offset),
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
                    Some(source_offset),
                )
            })?;
        if next_reserved_utf8 > self.limits.max_total_title_utf8_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineTotalTitleUtf8Bytes,
                self.limits.max_total_title_utf8_bytes(),
                self.stats.title_reserved_utf8_bytes,
                title.reserved_utf8_bytes(),
                reference,
                Some(source_offset),
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
                    Some(source_offset),
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
                    Some(source_offset),
                )
            })?;
        if next_retained > self.limits.max_retained_bytes() {
            return Err(DocumentError::outline_resource(
                DocumentLimitKind::OutlineRetainedBytes,
                self.limits.max_retained_bytes(),
                self.stats.reserved_bytes(),
                title.reserved_utf8_bytes(),
                reference,
                Some(source_offset),
            ));
        }
        self.stats.title_input_bytes = next_input;
        self.stats.title_utf8_bytes = next_utf8;
        self.stats.title_reserved_utf8_bytes = next_reserved_utf8;
        self.stats.reserved_result_bytes = next_result;
        self.stats.peak_retained_bytes = self.stats.peak_retained_bytes.max(next_retained);
        Ok(())
    }

    fn finish_outline_item(
        &mut self,
        item_index: usize,
        count_offset: u64,
    ) -> Result<(), DocumentError> {
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
        if self.active_items.pop() != Some(reference) {
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

    fn schedule_outline_item(
        &mut self,
        visit: VisitOutlineItem,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        if self.outline_active_contains(visit.reference, cancellation)? {
            return Err(DocumentError::for_code(
                DocumentErrorCode::OutlineCycle,
                Some(visit.reference),
                Some(visit.edge_offset),
            ));
        }
        if seen_contains(
            &self.seen_slots,
            visit.reference,
            cancellation,
            visit.edge_offset,
        )? {
            let code = if self.prior_outline_sibling(
                visit.reference,
                visit.parent_index,
                cancellation,
                visit.edge_offset,
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
        if !insert_seen(
            &mut self.seen_slots,
            &mut self.seen_count,
            visit.reference,
            cancellation,
            visit.edge_offset,
        )? {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(visit.reference),
                Some(visit.edge_offset),
            ));
        }
        self.push_outline_work(OutlineWork::Visit(visit))
    }

    fn outline_active_contains(
        &self,
        reference: ObjectRef,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<bool, DocumentError> {
        let source_offset = self.owner.object_source_offset(reference).unwrap_or(0);
        if self.root == Some(reference) {
            return Ok(true);
        }
        for (index, active) in self.active_items.iter().enumerate() {
            probe(cancellation, index, reference, source_offset)?;
            if *active == reference {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn prior_outline_sibling(
        &self,
        reference: ObjectRef,
        parent_index: Option<usize>,
        cancellation: &dyn DocumentCancellation,
        source_offset: u64,
    ) -> Result<bool, DocumentError> {
        for (index, item) in self.items.iter().enumerate() {
            probe(cancellation, index, reference, source_offset)?;
            if item.reference == reference && item.parent_index == parent_index {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn push_outline_work(&mut self, work: OutlineWork) -> Result<(), DocumentError> {
        if self.work.len() >= self.work.capacity() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_outline_reference(),
                self.current_outline_offset(),
            ));
        }
        self.work.push(work);
        Ok(())
    }

    fn finish_outline_ready(&mut self) -> AcquiredOutlinePoll {
        if !self.active_items.is_empty() || self.current.is_some() || self.child.is_some() {
            return self.fail_outline(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_outline_reference(),
                self.current_outline_offset(),
            ));
        }
        let Some(catalog) = self.catalog.take() else {
            return self.fail_outline(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.owner.root()),
                self.owner.object_source_offset(self.owner.root()),
            ));
        };
        if self.items.is_empty() {
            if self.root_count.is_some() {
                return self.fail_outline(DocumentError::for_code(
                    DocumentErrorCode::OutlineCountMismatch,
                    self.root,
                    self.root_count_offset.or_else(|| {
                        self.root
                            .and_then(|reference| self.owner.object_source_offset(reference))
                    }),
                ));
            }
        } else if self.root_count != Some(self.visible_items) {
            return self.fail_outline(DocumentError::for_code(
                DocumentErrorCode::OutlineCountMismatch,
                self.root,
                self.root_count_offset.or_else(|| {
                    self.root
                        .and_then(|reference| self.owner.object_source_offset(reference))
                }),
            ));
        }
        let result = AcquiredOutline {
            catalog,
            root: self.root,
            root_count: self.root_count,
            visible_items: self.visible_items,
            items: mem::take(&mut self.items),
            stats: self.stats,
        };
        self.release_outline_working();
        self.state = ServiceState::Ready;
        self.terminal_error = DocumentError::for_code(
            DocumentErrorCode::JobAlreadyComplete,
            Some(self.owner.root()),
            self.owner.object_source_offset(self.owner.root()),
        );
        AcquiredOutlinePoll::Ready(result)
    }

    fn fail_outline(&mut self, error: DocumentError) -> AcquiredOutlinePoll {
        self.child = None;
        self.current = None;
        self.catalog = None;
        self.release_outline_working();
        self.items = Vec::new();
        self.state = ServiceState::Failed;
        self.terminal_error = error;
        AcquiredOutlinePoll::Failed(error)
    }

    fn release_outline_working(&mut self) {
        self.work = Vec::new();
        self.seen_slots = Vec::new();
        self.seen_count = 0;
        self.active_items = Vec::new();
    }

    fn current_outline_reference(&self) -> Option<ObjectRef> {
        match self.current {
            Some(OutlineTarget::Catalog) => Some(self.owner.root()),
            Some(OutlineTarget::Root { reference, .. }) => Some(reference),
            Some(OutlineTarget::Item(visit)) => Some(visit.reference),
            None => None,
        }
    }

    fn current_outline_offset(&self) -> Option<u64> {
        match self.current {
            Some(OutlineTarget::Root { edge_offset, .. }) => Some(edge_offset),
            Some(OutlineTarget::Item(visit)) => Some(visit.edge_offset),
            _ => self
                .current_outline_reference()
                .and_then(|reference| self.owner.object_source_offset(reference)),
        }
    }
}

impl fmt::Debug for ReadAcquiredOutlineJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadAcquiredOutlineJob")
            .field("snapshot", &self.snapshot())
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

fn acquired_context_valid(context: AcquiredObjectJobContext) -> bool {
    let checkpoints = [
        context.object_envelope_checkpoint(),
        context.object_boundary_checkpoint(),
        context.length_envelope_checkpoint(),
        context.length_boundary_checkpoint(),
        context.payload_checkpoint(),
    ];
    checkpoints
        .iter()
        .enumerate()
        .all(|(index, checkpoint)| !checkpoints[index + 1..].contains(checkpoint))
}

fn outline_reference_pair_view(
    first: Option<ValueView<'_>>,
    last: Option<ValueView<'_>>,
    owner: ObjectRef,
    source_offset: u64,
    invalid_code: DocumentErrorCode,
) -> Result<Option<(ObjectRef, ObjectRef)>, DocumentError> {
    match (first, last) {
        (None, None) => Ok(None),
        (Some(first), Some(last)) => {
            let first = first.as_reference().ok_or_else(|| {
                DocumentError::for_code(invalid_code, Some(owner), Some(source_offset))
            })?;
            let last = last.as_reference().ok_or_else(|| {
                DocumentError::for_code(invalid_code, Some(owner), Some(source_offset))
            })?;
            Ok(Some((first, last)))
        }
        (Some(_), None) | (None, Some(_)) => Err(DocumentError::for_code(
            invalid_code,
            Some(owner),
            Some(source_offset),
        )),
    }
}

fn validate_outline_prev(
    reference: ObjectRef,
    expected: Option<ObjectRef>,
    observed: Option<ValueView<'_>>,
    source_offset: u64,
) -> Result<(), DocumentError> {
    match (expected, observed) {
        (None, None) => Ok(()),
        (Some(expected), Some(observed)) if observed.as_reference() == Some(expected) => Ok(()),
        _ => Err(DocumentError::for_code(
            DocumentErrorCode::OutlineSiblingMismatch,
            Some(reference),
            Some(source_offset),
        )),
    }
}

fn validate_outline_target(
    reference: ObjectRef,
    destination: Option<ValueView<'_>>,
    action: Option<ValueView<'_>>,
    source_offset: u64,
) -> Result<OutlineTargetKind, DocumentError> {
    if destination.is_some() && action.is_some() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidOutlineTarget,
            Some(reference),
            Some(source_offset),
        ));
    }
    if let Some(value) = destination {
        if value.is_direct_destination() {
            return Ok(OutlineTargetKind::Destination);
        }
        let code = if value.as_reference().is_some() {
            DocumentErrorCode::UnsupportedOutlineRepresentation
        } else {
            DocumentErrorCode::InvalidOutlineTarget
        };
        return Err(DocumentError::for_code(
            code,
            Some(reference),
            Some(source_offset),
        ));
    }
    if let Some(value) = action {
        if value.as_dictionary().is_some() {
            return Ok(OutlineTargetKind::Action);
        }
        let code = if value.as_reference().is_some() {
            DocumentErrorCode::UnsupportedOutlineRepresentation
        } else {
            DocumentErrorCode::InvalidOutlineTarget
        };
        return Err(DocumentError::for_code(
            code,
            Some(reference),
            Some(source_offset),
        ));
    }
    Ok(OutlineTargetKind::None)
}

fn outline_working_bytes(work: usize, seen: usize, active: usize) -> Option<u64> {
    capacity_bytes::<OutlineWork>(work)?
        .checked_add(capacity_bytes::<u64>(seen)?)?
        .checked_add(capacity_bytes::<ObjectRef>(active)?)
}

fn outline_result_bytes(items: usize) -> Option<u64> {
    capacity_bytes::<AcquiredOutlineItem>(items)
}

fn outline_retained_bytes(capacities: [usize; 4]) -> Option<u64> {
    let [items, work, seen, active] = capacities;
    outline_result_bytes(items)?.checked_add(outline_working_bytes(work, seen, active)?)
}

fn validate_outline_retained_plan(
    limits: OutlineLimits,
    root: ObjectRef,
    source_offset: u64,
    capacities: [usize; 4],
) -> Result<u64, DocumentError> {
    let attempted = outline_retained_bytes(capacities).ok_or_else(|| {
        DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(root),
            Some(source_offset),
        )
    })?;
    if attempted > limits.max_retained_bytes() {
        return Err(DocumentError::outline_resource(
            DocumentLimitKind::OutlineRetainedBytes,
            limits.max_retained_bytes(),
            0,
            attempted,
            root,
            Some(source_offset),
        ));
    }
    Ok(attempted)
}

fn page_traversal_bytes(work: usize, seen: usize, active: usize) -> Option<u64> {
    capacity_bytes::<PageWork>(work)?
        .checked_add(capacity_bytes::<u64>(seen)?)?
        .checked_add(capacity_bytes::<ObjectRef>(active)?)
}

fn probe(
    cancellation: &dyn DocumentCancellation,
    index: usize,
    reference: ObjectRef,
    source_offset: u64,
) -> Result<(), DocumentError> {
    if index.is_multiple_of(CANCELLATION_INTERVAL) && cancellation.is_cancelled() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::Cancelled,
            Some(reference),
            Some(source_offset),
        ));
    }
    Ok(())
}

fn insert_seen(
    slots: &mut [u64],
    count: &mut u64,
    reference: ObjectRef,
    cancellation: &dyn DocumentCancellation,
    source_offset: u64,
) -> Result<bool, DocumentError> {
    if slots.is_empty() || !slots.len().is_power_of_two() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(reference),
            Some(source_offset),
        ));
    }
    let key = encode_reference(reference);
    let mask = slots.len() - 1;
    let mut slot = reference_slot(key, mask);
    for probe_index in 0..slots.len() {
        probe(cancellation, probe_index, reference, source_offset)?;
        match slots[slot] {
            0 => {
                slots[slot] = key;
                *count = count.checked_add(1).ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(source_offset),
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
        Some(source_offset),
    ))
}

fn seen_contains(
    slots: &[u64],
    reference: ObjectRef,
    cancellation: &dyn DocumentCancellation,
    source_offset: u64,
) -> Result<bool, DocumentError> {
    if slots.is_empty() || !slots.len().is_power_of_two() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(reference),
            Some(source_offset),
        ));
    }
    let key = encode_reference(reference);
    let mask = slots.len() - 1;
    let mut slot = reference_slot(key, mask);
    for probe_index in 0..slots.len() {
        probe(cancellation, probe_index, reference, source_offset)?;
        match slots[slot] {
            0 => return Ok(false),
            existing if existing == key => return Ok(true),
            _ => slot = (slot + 1) & mask,
        }
    }
    Err(DocumentError::for_code(
        DocumentErrorCode::InternalState,
        Some(reference),
        Some(source_offset),
    ))
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

fn capacity_bytes<T>(capacity: usize) -> Option<u64> {
    u64::try_from(capacity)
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<T>()).ok()?)
}
