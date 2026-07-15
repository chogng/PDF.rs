use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStoreLimitConfig, RangeStoreLimits, ReadPoll,
    ReadRequest, RequestPriority, ResumeCheckpoint, SourceError, SourceErrorCode, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_session::{
    RangeResumeArbiter, RangeResumeCancelOutcome, RangeResumeCompletion, RangeResumeDispatch,
    RangeResumeErrorCategory, RangeResumeErrorCode, RangeResumeGeneration, RangeResumePermit,
    RangeResumePhase, RangeResumeRecoverability, RangeResumeRegistrationOutcome, RangeResumeTarget,
};

fn snapshot(seed: u8, len: Option<u64>) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([seed; 32]),
            SourceRevision::new(u64::from(seed)),
        ),
        len,
        SourceValidator::new(
            SourceValidatorKind::StrongEntityTag,
            [seed.wrapping_add(1); 32],
        ),
    )
}

fn limits(max_total_subscriptions: usize) -> RangeStoreLimits {
    RangeStoreLimits::validate(RangeStoreLimitConfig {
        max_input_bytes: 32,
        max_read_bytes: 16,
        max_cached_bytes: 32,
        max_resident_bytes: 64,
        max_segments: 8,
        max_tickets: 8,
        max_subscribers_per_ticket: max_total_subscriptions,
        max_total_subscriptions,
        max_missing_ranges: 8,
    })
    .expect("test Range-store limits are valid")
}

fn request(start: u64, len: u64, job: u64, checkpoint: u64) -> ReadRequest {
    ReadRequest::new(
        ByteRange::new(start, len).expect("test range is valid"),
        RequestPriority::VisiblePage,
        JobId::new(job),
        ResumeCheckpoint::new(checkpoint),
    )
}

fn response(snapshot: SourceSnapshot, start: u64, bytes: &[u8]) -> RangeResponse {
    RangeResponse::new(
        snapshot,
        ByteRange::new(
            start,
            u64::try_from(bytes.len()).expect("test byte count fits u64"),
        )
        .expect("response range is valid"),
        bytes.to_vec(),
    )
    .expect("response geometry is valid")
}

fn pending(arbiter: &RangeResumeArbiter, request: ReadRequest) -> pdf_rs_bytes::DataTicket {
    match arbiter
        .byte_source()
        .expect("active arbiter lends its byte source")
        .poll(request)
    {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected Pending, got {other:?}"),
    }
}

fn target(job: u64, checkpoint: u64, generation: u64) -> RangeResumeTarget {
    RangeResumeTarget::new(
        JobId::new(job),
        ResumeCheckpoint::new(checkpoint),
        RangeResumeGeneration::new(generation),
    )
}

fn byte_source_error(arbiter: &RangeResumeArbiter) -> pdf_rs_session::RangeResumeError {
    match arbiter.byte_source() {
        Ok(_) => panic!("terminal arbiter must not lend its byte source"),
        Err(error) => error,
    }
}

fn take_permit(arbiter: &mut RangeResumeArbiter) -> RangeResumePermit {
    let expected_arbiter = arbiter.arbiter_id();
    match arbiter.take_requeue().unwrap() {
        RangeResumeDispatch::Requeue(permit) => {
            assert_eq!(permit.arbiter_id(), expected_arbiter);
            permit
        }
        RangeResumeDispatch::Empty => panic!("expected one completed Range resume permit"),
    }
}

