use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteSource, DataTicket, JobId, RequestPriority, ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_object::{IndirectObjectValue, ObjectLimitKind, ObjectStats, ObjectWorkCaps};
use pdf_rs_syntax::{Located, ObjectRef, PdfReal, SyntaxObject};

use crate::dictionary::{
    collect_structural_fields, direct_dictionary, optional_field, reject_duplicate_field,
};
use crate::model::AttestedRevisionIndexOwner;
use crate::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, AttestedRevisionIndex,
    DocumentCancellation, DocumentError, DocumentErrorCode, DocumentLimitKind, InheritedPageValue,
    LocallyRepairedRevisionIndex, OpenAttestedObjectJob, PageBoxes, PageCoordinate, PageHandle,
    PageIndex, PageMaterializationLimits, PageRectangle, PageResourceScope, PageRotation,
    PageValueProvenance, SharedAttestedRevisionIndex, SharedLocallyRepairedRevisionIndex,
};

const INHERITED_FIELD_COUNT: usize = 4;

/// Runtime identity, child checkpoints, and priority for inherited page-value materialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageMaterializationJobContext {
    job: JobId,
    object_envelope_checkpoint: ResumeCheckpoint,
    object_boundary_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl PageMaterializationJobContext {
    /// Creates a context whose two proof-preserving object checkpoints remain runtime-owned.
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

/// Public phase of one inherited page-value materialization job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageMaterializationPhase {
    /// The next Page or Pages dictionary is being selected or opened.
    Ancestors,
    /// One whole-object inherited-value alias is being resolved.
    Aliases,
    /// MediaBox, effective CropBox/Rotate, and Resources were published.
    Ready,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Deterministic work and retained-state accounting for page materialization.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageMaterializationStats {
    objects_started: u64,
    ancestors_opened: u64,
    reference_edges: u64,
    max_alias_depth: u64,
    object_read_bytes: u64,
    object_parse_bytes: u64,
    retained_state_bytes: u64,
    peak_retained_state_bytes: u64,
}

impl PageMaterializationStats {
    /// Returns proof-preserving object jobs started across ancestors and aliases.
    pub const fn objects_started(self) -> u64 {
        self.objects_started
    }

    /// Returns Page or Pages dictionaries successfully reopened.
    pub const fn ancestors_opened(self) -> u64 {
        self.ancestors_opened
    }

    /// Returns whole-object direct-reference alias edges followed.
    pub const fn reference_edges(self) -> u64 {
        self.reference_edges
    }

    /// Returns the greatest root-to-terminal alias-chain length.
    pub const fn max_alias_depth(self) -> u64 {
        self.max_alias_depth
    }

    /// Returns cumulative exact-read bytes charged by child object jobs.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }

    /// Returns cumulative parser-window bytes charged by child object jobs.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }

    /// Returns retained bytes owned by the published materialized value.
    pub const fn retained_state_bytes(self) -> u64 {
        self.retained_state_bytes
    }

    /// Returns peak allocator-reported state retained while the job was active.
    pub const fn peak_retained_state_bytes(self) -> u64 {
        self.peak_retained_state_bytes
    }
}

/// Move-only inherited values and resource scope for one exact Page handle.
pub struct MaterializedPage {
    handle: PageHandle,
    boxes: PageBoxes,
    rotation: Option<InheritedPageValue<PageRotation>>,
    resources: PageResourceScope,
    limits: PageMaterializationLimits,
    stats: PageMaterializationStats,
}

impl MaterializedPage {
    /// Returns the exact source- and revision-bound Page handle.
    pub const fn handle(&self) -> PageHandle {
        self.handle
    }

    /// Returns the inherited MediaBox and effective CropBox.
    pub const fn boxes(&self) -> &PageBoxes {
        &self.boxes
    }

    /// Returns the inherited rotation or the normative zero-degree default.
    pub fn rotation(&self) -> PageRotation {
        self.rotation
            .as_ref()
            .map_or(PageRotation::Degrees0, |rotation| *rotation.value())
    }

    /// Returns exact Rotate provenance, or `None` when rotation defaulted to zero.
    pub fn rotation_provenance(&self) -> Option<&PageValueProvenance> {
        self.rotation.as_ref().map(InheritedPageValue::provenance)
    }

    /// Reports whether Rotate was absent on the complete inheritance chain.
    pub const fn rotation_defaults_to_zero(&self) -> bool {
        self.rotation.is_none()
    }

    /// Returns the nearest inherited Resources dictionary and proof chains.
    pub const fn resources(&self) -> &PageResourceScope {
        &self.resources
    }

    /// Returns the complete validated materialization limit profile.
    pub const fn limits(&self) -> PageMaterializationLimits {
        self.limits
    }

    /// Returns deterministic work and retained-state accounting.
    pub const fn stats(&self) -> PageMaterializationStats {
        self.stats
    }
}

impl fmt::Debug for MaterializedPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MaterializedPage")
            .field("handle", &self.handle)
            .field("boxes", &self.boxes)
            .field("rotation", &self.rotation())
            .field("rotation_is_default", &self.rotation_defaults_to_zero())
            .field("resources", &self.resources)
            .field("stats", &self.stats)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

/// Result of polling one inherited page-value materialization job.
#[allow(
    clippy::large_enum_variant,
    reason = "the move-only Ready value retains proof-bearing resource ownership inline"
)]
pub enum PageMaterializationPoll {
    /// The complete inherited page model is ready.
    Ready(MaterializedPage),
    /// The active ancestor or alias child requires exact source ranges.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the active child request.
        missing: SmallRanges,
        /// Child envelope or stream-boundary checkpoint retained while waiting.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a stable terminal failure.
    Failed(DocumentError),
}

