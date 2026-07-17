use pdf_rs_scheduler::{
    CompletionDiscardReason, CriticalAdmission, CriticalAdmissionError, CriticalDispatch, Distance,
    Generation, Priority, ReplaceKey, ResourceId, SchedulerDispatch, SchedulerError,
    SchedulerLimits, SchedulerPhase, ScrollRelation, SessionId, SubmitOutcome, TerminalDecision,
    TerminalSignal, ViewportScheduler, WorkAdmissionError, WorkId, WorkRequest,
};

fn sid(value: u64) -> SessionId {
    SessionId::new(value).expect("nonzero test session")
}

fn wid(value: u64) -> WorkId {
    WorkId::new(value).expect("nonzero test work")
}

fn rid(value: u64) -> ResourceId {
    ResourceId::new(value).expect("nonzero test resource")
}

fn generation(value: u64) -> Generation {
    Generation::new(value).expect("nonzero test generation")
}

fn limits() -> SchedulerLimits {
    SchedulerLimits::new(12, 2, 10, 8, 8, 3, 64, 10, 4, 2).expect("valid test limits")
}

fn request(
    work: u64,
    session: u64,
    viewport_generation: u64,
    replacement: u64,
    priority: Priority,
) -> WorkRequest {
    WorkRequest {
        work_id: wid(work),
        session_id: sid(session),
        generation: generation(viewport_generation),
        replace_key: ReplaceKey::tile(replacement),
        priority,
        center_distance: Distance::new(100),
        edge_distance: Distance::new(50),
        scroll_relation: ScrollRelation::Neutral,
    }
}

fn next_normal(scheduler: &mut ViewportScheduler) -> pdf_rs_scheduler::ScheduledWork {
    match scheduler
        .dispatch_next()
        .expect("dispatch succeeds")
        .expect("work available")
    {
        SchedulerDispatch::Normal(work) => work,
        SchedulerDispatch::Critical(event) => panic!("unexpected critical event: {event:?}"),
    }
}

fn next_critical(scheduler: &mut ViewportScheduler) -> CriticalDispatch {
    match scheduler
        .dispatch_next()
        .expect("dispatch succeeds")
        .expect("critical work available")
    {
        SchedulerDispatch::Critical(event) => event,
        SchedulerDispatch::Normal(work) => panic!("unexpected normal work: {work:?}"),
    }
}

fn single_in_flight() -> (ViewportScheduler, TerminalSignal) {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P0))
        .expect("admit");
    let work = next_normal(&mut scheduler);
    (
        scheduler,
        TerminalSignal {
            work_id: work.request.work_id,
            session_id: work.request.session_id,
            generation: work.request.generation,
        },
    )
}

#[test]
fn total_key_orders_priority_scroll_geometry_and_enqueue_order() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");

    let mut behind = request(1, 1, 1, 1, Priority::P1);
    behind.scroll_relation = ScrollRelation::Behind;
    behind.center_distance = Distance::new(0);
    let mut ahead_far = request(2, 1, 1, 2, Priority::P1);
    ahead_far.scroll_relation = ScrollRelation::Ahead;
    ahead_far.center_distance = Distance::new(1000);
    let first_tie = request(3, 1, 1, 3, Priority::P2);
    let second_tie = request(4, 1, 1, 4, Priority::P2);
    let mut center_near = request(5, 1, 1, 5, Priority::P1);
    center_near.center_distance = Distance::new(10);
    center_near.edge_distance = Distance::new(100);
    let mut center_far = request(6, 1, 1, 6, Priority::P1);
    center_far.center_distance = Distance::new(20);
    center_far.edge_distance = Distance::new(0);
    let mut edge_near = request(7, 1, 1, 7, Priority::P1);
    edge_near.center_distance = Distance::new(30);
    edge_near.edge_distance = Distance::new(10);
    let mut edge_far = request(8, 1, 1, 8, Priority::P1);
    edge_far.center_distance = Distance::new(30);
    edge_far.edge_distance = Distance::new(20);
    for work in [
        behind,
        ahead_far,
        first_tie,
        second_tie,
        center_near,
        center_far,
        edge_near,
        edge_far,
    ] {
        scheduler.submit(work).expect("admit");
    }

    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(2));
    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(5));
    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(6));
    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(7));
    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(8));
    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(1));
    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(3));
    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(4));
}

