use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use pdf_rs_cache::{
    NativeTile, NeverCancelledTileCache, TileAdmission, TileCache, TileCacheAddress,
    TileCacheBinding, TileCacheLookup, TileCacheOwnerId, TileCacheSessionId, TileRenderOutcome,
    TileRetentionClass,
};
use pdf_rs_policy::{
    CapabilityDecision, CapabilityProfile, CapabilityStatus, DeviceRect, NativeBackend,
    OptionalContentIdentity, QualityPolicy, RenderConfig, RenderConfigInput, RenderPlan,
    RenderPlanRequest, RendererEpoch as PolicyRendererEpoch, ZoomRatio, page_geometry_identity,
};
use pdf_rs_protocol::{
    CancelAcknowledgedEvent, CancelCommand, CapabilityProfileId, CloseSessionAcknowledgedEvent,
    CloseSessionCommand, Correlation, DataTicketCommitOutcome, DataTicketLedger, DiagnosticId,
    DocumentReadyEvent, EngineError, EngineErrorCode, FailDataCommand, GenerationCompletedEvent,
    GenerationCompletionStatus, GetPageMetricsCommand, MESSAGE_ID_CANCEL, MESSAGE_ID_CLOSE_SESSION,
    MESSAGE_ID_GET_PAGE_METRICS, MESSAGE_ID_OPEN, MESSAGE_ID_RELEASE_SURFACE, MESSAGE_ID_SHUTDOWN,
    OpenCommand, OperationAckStatus, OutputProfile, PAGE_METRICS_EVENT_PAGES_MAX_COUNT,
    PageGeometry as ProtocolPageGeometry, PageMetric, PageMetricsEvent,
    PageRotation as ProtocolPageRotation, ProtocolValidator, ProvideDataCommand,
    ReleaseSurfaceCommand, RendererEpoch as ProtocolRendererEpoch, RequestCancelledEvent,
    RequestFailedEvent, RequestId, SessionClosedEvent, SessionId as ProtocolSessionId,
    SetViewportCommand, ShutdownAcknowledgedEvent, ShutdownCommand, SourceDescriptor,
    SourceFailureCode, SurfaceMetadata, SurfaceReadyEvent, SurfaceReclaimReason,
    SurfaceReclaimedEvent, SurfaceReleaseAcknowledgedEvent, WorkerId, WorkerStoppedEvent,
};
use pdf_rs_raster::fast::FastTileSet;
use pdf_rs_scene::{PageRotation, Scene};
use pdf_rs_scheduler::{
    CriticalDispatch, Distance, Generation, Priority, ReplaceKey, ResourceId, SchedulerDispatch,
    ScrollRelation, SessionId as SchedulerSessionId, SubmitOutcome, TerminalDecision,
    TerminalSignal, ViewportScheduler, WorkId, WorkRequest,
};
use pdf_rs_surface::{
    SurfaceAccess, SurfaceAllocation, SurfaceConsumerContext, SurfaceOwner, SurfacePlanIdentity,
    SurfaceTransfer, WorkerEpoch,
};

use crate::error::{
    backpressure, cache, identity_mismatch, internal, invalid_config, invalid_identity,
    invalid_state, policy, protocol, scheduler, surface,
};
use crate::model::worker_correlation;
use crate::model::{
    NativePolicyCancellation, NativePolicyPermit, NativeRasterCancellation,
    NativeRasterReservation, NativeSceneReservation, PolicyTaskTracker, RasterBudget, SceneBudget,
};
use crate::{
    ActorProgress, EngineIntegrationError, ImportedSurfaceBytes, NativePolicyTask,
    NativeRasterTask, NativeWorkerConfig, NativeWorkerEvent, NativeWorkerPhase,
    NativeWorkerResources, OpenCompletion, Reentry, ReentryAdmissionError, SessionPhase,
    SurfacePublication,
};

const CACHE_COPY_CHUNK_BYTES: usize = 64 * 1024;
const SCENE_OWNERSHIP_FLOOR_BYTES: u64 = 4 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestState {
    Active(ProtocolSessionId),
    Succeeded(ProtocolSessionId),
    Failed(ProtocolSessionId),
    Cancelled(ProtocolSessionId),
}

struct Session {
    phase: SessionPhase,
    source: SourceDescriptor,
    open_request: RequestId,
    document_revision: Option<u64>,
    scenes: Vec<Arc<Scene>>,
    scene_reservation: NativeSceneReservation,
    cache: Option<TileCache>,
    viewport_generation: Option<u64>,
}

#[derive(Clone)]
struct ViewportJob {
    correlation: Correlation,
    command: SetViewportCommand,
}

struct CompletedPlan {
    plan: Arc<RenderPlan>,
    tiles: CompletedTiles,
}

enum CompletedTiles {
    Raster {
        tiles: FastTileSet,
        _reservation: NativeRasterReservation,
    },
    CacheHit {
        tiles: Vec<CachedTilePixels>,
        _reservation: NativeRasterReservation,
    },
}

struct CachedTilePixels {
    content_key: pdf_rs_policy::TileContentKey,
    stride: u32,
    pixels: Vec<u8>,
}

struct CachedTileCopy {
    content_key: pdf_rs_policy::TileContentKey,
    stride: u32,
    total_bytes: usize,
    pixels: Vec<u8>,
}

struct CacheLookupState {
    plan: Arc<RenderPlan>,
    reservation: NativeRasterReservation,
    cancellation: NativeRasterCancellation,
    tiles: Vec<CachedTilePixels>,
    tile_index: usize,
    retained_bytes: u64,
    current: Option<CachedTileCopy>,
}

enum CacheCopyProgress {
    Continue,
    Complete,
    Miss,
}

struct CapabilityStage {
    decision: CapabilityDecision,
    permit: NativePolicyPermit,
}

struct PlanStage {
    plan: Arc<RenderPlan>,
    permit: NativePolicyPermit,
}

impl CompletedPlan {
    fn tile_count(&self) -> usize {
        match &self.tiles {
            CompletedTiles::Raster { tiles, .. } => tiles.tiles().len(),
            CompletedTiles::CacheHit { tiles, .. } => tiles.len(),
        }
    }
}

enum PageStage {
    Evaluate,
    CapabilityPending {
        cancellation: NativePolicyCancellation,
    },
    CapabilityQueued(CapabilityStage),
    CapabilityDelivered(CapabilityStage),
    PlanPending {
        cancellation: NativePolicyCancellation,
    },
    PlanQueued(PlanStage),
    PlanDelivered(PlanStage),
    CacheLookup(CacheLookupState),
    RasterPending {
        plan: Arc<RenderPlan>,
        cancellation: NativeRasterCancellation,
    },
}

struct ActiveViewport {
    signal: TerminalSignal,
    job: ViewportJob,
    page_cursor: usize,
    stage: PageStage,
    completed: Vec<CompletedPlan>,
    failure: Option<EngineErrorCode>,
}

impl ActiveViewport {
    fn cancel_work(&self) {
        match &self.stage {
            PageStage::CapabilityPending { cancellation }
            | PageStage::PlanPending { cancellation } => cancellation.cancel(),
            PageStage::CacheLookup(state) => state.cancellation.cancel(),
            PageStage::RasterPending { cancellation, .. } => cancellation.cancel(),
            PageStage::Evaluate
            | PageStage::CapabilityQueued(_)
            | PageStage::CapabilityDelivered(_)
            | PageStage::PlanQueued(_)
            | PageStage::PlanDelivered(_) => {}
        }
    }
}

struct TerminalJob {
    signal: TerminalSignal,
    correlation: Correlation,
    failure: EngineErrorCode,
}

struct CompletedViewport {
    signal: TerminalSignal,
    correlation: Correlation,
    plans: Vec<CompletedPlan>,
}

struct PublicationBatch {
    correlation: Correlation,
    plans: Vec<CompletedPlan>,
    plan_index: usize,
    tile_index: usize,
    produced_regions: u32,
    staged: VecDeque<SurfacePublication>,
    staging_complete: bool,
}

#[derive(Clone, Copy)]
enum SessionCloseReason {
    Explicit,
    SourceChanged,
    Cancelled,
    OpenFailed,
    Internal,
}

struct PendingSessionClose {
    session: ProtocolSessionId,
    scheduler_session: SchedulerSessionId,
    correlation: Option<Correlation>,
    reason: SessionCloseReason,
    queued_terminals: Vec<Correlation>,
}

struct DeliveredSurfaceLease {
    correlation: Correlation,
    generation: u64,
    surface: pdf_rs_protocol::SurfaceId,
    lease_token: u64,
}

/// Single-writer bounded owner for one exact Native Worker epoch.
pub struct NativeWorkerRegistry {
    config: NativeWorkerConfig,
    phase: NativeWorkerPhase,
    validator: ProtocolValidator,
    tickets: DataTicketLedger,
    scheduler: ViewportScheduler,
    surfaces: SurfaceOwner,
    sessions: BTreeMap<ProtocolSessionId, Session>,
    closed_sessions: BTreeSet<ProtocolSessionId>,
    requests: BTreeMap<RequestId, RequestState>,
    queued_jobs: BTreeMap<WorkId, ViewportJob>,
    active: BTreeMap<WorkId, ActiveViewport>,
    terminal_jobs: BTreeMap<WorkId, TerminalJob>,
    pending_resources: BTreeMap<ResourceId, CompletedViewport>,
    publications: VecDeque<PublicationBatch>,
    close_backlog: VecDeque<PendingSessionClose>,
    pending_closes: BTreeMap<SchedulerSessionId, PendingSessionClose>,
    pending_shutdown: Option<Correlation>,
    shutdown_admitted: bool,
    shutdown_queued_terminals: Vec<Correlation>,
    cancel_backlog: VecDeque<TerminalSignal>,
    normal_reentries: VecDeque<Reentry>,
    critical_reentries: VecDeque<Reentry>,
    lifecycle_reentries: VecDeque<Reentry>,
    policy_tasks: VecDeque<NativePolicyTask>,
    policy_task_tracker: Arc<PolicyTaskTracker>,
    raster_tasks: VecDeque<NativeRasterTask>,
    raster_budget: Arc<RasterBudget>,
    scene_budget: Arc<SceneBudget>,
    critical_events: VecDeque<NativeWorkerEvent>,
    progress_events: BTreeMap<u64, NativeWorkerEvent>,
    delivered_surfaces: Vec<DeliveredSurfaceLease>,
    pending_surface_reclaims: VecDeque<NativeWorkerEvent>,
    deferred_generation_terminals: VecDeque<Correlation>,
    delivered_regions: BTreeMap<(ProtocolSessionId, u64), u32>,
    next_session_id: u64,
    next_work_id: u64,
    next_resource_id: u64,
    next_diagnostic_id: u64,
    work_history_len: usize,
}

impl NativeWorkerRegistry {
    /// Creates an empty ready Worker from one exact validated epoch configuration.
    pub fn new(config: NativeWorkerConfig) -> Result<Self, EngineIntegrationError> {
        let limits = config.limits();
        let protocol_renderer = ProtocolRendererEpoch::new(config.renderer_epoch());
        let surfaces = SurfaceOwner::new(
            config.worker(),
            config.worker_epoch(),
            protocol_renderer,
            limits.surface,
        )
        .map_err(|_| surface())?;
        let tickets =
            DataTicketLedger::new(limits.scheduler.max_sessions()).map_err(|_| protocol())?;
        let close_backlog = reserved_queue(limits.scheduler.max_sessions())?;
        let shutdown_queued_terminals = reserved_vec(limits.scheduler.max_sessions())?;
        let cancel_backlog = reserved_queue(limits.scheduler.in_flight_capacity())?;
        let normal_reentries = reserved_queue(limits.reentry_capacity)?;
        let critical_reentries = reserved_queue(limits.reentry_capacity)?;
        let lifecycle_reentries = reserved_queue(limits.lifecycle_reentry_capacity)?;
        let policy_tasks = reserved_queue(limits.reentry_capacity)?;
        let policy_task_tracker = Arc::new(PolicyTaskTracker::new(
            limits.reentry_capacity,
            limits.retained_policy_job_byte_capacity,
            config.worker_epoch(),
        ));
        let raster_tasks = reserved_queue(limits.reentry_capacity)?;
        let raster_budget = Arc::new(RasterBudget::new(
            limits.retained_raster_byte_capacity,
            config.worker_epoch(),
        ));
        let scene_budget = Arc::new(SceneBudget::new(limits.retained_scene_byte_capacity));
        let publications = reserved_queue(limits.pending_resource_capacity)?;
        let critical_events = reserved_queue(limits.critical_event_capacity)?;
        let delivered_surfaces = reserved_vec(limits.surface.max_live_surfaces())?;
        let pending_surface_reclaims = reserved_queue(limits.surface.max_live_surfaces())?;
        let deferred_generation_terminals = reserved_queue(limits.reentry_capacity)?;
        Ok(Self {
            config,
            phase: NativeWorkerPhase::Ready,
            validator: ProtocolValidator::new(limits.protocol),
            tickets,
            scheduler: ViewportScheduler::new(limits.scheduler),
            surfaces,
            sessions: BTreeMap::new(),
            closed_sessions: BTreeSet::new(),
            requests: BTreeMap::new(),
            queued_jobs: BTreeMap::new(),
            active: BTreeMap::new(),
            terminal_jobs: BTreeMap::new(),
            pending_resources: BTreeMap::new(),
            publications,
            close_backlog,
            pending_closes: BTreeMap::new(),
            pending_shutdown: None,
            shutdown_admitted: false,
            shutdown_queued_terminals,
            cancel_backlog,
            normal_reentries,
            critical_reentries,
            lifecycle_reentries,
            policy_tasks,
            policy_task_tracker,
            raster_tasks,
            raster_budget,
            scene_budget,
            critical_events,
            progress_events: BTreeMap::new(),
            delivered_surfaces,
            pending_surface_reclaims,
            deferred_generation_terminals,
            delivered_regions: BTreeMap::new(),
            next_session_id: 1,
            next_work_id: 1,
            next_resource_id: 1,
            next_diagnostic_id: 1,
            work_history_len: 0,
        })
    }

    /// Returns the current Worker lifecycle.
    pub const fn phase(&self) -> NativeWorkerPhase {
        self.phase
    }

    /// Returns the exact protocol Worker identity.
    pub const fn worker(&self) -> WorkerId {
        self.config.worker()
    }

    /// Returns the exact process Worker epoch.
    pub const fn worker_epoch(&self) -> WorkerEpoch {
        self.config.worker_epoch()
    }

    /// Returns the current lifecycle of one issued Session.
    pub fn session_phase(&self, session: ProtocolSessionId) -> Option<SessionPhase> {
        self.sessions
            .get(&session)
            .map(|entry| entry.phase)
            .or_else(|| {
                self.closed_sessions
                    .contains(&session)
                    .then_some(SessionPhase::Closed)
            })
    }

    /// Validates and admits a parser-free Open command.
    ///
    /// The actor allocates Session ownership but invokes no parser. A parser
    /// adapter must later submit [`Reentry::NeedData`] and one terminal
    /// [`Reentry::Open`].
    pub fn open(
        &mut self,
        correlation: &Correlation,
        command: &OpenCommand,
    ) -> Result<ProtocolSessionId, EngineIntegrationError> {
        if self.phase != NativeWorkerPhase::Ready {
            return Err(invalid_state());
        }
        self.validator
            .validate_correlation(MESSAGE_ID_OPEN, correlation, self.worker(), None)
            .map_err(|_| protocol())?;
        let request = correlation.request.ok_or_else(protocol)?;
        if self.requests.contains_key(&request)
            || self.requests.len() == self.config.limits().request_history_capacity
            || !valid_source(&command.source)
            || self
                .sessions
                .len()
                .checked_add(self.closed_sessions.len())
                .is_none_or(|issued| issued >= self.config.limits().scheduler.max_sessions())
        {
            return Err(invalid_identity());
        }
        let Some(scene_reservation) = self
            .scene_budget
            .try_reserve(self.config.limits().max_scene_bytes_per_open)
        else {
            return Err(backpressure());
        };
        let session_value = self.next_session_id;
        let next_session_id = session_value.checked_add(1).ok_or_else(invalid_identity)?;
        let session = ProtocolSessionId::new(session_value);
        let scheduler_session =
            SchedulerSessionId::new(session_value).ok_or_else(invalid_identity)?;
        let initial_generation = Generation::new(1).ok_or_else(invalid_identity)?;

        self.tickets
            .bind_session(self.worker(), session, &command.source)
            .map_err(|_| protocol())?;
        self.scheduler
            .register_session(scheduler_session, initial_generation)
            .map_err(|_| scheduler())?;
        self.surfaces
            .open_session(session, 1)
            .map_err(|_| surface())?;
        self.sessions.insert(
            session,
            Session {
                phase: SessionPhase::Opening,
                source: command.source.clone(),
                open_request: request,
                document_revision: None,
                scenes: Vec::new(),
                scene_reservation,
                cache: None,
                viewport_generation: None,
            },
        );
        self.requests.insert(request, RequestState::Active(session));
        self.next_session_id = next_session_id;
        Ok(session)
    }

    /// Validates a ProvideData terminal and queues only its bounded resume token.
    ///
    /// Received bytes remain owned by the lower-layer range adapter; no parser
    /// or document work executes in this command turn.
    pub fn provide_data(
        &mut self,
        correlation: &Correlation,
        command: &ProvideDataCommand,
        transfer_lengths: &[u64],
    ) -> Result<(), EngineIntegrationError> {
        let session = self.ready_or_opening_session(correlation)?;
        self.ensure_critical_reentry_space()?;
        self.ensure_lifecycle_reentry_space()?;
        self.validator
            .validate_provide_data(
                correlation,
                command,
                self.worker(),
                session,
                transfer_lengths,
            )
            .map_err(|_| protocol())?;
        let pending = self
            .tickets
            .prepare_provide_data(correlation, command)
            .map_err(|_| protocol())?;
        let ticket = pending.owner().ticket();
        let worker = self.worker();
        let worker_epoch = self.worker_epoch();
        match self.tickets.commit(pending).map_err(|_| protocol())? {
            DataTicketCommitOutcome::TicketCompleted { .. } => {
                self.critical_reentries.push_back(Reentry::RangeCompleted {
                    worker,
                    worker_epoch,
                    session,
                    ticket,
                    source_changed: false,
                });
            }
            DataTicketCommitOutcome::SessionSourceChanged { .. } => {
                self.lifecycle_reentries.push_back(Reentry::SourceChanged {
                    worker,
                    worker_epoch,
                    session,
                });
            }
        }
        Ok(())
    }

