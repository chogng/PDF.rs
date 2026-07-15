use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, ReadPoll, ReadRequest, SmallRanges, SourceSnapshot,
};
use pdf_rs_syntax::{ByteSpan, InputExtent, SyntaxLimits};

use crate::parser::{
    BoundaryParse, EnvelopeContext, EnvelopeParse, parse_boundary, parse_envelope,
};
use crate::{
    FramedStream, IndirectObject, IndirectObjectTarget, IndirectObjectValue, ObjectCancellation,
    ObjectError, ObjectErrorCode, ObjectJobContext, ObjectLimitKind, ObjectLimits, ObjectPhase,
    ObjectPoll, ObjectStats, ObjectWorkCaps, StreamEnvelope, StreamLengthClaim,
};

/// Result of polling a staged indirect-object envelope job.
#[allow(
    clippy::large_enum_variant,
    reason = "ready envelopes remain inline so allocation accounting stays explicit"
)]
#[derive(Debug, Eq, PartialEq)]
pub enum ObjectEnvelopePoll {
    /// A complete non-stream indirect object was validated.
    Direct(IndirectObject),
    /// A stream dictionary and payload start were validated; length framing remains.
    Stream(StreamEnvelope),
    /// Required bytes are absent and the runtime must wait for the ticket.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: pdf_rs_bytes::DataTicket,
        /// Canonical exact ranges still missing from the requested window.
        missing: SmallRanges,
        /// Envelope checkpoint that the runtime must retain when requeueing the job.
        checkpoint: pdf_rs_bytes::ResumeCheckpoint,
    },
    /// The job reached a terminal structured failure.
    Failed(ObjectError),
}

enum EnvelopeJobState {
    Reading { window: u64, charged: bool },
    Complete,
    Failed(ObjectError),
}

/// One-shot job that stops after validating an object envelope.
///
/// Direct objects are returned complete. Stream objects retain their dictionary,
/// direct value or indirect `/Length` dependency, and payload start without
/// requesting payload-sized input or guessing the terminal boundary.
pub struct OpenObjectEnvelopeJob {
    target: IndirectObjectTarget,
    source_len: u64,
    context: ObjectJobContext,
    limits: ObjectLimits,
    work_caps: ObjectWorkCaps,
    syntax_limits: SyntaxLimits,
    stats: ObjectStats,
    state: EnvelopeJobState,
}

impl OpenObjectEnvelopeJob {
    /// Validates configuration and binds a staged envelope job to an object target.
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

