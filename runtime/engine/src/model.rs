use std::{
    fmt,
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use pdf_rs_cache::TileCacheCancellation;
use pdf_rs_policy::{
    CapabilityDecision, CapabilityEvaluationJob, CapabilityEvaluator, CapabilityProfile,
    PolicyCancellation, PolicyErrorCode, PolicyJobLimits, PolicyJobPoll, PolicyLimits,
    PolicyPollBudget, RenderConfig, RenderPlan, RenderPlanJob, RenderPlanOutcome,
    RenderPlanRequest, RendererEpoch as PolicyRendererEpoch,
};
use pdf_rs_protocol::{
    CancelAcknowledgedEvent, CapabilityReportedEvent, CloseSessionAcknowledgedEvent, Correlation,
    DocumentReadyEvent, EngineErrorCode, GenerationCompletedEvent, GenerationPlannedEvent,
    NeedDataEvent, PageMetricsEvent, RequestCancelledEvent, RequestFailedEvent, RequestId,
    SessionClosedEvent, SessionId, ShutdownAcknowledgedEvent, SurfaceReadyEvent,
    SurfaceReclaimedEvent, SurfaceReleaseAcknowledgedEvent, WorkerId, WorkerStoppedEvent,
};
use pdf_rs_raster::fast::{
    FastRasterCancellation, FastRasterError, FastRasterErrorCategory, FastRasterJobLimits,
    FastRasterJobPoll, FastRasterLimits, FastRasterOwnedJob, FastRasterPollBudget, FastTileSet,
};
use pdf_rs_scene::Scene;
use pdf_rs_scheduler::TerminalSignal;
use pdf_rs_surface::SurfacePlanIdentity;
use pdf_rs_surface::{SurfaceResourceReport, SurfaceTransfer, WorkerEpoch};

use crate::{EngineIntegrationError, NativeWorkerConfig};

/// Whole-Worker actor lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWorkerPhase {
    /// Commands and bounded reentries are accepted.
    Ready,
    /// Normal ingress is stopped while critical cleanup drains.
    Draining,
    /// The old Worker epoch owns no live work or Surface.
    Stopped,
}

/// One Session lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPhase {
    /// Open is waiting on parser/range work.
    Opening,
    /// Immutable Scenes are installed and viewport work is accepted.
    Ready,
    /// New work is rejected and owned resources are being invalidated.
    Closing,
    /// No live Session resource remains.
    Closed,
}

/// Parser completion for one exact Open request.
pub enum OpenCompletion {
    /// A complete immutable document Scene set became ready.
    Ready {
        /// Exact Worker that dispatched the parser operation.
        worker: WorkerId,
        /// Exact Worker process epoch that dispatched the parser operation.
        worker_epoch: WorkerEpoch,
        /// Exact allocated Session.
        session: SessionId,
        /// Exact Open request.
        request: RequestId,
        /// Nonzero product document revision.
        document_revision: u64,
        /// One immutable Scene per ready page.
        scenes: Vec<Arc<Scene>>,
    },
    /// Open reached a terminal parser/document failure.
    Failed {
        /// Exact Worker that dispatched the parser operation.
        worker: WorkerId,
        /// Exact Worker process epoch that dispatched the parser operation.
        worker_epoch: WorkerEpoch,
        /// Exact allocated Session.
        session: SessionId,
        /// Exact Open request.
        request: RequestId,
    },
}

impl fmt::Debug for OpenCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready {
                worker,
                worker_epoch,
                session,
                request,
                document_revision,
                scenes,
            } => formatter
                .debug_struct("Ready")
                .field("worker", worker)
                .field("worker_epoch", worker_epoch)
                .field("session", session)
                .field("request", request)
                .field("document_revision", document_revision)
                .field("scenes", &format_args!("[SCENES:{}]", scenes.len()))
                .finish(),
            Self::Failed {
                worker,
                worker_epoch,
                session,
                request,
            } => formatter
                .debug_struct("Failed")
                .field("worker", worker)
                .field("worker_epoch", worker_epoch)
                .field("session", session)
                .field("request", request)
                .finish(),
        }
    }
}

/// Opaque capability result produced only by [`NativePolicyTask::run`].
///
/// Its fields stay private outside this crate so an external executor can move
/// the result but cannot detach the policy permit or substitute another task's
/// decision or protocol projection.
pub struct NativeCapabilityCompletion {
    pub(crate) worker: WorkerId,
    pub(crate) worker_epoch: WorkerEpoch,
    pub(crate) signal: TerminalSignal,
    pub(crate) decision: CapabilityDecision,
    pub(crate) event: CapabilityReportedEvent,
    pub(crate) permit: NativePolicyPermit,
}

/// Opaque render-plan result produced only by [`NativePolicyTask::run`].
///
/// Its fields stay private outside this crate so the plan, projection, task
/// identity, and move-only permit remain one indivisible actor message.
pub struct NativePlanCompletion {
    pub(crate) worker: WorkerId,
    pub(crate) worker_epoch: WorkerEpoch,
    pub(crate) signal: TerminalSignal,
    pub(crate) plan: Arc<RenderPlan>,
    pub(crate) event: GenerationPlannedEvent,
    pub(crate) permit: NativePolicyPermit,
}

/// Opaque policy failure produced only by [`NativePolicyTask::run`].
///
/// The failure remains inseparable from the exact task identity and permit
/// that observed it.
pub struct NativePolicyFailure {
    pub(crate) worker: WorkerId,
    pub(crate) worker_epoch: WorkerEpoch,
    pub(crate) signal: TerminalSignal,
    pub(crate) failure: EngineErrorCode,
    pub(crate) permit: NativePolicyPermit,
}

/// Opaque all-or-nothing Fast raster result produced by
/// [`NativeRasterTask::run`].
///
/// The tile set remains inseparable from its scheduler identity and aggregate
/// byte reservation while it is outside the actor.
pub struct NativeRasterCompletion {
    pub(crate) signal: TerminalSignal,
    pub(crate) tiles: FastTileSet,
    pub(crate) reservation: NativeRasterReservation,
}

/// Opaque Fast raster failure produced by [`NativeRasterTask::run`].
///
/// The stable failure remains inseparable from the exact task identity and
/// aggregate byte reservation that observed it.
pub struct NativeRasterFailure {
    pub(crate) signal: TerminalSignal,
    pub(crate) failure: EngineErrorCode,
    pub(crate) reservation: NativeRasterReservation,
}