    /// Validates a FailData terminal and queues its bounded resume or
    /// fail-closed source-change transition.
    pub fn fail_data(
        &mut self,
        correlation: &Correlation,
        command: &FailDataCommand,
    ) -> Result<(), EngineIntegrationError> {
        let session = self.ready_or_opening_session(correlation)?;
        let source_changed = command.code == SourceFailureCode::SourceChanged;
        if source_changed {
            self.ensure_lifecycle_reentry_space()?;
        } else {
            self.ensure_critical_reentry_space()?;
            self.ensure_lifecycle_reentry_space()?;
        }
        let pending = self
            .tickets
            .prepare_fail_data(correlation, command)
            .map_err(|_| protocol())?;
        let ticket = pending.owner().ticket();
        let worker = self.worker();
        let worker_epoch = self.worker_epoch();
        let outcome = if source_changed {
            self.tickets.commit_source_changed(pending)
        } else {
            self.tickets.commit(pending)
        }
        .map_err(|_| protocol())?;
        match outcome {
            DataTicketCommitOutcome::TicketCompleted { .. } if !source_changed => {
                self.critical_reentries.push_back(Reentry::RangeCompleted {
                    worker,
                    worker_epoch,
                    session,
                    ticket,
                    source_changed: false,
                });
            }
            DataTicketCommitOutcome::SessionSourceChanged { .. } => {
                self.lifecycle_reentries.push_back(Reentry::SourceChanged {
                    worker,
                    worker_epoch,
                    session,
                });
            }
            DataTicketCommitOutcome::TicketCompleted { .. } => {
                return Err(identity_mismatch());
            }
        }
        Ok(())
    }

    /// Validates and admits a parser-free replaceable viewport command.
    pub fn set_viewport(
        &mut self,
        correlation: &Correlation,
        command: &SetViewportCommand,
    ) -> Result<(), EngineIntegrationError> {
        if self.phase != NativeWorkerPhase::Ready {
            return Err(invalid_state());
        }
        let session_id = correlation.session.ok_or_else(protocol)?;
        self.validator
            .validate_set_viewport(correlation, command, self.worker(), session_id)
            .map_err(|_| protocol())?;
        let session = self.sessions.get(&session_id).ok_or_else(invalid_state)?;
        let replaces_generation = session.viewport_generation.is_some();
        if session.phase != SessionPhase::Ready
            || session.document_revision != Some(command.viewport.document_revision)
            || command.viewport.visible_pages.is_empty()
            || session
                .viewport_generation
                .is_some_and(|generation| command.viewport.generation <= generation)
        {
            return Err(invalid_state());
        }
        for page in &command.viewport.visible_pages {
            let scene =
                find_scene(&session.scenes, page.page_index).ok_or_else(identity_mismatch)?;
            if page.coordinate_space != pdf_rs_protocol::PageCoordinateSpace::PdfPointsBottomLeft
                || canonical_page_geometry(scene)? != page.geometry
            {
                return Err(identity_mismatch());
            }
        }
        if replaces_generation {
            let reclaim_count = self
                .delivered_surfaces
                .iter()
                .filter(|lease| {
                    lease.correlation.session == Some(session_id)
                        && lease.generation < command.viewport.generation
                })
                .count();
            self.ensure_surface_reclaim_space(reclaim_count)?;
            let deferred_need =
                self.replacement_terminal_upper_bound(session_id, command.viewport.generation);
            if self
                .deferred_generation_terminals
                .len()
                .checked_add(deferred_need)
                .is_none_or(|required| required > self.config.limits().reentry_capacity)
            {
                return Err(backpressure());
            }
        }
        if self.work_history_len == self.config.limits().scheduler.max_work_ids_per_epoch() {
            return Err(backpressure());
        }

        let scheduler_session =
            SchedulerSessionId::new(session_id.value()).ok_or_else(invalid_identity)?;
        let cancellations = self
            .active
            .values()
            .filter(|active| {
                active.signal.session_id == scheduler_session
                    && active.signal.generation.get() < command.viewport.generation
            })
            .count();
        self.ensure_cancel_backlog_space(cancellations)?;

        let work_id = self.allocate_work_id()?;
        let generation =
            Generation::new(command.viewport.generation).ok_or_else(invalid_identity)?;
        let request = WorkRequest {
            work_id,
            session_id: scheduler_session,
            generation,
            replace_key: ReplaceKey::viewport(1),
            priority: Priority::P0,
            center_distance: Distance::new(0),
            edge_distance: Distance::new(0),
            scroll_relation: ScrollRelation::Neutral,
        };
        let outcome = self.scheduler.submit(request).map_err(|_| scheduler())?;
        self.work_history_len = self.work_history_len.checked_add(1).ok_or_else(internal)?;
        self.handle_submit_replacement(session_id, command.viewport.generation, &outcome)?;
        self.queued_jobs.insert(
            work_id,
            ViewportJob {
                correlation: correlation.clone(),
                command: command.clone(),
            },
        );
        let session = self.sessions.get_mut(&session_id).ok_or_else(internal)?;
        session.viewport_generation = Some(command.viewport.generation);
        Ok(())
    }

    /// Returns one bounded canonical page-geometry slice without parser work.
    pub fn get_page_metrics(
        &mut self,
        correlation: &Correlation,
        command: &GetPageMetricsCommand,
    ) -> Result<(), EngineIntegrationError> {
        if self.phase != NativeWorkerPhase::Ready {
            return Err(invalid_state());
        }
        let session_id = correlation.session.ok_or_else(protocol)?;
        let request = correlation.request.ok_or_else(protocol)?;
        self.validator
            .validate_correlation(
                MESSAGE_ID_GET_PAGE_METRICS,
                correlation,
                self.worker(),
                Some(session_id),
            )
            .map_err(|_| protocol())?;
        let session = self.sessions.get(&session_id).ok_or_else(invalid_state)?;
        let max_count = usize::from(command.max_count);
        let start = usize::try_from(command.start_index).map_err(|_| protocol())?;
        if session.phase != SessionPhase::Ready
            || session.document_revision != Some(command.document_revision)
            || max_count == 0
            || max_count > PAGE_METRICS_EVENT_PAGES_MAX_COUNT
            || start > session.scenes.len()
        {
            return Err(invalid_state());
        }
        if self.requests.contains_key(&request)
            || self.requests.len() == self.config.limits().request_history_capacity
        {
            return Err(invalid_identity());
        }
        if !self.ensure_event_space(1) {
            return Err(backpressure());
        }
        let end = start
            .checked_add(max_count)
            .map(|candidate| candidate.min(session.scenes.len()))
            .ok_or_else(protocol)?;
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(end - start)
            .map_err(|_| backpressure())?;
        for scene in &session.scenes[start..end] {
            pages.push(PageMetric {
                page_index: scene.binding().page_index(),
                geometry: canonical_page_geometry(scene)?,
            });
        }
        let total_pages = u32::try_from(session.scenes.len()).map_err(|_| internal())?;
        self.requests
            .insert(request, RequestState::Succeeded(session_id));
        self.emit_critical(NativeWorkerEvent::PageMetrics {
            correlation: correlation.clone(),
            event: PageMetricsEvent {
                document_revision: command.document_revision,
                start_index: command.start_index,
                total_pages,
                pages,
            },
        })
    }

    /// Validates replayable Cancel and admits it only through the critical
    /// reentry queue.
    pub fn cancel(
        &mut self,
        correlation: &Correlation,
        command: &CancelCommand,
    ) -> Result<(), EngineIntegrationError> {
        self.validator
            .validate_correlation(
                MESSAGE_ID_CANCEL,
                correlation,
                self.worker(),
                correlation.session,
            )
            .map_err(|_| protocol())?;
        if correlation.request != Some(command.target) {
            return Err(protocol());
        }
        self.enqueue_reentry(Reentry::Cancel {
            worker_epoch: self.worker_epoch(),
            correlation: correlation.clone(),
            target: command.target,
        })
        .map_err(|error| error.error())
    }

    /// Validates replayable Surface release and admits it only through the
    /// critical reentry queue.
    pub fn release_surface(
        &mut self,
        correlation: &Correlation,
        command: &ReleaseSurfaceCommand,
    ) -> Result<(), EngineIntegrationError> {
        let session = correlation.session.ok_or_else(protocol)?;
        self.validator
            .validate_correlation(
                MESSAGE_ID_RELEASE_SURFACE,
                correlation,
                self.worker(),
                Some(session),
            )
            .map_err(|_| protocol())?;
        if command.surface.value() == 0 || command.lease_token == 0 {
            return Err(protocol());
        }
        self.enqueue_reentry(Reentry::Release {
            worker_epoch: self.worker_epoch(),
            correlation: correlation.clone(),
            surface: command.surface,
            lease_token: command.lease_token,
        })
        .map_err(|error| error.error())
    }

    /// Validates replayable Session close and admits it only through the
    /// critical reentry queue.
    pub fn close_session(
        &mut self,
        correlation: &Correlation,
        _command: &CloseSessionCommand,
    ) -> Result<(), EngineIntegrationError> {
        let session = correlation.session.ok_or_else(protocol)?;
        self.validator
            .validate_correlation(
                MESSAGE_ID_CLOSE_SESSION,
                correlation,
                self.worker(),
                Some(session),
            )
            .map_err(|_| protocol())?;
        self.enqueue_reentry(Reentry::Close {
            worker_epoch: self.worker_epoch(),
            correlation: correlation.clone(),
        })
        .map_err(|error| error.error())
    }

    /// Validates replayable Worker shutdown and admits it only through the
    /// critical reentry queue.
    pub fn shutdown(
        &mut self,
        correlation: &Correlation,
        _command: &ShutdownCommand,
    ) -> Result<(), EngineIntegrationError> {
        self.validator
            .validate_correlation(MESSAGE_ID_SHUTDOWN, correlation, self.worker(), None)
            .map_err(|_| protocol())?;
        self.enqueue_reentry(Reentry::Shutdown {
            worker_epoch: self.worker_epoch(),
            correlation: correlation.clone(),
        })
        .map_err(|error| error.error())
    }

    /// Enqueues a typed parser/range/raster/lifecycle completion with ownership
    /// retained on bounded rejection.
    pub fn enqueue_reentry(&mut self, mut reentry: Reentry) -> Result<(), ReentryAdmissionError> {
        let current_identity = match &reentry {
            Reentry::Open(OpenCompletion::Ready {
                worker,
                worker_epoch,
                ..
            })
            | Reentry::Open(OpenCompletion::Failed {
                worker,
                worker_epoch,
                ..
            }) => *worker == self.worker() && *worker_epoch == self.worker_epoch(),
            Reentry::NeedData {
                worker_epoch,
                correlation,
                ..
            } => correlation.worker == self.worker() && *worker_epoch == self.worker_epoch(),
            Reentry::RangeCompleted {
                worker,
                worker_epoch,
                ..
            }
            | Reentry::SourceChanged {
                worker,
                worker_epoch,
                ..
            } => *worker == self.worker() && *worker_epoch == self.worker_epoch(),
            Reentry::CapabilityCompleted(completion) => {
                completion.worker == self.worker() && completion.worker_epoch == self.worker_epoch()
            }
            Reentry::PlanCompleted(completion) => {
                completion.worker == self.worker() && completion.worker_epoch == self.worker_epoch()
            }
            Reentry::PolicyFailed(completion) => {
                completion.worker == self.worker() && completion.worker_epoch == self.worker_epoch()
            }
            Reentry::Cancel {
                worker_epoch,
                correlation,
                ..
            }
            | Reentry::Release {
                worker_epoch,
                correlation,
                ..
            }
            | Reentry::Close {
                worker_epoch,
                correlation,
            }
            | Reentry::Shutdown {
                worker_epoch,
                correlation,
            } => correlation.worker == self.worker() && *worker_epoch == self.worker_epoch(),
            Reentry::RasterCompleted(_) | Reentry::RasterFailed(_) | Reentry::Restart { .. } => {
                true
            }
        };
        if !current_identity {
            return Err(ReentryAdmissionError::new(invalid_identity(), reentry));
        }
        let open_scenes_fit = match &reentry {
            Reentry::Open(OpenCompletion::Ready { scenes, .. }) => {
                !scenes.is_empty()
                    && scenes.len() <= self.config.limits().max_scenes_per_open
                    && retained_scene_bytes(scenes, scenes.capacity()).is_some_and(|incoming| {
                        incoming <= self.config.limits().max_scene_bytes_per_open
                    })
            }
            _ => true,
        };
        if !open_scenes_fit {
            return Err(ReentryAdmissionError::new(backpressure(), reentry));
        }
        let valid_policy_permit = match &reentry {
            Reentry::CapabilityCompleted(completion) => {
                completion.permit.belongs_to(&self.policy_task_tracker)
                    && completion.permit.matches(completion.signal)
                    && completion.permit.matches_worker_epoch(self.worker_epoch())
            }
            Reentry::PlanCompleted(completion) => {
                completion.permit.belongs_to(&self.policy_task_tracker)
                    && completion.permit.matches(completion.signal)
                    && completion.permit.matches_worker_epoch(self.worker_epoch())
            }
            Reentry::PolicyFailed(completion) => {
                completion.permit.belongs_to(&self.policy_task_tracker)
                    && completion.permit.matches(completion.signal)
                    && completion.permit.matches_worker_epoch(self.worker_epoch())
            }
            _ => true,
        };
        if !valid_policy_permit {
            return Err(ReentryAdmissionError::new(invalid_identity(), reentry));
        }
        let valid_raster_reservation = match &reentry {
            Reentry::RasterCompleted(completion) => {
                completion.reservation.belongs_to(&self.raster_budget)
                    && completion.reservation.matches(completion.signal)
                    && completion
                        .reservation
                        .matches_worker_epoch(self.worker_epoch())
                    && completion
                        .reservation
                        .covers(completion.tiles.stats().retained_bytes())
            }
            Reentry::RasterFailed(completion) => {
                completion.reservation.belongs_to(&self.raster_budget)
                    && completion.reservation.matches(completion.signal)
                    && completion
                        .reservation
                        .matches_worker_epoch(self.worker_epoch())
            }
            _ => true,
        };
        if !valid_raster_reservation {
            return Err(ReentryAdmissionError::new(invalid_identity(), reentry));
        }
        let lifecycle = matches!(
            reentry,
            Reentry::Cancel { .. }
                | Reentry::Release { .. }
                | Reentry::Close { .. }
                | Reentry::Shutdown { .. }
                | Reentry::SourceChanged { .. }
                | Reentry::Restart { .. }
        );
        let (queue, capacity) = if lifecycle {
            (
                &mut self.lifecycle_reentries,
                self.config.limits().lifecycle_reentry_capacity,
            )
        } else if matches!(reentry, Reentry::NeedData { .. }) {
            (
                &mut self.normal_reentries,
                self.config.limits().reentry_capacity,
            )
        } else {
            (
                &mut self.critical_reentries,
                self.config.limits().reentry_capacity,
            )
        };
        if queue.len() == capacity {
            return Err(ReentryAdmissionError::new(backpressure(), reentry));
        }
        match &mut reentry {
            Reentry::CapabilityCompleted(completion) => completion.permit.mark_internal(),
            Reentry::PlanCompleted(completion) => completion.permit.mark_internal(),
            Reentry::PolicyFailed(completion) => completion.permit.mark_internal(),
            Reentry::RasterCompleted(completion) => completion.reservation.mark_internal(),
            Reentry::RasterFailed(completion) => completion.reservation.mark_internal(),
            _ => {}
        }
        queue.push_back(reentry);
        Ok(())
    }

    /// Removes the next event, releasing capability and plan barriers only
    /// after the event has left Worker ownership.
    pub fn next_event(&mut self) -> Option<NativeWorkerEvent> {
        let event = self.critical_events.pop_front().or_else(|| {
            let key = self.progress_events.keys().next().copied()?;
            self.progress_events.remove(&key)
        })?;
        match &event {
            NativeWorkerEvent::SurfaceReady(publication) => {
                let metadata = &publication.event().metadata;
                debug_assert!(
                    self.delivered_surfaces.len()
                        < self.config.limits().surface.max_live_surfaces()
                );
                debug_assert!(
                    !self
                        .delivered_surfaces
                        .iter()
                        .any(|lease| lease.surface == metadata.id)
                );
                self.delivered_surfaces.push(DeliveredSurfaceLease {
                    correlation: Correlation {
                        worker: metadata.owner.worker,
                        session: Some(metadata.owner.session),
                        request: None,
                        generation: None,
                    },
                    generation: metadata.generation,
                    surface: metadata.id,
                    lease_token: metadata.lease_token,
                });
                if let Some(key) = generation_delivery_key(publication.correlation()) {
                    let delivered = self.delivered_regions.entry(key).or_default();
                    *delivered = delivered.saturating_add(1);
                }
            }
            NativeWorkerEvent::GenerationCompleted { correlation, event } => {
                if let Some(key) = generation_delivery_key(correlation) {
                    let delivered = self.delivered_regions.remove(&key).unwrap_or(0);
                    debug_assert_eq!(delivered, event.produced_regions);
                }
            }
            _ => {}
        }
        if let Some(work_value) = event.barrier_work_id()
            && let Some(work_id) = WorkId::new(work_value)
            && let Some(active) = self.active.get_mut(&work_id)
        {
            if event.is_capability_barrier() {
                let stage = std::mem::replace(&mut active.stage, PageStage::Evaluate);
                active.stage = match stage {
                    PageStage::CapabilityQueued(decision) => {
                        PageStage::CapabilityDelivered(decision)
                    }
                    other => other,
                };
            } else if event.is_progress() {
                let stage = std::mem::replace(&mut active.stage, PageStage::Evaluate);
                active.stage = match stage {
                    PageStage::PlanQueued(plan) => PageStage::PlanDelivered(plan),
                    other => other,
                };
            }
        }
        Some(event)
    }

    /// Removes the next capability/planning task for execution outside the actor.
    ///
    /// The executor can move but cannot decompose the opaque policy completion
    /// returned by the task, and must admit it through [`Self::enqueue_reentry`].
    pub fn next_policy_task(&mut self) -> Option<NativePolicyTask> {
        let mut task = self.policy_tasks.pop_front()?;
        task.mark_external();
        Some(task)
    }

    /// Removes the next Fast CPU task for execution outside the actor.
    ///
    /// Long-running raster work therefore never borrows or blocks the registry.
    /// The executor must run the task and admit its returned [`Reentry`].
    pub fn next_raster_task(&mut self) -> Option<NativeRasterTask> {
        let mut task = self.raster_tasks.pop_front()?;
        task.mark_external();
        Some(task)
    }

