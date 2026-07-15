use std::error::Error;
use std::fmt;
use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteSource, DataTicket, JobId, RequestPriority, ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    FinalStartXref, FinalStartXrefJobContext, FinalStartXrefPoll, HybridSupplement,
    OpenFinalStartXrefJob, OpenTraditionalRevisionJob, OpenXrefAnchorJob, ResolvedXrefEntry,
    RevisionCandidate, RevisionChain, RevisionEntry, RevisionError, RevisionErrorCategory,
    RevisionLimits, RevisionPrimaryKind, RevisionStats, TraditionalRevisionJobContext,
    TraditionalRevisionPoll, TraditionalRevisionSection, XrefAnchor, XrefAnchorJobContext,
    XrefAnchorKind, XrefAnchorLimits, XrefAnchorPoll, XrefCancellation, XrefError,
    XrefErrorCategory, XrefLimits, XrefRecoverability, XrefStreamLimits, compose_revision_chain,
};

use crate::{
    OpenSourceXrefStreamJob, SourceAcquiredXrefStream, SourceXrefStreamCancellation,
    SourceXrefStreamError, SourceXrefStreamErrorCategory, SourceXrefStreamJobContext,
    SourceXrefStreamPoll, SourceXrefStreamRecoverability,
};

const HARD_MAX_TOTAL_WORK_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_BOUND_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const CANCELLATION_INTERVAL: usize = 256;

/// Runtime identity and pairwise-distinct checkpoints for one source revision-chain job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRevisionChainJobContext {
    job: JobId,
    tail_checkpoint: ResumeCheckpoint,
    anchor_checkpoint: ResumeCheckpoint,
    traditional_checkpoint: ResumeCheckpoint,
    stream_envelope_checkpoint: ResumeCheckpoint,
    stream_boundary_checkpoint: ResumeCheckpoint,
    stream_payload_checkpoint: ResumeCheckpoint,
}

impl SourceRevisionChainJobContext {
    /// Creates a context whose six checkpoints must be pairwise distinct.
    pub const fn new(
        job: JobId,
        tail_checkpoint: ResumeCheckpoint,
        anchor_checkpoint: ResumeCheckpoint,
        traditional_checkpoint: ResumeCheckpoint,
        stream_envelope_checkpoint: ResumeCheckpoint,
        stream_boundary_checkpoint: ResumeCheckpoint,
        stream_payload_checkpoint: ResumeCheckpoint,
    ) -> Self {
        Self {
            job,
            tail_checkpoint,
            anchor_checkpoint,
            traditional_checkpoint,
            stream_envelope_checkpoint,
            stream_boundary_checkpoint,
            stream_payload_checkpoint,
        }
    }

    /// Returns the single caller-supplied runtime job identity used by every child.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the final-marker checkpoint.
    pub const fn tail_checkpoint(self) -> ResumeCheckpoint {
        self.tail_checkpoint
    }

    /// Returns the checkpoint reused by sequential primary and hybrid anchor classifiers.
    pub const fn anchor_checkpoint(self) -> ResumeCheckpoint {
        self.anchor_checkpoint
    }

    /// Returns the checkpoint reused by sequential traditional-section jobs.
    pub const fn traditional_checkpoint(self) -> ResumeCheckpoint {
        self.traditional_checkpoint
    }

    /// Returns the stream-envelope checkpoint reused by sequential stream acquisitions.
    pub const fn stream_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.stream_envelope_checkpoint
    }

    /// Returns the stream-boundary checkpoint reused by sequential stream acquisitions.
    pub const fn stream_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.stream_boundary_checkpoint
    }

    /// Returns the stream-payload checkpoint reused by sequential stream acquisitions.
    pub const fn stream_payload_checkpoint(self) -> ResumeCheckpoint {
        self.stream_payload_checkpoint
    }

    fn is_valid(self) -> bool {
        let checkpoints = [
            self.tail_checkpoint,
            self.anchor_checkpoint,
            self.traditional_checkpoint,
            self.stream_envelope_checkpoint,
            self.stream_boundary_checkpoint,
            self.stream_payload_checkpoint,
        ];
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            if checkpoints[index + 1..].contains(checkpoint) {
                return false;
            }
        }
        true
    }

    const fn tail(self) -> FinalStartXrefJobContext {
        FinalStartXrefJobContext::new(self.job, self.tail_checkpoint)
    }

    const fn anchor(self) -> XrefAnchorJobContext {
        XrefAnchorJobContext::new(self.job, self.anchor_checkpoint)
    }

    const fn traditional(self) -> TraditionalRevisionJobContext {
        TraditionalRevisionJobContext::new(self.job, self.traditional_checkpoint)
    }

    const fn stream(self) -> SourceXrefStreamJobContext {
        SourceXrefStreamJobContext::new(
            self.job,
            self.stream_envelope_checkpoint,
            self.stream_boundary_checkpoint,
            self.stream_payload_checkpoint,
            RequestPriority::Metadata,
        )
    }
}

/// Unvalidated aggregate limits owned by source revision-chain acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRevisionChainLimitConfig {
    /// Maximum cumulative exact bytes charged by every lower source read.
    pub max_total_read_bytes: u64,
    /// Maximum cumulative bytes charged by every lower parser.
    pub max_total_parse_bytes: u64,
    /// Maximum conservative heap bound for every proof and candidate retained at publication.
    pub max_retained_bound_bytes: u64,
}

impl Default for SourceRevisionChainLimitConfig {
    fn default() -> Self {
        Self {
            max_total_read_bytes: 512 * 1024 * 1024,
            max_total_parse_bytes: 512 * 1024 * 1024,
            max_retained_bound_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Validated aggregate limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRevisionChainLimits {
    max_total_read_bytes: u64,
    max_total_parse_bytes: u64,
    max_retained_bound_bytes: u64,
}

impl SourceRevisionChainLimits {
    /// Validates one aggregate source-chain profile.
    pub fn validate(
        config: SourceRevisionChainLimitConfig,
    ) -> Result<Self, SourceRevisionChainError> {
        if config.max_total_read_bytes == 0
            || config.max_total_read_bytes > HARD_MAX_TOTAL_WORK_BYTES
            || config.max_total_parse_bytes == 0
            || config.max_total_parse_bytes > HARD_MAX_TOTAL_WORK_BYTES
            || config.max_retained_bound_bytes == 0
            || config.max_retained_bound_bytes > HARD_MAX_RETAINED_BOUND_BYTES
        {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InvalidLimits,
                None,
            ));
        }
        Ok(Self {
            max_total_read_bytes: config.max_total_read_bytes,
            max_total_parse_bytes: config.max_total_parse_bytes,
            max_retained_bound_bytes: config.max_retained_bound_bytes,
        })
    }

    /// Returns the cumulative lower read ceiling.
    pub const fn max_total_read_bytes(self) -> u64 {
        self.max_total_read_bytes
    }

    /// Returns the cumulative lower parse ceiling.
    pub const fn max_total_parse_bytes(self) -> u64 {
        self.max_total_parse_bytes
    }

    /// Returns the conservative publication heap-retention ceiling.
    pub const fn max_retained_bound_bytes(self) -> u64 {
        self.max_retained_bound_bytes
    }
}

impl Default for SourceRevisionChainLimits {
    fn default() -> Self {
        Self::validate(SourceRevisionChainLimitConfig::default())
            .expect("built-in source revision-chain limits satisfy hard ceilings")
    }
}

/// Aggregate resource dimension rejected by source revision-chain acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRevisionChainLimitKind {
    /// Cumulative lower exact-read bytes.
    ReadBytes,
    /// Cumulative lower parse bytes.
    ParseBytes,
    /// Primary revision count.
    Revisions,
    /// Primary plus hybrid section count.
    Sections,
    /// Primary plus hybrid entry count.
    Entries,
    /// Conservative bound for all publication-retained proof and candidate heap storage.
    RetainedBoundBytes,
}

/// Structured aggregate resource-limit context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRevisionChainLimit {
    kind: SourceRevisionChainLimitKind,
    limit: u64,
    attempted: u64,
}

impl SourceRevisionChainLimit {
    /// Returns the rejected resource dimension.
    pub const fn kind(self) -> SourceRevisionChainLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the rejected cumulative amount.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable failure for complete source revision-chain acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRevisionChainErrorCode {
    /// Aggregate limits do not satisfy hard implementation ceilings.
    InvalidLimits,
    /// The six phase checkpoints are not pairwise distinct.
    InvalidJobContext,
    /// The polled byte source no longer matches the immutable snapshot.
    SnapshotMismatch,
    /// A lower tail, anchor, or traditional-section operation failed.
    XrefFailure,
    /// A lower source-framed xref-stream operation failed.
    SourceXrefStreamFailure,
    /// A classified anchor has the wrong kind for its primary or hybrid role.
    InvalidAnchorKind,
    /// `/Prev`, `/XRefStm`, or an already-visited anchor violates strict source geometry.
    InvalidChainGeometry,
    /// Final pure revision composition rejected the acquired candidates.
    RevisionFailure,
    /// Aggregate work, count, allocation, or retained-bound admission failed.
    ResourceLimit,
    /// The owning runtime cancelled acquisition.
    Cancelled,
    /// A checked internal state invariant could not be maintained.
    InternalState,
    /// A completed one-shot acquisition job was polled again.
    JobAlreadyComplete,
}

/// Coarse source revision-chain failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRevisionChainErrorCategory {
    /// Invalid caller configuration.
    Configuration,
    /// Immutable source identity or source availability failure.
    Source,
    /// Malformed source revision geometry or syntax.
    Syntax,
    /// A valid construct requires a later acquisition profile.
    Unsupported,
    /// Deterministic resource exhaustion.
    Resource,
    /// Normal cooperative cancellation.
    Cancellation,
    /// Internal implementation failure.
    Internal,
}