#[test]
fn out_of_order_supply_queues_one_requeue_and_close_drops_all_resources() {
    let bound = snapshot(0x31, Some(8));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let read = request(0, 8, 7, 11);
    let resume = target(7, 11, 3);
    let ticket = pending(&arbiter, read);

    assert_eq!(arbiter.snapshot(), bound);
    assert_eq!(arbiter.phase(), RangeResumePhase::Active);
    assert_eq!(
        arbiter.register_pending(ticket, resume).unwrap(),
        RangeResumeRegistrationOutcome::Registered
    );
    assert_eq!(
        arbiter.register_pending(ticket, resume).unwrap(),
        RangeResumeRegistrationOutcome::AlreadyRegistered
    );
    assert_eq!(arbiter.resources().registrations(), 1);
    assert_eq!(arbiter.resources().pending_tickets(), 1);

    let upper = arbiter.supply(response(bound, 4, b"EFGH")).unwrap();
    assert_eq!(upper.ready_tickets(), 0);
    assert_eq!(upper.queued_requeues(), 0);
    assert_eq!(upper.cached_bytes(), 4);
    assert_eq!(arbiter.resources().ready_requeues(), 0);

    let lower = arbiter.supply(response(bound, 0, b"ABCD")).unwrap();
    assert_eq!(lower.ready_tickets(), 1);
    assert_eq!(lower.queued_requeues(), 1);
    assert_eq!(lower.cached_bytes(), 8);
    assert_eq!(arbiter.resources().pending_tickets(), 0);
    assert_eq!(arbiter.resources().ready_requeues(), 1);

    let permit = take_permit(&mut arbiter);
    assert_eq!(permit.ticket(), ticket);
    assert_eq!(permit.target(), resume);
    assert_eq!(arbiter.take_requeue().unwrap(), RangeResumeDispatch::Empty);
    assert_eq!(arbiter.resources().registrations(), 0);
    match arbiter.byte_source().unwrap().poll(read) {
        ReadPoll::Ready(bytes) => assert_eq!(bytes.bytes(), b"ABCDEFGH"),
        other => panic!("completed response must be readable: {other:?}"),
    }

    let report = arbiter.close();
    assert_eq!(report.released_registrations(), 0);
    assert_eq!(report.released_cached_bytes(), 8);
    assert_eq!(report.released_source_resident_bytes(), 8);
    assert!(report.released_registration_metadata_bytes() > 0);
    assert_eq!(
        report.released_resident_bytes(),
        report.released_source_resident_bytes() + report.released_registration_metadata_bytes()
    );
    assert_eq!(arbiter.phase(), RangeResumePhase::Closed);
    assert_eq!(arbiter.resources().cached_bytes(), 0);
    assert_eq!(arbiter.resources().resident_bytes(), 0);
    assert_eq!(arbiter.close(), report);
    assert_eq!(arbiter.release_report(), Some(report));
    let closed = byte_source_error(&arbiter);
    assert_eq!(closed.code(), RangeResumeErrorCode::Closed);
    assert_eq!(closed.category(), RangeResumeErrorCategory::Lifecycle);
    assert_eq!(
        closed.recoverability(),
        RangeResumeRecoverability::OpenNewSession
    );
}

#[test]
fn reverse_ticket_completion_preserves_sequence_for_scheduler_generation_validation() {
    let bound = snapshot(0x32, Some(8));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let first_target = target(1, 10, 5);
    let second_target = target(2, 20, 6);
    let first_ticket = pending(&arbiter, request(0, 4, 1, 10));
    arbiter
        .register_pending(first_ticket, first_target)
        .unwrap();
    let second_ticket = pending(&arbiter, request(4, 4, 2, 20));
    arbiter
        .register_pending(second_ticket, second_target)
        .unwrap();

    assert_eq!(
        arbiter
            .supply(response(bound, 4, b"EFGH"))
            .unwrap()
            .queued_requeues(),
        1
    );
    assert_eq!(
        arbiter
            .supply(response(bound, 0, b"ABCD"))
            .unwrap()
            .queued_requeues(),
        1
    );
    let earliest = take_permit(&mut arbiter);
    assert_eq!(earliest.ticket(), second_ticket);
    assert_eq!(earliest.target(), second_target);
    assert_ne!(second_target.generation(), RangeResumeGeneration::new(5));
    let next = take_permit(&mut arbiter);
    assert_eq!(next.ticket(), first_ticket);
    assert_eq!(next.target(), first_target);
    assert_eq!(arbiter.resources().registrations(), 0);
}

