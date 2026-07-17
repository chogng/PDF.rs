//! Typed fail-closed configuration and admission errors.

use crate::{CriticalIngress, Generation, SessionId, WorkId};

/// An invalid scheduler limit relationship.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitConfigError {
    /// The normal queue would accept no work.
    ZeroNormalCapacity,
    /// A session would have no reserved slot.
    ZeroSessionReservation,
    /// A session would accept no queued work.
    ZeroSessionCapacity,
    /// Lifecycle traffic would have no dedicated slot.
    ZeroCriticalCapacity,
    /// No normal work could enter execution.
    ZeroInFlightCapacity,
    /// No session could be registered.
    ZeroSessionLimit,
    /// No work identity could be retained.
    ZeroWorkIdCapacity,
    /// Aging would divide by zero virtual ticks.
    ZeroAgingQuantum,
    /// Aging would have no bounded progression.
    ZeroAgingSteps,
    /// Fairness would allow no service.
    ZeroFairnessBurst,
    /// A session reservation is larger than its queue limit.
    ReservationExceedsSessionCapacity,
    /// The configured session reservations do not fit the normal queue.
    ReservationsExceedNormalCapacity,
    /// The work-ID history cannot cover all queued and in-flight work.
    WorkIdCapacityBelowLiveBound,
    /// Checked capacity arithmetic overflowed.
    CapacityArithmeticOverflow,
}

/// A session registration rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionRegistrationError {
    /// This non-reusable session identity is already registered.
    DuplicateSession(SessionId),
    /// The bounded session registry is full.
    SessionLimitReached,
    /// Existing shared work has consumed space needed to precharge this reservation.
    ReservationUnavailable,
    /// The scheduler has begun shutdown.
    SchedulerShuttingDown,
}

/// A replaceable normal-work admission rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkAdmissionError {
    /// The session identity is not registered.
    UnknownSession(SessionId),
    /// The session no longer accepts normal work.
    SessionClosing(SessionId),
    /// The scheduler no longer accepts normal work.
    SchedulerShuttingDown,
    /// This generation is older than the session's current generation.
    SupersededGeneration {
        /// Rejected request generation.
        submitted: Generation,
        /// Current authoritative generation.
        current: Generation,
    },
    /// A generation failed to increase monotonically.
    NonIncreasingGeneration {
        /// Requested generation.
        requested: Generation,
        /// Current authoritative generation.
        current: Generation,
    },
    /// The identity was already used by different queued or in-flight work.
    DuplicateWorkId(WorkId),
    /// The bounded never-reuse identity history is full.
    WorkIdHistoryFull,
    /// The session's own queued capacity is full.
    SessionQueueFull(SessionId),
    /// Shared capacity is unavailable after preserving every other reservation.
    ReservedNormalCapacity,
    /// An internal monotonic counter reached its representable end.
    CounterExhausted,
}

/// A critical ingress rejection which returns ownership of the event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CriticalAdmissionError {
    /// The dedicated critical queue is full; the caller still owns the event.
    QueueFull(CriticalIngress),
    /// The event names a session which was never registered.
    UnknownSession(CriticalIngress),
    /// The scheduler has fully terminated and cannot own another event.
    SchedulerTerminated(CriticalIngress),
    /// The FIFO order counter is exhausted; the caller still owns the event.
    CounterExhausted(CriticalIngress),
}

/// A direct terminal-arbiter state transition rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalArbiterError {
    /// This session identity is already registered.
    DuplicateSession(SessionId),
    /// The arbiter's session registry is full.
    SessionLimitReached,
    /// The named session is unknown.
    UnknownSession(SessionId),
    /// The requested generation did not increase.
    NonIncreasingGeneration {
        /// Requested generation.
        requested: Generation,
        /// Current authoritative generation.
        current: Generation,
    },
    /// The session has begun close.
    SessionClosing(SessionId),
    /// Whole-arbiter shutdown has begun.
    SchedulerShuttingDown,
    /// The independent in-flight bound is full.
    InFlightCapacityReached,
    /// This work identity is already in flight.
    DuplicateInFlightWork(WorkId),
}

/// A fail-closed dispatch or arbitration error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerError {
    /// A monotonic ordering or fairness counter was exhausted.
    CounterExhausted,
    /// Internal bounded state disagreed with a validated capacity.
    InvariantViolation,
}