/// Stable recovery policy for source revision-chain acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRevisionChainRecoverability {
    /// Correct job checkpoints or limit configuration.
    CorrectConfiguration,
    /// Correct the source bytes or choose an explicit repair path.
    CorrectInput,
    /// Reopen against a newly bound immutable snapshot.
    ReopenSource,
    /// Retry the lower source operation while preserving snapshot identity.
    RetrySource,
    /// Reduce work or select an approved larger deterministic profile.
    ReduceWorkload,
    /// Select a later profile supporting the requested construct.
    UseSupportedFeature,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ErrorDetail {
    None,
    Limit(SourceRevisionChainLimit),
    Xref(XrefError),
    SourceXrefStream(SourceXrefStreamError),
    Revision(RevisionError),
}

/// Source-redacted error retaining complete lower-layer evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRevisionChainError {
    code: SourceRevisionChainErrorCode,
    category: SourceRevisionChainErrorCategory,
    recoverability: SourceRevisionChainRecoverability,
    diagnostic_id: &'static str,
    offset: Option<u64>,
    detail: ErrorDetail,
}

impl SourceRevisionChainError {
    const fn for_code(code: SourceRevisionChainErrorCode, offset: Option<u64>) -> Self {
        let (category, recoverability, diagnostic_id) = error_policy(code);
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            offset,
            detail: ErrorDetail::None,
        }
    }

    fn from_xref(error: XrefError) -> Self {
        let (category, recoverability) = xref_policy(error);
        Self {
            code: SourceRevisionChainErrorCode::XrefFailure,
            category,
            recoverability,
            diagnostic_id: "RPE-SOURCE-CHAIN-0004",
            offset: error.offset(),
            detail: ErrorDetail::Xref(error),
        }
    }

    fn from_stream(error: SourceXrefStreamError) -> Self {
        let (category, recoverability) = stream_policy(error);
        Self {
            code: SourceRevisionChainErrorCode::SourceXrefStreamFailure,
            category,
            recoverability,
            diagnostic_id: "RPE-SOURCE-CHAIN-0005",
            offset: error.offset(),
            detail: ErrorDetail::SourceXrefStream(error),
        }
    }

    fn from_revision(error: RevisionError) -> Self {
        let category = match error.category() {
            RevisionErrorCategory::Configuration => SourceRevisionChainErrorCategory::Configuration,
            RevisionErrorCategory::Source => SourceRevisionChainErrorCategory::Source,
            RevisionErrorCategory::Syntax => SourceRevisionChainErrorCategory::Syntax,
            RevisionErrorCategory::Resource => SourceRevisionChainErrorCategory::Resource,
            RevisionErrorCategory::Cancellation => SourceRevisionChainErrorCategory::Cancellation,
            RevisionErrorCategory::Internal => SourceRevisionChainErrorCategory::Internal,
        };
        Self {
            code: SourceRevisionChainErrorCode::RevisionFailure,
            category,
            recoverability: map_xref_recoverability(error.recoverability()),
            diagnostic_id: "RPE-SOURCE-CHAIN-0008",
            offset: error.startxref(),
            detail: ErrorDetail::Revision(error),
        }
    }

    const fn resource(kind: SourceRevisionChainLimitKind, limit: u64, attempted: u64) -> Self {
        Self {
            code: SourceRevisionChainErrorCode::ResourceLimit,
            category: SourceRevisionChainErrorCategory::Resource,
            recoverability: SourceRevisionChainRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-SOURCE-CHAIN-0009",
            offset: None,
            detail: ErrorDetail::Limit(SourceRevisionChainLimit {
                kind,
                limit,
                attempted,
            }),
        }
    }

    /// Returns the stable machine-readable failure code.
    pub const fn code(self) -> SourceRevisionChainErrorCode {
        self.code
    }

    /// Returns the stable coarse category.
    pub const fn category(self) -> SourceRevisionChainErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> SourceRevisionChainRecoverability {
        self.recoverability
    }

    /// Returns the source-redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the related physical anchor when known.
    pub const fn offset(self) -> Option<u64> {
        self.offset
    }

    /// Returns aggregate resource context when admission failed.
    pub const fn limit(self) -> Option<SourceRevisionChainLimit> {
        match self.detail {
            ErrorDetail::Limit(limit) => Some(limit),
            ErrorDetail::None
            | ErrorDetail::Xref(_)
            | ErrorDetail::SourceXrefStream(_)
            | ErrorDetail::Revision(_) => None,
        }
    }

    /// Returns the complete lower xref error when tail, anchor, or traditional parsing failed.
    pub const fn xref_error(self) -> Option<XrefError> {
        match self.detail {
            ErrorDetail::Xref(error) => Some(error),
            _ => None,
        }
    }

    /// Returns the complete lower source xref-stream error when stream acquisition failed.
    pub const fn source_xref_stream_error(self) -> Option<SourceXrefStreamError> {
        match self.detail {
            ErrorDetail::SourceXrefStream(error) => Some(error),
            _ => None,
        }
    }

    /// Returns the complete lower revision-composition error when composition failed.
    pub const fn revision_error(self) -> Option<RevisionError> {
        match self.detail {
            ErrorDetail::Revision(error) => Some(error),
            _ => None,
        }
    }
}

impl fmt::Display for SourceRevisionChainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)
    }
}

impl Error for SourceRevisionChainError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.detail {
            ErrorDetail::Xref(error) => Some(error),
            ErrorDetail::SourceXrefStream(error) => Some(error),
            ErrorDetail::Revision(error) => Some(error),
            ErrorDetail::None | ErrorDetail::Limit(_) => None,
        }
    }
}

const fn error_policy(
    code: SourceRevisionChainErrorCode,
) -> (
    SourceRevisionChainErrorCategory,
    SourceRevisionChainRecoverability,
    &'static str,
) {
    match code {
        SourceRevisionChainErrorCode::InvalidLimits => (
            SourceRevisionChainErrorCategory::Configuration,
            SourceRevisionChainRecoverability::CorrectConfiguration,
            "RPE-SOURCE-CHAIN-0001",
        ),
        SourceRevisionChainErrorCode::InvalidJobContext => (
            SourceRevisionChainErrorCategory::Configuration,
            SourceRevisionChainRecoverability::CorrectConfiguration,
            "RPE-SOURCE-CHAIN-0002",
        ),
        SourceRevisionChainErrorCode::SnapshotMismatch => (
            SourceRevisionChainErrorCategory::Source,
            SourceRevisionChainRecoverability::ReopenSource,
            "RPE-SOURCE-CHAIN-0003",
        ),
        SourceRevisionChainErrorCode::XrefFailure => (
            SourceRevisionChainErrorCategory::Internal,
            SourceRevisionChainRecoverability::DoNotRetry,
            "RPE-SOURCE-CHAIN-0004",
        ),
        SourceRevisionChainErrorCode::SourceXrefStreamFailure => (
            SourceRevisionChainErrorCategory::Internal,
            SourceRevisionChainRecoverability::DoNotRetry,
            "RPE-SOURCE-CHAIN-0005",
        ),
        SourceRevisionChainErrorCode::InvalidAnchorKind => (
            SourceRevisionChainErrorCategory::Syntax,
            SourceRevisionChainRecoverability::CorrectInput,
            "RPE-SOURCE-CHAIN-0006",
        ),
        SourceRevisionChainErrorCode::InvalidChainGeometry => (
            SourceRevisionChainErrorCategory::Syntax,
            SourceRevisionChainRecoverability::CorrectInput,
            "RPE-SOURCE-CHAIN-0007",
        ),
        SourceRevisionChainErrorCode::RevisionFailure => (
            SourceRevisionChainErrorCategory::Internal,
            SourceRevisionChainRecoverability::DoNotRetry,
            "RPE-SOURCE-CHAIN-0008",
        ),
        SourceRevisionChainErrorCode::ResourceLimit => (
            SourceRevisionChainErrorCategory::Resource,
            SourceRevisionChainRecoverability::ReduceWorkload,
            "RPE-SOURCE-CHAIN-0009",
        ),
        SourceRevisionChainErrorCode::Cancelled => (
            SourceRevisionChainErrorCategory::Cancellation,
            SourceRevisionChainRecoverability::AbandonOperation,
            "RPE-SOURCE-CHAIN-0010",
        ),
        SourceRevisionChainErrorCode::InternalState => (
            SourceRevisionChainErrorCategory::Internal,
            SourceRevisionChainRecoverability::DoNotRetry,
            "RPE-SOURCE-CHAIN-0011",
        ),
        SourceRevisionChainErrorCode::JobAlreadyComplete => (
            SourceRevisionChainErrorCategory::Configuration,
            SourceRevisionChainRecoverability::CorrectConfiguration,
            "RPE-SOURCE-CHAIN-0012",
        ),
    }
}

