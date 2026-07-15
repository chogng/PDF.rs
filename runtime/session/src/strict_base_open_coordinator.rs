use std::fmt;

use pdf_rs_bytes::{
    DataTicket, RangeResponse, RangeStoreLimits, ResumeCheckpoint, SmallRanges, SourceError,
    SourceSnapshot,
};
use pdf_rs_document::{
    AttestedRevisionIndex, DocumentCancellation, OpenStrictBaseRevisionJob, StrictBaseOpenError,
    StrictBaseOpenPhase, StrictBaseOpenStats,
};

use crate::{
    RangeResumeArbiter, RangeResumeCancelOutcome, RangeResumeCompletion, RangeResumeError,
    RangeResumeErrorCategory, RangeResumeFailureOutcome, RangeResumeGeneration, RangeResumePhase,
    RangeResumeRegistrationOutcome, RangeResumeReleaseReport, RangeResumeResources,
    RangeResumeSupplyOutcome, StrictBaseOpenJobOwner, StrictBaseOpenOwnerCancelOutcome,
    StrictBaseOpenOwnerCloseReport, StrictBaseOpenOwnerFail, StrictBaseOpenOwnerPhase,
    StrictBaseOpenOwnerPoll, StrictBaseOpenOwnerResume, StrictBaseOpenOwnerSourceChangeOutcome,
    StrictBaseOpenOwnerStart,
};

/// Public lifecycle phase of one strict-base opening coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenCoordinatorPhase {
    /// The privately owned job has not executed its first parser poll.
    Queued,
    /// The job is registered on one pending Range ticket.
    WaitingForData,
    /// Immutable data completed the ticket and one resume turn is queued.
    ResumeQueued,
    /// Host availability failed and one non-polling failure turn is queued.
    FailureQueued,
    /// The successful index and its source owner were moved to a Ready handoff.
    ReadyHandedOff,
    /// Parser, source, or runtime coordination failed before Ready publication.
    Failed,
    /// Cancellation won before Ready publication.
    Cancelled,
    /// Source integrity changed before Ready publication.
    SourceChanged,
    /// Explicit coordinator close completed.
    Closed,
}

/// Complete failure evidence retained by the strict-open coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenCoordinatorFailure {
    /// The strict parser job produced a stable child-layer failure.
    Parser(StrictBaseOpenError),
    /// A host ticket failure terminated the exact waiting job.
    Source(SourceError),
    /// Range ownership or actor-turn coordination failed closed.
    Runtime(RangeResumeError),
}

/// Reason host ingress was rejected without changing coordinator state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenIngressRejectReason {
    /// Opening has not suspended on host data yet.
    NotWaiting,
    /// A committed terminal or Ready handoff no longer accepts opening data.
    TerminalPhase,
    /// The active Range store rejected this response, snapshot, or ticket.
    Range(RangeResumeError),
}

/// Result of one host data, metadata, or availability callback.
///
/// No variant executes parser code. `wake_scheduler` reports only newly queued
/// work; the caller must invoke [`StrictBaseOpenCoordinator::run_one`] on a
/// separate actor turn to make parser progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenIngress {
    /// The callback was accepted by the private Range owner.
    Accepted {
        /// Whether this callback newly queued one coordinator run.
        wake_scheduler: bool,
        /// Unique immutable bytes currently cached by the source owner.
        cached_bytes: u64,
    },
    /// Snapshot integrity failed and both opening owners were terminated.
    SourceChanged {
        /// Complete lower Range evidence when the callback detected the change.
        error: Option<RangeResumeError>,
    },
    /// An internal ownership invariant failed and both owners were closed.
    Failed(StrictBaseOpenCoordinatorFailure),
    /// The callback was ignored without changing active or terminal state.
    Rejected {
        /// Coordinator phase that rejected the callback.
        phase: StrictBaseOpenCoordinatorPhase,
        /// Stable rejection evidence.
        reason: StrictBaseOpenIngressRejectReason,
    },
}