#[test]
fn exact_cancel_leaves_shared_ticket_live_and_never_requeues_cancelled_job() {
    let bound = snapshot(0x33, Some(4));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let first = target(1, 10, 1);
    let second = target(2, 20, 1);
    let first_ticket = pending(&arbiter, request(0, 4, 1, 10));
    let second_ticket = pending(&arbiter, request(0, 4, 2, 20));
    assert_eq!(second_ticket, first_ticket);
    arbiter.register_pending(first_ticket, first).unwrap();
    arbiter.register_pending(second_ticket, second).unwrap();

    assert_eq!(
        arbiter
            .cancel(JobId::new(1), RangeResumeGeneration::new(1))
            .unwrap(),
        RangeResumeCancelOutcome::Cancelled { target: first }
    );
    assert_eq!(
        arbiter
            .cancel(JobId::new(1), RangeResumeGeneration::new(1))
            .unwrap(),
        RangeResumeCancelOutcome::NotPending
    );
    assert_eq!(arbiter.resources().registrations(), 1);
    assert_eq!(arbiter.resources().pending_tickets(), 1);

    let supplied = arbiter.supply(response(bound, 0, b"ABCD")).unwrap();
    assert_eq!(supplied.queued_requeues(), 1);
    let permit = take_permit(&mut arbiter);
    assert_eq!(permit.ticket(), second_ticket);
    assert_eq!(permit.target(), second);
    assert_eq!(arbiter.take_requeue().unwrap(), RangeResumeDispatch::Empty);
}

#[test]
fn sole_cancel_releases_abandoned_ticket_and_ready_cancel_consumes_queue() {
    let bound = snapshot(0x34, Some(8));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let pending_target = target(1, 10, 7);
    let ticket = pending(&arbiter, request(0, 4, 1, 10));
    arbiter.register_pending(ticket, pending_target).unwrap();
    assert!(matches!(
        arbiter
            .cancel(JobId::new(1), RangeResumeGeneration::new(7))
            .unwrap(),
        RangeResumeCancelOutcome::Cancelled { .. }
    ));
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.resources().pending_tickets(), 0);
    assert_eq!(arbiter.resources().cached_bytes(), 0);
    assert_eq!(arbiter.resources().source_resident_bytes(), 0);
    assert!(arbiter.resources().registration_metadata_bytes() > 0);
    assert_eq!(
        arbiter.resources().resident_bytes(),
        arbiter.resources().registration_metadata_bytes()
    );

    let ready_target = target(2, 20, 7);
    let ready_ticket = pending(&arbiter, request(4, 4, 2, 20));
    arbiter
        .register_pending(ready_ticket, ready_target)
        .unwrap();
    arbiter.supply(response(bound, 4, b"EFGH")).unwrap();
    assert_eq!(arbiter.resources().ready_requeues(), 1);
    assert_eq!(
        arbiter
            .cancel(JobId::new(2), RangeResumeGeneration::new(7))
            .unwrap(),
        RangeResumeCancelOutcome::Cancelled {
            target: ready_target
        }
    );
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.take_requeue().unwrap(), RangeResumeDispatch::Empty);
}

#[test]
fn shared_host_failure_queues_ordered_move_only_failures_without_resume() {
    let bound = snapshot(0x44, Some(4));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let first = target(1, 10, 3);
    let second = target(2, 20, 4);
    let third = target(3, 30, 5);
    let ticket = pending(&arbiter, request(0, 4, 1, 10));
    let shared = pending(&arbiter, request(0, 4, 2, 20));
    let also_shared = pending(&arbiter, request(0, 4, 3, 30));
    assert_eq!(shared, ticket);
    assert_eq!(also_shared, ticket);
    arbiter.register_pending(ticket, first).unwrap();
    arbiter.register_pending(shared, second).unwrap();
    arbiter.register_pending(also_shared, third).unwrap();

    let phase_before = arbiter.phase();
    let failure = SourceError::source_unavailable();
    let outcome = arbiter.fail_ticket(ticket).unwrap();
    assert_eq!(outcome.ticket(), ticket);
    assert_eq!(outcome.queued_failures(), 3);
    assert_eq!(arbiter.phase(), phase_before);
    assert_eq!(arbiter.resources().pending_tickets(), 0);
    assert_eq!(arbiter.resources().ready_resumes(), 0);
    assert_eq!(arbiter.resources().ready_requeues(), 0);
    assert_eq!(arbiter.resources().queued_failures(), 3);
    let duplicate = arbiter
        .fail_ticket(ticket)
        .expect_err("a released failed ticket cannot complete twice");
    assert_eq!(
        duplicate.code(),
        RangeResumeErrorCode::Source(SourceErrorCode::UnknownTicket)
    );
    assert_eq!(
        duplicate.source_error().map(SourceError::code),
        Some(SourceErrorCode::UnknownTicket)
    );
    assert_eq!(arbiter.phase(), RangeResumePhase::Active);
    assert_eq!(arbiter.resources().queued_failures(), 3);
    assert_eq!(
        arbiter.take_requeue().unwrap(),
        RangeResumeDispatch::Empty,
        "host failure must never be converted into parser-resume authority"
    );

    for expected in [first, second, third] {
        match arbiter.take_completion().unwrap() {
            RangeResumeCompletion::Failed(permit) => {
                assert_eq!(permit.arbiter_id(), arbiter.arbiter_id());
                assert_eq!(permit.ticket(), ticket);
                assert_eq!(permit.target(), expected);
                assert_eq!(permit.error(), failure);
            }
            other => panic!("shared host failure must preserve subscription order: {other:?}"),
        }
    }
    assert_eq!(
        arbiter.take_completion().unwrap(),
        RangeResumeCompletion::Empty
    );
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.resources().queued_failures(), 0);
}