fn xref_policy(
    error: XrefError,
) -> (
    SourceRevisionChainErrorCategory,
    SourceRevisionChainRecoverability,
) {
    let category = match error.category() {
        XrefErrorCategory::Configuration => SourceRevisionChainErrorCategory::Configuration,
        XrefErrorCategory::Source => SourceRevisionChainErrorCategory::Source,
        XrefErrorCategory::Syntax => SourceRevisionChainErrorCategory::Syntax,
        XrefErrorCategory::Unsupported => SourceRevisionChainErrorCategory::Unsupported,
        XrefErrorCategory::Resource => SourceRevisionChainErrorCategory::Resource,
        XrefErrorCategory::Cancellation => SourceRevisionChainErrorCategory::Cancellation,
        XrefErrorCategory::Internal => SourceRevisionChainErrorCategory::Internal,
    };
    (category, map_xref_recoverability(error.recoverability()))
}

fn stream_policy(
    error: SourceXrefStreamError,
) -> (
    SourceRevisionChainErrorCategory,
    SourceRevisionChainRecoverability,
) {
    let category = match error.category() {
        SourceXrefStreamErrorCategory::Configuration => {
            SourceRevisionChainErrorCategory::Configuration
        }
        SourceXrefStreamErrorCategory::Source => SourceRevisionChainErrorCategory::Source,
        SourceXrefStreamErrorCategory::Syntax => SourceRevisionChainErrorCategory::Syntax,
        SourceXrefStreamErrorCategory::Unsupported => SourceRevisionChainErrorCategory::Unsupported,
        SourceXrefStreamErrorCategory::Resource => SourceRevisionChainErrorCategory::Resource,
        SourceXrefStreamErrorCategory::Cancellation => {
            SourceRevisionChainErrorCategory::Cancellation
        }
        SourceXrefStreamErrorCategory::Internal => SourceRevisionChainErrorCategory::Internal,
    };
    let recoverability = match error.recoverability() {
        SourceXrefStreamRecoverability::CorrectConfiguration => {
            SourceRevisionChainRecoverability::CorrectConfiguration
        }
        SourceXrefStreamRecoverability::CorrectInput => {
            SourceRevisionChainRecoverability::CorrectInput
        }
        SourceXrefStreamRecoverability::ReopenSource => {
            SourceRevisionChainRecoverability::ReopenSource
        }
        SourceXrefStreamRecoverability::RetrySource => {
            SourceRevisionChainRecoverability::RetrySource
        }
        SourceXrefStreamRecoverability::ReduceWorkload => {
            SourceRevisionChainRecoverability::ReduceWorkload
        }
        SourceXrefStreamRecoverability::UseSupportedFeature => {
            SourceRevisionChainRecoverability::UseSupportedFeature
        }
        SourceXrefStreamRecoverability::AbandonOperation => {
            SourceRevisionChainRecoverability::AbandonOperation
        }
        SourceXrefStreamRecoverability::DoNotRetry => SourceRevisionChainRecoverability::DoNotRetry,
    };
    (category, recoverability)
}

const fn map_xref_recoverability(value: XrefRecoverability) -> SourceRevisionChainRecoverability {
    match value {
        XrefRecoverability::CorrectConfiguration => {
            SourceRevisionChainRecoverability::CorrectConfiguration
        }
        XrefRecoverability::CorrectInput => SourceRevisionChainRecoverability::CorrectInput,
        XrefRecoverability::ReopenSource => SourceRevisionChainRecoverability::ReopenSource,
        XrefRecoverability::RetrySource => SourceRevisionChainRecoverability::RetrySource,
        XrefRecoverability::ReduceWorkload => SourceRevisionChainRecoverability::ReduceWorkload,
        XrefRecoverability::UseSupportedFeature => {
            SourceRevisionChainRecoverability::UseSupportedFeature
        }
        XrefRecoverability::AbandonOperation => SourceRevisionChainRecoverability::AbandonOperation,
        XrefRecoverability::DoNotRetry => SourceRevisionChainRecoverability::DoNotRetry,
    }
}

/// Cooperative cancellation probe supplied by the owning runtime.
pub trait SourceRevisionChainCancellation: Send + Sync {
    /// Reports whether acquisition must stop at the next bounded probe.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation probe that never requests cancellation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NeverCancelSourceRevisionChain;

impl SourceRevisionChainCancellation for NeverCancelSourceRevisionChain {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl SourceRevisionChainCancellation for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

struct CancellationAdapter<'a>(&'a dyn SourceRevisionChainCancellation);

impl XrefCancellation for CancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl SourceXrefStreamCancellation for CancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// Source-retained primary section for one acquired revision.
#[allow(
    clippy::large_enum_variant,
    reason = "move-only lower proofs remain inline so every retained allocation stays accounted"
)]
#[derive(Debug, Eq, PartialEq)]
pub enum SourceRevisionPrimaryProof {
    /// A classified traditional anchor and its complete parsed source section.
    Traditional {
        /// Exact classified primary anchor.
        anchor: XrefAnchor,
        /// Original source-bound traditional section.
        section: TraditionalRevisionSection,
    },
    /// A classified stream-object anchor and its framed unfiltered xref-stream proof.
    Stream {
        /// Exact classified primary anchor.
        anchor: XrefAnchor,
        /// Original source-framed stream proof.
        section: SourceAcquiredXrefStream,
    },
}

impl SourceRevisionPrimaryProof {
    /// Returns the exact classified primary anchor.
    pub const fn anchor(&self) -> XrefAnchor {
        match self {
            Self::Traditional { anchor, .. } | Self::Stream { anchor, .. } => *anchor,
        }
    }

    /// Returns the primary representation.
    pub const fn kind(&self) -> RevisionPrimaryKind {
        match self {
            Self::Traditional { .. } => RevisionPrimaryKind::Traditional,
            Self::Stream { .. } => RevisionPrimaryKind::Stream,
        }
    }

    /// Borrows the original traditional section when this primary is textual.
    pub const fn traditional(&self) -> Option<&TraditionalRevisionSection> {
        match self {
            Self::Traditional { section, .. } => Some(section),
            Self::Stream { .. } => None,
        }
    }

    /// Borrows the proof-bound stream acquisition when this primary is a stream.
    pub const fn stream(&self) -> Option<&SourceAcquiredXrefStream> {
        match self {
            Self::Stream { section, .. } => Some(section),
            Self::Traditional { .. } => None,
        }
    }
}

/// Source-retained hybrid xref-stream supplement.
#[derive(Debug, Eq, PartialEq)]
pub struct SourceHybridRevisionProof {
    anchor: XrefAnchor,
    section: SourceAcquiredXrefStream,
}

impl SourceHybridRevisionProof {
    /// Returns the exact classified `/XRefStm` anchor.
    pub const fn anchor(&self) -> XrefAnchor {
        self.anchor
    }

    /// Borrows the proof-bound source-framed supplement.
    pub const fn section(&self) -> &SourceAcquiredXrefStream {
        &self.section
    }
}

/// Complete raw source evidence for one newest-to-oldest revision.
#[derive(Debug, Eq, PartialEq)]
pub struct SourceRevisionProof {
    primary: SourceRevisionPrimaryProof,
    hybrid: Option<SourceHybridRevisionProof>,
}

impl SourceRevisionProof {
    /// Borrows the classified and parsed primary proof.
    pub const fn primary(&self) -> &SourceRevisionPrimaryProof {
        &self.primary
    }

    /// Borrows the optional same-revision hybrid supplement proof.
    pub const fn hybrid(&self) -> Option<&SourceHybridRevisionProof> {
        self.hybrid.as_ref()
    }
}

/// Cumulative aggregate work and conservative retained-publication evidence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceRevisionChainStats {
    read_bytes: u64,
    parse_bytes: u64,
    max_admitted_read_bytes: u64,
    max_admitted_parse_bytes: u64,
    max_admitted_retained_bound_bytes: u64,
    anchor_jobs: u32,
    traditional_jobs: u32,
    stream_jobs: u32,
    revisions: u32,
    sections: u32,
    entries: u64,
    retained_bound_bytes: u64,
    chain: Option<RevisionStats>,
}

impl SourceRevisionChainStats {
    /// Returns cumulative exact bytes charged by all lower source reads.
    pub const fn read_bytes(self) -> u64 {
        self.read_bytes
    }

    /// Returns cumulative bytes charged by all lower parsers.
    pub const fn parse_bytes(self) -> u64 {
        self.parse_bytes
    }

    /// Returns the highest cumulative read amount admitted before any lower child work.
    pub const fn max_admitted_read_bytes(self) -> u64 {
        self.max_admitted_read_bytes
    }

    /// Returns the highest cumulative parse amount admitted before any lower child work.
    pub const fn max_admitted_parse_bytes(self) -> u64 {
        self.max_admitted_parse_bytes
    }

    /// Returns the highest persistent-plus-active retained bound admitted before child work.
    pub const fn max_admitted_retained_bound_bytes(self) -> u64 {
        self.max_admitted_retained_bound_bytes
    }

