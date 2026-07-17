//! Validated scheduler capacity and fairness limits.

use crate::LimitConfigError;

/// Capacity, reservation, aging, and fairness configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerLimits {
    normal_capacity: usize,
    per_session_reservation: usize,
    per_session_capacity: usize,
    critical_capacity: usize,
    in_flight_capacity: usize,
    max_sessions: usize,
    max_work_ids_per_epoch: usize,
    aging_quantum_ticks: u64,
    max_aging_steps: u8,
    fairness_burst: u64,
}

impl SchedulerLimits {
    /// Validates and creates scheduler limits.
    ///
    /// Every capacity and time quantum must be nonzero. Per-session capacity
    /// must cover its reservation, all session reservations must fit the normal
    /// queue, and the work-ID history must cover queued plus in-flight work.
    ///
    /// # Errors
    ///
    /// Returns [`LimitConfigError`] for a zero value, an invalid capacity
    /// relationship, or checked capacity arithmetic overflow.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        normal_capacity: usize,
        per_session_reservation: usize,
        per_session_capacity: usize,
        critical_capacity: usize,
        in_flight_capacity: usize,
        max_sessions: usize,
        max_work_ids_per_epoch: usize,
        aging_quantum_ticks: u64,
        max_aging_steps: u8,
        fairness_burst: u64,
    ) -> Result<Self, LimitConfigError> {
        if normal_capacity == 0 {
            return Err(LimitConfigError::ZeroNormalCapacity);
        }
        if per_session_reservation == 0 {
            return Err(LimitConfigError::ZeroSessionReservation);
        }
        if per_session_capacity == 0 {
            return Err(LimitConfigError::ZeroSessionCapacity);
        }
        if critical_capacity == 0 {
            return Err(LimitConfigError::ZeroCriticalCapacity);
        }
        if in_flight_capacity == 0 {
            return Err(LimitConfigError::ZeroInFlightCapacity);
        }
        if max_sessions == 0 {
            return Err(LimitConfigError::ZeroSessionLimit);
        }
        if max_work_ids_per_epoch == 0 {
            return Err(LimitConfigError::ZeroWorkIdCapacity);
        }
        if aging_quantum_ticks == 0 {
            return Err(LimitConfigError::ZeroAgingQuantum);
        }
        if max_aging_steps == 0 {
            return Err(LimitConfigError::ZeroAgingSteps);
        }
        if fairness_burst == 0 {
            return Err(LimitConfigError::ZeroFairnessBurst);
        }
        if per_session_reservation > per_session_capacity {
            return Err(LimitConfigError::ReservationExceedsSessionCapacity);
        }
        let reserved = per_session_reservation
            .checked_mul(max_sessions)
            .ok_or(LimitConfigError::CapacityArithmeticOverflow)?;
        if reserved > normal_capacity {
            return Err(LimitConfigError::ReservationsExceedNormalCapacity);
        }
        let live_bound = normal_capacity
            .checked_add(in_flight_capacity)
            .ok_or(LimitConfigError::CapacityArithmeticOverflow)?;
        if max_work_ids_per_epoch < live_bound {
            return Err(LimitConfigError::WorkIdCapacityBelowLiveBound);
        }
        Ok(Self {
            normal_capacity,
            per_session_reservation,
            per_session_capacity,
            critical_capacity,
            in_flight_capacity,
            max_sessions,
            max_work_ids_per_epoch,
            aging_quantum_ticks,
            max_aging_steps,
            fairness_burst,
        })
    }

    /// Returns the total queued normal-work capacity.
    #[must_use]
    pub const fn normal_capacity(self) -> usize {
        self.normal_capacity
    }

    /// Returns the precharged queued slots per registered session.
    #[must_use]
    pub const fn per_session_reservation(self) -> usize {
        self.per_session_reservation
    }

    /// Returns the maximum queued work for any one session.
    #[must_use]
    pub const fn per_session_capacity(self) -> usize {
        self.per_session_capacity
    }

    /// Returns the dedicated critical-event capacity.
    #[must_use]
    pub const fn critical_capacity(self) -> usize {
        self.critical_capacity
    }

    /// Returns the global in-flight normal-work capacity.
    #[must_use]
    pub const fn in_flight_capacity(self) -> usize {
        self.in_flight_capacity
    }

    /// Returns the maximum session registry size.
    #[must_use]
    pub const fn max_sessions(self) -> usize {
        self.max_sessions
    }

    /// Returns the never-reused work-ID history capacity.
    #[must_use]
    pub const fn max_work_ids_per_epoch(self) -> usize {
        self.max_work_ids_per_epoch
    }

    /// Returns the virtual ticks per aging step.
    #[must_use]
    pub const fn aging_quantum_ticks(self) -> u64 {
        self.aging_quantum_ticks
    }

    /// Returns the maximum bounded aging credit.
    #[must_use]
    pub const fn max_aging_steps(self) -> u8 {
        self.max_aging_steps
    }

    /// Returns the maximum service lead of one backlogged session.
    #[must_use]
    pub const fn fairness_burst(self) -> u64 {
        self.fairness_burst
    }
}
