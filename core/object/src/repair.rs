use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, ReadPoll, ReadRequest, ResumeCheckpoint,
    SmallRanges, SourceSnapshot,
};
use pdf_rs_syntax::{ByteSpan, InputExtent, ObjectRef, SyntaxLimits};

use crate::parser::{BoundaryParse, ParsedBoundary, parse_boundary};
use crate::{
    DeclaredStreamLength, FramedStream, IndirectObject, IndirectObjectTarget,
    IndirectObjectTargetKind, IndirectObjectValue, ObjectCancellation, ObjectEnvelopePoll,
    ObjectError, ObjectErrorCategory, ObjectErrorCode, ObjectJobContext, ObjectLimitKind,
    ObjectLimits, ObjectPhase, ObjectPoll, ObjectStats, ObjectWorkCaps, OpenObjectEnvelopeJob,
    OpenObjectJob, StreamEnvelope, StreamLengthClaim,
};

const HARD_MAX_OFFSET_DELTA: u64 = 4096;
const HARD_MAX_LENGTH_DELTA: u64 = 64 * 1024;
const HARD_MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_HEADER_CANDIDATES: u64 = 64;
const HARD_MAX_BOUNDARY_CANDIDATES: u64 = 64;
const HEADER_LOOKAHEAD: u64 = 64;

/// Caller-configurable ceilings for explicit local indirect-object repair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectRepairLimitConfig {
    /// Maximum absolute correction to an xref-supplied object offset.
    pub max_object_offset_delta: u64,
    /// Maximum absolute correction to one direct stream `/Length`.
    pub max_stream_length_delta: u64,
    /// Cumulative bytes read and examined only by repair phases.
    pub max_scan_bytes: u64,
    /// Matching expected object headers considered in one local scan.
    pub max_header_candidates: u64,
    /// Looks-like stream-boundary anchors considered in one local scan.
    pub max_boundary_candidates: u64,
}

impl Default for ObjectRepairLimitConfig {
    fn default() -> Self {
        Self {
            max_object_offset_delta: 32,
            max_stream_length_delta: 1024,
            max_scan_bytes: 2 * 1024 * 1024,
            max_header_candidates: 8,
            max_boundary_candidates: 8,
        }
    }
}

/// Validated local indirect-object repair ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectRepairLimits {
    max_object_offset_delta: u64,
    max_stream_length_delta: u64,
    max_scan_bytes: u64,
    max_header_candidates: u64,
    max_boundary_candidates: u64,
}

impl ObjectRepairLimits {
    /// Validates positive values beneath fixed hard ceilings.
    pub fn validate(config: ObjectRepairLimitConfig) -> Result<Self, ObjectError> {
        if config.max_object_offset_delta == 0
            || config.max_object_offset_delta > HARD_MAX_OFFSET_DELTA
            || config.max_stream_length_delta == 0
            || config.max_stream_length_delta > HARD_MAX_LENGTH_DELTA
            || config.max_scan_bytes == 0
            || config.max_scan_bytes > HARD_MAX_SCAN_BYTES
            || config.max_header_candidates == 0
            || config.max_header_candidates > HARD_MAX_HEADER_CANDIDATES
            || config.max_boundary_candidates == 0
            || config.max_boundary_candidates > HARD_MAX_BOUNDARY_CANDIDATES
        {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidRepairLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_object_offset_delta: config.max_object_offset_delta,
            max_stream_length_delta: config.max_stream_length_delta,
            max_scan_bytes: config.max_scan_bytes,
            max_header_candidates: config.max_header_candidates,
            max_boundary_candidates: config.max_boundary_candidates,
        })
    }

    /// Returns the accepted absolute object-offset correction.
    pub const fn max_object_offset_delta(self) -> u64 {
        self.max_object_offset_delta
    }

    /// Returns the accepted absolute direct stream-length correction.
    pub const fn max_stream_length_delta(self) -> u64 {
        self.max_stream_length_delta
    }

    /// Returns the cumulative repair-only scan-byte ceiling.
    pub const fn max_scan_bytes(self) -> u64 {
        self.max_scan_bytes
    }

    /// Returns the expected-header candidate ceiling.
    pub const fn max_header_candidates(self) -> u64 {
        self.max_header_candidates
    }

    /// Returns the looks-like stream-boundary anchor ceiling.
    pub const fn max_boundary_candidates(self) -> u64 {
        self.max_boundary_candidates
    }
}

impl Default for ObjectRepairLimits {
    fn default() -> Self {
        Self::validate(ObjectRepairLimitConfig::default())
            .expect("built-in object repair limits satisfy hard ceilings")
    }
}

/// Parent-lent repair-only work ceilings for one local object job.
///
/// Unlike [`ObjectRepairLimits`], these runtime caps may be zero. A zero cap
/// leaves the unchanged strict child available but rejects the corresponding
/// repair work before it can consume parent-owned aggregate budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectRepairWorkCaps {
    max_scan_bytes: u64,
    max_header_candidates: u64,
    max_boundary_candidates: u64,
}

impl ObjectRepairWorkCaps {
    /// Creates repair-only work caps beneath fixed implementation ceilings.
    pub fn new(
        max_scan_bytes: u64,
        max_header_candidates: u64,
        max_boundary_candidates: u64,
    ) -> Result<Self, ObjectError> {
        if max_scan_bytes > HARD_MAX_SCAN_BYTES
            || max_header_candidates > HARD_MAX_HEADER_CANDIDATES
            || max_boundary_candidates > HARD_MAX_BOUNDARY_CANDIDATES
        {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidRepairLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_scan_bytes,
            max_header_candidates,
            max_boundary_candidates,
        })
    }

    /// Copies the configured per-object repair ceilings as parent work caps.
    pub const fn from_limits(limits: ObjectRepairLimits) -> Self {
        Self {
            max_scan_bytes: limits.max_scan_bytes,
            max_header_candidates: limits.max_header_candidates,
            max_boundary_candidates: limits.max_boundary_candidates,
        }
    }

    /// Returns the repair-only exact-read and scan ceiling.
    pub const fn max_scan_bytes(self) -> u64 {
        self.max_scan_bytes
    }

    /// Returns the matching object-header candidate ceiling.
    pub const fn max_header_candidates(self) -> u64 {
        self.max_header_candidates
    }

    /// Returns the stream-boundary candidate ceiling.
    pub const fn max_boundary_candidates(self) -> u64 {
        self.max_boundary_candidates
    }
}

/// Strict-child checkpoints plus repair-only scan and candidate checkpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalObjectJobContext {
    strict: ObjectJobContext,
    candidate_envelope_checkpoint: ResumeCheckpoint,
    candidate_boundary_checkpoint: ResumeCheckpoint,
    header_scan_checkpoint: ResumeCheckpoint,
    length_scan_checkpoint: ResumeCheckpoint,
}