    /// Imports one untrusted transferred handle exactly once and copies its
    /// independently validated immutable pixel range for a platform adapter.
    ///
    /// Callers pass a cloned [`SurfaceTransfer`] from `publication.transfer()`.
    /// Cloning does not clone ownership: the Surface owner accepts only the
    /// first exact descriptor and rejects replay, foreign, stale, released, or
    /// tampered transfers.
    pub fn import_surface_bytes(
        &mut self,
        publication: &SurfacePublication,
        transfer: SurfaceTransfer,
    ) -> Result<ImportedSurfaceBytes, EngineIntegrationError> {
        self.import_surface_bytes_bounded(
            publication,
            transfer,
            self.config.limits().retained_raster_byte_capacity,
        )
    }

    /// Reclaims a Surface publication that crossed the registry event boundary
    /// but could not be represented by the platform adapter.
    ///
    /// No reclaim event is emitted because the Host never received the lease.
    /// The exact delivered-lease entry is retired together with any published
    /// or already-imported Surface storage.
    pub fn reclaim_undelivered_surface(
        &mut self,
        publication: &SurfacePublication,
    ) -> Result<(), EngineIntegrationError> {
        self.reclaim_undelivered_surface_identity(
            publication.correlation(),
            &publication.event().metadata,
        )
    }

    /// Reclaims a Surface after platform adaptation succeeded but publication
    /// to the Host failed transactionally.
    ///
    /// This is an internal-delivery rollback, not a synthesized Host release:
    /// the exact delivered lease and its published or imported storage are
    /// retired without emitting an acknowledgement or reclaim event.
    pub fn reclaim_undelivered_surface_identity(
        &mut self,
        correlation: &Correlation,
        metadata: &SurfaceMetadata,
    ) -> Result<(), EngineIntegrationError> {
        let session = correlation.session.ok_or_else(identity_mismatch)?;
        let generation = correlation.generation.ok_or_else(identity_mismatch)?;
        if correlation.worker != self.worker()
            || metadata.owner.worker != self.worker()
            || metadata.owner.session != session
            || metadata.generation != generation
        {
            return Err(identity_mismatch());
        }
        let index = self
            .delivered_surface_position(Some(session), metadata.id, metadata.lease_token)
            .ok_or_else(invalid_state)?;
        let lease = &self.delivered_surfaces[index];
        if lease.correlation.worker != correlation.worker
            || lease.correlation.session != Some(session)
            || lease.generation != generation
        {
            return Err(identity_mismatch());
        }
        let access = SurfaceAccess::new(
            self.worker(),
            session,
            self.worker_epoch(),
            metadata.id,
            metadata.lease_token,
        );
        self.surfaces.release(access).map_err(|_| surface())?;
        self.delivered_surfaces.remove(index);
        Ok(())
    }

    /// Imports one Surface while enforcing an adapter-owned allocator-capacity
    /// ceiling before the one-shot platform handle is consumed.
    ///
    /// Platform adapters use this when their destination buffer and this
    /// temporary immutable import coexist. Allocation overcapacity therefore
    /// fails before `SurfaceOwner::import`, leaving the publication available
    /// for deterministic retry or reclaim.
    pub fn import_surface_bytes_bounded(
        &mut self,
        publication: &SurfacePublication,
        transfer: SurfaceTransfer,
        max_retained_capacity: u64,
    ) -> Result<ImportedSurfaceBytes, EngineIntegrationError> {
        if max_retained_capacity == 0
            || max_retained_capacity > self.config.limits().retained_raster_byte_capacity
        {
            return Err(invalid_config());
        }
        let correlation = publication.correlation();
        let session_id = correlation.session.ok_or_else(identity_mismatch)?;
        let generation = correlation.generation.ok_or_else(identity_mismatch)?;
        let session = self.sessions.get(&session_id).ok_or_else(invalid_state)?;
        if self.phase != NativeWorkerPhase::Ready
            || session.phase != SessionPhase::Ready
            || session.viewport_generation != Some(generation)
            || correlation.worker != self.worker()
            || publication.plan().generation() != generation
            || publication.event().metadata.owner.worker != self.worker()
            || publication.event().metadata.owner.session != session_id
            || publication.event().metadata.generation != generation
            || publication.event().metadata.renderer_epoch.value() != self.config.renderer_epoch()
            || transfer.metadata != publication.event().metadata
            || transfer.transport != publication.event().transport
        {
            return Err(identity_mismatch());
        }
        let declared_length = publication.event().metadata.byte_length;
        if declared_length > self.config.limits().surface.max_total_bytes() {
            return Err(identity_mismatch());
        }
        if declared_length > max_retained_capacity {
            return Err(backpressure());
        }
        let expected_length = usize::try_from(declared_length).map_err(|_| identity_mismatch())?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(expected_length)
            .map_err(|_| backpressure())?;
        let retained_capacity = u64::try_from(bytes.capacity()).map_err(|_| backpressure())?;
        if retained_capacity > max_retained_capacity {
            return Err(backpressure());
        }
        let reservation = self
            .raster_budget
            .try_reserve_bytes(retained_capacity)
            .ok_or_else(backpressure)?;
        let context = SurfaceConsumerContext {
            worker: self.worker(),
            session: session_id,
            worker_epoch: self.worker_epoch(),
            plan: publication.plan().clone(),
        };
        let imported = self
            .surfaces
            .import(transfer, &context)
            .map_err(|_| surface())?;
        let acquired = match self.surfaces.acquire(imported, &context) {
            Ok(acquired) => acquired,
            Err(_) => {
                let _ = self.surfaces.release(imported.access());
                return Err(surface());
            }
        };
        let metadata = if acquired.metadata() == &publication.event().metadata
            && acquired.bytes().len() == expected_length
        {
            bytes.extend_from_slice(acquired.bytes());
            Some(acquired.metadata().clone())
        } else {
            None
        };
        let Some(metadata) = metadata else {
            let _ = self.surfaces.release(imported.access());
            return Err(identity_mismatch());
        };
        Ok(ImportedSurfaceBytes::new(
            correlation.clone(),
            metadata,
            publication.plan().clone(),
            bytes,
            reservation,
        ))
    }

    /// Runs at most one deterministic actor turn.
    pub fn pump(&mut self) -> Result<ActorProgress, EngineIntegrationError> {
        if let Some(progress) = self.pump_surface_lifecycle_event()? {
            return Ok(progress);
        }
        if self.phase == NativeWorkerPhase::Stopped {
            return Ok(self
                .pump_lifecycle_reentry()?
                .unwrap_or(ActorProgress::Idle));
        }
        if let Some(progress) = self.pump_cancel_backlog()? {
            return Ok(progress);
        }
        if let Some(progress) = self.pump_close_backlog()? {
            return Ok(progress);
        }
        if let Some(progress) = self.pump_shutdown_admission()? {
            return Ok(progress);
        }
        if let Some(progress) = self.pump_lifecycle_reentry()? {
            return Ok(progress);
        }
        if let Some(progress) = self.pump_critical_reentry()? {
            return Ok(progress);
        }
        if let Some(progress) = self.pump_scheduler()? {
            return Ok(progress);
        }
        if let Some(progress) = self.pump_publication()? {
            return Ok(progress);
        }
        if let Some(progress) = self.pump_active()? {
            return Ok(progress);
        }
        if let Some(progress) = self.pump_normal_reentry()? {
            return Ok(progress);
        }
        if self.phase == NativeWorkerPhase::Draining
            && self.cancel_backlog.is_empty()
            && self.policy_task_tracker.used() == 0
            && self.raster_budget.used() == 0
            && self.scheduler.try_finish_shutdown()
        {
            if !self.ensure_event_space(1) {
                return Ok(ActorProgress::Idle);
            }
            self.phase = NativeWorkerPhase::Stopped;
            let correlation = worker_correlation(self.worker());
            self.critical_events
                .push_back(NativeWorkerEvent::WorkerStopped {
                    correlation,
                    event: WorkerStoppedEvent {
                        worker: self.worker(),
                    },
                });
            return Ok(ActorProgress::Stopped);
        }
        Ok(ActorProgress::Idle)
    }

    /// Returns exact current actor and Surface resource accounting.
    pub fn resources(&self) -> NativeWorkerResources {
        let active_rasters = self
            .active
            .values()
            .map(|active| active.completed.len())
            .sum::<usize>();
        let queued_rasters = self
            .critical_reentries
            .iter()
            .filter(|reentry| matches!(reentry, Reentry::RasterCompleted(_)))
            .count();
        NativeWorkerResources::new(
            self.sessions.len(),
            self.normal_reentries.len()
                + self.critical_reentries.len()
                + self.lifecycle_reentries.len()
                + self.cancel_backlog.len()
                + self.close_backlog.len()
                + usize::from(self.pending_shutdown.is_some() && !self.shutdown_admitted),
            self.scheduler.normal_len(),
            self.scheduler.critical_len(),
            self.scheduler.in_flight_len(),
            self.policy_task_tracker.used(),
            self.policy_task_tracker.bytes_used(),
            self.pending_resources.len()
                + active_rasters
                + queued_rasters
                + self.raster_tasks.len(),
            self.raster_budget.used(),
            self.cache_resident_bytes(),
            self.scene_budget.used(),
            self.publications.len(),
            self.delivered_surfaces.len(),
            self.critical_events.len()
                + self.progress_events.len()
                + self.pending_surface_reclaims.len()
                + self.deferred_generation_terminals.len(),
            self.surfaces.current_resources(),
        )
    }

    fn ready_or_opening_session(
        &self,
        correlation: &Correlation,
    ) -> Result<ProtocolSessionId, EngineIntegrationError> {
        let session = correlation.session.ok_or_else(protocol)?;
        let state = self.sessions.get(&session).ok_or_else(invalid_state)?;
        if !matches!(state.phase, SessionPhase::Opening | SessionPhase::Ready) {
            return Err(invalid_state());
        }
        Ok(session)
    }

    fn ensure_critical_reentry_space(&self) -> Result<(), EngineIntegrationError> {
        if self.critical_reentries.len() == self.config.limits().reentry_capacity {
            Err(backpressure())
        } else {
            Ok(())
        }
    }

    fn ensure_lifecycle_reentry_space(&self) -> Result<(), EngineIntegrationError> {
        if self.lifecycle_reentries.len() == self.config.limits().lifecycle_reentry_capacity {
            Err(backpressure())
        } else {
            Ok(())
        }
    }

    fn allocate_work_id(&mut self) -> Result<WorkId, EngineIntegrationError> {
        let value = self.next_work_id;
        self.next_work_id = value.checked_add(1).ok_or_else(invalid_identity)?;
        WorkId::new(value).ok_or_else(invalid_identity)
    }

    fn allocate_resource_id(&mut self) -> Result<ResourceId, EngineIntegrationError> {
        let value = self.next_resource_id;
        self.next_resource_id = value.checked_add(1).ok_or_else(invalid_identity)?;
        ResourceId::new(value).ok_or_else(invalid_identity)
    }

    fn ensure_event_space(&self, count: usize) -> bool {
        self.critical_events
            .len()
            .checked_add(count)
            .is_some_and(|value| value <= self.config.limits().critical_event_capacity)
    }

    fn delivered_surface_position(
        &self,
        session: Option<ProtocolSessionId>,
        surface: pdf_rs_protocol::SurfaceId,
        lease_token: u64,
    ) -> Option<usize> {
        self.delivered_surfaces.iter().position(|lease| {
            lease.correlation.session == session
                && lease.surface == surface
                && lease.lease_token == lease_token
        })
    }

    fn has_delivered_surface(
        &self,
        session: Option<ProtocolSessionId>,
        surface: pdf_rs_protocol::SurfaceId,
        lease_token: u64,
    ) -> bool {
        self.delivered_surface_position(session, surface, lease_token)
            .is_some()
    }

    fn ensure_surface_reclaim_space(
        &self,
        additional: usize,
    ) -> Result<(), EngineIntegrationError> {
        if self
            .pending_surface_reclaims
            .len()
            .checked_add(additional)
            .is_none_or(|required| required > self.config.limits().surface.max_live_surfaces())
        {
            return Err(backpressure());
        }
        Ok(())
    }

    fn enqueue_delivered_surface_reclaims(
        &mut self,
        session: ProtocolSessionId,
        generations_before: Option<u64>,
        reason: SurfaceReclaimReason,
    ) {
        let pending = &mut self.pending_surface_reclaims;
        let capacity = self.config.limits().surface.max_live_surfaces();
        self.delivered_surfaces.retain(|lease| {
            let should_reclaim = lease.correlation.session == Some(session)
                && generations_before.is_none_or(|generation| lease.generation < generation);
            if should_reclaim {
                debug_assert!(pending.len() < capacity);
                pending.push_back(NativeWorkerEvent::SurfaceReclaimed {
                    correlation: lease.correlation.clone(),
                    event: SurfaceReclaimedEvent {
                        surface: lease.surface,
                        lease_token: lease.lease_token,
                        reason,
                    },
                });
            }
            !should_reclaim
        });
    }