#[test]
fn capped_virtual_aging_prevents_fresh_p0_starvation() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P4))
        .expect("old background work");
    scheduler.advance_tick(40).expect("bounded virtual time");
    scheduler
        .submit(request(2, 1, 1, 2, Priority::P0))
        .expect("fresh visible work");

    let aged = next_normal(&mut scheduler);
    assert_eq!(aged.request.work_id, wid(1));
    assert_eq!(aged.scheduling_key.aging_lane(), 0);
    assert_eq!(aged.scheduling_key.effective_priority(), 0);
}

#[test]
fn maximum_virtual_tick_is_exact_and_aging_replays_without_overflow() {
    let mut keys = Vec::new();
    for _ in 0..2 {
        let mut scheduler = ViewportScheduler::new(limits());
        scheduler
            .register_session(sid(1), generation(1))
            .expect("register");
        scheduler
            .submit(request(1, 1, 1, 1, Priority::P4))
            .expect("old work");
        assert_eq!(scheduler.advance_tick(u64::MAX), Ok(u64::MAX));
        assert_eq!(scheduler.tick(), u64::MAX);
        assert_eq!(
            scheduler.advance_tick(1),
            Err(SchedulerError::CounterExhausted)
        );
        assert_eq!(
            scheduler.advance_tick(1),
            Err(SchedulerError::CounterExhausted)
        );
        assert_eq!(scheduler.tick(), u64::MAX);

        let aged = next_normal(&mut scheduler);
        assert_eq!(aged.scheduling_key.aging_lane(), 0);
        assert_eq!(aged.scheduling_key.effective_priority(), 0);
        keys.push(aged.scheduling_key);
    }
    assert_eq!(keys[0], keys[1]);
}

fn deterministic_trace() -> Vec<WorkId> {
    let mut scheduler = ViewportScheduler::new(limits());
    for session in 1..=3 {
        scheduler
            .register_session(sid(session), generation(1))
            .expect("register");
    }
    let arrivals = [
        request(1, 1, 1, 1, Priority::P0),
        request(2, 2, 1, 1, Priority::P3),
        request(3, 1, 1, 2, Priority::P1),
        request(4, 3, 1, 1, Priority::P4),
        request(5, 2, 1, 2, Priority::P2),
    ];
    for work in arrivals {
        scheduler.submit(work).expect("admit deterministic work");
    }
    (0..arrivals.len())
        .map(|_| next_normal(&mut scheduler).request.work_id)
        .collect()
}

#[test]
fn identical_state_replays_byte_for_byte_order() {
    assert_eq!(deterministic_trace(), deterministic_trace());
    assert_eq!(
        deterministic_trace(),
        vec![wid(1), wid(3), wid(5), wid(4), wid(2)]
    );
}

#[test]
fn service_burst_bounds_cross_session_starvation() {
    let mut scheduler = ViewportScheduler::new(limits());
    for session in 1..=3 {
        scheduler
            .register_session(sid(session), generation(1))
            .expect("register");
    }
    for work in 1..=8 {
        scheduler
            .submit(request(work, 1, 1, work, Priority::P0))
            .expect("session one shared admission");
    }
    scheduler
        .submit(request(20, 2, 1, 1, Priority::P4))
        .expect("session two reservation");
    scheduler
        .submit(request(30, 3, 1, 1, Priority::P4))
        .expect("session three reservation");

    let first_four: Vec<_> = (0..4)
        .map(|_| next_normal(&mut scheduler).request.session_id)
        .collect();
    assert_eq!(first_four, vec![sid(1), sid(1), sid(2), sid(3)]);
}

#[test]
fn returning_idle_session_gets_one_turn_without_creating_historical_debt() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("busy session");
    scheduler
        .register_session(sid(2), generation(1))
        .expect("idle session");
    for work in 1..=10 {
        scheduler
            .submit(request(work, 1, 1, work, Priority::P0))
            .expect("historical busy work");
        let active = next_normal(&mut scheduler);
        scheduler
            .enqueue_cancel(TerminalSignal {
                work_id: active.request.work_id,
                session_id: active.request.session_id,
                generation: active.request.generation,
            })
            .expect("release in-flight slot");
        let _ = next_critical(&mut scheduler);
    }

    scheduler
        .submit(request(100, 1, 1, 100, Priority::P0))
        .expect("busy session remains ready");
    for work in 200..=203 {
        scheduler
            .submit(request(work, 2, 1, work, Priority::P4))
            .expect("returning idle work");
    }
    assert_eq!(next_normal(&mut scheduler).request.session_id, sid(2));
    assert_eq!(next_normal(&mut scheduler).request.session_id, sid(1));
}

