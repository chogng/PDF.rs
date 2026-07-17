//! Bounded deterministic normal and critical scheduling state.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::{
    CriticalAdmissionError, CriticalDispatch, CriticalIngress, FairnessEvidence, Generation,
    ResourceId, ScheduledWork, SchedulerError, SchedulerLimits, SchedulingKey, SessionId,
    SessionRegistrationError, TerminalArbiter, TerminalArbiterError, TerminalArbiterLimits,
    TerminalSessionPhase, TerminalSignal, WorkAdmissionError, WorkId, WorkRequest,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct QueueSession {
    queued: usize,
    last_service_turn: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct QueuedWork {
    request: WorkRequest,
    enqueue_order: u64,
    enqueue_tick: u64,
}

/// Whole-scheduler lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerPhase {
    /// Normal and critical ingress are accepted.
    Running,
    /// Normal ingress is stopped while critical cleanup drains.
    ShuttingDown,
    /// All queues and in-flight work are empty.
    Terminated,
}

/// Result of admitting a critical event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CriticalAdmission {
    /// A new event owns one dedicated FIFO slot.
    Enqueued {
        /// Stable critical FIFO order.
        fifo_order: u64,
    },
    /// The exact close or shutdown transition was already requested.
    AlreadyPending,
}

/// A monotonic generation change and every queued identity it superseded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationAdvance {
    /// Previous authoritative generation.
    pub previous: Generation,
    /// New authoritative generation.
    pub current: Generation,
    /// Queued jobs removed before they could start.
    pub superseded_queued: Vec<WorkId>,
}

/// Normal-work admission result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubmitOutcome {
    /// A new normal queue slot was consumed.
    Enqueued {
        /// Optional generation change performed before admission.
        generation_advance: Option<GenerationAdvance>,
    },
    /// Existing same-generation replaceable work was updated in place.
    Coalesced {
        /// Identity removed from the queue.
        replaced_work_id: WorkId,
        /// Identity now represented by the retained queue position.
        current_work_id: WorkId,
        /// Optional generation change performed before coalescing.
        generation_advance: Option<GenerationAdvance>,
    },
}

/// Atomic close admission evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseReceipt {
    /// Dedicated critical-queue admission.
    pub critical: CriticalAdmission,
    /// Queued normal work removed at close ingress.
    pub superseded_queued: Vec<WorkId>,
}

/// Atomic shutdown admission evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownReceipt {
    /// Dedicated critical-queue admission.
    pub critical: CriticalAdmission,
    /// All queued normal work removed at shutdown ingress.
    pub superseded_queued: Vec<WorkId>,
}

/// Next deterministic dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerDispatch {
    /// Critical FIFO work, already terminal-arbitrated where applicable.
    Critical(CriticalDispatch),
    /// One normal work item now owned by the in-flight arbiter.
    Normal(ScheduledWork),
}

/// Bounded deterministic scheduler for replaceable viewport and tile work.
#[derive(Debug)]
pub struct ViewportScheduler {
    limits: SchedulerLimits,
    phase: SchedulerPhase,
    tick: u64,
    next_enqueue_order: u64,
    next_critical_order: u64,
    next_service_turn: u64,
    sessions: BTreeMap<SessionId, QueueSession>,
    normal: Vec<QueuedWork>,
    critical: VecDeque<(u64, CriticalIngress)>,
    seen_work_ids: BTreeSet<WorkId>,
    terminal: TerminalArbiter,
}

impl ViewportScheduler {
    /// Creates an empty scheduler from validated limits.
    #[must_use]
    pub fn new(limits: SchedulerLimits) -> Self {
        let arbiter_limits = TerminalArbiterLimits::from_validated_scheduler(
            limits.max_sessions(),
            limits.in_flight_capacity(),
        );
        Self {
            limits,
            phase: SchedulerPhase::Running,
            tick: 0,
            next_enqueue_order: 0,
            next_critical_order: 0,
            next_service_turn: 1,
            sessions: BTreeMap::new(),
            normal: Vec::with_capacity(limits.normal_capacity()),
            critical: VecDeque::with_capacity(limits.critical_capacity()),
            seen_work_ids: BTreeSet::new(),
            terminal: TerminalArbiter::new(arbiter_limits),
        }
    }