/// Result of one bounded coordinator execution turn.
///
/// A turn performs at most one parser poll and consumes at most one Range
/// completion. Host-failure completion does not poll or probe cancellation.
pub enum StrictBaseOpenCoordinatorRun {
    /// The parser suspended and registration completed before this was returned.
    WaitingForData {
        /// Exact host-visible ticket for the suspension.
        ticket: DataTicket,
        /// Canonical source ranges still missing for this ticket.
        missing: SmallRanges,
    },
    /// No completion was ready, so no parser or cancellation code executed.
    NoWork,
    /// Opening succeeded and moved its index plus source owner out exactly once.
    Ready(StrictBaseOpenReady),
    /// Opening reached its only failed terminal result.
    Failed(StrictBaseOpenCoordinatorFailure),
    /// A cancellation probe stopped the parser during this permitted turn.
    Cancelled {
        /// Complete lower parser cancellation evidence.
        error: StrictBaseOpenError,
    },
    /// Source integrity changed before a poll could commit another result.
    SourceChanged {
        /// Complete lower Range evidence when available.
        error: Option<RangeResumeError>,
    },
    /// A prior Ready or terminal result already won, so no work executed.
    AlreadyTerminal {
        /// Winning coordinator phase.
        phase: StrictBaseOpenCoordinatorPhase,
    },
}

impl fmt::Debug for StrictBaseOpenCoordinatorRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WaitingForData { ticket, missing } => formatter
                .debug_struct("WaitingForData")
                .field("ticket", ticket)
                .field("missing", missing)
                .finish(),
            Self::NoWork => formatter.write_str("NoWork"),
            Self::Ready(ready) => formatter.debug_tuple("Ready").field(ready).finish(),
            Self::Failed(error) => formatter.debug_tuple("Failed").field(error).finish(),
            Self::Cancelled { error } => formatter
                .debug_struct("Cancelled")
                .field("error", error)
                .finish(),
            Self::SourceChanged { error } => formatter
                .debug_struct("SourceChanged")
                .field("error", error)
                .finish(),
            Self::AlreadyTerminal { phase } => formatter
                .debug_struct("AlreadyTerminal")
                .field("phase", phase)
                .finish(),
        }
    }
}

/// Result of cancelling one coordinator between synchronous actor turns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenCoordinatorCancel {
    /// Queued, pending, or completed-but-undispatched work was removed.
    Cancelled,
    /// A prior Ready or terminal result remained authoritative.
    AlreadyTerminal {
        /// Winning coordinator phase.
        phase: StrictBaseOpenCoordinatorPhase,
    },
    /// Owner disagreement caused a fail-closed runtime terminal.
    Failed(StrictBaseOpenCoordinatorFailure),
}

/// Result of reporting source-integrity change to one coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrictBaseOpenCoordinatorSourceChange {
    /// Both active owners terminated and released their resources.
    SourceChanged,
    /// A prior Ready or terminal result remained authoritative.
    AlreadyTerminal {
        /// Winning coordinator phase.
        phase: StrictBaseOpenCoordinatorPhase,
    },
    /// Owner disagreement caused a fail-closed runtime terminal.
    Failed(StrictBaseOpenCoordinatorFailure),
}

/// Current resources still owned by the strict-open coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StrictBaseOpenCoordinatorResources {
    jobs: usize,
    waiting_targets: usize,
    registrations: usize,
    pending_tickets: usize,
    ready_resumes: usize,
    queued_failures: usize,
    cached_bytes: u64,
    registration_metadata_bytes: u64,
    source_resident_bytes: u64,
    resident_bytes: u64,
}

impl StrictBaseOpenCoordinatorResources {
    /// Returns privately retained strict-open jobs.
    pub const fn jobs(self) -> usize {
        self.jobs
    }

    /// Returns exact job targets awaiting one terminal ticket completion.
    pub const fn waiting_targets(self) -> usize {
        self.waiting_targets
    }

    /// Returns pending plus completed-but-not-consumed Range registrations.
    pub const fn registrations(self) -> usize {
        self.registrations
    }

    /// Returns distinct Range tickets still awaiting a lower terminal result.
    pub const fn pending_tickets(self) -> usize {
        self.pending_tickets
    }

    /// Returns queued data-ready completions awaiting one `run_one` turn.
    pub const fn ready_resumes(self) -> usize {
        self.ready_resumes
    }