#[test]
fn foreign_store_ticket_collision_cannot_fail_local_work() {
    let bound = snapshot(0x4e, Some(4));
    let mut local = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let foreign = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let local_ticket = pending(&local, request(0, 4, 1, 10));
    let foreign_ticket = pending(&foreign, request(0, 4, 1, 10));
    assert_eq!(local_ticket.value(), foreign_ticket.value());
    assert_ne!(local_ticket, foreign_ticket);
    local
        .register_pending(local_ticket, target(1, 10, 1))
        .unwrap();

    let error = local
        .fail_ticket(foreign_ticket)
        .expect_err("a foreign store ticket cannot terminate a numeric collision");
    assert_eq!(
        error.code(),
        RangeResumeErrorCode::Source(SourceErrorCode::UnknownTicket)
    );
    assert_eq!(local.phase(), RangeResumePhase::Active);
    assert_eq!(local.resources().registrations(), 1);
    assert_eq!(local.resources().pending_tickets(), 1);
    assert_eq!(local.resources().queued_failures(), 0);

    assert_eq!(
        local.fail_ticket(local_ticket).unwrap().queued_failures(),
        1
    );
    match local.take_completion().unwrap() {
        RangeResumeCompletion::Failed(permit) => {
            assert_eq!(permit.ticket(), local_ticket);
            assert_eq!(permit.error(), SourceError::source_unavailable());
        }
        other => panic!("the exact local ticket must fail once: {other:?}"),
    }
}

#[test]
fn unified_completion_order_covers_data_ready_then_host_failure() {
    let bound = snapshot(0x45, Some(8));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let resume_target = target(1, 10, 1);
    let failure_target = target(2, 20, 1);
    let resume_ticket = pending(&arbiter, request(0, 4, 1, 10));
    arbiter
        .register_pending(resume_ticket, resume_target)
        .unwrap();
    let failure_ticket = pending(&arbiter, request(4, 4, 2, 20));
    arbiter
        .register_pending(failure_ticket, failure_target)
        .unwrap();

    arbiter.supply(response(bound, 0, b"ABCD")).unwrap();
    let failure = SourceError::source_unavailable();
    arbiter.fail_ticket(failure_ticket).unwrap();
    assert_eq!(arbiter.resources().ready_resumes(), 1);
    assert_eq!(arbiter.resources().queued_failures(), 1);

    match arbiter.take_completion().unwrap() {
        RangeResumeCompletion::Resume(permit) => {
            assert_eq!(permit.ticket(), resume_ticket);
            assert_eq!(permit.target(), resume_target);
        }
        other => panic!("data-ready completion was sequenced first: {other:?}"),
    }
    match arbiter.take_completion().unwrap() {
        RangeResumeCompletion::Failed(permit) => {
            assert_eq!(permit.ticket(), failure_ticket);
            assert_eq!(permit.target(), failure_target);
            assert_eq!(permit.error(), failure);
        }
        other => panic!("host failure was sequenced second: {other:?}"),
    }
    assert_eq!(
        arbiter.take_completion().unwrap(),
        RangeResumeCompletion::Empty
    );
}