impl LocalObjectJobContext {
    /// Creates one local-repair context; job construction later validates checkpoint uniqueness.
    pub const fn new(
        strict: ObjectJobContext,
        candidate_envelope_checkpoint: ResumeCheckpoint,
        candidate_boundary_checkpoint: ResumeCheckpoint,
        header_scan_checkpoint: ResumeCheckpoint,
        length_scan_checkpoint: ResumeCheckpoint,
    ) -> Self {
        Self {
            strict,
            candidate_envelope_checkpoint,
            candidate_boundary_checkpoint,
            header_scan_checkpoint,
            length_scan_checkpoint,
        }
    }

    /// Returns the unchanged strict R0 child context.
    pub const fn strict(self) -> ObjectJobContext {
        self.strict
    }

    /// Returns the context used to normally frame a repaired candidate.
    pub const fn candidate(self) -> ObjectJobContext {
        ObjectJobContext::new(
            self.strict.job(),
            self.candidate_envelope_checkpoint,
            self.candidate_boundary_checkpoint,
            self.strict.priority(),
        )
    }

    /// Returns the bounded object-header scan checkpoint.
    pub const fn header_scan_checkpoint(self) -> ResumeCheckpoint {
        self.header_scan_checkpoint
    }

    /// Returns the bounded stream-boundary scan checkpoint.
    pub const fn length_scan_checkpoint(self) -> ResumeCheckpoint {
        self.length_scan_checkpoint
    }
}

/// Machine-readable local indirect-object repair action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectRepairKind {
    /// The xref-supplied offset was corrected to a nearby exact expected header.
    ObjectOffset,
    /// A direct `/Length` was corrected to a nearby normally parsed boundary.
    DirectStreamLength,
}

/// Source-bound, content-redacted evidence for one local object repair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectRepairDiagnostic {
    snapshot: SourceSnapshot,
    reference: ObjectRef,
    kind: ObjectRepairKind,
    declared: u64,
    effective: u64,
    scan_bytes: u64,
    candidates_examined: u64,
}

impl ObjectRepairDiagnostic {
    const fn new(
        snapshot: SourceSnapshot,
        reference: ObjectRef,
        kind: ObjectRepairKind,
        declared: u64,
        effective: u64,
        scan_bytes: u64,
        candidates_examined: u64,
    ) -> Self {
        Self {
            snapshot,
            reference,
            kind,
            declared,
            effective,
            scan_bytes,
            candidates_examined,
        }
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        match self.kind {
            ObjectRepairKind::ObjectOffset => "RPE-OBJECT-REPAIR-0001",
            ObjectRepairKind::DirectStreamLength => "RPE-OBJECT-REPAIR-0002",
        }
    }

    /// Returns the immutable snapshot that supplied all examined bytes.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the indirect object whose framing was repaired.
    pub const fn reference(self) -> ObjectRef {
        self.reference
    }

    /// Returns the repair action kind.
    pub const fn kind(self) -> ObjectRepairKind {
        self.kind
    }

    /// Returns the xref offset or direct length declared before repair.
    pub const fn declared(self) -> u64 {
        self.declared
    }

    /// Returns the normally validated offset or direct length after repair.
    pub const fn effective(self) -> u64 {
        self.effective
    }

    /// Returns repair-only source bytes examined for this decision.
    pub const fn scan_bytes(self) -> u64 {
        self.scan_bytes
    }

    /// Returns matching anchors considered by the bounded scan.
    pub const fn candidates_examined(self) -> u64 {
        self.candidates_examined
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
enum RepairDiagnostics {
    #[default]
    None,
    One(ObjectRepairDiagnostic),
    Two([ObjectRepairDiagnostic; 2]),
}

impl RepairDiagnostics {
    fn push(self, diagnostic: ObjectRepairDiagnostic) -> Result<Self, ObjectError> {
        match self {
            Self::None => Ok(Self::One(diagnostic)),
            Self::One(first) => Ok(Self::Two([first, diagnostic])),
            Self::Two(_) => Err(ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(diagnostic.reference),
                Some(diagnostic.effective),
            )),
        }
    }

    fn as_slice(&self) -> &[ObjectRepairDiagnostic] {
        match self {
            Self::None => &[],
            Self::One(diagnostic) => std::slice::from_ref(diagnostic),
            Self::Two(diagnostics) => diagnostics,
        }
    }
}

/// Strict-child and repair-only work charged by one local object job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObjectRepairStats {
    strict: ObjectStats,
    candidate: ObjectStats,
    envelope_replay: ObjectStats,
    repair_scan_bytes: u64,
    header_candidates: u64,
    boundary_candidates: u64,
}

impl ObjectRepairStats {
    /// Returns work charged by the unchanged strict child.
    pub const fn strict(self) -> ObjectStats {
        self.strict
    }

    /// Returns work charged while normally framing a corrected header candidate.
    pub const fn candidate(self) -> ObjectStats {
        self.candidate
    }

    /// Returns work charged while replaying the envelope before length repair.
    pub const fn envelope_replay(self) -> ObjectStats {
        self.envelope_replay
    }

    /// Returns cumulative bytes read and examined only by repair scans.
    pub const fn repair_scan_bytes(self) -> u64 {
        self.repair_scan_bytes
    }

    /// Returns matching expected object headers considered.
    pub const fn header_candidates(self) -> u64 {
        self.header_candidates
    }

    /// Returns looks-like stream-boundary anchors considered.
    pub const fn boundary_candidates(self) -> u64 {
        self.boundary_candidates
    }

    /// Returns cumulative exact source bytes charged by validation children and repair scans.
    pub const fn read_bytes(self) -> u64 {
        self.strict.read_bytes()
            + self.candidate.read_bytes()
            + self.envelope_replay.read_bytes()
            + self.repair_scan_bytes
    }

    /// Returns cumulative parser-window bytes charged by every validation child.
    pub const fn parse_bytes(self) -> u64 {
        self.strict.parse_bytes()
            + self.candidate.parse_bytes()
            + self.envelope_replay.parse_bytes()
    }
}

/// Proof-bearing strict or locally repaired indirect object.
pub struct LocallyFramedObject {
    object: IndirectObject,
    declared_xref_offset: u64,
    diagnostics: RepairDiagnostics,
    stats: ObjectRepairStats,
}

impl LocallyFramedObject {
    fn new(
        object: IndirectObject,
        declared_xref_offset: u64,
        diagnostics: RepairDiagnostics,
        stats: ObjectRepairStats,
    ) -> Self {
        Self {
            object,
            declared_xref_offset,
            diagnostics,
            stats,
        }
    }

