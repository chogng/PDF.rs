use std::fmt;

use pdf_rs_bytes::{
    ByteSource, DataTicket, JobId, RequestPriority, ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_object::{
    IndirectObject, IndirectObjectTarget, IndirectObjectValue, ObjectCancellation,
    ObjectJobContext, ObjectLimits, ObjectPhase, ObjectPoll, ObjectStats, ObjectWorkCaps,
    OpenObjectJob,
};
use pdf_rs_syntax::{ByteSpan, Located, ObjectRef, PdfDictionary, SyntaxLimits, SyntaxObject};

use crate::{
    AttestedRevisionIndex, DocumentCancellation, DocumentError, DocumentErrorCode,
    DocumentResidentFootprint, ObjectAttestation, ObjectAttestationKind, PhysicalObjectInterval,
    RevisionId,
};

/// Runtime identity, phase checkpoints, and scheduling priority for attested object access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttestedObjectJobContext {
    job: JobId,
    envelope_checkpoint: ResumeCheckpoint,
    boundary_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl AttestedObjectJobContext {
    /// Creates a context whose envelope and stream-boundary checkpoints remain runtime-owned.
    pub const fn new(
        job: JobId,
        envelope_checkpoint: ResumeCheckpoint,
        boundary_checkpoint: ResumeCheckpoint,
        priority: RequestPriority,
    ) -> Self {
        Self {
            job,
            envelope_checkpoint,
            boundary_checkpoint,
            priority,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the checkpoint used for object-envelope reads.
    pub const fn envelope_checkpoint(self) -> ResumeCheckpoint {
        self.envelope_checkpoint
    }

    /// Returns the checkpoint used at a declared stream payload end.
    pub const fn boundary_checkpoint(self) -> ResumeCheckpoint {
        self.boundary_checkpoint
    }

    /// Returns the scheduling priority copied to every exact byte request.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }
}

/// Coarse resumable phase of one proof-preserving attested object access job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttestedObjectPhase {
    /// Reading and parsing the previously attested object envelope.
    Envelope,
    /// Validating framing at the exact declared stream payload end.
    StreamBoundary,
    /// The proof-bound object was returned and the one-shot job is complete.
    Complete,
    /// The job reached a terminal structured failure.
    Failed,
}

/// Result of polling one proof-preserving attested object access job.
#[allow(
    clippy::large_enum_variant,
    reason = "the proof-bound object stays inline without an untracked allocation"
)]
pub enum AttestedObjectPoll {
    /// The exact attested object was reopened and its retained evidence reproduced.
    Ready(AttestedObject),
    /// Required bytes are absent and the runtime must wait for the returned ticket.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the active request.
        missing: SmallRanges,
        /// Envelope or stream-boundary checkpoint to retain when requeueing the job.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a terminal structured failure.
    Failed(DocumentError),
}

impl fmt::Debug for AttestedObjectPoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(object) => formatter.debug_tuple("Ready").field(object).finish(),
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

/// Parsed indirect object owned beside the reproduced top-level evidence that authorized access.
///
/// The wrapper has no consuming API that returns the lower object or an owned
/// [`IndirectObjectValue`]. Lower semantic value types retain their own public borrow and clone
/// behavior, which does not confer raw-target or resolver authority.
pub struct AttestedObject {
    attestation: ObjectAttestation,
    object: IndirectObject,
    object_limits: ObjectLimits,
    syntax_limits: SyntaxLimits,
}

