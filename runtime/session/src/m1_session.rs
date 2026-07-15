use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    DataTicket, JobId, RangeResponse, RangeStoreLimits, ResumeCheckpoint, SmallRanges, SourceError,
    SourceSnapshot,
};
use pdf_rs_cache::{ReadyStoreBinding, ReadyStoreEpoch, ReadyStoreLimits, ReadyStoreSessionId};
use pdf_rs_document::{
    CountPagesJob, DocumentCancellation, DocumentError, OpenStrictBaseRevisionJob, Outline,
    OutlineJobContext, OutlineLimits, OutlinePoll, PageCount, PageCountPoll, PageTreeJobContext,
    PageTreeLimits, ReadOutlineJob, SharedAttestedRevisionIndex, StrictBaseOpenPhase,
    StrictBaseOpenStats,
};

use crate::{
    RangeResumeArbiter, RangeResumeCancelOutcome, RangeResumeCompletion, RangeResumeError,
    RangeResumeErrorCategory, RangeResumeFailurePermit, RangeResumeGeneration, RangeResumePermit,
    RangeResumePhase, RangeResumeRegistrationOutcome, RangeResumeReleaseReport, RangeResumeTarget,
    ReadySessionCloseReport, ReadySessionError, ReadySessionOwner, StrictBaseOpenCoordinator,
    StrictBaseOpenCoordinatorCloseReport, StrictBaseOpenCoordinatorFailure,
    StrictBaseOpenCoordinatorPhase, StrictBaseOpenCoordinatorRun, StrictBaseOpenIngress,
    StrictBaseOpenIngressRejectReason,
};

/// Opaque caller-issued identity for one M1 session request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct M1RequestId(u64);

impl M1RequestId {
    /// Wraps one caller-issued request identity.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric identity for protocol adaptation.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Complete caller-owned request, job, and generation identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct M1RequestIdentity {
    request_id: M1RequestId,
    job: JobId,
    generation: RangeResumeGeneration,
}

impl M1RequestIdentity {
    /// Creates one exact request identity without allocating any ID internally.
    pub const fn new(
        request_id: M1RequestId,
        job: JobId,
        generation: RangeResumeGeneration,
    ) -> Self {
        Self {
            request_id,
            job,
            generation,
        }
    }

    /// Returns the caller-issued request ID.
    pub const fn request_id(self) -> M1RequestId {
        self.request_id
    }

    /// Returns the caller-issued runtime job ID.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the caller-issued request generation.
    pub const fn generation(self) -> RangeResumeGeneration {
        self.generation
    }
}

/// The only two document services admitted by the bounded M1 actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1Service {
    /// Strict Catalog and page-tree counting.
    PageCount,
    /// Strict Catalog and outline traversal.
    Outline,
}

impl M1Service {
    const fn other(self) -> Self {
        match self {
            Self::PageCount => Self::Outline,
            Self::Outline => Self::PageCount,
        }
    }
}

/// Public lifecycle of the bounded M1 strict-document actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1SessionPhase {
    /// Owners were constructed but no parser poll has executed.
    Created,
    /// Strict opening has runnable actor work.
    Opening,
    /// Every active parser job is suspended on exact source data.
    WaitingForData,
    /// The attested index, source, and cache are owned and may accept services.
    Ready,
    /// Close was queued and awaits one non-parser actor turn.
    Closing,
    /// Ordered resource release completed.
    Closed,
    /// A session-wide failure won and all owned resources were released.
    Failed,
}

/// Stable request-admission rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1SessionRequestError {
    /// The lifecycle cannot admit a Ready service.
    NotReady(M1SessionPhase),
    /// The supplied generation does not match this session generation.
    StaleGeneration {
        /// The generation retained by the session.
        expected: RangeResumeGeneration,
        /// The rejected caller generation.
        actual: RangeResumeGeneration,
    },
    /// The service job context and request identity name different jobs.
    JobContextMismatch,
    /// This service already owns its one bounded job slot.
    SlotBusy(M1Service),
    /// Another active service already owns this request ID.
    DuplicateRequestId,
    /// Another active service already owns this job ID.
    DuplicateJobId,
    /// The lower proof-preserving document job rejected construction.
    Document(DocumentError),
    /// The bounded Range owner rejected construction.
    Runtime(RangeResumeError),
}

/// Reason an exact cancellation request was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1SessionCancelRejectReason {
    /// No active opening or service request has the supplied request ID.
    NotActive,
    /// The request ID exists but its job or generation does not match.
    IdentityMismatch,
    /// The lifecycle no longer accepts cancellation.
    TerminalPhase,
    /// Lower Range ownership disagreed with the retained waiting target.
    Runtime(RangeResumeError),
}

/// Result of queue-free cancellation between parser turns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1SessionCancel {
    /// The exact active request and any pending permit were synchronously removed.
    Cancelled {
        /// The cancelled request identity.
        request: M1RequestIdentity,
        /// The cancelled service, or `None` for opening.
        service: Option<M1Service>,
    },
    /// Cancellation changed no actor state.
    Rejected {
        /// The lifecycle that rejected cancellation.
        phase: M1SessionPhase,
        /// Stable identity or lifecycle reason.
        reason: M1SessionCancelRejectReason,
    },
}

/// Result of requesting idempotent close through host ingress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1SessionClose {
    /// One later actor turn must perform ordered release.
    Queued,
    /// A prior close request is already waiting for its actor turn.
    AlreadyClosing,
    /// Ordered close has already completed.
    AlreadyClosed(M1SessionCloseReport),
}

/// Stable host-ingress rejection reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1SessionIngressRejectReason {
    /// No parser job is currently subscribed to source data.
    NotWaiting,
    /// Closing, Closed, or Failed no longer accepts source ingress.
    TerminalPhase,
    /// The exact Range store rejected the response or ticket.
    Range(RangeResumeError),
}

/// Result of source ingress, which never polls a parser job inline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1SessionIngress {
    /// Source state accepted the callback.
    Accepted {
        /// Whether one later actor turn is now runnable.
        wake_scheduler: bool,
        /// Unique immutable bytes currently retained by the source owner.
        cached_bytes: u64,
    },
    /// Source integrity changed and the old session was terminated.
    SourceChanged,
    /// A session-wide runtime failure terminated the actor.
    Failed(M1SessionFailure),
    /// The callback was ignored without parser execution.
    Rejected {
        /// Lifecycle observed at rejection.
        phase: M1SessionPhase,
        /// Stable rejection reason.
        reason: M1SessionIngressRejectReason,
    },
}

/// Complete failure of one Ready service request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1ServiceFailure {
    /// The bounded document job returned a structured failure.
    Document(DocumentError),
    /// The host failed the exact pending source ticket.
    Source(SourceError),
}

/// Session-wide terminal failure that invalidates every active request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1SessionFailure {
    /// Strict base opening failed before Ready.
    Opening(StrictBaseOpenCoordinatorFailure),
    /// Cooperative cancellation stopped strict opening.
    OpeningCancelled,
    /// Immutable source identity or bytes changed.
    SourceChanged(Option<RangeResumeError>),
    /// Range ownership or completion identity failed closed.
    Runtime(RangeResumeError),
    /// Ready-cache construction failed during the proof-preserving handoff.
    Cache(ReadySessionError),
    /// A private bounded-session invariant failed closed.
    Internal,
}