    /// Registers a never-reused session and precharges its queue reservation.
    ///
    /// # Errors
    ///
    /// Returns [`SessionRegistrationError`] when lifecycle or identity
    /// validation fails, the registry is full, or a new reservation cannot be
    /// precharged without exceeding capacity.
    pub fn register_session(
        &mut self,
        session_id: SessionId,
        initial_generation: Generation,
    ) -> Result<(), SessionRegistrationError> {
        if self.phase != SchedulerPhase::Running {
            return Err(SessionRegistrationError::SchedulerShuttingDown);
        }
        if self.sessions.contains_key(&session_id) {
            return Err(SessionRegistrationError::DuplicateSession(session_id));
        }
        if self.sessions.len() == self.limits.max_sessions() {
            return Err(SessionRegistrationError::SessionLimitReached);
        }
        let unused_existing = self.unused_open_reservations(None);
        let required = self
            .normal
            .len()
            .checked_add(unused_existing)
            .and_then(|value| value.checked_add(self.limits.per_session_reservation()))
            .ok_or(SessionRegistrationError::ReservationUnavailable)?;
        if required > self.limits.normal_capacity() {
            return Err(SessionRegistrationError::ReservationUnavailable);
        }
        self.terminal
            .register_session(session_id, initial_generation)
            .map_err(|error| match error {
                TerminalArbiterError::DuplicateSession(id) => {
                    SessionRegistrationError::DuplicateSession(id)
                }
                TerminalArbiterError::SessionLimitReached => {
                    SessionRegistrationError::SessionLimitReached
                }
                _ => SessionRegistrationError::SchedulerShuttingDown,
            })?;
        self.sessions.insert(
            session_id,
            QueueSession {
                queued: 0,
                last_service_turn: 0,
            },
        );
        Ok(())
    }

    /// Advances only the caller-controlled virtual tick.
    ///
    /// # Errors
    ///
    /// Returns [`SchedulerError::CounterExhausted`] if virtual time overflows.
    pub fn advance_tick(&mut self, delta: u64) -> Result<u64, SchedulerError> {
        self.tick = self
            .tick
            .checked_add(delta)
            .ok_or(SchedulerError::CounterExhausted)?;
        Ok(self.tick)
    }

    /// Returns the current virtual tick.
    #[must_use]
    pub const fn tick(&self) -> u64 {
        self.tick
    }

    /// Advances one session generation and removes older queued work.
    ///
    /// # Errors
    ///
    /// Returns [`WorkAdmissionError`] for shutdown, unknown or closing session,
    /// or a generation which does not strictly increase.
    pub fn advance_generation(
        &mut self,
        session_id: SessionId,
        generation: Generation,
    ) -> Result<GenerationAdvance, WorkAdmissionError> {
        if self.phase != SchedulerPhase::Running {
            return Err(WorkAdmissionError::SchedulerShuttingDown);
        }
        let current = self
            .terminal
            .current_generation(session_id)
            .ok_or(WorkAdmissionError::UnknownSession(session_id))?;
        if self.terminal.session_phase(session_id) != Some(TerminalSessionPhase::Open) {
            return Err(WorkAdmissionError::SessionClosing(session_id));
        }
        if generation <= current {
            return Err(WorkAdmissionError::NonIncreasingGeneration {
                requested: generation,
                current,
            });
        }
        self.terminal
            .advance_generation(session_id, generation)
            .map_err(|_| WorkAdmissionError::SessionClosing(session_id))?;
        let superseded_queued = self.remove_queued_for_session(session_id);
        Ok(GenerationAdvance {
            previous: current,
            current: generation,
            superseded_queued,
        })
    }