impl fmt::Debug for PageMaterializationPoll {
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
enum MaterializationState {
    Active,
    Ready,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InheritedField {
    MediaBox,
    CropBox,
    Rotate,
    Resources,
}

impl InheritedField {
    const ALL: [Self; INHERITED_FIELD_COUNT] =
        [Self::MediaBox, Self::CropBox, Self::Rotate, Self::Resources];

    const fn index(self) -> usize {
        match self {
            Self::MediaBox => 0,
            Self::CropBox => 1,
            Self::Rotate => 2,
            Self::Resources => 3,
        }
    }

    const fn invalid_code(self) -> DocumentErrorCode {
        match self {
            Self::MediaBox | Self::CropBox => DocumentErrorCode::InvalidPageBox,
            Self::Rotate => DocumentErrorCode::InvalidPageRotation,
            Self::Resources => DocumentErrorCode::InvalidPageResources,
        }
    }
}

#[derive(Clone, Copy)]
struct AliasSeed {
    field: InheritedField,
    defining_object: ObjectRef,
    defining_value_offset: u64,
    defining_ancestor_index: usize,
    root: ObjectRef,
}

struct AliasState {
    seed: AliasSeed,
    chain: Vec<ObjectRef>,
}

#[derive(Clone, Copy)]
enum CurrentTarget {
    Ancestor {
        index: usize,
        reference: ObjectRef,
    },
    Alias {
        field: InheritedField,
        reference: ObjectRef,
    },
}

struct ChildState {
    job: OpenAttestedObjectJob,
    accounted_stats: ObjectStats,
    work_caps: ObjectWorkCaps,
    reference: ObjectRef,
    offset: u64,
}

/// One-shot bounded materialization of inherited page geometry and Resources.
///
/// The job walks only the exact Page-to-root chain already proven by the supplied immutable
/// [`PageIndex`]. Each ancestor is reopened at most once. MediaBox, CropBox, Rotate, and Resources
/// use nearest-definition inheritance; whole-value indirect aliases are followed under aggregate
/// edge/object budgets, while nested component aliases remain an explicit unsupported outcome.
pub struct MaterializePageJob<'index> {
    authority: AttestedRevisionIndexOwner<'index>,
    snapshot: SourceSnapshot,
    context: PageMaterializationJobContext,
    limits: PageMaterializationLimits,
    handle: PageHandle,
    ancestor_chain: Vec<ObjectRef>,
    next_ancestor: usize,
    pending_aliases: [Option<AliasSeed>; INHERITED_FIELD_COUNT],
    next_pending_alias: usize,
    active_alias: Option<AliasState>,
    current: Option<CurrentTarget>,
    child: Option<ChildState>,
    media_box: Option<InheritedPageValue<PageRectangle>>,
    crop_box: Option<InheritedPageValue<PageRectangle>>,
    rotation: Option<InheritedPageValue<PageRotation>>,
    resources: Option<PageResourceScope>,
    stats: PageMaterializationStats,
    state: MaterializationState,
    terminal_error: DocumentError,
}

impl MaterializePageJob<'_> {
    /// Returns the immutable source snapshot covered by the authority and Page handle.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns runtime identity, child checkpoints, and priority.
    pub const fn context(&self) -> PageMaterializationJobContext {
        self.context
    }

    /// Returns the complete validated materialization limits.
    pub const fn limits(&self) -> PageMaterializationLimits {
        self.limits
    }

    /// Returns the exact Page handle being materialized.
    pub const fn handle(&self) -> PageHandle {
        self.handle
    }

    /// Returns deterministic accounting through the latest poll.
    pub const fn stats(&self) -> PageMaterializationStats {
        self.stats
    }

    /// Returns the public resumable materialization phase.
    pub const fn phase(&self) -> PageMaterializationPhase {
        match self.state {
            MaterializationState::Ready => PageMaterializationPhase::Ready,
            MaterializationState::Failed => PageMaterializationPhase::Failed,
            MaterializationState::Active if self.active_alias.is_some() => {
                PageMaterializationPhase::Aliases
            }
            MaterializationState::Active => PageMaterializationPhase::Ancestors,
        }
    }

    /// Advances materialization without platform I/O or callback-owned resumption.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> PageMaterializationPoll {
        if !matches!(self.state, MaterializationState::Active) {
            return PageMaterializationPoll::Failed(self.terminal_error);
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
                    match self.schedule_next_target() {
                        Ok(true) => {}
                        Ok(false) => return self.finish_ready(),
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
            let child_retained_bytes = child.job.stats().retained_heap_bytes();
            if let Err(error) = self.refresh_peak_state_with(
                child_retained_bytes,
                child.reference,
                Some(child.offset),
            ) {
                return self.fail(error);
            }
            match outcome {
                AttestedObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    self.child = Some(child);
                    return PageMaterializationPoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    };
                }
                AttestedObjectPoll::Failed(error) => {
                    return self.fail(self.map_child_error(error, &child));
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
                    let accepted = match target {
                        CurrentTarget::Ancestor { index, reference } => self.accept_ancestor(
                            index,
                            reference,
                            object,
                            cancellation,
                            child_retained_bytes,
                        ),
                        CurrentTarget::Alias { field, reference } => {
                            self.accept_alias(field, reference, object, child_retained_bytes)
                        }
                    };
                    if let Err(error) = accepted {
                        return self.fail(error);
                    }
                }
            }
        }
    }
}