    /// Creates an envelope job with parent-supplied cumulative work caps.
    pub fn new_with_work_caps(
        target: IndirectObjectTarget,
        context: ObjectJobContext,
        limits: ObjectLimits,
        syntax_limits: SyntaxLimits,
        work_caps: ObjectWorkCaps,
    ) -> Result<Self, ObjectError> {
        validate_common(target, context, limits, syntax_limits, work_caps)?;
        let source_len = target.snapshot().len().ok_or_else(|| {
            ObjectError::for_code(
                ObjectErrorCode::UnknownSourceLength,
                Some(target.reference()),
                None,
            )
        })?;
        let prefix_bytes = u64::from(target.xref_offset() != 0);
        let available = target
            .object_upper_bound()
            .checked_sub(target.xref_offset())
            .and_then(|value| value.checked_add(prefix_bytes))
            .ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InvalidTarget,
                    Some(target.reference()),
                    Some(target.xref_offset()),
                )
            })?;
        Ok(Self {
            target,
            source_len,
            context,
            limits,
            work_caps,
            syntax_limits,
            stats: ObjectStats::default(),
            state: EnvelopeJobState::Reading {
                window: limits.initial_envelope_bytes().min(available),
                charged: false,
            },
        })
    }

    /// Returns the immutable snapshot bound at job creation.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.target.snapshot()
    }

    /// Returns the xref-derived object target.
    pub const fn target(&self) -> IndirectObjectTarget {
        self.target
    }

    /// Returns work accounting through the most recent poll.
    pub const fn stats(&self) -> ObjectStats {
        self.stats
    }

    /// Returns the job's current coarse phase.
    pub const fn phase(&self) -> ObjectPhase {
        match self.state {
            EnvelopeJobState::Reading { .. } => ObjectPhase::Envelope,
            EnvelopeJobState::Complete => ObjectPhase::Complete,
            EnvelopeJobState::Failed(_) => ObjectPhase::Failed,
        }
    }

    /// Advances envelope validation without performing host I/O itself.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn ObjectCancellation + '_),
    ) -> ObjectEnvelopePoll {
        let (window, charged) = match self.state {
            EnvelopeJobState::Reading { window, charged } => (window, charged),
            EnvelopeJobState::Complete => {
                return ObjectEnvelopePoll::Failed(ObjectError::for_code(
                    ObjectErrorCode::JobAlreadyComplete,
                    Some(self.target.reference()),
                    None,
                ));
            }
            EnvelopeJobState::Failed(error) => return ObjectEnvelopePoll::Failed(error),
        };
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
        self.poll_read(source, cancellation, window, charged)
    }

    fn poll_read(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn ObjectCancellation,
        window: u64,
        charged: bool,
    ) -> ObjectEnvelopePoll {
        let object_start = self.target.xref_offset();
        let prefix_bytes = u64::from(object_start != 0);
        let Some(range_start) = object_start.checked_sub(prefix_bytes) else {
            return self.fail_internal(Some(object_start));
        };
        let Ok(range) = ByteRange::new(range_start, window) else {
            return self.fail_internal(Some(range_start));
        };
        if range.end_exclusive() > self.target.object_upper_bound() {
            return self.fail_internal(Some(range_start));
        }
        if !charged {
            if let Err(error) = charge_read(
                &mut self.stats,
                self.work_caps,
                window,
                self.target.reference(),
                Some(range_start),
            ) {
                return self.fail(error);
            }
            let Some(attempts) = self.stats.envelope_attempts.checked_add(1) else {
                return self.fail_internal(Some(range_start));
            };
            self.stats.envelope_attempts = attempts;
            self.state = EnvelopeJobState::Reading {
                window,
                charged: true,
            };
        }
        let request = ReadRequest::new(
            range,
            self.context.priority(),
            self.context.job(),
            self.context.envelope_checkpoint(),
        );
        match source.poll(request) {
            ReadPoll::Pending { ticket, missing } => ObjectEnvelopePoll::Pending {
                ticket,
                missing,
                checkpoint: self.context.envelope_checkpoint(),
            },
            ReadPoll::EndOfFile => self.fail(ObjectError::for_code(
                ObjectErrorCode::UnexpectedEndOfSource,
                Some(self.target.reference()),
                Some(object_start),
            )),
            ReadPoll::Failed(error) => self.fail(ObjectError::from_source(
                error,
                Some(self.target.reference()),
                Some(object_start),
            )),
            ReadPoll::Ready(bytes) => self.parse_ready(
                source,
                &bytes,
                range,
                range_start,
                prefix_bytes,
                window,
                cancellation,
            ),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the parser attempt receives exact source-window geometry without hidden mutable state"
    )]
    fn parse_ready(
        &mut self,
        source: &dyn ByteSource,
        bytes: &ByteSlice,
        range: ByteRange,
        range_start: u64,
        prefix_bytes: u64,
        window: u64,
        cancellation: &dyn ObjectCancellation,
    ) -> ObjectEnvelopePoll {
        if let Err(error) = validate_slice(bytes, range, self.target) {
            return self.fail(error);
        }
        if let Err(error) = charge_parse(
            &mut self.stats,
            self.work_caps,
            window,
            self.target.reference(),
            Some(range_start),
        ) {
            return self.fail(error);
        }
        let Ok(prefix_len) = usize::try_from(prefix_bytes) else {
            return self.fail_internal(Some(self.target.xref_offset()));
        };
        if prefix_len != 0 && !is_object_header_boundary(bytes.bytes()[0]) {
            return self.fail(ObjectError::for_code(
                ObjectErrorCode::InvalidObjectHeader,
                Some(self.target.reference()),
                Some(self.target.xref_offset()),
            ));
        }
        match parse_envelope(
            EnvelopeContext {
                source: self.target.snapshot().identity(),
                reference: self.target.reference(),
                xref_offset: self.target.xref_offset(),
                object_upper_bound: self.target.object_upper_bound(),
                allow_indirect_length: true,
                syntax_limits: self.syntax_limits,
            },
            &bytes.bytes()[prefix_len..],
            InputExtent::MayContinue,
            cancellation,
        ) {
            Ok(EnvelopeParse::Direct(parsed)) => {
                if cancellation.is_cancelled() {
                    return self.fail(ObjectError::for_code(
                        ObjectErrorCode::Cancelled,
                        Some(self.target.reference()),
                        Some(parsed.endobj_span.start()),
                    ));
                }
                let object_span = match object_span(self.target, parsed.endobj_span) {
                    Ok(value) => value,
                    Err(error) => return self.fail(error),
                };
                self.stats.retained_heap_bytes = parsed.retained_heap_bytes;
                self.state = EnvelopeJobState::Complete;
                ObjectEnvelopePoll::Direct(IndirectObject::new(
                    self.target,
                    parsed.header_span,
                    object_span,
                    parsed.endobj_span,
                    parsed.retained_heap_bytes,
                    IndirectObjectValue::Direct(parsed.value),
                ))
            }
            Ok(EnvelopeParse::Stream(parsed)) => {
                if cancellation.is_cancelled() {
                    return self.fail(ObjectError::for_code(
                        ObjectErrorCode::Cancelled,
                        Some(self.target.reference()),
                        Some(parsed.stream_keyword_span.start()),
                    ));
                }
                self.stats.declared_stream_bytes =
                    parsed.declared_length.direct_value().unwrap_or_default();
                self.stats.retained_heap_bytes = parsed.retained_heap_bytes;
                let stats = self.stats;
                self.state = EnvelopeJobState::Complete;
                ObjectEnvelopePoll::Stream(StreamEnvelope::new(
                    self.target,
                    parsed.header_span,
                    parsed.dictionary,
                    parsed.declared_length,
                    parsed.stream_keyword_span,
                    parsed.stream_line_ending_span,
                    parsed.data_start,
                    parsed.retained_heap_bytes,
                    self.context,
                    self.limits,
                    self.work_caps,
                    self.syntax_limits,
                    stats,
                ))
            }
            Ok(EnvelopeParse::NeedMore { minimum_end }) => {
                let Some(available) = self.target.object_upper_bound().checked_sub(range_start)
                else {
                    return self.fail_internal(Some(range_start));
                };
                match grow_or_fail(
                    self.target,
                    range_start,
                    window,
                    available,
                    self.limits.max_envelope_bytes(),
                    minimum_end,
                    ObjectLimitKind::EnvelopeBytes,
                ) {
                    Ok(next) => {
                        self.state = EnvelopeJobState::Reading {
                            window: next,
                            charged: false,
                        };
                        self.poll(source, cancellation)
                    }
                    Err(error) => self.fail(error),
                }
            }
            Err(error) => self.fail(error),
        }
    }

    fn fail(&mut self, error: ObjectError) -> ObjectEnvelopePoll {
        self.state = EnvelopeJobState::Failed(error);
        ObjectEnvelopePoll::Failed(error)
    }

    fn fail_internal(&mut self, offset: Option<u64>) -> ObjectEnvelopePoll {
        self.fail(ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(self.target.reference()),
            offset,
        ))
    }
}

