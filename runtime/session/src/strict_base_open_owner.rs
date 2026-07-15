use std::fmt;
use std::mem;

use pdf_rs_bytes::{ByteSource, DataTicket, JobId, SmallRanges, SourceError};
use pdf_rs_document::{
    AttestedRevisionIndex, DocumentCancellation, OpenStrictBaseRevisionJob, StrictBaseOpenError,
    StrictBaseOpenPhase, StrictBaseOpenPoll, StrictBaseOpenStats,
};

use crate::{
    RangeResumeArbiterId, RangeResumeFailurePermit, RangeResumeGeneration, RangeResumePermit,
    RangeResumeTarget,
};

/// Public lifecycle phase of one runtime-owned strict base-open job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenOwnerPhase {
    /// The job may execute its first synchronous poll.
    Queued,
    /// The job is suspended on one exact Range ticket and checkpoint.
    WaitingForData,
    /// The owner published its only successful attested revision result.
    Ready,
    /// The owned strict-open job reached a stable lower-layer failure.
    Failed,
    /// Runtime cancellation won before a terminal result was published.
    Cancelled,
    /// Source integrity changed before a terminal result was published.
    SourceChanged,
    /// Explicit close dropped any remaining job and waiting target.
    Closed,
}

/// Current bounded scheduler resources retained by one strict-open owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StrictBaseOpenOwnerResources {
    jobs: usize,
    waiting_targets: usize,
}

impl StrictBaseOpenOwnerResources {
    const ZERO: Self = Self {
        jobs: 0,
        waiting_targets: 0,
    };

    /// Returns the number of privately retained strict-open jobs.
    pub const fn jobs(self) -> usize {
        self.jobs
    }

    /// Returns the number of exact ticket targets awaiting one resume permit.
    pub const fn waiting_targets(self) -> usize {
        self.waiting_targets
    }
}

/// Result of one permitted strict-open job poll.
#[allow(
    clippy::large_enum_variant,
    reason = "the proof-bearing attested result stays move-only and inline"
)]
pub enum StrictBaseOpenOwnerPoll {
    /// The job suspended and must be registered with the Range-resume arbiter.
    WaitingForData {
        /// The exact one-shot byte ticket returned by the core job.
        ticket: DataTicket,
        /// Canonical source ranges still missing for this suspension.
        missing: SmallRanges,
        /// Complete runtime job, checkpoint, and generation registration target.
        target: RangeResumeTarget,
    },
    /// The only successful attested revision result was published.
    Ready(AttestedRevisionIndex),
    /// The complete lower strict-open error reached a stable terminal.
    Failed(StrictBaseOpenError),
    /// The cancellation token stopped a permitted poll without publication.
    Cancelled(StrictBaseOpenError),
}

impl fmt::Debug for StrictBaseOpenOwnerPoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WaitingForData {
                ticket,
                missing,
                target,
            } => formatter
                .debug_struct("WaitingForData")
                .field("ticket", ticket)
                .field("missing", missing)
                .field("target", target)
                .finish(),
            Self::Ready(index) => formatter.debug_tuple("Ready").field(index).finish(),
            Self::Failed(error) => formatter.debug_tuple("Failed").field(error).finish(),
            Self::Cancelled(error) => formatter.debug_tuple("Cancelled").field(error).finish(),
        }
    }
}

/// Result of attempting the one permitted initial job poll.
#[allow(
    clippy::large_enum_variant,
    reason = "the successful poll result remains move-only without an untracked box"
)]
pub enum StrictBaseOpenOwnerStart {
    /// The queued job executed and produced this result.
    Polled(StrictBaseOpenOwnerPoll),
    /// The owner was not queued, so no parser code executed.
    Rejected {
        /// The phase that rejected the repeated or late start.
        phase: StrictBaseOpenOwnerPhase,
    },
}

impl fmt::Debug for StrictBaseOpenOwnerStart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Polled(outcome) => formatter.debug_tuple("Polled").field(outcome).finish(),
            Self::Rejected { phase } => formatter
                .debug_struct("Rejected")
                .field("phase", phase)
                .finish(),
        }
    }
}