    /// Returns the complete immutable source snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.object.snapshot()
    }

    /// Returns the validated indirect-object reference.
    pub const fn reference(&self) -> ObjectRef {
        self.object.reference()
    }

    /// Returns the xref-supplied offset before local repair.
    pub const fn declared_xref_offset(&self) -> u64 {
        self.declared_xref_offset
    }

    /// Returns the normally validated object-header offset.
    pub const fn effective_xref_offset(&self) -> u64 {
        self.object.xref_offset()
    }

    /// Returns the exact normally validated object span.
    pub const fn object_span(&self) -> ByteSpan {
        self.object.object_span()
    }

    /// Returns the exact normally validated object-header span.
    pub const fn header_span(&self) -> ByteSpan {
        self.object.header_span()
    }

    /// Returns the exact terminal `endobj` span.
    pub const fn endobj_span(&self) -> ByteSpan {
        self.object.endobj_span()
    }

    /// Returns the exclusive physical bound used during normal validation.
    pub const fn object_upper_bound(&self) -> u64 {
        self.object.object_upper_bound()
    }

    /// Returns the revision anchor retained by the normally validated object.
    pub const fn revision_startxref(&self) -> u64 {
        self.object.revision_startxref()
    }

    /// Returns the normally validated direct or stream value.
    pub const fn value(&self) -> &IndirectObjectValue {
        self.object.value()
    }

    /// Returns allocator-reported syntax capacity retained by the object.
    pub const fn retained_heap_bytes(&self) -> u64 {
        self.object.retained_heap_bytes()
    }

    /// Returns inseparable source-bound repair diagnostics.
    pub fn diagnostics(&self) -> &[ObjectRepairDiagnostic] {
        self.diagnostics.as_slice()
    }

    /// Returns strict-child and repair-only work charged before publication.
    pub const fn stats(&self) -> ObjectRepairStats {
        self.stats
    }
}

impl fmt::Debug for LocallyFramedObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocallyFramedObject")
            .field("snapshot", &self.object.snapshot())
            .field("reference", &self.object.reference())
            .field("declared_xref_offset", &self.declared_xref_offset)
            .field("effective_xref_offset", &self.object.xref_offset())
            .field("object_span", &self.object.object_span())
            .field("diagnostics", &self.diagnostics)
            .field("stats", &self.stats)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// Coarse phase of an explicit local indirect-object repair job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalObjectPhase {
    /// Running the unchanged strict R0 child.
    Strict(ObjectPhase),
    /// Scanning for a nearby exact expected object header.
    HeaderScan,
    /// Normally framing the unique corrected header candidate.
    Candidate,
    /// Replaying the validated stream envelope before length repair.
    EnvelopeReplay,
    /// Scanning for a nearby strict stream boundary.
    LengthScan,
    /// The proof-bearing object was returned.
    Complete,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Poll result for one explicit local indirect-object repair job.
#[allow(
    clippy::large_enum_variant,
    reason = "proof-bearing one-shot values stay inline without unbudgeted allocation"
)]
#[derive(Debug)]
pub enum LocalObjectPoll {
    /// A strict or locally repaired object is ready with inseparable evidence.
    Ready(LocallyFramedObject),
    /// Required source bytes are absent.
    Pending {
        /// One-shot source ticket.
        ticket: DataTicket,
        /// Canonical exact missing ranges.
        missing: SmallRanges,
        /// Exact strict or repair checkpoint to requeue.
        checkpoint: ResumeCheckpoint,
    },
    /// Strict framing or bounded local repair reached terminal failure.
    Failed(ObjectError),
}

#[allow(
    clippy::large_enum_variant,
    reason = "one-shot strict jobs and proof-bearing envelopes remain inline and allocation-auditable"
)]
enum RepairState {
    Strict(OpenObjectJob),
    HeaderScan {
        range: ByteRange,
        charged: bool,
    },
    Candidate {
        target: IndirectObjectTarget,
        diagnostics: RepairDiagnostics,
        job: OpenObjectJob,
    },
    EnvelopeReplay {
        target: IndirectObjectTarget,
        diagnostics: RepairDiagnostics,
        lower_error: ObjectError,
        job: OpenObjectEnvelopeJob,
    },
    LengthScan {
        envelope: StreamEnvelope,
        diagnostics: RepairDiagnostics,
        lower_error: ObjectError,
        declared_end: u64,
        range: ByteRange,
        charged: bool,
    },
    Transition,
    Complete,
    Failed(ObjectError),
}

/// Strict-first R1 sibling for slight object-offset and direct-length repair.
pub struct OpenLocalObjectJob {
    declared_target: IndirectObjectTarget,
    context: LocalObjectJobContext,
    object_limits: ObjectLimits,
    repair_limits: ObjectRepairLimits,
    work_caps: ObjectWorkCaps,
    repair_work_caps: ObjectRepairWorkCaps,
    syntax_limits: SyntaxLimits,
    stats: ObjectRepairStats,
    state: RepairState,
}

impl OpenLocalObjectJob {
    /// Validates checkpoints and starts the unchanged strict child.
    pub fn new(
        target: IndirectObjectTarget,
        context: LocalObjectJobContext,
        object_limits: ObjectLimits,
        repair_limits: ObjectRepairLimits,
        syntax_limits: SyntaxLimits,
    ) -> Result<Self, ObjectError> {
        Self::new_with_parent_caps(
            target,
            context,
            object_limits,
            repair_limits,
            syntax_limits,
            ObjectWorkCaps::from_limits(object_limits),
            ObjectRepairWorkCaps::from_limits(repair_limits),
        )
    }

    /// Starts local repair beneath parent-supplied aggregate validation and scan work caps.
    ///
    /// Repair-only exact reads share the read cap with strict, candidate, and envelope-replay
    /// children. Parse work is the sum of those validation children. The caps may be smaller
    /// than the configured object totals but cannot exceed them.
    pub fn new_with_work_caps(
        target: IndirectObjectTarget,
        context: LocalObjectJobContext,
        object_limits: ObjectLimits,
        repair_limits: ObjectRepairLimits,
        syntax_limits: SyntaxLimits,
        work_caps: ObjectWorkCaps,
    ) -> Result<Self, ObjectError> {
        Self::new_with_parent_caps(
            target,
            context,
            object_limits,
            repair_limits,
            syntax_limits,
            work_caps,
            ObjectRepairWorkCaps::from_limits(repair_limits),
        )
    }