#[test]
fn unified_completion_order_covers_host_failure_then_data_ready() {
    let bound = snapshot(0x4b, Some(8));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let failure_target = target(1, 10, 1);
    let resume_target = target(2, 20, 1);
    let failure_ticket = pending(&arbiter, request(0, 4, 1, 10));
    arbiter
        .register_pending(failure_ticket, failure_target)
        .unwrap();
    let resume_ticket = pending(&arbiter, request(4, 4, 2, 20));
    arbiter
        .register_pending(resume_ticket, resume_target)
        .unwrap();

    let failure = SourceError::source_unavailable();
    arbiter.fail_ticket(failure_ticket).unwrap();
    arbiter.supply(response(bound, 4, b"EFGH")).unwrap();

    match arbiter.take_completion().unwrap() {
        RangeResumeCompletion::Failed(permit) => {
            assert_eq!(permit.ticket(), failure_ticket);
            assert_eq!(permit.target(), failure_target);
            assert_eq!(permit.error(), failure);
        }
        other => panic!("host failure was sequenced first: {other:?}"),
    }
    match arbiter.take_completion().unwrap() {
        RangeResumeCompletion::Resume(permit) => {
            assert_eq!(permit.ticket(), resume_ticket);
            assert_eq!(permit.target(), resume_target);
        }
        other => panic!("data-ready completion was sequenced second: {other:?}"),
    }
    assert_eq!(
        arbiter.take_completion().unwrap(),
        RangeResumeCompletion::Empty
    );
}

#[test]
fn cancel_before_failure_preserves_shared_survivor_and_taken_permit() {
    let bound = snapshot(0x4c, Some(4));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let cancelled = target(1, 10, 9);
    let survivor = target(2, 20, 9);
    let ticket = pending(&arbiter, request(0, 4, 1, 10));
    let shared = pending(&arbiter, request(0, 4, 2, 20));
    arbiter.register_pending(ticket, cancelled).unwrap();
    arbiter.register_pending(shared, survivor).unwrap();

    assert_eq!(
        arbiter
            .cancel(JobId::new(1), RangeResumeGeneration::new(9))
            .unwrap(),
        RangeResumeCancelOutcome::Cancelled { target: cancelled }
    );
    let failure = SourceError::source_unavailable();
    let outcome = arbiter.fail_ticket(ticket).unwrap();
    assert_eq!(outcome.queued_failures(), 1);
    let permit = match arbiter.take_completion().unwrap() {
        RangeResumeCompletion::Failed(permit) => permit,
        other => panic!("the shared survivor must receive the failure: {other:?}"),
    };
    assert_eq!(permit.target(), survivor);
    assert_eq!(permit.error(), failure);
    assert_eq!(
        arbiter
            .cancel(JobId::new(2), RangeResumeGeneration::new(9))
            .unwrap(),
        RangeResumeCancelOutcome::NotPending,
        "a taken move-only permit is already outside arbiter ownership"
    );
    assert_eq!(permit.ticket(), ticket);
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.resources().queued_failures(), 0);
}

#[test]
fn exact_cancel_removes_one_queued_failure_and_close_releases_the_rest() {
    let bound = snapshot(0x46, Some(4));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let first = target(1, 10, 7);
    let second = target(2, 20, 7);
    let ticket = pending(&arbiter, request(0, 4, 1, 10));
    let shared = pending(&arbiter, request(0, 4, 2, 20));
    arbiter.register_pending(ticket, first).unwrap();
    arbiter.register_pending(shared, second).unwrap();
    arbiter.fail_ticket(ticket).unwrap();
    assert_eq!(arbiter.resources().queued_failures(), 2);

    assert_eq!(
        arbiter
            .cancel(JobId::new(1), RangeResumeGeneration::new(7))
            .unwrap(),
        RangeResumeCancelOutcome::Cancelled { target: first }
    );
    assert_eq!(arbiter.resources().registrations(), 1);
    assert_eq!(arbiter.resources().queued_failures(), 1);
    assert_eq!(
        arbiter
            .cancel(JobId::new(1), RangeResumeGeneration::new(7))
            .unwrap(),
        RangeResumeCancelOutcome::NotPending
    );

    let report = arbiter.close();
    assert_eq!(report.released_registrations(), 1);
    assert_eq!(report.released_pending_tickets(), 0);
    assert_eq!(report.released_ready_resumes(), 0);
    assert_eq!(report.released_ready_requeues(), 0);
    assert_eq!(report.released_queued_failures(), 1);
    assert_eq!(arbiter.resources().queued_failures(), 0);
    assert_eq!(arbiter.resources().resident_bytes(), 0);
    assert_eq!(arbiter.close(), report);
    assert_eq!(
        arbiter.take_completion().unwrap_err().code(),
        RangeResumeErrorCode::Closed
    );
}