/// Identity attached to one published `WaitingForData` result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M1SessionWait {
    /// Strict opening owns the suspension.
    Opening(M1RequestIdentity),
    /// One Ready service owns the suspension.
    Service {
        /// The suspended service slot.
        service: M1Service,
        /// Exact request identity retained by that slot.
        request: M1RequestIdentity,
    },
}

/// Result of one bounded actor execution turn.
///
/// A turn polls at most one parser job. It may privately collect at most two
/// already-terminal Range permits so strict round-robin selection can choose
/// fairly between the two service slots.
pub enum M1SessionRun {
    /// One job suspended after its exact ticket was registered.
    WaitingForData {
        /// Opening or service owner of the suspension.
        owner: M1SessionWait,
        /// Exact one-shot source ticket.
        ticket: DataTicket,
        /// Canonical ranges still missing.
        missing: SmallRanges,
    },
    /// Strict opening completed and all Ready owners were installed.
    Ready,
    /// One page-count request completed.
    PageCountReady {
        /// Exact caller request identity.
        request: M1RequestIdentity,
        /// Strict validated page-count result.
        result: PageCount,
    },
    /// One outline request completed.
    OutlineReady {
        /// Exact caller request identity.
        request: M1RequestIdentity,
        /// Strict validated outline result.
        result: Outline,
    },
    /// One service failed without invalidating the Ready document.
    RequestFailed {
        /// Failed service slot.
        service: M1Service,
        /// Exact caller request identity.
        request: M1RequestIdentity,
        /// Complete source-redacted service failure.
        failure: M1ServiceFailure,
    },
    /// No parser poll or terminal service disposition was runnable.
    NoWork,
    /// A session-wide failure won this turn.
    Failed(M1SessionFailure),
    /// Ordered close completed this turn.
    Closed(M1SessionCloseReport),
    /// Closed or Failed had no later executable work.
    AlreadyTerminal {
        /// Winning terminal phase.
        phase: M1SessionPhase,
    },
}

impl fmt::Debug for M1SessionRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WaitingForData {
                owner,
                ticket,
                missing,
            } => formatter
                .debug_struct("WaitingForData")
                .field("owner", owner)
                .field("ticket", ticket)
                .field("missing", missing)
                .finish(),
            Self::Ready => formatter.write_str("Ready"),
            Self::PageCountReady { request, result } => formatter
                .debug_struct("PageCountReady")
                .field("request", request)
                .field("result", result)
                .finish(),
            Self::OutlineReady { request, result } => formatter
                .debug_struct("OutlineReady")
                .field("request", request)
                .field("result", result)
                .finish(),
            Self::RequestFailed {
                service,
                request,
                failure,
            } => formatter
                .debug_struct("RequestFailed")
                .field("service", service)
                .field("request", request)
                .field("failure", failure)
                .finish(),
            Self::NoWork => formatter.write_str("NoWork"),
            Self::Failed(failure) => formatter.debug_tuple("Failed").field(failure).finish(),
            Self::Closed(report) => formatter.debug_tuple("Closed").field(report).finish(),
            Self::AlreadyTerminal { phase } => formatter
                .debug_struct("AlreadyTerminal")
                .field("phase", phase)
                .finish(),
        }
    }
}

/// Current resources retained by the bounded M1 actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct M1SessionResources {
    opening_jobs: usize,
    service_jobs: usize,
    waiting_targets: usize,
    held_completions: usize,
    range_registrations: usize,
    range_pending_tickets: usize,
    cached_bytes: u64,
    range_resident_bytes: u64,
    cache_entries: u64,
    cache_resident_bytes: u64,
    index_handles: usize,
    resident_bytes: u64,
}

/// Read-only evidence of opening parser state at an M1 actor boundary.
///
/// Host ingress may change Range completion ownership, but it must leave every
/// field in this snapshot unchanged. Only [`M1StrictDocumentSession::run_one`]
/// may advance these parser-owned values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct M1OpeningParserAudit {
    job_phase: StrictBaseOpenPhase,
    stats: StrictBaseOpenStats,
    waiting_checkpoint: Option<ResumeCheckpoint>,
}

impl M1OpeningParserAudit {
    /// Returns the resumable child-parser phase retained by the opening owner.
    pub const fn job_phase(self) -> StrictBaseOpenPhase {
        self.job_phase
    }

    /// Returns cumulative parser work committed by prior actor turns.
    pub const fn stats(self) -> StrictBaseOpenStats {
        self.stats
    }

    /// Returns the exact checkpoint still waiting on host data, when suspended.
    pub const fn waiting_checkpoint(self) -> Option<ResumeCheckpoint> {
        self.waiting_checkpoint
    }
}

impl M1SessionResources {
    const ZERO: Self = Self {
        opening_jobs: 0,
        service_jobs: 0,
        waiting_targets: 0,
        held_completions: 0,
        range_registrations: 0,
        range_pending_tickets: 0,
        cached_bytes: 0,
        range_resident_bytes: 0,
        cache_entries: 0,
        cache_resident_bytes: 0,
        index_handles: 0,
        resident_bytes: 0,
    };

    /// Returns strict-open jobs still retained.
    pub const fn opening_jobs(self) -> usize {
        self.opening_jobs
    }

    /// Returns page-count plus outline jobs still retained.
    pub const fn service_jobs(self) -> usize {
        self.service_jobs
    }

    /// Returns service or opening jobs suspended on exact targets.
    pub const fn waiting_targets(self) -> usize {
        self.waiting_targets
    }

    /// Returns move-only Range completions privately held for fair selection.
    pub const fn held_completions(self) -> usize {
        self.held_completions
    }

    /// Returns pending plus terminal-but-not-collected Range registrations.
    pub const fn range_registrations(self) -> usize {
        self.range_registrations
    }

    /// Returns distinct Range tickets still waiting for host completion.
    pub const fn range_pending_tickets(self) -> usize {
        self.range_pending_tickets
    }

    /// Returns unique immutable bytes retained by the source owner.
    pub const fn cached_bytes(self) -> u64 {
        self.cached_bytes
    }

    /// Returns source and Range-registration owned bytes.
    pub const fn range_resident_bytes(self) -> u64 {
        self.range_resident_bytes
    }

    /// Returns successful values retained by the Ready cache.
    pub const fn cache_entries(self) -> u64 {
        self.cache_entries
    }

    /// Returns bytes owned by the Ready cache.
    pub const fn cache_resident_bytes(self) -> u64 {
        self.cache_resident_bytes
    }

    /// Returns the private shared-index root plus active service proof handles.
    pub const fn index_handles(self) -> usize {
        self.index_handles
    }

    /// Returns checked Range plus Ready-cache owned bytes.
    pub const fn resident_bytes(self) -> u64 {
        self.resident_bytes
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ReleaseSummary {
    released_service_jobs: usize,
    released_waiting_targets: usize,
    released_held_completions: usize,
    cache: Option<ReadySessionCloseReport>,
    released_index_handles: usize,
    source: Option<RangeResumeReleaseReport>,
    opening: Option<StrictBaseOpenCoordinatorCloseReport>,
}

/// Stable evidence returned after ordered M1 actor close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct M1SessionCloseReport {
    previous_phase: M1SessionPhase,
    failure: Option<M1SessionFailure>,
    released: ReleaseSummary,
}

impl M1SessionCloseReport {
    /// Returns the lifecycle observed when close was first queued.
    pub const fn previous_phase(self) -> M1SessionPhase {
        self.previous_phase
    }

