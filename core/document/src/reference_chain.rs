use std::error::Error;
use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteSource, DataTicket, JobId, RequestPriority, ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_object::{IndirectObjectValue, ObjectLimitKind, ObjectStats, ObjectWorkCaps};
use pdf_rs_syntax::{ObjectRef, SyntaxObject};

use crate::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, AttestedRevisionIndex,
    DocumentCancellation, DocumentError, DocumentErrorCategory, DocumentErrorCode, DocumentLimit,
    DocumentLimitKind, DocumentRecoverability, DocumentResidentFootprint, OpenAttestedObjectJob,
    ReferenceChainLimits,
};

const CANCELLATION_PROBE_INTERVAL: usize = 256;

/// Runtime identity, lower checkpoints, and priority for one reference-chain job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceChainJobContext {
    job: JobId,
    object_envelope_checkpoint: ResumeCheckpoint,
    object_boundary_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl ReferenceChainJobContext {
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

    /// Returns the scheduling priority copied to child exact reads.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }
}

/// Public four-state phase of one bounded top-level direct-reference chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceChainPhase {
    /// No child object job has started.
    Unresolved,
    /// At least one exact reference is being opened or followed.
    Resolving,
    /// The terminal proof-bound object was returned.
    Ready,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Deterministic work and retained-path accounting for one reference-chain job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReferenceChainStats {
    objects_started: u64,
    reference_edges: u64,
    max_depth: u64,
    object_read_bytes: u64,
    object_parse_bytes: u64,
    retained_path_bytes: u64,
}

impl ReferenceChainStats {
    /// Returns proof-preserving child object jobs successfully started.
    pub const fn objects_started(self) -> u64 {
        self.objects_started
    }

    /// Returns top-level direct-reference edges charged before traversal.
    pub const fn reference_edges(self) -> u64 {
        self.reference_edges
    }

    /// Returns the greatest distinct-reference depth reached.
    pub const fn max_depth(self) -> u64 {
        self.max_depth
    }

    /// Returns cumulative exact-read bytes charged by child object jobs.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }

    /// Returns cumulative parser-window bytes charged by child object jobs.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }

    /// Returns allocator-reported byte capacity retained by the path vector.
    pub const fn retained_path_bytes(self) -> u64 {
        self.retained_path_bytes
    }
}

/// Move-only exact-reference chain represented as a prefix and one terminal identity.
#[derive(Eq, PartialEq)]
pub struct ReferenceChain {
    prefix: Vec<ObjectRef>,
    terminal: ObjectRef,
}

impl ReferenceChain {
    fn single(reference: ObjectRef) -> Self {
        Self {
            prefix: Vec::new(),
            terminal: reference,
        }
    }

    /// Returns all identities before the terminal or rejected chain identity.
    pub fn prefix(&self) -> &[ObjectRef] {
        &self.prefix
    }

    /// Returns the terminal identity, including a repeated cycle-closing reference on failure.
    pub const fn terminal(&self) -> ObjectRef {
        self.terminal
    }

    /// Returns the originally requested root identity.
    pub fn root(&self) -> ObjectRef {
        self.prefix.first().copied().unwrap_or(self.terminal)
    }

    /// Returns the number of references in the complete prefix-plus-terminal chain.
    pub fn len(&self) -> usize {
        self.prefix.len() + 1
    }

    /// Reports whether the chain contains no references.
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Returns one exact identity by complete-chain position.
    pub fn get(&self, index: usize) -> Option<ObjectRef> {
        if index < self.prefix.len() {
            self.prefix.get(index).copied()
        } else if index == self.prefix.len() {
            Some(self.terminal)
        } else {
            None
        }
    }

    /// Iterates over the complete prefix-plus-terminal chain without allocation.
    pub fn iter(&self) -> impl Iterator<Item = ObjectRef> + '_ {
        self.prefix
            .iter()
            .copied()
            .chain(std::iter::once(self.terminal))
    }

    fn capacity_bytes(&self, offset: Option<u64>) -> Result<u64, DocumentError> {
        crate::residency::checked_capacity_bytes(
            self.prefix.capacity(),
            mem::size_of::<ObjectRef>(),
            self.terminal,
            offset,
        )
    }
}

