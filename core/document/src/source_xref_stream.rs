use std::error::Error;
use std::fmt;
use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceError, SourceErrorCategory, SourceRecoverability,
    SourceSnapshot,
};
use pdf_rs_object::{
    DeclaredStreamLength, IndirectObject, IndirectObjectTarget, IndirectObjectTargetKind,
    IndirectObjectValue, ObjectCancellation, ObjectEnvelopePoll, ObjectError, ObjectErrorCategory,
    ObjectJobContext, ObjectLimits, ObjectPoll, ObjectRecoverability, ObjectStats,
    OpenObjectEnvelopeJob, OpenStreamBoundaryJob,
};
use pdf_rs_syntax::{ByteSpan, ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    XrefCancellation, XrefRecoverability, XrefStream, XrefStreamEntry, XrefStreamEntryKind,
    XrefStreamError, XrefStreamErrorCategory, XrefStreamLimits, XrefStreamStats,
    parse_unfiltered_xref_stream,
};

/// Runtime identity, phase checkpoints, and priority for one source xref-stream job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceXrefStreamJobContext {
    job: JobId,
    envelope_checkpoint: ResumeCheckpoint,
    boundary_checkpoint: ResumeCheckpoint,
    payload_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl SourceXrefStreamJobContext {
    /// Creates a context whose three checkpoints must be pairwise distinct.
    pub const fn new(
        job: JobId,
        envelope_checkpoint: ResumeCheckpoint,
        boundary_checkpoint: ResumeCheckpoint,
        payload_checkpoint: ResumeCheckpoint,
        priority: RequestPriority,
    ) -> Self {
        Self {
            job,
            envelope_checkpoint,
            boundary_checkpoint,
            payload_checkpoint,
            priority,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the indirect-object envelope checkpoint.
    pub const fn envelope_checkpoint(self) -> ResumeCheckpoint {
        self.envelope_checkpoint
    }

    /// Returns the exact stream-boundary checkpoint.
    pub const fn boundary_checkpoint(self) -> ResumeCheckpoint {
        self.boundary_checkpoint
    }

    /// Returns the exact encoded-payload checkpoint.
    pub const fn payload_checkpoint(self) -> ResumeCheckpoint {
        self.payload_checkpoint
    }

    /// Returns the scheduling priority copied to all three exact reads.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }

    fn is_valid(self) -> bool {
        self.envelope_checkpoint != self.boundary_checkpoint
            && self.envelope_checkpoint != self.payload_checkpoint
            && self.boundary_checkpoint != self.payload_checkpoint
    }

    const fn object(self) -> ObjectJobContext {
        ObjectJobContext::new(
            self.job,
            self.envelope_checkpoint,
            self.boundary_checkpoint,
            self.priority,
        )
    }
}

/// Cooperative cancellation probe supplied by the owning runtime.
pub trait SourceXrefStreamCancellation: Send + Sync {
    /// Reports whether acquisition must stop at the next bounded probe.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation probe that never requests cancellation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NeverCancelSourceXrefStream;

impl SourceXrefStreamCancellation for NeverCancelSourceXrefStream {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl SourceXrefStreamCancellation for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

struct ObjectCancellationAdapter<'a>(&'a dyn SourceXrefStreamCancellation);

impl ObjectCancellation for ObjectCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct XrefCancellationAdapter<'a>(&'a dyn SourceXrefStreamCancellation);

impl XrefCancellation for XrefCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// Stable machine-readable failure for source-framed xref-stream acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceXrefStreamErrorCode {
    /// Job checkpoints are not pairwise distinct.
    InvalidJobContext,
    /// A lower indirect-object operation failed.
    ObjectFailure,
    /// Bootstrap acquisition cannot resolve an indirect `/Length` dependency.
    UnsupportedIndirectLength,
    /// The polled byte source no longer matches the immutable snapshot.
    SnapshotMismatch,
    /// The exact encoded-payload read failed in the byte layer.
    SourceFailure,
    /// An in-bounds exact encoded-payload read unexpectedly reached EOF.
    UnexpectedEndOfSource,
    /// Returned payload bytes do not match the requested snapshot and range.
    SourceGeometryMismatch,
    /// Framing returned a non-stream or inconsistent xref-stream container.
    InvalidContainer,
    /// A primary stream lacks its exact uncompressed self entry, or a present hybrid self entry is wrong.
    InvalidSelfEntry,
    /// Decoded xref-stream validation failed.
    XrefStreamFailure,
    /// A payload or combined retained-proof budget was exceeded.
    ResourceLimit,
    /// The owning runtime cancelled acquisition.
    Cancelled,
    /// A checked internal state invariant could not be maintained.
    InternalState,
    /// A completed one-shot acquisition job was polled again.
    JobAlreadyComplete,
}

/// Acquisition-level resource dimension rejected before publishing proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceXrefStreamLimitKind {
    /// Exact encoded bytes retained transiently for the unfiltered parser.
    PayloadBytes,
    /// Combined framed-dictionary and parsed-entry bytes retained by the result.
    RetainedProofBytes,
}

/// Structured acquisition resource-limit context without source bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceXrefStreamLimit {
    kind: SourceXrefStreamLimitKind,
    limit: u64,
    attempted: u64,
}

impl SourceXrefStreamLimit {
    /// Returns the rejected acquisition resource dimension.
    pub const fn kind(self) -> SourceXrefStreamLimitKind {
        self.kind
    }

    /// Returns the configured or derived ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the rejected byte count.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Coarse source xref-stream failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceXrefStreamErrorCategory {
    /// Invalid caller configuration.
    Configuration,
    /// Immutable byte-source or source-proof failure.
    Source,
    /// Malformed indirect-object, stream, or xref metadata.
    Syntax,
    /// A valid construct requires a later bootstrap profile.
    Unsupported,
    /// Deterministic work or allocation exhaustion.
    Resource,
    /// Normal cooperative cancellation.
    Cancellation,
    /// Internal implementation failure.
    Internal,
}