    /// Returns a session-wide failure that released resources before close.
    pub const fn failure(self) -> Option<M1SessionFailure> {
        self.failure
    }

    /// Returns page-count plus outline jobs dropped by close or prior failure.
    pub const fn released_service_jobs(self) -> usize {
        self.released.released_service_jobs
    }

    /// Returns waiting targets removed before cache release.
    pub const fn released_waiting_targets(self) -> usize {
        self.released.released_waiting_targets
    }

    /// Returns privately collected completions dropped before cache release.
    pub const fn released_held_completions(self) -> usize {
        self.released.released_held_completions
    }

    /// Returns Ready-cache release evidence when Ready was reached.
    pub const fn cache(self) -> Option<ReadySessionCloseReport> {
        self.released.cache
    }

    /// Returns the private shared-index root handles dropped after the cache.
    pub const fn released_index_handles(self) -> usize {
        self.released.released_index_handles
    }

    /// Returns source-owner release evidence.
    pub const fn source(self) -> Option<RangeResumeReleaseReport> {
        self.released.source
    }

    /// Returns strict-open close evidence when Ready was not reached.
    pub const fn opening(self) -> Option<StrictBaseOpenCoordinatorCloseReport> {
        self.released.opening
    }
}

enum ServiceCompletion {
    Resume(RangeResumePermit),
    Failed(RangeResumeFailurePermit),
}

struct WaitingService {
    ticket: DataTicket,
    target: RangeResumeTarget,
    completion: Option<ServiceCompletion>,
}

struct ServiceSlot<J> {
    identity: M1RequestIdentity,
    job: J,
    waiting: Option<WaitingService>,
}

impl<J> ServiceSlot<J> {
    fn new(identity: M1RequestIdentity, job: J) -> Self {
        Self {
            identity,
            job,
            waiting: None,
        }
    }

    fn is_runnable(&self) -> bool {
        self.waiting
            .as_ref()
            .is_none_or(|waiting| waiting.completion.is_some())
    }

    fn has_waiting_target(&self) -> bool {
        self.waiting.is_some()
    }

    fn has_held_completion(&self) -> bool {
        self.waiting
            .as_ref()
            .is_some_and(|waiting| waiting.completion.is_some())
    }

    fn matches_completion(&self, ticket: DataTicket, target: RangeResumeTarget) -> bool {
        self.identity.job == target.job()
            && self.identity.generation == target.generation()
            && self.waiting.as_ref().is_some_and(|waiting| {
                waiting.ticket == ticket && waiting.target == target && waiting.completion.is_none()
            })
    }
}

struct ReadyState {
    index: Option<SharedAttestedRevisionIndex>,
    source: Option<RangeResumeArbiter>,
    cache: Option<ReadySessionOwner>,
    page_count: Option<ServiceSlot<CountPagesJob<'static>>>,
    outline: Option<ServiceSlot<ReadOutlineJob<'static>>>,
    next_service: M1Service,
}

impl ReadyState {
    fn service_jobs(&self) -> usize {
        usize::from(self.page_count.is_some()) + usize::from(self.outline.is_some())
    }

    fn waiting_targets(&self) -> usize {
        usize::from(
            self.page_count
                .as_ref()
                .is_some_and(ServiceSlot::has_waiting_target),
        ) + usize::from(
            self.outline
                .as_ref()
                .is_some_and(ServiceSlot::has_waiting_target),
        )
    }

    fn held_completions(&self) -> usize {
        usize::from(
            self.page_count
                .as_ref()
                .is_some_and(ServiceSlot::has_held_completion),
        ) + usize::from(
            self.outline
                .as_ref()
                .is_some_and(ServiceSlot::has_held_completion),
        )
    }

    fn is_runnable(&self, service: M1Service) -> bool {
        match service {
            M1Service::PageCount => self
                .page_count
                .as_ref()
                .is_some_and(ServiceSlot::is_runnable),
            M1Service::Outline => self.outline.as_ref().is_some_and(ServiceSlot::is_runnable),
        }
    }

    fn choose_runnable(&mut self) -> Option<M1Service> {
        let first = self.next_service;
        let selected = if self.is_runnable(first) {
            Some(first)
        } else if self.is_runnable(first.other()) {
            Some(first.other())
        } else {
            None
        };
        if let Some(service) = selected {
            self.next_service = service.other();
        }
        selected
    }

    fn resources(&self) -> M1SessionResources {
        let range = self
            .source
            .as_ref()
            .expect("Ready retains its source owner")
            .resources();
        let cache = self.cache.as_ref().map(ReadySessionOwner::resources);
        let range_resident_bytes = range.resident_bytes();
        let cache_resident_bytes = cache.map_or(0, crate::ReadySessionResources::resident_bytes);
        M1SessionResources {
            opening_jobs: 0,
            service_jobs: self.service_jobs(),
            waiting_targets: self.waiting_targets(),
            held_completions: self.held_completions(),
            range_registrations: range.registrations(),
            range_pending_tickets: range.pending_tickets(),
            cached_bytes: range.cached_bytes(),
            range_resident_bytes,
            cache_entries: cache.map_or(0, crate::ReadySessionResources::entries),
            cache_resident_bytes,
            index_handles: usize::from(self.index.is_some()) + self.service_jobs(),
            resident_bytes: range_resident_bytes
                .checked_add(cache_resident_bytes)
                .expect("validated Range and cache hard ceilings fit u64"),
        }
    }
}

enum SessionState {
    Opening(StrictBaseOpenCoordinator),
    Ready(ReadyState),
    Failed {
        failure: M1SessionFailure,
        released: ReleaseSummary,
    },
    Closed(M1SessionCloseReport),
    Transition,
}

/// Bounded M1 actor for strict opening, page count, and outline services.
///
/// This actor deliberately has no general queue, priority scheduler, worker,
/// transport, surface, password, rendering, or arbitrary service registry. It
/// owns exactly one strict-open coordinator and, after Ready, at most one
/// page-count job and one outline job. All parser execution is confined to
/// [`Self::run_one`].
pub struct M1StrictDocumentSession {
    session_id: ReadyStoreSessionId,
    generation: RangeResumeGeneration,
    open_request: M1RequestIdentity,
    cache_epoch: ReadyStoreEpoch,
    cache_limits: ReadyStoreLimits,
    phase: M1SessionPhase,
    close_previous_phase: Option<M1SessionPhase>,
    state: SessionState,
}

impl M1StrictDocumentSession {
    /// Constructs all opening owners without executing parser code.
    pub fn new(
        session_id: ReadyStoreSessionId,
        open_request: M1RequestIdentity,
        open_job: OpenStrictBaseRevisionJob,
        range_limits: RangeStoreLimits,
        cache_epoch: ReadyStoreEpoch,
        cache_limits: ReadyStoreLimits,
    ) -> Result<Self, M1SessionRequestError> {
        if open_job.context().job() != open_request.job() {
            return Err(M1SessionRequestError::JobContextMismatch);
        }
        let generation = open_request.generation();
        let coordinator = StrictBaseOpenCoordinator::new(open_job, generation, range_limits)
            .map_err(M1SessionRequestError::Runtime)?;
        Ok(Self {
            session_id,
            generation,
            open_request,
            cache_epoch,
            cache_limits,
            phase: M1SessionPhase::Created,
            close_previous_phase: None,
            state: SessionState::Opening(coordinator),
        })
    }