impl MaterializePageJob<'_> {
    fn schedule_next_target(&mut self) -> Result<bool, DocumentError> {
        if self.current.is_some() || self.child.is_some() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_reference(),
                self.current_offset(),
            ));
        }

        if let Some(alias) = &self.active_alias {
            let reference = alias.chain.last().copied().ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(alias.seed.root),
                    Some(alias.seed.defining_value_offset),
                )
            })?;
            self.current = Some(CurrentTarget::Alias {
                field: alias.seed.field,
                reference,
            });
            return Ok(true);
        }

        while self.next_pending_alias < INHERITED_FIELD_COUNT {
            let index = self.next_pending_alias;
            self.next_pending_alias += 1;
            let Some(seed) = self.pending_aliases[index].take() else {
                continue;
            };
            if seed.field.index() != index {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(seed.defining_object),
                    Some(seed.defining_value_offset),
                ));
            }
            let mut chain = Vec::new();
            let attempted = capacity_bytes::<ObjectRef>(1)?;
            self.ensure_state_budget_with(attempted, seed.root, Some(seed.defining_value_offset))?;
            chain.try_reserve_exact(1).map_err(|_| {
                DocumentError::page_materialization_resource(
                    DocumentLimitKind::PageMaterializationStateBytes,
                    self.limits.max_retained_state_bytes(),
                    self.current_retained_state_bytes().unwrap_or(u64::MAX),
                    attempted,
                    seed.root,
                    Some(seed.defining_value_offset),
                )
            })?;
            chain.push(seed.root);
            self.charge_reference_edge(seed.root, seed.defining_value_offset)?;
            self.active_alias = Some(AliasState { seed, chain });
            self.refresh_peak_state(seed.root, Some(seed.defining_value_offset))?;
            self.current = Some(CurrentTarget::Alias {
                field: seed.field,
                reference: seed.root,
            });
            return Ok(true);
        }

        self.pending_aliases = [None; INHERITED_FIELD_COUNT];
        self.next_pending_alias = 0;

        if self.all_fields_resolved() {
            return Ok(false);
        }
        let Some(reference) = self.ancestor_chain.get(self.next_ancestor).copied() else {
            return Ok(false);
        };
        let index = self.next_ancestor;
        self.current = Some(CurrentTarget::Ancestor { index, reference });
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
        let reference = match target {
            CurrentTarget::Ancestor { reference, .. } | CurrentTarget::Alias { reference, .. } => {
                reference
            }
        };
        let attestation = self.authority.attestation(reference)?;
        let offset = attestation.xref_offset();
        if self.stats.objects_started >= self.limits.max_objects() {
            return Err(DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationObjects,
                self.limits.max_objects(),
                self.stats.objects_started,
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
            return Err(DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationObjectReadBytes,
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
            return Err(DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationObjectParseBytes,
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
                        < self.authority.object_limits().max_total_read_bytes() =>
                {
                    return DocumentError::aggregate_object_resource(
                        DocumentLimitKind::PageMaterializationObjectReadBytes,
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
                        DocumentLimitKind::PageMaterializationObjectParseBytes,
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

    fn current_reference(&self) -> Option<ObjectRef> {
        match self.current {
            Some(CurrentTarget::Ancestor { reference, .. })
            | Some(CurrentTarget::Alias { reference, .. }) => Some(reference),
            None => self
                .active_alias
                .as_ref()
                .and_then(|alias| alias.chain.last().copied())
                .or_else(|| self.ancestor_chain.get(self.next_ancestor).copied())
                .or(Some(self.handle.object())),
        }
    }

    fn current_offset(&self) -> Option<u64> {
        self.current_reference()
            .and_then(|reference| self.authority.attestation(reference).ok())
            .map(crate::ObjectAttestation::xref_offset)
    }
}

impl MaterializePageJob<'_> {
    fn accept_ancestor(
        &mut self,
        ancestor_index: usize,
        reference: ObjectRef,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
        transient_object_bytes: u64,
    ) -> Result<(), DocumentError> {
        if object.reference() != reference
            || self.ancestor_chain.get(ancestor_index).copied() != Some(reference)
            || ancestor_index != self.next_ancestor
            || self.active_alias.is_some()
            || self.pending_aliases.iter().any(Option::is_some)
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(object.attestation().xref_offset()),
            ));
        }

        let object_offset = object.attestation().xref_offset();
        let dictionary = direct_dictionary(
            &object,
            self.snapshot,
            DocumentErrorCode::InvalidPageTreeNode,
        )?;
        let fields = collect_structural_fields(
            dictionary,
            [
                b"MediaBox".as_slice(),
                b"CropBox".as_slice(),
                b"Rotate".as_slice(),
                b"Resources".as_slice(),
            ],
            reference,
            cancellation,
        )?;
        for field_index in 0..INHERITED_FIELD_COUNT {
            reject_duplicate_field(&fields, field_index, reference)?;
        }

        let mut direct_resource_offset = None;
        for field in InheritedField::ALL {
            if self.field_is_resolved(field) {
                continue;
            }
            let Some(value) = optional_field(&fields, field.index()) else {
                continue;
            };
            if matches!(value.value(), SyntaxObject::Null) {
                continue;
            }
            if let Some(root) = value.value().as_reference() {
                let slot = &mut self.pending_aliases[field.index()];
                if slot.is_some() {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(value.span().start()),
                    ));
                }
                *slot = Some(AliasSeed {
                    field,
                    defining_object: reference,
                    defining_value_offset: value.span().start(),
                    defining_ancestor_index: ancestor_index,
                    root,
                });
                continue;
            }

            let provenance = PageValueProvenance::direct(reference, value.span().start());
            match field {
                InheritedField::MediaBox => {
                    let rectangle = parse_page_rectangle(value, reference)?;
                    self.media_box = Some(InheritedPageValue::new(rectangle, provenance));
                }
                InheritedField::CropBox => {
                    let rectangle = parse_page_rectangle(value, reference)?;
                    self.crop_box = Some(InheritedPageValue::new(rectangle, provenance));
                }
                InheritedField::Rotate => {
                    let rotation = parse_page_rotation(value, reference)?;
                    self.rotation = Some(InheritedPageValue::new(rotation, provenance));
                }
                InheritedField::Resources => {
                    if !matches!(value.value(), SyntaxObject::Dictionary(_)) {
                        return Err(DocumentError::for_code(
                            DocumentErrorCode::InvalidPageResources,
                            Some(reference),
                            Some(value.span().start()),
                        ));
                    }
                    direct_resource_offset = Some(value.span().start());
                }
            }
        }

        self.next_ancestor = self.next_ancestor.checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(object_offset),
            )
        })?;
        self.next_pending_alias = 0;
        self.stats.ancestors_opened =
            self.stats.ancestors_opened.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(object_offset),
                )
            })?;

        if let Some(value_offset) = direct_resource_offset {
            let lookup_chain = self.copy_ancestor_prefix(
                ancestor_index,
                reference,
                Some(value_offset),
                transient_object_bytes,
            )?;
            let additional = transient_object_bytes
                .checked_add(capacity_bytes::<ObjectRef>(lookup_chain.capacity())?)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(value_offset),
                    )
                })?;
            self.ensure_state_budget_with(additional, reference, Some(value_offset))?;
            self.resources = Some(PageResourceScope::direct(
                reference,
                value_offset,
                lookup_chain,
                object,
            )?);
            return self.refresh_peak_state(reference, Some(object_offset));
        }
        self.refresh_peak_state_with(transient_object_bytes, reference, Some(object_offset))
    }

    fn accept_alias(
        &mut self,
        field: InheritedField,
        reference: ObjectRef,
        object: AttestedObject,
        transient_object_bytes: u64,
    ) -> Result<(), DocumentError> {
        let mut alias = self.active_alias.take().ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(object.attestation().xref_offset()),
            )
        })?;
        if alias.seed.field != field
            || alias.chain.last().copied() != Some(reference)
            || object.reference() != reference
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(object.attestation().xref_offset()),
            ));
        }
        let transient_alias_bytes = capacity_bytes::<ObjectRef>(alias.chain.capacity())?;

        match object.value() {
            IndirectObjectValue::Direct(value) => match value.value() {
                SyntaxObject::Reference(next) => {
                    self.advance_alias(
                        &mut alias,
                        *next,
                        value.span().start(),
                        transient_object_bytes,
                    )?;
                    self.active_alias = Some(alias);
                    self.refresh_peak_state_with(
                        transient_object_bytes,
                        reference,
                        Some(value.span().start()),
                    )
                }
                SyntaxObject::Null => {
                    let transient = transient_object_bytes
                        .checked_add(transient_alias_bytes)
                        .ok_or_else(|| {
                            DocumentError::for_code(
                                DocumentErrorCode::InternalState,
                                Some(reference),
                                Some(value.span().start()),
                            )
                        })?;
                    self.refresh_peak_state_with(
                        transient,
                        alias.seed.defining_object,
                        Some(alias.seed.defining_value_offset),
                    )
                }
                terminal => {
                    let terminal_offset = value.span().start();
                    match field {
                        InheritedField::MediaBox | InheritedField::CropBox => {
                            let rectangle = parse_page_rectangle(value, reference)?;
                            let provenance = PageValueProvenance::indirect(
                                alias.seed.defining_object,
                                alias.seed.defining_value_offset,
                                mem::take(&mut alias.chain),
                            )
                            .ok_or_else(|| {
                                DocumentError::for_code(
                                    DocumentErrorCode::InternalState,
                                    Some(reference),
                                    Some(alias.seed.defining_value_offset),
                                )
                            })?;
                            let inherited = InheritedPageValue::new(rectangle, provenance);
                            match field {
                                InheritedField::MediaBox => self.media_box = Some(inherited),
                                InheritedField::CropBox => self.crop_box = Some(inherited),
                                InheritedField::Rotate | InheritedField::Resources => {
                                    return Err(DocumentError::for_code(
                                        DocumentErrorCode::InternalState,
                                        Some(reference),
                                        Some(terminal_offset),
                                    ));
                                }
                            }
                        }
                        InheritedField::Rotate => {
                            let rotation = parse_page_rotation(value, reference)?;
                            let provenance = PageValueProvenance::indirect(
                                alias.seed.defining_object,
                                alias.seed.defining_value_offset,
                                mem::take(&mut alias.chain),
                            )
                            .ok_or_else(|| {
                                DocumentError::for_code(
                                    DocumentErrorCode::InternalState,
                                    Some(reference),
                                    Some(alias.seed.defining_value_offset),
                                )
                            })?;
                            self.rotation = Some(InheritedPageValue::new(rotation, provenance));
                        }
                        InheritedField::Resources => {
                            if !matches!(terminal, SyntaxObject::Dictionary(_)) {
                                return Err(DocumentError::for_code(
                                    DocumentErrorCode::InvalidPageResources,
                                    Some(reference),
                                    Some(terminal_offset),
                                ));
                            }
                            let lookup_chain = self.copy_ancestor_prefix(
                                alias.seed.defining_ancestor_index,
                                alias.seed.defining_object,
                                Some(alias.seed.defining_value_offset),
                                transient_object_bytes
                                    .checked_add(transient_alias_bytes)
                                    .ok_or_else(|| {
                                        DocumentError::for_code(
                                            DocumentErrorCode::InternalState,
                                            Some(reference),
                                            Some(terminal_offset),
                                        )
                                    })?,
                            )?;
                            let additional = transient_object_bytes
                                .checked_add(transient_alias_bytes)
                                .and_then(|value| {
                                    value.checked_add(
                                        capacity_bytes::<ObjectRef>(lookup_chain.capacity())
                                            .ok()?,
                                    )
                                })
                                .ok_or_else(|| {
                                    DocumentError::for_code(
                                        DocumentErrorCode::InternalState,
                                        Some(reference),
                                        Some(terminal_offset),
                                    )
                                })?;
                            self.ensure_state_budget_with(
                                additional,
                                reference,
                                Some(terminal_offset),
                            )?;
                            self.resources = Some(PageResourceScope::indirect(
                                alias.seed.defining_object,
                                alias.seed.defining_value_offset,
                                lookup_chain,
                                mem::take(&mut alias.chain),
                                object,
                            )?);
                            return self.refresh_peak_state(reference, Some(terminal_offset));
                        }
                    }
                    self.refresh_peak_state_with(
                        transient_object_bytes,
                        reference,
                        Some(terminal_offset),
                    )
                }
            },
            IndirectObjectValue::Stream(_) => Err(DocumentError::for_code(
                field.invalid_code(),
                Some(reference),
                Some(object.attestation().xref_offset()),
            )),
        }
    }

    fn advance_alias(
        &mut self,
        alias: &mut AliasState,
        next: ObjectRef,
        edge_offset: u64,
        transient_object_bytes: u64,
    ) -> Result<(), DocumentError> {
        if alias.chain.contains(&next) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageValueAliasCycle,
                Some(next),
                Some(edge_offset),
            ));
        }
        self.charge_reference_edge(next, edge_offset)?;
        let requested_capacity = alias.chain.len().checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(next),
                Some(edge_offset),
            )
        })?;
        self.ensure_state_budget_with_extra(
            capacity_bytes::<ObjectRef>(requested_capacity)?,
            transient_object_bytes,
            next,
            Some(edge_offset),
        )?;
        alias.chain.try_reserve_exact(1).map_err(|_| {
            DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationStateBytes,
                self.limits.max_retained_state_bytes(),
                self.current_retained_state_bytes().unwrap_or(u64::MAX),
                capacity_bytes::<ObjectRef>(1).unwrap_or(u64::MAX),
                next,
                Some(edge_offset),
            )
        })?;
        self.ensure_state_budget_with_extra(
            capacity_bytes::<ObjectRef>(alias.chain.capacity())?,
            transient_object_bytes,
            next,
            Some(edge_offset),
        )?;
        alias.chain.push(next);
        self.stats.max_alias_depth = self
            .stats
            .max_alias_depth
            .max(u64::try_from(alias.chain.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    fn charge_reference_edge(
        &mut self,
        reference: ObjectRef,
        offset: u64,
    ) -> Result<(), DocumentError> {
        if self.stats.reference_edges >= self.limits.max_reference_edges() {
            return Err(DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationReferenceEdges,
                self.limits.max_reference_edges(),
                self.stats.reference_edges,
                1,
                reference,
                Some(offset),
            ));
        }
        self.stats.reference_edges =
            self.stats.reference_edges.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        self.stats.max_alias_depth = self.stats.max_alias_depth.max(1);
        Ok(())
    }
}