/// Stable recovery policy for source xref-stream acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceXrefStreamRecoverability {
    /// Correct job identity, checkpoints, bounds, or limit configuration.
    CorrectConfiguration,
    /// Correct the PDF bytes or select an explicitly approved repair path.
    CorrectInput,
    /// Reopen against a newly bound immutable source snapshot.
    ReopenSource,
    /// Retry the host byte-source operation while preserving snapshot identity.
    RetrySource,
    /// Reduce work or choose an approved larger deterministic profile.
    ReduceWorkload,
    /// Select a profile that supports the requested construct.
    UseSupportedFeature,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceXrefStreamErrorDetail {
    None,
    Limit(SourceXrefStreamLimit),
    Object(ObjectError),
    XrefStream(XrefStreamError),
    Source(SourceError),
}

/// Source-redacted error that retains complete lower-layer evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceXrefStreamError {
    code: SourceXrefStreamErrorCode,
    category: SourceXrefStreamErrorCategory,
    recoverability: SourceXrefStreamRecoverability,
    diagnostic_id: &'static str,
    container: Option<ObjectRef>,
    dependency: Option<ObjectRef>,
    offset: Option<u64>,
    detail: SourceXrefStreamErrorDetail,
}

impl SourceXrefStreamError {
    const fn for_code(
        code: SourceXrefStreamErrorCode,
        container: Option<ObjectRef>,
        dependency: Option<ObjectRef>,
        offset: Option<u64>,
    ) -> Self {
        let (category, recoverability, diagnostic_id) = policy(code);
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            container,
            dependency,
            offset,
            detail: SourceXrefStreamErrorDetail::None,
        }
    }

    fn from_object(error: ObjectError) -> Self {
        let (category, recoverability) = object_policy(error);
        Self {
            code: SourceXrefStreamErrorCode::ObjectFailure,
            category,
            recoverability,
            diagnostic_id: "RPE-SOURCE-XREF-0002",
            container: error.reference(),
            dependency: None,
            offset: error.offset(),
            detail: SourceXrefStreamErrorDetail::Object(error),
        }
    }

    fn from_xref_stream(error: XrefStreamError, container: ObjectRef) -> Self {
        let (category, recoverability) = xref_stream_policy(error);
        Self {
            code: SourceXrefStreamErrorCode::XrefStreamFailure,
            category,
            recoverability,
            diagnostic_id: "RPE-SOURCE-XREF-0010",
            container: Some(container),
            dependency: None,
            offset: error.source_offset(),
            detail: SourceXrefStreamErrorDetail::XrefStream(error),
        }
    }

    fn from_source(error: SourceError, container: ObjectRef, offset: u64) -> Self {
        let (category, recoverability) = source_policy(error);
        Self {
            code: SourceXrefStreamErrorCode::SourceFailure,
            category,
            recoverability,
            diagnostic_id: "RPE-SOURCE-XREF-0005",
            container: Some(container),
            dependency: None,
            offset: Some(offset),
            detail: SourceXrefStreamErrorDetail::Source(error),
        }
    }

    const fn resource(
        kind: SourceXrefStreamLimitKind,
        limit: u64,
        attempted: u64,
        container: ObjectRef,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: SourceXrefStreamErrorCode::ResourceLimit,
            category: SourceXrefStreamErrorCategory::Resource,
            recoverability: SourceXrefStreamRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-SOURCE-XREF-0014",
            container: Some(container),
            dependency: None,
            offset,
            detail: SourceXrefStreamErrorDetail::Limit(SourceXrefStreamLimit {
                kind,
                limit,
                attempted,
            }),
        }
    }

    /// Returns the stable machine-readable failure code.
    pub const fn code(self) -> SourceXrefStreamErrorCode {
        self.code
    }

    /// Returns the stable coarse category.
    pub const fn category(self) -> SourceXrefStreamErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> SourceXrefStreamRecoverability {
        self.recoverability
    }

    /// Returns the stable source-redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the selected xref-stream container when known.
    pub const fn container(self) -> Option<ObjectRef> {
        self.container
    }

    /// Returns the unsupported indirect `/Length` dependency when applicable.
    pub const fn dependency(self) -> Option<ObjectRef> {
        self.dependency
    }

    /// Returns the physical source offset associated with the failure when known.
    pub const fn offset(self) -> Option<u64> {
        self.offset
    }

    /// Returns acquisition-level resource context when work was rejected before a child parser.
    pub const fn limit(self) -> Option<SourceXrefStreamLimit> {
        match self.detail {
            SourceXrefStreamErrorDetail::Limit(limit) => Some(limit),
            SourceXrefStreamErrorDetail::None
            | SourceXrefStreamErrorDetail::Object(_)
            | SourceXrefStreamErrorDetail::XrefStream(_)
            | SourceXrefStreamErrorDetail::Source(_) => None,
        }
    }

    /// Returns the complete lower object error when object framing failed.
    pub const fn object_error(self) -> Option<ObjectError> {
        match self.detail {
            SourceXrefStreamErrorDetail::Object(error) => Some(error),
            SourceXrefStreamErrorDetail::None
            | SourceXrefStreamErrorDetail::Limit(_)
            | SourceXrefStreamErrorDetail::XrefStream(_)
            | SourceXrefStreamErrorDetail::Source(_) => None,
        }
    }

    /// Returns the complete lower xref-stream error when table validation failed.
    pub const fn xref_stream_error(self) -> Option<XrefStreamError> {
        match self.detail {
            SourceXrefStreamErrorDetail::XrefStream(error) => Some(error),
            SourceXrefStreamErrorDetail::None
            | SourceXrefStreamErrorDetail::Limit(_)
            | SourceXrefStreamErrorDetail::Object(_)
            | SourceXrefStreamErrorDetail::Source(_) => None,
        }
    }

    /// Returns the complete lower byte-source error when the payload read failed.
    pub const fn source_error(self) -> Option<SourceError> {
        match self.detail {
            SourceXrefStreamErrorDetail::Source(error) => Some(error),
            SourceXrefStreamErrorDetail::None
            | SourceXrefStreamErrorDetail::Limit(_)
            | SourceXrefStreamErrorDetail::Object(_)
            | SourceXrefStreamErrorDetail::XrefStream(_) => None,
        }
    }
}