    /// Returns the caller-issued opaque session identity.
    pub const fn session_id(&self) -> ReadyStoreSessionId {
        self.session_id
    }

    /// Returns the fixed generation accepted by this bounded actor.
    pub const fn generation(&self) -> RangeResumeGeneration {
        self.generation
    }

    /// Returns the exact opening request identity.
    pub const fn open_request(&self) -> M1RequestIdentity {
        self.open_request
    }

    /// Returns the current public lifecycle phase.
    pub const fn phase(&self) -> M1SessionPhase {
        self.phase
    }

    /// Returns current resources; Closed and Failed are provably all zero.
    pub fn resources(&self) -> M1SessionResources {
        match &self.state {
            SessionState::Opening(coordinator) => {
                let resources = coordinator.resources();
                M1SessionResources {
                    opening_jobs: resources.jobs(),
                    service_jobs: 0,
                    waiting_targets: resources.waiting_targets(),
                    held_completions: 0,
                    range_registrations: resources.registrations(),
                    range_pending_tickets: resources.pending_tickets(),
                    cached_bytes: resources.cached_bytes(),
                    range_resident_bytes: resources.resident_bytes(),
                    cache_entries: 0,
                    cache_resident_bytes: 0,
                    index_handles: 0,
                    resident_bytes: resources.resident_bytes(),
                }
            }
            SessionState::Ready(ready) => ready.resources(),
            SessionState::Failed { .. } | SessionState::Closed(_) | SessionState::Transition => {
                M1SessionResources::ZERO
            }
        }
    }

    /// Returns opening parser progress without exposing either execution owner.
    ///
    /// The snapshot is absent after the opening coordinator has moved into the
    /// Ready handoff or a session terminal. It is intended for runtime auditing
    /// and cannot poll, resume, or otherwise mutate parser state.
    pub fn opening_parser_audit(&self) -> Option<M1OpeningParserAudit> {
        let SessionState::Opening(coordinator) = &self.state else {
            return None;
        };
        Some(M1OpeningParserAudit {
            job_phase: coordinator.job_phase(),
            stats: coordinator.stats(),
            waiting_checkpoint: coordinator.waiting_checkpoint(),
        })
    }

    /// Admits the only page-count slot after validating caller identities.
    pub fn request_page_count(
        &mut self,
        request: M1RequestIdentity,
        context: PageTreeJobContext,
        limits: PageTreeLimits,
    ) -> Result<(), M1SessionRequestError> {
        self.validate_service_request(M1Service::PageCount, request, context.job())?;
        let ready = self.ready_mut()?;
        let index = ready
            .index
            .as_ref()
            .expect("Ready retains its private shared index");
        let job = index
            .count_pages_owned(context, limits)
            .map_err(M1SessionRequestError::Document)?;
        ready.page_count = Some(ServiceSlot::new(request, job));
        self.phase = M1SessionPhase::Ready;
        Ok(())
    }

    /// Admits the only outline slot after validating caller identities.
    pub fn request_outline(
        &mut self,
        request: M1RequestIdentity,
        context: OutlineJobContext,
        limits: OutlineLimits,
    ) -> Result<(), M1SessionRequestError> {
        self.validate_service_request(M1Service::Outline, request, context.job())?;
        let ready = self.ready_mut()?;
        let index = ready
            .index
            .as_ref()
            .expect("Ready retains its private shared index");
        let job = index
            .read_outline_owned(context, limits)
            .map_err(M1SessionRequestError::Document)?;
        ready.outline = Some(ServiceSlot::new(request, job));
        self.phase = M1SessionPhase::Ready;
        Ok(())
    }

    /// Cancels one exact opening or Ready-service request without parser work.
    pub fn cancel_request(&mut self, request: M1RequestIdentity) -> M1SessionCancel {
        match self.phase {
            M1SessionPhase::Created | M1SessionPhase::Opening | M1SessionPhase::WaitingForData => {
                if matches!(self.state, SessionState::Opening(_)) {
                    return self.cancel_open(request);
                }
            }
            M1SessionPhase::Ready => {}
            M1SessionPhase::Closing | M1SessionPhase::Closed | M1SessionPhase::Failed => {
                return M1SessionCancel::Rejected {
                    phase: self.phase,
                    reason: M1SessionCancelRejectReason::TerminalPhase,
                };
            }
        }
        self.cancel_ready_request(request)
    }

    /// Supplies immutable source bytes without executing parser code.
    pub fn supply(&mut self, response: RangeResponse) -> M1SessionIngress {
        if matches!(
            self.phase,
            M1SessionPhase::Closing | M1SessionPhase::Closed | M1SessionPhase::Failed
        ) {
            return self.terminal_ingress();
        }
        if matches!(self.state, SessionState::Opening(_)) {
            let ingress = match &mut self.state {
                SessionState::Opening(coordinator) => coordinator.supply(response),
                _ => unreachable!(),
            };
            return self.finish_open_ingress(ingress);
        }
        self.ready_supply(response)
    }

    /// Observes source metadata without executing parser code.
    pub fn observe_snapshot(&mut self, observed: SourceSnapshot) -> M1SessionIngress {
        if matches!(
            self.phase,
            M1SessionPhase::Closing | M1SessionPhase::Closed | M1SessionPhase::Failed
        ) {
            return self.terminal_ingress();
        }
        if matches!(self.state, SessionState::Opening(_)) {
            let ingress = match &mut self.state {
                SessionState::Opening(coordinator) => coordinator.observe_snapshot(observed),
                _ => unreachable!(),
            };
            return self.finish_open_ingress(ingress);
        }
        self.ready_observe_snapshot(observed)
    }

    /// Queues one exact host ticket failure without executing parser code.
    pub fn fail_data(&mut self, ticket: DataTicket) -> M1SessionIngress {
        if matches!(
            self.phase,
            M1SessionPhase::Closing | M1SessionPhase::Closed | M1SessionPhase::Failed
        ) {
            return self.terminal_ingress();
        }
        if matches!(self.state, SessionState::Opening(_)) {
            let ingress = match &mut self.state {
                SessionState::Opening(coordinator) => coordinator.fail_data(ticket),
                _ => unreachable!(),
            };
            return self.finish_open_ingress(ingress);
        }
        self.ready_fail_data(ticket)
    }