    /// Returns queued host-failure completions awaiting one `run_one` turn.
    pub const fn queued_failures(self) -> usize {
        self.queued_failures
    }

    /// Returns unique immutable bytes cached by the private source owner.
    pub const fn cached_bytes(self) -> u64 {
        self.cached_bytes
    }

    /// Returns allocator-capacity bytes precharged for Range registrations.
    pub const fn registration_metadata_bytes(self) -> u64 {
        self.registration_metadata_bytes
    }

    /// Returns source backing capacity retained by the Range store.
    pub const fn source_resident_bytes(self) -> u64 {
        self.source_resident_bytes
    }

    /// Returns checked Range registration plus source owner capacity.
    pub const fn resident_bytes(self) -> u64 {
        self.resident_bytes
    }
}

/// Stable evidence returned after explicit coordinator close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StrictBaseOpenCoordinatorCloseReport {
    previous_phase: StrictBaseOpenCoordinatorPhase,
    owner: StrictBaseOpenOwnerCloseReport,
    source: Option<RangeResumeReleaseReport>,
    failure: Option<StrictBaseOpenCoordinatorFailure>,
    source_change_error: Option<RangeResumeError>,
}

impl StrictBaseOpenCoordinatorCloseReport {
    /// Returns the coordinator phase observed before the first close.
    pub const fn previous_phase(self) -> StrictBaseOpenCoordinatorPhase {
        self.previous_phase
    }

    /// Returns job-owner release evidence from the first close.
    pub const fn owner(self) -> StrictBaseOpenOwnerCloseReport {
        self.owner
    }

    /// Returns source-owner release evidence when it had not been handed off.
    pub const fn source(self) -> Option<RangeResumeReleaseReport> {
        self.source
    }

    /// Returns the semantic failure that won before close, if any.
    pub const fn failure(self) -> Option<StrictBaseOpenCoordinatorFailure> {
        self.failure
    }

    /// Returns lower source-change evidence that won before close, if any.
    pub const fn source_change_error(self) -> Option<RangeResumeError> {
        self.source_change_error
    }
}

/// Move-only successful strict-open handoff.
///
/// The attested index remains bound to the same private source owner that
/// supplied its bytes. Public code may inspect the index and accounting, but it
/// cannot extract a raw byte source or Range arbiter.
pub struct StrictBaseOpenReady {
    index: AttestedRevisionIndex,
    source_owner: RangeResumeArbiter,
}

impl StrictBaseOpenReady {
    /// Borrows the only successfully attested base-revision index.
    pub const fn index(&self) -> &AttestedRevisionIndex {
        &self.index
    }

    /// Returns source resources retained for later document-service ownership.
    pub fn source_resources(&self) -> RangeResumeResources {
        self.source_owner.resources()
    }

    /// Drops the source owner and returns its release evidence.
    pub fn close(mut self) -> RangeResumeReleaseReport {
        self.source_owner.close()
    }

    #[allow(
        dead_code,
        reason = "a later complete Ready-session actor will consume this internal handoff"
    )]
    pub(crate) fn into_parts(self) -> (AttestedRevisionIndex, RangeResumeArbiter) {
        (self.index, self.source_owner)
    }
}

impl fmt::Debug for StrictBaseOpenReady {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StrictBaseOpenReady")
            .field("index", &self.index)
            .field("source_resources", &self.source_resources())
            .finish()
    }
}

/// Exclusive actor-turn coordinator for one strict base-revision open.
///
/// This type is the only owner allowed to borrow the Range source for its job,
/// register a returned suspension, take a terminal completion, and consume its
/// permit. [`Self::run_one`] is the only method that can execute parser code.
/// Host ingress only mutates Range state and may request a later scheduler wake.
pub struct StrictBaseOpenCoordinator {
    snapshot: SourceSnapshot,
    generation: RangeResumeGeneration,
    owner: StrictBaseOpenJobOwner,
    source_owner: Option<RangeResumeArbiter>,
    phase: StrictBaseOpenCoordinatorPhase,
    failure: Option<StrictBaseOpenCoordinatorFailure>,
    source_change_error: Option<RangeResumeError>,
    close_report: Option<StrictBaseOpenCoordinatorCloseReport>,
}

