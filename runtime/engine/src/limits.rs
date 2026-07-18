use pdf_rs_fast_raster::fast::{FastRasterJobLimits, FastRasterLimits};
use pdf_rs_policy::{PolicyJobLimits, PolicyLimits};
use pdf_rs_protocol::{ProtocolLimits, WorkerId};
use pdf_rs_scheduler::SchedulerLimits;
use pdf_rs_surface::{SurfaceLimits, WorkerEpoch};
use pdf_rs_tile_cache::TileCacheLimits;

use crate::EngineIntegrationError;
use crate::error::invalid_config;

const HARD_MAX_SCENES_PER_OPEN: usize = 1_000_000;
const HARD_MAX_INTEGRATION_QUEUE_CAPACITY: usize = 1_000_000;
const HARD_MAX_RETAINED_POLICY_JOB_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_RASTER_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_CACHE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_SCENE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Unvalidated integration capacities and component limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeWorkerLimitConfig {
    /// Maximum queued parser/range/raster completions in each work queue.
    pub reentry_capacity: usize,
    /// Maximum queued cancel/release/close/shutdown/source-change/restart messages.
    pub lifecycle_reentry_capacity: usize,
    /// Maximum undelivered critical protocol/lifecycle events.
    pub critical_event_capacity: usize,
    /// Maximum coalesced viewport/progress events.
    pub progress_event_capacity: usize,
    /// Maximum request identities remembered for one Worker epoch.
    pub request_history_capacity: usize,
    /// Maximum pending complete raster resources awaiting terminal arbitration.
    pub pending_resource_capacity: usize,
    /// Maximum aggregate owned policy-job bytes across queued, external, and
    /// completed capability or planning tasks.
    pub retained_policy_job_byte_capacity: u64,
    /// Maximum aggregate Fast working plus retained bytes across external tasks
    /// and actor queues.
    pub retained_raster_byte_capacity: u64,
    /// Maximum aggregate tile-cache bytes retained across every Session.
    pub retained_cache_byte_capacity: u64,
    /// Maximum aggregate Scene payload and integration-ownership bytes retained
    /// in queues and Sessions.
    pub retained_scene_byte_capacity: u64,
    /// Maximum Scene payload and integration-ownership bytes one parser/Open
    /// operation may produce.
    pub max_scene_bytes_per_open: u64,
    /// Maximum immutable page Scenes retained by one Open completion.
    pub max_scenes_per_open: usize,
    /// Protocol validation limits.
    pub protocol: ProtocolLimits,
    /// Scheduler capacities and fairness.
    pub scheduler: SchedulerLimits,
    /// Capability and RenderPlan limits.
    pub policy: PolicyLimits,
    /// Owned capability and RenderPlan job allocation ceilings.
    pub policy_job: PolicyJobLimits,
    /// Fast CPU raster limits.
    pub raster: FastRasterLimits,
    /// Owned resumable Fast CPU raster job limits.
    pub raster_job: FastRasterJobLimits,
    /// Per-session product tile-cache limits.
    pub cache: TileCacheLimits,
    /// Worker/session Surface lifecycle limits.
    pub surface: SurfaceLimits,
}

impl Default for NativeWorkerLimitConfig {
    fn default() -> Self {
        Self {
            reentry_capacity: 256,
            lifecycle_reentry_capacity: 64,
            critical_event_capacity: 256,
            progress_event_capacity: 128,
            request_history_capacity: 4_096,
            pending_resource_capacity: 32,
            retained_policy_job_byte_capacity: 1024 * 1024 * 1024,
            retained_raster_byte_capacity: 1024 * 1024 * 1024,
            retained_cache_byte_capacity: 1024 * 1024 * 1024,
            retained_scene_byte_capacity: 1024 * 1024 * 1024,
            max_scene_bytes_per_open: 512 * 1024 * 1024,
            max_scenes_per_open: 25_000,
            protocol: ProtocolLimits::default(),
            scheduler: SchedulerLimits::new(128, 1, 32, 128, 32, 16, 4_096, 1, 4, 4)
                .expect("crate-owned scheduler defaults are valid"),
            policy: PolicyLimits::default(),
            policy_job: PolicyJobLimits::default(),
            raster: FastRasterLimits::default(),
            raster_job: FastRasterJobLimits::default(),
            cache: TileCacheLimits::default(),
            surface: SurfaceLimits::default(),
        }
    }
}