impl fmt::Debug for OpenObjectEnvelopeJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenObjectEnvelopeJob")
            .field("target", &self.target)
            .field("source_len", &self.source_len)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("work_caps", &self.work_caps)
            .field("syntax_limits", &self.syntax_limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .finish()
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "the one-shot stream envelope stays inline to avoid an untracked infallible allocation"
)]
enum BoundaryJobState {
    Reading {
        envelope: StreamEnvelope,
        data_span: ByteSpan,
        window: u64,
        charged: bool,
    },
    Complete,
    Failed(ObjectError),
}

/// One-shot job that validates stream framing at an exact claimed payload end.
pub struct OpenStreamBoundaryJob {
    target: IndirectObjectTarget,
    source_len: u64,
    context: ObjectJobContext,
    limits: ObjectLimits,
    work_caps: ObjectWorkCaps,
    syntax_limits: SyntaxLimits,
    length_claim: StreamLengthClaim,
    stats: ObjectStats,
    state: BoundaryJobState,
}

impl OpenStreamBoundaryJob {
    /// Validates a direct or resolver-supplied claim and continues the sealed object budget.
    pub fn new(
        envelope: StreamEnvelope,
        length_claim: StreamLengthClaim,
    ) -> Result<Self, ObjectError> {
        let target = envelope.target();
        let context = envelope.context();
        let limits = envelope.limits();
        let syntax_limits = envelope.syntax_limits();
        let work_caps = envelope.work_caps();
        validate_common(target, context, limits, syntax_limits, work_caps)?;
        if length_claim.snapshot() != envelope.snapshot()
            || length_claim.owner() != target.reference()
            || length_claim.declaration() != envelope.declared_length()
            || matches!(
                envelope.declared_length(),
                crate::DeclaredStreamLength::Direct { value, .. }
                    if length_claim.value() != value
                        || length_claim.resolved_value_span().is_some()
            )
            || matches!(
                envelope.declared_length(),
                crate::DeclaredStreamLength::Indirect { .. }
                    if length_claim.resolved_value_span().is_none()
            )
        {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidStreamLengthClaim,
                Some(target.reference()),
                Some(envelope.declared_length().operand_span().start()),
            ));
        }
        if length_claim.value() > limits.max_stream_bytes() {
            return Err(ObjectError::resource(
                ObjectLimitKind::StreamBytes,
                limits.max_stream_bytes(),
                0,
                length_claim.value(),
                Some(target.reference()),
                Some(envelope.declared_length().operand_span().start()),
            ));
        }
        let data_end = envelope
            .data_start()
            .checked_add(length_claim.value())
            .ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InvalidStreamLength,
                    Some(target.reference()),
                    Some(envelope.declared_length().operand_span().start()),
                )
            })?;
        if data_end >= target.object_upper_bound() {
            return Err(ObjectError::for_code(
                ObjectErrorCode::ObjectCrossesPhysicalBound,
                Some(target.reference()),
                Some(target.object_upper_bound()),
            ));
        }
        let data_span =
            ByteSpan::new(envelope.data_start(), length_claim.value()).map_err(|_| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(envelope.data_start()),
                )
            })?;
        let remaining = target
            .object_upper_bound()
            .checked_sub(data_end)
            .ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(data_end),
                )
            })?;
        let source_len = target.snapshot().len().ok_or_else(|| {
            ObjectError::for_code(
                ObjectErrorCode::UnknownSourceLength,
                Some(target.reference()),
                None,
            )
        })?;
        let mut stats = envelope.stats();
        stats.declared_stream_bytes = length_claim.value();
        Ok(Self {
            target,
            source_len,
            context,
            limits,
            work_caps,
            syntax_limits,
            length_claim,
            stats,
            state: BoundaryJobState::Reading {
                envelope,
                data_span,
                window: limits.initial_boundary_bytes().min(remaining),
                charged: false,
            },
        })
    }

    /// Returns the immutable snapshot bound at job creation.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.target.snapshot()
    }

    /// Returns the accepted direct or resolver-supplied length claim.
    pub const fn length_claim(&self) -> StreamLengthClaim {
        self.length_claim
    }

    /// Returns work accounting through the most recent poll.
    pub const fn stats(&self) -> ObjectStats {
        self.stats
    }

    /// Returns the job's current coarse phase.
    pub const fn phase(&self) -> ObjectPhase {
        match self.state {
            BoundaryJobState::Reading { .. } => ObjectPhase::StreamBoundary,
            BoundaryJobState::Complete => ObjectPhase::Complete,
            BoundaryJobState::Failed(_) => ObjectPhase::Failed,
        }
    }

    /// Advances exact terminal-boundary validation without performing host I/O itself.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn ObjectCancellation + '_),
    ) -> ObjectPoll {
        let (data_end, window, charged) = match &self.state {
            BoundaryJobState::Reading {
                data_span,
                window,
                charged,
                ..
            } => (data_span.end_exclusive(), *window, *charged),
            BoundaryJobState::Complete => {
                return ObjectPoll::Failed(ObjectError::for_code(
                    ObjectErrorCode::JobAlreadyComplete,
                    Some(self.target.reference()),
                    None,
                ));
            }
            BoundaryJobState::Failed(error) => return ObjectPoll::Failed(*error),
        };
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
        let Ok(range) = ByteRange::new(data_end, window) else {
            return self.fail_internal(Some(data_end));
        };
        if range.end_exclusive() > self.target.object_upper_bound() {
            return self.fail_internal(Some(data_end));
        }
        if !charged {
            if let Err(error) = charge_read(
                &mut self.stats,
                self.work_caps,
                window,
                self.target.reference(),
                Some(data_end),
            ) {
                return self.fail(error);
            }
            let Some(attempts) = self.stats.boundary_attempts.checked_add(1) else {
                return self.fail_internal(Some(data_end));
            };
            self.stats.boundary_attempts = attempts;
            if let BoundaryJobState::Reading { charged, .. } = &mut self.state {
                *charged = true;
            }
        }
        let request = ReadRequest::new(
            range,
            self.context.priority(),
            self.context.job(),
            self.context.boundary_checkpoint(),
        );
        match source.poll(request) {
            ReadPoll::Pending { ticket, missing } => ObjectPoll::Pending {
                ticket,
                missing,
                checkpoint: self.context.boundary_checkpoint(),
            },
            ReadPoll::EndOfFile => self.fail(ObjectError::for_code(
                ObjectErrorCode::UnexpectedEndOfSource,
                Some(self.target.reference()),
                Some(data_end),
            )),
            ReadPoll::Failed(error) => self.fail(ObjectError::from_source(
                error,
                Some(self.target.reference()),
                Some(data_end),
            )),
            ReadPoll::Ready(bytes) => {
                if let Err(error) = validate_slice(&bytes, range, self.target) {
                    return self.fail(error);
                }
                if let Err(error) = charge_parse(
                    &mut self.stats,
                    self.work_caps,
                    window,
                    self.target.reference(),
                    Some(data_end),
                ) {
                    return self.fail(error);
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
                            return self.fail(ObjectError::for_code(
                                ObjectErrorCode::Cancelled,
                                Some(self.target.reference()),
                                Some(boundary.endobj_span.start()),
                            ));
                        }
                        let object_span = match object_span(self.target, boundary.endobj_span) {
                            Ok(value) => value,
                            Err(error) => return self.fail(error),
                        };
                        let state = mem::replace(&mut self.state, BoundaryJobState::Complete);
                        let BoundaryJobState::Reading {
                            envelope,
                            data_span,
                            ..
                        } = state
                        else {
                            return self.fail_internal(Some(data_end));
                        };
                        let retained_heap_bytes = envelope.retained_heap_bytes();
                        let stream = FramedStream::new(
                            envelope.dictionary,
                            self.length_claim,
                            envelope.stream_keyword_span,
                            envelope.stream_line_ending_span,
                            data_span,
                            boundary.data_delimiter_span,
                            boundary.endstream_span,
                        );
                        ObjectPoll::Ready(IndirectObject::new(
                            self.target,
                            envelope.header_span,
                            object_span,
                            boundary.endobj_span,
                            retained_heap_bytes,
                            IndirectObjectValue::Stream(stream),
                        ))
                    }
                    Ok(BoundaryParse::NeedMore { minimum_end }) => {
                        let Some(available) =
                            self.target.object_upper_bound().checked_sub(data_end)
                        else {
                            return self.fail_internal(Some(data_end));
                        };
                        match grow_or_fail(
                            self.target,
                            data_end,
                            window,
                            available,
                            self.limits.max_boundary_bytes(),
                            minimum_end,
                            ObjectLimitKind::BoundaryBytes,
                        ) {
                            Ok(next) => {
                                if let BoundaryJobState::Reading {
                                    window, charged, ..
                                } = &mut self.state
                                {
                                    *window = next;
                                    *charged = false;
                                }
                                self.poll(source, cancellation)
                            }
                            Err(error) => self.fail(error),
                        }
                    }
                    Err(error) => self.fail(error),
                }
            }
        }
    }

    fn fail(&mut self, error: ObjectError) -> ObjectPoll {
        self.state = BoundaryJobState::Failed(error);
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

impl fmt::Debug for OpenStreamBoundaryJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenStreamBoundaryJob")
            .field("target", &self.target)
            .field("source_len", &self.source_len)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("work_caps", &self.work_caps)
            .field("syntax_limits", &self.syntax_limits)
            .field("length_claim", &self.length_claim)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .finish()
    }
}