impl AttestedObject {
    /// Returns the immutable source snapshot shared by the object and its owning revision proof.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.object.snapshot()
    }

    /// Returns the `startxref` anchor of the attested revision.
    pub const fn revision_startxref(&self) -> u64 {
        self.object.revision_startxref()
    }

    /// Returns the caller-assigned identity of the attested revision.
    pub const fn revision_id(&self) -> RevisionId {
        self.attestation.revision_id()
    }

    /// Returns the exact object number and generation reproduced by reopening.
    pub const fn reference(&self) -> ObjectRef {
        self.attestation.reference()
    }

    /// Returns the validated indirect-object profile that produced this value.
    pub const fn object_limits(&self) -> ObjectLimits {
        self.object_limits
    }

    /// Returns the validated direct-syntax profile that produced this value.
    pub const fn syntax_limits(&self) -> SyntaxLimits {
        self.syntax_limits
    }

    /// Borrows the fixed-size top-level framing evidence reproduced by this object.
    pub const fn attestation(&self) -> &ObjectAttestation {
        &self.attestation
    }

    /// Returns the exact number, generation, and `obj` header span.
    pub const fn header_span(&self) -> ByteSpan {
        self.attestation.header_span()
    }

    /// Returns the exact source span from the object header through `endobj`.
    pub const fn object_span(&self) -> ByteSpan {
        self.attestation.object_span()
    }

    /// Returns the exact terminal `endobj` keyword span.
    pub const fn endobj_span(&self) -> ByteSpan {
        self.attestation.endobj_span()
    }

    /// Returns allocator-reported syntax heap capacity retained by the parsed value.
    pub const fn syntax_heap_bytes(&self) -> u64 {
        self.object.retained_heap_bytes()
    }

    /// Computes the checked value-owned footprint suitable for future cache admission.
    ///
    /// This does not reserve cache space or account for allocator metadata, source
    /// storage, byte caches, stream payloads, or an outer cache container.
    pub fn try_resident_footprint(&self) -> Result<DocumentResidentFootprint, DocumentError> {
        DocumentResidentFootprint::for_value::<Self>(
            self.syntax_heap_bytes(),
            0,
            self.reference(),
            Some(self.attestation.xref_offset()),
        )
    }

    /// Borrows the parsed direct value or strictly framed stream retained beside its proof.
    pub const fn value(&self) -> &IndirectObjectValue {
        self.object.value()
    }

    /// Borrows the source-located direct value, or returns `None` for a framed stream.
    pub const fn direct_value(&self) -> Option<&Located<SyntaxObject>> {
        match self.object.value() {
            IndirectObjectValue::Direct(value) => Some(value),
            IndirectObjectValue::Stream(_) => None,
        }
    }

    /// Borrows the source-located dictionary of a strictly framed stream object.
    pub const fn stream_dictionary(&self) -> Option<&Located<PdfDictionary>> {
        match self.object.value() {
            IndirectObjectValue::Direct(_) => None,
            IndirectObjectValue::Stream(stream) => Some(stream.dictionary()),
        }
    }
}

impl fmt::Debug for AttestedObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttestedObject")
            .field("snapshot", &self.object.snapshot())
            .field("revision_startxref", &self.object.revision_startxref())
            .field("attestation", &self.attestation)
            .field("object_limits", &self.object_limits)
            .field("syntax_limits", &self.syntax_limits)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

enum AccessJobState {
    Active,
    Complete,
    Failed(DocumentError),
}

struct CancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl ObjectCancellation for CancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// One-shot job privately minted by an attested revision to reopen one exact proven object.
pub struct OpenAttestedObjectJob {
    snapshot: SourceSnapshot,
    revision_id: RevisionId,
    revision_startxref: u64,
    attestation: ObjectAttestation,
    context: AttestedObjectJobContext,
    object_limits: ObjectLimits,
    syntax_limits: SyntaxLimits,
    work_caps: ObjectWorkCaps,
    child: OpenObjectJob,
    state: AccessJobState,
}

impl OpenAttestedObjectJob {
    /// Returns the immutable source snapshot bound by the owning attested revision.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the caller-assigned identity of the owning attested revision.
    pub const fn revision_id(&self) -> RevisionId {
        self.revision_id
    }

    /// Returns the exact object number and generation selected from the attested index.
    pub const fn reference(&self) -> ObjectRef {
        self.attestation.reference()
    }

    /// Returns runtime identity, phase checkpoints, and scheduling priority.
    pub const fn context(&self) -> AttestedObjectJobContext {
        self.context
    }

    /// Returns the original validated object-framing profile retained by the attested index.
    pub const fn object_limits(&self) -> ObjectLimits {
        self.object_limits
    }

    /// Returns the original validated syntax profile retained by the attested index.
    pub const fn syntax_limits(&self) -> SyntaxLimits {
        self.syntax_limits
    }

    /// Returns the explicit per-access work caps enforced by the private child job.
    pub const fn work_caps(&self) -> ObjectWorkCaps {
        self.work_caps
    }

    /// Returns cumulative exact-read and parse work through the most recent poll.
    pub const fn stats(&self) -> ObjectStats {
        self.child.stats()
    }