    /// Returns the number of sequential primary and hybrid anchor jobs installed.
    pub const fn anchor_jobs(self) -> u32 {
        self.anchor_jobs
    }

    /// Returns the number of traditional section jobs installed.
    pub const fn traditional_jobs(self) -> u32 {
        self.traditional_jobs
    }

    /// Returns the number of primary or hybrid stream jobs installed.
    pub const fn stream_jobs(self) -> u32 {
        self.stream_jobs
    }

    /// Returns primary revisions admitted for publication.
    pub const fn revisions(self) -> u32 {
        self.revisions
    }

    /// Returns primary sections plus hybrid supplements admitted for publication.
    pub const fn sections(self) -> u32 {
        self.sections
    }

    /// Returns primary plus hybrid entries admitted for composition.
    pub const fn entries(self) -> u64 {
        self.entries
    }

    /// Returns the conservative checked upper bound for all publication-retained heap storage.
    ///
    /// This is deliberately not allocator-exact: traditional trailer ownership lacks a lower
    /// capacity metric, so each such proof reserves its physical section span plus the configured
    /// syntax-owned and container ceilings and the complete configured xref-entry ceiling.
    pub const fn retained_bound_bytes(self) -> u64 {
        self.retained_bound_bytes
    }

    /// Returns pure composition accounting after the chain is validated.
    pub const fn chain(self) -> Option<RevisionStats> {
        self.chain
    }
}

/// Move-only complete source acquisition proof and composed revision semantics.
#[derive(Eq, PartialEq)]
pub struct SourceAcquiredRevisionChain {
    final_marker: FinalStartXref,
    proofs: Vec<SourceRevisionProof>,
    chain: RevisionChain,
    stats: SourceRevisionChainStats,
}

impl SourceAcquiredRevisionChain {
    /// Returns the immutable source snapshot shared by every retained proof.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.chain.snapshot()
    }

    /// Returns the source-bound final `startxref` marker proof.
    pub const fn final_marker(&self) -> FinalStartXref {
        self.final_marker
    }

    /// Borrows raw revision proofs from newest to oldest.
    pub fn proofs(&self) -> &[SourceRevisionProof] {
        &self.proofs
    }

    /// Returns the effective inherited document root.
    pub const fn root(&self) -> ObjectRef {
        self.chain.root()
    }

    /// Looks up one latest-wins entry without lending the cloneable naked chain.
    pub fn entry(&self, object_number: u32) -> Option<ResolvedXrefEntry> {
        self.chain.entry(object_number)
    }

    /// Returns aggregate acquisition, retention-bound, and composition accounting.
    pub const fn stats(&self) -> SourceRevisionChainStats {
        self.stats
    }

    /// Borrows the chain only for proof-preserving document composition inside this crate.
    #[allow(
        dead_code,
        reason = "the future revision-aware opener will consume this internal proof"
    )]
    pub(crate) const fn revision_chain(&self) -> &RevisionChain {
        &self.chain
    }
}

impl fmt::Debug for SourceAcquiredRevisionChain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceAcquiredRevisionChain")
            .field("final_marker", &self.final_marker)
            .field("proof_count", &self.proofs.len())
            .field("chain", &self.chain)
            .field("stats", &self.stats)
            .finish()
    }
}

/// Coarse phase of complete source revision-chain acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRevisionChainPhase {
    /// Discovering the final `startxref` marker.
    Tail,
    /// Classifying one primary or hybrid anchor.
    Anchor,
    /// Parsing one traditional primary section.
    Traditional,
    /// Framing and parsing one primary or hybrid xref stream.
    Stream,
    /// Validating the pure newest-to-oldest chain composition.
    Compose,
    /// The move-only source proof was returned.
    Complete,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Result of polling one complete source revision-chain job.
#[allow(
    clippy::large_enum_variant,
    reason = "one-shot source proof remains inline and move-only"
)]
#[derive(Debug, Eq, PartialEq)]
pub enum SourceRevisionChainPoll {
    /// Complete raw source proof and composed latest-wins semantics are ready.
    Ready(SourceAcquiredRevisionChain),
    /// Exactly one lower source request is waiting for bytes.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges missing from the active request.
        missing: SmallRanges,
        /// Exact child checkpoint to retain while requeueing the caller job.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a stable structured failure.
    Failed(SourceRevisionChainError),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ObservedWork {
    read_bytes: u64,
    parse_bytes: u64,
}

enum StreamRole {
    Primary,
    Hybrid,
}

struct PendingTraditional {
    anchor: XrefAnchor,
    section: TraditionalRevisionSection,
    candidate: RevisionCandidate,
}

#[allow(
    clippy::large_enum_variant,
    reason = "active lower jobs remain inline without hidden allocation"
)]
enum JobState {
    Tail {
        job: OpenFinalStartXrefJob,
        observed: ObservedWork,
    },
    Anchor {
        job: OpenXrefAnchorJob,
        observed: ObservedWork,
        startxref: u64,
        upper_bound: u64,
        hybrid: bool,
    },
    Traditional {
        job: OpenTraditionalRevisionJob,
        observed: ObservedWork,
        anchor: XrefAnchor,
    },
    Stream {
        job: OpenSourceXrefStreamJob,
        observed: ObservedWork,
        anchor: XrefAnchor,
        role: StreamRole,
    },
    Compose,
    Transition,
    Complete,
    Failed(SourceRevisionChainError),
}

/// One-shot bounded source job for unfiltered direct-Length mixed revision chains.
pub struct OpenSourceRevisionChainJob {
    snapshot: SourceSnapshot,
    context: SourceRevisionChainJobContext,
    limits: SourceRevisionChainLimits,
    xref_limits: XrefLimits,
    anchor_limits: XrefAnchorLimits,
    object_limits: ObjectLimits,
    syntax_limits: SyntaxLimits,
    xref_stream_limits: XrefStreamLimits,
    revision_limits: RevisionLimits,
    final_marker: Option<FinalStartXref>,
    proofs: Vec<SourceRevisionProof>,
    candidates: Vec<RevisionCandidate>,
    pending_traditional: Option<PendingTraditional>,
    active_work_reservation: ObservedWork,
    active_retained_reservation: u64,
    stats: SourceRevisionChainStats,
    state: JobState,
}