    fn replacement_terminal_upper_bound(
        &self,
        session: ProtocolSessionId,
        generation: u64,
    ) -> usize {
        let is_older = |correlation: &Correlation| {
            correlation.session == Some(session)
                && correlation
                    .generation
                    .is_some_and(|candidate| candidate < generation)
        };
        self.queued_jobs
            .values()
            .filter(|job| is_older(&job.correlation))
            .count()
            .saturating_add(
                self.active
                    .values()
                    .filter(|active| is_older(&active.job.correlation))
                    .count(),
            )
            .saturating_add(
                self.terminal_jobs
                    .values()
                    .filter(|terminal| is_older(&terminal.correlation))
                    .count(),
            )
            .saturating_add(
                self.pending_resources
                    .values()
                    .filter(|resource| is_older(&resource.correlation))
                    .count(),
            )
            .saturating_add(
                self.publications
                    .iter()
                    .filter(|batch| is_older(&batch.correlation))
                    .count(),
            )
            .saturating_add(
                self.critical_events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            NativeWorkerEvent::GenerationCompleted { correlation, .. }
                                if is_older(correlation)
                        )
                    })
                    .count(),
            )
    }

    fn emit_critical(&mut self, event: NativeWorkerEvent) -> Result<(), EngineIntegrationError> {
        if !self.ensure_event_space(1) {
            return Err(backpressure());
        }
        self.critical_events.push_back(event);
        Ok(())
    }

    fn emit_progress(
        &mut self,
        work_id: WorkId,
        event: NativeWorkerEvent,
    ) -> Result<(), EngineIntegrationError> {
        if !self.ensure_progress_space(work_id) {
            return Err(backpressure());
        }
        self.progress_events.insert(work_id.get(), event);
        Ok(())
    }

    fn ensure_progress_space(&self, work_id: WorkId) -> bool {
        self.progress_events.contains_key(&work_id.get())
            || self.progress_events.len() < self.config.limits().progress_event_capacity
    }

    fn handle_submit_replacement(
        &mut self,
        protocol_session: ProtocolSessionId,
        generation: u64,
        outcome: &SubmitOutcome,
    ) -> Result<(), EngineIntegrationError> {
        let (replaced, advance) = match outcome {
            SubmitOutcome::Enqueued { generation_advance } => (None, generation_advance.as_ref()),
            SubmitOutcome::Coalesced {
                replaced_work_id,
                generation_advance,
                ..
            } => (Some(*replaced_work_id), generation_advance.as_ref()),
        };
        if let Some(replaced) = replaced {
            self.queued_jobs.remove(&replaced);
        }
        if let Some(advance) = advance {
            let mut superseded_correlations = Vec::new();
            for superseded in &advance.superseded_queued {
                if let Some(job) = self.queued_jobs.remove(superseded) {
                    push_unique_correlation(&mut superseded_correlations, job.correlation);
                }
            }
            for correlation in self.invalidate_older_work(protocol_session, generation)? {
                push_unique_correlation(&mut superseded_correlations, correlation);
            }
            self.surfaces
                .replace_generation(protocol_session, generation)
                .map_err(|_| surface())?;
            self.enqueue_delivered_surface_reclaims(
                protocol_session,
                Some(generation),
                SurfaceReclaimReason::GenerationReplaced,
            );
            for correlation in superseded_correlations {
                debug_assert!(
                    self.deferred_generation_terminals.len()
                        < self.config.limits().reentry_capacity
                );
                self.deferred_generation_terminals.push_back(correlation);
            }
        }
        Ok(())
    }

    fn invalidate_older_work(
        &mut self,
        session: ProtocolSessionId,
        generation: u64,
    ) -> Result<Vec<Correlation>, EngineIntegrationError> {
        let scheduler_session =
            SchedulerSessionId::new(session.value()).ok_or_else(invalid_identity)?;
        let active_ids = self
            .active
            .iter()
            .filter_map(|(work_id, active)| {
                (active.signal.session_id == scheduler_session
                    && active.signal.generation.get() < generation)
                    .then_some(*work_id)
            })
            .collect::<Vec<_>>();
        for work_id in active_ids {
            if let Some(active) = self.active.remove(&work_id) {
                active.cancel_work();
                self.terminal_jobs.insert(
                    work_id,
                    TerminalJob {
                        signal: active.signal,
                        correlation: active.job.correlation,
                        failure: EngineErrorCode::StaleGeneration,
                    },
                );
                self.push_cancel(active.signal);
            }
        }
        self.purge_generation_reentries(session, generation);
        let mut superseded = Vec::new();
        self.pending_resources.retain(|_, resource| {
            let keep = resource.signal.session_id != scheduler_session
                || resource.signal.generation.get() >= generation;
            if !keep {
                push_unique_correlation(&mut superseded, resource.correlation.clone());
            }
            keep
        });
        self.publications.retain(|batch| {
            let keep = batch.correlation.session != Some(session)
                || batch
                    .correlation
                    .generation
                    .is_some_and(|value| value >= generation);
            if !keep {
                push_unique_correlation(&mut superseded, batch.correlation.clone());
            }
            keep
        });
        for event in &self.critical_events {
            if let NativeWorkerEvent::GenerationCompleted { correlation, .. } = event
                && correlation.session == Some(session)
                && correlation
                    .generation
                    .is_some_and(|value| value < generation)
            {
                push_unique_correlation(&mut superseded, correlation.clone());
            }
        }
        self.purge_events(session, Some(generation));
        Ok(superseded)
    }

    fn ensure_cancel_backlog_space(&self, additional: usize) -> Result<(), EngineIntegrationError> {
        if self
            .cancel_backlog
            .len()
            .checked_add(additional)
            .is_none_or(|required| required > self.config.limits().scheduler.in_flight_capacity())
        {
            return Err(backpressure());
        }
        Ok(())
    }

    fn push_cancel(&mut self, signal: TerminalSignal) {
        if !self.cancel_backlog.contains(&signal) {
            debug_assert!(
                self.cancel_backlog.len() < self.config.limits().scheduler.in_flight_capacity()
            );
            self.cancel_backlog.push_back(signal);
        }
    }

    fn purge_events(&mut self, session: ProtocolSessionId, keep_generation: Option<u64>) {
        let delivered_regions = &self.delivered_regions;
        for event in &mut self.critical_events {
            let purged_generation = event.session() == Some(session)
                && keep_generation.is_none_or(|generation| {
                    event.generation().is_none_or(|value| value < generation)
                });
            if purged_generation
                && keep_generation.is_none()
                && let NativeWorkerEvent::GenerationCompleted { correlation, event } = event
            {
                event.produced_regions = generation_delivery_key(correlation)
                    .and_then(|key| delivered_regions.get(&key).copied())
                    .unwrap_or(0);
            }
        }
        self.critical_events.retain(|event| {
            let replaced_generation_terminal = keep_generation.is_some()
                && event.session() == Some(session)
                && event.generation().is_some_and(|value| {
                    keep_generation.is_some_and(|generation| value < generation)
                })
                && matches!(event, NativeWorkerEvent::GenerationCompleted { .. });
            !replaced_generation_terminal
                && (event.is_delivery_terminal()
                    || event.session() != Some(session)
                    || keep_generation.is_some_and(|generation| {
                        event.generation().is_some_and(|value| value >= generation)
                    }))
        });
        self.progress_events.retain(|_, event| {
            event.session() != Some(session)
                || keep_generation.is_some_and(|generation| {
                    event.generation().is_some_and(|value| value >= generation)
                })
        });
    }

    fn purge_generation_reentries(&mut self, session: ProtocolSessionId, generation: u64) {
        self.critical_reentries.retain(|reentry| match reentry {
            Reentry::CapabilityCompleted(completion) => {
                completion.signal.session_id.get() != session.value()
                    || completion.signal.generation.get() >= generation
            }
            Reentry::PlanCompleted(completion) => {
                completion.signal.session_id.get() != session.value()
                    || completion.signal.generation.get() >= generation
            }
            Reentry::PolicyFailed(completion) => {
                completion.signal.session_id.get() != session.value()
                    || completion.signal.generation.get() >= generation
            }
            Reentry::RasterCompleted(completion) => {
                completion.signal.session_id.get() != session.value()
                    || completion.signal.generation.get() >= generation
            }
            Reentry::RasterFailed(completion) => {
                completion.signal.session_id.get() != session.value()
                    || completion.signal.generation.get() >= generation
            }
            _ => true,
        });
        self.policy_tasks.retain(|task| {
            task.signal().session_id.get() != session.value()
                || task.signal().generation.get() >= generation
        });
        self.raster_tasks.retain(|task| {
            task.signal().session_id.get() != session.value()
                || task.signal().generation.get() >= generation
        });
    }

    fn purge_session_work_reentries(&mut self, session: ProtocolSessionId) {
        self.normal_reentries.retain(|reentry| match reentry {
            Reentry::NeedData { correlation, .. } => correlation.session != Some(session),
            _ => true,
        });
        self.critical_reentries.retain(|reentry| match reentry {
            Reentry::Open(OpenCompletion::Ready { session: owner, .. })
            | Reentry::Open(OpenCompletion::Failed { session: owner, .. })
            | Reentry::RangeCompleted { session: owner, .. }
            | Reentry::SourceChanged { session: owner, .. } => *owner != session,
            Reentry::CapabilityCompleted(completion) => {
                completion.signal.session_id.get() != session.value()
            }
            Reentry::PlanCompleted(completion) => {
                completion.signal.session_id.get() != session.value()
            }
            Reentry::PolicyFailed(completion) => {
                completion.signal.session_id.get() != session.value()
            }
            Reentry::RasterCompleted(completion) => {
                completion.signal.session_id.get() != session.value()
            }
            Reentry::RasterFailed(completion) => {
                completion.signal.session_id.get() != session.value()
            }
            Reentry::NeedData { .. }
            | Reentry::Cancel { .. }
            | Reentry::Release { .. }
            | Reentry::Close { .. }
            | Reentry::Shutdown { .. }
            | Reentry::Restart { .. } => true,
        });
        self.policy_tasks
            .retain(|task| task.signal().session_id.get() != session.value());
        self.raster_tasks
            .retain(|task| task.signal().session_id.get() != session.value());
    }

    fn pump_surface_lifecycle_event(
        &mut self,
    ) -> Result<Option<ActorProgress>, EngineIntegrationError> {
        if let Some(event) = self.pending_surface_reclaims.front() {
            if !matches!(event, NativeWorkerEvent::SurfaceReclaimed { .. }) {
                return Err(internal());
            }
            if !self.ensure_event_space(1) {
                return Ok(Some(ActorProgress::Idle));
            }
            let event = self
                .pending_surface_reclaims
                .pop_front()
                .ok_or_else(internal)?;
            self.critical_events.push_back(event);
            return Ok(Some(ActorProgress::Lifecycle));
        }
        if let Some(correlation) = self.deferred_generation_terminals.front().cloned() {
            if !self.ensure_event_space(1) {
                return Ok(Some(ActorProgress::Idle));
            }
            self.emit_superseded_generation(correlation)?;
            self.deferred_generation_terminals.pop_front();
            return Ok(Some(ActorProgress::Lifecycle));
        }
        Ok(None)
    }

    fn pump_cancel_backlog(&mut self) -> Result<Option<ActorProgress>, EngineIntegrationError> {
        let Some(signal) = self.cancel_backlog.front().copied() else {
            return Ok(None);
        };
        if self.scheduler.critical_len() == self.config.limits().scheduler.critical_capacity() {
            return Ok(None);
        }
        self.scheduler
            .enqueue_cancel(signal)
            .map_err(|_| scheduler())?;
        self.cancel_backlog.pop_front();
        Ok(Some(ActorProgress::Lifecycle))
    }

    fn pump_lifecycle_reentry(&mut self) -> Result<Option<ActorProgress>, EngineIntegrationError> {
        let Some(front) = self.lifecycle_reentries.front() else {
            return Ok(None);
        };
        if matches!(front, Reentry::Restart { .. })
            && (self.phase == NativeWorkerPhase::Draining
                || self
                    .critical_events
                    .iter()
                    .any(|event| matches!(event, NativeWorkerEvent::SurfaceReclaimed { .. }))
                || (self.phase == NativeWorkerPhase::Stopped
                    && self
                        .critical_events
                        .iter()
                        .any(|event| matches!(event, NativeWorkerEvent::WorkerStopped { .. }))))
        {
            return Ok(None);
        }
        let event_need = match front {
            Reentry::Cancel { .. } => 2,
            Reentry::Release {
                correlation,
                surface,
                lease_token,
                ..
            } => {
                1 + usize::from(self.has_delivered_surface(
                    correlation.session,
                    *surface,
                    *lease_token,
                ))
            }
            Reentry::Close { .. } | Reentry::Shutdown { .. } => 1,
            Reentry::SourceChanged { .. } | Reentry::Restart { .. } => 0,
            Reentry::Open(_)
            | Reentry::NeedData { .. }
            | Reentry::RangeCompleted { .. }
            | Reentry::CapabilityCompleted(_)
            | Reentry::PlanCompleted(_)
            | Reentry::PolicyFailed(_)
            | Reentry::RasterCompleted(_)
            | Reentry::RasterFailed(_) => return Err(internal()),
        };
        if !self.ensure_event_space(event_need) {
            return Ok(None);
        }
        let reentry = self.lifecycle_reentries.pop_front().ok_or_else(internal)?;
        self.process_reentry(reentry)?;
        Ok(Some(ActorProgress::Reentry))
    }

    fn pump_critical_reentry(&mut self) -> Result<Option<ActorProgress>, EngineIntegrationError> {
        let Some(front) = self.critical_reentries.front() else {
            return Ok(None);
        };
        if let Reentry::RasterCompleted(completion) = front {
            match self.raster_completion_needs_resource(completion.signal)? {
                None => {
                    self.critical_reentries.pop_front();
                    return Ok(Some(ActorProgress::Reentry));
                }
                Some(true)
                    if self.pending_resources.len() + self.publications.len()
                        == self.config.limits().pending_resource_capacity =>
                {
                    return Ok(None);
                }
                Some(true) | Some(false) => {}
            }
        }
        if let Reentry::RasterFailed(completion) = front
            && !self.active.contains_key(&completion.signal.work_id)
        {
            self.critical_reentries.pop_front();
            return Ok(Some(ActorProgress::Reentry));
        }
        let inactive_policy_result = match front {
            Reentry::CapabilityCompleted(completion) => {
                !self.active.contains_key(&completion.signal.work_id)
            }
            Reentry::PlanCompleted(completion) => {
                !self.active.contains_key(&completion.signal.work_id)
            }
            Reentry::PolicyFailed(completion) => {
                !self.active.contains_key(&completion.signal.work_id)
            }
            _ => false,
        };
        if inactive_policy_result {
            self.critical_reentries.pop_front();
            return Ok(Some(ActorProgress::Reentry));
        }
        if let Reentry::PlanCompleted(completion) = front
            && !self.ensure_progress_space(completion.signal.work_id)
        {
            // Keep the opaque completion and its permit queued until the
            // coalescing progress queue can accept this work item.
            return Ok(None);
        }
        let needs_scheduler_slot = matches!(
            front,
            Reentry::CapabilityCompleted(_)
                | Reentry::PlanCompleted(_)
                | Reentry::PolicyFailed(_)
                | Reentry::RasterCompleted(_)
                | Reentry::RasterFailed(_)
        );
        if needs_scheduler_slot
            && self.scheduler.critical_len() == self.config.limits().scheduler.critical_capacity()
        {
            return Ok(None);
        }
        let event_need = match front {
            Reentry::Open(OpenCompletion::Ready { .. })
            | Reentry::Open(OpenCompletion::Failed { .. })
            | Reentry::CapabilityCompleted(_) => 1,
            Reentry::RangeCompleted { .. }
            | Reentry::PlanCompleted(_)
            | Reentry::PolicyFailed(_)
            | Reentry::RasterCompleted(_)
            | Reentry::RasterFailed(_) => 0,
            Reentry::NeedData { .. }
            | Reentry::Cancel { .. }
            | Reentry::Release { .. }
            | Reentry::Close { .. }
            | Reentry::Shutdown { .. }
            | Reentry::SourceChanged { .. }
            | Reentry::Restart { .. } => return Err(internal()),
        };
        if !self.ensure_event_space(event_need) {
            return Ok(None);
        }
        let reentry = self.critical_reentries.pop_front().ok_or_else(internal)?;
        self.process_reentry(reentry)?;
        Ok(Some(ActorProgress::Reentry))
    }

    fn pump_normal_reentry(&mut self) -> Result<Option<ActorProgress>, EngineIntegrationError> {
        let Some(front) = self.normal_reentries.front() else {
            return Ok(None);
        };
        if !self.ensure_event_space(1) {
            return Ok(None);
        }
        if !matches!(front, Reentry::NeedData { .. }) {
            return Err(internal());
        }
        let reentry = self.normal_reentries.pop_front().ok_or_else(internal)?;
        self.process_reentry(reentry)?;
        Ok(Some(ActorProgress::Reentry))
    }

    fn process_reentry(&mut self, reentry: Reentry) -> Result<(), EngineIntegrationError> {
        match reentry {
            Reentry::Open(completion) => self.process_open_completion(completion),
            Reentry::NeedData {
                worker_epoch,
                correlation,
                event,
            } => {
                if correlation.worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                let session = correlation.session.ok_or_else(protocol)?;
                let claimed_request = correlation.request.ok_or_else(protocol)?;
                let Some(state) = self.sessions.get(&session) else {
                    return self.fail_open_request(
                        session,
                        claimed_request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                };
                if state.phase != SessionPhase::Opening {
                    return Ok(());
                }
                let open_request = state.open_request;
                if correlation.worker != self.worker()
                    || correlation.request != Some(open_request)
                    || self
                        .tickets
                        .register_need_data(&correlation, &event)
                        .is_err()
                {
                    return self.fail_open_request(
                        session,
                        open_request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                }
                self.emit_critical(NativeWorkerEvent::NeedData { correlation, event })
            }
            Reentry::RangeCompleted {
                worker,
                worker_epoch,
                session,
                source_changed,
                ..
            } => {
                if worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                if source_changed {
                    self.begin_session_close(session, None, SessionCloseReason::SourceChanged)?;
                } else if !self.sessions.contains_key(&session)
                    && !self.closed_sessions.contains(&session)
                {
                    return Err(invalid_state());
                }
                Ok(())
            }
            Reentry::CapabilityCompleted(completion) => {
                let crate::NativeCapabilityCompletion {
                    worker,
                    worker_epoch,
                    signal,
                    decision,
                    event,
                    permit,
                } = completion;
                if worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                let Some(active) = self.active.get(&signal.work_id) else {
                    return Ok(());
                };
                if active.signal != signal
                    || !matches!(active.stage, PageStage::CapabilityPending { .. })
                {
                    return self
                        .fail_active(
                            signal.work_id,
                            ActorProgress::Capability,
                            EngineErrorCode::Internal,
                        )
                        .map(|_| ());
                }
                let correlation = active.job.correlation.clone();
                self.emit_critical(NativeWorkerEvent::CapabilityReported {
                    correlation,
                    event,
                    work_id: signal.work_id.get(),
                })?;
                self.active
                    .get_mut(&signal.work_id)
                    .ok_or_else(internal)?
                    .stage = PageStage::CapabilityQueued(CapabilityStage { decision, permit });
                Ok(())
            }
            Reentry::PlanCompleted(completion) => {
                let crate::NativePlanCompletion {
                    worker,
                    worker_epoch,
                    signal,
                    plan,
                    event,
                    permit,
                } = completion;
                if worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                let Some(active) = self.active.get(&signal.work_id) else {
                    return Ok(());
                };
                let expected_generation = active.job.command.viewport.generation;
                let Some(renderer_epoch) = PolicyRendererEpoch::new(self.config.renderer_epoch())
                else {
                    return Err(internal());
                };
                if active.signal != signal
                    || !matches!(active.stage, PageStage::PlanPending { .. })
                    || plan.config().backend() != NativeBackend::FastCpu
                    || plan.renderer_epoch() != renderer_epoch
                    || plan.viewport().generation() != expected_generation
                {
                    return self
                        .fail_active(
                            signal.work_id,
                            ActorProgress::Capability,
                            EngineErrorCode::Internal,
                        )
                        .map(|_| ());
                }
                let correlation = active.job.correlation.clone();
                self.emit_progress(
                    signal.work_id,
                    NativeWorkerEvent::GenerationPlanned {
                        correlation,
                        event,
                        work_id: signal.work_id.get(),
                    },
                )?;
                self.active
                    .get_mut(&signal.work_id)
                    .ok_or_else(internal)?
                    .stage = PageStage::PlanQueued(PlanStage { plan, permit });
                Ok(())
            }
            Reentry::PolicyFailed(completion) => {
                let worker = completion.worker;
                let worker_epoch = completion.worker_epoch;
                let signal = completion.signal;
                if worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                let Some(active) = self.active.get(&signal.work_id) else {
                    return Ok(());
                };
                if active.signal != signal
                    || !matches!(
                        active.stage,
                        PageStage::CapabilityPending { .. } | PageStage::PlanPending { .. }
                    )
                {
                    return self
                        .fail_active(
                            signal.work_id,
                            ActorProgress::Capability,
                            EngineErrorCode::Internal,
                        )
                        .map(|_| ());
                }
                self.fail_active(
                    signal.work_id,
                    ActorProgress::Capability,
                    completion.failure,
                )
                .map(|_| ())
            }
            Reentry::RasterCompleted(completion) => self.process_raster_completed(
                completion.signal,
                completion.tiles,
                completion.reservation,
            ),
            Reentry::RasterFailed(completion) => {
                let signal = completion.signal;
                if let Some(active) = self.active.remove(&signal.work_id) {
                    let failure = if active.signal == signal {
                        active.failure.unwrap_or(completion.failure)
                    } else {
                        EngineErrorCode::Internal
                    };
                    self.terminal_jobs.insert(
                        signal.work_id,
                        TerminalJob {
                            signal: active.signal,
                            correlation: active.job.correlation,
                            failure,
                        },
                    );
                    self.scheduler
                        .enqueue_failure(active.signal)
                        .map_err(|_| scheduler())?;
                } else {
                    return Ok(());
                }
                Ok(())
            }
            Reentry::Cancel {
                worker_epoch,
                correlation,
                target,
            } => {
                if correlation.worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                self.process_cancel(correlation, target)
            }
            Reentry::Release {
                worker_epoch,
                correlation,
                surface: surface_id,
                lease_token,
            } => {
                if correlation.worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                self.process_release(correlation, surface_id, lease_token)
            }
            Reentry::Close {
                worker_epoch,
                correlation,
            } => {
                if correlation.worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                let session = correlation.session.ok_or_else(protocol)?;
                self.begin_session_close(session, Some(correlation), SessionCloseReason::Explicit)
            }
            Reentry::Shutdown {
                worker_epoch,
                correlation,
            } => {
                if correlation.worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                self.begin_shutdown(correlation)
            }
            Reentry::SourceChanged {
                worker,
                worker_epoch,
                session,
            } => {
                if worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                self.begin_session_close(session, None, SessionCloseReason::SourceChanged)
            }
            Reentry::Restart { config } => self.restart_now(config),
        }
    }

    fn process_open_completion(
        &mut self,
        completion: OpenCompletion,
    ) -> Result<(), EngineIntegrationError> {
        match completion {
            OpenCompletion::Ready {
                worker,
                worker_epoch,
                session,
                request,
                document_revision,
                mut scenes,
            } => {
                if worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                let Some(state) = self.sessions.get(&session) else {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                };
                if state.phase != SessionPhase::Opening {
                    return Ok(());
                }
                let source_descriptor = state.source.clone();
                if state.open_request != request
                    || self.requests.get(&request) != Some(&RequestState::Active(session))
                {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                }
                if document_revision == 0
                    || scenes.is_empty()
                    || scenes.len() > self.config.limits().max_scenes_per_open
                {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                }
                let Some(incoming_scene_bytes) = retained_scene_bytes(&scenes, scenes.capacity())
                else {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                };
                if !state.scene_reservation.covers(incoming_scene_bytes) {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::ResourceLimit,
                        SessionCloseReason::Internal,
                    );
                }
                let first = scenes
                    .first()
                    .expect("nonempty Open completion was checked before indexing");
                let source = first.binding().source();
                let revision_startxref = first.binding().revision_startxref();
                if !source_matches(&source_descriptor, source) {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                }
                for scene in &scenes {
                    if !source_matches(&source_descriptor, scene.binding().source())
                        || scene.binding().revision_startxref() != revision_startxref
                        || canonical_page_geometry(scene).is_err()
                    {
                        return self.fail_open_request(
                            session,
                            request,
                            EngineErrorCode::Internal,
                            SessionCloseReason::Internal,
                        );
                    }
                }
                scenes.sort_unstable_by_key(|scene| scene.binding().page_index());
                if scenes.iter().enumerate().any(|(index, scene)| {
                    u32::try_from(index).ok() != Some(scene.binding().page_index())
                }) {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                }
                let Some(policy_renderer) = PolicyRendererEpoch::new(self.config.renderer_epoch())
                else {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                };
                let binding = TileCacheBinding::new(
                    TileCacheOwnerId::new(self.worker().value()),
                    TileCacheSessionId::new(session.value()),
                    source,
                    document_revision,
                    revision_startxref,
                    policy_renderer,
                );
                let cache = match TileCache::new(binding, self.config.limits().cache) {
                    Ok(cache) => cache,
                    Err(_) => {
                        return self.fail_open_request(
                            session,
                            request,
                            EngineErrorCode::Internal,
                            SessionCloseReason::Internal,
                        );
                    }
                };
                let cache_resident_bytes = cache.stats().resident_bytes();
                if self
                    .cache_resident_bytes()
                    .checked_add(cache_resident_bytes)
                    .is_none_or(|resident| {
                        resident > self.config.limits().retained_cache_byte_capacity
                    })
                {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::ResourceLimit,
                        SessionCloseReason::Internal,
                    );
                }
                let page_count = match u32::try_from(scenes.len()) {
                    Ok(page_count) => page_count,
                    Err(_) => {
                        return self.fail_open_request(
                            session,
                            request,
                            EngineErrorCode::Internal,
                            SessionCloseReason::Internal,
                        );
                    }
                };
                let state = self.sessions.get_mut(&session).ok_or_else(internal)?;
                state.phase = SessionPhase::Ready;
                state.document_revision = Some(document_revision);
                state.scenes = scenes;
                state.scene_reservation.shrink_to(incoming_scene_bytes);
                state.cache = Some(cache);
                self.requests
                    .insert(request, RequestState::Succeeded(session));
                let profile = CapabilityProfile::m3_reference_v1();
                let correlation = Correlation {
                    worker: self.worker(),
                    session: Some(session),
                    request: Some(request),
                    generation: None,
                };
                self.emit_critical(NativeWorkerEvent::DocumentReady {
                    correlation,
                    event: DocumentReadyEvent {
                        session,
                        document_revision,
                        page_count,
                        profile: CapabilityProfileId::BaselineNative,
                        policy_version: profile.policy_version(),
                    },
                })
            }
            OpenCompletion::Failed {
                worker,
                worker_epoch,
                session,
                request,
            } => {
                if worker != self.worker() || worker_epoch != self.worker_epoch() {
                    return Ok(());
                }
                let Some(state) = self.sessions.get(&session) else {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                };
                if state.phase != SessionPhase::Opening {
                    return Ok(());
                }
                if state.open_request != request
                    || self.requests.get(&request) != Some(&RequestState::Active(session))
                {
                    return self.fail_open_request(
                        session,
                        request,
                        EngineErrorCode::Internal,
                        SessionCloseReason::Internal,
                    );
                }
                self.requests.insert(request, RequestState::Failed(session));
                let correlation = Correlation {
                    worker: self.worker(),
                    session: Some(session),
                    request: Some(request),
                    generation: None,
                };
                let error = self.protocol_engine_error(EngineErrorCode::InvalidDocument)?;
                self.begin_session_close(session, None, SessionCloseReason::OpenFailed)?;
                self.emit_critical(NativeWorkerEvent::RequestFailed {
                    correlation,
                    event: RequestFailedEvent { error },
                })
            }
        }
    }

    fn fail_open_request(
        &mut self,
        claimed_session: ProtocolSessionId,
        claimed_request: RequestId,
        code: EngineErrorCode,
        reason: SessionCloseReason,
    ) -> Result<(), EngineIntegrationError> {
        let claimed_target = self.sessions.get(&claimed_session).and_then(|state| {
            (state.phase == SessionPhase::Opening
                && self.requests.get(&state.open_request)
                    == Some(&RequestState::Active(claimed_session)))
            .then_some((claimed_session, state.open_request))
        });
        let (session, request) = match claimed_target {
            Some(target) => target,
            None => match self.requests.get(&claimed_request).copied() {
                Some(RequestState::Active(session)) => (session, claimed_request),
                _ => return Ok(()),
            },
        };
        let Some(state) = self.sessions.get(&session) else {
            return Ok(());
        };
        if state.phase != SessionPhase::Opening {
            return Ok(());
        }
        if state.open_request != request
            || self.requests.get(&request) != Some(&RequestState::Active(session))
        {
            return Ok(());
        }
        self.requests.insert(request, RequestState::Failed(session));
        let correlation = Correlation {
            worker: self.worker(),
            session: Some(session),
            request: Some(request),
            generation: None,
        };
        let error = self.protocol_engine_error(code)?;
        self.begin_session_close(session, None, reason)?;
        self.emit_critical(NativeWorkerEvent::RequestFailed {
            correlation,
            event: RequestFailedEvent { error },
        })
    }

    fn process_raster_completed(
        &mut self,
        signal: TerminalSignal,
        tiles: FastTileSet,
        reservation: NativeRasterReservation,
    ) -> Result<(), EngineIntegrationError> {
        let Some(active) = self.active.get(&signal.work_id) else {
            return Ok(());
        };
        if active.signal != signal
            || !matches!(
                &active.stage,
                PageStage::RasterPending { plan, .. } if plan.hash() == tiles.plan_hash()
            )
        {
            return self.fail_raster_work(signal.work_id, EngineErrorCode::Internal);
        }
        let active = self.active.get_mut(&signal.work_id).ok_or_else(internal)?;
        let stage = std::mem::replace(&mut active.stage, PageStage::Evaluate);
        let plan = match stage {
            PageStage::RasterPending { plan, .. } if plan.hash() == tiles.plan_hash() => plan,
            _ => unreachable!("raster stage identity was validated before mutation"),
        };
        self.commit_completed_plan(
            signal,
            CompletedPlan {
                plan,
                tiles: CompletedTiles::Raster {
                    tiles,
                    _reservation: reservation,
                },
            },
        )
    }

    fn commit_completed_plan(
        &mut self,
        signal: TerminalSignal,
        completed: CompletedPlan,
    ) -> Result<(), EngineIntegrationError> {
        let Some(active) = self.active.get(&signal.work_id) else {
            return Ok(());
        };
        let Some(next_page) = active.page_cursor.checked_add(1) else {
            return self.fail_raster_work(signal.work_id, EngineErrorCode::Internal);
        };
        let final_page = next_page == active.job.command.viewport.visible_pages.len();
        if next_page > active.job.command.viewport.visible_pages.len() {
            return self.fail_raster_work(signal.work_id, EngineErrorCode::Internal);
        }
        let needs_resource = final_page && active.failure.is_none();
        if needs_resource
            && self.pending_resources.len() + self.publications.len()
                == self.config.limits().pending_resource_capacity
        {
            return Err(backpressure());
        }
        let resource_id = needs_resource
            .then(|| self.allocate_resource_id())
            .transpose()?;

        let active = self.active.get_mut(&signal.work_id).ok_or_else(internal)?;
        active.completed.push(completed);
        active.page_cursor = next_page;
        if !final_page {
            active.stage = PageStage::Evaluate;
            return Ok(());
        }

        let active = self.active.remove(&signal.work_id).ok_or_else(internal)?;
        if let Some(failure) = active.failure {
            self.terminal_jobs.insert(
                signal.work_id,
                TerminalJob {
                    signal,
                    correlation: active.job.correlation,
                    failure,
                },
            );
            self.scheduler
                .enqueue_failure(signal)
                .map_err(|_| scheduler())?;
            return Ok(());
        }
        let resource_id = resource_id.ok_or_else(internal)?;
        self.pending_resources.insert(
            resource_id,
            CompletedViewport {
                signal,
                correlation: active.job.correlation,
                plans: active.completed,
            },
        );
        self.scheduler
            .enqueue_completion(signal, resource_id)
            .map_err(|_| scheduler())?;
        Ok(())
    }

    fn fail_raster_work(
        &mut self,
        work_id: WorkId,
        failure: EngineErrorCode,
    ) -> Result<(), EngineIntegrationError> {
        let Some(active) = self.active.remove(&work_id) else {
            return Ok(());
        };
        self.terminal_jobs.insert(
            work_id,
            TerminalJob {
                signal: active.signal,
                correlation: active.job.correlation,
                failure,
            },
        );
        self.scheduler
            .enqueue_failure(active.signal)
            .map_err(|_| scheduler())?;
        Ok(())
    }

    fn raster_completion_needs_resource(
        &self,
        signal: TerminalSignal,
    ) -> Result<Option<bool>, EngineIntegrationError> {
        let Some(active) = self.active.get(&signal.work_id) else {
            return Ok(None);
        };
        if active.signal != signal {
            return Ok(Some(false));
        }
        Ok(Some(
            active.failure.is_none()
                && active
                    .page_cursor
                    .checked_add(1)
                    .is_some_and(|next| next == active.job.command.viewport.visible_pages.len()),
        ))
    }

    fn process_cancel(
        &mut self,
        correlation: Correlation,
        target: RequestId,
    ) -> Result<(), EngineIntegrationError> {
        let state = self.requests.get(&target).copied();
        let owner = state.map(request_state_session);
        let session_matches = correlation
            .session
            .is_none_or(|session| owner == Some(session));
        let (status, cancelled_session) = match state {
            Some(_) if !session_matches => (OperationAckStatus::UnknownTarget, None),
            Some(RequestState::Active(session)) if self.session_close_reason(session).is_some() => {
                (OperationAckStatus::AlreadyTerminal, None)
            }
            Some(RequestState::Active(session)) => {
                self.requests
                    .insert(target, RequestState::Cancelled(session));
                (OperationAckStatus::Applied, Some(session))
            }
            Some(_) => (OperationAckStatus::AlreadyTerminal, None),
            None => (OperationAckStatus::UnknownTarget, None),
        };
        if let Some(session) = cancelled_session {
            self.begin_session_close(session, None, SessionCloseReason::Cancelled)?;
            self.emit_critical(NativeWorkerEvent::RequestCancelled {
                correlation: correlation.clone(),
                event: RequestCancelledEvent { target },
            })?;
        }
        self.emit_critical(NativeWorkerEvent::CancelAcknowledged {
            correlation,
            event: CancelAcknowledgedEvent { target, status },
        })
    }

    fn session_close_reason(&self, session: ProtocolSessionId) -> Option<SessionCloseReason> {
        self.close_backlog
            .iter()
            .find(|pending| pending.session == session)
            .map(|pending| pending.reason)
            .or_else(|| {
                self.pending_closes
                    .values()
                    .find(|pending| pending.session == session)
                    .map(|pending| pending.reason)
            })
            .or_else(|| {
                (self.phase == NativeWorkerPhase::Draining && self.sessions.contains_key(&session))
                    .then_some(SessionCloseReason::Cancelled)
            })
    }

    fn process_release(
        &mut self,
        correlation: Correlation,
        surface_id: pdf_rs_protocol::SurfaceId,
        lease_token: u64,
    ) -> Result<(), EngineIntegrationError> {
        let session = correlation.session.ok_or_else(protocol)?;
        let access = SurfaceAccess::new(
            self.worker(),
            session,
            self.worker_epoch(),
            surface_id,
            lease_token,
        );
        let delivered = self.delivered_surface_position(Some(session), surface_id, lease_token);
        let status = match self.surfaces.release(access) {
            Ok(pdf_rs_surface::ReleaseOutcome::Released(_)) => {
                if let Some(index) = delivered {
                    let lease = self.delivered_surfaces.remove(index);
                    self.emit_critical(NativeWorkerEvent::SurfaceReclaimed {
                        correlation: lease.correlation,
                        event: SurfaceReclaimedEvent {
                            surface: lease.surface,
                            lease_token: lease.lease_token,
                            reason: SurfaceReclaimReason::ReleasedByHost,
                        },
                    })?;
                }
                OperationAckStatus::Applied
            }
            Ok(pdf_rs_surface::ReleaseOutcome::AlreadyRetired(_)) => {
                OperationAckStatus::AlreadyApplied
            }
            Err(_) => OperationAckStatus::UnknownTarget,
        };
        self.emit_critical(NativeWorkerEvent::SurfaceReleaseAcknowledged {
            correlation,
            event: SurfaceReleaseAcknowledgedEvent {
                surface: surface_id,
                lease_token,
                status,
            },
        })
    }

    fn begin_session_close(
        &mut self,
        session: ProtocolSessionId,
        correlation: Option<Correlation>,
        reason: SessionCloseReason,
    ) -> Result<(), EngineIntegrationError> {
        if self.closed_sessions.contains(&session) {
            if let Some(correlation) = correlation {
                self.emit_critical(NativeWorkerEvent::CloseSessionAcknowledged {
                    correlation,
                    event: CloseSessionAcknowledgedEvent {
                        session,
                        status: OperationAckStatus::AlreadyApplied,
                    },
                })?;
            }
            return Ok(());
        }
        if !self.sessions.contains_key(&session) {
            if let Some(correlation) = correlation {
                self.emit_critical(NativeWorkerEvent::CloseSessionAcknowledged {
                    correlation,
                    event: CloseSessionAcknowledgedEvent {
                        session,
                        status: OperationAckStatus::UnknownTarget,
                    },
                })?;
            }
            return Ok(());
        }
        let state = self.sessions.get_mut(&session).ok_or_else(invalid_state)?;
        if state.phase == SessionPhase::Closing {
            if let Some(correlation) = correlation {
                self.emit_critical(NativeWorkerEvent::CloseSessionAcknowledged {
                    correlation,
                    event: CloseSessionAcknowledgedEvent {
                        session,
                        status: OperationAckStatus::AlreadyApplied,
                    },
                })?;
            }
            return Ok(());
        }
        let scheduler_session =
            SchedulerSessionId::new(session.value()).ok_or_else(invalid_identity)?;
        let active_ids = self
            .active
            .iter()
            .filter_map(|(work_id, active)| {
                (active.signal.session_id == scheduler_session).then_some(*work_id)
            })
            .collect::<Vec<_>>();
        self.ensure_cancel_backlog_space(active_ids.len())?;
        let reclaim_count = self
            .delivered_surfaces
            .iter()
            .filter(|lease| lease.correlation.session == Some(session))
            .count();
        self.ensure_surface_reclaim_space(reclaim_count)?;
        self.surfaces
            .close_session(session)
            .map_err(|_| surface())?;
        self.enqueue_delivered_surface_reclaims(session, None, SurfaceReclaimReason::SessionClosed);
        self.sessions
            .get_mut(&session)
            .ok_or_else(invalid_state)?
            .phase = SessionPhase::Closing;
        self.purge_session_work_reentries(session);
        for work_id in active_ids {
            if let Some(active) = self.active.remove(&work_id) {
                active.cancel_work();
                self.terminal_jobs.insert(
                    work_id,
                    TerminalJob {
                        signal: active.signal,
                        correlation: active.job.correlation,
                        failure: EngineErrorCode::Cancelled,
                    },
                );
                self.push_cancel(active.signal);
            }
        }
        let mut queued_terminals = Vec::new();
        self.pending_resources.retain(|_, resource| {
            let keep = resource.signal.session_id != scheduler_session;
            if !keep {
                push_unique_correlation(&mut queued_terminals, resource.correlation.clone());
            }
            keep
        });
        self.publications.retain(|batch| {
            let keep = batch.correlation.session != Some(session);
            if !keep {
                push_unique_correlation(&mut queued_terminals, batch.correlation.clone());
            }
            keep
        });
        self.purge_events(session, None);
        self.close_backlog.push_back(PendingSessionClose {
            session,
            scheduler_session,
            correlation,
            reason,
            queued_terminals,
        });
        Ok(())
    }

    fn begin_shutdown(&mut self, correlation: Correlation) -> Result<(), EngineIntegrationError> {
        if self.phase == NativeWorkerPhase::Stopped {
            self.emit_critical(NativeWorkerEvent::ShutdownAcknowledged {
                correlation,
                event: ShutdownAcknowledgedEvent {
                    worker: self.worker(),
                    status: OperationAckStatus::AlreadyApplied,
                },
            })?;
            return Ok(());
        }
        if self.phase == NativeWorkerPhase::Draining {
            self.emit_critical(NativeWorkerEvent::ShutdownAcknowledged {
                correlation,
                event: ShutdownAcknowledgedEvent {
                    worker: self.worker(),
                    status: OperationAckStatus::AlreadyApplied,
                },
            })?;
            return Ok(());
        }
        self.ensure_cancel_backlog_space(self.active.len())?;
        self.ensure_surface_reclaim_space(self.delivered_surfaces.len())?;
        let sessions = self.sessions.keys().copied().collect::<Vec<_>>();
        for session in &sessions {
            self.surfaces
                .close_session(*session)
                .map_err(|_| surface())?;
            self.enqueue_delivered_surface_reclaims(
                *session,
                None,
                SurfaceReclaimReason::SessionClosed,
            );
        }
        self.phase = NativeWorkerPhase::Draining;
        self.pending_shutdown = Some(correlation);
        self.shutdown_admitted = false;
        self.shutdown_queued_terminals.clear();

        for session in sessions {
            if let Some(state) = self.sessions.get_mut(&session) {
                state.phase = SessionPhase::Closing;
            }
            self.purge_session_work_reentries(session);
            self.purge_events(session, None);
        }
        let active = std::mem::take(&mut self.active);
        for (_, work) in active {
            work.cancel_work();
            self.terminal_jobs.insert(
                work.signal.work_id,
                TerminalJob {
                    signal: work.signal,
                    correlation: work.job.correlation,
                    failure: EngineErrorCode::Cancelled,
                },
            );
            self.push_cancel(work.signal);
        }
        let pending_resources = std::mem::take(&mut self.pending_resources);
        for (_, resource) in pending_resources {
            push_unique_correlation(&mut self.shutdown_queued_terminals, resource.correlation);
        }
        let publications = std::mem::take(&mut self.publications);
        for batch in publications {
            push_unique_correlation(&mut self.shutdown_queued_terminals, batch.correlation);
        }
        self.progress_events.clear();
        Ok(())
    }

    fn pump_close_backlog(&mut self) -> Result<Option<ActorProgress>, EngineIntegrationError> {
        let Some(pending) = self.close_backlog.front() else {
            return Ok(None);
        };
        if self.scheduler.critical_len() == self.config.limits().scheduler.critical_capacity() {
            return Ok(None);
        }
        let scheduler_session = pending.scheduler_session;
        let receipt = self
            .scheduler
            .close_session(scheduler_session)
            .map_err(|_| scheduler())?;
        let mut pending = self.close_backlog.pop_front().ok_or_else(internal)?;
        for work_id in receipt.superseded_queued {
            if let Some(job) = self.queued_jobs.remove(&work_id) {
                push_unique_correlation(&mut pending.queued_terminals, job.correlation);
            }
        }
        self.pending_closes.insert(scheduler_session, pending);
        Ok(Some(ActorProgress::Lifecycle))
    }

    fn pump_shutdown_admission(&mut self) -> Result<Option<ActorProgress>, EngineIntegrationError> {
        if self.phase != NativeWorkerPhase::Draining
            || self.pending_shutdown.is_none()
            || self.shutdown_admitted
            || !self.close_backlog.is_empty()
        {
            return Ok(None);
        }
        if self.scheduler.critical_len() == self.config.limits().scheduler.critical_capacity() {
            return Ok(None);
        }
        let receipt = self.scheduler.begin_shutdown().map_err(|_| scheduler())?;
        for work_id in receipt.superseded_queued {
            if let Some(job) = self.queued_jobs.remove(&work_id) {
                push_unique_correlation(&mut self.shutdown_queued_terminals, job.correlation);
            }
        }
        self.shutdown_admitted = true;
        Ok(Some(ActorProgress::Lifecycle))
    }

    fn scheduler_event_space_requirement(&self) -> usize {
        let mut required = 1;
        for pending in self.pending_closes.values() {
            let request_terminal = self.sessions.get(&pending.session).is_some_and(|session| {
                matches!(
                    self.requests.get(&session.open_request),
                    Some(RequestState::Active(_))
                )
            });
            let close_events = pending.queued_terminals.len()
                + usize::from(request_terminal)
                + usize::from(pending.correlation.is_some())
                + 1;
            required = required.max(close_events);
        }
        if self.shutdown_admitted {
            let request_terminals = self
                .sessions
                .values()
                .filter(|session| {
                    matches!(
                        self.requests.get(&session.open_request),
                        Some(RequestState::Active(_))
                    )
                })
                .count();
            required = required.max(
                self.shutdown_queued_terminals.len()
                    + request_terminals
                    + usize::from(self.pending_shutdown.is_some()),
            );
        }
        required
    }

    fn pump_scheduler(&mut self) -> Result<Option<ActorProgress>, EngineIntegrationError> {
        if !self.ensure_event_space(self.scheduler_event_space_requirement()) {
            return Ok(None);
        }
        let Some(dispatch) = self.scheduler.dispatch_next().map_err(|_| scheduler())? else {
            return Ok(None);
        };
        match dispatch {
            SchedulerDispatch::Normal(scheduled) => {
                let job = self
                    .queued_jobs
                    .remove(&scheduled.request.work_id)
                    .ok_or_else(internal)?;
                let signal = TerminalSignal {
                    work_id: scheduled.request.work_id,
                    session_id: scheduled.request.session_id,
                    generation: scheduled.request.generation,
                };
                self.active.insert(
                    scheduled.request.work_id,
                    ActiveViewport {
                        signal,
                        job,
                        page_cursor: 0,
                        stage: PageStage::Evaluate,
                        completed: Vec::new(),
                        failure: None,
                    },
                );
            }
            SchedulerDispatch::Critical(dispatch) => self.process_scheduler_critical(dispatch)?,
        }
        Ok(Some(ActorProgress::Scheduled))
    }

    fn process_scheduler_critical(
        &mut self,
        dispatch: CriticalDispatch,
    ) -> Result<(), EngineIntegrationError> {
        match dispatch {
            CriticalDispatch::Cancel(decision) => {
                if let TerminalDecision::Cancelled { work_id } = decision {
                    if let Some(active) = self.active.remove(&work_id) {
                        active.cancel_work();
                    }
                    self.finish_cancelled_generation(work_id)?;
                }
            }
            CriticalDispatch::Failure(decision) => {
                if let TerminalDecision::Failed { work_id } = decision {
                    self.finish_failed_generation(work_id)?;
                }
            }
            CriticalDispatch::Completion(decision) => match decision {
                TerminalDecision::Publish {
                    work_id: _,
                    resource_id,
                } => {
                    let resource = self
                        .pending_resources
                        .remove(&resource_id)
                        .ok_or_else(internal)?;
                    self.publications.push_back(PublicationBatch {
                        correlation: resource.correlation,
                        plans: resource.plans,
                        plan_index: 0,
                        tile_index: 0,
                        produced_regions: 0,
                        staged: VecDeque::new(),
                        staging_complete: false,
                    });
                }
                TerminalDecision::DiscardAndRelease { resource_id, .. } => {
                    self.pending_resources.remove(&resource_id);
                }
                TerminalDecision::Cancelled { .. }
                | TerminalDecision::Failed { .. }
                | TerminalDecision::Ignored { .. } => return Err(internal()),
            },
            CriticalDispatch::Close { session_id } => {
                let pending = self
                    .pending_closes
                    .remove(&session_id)
                    .ok_or_else(internal)?;
                for correlation in pending.queued_terminals {
                    self.emit_superseded_generation(correlation)?;
                }
                self.cleanup_session(pending.session, pending.correlation, true, pending.reason)?;
            }
            CriticalDispatch::Release { .. } => return Err(internal()),
            CriticalDispatch::Shutdown => {
                let queued = std::mem::take(&mut self.shutdown_queued_terminals);
                for correlation in queued {
                    self.emit_superseded_generation(correlation)?;
                }
                let sessions = self.sessions.keys().copied().collect::<Vec<_>>();
                for session in sessions {
                    self.cleanup_session(session, None, false, SessionCloseReason::Cancelled)?;
                }
                self.normal_reentries.clear();
                self.critical_reentries.clear();
                self.close_backlog.clear();
                self.pending_closes.clear();
                let correlation = self
                    .pending_shutdown
                    .take()
                    .unwrap_or_else(|| worker_correlation(self.worker()));
                self.shutdown_admitted = false;
                self.emit_critical(NativeWorkerEvent::ShutdownAcknowledged {
                    correlation,
                    event: ShutdownAcknowledgedEvent {
                        worker: self.worker(),
                        status: OperationAckStatus::Applied,
                    },
                })?;
            }
        }
        Ok(())
    }

    fn finish_cancelled_generation(
        &mut self,
        work_id: WorkId,
    ) -> Result<(), EngineIntegrationError> {
        let Some(terminal) = self.terminal_jobs.remove(&work_id) else {
            return Ok(());
        };
        let session = ProtocolSessionId::new(terminal.signal.session_id.get());
        if !self.sessions.contains_key(&session) {
            return Ok(());
        }
        self.emit_superseded_generation(terminal.correlation)
    }

    fn emit_superseded_generation(
        &mut self,
        correlation: Correlation,
    ) -> Result<(), EngineIntegrationError> {
        let produced_regions = self.delivered_region_count(&correlation);
        self.emit_critical(NativeWorkerEvent::GenerationCompleted {
            correlation,
            event: GenerationCompletedEvent {
                status: GenerationCompletionStatus::Superseded,
                produced_regions,
                error: None,
            },
        })
    }

    fn finish_failed_generation(&mut self, work_id: WorkId) -> Result<(), EngineIntegrationError> {
        let terminal = self.terminal_jobs.remove(&work_id).or_else(|| {
            self.active.remove(&work_id).map(|active| {
                active.cancel_work();
                TerminalJob {
                    signal: active.signal,
                    correlation: active.job.correlation,
                    failure: active.failure.unwrap_or(EngineErrorCode::Internal),
                }
            })
        });
        let Some(terminal) = terminal else {
            return Ok(());
        };
        let session = ProtocolSessionId::new(terminal.signal.session_id.get());
        if !self.sessions.contains_key(&session) {
            return Ok(());
        }
        let error = self.protocol_engine_error(terminal.failure)?;
        let produced_regions = self.delivered_region_count(&terminal.correlation);
        self.emit_critical(NativeWorkerEvent::GenerationCompleted {
            correlation: terminal.correlation,
            event: GenerationCompletedEvent {
                status: GenerationCompletionStatus::Failed,
                produced_regions,
                error: Some(error),
            },
        })
    }

    fn pump_active(&mut self) -> Result<Option<ActorProgress>, EngineIntegrationError> {
        let Some(work_id) = self.active.iter().find_map(|(work_id, active)| {
            (!matches!(
                active.stage,
                PageStage::CapabilityPending { .. }
                    | PageStage::CapabilityQueued(_)
                    | PageStage::PlanPending { .. }
                    | PageStage::PlanQueued(_)
                    | PageStage::RasterPending { .. }
            ))
            .then_some(*work_id)
        }) else {
            return Ok(None);
        };
        enum Action {
            Evaluate,
            Plan,
            Raster,
            Cache,
            Wait,
        }
        let action = match &self.active.get(&work_id).ok_or_else(internal)?.stage {
            PageStage::Evaluate => Action::Evaluate,
            PageStage::CapabilityDelivered(_) => Action::Plan,
            PageStage::PlanDelivered(_) => Action::Raster,
            PageStage::CacheLookup(_) => Action::Cache,
            PageStage::CapabilityPending { .. }
            | PageStage::CapabilityQueued(_)
            | PageStage::PlanPending { .. }
            | PageStage::PlanQueued(_)
            | PageStage::RasterPending { .. } => Action::Wait,
        };
        match action {
            Action::Wait => Ok(None),
            Action::Evaluate => self.evaluate_active(work_id).map(Some),
            Action::Plan => self.plan_active(work_id).map(Some),
            Action::Raster => self.raster_active(work_id).map(Some),
            Action::Cache => self.cache_active(work_id).map(Some),
        }
    }

    fn evaluate_active(
        &mut self,
        work_id: WorkId,
    ) -> Result<ActorProgress, EngineIntegrationError> {
        if self.policy_tasks.len() == self.config.limits().reentry_capacity {
            return Ok(ActorProgress::Idle);
        }
        let (_, document_revision, scene) = self.active_scene(work_id)?;
        let signal = self.active.get(&work_id).ok_or_else(internal)?.signal;
        let Some(permit) = self
            .policy_task_tracker
            .try_acquire(signal, self.config.limits().policy_job.max_retained_bytes())
        else {
            return Ok(ActorProgress::Idle);
        };
        let cancellation = NativePolicyCancellation::default();
        self.active.get_mut(&work_id).ok_or_else(internal)?.stage = PageStage::CapabilityPending {
            cancellation: cancellation.clone(),
        };
        self.policy_tasks.push_back(NativePolicyTask::evaluate(
            self.worker(),
            self.worker_epoch(),
            signal,
            scene,
            document_revision,
            self.config.limits().policy,
            self.config.limits().policy_job,
            cancellation,
            permit,
        ));
        Ok(ActorProgress::Capability)
    }

    fn plan_active(&mut self, work_id: WorkId) -> Result<ActorProgress, EngineIntegrationError> {
        if self.policy_tasks.len() == self.config.limits().reentry_capacity {
            return Ok(ActorProgress::Idle);
        }
        let stage = {
            let active = self.active.get_mut(&work_id).ok_or_else(internal)?;
            std::mem::replace(&mut active.stage, PageStage::Evaluate)
        };
        let PageStage::CapabilityDelivered(CapabilityStage { decision, permit }) = stage else {
            self.active.get_mut(&work_id).ok_or_else(internal)?.stage = stage;
            return Err(internal());
        };
        if decision.status() != CapabilityStatus::Supported {
            let active = self.active.get_mut(&work_id).ok_or_else(internal)?;
            active
                .failure
                .get_or_insert(EngineErrorCode::UnsupportedFeature);
            active.page_cursor = active.page_cursor.checked_add(1).ok_or_else(internal)?;
            if active.page_cursor < active.job.command.viewport.visible_pages.len() {
                active.stage = PageStage::Evaluate;
                return Ok(ActorProgress::Capability);
            }
            if self.scheduler.critical_len() == self.config.limits().scheduler.critical_capacity() {
                active.page_cursor -= 1;
                active.stage = PageStage::CapabilityDelivered(CapabilityStage { decision, permit });
                return Ok(ActorProgress::Idle);
            }
            let active = self.active.remove(&work_id).ok_or_else(internal)?;
            self.terminal_jobs.insert(
                work_id,
                TerminalJob {
                    signal: active.signal,
                    correlation: active.job.correlation,
                    failure: active
                        .failure
                        .unwrap_or(EngineErrorCode::UnsupportedFeature),
                },
            );
            self.scheduler
                .enqueue_failure(active.signal)
                .map_err(|_| scheduler())?;
            return Ok(ActorProgress::Capability);
        }
        let (_, _, scene) = self.active_scene(work_id)?;
        let active = self.active.get(&work_id).ok_or_else(internal)?;
        let page = active
            .job
            .command
            .viewport
            .visible_pages
            .get(active.page_cursor)
            .ok_or_else(internal)?;
        let request = match render_plan_request(&active.job.command, page, scene.as_ref()) {
            Ok(request) => request,
            Err(_) => {
                return self.fail_active(
                    work_id,
                    ActorProgress::Capability,
                    EngineErrorCode::Internal,
                );
            }
        };
        let config = match render_config(&active.job.command) {
            Ok(config) => config,
            Err(_) => {
                return self.fail_active(
                    work_id,
                    ActorProgress::Capability,
                    EngineErrorCode::Internal,
                );
            }
        };
        let signal = active.signal;
        let Some(renderer_epoch) = PolicyRendererEpoch::new(self.config.renderer_epoch()) else {
            return self.fail_active(
                work_id,
                ActorProgress::Capability,
                EngineErrorCode::Internal,
            );
        };
        let cancellation = NativePolicyCancellation::default();
        self.active.get_mut(&work_id).ok_or_else(internal)?.stage = PageStage::PlanPending {
            cancellation: cancellation.clone(),
        };
        self.policy_tasks.push_back(NativePolicyTask::plan(
            self.worker(),
            self.worker_epoch(),
            signal,
            scene,
            decision,
            config,
            request,
            renderer_epoch,
            self.config.limits().policy,
            self.config.limits().policy_job,
            cancellation,
            permit,
        ));
        Ok(ActorProgress::Capability)
    }

    fn raster_active(&mut self, work_id: WorkId) -> Result<ActorProgress, EngineIntegrationError> {
        if self.raster_tasks.len() == self.config.limits().reentry_capacity {
            return Ok(ActorProgress::Idle);
        }
        let stage = {
            let active = self.active.get_mut(&work_id).ok_or_else(internal)?;
            std::mem::replace(&mut active.stage, PageStage::Evaluate)
        };
        let PageStage::PlanDelivered(PlanStage { plan, permit }) = stage else {
            self.active.get_mut(&work_id).ok_or_else(internal)?.stage = stage;
            return Err(internal());
        };
        let signal = self.active.get(&work_id).ok_or_else(internal)?.signal;
        let Some(reservation) = self
            .raster_budget
            .try_reserve(self.config.raster_task_byte_reservation(), signal)
        else {
            self.active.get_mut(&work_id).ok_or_else(internal)?.stage =
                PageStage::PlanDelivered(PlanStage { plan, permit });
            return Ok(ActorProgress::Idle);
        };
        drop(permit);
        let cancellation = NativeRasterCancellation::default();
        let mut tiles = Vec::new();
        if tiles.try_reserve_exact(plan.tiles().len()).is_err() {
            return self.dispatch_raster(work_id, plan, reservation, cancellation);
        }
        self.active.get_mut(&work_id).ok_or_else(internal)?.stage =
            PageStage::CacheLookup(CacheLookupState {
                plan,
                reservation,
                cancellation,
                tiles,
                tile_index: 0,
                retained_bytes: 0,
                current: None,
            });
        self.cache_active(work_id)
    }

    fn cache_active(&mut self, work_id: WorkId) -> Result<ActorProgress, EngineIntegrationError> {
        let active = self.active.get_mut(&work_id).ok_or_else(internal)?;
        let signal = active.signal;
        let stage = std::mem::replace(&mut active.stage, PageStage::Evaluate);
        let PageStage::CacheLookup(mut state) = stage else {
            active.stage = stage;
            return Err(internal());
        };
        let session_id = ProtocolSessionId::new(signal.session_id.get());
        match self.copy_cached_tile_chunk(session_id, &mut state)? {
            CacheCopyProgress::Continue => {
                self.active.get_mut(&work_id).ok_or_else(internal)?.stage =
                    PageStage::CacheLookup(state);
                Ok(ActorProgress::CacheHit)
            }
            CacheCopyProgress::Miss => {
                if self.raster_tasks.len() == self.config.limits().reentry_capacity {
                    self.active.get_mut(&work_id).ok_or_else(internal)?.stage =
                        PageStage::CacheLookup(state);
                    return Ok(ActorProgress::Idle);
                }
                self.dispatch_raster(work_id, state.plan, state.reservation, state.cancellation)
            }
            CacheCopyProgress::Complete => {
                let active = self.active.get(&work_id).ok_or_else(internal)?;
                let next_page = active.page_cursor.checked_add(1).ok_or_else(internal)?;
                let final_page = next_page == active.job.command.viewport.visible_pages.len();
                let needs_resource = final_page && active.failure.is_none();
                if final_page
                    && self.scheduler.critical_len()
                        == self.config.limits().scheduler.critical_capacity()
                    || needs_resource
                        && self.pending_resources.len() + self.publications.len()
                            == self.config.limits().pending_resource_capacity
                {
                    self.active.get_mut(&work_id).ok_or_else(internal)?.stage =
                        PageStage::CacheLookup(state);
                    return Ok(ActorProgress::Idle);
                }
                state.reservation.shrink_to(state.retained_bytes);
                self.commit_completed_plan(
                    signal,
                    CompletedPlan {
                        plan: state.plan,
                        tiles: CompletedTiles::CacheHit {
                            tiles: state.tiles,
                            _reservation: state.reservation,
                        },
                    },
                )?;
                Ok(ActorProgress::CacheHit)
            }
        }
    }

    fn dispatch_raster(
        &mut self,
        work_id: WorkId,
        plan: Arc<RenderPlan>,
        reservation: NativeRasterReservation,
        cancellation: NativeRasterCancellation,
    ) -> Result<ActorProgress, EngineIntegrationError> {
        let (_, _, scene) = self.active_scene(work_id)?;
        let signal = self.active.get(&work_id).ok_or_else(internal)?.signal;
        self.active.get_mut(&work_id).ok_or_else(internal)?.stage = PageStage::RasterPending {
            plan: Arc::clone(&plan),
            cancellation: cancellation.clone(),
        };
        self.raster_tasks.push_back(NativeRasterTask::new(
            signal,
            scene,
            plan,
            self.config.limits().raster,
            self.config.limits().policy_job,
            self.config.limits().raster_job,
            cancellation,
            reservation,
        ));
        Ok(ActorProgress::Raster)
    }

    fn copy_cached_tile_chunk(
        &mut self,
        session_id: ProtocolSessionId,
        state: &mut CacheLookupState,
    ) -> Result<CacheCopyProgress, EngineIntegrationError> {
        if state.tile_index == state.plan.tiles().len() {
            return Ok(CacheCopyProgress::Complete);
        }
        let worker = self.worker();
        let planned = state
            .plan
            .tiles()
            .get(state.tile_index)
            .ok_or_else(internal)?;
        let address = TileCacheAddress::new(
            TileCacheOwnerId::new(worker.value()),
            TileCacheSessionId::new(session_id.value()),
            planned.content_key().clone(),
        );
        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or_else(invalid_state)?;
        let tile_cache = session.cache.as_mut().ok_or_else(internal)?;
        let lookup = match tile_cache.lookup(&address, &state.cancellation) {
            Ok(lookup) => lookup,
            Err(_) => return Ok(CacheCopyProgress::Miss),
        };
        let TileCacheLookup::Hit(tile) = lookup else {
            return Ok(CacheCopyProgress::Miss);
        };
        if tile.content_key() != planned.content_key() {
            return Err(identity_mismatch());
        }
        if state.current.is_none() {
            let mut pixels = Vec::new();
            if pixels.try_reserve_exact(tile.pixels().len()).is_err() {
                return Ok(CacheCopyProgress::Miss);
            }
            let capacity = u64::try_from(pixels.capacity()).map_err(|_| internal())?;
            let retained = state
                .retained_bytes
                .checked_add(capacity)
                .ok_or_else(internal)?;
            if !state.reservation.covers(retained) {
                return Ok(CacheCopyProgress::Miss);
            }
            state.retained_bytes = retained;
            state.current = Some(CachedTileCopy {
                content_key: planned.content_key().clone(),
                stride: tile.stride(),
                total_bytes: tile.pixels().len(),
                pixels,
            });
        }
        let current = state.current.as_mut().ok_or_else(internal)?;
        if current.content_key != *planned.content_key()
            || current.stride != tile.stride()
            || current.total_bytes != tile.pixels().len()
        {
            return Err(identity_mismatch());
        }
        let offset = current.pixels.len();
        let end = offset
            .checked_add(CACHE_COPY_CHUNK_BYTES)
            .map_or(current.total_bytes, |end| end.min(current.total_bytes));
        current.pixels.extend_from_slice(
            tile.pixels()
                .get(offset..end)
                .ok_or_else(identity_mismatch)?,
        );
        if end == current.total_bytes {
            let complete = state.current.take().ok_or_else(internal)?;
            state.tiles.push(CachedTilePixels {
                content_key: complete.content_key,
                stride: complete.stride,
                pixels: complete.pixels,
            });
            state.tile_index = state.tile_index.checked_add(1).ok_or_else(internal)?;
        }
        if state.tile_index == state.plan.tiles().len() {
            Ok(CacheCopyProgress::Complete)
        } else {
            Ok(CacheCopyProgress::Continue)
        }
    }

    fn fail_active(
        &mut self,
        work_id: WorkId,
        progress: ActorProgress,
        failure: EngineErrorCode,
    ) -> Result<ActorProgress, EngineIntegrationError> {
        if self.scheduler.critical_len() == self.config.limits().scheduler.critical_capacity() {
            return Ok(ActorProgress::Idle);
        }
        let active = self.active.remove(&work_id).ok_or_else(internal)?;
        active.cancel_work();
        self.terminal_jobs.insert(
            work_id,
            TerminalJob {
                signal: active.signal,
                correlation: active.job.correlation,
                failure,
            },
        );
        self.scheduler
            .enqueue_failure(active.signal)
            .map_err(|_| scheduler())?;
        Ok(progress)
    }

    fn active_scene(
        &self,
        work_id: WorkId,
    ) -> Result<(ProtocolSessionId, u64, Arc<Scene>), EngineIntegrationError> {
        let active = self.active.get(&work_id).ok_or_else(internal)?;
        let session_id = ProtocolSessionId::new(active.signal.session_id.get());
        let session = self.sessions.get(&session_id).ok_or_else(invalid_state)?;
        if session.phase != SessionPhase::Ready {
            return Err(invalid_state());
        }
        let page = active
            .job
            .command
            .viewport
            .visible_pages
            .get(active.page_cursor)
            .ok_or_else(internal)?;
        let scene = find_scene(&session.scenes, page.page_index)
            .cloned()
            .ok_or_else(identity_mismatch)?;
        let document_revision = session.document_revision.ok_or_else(internal)?;
        Ok((session_id, document_revision, scene))
    }

    fn pump_publication(&mut self) -> Result<Option<ActorProgress>, EngineIntegrationError> {
        let Some(mut batch) = self.publications.pop_front() else {
            return Ok(None);
        };
        if !self.ensure_event_space(1) {
            self.publications.push_front(batch);
            return Ok(Some(ActorProgress::Idle));
        }

        let correlation = batch.correlation.clone();
        let session_id = correlation.session.ok_or_else(internal)?;
        let generation = correlation.generation.ok_or_else(internal)?;
        let session_current = self.sessions.get(&session_id).is_some_and(|session| {
            session.phase == SessionPhase::Ready && session.viewport_generation == Some(generation)
        });
        if !session_current {
            self.release_staged_publications(&mut batch);
            return Ok(Some(ActorProgress::Lifecycle));
        }

        if batch.staging_complete {
            if let Some(publication) = batch.staged.pop_front() {
                self.emit_critical(NativeWorkerEvent::SurfaceReady(publication))?;
                self.publications.push_front(batch);
                return Ok(Some(ActorProgress::Published));
            }
            self.emit_critical(NativeWorkerEvent::GenerationCompleted {
                correlation: batch.correlation,
                event: GenerationCompletedEvent {
                    status: GenerationCompletionStatus::Completed,
                    produced_regions: batch.produced_regions,
                    error: None,
                },
            })?;
            return Ok(Some(ActorProgress::Published));
        }

        if batch.plan_index == batch.plans.len() {
            batch.plans.clear();
            batch.staging_complete = true;
            self.publications.push_front(batch);
            return Ok(Some(ActorProgress::Published));
        }

        let completed = batch.plans.get(batch.plan_index).ok_or_else(internal)?;
        if batch.tile_index == completed.tile_count() {
            batch.plan_index = batch.plan_index.checked_add(1).ok_or_else(internal)?;
            batch.tile_index = 0;
            self.publications.push_front(batch);
            return Ok(Some(ActorProgress::Published));
        }
        let plan = Arc::clone(&completed.plan);
        let tile_ordinal = batch.tile_index;
        let publication = match &completed.tiles {
            CompletedTiles::Raster { tiles, .. } => {
                let fast_tile = tiles.tiles().get(tile_ordinal).ok_or_else(internal)?;
                let actual_ordinal = usize::try_from(fast_tile.identity().ordinal())
                    .map_err(|_| identity_mismatch())?;
                if actual_ordinal != tile_ordinal
                    || fast_tile.identity().plan_hash() != plan.hash()
                    || fast_tile.identity().generation() != generation
                {
                    Err(identity_mismatch())
                } else {
                    self.publish_tile(
                        session_id,
                        generation,
                        correlation.clone(),
                        &plan,
                        tile_ordinal,
                        fast_tile.identity().content_key().clone(),
                        fast_tile.stride(),
                        fast_tile.pixels(),
                    )
                }
            }
            CompletedTiles::CacheHit { tiles, .. } => {
                let cached = tiles.get(tile_ordinal).ok_or_else(internal)?;
                let planned = plan.tiles().get(tile_ordinal).ok_or_else(internal)?;
                if &cached.content_key != planned.content_key() {
                    Err(identity_mismatch())
                } else {
                    let worker = self.worker();
                    let worker_epoch = self.worker_epoch();
                    let renderer_epoch = self.config.renderer_epoch();
                    Self::publish_surface(
                        &mut self.surfaces,
                        worker,
                        worker_epoch,
                        renderer_epoch,
                        session_id,
                        generation,
                        correlation.clone(),
                        &plan,
                        tile_ordinal,
                        cached.stride,
                        &cached.pixels,
                    )
                }
            }
        };
        let publication = match publication {
            Ok(publication) => publication,
            Err(integration_error) => {
                self.release_staged_publications(&mut batch);
                let code = publication_failure_code(integration_error);
                if code == EngineErrorCode::Internal {
                    self.begin_session_close(session_id, None, SessionCloseReason::Internal)?;
                }
                let error = self.protocol_engine_error(code)?;
                let produced_regions = self.delivered_region_count(&correlation);
                self.emit_critical(NativeWorkerEvent::GenerationCompleted {
                    correlation,
                    event: GenerationCompletedEvent {
                        status: GenerationCompletionStatus::Failed,
                        produced_regions,
                        error: Some(error),
                    },
                })?;
                return Ok(Some(ActorProgress::Lifecycle));
            }
        };
        batch.staged.push_back(publication);
        batch.tile_index = batch.tile_index.checked_add(1).ok_or_else(internal)?;
        batch.produced_regions = batch.produced_regions.checked_add(1).ok_or_else(internal)?;
        self.publications.push_front(batch);
        Ok(Some(ActorProgress::Published))
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_tile(
        &mut self,
        session_id: ProtocolSessionId,
        generation: u64,
        correlation: Correlation,
        plan: &RenderPlan,
        tile_ordinal: usize,
        content_key: pdf_rs_policy::TileContentKey,
        stride: u32,
        pixels: &[u8],
    ) -> Result<SurfacePublication, EngineIntegrationError> {
        let current_cache_bytes = self.cache_resident_bytes();
        let cache_capacity = self.config.limits().retained_cache_byte_capacity;
        let input_bytes = u64::try_from(pixels.len()).map_err(|_| cache())?;
        if current_cache_bytes
            .checked_add(input_bytes)
            .is_some_and(|resident| resident <= cache_capacity)
        {
            let mut cache_pixels = Vec::new();
            if cache_pixels.try_reserve_exact(pixels.len()).is_ok() {
                cache_pixels.extend_from_slice(pixels);
                let retained = u64::try_from(cache_pixels.capacity()).map_err(|_| cache())?;
                if current_cache_bytes
                    .checked_add(retained)
                    .is_some_and(|resident| resident <= cache_capacity)
                    && let Ok(native) = NativeTile::try_new(content_key, cache_pixels)
                {
                    let address = TileCacheAddress::new(
                        TileCacheOwnerId::new(self.worker().value()),
                        TileCacheSessionId::new(session_id.value()),
                        native.content_key().clone(),
                    );
                    let session = self
                        .sessions
                        .get_mut(&session_id)
                        .ok_or_else(invalid_state)?;
                    let tile_cache = session.cache.as_mut().ok_or_else(internal)?;
                    match tile_cache.try_admit(
                        &address,
                        TileRenderOutcome::Complete(native),
                        TileRetentionClass::ProtectedViewport,
                        &NeverCancelledTileCache,
                    ) {
                        Ok(TileAdmission::Admitted(_))
                        | Ok(TileAdmission::Rejected(_))
                        | Err(_) => {}
                    }
                    debug_assert!(self.cache_resident_bytes() <= cache_capacity);
                }
            }
        }

        let worker = self.worker();
        let worker_epoch = self.worker_epoch();
        let renderer_epoch = self.config.renderer_epoch();
        Self::publish_surface(
            &mut self.surfaces,
            worker,
            worker_epoch,
            renderer_epoch,
            session_id,
            generation,
            correlation,
            plan,
            tile_ordinal,
            stride,
            pixels,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_surface(
        surfaces: &mut SurfaceOwner,
        worker: WorkerId,
        worker_epoch: WorkerEpoch,
        renderer_epoch: u32,
        session_id: ProtocolSessionId,
        generation: u64,
        correlation: Correlation,
        plan: &RenderPlan,
        tile_ordinal: usize,
        stride: u32,
        pixels: &[u8],
    ) -> Result<SurfacePublication, EngineIntegrationError> {
        let content_key = plan
            .tiles()
            .get(tile_ordinal)
            .ok_or_else(identity_mismatch)?
            .content_key();
        let width = content_key.tile().width();
        let height = content_key.tile().height();
        let plan_identity =
            SurfacePlanIdentity::from_render_plan(plan, tile_ordinal).map_err(|_| surface())?;
        let region_length = u64::try_from(pixels.len()).map_err(|_| surface())?;
        let allocated = surfaces
            .allocate(SurfaceAllocation {
                worker,
                session: session_id,
                worker_epoch,
                plan: plan_identity.clone(),
                width,
                height,
                stride,
                format: plan_identity.format(),
                alpha: plan_identity.alpha(),
                byte_offset: 0,
                region_length,
            })
            .map_err(|_| surface())?;
        if allocated.layout_bytes() != region_length {
            let _ =
                surfaces.discard_private(allocated.access(), pdf_rs_surface::RetireReason::Failed);
            return Err(identity_mismatch());
        }
        if surfaces
            .write_private_pixels(allocated.access(), pixels)
            .is_err()
        {
            let _ =
                surfaces.discard_private(allocated.access(), pdf_rs_surface::RetireReason::Failed);
            return Err(surface());
        }
        if surfaces.publish(allocated.access()).is_err() {
            let _ =
                surfaces.discard_private(allocated.access(), pdf_rs_surface::RetireReason::Failed);
            return Err(surface());
        }
        let transfer = match surfaces.transfer(allocated.access()) {
            Ok(transfer) => transfer,
            Err(_) => {
                let _ = surfaces.release(allocated.access());
                return Err(surface());
            }
        };
        let event = SurfaceReadyEvent {
            metadata: transfer.metadata.clone(),
            transport: transfer.transport.clone(),
        };
        if event.metadata.generation != generation
            || event.metadata.backend != pdf_rs_protocol::NativeBackend::FastCpu
            || event.metadata.renderer_epoch.value() != renderer_epoch
            || event.metadata.plan_id != plan.protocol_manifest().plan_id
            || event.metadata.plan_hash
                != pdf_rs_protocol::RenderPlanHash::new(plan.hash().into_digest())
        {
            let _ = surfaces.release(allocated.access());
            return Err(identity_mismatch());
        }
        Ok(SurfacePublication::new(
            correlation,
            event,
            plan_identity,
            transfer,
        ))
    }

    fn release_staged_publications(&mut self, batch: &mut PublicationBatch) {
        for publication in batch.staged.drain(..) {
            let metadata = &publication.event().metadata;
            let access = SurfaceAccess::new(
                metadata.owner.worker,
                metadata.owner.session,
                self.worker_epoch(),
                metadata.id,
                metadata.lease_token,
            );
            let _ = self.surfaces.release(access);
        }
    }

    fn cleanup_session(
        &mut self,
        session_id: ProtocolSessionId,
        correlation: Option<Correlation>,
        emit_closed: bool,
        reason: SessionCloseReason,
    ) -> Result<(), EngineIntegrationError> {
        let Some(mut session) = self.sessions.remove(&session_id) else {
            return Ok(());
        };
        session.phase = SessionPhase::Closed;
        if let Some(cache) = session.cache.as_mut() {
            cache.close();
        }
        self.tickets.invalidate_session(self.worker(), session_id);
        self.queued_jobs
            .retain(|_, job| job.correlation.session != Some(session_id));
        self.pending_resources
            .retain(|_, resource| resource.correlation.session != Some(session_id));
        self.publications
            .retain(|batch| batch.correlation.session != Some(session_id));
        self.terminal_jobs
            .retain(|_, terminal| terminal.correlation.session != Some(session_id));
        self.purge_events(session_id, None);
        self.surfaces
            .close_session(session_id)
            .map_err(|_| surface())?;
        self.closed_sessions.insert(session_id);

        if let Some(RequestState::Active(_)) = self.requests.get(&session.open_request).copied() {
            let request_correlation = Correlation {
                worker: self.worker(),
                session: Some(session_id),
                request: Some(session.open_request),
                generation: None,
            };
            match reason {
                SessionCloseReason::Explicit | SessionCloseReason::Cancelled => {
                    self.requests
                        .insert(session.open_request, RequestState::Cancelled(session_id));
                    self.emit_critical(NativeWorkerEvent::RequestCancelled {
                        correlation: request_correlation,
                        event: RequestCancelledEvent {
                            target: session.open_request,
                        },
                    })?;
                }
                SessionCloseReason::SourceChanged
                | SessionCloseReason::OpenFailed
                | SessionCloseReason::Internal => {
                    self.requests
                        .insert(session.open_request, RequestState::Failed(session_id));
                    let code = match reason {
                        SessionCloseReason::SourceChanged => EngineErrorCode::SourceChanged,
                        SessionCloseReason::OpenFailed => EngineErrorCode::InvalidDocument,
                        SessionCloseReason::Internal => EngineErrorCode::Internal,
                        SessionCloseReason::Explicit | SessionCloseReason::Cancelled => {
                            unreachable!()
                        }
                    };
                    let error = self.protocol_engine_error(code)?;
                    self.emit_critical(NativeWorkerEvent::RequestFailed {
                        correlation: request_correlation,
                        event: RequestFailedEvent { error },
                    })?;
                }
            }
        }
        if let Some(correlation) = correlation {
            self.emit_critical(NativeWorkerEvent::CloseSessionAcknowledged {
                correlation: correlation.clone(),
                event: CloseSessionAcknowledgedEvent {
                    session: session_id,
                    status: OperationAckStatus::Applied,
                },
            })?;
        }
        if emit_closed {
            let correlation = Correlation {
                worker: self.worker(),
                session: Some(session_id),
                request: None,
                generation: None,
            };
            self.emit_critical(NativeWorkerEvent::SessionClosed {
                correlation,
                event: SessionClosedEvent {
                    session: session_id,
                },
            })?;
        }
        Ok(())
    }

    fn restart_now(&mut self, config: NativeWorkerConfig) -> Result<(), EngineIntegrationError> {
        if config.worker() == self.worker()
            || config.worker_epoch().value() <= self.worker_epoch().value()
        {
            return Err(invalid_identity());
        }
        if self.policy_task_tracker.external() != 0 || self.raster_budget.external() != 0 {
            return Err(backpressure());
        }
        let limits = config.limits();
        let surfaces = SurfaceOwner::new(
            config.worker(),
            config.worker_epoch(),
            ProtocolRendererEpoch::new(config.renderer_epoch()),
            limits.surface,
        )
        .map_err(|_| surface())?;
        let tickets =
            DataTicketLedger::new(limits.scheduler.max_sessions()).map_err(|_| protocol())?;
        let close_backlog = reserved_queue(limits.scheduler.max_sessions())?;
        let shutdown_queued_terminals = reserved_vec(limits.scheduler.max_sessions())?;
        let cancel_backlog = reserved_queue(limits.scheduler.in_flight_capacity())?;
        let normal_reentries = reserved_queue(limits.reentry_capacity)?;
        let critical_reentries = reserved_queue(limits.reentry_capacity)?;
        let lifecycle_reentries = reserved_queue(limits.lifecycle_reentry_capacity)?;
        let policy_tasks = reserved_queue(limits.reentry_capacity)?;
        let raster_tasks = reserved_queue(limits.reentry_capacity)?;
        let critical_events = reserved_queue(limits.critical_event_capacity)?;
        let publications = reserved_queue(limits.pending_resource_capacity)?;
        if self.delivered_surfaces.len() > limits.surface.max_live_surfaces() {
            return Err(backpressure());
        }
        let delivered_surfaces = reserved_vec(limits.surface.max_live_surfaces())?;
        let mut pending_surface_reclaims = reserved_queue(limits.surface.max_live_surfaces())?;
        for lease in &self.delivered_surfaces {
            pending_surface_reclaims.push_back(NativeWorkerEvent::SurfaceReclaimed {
                correlation: lease.correlation.clone(),
                event: SurfaceReclaimedEvent {
                    surface: lease.surface,
                    lease_token: lease.lease_token,
                    reason: SurfaceReclaimReason::RendererRestarted,
                },
            });
        }
        let deferred_generation_terminals = reserved_queue(limits.reentry_capacity)?;
        let validator = ProtocolValidator::new(limits.protocol);
        let scheduler = ViewportScheduler::new(limits.scheduler);
        let policy_task_tracker = Arc::new(PolicyTaskTracker::new(
            limits.reentry_capacity,
            limits.retained_policy_job_byte_capacity,
            config.worker_epoch(),
        ));
        let raster_budget = Arc::new(RasterBudget::new(
            limits.retained_raster_byte_capacity,
            config.worker_epoch(),
        ));

        for session in self.sessions.values_mut() {
            if let Some(cache) = session.cache.as_mut() {
                cache.close();
            }
        }
        for active in self.active.values() {
            active.cancel_work();
        }
        self.scene_budget
            .reconfigure(limits.retained_scene_byte_capacity);
        self.config = config;
        self.phase = NativeWorkerPhase::Ready;
        self.validator = validator;
        self.tickets = tickets;
        self.scheduler = scheduler;
        self.surfaces = surfaces;
        self.sessions.clear();
        self.closed_sessions.clear();
        self.requests.clear();
        self.queued_jobs.clear();
        self.active.clear();
        self.terminal_jobs.clear();
        self.pending_resources.clear();
        self.publications = publications;
        self.close_backlog = close_backlog;
        self.pending_closes.clear();
        self.pending_shutdown = None;
        self.shutdown_admitted = false;
        self.shutdown_queued_terminals = shutdown_queued_terminals;
        self.cancel_backlog = cancel_backlog;
        self.normal_reentries = normal_reentries;
        self.critical_reentries = critical_reentries;
        self.lifecycle_reentries = lifecycle_reentries;
        self.policy_tasks = policy_tasks;
        self.policy_task_tracker = policy_task_tracker;
        self.raster_tasks = raster_tasks;
        self.raster_budget = raster_budget;
        self.critical_events = critical_events;
        self.progress_events.clear();
        self.delivered_surfaces = delivered_surfaces;
        self.pending_surface_reclaims = pending_surface_reclaims;
        self.deferred_generation_terminals = deferred_generation_terminals;
        self.delivered_regions.clear();
        self.next_session_id = 1;
        self.next_work_id = 1;
        self.next_resource_id = 1;
        self.next_diagnostic_id = 1;
        self.work_history_len = 0;
        Ok(())
    }

    fn protocol_engine_error(
        &mut self,
        code: EngineErrorCode,
    ) -> Result<EngineError, EngineIntegrationError> {
        let descriptor = pdf_rs_protocol::ENGINE_ERROR_DESCRIPTORS
            .iter()
            .find(|descriptor| descriptor.code == code)
            .ok_or_else(internal)?;
        let diagnostic = self.next_diagnostic_id;
        self.next_diagnostic_id = diagnostic.checked_add(1).ok_or_else(internal)?;
        Ok(EngineError {
            code,
            category: descriptor.category,
            severity: descriptor.severity,
            recoverability: descriptor.recoverability,
            diagnostic_id: DiagnosticId::new(diagnostic),
        })
    }

    fn delivered_region_count(&self, correlation: &Correlation) -> u32 {
        generation_delivery_key(correlation)
            .and_then(|key| self.delivered_regions.get(&key).copied())
            .unwrap_or(0)
    }

    fn cache_resident_bytes(&self) -> u64 {
        self.sessions
            .values()
            .filter_map(|session| session.cache.as_ref())
            .map(|cache| cache.stats().resident_bytes())
            .fold(0_u64, u64::saturating_add)
    }
}

fn generation_delivery_key(correlation: &Correlation) -> Option<(ProtocolSessionId, u64)> {
    Some((correlation.session?, correlation.generation?))
}

fn retained_scene_bytes(scenes: &[Arc<Scene>], capacity: usize) -> Option<u64> {
    let vector_capacity = u64::try_from(capacity).ok()?;
    let vector_slot_bytes = u64::try_from(std::mem::size_of::<Arc<Scene>>()).ok()?;
    let outer_bytes = vector_capacity.checked_mul(vector_slot_bytes)?;
    scenes.iter().try_fold(outer_bytes, |retained, scene| {
        retained
            .checked_add(SCENE_OWNERSHIP_FLOOR_BYTES)?
            .checked_add(scene.stats().retained_bytes())
    })
}

fn find_scene(scenes: &[Arc<Scene>], page_index: u32) -> Option<&Arc<Scene>> {
    scenes
        .binary_search_by_key(&page_index, |scene| scene.binding().page_index())
        .ok()
        .and_then(|index| scenes.get(index))
}

fn valid_source(source: &SourceDescriptor) -> bool {
    source.identity.revision != 0
        && source.identity.stable_id.iter().any(|byte| *byte != 0)
        && source.validator.iter().any(|byte| *byte != 0)
        && source.length != Some(0)
}

fn source_matches(descriptor: &SourceDescriptor, source: pdf_rs_bytes::SourceIdentity) -> bool {
    descriptor.identity.stable_id == source.stable_id().digest()
        && descriptor.identity.revision == source.revision().value()
}

const fn request_state_session(state: RequestState) -> ProtocolSessionId {
    match state {
        RequestState::Active(session)
        | RequestState::Succeeded(session)
        | RequestState::Failed(session)
        | RequestState::Cancelled(session) => session,
    }
}

fn reserved_queue<T>(capacity: usize) -> Result<VecDeque<T>, EngineIntegrationError> {
    let mut queue = VecDeque::new();
    queue
        .try_reserve_exact(capacity)
        .map_err(|_| invalid_config())?;
    Ok(queue)
}

fn reserved_vec<T>(capacity: usize) -> Result<Vec<T>, EngineIntegrationError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| invalid_config())?;
    Ok(values)
}