impl fmt::Display for SourceXrefStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)
    }
}

impl Error for SourceXrefStreamError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.detail {
            SourceXrefStreamErrorDetail::Object(error) => Some(error),
            SourceXrefStreamErrorDetail::XrefStream(error) => Some(error),
            SourceXrefStreamErrorDetail::Source(error) => Some(error),
            SourceXrefStreamErrorDetail::None | SourceXrefStreamErrorDetail::Limit(_) => None,
        }
    }
}

const fn policy(
    code: SourceXrefStreamErrorCode,
) -> (
    SourceXrefStreamErrorCategory,
    SourceXrefStreamRecoverability,
    &'static str,
) {
    match code {
        SourceXrefStreamErrorCode::InvalidJobContext => (
            SourceXrefStreamErrorCategory::Configuration,
            SourceXrefStreamRecoverability::CorrectConfiguration,
            "RPE-SOURCE-XREF-0001",
        ),
        SourceXrefStreamErrorCode::ObjectFailure => (
            SourceXrefStreamErrorCategory::Internal,
            SourceXrefStreamRecoverability::DoNotRetry,
            "RPE-SOURCE-XREF-0002",
        ),
        SourceXrefStreamErrorCode::UnsupportedIndirectLength => (
            SourceXrefStreamErrorCategory::Unsupported,
            SourceXrefStreamRecoverability::UseSupportedFeature,
            "RPE-SOURCE-XREF-0003",
        ),
        SourceXrefStreamErrorCode::SnapshotMismatch => (
            SourceXrefStreamErrorCategory::Source,
            SourceXrefStreamRecoverability::ReopenSource,
            "RPE-SOURCE-XREF-0004",
        ),
        SourceXrefStreamErrorCode::SourceFailure => (
            SourceXrefStreamErrorCategory::Source,
            SourceXrefStreamRecoverability::DoNotRetry,
            "RPE-SOURCE-XREF-0005",
        ),
        SourceXrefStreamErrorCode::UnexpectedEndOfSource => (
            SourceXrefStreamErrorCategory::Source,
            SourceXrefStreamRecoverability::ReopenSource,
            "RPE-SOURCE-XREF-0006",
        ),
        SourceXrefStreamErrorCode::SourceGeometryMismatch => (
            SourceXrefStreamErrorCategory::Source,
            SourceXrefStreamRecoverability::ReopenSource,
            "RPE-SOURCE-XREF-0007",
        ),
        SourceXrefStreamErrorCode::InvalidContainer => (
            SourceXrefStreamErrorCategory::Syntax,
            SourceXrefStreamRecoverability::CorrectInput,
            "RPE-SOURCE-XREF-0008",
        ),
        SourceXrefStreamErrorCode::InvalidSelfEntry => (
            SourceXrefStreamErrorCategory::Syntax,
            SourceXrefStreamRecoverability::CorrectInput,
            "RPE-SOURCE-XREF-0009",
        ),
        SourceXrefStreamErrorCode::XrefStreamFailure => (
            SourceXrefStreamErrorCategory::Internal,
            SourceXrefStreamRecoverability::DoNotRetry,
            "RPE-SOURCE-XREF-0010",
        ),
        SourceXrefStreamErrorCode::ResourceLimit => (
            SourceXrefStreamErrorCategory::Resource,
            SourceXrefStreamRecoverability::ReduceWorkload,
            "RPE-SOURCE-XREF-0014",
        ),
        SourceXrefStreamErrorCode::Cancelled => (
            SourceXrefStreamErrorCategory::Cancellation,
            SourceXrefStreamRecoverability::AbandonOperation,
            "RPE-SOURCE-XREF-0011",
        ),
        SourceXrefStreamErrorCode::InternalState => (
            SourceXrefStreamErrorCategory::Internal,
            SourceXrefStreamRecoverability::DoNotRetry,
            "RPE-SOURCE-XREF-0012",
        ),
        SourceXrefStreamErrorCode::JobAlreadyComplete => (
            SourceXrefStreamErrorCategory::Configuration,
            SourceXrefStreamRecoverability::CorrectConfiguration,
            "RPE-SOURCE-XREF-0013",
        ),
    }
}

fn object_policy(
    error: ObjectError,
) -> (
    SourceXrefStreamErrorCategory,
    SourceXrefStreamRecoverability,
) {
    let category = match error.category() {
        ObjectErrorCategory::Configuration => SourceXrefStreamErrorCategory::Configuration,
        ObjectErrorCategory::Source => SourceXrefStreamErrorCategory::Source,
        ObjectErrorCategory::Syntax => SourceXrefStreamErrorCategory::Syntax,
        ObjectErrorCategory::Unsupported => SourceXrefStreamErrorCategory::Unsupported,
        ObjectErrorCategory::Resource => SourceXrefStreamErrorCategory::Resource,
        ObjectErrorCategory::Cancellation => SourceXrefStreamErrorCategory::Cancellation,
        ObjectErrorCategory::Internal => SourceXrefStreamErrorCategory::Internal,
    };
    let recoverability = match error.recoverability() {
        ObjectRecoverability::CorrectConfiguration => {
            SourceXrefStreamRecoverability::CorrectConfiguration
        }
        ObjectRecoverability::CorrectInput => SourceXrefStreamRecoverability::CorrectInput,
        ObjectRecoverability::ReopenSource => SourceXrefStreamRecoverability::ReopenSource,
        ObjectRecoverability::RetrySource => SourceXrefStreamRecoverability::RetrySource,
        ObjectRecoverability::ReduceWorkload => SourceXrefStreamRecoverability::ReduceWorkload,
        ObjectRecoverability::UseSupportedFeature => {
            SourceXrefStreamRecoverability::UseSupportedFeature
        }
        ObjectRecoverability::AbandonOperation => SourceXrefStreamRecoverability::AbandonOperation,
        ObjectRecoverability::DoNotRetry => SourceXrefStreamRecoverability::DoNotRetry,
    };
    (category, recoverability)
}