impl fmt::Debug for ReferenceChain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReferenceChain")
            .field("len", &self.len())
            .field("references", &"[REDACTED]")
            .finish()
    }
}

/// Terminal object returned beside the exact chain used to reach it.
pub struct ResolvedReference {
    chain: ReferenceChain,
    object: AttestedObject,
    limits: ReferenceChainLimits,
    stats: ReferenceChainStats,
}

impl ResolvedReference {
    /// Returns the requested root identity.
    pub fn root(&self) -> ObjectRef {
        self.chain
            .prefix
            .first()
            .copied()
            .unwrap_or(self.chain.terminal)
    }

    /// Returns the final exact identity whose non-reference value terminated the chain.
    pub const fn terminal_reference(&self) -> ObjectRef {
        self.chain.terminal
    }

    /// Returns the final exact identity whose non-reference value terminated the chain.
    pub const fn terminal(&self) -> ObjectRef {
        self.chain.terminal
    }

    /// Borrows the complete successful reference chain.
    pub const fn chain(&self) -> &ReferenceChain {
        &self.chain
    }

    /// Borrows the proof-bound terminal object without detaching its attestation.
    pub const fn object(&self) -> &AttestedObject {
        &self.object
    }

    /// Returns the complete validated resolution profile that produced this value.
    ///
    /// A future Ready store must include this profile in its key so a warm result cannot bypass a
    /// stricter cold-path object, edge, depth, retained-path, read, or parse budget.
    pub const fn limits(&self) -> ReferenceChainLimits {
        self.limits
    }

    /// Returns deterministic work and retained-path accounting for this resolution.
    pub const fn stats(&self) -> ReferenceChainStats {
        self.stats
    }

    /// Computes the checked value-owned footprint suitable for future cache admission.
    ///
    /// The inline component already contains the terminal `AttestedObject` and
    /// chain vector header. Only the terminal object's syntax heap and the
    /// chain's separately allocated backing capacity are added.
    pub fn try_resident_footprint(&self) -> Result<DocumentResidentFootprint, DocumentError> {
        let offset = Some(self.object.attestation().xref_offset());
        DocumentResidentFootprint::for_value::<Self>(
            self.object.syntax_heap_bytes(),
            self.chain.capacity_bytes(offset)?,
            self.terminal_reference(),
            offset,
        )
    }
}

impl fmt::Debug for ResolvedReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedReference")
            .field("root", &self.root())
            .field("terminal_reference", &self.terminal_reference())
            .field("chain", &self.chain)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("object", &"[REDACTED]")
            .finish()
    }
}

/// Stable move-only chain failure retaining the complete attempted reference path.
#[derive(Eq, PartialEq)]
pub struct ReferenceChainError {
    cause: DocumentError,
    chain: ReferenceChain,
}

impl ReferenceChainError {
    fn single(cause: DocumentError, reference: ObjectRef) -> Self {
        Self {
            cause,
            chain: ReferenceChain::single(reference),
        }
    }

    /// Returns the stable document-layer failure code.
    pub const fn code(&self) -> DocumentErrorCode {
        self.cause.code()
    }

