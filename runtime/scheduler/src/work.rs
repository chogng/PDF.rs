//! Work metadata, total scheduling keys, critical traffic, and terminal decisions.

use crate::{Generation, ResourceId, SessionId, WorkId};

/// P0 through P4 normal-work priority, where P0 is most urgent.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Priority {
    /// Visible response-blocking work.
    P0 = 0,
    /// Visible high-priority work.
    P1 = 1,
    /// Near-viewport work.
    P2 = 2,
    /// Predictive work.
    P3 = 3,
    /// Background work.
    P4 = 4,
}

impl Priority {
    pub(crate) const fn rank(self) -> u8 {
        self as u8
    }
}

/// Relation of a tile to the caller's predicted scroll direction.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ScrollRelation {
    /// The work lies ahead of predicted movement.
    Ahead = 0,
    /// The work is direction-neutral or intersects the current viewport.
    Neutral = 1,
    /// The work lies behind predicted movement.
    Behind = 2,
}

impl ScrollRelation {
    const fn rank(self) -> u8 {
        self as u8
    }
}

/// A saturated geometry distance in caller-defined canonical units.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Distance(u32);

impl Distance {
    /// Creates a distance.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the canonical distance value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// The replaceable normal-work class.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ReplaceableKind {
    /// Recomputes one logical viewport plan.
    Viewport,
    /// Produces one logical tile.
    Tile,
}

/// A session-local coalescing identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ReplaceKey {
    kind: ReplaceableKind,
    identity: u64,
}

impl ReplaceKey {
    /// Creates a viewport replacement key.
    #[must_use]
    pub const fn viewport(identity: u64) -> Self {
        Self {
            kind: ReplaceableKind::Viewport,
            identity,
        }
    }

    /// Creates a tile replacement key.
    #[must_use]
    pub const fn tile(identity: u64) -> Self {
        Self {
            kind: ReplaceableKind::Tile,
            identity,
        }
    }

    /// Returns the work class.
    #[must_use]
    pub const fn kind(self) -> ReplaceableKind {
        self.kind
    }

    /// Returns the opaque session-local identity.
    #[must_use]
    pub const fn identity(self) -> u64 {
        self.identity
    }
}

/// Metadata submitted to the replaceable normal queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkRequest {
    /// Never-reused work identity.
    pub work_id: WorkId,
    /// Owning session.
    pub session_id: SessionId,
    /// Viewport generation.
    pub generation: Generation,
    /// Session-local viewport or tile coalescing identity.
    pub replace_key: ReplaceKey,
    /// Base P0-P4 priority.
    pub priority: Priority,
    /// Distance from the visible center.
    pub center_distance: Distance,
    /// Distance from the nearest visible edge.
    pub edge_distance: Distance,
    /// Predicted scroll relation.
    pub scroll_relation: ScrollRelation,
}

/// A lexicographically ordered, unique scheduling key.
///
/// `aging_lane == 0` identifies work that reached the bounded aging cap.
/// Such work is ordered by original enqueue order before geometry and precedes
/// non-capped work. Other work uses effective priority, predicted direction,
/// center distance, edge distance, and enqueue order. Session and work IDs make
/// the key total even when every scheduling attribute ties.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SchedulingKey {
    aging_lane: u8,
    effective_priority: u8,
    capped_enqueue_order: u64,
    scroll_relation: u8,
    center_distance: u32,
    edge_distance: u32,
    base_priority: u8,
    enqueue_order: u64,
    session_id: u64,
    work_id: u64,
}

impl SchedulingKey {
    /// Returns zero for capped-aging work and one otherwise.
    #[must_use]
    pub const fn aging_lane(self) -> u8 {
        self.aging_lane
    }

    /// Returns the priority rank after bounded aging credit.
    #[must_use]
    pub const fn effective_priority(self) -> u8 {
        self.effective_priority
    }

    /// Returns the immutable enqueue order.
    #[must_use]
    pub const fn enqueue_order(self) -> u64 {
        self.enqueue_order
    }
}

/// A normal work item selected for execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduledWork {
    /// Original or coalesced request metadata.
    pub request: WorkRequest,
    /// Total key evaluated at the dispatch virtual tick.
    pub scheduling_key: SchedulingKey,
    /// Cross-session fairness state which admitted this candidate.
    pub fairness: FairnessEvidence,
}

/// Evidence for the bounded cross-session service choice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FairnessEvidence {
    /// Oldest last-service turn among currently backlogged sessions.
    pub minimum_last_service_turn: u64,
    /// Owning session's last-service turn before this dispatch.
    pub session_last_service_turn: u64,
    /// Exclusive admitted turn, `minimum + fairness_burst`.
    pub exclusive_turn_limit: u64,
}

/// A critical lifecycle event class.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CriticalKind {
    /// Cancel an in-flight work identity.
    Cancel,
    /// Close one session.
    Close,
    /// Release an externally owned resource.
    Release,
    /// Terminate in-flight work with failure.
    Failure,
    /// Arbitrate a completed resource.
    Completion,
    /// Stop the whole scheduler epoch.
    Shutdown,
}