/// Values that may cross into the actor only through its bounded reentry queue.
pub enum Reentry {
    /// A parser/open operation reached one terminal.
    Open(OpenCompletion),
    /// A parser suspended on exact immutable source ranges.
    NeedData {
        /// Exact Worker process epoch that dispatched parser work.
        worker_epoch: WorkerEpoch,
        /// Correlation retained by the Open request.
        correlation: Correlation,
        /// Exact range ticket event.
        event: NeedDataEvent,
    },
    /// A validated range terminal is ready to resume lower-layer work.
    RangeCompleted {
        /// Exact Worker that dispatched range work.
        worker: WorkerId,
        /// Exact Worker process epoch that dispatched range work.
        worker_epoch: WorkerEpoch,
        /// Owning Session.
        session: SessionId,
        /// Ticket whose bytes or source failure became terminal.
        ticket: pdf_rs_protocol::DataTicket,
        /// Whether the completion observed immutable source drift.
        source_changed: bool,
    },
    /// Capability evaluation completed outside the actor.
    CapabilityCompleted(NativeCapabilityCompletion),
    /// Render planning completed outside the actor.
    PlanCompleted(NativePlanCompletion),
    /// External capability evaluation or planning failed.
    PolicyFailed(NativePolicyFailure),
    /// A complete all-or-nothing Fast raster result.
    RasterCompleted(NativeRasterCompletion),
    /// Fast raster failed before a complete tile set existed.
    RasterFailed(NativeRasterFailure),
    /// Replayable cancellation of one exact request.
    Cancel {
        /// Exact Worker process epoch that admitted the command.
        worker_epoch: WorkerEpoch,
        /// Command correlation.
        correlation: Correlation,
        /// Exact request target.
        target: RequestId,
    },
    /// Explicit release of one exact Surface lease.
    Release {
        /// Exact Worker process epoch that admitted the command.
        worker_epoch: WorkerEpoch,
        /// Command correlation.
        correlation: Correlation,
        /// Surface identity.
        surface: pdf_rs_protocol::SurfaceId,
        /// Sensitive exact lease token.
        lease_token: u64,
    },
    /// Session close after protocol/state validation.
    Close {
        /// Exact Worker process epoch that admitted the command.
        worker_epoch: WorkerEpoch,
        /// Command correlation.
        correlation: Correlation,
    },
    /// Whole-Worker shutdown after protocol/state validation.
    Shutdown {
        /// Exact Worker process epoch that admitted the command.
        worker_epoch: WorkerEpoch,
        /// Command correlation.
        correlation: Correlation,
    },
    /// Immutable source drift for one Session.
    SourceChanged {
        /// Exact Worker that observed immutable source drift.
        worker: WorkerId,
        /// Exact Worker process epoch that observed immutable source drift.
        worker_epoch: WorkerEpoch,
        /// Exact Session whose source binding changed.
        session: SessionId,
    },
    /// Disconnect/crash replacement by a strictly newer Worker epoch.
    Restart {
        /// Complete configuration for the distinct replacement epoch.
        config: NativeWorkerConfig,
    },
}

impl fmt::Debug for Reentry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(completion) => formatter.debug_tuple("Open").field(completion).finish(),
            Self::NeedData {
                worker_epoch,
                correlation,
                event,
            } => formatter
                .debug_struct("NeedData")
                .field("worker_epoch", worker_epoch)
                .field("correlation", correlation)
                .field("event", event)
                .finish(),
            Self::RangeCompleted {
                worker,
                worker_epoch,
                session,
                ticket,
                source_changed,
            } => formatter
                .debug_struct("RangeCompleted")
                .field("worker", worker)
                .field("worker_epoch", worker_epoch)
                .field("session", session)
                .field("ticket", ticket)
                .field("source_changed", source_changed)
                .finish(),
            Self::CapabilityCompleted(completion) => formatter
                .debug_struct("CapabilityCompleted")
                .field("worker", &completion.worker)
                .field("worker_epoch", &completion.worker_epoch)
                .field("signal", &completion.signal)
                .field("decision", &completion.decision)
                .field("event", &completion.event)
                .finish(),
            Self::PlanCompleted(completion) => formatter
                .debug_struct("PlanCompleted")
                .field("worker", &completion.worker)
                .field("worker_epoch", &completion.worker_epoch)
                .field("signal", &completion.signal)
                .field("plan_hash", &completion.plan.hash())
                .field("event", &completion.event)
                .finish(),
            Self::PolicyFailed(completion) => formatter
                .debug_struct("PolicyFailed")
                .field("worker", &completion.worker)
                .field("worker_epoch", &completion.worker_epoch)
                .field("signal", &completion.signal)
                .field("failure", &completion.failure)
                .finish(),
            Self::RasterCompleted(completion) => formatter
                .debug_struct("RasterCompleted")
                .field("signal", &completion.signal)
                .field("tiles", &completion.tiles)
                .finish(),
            Self::RasterFailed(completion) => formatter
                .debug_struct("RasterFailed")
                .field("signal", &completion.signal)
                .field("failure", &completion.failure)
                .finish(),
            Self::Cancel {
                worker_epoch,
                correlation,
                target,
            } => formatter
                .debug_struct("Cancel")
                .field("worker_epoch", worker_epoch)
                .field("correlation", correlation)
                .field("target", target)
                .finish(),
            Self::Release {
                worker_epoch,
                correlation,
                surface,
                ..
            } => formatter
                .debug_struct("Release")
                .field("worker_epoch", worker_epoch)
                .field("correlation", correlation)
                .field("surface", surface)
                .field("lease_token", &"[REDACTED]")
                .finish(),
            Self::Close {
                worker_epoch,
                correlation,
            } => formatter
                .debug_struct("Close")
                .field("worker_epoch", worker_epoch)
                .field("correlation", correlation)
                .finish(),
            Self::Shutdown {
                worker_epoch,
                correlation,
            } => formatter
                .debug_struct("Shutdown")
                .field("worker_epoch", worker_epoch)
                .field("correlation", correlation)
                .finish(),
            Self::SourceChanged {
                worker,
                worker_epoch,
                session,
            } => formatter
                .debug_struct("SourceChanged")
                .field("worker", worker)
                .field("worker_epoch", worker_epoch)
                .field("session", session)
                .finish(),
            Self::Restart { config } => formatter
                .debug_struct("Restart")
                .field("config", config)
                .finish(),
        }
    }
}

enum NativePolicyTaskKind {
    EvaluateInput {
        scene: Arc<Scene>,
        document_revision: u64,
        limits: PolicyLimits,
        job_limits: PolicyJobLimits,
    },
    Evaluate(CapabilityEvaluationJob),
    PlanInput {
        scene: Arc<Scene>,
        decision: CapabilityDecision,
        config: RenderConfig,
        request: RenderPlanRequest,
        renderer_epoch: PolicyRendererEpoch,
        limits: PolicyLimits,
        job_limits: PolicyJobLimits,
    },
    Plan(RenderPlanJob),
    Terminal,
}

/// Ownership-typed result of advancing one external task by a bounded poll.
pub enum NativeTaskPoll<T> {
    /// The same owned task retains private state and requires another poll.
    Pending(T),
    /// The task reached one terminal actor reentry.
    Ready(Reentry),
}