    /// Starts local repair beneath parent-supplied validation and repair-only work caps.
    ///
    /// Validation read/parse caps and repair scan/candidate caps are independent. Repair-only
    /// caps may be zero so a strict-valid object remains usable after a parent aggregate repair
    /// budget is exhausted.
    #[allow(
        clippy::too_many_arguments,
        reason = "the public parent composition boundary keeps every validated profile explicit"
    )]
    pub fn new_with_parent_caps(
        target: IndirectObjectTarget,
        context: LocalObjectJobContext,
        object_limits: ObjectLimits,
        repair_limits: ObjectRepairLimits,
        syntax_limits: SyntaxLimits,
        work_caps: ObjectWorkCaps,
        repair_work_caps: ObjectRepairWorkCaps,
    ) -> Result<Self, ObjectError> {
        if target.kind() != IndirectObjectTargetKind::XrefEntry {
            return Err(ObjectError::for_code(
                ObjectErrorCode::UnsupportedRepairTarget,
                Some(target.reference()),
                Some(target.xref_offset()),
            ));
        }
        let checkpoints = [
            context.strict().envelope_checkpoint(),
            context.strict().boundary_checkpoint(),
            context.candidate().envelope_checkpoint(),
            context.candidate().boundary_checkpoint(),
            context.header_scan_checkpoint(),
            context.length_scan_checkpoint(),
        ];
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            if checkpoints[..index].contains(checkpoint) {
                return Err(ObjectError::for_code(
                    ObjectErrorCode::InvalidRepairJobContext,
                    Some(target.reference()),
                    None,
                ));
            }
        }
        if work_caps.max_read_bytes() > object_limits.max_total_read_bytes()
            || work_caps.max_parse_bytes() > object_limits.max_total_parse_bytes()
            || repair_work_caps.max_scan_bytes() > repair_limits.max_scan_bytes()
            || repair_work_caps.max_header_candidates() > repair_limits.max_header_candidates()
            || repair_work_caps.max_boundary_candidates() > repair_limits.max_boundary_candidates()
        {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidLimits,
                Some(target.reference()),
                None,
            ));
        }
        let strict = OpenObjectJob::new_with_work_caps(
            target,
            context.strict(),
            object_limits,
            syntax_limits,
            work_caps,
        )?;
        Ok(Self {
            declared_target: target,
            context,
            object_limits,
            repair_limits,
            work_caps,
            repair_work_caps,
            syntax_limits,
            stats: ObjectRepairStats::default(),
            state: RepairState::Strict(strict),
        })
    }

    /// Returns the immutable source snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.declared_target.snapshot()
    }

    /// Returns the original xref-derived target.
    pub const fn declared_target(&self) -> IndirectObjectTarget {
        self.declared_target
    }

    /// Returns strict and repair checkpoint identity.
    pub const fn context(&self) -> LocalObjectJobContext {
        self.context
    }

    /// Returns the validated local-repair ceilings.
    pub const fn repair_limits(&self) -> ObjectRepairLimits {
        self.repair_limits
    }

    /// Returns the validated per-object framing limits inherited by every validation child.
    pub const fn object_limits(&self) -> ObjectLimits {
        self.object_limits
    }

    /// Returns the parent-supplied aggregate read and parse caps for this local job.
    pub const fn work_caps(&self) -> ObjectWorkCaps {
        self.work_caps
    }

    /// Returns the parent-lent repair-only scan and candidate caps.
    pub const fn repair_work_caps(&self) -> ObjectRepairWorkCaps {
        self.repair_work_caps
    }

    /// Returns strict-child and repair-only work charged so far.
    pub const fn stats(&self) -> ObjectRepairStats {
        self.stats
    }

    /// Returns the current coarse phase.
    pub const fn phase(&self) -> LocalObjectPhase {
        match &self.state {
            RepairState::Strict(job) => LocalObjectPhase::Strict(job.phase()),
            RepairState::HeaderScan { .. } => LocalObjectPhase::HeaderScan,
            RepairState::Candidate { .. } => LocalObjectPhase::Candidate,
            RepairState::EnvelopeReplay { .. } => LocalObjectPhase::EnvelopeReplay,
            RepairState::LengthScan { .. } => LocalObjectPhase::LengthScan,
            RepairState::Complete => LocalObjectPhase::Complete,
            RepairState::Failed(_) | RepairState::Transition => LocalObjectPhase::Failed,
        }
    }

    /// Advances strict framing or explicit bounded repair without host I/O.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn ObjectCancellation + '_),
    ) -> LocalObjectPoll {
        loop {
            let state = mem::replace(&mut self.state, RepairState::Transition);
            match state {
                RepairState::Strict(mut job) => match job.poll(source, cancellation) {
                    ObjectPoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    } => {
                        self.stats.strict = job.stats();
                        self.state = RepairState::Strict(job);
                        return LocalObjectPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        };
                    }
                    ObjectPoll::Ready(object) => {
                        self.stats.strict = job.stats();
                        return self.ready(object, RepairDiagnostics::None);
                    }
                    ObjectPoll::Failed(error) => {
                        self.stats.strict = job.stats();
                        match error.code() {
                            ObjectErrorCode::InvalidObjectHeader => {
                                let range = match header_scan_range(
                                    self.declared_target,
                                    self.repair_limits.max_object_offset_delta,
                                ) {
                                    Ok(range) => range,
                                    Err(error) => return self.fail(error),
                                };
                                self.state = RepairState::HeaderScan {
                                    range,
                                    charged: false,
                                };
                            }
                            ObjectErrorCode::InvalidStreamBoundary
                            | ObjectErrorCode::ObjectCrossesPhysicalBound => {
                                if let Err(error) = self.begin_envelope_replay(
                                    self.declared_target,
                                    RepairDiagnostics::None,
                                    error,
                                ) {
                                    return self.fail(error);
                                }
                            }
                            _ => return self.fail(error),
                        }
                    }
                },
                RepairState::HeaderScan { range, charged } => {
                    if source.snapshot() != self.snapshot() {
                        return self.fail(ObjectError::for_code(
                            ObjectErrorCode::SnapshotMismatch,
                            Some(self.declared_target.reference()),
                            None,
                        ));
                    }
                    if cancellation.is_cancelled() {
                        return self.fail(ObjectError::for_code(
                            ObjectErrorCode::Cancelled,
                            Some(self.declared_target.reference()),
                            None,
                        ));
                    }
                    if !charged
                        && let Err(error) = self.charge_scan(range.len(), Some(range.start()))
                    {
                        return self.fail(error);
                    }
                    let request = ReadRequest::new(
                        range,
                        self.context.strict().priority(),
                        self.context.strict().job(),
                        self.context.header_scan_checkpoint(),
                    );
                    match source.poll(request) {
                        ReadPoll::Pending { ticket, missing } => {
                            self.state = RepairState::HeaderScan {
                                range,
                                charged: true,
                            };
                            return LocalObjectPoll::Pending {
                                ticket,
                                missing,
                                checkpoint: self.context.header_scan_checkpoint(),
                            };
                        }
                        ReadPoll::EndOfFile => {
                            return self.fail(ObjectError::for_code(
                                ObjectErrorCode::UnexpectedEndOfSource,
                                Some(self.declared_target.reference()),
                                Some(range.start()),
                            ));
                        }
                        ReadPoll::Failed(error) => {
                            return self.fail(ObjectError::from_source(
                                error,
                                Some(self.declared_target.reference()),
                                Some(range.start()),
                            ));
                        }
                        ReadPoll::Ready(bytes) => {
                            if let Err(error) = validate_slice(&bytes, range, self.declared_target)
                            {
                                return self.fail(error);
                            }
                            let (effective, candidates) = match scan_expected_headers(
                                &bytes,
                                self.declared_target,
                                self.repair_limits,
                                self.repair_work_caps.max_header_candidates,
                                cancellation,
                            ) {
                                Ok(result) => result,
                                Err(error) => return self.fail(error),
                            };
                            self.stats.header_candidates = candidates;
                            let Some(effective) = effective else {
                                return self.fail_local(Some(self.declared_target.xref_offset()));
                            };
                            let target = match IndirectObjectTarget::new(
                                self.snapshot(),
                                self.declared_target.reference(),
                                effective,
                                self.declared_target.object_upper_bound(),
                                self.declared_target.revision_startxref(),
                            ) {
                                Ok(target) => target,
                                Err(error) => return self.fail(error),
                            };
                            let diagnostic = ObjectRepairDiagnostic::new(
                                self.snapshot(),
                                target.reference(),
                                ObjectRepairKind::ObjectOffset,
                                self.declared_target.xref_offset(),
                                effective,
                                range.len(),
                                candidates,
                            );
                            let diagnostics = RepairDiagnostics::One(diagnostic);
                            let validation_caps = match self.remaining_validation_caps(target) {
                                Ok(caps) => caps,
                                Err(error) => return self.fail(error),
                            };
                            let job = match OpenObjectJob::new_with_work_caps(
                                target,
                                self.context.candidate(),
                                self.object_limits,
                                self.syntax_limits,
                                validation_caps,
                            ) {
                                Ok(job) => job,
                                Err(error) => return self.fail(error),
                            };
                            self.state = RepairState::Candidate {
                                target,
                                diagnostics,
                                job,
                            };
                        }
                    }
                }
                RepairState::Candidate {
                    target,
                    diagnostics,
                    mut job,
                } => match job.poll(source, cancellation) {
                    ObjectPoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    } => {
                        self.stats.candidate = job.stats();
                        self.state = RepairState::Candidate {
                            target,
                            diagnostics,
                            job,
                        };
                        return LocalObjectPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        };
                    }
                    ObjectPoll::Ready(object) => {
                        self.stats.candidate = job.stats();
                        return self.ready(object, diagnostics);
                    }
                    ObjectPoll::Failed(error) => {
                        self.stats.candidate = job.stats();
                        if repairable_length_error(error) {
                            if let Err(error) =
                                self.begin_envelope_replay(target, diagnostics, error)
                            {
                                return self.fail(error);
                            }
                        } else if error.category() == ObjectErrorCategory::Syntax {
                            return self.fail_local(error.offset());
                        } else {
                            return self.fail(error);
                        }
                    }
                },
                RepairState::EnvelopeReplay {
                    target,
                    diagnostics,
                    lower_error,
                    mut job,
                } => match job.poll(source, cancellation) {
                    ObjectEnvelopePoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    } => {
                        self.stats.envelope_replay = job.stats();
                        self.state = RepairState::EnvelopeReplay {
                            target,
                            diagnostics,
                            lower_error,
                            job,
                        };
                        return LocalObjectPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        };
                    }
                    ObjectEnvelopePoll::Direct(_) => return self.fail(lower_error),
                    ObjectEnvelopePoll::Stream(envelope) => {
                        self.stats.envelope_replay = envelope.stats();
                        let declared = match envelope.declared_length() {
                            DeclaredStreamLength::Direct { value, .. } => value,
                            DeclaredStreamLength::Indirect { .. } => return self.fail(lower_error),
                        };
                        let declared_end = match envelope.data_start().checked_add(declared) {
                            Some(value) => value,
                            None => return self.fail(lower_error),
                        };
                        let range =
                            match length_scan_range(&envelope, declared_end, self.repair_limits) {
                                Ok(Some(range)) => range,
                                Ok(None) => return self.fail_local(Some(declared_end)),
                                Err(error) => return self.fail(error),
                            };
                        self.state = RepairState::LengthScan {
                            envelope,
                            diagnostics,
                            lower_error,
                            declared_end,
                            range,
                            charged: false,
                        };
                    }
                    ObjectEnvelopePoll::Failed(error) => {
                        self.stats.envelope_replay = job.stats();
                        if error.category() == ObjectErrorCategory::Syntax {
                            return self.fail_local(error.offset());
                        }
                        return self.fail(error);
                    }
                },
                RepairState::LengthScan {
                    envelope,
                    diagnostics,
                    lower_error,
                    declared_end,
                    range,
                    charged,
                } => {
                    if source.snapshot() != self.snapshot() {
                        return self.fail(ObjectError::for_code(
                            ObjectErrorCode::SnapshotMismatch,
                            Some(self.declared_target.reference()),
                            None,
                        ));
                    }
                    if cancellation.is_cancelled() {
                        return self.fail(ObjectError::for_code(
                            ObjectErrorCode::Cancelled,
                            Some(self.declared_target.reference()),
                            None,
                        ));
                    }
                    if !charged
                        && let Err(error) = self.charge_scan(range.len(), Some(range.start()))
                    {
                        return self.fail(error);
                    }
                    let request = ReadRequest::new(
                        range,
                        self.context.strict().priority(),
                        self.context.strict().job(),
                        self.context.length_scan_checkpoint(),
                    );
                    match source.poll(request) {
                        ReadPoll::Pending { ticket, missing } => {
                            self.state = RepairState::LengthScan {
                                envelope,
                                diagnostics,
                                lower_error,
                                declared_end,
                                range,
                                charged: true,
                            };
                            return LocalObjectPoll::Pending {
                                ticket,
                                missing,
                                checkpoint: self.context.length_scan_checkpoint(),
                            };
                        }
                        ReadPoll::EndOfFile => {
                            return self.fail(ObjectError::for_code(
                                ObjectErrorCode::UnexpectedEndOfSource,
                                Some(self.declared_target.reference()),
                                Some(range.start()),
                            ));
                        }
                        ReadPoll::Failed(error) => {
                            return self.fail(ObjectError::from_source(
                                error,
                                Some(self.declared_target.reference()),
                                Some(range.start()),
                            ));
                        }
                        ReadPoll::Ready(bytes) => {
                            if let Err(error) = validate_slice(&bytes, range, envelope.target()) {
                                return self.fail(error);
                            }
                            let candidate = match scan_stream_boundaries(
                                &bytes,
                                &envelope,
                                declared_end,
                                self.repair_limits,
                                self.repair_work_caps.max_boundary_candidates,
                                cancellation,
                            ) {
                                Ok(candidate) => candidate,
                                Err(error) => return self.fail(error),
                            };
                            self.stats.boundary_candidates = candidate.count;
                            let Some(boundary) = candidate.selected else {
                                return self.fail_local(Some(declared_end));
                            };
                            let diagnostic = ObjectRepairDiagnostic::new(
                                self.snapshot(),
                                envelope.target().reference(),
                                ObjectRepairKind::DirectStreamLength,
                                declared_end - envelope.data_start(),
                                boundary.effective_length,
                                range.len(),
                                candidate.count,
                            );
                            let diagnostics = match diagnostics.push(diagnostic) {
                                Ok(diagnostics) => diagnostics,
                                Err(error) => return self.fail(error),
                            };
                            let object = match repaired_stream_object(envelope, boundary) {
                                Ok(object) => object,
                                Err(error) => return self.fail(error),
                            };
                            return self.ready(object, diagnostics);
                        }
                    }
                }
                RepairState::Complete => {
                    return self.fail(ObjectError::for_code(
                        ObjectErrorCode::JobAlreadyComplete,
                        Some(self.declared_target.reference()),
                        None,
                    ));
                }
                RepairState::Failed(error) => return LocalObjectPoll::Failed(error),
                RepairState::Transition => {
                    return self.fail(ObjectError::for_code(
                        ObjectErrorCode::InternalState,
                        Some(self.declared_target.reference()),
                        None,
                    ));
                }
            }
        }
    }

    fn begin_envelope_replay(
        &mut self,
        target: IndirectObjectTarget,
        diagnostics: RepairDiagnostics,
        lower_error: ObjectError,
    ) -> Result<(), ObjectError> {
        let validation_caps = self.remaining_validation_caps(target)?;
        let job = OpenObjectEnvelopeJob::new_with_work_caps(
            target,
            self.context.candidate(),
            self.object_limits,
            self.syntax_limits,
            validation_caps,
        )?;
        self.state = RepairState::EnvelopeReplay {
            target,
            diagnostics,
            lower_error,
            job,
        };
        Ok(())
    }

    fn remaining_validation_caps(
        &self,
        target: IndirectObjectTarget,
    ) -> Result<ObjectWorkCaps, ObjectError> {
        let max_read = self.work_caps.max_read_bytes();
        let max_parse = self.work_caps.max_parse_bytes();
        let consumed_read = self
            .stats
            .strict
            .read_bytes()
            .checked_add(self.stats.candidate.read_bytes())
            .and_then(|value| value.checked_add(self.stats.envelope_replay.read_bytes()))
            .and_then(|value| value.checked_add(self.stats.repair_scan_bytes))
            .ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(target.xref_offset()),
                )
            })?;
        let consumed_parse = self
            .stats
            .strict
            .parse_bytes()
            .checked_add(self.stats.candidate.parse_bytes())
            .and_then(|value| value.checked_add(self.stats.envelope_replay.parse_bytes()))
            .ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(target.xref_offset()),
                )
            })?;
        let remaining_read = max_read.checked_sub(consumed_read).ok_or_else(|| {
            ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(target.reference()),
                Some(target.xref_offset()),
            )
        })?;
        let remaining_parse = max_parse.checked_sub(consumed_parse).ok_or_else(|| {
            ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(target.reference()),
                Some(target.xref_offset()),
            )
        })?;
        if remaining_read == 0 {
            return Err(ObjectError::resource(
                ObjectLimitKind::TotalReadBytes,
                max_read,
                consumed_read,
                1,
                Some(target.reference()),
                Some(target.xref_offset()),
            ));
        }
        if remaining_parse == 0 {
            return Err(ObjectError::resource(
                ObjectLimitKind::TotalParseBytes,
                max_parse,
                consumed_parse,
                1,
                Some(target.reference()),
                Some(target.xref_offset()),
            ));
        }
        ObjectWorkCaps::new(remaining_read, remaining_parse)
    }

    fn charge_scan(&mut self, amount: u64, offset: Option<u64>) -> Result<(), ObjectError> {
        let Some(total) = self.stats.repair_scan_bytes.checked_add(amount) else {
            return Err(ObjectError::resource(
                ObjectLimitKind::RepairScanBytes,
                self.repair_work_caps.max_scan_bytes,
                self.stats.repair_scan_bytes,
                amount,
                Some(self.declared_target.reference()),
                offset,
            ));
        };
        if total > self.repair_work_caps.max_scan_bytes {
            return Err(ObjectError::resource(
                ObjectLimitKind::RepairScanBytes,
                self.repair_work_caps.max_scan_bytes,
                self.stats.repair_scan_bytes,
                amount,
                Some(self.declared_target.reference()),
                offset,
            ));
        }
        let validation_read = self
            .stats
            .strict
            .read_bytes()
            .checked_add(self.stats.candidate.read_bytes())
            .and_then(|value| value.checked_add(self.stats.envelope_replay.read_bytes()))
            .ok_or_else(|| {
                ObjectError::resource(
                    ObjectLimitKind::TotalReadBytes,
                    self.work_caps.max_read_bytes(),
                    self.work_caps.max_read_bytes(),
                    amount,
                    Some(self.declared_target.reference()),
                    offset,
                )
            })?;
        let aggregate = validation_read.checked_add(total).ok_or_else(|| {
            ObjectError::resource(
                ObjectLimitKind::TotalReadBytes,
                self.work_caps.max_read_bytes(),
                validation_read.saturating_add(self.stats.repair_scan_bytes),
                amount,
                Some(self.declared_target.reference()),
                offset,
            )
        })?;
        if aggregate > self.work_caps.max_read_bytes() {
            return Err(ObjectError::resource(
                ObjectLimitKind::TotalReadBytes,
                self.work_caps.max_read_bytes(),
                validation_read + self.stats.repair_scan_bytes,
                amount,
                Some(self.declared_target.reference()),
                offset,
            ));
        }
        self.stats.repair_scan_bytes = total;
        Ok(())
    }

    fn ready(&mut self, object: IndirectObject, diagnostics: RepairDiagnostics) -> LocalObjectPoll {
        self.state = RepairState::Complete;
        LocalObjectPoll::Ready(LocallyFramedObject::new(
            object,
            self.declared_target.xref_offset(),
            diagnostics,
            self.stats,
        ))
    }

    fn fail_local(&mut self, offset: Option<u64>) -> LocalObjectPoll {
        self.fail(ObjectError::for_code(
            ObjectErrorCode::LocalRepairFailed,
            Some(self.declared_target.reference()),
            offset,
        ))
    }

    fn fail(&mut self, error: ObjectError) -> LocalObjectPoll {
        self.state = RepairState::Failed(error);
        LocalObjectPoll::Failed(error)
    }
}