impl StrictBaseOpenCoordinator {
    /// Constructs the private Range and job owners from the job's exact snapshot.
    pub fn new(
        job: OpenStrictBaseRevisionJob,
        generation: RangeResumeGeneration,
        range_limits: RangeStoreLimits,
    ) -> Result<Self, RangeResumeError> {
        let snapshot = job.snapshot();
        let source_owner = RangeResumeArbiter::new(snapshot, range_limits)?;
        let owner = StrictBaseOpenJobOwner::new(job, generation, source_owner.arbiter_id());
        Ok(Self {
            snapshot,
            generation,
            owner,
            source_owner: Some(source_owner),
            phase: StrictBaseOpenCoordinatorPhase::Queued,
            failure: None,
            source_change_error: None,
            close_report: None,
        })
    }

    /// Returns the current coordinator phase.
    pub const fn phase(&self) -> StrictBaseOpenCoordinatorPhase {
        self.phase
    }

    /// Returns the exact immutable source snapshot retained through opening.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the latest underlying parser phase.
    pub const fn job_phase(&self) -> StrictBaseOpenPhase {
        self.owner.job_phase()
    }

    /// Returns cumulative parser work through the last permitted poll.
    pub const fn stats(&self) -> StrictBaseOpenStats {
        self.owner.stats()
    }

    /// Returns the parser checkpoint currently registered on host data.
    pub fn waiting_checkpoint(&self) -> Option<ResumeCheckpoint> {
        self.owner
            .waiting_target()
            .map(|target| target.checkpoint())
    }

    /// Returns the winning failed-terminal evidence, if any.
    pub const fn failure(&self) -> Option<StrictBaseOpenCoordinatorFailure> {
        self.failure
    }

    /// Returns lower parser cancellation evidence after a cancelled poll.
    pub fn cancellation_error(&self) -> Option<StrictBaseOpenError> {
        self.owner.cancellation_error()
    }

    /// Returns lower integrity evidence after a SourceChanged terminal.
    pub const fn source_change_error(&self) -> Option<RangeResumeError> {
        self.source_change_error
    }

    /// Returns current coordinator-owned job and Range resources.
    pub fn resources(&self) -> StrictBaseOpenCoordinatorResources {
        let owner = self.owner.resources();
        let source = self
            .source_owner
            .as_ref()
            .map(RangeResumeArbiter::resources);
        StrictBaseOpenCoordinatorResources {
            jobs: owner.jobs(),
            waiting_targets: owner.waiting_targets(),
            registrations: source.map_or(0, RangeResumeResources::registrations),
            pending_tickets: source.map_or(0, RangeResumeResources::pending_tickets),
            ready_resumes: source.map_or(0, RangeResumeResources::ready_resumes),
            queued_failures: source.map_or(0, RangeResumeResources::queued_failures),
            cached_bytes: source.map_or(0, RangeResumeResources::cached_bytes),
            registration_metadata_bytes: source
                .map_or(0, RangeResumeResources::registration_metadata_bytes),
            source_resident_bytes: source.map_or(0, RangeResumeResources::source_resident_bytes),
            resident_bytes: source.map_or(0, RangeResumeResources::resident_bytes),
        }
    }