#[test]
fn surplus_runtime_registration_fails_closed_for_data_and_host_failure() {
    let bound = snapshot(0x4d, Some(4));
    for host_failure in [false, true] {
        let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
        let ticket = pending(&arbiter, request(0, 4, 1, 10));
        arbiter.register_pending(ticket, target(1, 10, 1)).unwrap();
        arbiter
            .register_pending(ticket, target(2, 20, 1))
            .expect("the surplus registration exposes the terminal bijection guard");

        let error = if host_failure {
            arbiter
                .fail_ticket(ticket)
                .expect_err("host failure must reject a surplus runtime registration")
        } else {
            arbiter
                .supply(response(bound, 0, b"ABCD"))
                .expect_err("data readiness must reject a surplus runtime registration")
        };
        assert_eq!(error.code(), RangeResumeErrorCode::UnregisteredSubscription);
        assert_eq!(error.category(), RangeResumeErrorCategory::Internal);
        assert_eq!(arbiter.phase(), RangeResumePhase::Failed);
        assert_eq!(arbiter.resources().registrations(), 0);
        assert_eq!(arbiter.resources().pending_tickets(), 0);
        assert_eq!(arbiter.resources().ready_resumes(), 0);
        assert_eq!(arbiter.resources().queued_failures(), 0);
        assert_eq!(arbiter.resources().resident_bytes(), 0);
        let report = arbiter.release_report().unwrap();
        assert_eq!(report.released_registrations(), 2);
        assert_eq!(report.released_pending_tickets(), 1);
        assert_eq!(report.released_ready_resumes(), 0);
        assert_eq!(report.released_queued_failures(), 0);
    }
}

#[test]
fn source_change_signal_poisoning_wins_without_failure_permit() {
    let bound = snapshot(0x47, Some(4));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let ticket = pending(&arbiter, request(0, 4, 1, 10));
    arbiter.register_pending(ticket, target(1, 10, 1)).unwrap();
    arbiter.supply(response(bound, 0, b"AB")).unwrap();

    let report = arbiter
        .signal_source_changed()
        .expect("the source-wide signal poisons the complete arbiter");
    assert_eq!(arbiter.phase(), RangeResumePhase::SourceChanged);
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.resources().ready_resumes(), 0);
    assert_eq!(arbiter.resources().queued_failures(), 0);
    assert_eq!(arbiter.resources().resident_bytes(), 0);

    assert_eq!(arbiter.release_report(), Some(report));
    assert_eq!(report.released_registrations(), 1);
    assert_eq!(report.released_pending_tickets(), 1);
    assert_eq!(report.released_ready_resumes(), 0);
    assert_eq!(report.released_queued_failures(), 0);
    assert_eq!(report.released_cached_bytes(), 2);
    assert_eq!(
        arbiter.take_completion().unwrap_err().code(),
        RangeResumeErrorCode::SourceChanged
    );
    assert_eq!(arbiter.close(), report);
    assert_eq!(arbiter.phase(), RangeResumePhase::SourceChanged);
}