fn xref_stream_policy(
    error: XrefStreamError,
) -> (
    SourceXrefStreamErrorCategory,
    SourceXrefStreamRecoverability,
) {
    let category = match error.category() {
        XrefStreamErrorCategory::Configuration => SourceXrefStreamErrorCategory::Configuration,
        XrefStreamErrorCategory::Source => SourceXrefStreamErrorCategory::Source,
        XrefStreamErrorCategory::Syntax => SourceXrefStreamErrorCategory::Syntax,
        XrefStreamErrorCategory::Unsupported => SourceXrefStreamErrorCategory::Unsupported,
        XrefStreamErrorCategory::Resource => SourceXrefStreamErrorCategory::Resource,
        XrefStreamErrorCategory::Cancellation => SourceXrefStreamErrorCategory::Cancellation,
        XrefStreamErrorCategory::Internal => SourceXrefStreamErrorCategory::Internal,
    };
    let recoverability = match error.recoverability() {
        XrefRecoverability::CorrectConfiguration => {
            SourceXrefStreamRecoverability::CorrectConfiguration
        }
        XrefRecoverability::CorrectInput => SourceXrefStreamRecoverability::CorrectInput,
        XrefRecoverability::ReopenSource => SourceXrefStreamRecoverability::ReopenSource,
        XrefRecoverability::RetrySource => SourceXrefStreamRecoverability::RetrySource,
        XrefRecoverability::ReduceWorkload => SourceXrefStreamRecoverability::ReduceWorkload,
        XrefRecoverability::UseSupportedFeature => {
            SourceXrefStreamRecoverability::UseSupportedFeature
        }
        XrefRecoverability::AbandonOperation => SourceXrefStreamRecoverability::AbandonOperation,
        XrefRecoverability::DoNotRetry => SourceXrefStreamRecoverability::DoNotRetry,
    };
    (category, recoverability)
}

fn source_policy(
    error: SourceError,
) -> (
    SourceXrefStreamErrorCategory,
    SourceXrefStreamRecoverability,
) {
    let category = match error.category() {
        SourceErrorCategory::Input | SourceErrorCategory::Lifecycle => {
            SourceXrefStreamErrorCategory::Configuration
        }
        SourceErrorCategory::Integrity | SourceErrorCategory::Availability => {
            SourceXrefStreamErrorCategory::Source
        }
        SourceErrorCategory::Resource => SourceXrefStreamErrorCategory::Resource,
        SourceErrorCategory::Internal => SourceXrefStreamErrorCategory::Internal,
    };
    let recoverability = match error.recoverability() {
        SourceRecoverability::CorrectInput => SourceXrefStreamRecoverability::CorrectConfiguration,
        SourceRecoverability::ReopenSource => SourceXrefStreamRecoverability::ReopenSource,
        SourceRecoverability::ReduceWorkload => SourceXrefStreamRecoverability::ReduceWorkload,
        SourceRecoverability::RetrySource => SourceXrefStreamRecoverability::RetrySource,
        SourceRecoverability::DoNotRetry => SourceXrefStreamRecoverability::DoNotRetry,
    };
    (category, recoverability)
}

/// Coarse phase of one source-framed xref-stream acquisition job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceXrefStreamPhase {
    /// Framing the indirect-object dictionary and payload start.
    Envelope,
    /// Independently acquiring the exact payload and validating its exact terminal boundary.
    PayloadAndBoundary,
    /// Parsing and validating the complete unfiltered xref table.
    Parse,
    /// The proof-bearing result was returned.
    Complete,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Cumulative work and child-parser evidence for one acquisition job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceXrefStreamStats {
    object: ObjectStats,
    payload_read_bytes: u64,
    payload_read_attempts: u64,
    xref_stream: Option<XrefStreamStats>,
    retained_proof_bytes: u64,
}

impl SourceXrefStreamStats {
    /// Returns cumulative staged object-framing work.
    pub const fn object(self) -> ObjectStats {
        self.object
    }

    /// Returns exact encoded-payload bytes charged once when its request is installed.
    pub const fn payload_read_bytes(self) -> u64 {
        self.payload_read_bytes
    }

    /// Returns the number of logical exact payload requests installed.
    pub const fn payload_read_attempts(self) -> u64 {
        self.payload_read_attempts
    }

    /// Returns decoded table work after xref-stream validation succeeds.
    pub const fn xref_stream(self) -> Option<XrefStreamStats> {
        self.xref_stream
    }

    /// Returns combined object-dictionary and xref-entry capacity retained by ready proof.
    pub const fn retained_proof_bytes(self) -> u64 {
        self.retained_proof_bytes
    }
}

/// Source-acquired xref-stream proof retaining its complete framed container.
#[derive(Eq, PartialEq)]
pub struct SourceAcquiredXrefStream {
    framed_container: IndirectObject,
    xref_stream: XrefStream,
    stats: SourceXrefStreamStats,
}

