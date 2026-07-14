use std::fmt;
use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_syntax::{ByteSpan, InputExtent, SyntaxLimits};

use crate::parser::{
    BoundaryParse, EnvelopeContext, EnvelopeParse, ParsedStreamEnvelope, parse_boundary,
    parse_envelope,
};
use crate::{
    FramedStream, IndirectObject, IndirectObjectTarget, IndirectObjectValue, ObjectError,
    ObjectErrorCode, ObjectLimitKind, ObjectLimits, ObjectWorkCaps,
};

/// Runtime identity, phase checkpoints, and scheduling priority for one object job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectJobContext {
    job: JobId,
    envelope_checkpoint: ResumeCheckpoint,
    boundary_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl ObjectJobContext {
    /// Creates a context with distinct envelope and stream-boundary checkpoints.
    ///
    /// [`OpenObjectJob::new`] rejects a context whose checkpoints are equal.
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

    /// Returns the runtime scheduling priority copied to every read request.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }
}

/// Coarse resumable phase of one indirect-object framing job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectPhase {
    /// Reading and parsing the indirect-object envelope.
    Envelope,
    /// Validating framing at the exact declared stream payload end.
    StreamBoundary,
    /// The framed object was returned and the one-shot job is complete.
    Complete,
    /// The job reached a terminal structured failure.
    Failed,
}

/// Cumulative deterministic work charged by one open object job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObjectStats {
    read_bytes: u64,
    parse_bytes: u64,
    envelope_attempts: u64,
    boundary_attempts: u64,
    declared_stream_bytes: u64,
}

impl ObjectStats {
    /// Returns bytes charged on the first attempt of each logical exact request.
    pub const fn read_bytes(self) -> u64 {
        self.read_bytes
    }

    /// Returns complete window bytes charged before parser attempts.
    pub const fn parse_bytes(self) -> u64 {
        self.parse_bytes
    }

    /// Returns the number of distinct object-envelope windows requested.
    pub const fn envelope_attempts(self) -> u64 {
        self.envelope_attempts
    }

    /// Returns the number of distinct stream-boundary windows requested.
    pub const fn boundary_attempts(self) -> u64 {
        self.boundary_attempts
    }

    /// Returns the accepted direct stream length, or zero for direct objects.
    pub const fn declared_stream_bytes(self) -> u64 {
        self.declared_stream_bytes
    }
}

/// Cooperative cancellation probe supplied by the owning runtime.
pub trait ObjectCancellation: Send + Sync {
    /// Reports whether the job must stop at the next bounded probe.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation probe that never requests cancellation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NeverCancelled;

impl ObjectCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl ObjectCancellation for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

/// Result of polling one resumable indirect-object framing job.
#[allow(
    clippy::large_enum_variant,
    reason = "the one-shot ready value stays inline to avoid an untracked infallible heap allocation"
)]
#[derive(Debug, Eq, PartialEq)]
pub enum ObjectPoll {
    /// The complete source-bound indirect object is framed.
    Ready(IndirectObject),
    /// Required bytes are absent and the runtime must wait for the ticket.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the requested window.
        missing: SmallRanges,
        /// Phase checkpoint that the runtime must retain when requeueing the job.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a terminal structured failure.
    Failed(ObjectError),
}

enum JobState {
    Envelope {
        window: u64,
        charged: bool,
    },
    StreamBoundary {
        envelope: ParsedStreamEnvelope,
        window: u64,
        charged: bool,
    },
    Complete,
    Failed(ObjectError),
}

#[derive(Clone, Copy)]
enum PollStep {
    Envelope { window: u64, charged: bool },
    StreamBoundary { window: u64, charged: bool },
}

/// One-shot, snapshot-bound job for framing one indirect object.
pub struct OpenObjectJob {
    target: IndirectObjectTarget,
    source_len: u64,
    context: ObjectJobContext,
    limits: ObjectLimits,
    work_caps: ObjectWorkCaps,
    syntax_limits: SyntaxLimits,
    stats: ObjectStats,
    state: JobState,
}