#[test]
fn late_source_change_wins_after_resume_failure_or_ticket_release() {
    let bound = snapshot(0x49, Some(4));

    let mut resume_ready = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let resume_ticket = pending(&resume_ready, request(0, 4, 1, 10));
    resume_ready
        .register_pending(resume_ticket, target(1, 10, 1))
        .unwrap();
    resume_ready.supply(response(bound, 0, b"ABCD")).unwrap();
    assert_eq!(resume_ready.resources().ready_resumes(), 1);
    let resume_report = resume_ready.signal_source_changed().unwrap();
    assert_eq!(resume_report.released_ready_resumes(), 1);
    assert_eq!(resume_report.released_queued_failures(), 0);

    let mut failure_ready = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let failure_ticket = pending(&failure_ready, request(0, 4, 2, 20));
    failure_ready
        .register_pending(failure_ticket, target(2, 20, 1))
        .unwrap();
    failure_ready.fail_ticket(failure_ticket).unwrap();
    assert_eq!(failure_ready.resources().queued_failures(), 1);
    let failure_report = failure_ready.signal_source_changed().unwrap();
    assert_eq!(failure_report.released_ready_resumes(), 0);
    assert_eq!(failure_report.released_queued_failures(), 1);

    let mut released = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let released_ticket = pending(&released, request(0, 4, 3, 30));
    released
        .register_pending(released_ticket, target(3, 30, 1))
        .unwrap();
    released.supply(response(bound, 0, b"ABCD")).unwrap();
    assert!(matches!(
        released.take_completion().unwrap(),
        RangeResumeCompletion::Resume(_)
    ));
    assert_eq!(released.resources().registrations(), 0);
    let released_report = released.signal_source_changed().unwrap();
    assert_eq!(released_report.released_registrations(), 0);
    assert_eq!(released_report.released_ready_resumes(), 0);
    assert_eq!(released_report.released_queued_failures(), 0);
    assert_eq!(released_report.released_cached_bytes(), 4);

    let mut closed = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    closed.close();
    assert_eq!(
        closed.signal_source_changed().unwrap_err().code(),
        RangeResumeErrorCode::Closed
    );
    assert_eq!(closed.phase(), RangeResumePhase::Closed);
}

#[test]
fn mismatched_snapshot_is_a_stable_terminal_and_drops_cached_and_pending_state() {
    let bound = snapshot(0x35, Some(8));
    let changed = snapshot(0x36, Some(8));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let ticket = pending(&arbiter, request(0, 8, 1, 10));
    arbiter.register_pending(ticket, target(1, 10, 1)).unwrap();
    arbiter.supply(response(bound, 0, b"ABCD")).unwrap();
    assert_eq!(arbiter.resources().cached_bytes(), 4);
    assert_eq!(arbiter.resources().registrations(), 1);

    let error = arbiter
        .supply(response(changed, 4, b"EFGH"))
        .expect_err("snapshot mismatch terminates the arbiter");
    assert_eq!(error.code(), RangeResumeErrorCode::SourceChanged);
    assert_eq!(error.category(), RangeResumeErrorCategory::Integrity);
    assert_eq!(
        error.recoverability(),
        RangeResumeRecoverability::ReopenSource
    );
    assert_eq!(
        error
            .source_error()
            .expect("lower evidence is retained")
            .code(),
        SourceErrorCode::SourceChanged
    );
    assert_eq!(arbiter.phase(), RangeResumePhase::SourceChanged);
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.resources().cached_bytes(), 0);
    assert_eq!(arbiter.resources().resident_bytes(), 0);

    let report = arbiter.release_report().unwrap();
    assert_eq!(report.released_registrations(), 1);
    assert_eq!(report.released_pending_tickets(), 1);
    assert_eq!(report.released_ready_requeues(), 0);
    assert_eq!(report.released_cached_bytes(), 4);
    assert_eq!(report.released_source_resident_bytes(), 4);
    assert!(report.released_registration_metadata_bytes() > 0);
    assert_eq!(
        report.released_resident_bytes(),
        report.released_source_resident_bytes() + report.released_registration_metadata_bytes()
    );
    assert_eq!(arbiter.signal_source_changed().unwrap(), report);
    assert_eq!(arbiter.close(), report);
    assert_eq!(arbiter.phase(), RangeResumePhase::SourceChanged);
    for terminal in [
        byte_source_error(&arbiter),
        arbiter.take_requeue().unwrap_err(),
    ] {
        assert_eq!(terminal.code(), RangeResumeErrorCode::SourceChanged);
        assert_eq!(terminal.diagnostic_id(), "RPE-SESSION-0004");
    }
}