    /// Submits replaceable normal work.
    ///
    /// A newer generation first becomes authoritative and supersedes all older
    /// queued jobs even if this particular work later fails capacity admission.
    /// Same-generation work with the same replacement key retains its original
    /// enqueue tick and order while atomically adopting the latest metadata.
    ///
    /// # Errors
    ///
    /// Returns [`WorkAdmissionError`] for lifecycle, generation, identity,
    /// queue, reservation, history, or monotonic-counter rejection.
    pub fn submit(&mut self, request: WorkRequest) -> Result<SubmitOutcome, WorkAdmissionError> {
        if self.phase != SchedulerPhase::Running {
            return Err(WorkAdmissionError::SchedulerShuttingDown);
        }
        let current = self
            .terminal
            .current_generation(request.session_id)
            .ok_or(WorkAdmissionError::UnknownSession(request.session_id))?;
        if self.terminal.session_phase(request.session_id) != Some(TerminalSessionPhase::Open) {
            return Err(WorkAdmissionError::SessionClosing(request.session_id));
        }
        if request.generation < current {
            return Err(WorkAdmissionError::SupersededGeneration {
                submitted: request.generation,
                current,
            });
        }
        let generation_advance = if request.generation > current {
            Some(self.advance_generation(request.session_id, request.generation)?)
        } else {
            None
        };

        if let Some(index) = self.normal.iter().position(|queued| {
            queued.request.session_id == request.session_id
                && queued.request.generation == request.generation
                && queued.request.replace_key == request.replace_key
        }) {
            let replaced = self.normal[index].request.work_id;
            if request.work_id != replaced {
                self.reserve_work_id(request.work_id)?;
            }
            self.normal[index].request = request;
            return Ok(SubmitOutcome::Coalesced {
                replaced_work_id: replaced,
                current_work_id: request.work_id,
                generation_advance,
            });
        }

        if self.seen_work_ids.contains(&request.work_id) {
            return Err(WorkAdmissionError::DuplicateWorkId(request.work_id));
        }
        let session = self
            .sessions
            .get(&request.session_id)
            .ok_or(WorkAdmissionError::UnknownSession(request.session_id))?;
        if session.queued == self.limits.per_session_capacity() {
            return Err(WorkAdmissionError::SessionQueueFull(request.session_id));
        }
        let reserved_for_others = self.unused_open_reservations(Some(request.session_id));
        let charged = self
            .normal
            .len()
            .checked_add(1)
            .and_then(|value| value.checked_add(reserved_for_others))
            .ok_or(WorkAdmissionError::ReservedNormalCapacity)?;
        if charged > self.limits.normal_capacity() {
            return Err(WorkAdmissionError::ReservedNormalCapacity);
        }
        if self.next_enqueue_order == u64::MAX {
            return Err(WorkAdmissionError::CounterExhausted);
        }
        self.reserve_work_id(request.work_id)?;
        let Some(session) = self.sessions.get_mut(&request.session_id) else {
            self.seen_work_ids.remove(&request.work_id);
            return Err(WorkAdmissionError::UnknownSession(request.session_id));
        };
        session.queued += 1;
        let enqueue_order = self.next_enqueue_order;
        self.next_enqueue_order += 1;
        self.normal.push(QueuedWork {
            request,
            enqueue_order,
            enqueue_tick: self.tick,
        });
        Ok(SubmitOutcome::Enqueued { generation_advance })
    }

    /// Enqueues critical cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`CriticalAdmissionError`] with event ownership when validation
    /// or dedicated-capacity admission fails.
    pub fn enqueue_cancel(
        &mut self,
        signal: TerminalSignal,
    ) -> Result<CriticalAdmission, CriticalAdmissionError> {
        self.enqueue_critical(CriticalIngress::Cancel(signal))
    }

    /// Enqueues a critical release.
    ///
    /// # Errors
    ///
    /// Returns [`CriticalAdmissionError`] with event ownership when validation
    /// or dedicated-capacity admission fails.
    pub fn enqueue_release(
        &mut self,
        session_id: SessionId,
        resource_id: ResourceId,
    ) -> Result<CriticalAdmission, CriticalAdmissionError> {
        self.enqueue_critical(CriticalIngress::Release {
            session_id,
            resource_id,
        })
    }

    /// Enqueues critical failure.
    ///
    /// # Errors
    ///
    /// Returns [`CriticalAdmissionError`] with event ownership when validation
    /// or dedicated-capacity admission fails.
    pub fn enqueue_failure(
        &mut self,
        signal: TerminalSignal,
    ) -> Result<CriticalAdmission, CriticalAdmissionError> {
        self.enqueue_critical(CriticalIngress::Failure(signal))
    }

    /// Transfers a completed resource into the critical queue.
    ///
    /// On rejection, [`CriticalAdmissionError`] returns the entire event, so
    /// the caller retains resource ownership and can release it.
    ///
    /// # Errors
    ///
    /// Returns [`CriticalAdmissionError`] with the completed resource when
    /// validation or dedicated-capacity admission fails.
    pub fn enqueue_completion(
        &mut self,
        signal: TerminalSignal,
        resource_id: ResourceId,
    ) -> Result<CriticalAdmission, CriticalAdmissionError> {
        self.enqueue_critical(CriticalIngress::Completion {
            signal,
            resource_id,
        })
    }