/// Reason a consumed one-shot permit did not execute parser code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenResumeDiscardReason {
    /// The permit was issued by a different Range-resume arbiter.
    ArbiterMismatch,
    /// The permit belongs to a different runtime generation.
    StaleGeneration,
    /// The permit belongs to a different runtime job.
    JobMismatch,
    /// The permit targets a different resumable parser checkpoint.
    CheckpointMismatch,
    /// The permit completed a different byte ticket.
    TicketMismatch,
    /// The owner no longer has a job waiting for a permit.
    NotWaiting(StrictBaseOpenOwnerPhase),
}

/// Result of consuming one move-only Range resume permit.
#[allow(
    clippy::large_enum_variant,
    reason = "the successful poll result remains move-only without an untracked box"
)]
pub enum StrictBaseOpenOwnerResume {
    /// Every permit identity matched and the job executed one poll.
    Polled(StrictBaseOpenOwnerPoll),
    /// The permit was consumed without executing parser code.
    Discarded {
        /// The completed Range ticket carried by the consumed permit.
        ticket: DataTicket,
        /// The scheduler target carried by the consumed permit.
        target: RangeResumeTarget,
        /// The identity or lifecycle check that prevented execution.
        reason: StrictBaseOpenResumeDiscardReason,
    },
}

impl fmt::Debug for StrictBaseOpenOwnerResume {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Polled(outcome) => formatter.debug_tuple("Polled").field(outcome).finish(),
            Self::Discarded {
                ticket,
                target,
                reason,
            } => formatter
                .debug_struct("Discarded")
                .field("ticket", ticket)
                .field("target", target)
                .field("reason", reason)
                .finish(),
        }
    }
}

/// Result of consuming one move-only Range failure permit.
#[derive(Debug, Eq, PartialEq)]
pub enum StrictBaseOpenOwnerFail {
    /// Every permit identity matched and the waiting job became terminal.
    Failed {
        /// The failed Range ticket carried by the consumed permit.
        ticket: DataTicket,
        /// The exact target whose source operation failed.
        target: RangeResumeTarget,
        /// Complete source-redacted host failure evidence.
        error: SourceError,
    },
    /// The permit was consumed without changing the owned job.
    Discarded {
        /// The failed Range ticket carried by the consumed permit.
        ticket: DataTicket,
        /// The scheduler target carried by the consumed permit.
        target: RangeResumeTarget,
        /// Complete source-redacted host failure evidence.
        error: SourceError,
        /// The identity or lifecycle check that rejected the failure.
        reason: StrictBaseOpenResumeDiscardReason,
    },
}

/// Result of requesting cancellation between synchronous actor turns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenOwnerCancelOutcome {
    /// Cancellation dropped the only active job.
    Cancelled {
        /// Waiting target the caller must remove from its Range arbiter, if any.
        target: Option<RangeResumeTarget>,
    },
    /// A terminal result had already won, so cancellation changed nothing.
    AlreadyTerminal {
        /// The winning terminal phase.
        phase: StrictBaseOpenOwnerPhase,
    },
}

/// Result of reporting source-integrity change between synchronous actor turns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenOwnerSourceChangeOutcome {
    /// Source change dropped the only active job.
    SourceChanged {
        /// Waiting target the caller must remove from its Range arbiter, if any.
        target: Option<RangeResumeTarget>,
    },
    /// A terminal result had already won, so source change changed nothing.
    AlreadyTerminal {
        /// The winning terminal phase.
        phase: StrictBaseOpenOwnerPhase,
    },
}

/// Stable evidence returned after explicit close drops the owner state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StrictBaseOpenOwnerCloseReport {
    previous_phase: StrictBaseOpenOwnerPhase,
    released_jobs: usize,
    released_waiting_targets: usize,
}

impl StrictBaseOpenOwnerCloseReport {
    /// Returns the phase observed before the first close transition.
    pub const fn previous_phase(self) -> StrictBaseOpenOwnerPhase {
        self.previous_phase
    }

    /// Returns the number of jobs dropped by the first close.
    pub const fn released_jobs(self) -> usize {
        self.released_jobs
    }

    /// Returns the number of waiting targets dropped by the first close.
    pub const fn released_waiting_targets(self) -> usize {
        self.released_waiting_targets
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "the single active job remains inline under one explicit owner"
)]
enum OwnerState {
    Queued(OpenStrictBaseRevisionJob),
    Waiting {
        job: OpenStrictBaseRevisionJob,
        ticket: DataTicket,
        target: RangeResumeTarget,
    },
    Ready,
    Failed(StrictBaseOpenError),
    SourceFailed(SourceError),
    Cancelled(Option<StrictBaseOpenError>),
    SourceChanged,
    Closed(StrictBaseOpenOwnerCloseReport),
    Transition,
}