    /// Returns the current coarse resumable phase.
    pub const fn phase(&self) -> AttestedObjectPhase {
        match &self.state {
            AccessJobState::Complete => AttestedObjectPhase::Complete,
            AccessJobState::Failed(_) => AttestedObjectPhase::Failed,
            AccessJobState::Active => match self.child.phase() {
                ObjectPhase::Envelope => AttestedObjectPhase::Envelope,
                ObjectPhase::StreamBoundary => AttestedObjectPhase::StreamBoundary,
                ObjectPhase::Complete | ObjectPhase::Failed => AttestedObjectPhase::Failed,
            },
        }
    }

    /// Advances the job without performing file, network, callback, or async-runtime I/O.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> AttestedObjectPoll {
        match &self.state {
            AccessJobState::Failed(error) => return AttestedObjectPoll::Failed(*error),
            AccessJobState::Complete => {
                return AttestedObjectPoll::Failed(DocumentError::for_code(
                    DocumentErrorCode::JobAlreadyComplete,
                    Some(self.attestation.reference()),
                    Some(self.attestation.xref_offset()),
                ));
            }
            AccessJobState::Active => {}
        }

        let adapter = CancellationAdapter(cancellation);
        match self.child.poll(source, &adapter) {
            ObjectPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                let expected_checkpoint = match self.child.phase() {
                    ObjectPhase::Envelope => self.context.envelope_checkpoint,
                    ObjectPhase::StreamBoundary => self.context.boundary_checkpoint,
                    ObjectPhase::Complete | ObjectPhase::Failed => {
                        return self.fail(DocumentError::for_code(
                            DocumentErrorCode::AttestedObjectEvidenceMismatch,
                            Some(self.attestation.reference()),
                            Some(self.attestation.xref_offset()),
                        ));
                    }
                };
                if checkpoint != expected_checkpoint {
                    return self.fail(DocumentError::for_code(
                        DocumentErrorCode::AttestedObjectEvidenceMismatch,
                        Some(self.attestation.reference()),
                        Some(self.attestation.xref_offset()),
                    ));
                }
                AttestedObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                }
            }
            ObjectPoll::Failed(error) => self.fail(DocumentError::from_object_access_poll(
                error,
                self.attestation.reference(),
                self.attestation.xref_offset(),
            )),
            ObjectPoll::Ready(object) => self.finish_ready(source, cancellation, object),
        }
    }

    fn finish_ready(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
        object: IndirectObject,
    ) -> AttestedObjectPoll {
        if source.snapshot() != self.snapshot {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(self.attestation.reference()),
                Some(self.attestation.xref_offset()),
            ));
        }
        if cancellation.is_cancelled() {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                Some(self.attestation.reference()),
                Some(self.attestation.xref_offset()),
            ));
        }

        let observed = ObjectAttestation::from_object(self.revision_id, &object);
        if object.snapshot() != self.snapshot
            || object.revision_startxref() != self.revision_startxref
            || self.child.stats().retained_heap_bytes() != object.retained_heap_bytes()
            || observed != self.attestation
        {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::AttestedObjectEvidenceMismatch,
                Some(self.attestation.reference()),
                Some(self.attestation.xref_offset()),
            ));
        }

        let attestation = self.attestation.duplicate();
        self.state = AccessJobState::Complete;
        AttestedObjectPoll::Ready(AttestedObject {
            attestation,
            object,
            object_limits: self.object_limits,
            syntax_limits: self.syntax_limits,
        })
    }

    fn fail(&mut self, error: DocumentError) -> AttestedObjectPoll {
        self.state = AccessJobState::Failed(error);
        AttestedObjectPoll::Failed(error)
    }
}

impl fmt::Debug for OpenAttestedObjectJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAttestedObjectJob")
            .field("snapshot", &self.snapshot)
            .field("revision_id", &self.revision_id)
            .field("revision_startxref", &self.revision_startxref)
            .field("reference", &self.attestation.reference())
            .field("context", &self.context)
            .field("object_limits", &self.object_limits)
            .field("syntax_limits", &self.syntax_limits)
            .field("work_caps", &self.work_caps)
            .field("stats", &self.stats())
            .field("phase", &self.phase())
            .field("attestation", &"[REDACTED]")
            .field("child", &"[REDACTED]")
            .finish()
    }
}