    /// Returns the stable coarse failure category.
    pub const fn category(&self) -> DocumentErrorCategory {
        self.cause.category()
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(&self) -> DocumentRecoverability {
        self.cause.recoverability()
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(&self) -> &'static str {
        self.cause.diagnostic_id()
    }

    /// Returns the exact reference attached to the document-layer cause, when present.
    pub const fn reference(&self) -> Option<ObjectRef> {
        self.cause.reference()
    }

    /// Returns the absolute source offset attached to the cause, when present.
    pub const fn offset(&self) -> Option<u64> {
        self.cause.offset()
    }

    /// Returns structured deterministic limit context, when applicable.
    pub const fn limit(&self) -> Option<DocumentLimit> {
        self.cause.limit()
    }

    /// Returns the complete retained lower document error.
    pub const fn document_error(&self) -> DocumentError {
        self.cause
    }

    /// Returns the complete retained document-layer cause.
    pub const fn cause(&self) -> DocumentError {
        self.cause
    }

    /// Borrows the complete attempted chain, including a cycle-closing terminal identity.
    pub const fn chain(&self) -> &ReferenceChain {
        &self.chain
    }
}

impl fmt::Debug for ReferenceChainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReferenceChainError")
            .field("cause", &self.cause)
            .field("chain_len", &self.chain.len())
            .field("chain", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for ReferenceChainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.cause.fmt(formatter)
    }
}

impl Error for ReferenceChainError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.cause)
    }
}

/// Result of polling one bounded top-level direct-reference chain.
#[allow(
    clippy::large_enum_variant,
    reason = "the move-only proof-bound Ready value stays inline without an untracked allocation"
)]
pub enum ReferenceChainPoll<'job> {
    /// A non-reference direct value or stream terminated the chain.
    Ready(ResolvedReference),
    /// The current proof-preserving child object job requires exact source ranges.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the active child request.
        missing: SmallRanges,
        /// Child envelope or stream-boundary checkpoint to retain while waiting.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a stable terminal failure owned by the job.
    Failed(&'job ReferenceChainError),
}

impl fmt::Debug for ReferenceChainPoll<'_> {
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
enum ChainState {
    Unresolved,
    Resolving,
    Ready,
    Failed,
}

struct ChildState {
    job: OpenAttestedObjectJob,
    accounted_stats: ObjectStats,
    work_caps: ObjectWorkCaps,
    reference: ObjectRef,
    offset: u64,
}

/// One-shot job that follows only whole-object direct indirect-reference aliases.
pub struct ResolveReferenceChainJob<'index> {
    index: &'index AttestedRevisionIndex,
    root: ObjectRef,
    current: ObjectRef,
    prefix: Vec<ObjectRef>,
    context: ReferenceChainJobContext,
    limits: ReferenceChainLimits,
    stats: ReferenceChainStats,
    child: Option<ChildState>,
    state: ChainState,
    terminal_error: ReferenceChainError,
}