impl fmt::Debug for OpenLocalObjectJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenLocalObjectJob")
            .field("declared_target", &self.declared_target)
            .field("context", &self.context)
            .field("object_limits", &self.object_limits)
            .field("repair_limits", &self.repair_limits)
            .field("work_caps", &self.work_caps)
            .field("repair_work_caps", &self.repair_work_caps)
            .field("syntax_limits", &self.syntax_limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .field("state_payload", &"[REDACTED]")
            .finish()
    }
}

struct BoundaryCandidate {
    effective_length: u64,
    data_delimiter_span: ByteSpan,
    endstream_span: ByteSpan,
    endobj_span: ByteSpan,
}

struct BoundarySelection {
    selected: Option<BoundaryCandidate>,
    count: u64,
}

fn repairable_length_error(error: ObjectError) -> bool {
    matches!(
        error.code(),
        ObjectErrorCode::InvalidStreamBoundary | ObjectErrorCode::ObjectCrossesPhysicalBound
    )
}

fn header_scan_range(target: IndirectObjectTarget, delta: u64) -> Result<ByteRange, ObjectError> {
    let lower = target.xref_offset().saturating_sub(delta);
    let upper = target
        .xref_offset()
        .saturating_add(delta)
        .min(target.object_upper_bound().saturating_sub(1));
    let start = lower.saturating_sub(1);
    let end = upper
        .saturating_add(HEADER_LOOKAHEAD)
        .min(target.object_upper_bound());
    let len = end.checked_sub(start).ok_or_else(|| {
        ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(target.reference()),
            Some(target.xref_offset()),
        )
    })?;
    ByteRange::new(start, len).map_err(|_| {
        ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(target.reference()),
            Some(start),
        )
    })
}