impl MaterializePageJob<'_> {
    fn field_is_resolved(&self, field: InheritedField) -> bool {
        match field {
            InheritedField::MediaBox => self.media_box.is_some(),
            InheritedField::CropBox => self.crop_box.is_some(),
            InheritedField::Rotate => self.rotation.is_some(),
            InheritedField::Resources => self.resources.is_some(),
        }
    }

    fn all_fields_resolved(&self) -> bool {
        self.media_box.is_some()
            && self.crop_box.is_some()
            && self.rotation.is_some()
            && self.resources.is_some()
    }

    fn copy_ancestor_prefix(
        &self,
        defining_index: usize,
        defining_object: ObjectRef,
        offset: Option<u64>,
        transient_bytes: u64,
    ) -> Result<Vec<ObjectRef>, DocumentError> {
        let end = defining_index.checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(defining_object),
                offset,
            )
        })?;
        let prefix = self.ancestor_chain.get(..end).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(defining_object),
                offset,
            )
        })?;
        if prefix.last().copied() != Some(defining_object) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(defining_object),
                offset,
            ));
        }
        let requested = capacity_bytes::<ObjectRef>(prefix.len())?;
        self.ensure_state_budget_with_extra(requested, transient_bytes, defining_object, offset)?;
        let mut copy = Vec::new();
        copy.try_reserve_exact(prefix.len()).map_err(|_| {
            DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationStateBytes,
                self.limits.max_retained_state_bytes(),
                self.current_retained_state_bytes().unwrap_or(u64::MAX),
                requested,
                defining_object,
                offset,
            )
        })?;
        copy.extend_from_slice(prefix);
        let actual = capacity_bytes::<ObjectRef>(copy.capacity())?;
        self.ensure_state_budget_with_extra(actual, transient_bytes, defining_object, offset)?;
        Ok(copy)
    }

    fn current_retained_state_bytes(&self) -> Result<u64, DocumentError> {
        let mut total = capacity_bytes::<ObjectRef>(self.ancestor_chain.capacity())?;
        if let Some(child) = &self.child {
            total = total
                .checked_add(child.job.stats().retained_heap_bytes())
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(child.reference),
                        Some(child.offset),
                    )
                })?;
        }
        if let Some(alias) = &self.active_alias {
            total = total
                .checked_add(capacity_bytes::<ObjectRef>(alias.chain.capacity())?)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(alias.seed.root),
                        Some(alias.seed.defining_value_offset),
                    )
                })?;
        }
        for inherited in [&self.media_box, &self.crop_box].into_iter().flatten() {
            total = total
                .checked_add(
                    inherited
                        .provenance()
                        .retained_alias_chain_bytes()
                        .ok_or_else(|| {
                            DocumentError::for_code(
                                DocumentErrorCode::InternalState,
                                Some(inherited.provenance().defining_object()),
                                Some(inherited.provenance().defining_value_offset()),
                            )
                        })?,
                )
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(inherited.provenance().defining_object()),
                        Some(inherited.provenance().defining_value_offset()),
                    )
                })?;
        }
        if let Some(rotation) = &self.rotation {
            total = total
                .checked_add(
                    rotation
                        .provenance()
                        .retained_alias_chain_bytes()
                        .ok_or_else(|| {
                            DocumentError::for_code(
                                DocumentErrorCode::InternalState,
                                Some(rotation.provenance().defining_object()),
                                Some(rotation.provenance().defining_value_offset()),
                            )
                        })?,
                )
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(rotation.provenance().defining_object()),
                        Some(rotation.provenance().defining_value_offset()),
                    )
                })?;
        }
        if let Some(resources) = &self.resources {
            total = total
                .checked_add(resources.checked_retained_state_bytes()?)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(resources.defining_object()),
                        Some(resources.defining_value_offset()),
                    )
                })?;
        }
        Ok(total)
    }

    fn published_retained_state_bytes(
        media_box: &InheritedPageValue<PageRectangle>,
        crop_box: Option<&InheritedPageValue<PageRectangle>>,
        rotation: Option<&InheritedPageValue<PageRotation>>,
        resources: &PageResourceScope,
    ) -> Result<u64, DocumentError> {
        let mut total = media_box
            .provenance()
            .retained_alias_chain_bytes()
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(media_box.provenance().defining_object()),
                    Some(media_box.provenance().defining_value_offset()),
                )
            })?;
        if let Some(crop_box) = crop_box {
            total = total
                .checked_add(
                    crop_box
                        .provenance()
                        .retained_alias_chain_bytes()
                        .ok_or_else(|| {
                            DocumentError::for_code(
                                DocumentErrorCode::InternalState,
                                Some(crop_box.provenance().defining_object()),
                                Some(crop_box.provenance().defining_value_offset()),
                            )
                        })?,
                )
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(crop_box.provenance().defining_object()),
                        Some(crop_box.provenance().defining_value_offset()),
                    )
                })?;
        }
        if let Some(rotation) = rotation {
            total = total
                .checked_add(
                    rotation
                        .provenance()
                        .retained_alias_chain_bytes()
                        .ok_or_else(|| {
                            DocumentError::for_code(
                                DocumentErrorCode::InternalState,
                                Some(rotation.provenance().defining_object()),
                                Some(rotation.provenance().defining_value_offset()),
                            )
                        })?,
                )
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(rotation.provenance().defining_object()),
                        Some(rotation.provenance().defining_value_offset()),
                    )
                })?;
        }
        total
            .checked_add(resources.checked_retained_state_bytes()?)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(resources.defining_object()),
                    Some(resources.defining_value_offset()),
                )
            })
    }

    fn ensure_state_budget_with(
        &self,
        additional: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        self.ensure_state_budget_with_extra(additional, 0, reference, offset)
    }

    fn ensure_state_budget_with_extra(
        &self,
        additional: u64,
        transient: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        let consumed = self.current_retained_state_bytes()?;
        let attempted = transient.checked_add(additional).ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
        })?;
        let prospective = consumed.checked_add(attempted).ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
        })?;
        if prospective > self.limits.max_retained_state_bytes() {
            return Err(DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationStateBytes,
                self.limits.max_retained_state_bytes(),
                consumed,
                attempted,
                reference,
                offset,
            ));
        }
        Ok(())
    }

    fn refresh_peak_state(
        &mut self,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        self.refresh_peak_state_with(0, reference, offset)
    }

    fn refresh_peak_state_with(
        &mut self,
        transient: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        let retained = self
            .current_retained_state_bytes()?
            .checked_add(transient)
            .ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
            })?;
        if retained > self.limits.max_retained_state_bytes() {
            return Err(DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationStateBytes,
                self.limits.max_retained_state_bytes(),
                0,
                retained,
                reference,
                offset,
            ));
        }
        self.stats.peak_retained_state_bytes = self.stats.peak_retained_state_bytes.max(retained);
        Ok(())
    }

    fn finish_ready(&mut self) -> PageMaterializationPoll {
        if self.active_alias.is_some()
            || self.current.is_some()
            || self.child.is_some()
            || self.pending_aliases.iter().any(Option::is_some)
        {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.handle.object()),
                None,
            ));
        }
        let Some(media_box) = self.media_box.take() else {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::MissingPageMediaBox,
                Some(self.handle.object()),
                None,
            ));
        };
        let crop_box = self.crop_box.take();
        let rotation = self.rotation.take();
        let Some(resources) = self.resources.take() else {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::MissingPageResources,
                Some(self.handle.object()),
                None,
            ));
        };
        let retained = match Self::published_retained_state_bytes(
            &media_box,
            crop_box.as_ref(),
            rotation.as_ref(),
            &resources,
        ) {
            Ok(retained) => retained,
            Err(error) => return self.fail(error),
        };
        if retained > self.limits.max_retained_state_bytes() {
            return self.fail(DocumentError::page_materialization_resource(
                DocumentLimitKind::PageMaterializationStateBytes,
                self.limits.max_retained_state_bytes(),
                0,
                retained,
                self.handle.object(),
                None,
            ));
        }
        self.stats.retained_state_bytes = retained;
        self.stats.peak_retained_state_bytes = self.stats.peak_retained_state_bytes.max(retained);
        let value = MaterializedPage {
            handle: self.handle,
            boxes: PageBoxes::new(media_box, crop_box),
            rotation,
            resources,
            limits: self.limits,
            stats: self.stats,
        };
        self.ancestor_chain.clear();
        self.state = MaterializationState::Ready;
        self.terminal_error = DocumentError::for_code(
            DocumentErrorCode::JobAlreadyComplete,
            Some(self.handle.object()),
            None,
        );
        PageMaterializationPoll::Ready(value)
    }

    fn fail(&mut self, error: DocumentError) -> PageMaterializationPoll {
        self.child = None;
        self.current = None;
        self.active_alias = None;
        self.pending_aliases = [None; INHERITED_FIELD_COUNT];
        self.media_box = None;
        self.crop_box = None;
        self.rotation = None;
        self.resources = None;
        self.ancestor_chain.clear();
        self.state = MaterializationState::Failed;
        self.terminal_error = error;
        PageMaterializationPoll::Failed(error)
    }
}