impl ResolveReferenceChainJob<'_> {
    /// Returns the immutable source snapshot covered by the owning attested index.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.index.snapshot()
    }

    /// Returns the originally requested exact object identity.
    pub const fn root(&self) -> ObjectRef {
        self.root
    }

    /// Returns the exact identity currently being opened or classified.
    pub const fn current_reference(&self) -> ObjectRef {
        self.current
    }

    /// Returns runtime identity, child checkpoints, and scheduling priority.
    pub const fn context(&self) -> ReferenceChainJobContext {
        self.context
    }

    /// Returns the validated per-job aggregate limits.
    pub const fn limits(&self) -> ReferenceChainLimits {
        self.limits
    }

    /// Returns deterministic work and retained-path accounting through the latest poll.
    pub const fn stats(&self) -> ReferenceChainStats {
        self.stats
    }

    /// Returns the public four-state phase.
    pub const fn phase(&self) -> ReferenceChainPhase {
        match self.state {
            ChainState::Unresolved => ReferenceChainPhase::Unresolved,
            ChainState::Resolving => ReferenceChainPhase::Resolving,
            ChainState::Ready => ReferenceChainPhase::Ready,
            ChainState::Failed => ReferenceChainPhase::Failed,
        }
    }

    /// Advances the chain without performing file, network, callback, or async-runtime I/O.
    pub fn poll<'job>(
        &'job mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> ReferenceChainPoll<'job> {
        match self.state {
            ChainState::Ready | ChainState::Failed => self.poll_terminal(),
            ChainState::Unresolved | ChainState::Resolving => {
                self.poll_active(source, cancellation)
            }
        }
    }

    fn poll_active<'job>(
        &'job mut self,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> ReferenceChainPoll<'job> {
        loop {
            if source.snapshot() != self.index.snapshot() {
                self.fail_current(DocumentError::for_code(
                    DocumentErrorCode::SourceSnapshotMismatch,
                    Some(self.current),
                    self.current_offset(),
                ));
                return self.poll_terminal();
            }
            if cancellation.is_cancelled() {
                self.fail_current(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(self.current),
                    self.current_offset(),
                ));
                return self.poll_terminal();
            }

            if self.child.is_none() {
                match self.start_child() {
                    Ok(()) => {}
                    Err(error) => {
                        self.fail_current(error);
                        return self.poll_terminal();
                    }
                }
            }

            let Some(mut child) = self.child.take() else {
                self.fail_current(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(self.current),
                    self.current_offset(),
                ));
                return self.poll_terminal();
            };
            let outcome = child.job.poll(source, cancellation);
            if let Err(error) = self.account_child_stats(&mut child) {
                self.fail_current(error);
                return self.poll_terminal();
            }

            match outcome {
                AttestedObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    self.child = Some(child);
                    return ReferenceChainPoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    };
                }
                AttestedObjectPoll::Failed(error) => {
                    let mapped = self.map_child_error(error, &child);
                    self.fail_current(mapped);
                    return self.poll_terminal();
                }
                AttestedObjectPoll::Ready(object) => {
                    if source.snapshot() != self.index.snapshot() {
                        self.fail_current(DocumentError::for_code(
                            DocumentErrorCode::SourceSnapshotMismatch,
                            Some(self.current),
                            self.current_offset(),
                        ));
                        return self.poll_terminal();
                    }
                    if cancellation.is_cancelled() {
                        self.fail_current(DocumentError::for_code(
                            DocumentErrorCode::Cancelled,
                            Some(self.current),
                            self.current_offset(),
                        ));
                        return self.poll_terminal();
                    }

                    let next = match object.value() {
                        IndirectObjectValue::Direct(value) => match value.value() {
                            SyntaxObject::Reference(reference) => Some(*reference),
                            SyntaxObject::Null
                            | SyntaxObject::Boolean(_)
                            | SyntaxObject::Integer(_)
                            | SyntaxObject::Real(_)
                            | SyntaxObject::Name(_)
                            | SyntaxObject::String(_)
                            | SyntaxObject::Array(_)
                            | SyntaxObject::Dictionary(_) => None,
                        },
                        IndirectObjectValue::Stream(_) => None,
                    };
                    if let Some(next) = next {
                        match self.advance(next, cancellation) {
                            Ok(()) => {
                                self.state = ChainState::Resolving;
                            }
                            Err(error) => {
                                self.fail_attempt(error, next);
                                return self.poll_terminal();
                            }
                        }
                        continue;
                    }

                    let chain = ReferenceChain {
                        prefix: mem::take(&mut self.prefix),
                        terminal: self.current,
                    };
                    let stats = self.stats;
                    let repoll = ReferenceChainError::single(
                        DocumentError::for_code(
                            DocumentErrorCode::JobAlreadyComplete,
                            Some(self.root),
                            self.root_offset(),
                        ),
                        self.root,
                    );
                    self.terminal_error = repoll;
                    self.state = ChainState::Ready;
                    return ReferenceChainPoll::Ready(ResolvedReference {
                        chain,
                        object,
                        limits: self.limits,
                        stats,
                    });
                }
            }
        }
    }

    fn poll_terminal(&self) -> ReferenceChainPoll<'_> {
        ReferenceChainPoll::Failed(&self.terminal_error)
    }

    fn start_child(&mut self) -> Result<(), DocumentError> {
        let attestation = self.index.attestation(self.current)?;
        let offset = attestation.xref_offset();
        if self.stats.objects_started >= self.limits.max_objects() {
            return Err(DocumentError::reference_chain_resource(
                DocumentLimitKind::ReferenceChainObjects,
                self.limits.max_objects(),
                self.stats.objects_started,
                1,
                self.current,
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
                    Some(self.current),
                    Some(offset),
                )
            })?;
        if read_remaining == 0 {
            return Err(DocumentError::reference_chain_resource(
                DocumentLimitKind::ReferenceChainObjectReadBytes,
                self.limits.max_total_object_read_bytes(),
                self.stats.object_read_bytes,
                1,
                self.current,
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
                    Some(self.current),
                    Some(offset),
                )
            })?;
        if parse_remaining == 0 {
            return Err(DocumentError::reference_chain_resource(
                DocumentLimitKind::ReferenceChainObjectParseBytes,
                self.limits.max_total_object_parse_bytes(),
                self.stats.object_parse_bytes,
                1,
                self.current,
                Some(offset),
            ));
        }
        let work_caps = ObjectWorkCaps::new(
            read_remaining.min(self.index.object_limits().max_total_read_bytes()),
            parse_remaining.min(self.index.object_limits().max_total_parse_bytes()),
        )
        .map_err(|error| {
            DocumentError::from_object_access_constructor(error, self.current, offset)
        })?;
        let context = AttestedObjectJobContext::new(
            self.context.job,
            self.context.object_envelope_checkpoint,
            self.context.object_boundary_checkpoint,
            self.context.priority,
        );
        let job = self.index.open_object(self.current, context, work_caps)?;
        self.stats.objects_started =
            self.stats.objects_started.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(self.current),
                    Some(offset),
                )
            })?;
        let depth = u64::try_from(self.prefix.len())
            .ok()
            .and_then(|prefix| prefix.checked_add(1))
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(self.current),
                    Some(offset),
                )
            })?;
        self.stats.max_depth = self.stats.max_depth.max(depth);
        self.child = Some(ChildState {
            job,
            accounted_stats: ObjectStats::default(),
            work_caps,
            reference: self.current,
            offset,
        });
        self.state = ChainState::Resolving;
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
                        DocumentLimitKind::ReferenceChainObjectReadBytes,
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
                        DocumentLimitKind::ReferenceChainObjectParseBytes,
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

    fn advance(
        &mut self,
        next: ObjectRef,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        if self.stats.reference_edges >= self.limits.max_reference_edges() {
            return Err(DocumentError::reference_chain_resource(
                DocumentLimitKind::ReferenceChainEdges,
                self.limits.max_reference_edges(),
                self.stats.reference_edges,
                1,
                next,
                None,
            ));
        }
        self.stats.reference_edges =
            self.stats.reference_edges.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(next), None)
            })?;

        for (index, reference) in self
            .prefix
            .iter()
            .chain(std::iter::once(&self.current))
            .enumerate()
        {
            if index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(next),
                    None,
                ));
            }
            if *reference == next {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::ReferenceCycle,
                    Some(next),
                    None,
                ));
            }
        }

        let depth = u64::try_from(self.prefix.len())
            .ok()
            .and_then(|prefix| prefix.checked_add(1))
            .ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(next), None)
            })?;
        if depth >= self.limits.max_depth() {
            return Err(DocumentError::reference_chain_resource(
                DocumentLimitKind::ReferenceChainDepth,
                self.limits.max_depth(),
                depth,
                1,
                next,
                None,
            ));
        }
        if self.prefix.len() >= self.prefix.capacity() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(next),
                None,
            ));
        }
        self.prefix.push(self.current);
        self.current = next;
        self.stats.max_depth = self
            .stats
            .max_depth
            .max(depth.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(next), None)
            })?);
        Ok(())
    }

    fn current_offset(&self) -> Option<u64> {
        self.index
            .attestation(self.current)
            .ok()
            .map(crate::ObjectAttestation::xref_offset)
    }

    fn root_offset(&self) -> Option<u64> {
        self.index
            .attestation(self.root)
            .ok()
            .map(crate::ObjectAttestation::xref_offset)
    }

    fn fail_current(&mut self, cause: DocumentError) {
        let chain = ReferenceChain {
            prefix: mem::take(&mut self.prefix),
            terminal: self.current,
        };
        self.child = None;
        self.terminal_error = ReferenceChainError { cause, chain };
        self.state = ChainState::Failed;
    }

    fn fail_attempt(&mut self, cause: DocumentError, attempted: ObjectRef) {
        if self.prefix.len() < self.prefix.capacity() {
            self.prefix.push(self.current);
            let chain = ReferenceChain {
                prefix: mem::take(&mut self.prefix),
                terminal: attempted,
            };
            self.child = None;
            self.terminal_error = ReferenceChainError { cause, chain };
            self.state = ChainState::Failed;
        } else {
            self.fail_current(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.current),
                self.current_offset(),
            ));
        }
    }
}

