use std::error::Error;
use std::fmt;
use std::mem;

use pdf_rs_bytes::{ByteSource, DataTicket, JobId, ResumeCheckpoint, SmallRanges, SourceSnapshot};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    OpenXrefJob, XrefCancellation, XrefError, XrefJobContext, XrefLimits, XrefPhase, XrefPoll,
    XrefStats,
};

use crate::{
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex, DocumentCancellation,
    DocumentError, DocumentErrorCode, DocumentIndexStats, DocumentLimits,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionAttestationPhase,
    RevisionAttestationPoll, RevisionAttestationStats, RevisionId,
};

/// Runtime identity and all phase-specific checkpoints for one strict base-revision open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StrictBaseOpenContext {
    xref: XrefJobContext,
    attestation: RevisionAttestationJobContext,
}

impl StrictBaseOpenContext {
    /// Creates a context whose child jobs must share one job identity and five distinct checkpoints.
    pub const fn new(xref: XrefJobContext, attestation: RevisionAttestationJobContext) -> Self {
        Self { xref, attestation }
    }

    /// Returns the xref tail and section context.
    pub const fn xref(self) -> XrefJobContext {
        self.xref
    }

    /// Returns the prefix, object-envelope, and stream-boundary attestation context.
    pub const fn attestation(self) -> RevisionAttestationJobContext {
        self.attestation
    }

    /// Returns the one runtime job identity shared by every open phase.
    pub const fn job(self) -> JobId {
        self.xref.job()
    }

    fn is_valid(self) -> bool {
        if self.xref.job() != self.attestation.job() {
            return false;
        }
        let checkpoints = [
            self.xref.tail_checkpoint(),
            self.xref.section_checkpoint(),
            self.attestation.scan_checkpoint(),
            self.attestation.object_envelope_checkpoint(),
            self.attestation.object_boundary_checkpoint(),
        ];
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            if checkpoints[index + 1..].contains(checkpoint) {
                return false;
            }
        }
        true
    }
}

/// Complete validated child profiles used by one strict base-revision open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StrictBaseOpenLimits {
    xref: XrefLimits,
    document: DocumentLimits,
    attestation: RevisionAttestationLimits,
    object: ObjectLimits,
    syntax: SyntaxLimits,
}

impl StrictBaseOpenLimits {
    /// Bundles already-validated xref, candidate-index, attestation, object, and syntax limits.
    pub const fn new(
        xref: XrefLimits,
        document: DocumentLimits,
        attestation: RevisionAttestationLimits,
        object: ObjectLimits,
        syntax: SyntaxLimits,
    ) -> Self {
        Self {
            xref,
            document,
            attestation,
            object,
            syntax,
        }
    }

    /// Returns the traditional-xref limits.
    pub const fn xref(self) -> XrefLimits {
        self.xref
    }

    /// Returns the candidate revision-index limits.
    pub const fn document(self) -> DocumentLimits {
        self.document
    }

    /// Returns the strict top-level attestation limits.
    pub const fn attestation(self) -> RevisionAttestationLimits {
        self.attestation
    }

    /// Returns the indirect-object framing limits.
    pub const fn object(self) -> ObjectLimits {
        self.object
    }

    /// Returns the direct syntax limits.
    pub const fn syntax(self) -> SyntaxLimits {
        self.syntax
    }
}

/// Coarse resumable phase of one strict base-revision open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenPhase {
    /// Locating and parsing the final traditional xref section.
    Xref(XrefPhase),
    /// Authenticating every top-level in-use object and surrounding trivia.
    Attestation(RevisionAttestationPhase),
    /// The attested index was returned and the one-shot open is complete.
    Ready,
    /// The open reached a stable terminal failure.
    Failed,
}

/// Cumulative child work and retained-index accounting for one strict base open.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StrictBaseOpenStats {
    xref: XrefStats,
    index: Option<DocumentIndexStats>,
    attestation: RevisionAttestationStats,
}