fn push_unique_correlation(correlations: &mut Vec<Correlation>, correlation: Correlation) {
    if !correlations.contains(&correlation) {
        correlations.push(correlation);
    }
}

fn publication_failure_code(error: EngineIntegrationError) -> EngineErrorCode {
    match error.code() {
        crate::EngineIntegrationErrorCode::Backpressure
        | crate::EngineIntegrationErrorCode::Cache
        | crate::EngineIntegrationErrorCode::Surface => EngineErrorCode::ResourceLimit,
        crate::EngineIntegrationErrorCode::InvalidConfig
        | crate::EngineIntegrationErrorCode::Protocol
        | crate::EngineIntegrationErrorCode::InvalidState
        | crate::EngineIntegrationErrorCode::InvalidIdentity
        | crate::EngineIntegrationErrorCode::IdentityMismatch
        | crate::EngineIntegrationErrorCode::Policy
        | crate::EngineIntegrationErrorCode::Raster
        | crate::EngineIntegrationErrorCode::Scheduler
        | crate::EngineIntegrationErrorCode::Internal => EngineErrorCode::Internal,
    }
}

fn render_config(command: &SetViewportCommand) -> Result<RenderConfig, EngineIntegrationError> {
    if command.viewport.output_profile != OutputProfile::Srgb {
        return Err(policy());
    }
    let mut input = RenderConfigInput::fast_cpu_full();
    input.backend = NativeBackend::FastCpu;
    input.quality = match command.viewport.quality {
        pdf_rs_protocol::QualityPolicy::Preview => QualityPolicy::Preview,
        pdf_rs_protocol::QualityPolicy::Full => QualityPolicy::Full,
    };
    RenderConfig::validate(input).map_err(|_| policy())
}