fn capacity_bytes<T>(capacity: usize) -> Result<u64, DocumentError> {
    u64::try_from(capacity)
        .ok()
        .and_then(|count| {
            u64::try_from(mem::size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))
}

fn parse_page_rectangle(
    value: &Located<SyntaxObject>,
    reference: ObjectRef,
) -> Result<PageRectangle, DocumentError> {
    let SyntaxObject::Array(array) = value.value() else {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidPageBox,
            Some(reference),
            Some(value.span().start()),
        ));
    };
    let [left, bottom, right, top] = array.values() else {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidPageBox,
            Some(reference),
            Some(value.span().start()),
        ));
    };
    let coordinates = [
        parse_page_coordinate(left, reference)?,
        parse_page_coordinate(bottom, reference)?,
        parse_page_coordinate(right, reference)?,
        parse_page_coordinate(top, reference)?,
    ];
    PageRectangle::new(coordinates).ok_or_else(|| {
        DocumentError::for_code(
            DocumentErrorCode::InvalidPageBox,
            Some(reference),
            Some(value.span().start()),
        )
    })
}

fn parse_page_coordinate(
    value: &Located<SyntaxObject>,
    reference: ObjectRef,
) -> Result<PageCoordinate, DocumentError> {
    match value.value() {
        SyntaxObject::Integer(integer) => PageCoordinate::from_integer(*integer).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InvalidPageBox,
                Some(reference),
                Some(value.span().start()),
            )
        }),
        SyntaxObject::Real(real) => parse_page_real(real)
            .map(PageCoordinate::from_scaled)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InvalidPageBox,
                    Some(reference),
                    Some(value.span().start()),
                )
            }),
        SyntaxObject::Reference(_) => Err(DocumentError::for_code(
            DocumentErrorCode::UnsupportedPageValueRepresentation,
            Some(reference),
            Some(value.span().start()),
        )),
        SyntaxObject::Null
        | SyntaxObject::Boolean(_)
        | SyntaxObject::Name(_)
        | SyntaxObject::String(_)
        | SyntaxObject::Array(_)
        | SyntaxObject::Dictionary(_) => Err(DocumentError::for_code(
            DocumentErrorCode::InvalidPageBox,
            Some(reference),
            Some(value.span().start()),
        )),
    }
}