/// Validated identity and capacity configuration for one Worker epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeWorkerConfig {
    worker: WorkerId,
    worker_epoch: WorkerEpoch,
    renderer_epoch: u32,
    limits: NativeWorkerLimitConfig,
}

impl NativeWorkerConfig {
    /// Validates one exact nonzero Worker and renderer epoch.
    pub fn new(
        worker: WorkerId,
        worker_epoch: WorkerEpoch,
        renderer_epoch: u32,
        limits: NativeWorkerLimitConfig,
    ) -> Result<Self, EngineIntegrationError> {
        let Some(raster_task_byte_reservation) = limits
            .raster
            .max_retained_bytes()
            .checked_add(limits.raster.max_intermediate_bytes())
            .and_then(|bytes| bytes.checked_add(limits.policy_job.max_retained_bytes()))
        else {
            return Err(invalid_config());
        };
        let Some(shutdown_event_capacity) = limits.scheduler.max_sessions().checked_add(1) else {
            return Err(invalid_config());
        };
        let required_critical_events = shutdown_event_capacity.max(3);
        if worker.value() == 0
            || renderer_epoch == 0
            || limits.reentry_capacity == 0
            || limits.reentry_capacity > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.lifecycle_reentry_capacity == 0
            || limits.lifecycle_reentry_capacity > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.critical_event_capacity < required_critical_events
            || limits.critical_event_capacity > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.progress_event_capacity == 0
            || limits.progress_event_capacity > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.request_history_capacity == 0
            || limits.request_history_capacity > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.pending_resource_capacity == 0
            || limits.pending_resource_capacity > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.retained_policy_job_byte_capacity == 0
            || limits.retained_policy_job_byte_capacity > HARD_MAX_RETAINED_POLICY_JOB_BYTES
            || limits.retained_policy_job_byte_capacity < limits.policy_job.max_retained_bytes()
            || limits.retained_raster_byte_capacity == 0
            || limits.retained_raster_byte_capacity > HARD_MAX_RETAINED_RASTER_BYTES
            || limits.retained_raster_byte_capacity < raster_task_byte_reservation
            || limits.retained_cache_byte_capacity == 0
            || limits.retained_cache_byte_capacity > HARD_MAX_RETAINED_CACHE_BYTES
            || limits.retained_scene_byte_capacity == 0
            || limits.retained_scene_byte_capacity > HARD_MAX_RETAINED_SCENE_BYTES
            || limits.max_scene_bytes_per_open == 0
            || limits.max_scene_bytes_per_open > limits.retained_scene_byte_capacity
            || limits.max_scenes_per_open == 0
            || limits.max_scenes_per_open > HARD_MAX_SCENES_PER_OPEN
            || limits.scheduler.normal_capacity() > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.scheduler.per_session_reservation() > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.scheduler.per_session_capacity() > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.scheduler.critical_capacity() > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.scheduler.in_flight_capacity() > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.scheduler.max_sessions() > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.scheduler.max_work_ids_per_epoch() > HARD_MAX_INTEGRATION_QUEUE_CAPACITY
            || limits.pending_resource_capacity > limits.scheduler.in_flight_capacity()
            || limits.scheduler.max_sessions() > limits.surface.max_sessions_per_epoch()
            || limits.scheduler.max_sessions() > pdf_rs_protocol::MAX_OUTSTANDING_DATA_TICKETS
        {
            return Err(invalid_config());
        }
        Ok(Self {
            worker,
            worker_epoch,
            renderer_epoch,
            limits,
        })
    }

    /// Returns the protocol Worker identity.
    pub const fn worker(self) -> WorkerId {
        self.worker
    }

    /// Returns the process Worker epoch.
    pub const fn worker_epoch(self) -> WorkerEpoch {
        self.worker_epoch
    }

    /// Returns the nonzero Native renderer epoch.
    pub const fn renderer_epoch(self) -> u32 {
        self.renderer_epoch
    }

    /// Returns all validated component and queue limits.
    pub const fn limits(self) -> NativeWorkerLimitConfig {
        self.limits
    }

    pub(crate) fn raster_task_byte_reservation(self) -> u64 {
        self.limits
            .raster
            .max_retained_bytes()
            .checked_add(self.limits.raster.max_intermediate_bytes())
            .and_then(|bytes| bytes.checked_add(self.limits.policy_job.max_retained_bytes()))
            .expect("validated Native Worker raster limits have a bounded sum")
    }
}