impl OpenSourceRevisionChainJob {
    /// Validates parent configuration and starts final-marker discovery.
    #[allow(
        clippy::too_many_arguments,
        reason = "every independent lower and parent profile is explicit"
    )]
    pub fn new(
        snapshot: SourceSnapshot,
        context: SourceRevisionChainJobContext,
        limits: SourceRevisionChainLimits,
        xref_limits: XrefLimits,
        anchor_limits: XrefAnchorLimits,
        object_limits: ObjectLimits,
        syntax_limits: SyntaxLimits,
        xref_stream_limits: XrefStreamLimits,
        revision_limits: RevisionLimits,
    ) -> Result<Self, SourceRevisionChainError> {
        if !context.is_valid() {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InvalidJobContext,
                None,
            ));
        }
        let tail = OpenFinalStartXrefJob::new(snapshot, context.tail(), xref_limits)
            .map_err(SourceRevisionChainError::from_xref)?;
        let source_len = snapshot.len().ok_or_else(|| {
            SourceRevisionChainError::for_code(SourceRevisionChainErrorCode::InternalState, None)
        })?;
        let tail_cap = xref_limits.max_tail_bytes().min(source_len);
        let tail_read = window_work_bound(
            xref_limits.initial_tail_bytes().min(source_len),
            tail_cap,
            xref_limits.max_total_read_bytes(),
        )?;
        let tail_parse = window_work_bound(
            xref_limits.initial_tail_bytes().min(source_len),
            tail_cap,
            xref_limits.max_total_parse_bytes(),
        )?;
        let mut result = Self {
            snapshot,
            context,
            limits,
            xref_limits,
            anchor_limits,
            object_limits,
            syntax_limits,
            xref_stream_limits,
            revision_limits,
            final_marker: None,
            proofs: Vec::new(),
            candidates: Vec::new(),
            pending_traditional: None,
            active_work_reservation: ObservedWork::default(),
            active_retained_reservation: 0,
            stats: SourceRevisionChainStats::default(),
            state: JobState::Tail {
                job: tail,
                observed: ObservedWork::default(),
            },
        };
        result.reserve_active_child(tail_read, tail_parse, 0)?;
        Ok(result)
    }

    /// Returns the immutable source snapshot bound at construction.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the single job identity and pairwise-distinct checkpoints.
    pub const fn context(&self) -> SourceRevisionChainJobContext {
        self.context
    }

    /// Returns the aggregate acquisition and retained-bound profile.
    pub const fn limits(&self) -> SourceRevisionChainLimits {
        self.limits
    }

    /// Returns cumulative work and conservative retained-publication evidence.
    pub const fn stats(&self) -> SourceRevisionChainStats {
        self.stats
    }

    /// Returns the current coarse phase.
    pub const fn phase(&self) -> SourceRevisionChainPhase {
        match self.state {
            JobState::Tail { .. } => SourceRevisionChainPhase::Tail,
            JobState::Anchor { .. } => SourceRevisionChainPhase::Anchor,
            JobState::Traditional { .. } => SourceRevisionChainPhase::Traditional,
            JobState::Stream { .. } => SourceRevisionChainPhase::Stream,
            JobState::Compose => SourceRevisionChainPhase::Compose,
            JobState::Complete => SourceRevisionChainPhase::Complete,
            JobState::Transition | JobState::Failed(_) => SourceRevisionChainPhase::Failed,
        }
    }

    /// Advances acquisition without performing host I/O or exposing incomplete chain semantics.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn SourceRevisionChainCancellation + '_),
    ) -> SourceRevisionChainPoll {
        if let JobState::Failed(error) = self.state {
            return SourceRevisionChainPoll::Failed(error);
        }
        if matches!(self.state, JobState::Complete) {
            return SourceRevisionChainPoll::Failed(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::JobAlreadyComplete,
                None,
            ));
        }
        if source.snapshot() != self.snapshot {
            return self.fail(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::SnapshotMismatch,
                None,
            ));
        }
        if cancellation.is_cancelled() {
            return self.fail(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::Cancelled,
                None,
            ));
        }
        let adapter = CancellationAdapter(cancellation);

        loop {
            let state = mem::replace(&mut self.state, JobState::Transition);
            match state {
                JobState::Tail {
                    mut job,
                    mut observed,
                } => {
                    let outcome = job.poll(source, &adapter);
                    let child = job.stats();
                    if let Err(error) =
                        self.observe_work(&mut observed, child.read_bytes(), child.parse_bytes())
                    {
                        return self.fail(error);
                    }
                    match outcome {
                        FinalStartXrefPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = JobState::Tail { job, observed };
                            return SourceRevisionChainPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        FinalStartXrefPoll::Failed(error) => {
                            return self.fail(SourceRevisionChainError::from_xref(error));
                        }
                        FinalStartXrefPoll::Ready(marker) => {
                            if let Err(error) = self.complete_active_child(0) {
                                return self.fail(error);
                            }
                            let startxref = marker.startxref();
                            let upper_bound = marker.tail_start();
                            if startxref == 0 || startxref >= upper_bound {
                                return self.fail(SourceRevisionChainError::for_code(
                                    SourceRevisionChainErrorCode::InvalidChainGeometry,
                                    Some(startxref),
                                ));
                            }
                            self.final_marker = Some(marker);
                            if let Err(error) = self.start_anchor(startxref, upper_bound, false) {
                                return self.fail(error);
                            }
                        }
                    }
                }
                JobState::Anchor {
                    mut job,
                    mut observed,
                    startxref,
                    upper_bound,
                    hybrid,
                } => {
                    let outcome = job.poll(source, &adapter);
                    let child = job.stats();
                    if let Err(error) =
                        self.observe_work(&mut observed, child.read_bytes(), child.parse_bytes())
                    {
                        return self.fail(error);
                    }
                    match outcome {
                        XrefAnchorPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = JobState::Anchor {
                                job,
                                observed,
                                startxref,
                                upper_bound,
                                hybrid,
                            };
                            return SourceRevisionChainPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        XrefAnchorPoll::Failed(error) => {
                            return self.fail(SourceRevisionChainError::from_xref(error));
                        }
                        XrefAnchorPoll::Ready(anchor) => {
                            if let Err(error) = self.complete_active_child(0) {
                                return self.fail(error);
                            }
                            if hybrid {
                                let XrefAnchorKind::StreamObject(container) = anchor.kind() else {
                                    return self.fail(SourceRevisionChainError::for_code(
                                        SourceRevisionChainErrorCode::InvalidAnchorKind,
                                        Some(startxref),
                                    ));
                                };
                                let Some(pending) = self.pending_traditional.as_ref() else {
                                    return self.fail_internal(Some(startxref));
                                };
                                let revision_startxref = pending.anchor.startxref();
                                if let Err(error) = self.start_stream(
                                    anchor,
                                    container,
                                    upper_bound,
                                    revision_startxref,
                                    StreamRole::Hybrid,
                                ) {
                                    return self.fail(error);
                                }
                            } else {
                                match anchor.kind() {
                                    XrefAnchorKind::Traditional => {
                                        if let Err(error) =
                                            self.start_traditional(anchor, upper_bound)
                                        {
                                            return self.fail(error);
                                        }
                                    }
                                    XrefAnchorKind::StreamObject(container) => {
                                        if let Err(error) = self.start_stream(
                                            anchor,
                                            container,
                                            upper_bound,
                                            startxref,
                                            StreamRole::Primary,
                                        ) {
                                            return self.fail(error);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                JobState::Traditional {
                    mut job,
                    mut observed,
                    anchor,
                } => {
                    let outcome = job.poll(source, &adapter);
                    let child = job.stats();
                    if let Err(error) =
                        self.observe_work(&mut observed, child.read_bytes(), child.parse_bytes())
                    {
                        return self.fail(error);
                    }
                    match outcome {
                        TraditionalRevisionPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = JobState::Traditional {
                                job,
                                observed,
                                anchor,
                            };
                            return SourceRevisionChainPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        TraditionalRevisionPoll::Failed(error) => {
                            return self.fail(SourceRevisionChainError::from_xref(error));
                        }
                        TraditionalRevisionPoll::Ready(section) => {
                            let previous = section.previous();
                            let hybrid_anchor = section.xref_stream();
                            if let Err(error) = self.validate_previous_geometry(
                                previous,
                                anchor.startxref(),
                                cancellation,
                            ) {
                                return self.fail(error);
                            }
                            if let Some(hybrid_anchor) = hybrid_anchor
                                && let Err(error) = self.validate_hybrid_geometry(
                                    hybrid_anchor,
                                    anchor.startxref(),
                                    previous,
                                    cancellation,
                                )
                            {
                                return self.fail(error);
                            }
                            if let Err(error) = self.admit_section(section.entries().len(), true) {
                                return self.fail(error);
                            }
                            let retained_bound = match self.traditional_bound(&section) {
                                Ok(bound) => bound,
                                Err(error) => return self.fail(error),
                            };
                            if let Err(error) = self.complete_active_child(retained_bound) {
                                return self.fail(error);
                            }
                            let entries = match self.copy_entries(section.entries(), cancellation) {
                                Ok(entries) => entries,
                                Err(error) => return self.fail(error),
                            };
                            let mut candidate = RevisionCandidate::traditional(
                                self.snapshot,
                                anchor.startxref(),
                                section.declared_size(),
                                section.root(),
                                previous,
                                entries,
                            );
                            if let Some(hybrid_anchor) = hybrid_anchor {
                                candidate = candidate.with_xref_stream(hybrid_anchor);
                                self.pending_traditional = Some(PendingTraditional {
                                    anchor,
                                    section,
                                    candidate,
                                });
                                if let Err(error) =
                                    self.start_anchor(hybrid_anchor, anchor.startxref(), true)
                                {
                                    return self.fail(error);
                                }
                            } else {
                                let proof = SourceRevisionProof {
                                    primary: SourceRevisionPrimaryProof::Traditional {
                                        anchor,
                                        section,
                                    },
                                    hybrid: None,
                                };
                                if let Err(error) = self.finish_revision(
                                    proof,
                                    candidate,
                                    previous,
                                    anchor.startxref(),
                                    cancellation,
                                ) {
                                    return self.fail(error);
                                }
                            }
                        }
                    }
                }
                JobState::Stream {
                    mut job,
                    mut observed,
                    anchor,
                    role,
                } => {
                    let outcome = job.poll(source, &adapter);
                    let child = job.stats();
                    let parse = child
                        .object()
                        .parse_bytes()
                        .checked_add(child.xref_stream().map_or(0, |stats| stats.decoded_bytes()));
                    let read = child
                        .object()
                        .read_bytes()
                        .checked_add(child.payload_read_bytes());
                    let (Some(read), Some(parse)) = (read, parse) else {
                        return self.fail_internal(Some(anchor.startxref()));
                    };
                    if let Err(error) = self.observe_work(&mut observed, read, parse) {
                        return self.fail(error);
                    }
                    match outcome {
                        SourceXrefStreamPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = JobState::Stream {
                                job,
                                observed,
                                anchor,
                                role,
                            };
                            return SourceRevisionChainPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        SourceXrefStreamPoll::Failed(error) => {
                            return self.fail(SourceRevisionChainError::from_stream(error));
                        }
                        SourceXrefStreamPoll::Ready(section) => {
                            if let Err(error) =
                                self.complete_active_child(section.stats().retained_proof_bytes())
                            {
                                return self.fail(error);
                            }
                            match role {
                                StreamRole::Primary => {
                                    let previous = section.previous();
                                    if let Err(error) = self.validate_previous_geometry(
                                        previous,
                                        anchor.startxref(),
                                        cancellation,
                                    ) {
                                        return self.fail(error);
                                    }
                                    if let Err(error) =
                                        self.admit_section(section.entries().len(), false)
                                    {
                                        return self.fail(error);
                                    }
                                    let entries =
                                        match self.copy_entries(section.entries(), cancellation) {
                                            Ok(entries) => entries,
                                            Err(error) => return self.fail(error),
                                        };
                                    let candidate = RevisionCandidate::xref_stream(
                                        self.snapshot,
                                        anchor.startxref(),
                                        section.container(),
                                        section.declared_size(),
                                        section.root(),
                                        previous,
                                        entries,
                                    );
                                    let proof = SourceRevisionProof {
                                        primary: SourceRevisionPrimaryProof::Stream {
                                            anchor,
                                            section,
                                        },
                                        hybrid: None,
                                    };
                                    if let Err(error) = self.finish_revision(
                                        proof,
                                        candidate,
                                        previous,
                                        anchor.startxref(),
                                        cancellation,
                                    ) {
                                        return self.fail(error);
                                    }
                                }
                                StreamRole::Hybrid => {
                                    if let Err(error) =
                                        self.admit_section(section.entries().len(), false)
                                    {
                                        return self.fail(error);
                                    }
                                    let entries =
                                        match self.copy_entries(section.entries(), cancellation) {
                                            Ok(entries) => entries,
                                            Err(error) => return self.fail(error),
                                        };
                                    let Some(pending) = self.pending_traditional.take() else {
                                        return self.fail_internal(Some(anchor.startxref()));
                                    };
                                    let previous = pending.candidate.previous();
                                    let primary_startxref = pending.anchor.startxref();
                                    let supplement = HybridSupplement::new(
                                        self.snapshot,
                                        anchor.startxref(),
                                        section.container(),
                                        section.declared_size(),
                                        section.previous(),
                                        entries,
                                    );
                                    let candidate =
                                        pending.candidate.with_hybrid_supplement(supplement);
                                    let proof = SourceRevisionProof {
                                        primary: SourceRevisionPrimaryProof::Traditional {
                                            anchor: pending.anchor,
                                            section: pending.section,
                                        },
                                        hybrid: Some(SourceHybridRevisionProof { anchor, section }),
                                    };
                                    if let Err(error) = self.finish_revision(
                                        proof,
                                        candidate,
                                        previous,
                                        primary_startxref,
                                        cancellation,
                                    ) {
                                        return self.fail(error);
                                    }
                                }
                            }
                        }
                    }
                }
                JobState::Compose => {
                    if self.active_work_reservation != ObservedWork::default()
                        || self.active_retained_reservation != 0
                    {
                        return self.fail_internal(None);
                    }
                    if cancellation.is_cancelled() {
                        return self.fail(SourceRevisionChainError::for_code(
                            SourceRevisionChainErrorCode::Cancelled,
                            None,
                        ));
                    }
                    let candidates = mem::take(&mut self.candidates);
                    let chain =
                        match compose_revision_chain(candidates, self.revision_limits, &adapter) {
                            Ok(chain) => chain,
                            Err(error) => {
                                return self.fail(SourceRevisionChainError::from_revision(error));
                            }
                        };
                    if chain.stats().retained_bytes() > self.stats.retained_bound_bytes {
                        return self.fail_internal(None);
                    }
                    self.stats.chain = Some(chain.stats());
                    let Some(final_marker) = self.final_marker.take() else {
                        return self.fail_internal(None);
                    };
                    let proofs = mem::take(&mut self.proofs);
                    self.state = JobState::Complete;
                    return SourceRevisionChainPoll::Ready(SourceAcquiredRevisionChain {
                        final_marker,
                        proofs,
                        chain,
                        stats: self.stats,
                    });
                }
                JobState::Complete => {
                    return SourceRevisionChainPoll::Failed(SourceRevisionChainError::for_code(
                        SourceRevisionChainErrorCode::JobAlreadyComplete,
                        None,
                    ));
                }
                JobState::Failed(error) => return SourceRevisionChainPoll::Failed(error),
                JobState::Transition => return self.fail_internal(None),
            }
        }
    }

    fn start_anchor(
        &mut self,
        startxref: u64,
        upper_bound: u64,
        hybrid: bool,
    ) -> Result<(), SourceRevisionChainError> {
        if startxref == 0 || startxref >= upper_bound {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InvalidChainGeometry,
                Some(startxref),
            ));
        }
        if hybrid {
            if self.stats.sections >= self.revision_limits.max_sections() {
                return Err(SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::Sections,
                    u64::from(self.revision_limits.max_sections()),
                    u64::from(self.stats.sections) + 1,
                ));
            }
        } else {
            if self.stats.revisions >= self.revision_limits.max_revisions() {
                return Err(SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::Revisions,
                    u64::from(self.revision_limits.max_revisions()),
                    u64::from(self.stats.revisions) + 1,
                ));
            }
            if self.stats.sections >= self.revision_limits.max_sections() {
                return Err(SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::Sections,
                    u64::from(self.revision_limits.max_sections()),
                    u64::from(self.stats.sections) + 1,
                ));
            }
        }
        let job = OpenXrefAnchorJob::new(
            self.snapshot,
            startxref,
            upper_bound,
            self.context.anchor(),
            self.anchor_limits,
        )
        .map_err(SourceRevisionChainError::from_xref)?;
        let anchor_bytes = upper_bound
            .checked_sub(startxref)
            .map(|available| available.min(self.anchor_limits.max_anchor_bytes()))
            .ok_or_else(|| {
                SourceRevisionChainError::for_code(
                    SourceRevisionChainErrorCode::InternalState,
                    Some(startxref),
                )
            })?;
        self.reserve_active_child(anchor_bytes, anchor_bytes, 0)?;
        self.stats.anchor_jobs = self.stats.anchor_jobs.checked_add(1).ok_or_else(|| {
            SourceRevisionChainError::for_code(SourceRevisionChainErrorCode::InternalState, None)
        })?;
        self.state = JobState::Anchor {
            job,
            observed: ObservedWork::default(),
            startxref,
            upper_bound,
            hybrid,
        };
        Ok(())
    }

    fn start_traditional(
        &mut self,
        anchor: XrefAnchor,
        upper_bound: u64,
    ) -> Result<(), SourceRevisionChainError> {
        let job = OpenTraditionalRevisionJob::new(
            self.snapshot,
            anchor.startxref(),
            upper_bound,
            self.context.traditional(),
            self.xref_limits,
            self.syntax_limits,
        )
        .map_err(SourceRevisionChainError::from_xref)?;
        let available = upper_bound.checked_sub(anchor.startxref()).ok_or_else(|| {
            SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InternalState,
                Some(anchor.startxref()),
            )
        })?;
        let section_cap = self.xref_limits.max_section_bytes().min(available);
        let initial = self.xref_limits.initial_section_bytes().min(available);
        let read_bound = window_work_bound(
            initial,
            section_cap,
            self.xref_limits.max_total_read_bytes(),
        )?;
        let parse_bound = window_work_bound(
            initial,
            section_cap,
            self.xref_limits.max_total_parse_bytes(),
        )?;
        let retained_bound = self.traditional_ceiling_bound(section_cap)?;
        self.reserve_active_child(read_bound, parse_bound, retained_bound)?;
        self.stats.traditional_jobs =
            self.stats.traditional_jobs.checked_add(1).ok_or_else(|| {
                SourceRevisionChainError::for_code(
                    SourceRevisionChainErrorCode::InternalState,
                    None,
                )
            })?;
        self.state = JobState::Traditional {
            job,
            observed: ObservedWork::default(),
            anchor,
        };
        Ok(())
    }

    fn start_stream(
        &mut self,
        anchor: XrefAnchor,
        container: ObjectRef,
        object_upper_bound: u64,
        revision_startxref: u64,
        role: StreamRole,
    ) -> Result<(), SourceRevisionChainError> {
        let job = OpenSourceXrefStreamJob::new(
            self.snapshot,
            container,
            anchor.startxref(),
            object_upper_bound,
            revision_startxref,
            self.context.stream(),
            self.object_limits,
            self.syntax_limits,
            self.xref_stream_limits,
        )
        .map_err(SourceRevisionChainError::from_stream)?;
        let (read_bound, parse_bound, retained_bound) =
            self.stream_child_bounds(anchor.startxref(), object_upper_bound)?;
        self.reserve_active_child(read_bound, parse_bound, retained_bound)?;
        self.stats.stream_jobs = self.stats.stream_jobs.checked_add(1).ok_or_else(|| {
            SourceRevisionChainError::for_code(SourceRevisionChainErrorCode::InternalState, None)
        })?;
        self.state = JobState::Stream {
            job,
            observed: ObservedWork::default(),
            anchor,
            role,
        };
        Ok(())
    }

    fn observe_work(
        &mut self,
        observed: &mut ObservedWork,
        read_bytes: u64,
        parse_bytes: u64,
    ) -> Result<(), SourceRevisionChainError> {
        if read_bytes > self.active_work_reservation.read_bytes
            || parse_bytes > self.active_work_reservation.parse_bytes
        {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InternalState,
                None,
            ));
        }
        let read_delta = read_bytes.checked_sub(observed.read_bytes).ok_or_else(|| {
            SourceRevisionChainError::for_code(SourceRevisionChainErrorCode::InternalState, None)
        })?;
        let parse_delta = parse_bytes
            .checked_sub(observed.parse_bytes)
            .ok_or_else(|| {
                SourceRevisionChainError::for_code(
                    SourceRevisionChainErrorCode::InternalState,
                    None,
                )
            })?;
        let read_total = self
            .stats
            .read_bytes
            .checked_add(read_delta)
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::ReadBytes,
                    self.limits.max_total_read_bytes,
                    u64::MAX,
                )
            })?;
        if read_total > self.limits.max_total_read_bytes {
            return Err(SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::ReadBytes,
                self.limits.max_total_read_bytes,
                read_total,
            ));
        }
        let parse_total = self
            .stats
            .parse_bytes
            .checked_add(parse_delta)
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::ParseBytes,
                    self.limits.max_total_parse_bytes,
                    u64::MAX,
                )
            })?;
        if parse_total > self.limits.max_total_parse_bytes {
            return Err(SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::ParseBytes,
                self.limits.max_total_parse_bytes,
                parse_total,
            ));
        }
        self.stats.read_bytes = read_total;
        self.stats.parse_bytes = parse_total;
        observed.read_bytes = read_bytes;
        observed.parse_bytes = parse_bytes;
        Ok(())
    }

    fn reserve_active_child(
        &mut self,
        read_bytes: u64,
        parse_bytes: u64,
        retained_bound_bytes: u64,
    ) -> Result<(), SourceRevisionChainError> {
        if self.active_work_reservation != ObservedWork::default()
            || self.active_retained_reservation != 0
        {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InternalState,
                None,
            ));
        }
        let read_attempted = self
            .stats
            .read_bytes
            .checked_add(read_bytes)
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::ReadBytes,
                    self.limits.max_total_read_bytes,
                    u64::MAX,
                )
            })?;
        if read_attempted > self.limits.max_total_read_bytes {
            return Err(SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::ReadBytes,
                self.limits.max_total_read_bytes,
                read_attempted,
            ));
        }
        let parse_attempted = self
            .stats
            .parse_bytes
            .checked_add(parse_bytes)
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::ParseBytes,
                    self.limits.max_total_parse_bytes,
                    u64::MAX,
                )
            })?;
        if parse_attempted > self.limits.max_total_parse_bytes {
            return Err(SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::ParseBytes,
                self.limits.max_total_parse_bytes,
                parse_attempted,
            ));
        }
        let retained_attempted = self
            .stats
            .retained_bound_bytes
            .checked_add(retained_bound_bytes)
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::RetainedBoundBytes,
                    self.limits.max_retained_bound_bytes,
                    u64::MAX,
                )
            })?;
        if retained_attempted > self.limits.max_retained_bound_bytes {
            return Err(SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::RetainedBoundBytes,
                self.limits.max_retained_bound_bytes,
                retained_attempted,
            ));
        }
        self.active_work_reservation = ObservedWork {
            read_bytes,
            parse_bytes,
        };
        self.active_retained_reservation = retained_bound_bytes;
        self.stats.max_admitted_read_bytes = self.stats.max_admitted_read_bytes.max(read_attempted);
        self.stats.max_admitted_parse_bytes =
            self.stats.max_admitted_parse_bytes.max(parse_attempted);
        self.stats.max_admitted_retained_bound_bytes = self
            .stats
            .max_admitted_retained_bound_bytes
            .max(retained_attempted);
        Ok(())
    }

    fn complete_active_child(
        &mut self,
        retained_bound_bytes: u64,
    ) -> Result<(), SourceRevisionChainError> {
        if self.active_work_reservation == ObservedWork::default()
            || retained_bound_bytes > self.active_retained_reservation
        {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InternalState,
                None,
            ));
        }
        let retained = self
            .stats
            .retained_bound_bytes
            .checked_add(retained_bound_bytes)
            .ok_or_else(|| self.allocation_error(u64::MAX))?;
        if retained > self.limits.max_retained_bound_bytes {
            return Err(SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::RetainedBoundBytes,
                self.limits.max_retained_bound_bytes,
                retained,
            ));
        }
        self.stats.retained_bound_bytes = retained;
        self.active_work_reservation = ObservedWork::default();
        self.active_retained_reservation = 0;
        Ok(())
    }

    fn admit_section(
        &mut self,
        entries: usize,
        traditional: bool,
    ) -> Result<(), SourceRevisionChainError> {
        let sections = self.stats.sections.checked_add(1).ok_or_else(|| {
            SourceRevisionChainError::for_code(SourceRevisionChainErrorCode::InternalState, None)
        })?;
        if sections > self.revision_limits.max_sections() {
            return Err(SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::Sections,
                u64::from(self.revision_limits.max_sections()),
                u64::from(sections),
            ));
        }
        let entries = u64::try_from(entries).map_err(|_| {
            SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::Entries,
                self.revision_limits.max_entries(),
                u64::MAX,
            )
        })?;
        let total = self.stats.entries.checked_add(entries).ok_or_else(|| {
            SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::Entries,
                self.revision_limits.max_entries(),
                u64::MAX,
            )
        })?;
        if total > self.revision_limits.max_entries() {
            return Err(SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::Entries,
                self.revision_limits.max_entries(),
                total,
            ));
        }
        self.stats.sections = sections;
        self.stats.entries = total;
        if traditional && self.pending_traditional.is_some() {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InternalState,
                None,
            ));
        }
        Ok(())
    }

    fn traditional_bound(
        &self,
        section: &TraditionalRevisionSection,
    ) -> Result<u64, SourceRevisionChainError> {
        self.traditional_ceiling_bound(section.span().len())
    }

    fn traditional_ceiling_bound(
        &self,
        section_span_bytes: u64,
    ) -> Result<u64, SourceRevisionChainError> {
        let entry_bytes =
            count_width_bound::<pdf_rs_xref::XrefEntry>(self.xref_limits.max_entries())?;
        section_span_bytes
            .checked_add(self.syntax_limits.max_owned_bytes())
            .and_then(|value| value.checked_add(self.syntax_limits.max_container_bytes()))
            .and_then(|value| value.checked_add(entry_bytes))
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::RetainedBoundBytes,
                    self.limits.max_retained_bound_bytes,
                    u64::MAX,
                )
            })
    }

    fn stream_child_bounds(
        &self,
        startxref: u64,
        upper_bound: u64,
    ) -> Result<(u64, u64, u64), SourceRevisionChainError> {
        let available = upper_bound.checked_sub(startxref).ok_or_else(|| {
            SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InternalState,
                Some(startxref),
            )
        })?;
        let envelope_available = available.checked_add(1).ok_or_else(|| {
            SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InternalState,
                Some(startxref),
            )
        })?;
        let envelope_cap = self
            .object_limits
            .max_envelope_bytes()
            .min(envelope_available);
        let envelope = window_sum(
            self.object_limits
                .initial_envelope_bytes()
                .min(envelope_available),
            envelope_cap,
        )?;
        let boundary_cap = self.object_limits.max_boundary_bytes().min(available);
        let boundary = window_sum(
            self.object_limits.initial_boundary_bytes().min(available),
            boundary_cap,
        )?;
        let object_work = envelope.checked_add(boundary).ok_or_else(|| {
            SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::ReadBytes,
                self.limits.max_total_read_bytes,
                u64::MAX,
            )
        })?;
        let payload = available
            .min(self.object_limits.max_stream_bytes())
            .min(self.xref_stream_limits.max_decoded_bytes());
        let read = object_work
            .min(self.object_limits.max_total_read_bytes())
            .checked_add(payload)
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::ReadBytes,
                    self.limits.max_total_read_bytes,
                    u64::MAX,
                )
            })?;
        let parse = object_work
            .min(self.object_limits.max_total_parse_bytes())
            .checked_add(payload)
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::ParseBytes,
                    self.limits.max_total_parse_bytes,
                    u64::MAX,
                )
            })?;
        let retained = self
            .syntax_limits
            .max_owned_bytes()
            .checked_add(self.syntax_limits.max_container_bytes())
            .and_then(|value| value.checked_add(self.xref_stream_limits.max_retained_entry_bytes()))
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::RetainedBoundBytes,
                    self.limits.max_retained_bound_bytes,
                    u64::MAX,
                )
            })?;
        Ok((read, parse, retained))
    }

    fn copy_entries<T>(
        &mut self,
        entries: &[T],
        cancellation: &dyn SourceRevisionChainCancellation,
    ) -> Result<Vec<RevisionEntry>, SourceRevisionChainError>
    where
        T: Copy,
        RevisionEntry: From<T>,
    {
        let requested = capacity_bound::<RevisionEntry>(entries.len())?;
        self.check_retained_bound(requested)?;
        let mut copied = Vec::new();
        copied.try_reserve_exact(entries.len()).map_err(|_| {
            SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::RetainedBoundBytes,
                self.limits.max_retained_bound_bytes,
                self.stats.retained_bound_bytes.saturating_add(requested),
            )
        })?;
        let actual = capacity_bound::<RevisionEntry>(copied.capacity())?;
        self.charge_retained_bound(actual)?;
        for (index, entry) in entries.iter().copied().enumerate() {
            if index.is_multiple_of(CANCELLATION_INTERVAL) && cancellation.is_cancelled() {
                return Err(SourceRevisionChainError::for_code(
                    SourceRevisionChainErrorCode::Cancelled,
                    None,
                ));
            }
            copied.push(RevisionEntry::from(entry));
        }
        if cancellation.is_cancelled() {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::Cancelled,
                None,
            ));
        }
        Ok(copied)
    }

    fn validate_hybrid_geometry(
        &self,
        hybrid: u64,
        primary: u64,
        previous: Option<u64>,
        cancellation: &dyn SourceRevisionChainCancellation,
    ) -> Result<(), SourceRevisionChainError> {
        if hybrid == 0
            || hybrid >= primary
            || previous.is_some_and(|previous| hybrid <= previous)
            || self.anchor_was_seen(hybrid, cancellation)?
        {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InvalidChainGeometry,
                Some(hybrid),
            ));
        }
        Ok(())
    }

    fn validate_previous_geometry(
        &self,
        previous: Option<u64>,
        current: u64,
        cancellation: &dyn SourceRevisionChainCancellation,
    ) -> Result<(), SourceRevisionChainError> {
        if let Some(previous) = previous
            && (previous == 0
                || previous >= current
                || self.anchor_was_seen(previous, cancellation)?)
        {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::InvalidChainGeometry,
                Some(previous),
            ));
        }
        Ok(())
    }

    fn finish_revision(
        &mut self,
        proof: SourceRevisionProof,
        candidate: RevisionCandidate,
        previous: Option<u64>,
        current: u64,
        cancellation: &dyn SourceRevisionChainCancellation,
    ) -> Result<(), SourceRevisionChainError> {
        if self.stats.revisions >= self.revision_limits.max_revisions() {
            return Err(SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::Revisions,
                u64::from(self.revision_limits.max_revisions()),
                u64::from(self.stats.revisions) + 1,
            ));
        }
        self.validate_previous_geometry(previous, current, cancellation)?;

        self.reserve_result_slot()?;
        self.proofs.push(proof);
        self.candidates.push(candidate);
        self.stats.revisions = self.stats.revisions.checked_add(1).ok_or_else(|| {
            SourceRevisionChainError::for_code(SourceRevisionChainErrorCode::InternalState, None)
        })?;

        if let Some(previous) = previous {
            self.start_anchor(previous, current, false)
        } else {
            self.state = JobState::Compose;
            Ok(())
        }
    }

    fn reserve_result_slot(&mut self) -> Result<(), SourceRevisionChainError> {
        let proof_delta = reserve_one_bound::<SourceRevisionProof>(&self.proofs)?;
        let candidate_delta = reserve_one_bound::<RevisionCandidate>(&self.candidates)?;
        let requested = proof_delta.checked_add(candidate_delta).ok_or_else(|| {
            SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::RetainedBoundBytes,
                self.limits.max_retained_bound_bytes,
                u64::MAX,
            )
        })?;
        self.check_retained_bound(requested)?;

        let old_proof_capacity = self.proofs.capacity();
        let old_candidate_capacity = self.candidates.capacity();
        self.proofs
            .try_reserve_exact(1)
            .map_err(|_| self.allocation_error(requested))?;
        self.candidates
            .try_reserve_exact(1)
            .map_err(|_| self.allocation_error(requested))?;
        let actual_proof_delta =
            capacity_delta::<SourceRevisionProof>(old_proof_capacity, self.proofs.capacity())?;
        let actual_candidate_delta = capacity_delta::<RevisionCandidate>(
            old_candidate_capacity,
            self.candidates.capacity(),
        )?;
        let actual = actual_proof_delta
            .checked_add(actual_candidate_delta)
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::RetainedBoundBytes,
                    self.limits.max_retained_bound_bytes,
                    u64::MAX,
                )
            })?;
        self.charge_retained_bound(actual)
    }

    fn anchor_was_seen(
        &self,
        offset: u64,
        cancellation: &dyn SourceRevisionChainCancellation,
    ) -> Result<bool, SourceRevisionChainError> {
        for (index, proof) in self.proofs.iter().enumerate() {
            if index.is_multiple_of(CANCELLATION_INTERVAL) && cancellation.is_cancelled() {
                return Err(SourceRevisionChainError::for_code(
                    SourceRevisionChainErrorCode::Cancelled,
                    None,
                ));
            }
            if proof.primary.anchor().startxref() == offset
                || proof
                    .hybrid
                    .as_ref()
                    .is_some_and(|hybrid| hybrid.anchor.startxref() == offset)
            {
                return Ok(true);
            }
        }
        if cancellation.is_cancelled() {
            return Err(SourceRevisionChainError::for_code(
                SourceRevisionChainErrorCode::Cancelled,
                None,
            ));
        }
        Ok(self
            .pending_traditional
            .as_ref()
            .is_some_and(|pending| pending.anchor.startxref() == offset))
    }

    fn check_retained_bound(&self, amount: u64) -> Result<(), SourceRevisionChainError> {
        let attempted = self
            .stats
            .retained_bound_bytes
            .checked_add(self.active_retained_reservation)
            .and_then(|value| value.checked_add(amount))
            .ok_or_else(|| {
                SourceRevisionChainError::resource(
                    SourceRevisionChainLimitKind::RetainedBoundBytes,
                    self.limits.max_retained_bound_bytes,
                    u64::MAX,
                )
            })?;
        if attempted > self.limits.max_retained_bound_bytes {
            return Err(SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::RetainedBoundBytes,
                self.limits.max_retained_bound_bytes,
                attempted,
            ));
        }
        Ok(())
    }

    fn charge_retained_bound(&mut self, amount: u64) -> Result<(), SourceRevisionChainError> {
        self.check_retained_bound(amount)?;
        self.stats.retained_bound_bytes = self
            .stats
            .retained_bound_bytes
            .checked_add(amount)
            .ok_or_else(|| self.allocation_error(u64::MAX))?;
        Ok(())
    }

    const fn allocation_error(&self, amount: u64) -> SourceRevisionChainError {
        SourceRevisionChainError::resource(
            SourceRevisionChainLimitKind::RetainedBoundBytes,
            self.limits.max_retained_bound_bytes,
            self.stats
                .retained_bound_bytes
                .saturating_add(self.active_retained_reservation)
                .saturating_add(amount),
        )
    }

    fn fail(&mut self, error: SourceRevisionChainError) -> SourceRevisionChainPoll {
        self.state = JobState::Failed(error);
        SourceRevisionChainPoll::Failed(error)
    }

    fn fail_internal(&mut self, offset: Option<u64>) -> SourceRevisionChainPoll {
        self.fail(SourceRevisionChainError::for_code(
            SourceRevisionChainErrorCode::InternalState,
            offset,
        ))
    }
}