fn parse_page_rotation(
    value: &Located<SyntaxObject>,
    reference: ObjectRef,
) -> Result<PageRotation, DocumentError> {
    let SyntaxObject::Integer(degrees) = value.value() else {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidPageRotation,
            Some(reference),
            Some(value.span().start()),
        ));
    };
    PageRotation::from_degrees(*degrees).ok_or_else(|| {
        DocumentError::for_code(
            DocumentErrorCode::InvalidPageRotation,
            Some(reference),
            Some(value.span().start()),
        )
    })
}

fn parse_page_real(real: &PdfReal) -> Option<i64> {
    parse_page_real_bytes(real.raw())
}

fn parse_page_real_bytes(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let (negative, unsigned) = match bytes[0] {
        b'-' => (true, bytes.get(1..)?),
        b'+' => (false, bytes.get(1..)?),
        _ => (false, bytes),
    };
    if unsigned.is_empty() {
        return None;
    }

    let exponent_index = unsigned.iter().position(|byte| matches!(byte, b'e' | b'E'));
    let (mantissa, exponent_bytes) = match exponent_index {
        Some(index) => (
            unsigned.get(..index)?,
            Some(unsigned.get(index.checked_add(1)?..)?),
        ),
        None => (unsigned, None),
    };
    if mantissa.is_empty() {
        return None;
    }
    let exponent = exponent_bytes.map_or(Some(0_i64), parse_signed_decimal)?;

    let mut magnitude = 0_u128;
    let mut fractional_digits = 0_i64;
    let mut seen_decimal = false;
    let mut digits = 0_usize;
    for byte in mantissa {
        match *byte {
            b'.' if !seen_decimal => seen_decimal = true,
            b'0'..=b'9' => {
                magnitude = magnitude
                    .checked_mul(10)?
                    .checked_add(u128::from(*byte - b'0'))?;
                digits = digits.checked_add(1)?;
                if seen_decimal {
                    fractional_digits = fractional_digits.checked_add(1)?;
                }
            }
            _ => return None,
        }
    }
    if digits == 0 {
        return None;
    }
    if magnitude == 0 {
        return Some(0);
    }

    let decimal_shift = exponent.checked_sub(fractional_digits)?.checked_add(9)?;
    let scaled_magnitude = if decimal_shift >= 0 {
        let shift = u32::try_from(decimal_shift).ok()?;
        magnitude.checked_mul(10_u128.checked_pow(shift)?)?
    } else {
        let shift = u32::try_from(decimal_shift.checked_neg()?).ok()?;
        let divisor = 10_u128.checked_pow(shift)?;
        if !magnitude.is_multiple_of(divisor) {
            return None;
        }
        magnitude / divisor
    };
    signed_i64(scaled_magnitude, negative)
}

