use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStoreLimitConfig, RangeStoreLimits, ReadPoll,
    ReadRequest, RequestPriority, ResumeCheckpoint, SourceErrorCode, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_session::{
    RangeResumeArbiter, RangeResumeCancelOutcome, RangeResumeDispatch, RangeResumeErrorCategory,
    RangeResumeErrorCode, RangeResumeGeneration, RangeResumePhase, RangeResumeRecoverability,
    RangeResumeRegistrationOutcome, RangeResumeTarget,
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

    assert_eq!(
        arbiter.take_requeue().unwrap(),
        RangeResumeDispatch::Requeue(resume)
    );
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
    let earliest = arbiter.take_requeue().unwrap();
    assert_eq!(earliest, RangeResumeDispatch::Requeue(second_target));
    assert_ne!(second_target.generation(), RangeResumeGeneration::new(5));
    assert_eq!(
        arbiter.take_requeue().unwrap(),
        RangeResumeDispatch::Requeue(first_target)
    );
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
    assert_eq!(
        arbiter.take_requeue().unwrap(),
        RangeResumeDispatch::Requeue(second)
    );
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

    assert_eq!(
        arbiter.take_requeue().unwrap(),
        RangeResumeDispatch::Requeue(first)
    );
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