fn render_plan_request(
    command: &SetViewportCommand,
    page: &pdf_rs_protocol::PageViewport,
    scene: &Scene,
) -> Result<RenderPlanRequest, EngineIntegrationError> {
    let clip = device_clip(command, page, scene)?;
    let zoom = ZoomRatio::new(
        command.viewport.zoom_numerator,
        command.viewport.zoom_denominator,
    )
    .map_err(|_| policy())?;
    let rotation = match command.viewport.rotation {
        pdf_rs_protocol::PageRotation::Degrees0 => PageRotation::Degrees0,
        pdf_rs_protocol::PageRotation::Degrees90 => PageRotation::Degrees90,
        pdf_rs_protocol::PageRotation::Degrees180 => PageRotation::Degrees180,
        pdf_rs_protocol::PageRotation::Degrees270 => PageRotation::Degrees270,
    };
    RenderPlanRequest::new(
        command.viewport.generation,
        clip,
        zoom,
        command.viewport.device_scale_milli,
        rotation,
        OptionalContentIdentity::new(command.viewport.optional_content_id),
        command.viewport.annotation_revision,
    )
    .map_err(|_| policy())
}

fn device_clip(
    command: &SetViewportCommand,
    page: &pdf_rs_protocol::PageViewport,
    scene: &Scene,
) -> Result<DeviceRect, EngineIntegrationError> {
    let geometry = canonical_page_geometry(scene)?;
    if geometry != page.geometry {
        return Err(identity_mismatch());
    }

    const SCENE_SCALED_PER_MILLI_POINT: i128 = 1_000_000;
    const SCENE_SCALED_PER_POINT: i128 = 1_000_000_000;
    let clip_left = i128::from(page.clip_x_milli_points)
        .checked_mul(SCENE_SCALED_PER_MILLI_POINT)
        .ok_or_else(policy)?;
    let clip_bottom = i128::from(page.clip_y_milli_points)
        .checked_mul(SCENE_SCALED_PER_MILLI_POINT)
        .ok_or_else(policy)?;
    let clip_right = clip_left
        .checked_add(
            i128::from(page.clip_width_milli_points)
                .checked_mul(SCENE_SCALED_PER_MILLI_POINT)
                .ok_or_else(policy)?,
        )
        .ok_or_else(policy)?;
    let clip_top = clip_bottom
        .checked_add(
            i128::from(page.clip_height_milli_points)
                .checked_mul(SCENE_SCALED_PER_MILLI_POINT)
                .ok_or_else(policy)?,
        )
        .ok_or_else(policy)?;
    let [crop_left, crop_bottom, crop_right, crop_top] = scene
        .geometry()
        .crop_box()
        .coordinates()
        .map(|coordinate| i128::from(coordinate.scaled()));
    let crop_width = crop_right.checked_sub(crop_left).ok_or_else(policy)?;
    let crop_height = crop_top.checked_sub(crop_bottom).ok_or_else(policy)?;

    let combined_rotation = rotation_quarters(geometry.intrinsic_rotation)
        .checked_add(rotation_quarters(command.viewport.rotation))
        .ok_or_else(policy)?
        % 4;
    let mut minimum_x = i128::MAX;
    let mut minimum_y = i128::MAX;
    let mut maximum_x = i128::MIN;
    let mut maximum_y = i128::MIN;
    for (x, y) in [
        (clip_left, clip_bottom),
        (clip_left, clip_top),
        (clip_right, clip_bottom),
        (clip_right, clip_top),
    ] {
        let unrotated_x = x.checked_sub(crop_left).ok_or_else(policy)?;
        let unrotated_y = crop_top.checked_sub(y).ok_or_else(policy)?;
        let (rotated_x, rotated_y) = match combined_rotation {
            0 => (unrotated_x, unrotated_y),
            1 => (
                crop_height.checked_sub(unrotated_y).ok_or_else(policy)?,
                unrotated_x,
            ),
            2 => (
                crop_width.checked_sub(unrotated_x).ok_or_else(policy)?,
                crop_height.checked_sub(unrotated_y).ok_or_else(policy)?,
            ),
            3 => (
                unrotated_y,
                crop_width.checked_sub(unrotated_x).ok_or_else(policy)?,
            ),
            _ => return Err(internal()),
        };
        minimum_x = minimum_x.min(rotated_x);
        minimum_y = minimum_y.min(rotated_y);
        maximum_x = maximum_x.max(rotated_x);
        maximum_y = maximum_y.max(rotated_y);
    }

    let denominator = i128::from(command.viewport.zoom_denominator)
        .checked_mul(1_000)
        .and_then(|value| value.checked_mul(SCENE_SCALED_PER_POINT))
        .ok_or_else(policy)?;
    let numerator = i128::from(command.viewport.zoom_numerator)
        .checked_mul(i128::from(command.viewport.device_scale_milli))
        .ok_or_else(policy)?;
    let scale_floor = |value: i128| -> Result<i128, EngineIntegrationError> {
        let product = value.checked_mul(numerator).ok_or_else(policy)?;
        Ok(floor_div(product, denominator))
    };
    let scale_ceil = |value: i128| -> Result<i128, EngineIntegrationError> {
        let product = value.checked_mul(numerator).ok_or_else(policy)?;
        Ok(ceil_div_signed(product, denominator))
    };
    let left = scale_floor(minimum_x)?;
    let top = scale_floor(minimum_y)?;
    let right = scale_ceil(maximum_x)?;
    let bottom = scale_ceil(maximum_y)?;
    let width = right.checked_sub(left).ok_or_else(policy)?;
    let height = bottom.checked_sub(top).ok_or_else(policy)?;
    DeviceRect::new(
        i32::try_from(left).map_err(|_| policy())?,
        i32::try_from(top).map_err(|_| policy())?,
        u32::try_from(width).map_err(|_| policy())?,
        u32::try_from(height).map_err(|_| policy())?,
    )
    .map_err(|_| policy())
}

