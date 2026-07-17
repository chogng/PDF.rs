//! The sole completion-to-publication terminal arbiter.

use std::collections::BTreeMap;

use crate::{
    CompletionDiscardReason, Generation, ResourceId, ScheduledWork, SessionId,
    TerminalArbiterError, TerminalDecision, TerminalSignal, WorkId,
};

/// Independent bounds for terminal arbitration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalArbiterLimits {
    max_sessions: usize,
    max_in_flight: usize,
}

impl TerminalArbiterLimits {
    /// Creates nonzero arbiter bounds.
    #[must_use]
    pub const fn new(max_sessions: usize, max_in_flight: usize) -> Option<Self> {
        if max_sessions == 0 || max_in_flight == 0 {
            None
        } else {
            Some(Self {
                max_sessions,
                max_in_flight,
            })
        }
    }

    pub(crate) const fn from_validated_scheduler(
        max_sessions: usize,
        max_in_flight: usize,
    ) -> Self {
        Self {
            max_sessions,
            max_in_flight,
        }
    }

    /// Returns the bounded session count.
    #[must_use]
    pub const fn max_sessions(self) -> usize {
        self.max_sessions
    }

    /// Returns the bounded in-flight count.
    #[must_use]
    pub const fn max_in_flight(self) -> usize {
        self.max_in_flight
    }
}