#[test]
fn reservations_survive_exact_shared_queue_pressure() {
    let constrained =
        SchedulerLimits::new(6, 2, 6, 2, 2, 2, 16, 1, 4, 1).expect("valid exact limits");
    let mut scheduler = ViewportScheduler::new(constrained);
    scheduler
        .register_session(sid(1), generation(1))
        .expect("first reservation");
    scheduler
        .register_session(sid(2), generation(1))
        .expect("second reservation");

    for work in 1..=4 {
        scheduler
            .submit(request(work, 1, 1, work, Priority::P1))
            .expect("shared slots plus own reservation");
    }
    assert_eq!(
        scheduler.submit(request(5, 1, 1, 5, Priority::P1)),
        Err(WorkAdmissionError::ReservedNormalCapacity)
    );
    scheduler
        .submit(request(10, 2, 1, 1, Priority::P1))
        .expect("first protected slot");
    scheduler
        .submit(request(11, 2, 1, 2, Priority::P1))
        .expect("exact protected capacity");
    assert_eq!(scheduler.normal_len(), 6);
    assert_eq!(
        scheduler.submit(request(12, 2, 1, 3, Priority::P1)),
        Err(WorkAdmissionError::ReservedNormalCapacity)
    );
}

#[test]
fn late_session_registration_precharges_before_mutation() {
    let constrained = SchedulerLimits::new(4, 2, 4, 2, 2, 2, 12, 1, 4, 1).expect("valid limits");
    let mut scheduler = ViewportScheduler::new(constrained);
    scheduler
        .register_session(sid(1), generation(1))
        .expect("first registration");
    for work in 1..=4 {
        scheduler
            .submit(request(work, 1, 1, work, Priority::P1))
            .expect("single session may use shared slots");
    }
    assert_eq!(
        scheduler.register_session(sid(2), generation(1)),
        Err(pdf_rs_scheduler::SessionRegistrationError::ReservationUnavailable)
    );
}

#[test]
fn duplicate_replaceable_work_coalesces_without_capacity_growth() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 77, Priority::P4))
        .expect("first");
    let mut replacement = request(2, 1, 1, 77, Priority::P0);
    replacement.center_distance = Distance::new(0);
    assert_eq!(
        scheduler.submit(replacement),
        Ok(SubmitOutcome::Coalesced {
            replaced_work_id: wid(1),
            current_work_id: wid(2),
            generation_advance: None,
        })
    );
    assert_eq!(scheduler.normal_len(), 1);
    let selected = next_normal(&mut scheduler);
    assert_eq!(selected.request.work_id, wid(2));
    assert_eq!(selected.scheduling_key.enqueue_order(), 0);
}

#[test]
fn viewport_and_tile_replacement_namespaces_do_not_alias() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    let mut viewport = request(1, 1, 1, 77, Priority::P2);
    viewport.replace_key = ReplaceKey::viewport(77);
    let tile = request(2, 1, 1, 77, Priority::P1);
    scheduler.submit(viewport).expect("viewport");
    scheduler.submit(tile).expect("tile");
    let mut newer_viewport = request(3, 1, 1, 77, Priority::P0);
    newer_viewport.replace_key = ReplaceKey::viewport(77);
    assert!(matches!(
        scheduler.submit(newer_viewport),
        Ok(SubmitOutcome::Coalesced {
            replaced_work_id,
            current_work_id,
            ..
        }) if replaced_work_id == wid(1) && current_work_id == wid(3)
    ));
    assert_eq!(scheduler.normal_len(), 2);
    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(3));
    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(2));
}

fn final_state_after_generation_arrivals(new_first: bool) -> (Generation, Vec<WorkId>) {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    let old = request(1, 1, 1, 1, Priority::P0);
    let new = request(2, 1, 2, 1, Priority::P0);
    if new_first {
        scheduler.submit(new).expect("new generation");
        assert!(matches!(
            scheduler.submit(old),
            Err(WorkAdmissionError::SupersededGeneration { .. })
        ));
    } else {
        scheduler.submit(old).expect("old generation");
        scheduler.submit(new).expect("new generation supersedes");
    }
    (
        scheduler.current_generation(sid(1)).expect("generation"),
        vec![next_normal(&mut scheduler).request.work_id],
    )
}