fn canonical_page_geometry(scene: &Scene) -> Result<ProtocolPageGeometry, EngineIntegrationError> {
    let geometry = scene.geometry();
    let media_box = geometry.media_box();
    let crop_box = geometry.crop_box();
    let [media_left, media_bottom, _, _] = media_box.coordinates();
    let [crop_left, crop_bottom, _, _] = crop_box.coordinates();
    Ok(ProtocolPageGeometry {
        identity: page_geometry_identity(scene).map_err(|_| policy())?,
        media_box_x_milli_points: scene_coordinate_milli_points(media_left.scaled())?,
        media_box_y_milli_points: scene_coordinate_milli_points(media_bottom.scaled())?,
        media_box_width_milli_points: scene_extent_milli_points(
            media_box.width().map_err(|_| policy())?.scaled(),
        )?,
        media_box_height_milli_points: scene_extent_milli_points(
            media_box.height().map_err(|_| policy())?.scaled(),
        )?,
        crop_box_x_milli_points: scene_coordinate_milli_points(crop_left.scaled())?,
        crop_box_y_milli_points: scene_coordinate_milli_points(crop_bottom.scaled())?,
        crop_box_width_milli_points: scene_extent_milli_points(
            crop_box.width().map_err(|_| policy())?.scaled(),
        )?,
        crop_box_height_milli_points: scene_extent_milli_points(
            crop_box.height().map_err(|_| policy())?.scaled(),
        )?,
        intrinsic_rotation: match geometry.rotation() {
            PageRotation::Degrees0 => ProtocolPageRotation::Degrees0,
            PageRotation::Degrees90 => ProtocolPageRotation::Degrees90,
            PageRotation::Degrees180 => ProtocolPageRotation::Degrees180,
            PageRotation::Degrees270 => ProtocolPageRotation::Degrees270,
        },
    })
}