/// Owned capability or planning work dispatched out of the actor.
pub struct NativePolicyTask {
    worker: WorkerId,
    worker_epoch: WorkerEpoch,
    signal: TerminalSignal,
    kind: NativePolicyTaskKind,
    cancellation: NativePolicyCancellation,
    permit: NativePolicyPermit,
}

impl NativePolicyTask {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn evaluate(
        worker: WorkerId,
        worker_epoch: WorkerEpoch,
        signal: TerminalSignal,
        scene: Arc<Scene>,
        document_revision: u64,
        limits: PolicyLimits,
        job_limits: PolicyJobLimits,
        cancellation: NativePolicyCancellation,
        permit: NativePolicyPermit,
    ) -> Self {
        Self {
            worker,
            worker_epoch,
            signal,
            kind: NativePolicyTaskKind::EvaluateInput {
                scene,
                document_revision,
                limits,
                job_limits,
            },
            cancellation,
            permit,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn plan(
        worker: WorkerId,
        worker_epoch: WorkerEpoch,
        signal: TerminalSignal,
        scene: Arc<Scene>,
        decision: CapabilityDecision,
        config: RenderConfig,
        request: RenderPlanRequest,
        renderer_epoch: PolicyRendererEpoch,
        limits: PolicyLimits,
        job_limits: PolicyJobLimits,
        cancellation: NativePolicyCancellation,
        permit: NativePolicyPermit,
    ) -> Self {
        Self {
            worker,
            worker_epoch,
            signal,
            kind: NativePolicyTaskKind::PlanInput {
                scene,
                decision,
                config,
                request,
                renderer_epoch,
                limits,
                job_limits,
            },
            cancellation,
            permit,
        }
    }

    /// Returns the exact scheduler terminal identity.
    pub const fn signal(&self) -> TerminalSignal {
        self.signal
    }

    pub(crate) fn mark_external(&mut self) {
        self.permit.mark_external();
    }

    /// Advances at most one explicit policy work budget.
    pub fn poll(
        mut self,
        budget: PolicyPollBudget,
        cancellation: &dyn PolicyCancellation,
    ) -> NativeTaskPoll<Self> {
        let registry_cancellation = self.cancellation.clone();
        let cancellation = CombinedPolicyCancellation {
            registry: &registry_cancellation,
            executor: cancellation,
        };
        let kind = std::mem::replace(&mut self.kind, NativePolicyTaskKind::Terminal);
        let kind = match kind {
            NativePolicyTaskKind::EvaluateInput {
                scene,
                document_revision,
                limits,
                job_limits,
            } => {
                match CapabilityEvaluator::new(CapabilityProfile::m3_reference_v1(), limits)
                    .start_job(scene, document_revision, job_limits)
                {
                    Ok(job) => NativePolicyTaskKind::Evaluate(job),
                    Err(error) => return self.policy_failure(policy_failure_code(error.code())),
                }
            }
            NativePolicyTaskKind::PlanInput {
                scene,
                decision,
                config,
                request,
                renderer_epoch,
                limits,
                job_limits,
            } => match RenderPlanJob::new(
                scene,
                decision,
                config,
                request,
                renderer_epoch,
                limits,
                job_limits,
            ) {
                Ok(job) => NativePolicyTaskKind::Plan(job),
                Err(error) => return self.policy_failure(policy_failure_code(error.code())),
            },
            NativePolicyTaskKind::Terminal => {
                return self.policy_failure(EngineErrorCode::Internal);
            }
            job => job,
        };
        match kind {
            NativePolicyTaskKind::Evaluate(mut job) => match job.poll(budget, &cancellation) {
                PolicyJobPoll::Pending => {
                    self.kind = NativePolicyTaskKind::Evaluate(job);
                    NativeTaskPoll::Pending(self)
                }
                PolicyJobPoll::Ready => {
                    let Some(result) = job.take_result() else {
                        return self.policy_failure(EngineErrorCode::Internal);
                    };
                    let decision = match result {
                        Ok(decision) => decision,
                        Err(error) => {
                            return self.policy_failure(policy_failure_code(error.code()));
                        }
                    };
                    let projection = match decision.protocol_projection() {
                        Ok(projection) => projection,
                        Err(error) => {
                            return self.policy_failure(policy_failure_code(error.code()));
                        }
                    };
                    let event = CapabilityReportedEvent {
                        decision: projection,
                        decision_hash: pdf_rs_protocol::CapabilityDecisionHash::new(
                            decision.hash().into_digest(),
                        ),
                    };
                    NativeTaskPoll::Ready(Reentry::CapabilityCompleted(
                        NativeCapabilityCompletion {
                            worker: self.worker,
                            worker_epoch: self.worker_epoch,
                            signal: self.signal,
                            decision,
                            event,
                            permit: self.permit,
                        },
                    ))
                }
            },
            NativePolicyTaskKind::Plan(mut job) => match job.poll(budget, &cancellation) {
                PolicyJobPoll::Pending => {
                    self.kind = NativePolicyTaskKind::Plan(job);
                    NativeTaskPoll::Pending(self)
                }
                PolicyJobPoll::Ready => {
                    let Some(result) = job.take_result() else {
                        return self.policy_failure(EngineErrorCode::Internal);
                    };
                    let outcome = match result {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            return self.policy_failure(policy_failure_code(error.code()));
                        }
                    };
                    let RenderPlanOutcome::Ready(plan) = outcome else {
                        return self.policy_failure(EngineErrorCode::Internal);
                    };
                    let event = GenerationPlannedEvent {
                        manifest: plan.protocol_manifest().clone(),
                        plan_hash: pdf_rs_protocol::RenderPlanHash::new(plan.hash().into_digest()),
                    };
                    NativeTaskPoll::Ready(Reentry::PlanCompleted(NativePlanCompletion {
                        worker: self.worker,
                        worker_epoch: self.worker_epoch,
                        signal: self.signal,
                        plan: Arc::new(plan),
                        event,
                        permit: self.permit,
                    }))
                }
            },
            NativePolicyTaskKind::EvaluateInput { .. }
            | NativePolicyTaskKind::PlanInput { .. }
            | NativePolicyTaskKind::Terminal => unreachable!("task input was normalized above"),
        }
    }

    /// Runs the same resumable job to completion for synchronous executors.
    pub fn run(self, cancellation: &dyn PolicyCancellation) -> Reentry {
        let budget = PolicyPollBudget::new(
            NonZeroU32::new(4_096).expect("fixed synchronous budget is nonzero"),
        )
        .expect("fixed synchronous budget satisfies the policy hard ceiling");
        let mut task = self;
        loop {
            match task.poll(budget, cancellation) {
                NativeTaskPoll::Pending(pending) => task = pending,
                NativeTaskPoll::Ready(reentry) => return reentry,
            }
        }
    }

    fn policy_failure(self, failure: EngineErrorCode) -> NativeTaskPoll<Self> {
        NativeTaskPoll::Ready(Reentry::PolicyFailed(NativePolicyFailure {
            worker: self.worker,
            worker_epoch: self.worker_epoch,
            signal: self.signal,
            failure,
            permit: self.permit,
        }))
    }
}

impl fmt::Debug for NativePolicyTask {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match &self.kind {
            NativePolicyTaskKind::EvaluateInput { .. } | NativePolicyTaskKind::Evaluate(_) => {
                "Evaluate"
            }
            NativePolicyTaskKind::PlanInput { .. } | NativePolicyTaskKind::Plan(_) => "Plan",
            NativePolicyTaskKind::Terminal => "Terminal",
        };
        formatter
            .debug_struct("NativePolicyTask")
            .field("worker", &self.worker)
            .field("worker_epoch", &self.worker_epoch)
            .field("signal", &self.signal)
            .field("kind", &kind)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct PolicyTaskTracker {
    limit: AtomicUsize,
    used: AtomicUsize,
    byte_limit: AtomicU64,
    bytes_used: AtomicU64,
    external: AtomicUsize,
    worker_epoch: AtomicU64,
}

impl PolicyTaskTracker {
    pub(crate) const fn new(limit: usize, byte_limit: u64, worker_epoch: WorkerEpoch) -> Self {
        Self {
            limit: AtomicUsize::new(limit),
            used: AtomicUsize::new(0),
            byte_limit: AtomicU64::new(byte_limit),
            bytes_used: AtomicU64::new(0),
            external: AtomicUsize::new(0),
            worker_epoch: AtomicU64::new(worker_epoch.value()),
        }
    }