#[test]
fn generation_arrival_permutations_converge() {
    assert_eq!(
        final_state_after_generation_arrivals(false),
        final_state_after_generation_arrivals(true)
    );
    assert_eq!(
        final_state_after_generation_arrivals(false),
        (generation(2), vec![wid(2)])
    );
}

#[test]
fn rapid_generation_makes_in_flight_completion_discard_and_release() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P0))
        .expect("admit old work");
    let old = next_normal(&mut scheduler);
    scheduler
        .submit(request(2, 1, 2, 1, Priority::P0))
        .expect("advance generation");
    let signal = TerminalSignal {
        work_id: old.request.work_id,
        session_id: old.request.session_id,
        generation: old.request.generation,
    };
    scheduler
        .enqueue_completion(signal, rid(1))
        .expect("critical completion");
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Completion(TerminalDecision::DiscardAndRelease {
            work_id: wid(1),
            resource_id: rid(1),
            reason: CompletionDiscardReason::StaleGeneration,
        })
    );
}

#[test]
fn one_terminal_arbiter_publishes_once_and_discards_duplicate() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P0))
        .expect("admit");
    let work = next_normal(&mut scheduler);
    let signal = TerminalSignal {
        work_id: work.request.work_id,
        session_id: work.request.session_id,
        generation: work.request.generation,
    };
    scheduler
        .enqueue_completion(signal, rid(1))
        .expect("first completion");
    scheduler
        .enqueue_completion(signal, rid(2))
        .expect("duplicate completion");
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Completion(TerminalDecision::Publish {
            work_id: wid(1),
            resource_id: rid(1),
        })
    );
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Completion(TerminalDecision::DiscardAndRelease {
            work_id: wid(1),
            resource_id: rid(2),
            reason: CompletionDiscardReason::UnknownOrAlreadyTerminal,
        })
    );
}

fn cancel_completion_trace(cancel_first: bool) -> (CriticalDispatch, CriticalDispatch) {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P0))
        .expect("admit");
    let work = next_normal(&mut scheduler);
    let signal = TerminalSignal {
        work_id: work.request.work_id,
        session_id: work.request.session_id,
        generation: work.request.generation,
    };
    if cancel_first {
        scheduler.enqueue_cancel(signal).expect("cancel");
        scheduler
            .enqueue_completion(signal, rid(1))
            .expect("completion");
    } else {
        scheduler
            .enqueue_completion(signal, rid(1))
            .expect("completion");
        scheduler.enqueue_cancel(signal).expect("cancel");
    }
    (next_critical(&mut scheduler), next_critical(&mut scheduler))
}

#[test]
fn cancel_completion_arrival_permutations_have_one_terminal_winner() {
    assert_eq!(
        cancel_completion_trace(true),
        (
            CriticalDispatch::Cancel(TerminalDecision::Cancelled { work_id: wid(1) }),
            CriticalDispatch::Completion(TerminalDecision::DiscardAndRelease {
                work_id: wid(1),
                resource_id: rid(1),
                reason: CompletionDiscardReason::UnknownOrAlreadyTerminal,
            }),
        )
    );
    assert_eq!(
        cancel_completion_trace(false),
        (
            CriticalDispatch::Completion(TerminalDecision::Publish {
                work_id: wid(1),
                resource_id: rid(1),
            }),
            CriticalDispatch::Cancel(TerminalDecision::Ignored {
                work_id: wid(1),
                reason: CompletionDiscardReason::UnknownOrAlreadyTerminal,
            }),
        )
    );
}

fn failure_completion_trace(failure_first: bool) -> (CriticalDispatch, CriticalDispatch) {
    let (mut scheduler, signal) = single_in_flight();
    if failure_first {
        scheduler.enqueue_failure(signal).expect("failure");
        scheduler
            .enqueue_completion(signal, rid(1))
            .expect("completion");
    } else {
        scheduler
            .enqueue_completion(signal, rid(1))
            .expect("completion");
        scheduler.enqueue_failure(signal).expect("failure");
    }
    (next_critical(&mut scheduler), next_critical(&mut scheduler))
}