/// Dedicated critical-queue ingress.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CriticalIngress {
    /// Requests cancellation of exact in-flight work.
    Cancel(TerminalSignal),
    /// Finishes a session close after close admission already stopped normal work.
    Close {
        /// Session being closed.
        session_id: SessionId,
    },
    /// Relays a release which normal work may not crowd out.
    Release {
        /// Session which owned the resource.
        session_id: SessionId,
        /// Resource to release.
        resource_id: ResourceId,
    },
    /// Reports exact in-flight work failure.
    Failure(TerminalSignal),
    /// Reports an exact completed resource for arbitration.
    Completion {
        /// Exact work identity.
        signal: TerminalSignal,
        /// Completed resource whose ownership enters the critical queue.
        resource_id: ResourceId,
    },
    /// Completes whole-scheduler shutdown.
    Shutdown,
}

impl CriticalIngress {
    /// Returns the event class.
    #[must_use]
    pub const fn kind(&self) -> CriticalKind {
        match self {
            Self::Cancel(_) => CriticalKind::Cancel,
            Self::Close { .. } => CriticalKind::Close,
            Self::Release { .. } => CriticalKind::Release,
            Self::Failure(_) => CriticalKind::Failure,
            Self::Completion { .. } => CriticalKind::Completion,
            Self::Shutdown => CriticalKind::Shutdown,
        }
    }

    pub(crate) const fn session_id(&self) -> Option<SessionId> {
        match self {
            Self::Cancel(signal) | Self::Failure(signal) | Self::Completion { signal, .. } => {
                Some(signal.session_id)
            }
            Self::Close { session_id } | Self::Release { session_id, .. } => Some(*session_id),
            Self::Shutdown => None,
        }
    }
}

/// Exact identity attached to cancel, failure, or completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSignal {
    /// Work identity.
    pub work_id: WorkId,
    /// Owning session identity.
    pub session_id: SessionId,
    /// Viewport generation at dispatch.
    pub generation: Generation,
}

/// Why a completed resource cannot enter the visible stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionDiscardReason {
    /// No matching in-flight identity remains, including duplicate completion.
    UnknownOrAlreadyTerminal,
    /// Session or generation fields do not match the in-flight identity.
    IdentityMismatch,
    /// A newer generation superseded this exact in-flight work.
    StaleGeneration,
    /// The owning session began close.
    SessionClosing,
    /// Whole-scheduler shutdown began.
    SchedulerShuttingDown,
}

/// The single terminal arbiter's decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalDecision {
    /// Exact current work may publish this resource once.
    Publish {
        /// Terminal work identity.
        work_id: WorkId,
        /// Publishable resource identity.
        resource_id: ResourceId,
    },
    /// The caller must release this completed resource without publishing it.
    DiscardAndRelease {
        /// Reported work identity.
        work_id: WorkId,
        /// Resource which must be released.
        resource_id: ResourceId,
        /// Deterministic discard reason.
        reason: CompletionDiscardReason,
    },
    /// Exact in-flight work reached cancellation.
    Cancelled {
        /// Terminal work identity.
        work_id: WorkId,
    },
    /// Exact in-flight work reached failure.
    Failed {
        /// Terminal work identity.
        work_id: WorkId,
    },
    /// A cancel or failure did not match live exact work.
    Ignored {
        /// Reported work identity.
        work_id: WorkId,
        /// Deterministic mismatch reason.
        reason: CompletionDiscardReason,
    },
}

/// Critical work after FIFO dispatch and, where required, terminal arbitration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CriticalDispatch {
    /// A cancellation decision.
    Cancel(TerminalDecision),
    /// A session's queued close marker.
    Close {
        /// Closed session.
        session_id: SessionId,
    },
    /// An external resource release.
    Release {
        /// Owning session.
        session_id: SessionId,
        /// Resource to release.
        resource_id: ResourceId,
    },
    /// A failure decision.
    Failure(TerminalDecision),
    /// A completion publication or discard decision.
    Completion(TerminalDecision),
    /// The scheduler shutdown marker.
    Shutdown,
}

impl SchedulingKey {
    pub(crate) fn for_request(
        request: WorkRequest,
        enqueue_order: u64,
        enqueue_tick: u64,
        current_tick: u64,
        aging_quantum_ticks: u64,
        max_aging_steps: u8,
    ) -> Self {
        let age = current_tick.saturating_sub(enqueue_tick);
        let raw_steps = age / aging_quantum_ticks;
        let steps = match u8::try_from(raw_steps.min(u64::from(max_aging_steps))) {
            Ok(steps) => steps,
            Err(_) => max_aging_steps,
        };
        let capped = steps == max_aging_steps;
        Self {
            aging_lane: u8::from(!capped),
            effective_priority: request.priority.rank().saturating_sub(steps),
            capped_enqueue_order: if capped { enqueue_order } else { 0 },
            scroll_relation: request.scroll_relation.rank(),
            center_distance: request.center_distance.get(),
            edge_distance: request.edge_distance.get(),
            base_priority: request.priority.rank(),
            enqueue_order,
            session_id: request.session_id.get(),
            work_id: request.work_id.get(),
        }
    }
}