    pub(crate) fn try_acquire(
        self: &Arc<Self>,
        signal: TerminalSignal,
        bytes: u64,
    ) -> Option<NativePolicyPermit> {
        let mut byte_current = self.bytes_used.load(Ordering::Acquire);
        loop {
            let byte_next = byte_current.checked_add(bytes)?;
            if byte_next > self.byte_limit.load(Ordering::Acquire) {
                return None;
            }
            match self.bytes_used.compare_exchange_weak(
                byte_current,
                byte_next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => byte_current = observed,
            }
        }
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(1) else {
                self.bytes_used.fetch_sub(bytes, Ordering::AcqRel);
                return None;
            };
            if next > self.limit.load(Ordering::Acquire) {
                self.bytes_used.fetch_sub(bytes, Ordering::AcqRel);
                return None;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(NativePolicyPermit {
                        tracker: Arc::clone(self),
                        signal,
                        worker_epoch: self.worker_epoch.load(Ordering::Acquire),
                        bytes,
                        external: false,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    pub(crate) fn bytes_used(&self) -> u64 {
        self.bytes_used.load(Ordering::Acquire)
    }

    pub(crate) fn external(&self) -> usize {
        self.external.load(Ordering::Acquire)
    }
}

/// Opaque move-only lifetime permit for external policy work and its result.
pub(crate) struct NativePolicyPermit {
    tracker: Arc<PolicyTaskTracker>,
    signal: TerminalSignal,
    worker_epoch: u64,
    bytes: u64,
    external: bool,
}

impl NativePolicyPermit {
    pub(crate) fn belongs_to(&self, tracker: &Arc<PolicyTaskTracker>) -> bool {
        Arc::ptr_eq(&self.tracker, tracker)
    }

    pub(crate) const fn matches(&self, signal: TerminalSignal) -> bool {
        self.signal.work_id.get() == signal.work_id.get()
            && self.signal.session_id.get() == signal.session_id.get()
            && self.signal.generation.get() == signal.generation.get()
    }

    pub(crate) const fn matches_worker_epoch(&self, worker_epoch: WorkerEpoch) -> bool {
        self.worker_epoch == worker_epoch.value()
    }

    pub(crate) fn mark_external(&mut self) {
        if self.external {
            return;
        }
        self.tracker.external.fetch_add(1, Ordering::AcqRel);
        self.external = true;
    }

    pub(crate) fn mark_internal(&mut self) {
        if !self.external {
            return;
        }
        let previous = self.tracker.external.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
        self.external = false;
    }
}

impl fmt::Debug for NativePolicyPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativePolicyPermit")
            .field("signal", &self.signal)
            .field("worker_epoch", &self.worker_epoch)
            .field("bytes", &self.bytes)
            .field("external", &self.external)
            .field("tracker", &"[REDACTED]")
            .finish()
    }
}

impl Drop for NativePolicyPermit {
    fn drop(&mut self) {
        if self.external {
            let previous = self.tracker.external.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0);
        }
        let previous_bytes = self
            .tracker
            .bytes_used
            .fetch_sub(self.bytes, Ordering::AcqRel);
        debug_assert!(previous_bytes >= self.bytes);
        let previous = self.tracker.used.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NativePolicyCancellation(Arc<AtomicBool>);

impl NativePolicyCancellation {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

impl PolicyCancellation for NativePolicyCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct CombinedPolicyCancellation<'a> {
    registry: &'a NativePolicyCancellation,
    executor: &'a dyn PolicyCancellation,
}

impl PolicyCancellation for CombinedPolicyCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.registry.is_cancelled() || self.executor.is_cancelled()
    }
}

/// Owned Fast CPU work dispatched out of the single-writer registry.
///
/// The task retains immutable Scene and RenderPlan ownership, so a platform
/// executor may run it without borrowing or blocking the registry actor.
pub struct NativeRasterTask {
    signal: TerminalSignal,
    scene: Arc<Scene>,
    plan: Arc<RenderPlan>,
    limits: FastRasterLimits,
    policy_job_limits: PolicyJobLimits,
    job_limits: FastRasterJobLimits,
    job: Option<FastRasterOwnedJob>,
    cancellation: NativeRasterCancellation,
    reservation: NativeRasterReservation,
}

impl NativeRasterTask {
    #[allow(
        clippy::too_many_arguments,
        reason = "the move-only external task binds all immutable inputs, independent limits, cancellation, and its byte reservation"
    )]
    pub(crate) const fn new(
        signal: TerminalSignal,
        scene: Arc<Scene>,
        plan: Arc<RenderPlan>,
        limits: FastRasterLimits,
        policy_job_limits: PolicyJobLimits,
        job_limits: FastRasterJobLimits,
        cancellation: NativeRasterCancellation,
        reservation: NativeRasterReservation,
    ) -> Self {
        Self {
            signal,
            scene,
            plan,
            limits,
            policy_job_limits,
            job_limits,
            job: None,
            cancellation,
            reservation,
        }
    }

    /// Returns the exact scheduler terminal identity.
    pub const fn signal(&self) -> TerminalSignal {
        self.signal
    }