#[test]
fn failure_completion_arrival_permutations_assign_resource_ownership_once() {
    assert_eq!(
        failure_completion_trace(true),
        (
            CriticalDispatch::Failure(TerminalDecision::Failed { work_id: wid(1) }),
            CriticalDispatch::Completion(TerminalDecision::DiscardAndRelease {
                work_id: wid(1),
                resource_id: rid(1),
                reason: CompletionDiscardReason::UnknownOrAlreadyTerminal,
            }),
        )
    );
    assert_eq!(
        failure_completion_trace(false),
        (
            CriticalDispatch::Completion(TerminalDecision::Publish {
                work_id: wid(1),
                resource_id: rid(1),
            }),
            CriticalDispatch::Failure(TerminalDecision::Ignored {
                work_id: wid(1),
                reason: CompletionDiscardReason::UnknownOrAlreadyTerminal,
            }),
        )
    );
}

#[test]
fn cancel_failure_completion_three_way_race_has_one_terminal_winner() {
    let (mut scheduler, signal) = single_in_flight();
    scheduler.enqueue_cancel(signal).expect("cancel");
    scheduler.enqueue_failure(signal).expect("failure");
    scheduler
        .enqueue_completion(signal, rid(1))
        .expect("completion");

    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Cancel(TerminalDecision::Cancelled { work_id: wid(1) })
    );
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Failure(TerminalDecision::Ignored {
            work_id: wid(1),
            reason: CompletionDiscardReason::UnknownOrAlreadyTerminal,
        })
    );
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Completion(TerminalDecision::DiscardAndRelease {
            work_id: wid(1),
            resource_id: rid(1),
            reason: CompletionDiscardReason::UnknownOrAlreadyTerminal,
        })
    );
}

#[test]
fn mismatched_completion_cannot_consume_exact_in_flight_work() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P0))
        .expect("admit");
    let work = next_normal(&mut scheduler);
    let wrong = TerminalSignal {
        work_id: work.request.work_id,
        session_id: work.request.session_id,
        generation: generation(2),
    };
    let exact = TerminalSignal {
        work_id: work.request.work_id,
        session_id: work.request.session_id,
        generation: work.request.generation,
    };
    scheduler
        .enqueue_completion(wrong, rid(1))
        .expect("wrong completion owned");
    scheduler
        .enqueue_completion(exact, rid(2))
        .expect("exact completion owned");
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Completion(TerminalDecision::DiscardAndRelease {
            work_id: wid(1),
            resource_id: rid(1),
            reason: CompletionDiscardReason::IdentityMismatch,
        })
    );
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Completion(TerminalDecision::Publish {
            work_id: wid(1),
            resource_id: rid(2),
        })
    );
}

#[test]
fn critical_capacity_is_exact_and_never_shared_with_normal_work() {
    let constrained = SchedulerLimits::new(2, 1, 2, 2, 2, 1, 8, 1, 4, 1).expect("valid limits");
    let mut scheduler = ViewportScheduler::new(constrained);
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P0))
        .expect("normal one");
    scheduler
        .submit(request(2, 1, 1, 2, Priority::P0))
        .expect("normal exact capacity");
    let signal = TerminalSignal {
        work_id: wid(1),
        session_id: sid(1),
        generation: generation(1),
    };
    assert_eq!(
        scheduler.enqueue_cancel(signal),
        Ok(CriticalAdmission::Enqueued { fifo_order: 0 })
    );
    assert_eq!(
        scheduler.enqueue_release(sid(1), rid(1)),
        Ok(CriticalAdmission::Enqueued { fifo_order: 1 })
    );
    assert_eq!(scheduler.normal_len(), 2);
    let rejected = scheduler.enqueue_failure(signal);
    assert_eq!(
        rejected,
        Err(CriticalAdmissionError::QueueFull(
            pdf_rs_scheduler::CriticalIngress::Failure(signal)
        ))
    );
}