fn scene_coordinate_milli_points(scaled: i64) -> Result<i32, EngineIntegrationError> {
    let rounded = round_half_away_from_zero(i128::from(scaled), 1_000_000);
    i32::try_from(rounded).map_err(|_| policy())
}

fn scene_extent_milli_points(scaled: i64) -> Result<u32, EngineIntegrationError> {
    let rounded = round_half_away_from_zero(i128::from(scaled), 1_000_000);
    let extent = u32::try_from(rounded).map_err(|_| policy())?;
    if extent == 0 {
        return Err(policy());
    }
    Ok(extent)
}

fn round_half_away_from_zero(value: i128, divisor: i128) -> i128 {
    let quotient = value / divisor;
    let remainder = value % divisor;
    if remainder.unsigned_abs() * 2 >= divisor.unsigned_abs() {
        quotient + if value.is_negative() { -1 } else { 1 }
    } else {
        quotient
    }
}

const fn rotation_quarters(rotation: ProtocolPageRotation) -> i64 {
    match rotation {
        ProtocolPageRotation::Degrees0 => 0,
        ProtocolPageRotation::Degrees90 => 1,
        ProtocolPageRotation::Degrees180 => 2,
        ProtocolPageRotation::Degrees270 => 3,
    }
}

fn floor_div(value: i128, divisor: i128) -> i128 {
    let quotient = value / divisor;
    let remainder = value % divisor;
    if remainder != 0 && value.is_negative() {
        quotient - 1
    } else {
        quotient
    }
}

fn ceil_div_signed(value: i128, divisor: i128) -> i128 {
    let quotient = value / divisor;
    let remainder = value % divisor;
    if remainder != 0 && value.is_positive() {
        quotient + 1
    } else {
        quotient
    }
}