impl StrictBaseOpenStats {
    /// Returns cumulative xref read, parse, and entry work.
    pub const fn xref(self) -> XrefStats {
        self.xref
    }

    /// Returns candidate-index accounting after the synchronous indexing transition succeeds.
    pub const fn index(self) -> Option<DocumentIndexStats> {
        self.index
    }

    /// Returns cumulative top-level attestation work.
    pub const fn attestation(self) -> RevisionAttestationStats {
        self.attestation
    }
}

/// Stable error preserving the complete failing child-layer evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenError {
    /// Traditional-xref opening failed with its original structured error.
    Xref(XrefError),
    /// Candidate indexing or top-level attestation failed with its original document error.
    Document(DocumentError),
}

impl StrictBaseOpenError {
    /// Returns the complete xref error when the xref phase failed.
    pub const fn xref(self) -> Option<XrefError> {
        match self {
            Self::Xref(error) => Some(error),
            Self::Document(_) => None,
        }
    }

    /// Returns the complete document error when indexing or attestation failed.
    pub const fn document(self) -> Option<DocumentError> {
        match self {
            Self::Xref(_) => None,
            Self::Document(error) => Some(error),
        }
    }
}

impl fmt::Display for StrictBaseOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xref(error) => write!(formatter, "strict base open xref failed: {error}"),
            Self::Document(error) => write!(formatter, "strict base open document failed: {error}"),
        }
    }
}

impl Error for StrictBaseOpenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Xref(error) => Some(error),
            Self::Document(error) => Some(error),
        }
    }
}

impl From<XrefError> for StrictBaseOpenError {
    fn from(error: XrefError) -> Self {
        Self::Xref(error)
    }
}

impl From<DocumentError> for StrictBaseOpenError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}

/// Result of polling one resumable strict base-revision open.
#[allow(
    clippy::large_enum_variant,
    reason = "the one-shot attested index stays inline without an untracked allocation"
)]
pub enum StrictBaseOpenPoll {
    /// The strict traditional base revision is fully authenticated.
    Ready(AttestedRevisionIndex),
    /// The active xref or attestation child requires exact source ranges.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the active child request.
        missing: SmallRanges,
        /// Child phase checkpoint to retain when requeueing the open job.
        checkpoint: ResumeCheckpoint,
    },
    /// The open reached a stable structured failure.
    Failed(StrictBaseOpenError),
}

impl fmt::Debug for StrictBaseOpenPoll {
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

struct XrefCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl XrefCancellation for XrefCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "active child jobs stay inline so every transition preserves bounded owned state"
)]
enum JobState {
    Xref(OpenXrefJob),
    Attestation(AttestRevisionJob),
    Transition,
    Complete,
    Failed(StrictBaseOpenError),
}

/// One-shot job that opens and authenticates one strict traditional base revision.
///
/// The job composes the existing xref, candidate-index, and top-level attestation
/// layers without performing host I/O or exposing an unauthenticated intermediate.
/// Missing bytes remain explicit [`StrictBaseOpenPoll::Pending`] control flow.
pub struct OpenStrictBaseRevisionJob {
    snapshot: SourceSnapshot,
    revision_id: RevisionId,
    context: StrictBaseOpenContext,
    limits: StrictBaseOpenLimits,
    stats: StrictBaseOpenStats,
    state: JobState,
}