    /// Borrows the exact immutable RenderPlan.
    pub fn plan(&self) -> &RenderPlan {
        &self.plan
    }

    pub(crate) fn mark_external(&mut self) {
        self.reservation.mark_external();
    }

    /// Advances at most one explicit Fast raster work budget.
    pub fn poll(
        mut self,
        budget: FastRasterPollBudget,
        cancellation: &dyn FastRasterCancellation,
    ) -> NativeTaskPoll<Self> {
        if self.job.is_none() {
            let job = match FastRasterOwnedJob::new(
                Arc::clone(&self.scene),
                Arc::clone(&self.plan),
                self.limits,
                self.policy_job_limits,
                self.job_limits,
            ) {
                Ok(job) => job,
                Err(error) => return self.raster_failure(raster_failure_code(error)),
            };
            self.job = Some(job);
        }
        let registry_cancellation = self.cancellation.clone();
        let cancellation = CombinedRasterCancellation {
            registry: &registry_cancellation,
            executor: cancellation,
        };
        let Some(job) = self.job.as_mut() else {
            return self.raster_failure(EngineErrorCode::Internal);
        };
        if job.poll(budget, &cancellation) == FastRasterJobPoll::Pending {
            return NativeTaskPoll::Pending(self);
        }
        let Some(result) = job.take_result() else {
            return self.raster_failure(EngineErrorCode::Internal);
        };
        match result {
            Ok(tiles) => {
                let retained = tiles.stats().retained_bytes();
                if !self.reservation.covers(retained) {
                    return self.raster_failure(EngineErrorCode::Internal);
                }
                self.reservation.shrink_to(retained);
                NativeTaskPoll::Ready(Reentry::RasterCompleted(NativeRasterCompletion {
                    signal: self.signal,
                    tiles,
                    reservation: self.reservation,
                }))
            }
            Err(error) => self.raster_failure(raster_failure_code(error)),
        }
    }

    /// Runs the same owned job to completion for synchronous executors.
    pub fn run(self, cancellation: &dyn FastRasterCancellation) -> Reentry {
        let budget = FastRasterPollBudget::new(
            NonZeroU32::new(4_096).expect("fixed synchronous budget is nonzero"),
        )
        .expect("fixed synchronous raster budget satisfies its hard ceiling");
        let mut task = self;
        loop {
            match task.poll(budget, cancellation) {
                NativeTaskPoll::Pending(pending) => task = pending,
                NativeTaskPoll::Ready(reentry) => return reentry,
            }
        }
    }

    fn raster_failure(mut self, failure: EngineErrorCode) -> NativeTaskPoll<Self> {
        self.reservation.shrink_to(0);
        NativeTaskPoll::Ready(Reentry::RasterFailed(NativeRasterFailure {
            signal: self.signal,
            failure,
            reservation: self.reservation,
        }))
    }
}

#[derive(Debug)]
pub(crate) struct RasterBudget {
    limit: AtomicU64,
    used: AtomicU64,
    external: AtomicUsize,
    worker_epoch: AtomicU64,
}

impl RasterBudget {
    pub(crate) const fn new(limit: u64, worker_epoch: pdf_rs_surface::WorkerEpoch) -> Self {
        Self {
            limit: AtomicU64::new(limit),
            used: AtomicU64::new(0),
            external: AtomicUsize::new(0),
            worker_epoch: AtomicU64::new(worker_epoch.value()),
        }
    }

    pub(crate) fn try_reserve(
        self: &Arc<Self>,
        bytes: u64,
        signal: TerminalSignal,
    ) -> Option<NativeRasterReservation> {
        self.try_charge(bytes)?;
        Some(NativeRasterReservation {
            budget: Arc::clone(self),
            bytes,
            signal,
            worker_epoch: self.worker_epoch.load(Ordering::Acquire),
            external: false,
        })
    }

    pub(crate) fn try_reserve_bytes(
        self: &Arc<Self>,
        bytes: u64,
    ) -> Option<NativeRasterByteReservation> {
        self.try_charge(bytes)?;
        Some(NativeRasterByteReservation {
            budget: Arc::clone(self),
            bytes,
        })
    }

    fn try_charge(&self, bytes: u64) -> Option<()> {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(bytes)?;
            if next > self.limit.load(Ordering::Acquire) {
                return None;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(()),
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn used(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }

    pub(crate) fn external(&self) -> usize {
        self.external.load(Ordering::Acquire)
    }
}

/// Move-only charge for immutable bytes copied out of Surface ownership.
pub(crate) struct NativeRasterByteReservation {
    budget: Arc<RasterBudget>,
    bytes: u64,
}

impl fmt::Debug for NativeRasterByteReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeRasterByteReservation")
            .field("bytes", &self.bytes)
            .field("budget", &"[REDACTED]")
            .finish()
    }
}

impl Drop for NativeRasterByteReservation {
    fn drop(&mut self) {
        let previous = self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
        debug_assert!(previous >= self.bytes);
    }
}

/// Opaque reservation that charges external and actor-owned raster bytes.
///
/// The permit is move-only and releases its charge automatically when its
/// associated task, completion, or retained tile set is dropped.
pub(crate) struct NativeRasterReservation {
    budget: Arc<RasterBudget>,
    bytes: u64,
    signal: TerminalSignal,
    worker_epoch: u64,
    external: bool,
}

impl NativeRasterReservation {
    pub(crate) fn belongs_to(&self, budget: &Arc<RasterBudget>) -> bool {
        Arc::ptr_eq(&self.budget, budget)
    }

    pub(crate) const fn covers(&self, bytes: u64) -> bool {
        bytes <= self.bytes
    }

    pub(crate) fn shrink_to(&mut self, bytes: u64) {
        debug_assert!(bytes <= self.bytes);
        if bytes >= self.bytes {
            return;
        }
        let released = self.bytes - bytes;
        let previous = self.budget.used.fetch_sub(released, Ordering::AcqRel);
        debug_assert!(previous >= released);
        self.bytes = bytes;
    }

    pub(crate) const fn matches(&self, signal: TerminalSignal) -> bool {
        self.signal.work_id.get() == signal.work_id.get()
            && self.signal.session_id.get() == signal.session_id.get()
            && self.signal.generation.get() == signal.generation.get()
    }

    pub(crate) const fn matches_worker_epoch(
        &self,
        worker_epoch: pdf_rs_surface::WorkerEpoch,
    ) -> bool {
        self.worker_epoch == worker_epoch.value()
    }

    pub(crate) fn mark_external(&mut self) {
        if self.external {
            return;
        }
        self.budget.external.fetch_add(1, Ordering::AcqRel);
        self.external = true;
    }