#[test]
fn explicit_source_change_and_close_are_idempotent_zero_resource_terminals() {
    let bound = snapshot(0x37, Some(4));
    let mut changed = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let ticket = pending(&changed, request(0, 4, 1, 1));
    changed.register_pending(ticket, target(1, 1, 1)).unwrap();
    let changed_report = changed.signal_source_changed().unwrap();
    assert_eq!(changed_report.released_registrations(), 1);
    assert_eq!(changed.phase(), RangeResumePhase::SourceChanged);
    assert_eq!(changed.resources().registrations(), 0);
    assert_eq!(changed.signal_source_changed().unwrap(), changed_report);

    let mut closed = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let ticket = pending(&closed, request(0, 4, 2, 2));
    closed.register_pending(ticket, target(2, 2, 2)).unwrap();
    let close_report = closed.close();
    assert_eq!(close_report.released_registrations(), 1);
    assert_eq!(close_report.released_pending_tickets(), 1);
    assert_eq!(closed.phase(), RangeResumePhase::Closed);
    assert_eq!(closed.resources().registrations(), 0);
    assert_eq!(closed.resources().resident_bytes(), 0);
    assert_eq!(closed.close(), close_report);
    assert_eq!(
        closed.signal_source_changed().unwrap_err().code(),
        RangeResumeErrorCode::Closed
    );
}

#[test]
fn queued_metadata_is_bounded_and_rejected_subscription_is_rolled_back() {
    let bound = snapshot(0x38, Some(8));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(1)).unwrap();
    let first = target(1, 10, 1);
    let first_ticket = pending(&arbiter, request(0, 4, 1, 10));
    arbiter.register_pending(first_ticket, first).unwrap();
    arbiter.supply(response(bound, 0, b"ABCD")).unwrap();
    assert_eq!(arbiter.resources().ready_requeues(), 1);

    let second_ticket = pending(&arbiter, request(4, 4, 2, 20));
    let error = arbiter
        .register_pending(second_ticket, target(2, 20, 1))
        .expect_err("queued target consumes the bounded metadata slot");
    assert_eq!(error.code(), RangeResumeErrorCode::RegistrationLimit);
    assert_eq!(error.category(), RangeResumeErrorCategory::Resource);
    let limit = error.limit().expect("bounded rejection has context");
    assert_eq!(limit.limit(), 1);
    assert_eq!(limit.attempted(), 2);
    assert_eq!(arbiter.resources().registrations(), 1);
    assert_eq!(arbiter.resources().pending_tickets(), 0);

    let permit = take_permit(&mut arbiter);
    assert_eq!(permit.ticket(), first_ticket);
    assert_eq!(permit.target(), first);
    assert_eq!(arbiter.resources().registrations(), 0);
    let replacement_ticket = pending(&arbiter, request(4, 4, 2, 20));
    arbiter
        .register_pending(replacement_ticket, target(2, 20, 1))
        .expect("rollback released the rejected store subscription");
}

#[test]
fn unregistered_checkpoint_mismatch_fails_closed_without_leaking_resources() {
    let bound = snapshot(0x39, Some(4));
    let mut arbiter = RangeResumeArbiter::new(bound, limits(8)).unwrap();
    let ticket = pending(&arbiter, request(0, 4, 1, 10));
    arbiter
        .register_pending(ticket, target(1, 99, 1))
        .expect("ticket shape is pending; checkpoint is verified at completion");

    let error = arbiter
        .supply(response(bound, 0, b"ABCD"))
        .expect_err("subscription evidence must match the runtime registration");
    assert_eq!(error.code(), RangeResumeErrorCode::UnregisteredSubscription);
    assert_eq!(error.category(), RangeResumeErrorCategory::Internal);
    assert_eq!(arbiter.phase(), RangeResumePhase::Failed);
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.resources().cached_bytes(), 0);
    assert_eq!(arbiter.resources().resident_bytes(), 0);
    assert_eq!(
        byte_source_error(&arbiter).code(),
        RangeResumeErrorCode::ArbiterFailed
    );
}