fn validate_common(
    target: IndirectObjectTarget,
    context: ObjectJobContext,
    limits: ObjectLimits,
    syntax_limits: SyntaxLimits,
    work_caps: ObjectWorkCaps,
) -> Result<(), ObjectError> {
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
    if context.envelope_checkpoint() == context.boundary_checkpoint() {
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
    Ok(())
}

fn validate_slice(
    bytes: &ByteSlice,
    expected: ByteRange,
    target: IndirectObjectTarget,
) -> Result<(), ObjectError> {
    if bytes.identity() != target.snapshot().identity() {
        return Err(ObjectError::for_code(
            ObjectErrorCode::SnapshotMismatch,
            Some(target.reference()),
            None,
        ));
    }
    if bytes.range() != expected {
        return Err(ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(target.reference()),
            Some(expected.start()),
        ));
    }
    Ok(())
}

fn charge_read(
    stats: &mut ObjectStats,
    caps: ObjectWorkCaps,
    amount: u64,
    reference: pdf_rs_syntax::ObjectRef,
    offset: Option<u64>,
) -> Result<(), ObjectError> {
    let Some(total) = stats.read_bytes.checked_add(amount) else {
        return Err(ObjectError::resource(
            ObjectLimitKind::TotalReadBytes,
            caps.max_read_bytes(),
            stats.read_bytes,
            amount,
            Some(reference),
            offset,
        ));
    };
    if total > caps.max_read_bytes() {
        return Err(ObjectError::resource(
            ObjectLimitKind::TotalReadBytes,
            caps.max_read_bytes(),
            stats.read_bytes,
            amount,
            Some(reference),
            offset,
        ));
    }
    stats.read_bytes = total;
    Ok(())
}