impl SourceAcquiredXrefStream {
    /// Returns the immutable source snapshot shared by container and table evidence.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.framed_container.snapshot()
    }

    /// Returns the complete framed xref-stream indirect object.
    pub const fn framed_container(&self) -> &IndirectObject {
        &self.framed_container
    }

    /// Borrows the complete table only for proof-preserving composition inside this crate.
    #[allow(
        dead_code,
        reason = "the next mixed-revision coordinator will consume this proof-bound internal view"
    )]
    pub(crate) const fn xref_stream(&self) -> &XrefStream {
        &self.xref_stream
    }

    /// Returns the xref-stream container object identity.
    pub const fn container(&self) -> ObjectRef {
        self.xref_stream.container()
    }

    /// Returns the exact physical encoded-payload span.
    pub const fn encoded_payload_span(&self) -> ByteSpan {
        self.xref_stream.encoded_payload_span()
    }

    /// Returns the validated `/Size` object-number space.
    pub const fn declared_size(&self) -> u32 {
        self.xref_stream.declared_size()
    }

    /// Returns the optional explicit root reference.
    pub const fn root(&self) -> Option<ObjectRef> {
        self.xref_stream.root()
    }

    /// Returns the optional older primary anchor named by `/Prev`.
    pub const fn previous(&self) -> Option<u64> {
        self.xref_stream.previous()
    }

    /// Returns the validated three field widths.
    pub const fn widths(&self) -> [u8; 3] {
        self.xref_stream.widths()
    }

    /// Borrows validated rows without exposing the cloneable naked table proof.
    pub fn entries(&self) -> &[XrefStreamEntry] {
        self.xref_stream.entries()
    }

    /// Returns complete acquisition and parsing accounting.
    pub const fn stats(&self) -> SourceXrefStreamStats {
        self.stats
    }
}

impl fmt::Debug for SourceAcquiredXrefStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceAcquiredXrefStream")
            .field("framed_container", &self.framed_container)
            .field("xref_stream", &self.xref_stream)
            .field("stats", &self.stats)
            .finish()
    }
}

/// Result of polling one source-framed xref-stream acquisition job.
#[allow(
    clippy::large_enum_variant,
    reason = "the one-shot proof remains inline so retained allocation accounting is explicit"
)]
#[derive(Debug, Eq, PartialEq)]
pub enum SourceXrefStreamPoll {
    /// A complete framed container and validated unfiltered table are ready.
    Ready(SourceAcquiredXrefStream),
    /// One active exact read is missing source bytes.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges missing from the active request.
        missing: SmallRanges,
        /// Exact phase checkpoint to retain while requeueing the job.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a stable structured failure.
    Failed(SourceXrefStreamError),
}

enum PayloadState {
    Empty,
    Missing { range: ByteRange },
    Ready(ByteSlice),
}

impl PayloadState {
    const fn is_ready(&self) -> bool {
        matches!(self, Self::Empty | Self::Ready(_))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcquireStep {
    Boundary,
    Payload,
}

struct AcquireState {
    boundary: OpenStreamBoundaryJob,
    payload: PayloadState,
    framed: Option<IndirectObject>,
    next: AcquireStep,
}

#[allow(
    clippy::large_enum_variant,
    reason = "active proof-bearing children stay inline without an untracked infallible allocation"
)]
enum JobState {
    Envelope(OpenObjectEnvelopeJob),
    Acquire(AcquireState),
    Parse {
        framed: IndirectObject,
        payload: PayloadState,
    },
    Transition,
    Complete,
    Failed(SourceXrefStreamError),
}

/// One-shot job that frames and acquires one explicitly unfiltered xref stream from source.
pub struct OpenSourceXrefStreamJob {
    snapshot: SourceSnapshot,
    container: ObjectRef,
    startxref: u64,
    object_upper_bound: u64,
    revision_startxref: u64,
    context: SourceXrefStreamJobContext,
    object_limits: ObjectLimits,
    syntax_limits: SyntaxLimits,
    xref_stream_limits: XrefStreamLimits,
    stats: SourceXrefStreamStats,
    state: JobState,
}