#[test]
fn critical_cancel_failure_and_release_remain_fifo_and_explicit() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P0))
        .expect("cancel target");
    scheduler
        .submit(request(2, 1, 1, 2, Priority::P1))
        .expect("failure target");
    let cancel_work = next_normal(&mut scheduler);
    let fail_work = next_normal(&mut scheduler);
    let cancel = TerminalSignal {
        work_id: cancel_work.request.work_id,
        session_id: cancel_work.request.session_id,
        generation: cancel_work.request.generation,
    };
    let failure = TerminalSignal {
        work_id: fail_work.request.work_id,
        session_id: fail_work.request.session_id,
        generation: fail_work.request.generation,
    };
    scheduler.enqueue_cancel(cancel).expect("cancel ingress");
    scheduler.enqueue_failure(failure).expect("failure ingress");
    scheduler
        .enqueue_release(sid(1), rid(9))
        .expect("release ingress");
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Cancel(TerminalDecision::Cancelled { work_id: wid(1) })
    );
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Failure(TerminalDecision::Failed { work_id: wid(2) })
    );
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Release {
            session_id: sid(1),
            resource_id: rid(9),
        }
    );
}

#[test]
fn in_flight_exact_limit_backpressures_without_dropping_queue() {
    let constrained = SchedulerLimits::new(2, 1, 2, 2, 1, 1, 4, 1, 4, 1).expect("valid limits");
    let mut scheduler = ViewportScheduler::new(constrained);
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P0))
        .expect("one");
    scheduler
        .submit(request(2, 1, 1, 2, Priority::P1))
        .expect("two");
    let first = next_normal(&mut scheduler);
    assert_eq!(scheduler.normal_len(), 1);
    assert_eq!(scheduler.dispatch_next().expect("backpressure"), None);
    scheduler
        .enqueue_completion(
            TerminalSignal {
                work_id: first.request.work_id,
                session_id: first.request.session_id,
                generation: first.request.generation,
            },
            rid(1),
        )
        .expect("completion");
    let _ = next_critical(&mut scheduler);
    assert_eq!(next_normal(&mut scheduler).request.work_id, wid(2));
}

#[test]
fn work_identity_history_enforces_exact_epoch_bound() {
    let constrained = SchedulerLimits::new(2, 1, 2, 1, 1, 1, 3, 1, 4, 1)
        .expect("three IDs cover exact live bound");
    let mut scheduler = ViewportScheduler::new(constrained);
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P1))
        .expect("first identity");
    scheduler
        .submit(request(2, 1, 1, 1, Priority::P1))
        .expect("second identity coalesces");
    scheduler
        .submit(request(3, 1, 1, 1, Priority::P1))
        .expect("exact history capacity");
    assert_eq!(
        scheduler.submit(request(4, 1, 1, 1, Priority::P1)),
        Err(WorkAdmissionError::WorkIdHistoryFull)
    );
    assert_eq!(scheduler.normal_len(), 1);
}

fn close_completion_trace(completion_first: bool) -> (CriticalDispatch, CriticalDispatch) {
    let (mut scheduler, signal) = single_in_flight();
    if completion_first {
        scheduler
            .enqueue_completion(signal, rid(1))
            .expect("completion");
        scheduler.close_session(sid(1)).expect("close barrier");
    } else {
        scheduler.close_session(sid(1)).expect("close barrier");
        scheduler
            .enqueue_completion(signal, rid(1))
            .expect("completion");
    }
    (next_critical(&mut scheduler), next_critical(&mut scheduler))
}

#[test]
fn close_completion_arrival_permutations_always_release_after_close_ingress() {
    let discarded = CriticalDispatch::Completion(TerminalDecision::DiscardAndRelease {
        work_id: wid(1),
        resource_id: rid(1),
        reason: CompletionDiscardReason::SessionClosing,
    });
    let close = CriticalDispatch::Close { session_id: sid(1) };
    assert_eq!(close_completion_trace(true), (discarded, close));
    assert_eq!(close_completion_trace(false), (close, discarded));
}