fn capacity_bound<T>(count: usize) -> Result<u64, SourceRevisionChainError> {
    let count = u64::try_from(count).map_err(|_| {
        SourceRevisionChainError::resource(
            SourceRevisionChainLimitKind::RetainedBoundBytes,
            HARD_MAX_RETAINED_BOUND_BYTES,
            u64::MAX,
        )
    })?;
    let width = u64::try_from(mem::size_of::<T>()).map_err(|_| {
        SourceRevisionChainError::for_code(SourceRevisionChainErrorCode::InternalState, None)
    })?;
    count.checked_mul(width).ok_or_else(|| {
        SourceRevisionChainError::resource(
            SourceRevisionChainLimitKind::RetainedBoundBytes,
            HARD_MAX_RETAINED_BOUND_BYTES,
            u64::MAX,
        )
    })
}

fn count_width_bound<T>(count: u64) -> Result<u64, SourceRevisionChainError> {
    let width = u64::try_from(mem::size_of::<T>()).map_err(|_| {
        SourceRevisionChainError::for_code(SourceRevisionChainErrorCode::InternalState, None)
    })?;
    count.checked_mul(width).ok_or_else(|| {
        SourceRevisionChainError::resource(
            SourceRevisionChainLimitKind::RetainedBoundBytes,
            HARD_MAX_RETAINED_BOUND_BYTES,
            u64::MAX,
        )
    })
}