fn parse_signed_decimal(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let (negative, digits) = match bytes[0] {
        b'-' => (true, bytes.get(1..)?),
        b'+' => (false, bytes.get(1..)?),
        _ => (false, bytes),
    };
    if digits.is_empty() {
        return None;
    }
    let mut magnitude = 0_u128;
    for byte in digits {
        if !byte.is_ascii_digit() {
            return None;
        }
        magnitude = magnitude
            .checked_mul(10)?
            .checked_add(u128::from(*byte - b'0'))?;
    }
    signed_i64(magnitude, negative)
}

fn signed_i64(magnitude: u128, negative: bool) -> Option<i64> {
    if !negative {
        return i64::try_from(magnitude).ok();
    }
    let minimum_magnitude = (i64::MAX as u128).checked_add(1)?;
    if magnitude > minimum_magnitude {
        return None;
    }
    if magnitude == minimum_magnitude {
        Some(i64::MIN)
    } else {
        i64::try_from(magnitude).ok()?.checked_neg()
    }
}

impl fmt::Debug for MaterializePageJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MaterializePageJob")
            .field("snapshot", &self.snapshot)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("handle", &self.handle)
            .field("phase", &self.phase())
            .field("stats", &self.stats)
            .field("ancestor_count", &self.ancestor_chain.len())
            .field("next_ancestor", &self.next_ancestor)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl AttestedRevisionIndex {
    /// Materializes inherited page values while borrowing this strict revision proof.
    pub fn materialize_page(
        &self,
        page_index: &PageIndex,
        handle: PageHandle,
        context: PageMaterializationJobContext,
        limits: PageMaterializationLimits,
    ) -> Result<MaterializePageJob<'_>, DocumentError> {
        materialize_page_with_owner(
            AttestedRevisionIndexOwner::Borrowed(self),
            page_index,
            handle,
            context,
            limits,
        )
    }
}

