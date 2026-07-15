use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_object::{
    ObjectCancellation, ObjectError, ObjectErrorCode, ObjectJobContext, ObjectLimitKind,
    ObjectLimits, ObjectPoll, ObjectStats, ObjectWorkCaps, OpenObjectJob,
};
use pdf_rs_syntax::{
    InputExtent, Located, PdfHeader, SyntaxCancellation, SyntaxInput, SyntaxLimits, SyntaxParser,
    SyntaxPoll,
};

use crate::{
    AttestedRevisionIndex, CandidateRevisionIndex, DocumentCancellation, DocumentError,
    DocumentErrorCode, DocumentLimitKind, ObjectAttestation, RevisionAttestationLimits,
};

const CANCELLATION_INTERVAL: usize = 256;
const ACCOUNTED_OBJECT_ATTESTATION_BYTES: u64 = 192;

/// Runtime identity, resume checkpoints, and scheduling priority for revision attestation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionAttestationJobContext {
    job: JobId,
    scan_checkpoint: ResumeCheckpoint,
    object_envelope_checkpoint: ResumeCheckpoint,
    object_boundary_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl RevisionAttestationJobContext {
    /// Creates a context whose three checkpoints remain owned by the calling runtime.
    pub const fn new(
        job: JobId,
        scan_checkpoint: ResumeCheckpoint,
        object_envelope_checkpoint: ResumeCheckpoint,
        object_boundary_checkpoint: ResumeCheckpoint,
        priority: RequestPriority,
    ) -> Self {
        Self {
            job,
            scan_checkpoint,
            object_envelope_checkpoint,
            object_boundary_checkpoint,
            priority,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the checkpoint used for prefix and inter-object trivia reads.
    pub const fn scan_checkpoint(self) -> ResumeCheckpoint {
        self.scan_checkpoint
    }

    /// Returns the checkpoint used for child object-envelope reads.
    pub const fn object_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.object_envelope_checkpoint
    }

    /// Returns the checkpoint used for child stream-boundary reads.
    pub const fn object_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.object_boundary_checkpoint
    }

    /// Returns the priority copied to every exact byte request.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }
}

/// Cumulative deterministic work retained by one revision-attestation job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RevisionAttestationStats {
    objects_attested: u64,
    trivia_read_bytes: u64,
    trivia_scan_bytes: u64,
    object_read_bytes: u64,
    object_parse_bytes: u64,
    retained_evidence_bytes: u64,
}

impl RevisionAttestationStats {
    /// Returns physical in-use objects completely framed so far.
    pub const fn objects_attested(self) -> u64 {
        self.objects_attested
    }

    /// Returns prefix and gap bytes charged on first exact requests, including the header read.
    pub const fn trivia_read_bytes(self) -> u64 {
        self.trivia_read_bytes
    }

    /// Returns prefix and gap bytes actually classified by the trivia scanner.
    pub const fn trivia_scan_bytes(self) -> u64 {
        self.trivia_scan_bytes
    }

    /// Returns cumulative exact-read work charged by child object jobs.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }

    /// Returns cumulative parser-window work charged by child object jobs.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }

    /// Returns conservatively accounted allocator capacity retained for fixed-size evidence.
    pub const fn retained_evidence_bytes(self) -> u64 {
        self.retained_evidence_bytes
    }
}

/// Coarse resumable phase of one strict revision-attestation job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionAttestationPhase {
    /// Validating the source header and scanning trivia before the first object.
    Prefix,
    /// Framing one in-use object at its exact physical xref offset.
    Object,
    /// Scanning trivia from one framed object through the next physical boundary.
    Gap,
    /// The attested index was returned and the one-shot job is complete.
    Complete,
    /// The job reached a terminal structured failure.
    Failed,
}

/// Result of polling one resumable strict revision-attestation job.
#[allow(
    clippy::large_enum_variant,
    reason = "the one-shot attested index stays inline without an untracked allocation"
)]
pub enum RevisionAttestationPoll {
    /// Every in-use object and every surrounding top-level trivia span was authenticated.
    Ready(AttestedRevisionIndex),
    /// Required bytes are absent and the runtime must wait for the returned ticket.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the active request.
        missing: SmallRanges,
        /// Scanner or child-object checkpoint to retain when requeueing the job.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a terminal structured failure.
    Failed(DocumentError),
}