    pub(crate) fn mark_internal(&mut self) {
        if !self.external {
            return;
        }
        let previous = self.budget.external.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
        self.external = false;
    }
}

impl fmt::Debug for NativeRasterReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeRasterReservation")
            .field("bytes", &self.bytes)
            .field("signal", &self.signal)
            .field("worker_epoch", &self.worker_epoch)
            .field("external", &self.external)
            .field("budget", &"[REDACTED]")
            .finish()
    }
}

impl Drop for NativeRasterReservation {
    fn drop(&mut self) {
        if self.external {
            let previous = self.budget.external.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0);
        }
        let previous = self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
        debug_assert!(previous >= self.bytes);
    }
}

#[derive(Debug)]
pub(crate) struct SceneBudget {
    limit: AtomicU64,
    used: AtomicU64,
}

impl SceneBudget {
    pub(crate) const fn new(limit: u64) -> Self {
        Self {
            limit: AtomicU64::new(limit),
            used: AtomicU64::new(0),
        }
    }

    pub(crate) fn try_reserve(self: &Arc<Self>, bytes: u64) -> Option<NativeSceneReservation> {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(bytes)?;
            if next > self.limit.load(Ordering::Acquire) {
                return None;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(NativeSceneReservation {
                        budget: Arc::clone(self),
                        bytes,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn used(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }

    pub(crate) fn reconfigure(&self, limit: u64) {
        self.limit.store(limit, Ordering::Release);
    }
}

pub(crate) struct NativeSceneReservation {
    budget: Arc<SceneBudget>,
    bytes: u64,
}

impl NativeSceneReservation {
    pub(crate) const fn covers(&self, bytes: u64) -> bool {
        bytes <= self.bytes
    }

    pub(crate) fn shrink_to(&mut self, bytes: u64) {
        debug_assert!(bytes <= self.bytes);
        if bytes >= self.bytes {
            return;
        }
        let released = self.bytes - bytes;
        let previous = self.budget.used.fetch_sub(released, Ordering::AcqRel);
        debug_assert!(previous >= released);
        self.bytes = bytes;
    }
}

impl fmt::Debug for NativeSceneReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeSceneReservation")
            .field("bytes", &self.bytes)
            .field("budget", &"[REDACTED]")
            .finish()
    }
}

impl Drop for NativeSceneReservation {
    fn drop(&mut self) {
        let previous = self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
        debug_assert!(previous >= self.bytes);
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeRasterCancellation(Arc<AtomicBool>);

impl NativeRasterCancellation {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl FastRasterCancellation for NativeRasterCancellation {
    fn is_cancelled(&self) -> bool {
        Self::is_cancelled(self)
    }
}

impl TileCacheCancellation for NativeRasterCancellation {
    fn is_cancelled(&self) -> bool {
        Self::is_cancelled(self)
    }
}

struct CombinedRasterCancellation<'a> {
    registry: &'a NativeRasterCancellation,
    executor: &'a dyn FastRasterCancellation,
}

impl FastRasterCancellation for CombinedRasterCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.registry.is_cancelled() || self.executor.is_cancelled()
    }
}

impl fmt::Debug for NativeRasterTask {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeRasterTask")
            .field("signal", &self.signal)
            .field("plan_hash", &self.plan.hash())
            .field("scene", &"[REDACTED]")
            .finish()
    }
}

fn raster_failure_code(error: FastRasterError) -> EngineErrorCode {
    match error.category() {
        FastRasterErrorCategory::ResourceLimit => EngineErrorCode::ResourceLimit,
        FastRasterErrorCategory::Cancelled => EngineErrorCode::Cancelled,
        FastRasterErrorCategory::InvalidInput | FastRasterErrorCategory::Internal => {
            EngineErrorCode::Internal
        }
    }
}

fn policy_failure_code(error: PolicyErrorCode) -> EngineErrorCode {
    match error {
        PolicyErrorCode::ResourceLimit | PolicyErrorCode::Allocation => {
            EngineErrorCode::ResourceLimit
        }
        PolicyErrorCode::Cancelled => EngineErrorCode::Cancelled,
        PolicyErrorCode::InvalidLimits
        | PolicyErrorCode::NumericOverflow
        | PolicyErrorCode::SceneCanonicalization
        | PolicyErrorCode::InvalidRenderConfig
        | PolicyErrorCode::InvalidRenderRequest
        | PolicyErrorCode::IdentityMismatch
        | PolicyErrorCode::InvalidDocumentRevision => EngineErrorCode::Internal,
    }
}

/// Bounded reentry rejection that retains complete message ownership.
#[derive(Debug)]
pub struct ReentryAdmissionError {
    error: EngineIntegrationError,
    reentry: Reentry,
}

impl ReentryAdmissionError {
    pub(crate) const fn new(error: EngineIntegrationError, reentry: Reentry) -> Self {
        Self { error, reentry }
    }

    /// Returns the stable admission failure.
    pub const fn error(&self) -> EngineIntegrationError {
        self.error
    }

    /// Returns ownership of the rejected reentry.
    pub fn into_reentry(self) -> Reentry {
        self.reentry
    }
}

/// One exact shared-memory Surface publication and its out-of-band handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfacePublication {
    correlation: Correlation,
    event: SurfaceReadyEvent,
    plan: SurfacePlanIdentity,
    transfer: SurfaceTransfer,
}

impl SurfacePublication {
    pub(crate) const fn new(
        correlation: Correlation,
        event: SurfaceReadyEvent,
        plan: SurfacePlanIdentity,
        transfer: SurfaceTransfer,
    ) -> Self {
        Self {
            correlation,
            event,
            plan,
            transfer,
        }
    }

    /// Returns exact Worker, Session, and generation correlation.
    pub const fn correlation(&self) -> &Correlation {
        &self.correlation
    }

    /// Returns canonical SurfaceReady metadata and transport.
    pub const fn event(&self) -> &SurfaceReadyEvent {
        &self.event
    }

    /// Borrows the exact canonical RenderPlan tile identity used for import.
    pub const fn plan(&self) -> &SurfacePlanIdentity {
        &self.plan
    }

    /// Returns canonical metadata plus the one-shot shared-memory handle.
    pub const fn transfer(&self) -> &SurfaceTransfer {
        &self.transfer
    }
}

/// Immutable owned byte export produced only after one-shot Surface import.
pub struct ImportedSurfaceBytes {
    correlation: Correlation,
    metadata: pdf_rs_protocol::SurfaceMetadata,
    plan: SurfacePlanIdentity,
    bytes: Vec<u8>,
    _reservation: NativeRasterByteReservation,
}

impl ImportedSurfaceBytes {
    pub(crate) const fn new(
        correlation: Correlation,
        metadata: pdf_rs_protocol::SurfaceMetadata,
        plan: SurfacePlanIdentity,
        bytes: Vec<u8>,
        reservation: NativeRasterByteReservation,
    ) -> Self {
        Self {
            correlation,
            metadata,
            plan,
            bytes,
            _reservation: reservation,
        }
    }