impl AttestedRevisionIndex {
    /// Reopens an attested value under the revision's complete validated object work ceilings.
    ///
    /// Higher semantic layers use this when they need proof-preserving lazy resource access but do
    /// not own a narrower object work policy.
    pub fn open_object_with_attested_work_caps(
        &self,
        reference: ObjectRef,
        context: AttestedObjectJobContext,
    ) -> Result<OpenAttestedObjectJob, DocumentError> {
        let work_caps = ObjectWorkCaps::new(
            self.object_limits.max_total_read_bytes(),
            self.object_limits.max_total_parse_bytes(),
        )
        .map_err(|_| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), None)
        })?;
        self.open_object(reference, context, work_caps)
    }

    /// Creates the only public job capable of reopening a parsed value from retained attestation.
    pub fn open_object(
        &self,
        reference: ObjectRef,
        context: AttestedObjectJobContext,
        work_caps: ObjectWorkCaps,
    ) -> Result<OpenAttestedObjectJob, DocumentError> {
        let attestation = self.attestation(reference)?;
        let interval = self.candidate.interval(reference)?;
        if !evidence_matches_interval(
            self.snapshot().len(),
            self.revision_id(),
            reference,
            self.startxref(),
            interval,
            attestation,
        ) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::AttestedObjectEvidenceMismatch,
                Some(reference),
                Some(attestation.xref_offset()),
            ));
        }
        if context.envelope_checkpoint == context.boundary_checkpoint {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidObjectAccessJobContext,
                Some(reference),
                None,
            ));
        }
        let target = IndirectObjectTarget::new(
            self.snapshot(),
            reference,
            interval.xref_offset(),
            interval.object_upper_bound(),
            self.startxref(),
        )
        .map_err(|error| {
            DocumentError::from_object_access_constructor(
                error,
                reference,
                attestation.xref_offset(),
            )
        })?;
        let child_context = ObjectJobContext::new(
            context.job,
            context.envelope_checkpoint,
            context.boundary_checkpoint,
            context.priority,
        );
        let child = OpenObjectJob::new_with_work_caps(
            target,
            child_context,
            self.object_limits,
            self.syntax_limits,
            work_caps,
        )
        .map_err(|error| {
            DocumentError::from_object_access_constructor(
                error,
                reference,
                attestation.xref_offset(),
            )
        })?;

        Ok(OpenAttestedObjectJob {
            snapshot: self.snapshot(),
            revision_id: self.revision_id(),
            revision_startxref: self.startxref(),
            attestation: attestation.duplicate(),
            context,
            object_limits: self.object_limits,
            syntax_limits: self.syntax_limits,
            work_caps,
            child,
            state: AccessJobState::Active,
        })
    }
}