impl fmt::Debug for RevisionAttestationPoll {
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
enum ScanPurpose {
    Prefix,
    Gap { after_object: usize },
}

#[derive(Clone, Copy)]
enum TriviaState {
    Neutral,
    Comment { start: u64, bytes: u64 },
}

#[derive(Clone, Copy)]
struct ScanState {
    purpose: ScanPurpose,
    cursor: u64,
    end: u64,
    charged: bool,
    trivia: TriviaState,
}

struct ObjectState {
    physical_index: usize,
    child: OpenObjectJob,
    accounted_stats: ObjectStats,
}

#[allow(
    clippy::large_enum_variant,
    reason = "the active child job remains inline so phase transitions require no untracked allocation"
)]
enum JobState {
    Scan(ScanState),
    Object(ObjectState),
    Transition,
    Complete,
    Failed(DocumentError),
}

struct CancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl ObjectCancellation for CancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl SyntaxCancellation for CancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// One-shot job that consumes a candidate index and atomically publishes top-level attestation.
pub struct AttestRevisionJob {
    snapshot: SourceSnapshot,
    candidate: Option<CandidateRevisionIndex>,
    context: RevisionAttestationJobContext,
    limits: RevisionAttestationLimits,
    object_limits: ObjectLimits,
    syntax_limits: SyntaxLimits,
    stats: RevisionAttestationStats,
    header: Option<Located<PdfHeader>>,
    evidence: Vec<ObjectAttestation>,
    state: JobState,
}