#[test]
fn close_is_idempotent_and_every_late_completion_is_released() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P0))
        .expect("in flight");
    scheduler
        .submit(request(2, 1, 1, 2, Priority::P1))
        .expect("queued");
    let active = next_normal(&mut scheduler);
    let close = scheduler.close_session(sid(1)).expect("close");
    assert_eq!(close.superseded_queued, vec![wid(2)]);
    assert_eq!(
        scheduler
            .close_session(sid(1))
            .expect("idempotent close")
            .critical,
        CriticalAdmission::AlreadyPending
    );
    assert_eq!(
        scheduler.submit(request(3, 1, 1, 3, Priority::P0)),
        Err(WorkAdmissionError::SessionClosing(sid(1)))
    );
    scheduler
        .enqueue_completion(
            TerminalSignal {
                work_id: active.request.work_id,
                session_id: active.request.session_id,
                generation: active.request.generation,
            },
            rid(1),
        )
        .expect("late completion");
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Close { session_id: sid(1) }
    );
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Completion(TerminalDecision::DiscardAndRelease {
            work_id: wid(1),
            resource_id: rid(1),
            reason: CompletionDiscardReason::SessionClosing,
        })
    );
}

fn shutdown_completion_trace(completion_first: bool) -> (CriticalDispatch, CriticalDispatch) {
    let (mut scheduler, signal) = single_in_flight();
    if completion_first {
        scheduler
            .enqueue_completion(signal, rid(1))
            .expect("completion");
        scheduler.begin_shutdown().expect("shutdown barrier");
    } else {
        scheduler.begin_shutdown().expect("shutdown barrier");
        scheduler
            .enqueue_completion(signal, rid(1))
            .expect("completion");
    }
    let first = next_critical(&mut scheduler);
    let second = next_critical(&mut scheduler);
    assert!(scheduler.try_finish_shutdown());
    (first, second)
}

#[test]
fn shutdown_completion_arrival_permutations_never_publish_owned_resource() {
    let discarded = CriticalDispatch::Completion(TerminalDecision::DiscardAndRelease {
        work_id: wid(1),
        resource_id: rid(1),
        reason: CompletionDiscardReason::SchedulerShuttingDown,
    });
    assert_eq!(
        shutdown_completion_trace(true),
        (discarded, CriticalDispatch::Shutdown)
    );
    assert_eq!(
        shutdown_completion_trace(false),
        (CriticalDispatch::Shutdown, discarded)
    );
}

#[test]
fn shutdown_drains_queued_work_and_requires_zero_resources() {
    let mut scheduler = ViewportScheduler::new(limits());
    scheduler
        .register_session(sid(1), generation(1))
        .expect("register");
    scheduler
        .submit(request(1, 1, 1, 1, Priority::P0))
        .expect("in flight");
    scheduler
        .submit(request(2, 1, 1, 2, Priority::P1))
        .expect("queued");
    let active = next_normal(&mut scheduler);
    let shutdown = scheduler.begin_shutdown().expect("shutdown");
    assert_eq!(shutdown.superseded_queued, vec![wid(2)]);
    assert_eq!(scheduler.phase(), SchedulerPhase::ShuttingDown);
    assert!(!scheduler.try_finish_shutdown());
    scheduler
        .enqueue_completion(
            TerminalSignal {
                work_id: active.request.work_id,
                session_id: active.request.session_id,
                generation: active.request.generation,
            },
            rid(1),
        )
        .expect("shutdown completion");
    assert_eq!(next_critical(&mut scheduler), CriticalDispatch::Shutdown);
    assert_eq!(
        next_critical(&mut scheduler),
        CriticalDispatch::Completion(TerminalDecision::DiscardAndRelease {
            work_id: wid(1),
            resource_id: rid(1),
            reason: CompletionDiscardReason::SchedulerShuttingDown,
        })
    );
    assert!(scheduler.try_finish_shutdown());
    assert_eq!(scheduler.phase(), SchedulerPhase::Terminated);
    assert_eq!(scheduler.normal_len(), 0);
    assert_eq!(scheduler.critical_len(), 0);
    assert_eq!(scheduler.in_flight_len(), 0);
}

#[test]
fn limit_validation_covers_exact_and_one_less_relationships() {
    assert!(SchedulerLimits::new(4, 2, 4, 1, 1, 2, 5, 1, 1, 1).is_ok());
    assert_eq!(
        SchedulerLimits::new(3, 2, 3, 1, 1, 2, 4, 1, 1, 1),
        Err(pdf_rs_scheduler::LimitConfigError::ReservationsExceedNormalCapacity)
    );
    assert_eq!(
        SchedulerLimits::new(4, 2, 4, 1, 1, 2, 4, 1, 1, 1),
        Err(pdf_rs_scheduler::LimitConfigError::WorkIdCapacityBelowLiveBound)
    );
}