impl OpenSourceXrefStreamJob {
    /// Validates geometry and starts staged framing at the classified stream-object anchor.
    #[allow(
        clippy::too_many_arguments,
        reason = "the public constructor makes every source bound and validated child profile explicit"
    )]
    pub fn new(
        snapshot: SourceSnapshot,
        container: ObjectRef,
        startxref: u64,
        object_upper_bound: u64,
        revision_startxref: u64,
        context: SourceXrefStreamJobContext,
        object_limits: ObjectLimits,
        syntax_limits: SyntaxLimits,
        xref_stream_limits: XrefStreamLimits,
    ) -> Result<Self, SourceXrefStreamError> {
        if !context.is_valid() {
            return Err(SourceXrefStreamError::for_code(
                SourceXrefStreamErrorCode::InvalidJobContext,
                Some(container),
                None,
                None,
            ));
        }
        let target = IndirectObjectTarget::at_xref_stream_anchor(
            snapshot,
            container,
            startxref,
            object_upper_bound,
            revision_startxref,
        )
        .map_err(SourceXrefStreamError::from_object)?;
        let envelope =
            OpenObjectEnvelopeJob::new(target, context.object(), object_limits, syntax_limits)
                .map_err(SourceXrefStreamError::from_object)?;
        Ok(Self {
            snapshot,
            container,
            startxref,
            object_upper_bound,
            revision_startxref,
            context,
            object_limits,
            syntax_limits,
            xref_stream_limits,
            stats: SourceXrefStreamStats::default(),
            state: JobState::Envelope(envelope),
        })
    }

    /// Returns the immutable source snapshot bound at construction.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the classified stream-object reference.
    pub const fn container(&self) -> ObjectRef {
        self.container
    }

    /// Returns the physical stream-object anchor.
    pub const fn startxref(&self) -> u64 {
        self.startxref
    }

    /// Returns the caller-proved exclusive object bound.
    pub const fn object_upper_bound(&self) -> u64 {
        self.object_upper_bound
    }

    /// Returns the owning primary revision anchor.
    pub const fn revision_startxref(&self) -> u64 {
        self.revision_startxref
    }

    /// Returns runtime identity, checkpoints, and scheduling priority.
    pub const fn context(&self) -> SourceXrefStreamJobContext {
        self.context
    }

    /// Returns the validated object-framing limits.
    pub const fn object_limits(&self) -> ObjectLimits {
        self.object_limits
    }

    /// Returns the validated syntax limits.
    pub const fn syntax_limits(&self) -> SyntaxLimits {
        self.syntax_limits
    }

    /// Returns the validated xref-stream limits.
    pub const fn xref_stream_limits(&self) -> XrefStreamLimits {
        self.xref_stream_limits
    }

    /// Returns cumulative work through the latest poll.
    pub const fn stats(&self) -> SourceXrefStreamStats {
        self.stats
    }

    /// Returns the current coarse acquisition phase.
    pub fn phase(&self) -> SourceXrefStreamPhase {
        match &self.state {
            JobState::Envelope(_) => SourceXrefStreamPhase::Envelope,
            JobState::Acquire(_) => SourceXrefStreamPhase::PayloadAndBoundary,
            JobState::Parse { .. } => SourceXrefStreamPhase::Parse,
            JobState::Complete => SourceXrefStreamPhase::Complete,
            JobState::Transition | JobState::Failed(_) => SourceXrefStreamPhase::Failed,
        }
    }

    /// Advances acquisition without performing host I/O or retaining caller-provided bytes.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn SourceXrefStreamCancellation + '_),
    ) -> SourceXrefStreamPoll {
        if let JobState::Failed(error) = &self.state {
            return SourceXrefStreamPoll::Failed(*error);
        }
        if matches!(self.state, JobState::Complete) {
            return SourceXrefStreamPoll::Failed(SourceXrefStreamError::for_code(
                SourceXrefStreamErrorCode::JobAlreadyComplete,
                Some(self.container),
                None,
                None,
            ));
        }
        if source.snapshot() != self.snapshot {
            return self.fail(SourceXrefStreamError::for_code(
                SourceXrefStreamErrorCode::SnapshotMismatch,
                Some(self.container),
                None,
                None,
            ));
        }
        if cancellation.is_cancelled() {
            return self.fail(SourceXrefStreamError::for_code(
                SourceXrefStreamErrorCode::Cancelled,
                Some(self.container),
                None,
                Some(self.startxref),
            ));
        }

        loop {
            let state = mem::replace(&mut self.state, JobState::Transition);
            match state {
                JobState::Envelope(mut job) => {
                    let outcome = job.poll(source, &ObjectCancellationAdapter(cancellation));
                    self.stats.object = job.stats();
                    match outcome {
                        ObjectEnvelopePoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = JobState::Envelope(job);
                            return SourceXrefStreamPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        ObjectEnvelopePoll::Failed(error) => {
                            return self.fail(SourceXrefStreamError::from_object(error));
                        }
                        ObjectEnvelopePoll::Direct(_) => {
                            return self.fail(SourceXrefStreamError::for_code(
                                SourceXrefStreamErrorCode::InvalidContainer,
                                Some(self.container),
                                None,
                                Some(self.startxref),
                            ));
                        }
                        ObjectEnvelopePoll::Stream(envelope) => {
                            let declaration = envelope.declared_length();
                            let DeclaredStreamLength::Direct { value, .. } = declaration else {
                                return self.fail(SourceXrefStreamError::for_code(
                                    SourceXrefStreamErrorCode::UnsupportedIndirectLength,
                                    Some(self.container),
                                    declaration.indirect_reference(),
                                    Some(declaration.operand_span().start()),
                                ));
                            };
                            let data_start = envelope.data_start();
                            let data_end = match data_start.checked_add(value) {
                                Some(value) => value,
                                None => {
                                    return self.fail(SourceXrefStreamError::for_code(
                                        SourceXrefStreamErrorCode::SourceGeometryMismatch,
                                        Some(self.container),
                                        None,
                                        Some(data_start),
                                    ));
                                }
                            };
                            if data_end >= self.object_upper_bound {
                                let claim = match envelope.direct_length_claim() {
                                    Ok(claim) => claim,
                                    Err(error) => {
                                        return self
                                            .fail(SourceXrefStreamError::from_object(error));
                                    }
                                };
                                let error = match OpenStreamBoundaryJob::new(envelope, claim) {
                                    Ok(_) => SourceXrefStreamError::for_code(
                                        SourceXrefStreamErrorCode::InternalState,
                                        Some(self.container),
                                        None,
                                        Some(data_end),
                                    ),
                                    Err(error) => SourceXrefStreamError::from_object(error),
                                };
                                return self.fail(error);
                            }
                            let claim = match envelope.direct_length_claim() {
                                Ok(claim) => claim,
                                Err(error) => {
                                    return self.fail(SourceXrefStreamError::from_object(error));
                                }
                            };
                            let boundary = match OpenStreamBoundaryJob::new(envelope, claim) {
                                Ok(job) => job,
                                Err(error) => {
                                    return self.fail(SourceXrefStreamError::from_object(error));
                                }
                            };
                            if value > self.xref_stream_limits.max_decoded_bytes() {
                                return self.fail(SourceXrefStreamError::resource(
                                    SourceXrefStreamLimitKind::PayloadBytes,
                                    self.xref_stream_limits.max_decoded_bytes(),
                                    value,
                                    self.container,
                                    Some(data_start),
                                ));
                            }
                            let payload = if value == 0 {
                                PayloadState::Empty
                            } else {
                                let range = match ByteRange::new(data_start, value) {
                                    Ok(range) => range,
                                    Err(_) => {
                                        return self.fail(SourceXrefStreamError::for_code(
                                            SourceXrefStreamErrorCode::SourceGeometryMismatch,
                                            Some(self.container),
                                            None,
                                            Some(data_start),
                                        ));
                                    }
                                };
                                if range.end_exclusive() != data_end
                                    || range.end_exclusive() >= self.object_upper_bound
                                {
                                    return self.fail(SourceXrefStreamError::for_code(
                                        SourceXrefStreamErrorCode::SourceGeometryMismatch,
                                        Some(self.container),
                                        None,
                                        Some(data_start),
                                    ));
                                }
                                self.stats.payload_read_bytes = value;
                                self.stats.payload_read_attempts = 1;
                                PayloadState::Missing { range }
                            };
                            self.state = JobState::Acquire(AcquireState {
                                boundary,
                                payload,
                                framed: None,
                                next: AcquireStep::Payload,
                            });
                        }
                    }
                }
                JobState::Acquire(mut acquire) => {
                    if acquire.framed.is_some() && acquire.payload.is_ready() {
                        let Some(framed) = acquire.framed.take() else {
                            return self.fail(SourceXrefStreamError::for_code(
                                SourceXrefStreamErrorCode::InternalState,
                                Some(self.container),
                                None,
                                Some(self.startxref),
                            ));
                        };
                        self.state = JobState::Parse {
                            framed,
                            payload: acquire.payload,
                        };
                        continue;
                    }
                    if !acquire.payload.is_ready() {
                        acquire.next = AcquireStep::Payload;
                    } else if acquire.framed.is_none() {
                        acquire.next = AcquireStep::Boundary;
                    }
                    match acquire.next {
                        AcquireStep::Boundary => {
                            let outcome = acquire
                                .boundary
                                .poll(source, &ObjectCancellationAdapter(cancellation));
                            self.stats.object = acquire.boundary.stats();
                            match outcome {
                                ObjectPoll::Pending {
                                    ticket,
                                    missing,
                                    checkpoint,
                                } => {
                                    acquire.next = AcquireStep::Boundary;
                                    self.state = JobState::Acquire(acquire);
                                    return SourceXrefStreamPoll::Pending {
                                        ticket,
                                        missing,
                                        checkpoint,
                                    };
                                }
                                ObjectPoll::Failed(error) => {
                                    return self.fail(SourceXrefStreamError::from_object(error));
                                }
                                ObjectPoll::Ready(framed) => {
                                    acquire.framed = Some(framed);
                                    acquire.next = AcquireStep::Payload;
                                    self.state = JobState::Acquire(acquire);
                                }
                            }
                        }
                        AcquireStep::Payload => {
                            let PayloadState::Missing { range } = &acquire.payload else {
                                acquire.next = AcquireStep::Boundary;
                                self.state = JobState::Acquire(acquire);
                                continue;
                            };
                            let range = *range;
                            let request = ReadRequest::new(
                                range,
                                self.context.priority,
                                self.context.job,
                                self.context.payload_checkpoint,
                            );
                            match source.poll(request) {
                                ReadPoll::Pending { ticket, missing } => {
                                    acquire.next = AcquireStep::Payload;
                                    self.state = JobState::Acquire(acquire);
                                    return SourceXrefStreamPoll::Pending {
                                        ticket,
                                        missing,
                                        checkpoint: self.context.payload_checkpoint,
                                    };
                                }
                                ReadPoll::EndOfFile => {
                                    return self.fail(SourceXrefStreamError::for_code(
                                        SourceXrefStreamErrorCode::UnexpectedEndOfSource,
                                        Some(self.container),
                                        None,
                                        Some(range.start()),
                                    ));
                                }
                                ReadPoll::Failed(error) => {
                                    return self.fail(SourceXrefStreamError::from_source(
                                        error,
                                        self.container,
                                        range.start(),
                                    ));
                                }
                                ReadPoll::Ready(bytes) => {
                                    if bytes.identity() != self.snapshot.identity()
                                        || bytes.range() != range
                                        || u64::try_from(bytes.bytes().len()).ok()
                                            != Some(range.len())
                                    {
                                        return self.fail(SourceXrefStreamError::for_code(
                                            SourceXrefStreamErrorCode::SourceGeometryMismatch,
                                            Some(self.container),
                                            None,
                                            Some(range.start()),
                                        ));
                                    }
                                    acquire.payload = PayloadState::Ready(bytes);
                                    acquire.next = AcquireStep::Boundary;
                                    self.state = JobState::Acquire(acquire);
                                }
                            }
                        }
                    }
                }
                JobState::Parse { framed, payload } => {
                    return self.parse(framed, payload, cancellation);
                }
                JobState::Complete => {
                    self.state = JobState::Complete;
                    return SourceXrefStreamPoll::Failed(SourceXrefStreamError::for_code(
                        SourceXrefStreamErrorCode::JobAlreadyComplete,
                        Some(self.container),
                        None,
                        None,
                    ));
                }
                JobState::Failed(error) => {
                    self.state = JobState::Failed(error);
                    return SourceXrefStreamPoll::Failed(error);
                }
                JobState::Transition => {
                    return self.fail(SourceXrefStreamError::for_code(
                        SourceXrefStreamErrorCode::InternalState,
                        Some(self.container),
                        None,
                        None,
                    ));
                }
            }
        }
    }

    fn parse(
        &mut self,
        framed: IndirectObject,
        payload: PayloadState,
        cancellation: &dyn SourceXrefStreamCancellation,
    ) -> SourceXrefStreamPoll {
        if cancellation.is_cancelled() {
            return self.fail(SourceXrefStreamError::for_code(
                SourceXrefStreamErrorCode::Cancelled,
                Some(self.container),
                None,
                Some(self.startxref),
            ));
        }
        let Some(stream) = validate_container(
            &framed,
            self.snapshot,
            self.container,
            self.startxref,
            self.object_upper_bound,
            self.revision_startxref,
        ) else {
            return self.fail(SourceXrefStreamError::for_code(
                SourceXrefStreamErrorCode::InvalidContainer,
                Some(self.container),
                None,
                Some(self.startxref),
            ));
        };
        let data_span = stream.data_span();
        let payload_bytes = match &payload {
            PayloadState::Empty => &[][..],
            PayloadState::Ready(bytes)
                if bytes.identity() == self.snapshot.identity()
                    && bytes.range().start() == data_span.start()
                    && bytes.range().len() == data_span.len() =>
            {
                bytes.bytes()
            }
            PayloadState::Ready(_) | PayloadState::Missing { .. } => {
                return self.fail(SourceXrefStreamError::for_code(
                    SourceXrefStreamErrorCode::SourceGeometryMismatch,
                    Some(self.container),
                    None,
                    Some(data_span.start()),
                ));
            }
        };
        let xref_stream = match parse_unfiltered_xref_stream(
            self.snapshot,
            self.container,
            stream.dictionary().value(),
            data_span,
            payload_bytes,
            self.xref_stream_limits,
            &XrefCancellationAdapter(cancellation),
        ) {
            Ok(stream) => stream,
            Err(error) => {
                return self.fail(SourceXrefStreamError::from_xref_stream(
                    error,
                    self.container,
                ));
            }
        };
        self.stats.xref_stream = Some(xref_stream.stats());
        if !valid_self_entry(
            &xref_stream,
            self.container,
            self.startxref,
            self.revision_startxref == self.startxref,
        ) {
            return self.fail(SourceXrefStreamError::for_code(
                SourceXrefStreamErrorCode::InvalidSelfEntry,
                Some(self.container),
                None,
                Some(self.startxref),
            ));
        }
        let retained_limit = match self
            .syntax_limits
            .max_owned_bytes()
            .checked_add(self.syntax_limits.max_container_bytes())
            .and_then(|value| value.checked_add(self.xref_stream_limits.max_retained_entry_bytes()))
        {
            Some(limit) => limit,
            None => {
                return self.fail(SourceXrefStreamError::for_code(
                    SourceXrefStreamErrorCode::InternalState,
                    Some(self.container),
                    None,
                    Some(self.startxref),
                ));
            }
        };
        let retained_proof_bytes = match framed
            .retained_heap_bytes()
            .checked_add(xref_stream.stats().retained_entry_bytes())
        {
            Some(value) => value,
            None => {
                return self.fail(SourceXrefStreamError::resource(
                    SourceXrefStreamLimitKind::RetainedProofBytes,
                    retained_limit,
                    u64::MAX,
                    self.container,
                    Some(self.startxref),
                ));
            }
        };
        if retained_proof_bytes > retained_limit {
            return self.fail(SourceXrefStreamError::resource(
                SourceXrefStreamLimitKind::RetainedProofBytes,
                retained_limit,
                retained_proof_bytes,
                self.container,
                Some(self.startxref),
            ));
        }
        self.stats.retained_proof_bytes = retained_proof_bytes;
        if cancellation.is_cancelled() {
            return self.fail(SourceXrefStreamError::for_code(
                SourceXrefStreamErrorCode::Cancelled,
                Some(self.container),
                None,
                Some(self.startxref),
            ));
        }
        let result = SourceAcquiredXrefStream {
            framed_container: framed,
            xref_stream,
            stats: self.stats,
        };
        self.state = JobState::Complete;
        SourceXrefStreamPoll::Ready(result)
    }

    fn fail(&mut self, error: SourceXrefStreamError) -> SourceXrefStreamPoll {
        self.state = JobState::Failed(error);
        SourceXrefStreamPoll::Failed(error)
    }
}