    /// Returns the exact Worker, Session, and generation correlation.
    pub const fn correlation(&self) -> &Correlation {
        &self.correlation
    }

    /// Borrows the canonical validated Surface metadata and exact byte range.
    pub const fn metadata(&self) -> &pdf_rs_protocol::SurfaceMetadata {
        &self.metadata
    }

    /// Borrows the exact canonical RenderPlan tile identity used for import.
    pub const fn plan(&self) -> &SurfacePlanIdentity {
        &self.plan
    }

    /// Borrows immutable pixels for exactly `metadata.byte_length` bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the allocator-retained byte capacity charged to the Worker.
    pub fn retained_byte_capacity(&self) -> usize {
        self.bytes.capacity()
    }
}

impl PartialEq for ImportedSurfaceBytes {
    fn eq(&self, other: &Self) -> bool {
        self.correlation == other.correlation
            && self.metadata == other.metadata
            && self.plan == other.plan
            && self.bytes == other.bytes
    }
}

impl Eq for ImportedSurfaceBytes {}

impl fmt::Debug for ImportedSurfaceBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportedSurfaceBytes")
            .field("correlation", &self.correlation)
            .field("metadata", &self.metadata)
            .field("plan", &self.plan)
            .field("bytes", &format_args!("[BYTES:{}]", self.bytes.len()))
            .finish()
    }
}

/// Typed outbound event owned by the Worker actor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeWorkerEvent {
    /// An Open parser requested immutable source ranges.
    NeedData {
        /// Exact request correlation.
        correlation: Correlation,
        /// Bounded ticket and ranges.
        event: NeedDataEvent,
    },
    /// One Open request reached its successful terminal.
    DocumentReady {
        /// Exact Open request correlation.
        correlation: Correlation,
        /// Ready Session metadata.
        event: DocumentReadyEvent,
    },
    /// One bounded page-metrics request reached its successful terminal.
    PageMetrics {
        /// Exact Session request correlation.
        correlation: Correlation,
        /// Canonical source-bound page geometry slice.
        event: PageMetricsEvent,
    },
    /// Capability evaluation completed before planning/raster allocation.
    CapabilityReported {
        /// Exact generation correlation.
        correlation: Correlation,
        /// Canonical bounded capability decision.
        event: CapabilityReportedEvent,
        /// Internal never-reused scheduled work identity.
        work_id: u64,
    },
    /// One supported exact Native RenderPlan was created.
    GenerationPlanned {
        /// Exact generation correlation.
        correlation: Correlation,
        /// Canonical plan manifest and hash.
        event: GenerationPlannedEvent,
        /// Internal never-reused scheduled work identity.
        work_id: u64,
    },
    /// One exact current-generation immutable Surface became transferable.
    SurfaceReady(SurfacePublication),
    /// One previously delivered Surface lease became terminal.
    SurfaceReclaimed {
        /// Exact owning Session correlation.
        correlation: Correlation,
        /// Exact Surface and lease identity plus lifecycle reason.
        event: SurfaceReclaimedEvent,
    },
    /// One viewport generation reached one terminal.
    GenerationCompleted {
        /// Exact generation correlation.
        correlation: Correlation,
        /// Terminal generation status and accounting.
        event: GenerationCompletedEvent,
    },
    /// Cancellation stream event for the exact Open request.
    RequestCancelled {
        /// Exact request correlation.
        correlation: Correlation,
        /// Cancelled request.
        event: RequestCancelledEvent,
    },
    /// One request reached a non-cancellation failure terminal.
    RequestFailed {
        /// Exact request correlation.
        correlation: Correlation,
        /// Stable protocol error value.
        event: RequestFailedEvent,
    },
    /// Replayable cancellation command terminal.
    CancelAcknowledged {
        /// Exact request correlation.
        correlation: Correlation,
        /// Stable application status.
        event: CancelAcknowledgedEvent,
    },
    /// Replayable Surface release terminal.
    SurfaceReleaseAcknowledged {
        /// Exact Session correlation.
        correlation: Correlation,
        /// Stable application status.
        event: SurfaceReleaseAcknowledgedEvent,
    },
    /// Replayable close terminal.
    CloseSessionAcknowledged {
        /// Exact Session correlation.
        correlation: Correlation,
        /// Stable application status.
        event: CloseSessionAcknowledgedEvent,
    },
    /// Session lifecycle stream event.
    SessionClosed {
        /// Exact Session correlation.
        correlation: Correlation,
        /// Closed Session identity.
        event: SessionClosedEvent,
    },
    /// Replayable Worker shutdown terminal.
    ShutdownAcknowledged {
        /// Exact Worker correlation.
        correlation: Correlation,
        /// Stable application status.
        event: ShutdownAcknowledgedEvent,
    },
    /// Whole-Worker terminal stream event.
    WorkerStopped {
        /// Exact Worker correlation.
        correlation: Correlation,
        /// Stopped Worker identity.
        event: WorkerStoppedEvent,
    },
}

impl NativeWorkerEvent {
    pub(crate) const fn barrier_work_id(&self) -> Option<u64> {
        match self {
            Self::CapabilityReported { work_id, .. } | Self::GenerationPlanned { work_id, .. } => {
                Some(*work_id)
            }
            _ => None,
        }
    }

    pub(crate) const fn is_capability_barrier(&self) -> bool {
        matches!(self, Self::CapabilityReported { .. })
    }

    pub(crate) const fn session(&self) -> Option<SessionId> {
        match self {
            Self::NeedData { correlation, .. }
            | Self::DocumentReady { correlation, .. }
            | Self::PageMetrics { correlation, .. }
            | Self::CapabilityReported { correlation, .. }
            | Self::GenerationPlanned { correlation, .. }
            | Self::GenerationCompleted { correlation, .. }
            | Self::SurfaceReclaimed { correlation, .. }
            | Self::RequestCancelled { correlation, .. }
            | Self::RequestFailed { correlation, .. }
            | Self::CancelAcknowledged { correlation, .. }
            | Self::SurfaceReleaseAcknowledged { correlation, .. }
            | Self::CloseSessionAcknowledged { correlation, .. }
            | Self::SessionClosed { correlation, .. } => correlation.session,
            Self::SurfaceReady(publication) => publication.correlation.session,
            Self::ShutdownAcknowledged { .. } | Self::WorkerStopped { .. } => None,
        }
    }