impl AttestRevisionJob {
    /// Validates configuration and consumes one unauthenticated physical candidate index.
    pub fn new(
        candidate: CandidateRevisionIndex,
        context: RevisionAttestationJobContext,
        limits: RevisionAttestationLimits,
        object_limits: ObjectLimits,
        syntax_limits: SyntaxLimits,
    ) -> Result<Self, DocumentError> {
        if context.scan_checkpoint == context.object_envelope_checkpoint
            || context.scan_checkpoint == context.object_boundary_checkpoint
            || context.object_envelope_checkpoint == context.object_boundary_checkpoint
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidAttestationJobContext,
                None,
                None,
            ));
        }
        let snapshot = candidate.snapshot();
        let source_len = snapshot
            .len()
            .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))?;
        if source_len > limits.max_source_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::AttestationSourceBytes,
                limits.max_source_bytes,
                0,
                source_len,
                None,
            ));
        }
        if source_len > object_limits.max_source_bytes() {
            return Err(DocumentError::resource(
                DocumentLimitKind::AttestationSourceBytes,
                object_limits.max_source_bytes(),
                0,
                source_len,
                None,
            ));
        }
        if !header_syntax_limits_are_valid(syntax_limits)
            || object_limits.max_envelope_bytes() > syntax_limits.max_input_bytes()
            || object_limits.max_boundary_bytes() > syntax_limits.max_input_bytes()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        let object_count = u64::try_from(candidate.physical_intervals.len()).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::AttestationObjects,
                limits.max_objects,
                0,
                u64::MAX,
                None,
            )
        })?;
        if object_count == 0 {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(candidate.root()),
                None,
            ));
        }
        if object_count > limits.max_objects {
            return Err(DocumentError::resource(
                DocumentLimitKind::AttestationObjects,
                limits.max_objects,
                0,
                object_count,
                None,
            ));
        }
        let first_offset = candidate.physical_intervals[0].xref_offset;
        if first_offset < 9 {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidDocumentHeader,
                Some(candidate.physical_intervals[0].reference),
                Some(first_offset),
            ));
        }
        if mem::size_of::<ObjectAttestation>() > ACCOUNTED_OBJECT_ATTESTATION_BYTES as usize {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                None,
                None,
            ));
        }
        let requested_evidence_bytes = object_count
            .checked_mul(ACCOUNTED_OBJECT_ATTESTATION_BYTES)
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::AttestationEvidenceBytes,
                    limits.max_retained_evidence_bytes,
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        if requested_evidence_bytes > limits.max_retained_evidence_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::AttestationEvidenceBytes,
                limits.max_retained_evidence_bytes,
                0,
                requested_evidence_bytes,
                None,
            ));
        }
        let evidence_capacity = usize::try_from(object_count).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::Allocation,
                limits.max_retained_evidence_bytes,
                0,
                requested_evidence_bytes,
                None,
            )
        })?;
        let mut evidence = Vec::new();
        evidence.try_reserve_exact(evidence_capacity).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::Allocation,
                limits.max_retained_evidence_bytes,
                0,
                requested_evidence_bytes,
                None,
            )
        })?;
        let retained_evidence_bytes = u64::try_from(evidence.capacity())
            .ok()
            .and_then(|capacity| capacity.checked_mul(ACCOUNTED_OBJECT_ATTESTATION_BYTES))
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::AttestationEvidenceBytes,
                    limits.max_retained_evidence_bytes,
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        if retained_evidence_bytes > limits.max_retained_evidence_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::AttestationEvidenceBytes,
                limits.max_retained_evidence_bytes,
                0,
                retained_evidence_bytes,
                None,
            ));
        }

        Ok(Self {
            snapshot,
            candidate: Some(candidate),
            context,
            limits,
            object_limits,
            syntax_limits,
            stats: RevisionAttestationStats {
                retained_evidence_bytes,
                ..RevisionAttestationStats::default()
            },
            header: None,
            evidence,
            state: JobState::Scan(ScanState {
                purpose: ScanPurpose::Prefix,
                cursor: 0,
                end: first_offset,
                charged: false,
                trivia: TriviaState::Neutral,
            }),
        })
    }

    /// Returns the immutable source snapshot bound at job construction.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns runtime identity, checkpoints, and request priority.
    pub const fn context(&self) -> RevisionAttestationJobContext {
        self.context
    }

    /// Returns the validated deterministic attestation limits.
    pub const fn limits(&self) -> RevisionAttestationLimits {
        self.limits
    }

    /// Returns cumulative work through the most recent poll.
    pub const fn stats(&self) -> RevisionAttestationStats {
        self.stats
    }

    /// Returns the current coarse resumable phase.
    pub const fn phase(&self) -> RevisionAttestationPhase {
        match self.state {
            JobState::Scan(ScanState {
                purpose: ScanPurpose::Prefix,
                ..
            }) => RevisionAttestationPhase::Prefix,
            JobState::Scan(ScanState {
                purpose: ScanPurpose::Gap { .. },
                ..
            }) => RevisionAttestationPhase::Gap,
            JobState::Object(_) => RevisionAttestationPhase::Object,
            JobState::Complete => RevisionAttestationPhase::Complete,
            JobState::Failed(_) | JobState::Transition => RevisionAttestationPhase::Failed,
        }
    }

    /// Advances the job without performing file, network, callback, or async-runtime I/O.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> RevisionAttestationPoll {
        match self.state {
            JobState::Failed(error) => return RevisionAttestationPoll::Failed(error),
            JobState::Complete => {
                return RevisionAttestationPoll::Failed(DocumentError::for_code(
                    DocumentErrorCode::JobAlreadyComplete,
                    None,
                    None,
                ));
            }
            JobState::Transition => return self.fail_internal(None, None),
            JobState::Scan(_) | JobState::Object(_) => {}
        }

        loop {
            if source.snapshot() != self.snapshot {
                return self.fail(DocumentError::for_code(
                    DocumentErrorCode::SourceSnapshotMismatch,
                    None,
                    None,
                ));
            }
            if cancellation.is_cancelled() {
                return self.fail(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    None,
                    None,
                ));
            }

            let state = mem::replace(&mut self.state, JobState::Transition);
            let result = match state {
                JobState::Scan(scan) => self.poll_scan(source, cancellation, scan),
                JobState::Object(object) => self.poll_object(source, cancellation, object),
                JobState::Transition => Some(self.fail_internal(None, None)),
                JobState::Complete => {
                    self.state = JobState::Complete;
                    Some(RevisionAttestationPoll::Failed(DocumentError::for_code(
                        DocumentErrorCode::JobAlreadyComplete,
                        None,
                        None,
                    )))
                }
                JobState::Failed(error) => {
                    self.state = JobState::Failed(error);
                    Some(RevisionAttestationPoll::Failed(error))
                }
            };
            if let Some(result) = result {
                return result;
            }
        }
    }

    fn poll_scan(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
        mut scan: ScanState,
    ) -> Option<RevisionAttestationPoll> {
        if scan.cursor == scan.end {
            if let TriviaState::Comment { start, .. } = scan.trivia {
                return Some(self.fail(DocumentError::for_code(
                    DocumentErrorCode::UnterminatedTopLevelComment,
                    self.scan_boundary_reference(scan.purpose),
                    Some(start),
                )));
            }
            return self.complete_scan(scan.purpose, cancellation);
        }
        let remaining = match scan.end.checked_sub(scan.cursor) {
            Some(value) if value != 0 => value,
            _ => return Some(self.fail_internal(None, Some(scan.cursor))),
        };
        let len = remaining.min(self.limits.scan_chunk_bytes);
        let range = match ByteRange::new(scan.cursor, len) {
            Ok(value) if value.end_exclusive() <= scan.end => value,
            _ => return Some(self.fail_internal(None, Some(scan.cursor))),
        };
        if !scan.charged {
            let next = match self.stats.trivia_read_bytes.checked_add(len) {
                Some(value) => value,
                None => {
                    return Some(self.fail(DocumentError::resource(
                        DocumentLimitKind::AttestationTriviaBytes,
                        self.limits.max_trivia_bytes,
                        self.stats.trivia_read_bytes,
                        u64::MAX,
                        Some(scan.cursor),
                    )));
                }
            };
            if next > self.limits.max_trivia_bytes {
                return Some(self.fail(DocumentError::resource(
                    DocumentLimitKind::AttestationTriviaBytes,
                    self.limits.max_trivia_bytes,
                    self.stats.trivia_read_bytes,
                    len,
                    Some(scan.cursor),
                )));
            }
            self.stats.trivia_read_bytes = next;
            scan.charged = true;
        }
        let request = ReadRequest::new(
            range,
            self.context.priority,
            self.context.job,
            self.context.scan_checkpoint,
        );
        match source.poll(request) {
            ReadPoll::Pending { ticket, missing } => {
                self.state = JobState::Scan(scan);
                Some(RevisionAttestationPoll::Pending {
                    ticket,
                    missing,
                    checkpoint: self.context.scan_checkpoint,
                })
            }
            ReadPoll::EndOfFile => Some(self.fail(DocumentError::for_code(
                DocumentErrorCode::UnexpectedEndOfSource,
                self.scan_boundary_reference(scan.purpose),
                Some(scan.cursor),
            ))),
            ReadPoll::Failed(error) => {
                Some(self.fail(DocumentError::from_source(error, scan.cursor)))
            }
            ReadPoll::Ready(bytes) => {
                if let Err(error) = self.validate_slice(&bytes, range) {
                    return Some(self.fail(error));
                }
                if matches!(scan.purpose, ScanPurpose::Prefix)
                    && self.header.is_none()
                    && let Err(error) = self.validate_header(&bytes, cancellation)
                {
                    return Some(self.fail(error));
                }
                let (scan_bytes, scan_base) =
                    if matches!(scan.purpose, ScanPurpose::Prefix) && scan.cursor == 0 {
                        (&bytes.bytes()[8..], 8)
                    } else {
                        (bytes.bytes(), scan.cursor)
                    };
                if let Err(error) = self.scan_trivia(
                    scan_bytes,
                    scan_base,
                    &mut scan.trivia,
                    cancellation,
                    self.scan_boundary_reference(scan.purpose),
                ) {
                    return Some(self.fail(error));
                }
                scan.cursor = range.end_exclusive();
                scan.charged = false;
                self.state = JobState::Scan(scan);
                None
            }
        }
    }

    fn poll_object(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
        mut state: ObjectState,
    ) -> Option<RevisionAttestationPoll> {
        let adapter = CancellationAdapter(cancellation);
        let outcome = state.child.poll(source, &adapter);
        let current = state.child.stats();
        let read_delta = match current
            .read_bytes()
            .checked_sub(state.accounted_stats.read_bytes())
        {
            Some(value) => value,
            None => return Some(self.fail_internal(None, None)),
        };
        let parse_delta = match current
            .parse_bytes()
            .checked_sub(state.accounted_stats.parse_bytes())
        {
            Some(value) => value,
            None => return Some(self.fail_internal(None, None)),
        };
        self.stats.object_read_bytes = match self.stats.object_read_bytes.checked_add(read_delta) {
            Some(value) if value <= self.limits.max_total_object_read_bytes => value,
            _ => return Some(self.fail_internal(None, None)),
        };
        self.stats.object_parse_bytes = match self.stats.object_parse_bytes.checked_add(parse_delta)
        {
            Some(value) if value <= self.limits.max_total_object_parse_bytes => value,
            _ => return Some(self.fail_internal(None, None)),
        };
        state.accounted_stats = current;

        match outcome {
            ObjectPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                self.state = JobState::Object(state);
                Some(RevisionAttestationPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                })
            }
            ObjectPoll::Failed(error) => {
                let reference = state.child.target().reference();
                let offset = state.child.target().xref_offset();
                Some(self.fail(self.map_object_error(
                    error,
                    reference,
                    offset,
                    state.child.work_caps(),
                    state.child.limits(),
                )))
            }
            ObjectPoll::Ready(object) => {
                let interval = match self.candidate.as_ref().and_then(|candidate| {
                    candidate
                        .physical_intervals
                        .get(state.physical_index)
                        .copied()
                }) {
                    Some(value) => value,
                    None => return Some(self.fail_internal(None, None)),
                };
                if object.snapshot() != self.snapshot
                    || object.reference() != interval.reference
                    || object.xref_offset() != interval.xref_offset
                    || object.object_upper_bound() != interval.object_upper_bound
                    || object.revision_startxref()
                        != self
                            .candidate
                            .as_ref()
                            .map_or(u64::MAX, CandidateRevisionIndex::startxref)
                    || object.header_span().start() != interval.xref_offset
                    || object.object_span().start() != interval.xref_offset
                    || object.object_span().end_exclusive() > interval.object_upper_bound
                    || object.endobj_span().end_exclusive() != object.object_span().end_exclusive()
                    || self.evidence.len() != state.physical_index
                    || self.evidence.len() >= self.evidence.capacity()
                {
                    return Some(
                        self.fail_internal(Some(interval.reference), Some(interval.xref_offset)),
                    );
                }
                let object_end = object.object_span().end_exclusive();
                self.evidence.push(ObjectAttestation::from_object(
                    interval.revision_id,
                    &object,
                ));
                self.stats.objects_attested =
                    match self.stats.objects_attested.checked_add(1) {
                        Some(value) => value,
                        None => {
                            return Some(self.fail_internal(
                                Some(interval.reference),
                                Some(interval.xref_offset),
                            ));
                        }
                    };
                self.state = JobState::Scan(ScanState {
                    purpose: ScanPurpose::Gap {
                        after_object: state.physical_index,
                    },
                    cursor: object_end,
                    end: interval.object_upper_bound,
                    charged: false,
                    trivia: TriviaState::Neutral,
                });
                None
            }
        }
    }

    fn complete_scan(
        &mut self,
        purpose: ScanPurpose,
        cancellation: &dyn DocumentCancellation,
    ) -> Option<RevisionAttestationPoll> {
        match purpose {
            ScanPurpose::Prefix => match self.start_object(0) {
                Ok(()) => None,
                Err(error) => Some(self.fail(error)),
            },
            ScanPurpose::Gap { after_object } => {
                let count = match self
                    .candidate
                    .as_ref()
                    .map(|candidate| candidate.physical_intervals.len())
                {
                    Some(value) => value,
                    None => return Some(self.fail_internal(None, None)),
                };
                let next = match after_object.checked_add(1) {
                    Some(value) => value,
                    None => return Some(self.fail_internal(None, None)),
                };
                if next < count {
                    match self.start_object(next) {
                        Ok(()) => None,
                        Err(error) => Some(self.fail(error)),
                    }
                } else if next == count {
                    Some(self.publish(cancellation))
                } else {
                    Some(self.fail_internal(None, None))
                }
            }
        }
    }

    fn start_object(&mut self, physical_index: usize) -> Result<(), DocumentError> {
        let interval = self
            .candidate
            .as_ref()
            .and_then(|candidate| candidate.physical_intervals.get(physical_index).copied())
            .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))?;
        let read_remaining = self
            .limits
            .max_total_object_read_bytes
            .checked_sub(self.stats.object_read_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(interval.reference),
                    Some(interval.xref_offset),
                )
            })?;
        if read_remaining == 0 {
            return Err(DocumentError::resource(
                DocumentLimitKind::AttestationObjectReadBytes,
                self.limits.max_total_object_read_bytes,
                self.stats.object_read_bytes,
                1,
                Some(interval.xref_offset),
            ));
        }
        let parse_remaining = self
            .limits
            .max_total_object_parse_bytes
            .checked_sub(self.stats.object_parse_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(interval.reference),
                    Some(interval.xref_offset),
                )
            })?;
        if parse_remaining == 0 {
            return Err(DocumentError::resource(
                DocumentLimitKind::AttestationObjectParseBytes,
                self.limits.max_total_object_parse_bytes,
                self.stats.object_parse_bytes,
                1,
                Some(interval.xref_offset),
            ));
        }
        let work_caps = ObjectWorkCaps::new(
            read_remaining.min(self.object_limits.max_total_read_bytes()),
            parse_remaining.min(self.object_limits.max_total_parse_bytes()),
        )
        .map_err(|error| {
            DocumentError::from_attestation_object(error, interval.reference, interval.xref_offset)
        })?;
        let target = self
            .candidate
            .as_ref()
            .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))?
            .unattested_target(interval.reference)?;
        let context = ObjectJobContext::new(
            self.context.job,
            self.context.object_envelope_checkpoint,
            self.context.object_boundary_checkpoint,
            self.context.priority,
        );
        let child = OpenObjectJob::new_with_work_caps(
            target,
            context,
            self.object_limits,
            self.syntax_limits,
            work_caps,
        )
        .map_err(|error| {
            DocumentError::from_attestation_object(error, interval.reference, interval.xref_offset)
        })?;
        self.state = JobState::Object(ObjectState {
            physical_index,
            child,
            accounted_stats: ObjectStats::default(),
        });
        Ok(())
    }

    fn publish(&mut self, cancellation: &dyn DocumentCancellation) -> RevisionAttestationPoll {
        if cancellation.is_cancelled() {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                None,
                None,
            ));
        }
        let Some(candidate) = self.candidate.take() else {
            return self.fail_internal(None, None);
        };
        let Some(header) = self.header.take() else {
            return self.fail_internal(None, Some(0));
        };
        if self.evidence.len() != candidate.physical_intervals.len()
            || self.stats.objects_attested
                != u64::try_from(candidate.physical_intervals.len()).unwrap_or(u64::MAX)
        {
            return self.fail_internal(None, None);
        }
        let attestations = mem::take(&mut self.evidence);
        self.state = JobState::Complete;
        RevisionAttestationPoll::Ready(AttestedRevisionIndex {
            candidate,
            header,
            attestations,
            attestation_stats: self.stats,
            object_limits: self.object_limits,
            syntax_limits: self.syntax_limits,
        })
    }

    fn validate_header(
        &mut self,
        bytes: &ByteSlice,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        if bytes.range().start() != 0 || bytes.bytes().len() < 9 {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                None,
                Some(bytes.range().start()),
            ));
        }
        if !matches!(bytes.bytes()[8], b'\r' | b'\n') {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidDocumentHeader,
                None,
                Some(8),
            ));
        }
        let input = SyntaxInput::new(
            self.snapshot.identity(),
            0,
            &bytes.bytes()[..8],
            InputExtent::MayContinue,
        )
        .map_err(DocumentError::from_header_syntax)?;
        let adapter = CancellationAdapter(cancellation);
        let mut parser = SyntaxParser::new_with_cancellation(input, self.syntax_limits, &adapter)
            .map_err(DocumentError::from_header_syntax)?;
        match parser.parse_header() {
            SyntaxPoll::Ready(header)
                if header.span().start() == 0 && header.span().end_exclusive() == 8 =>
            {
                self.header = Some(header);
                Ok(())
            }
            SyntaxPoll::Failed(error) => Err(DocumentError::from_header_syntax(error)),
            SyntaxPoll::Ready(_) | SyntaxPoll::NeedMore { .. } | SyntaxPoll::EndOfInput => Err(
                DocumentError::for_code(DocumentErrorCode::InvalidDocumentHeader, None, Some(0)),
            ),
        }
    }

    fn scan_trivia(
        &mut self,
        bytes: &[u8],
        base: u64,
        state: &mut TriviaState,
        cancellation: &dyn DocumentCancellation,
        boundary_reference: Option<pdf_rs_syntax::ObjectRef>,
    ) -> Result<(), DocumentError> {
        for (index, byte) in bytes.iter().copied().enumerate() {
            if index.is_multiple_of(CANCELLATION_INTERVAL) && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    boundary_reference,
                    base.checked_add(index as u64),
                ));
            }
            let offset = base.checked_add(index as u64).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    boundary_reference,
                    Some(base),
                )
            })?;
            self.stats.trivia_scan_bytes =
                self.stats.trivia_scan_bytes.checked_add(1).ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        boundary_reference,
                        Some(offset),
                    )
                })?;
            match *state {
                TriviaState::Neutral if is_pdf_whitespace(byte) => {}
                TriviaState::Neutral if byte == b'%' => {
                    if self.limits.max_comment_bytes < 1 {
                        return Err(DocumentError::resource(
                            DocumentLimitKind::AttestationCommentBytes,
                            self.limits.max_comment_bytes,
                            0,
                            1,
                            Some(offset),
                        ));
                    }
                    *state = TriviaState::Comment {
                        start: offset,
                        bytes: 1,
                    };
                }
                TriviaState::Neutral => {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::TopLevelData,
                        boundary_reference,
                        Some(offset),
                    ));
                }
                TriviaState::Comment { .. } if matches!(byte, b'\r' | b'\n') => {
                    *state = TriviaState::Neutral;
                }
                TriviaState::Comment {
                    start,
                    bytes: consumed,
                } => {
                    let attempted = consumed.checked_add(1).ok_or_else(|| {
                        DocumentError::resource(
                            DocumentLimitKind::AttestationCommentBytes,
                            self.limits.max_comment_bytes,
                            consumed,
                            u64::MAX,
                            Some(start),
                        )
                    })?;
                    if attempted > self.limits.max_comment_bytes {
                        return Err(DocumentError::resource(
                            DocumentLimitKind::AttestationCommentBytes,
                            self.limits.max_comment_bytes,
                            consumed,
                            1,
                            Some(start),
                        ));
                    }
                    *state = TriviaState::Comment {
                        start,
                        bytes: attempted,
                    };
                }
            }
        }
        Ok(())
    }

    fn validate_slice(&self, bytes: &ByteSlice, expected: ByteRange) -> Result<(), DocumentError> {
        if bytes.identity() != self.snapshot.identity() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                None,
                Some(expected.start()),
            ));
        }
        if bytes.range() != expected {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                None,
                Some(expected.start()),
            ));
        }
        Ok(())
    }

    fn map_object_error(
        &self,
        error: ObjectError,
        reference: pdf_rs_syntax::ObjectRef,
        offset: u64,
        work_caps: ObjectWorkCaps,
        object_limits: ObjectLimits,
    ) -> DocumentError {
        if error.code() == ObjectErrorCode::ResourceLimit
            && let Some(limit) = error.limit()
        {
            let lower_offset = error.offset().or(Some(offset));
            match limit.kind() {
                ObjectLimitKind::TotalReadBytes
                    if read_cap_is_aggregate(work_caps, object_limits) =>
                {
                    return DocumentError::aggregate_object_resource(
                        DocumentLimitKind::AttestationObjectReadBytes,
                        self.limits.max_total_object_read_bytes,
                        self.stats.object_read_bytes,
                        limit.attempted(),
                        error,
                        reference,
                        lower_offset.unwrap_or(offset),
                    );
                }
                ObjectLimitKind::TotalParseBytes
                    if parse_cap_is_aggregate(work_caps, object_limits) =>
                {
                    return DocumentError::aggregate_object_resource(
                        DocumentLimitKind::AttestationObjectParseBytes,
                        self.limits.max_total_object_parse_bytes,
                        self.stats.object_parse_bytes,
                        limit.attempted(),
                        error,
                        reference,
                        lower_offset.unwrap_or(offset),
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
        DocumentError::from_attestation_object(error, reference, offset)
    }

    fn scan_boundary_reference(&self, purpose: ScanPurpose) -> Option<pdf_rs_syntax::ObjectRef> {
        let candidate = self.candidate.as_ref()?;
        match purpose {
            ScanPurpose::Prefix => candidate
                .physical_intervals
                .first()
                .map(|interval| interval.reference),
            ScanPurpose::Gap { after_object } => candidate
                .physical_intervals
                .get(after_object + 1)
                .map(|interval| interval.reference),
        }
    }

    fn fail(&mut self, error: DocumentError) -> RevisionAttestationPoll {
        self.state = JobState::Failed(error);
        RevisionAttestationPoll::Failed(error)
    }

    fn fail_internal(
        &mut self,
        reference: Option<pdf_rs_syntax::ObjectRef>,
        offset: Option<u64>,
    ) -> RevisionAttestationPoll {
        self.fail(DocumentError::for_code(
            DocumentErrorCode::InternalState,
            reference,
            offset,
        ))
    }
}

impl fmt::Debug for AttestRevisionJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttestRevisionJob")
            .field("snapshot", &self.snapshot)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("object_limits", &self.object_limits)
            .field("syntax_limits", &self.syntax_limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .field("header", &self.header.as_ref().map(|_| "[REDACTED]"))
            .field("evidence", &"[REDACTED]")
            .field("state_payload", &"[REDACTED]")
            .finish()
    }
}

const fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}

const fn header_syntax_limits_are_valid(limits: SyntaxLimits) -> bool {
    limits.max_input_bytes() >= 8 && limits.max_token_bytes() >= 8
}

const fn read_cap_is_aggregate(caps: ObjectWorkCaps, limits: ObjectLimits) -> bool {
    caps.max_read_bytes() < limits.max_total_read_bytes()
}

const fn parse_cap_is_aggregate(caps: ObjectWorkCaps, limits: ObjectLimits) -> bool {
    caps.max_parse_bytes() < limits.max_total_parse_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_rs_syntax::SyntaxLimitConfig;

    #[test]
    fn fixed_evidence_charge_dominates_the_retained_type() {
        assert!(
            mem::size_of::<ObjectAttestation>()
                <= usize::try_from(ACCOUNTED_OBJECT_ATTESTATION_BYTES).unwrap()
        );
    }

    #[test]
    fn whitespace_set_matches_the_strict_pdf_six_byte_set() {
        for byte in 0_u8..=u8::MAX {
            assert_eq!(
                is_pdf_whitespace(byte),
                matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
            );
        }
    }

    fn tiny_syntax_limits(input_bytes: u64, token_bytes: u64) -> SyntaxLimits {
        SyntaxLimits::validate(SyntaxLimitConfig {
            max_input_bytes: input_bytes,
            max_token_bytes: token_bytes,
            max_comment_bytes: 1,
            max_name_bytes: 1,
            max_string_source_bytes: 1,
            max_string_decoded_bytes: 1,
            max_owned_bytes: 1,
            max_total_tokens: 1,
            max_container_entries: 1,
            max_container_bytes: 1,
            max_container_depth: 1,
        })
        .unwrap()
    }

    #[test]
    fn header_parser_profile_requires_exactly_eight_input_and_token_bytes() {
        assert!(!header_syntax_limits_are_valid(tiny_syntax_limits(7, 7)));
        assert!(!header_syntax_limits_are_valid(tiny_syntax_limits(8, 7)));
        assert!(header_syntax_limits_are_valid(tiny_syntax_limits(8, 8)));
    }

    #[test]
    fn equal_child_caps_remain_object_local_but_smaller_caps_are_aggregate() {
        let limits = ObjectLimits::default();
        let equal = ObjectWorkCaps::new(
            limits.max_total_read_bytes(),
            limits.max_total_parse_bytes(),
        )
        .unwrap();
        assert!(!read_cap_is_aggregate(equal, limits));
        assert!(!parse_cap_is_aggregate(equal, limits));

        let smaller = ObjectWorkCaps::new(
            limits.max_total_read_bytes() - 1,
            limits.max_total_parse_bytes() - 1,
        )
        .unwrap();
        assert!(read_cap_is_aggregate(smaller, limits));
        assert!(parse_cap_is_aggregate(smaller, limits));
    }
}