impl SharedAttestedRevisionIndex {
    /// Materializes inherited page values in a job owning this strict proof handle.
    pub fn materialize_page_owned(
        &self,
        page_index: &PageIndex,
        handle: PageHandle,
        context: PageMaterializationJobContext,
        limits: PageMaterializationLimits,
    ) -> Result<MaterializePageJob<'static>, DocumentError> {
        materialize_page_with_owner(
            AttestedRevisionIndexOwner::Shared(self.clone()),
            page_index,
            handle,
            context,
            limits,
        )
    }
}

impl LocallyRepairedRevisionIndex {
    /// Materializes inherited page values while retaining this repaired proof typestate.
    pub fn materialize_page(
        &self,
        page_index: &PageIndex,
        handle: PageHandle,
        context: PageMaterializationJobContext,
        limits: PageMaterializationLimits,
    ) -> Result<MaterializePageJob<'_>, DocumentError> {
        materialize_page_with_owner(
            AttestedRevisionIndexOwner::RepairedBorrowed(self),
            page_index,
            handle,
            context,
            limits,
        )
    }
}

impl SharedLocallyRepairedRevisionIndex {
    /// Materializes inherited page values in a job owning this repaired proof handle.
    pub fn materialize_page_owned(
        &self,
        page_index: &PageIndex,
        handle: PageHandle,
        context: PageMaterializationJobContext,
        limits: PageMaterializationLimits,
    ) -> Result<MaterializePageJob<'static>, DocumentError> {
        materialize_page_with_owner(
            AttestedRevisionIndexOwner::RepairedShared(self.clone()),
            page_index,
            handle,
            context,
            limits,
        )
    }
}

fn materialize_page_with_owner<'index>(
    authority: AttestedRevisionIndexOwner<'index>,
    page_index: &PageIndex,
    handle: PageHandle,
    context: PageMaterializationJobContext,
    limits: PageMaterializationLimits,
) -> Result<MaterializePageJob<'index>, DocumentError> {
    let (snapshot, ancestor_chain) = {
        let attested = authority.as_attested();
        let root = attested.root();
        let root_offset = attested.attestation(root)?.xref_offset();
        if context.object_envelope_checkpoint() == context.object_boundary_checkpoint() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageMaterializationJobContext,
                Some(handle.object()),
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
        page_index.validate_handle(handle)?;
        let ancestor_chain = page_index.inheritance_chain(
            handle,
            limits.max_ancestor_depth(),
            limits.max_retained_state_bytes(),
        )?;
        (attested.snapshot(), ancestor_chain)
    };
    let retained = capacity_bytes::<ObjectRef>(ancestor_chain.capacity())?;
    if retained > limits.max_retained_state_bytes() {
        return Err(DocumentError::page_materialization_resource(
            DocumentLimitKind::PageMaterializationStateBytes,
            limits.max_retained_state_bytes(),
            0,
            retained,
            handle.object(),
            None,
        ));
    }
    Ok(MaterializePageJob {
        authority,
        snapshot,
        context,
        limits,
        handle,
        ancestor_chain,
        next_ancestor: 0,
        pending_aliases: [None; INHERITED_FIELD_COUNT],
        next_pending_alias: 0,
        active_alias: None,
        current: None,
        child: None,
        media_box: None,
        crop_box: None,
        rotation: None,
        resources: None,
        stats: PageMaterializationStats {
            peak_retained_state_bytes: retained,
            ..PageMaterializationStats::default()
        },
        state: MaterializationState::Active,
        terminal_error: DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(handle.object()),
            None,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_page_real_bytes, signed_i64};
    use crate::PageCoordinate;

    #[test]
    fn page_real_conversion_is_exact_for_decimal_and_exponent_forms() {
        assert_eq!(parse_page_real_bytes(b"1.25"), Some(1_250_000_000));
        assert_eq!(parse_page_real_bytes(b"-0.5"), Some(-500_000_000));
        assert_eq!(parse_page_real_bytes(b"1.25e2"), Some(125_000_000_000));
        assert_eq!(parse_page_real_bytes(b"125e-2"), Some(1_250_000_000));
        assert_eq!(
            parse_page_real_bytes(b"1.0000000000"),
            Some(PageCoordinate::SCALE)
        );
        assert_eq!(parse_page_real_bytes(b"0.0000000001"), None);
        assert_eq!(parse_page_real_bytes(b"1e100000"), None);
        assert_eq!(signed_i64((i64::MAX as u128) + 1, true), Some(i64::MIN));
    }
}