fn length_scan_range(
    envelope: &StreamEnvelope,
    declared_end: u64,
    limits: ObjectRepairLimits,
) -> Result<Option<ByteRange>, ObjectError> {
    let target = envelope.target();
    let lower = declared_end
        .saturating_sub(limits.max_stream_length_delta)
        .max(envelope.data_start());
    let upper = declared_end
        .saturating_add(limits.max_stream_length_delta)
        .min(target.object_upper_bound().saturating_sub(1));
    if lower > upper {
        return Ok(None);
    }
    let end = upper
        .saturating_add(envelope.limits().max_boundary_bytes())
        .min(target.object_upper_bound());
    let len = end.checked_sub(lower).ok_or_else(|| {
        ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(target.reference()),
            Some(lower),
        )
    })?;
    if len == 0 {
        return Ok(None);
    }
    ByteRange::new(lower, len).map(Some).map_err(|_| {
        ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(target.reference()),
            Some(lower),
        )
    })
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

fn scan_expected_headers(
    bytes: &ByteSlice,
    target: IndirectObjectTarget,
    limits: ObjectRepairLimits,
    max_candidates: u64,
    cancellation: &dyn ObjectCancellation,
) -> Result<(Option<u64>, u64), ObjectError> {
    let lower = target
        .xref_offset()
        .saturating_sub(limits.max_object_offset_delta);
    let upper = target
        .xref_offset()
        .saturating_add(limits.max_object_offset_delta)
        .min(target.object_upper_bound().saturating_sub(1));
    let raw = bytes.bytes();
    let mut selected = None;
    let mut count = 0_u64;
    for position in 0..raw.len() {
        if position.is_multiple_of(256) && cancellation.is_cancelled() {
            return Err(ObjectError::for_code(
                ObjectErrorCode::Cancelled,
                Some(target.reference()),
                None,
            ));
        }
        let absolute = bytes
            .range()
            .start()
            .checked_add(u64::try_from(position).map_err(|_| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(target.xref_offset()),
                )
            })?)
            .ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(target.xref_offset()),
                )
            })?;
        if absolute < lower || absolute > upper || absolute == target.xref_offset() {
            continue;
        }
        let predecessor_ok = if absolute == 0 {
            true
        } else {
            position
                .checked_sub(1)
                .and_then(|index| raw.get(index))
                .is_some_and(|byte| is_pdf_whitespace(*byte))
        };
        if !predecessor_ok || !matches_expected_header(&raw[position..], target.reference()) {
            continue;
        }
        count = count.checked_add(1).ok_or_else(|| {
            ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(target.reference()),
                Some(absolute),
            )
        })?;
        if count > max_candidates {
            return Err(ObjectError::resource(
                ObjectLimitKind::RepairHeaderCandidates,
                max_candidates,
                count - 1,
                1,
                Some(target.reference()),
                Some(absolute),
            ));
        }
        if selected.is_some() {
            return Err(ObjectError::for_code(
                ObjectErrorCode::AmbiguousRepair,
                Some(target.reference()),
                Some(target.xref_offset()),
            ));
        }
        selected = Some(absolute);
    }
    if cancellation.is_cancelled() {
        return Err(ObjectError::for_code(
            ObjectErrorCode::Cancelled,
            Some(target.reference()),
            None,
        ));
    }
    Ok((selected, count))
}