/// Terminal lifecycle of one registered session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalSessionPhase {
    /// The session accepts and may publish current-generation work.
    Open,
    /// The session accepts no new work and no completion may publish.
    Closing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TerminalSession {
    generation: Generation,
    phase: TerminalSessionPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InFlight {
    signal: TerminalSignal,
}

/// Bounded owner of in-flight identities and all terminal decisions.
///
/// Work enters through [`Self::start`]. Exact completion is removed once and
/// becomes publishable only while its session is open, its generation remains
/// current, and shutdown has not begun. Every other completion returns
/// [`TerminalDecision::DiscardAndRelease`].
#[derive(Debug)]
pub struct TerminalArbiter {
    limits: TerminalArbiterLimits,
    sessions: BTreeMap<SessionId, TerminalSession>,
    in_flight: BTreeMap<WorkId, InFlight>,
    shutting_down: bool,
}

impl TerminalArbiter {
    /// Creates an empty terminal arbiter.
    #[must_use]
    pub fn new(limits: TerminalArbiterLimits) -> Self {
        Self {
            limits,
            sessions: BTreeMap::new(),
            in_flight: BTreeMap::new(),
            shutting_down: false,
        }
    }

    /// Registers a never-reused session and its initial generation.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalArbiterError`] when shutdown began, the identity was
    /// already registered, or the bounded registry is full.
    pub fn register_session(
        &mut self,
        session_id: SessionId,
        generation: Generation,
    ) -> Result<(), TerminalArbiterError> {
        if self.shutting_down {
            return Err(TerminalArbiterError::SchedulerShuttingDown);
        }
        if self.sessions.contains_key(&session_id) {
            return Err(TerminalArbiterError::DuplicateSession(session_id));
        }
        if self.sessions.len() == self.limits.max_sessions {
            return Err(TerminalArbiterError::SessionLimitReached);
        }
        self.sessions.insert(
            session_id,
            TerminalSession {
                generation,
                phase: TerminalSessionPhase::Open,
            },
        );
        Ok(())
    }

    /// Returns a session's authoritative generation.
    #[must_use]
    pub fn current_generation(&self, session_id: SessionId) -> Option<Generation> {
        self.sessions.get(&session_id).map(|state| state.generation)
    }

    /// Returns a session's terminal lifecycle.
    #[must_use]
    pub fn session_phase(&self, session_id: SessionId) -> Option<TerminalSessionPhase> {
        self.sessions.get(&session_id).map(|state| state.phase)
    }

    /// Advances a session generation monotonically.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalArbiterError`] for an unknown or closing session or
    /// when `generation` does not strictly increase.
    pub fn advance_generation(
        &mut self,
        session_id: SessionId,
        generation: Generation,
    ) -> Result<Generation, TerminalArbiterError> {
        let state = self
            .sessions
            .get_mut(&session_id)
            .ok_or(TerminalArbiterError::UnknownSession(session_id))?;
        if state.phase != TerminalSessionPhase::Open {
            return Err(TerminalArbiterError::SessionClosing(session_id));
        }
        if generation <= state.generation {
            return Err(TerminalArbiterError::NonIncreasingGeneration {
                requested: generation,
                current: state.generation,
            });
        }
        let previous = state.generation;
        state.generation = generation;
        Ok(previous)
    }

    /// Starts exact scheduled work within the independent in-flight bound.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalArbiterError`] if lifecycle, session, generation,
    /// identity uniqueness, or in-flight capacity validation fails.
    pub fn start(&mut self, work: ScheduledWork) -> Result<(), TerminalArbiterError> {
        if self.shutting_down {
            return Err(TerminalArbiterError::SchedulerShuttingDown);
        }
        let request = work.request;
        let session = self
            .sessions
            .get(&request.session_id)
            .ok_or(TerminalArbiterError::UnknownSession(request.session_id))?;
        if session.phase != TerminalSessionPhase::Open {
            return Err(TerminalArbiterError::SessionClosing(request.session_id));
        }
        if request.generation != session.generation {
            return Err(TerminalArbiterError::NonIncreasingGeneration {
                requested: request.generation,
                current: session.generation,
            });
        }
        if self.in_flight.contains_key(&request.work_id) {
            return Err(TerminalArbiterError::DuplicateInFlightWork(request.work_id));
        }
        if self.in_flight.len() == self.limits.max_in_flight {
            return Err(TerminalArbiterError::InFlightCapacityReached);
        }
        self.in_flight.insert(
            request.work_id,
            InFlight {
                signal: TerminalSignal {
                    work_id: request.work_id,
                    session_id: request.session_id,
                    generation: request.generation,
                },
            },
        );
        Ok(())
    }

    /// Marks a session closing; repeated close is stable.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalArbiterError::UnknownSession`] for an unregistered
    /// identity.
    pub fn begin_close(&mut self, session_id: SessionId) -> Result<bool, TerminalArbiterError> {
        let state = self
            .sessions
            .get_mut(&session_id)
            .ok_or(TerminalArbiterError::UnknownSession(session_id))?;
        if state.phase == TerminalSessionPhase::Closing {
            return Ok(false);
        }
        state.phase = TerminalSessionPhase::Closing;
        Ok(true)
    }

    /// Marks every session closing and permanently prevents publication.
    pub fn begin_shutdown(&mut self) -> bool {
        if self.shutting_down {
            return false;
        }
        self.shutting_down = true;
        for session in self.sessions.values_mut() {
            session.phase = TerminalSessionPhase::Closing;
        }
        true
    }

    /// Resolves exact completion into one publish or mandatory discard/release.
    #[must_use]
    pub fn complete(
        &mut self,
        signal: TerminalSignal,
        resource_id: ResourceId,
    ) -> TerminalDecision {
        let Some(active) = self.in_flight.get(&signal.work_id).copied() else {
            return TerminalDecision::DiscardAndRelease {
                work_id: signal.work_id,
                resource_id,
                reason: CompletionDiscardReason::UnknownOrAlreadyTerminal,
            };
        };
        if active.signal != signal {
            return TerminalDecision::DiscardAndRelease {
                work_id: signal.work_id,
                resource_id,
                reason: CompletionDiscardReason::IdentityMismatch,
            };
        }
        self.in_flight.remove(&signal.work_id);
        if self.shutting_down {
            return TerminalDecision::DiscardAndRelease {
                work_id: signal.work_id,
                resource_id,
                reason: CompletionDiscardReason::SchedulerShuttingDown,
            };
        }
        let Some(session) = self.sessions.get(&signal.session_id) else {
            return TerminalDecision::DiscardAndRelease {
                work_id: signal.work_id,
                resource_id,
                reason: CompletionDiscardReason::IdentityMismatch,
            };
        };
        if session.phase != TerminalSessionPhase::Open {
            return TerminalDecision::DiscardAndRelease {
                work_id: signal.work_id,
                resource_id,
                reason: CompletionDiscardReason::SessionClosing,
            };
        }
        if session.generation != signal.generation {
            return TerminalDecision::DiscardAndRelease {
                work_id: signal.work_id,
                resource_id,
                reason: CompletionDiscardReason::StaleGeneration,
            };
        }
        TerminalDecision::Publish {
            work_id: signal.work_id,
            resource_id,
        }
    }

    /// Resolves exact cancellation once.
    #[must_use]
    pub fn cancel(&mut self, signal: TerminalSignal) -> TerminalDecision {
        self.finish_without_resource(signal, false)
    }

    /// Resolves exact failure once.
    #[must_use]
    pub fn fail(&mut self, signal: TerminalSignal) -> TerminalDecision {
        self.finish_without_resource(signal, true)
    }

    /// Returns the current in-flight count.
    #[must_use]
    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    /// Returns whether whole-arbiter shutdown has begun.
    #[must_use]
    pub const fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    fn finish_without_resource(
        &mut self,
        signal: TerminalSignal,
        failed: bool,
    ) -> TerminalDecision {
        let Some(active) = self.in_flight.get(&signal.work_id).copied() else {
            return TerminalDecision::Ignored {
                work_id: signal.work_id,
                reason: CompletionDiscardReason::UnknownOrAlreadyTerminal,
            };
        };
        if active.signal != signal {
            return TerminalDecision::Ignored {
                work_id: signal.work_id,
                reason: CompletionDiscardReason::IdentityMismatch,
            };
        }
        self.in_flight.remove(&signal.work_id);
        if failed {
            TerminalDecision::Failed {
                work_id: signal.work_id,
            }
        } else {
            TerminalDecision::Cancelled {
                work_id: signal.work_id,
            }
        }
    }
}