fn charge_parse(
    stats: &mut ObjectStats,
    caps: ObjectWorkCaps,
    amount: u64,
    reference: pdf_rs_syntax::ObjectRef,
    offset: Option<u64>,
) -> Result<(), ObjectError> {
    let Some(total) = stats.parse_bytes.checked_add(amount) else {
        return Err(ObjectError::resource(
            ObjectLimitKind::TotalParseBytes,
            caps.max_parse_bytes(),
            stats.parse_bytes,
            amount,
            Some(reference),
            offset,
        ));
    };
    if total > caps.max_parse_bytes() {
        return Err(ObjectError::resource(
            ObjectLimitKind::TotalParseBytes,
            caps.max_parse_bytes(),
            stats.parse_bytes,
            amount,
            Some(reference),
            offset,
        ));
    }
    stats.parse_bytes = total;
    Ok(())
}

fn grow_or_fail(
    target: IndirectObjectTarget,
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
            Some(target.reference()),
            Some(base),
        )
    })?;
    let cap = configured_cap.min(available);
    if current >= cap || required > cap {
        if cap == available {
            return Err(ObjectError::for_code(
                ObjectErrorCode::ObjectCrossesPhysicalBound,
                Some(target.reference()),
                Some(target.object_upper_bound()),
            ));
        }
        return Err(ObjectError::resource(
            kind,
            configured_cap,
            current,
            required.saturating_sub(current).max(1),
            Some(target.reference()),
            Some(base),
        ));
    }
    let next = current.saturating_mul(2).max(required).min(cap);
    if next <= current {
        return Err(ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(target.reference()),
            Some(base),
        ));
    }
    Ok(next)
}

fn object_span(
    target: IndirectObjectTarget,
    endobj_span: ByteSpan,
) -> Result<ByteSpan, ObjectError> {
    if endobj_span.end_exclusive() > target.object_upper_bound() {
        return Err(ObjectError::for_code(
            ObjectErrorCode::ObjectCrossesPhysicalBound,
            Some(target.reference()),
            Some(target.object_upper_bound()),
        ));
    }
    let len = endobj_span
        .end_exclusive()
        .checked_sub(target.xref_offset())
        .ok_or_else(|| {
            ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(target.reference()),
                Some(target.xref_offset()),
            )
        })?;
    ByteSpan::new(target.xref_offset(), len).map_err(|_| {
        ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(target.reference()),
            Some(target.xref_offset()),
        )
    })
}

fn is_object_header_boundary(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}