impl OpenObjectJob {
    /// Validates configuration and binds a job to an xref-derived object target.
    pub fn new(
        target: IndirectObjectTarget,
        context: ObjectJobContext,
        limits: ObjectLimits,
        syntax_limits: SyntaxLimits,
    ) -> Result<Self, ObjectError> {
        Self::new_with_work_caps(
            target,
            context,
            limits,
            syntax_limits,
            ObjectWorkCaps::from_limits(limits),
        )
    }

    /// Validates configuration and binds a job to parent-supplied cumulative work caps.
    ///
    /// The caps may be smaller than configured phase windows, but they cannot exceed the
    /// corresponding cumulative totals in `limits`.
    pub fn new_with_work_caps(
        target: IndirectObjectTarget,
        context: ObjectJobContext,
        limits: ObjectLimits,
        syntax_limits: SyntaxLimits,
        work_caps: ObjectWorkCaps,
    ) -> Result<Self, ObjectError> {
        let reference = target.reference();
        if work_caps.max_read_bytes() > limits.max_total_read_bytes()
            || work_caps.max_parse_bytes() > limits.max_total_parse_bytes()
        {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidLimits,
                Some(reference),
                None,
            ));
        }
        if context.envelope_checkpoint == context.boundary_checkpoint {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidJobContext,
                Some(reference),
                None,
            ));
        }
        let source_len = target.snapshot().len().ok_or_else(|| {
            ObjectError::for_code(ObjectErrorCode::UnknownSourceLength, Some(reference), None)
        })?;
        if source_len > limits.max_source_bytes() {
            return Err(ObjectError::resource(
                ObjectLimitKind::SourceBytes,
                limits.max_source_bytes(),
                0,
                source_len,
                Some(reference),
                None,
            ));
        }
        if limits.max_envelope_bytes() > syntax_limits.max_input_bytes()
            || limits.max_boundary_bytes() > syntax_limits.max_input_bytes()
        {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidLimits,
                Some(reference),
                None,
            ));
        }
        let prefix_bytes = u64::from(target.xref_offset() != 0);
        let available = target
            .object_upper_bound()
            .checked_sub(target.xref_offset())
            .and_then(|value| value.checked_add(prefix_bytes))
            .ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InvalidTarget,
                    Some(reference),
                    Some(target.xref_offset()),
                )
            })?;
        let window = limits.initial_envelope_bytes().min(available);
        Ok(Self {
            target,
            source_len,
            context,
            limits,
            work_caps,
            syntax_limits,
            stats: ObjectStats::default(),
            state: JobState::Envelope {
                window,
                charged: false,
            },
        })
    }

    /// Returns the immutable source snapshot bound at job creation.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.target.snapshot()
    }

    /// Returns the expected object reference and source geometry.
    pub const fn target(&self) -> IndirectObjectTarget {
        self.target
    }

    /// Returns the runtime identity, checkpoints, and request priority.
    pub const fn context(&self) -> ObjectJobContext {
        self.context
    }

    /// Returns the validated deterministic object limits.
    pub const fn limits(&self) -> ObjectLimits {
        self.limits
    }

    /// Returns the parent-supplied cumulative work caps enforced by this job.
    pub const fn work_caps(&self) -> ObjectWorkCaps {
        self.work_caps
    }

    /// Returns cumulative work charged through the most recent poll.
    pub const fn stats(&self) -> ObjectStats {
        self.stats
    }

    /// Returns the job's current coarse phase.
    pub const fn phase(&self) -> ObjectPhase {
        match self.state {
            JobState::Envelope { .. } => ObjectPhase::Envelope,
            JobState::StreamBoundary { .. } => ObjectPhase::StreamBoundary,
            JobState::Complete => ObjectPhase::Complete,
            JobState::Failed(_) => ObjectPhase::Failed,
        }
    }

    /// Advances the job without performing file, network, callback, or async-runtime I/O.
    ///
    /// A [`ObjectPoll::Pending`] result preserves the current logical window. Re-polling that
    /// window after its ticket completes does not charge the read budget a second time.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn ObjectCancellation + '_),
    ) -> ObjectPoll {
        match self.state {
            JobState::Failed(error) => return ObjectPoll::Failed(error),
            JobState::Complete => {
                return ObjectPoll::Failed(ObjectError::for_code(
                    ObjectErrorCode::JobAlreadyComplete,
                    Some(self.target.reference()),
                    None,
                ));
            }
            JobState::Envelope { .. } | JobState::StreamBoundary { .. } => {}
        }

        loop {
            if source.snapshot() != self.target.snapshot() {
                return self.fail(ObjectError::for_code(
                    ObjectErrorCode::SnapshotMismatch,
                    Some(self.target.reference()),
                    None,
                ));
            }
            if cancellation.is_cancelled() {
                return self.fail(ObjectError::for_code(
                    ObjectErrorCode::Cancelled,
                    Some(self.target.reference()),
                    None,
                ));
            }
            let step = match &self.state {
                JobState::Envelope { window, charged } => PollStep::Envelope {
                    window: *window,
                    charged: *charged,
                },
                JobState::StreamBoundary {
                    window, charged, ..
                } => PollStep::StreamBoundary {
                    window: *window,
                    charged: *charged,
                },
                JobState::Complete => {
                    return ObjectPoll::Failed(ObjectError::for_code(
                        ObjectErrorCode::JobAlreadyComplete,
                        Some(self.target.reference()),
                        None,
                    ));
                }
                JobState::Failed(error) => return ObjectPoll::Failed(*error),
            };
            let outcome = match step {
                PollStep::Envelope { window, charged } => {
                    self.poll_envelope(source, cancellation, window, charged)
                }
                PollStep::StreamBoundary { window, charged } => {
                    self.poll_boundary(source, cancellation, window, charged)
                }
            };
            if let Some(result) = outcome {
                return result;
            }
        }
    }

    fn poll_envelope(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn ObjectCancellation,
        window: u64,
        charged: bool,
    ) -> Option<ObjectPoll> {
        let object_start = self.target.xref_offset();
        let prefix_bytes = u64::from(object_start != 0);
        let range_start = match object_start.checked_sub(prefix_bytes) {
            Some(value) => value,
            None => return Some(self.fail_internal(Some(object_start))),
        };
        let range = match ByteRange::new(range_start, window) {
            Ok(value) if value.end_exclusive() <= self.target.object_upper_bound() => value,
            _ => return Some(self.fail_internal(Some(object_start))),
        };
        if !charged {
            if let Err(error) = self.charge_read(window, Some(range_start)) {
                return Some(self.fail(error));
            }
            self.stats.envelope_attempts = match self.stats.envelope_attempts.checked_add(1) {
                Some(value) => value,
                None => return Some(self.fail_internal(Some(object_start))),
            };
            self.state = JobState::Envelope {
                window,
                charged: true,
            };
        }
        let request = ReadRequest::new(
            range,
            self.context.priority,
            self.context.job,
            self.context.envelope_checkpoint,
        );
        match source.poll(request) {
            ReadPoll::Pending { ticket, missing } => Some(ObjectPoll::Pending {
                ticket,
                missing,
                checkpoint: self.context.envelope_checkpoint,
            }),
            ReadPoll::EndOfFile => Some(self.fail(ObjectError::for_code(
                ObjectErrorCode::UnexpectedEndOfSource,
                Some(self.target.reference()),
                Some(object_start),
            ))),
            ReadPoll::Failed(error) => Some(self.fail(ObjectError::from_source(
                error,
                Some(self.target.reference()),
                Some(object_start),
            ))),
            ReadPoll::Ready(bytes) => {
                if let Err(error) = self.validate_slice(&bytes, range) {
                    return Some(self.fail(error));
                }
                if let Err(error) = self.charge_parse(window, Some(range_start)) {
                    return Some(self.fail(error));
                }
                let prefix_len = match usize::try_from(prefix_bytes) {
                    Ok(value) => value,
                    Err(_) => return Some(self.fail_internal(Some(object_start))),
                };
                if prefix_len != 0 && !is_object_header_boundary(bytes.bytes()[0]) {
                    return Some(self.fail(ObjectError::for_code(
                        ObjectErrorCode::InvalidObjectHeader,
                        Some(self.target.reference()),
                        Some(object_start),
                    )));
                }
                match parse_envelope(
                    EnvelopeContext {
                        source: self.target.snapshot().identity(),
                        reference: self.target.reference(),
                        xref_offset: object_start,
                        object_upper_bound: self.target.object_upper_bound(),
                        limits: self.limits,
                        syntax_limits: self.syntax_limits,
                    },
                    &bytes.bytes()[prefix_len..],
                    InputExtent::MayContinue,
                    cancellation,
                ) {
                    Ok(EnvelopeParse::Direct(parsed)) => {
                        if cancellation.is_cancelled() {
                            return Some(self.fail(ObjectError::for_code(
                                ObjectErrorCode::Cancelled,
                                Some(self.target.reference()),
                                Some(parsed.endobj_span.start()),
                            )));
                        }
                        let object_span = match self.object_span(parsed.endobj_span) {
                            Ok(value) => value,
                            Err(error) => return Some(self.fail(error)),
                        };
                        let object = IndirectObject::new(
                            self.target,
                            parsed.header_span,
                            object_span,
                            parsed.endobj_span,
                            IndirectObjectValue::Direct(parsed.value),
                        );
                        self.state = JobState::Complete;
                        Some(ObjectPoll::Ready(object))
                    }
                    Ok(EnvelopeParse::Stream(envelope)) => {
                        let data_end = envelope.data_span.end_exclusive();
                        let remaining = match self.target.object_upper_bound().checked_sub(data_end)
                        {
                            Some(value) if value != 0 => value,
                            _ => {
                                return Some(self.fail(ObjectError::for_code(
                                    ObjectErrorCode::ObjectCrossesPhysicalBound,
                                    Some(self.target.reference()),
                                    Some(self.target.object_upper_bound()),
                                )));
                            }
                        };
                        self.stats.declared_stream_bytes = envelope.data_span.len();
                        self.state = JobState::StreamBoundary {
                            envelope,
                            window: self.limits.initial_boundary_bytes().min(remaining),
                            charged: false,
                        };
                        None
                    }
                    Ok(EnvelopeParse::NeedMore { minimum_end }) => {
                        let available =
                            match self.target.object_upper_bound().checked_sub(range_start) {
                                Some(value) => value,
                                None => return Some(self.fail_internal(Some(object_start))),
                            };
                        match self.grow_or_fail(
                            range_start,
                            window,
                            available,
                            self.limits.max_envelope_bytes(),
                            minimum_end,
                            ObjectLimitKind::EnvelopeBytes,
                        ) {
                            Ok(next) => {
                                self.state = JobState::Envelope {
                                    window: next,
                                    charged: false,
                                };
                                None
                            }
                            Err(error) => Some(self.fail(error)),
                        }
                    }
                    Err(error) => Some(self.fail(error)),
                }
            }
        }
    }

    fn poll_boundary(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn ObjectCancellation,
        window: u64,
        charged: bool,
    ) -> Option<ObjectPoll> {
        let data_end = match &self.state {
            JobState::StreamBoundary { envelope, .. } => envelope.data_span.end_exclusive(),
            _ => return Some(self.fail_internal(None)),
        };
        let range = match ByteRange::new(data_end, window) {
            Ok(value) if value.end_exclusive() <= self.target.object_upper_bound() => value,
            _ => return Some(self.fail_internal(Some(data_end))),
        };
        if !charged {
            if let Err(error) = self.charge_read(window, Some(data_end)) {
                return Some(self.fail(error));
            }
            self.stats.boundary_attempts = match self.stats.boundary_attempts.checked_add(1) {
                Some(value) => value,
                None => return Some(self.fail_internal(Some(data_end))),
            };
            if let JobState::StreamBoundary { charged, .. } = &mut self.state {
                *charged = true;
            } else {
                return Some(self.fail_internal(Some(data_end)));
            }
        }
        let request = ReadRequest::new(
            range,
            self.context.priority,
            self.context.job,
            self.context.boundary_checkpoint,
        );
        match source.poll(request) {
            ReadPoll::Pending { ticket, missing } => Some(ObjectPoll::Pending {
                ticket,
                missing,
                checkpoint: self.context.boundary_checkpoint,
            }),
            ReadPoll::EndOfFile => Some(self.fail(ObjectError::for_code(
                ObjectErrorCode::UnexpectedEndOfSource,
                Some(self.target.reference()),
                Some(data_end),
            ))),
            ReadPoll::Failed(error) => Some(self.fail(ObjectError::from_source(
                error,
                Some(self.target.reference()),
                Some(data_end),
            ))),
            ReadPoll::Ready(bytes) => {
                if let Err(error) = self.validate_slice(&bytes, range) {
                    return Some(self.fail(error));
                }
                if let Err(error) = self.charge_parse(window, Some(data_end)) {
                    return Some(self.fail(error));
                }
                match parse_boundary(
                    self.target.snapshot().identity(),
                    self.target.reference(),
                    data_end,
                    bytes.bytes(),
                    InputExtent::MayContinue,
                    self.syntax_limits,
                    cancellation,
                ) {
                    Ok(BoundaryParse::Complete(boundary)) => {
                        if cancellation.is_cancelled() {
                            return Some(self.fail(ObjectError::for_code(
                                ObjectErrorCode::Cancelled,
                                Some(self.target.reference()),
                                Some(boundary.endobj_span.start()),
                            )));
                        }
                        let object_span = match self.object_span(boundary.endobj_span) {
                            Ok(value) => value,
                            Err(error) => return Some(self.fail(error)),
                        };
                        let state = mem::replace(&mut self.state, JobState::Complete);
                        let JobState::StreamBoundary { envelope, .. } = state else {
                            return Some(self.fail_internal(Some(data_end)));
                        };
                        let stream = FramedStream::new(
                            envelope.dictionary,
                            envelope.length_value_span,
                            envelope.stream_keyword_span,
                            envelope.stream_line_ending_span,
                            envelope.data_span,
                            boundary.data_delimiter_span,
                            boundary.endstream_span,
                        );
                        let object = IndirectObject::new(
                            self.target,
                            envelope.header_span,
                            object_span,
                            boundary.endobj_span,
                            IndirectObjectValue::Stream(stream),
                        );
                        Some(ObjectPoll::Ready(object))
                    }
                    Ok(BoundaryParse::NeedMore { minimum_end }) => {
                        let available = match self.target.object_upper_bound().checked_sub(data_end)
                        {
                            Some(value) => value,
                            None => return Some(self.fail_internal(Some(data_end))),
                        };
                        match self.grow_or_fail(
                            data_end,
                            window,
                            available,
                            self.limits.max_boundary_bytes(),
                            minimum_end,
                            ObjectLimitKind::BoundaryBytes,
                        ) {
                            Ok(next) => {
                                if let JobState::StreamBoundary {
                                    window, charged, ..
                                } = &mut self.state
                                {
                                    *window = next;
                                    *charged = false;
                                    None
                                } else {
                                    Some(self.fail_internal(Some(data_end)))
                                }
                            }
                            Err(error) => Some(self.fail(error)),
                        }
                    }
                    Err(error) => Some(self.fail(error)),
                }
            }
        }
    }

    fn validate_slice(&self, bytes: &ByteSlice, expected: ByteRange) -> Result<(), ObjectError> {
        if bytes.identity() != self.target.snapshot().identity() {
            return Err(ObjectError::for_code(
                ObjectErrorCode::SnapshotMismatch,
                Some(self.target.reference()),
                None,
            ));
        }
        if bytes.range() != expected {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(self.target.reference()),
                Some(expected.start()),
            ));
        }
        Ok(())
    }

    fn charge_read(&mut self, amount: u64, offset: Option<u64>) -> Result<(), ObjectError> {
        let Some(total) = self.stats.read_bytes.checked_add(amount) else {
            return Err(ObjectError::resource(
                ObjectLimitKind::TotalReadBytes,
                self.work_caps.max_read_bytes(),
                self.stats.read_bytes,
                amount,
                Some(self.target.reference()),
                offset,
            ));
        };
        if total > self.work_caps.max_read_bytes() {
            return Err(ObjectError::resource(
                ObjectLimitKind::TotalReadBytes,
                self.work_caps.max_read_bytes(),
                self.stats.read_bytes,
                amount,
                Some(self.target.reference()),
                offset,
            ));
        }
        self.stats.read_bytes = total;
        Ok(())
    }

    fn charge_parse(&mut self, amount: u64, offset: Option<u64>) -> Result<(), ObjectError> {
        let Some(total) = self.stats.parse_bytes.checked_add(amount) else {
            return Err(ObjectError::resource(
                ObjectLimitKind::TotalParseBytes,
                self.work_caps.max_parse_bytes(),
                self.stats.parse_bytes,
                amount,
                Some(self.target.reference()),
                offset,
            ));
        };
        if total > self.work_caps.max_parse_bytes() {
            return Err(ObjectError::resource(
                ObjectLimitKind::TotalParseBytes,
                self.work_caps.max_parse_bytes(),
                self.stats.parse_bytes,
                amount,
                Some(self.target.reference()),
                offset,
            ));
        }
        self.stats.parse_bytes = total;
        Ok(())
    }

    fn grow_or_fail(
        &self,
        base: u64,
        current: u64,
        available: u64,
        configured_cap: u64,
        minimum_end: u64,
        kind: ObjectLimitKind,
    ) -> Result<u64, ObjectError> {
        let required = minimum_end.checked_sub(base).ok_or_else(|| {
            ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(self.target.reference()),
                Some(base),
            )
        })?;
        let cap = configured_cap.min(available);
        if current >= cap || required > cap {
            if cap == available {
                return Err(ObjectError::for_code(
                    ObjectErrorCode::ObjectCrossesPhysicalBound,
                    Some(self.target.reference()),
                    Some(self.target.object_upper_bound()),
                ));
            }
            return Err(ObjectError::resource(
                kind,
                configured_cap,
                current,
                required.saturating_sub(current).max(1),
                Some(self.target.reference()),
                Some(base),
            ));
        }
        let doubled = current.saturating_mul(2);
        let next = doubled.max(required).min(cap);
        if next <= current {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(self.target.reference()),
                Some(base),
            ));
        }
        Ok(next)
    }

    fn object_span(&self, endobj_span: ByteSpan) -> Result<ByteSpan, ObjectError> {
        if endobj_span.end_exclusive() > self.target.object_upper_bound() {
            return Err(ObjectError::for_code(
                ObjectErrorCode::ObjectCrossesPhysicalBound,
                Some(self.target.reference()),
                Some(self.target.object_upper_bound()),
            ));
        }
        let len = endobj_span
            .end_exclusive()
            .checked_sub(self.target.xref_offset())
            .ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(self.target.reference()),
                    Some(self.target.xref_offset()),
                )
            })?;
        ByteSpan::new(self.target.xref_offset(), len).map_err(|_| {
            ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(self.target.reference()),
                Some(self.target.xref_offset()),
            )
        })
    }

    fn fail(&mut self, error: ObjectError) -> ObjectPoll {
        self.state = JobState::Failed(error);
        ObjectPoll::Failed(error)
    }

    fn fail_internal(&mut self, offset: Option<u64>) -> ObjectPoll {
        self.fail(ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(self.target.reference()),
            offset,
        ))
    }
}

impl fmt::Debug for OpenObjectJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenObjectJob")
            .field("target", &self.target)
            .field("source_len", &self.source_len)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("work_caps", &self.work_caps)
            .field("syntax_limits", &self.syntax_limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .field("state_payload", &"[REDACTED]")
            .finish()
    }
}

fn is_object_header_boundary(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}