    /// Atomically reserves a critical close slot, stops normal ingress for the
    /// session, and removes all of its queued work.
    ///
    /// # Errors
    ///
    /// Returns [`CriticalAdmissionError`] without changing lifecycle if the
    /// session is unknown, the scheduler terminated, or critical admission
    /// fails.
    pub fn close_session(
        &mut self,
        session_id: SessionId,
    ) -> Result<CloseReceipt, CriticalAdmissionError> {
        let event = CriticalIngress::Close { session_id };
        if self.phase == SchedulerPhase::Terminated {
            return Err(CriticalAdmissionError::SchedulerTerminated(event));
        }
        let Some(phase) = self.terminal.session_phase(session_id) else {
            return Err(CriticalAdmissionError::UnknownSession(event));
        };
        if phase == TerminalSessionPhase::Closing {
            return Ok(CloseReceipt {
                critical: CriticalAdmission::AlreadyPending,
                superseded_queued: Vec::new(),
            });
        }
        let critical = self.enqueue_critical(event)?;
        if self.terminal.begin_close(session_id).is_err() {
            let _ = self.critical.pop_back();
            self.next_critical_order = self.next_critical_order.saturating_sub(1);
            return Err(CriticalAdmissionError::UnknownSession(
                CriticalIngress::Close { session_id },
            ));
        }
        let superseded_queued = self.remove_queued_for_session(session_id);
        Ok(CloseReceipt {
            critical,
            superseded_queued,
        })
    }

    /// Atomically reserves shutdown, stops all normal ingress, and supersedes
    /// every queued normal job.
    ///
    /// # Errors
    ///
    /// Returns [`CriticalAdmissionError`] without beginning shutdown when the
    /// dedicated queue cannot own the shutdown marker.
    pub fn begin_shutdown(&mut self) -> Result<ShutdownReceipt, CriticalAdmissionError> {
        if self.phase != SchedulerPhase::Running {
            return Ok(ShutdownReceipt {
                critical: CriticalAdmission::AlreadyPending,
                superseded_queued: Vec::new(),
            });
        }
        let critical = self.enqueue_critical(CriticalIngress::Shutdown)?;
        self.phase = SchedulerPhase::ShuttingDown;
        let started = self.terminal.begin_shutdown();
        debug_assert!(started);
        let superseded_queued = self
            .normal
            .iter()
            .map(|entry| entry.request.work_id)
            .collect();
        self.normal.clear();
        for session in self.sessions.values_mut() {
            session.queued = 0;
        }
        Ok(ShutdownReceipt {
            critical,
            superseded_queued,
        })
    }

    /// Dispatches critical FIFO traffic before eligible normal work.
    ///
    /// Completion is resolved by the embedded terminal arbiter before it is
    /// returned, so callers can only observe publish or discard/release.
    ///
    /// # Errors
    ///
    /// Returns [`SchedulerError`] on counter exhaustion or a fail-closed
    /// internal bounded-state inconsistency.
    pub fn dispatch_next(&mut self) -> Result<Option<SchedulerDispatch>, SchedulerError> {
        if let Some((_order, event)) = self.critical.pop_front() {
            let dispatch = match event {
                CriticalIngress::Cancel(signal) => {
                    CriticalDispatch::Cancel(self.terminal.cancel(signal))
                }
                CriticalIngress::Close { session_id } => CriticalDispatch::Close { session_id },
                CriticalIngress::Release {
                    session_id,
                    resource_id,
                } => CriticalDispatch::Release {
                    session_id,
                    resource_id,
                },
                CriticalIngress::Failure(signal) => {
                    CriticalDispatch::Failure(self.terminal.fail(signal))
                }
                CriticalIngress::Completion {
                    signal,
                    resource_id,
                } => CriticalDispatch::Completion(self.terminal.complete(signal, resource_id)),
                CriticalIngress::Shutdown => CriticalDispatch::Shutdown,
            };
            return Ok(Some(SchedulerDispatch::Critical(dispatch)));
        }
        if self.phase != SchedulerPhase::Running
            || self.terminal.in_flight_len() == self.limits.in_flight_capacity()
            || self.normal.is_empty()
        {
            return Ok(None);
        }

        let minimum_last_service_turn = self
            .normal
            .iter()
            .filter_map(|entry| {
                self.sessions
                    .get(&entry.request.session_id)
                    .map(|session| session.last_service_turn)
            })
            .min()
            .ok_or(SchedulerError::InvariantViolation)?;
        let exclusive_turn_limit = minimum_last_service_turn
            .checked_add(self.limits.fairness_burst())
            .ok_or(SchedulerError::CounterExhausted)?;
        if self.next_service_turn == u64::MAX {
            return Err(SchedulerError::CounterExhausted);
        }

        let mut selected: Option<(usize, SchedulingKey, u64)> = None;
        for (index, entry) in self.normal.iter().enumerate() {
            let session = self
                .sessions
                .get(&entry.request.session_id)
                .ok_or(SchedulerError::InvariantViolation)?;
            if session.last_service_turn >= exclusive_turn_limit {
                continue;
            }
            let key = SchedulingKey::for_request(
                entry.request,
                entry.enqueue_order,
                entry.enqueue_tick,
                self.tick,
                self.limits.aging_quantum_ticks(),
                self.limits.max_aging_steps(),
            );
            if selected.is_none_or(|(_, current_key, _)| key < current_key) {
                selected = Some((index, key, session.last_service_turn));
            }
        }
        let (index, scheduling_key, session_last_service_turn) =
            selected.ok_or(SchedulerError::InvariantViolation)?;
        let entry = self.normal[index];
        let scheduled = ScheduledWork {
            request: entry.request,
            scheduling_key,
            fairness: FairnessEvidence {
                minimum_last_service_turn,
                session_last_service_turn,
                exclusive_turn_limit,
            },
        };
        self.terminal
            .start(scheduled)
            .map_err(|_| SchedulerError::InvariantViolation)?;
        self.normal.remove(index);
        let session = self
            .sessions
            .get_mut(&entry.request.session_id)
            .ok_or(SchedulerError::InvariantViolation)?;
        session.queued = session
            .queued
            .checked_sub(1)
            .ok_or(SchedulerError::InvariantViolation)?;
        session.last_service_turn = self.next_service_turn;
        self.next_service_turn += 1;
        Ok(Some(SchedulerDispatch::Normal(scheduled)))
    }