impl OpenStrictBaseRevisionJob {
    /// Validates the cross-phase context and creates the initial xref child job.
    pub fn new(
        snapshot: SourceSnapshot,
        revision_id: RevisionId,
        context: StrictBaseOpenContext,
        limits: StrictBaseOpenLimits,
    ) -> Result<Self, StrictBaseOpenError> {
        if !context.is_valid() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidStrictBaseOpenContext,
                None,
                None,
            )
            .into());
        }
        let xref = OpenXrefJob::new(snapshot, context.xref, limits.xref, limits.syntax)?;
        Ok(Self {
            snapshot,
            revision_id,
            context,
            limits,
            stats: StrictBaseOpenStats::default(),
            state: JobState::Xref(xref),
        })
    }

    /// Returns the immutable source snapshot bound at construction.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the caller-assigned strict base revision identity.
    pub const fn revision_id(&self) -> RevisionId {
        self.revision_id
    }

    /// Returns runtime identity and all phase-specific checkpoints.
    pub const fn context(&self) -> StrictBaseOpenContext {
        self.context
    }

    /// Returns every validated child profile.
    pub const fn limits(&self) -> StrictBaseOpenLimits {
        self.limits
    }

    /// Returns cumulative child work through the latest poll.
    pub const fn stats(&self) -> StrictBaseOpenStats {
        self.stats
    }

    /// Returns the current coarse open phase.
    pub fn phase(&self) -> StrictBaseOpenPhase {
        match &self.state {
            JobState::Xref(job) => StrictBaseOpenPhase::Xref(job.phase()),
            JobState::Attestation(job) => StrictBaseOpenPhase::Attestation(job.phase()),
            JobState::Complete => StrictBaseOpenPhase::Ready,
            JobState::Transition | JobState::Failed(_) => StrictBaseOpenPhase::Failed,
        }
    }

    /// Advances the open without performing file, network, callback, or async-runtime I/O.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> StrictBaseOpenPoll {
        loop {
            let state = mem::replace(&mut self.state, JobState::Transition);
            match state {
                JobState::Xref(mut job) => {
                    let outcome = job.poll(source, &XrefCancellationAdapter(cancellation));
                    self.stats.xref = job.stats();
                    match outcome {
                        XrefPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = JobState::Xref(job);
                            return StrictBaseOpenPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        XrefPoll::Failed(error) => return self.fail(error.into()),
                        XrefPoll::Ready(section) => {
                            let candidate = match CandidateRevisionIndex::from_xref(
                                &section,
                                self.revision_id,
                                self.limits.document,
                                cancellation,
                            ) {
                                Ok(candidate) => candidate,
                                Err(error) => return self.fail(error.into()),
                            };
                            self.stats.index = Some(candidate.stats());
                            let attestation = match AttestRevisionJob::new(
                                candidate,
                                self.context.attestation,
                                self.limits.attestation,
                                self.limits.object,
                                self.limits.syntax,
                            ) {
                                Ok(attestation) => attestation,
                                Err(error) => return self.fail(error.into()),
                            };
                            self.state = JobState::Attestation(attestation);
                        }
                    }
                }
                JobState::Attestation(mut job) => {
                    let outcome = job.poll(source, cancellation);
                    self.stats.attestation = job.stats();
                    match outcome {
                        RevisionAttestationPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = JobState::Attestation(job);
                            return StrictBaseOpenPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        RevisionAttestationPoll::Failed(error) => {
                            return self.fail(error.into());
                        }
                        RevisionAttestationPoll::Ready(index) => {
                            self.state = JobState::Complete;
                            return StrictBaseOpenPoll::Ready(index);
                        }
                    }
                }
                JobState::Complete => {
                    self.state = JobState::Complete;
                    return StrictBaseOpenPoll::Failed(
                        DocumentError::for_code(DocumentErrorCode::JobAlreadyComplete, None, None)
                            .into(),
                    );
                }
                JobState::Failed(error) => {
                    self.state = JobState::Failed(error);
                    return StrictBaseOpenPoll::Failed(error);
                }
                JobState::Transition => {
                    return self.fail(
                        DocumentError::for_code(DocumentErrorCode::InternalState, None, None)
                            .into(),
                    );
                }
            }
        }
    }

    fn fail(&mut self, error: StrictBaseOpenError) -> StrictBaseOpenPoll {
        self.state = JobState::Failed(error);
        StrictBaseOpenPoll::Failed(error)
    }
}

impl fmt::Debug for OpenStrictBaseRevisionJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenStrictBaseRevisionJob")
            .field("snapshot", &self.snapshot)
            .field("revision_id", &self.revision_id)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .finish()
    }
}