impl fmt::Debug for ResolveReferenceChainJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolveReferenceChainJob")
            .field("snapshot", &self.index.snapshot())
            .field("root", &self.root)
            .field("current", &"[REDACTED]")
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .field("path", &"[REDACTED]")
            .field("child", &"[REDACTED]")
            .finish()
    }
}

impl AttestedRevisionIndex {
    /// Creates a bounded one-shot job that follows only top-level direct-reference aliases.
    pub fn resolve_reference_chain(
        &self,
        root: ObjectRef,
        context: ReferenceChainJobContext,
        limits: ReferenceChainLimits,
    ) -> Result<ResolveReferenceChainJob<'_>, ReferenceChainError> {
        let attestation = self
            .attestation(root)
            .map_err(|error| ReferenceChainError::single(error, root))?;
        if context.object_envelope_checkpoint == context.object_boundary_checkpoint {
            return Err(ReferenceChainError::single(
                DocumentError::for_code(
                    DocumentErrorCode::InvalidReferenceChainJobContext,
                    Some(root),
                    Some(attestation.xref_offset()),
                ),
                root,
            ));
        }

        let path_references =
            usize::try_from(limits.effective_path_references()).map_err(|_| {
                ReferenceChainError::single(
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(root),
                        Some(attestation.xref_offset()),
                    ),
                    root,
                )
            })?;
        let requested_path_bytes = limits
            .effective_path_references()
            .checked_mul(u64::try_from(mem::size_of::<ObjectRef>()).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                ReferenceChainError::single(
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(root),
                        Some(attestation.xref_offset()),
                    ),
                    root,
                )
            })?;
        let mut prefix = Vec::new();
        prefix.try_reserve_exact(path_references).map_err(|_| {
            ReferenceChainError::single(
                DocumentError::reference_chain_resource(
                    DocumentLimitKind::ReferenceChainPathBytes,
                    limits.max_retained_path_bytes(),
                    0,
                    requested_path_bytes,
                    root,
                    Some(attestation.xref_offset()),
                ),
                root,
            )
        })?;
        let retained_path_bytes = u64::try_from(prefix.capacity())
            .ok()
            .and_then(|capacity| {
                capacity.checked_mul(u64::try_from(mem::size_of::<ObjectRef>()).ok()?)
            })
            .ok_or_else(|| {
                ReferenceChainError::single(
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(root),
                        Some(attestation.xref_offset()),
                    ),
                    root,
                )
            })?;
        if retained_path_bytes > limits.max_retained_path_bytes() {
            return Err(ReferenceChainError::single(
                DocumentError::reference_chain_resource(
                    DocumentLimitKind::ReferenceChainPathBytes,
                    limits.max_retained_path_bytes(),
                    0,
                    retained_path_bytes,
                    root,
                    Some(attestation.xref_offset()),
                ),
                root,
            ));
        }

        let terminal_error = ReferenceChainError::single(
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                Some(attestation.xref_offset()),
            ),
            root,
        );
        Ok(ResolveReferenceChainJob {
            index: self,
            root,
            current: root,
            prefix,
            context,
            limits,
            stats: ReferenceChainStats {
                retained_path_bytes,
                ..ReferenceChainStats::default()
            },
            child: None,
            state: ChainState::Unresolved,
            terminal_error,
        })
    }
}