    /// Marks shutdown complete only after normal, critical, and in-flight state
    /// has reached exact zero.
    #[must_use]
    pub fn try_finish_shutdown(&mut self) -> bool {
        if self.phase == SchedulerPhase::ShuttingDown
            && self.normal.is_empty()
            && self.critical.is_empty()
            && self.terminal.in_flight_len() == 0
        {
            self.phase = SchedulerPhase::Terminated;
            true
        } else {
            false
        }
    }

    /// Returns the whole-scheduler lifecycle.
    #[must_use]
    pub const fn phase(&self) -> SchedulerPhase {
        self.phase
    }

    /// Returns the queued normal-work count.
    #[must_use]
    pub fn normal_len(&self) -> usize {
        self.normal.len()
    }

    /// Returns the queued critical-event count.
    #[must_use]
    pub fn critical_len(&self) -> usize {
        self.critical.len()
    }

    /// Returns the current in-flight count.
    #[must_use]
    pub fn in_flight_len(&self) -> usize {
        self.terminal.in_flight_len()
    }

    /// Returns the current generation of a registered session.
    #[must_use]
    pub fn current_generation(&self, session_id: SessionId) -> Option<Generation> {
        self.terminal.current_generation(session_id)
    }

    fn enqueue_critical(
        &mut self,
        event: CriticalIngress,
    ) -> Result<CriticalAdmission, CriticalAdmissionError> {
        if self.phase == SchedulerPhase::Terminated {
            return Err(CriticalAdmissionError::SchedulerTerminated(event));
        }
        if let Some(session_id) = event.session_id()
            && !self.sessions.contains_key(&session_id)
        {
            return Err(CriticalAdmissionError::UnknownSession(event));
        }
        if self.critical.len() == self.limits.critical_capacity() {
            return Err(CriticalAdmissionError::QueueFull(event));
        }
        if self.next_critical_order == u64::MAX {
            return Err(CriticalAdmissionError::CounterExhausted(event));
        }
        let fifo_order = self.next_critical_order;
        self.next_critical_order += 1;
        self.critical.push_back((fifo_order, event));
        Ok(CriticalAdmission::Enqueued { fifo_order })
    }

    fn reserve_work_id(&mut self, work_id: WorkId) -> Result<(), WorkAdmissionError> {
        if self.seen_work_ids.contains(&work_id) {
            return Err(WorkAdmissionError::DuplicateWorkId(work_id));
        }
        if self.seen_work_ids.len() == self.limits.max_work_ids_per_epoch() {
            return Err(WorkAdmissionError::WorkIdHistoryFull);
        }
        self.seen_work_ids.insert(work_id);
        Ok(())
    }

    fn unused_open_reservations(&self, excluded: Option<SessionId>) -> usize {
        self.sessions
            .iter()
            .filter(|(session_id, _)| Some(**session_id) != excluded)
            .filter(|(session_id, _)| {
                self.terminal.session_phase(**session_id) == Some(TerminalSessionPhase::Open)
            })
            .map(|(_, session)| {
                self.limits
                    .per_session_reservation()
                    .saturating_sub(session.queued)
            })
            .sum()
    }

    fn remove_queued_for_session(&mut self, session_id: SessionId) -> Vec<WorkId> {
        let mut removed = Vec::new();
        self.normal.retain(|entry| {
            if entry.request.session_id == session_id {
                removed.push(entry.request.work_id);
                false
            } else {
                true
            }
        });
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.queued = 0;
        }
        removed
    }
}