fn evidence_matches_interval(
    source_len: Option<u64>,
    revision_id: RevisionId,
    reference: ObjectRef,
    startxref: u64,
    interval: &PhysicalObjectInterval,
    attestation: &ObjectAttestation,
) -> bool {
    let Some(source_len) = source_len else {
        return false;
    };
    if interval.revision_id() != revision_id
        || interval.reference() != reference
        || attestation.revision_id() != revision_id
        || attestation.reference() != reference
        || interval.xref_offset() != attestation.xref_offset()
        || interval.object_upper_bound() != attestation.object_upper_bound()
        || interval.xref_offset() >= interval.object_upper_bound()
        || interval.object_upper_bound() > startxref
        || startxref >= source_len
    {
        return false;
    }

    let header = attestation.header_span();
    let object = attestation.object_span();
    let endobj = attestation.endobj_span();
    if header.is_empty()
        || object.is_empty()
        || endobj.is_empty()
        || header.start() != interval.xref_offset()
        || object.start() != interval.xref_offset()
        || header.end_exclusive() > endobj.start()
        || header.end_exclusive() > object.end_exclusive()
        || endobj.start() < object.start()
        || endobj.end_exclusive() != object.end_exclusive()
        || object.end_exclusive() > interval.object_upper_bound()
    {
        return false;
    }

    match attestation.kind() {
        ObjectAttestationKind::Stream {
            data_span,
            endstream_span,
        } => {
            !endstream_span.is_empty()
                && data_span.start() >= header.end_exclusive()
                && data_span.end_exclusive() <= endstream_span.start()
                && endstream_span.end_exclusive() <= endobj.start()
        }
        ObjectAttestationKind::Null
        | ObjectAttestationKind::Boolean
        | ObjectAttestationKind::Integer
        | ObjectAttestationKind::Real
        | ObjectAttestationKind::Name
        | ObjectAttestationKind::String
        | ObjectAttestationKind::Array
        | ObjectAttestationKind::Dictionary
        | ObjectAttestationKind::Reference => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: u64, len: u64) -> ByteSpan {
        ByteSpan::new(start, len).unwrap()
    }

    fn reference(number: u32) -> ObjectRef {
        ObjectRef::new(number, 0).unwrap()
    }

    fn interval() -> PhysicalObjectInterval {
        PhysicalObjectInterval {
            revision_id: RevisionId::new(7),
            reference: reference(1),
            xref_offset: 9,
            object_upper_bound: 80,
            logical_slot: 1,
        }
    }

    fn direct_attestation() -> ObjectAttestation {
        ObjectAttestation {
            revision_id: RevisionId::new(7),
            reference: reference(1),
            xref_offset: 9,
            object_upper_bound: 80,
            header_span: span(9, 7),
            object_span: span(9, 51),
            endobj_span: span(54, 6),
            kind: ObjectAttestationKind::Dictionary,
        }
    }

    fn matches(interval: &PhysicalObjectInterval, attestation: &ObjectAttestation) -> bool {
        evidence_matches_interval(
            Some(200),
            RevisionId::new(7),
            reference(1),
            100,
            interval,
            attestation,
        )
    }

    #[test]
    fn authority_gate_rejects_each_index_and_evidence_field_mismatch() {
        let interval = interval();
        let attestation = direct_attestation();
        assert!(matches(&interval, &attestation));

        assert!(!evidence_matches_interval(
            None,
            RevisionId::new(7),
            reference(1),
            100,
            &interval,
            &attestation,
        ));
        assert!(!evidence_matches_interval(
            Some(100),
            RevisionId::new(7),
            reference(1),
            100,
            &interval,
            &attestation,
        ));

        let mut changed_interval = interval;
        changed_interval.revision_id = RevisionId::new(8);
        assert!(!matches(&changed_interval, &attestation));
        let mut changed_interval = interval;
        changed_interval.reference = reference(2);
        assert!(!matches(&changed_interval, &attestation));
        let mut changed_interval = interval;
        changed_interval.xref_offset = 10;
        assert!(!matches(&changed_interval, &attestation));
        let mut changed_interval = interval;
        changed_interval.object_upper_bound = 81;
        assert!(!matches(&changed_interval, &attestation));

        let mut changed = attestation.duplicate();
        changed.revision_id = RevisionId::new(8);
        assert!(!matches(&interval, &changed));
        let mut changed = attestation.duplicate();
        changed.reference = reference(2);
        assert!(!matches(&interval, &changed));
        let mut changed = attestation.duplicate();
        changed.xref_offset = 10;
        assert!(!matches(&interval, &changed));
        let mut changed = attestation.duplicate();
        changed.object_upper_bound = 81;
        assert!(!matches(&interval, &changed));
    }

    #[test]
    fn authority_gate_rejects_invalid_object_and_stream_span_geometry() {
        let interval = interval();
        let attestation = direct_attestation();

        for changed in [
            ObjectAttestation {
                header_span: span(10, 6),
                ..attestation.duplicate()
            },
            ObjectAttestation {
                object_span: span(10, 50),
                ..attestation.duplicate()
            },
            ObjectAttestation {
                header_span: span(9, 46),
                ..attestation.duplicate()
            },
            ObjectAttestation {
                endobj_span: span(53, 6),
                ..attestation.duplicate()
            },
            ObjectAttestation {
                object_span: span(9, 72),
                endobj_span: span(75, 6),
                ..attestation.duplicate()
            },
        ] {
            assert!(!matches(&interval, &changed));
        }

        let valid_stream = ObjectAttestation {
            kind: ObjectAttestationKind::Stream {
                data_span: span(20, 20),
                endstream_span: span(41, 9),
            },
            ..attestation.duplicate()
        };
        assert!(matches(&interval, &valid_stream));

        for kind in [
            ObjectAttestationKind::Stream {
                data_span: span(15, 25),
                endstream_span: span(41, 9),
            },
            ObjectAttestationKind::Stream {
                data_span: span(20, 22),
                endstream_span: span(41, 9),
            },
            ObjectAttestationKind::Stream {
                data_span: span(20, 20),
                endstream_span: span(50, 9),
            },
            ObjectAttestationKind::Stream {
                data_span: span(20, 20),
                endstream_span: span(41, 0),
            },
        ] {
            let changed = ObjectAttestation {
                kind,
                ..attestation.duplicate()
            };
            assert!(!matches(&interval, &changed));
        }
    }
}