impl OwnerState {
    fn phase(&self) -> StrictBaseOpenOwnerPhase {
        match self {
            Self::Queued(_) => StrictBaseOpenOwnerPhase::Queued,
            Self::Waiting { .. } => StrictBaseOpenOwnerPhase::WaitingForData,
            Self::Ready => StrictBaseOpenOwnerPhase::Ready,
            Self::Failed(_) | Self::SourceFailed(_) | Self::Transition => {
                StrictBaseOpenOwnerPhase::Failed
            }
            Self::Cancelled(_) => StrictBaseOpenOwnerPhase::Cancelled,
            Self::SourceChanged => StrictBaseOpenOwnerPhase::SourceChanged,
            Self::Closed(_) => StrictBaseOpenOwnerPhase::Closed,
        }
    }

    fn resources(&self) -> StrictBaseOpenOwnerResources {
        match self {
            Self::Queued(_) => StrictBaseOpenOwnerResources {
                jobs: 1,
                waiting_targets: 0,
            },
            Self::Waiting { .. } => StrictBaseOpenOwnerResources {
                jobs: 1,
                waiting_targets: 1,
            },
            Self::Ready
            | Self::Failed(_)
            | Self::SourceFailed(_)
            | Self::Cancelled(_)
            | Self::SourceChanged
            | Self::Closed(_)
            | Self::Transition => StrictBaseOpenOwnerResources::ZERO,
        }
    }
}

/// Exclusive execution owner for one generation-bound strict base-open job.
///
/// The owner permits one initial poll, then requires a move-only
/// [`RangeResumePermit`] whose issuing arbiter, ticket, job, checkpoint, and
/// generation all match the current suspension. A rejected permit is consumed
/// without polling. The owner has no internal queue and performs no host I/O; a
/// separate [`crate::RangeResumeArbiter`] continues to own source bytes and
/// ticket completion.
pub struct StrictBaseOpenJobOwner {
    arbiter_id: RangeResumeArbiterId,
    job_id: JobId,
    generation: RangeResumeGeneration,
    stats: StrictBaseOpenStats,
    job_phase: StrictBaseOpenPhase,
    state: OwnerState,
}

impl StrictBaseOpenJobOwner {
    /// Takes exclusive ownership of one queued strict-open job generation.
    ///
    /// `arbiter_id` must identify the Range arbiter whose byte source will be
    /// polled and whose permits may later resume this owner.
    pub fn new(
        job: OpenStrictBaseRevisionJob,
        generation: RangeResumeGeneration,
        arbiter_id: RangeResumeArbiterId,
    ) -> Self {
        let job_id = job.context().job();
        let stats = job.stats();
        let job_phase = job.phase();
        Self {
            arbiter_id,
            job_id,
            generation,
            stats,
            job_phase,
            state: OwnerState::Queued(job),
        }
    }

    /// Returns the only Range-resume arbiter allowed to issue execution permits.
    pub const fn arbiter_id(&self) -> RangeResumeArbiterId {
        self.arbiter_id
    }

    /// Returns the only owned runtime job identity.
    pub const fn job_id(&self) -> JobId {
        self.job_id
    }

    /// Returns the fixed generation required by every resume permit.
    pub const fn generation(&self) -> RangeResumeGeneration {
        self.generation
    }

    /// Returns the current owner lifecycle phase.
    pub fn phase(&self) -> StrictBaseOpenOwnerPhase {
        self.state.phase()
    }

    /// Returns the latest underlying strict-open parser phase.
    pub const fn job_phase(&self) -> StrictBaseOpenPhase {
        self.job_phase
    }

    /// Returns cumulative strict-open work through the last permitted poll.
    pub const fn stats(&self) -> StrictBaseOpenStats {
        self.stats
    }

    /// Returns the retained lower error after a failed permitted poll.
    pub fn failure(&self) -> Option<StrictBaseOpenError> {
        match &self.state {
            OwnerState::Failed(error) => Some(*error),
            _ => None,
        }
    }