    /// Terminates the immutable source snapshot without executing parser code.
    pub fn signal_source_changed(&mut self) -> M1SessionIngress {
        if matches!(
            self.phase,
            M1SessionPhase::Closing | M1SessionPhase::Closed | M1SessionPhase::Failed
        ) {
            return self.terminal_ingress();
        }
        match &mut self.state {
            SessionState::Opening(coordinator) => {
                let _ = coordinator.signal_source_changed();
                let error = coordinator.source_change_error();
                self.fail_opening(M1SessionFailure::SourceChanged(error));
                M1SessionIngress::SourceChanged
            }
            SessionState::Ready(_) => {
                self.fail_ready(M1SessionFailure::SourceChanged(None), true);
                M1SessionIngress::SourceChanged
            }
            SessionState::Failed { .. } | SessionState::Closed(_) | SessionState::Transition => {
                M1SessionIngress::Rejected {
                    phase: self.phase,
                    reason: M1SessionIngressRejectReason::TerminalPhase,
                }
            }
        }
    }

    /// Queues idempotent close; release occurs only in a later [`Self::run_one`].
    pub fn close(&mut self) -> M1SessionClose {
        match &self.state {
            SessionState::Closed(report) => M1SessionClose::AlreadyClosed(*report),
            _ if self.phase == M1SessionPhase::Closing => M1SessionClose::AlreadyClosing,
            _ => {
                self.close_previous_phase = Some(self.phase);
                self.phase = M1SessionPhase::Closing;
                M1SessionClose::Queued
            }
        }
    }