impl fmt::Debug for OpenSourceXrefStreamJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenSourceXrefStreamJob")
            .field("snapshot", &self.snapshot)
            .field("container", &self.container)
            .field("startxref", &self.startxref)
            .field("object_upper_bound", &self.object_upper_bound)
            .field("revision_startxref", &self.revision_startxref)
            .field("context", &self.context)
            .field("object_limits", &self.object_limits)
            .field("syntax_limits", &self.syntax_limits)
            .field("xref_stream_limits", &self.xref_stream_limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .finish()
    }
}

fn validate_container(
    framed: &IndirectObject,
    snapshot: SourceSnapshot,
    container: ObjectRef,
    startxref: u64,
    object_upper_bound: u64,
    revision_startxref: u64,
) -> Option<&pdf_rs_object::FramedStream> {
    if framed.snapshot() != snapshot
        || framed.reference() != container
        || framed.target_kind() != IndirectObjectTargetKind::XrefStreamAnchor
        || framed.xref_offset() != startxref
        || framed.object_upper_bound() != object_upper_bound
        || framed.revision_startxref() != revision_startxref
        || framed.header_span().start() != startxref
        || framed.object_span().start() != startxref
        || framed.object_span().end_exclusive() > object_upper_bound
        || framed.endobj_span().end_exclusive() != framed.object_span().end_exclusive()
    {
        return None;
    }
    let IndirectObjectValue::Stream(stream) = framed.value() else {
        return None;
    };
    let dictionary = stream.dictionary();
    let data_span = stream.data_span();
    if dictionary.source() != snapshot.identity()
        || stream.length_claim().snapshot() != snapshot
        || stream.length_claim().owner() != container
        || stream.length_claim().resolved_value_span().is_some()
        || stream.length_claim().value() != data_span.len()
        || data_span.start() <= stream.stream_line_ending_span().start()
        || data_span.end_exclusive() >= object_upper_bound
        || stream.endstream_span().end_exclusive() > framed.endobj_span().start()
    {
        return None;
    }
    Some(stream)
}

fn valid_self_entry(
    stream: &XrefStream,
    container: ObjectRef,
    startxref: u64,
    required: bool,
) -> bool {
    if container.number() >= stream.declared_size() {
        return false;
    }
    let entry = stream
        .entries()
        .binary_search_by_key(&container.number(), |entry| entry.object_number())
        .ok()
        .map(|index| stream.entries()[index]);
    match entry {
        Some(entry) => matches!(
            entry.kind(),
            XrefStreamEntryKind::Uncompressed { offset, generation }
                if offset == startxref && generation == container.generation()
        ),
        None => !required,
    }
}