fn window_work_bound(
    initial: u64,
    cap: u64,
    total_cap: u64,
) -> Result<u64, SourceRevisionChainError> {
    Ok(window_sum(initial, cap)?.min(total_cap))
}

fn window_sum(initial: u64, cap: u64) -> Result<u64, SourceRevisionChainError> {
    if initial == 0 || cap == 0 || initial > cap {
        return Err(SourceRevisionChainError::for_code(
            SourceRevisionChainErrorCode::InternalState,
            None,
        ));
    }
    let mut current = initial;
    let mut total = 0_u64;
    loop {
        total = total.checked_add(current).ok_or_else(|| {
            SourceRevisionChainError::resource(
                SourceRevisionChainLimitKind::ReadBytes,
                HARD_MAX_TOTAL_WORK_BYTES,
                u64::MAX,
            )
        })?;
        if current == cap {
            return Ok(total);
        }
        current = current
            .checked_mul(2)
            .map(|value| value.min(cap))
            .filter(|next| *next > current)
            .ok_or_else(|| {
                SourceRevisionChainError::for_code(
                    SourceRevisionChainErrorCode::InternalState,
                    None,
                )
            })?;
    }
}

fn reserve_one_bound<T>(values: &Vec<T>) -> Result<u64, SourceRevisionChainError> {
    if values.len() < values.capacity() {
        Ok(0)
    } else {
        capacity_bound::<T>(1)
    }
}

fn capacity_delta<T>(old: usize, new: usize) -> Result<u64, SourceRevisionChainError> {
    let delta = new.checked_sub(old).ok_or_else(|| {
        SourceRevisionChainError::for_code(SourceRevisionChainErrorCode::InternalState, None)
    })?;
    capacity_bound::<T>(delta)
}