    /// Executes at most one parser poll on one logical actor turn.
    pub fn run_one(&mut self, cancellation: &(dyn DocumentCancellation + '_)) -> M1SessionRun {
        if self.phase == M1SessionPhase::Closing {
            return self.finish_close();
        }
        match &self.state {
            SessionState::Failed { failure, .. } => {
                return M1SessionRun::AlreadyTerminal {
                    phase: if *failure == M1SessionFailure::OpeningCancelled {
                        M1SessionPhase::Failed
                    } else {
                        self.phase
                    },
                };
            }
            SessionState::Closed(_) => {
                return M1SessionRun::AlreadyTerminal {
                    phase: M1SessionPhase::Closed,
                };
            }
            _ => {}
        }
        if matches!(self.state, SessionState::Opening(_)) {
            self.run_opening(cancellation)
        } else {
            self.run_ready(cancellation)
        }
    }

    fn validate_service_request(
        &self,
        service: M1Service,
        request: M1RequestIdentity,
        context_job: JobId,
    ) -> Result<(), M1SessionRequestError> {
        let SessionState::Ready(ready) = &self.state else {
            return Err(M1SessionRequestError::NotReady(self.phase));
        };
        if !matches!(
            self.phase,
            M1SessionPhase::Ready | M1SessionPhase::WaitingForData
        ) {
            return Err(M1SessionRequestError::NotReady(self.phase));
        }
        if request.generation != self.generation {
            return Err(M1SessionRequestError::StaleGeneration {
                expected: self.generation,
                actual: request.generation,
            });
        }
        if request.job != context_job {
            return Err(M1SessionRequestError::JobContextMismatch);
        }
        let requested_slot_busy = match service {
            M1Service::PageCount => ready.page_count.is_some(),
            M1Service::Outline => ready.outline.is_some(),
        };
        if requested_slot_busy {
            return Err(M1SessionRequestError::SlotBusy(service));
        }
        for identity in [
            ready.page_count.as_ref().map(|slot| slot.identity),
            ready.outline.as_ref().map(|slot| slot.identity),
        ]
        .into_iter()
        .flatten()
        {
            if identity.request_id == request.request_id {
                return Err(M1SessionRequestError::DuplicateRequestId);
            }
            if identity.job == request.job {
                return Err(M1SessionRequestError::DuplicateJobId);
            }
        }
        Ok(())
    }

    fn ready_mut(&mut self) -> Result<&mut ReadyState, M1SessionRequestError> {
        match &mut self.state {
            SessionState::Ready(ready) => Ok(ready),
            _ => Err(M1SessionRequestError::NotReady(self.phase)),
        }
    }

    fn run_opening(&mut self, cancellation: &(dyn DocumentCancellation + '_)) -> M1SessionRun {
        self.phase = M1SessionPhase::Opening;
        let outcome = match &mut self.state {
            SessionState::Opening(coordinator) => coordinator.run_one(cancellation),
            _ => unreachable!(),
        };
        match outcome {
            StrictBaseOpenCoordinatorRun::WaitingForData { ticket, missing } => {
                self.phase = M1SessionPhase::WaitingForData;
                M1SessionRun::WaitingForData {
                    owner: M1SessionWait::Opening(self.open_request),
                    ticket,
                    missing,
                }
            }
            StrictBaseOpenCoordinatorRun::NoWork => {
                self.phase = M1SessionPhase::WaitingForData;
                M1SessionRun::NoWork
            }
            StrictBaseOpenCoordinatorRun::Ready(ready) => self.install_ready(ready),
            StrictBaseOpenCoordinatorRun::Failed(failure) => {
                let session_failure = M1SessionFailure::Opening(failure);
                self.fail_opening(session_failure);
                M1SessionRun::Failed(session_failure)
            }
            StrictBaseOpenCoordinatorRun::Cancelled { .. } => {
                self.fail_opening(M1SessionFailure::OpeningCancelled);
                M1SessionRun::Failed(M1SessionFailure::OpeningCancelled)
            }
            StrictBaseOpenCoordinatorRun::SourceChanged { error } => {
                let failure = M1SessionFailure::SourceChanged(error);
                self.fail_opening(failure);
                M1SessionRun::Failed(failure)
            }
            StrictBaseOpenCoordinatorRun::AlreadyTerminal { phase } => {
                let failure = match phase {
                    StrictBaseOpenCoordinatorPhase::SourceChanged => {
                        M1SessionFailure::SourceChanged(None)
                    }
                    _ => M1SessionFailure::Internal,
                };
                self.fail_opening(failure);
                M1SessionRun::Failed(failure)
            }
        }
    }

    fn install_ready(&mut self, ready: crate::StrictBaseOpenReady) -> M1SessionRun {
        let (index, source) = ready.into_parts();
        let binding = ReadyStoreBinding::for_index(&index, self.session_id, self.cache_epoch);
        let cache = match ReadySessionOwner::new(binding, self.cache_limits) {
            Ok(cache) => cache,
            Err(error) => {
                drop(index);
                let mut source = source;
                let released = ReleaseSummary {
                    source: Some(source.close()),
                    ..ReleaseSummary::default()
                };
                let _ = mem::replace(
                    &mut self.state,
                    SessionState::Failed {
                        failure: M1SessionFailure::Cache(error),
                        released,
                    },
                );
                self.phase = M1SessionPhase::Failed;
                return M1SessionRun::Failed(M1SessionFailure::Cache(error));
            }
        };
        let shared = index.into_shared();
        let previous = mem::replace(&mut self.state, SessionState::Transition);
        let SessionState::Opening(mut coordinator) = previous else {
            unreachable!("Ready handoff can only replace opening")
        };
        let _ = coordinator.close();
        self.state = SessionState::Ready(ReadyState {
            index: Some(shared),
            source: Some(source),
            cache: Some(cache),
            page_count: None,
            outline: None,
            next_service: M1Service::PageCount,
        });
        self.phase = M1SessionPhase::Ready;
        M1SessionRun::Ready
    }

    fn run_ready(&mut self, cancellation: &(dyn DocumentCancellation + '_)) -> M1SessionRun {
        if let Err(failure) = self.collect_ready_completions() {
            self.fail_ready(failure, false);
            return M1SessionRun::Failed(failure);
        }
        let service = match &mut self.state {
            SessionState::Ready(ready) => ready.choose_runnable(),
            _ => unreachable!(),
        };
        let Some(service) = service else {
            self.refresh_ready_phase();
            return M1SessionRun::NoWork;
        };
        let outcome = match service {
            M1Service::PageCount => self.poll_page_count(cancellation),
            M1Service::Outline => self.poll_outline(cancellation),
        };
        match outcome {
            Ok(outcome) => {
                self.refresh_ready_phase();
                outcome
            }
            Err(failure) => {
                self.fail_ready(failure, false);
                M1SessionRun::Failed(failure)
            }
        }
    }

    fn collect_ready_completions(&mut self) -> Result<(), M1SessionFailure> {
        for _ in 0..2 {
            let completion = match &mut self.state {
                SessionState::Ready(ready) => ready
                    .source
                    .as_mut()
                    .expect("Ready retains its source owner")
                    .take_completion(),
                _ => unreachable!(),
            }
            .map_err(range_failure)?;
            match completion {
                RangeResumeCompletion::Empty => break,
                RangeResumeCompletion::Resume(permit) => {
                    self.attach_completion(ServiceCompletion::Resume(permit))?
                }
                RangeResumeCompletion::Failed(permit) => {
                    self.attach_completion(ServiceCompletion::Failed(permit))?
                }
            }
        }
        Ok(())
    }

    fn attach_completion(&mut self, completion: ServiceCompletion) -> Result<(), M1SessionFailure> {
        let (arbiter_id, ticket, target) = match &completion {
            ServiceCompletion::Resume(permit) => {
                (permit.arbiter_id(), permit.ticket(), permit.target())
            }
            ServiceCompletion::Failed(permit) => {
                (permit.arbiter_id(), permit.ticket(), permit.target())
            }
        };
        let SessionState::Ready(ready) = &mut self.state else {
            return Err(M1SessionFailure::Internal);
        };
        if ready
            .source
            .as_ref()
            .is_none_or(|source| source.arbiter_id() != arbiter_id)
        {
            return Err(M1SessionFailure::Internal);
        }
        if let Some(slot) = &mut ready.page_count
            && slot.matches_completion(ticket, target)
        {
            slot.waiting
                .as_mut()
                .expect("matching completion has waiting state")
                .completion = Some(completion);
            return Ok(());
        }
        if let Some(slot) = &mut ready.outline
            && slot.matches_completion(ticket, target)
        {
            slot.waiting
                .as_mut()
                .expect("matching completion has waiting state")
                .completion = Some(completion);
            return Ok(());
        }
        Err(M1SessionFailure::Internal)
    }

    fn poll_page_count(
        &mut self,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<M1SessionRun, M1SessionFailure> {
        let ready = match &mut self.state {
            SessionState::Ready(ready) => ready,
            _ => return Err(M1SessionFailure::Internal),
        };
        let mut slot = ready.page_count.take().ok_or(M1SessionFailure::Internal)?;
        if let Some(mut waiting) = slot.waiting.take() {
            let Some(completion) = waiting.completion.take() else {
                slot.waiting = Some(waiting);
                ready.page_count = Some(slot);
                return Err(M1SessionFailure::Internal);
            };
            match completion {
                ServiceCompletion::Resume(_) => {}
                ServiceCompletion::Failed(permit) => {
                    return Ok(M1SessionRun::RequestFailed {
                        service: M1Service::PageCount,
                        request: slot.identity,
                        failure: M1ServiceFailure::Source(permit.error()),
                    });
                }
            }
        }
        let poll = {
            let source = match ready
                .source
                .as_ref()
                .expect("Ready retains its source owner")
                .byte_source()
            {
                Ok(source) => source,
                Err(error) => {
                    ready.page_count = Some(slot);
                    return Err(range_failure(error));
                }
            };
            slot.job.poll(source, cancellation)
        };
        match poll {
            PageCountPoll::Ready(result) => Ok(M1SessionRun::PageCountReady {
                request: slot.identity,
                result,
            }),
            PageCountPoll::Failed(error) => Ok(M1SessionRun::RequestFailed {
                service: M1Service::PageCount,
                request: slot.identity,
                failure: M1ServiceFailure::Document(error),
            }),
            PageCountPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                let target =
                    RangeResumeTarget::new(slot.identity.job, checkpoint, slot.identity.generation);
                slot.waiting = Some(WaitingService {
                    ticket,
                    target,
                    completion: None,
                });
                if let Err(failure) = register_service_pending(ready, ticket, target) {
                    ready.page_count = Some(slot);
                    return Err(failure);
                }
                let request = slot.identity;
                ready.page_count = Some(slot);
                Ok(M1SessionRun::WaitingForData {
                    owner: M1SessionWait::Service {
                        service: M1Service::PageCount,
                        request,
                    },
                    ticket,
                    missing,
                })
            }
        }
    }

    fn poll_outline(
        &mut self,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<M1SessionRun, M1SessionFailure> {
        let ready = match &mut self.state {
            SessionState::Ready(ready) => ready,
            _ => return Err(M1SessionFailure::Internal),
        };
        let mut slot = ready.outline.take().ok_or(M1SessionFailure::Internal)?;
        if let Some(mut waiting) = slot.waiting.take() {
            let Some(completion) = waiting.completion.take() else {
                slot.waiting = Some(waiting);
                ready.outline = Some(slot);
                return Err(M1SessionFailure::Internal);
            };
            match completion {
                ServiceCompletion::Resume(_) => {}
                ServiceCompletion::Failed(permit) => {
                    return Ok(M1SessionRun::RequestFailed {
                        service: M1Service::Outline,
                        request: slot.identity,
                        failure: M1ServiceFailure::Source(permit.error()),
                    });
                }
            }
        }
        let poll = {
            let source = match ready
                .source
                .as_ref()
                .expect("Ready retains its source owner")
                .byte_source()
            {
                Ok(source) => source,
                Err(error) => {
                    ready.outline = Some(slot);
                    return Err(range_failure(error));
                }
            };
            slot.job.poll(source, cancellation)
        };
        match poll {
            OutlinePoll::Ready(result) => Ok(M1SessionRun::OutlineReady {
                request: slot.identity,
                result,
            }),
            OutlinePoll::Failed(error) => Ok(M1SessionRun::RequestFailed {
                service: M1Service::Outline,
                request: slot.identity,
                failure: M1ServiceFailure::Document(error),
            }),
            OutlinePoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                let target =
                    RangeResumeTarget::new(slot.identity.job, checkpoint, slot.identity.generation);
                slot.waiting = Some(WaitingService {
                    ticket,
                    target,
                    completion: None,
                });
                if let Err(failure) = register_service_pending(ready, ticket, target) {
                    ready.outline = Some(slot);
                    return Err(failure);
                }
                let request = slot.identity;
                ready.outline = Some(slot);
                Ok(M1SessionRun::WaitingForData {
                    owner: M1SessionWait::Service {
                        service: M1Service::Outline,
                        request,
                    },
                    ticket,
                    missing,
                })
            }
        }
    }

    fn cancel_open(&mut self, request: M1RequestIdentity) -> M1SessionCancel {
        if request.request_id != self.open_request.request_id {
            return M1SessionCancel::Rejected {
                phase: self.phase,
                reason: M1SessionCancelRejectReason::NotActive,
            };
        }
        if request != self.open_request {
            return M1SessionCancel::Rejected {
                phase: self.phase,
                reason: M1SessionCancelRejectReason::IdentityMismatch,
            };
        }
        let cancelled = match &mut self.state {
            SessionState::Opening(coordinator) => coordinator.cancel(),
            _ => unreachable!(),
        };
        if !matches!(cancelled, crate::StrictBaseOpenCoordinatorCancel::Cancelled) {
            let failure = M1SessionFailure::Internal;
            self.fail_opening(failure);
            return M1SessionCancel::Rejected {
                phase: M1SessionPhase::Failed,
                reason: M1SessionCancelRejectReason::TerminalPhase,
            };
        }
        self.fail_opening(M1SessionFailure::OpeningCancelled);
        M1SessionCancel::Cancelled {
            request,
            service: None,
        }
    }

    fn cancel_ready_request(&mut self, request: M1RequestIdentity) -> M1SessionCancel {
        let SessionState::Ready(ready) = &mut self.state else {
            return M1SessionCancel::Rejected {
                phase: self.phase,
                reason: M1SessionCancelRejectReason::NotActive,
            };
        };
        let candidate = if ready
            .page_count
            .as_ref()
            .is_some_and(|slot| slot.identity.request_id == request.request_id)
        {
            M1Service::PageCount
        } else if ready
            .outline
            .as_ref()
            .is_some_and(|slot| slot.identity.request_id == request.request_id)
        {
            M1Service::Outline
        } else {
            return M1SessionCancel::Rejected {
                phase: self.phase,
                reason: M1SessionCancelRejectReason::NotActive,
            };
        };
        let identity = match candidate {
            M1Service::PageCount => ready.page_count.as_ref().unwrap().identity,
            M1Service::Outline => ready.outline.as_ref().unwrap().identity,
        };
        if identity != request {
            return M1SessionCancel::Rejected {
                phase: self.phase,
                reason: M1SessionCancelRejectReason::IdentityMismatch,
            };
        }
        let waiting = match candidate {
            M1Service::PageCount => ready
                .page_count
                .as_ref()
                .and_then(|slot| slot.waiting.as_ref()),
            M1Service::Outline => ready
                .outline
                .as_ref()
                .and_then(|slot| slot.waiting.as_ref()),
        };
        if let Some(waiting) = waiting
            && waiting.completion.is_none()
        {
            let outcome = ready
                .source
                .as_mut()
                .expect("Ready retains its source owner")
                .cancel(identity.job, identity.generation);
            match outcome {
                Ok(RangeResumeCancelOutcome::Cancelled { target }) if target == waiting.target => {}
                Ok(_) => {
                    self.fail_ready(M1SessionFailure::Internal, false);
                    return M1SessionCancel::Rejected {
                        phase: M1SessionPhase::Failed,
                        reason: M1SessionCancelRejectReason::TerminalPhase,
                    };
                }
                Err(error) => {
                    let failure = range_failure(error);
                    self.fail_ready(failure, false);
                    return M1SessionCancel::Rejected {
                        phase: M1SessionPhase::Failed,
                        reason: M1SessionCancelRejectReason::Runtime(error),
                    };
                }
            }
        }
        match candidate {
            M1Service::PageCount => drop(ready.page_count.take()),
            M1Service::Outline => drop(ready.outline.take()),
        }
        self.refresh_ready_phase();
        M1SessionCancel::Cancelled {
            request,
            service: Some(candidate),
        }
    }

    fn ready_supply(&mut self, response: RangeResponse) -> M1SessionIngress {
        let SessionState::Ready(ready) = &mut self.state else {
            return self.terminal_ingress();
        };
        if ready
            .source
            .as_ref()
            .is_none_or(|source| source.resources().registrations() == 0)
        {
            return M1SessionIngress::Rejected {
                phase: self.phase,
                reason: M1SessionIngressRejectReason::NotWaiting,
            };
        }
        let outcome = ready
            .source
            .as_mut()
            .expect("Ready retains its source owner")
            .supply(response);
        self.finish_ready_supply(outcome)
    }

    fn ready_observe_snapshot(&mut self, observed: SourceSnapshot) -> M1SessionIngress {
        let SessionState::Ready(ready) = &mut self.state else {
            return self.terminal_ingress();
        };
        let outcome = ready
            .source
            .as_mut()
            .expect("Ready retains its source owner")
            .observe_snapshot(observed);
        self.finish_ready_supply(outcome)
    }

    fn ready_fail_data(&mut self, ticket: DataTicket) -> M1SessionIngress {
        let SessionState::Ready(ready) = &mut self.state else {
            return self.terminal_ingress();
        };
        if ready
            .source
            .as_ref()
            .is_none_or(|source| source.resources().registrations() == 0)
        {
            return M1SessionIngress::Rejected {
                phase: self.phase,
                reason: M1SessionIngressRejectReason::NotWaiting,
            };
        }
        let outcome = ready
            .source
            .as_mut()
            .expect("Ready retains its source owner")
            .fail_ticket(ticket);
        match outcome {
            Ok(outcome) => {
                let wake_scheduler = outcome.queued_failures() != 0;
                let cached_bytes = ready
                    .source
                    .as_ref()
                    .expect("Ready retains its source owner")
                    .resources()
                    .cached_bytes();
                if wake_scheduler {
                    self.phase = M1SessionPhase::Ready;
                }
                M1SessionIngress::Accepted {
                    wake_scheduler,
                    cached_bytes,
                }
            }
            Err(error) => self.finish_ready_range_error(error),
        }
    }

    fn finish_ready_supply(
        &mut self,
        outcome: Result<crate::RangeResumeSupplyOutcome, RangeResumeError>,
    ) -> M1SessionIngress {
        match outcome {
            Ok(outcome) => {
                let wake_scheduler = outcome.queued_requeues() != 0;
                if wake_scheduler {
                    self.phase = M1SessionPhase::Ready;
                }
                M1SessionIngress::Accepted {
                    wake_scheduler,
                    cached_bytes: outcome.cached_bytes(),
                }
            }
            Err(error) => self.finish_ready_range_error(error),
        }
    }

    fn finish_ready_range_error(&mut self, error: RangeResumeError) -> M1SessionIngress {
        if error.category() == RangeResumeErrorCategory::Integrity {
            self.fail_ready(M1SessionFailure::SourceChanged(Some(error)), false);
            M1SessionIngress::SourceChanged
        } else {
            let source_failed = matches!(
                &self.state,
                SessionState::Ready(ready)
                    if ready.source.as_ref().is_some_and(|source| source.phase() == RangeResumePhase::Failed)
            );
            if source_failed {
                let failure = M1SessionFailure::Runtime(error);
                self.fail_ready(failure, false);
                M1SessionIngress::Failed(failure)
            } else {
                M1SessionIngress::Rejected {
                    phase: self.phase,
                    reason: M1SessionIngressRejectReason::Range(error),
                }
            }
        }
    }

    fn finish_open_ingress(&mut self, ingress: StrictBaseOpenIngress) -> M1SessionIngress {
        match ingress {
            StrictBaseOpenIngress::Accepted {
                wake_scheduler,
                cached_bytes,
            } => {
                if wake_scheduler {
                    self.phase = M1SessionPhase::Opening;
                }
                M1SessionIngress::Accepted {
                    wake_scheduler,
                    cached_bytes,
                }
            }
            StrictBaseOpenIngress::SourceChanged { error } => {
                self.fail_opening(M1SessionFailure::SourceChanged(error));
                M1SessionIngress::SourceChanged
            }
            StrictBaseOpenIngress::Failed(failure) => {
                let failure = M1SessionFailure::Opening(failure);
                self.fail_opening(failure);
                M1SessionIngress::Failed(failure)
            }
            StrictBaseOpenIngress::Rejected { phase: _, reason } => M1SessionIngress::Rejected {
                phase: self.phase,
                reason: match reason {
                    StrictBaseOpenIngressRejectReason::NotWaiting => {
                        M1SessionIngressRejectReason::NotWaiting
                    }
                    StrictBaseOpenIngressRejectReason::TerminalPhase => {
                        M1SessionIngressRejectReason::TerminalPhase
                    }
                    StrictBaseOpenIngressRejectReason::Range(error) => {
                        M1SessionIngressRejectReason::Range(error)
                    }
                },
            },
        }
    }

    fn refresh_ready_phase(&mut self) {
        let SessionState::Ready(ready) = &self.state else {
            return;
        };
        let source_ready = ready.source.as_ref().is_some_and(|source| {
            let resources = source.resources();
            resources.ready_resumes() != 0 || resources.queued_failures() != 0
        });
        self.phase = if ready.is_runnable(M1Service::PageCount)
            || ready.is_runnable(M1Service::Outline)
            || source_ready
        {
            M1SessionPhase::Ready
        } else if ready.waiting_targets() != 0 {
            M1SessionPhase::WaitingForData
        } else {
            M1SessionPhase::Ready
        };
    }

    fn fail_opening(&mut self, failure: M1SessionFailure) {
        let previous = mem::replace(&mut self.state, SessionState::Transition);
        let released = match previous {
            SessionState::Opening(mut coordinator) => ReleaseSummary {
                opening: Some(coordinator.close()),
                ..ReleaseSummary::default()
            },
            SessionState::Failed { released, .. } => released,
            _ => ReleaseSummary::default(),
        };
        self.state = SessionState::Failed { failure, released };
        self.phase = M1SessionPhase::Failed;
    }

    fn fail_ready(&mut self, failure: M1SessionFailure, signal_source_changed: bool) {
        let previous = mem::replace(&mut self.state, SessionState::Transition);
        let released = match previous {
            SessionState::Ready(ready) => release_ready(ready, signal_source_changed),
            SessionState::Failed { released, .. } => released,
            _ => ReleaseSummary::default(),
        };
        self.state = SessionState::Failed { failure, released };
        self.phase = M1SessionPhase::Failed;
    }

    fn finish_close(&mut self) -> M1SessionRun {
        let previous_phase = self
            .close_previous_phase
            .take()
            .unwrap_or(M1SessionPhase::Closing);
        let previous = mem::replace(&mut self.state, SessionState::Transition);
        let (failure, released) = match previous {
            SessionState::Opening(mut coordinator) => (
                None,
                ReleaseSummary {
                    opening: Some(coordinator.close()),
                    ..ReleaseSummary::default()
                },
            ),
            SessionState::Ready(ready) => (None, release_ready(ready, false)),
            SessionState::Failed { failure, released } => (Some(failure), released),
            SessionState::Closed(report) => {
                self.state = SessionState::Closed(report);
                self.phase = M1SessionPhase::Closed;
                return M1SessionRun::Closed(report);
            }
            SessionState::Transition => {
                (Some(M1SessionFailure::Internal), ReleaseSummary::default())
            }
        };
        let report = M1SessionCloseReport {
            previous_phase,
            failure,
            released,
        };
        self.state = SessionState::Closed(report);
        self.phase = M1SessionPhase::Closed;
        M1SessionRun::Closed(report)
    }

    fn terminal_ingress(&self) -> M1SessionIngress {
        M1SessionIngress::Rejected {
            phase: self.phase,
            reason: if self.phase == M1SessionPhase::Ready {
                M1SessionIngressRejectReason::NotWaiting
            } else {
                M1SessionIngressRejectReason::TerminalPhase
            },
        }
    }
}

impl fmt::Debug for M1StrictDocumentSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("M1StrictDocumentSession")
            .field("session_id", &self.session_id)
            .field("generation", &self.generation)
            .field("open_request", &self.open_request)
            .field("phase", &self.phase)
            .field("resources", &self.resources())
            .finish()
    }
}

fn register_service_pending(
    ready: &mut ReadyState,
    ticket: DataTicket,
    target: RangeResumeTarget,
) -> Result<(), M1SessionFailure> {
    let registered = ready
        .source
        .as_mut()
        .expect("Ready retains its source owner")
        .register_pending(ticket, target)
        .map_err(range_failure)?;
    if registered != RangeResumeRegistrationOutcome::Registered {
        return Err(M1SessionFailure::Internal);
    }
    Ok(())
}

fn range_failure(error: RangeResumeError) -> M1SessionFailure {
    if error.category() == RangeResumeErrorCategory::Integrity {
        M1SessionFailure::SourceChanged(Some(error))
    } else {
        M1SessionFailure::Runtime(error)
    }
}

fn release_ready(mut ready: ReadyState, signal_source_changed: bool) -> ReleaseSummary {
    let mut released = ReleaseSummary::default();
    let source = ready
        .source
        .as_mut()
        .expect("Ready retains its source owner until ordered release");
    for service in [M1Service::PageCount, M1Service::Outline] {
        let slot_waiting = match service {
            M1Service::PageCount => ready
                .page_count
                .as_ref()
                .and_then(|slot| slot.waiting.as_ref()),
            M1Service::Outline => ready
                .outline
                .as_ref()
                .and_then(|slot| slot.waiting.as_ref()),
        };
        if let Some(waiting) = slot_waiting {
            released.released_waiting_targets += 1;
            if waiting.completion.is_some() {
                released.released_held_completions += 1;
            } else if source.phase() == RangeResumePhase::Active {
                let _ = source.cancel(waiting.target.job(), waiting.target.generation());
            }
        }
        let slot = match service {
            M1Service::PageCount => ready.page_count.take().map(|slot| {
                drop(slot);
            }),
            M1Service::Outline => ready.outline.take().map(|slot| {
                drop(slot);
            }),
        };
        if slot.is_some() {
            released.released_service_jobs += 1;
            released.released_index_handles += 1;
        }
    }
    if let Some(mut cache) = ready.cache.take() {
        released.cache = Some(cache.close());
        drop(cache);
    }
    if let Some(index) = ready.index.take() {
        drop(index);
        released.released_index_handles += 1;
    }
    if let Some(mut source) = ready.source.take() {
        released.source = Some(
            if signal_source_changed && source.phase() == RangeResumePhase::Active {
                source
                    .signal_source_changed()
                    .unwrap_or_else(|_| source.close())
            } else {
                source.close()
            },
        );
        drop(source);
    }
    released
}