    /// Returns the host source failure that terminated a waiting ticket.
    ///
    /// Parser-produced failures remain available through [`Self::failure`].
    pub fn source_failure(&self) -> Option<SourceError> {
        match &self.state {
            OwnerState::SourceFailed(error) => Some(*error),
            _ => None,
        }
    }

    /// Returns lower cancellation evidence when a token stopped a permitted poll.
    ///
    /// Explicit cancellation between turns has no lower error and returns `None`.
    pub fn cancellation_error(&self) -> Option<StrictBaseOpenError> {
        match &self.state {
            OwnerState::Cancelled(error) => *error,
            _ => None,
        }
    }

    /// Returns the exact current waiting target without exposing the owned job.
    pub fn waiting_target(&self) -> Option<RangeResumeTarget> {
        match &self.state {
            OwnerState::Waiting { target, .. } => Some(*target),
            _ => None,
        }
    }

    /// Returns current job and waiting-target counts.
    pub fn resources(&self) -> StrictBaseOpenOwnerResources {
        self.state.resources()
    }

    /// Executes the only permitted initial poll.
    ///
    /// Calling this after the owner leaves `Queued` returns `Rejected` without
    /// invoking the parser.
    pub fn start(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> StrictBaseOpenOwnerStart {
        if !matches!(self.state, OwnerState::Queued(_)) {
            return StrictBaseOpenOwnerStart::Rejected {
                phase: self.phase(),
            };
        }
        let previous = mem::replace(&mut self.state, OwnerState::Transition);
        let OwnerState::Queued(job) = previous else {
            unreachable!("the queued state was checked before replacement")
        };
        StrictBaseOpenOwnerStart::Polled(self.poll_job(job, source, cancellation))
    }

    /// Consumes one Range permit and polls only after every identity matches.
    ///
    /// Validation checks issuing arbiter and generation before job, checkpoint,
    /// and ticket. Any mismatch consumes the permit, preserves the owned job and
    /// its stats, and returns `Discarded` as normal stale-work control flow.
    pub fn resume(
        &mut self,
        permit: RangeResumePermit,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> StrictBaseOpenOwnerResume {
        let ticket = permit.ticket();
        let target = permit.target();
        let reason = self.discard_reason(permit.arbiter_id(), ticket, target);
        if let Some(reason) = reason {
            return StrictBaseOpenOwnerResume::Discarded {
                ticket,
                target,
                reason,
            };
        }

        let previous = mem::replace(&mut self.state, OwnerState::Transition);
        let OwnerState::Waiting { job, .. } = previous else {
            unreachable!("an exact permit requires the validated waiting state")
        };
        StrictBaseOpenOwnerResume::Polled(self.poll_job(job, source, cancellation))
    }

    /// Consumes one host-failure permit without polling parser code.
    ///
    /// An exact permit drops the waiting job and retains the lower byte-layer
    /// failure. A stale or mismatched permit is consumed while preserving the
    /// waiting job, parser phase, and cumulative statistics.
    pub fn fail_waiting(&mut self, permit: RangeResumeFailurePermit) -> StrictBaseOpenOwnerFail {
        let ticket = permit.ticket();
        let target = permit.target();
        let error = permit.error();
        if let Some(reason) = self.discard_reason(permit.arbiter_id(), ticket, target) {
            return StrictBaseOpenOwnerFail::Discarded {
                ticket,
                target,
                error,
                reason,
            };
        }

        let previous = mem::replace(&mut self.state, OwnerState::Transition);
        let OwnerState::Waiting { job, .. } = previous else {
            unreachable!("an exact failure permit requires the validated waiting state")
        };
        drop(job);
        self.state = OwnerState::SourceFailed(error);
        StrictBaseOpenOwnerFail::Failed {
            ticket,
            target,
            error,
        }
    }

    /// Cancels the active job between actor turns and returns its waiting target.
    pub fn cancel(&mut self) -> StrictBaseOpenOwnerCancelOutcome {
        match self.phase() {
            StrictBaseOpenOwnerPhase::Queued | StrictBaseOpenOwnerPhase::WaitingForData => {
                let target = self.waiting_target();
                let previous = mem::replace(&mut self.state, OwnerState::Cancelled(None));
                drop(previous);
                StrictBaseOpenOwnerCancelOutcome::Cancelled { target }
            }
            phase => StrictBaseOpenOwnerCancelOutcome::AlreadyTerminal { phase },
        }
    }

    /// Terminates the active job after the source snapshot loses integrity.
    pub fn signal_source_changed(&mut self) -> StrictBaseOpenOwnerSourceChangeOutcome {
        match self.phase() {
            StrictBaseOpenOwnerPhase::Queued | StrictBaseOpenOwnerPhase::WaitingForData => {
                let target = self.waiting_target();
                let previous = mem::replace(&mut self.state, OwnerState::SourceChanged);
                drop(previous);
                StrictBaseOpenOwnerSourceChangeOutcome::SourceChanged { target }
            }
            phase => StrictBaseOpenOwnerSourceChangeOutcome::AlreadyTerminal { phase },
        }
    }

    /// Drops any remaining job and returns one idempotent close report.
    pub fn close(&mut self) -> StrictBaseOpenOwnerCloseReport {
        if let OwnerState::Closed(report) = &self.state {
            return *report;
        }
        let previous_phase = self.phase();
        let resources = self.resources();
        let report = StrictBaseOpenOwnerCloseReport {
            previous_phase,
            released_jobs: resources.jobs,
            released_waiting_targets: resources.waiting_targets,
        };
        let previous = mem::replace(&mut self.state, OwnerState::Closed(report));
        drop(previous);
        report
    }

    fn poll_job(
        &mut self,
        mut job: OpenStrictBaseRevisionJob,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> StrictBaseOpenOwnerPoll {
        let outcome = job.poll(source, cancellation);
        self.stats = job.stats();
        self.job_phase = job.phase();
        match outcome {
            StrictBaseOpenPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                let target = RangeResumeTarget::new(self.job_id, checkpoint, self.generation);
                self.state = OwnerState::Waiting {
                    job,
                    ticket,
                    target,
                };
                StrictBaseOpenOwnerPoll::WaitingForData {
                    ticket,
                    missing,
                    target,
                }
            }
            StrictBaseOpenPoll::Ready(index) => {
                self.state = OwnerState::Ready;
                StrictBaseOpenOwnerPoll::Ready(index)
            }
            StrictBaseOpenPoll::Failed(error) => {
                if error.is_cancelled() {
                    self.state = OwnerState::Cancelled(Some(error));
                    StrictBaseOpenOwnerPoll::Cancelled(error)
                } else {
                    self.state = OwnerState::Failed(error);
                    StrictBaseOpenOwnerPoll::Failed(error)
                }
            }
        }
    }

    fn discard_reason(
        &self,
        arbiter_id: RangeResumeArbiterId,
        ticket: DataTicket,
        target: RangeResumeTarget,
    ) -> Option<StrictBaseOpenResumeDiscardReason> {
        if arbiter_id != self.arbiter_id {
            return Some(StrictBaseOpenResumeDiscardReason::ArbiterMismatch);
        }
        if target.generation() != self.generation {
            return Some(StrictBaseOpenResumeDiscardReason::StaleGeneration);
        }
        if target.job() != self.job_id {
            return Some(StrictBaseOpenResumeDiscardReason::JobMismatch);
        }
        match &self.state {
            OwnerState::Waiting {
                target: expected_target,
                ..
            } if target.checkpoint() != expected_target.checkpoint() => {
                Some(StrictBaseOpenResumeDiscardReason::CheckpointMismatch)
            }
            OwnerState::Waiting {
                ticket: expected_ticket,
                ..
            } if ticket != *expected_ticket => {
                Some(StrictBaseOpenResumeDiscardReason::TicketMismatch)
            }
            OwnerState::Waiting { .. } => None,
            _ => Some(StrictBaseOpenResumeDiscardReason::NotWaiting(self.phase())),
        }
    }
}

impl fmt::Debug for StrictBaseOpenJobOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StrictBaseOpenJobOwner")
            .field("arbiter_id", &self.arbiter_id)
            .field("job_id", &self.job_id)
            .field("generation", &self.generation)
            .field("phase", &self.phase())
            .field("job_phase", &self.job_phase)
            .field("stats", &self.stats)
            .field("waiting_target", &self.waiting_target())
            .field("resources", &self.resources())
            .field("failure", &self.failure())
            .field("source_failure", &self.source_failure())
            .field("cancellation_error", &self.cancellation_error())
            .finish()
    }
}