    /// Executes at most one parser poll or consumes one failure completion.
    pub fn run_one(
        &mut self,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> StrictBaseOpenCoordinatorRun {
        match self.phase {
            StrictBaseOpenCoordinatorPhase::Queued => self.start_one(cancellation),
            StrictBaseOpenCoordinatorPhase::WaitingForData
            | StrictBaseOpenCoordinatorPhase::ResumeQueued
            | StrictBaseOpenCoordinatorPhase::FailureQueued => self.consume_one(cancellation),
            phase => StrictBaseOpenCoordinatorRun::AlreadyTerminal { phase },
        }
    }

    /// Supplies one snapshot-bound host response without polling parser code.
    pub fn supply(&mut self, response: RangeResponse) -> StrictBaseOpenIngress {
        if !self.accepts_ingress() {
            return self.terminal_ingress_rejection();
        }
        let result = self
            .source_owner
            .as_mut()
            .expect("active coordinator retains its source owner")
            .supply(response);
        self.finish_supply_ingress(result)
    }

    /// Observes complete source metadata without polling parser code.
    pub fn observe_snapshot(&mut self, observed: SourceSnapshot) -> StrictBaseOpenIngress {
        if !self.accepts_ingress() {
            return self.terminal_ingress_rejection();
        }
        let result = self
            .source_owner
            .as_mut()
            .expect("active coordinator retains its source owner")
            .observe_snapshot(observed);
        self.finish_supply_ingress(result)
    }

    /// Queues one host ticket failure without polling or probing cancellation.
    pub fn fail_data(&mut self, ticket: DataTicket) -> StrictBaseOpenIngress {
        if !self.accepts_ingress() {
            return self.terminal_ingress_rejection();
        }
        let result = self
            .source_owner
            .as_mut()
            .expect("active coordinator retains its source owner")
            .fail_ticket(ticket);
        self.finish_failure_ingress(result)
    }

    /// Cancels queued or waiting work without executing parser code.
    pub fn cancel(&mut self) -> StrictBaseOpenCoordinatorCancel {
        match self.phase {
            StrictBaseOpenCoordinatorPhase::Queued => {
                if !matches!(
                    self.owner.cancel(),
                    StrictBaseOpenOwnerCancelOutcome::Cancelled { target: None }
                ) {
                    return StrictBaseOpenCoordinatorCancel::Failed(self.fail_invariant());
                }
            }
            StrictBaseOpenCoordinatorPhase::WaitingForData
            | StrictBaseOpenCoordinatorPhase::ResumeQueued
            | StrictBaseOpenCoordinatorPhase::FailureQueued => {
                let expected = self
                    .owner
                    .waiting_target()
                    .expect("a waiting coordinator retains its exact target");
                let cancelled = self
                    .source_owner
                    .as_mut()
                    .expect("active coordinator retains its source owner")
                    .cancel(self.owner.job_id(), self.generation);
                match cancelled {
                    Ok(RangeResumeCancelOutcome::Cancelled { target }) if target == expected => {}
                    Ok(RangeResumeCancelOutcome::Cancelled { .. })
                    | Ok(RangeResumeCancelOutcome::NotPending) => {
                        return StrictBaseOpenCoordinatorCancel::Failed(self.fail_invariant());
                    }
                    Err(error) => {
                        return StrictBaseOpenCoordinatorCancel::Failed(self.fail_runtime(error));
                    }
                }
                if !matches!(
                    self.owner.cancel(),
                    StrictBaseOpenOwnerCancelOutcome::Cancelled {
                        target: Some(target)
                    } if target == expected
                ) {
                    return StrictBaseOpenCoordinatorCancel::Failed(self.fail_invariant());
                }
            }
            phase => return StrictBaseOpenCoordinatorCancel::AlreadyTerminal { phase },
        }
        self.close_source_owner();
        self.phase = StrictBaseOpenCoordinatorPhase::Cancelled;
        StrictBaseOpenCoordinatorCancel::Cancelled
    }

    /// Terminates active opening after immutable source identity changed.
    pub fn signal_source_changed(&mut self) -> StrictBaseOpenCoordinatorSourceChange {
        match self.phase {
            StrictBaseOpenCoordinatorPhase::Queued
            | StrictBaseOpenCoordinatorPhase::WaitingForData
            | StrictBaseOpenCoordinatorPhase::ResumeQueued
            | StrictBaseOpenCoordinatorPhase::FailureQueued => {}
            phase => return StrictBaseOpenCoordinatorSourceChange::AlreadyTerminal { phase },
        }
        if let Err(error) = self
            .source_owner
            .as_mut()
            .expect("active coordinator retains its source owner")
            .signal_source_changed()
        {
            return StrictBaseOpenCoordinatorSourceChange::Failed(self.fail_runtime(error));
        }
        if !matches!(
            self.owner.signal_source_changed(),
            StrictBaseOpenOwnerSourceChangeOutcome::SourceChanged { .. }
        ) {
            return StrictBaseOpenCoordinatorSourceChange::Failed(self.fail_invariant());
        }
        self.phase = StrictBaseOpenCoordinatorPhase::SourceChanged;
        StrictBaseOpenCoordinatorSourceChange::SourceChanged
    }

    /// Drops every resource still owned by the coordinator and saves one report.
    ///
    /// A prior Ready handoff owns its source independently and is not reclaimed
    /// by closing the empty coordinator shell.
    pub fn close(&mut self) -> StrictBaseOpenCoordinatorCloseReport {
        if let Some(report) = self.close_report {
            return report;
        }
        let previous_phase = self.phase;
        let source = self.source_owner.as_mut().map(RangeResumeArbiter::close);
        let owner = self.owner.close();
        let report = StrictBaseOpenCoordinatorCloseReport {
            previous_phase,
            owner,
            source,
            failure: self.failure,
            source_change_error: self.source_change_error,
        };
        self.phase = StrictBaseOpenCoordinatorPhase::Closed;
        self.close_report = Some(report);
        report
    }

    /// Returns the saved report only after explicit close.
    pub const fn close_report(&self) -> Option<StrictBaseOpenCoordinatorCloseReport> {
        self.close_report
    }

    fn start_one(
        &mut self,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> StrictBaseOpenCoordinatorRun {
        let poll = {
            let source = self
                .source_owner
                .as_ref()
                .expect("queued coordinator retains its source owner")
                .byte_source();
            let source = match source {
                Ok(source) => source,
                Err(error) => return self.run_from_range_error(error),
            };
            self.owner.start(source, cancellation)
        };
        match poll {
            StrictBaseOpenOwnerStart::Polled(poll) => self.finish_poll(poll),
            StrictBaseOpenOwnerStart::Rejected { .. } => {
                StrictBaseOpenCoordinatorRun::Failed(self.fail_invariant())
            }
        }
    }

    fn consume_one(
        &mut self,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> StrictBaseOpenCoordinatorRun {
        let completion = self
            .source_owner
            .as_mut()
            .expect("waiting coordinator retains its source owner")
            .take_completion();
        let completion = match completion {
            Ok(completion) => completion,
            Err(error) => return self.run_from_range_error(error),
        };
        match completion {
            RangeResumeCompletion::Empty
                if self.phase == StrictBaseOpenCoordinatorPhase::WaitingForData =>
            {
                StrictBaseOpenCoordinatorRun::NoWork
            }
            RangeResumeCompletion::Empty => {
                StrictBaseOpenCoordinatorRun::Failed(self.fail_invariant())
            }
            RangeResumeCompletion::Resume(permit) => {
                if self.phase != StrictBaseOpenCoordinatorPhase::ResumeQueued {
                    return StrictBaseOpenCoordinatorRun::Failed(self.fail_invariant());
                }
                let outcome = {
                    let source = self
                        .source_owner
                        .as_ref()
                        .expect("resume turn retains its source owner")
                        .byte_source();
                    let source = match source {
                        Ok(source) => source,
                        Err(error) => return self.run_from_range_error(error),
                    };
                    self.owner.resume(permit, source, cancellation)
                };
                match outcome {
                    StrictBaseOpenOwnerResume::Polled(poll) => self.finish_poll(poll),
                    StrictBaseOpenOwnerResume::Discarded { .. } => {
                        StrictBaseOpenCoordinatorRun::Failed(self.fail_invariant())
                    }
                }
            }
            RangeResumeCompletion::Failed(permit) => {
                if self.phase != StrictBaseOpenCoordinatorPhase::FailureQueued {
                    return StrictBaseOpenCoordinatorRun::Failed(self.fail_invariant());
                }
                match self.owner.fail_waiting(permit) {
                    StrictBaseOpenOwnerFail::Failed { error, .. } => {
                        let failure = StrictBaseOpenCoordinatorFailure::Source(error);
                        self.failure = Some(failure);
                        self.close_source_owner();
                        self.phase = StrictBaseOpenCoordinatorPhase::Failed;
                        StrictBaseOpenCoordinatorRun::Failed(failure)
                    }
                    StrictBaseOpenOwnerFail::Discarded { .. } => {
                        StrictBaseOpenCoordinatorRun::Failed(self.fail_invariant())
                    }
                }
            }
        }
    }

    fn finish_poll(&mut self, poll: StrictBaseOpenOwnerPoll) -> StrictBaseOpenCoordinatorRun {
        match poll {
            StrictBaseOpenOwnerPoll::WaitingForData {
                ticket,
                missing,
                target,
            } => {
                let registered = self
                    .source_owner
                    .as_mut()
                    .expect("pending poll retains its source owner")
                    .register_pending(ticket, target);
                match registered {
                    Ok(RangeResumeRegistrationOutcome::Registered) => {
                        if self.sync_active_wait_phase().is_err() {
                            return StrictBaseOpenCoordinatorRun::Failed(self.fail_invariant());
                        }
                        StrictBaseOpenCoordinatorRun::WaitingForData { ticket, missing }
                    }
                    Ok(RangeResumeRegistrationOutcome::AlreadyRegistered) => {
                        StrictBaseOpenCoordinatorRun::Failed(self.fail_invariant())
                    }
                    Err(error) => self.run_from_range_error(error),
                }
            }
            StrictBaseOpenOwnerPoll::Ready(index) => {
                let source_owner = self
                    .source_owner
                    .as_ref()
                    .expect("ready poll retains its source owner");
                let resources = source_owner.resources();
                if source_owner.phase() != RangeResumePhase::Active
                    || resources.registrations() != 0
                    || resources.pending_tickets() != 0
                    || resources.ready_resumes() != 0
                    || resources.queued_failures() != 0
                {
                    return StrictBaseOpenCoordinatorRun::Failed(self.fail_invariant());
                }
                let source_owner = self
                    .source_owner
                    .take()
                    .expect("Ready handoff moves its only source owner");
                self.phase = StrictBaseOpenCoordinatorPhase::ReadyHandedOff;
                StrictBaseOpenCoordinatorRun::Ready(StrictBaseOpenReady {
                    index,
                    source_owner,
                })
            }
            StrictBaseOpenOwnerPoll::Failed(error) => {
                let failure = StrictBaseOpenCoordinatorFailure::Parser(error);
                self.failure = Some(failure);
                self.close_source_owner();
                self.phase = StrictBaseOpenCoordinatorPhase::Failed;
                StrictBaseOpenCoordinatorRun::Failed(failure)
            }
            StrictBaseOpenOwnerPoll::Cancelled(error) => {
                self.close_source_owner();
                self.phase = StrictBaseOpenCoordinatorPhase::Cancelled;
                StrictBaseOpenCoordinatorRun::Cancelled { error }
            }
        }
    }

    fn finish_supply_ingress(
        &mut self,
        result: Result<RangeResumeSupplyOutcome, RangeResumeError>,
    ) -> StrictBaseOpenIngress {
        match result {
            Ok(outcome) => match self.sync_active_wait_phase() {
                Ok(()) => StrictBaseOpenIngress::Accepted {
                    wake_scheduler: outcome.queued_requeues() != 0,
                    cached_bytes: outcome.cached_bytes(),
                },
                Err(()) => StrictBaseOpenIngress::Failed(self.fail_invariant()),
            },
            Err(error) => self.ingress_from_range_error(error),
        }
    }

    fn finish_failure_ingress(
        &mut self,
        result: Result<RangeResumeFailureOutcome, RangeResumeError>,
    ) -> StrictBaseOpenIngress {
        match result {
            Ok(outcome) => match self.sync_active_wait_phase() {
                Ok(()) => StrictBaseOpenIngress::Accepted {
                    wake_scheduler: outcome.queued_failures() != 0,
                    cached_bytes: self.resources().cached_bytes(),
                },
                Err(()) => StrictBaseOpenIngress::Failed(self.fail_invariant()),
            },
            Err(error) => self.ingress_from_range_error(error),
        }
    }

    fn sync_active_wait_phase(&mut self) -> Result<(), ()> {
        if self.owner.phase() != StrictBaseOpenOwnerPhase::WaitingForData {
            return Err(());
        }
        let resources = self.source_owner.as_ref().ok_or(())?.resources();
        if resources.registrations() != 1 {
            return Err(());
        }
        self.phase = match (
            resources.pending_tickets(),
            resources.ready_resumes(),
            resources.queued_failures(),
        ) {
            (1, 0, 0) => StrictBaseOpenCoordinatorPhase::WaitingForData,
            (0, 1, 0) => StrictBaseOpenCoordinatorPhase::ResumeQueued,
            (0, 0, 1) => StrictBaseOpenCoordinatorPhase::FailureQueued,
            _ => return Err(()),
        };
        Ok(())
    }

    fn ingress_from_range_error(&mut self, error: RangeResumeError) -> StrictBaseOpenIngress {
        if error.category() == RangeResumeErrorCategory::Integrity {
            self.transition_source_changed(Some(error));
            return StrictBaseOpenIngress::SourceChanged { error: Some(error) };
        }
        match self.source_owner.as_ref().map(RangeResumeArbiter::phase) {
            Some(RangeResumePhase::Failed) => {
                StrictBaseOpenIngress::Failed(self.fail_runtime(error))
            }
            Some(RangeResumePhase::Active) => StrictBaseOpenIngress::Rejected {
                phase: self.phase,
                reason: StrictBaseOpenIngressRejectReason::Range(error),
            },
            _ => StrictBaseOpenIngress::Failed(self.fail_runtime(error)),
        }
    }

    fn run_from_range_error(&mut self, error: RangeResumeError) -> StrictBaseOpenCoordinatorRun {
        if error.category() == RangeResumeErrorCategory::Integrity {
            self.transition_source_changed(Some(error));
            StrictBaseOpenCoordinatorRun::SourceChanged { error: Some(error) }
        } else {
            StrictBaseOpenCoordinatorRun::Failed(self.fail_runtime(error))
        }
    }

    fn transition_source_changed(&mut self, error: Option<RangeResumeError>) {
        self.source_change_error = error;
        if matches!(
            self.owner.phase(),
            StrictBaseOpenOwnerPhase::Queued | StrictBaseOpenOwnerPhase::WaitingForData
        ) {
            let _ = self.owner.signal_source_changed();
        } else {
            let _ = self.owner.close();
        }
        if let Some(source) = &mut self.source_owner
            && source.phase() == RangeResumePhase::Active
        {
            let _ = source.signal_source_changed();
        }
        self.phase = StrictBaseOpenCoordinatorPhase::SourceChanged;
    }

    fn fail_runtime(&mut self, error: RangeResumeError) -> StrictBaseOpenCoordinatorFailure {
        let failure = StrictBaseOpenCoordinatorFailure::Runtime(error);
        self.failure = Some(failure);
        self.close_source_owner();
        let _ = self.owner.close();
        self.phase = StrictBaseOpenCoordinatorPhase::Failed;
        failure
    }

    fn fail_invariant(&mut self) -> StrictBaseOpenCoordinatorFailure {
        self.fail_runtime(RangeResumeError::arbiter_failed())
    }

    fn close_source_owner(&mut self) {
        if let Some(source) = &mut self.source_owner {
            let _ = source.close();
        }
    }

    const fn accepts_ingress(&self) -> bool {
        matches!(
            self.phase,
            StrictBaseOpenCoordinatorPhase::WaitingForData
                | StrictBaseOpenCoordinatorPhase::ResumeQueued
                | StrictBaseOpenCoordinatorPhase::FailureQueued
        )
    }

    fn terminal_ingress_rejection(&self) -> StrictBaseOpenIngress {
        StrictBaseOpenIngress::Rejected {
            phase: self.phase,
            reason: if self.phase == StrictBaseOpenCoordinatorPhase::Queued {
                StrictBaseOpenIngressRejectReason::NotWaiting
            } else {
                StrictBaseOpenIngressRejectReason::TerminalPhase
            },
        }
    }
}

impl fmt::Debug for StrictBaseOpenCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StrictBaseOpenCoordinator")
            .field("generation", &self.generation)
            .field("phase", &self.phase)
            .field("job_phase", &self.job_phase())
            .field("stats", &self.stats())
            .field("resources", &self.resources())
            .field("failure", &self.failure)
            .field("source_change_error", &self.source_change_error)
            .field("close_report", &self.close_report)
            .finish()
    }
}