    pub(crate) const fn generation(&self) -> Option<u64> {
        match self {
            Self::NeedData { correlation, .. }
            | Self::DocumentReady { correlation, .. }
            | Self::PageMetrics { correlation, .. }
            | Self::CapabilityReported { correlation, .. }
            | Self::GenerationPlanned { correlation, .. }
            | Self::GenerationCompleted { correlation, .. }
            | Self::SurfaceReclaimed { correlation, .. }
            | Self::RequestCancelled { correlation, .. }
            | Self::RequestFailed { correlation, .. }
            | Self::CancelAcknowledged { correlation, .. }
            | Self::SurfaceReleaseAcknowledged { correlation, .. }
            | Self::CloseSessionAcknowledged { correlation, .. }
            | Self::SessionClosed { correlation, .. } => correlation.generation,
            Self::SurfaceReady(publication) => publication.correlation.generation,
            Self::ShutdownAcknowledged { .. } | Self::WorkerStopped { .. } => None,
        }
    }

    pub(crate) const fn is_progress(&self) -> bool {
        matches!(self, Self::GenerationPlanned { .. })
    }

    pub(crate) const fn is_delivery_terminal(&self) -> bool {
        matches!(
            self,
            Self::DocumentReady { .. }
                | Self::PageMetrics { .. }
                | Self::GenerationCompleted { .. }
                | Self::SurfaceReclaimed { .. }
                | Self::RequestCancelled { .. }
                | Self::RequestFailed { .. }
                | Self::CancelAcknowledged { .. }
                | Self::SurfaceReleaseAcknowledged { .. }
                | Self::CloseSessionAcknowledged { .. }
                | Self::SessionClosed { .. }
                | Self::ShutdownAcknowledged { .. }
                | Self::WorkerStopped { .. }
        )
    }
}

/// One explicit actor pump result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorProgress {
    /// No reentry, scheduling, raster, publication, or lifecycle work was ready.
    Idle,
    /// One bounded reentry was consumed.
    Reentry,
    /// One scheduler item was dispatched or terminal-arbitrated.
    Scheduled,
    /// One external policy task was dispatched or one policy barrier advanced.
    Capability,
    /// One immutable Fast CPU task was dispatched to an external executor.
    Raster,
    /// One bounded cache-hit copy slice or completed cache-hit set advanced.
    CacheHit,
    /// One current-generation Surface was published.
    Published,
    /// One critical close/release/shutdown/restart transition ran.
    Lifecycle,
    /// Worker shutdown reached exact terminal zero.
    Stopped,
}

/// Exact live-resource snapshot for integration tests and shutdown evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeWorkerResources {
    sessions: usize,
    queued_reentries: usize,
    queued_normal: usize,
    queued_critical: usize,
    in_flight: usize,
    pending_policy_tasks: usize,
    retained_policy_job_bytes: u64,
    pending_rasters: usize,
    retained_raster_bytes: u64,
    retained_cache_bytes: u64,
    retained_scene_bytes: u64,
    queued_publications: usize,
    delivered_surface_leases: usize,
    queued_events: usize,
    surface: SurfaceResourceReport,
}

impl NativeWorkerResources {
    #[allow(clippy::too_many_arguments)]
    pub(crate) const fn new(
        sessions: usize,
        queued_reentries: usize,
        queued_normal: usize,
        queued_critical: usize,
        in_flight: usize,
        pending_policy_tasks: usize,
        retained_policy_job_bytes: u64,
        pending_rasters: usize,
        retained_raster_bytes: u64,
        retained_cache_bytes: u64,
        retained_scene_bytes: u64,
        queued_publications: usize,
        delivered_surface_leases: usize,
        queued_events: usize,
        surface: SurfaceResourceReport,
    ) -> Self {
        Self {
            sessions,
            queued_reentries,
            queued_normal,
            queued_critical,
            in_flight,
            pending_policy_tasks,
            retained_policy_job_bytes,
            pending_rasters,
            retained_raster_bytes,
            retained_cache_bytes,
            retained_scene_bytes,
            queued_publications,
            delivered_surface_leases,
            queued_events,
            surface,
        }
    }

    /// Returns retained Session records.
    pub const fn sessions(self) -> usize {
        self.sessions
    }

    /// Returns queued parser/range/policy/raster/lifecycle reentries.
    pub const fn queued_reentries(self) -> usize {
        self.queued_reentries
    }

    /// Returns replaceable scheduler work.
    pub const fn queued_normal(self) -> usize {
        self.queued_normal
    }

    /// Returns dedicated scheduler lifecycle traffic.
    pub const fn queued_critical(self) -> usize {
        self.queued_critical
    }

    /// Returns exact in-flight scheduler work.
    pub const fn in_flight(self) -> usize {
        self.in_flight
    }

    /// Returns queued, external, or completed policy tasks not yet consumed.
    pub const fn pending_policy_tasks(self) -> usize {
        self.pending_policy_tasks
    }

    /// Returns worst-case owned bytes reserved by queued, external, or
    /// completed pollable policy jobs.
    pub const fn retained_policy_job_bytes(self) -> u64 {
        self.retained_policy_job_bytes
    }

    /// Returns complete raster resources awaiting terminal arbitration.
    pub const fn pending_rasters(self) -> usize {
        self.pending_rasters
    }

    /// Returns aggregate working and retained bytes reserved by raster or cache work.
    pub const fn retained_raster_bytes(self) -> u64 {
        self.retained_raster_bytes
    }

    /// Returns aggregate metadata and pixels retained by all Session tile caches.
    pub const fn retained_cache_bytes(self) -> u64 {
        self.retained_cache_bytes
    }

    /// Returns aggregate Scene payload and integration-ownership bytes retained.
    pub const fn retained_scene_bytes(self) -> u64 {
        self.retained_scene_bytes
    }

    /// Returns tile sets approved for incremental Surface publication.
    pub const fn queued_publications(self) -> usize {
        self.queued_publications
    }

    /// Returns Surface leases removed from the event queue but not yet
    /// released by the Host or rolled back by a platform adapter.
    pub const fn delivered_surface_leases(self) -> usize {
        self.delivered_surface_leases
    }

    /// Returns undelivered critical plus coalesced progress events.
    pub const fn queued_events(self) -> usize {
        self.queued_events
    }

    /// Returns exact Surface owner accounting.
    pub const fn surface(self) -> SurfaceResourceReport {
        self.surface
    }

    /// Reports exact zero across work, publications, events, and Surface storage.
    pub const fn has_zero_live_resources(self) -> bool {
        self.sessions == 0
            && self.queued_reentries == 0
            && self.queued_normal == 0
            && self.queued_critical == 0
            && self.in_flight == 0
            && self.pending_policy_tasks == 0
            && self.retained_policy_job_bytes == 0
            && self.pending_rasters == 0
            && self.retained_raster_bytes == 0
            && self.retained_cache_bytes == 0
            && self.retained_scene_bytes == 0
            && self.queued_publications == 0
            && self.delivered_surface_leases == 0
            && self.queued_events == 0
            && self.surface.active_sessions() == 0
            && self.surface.has_zero_surface_resources()
    }
}

pub(crate) const fn worker_correlation(worker: WorkerId) -> Correlation {
    Correlation {
        worker,
        session: None,
        request: None,
        generation: None,
    }
}