fn matches_expected_header(bytes: &[u8], expected: ObjectRef) -> bool {
    let mut position = 0;
    let Some(number) = parse_unsigned(bytes, &mut position) else {
        return false;
    };
    if number != u64::from(expected.number()) || !consume_pdf_whitespace(bytes, &mut position) {
        return false;
    }
    let Some(generation) = parse_unsigned(bytes, &mut position) else {
        return false;
    };
    if generation != u64::from(expected.generation())
        || !consume_pdf_whitespace(bytes, &mut position)
    {
        return false;
    }
    bytes.get(position..position.saturating_add(3)) == Some(b"obj")
        && bytes
            .get(position.saturating_add(3))
            .is_some_and(|byte| is_pdf_whitespace(*byte))
}

fn parse_unsigned(bytes: &[u8], position: &mut usize) -> Option<u64> {
    let start = *position;
    let mut value = 0_u64;
    while bytes.get(*position).is_some_and(u8::is_ascii_digit) {
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(bytes[*position] - b'0'))?;
        *position += 1;
    }
    (*position != start).then_some(value)
}

fn consume_pdf_whitespace(bytes: &[u8], position: &mut usize) -> bool {
    let start = *position;
    while bytes
        .get(*position)
        .is_some_and(|byte| is_pdf_whitespace(*byte))
    {
        *position += 1;
    }
    *position != start
}

fn scan_stream_boundaries(
    bytes: &ByteSlice,
    envelope: &StreamEnvelope,
    declared_end: u64,
    limits: ObjectRepairLimits,
    max_candidates: u64,
    cancellation: &dyn ObjectCancellation,
) -> Result<BoundarySelection, ObjectError> {
    let target = envelope.target();
    let lower = declared_end
        .saturating_sub(limits.max_stream_length_delta)
        .max(envelope.data_start());
    let upper = declared_end
        .saturating_add(limits.max_stream_length_delta)
        .min(target.object_upper_bound().saturating_sub(1));
    let raw = bytes.bytes();
    let mut selected = None;
    let mut count = 0_u64;
    for position in 0..raw.len() {
        if position.is_multiple_of(256) && cancellation.is_cancelled() {
            return Err(ObjectError::for_code(
                ObjectErrorCode::Cancelled,
                Some(target.reference()),
                None,
            ));
        }
        let absolute = bytes
            .range()
            .start()
            .checked_add(u64::try_from(position).map_err(|_| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(declared_end),
                )
            })?)
            .ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(declared_end),
                )
            })?;
        if absolute < lower || absolute > upper || absolute == declared_end {
            continue;
        }
        if !looks_like_stream_boundary(raw, position) {
            continue;
        }
        count = count.checked_add(1).ok_or_else(|| {
            ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(target.reference()),
                Some(absolute),
            )
        })?;
        if count > max_candidates {
            return Err(ObjectError::resource(
                ObjectLimitKind::RepairBoundaryCandidates,
                max_candidates,
                count - 1,
                1,
                Some(target.reference()),
                Some(absolute),
            ));
        }
        let effective_length = absolute.checked_sub(envelope.data_start()).ok_or_else(|| {
            ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(target.reference()),
                Some(absolute),
            )
        })?;
        if effective_length > envelope.limits().max_stream_bytes() {
            continue;
        }
        let max_candidate_bytes =
            usize::try_from(envelope.limits().max_boundary_bytes()).map_err(|_| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(absolute),
                )
            })?;
        let candidate_end = position
            .checked_add(max_candidate_bytes)
            .unwrap_or(raw.len())
            .min(raw.len());
        let candidate_end_absolute = bytes
            .range()
            .start()
            .checked_add(u64::try_from(candidate_end).map_err(|_| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(absolute),
                )
            })?)
            .ok_or_else(|| {
                ObjectError::for_code(
                    ObjectErrorCode::InternalState,
                    Some(target.reference()),
                    Some(absolute),
                )
            })?;
        match parse_boundary(
            target.snapshot().identity(),
            target.reference(),
            absolute,
            &raw[position..candidate_end],
            InputExtent::MayContinue,
            envelope.syntax_limits(),
            cancellation,
        ) {
            Ok(BoundaryParse::Complete(parsed)) => {
                if parsed.endobj_span.end_exclusive() > candidate_end_absolute {
                    return Err(ObjectError::for_code(
                        ObjectErrorCode::InternalState,
                        Some(target.reference()),
                        Some(absolute),
                    ));
                }
                if selected.is_some() {
                    return Err(ObjectError::for_code(
                        ObjectErrorCode::AmbiguousRepair,
                        Some(target.reference()),
                        Some(declared_end),
                    ));
                }
                selected = Some(boundary_candidate(effective_length, parsed));
            }
            Ok(BoundaryParse::NeedMore { .. }) => {}
            Err(error) if error.category() == ObjectErrorCategory::Syntax => {}
            Err(error) => return Err(error),
        }
    }
    if cancellation.is_cancelled() {
        return Err(ObjectError::for_code(
            ObjectErrorCode::Cancelled,
            Some(target.reference()),
            None,
        ));
    }
    Ok(BoundarySelection { selected, count })
}

fn looks_like_stream_boundary(bytes: &[u8], position: usize) -> bool {
    match bytes.get(position) {
        Some(b'\n') => {
            (position == 0 || bytes.get(position - 1) != Some(&b'\r'))
                && bytes.get(position + 1..position.saturating_add(10)) == Some(b"endstream")
        }
        Some(b'\r') => {
            bytes.get(position + 1) == Some(&b'\n')
                && bytes.get(position + 2..position.saturating_add(11)) == Some(b"endstream")
        }
        _ => false,
    }
}

fn boundary_candidate(effective_length: u64, parsed: ParsedBoundary) -> BoundaryCandidate {
    BoundaryCandidate {
        effective_length,
        data_delimiter_span: parsed.data_delimiter_span,
        endstream_span: parsed.endstream_span,
        endobj_span: parsed.endobj_span,
    }
}

fn repaired_stream_object(
    envelope: StreamEnvelope,
    boundary: BoundaryCandidate,
) -> Result<IndirectObject, ObjectError> {
    let target = envelope.target;
    let DeclaredStreamLength::Direct { .. } = envelope.declared_length else {
        return Err(ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(target.reference()),
            Some(envelope.declared_length.operand_span().start()),
        ));
    };
    if boundary.endobj_span.end_exclusive() > target.object_upper_bound() {
        return Err(ObjectError::for_code(
            ObjectErrorCode::ObjectCrossesPhysicalBound,
            Some(target.reference()),
            Some(target.object_upper_bound()),
        ));
    }
    let data_span =
        ByteSpan::new(envelope.data_start, boundary.effective_length).map_err(|_| {
            ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(target.reference()),
                Some(envelope.data_start),
            )
        })?;
    let object_len = boundary
        .endobj_span
        .end_exclusive()
        .checked_sub(target.xref_offset())
        .ok_or_else(|| {
            ObjectError::for_code(
                ObjectErrorCode::InternalState,
                Some(target.reference()),
                Some(target.xref_offset()),
            )
        })?;
    let object_span = ByteSpan::new(target.xref_offset(), object_len).map_err(|_| {
        ObjectError::for_code(
            ObjectErrorCode::InternalState,
            Some(target.reference()),
            Some(target.xref_offset()),
        )
    })?;
    let claim = StreamLengthClaim::repaired_direct(
        target.snapshot(),
        target.reference(),
        envelope.declared_length,
        boundary.effective_length,
    );
    let stream = FramedStream::new(
        envelope.dictionary,
        claim,
        envelope.stream_keyword_span,
        envelope.stream_line_ending_span,
        data_span,
        boundary.data_delimiter_span,
        boundary.endstream_span,
    );
    Ok(IndirectObject::new(
        target,
        envelope.header_span,
        object_span,
        boundary.endobj_span,
        envelope.retained_heap_bytes,
        IndirectObjectValue::Stream(stream),
    ))
}

const fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}
