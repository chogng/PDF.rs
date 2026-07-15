#[allow(
    dead_code,
    reason = "shared integration support also serves the sibling owner test binaries"
)]
mod support;

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RequestPriority, ResumeCheckpoint, SmallRanges, SourceError,
};
use pdf_rs_document::{
    DocumentCancellation, DocumentLimits, NeverCancelled, OpenStrictBaseRevisionJob,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionId, StrictBaseOpenContext,
    StrictBaseOpenLimits,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_session::{
    RangeResumeErrorCode, StrictBaseOpenCoordinator, StrictBaseOpenCoordinatorCancel,
    StrictBaseOpenCoordinatorFailure, StrictBaseOpenCoordinatorPhase, StrictBaseOpenCoordinatorRun,
    StrictBaseOpenCoordinatorSourceChange, StrictBaseOpenIngress,
    StrictBaseOpenIngressRejectReason,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{XrefJobContext, XrefLimits};

use support::{Fixture, fixture};

const OPEN_JOB: JobId = JobId::new(5_001);
const TAIL_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(5_002);
const SECTION_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(5_003);
const SCAN_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(5_004);
const ENVELOPE_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(5_005);
const BOUNDARY_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(5_006);
const GENERATION: pdf_rs_session::RangeResumeGeneration =
    pdf_rs_session::RangeResumeGeneration::new(29);

fn strict_job(fixture: &Fixture) -> OpenStrictBaseRevisionJob {
    OpenStrictBaseRevisionJob::new(
        fixture.snapshot(),
        RevisionId::new(61),
        StrictBaseOpenContext::new(
            XrefJobContext::new(OPEN_JOB, TAIL_CHECKPOINT, SECTION_CHECKPOINT),
            RevisionAttestationJobContext::new(
                OPEN_JOB,
                SCAN_CHECKPOINT,
                ENVELOPE_CHECKPOINT,
                BOUNDARY_CHECKPOINT,
                RequestPriority::Metadata,
            ),
        ),
        StrictBaseOpenLimits::new(
            XrefLimits::default(),
            DocumentLimits::default(),
            RevisionAttestationLimits::default(),
            ObjectLimits::default(),
            SyntaxLimits::default(),
        ),
    )
    .expect("the self-authored strict-open job validates")
}

fn coordinator(fixture: &Fixture) -> StrictBaseOpenCoordinator {
    StrictBaseOpenCoordinator::new(strict_job(fixture), GENERATION, Default::default())
        .expect("the default Range limits validate")
}

fn response(fixture: &Fixture, range: ByteRange) -> RangeResponse {
    response_for_snapshot(fixture, fixture.snapshot(), range)
}

fn response_for_snapshot(
    fixture: &Fixture,
    snapshot: pdf_rs_bytes::SourceSnapshot,
    range: ByteRange,
) -> RangeResponse {
    let start = usize::try_from(range.start()).expect("fixture offset fits usize");
    let end = usize::try_from(range.end_exclusive()).expect("fixture offset fits usize");
    RangeResponse::new(snapshot, range, fixture.bytes()[start..end].to_vec())
        .expect("fixture response exactly matches its range")
}

fn complete_missing_out_of_order(
    coordinator: &mut StrictBaseOpenCoordinator,
    fixture: &Fixture,
    missing: SmallRanges,
) {
    let mut lower_halves = Vec::with_capacity(missing.len());
    for range in missing.as_slice().iter().copied() {
        assert!(range.len() > 1, "fixture requests must remain splittable");
        let lower_len = range.len() / 2;
        let lower = ByteRange::new(range.start(), lower_len).unwrap();
        let upper = ByteRange::new(range.start() + lower_len, range.len() - lower_len).unwrap();
        assert!(matches!(
            coordinator.supply(response(fixture, upper)),
            StrictBaseOpenIngress::Accepted {
                wake_scheduler: false,
                ..
            }
        ));
        lower_halves.push(lower);
    }

    let last = lower_halves.len() - 1;
    for (index, lower) in lower_halves.into_iter().enumerate() {
        match coordinator.supply(response(fixture, lower)) {
            StrictBaseOpenIngress::Accepted { wake_scheduler, .. } => {
                assert_eq!(wake_scheduler, index == last)
            }
            other => panic!("valid snapshot data must be accepted: {other:?}"),
        }
    }
}

struct PanicOnCancellationProbe;

impl DocumentCancellation for PanicOnCancellationProbe {
    fn is_cancelled(&self) -> bool {
        panic!("a queued host failure must not probe parser cancellation")
    }
}

#[test]
fn pending_is_registered_before_publication_and_ingress_never_polls_inline() {
    let fixture = fixture(0x91);
    let mut coordinator = coordinator(&fixture);
    assert_eq!(coordinator.snapshot(), fixture.snapshot());
    assert_eq!(coordinator.resources().jobs(), 1);
    assert_eq!(coordinator.resources().registrations(), 0);
    assert!(matches!(
        coordinator.supply(response(
            &fixture,
            ByteRange::new(0, 1).expect("one-byte range validates")
        )),
        StrictBaseOpenIngress::Rejected {
            phase: StrictBaseOpenCoordinatorPhase::Queued,
            reason: StrictBaseOpenIngressRejectReason::NotWaiting,
        }
    ));

    let (ticket, missing) = match coordinator.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { ticket, missing } => (ticket, missing),
        other => panic!("empty source must suspend on its first turn: {other:?}"),
    };
    assert_eq!(ticket.value(), 1);
    assert_eq!(coordinator.waiting_checkpoint(), Some(TAIL_CHECKPOINT));
    assert_eq!(
        coordinator.phase(),
        StrictBaseOpenCoordinatorPhase::WaitingForData
    );
    assert_eq!(coordinator.resources().jobs(), 1);
    assert_eq!(coordinator.resources().waiting_targets(), 1);
    assert_eq!(coordinator.resources().registrations(), 1);
    assert_eq!(coordinator.resources().pending_tickets(), 1);

    let stats = coordinator.stats();
    let job_phase = coordinator.job_phase();
    let first = missing.as_slice()[0];
    let partial = ByteRange::new(first.start(), first.len() / 2).unwrap();
    assert!(matches!(
        coordinator.supply(response(&fixture, partial)),
        StrictBaseOpenIngress::Accepted {
            wake_scheduler: false,
            ..
        }
    ));
    assert_eq!(coordinator.stats(), stats);
    assert_eq!(coordinator.job_phase(), job_phase);
    assert!(matches!(
        coordinator.run_one(&PanicOnCancellationProbe),
        StrictBaseOpenCoordinatorRun::NoWork
    ));
    assert_eq!(coordinator.stats(), stats);

    let report = coordinator.close();
    assert_eq!(
        report.previous_phase(),
        StrictBaseOpenCoordinatorPhase::WaitingForData
    );
    assert_eq!(coordinator.resources().jobs(), 0);
    assert_eq!(coordinator.resources().resident_bytes(), 0);
}

#[test]
fn out_of_order_turns_reach_one_ready_handoff_without_exposing_source_owners() {
    let fixture = fixture(0x92);
    let mut coordinator = coordinator(&fixture);
    let mut checkpoints = Vec::new();
    let mut outcome = coordinator.run_one(&NeverCancelled);

    let ready = loop {
        match outcome {
            StrictBaseOpenCoordinatorRun::WaitingForData { missing, .. } => {
                checkpoints.push(
                    coordinator
                        .waiting_checkpoint()
                        .expect("published suspension retains its checkpoint"),
                );
                let stats = coordinator.stats();
                let job_phase = coordinator.job_phase();
                complete_missing_out_of_order(&mut coordinator, &fixture, missing);
                assert_eq!(
                    coordinator.phase(),
                    StrictBaseOpenCoordinatorPhase::ResumeQueued
                );
                assert_eq!(coordinator.stats(), stats);
                assert_eq!(coordinator.job_phase(), job_phase);
                assert_eq!(coordinator.resources().ready_resumes(), 1);
                outcome = coordinator.run_one(&NeverCancelled);
            }
            StrictBaseOpenCoordinatorRun::Ready(ready) => break ready,
            other => panic!("self-authored strict open must progress to Ready: {other:?}"),
        }
    };

    assert_eq!(checkpoints.first(), Some(&TAIL_CHECKPOINT));
    assert_eq!(ready.index().snapshot(), fixture.snapshot());
    assert_eq!(ready.index().object_attestations().len(), 2);
    assert_eq!(
        coordinator.phase(),
        StrictBaseOpenCoordinatorPhase::ReadyHandedOff
    );
    assert_eq!(coordinator.resources().jobs(), 0);
    assert_eq!(coordinator.resources().registrations(), 0);
    assert_eq!(coordinator.resources().resident_bytes(), 0);
    assert!(matches!(
        coordinator.run_one(&PanicOnCancellationProbe),
        StrictBaseOpenCoordinatorRun::AlreadyTerminal {
            phase: StrictBaseOpenCoordinatorPhase::ReadyHandedOff
        }
    ));
    assert_eq!(
        coordinator.cancel(),
        StrictBaseOpenCoordinatorCancel::AlreadyTerminal {
            phase: StrictBaseOpenCoordinatorPhase::ReadyHandedOff
        }
    );

    let handoff_resources = ready.source_resources();
    assert_eq!(handoff_resources.registrations(), 0);
    assert_eq!(handoff_resources.ready_resumes(), 0);
    assert_eq!(handoff_resources.queued_failures(), 0);
    assert_eq!(
        handoff_resources.cached_bytes(),
        u64::try_from(fixture.bytes().len()).unwrap()
    );
    let coordinator_report = coordinator.close();
    assert_eq!(
        coordinator_report.previous_phase(),
        StrictBaseOpenCoordinatorPhase::ReadyHandedOff
    );
    assert_eq!(coordinator_report.source(), None);
    assert_eq!(ready.source_resources(), handoff_resources);
    let source_report = ready.close();
    assert_eq!(
        source_report.released_cached_bytes(),
        u64::try_from(fixture.bytes().len()).unwrap()
    );
    assert_eq!(coordinator.close(), coordinator_report);
}

#[test]
fn host_failure_queues_before_a_non_polling_failed_turn() {
    let fixture = fixture(0x93);
    let mut coordinator = coordinator(&fixture);
    let ticket = match coordinator.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { ticket, .. } => ticket,
        other => panic!("empty source must suspend before host failure: {other:?}"),
    };
    let stats = coordinator.stats();
    let job_phase = coordinator.job_phase();
    assert!(matches!(
        coordinator.fail_data(ticket),
        StrictBaseOpenIngress::Accepted {
            wake_scheduler: true,
            ..
        }
    ));
    assert_eq!(
        coordinator.phase(),
        StrictBaseOpenCoordinatorPhase::FailureQueued
    );
    assert_eq!(coordinator.stats(), stats);
    assert_eq!(coordinator.job_phase(), job_phase);
    assert_eq!(coordinator.resources().queued_failures(), 1);
    match coordinator.fail_data(ticket) {
        StrictBaseOpenIngress::Rejected {
            phase: StrictBaseOpenCoordinatorPhase::FailureQueued,
            reason: StrictBaseOpenIngressRejectReason::Range(error),
        } => assert!(matches!(error.code(), RangeResumeErrorCode::Source(_))),
        other => panic!("duplicate host failure must be a stable active rejection: {other:?}"),
    }
    assert_eq!(
        coordinator.phase(),
        StrictBaseOpenCoordinatorPhase::FailureQueued
    );
    assert_eq!(coordinator.resources().queued_failures(), 1);

    let failure = StrictBaseOpenCoordinatorFailure::Source(SourceError::source_unavailable());
    assert!(matches!(
        coordinator.run_one(&PanicOnCancellationProbe),
        StrictBaseOpenCoordinatorRun::Failed(observed) if observed == failure
    ));
    assert_eq!(coordinator.failure(), Some(failure));
    assert_eq!(coordinator.phase(), StrictBaseOpenCoordinatorPhase::Failed);
    assert_eq!(coordinator.stats(), stats);
    assert_eq!(coordinator.job_phase(), job_phase);
    assert_eq!(coordinator.resources().jobs(), 0);
    assert_eq!(coordinator.resources().resident_bytes(), 0);
    assert!(matches!(
        coordinator.run_one(&PanicOnCancellationProbe),
        StrictBaseOpenCoordinatorRun::AlreadyTerminal {
            phase: StrictBaseOpenCoordinatorPhase::Failed
        }
    ));
    assert!(matches!(
        coordinator.fail_data(ticket),
        StrictBaseOpenIngress::Rejected {
            phase: StrictBaseOpenCoordinatorPhase::Failed,
            reason: StrictBaseOpenIngressRejectReason::TerminalPhase,
        }
    ));
}

#[test]
fn queued_completions_lose_to_cancel_close_and_source_change() {
    let fixture = fixture(0x94);

    let mut cancelled = coordinator(&fixture);
    let (ticket, missing) = match cancelled.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { ticket, missing } => (ticket, missing),
        other => panic!("cancel fixture must suspend: {other:?}"),
    };
    let repeated_range = missing.as_slice()[0];
    complete_missing_out_of_order(&mut cancelled, &fixture, missing);
    assert_eq!(
        cancelled.phase(),
        StrictBaseOpenCoordinatorPhase::ResumeQueued
    );
    assert!(matches!(
        cancelled.supply(response(&fixture, repeated_range)),
        StrictBaseOpenIngress::Accepted {
            wake_scheduler: false,
            ..
        }
    ));
    assert!(matches!(
        cancelled.fail_data(ticket),
        StrictBaseOpenIngress::Rejected {
            phase: StrictBaseOpenCoordinatorPhase::ResumeQueued,
            reason: StrictBaseOpenIngressRejectReason::Range(_),
        }
    ));
    assert_eq!(
        cancelled.cancel(),
        StrictBaseOpenCoordinatorCancel::Cancelled
    );
    assert_eq!(cancelled.phase(), StrictBaseOpenCoordinatorPhase::Cancelled);
    assert_eq!(cancelled.resources().resident_bytes(), 0);
    assert!(matches!(
        cancelled.run_one(&PanicOnCancellationProbe),
        StrictBaseOpenCoordinatorRun::AlreadyTerminal {
            phase: StrictBaseOpenCoordinatorPhase::Cancelled
        }
    ));

    let mut closed = coordinator(&fixture);
    let ticket = match closed.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { ticket, .. } => ticket,
        other => panic!("close fixture must suspend: {other:?}"),
    };
    assert!(matches!(
        closed.fail_data(ticket),
        StrictBaseOpenIngress::Accepted {
            wake_scheduler: true,
            ..
        }
    ));
    let report = closed.close();
    assert_eq!(closed.close(), report);
    assert_eq!(
        report.previous_phase(),
        StrictBaseOpenCoordinatorPhase::FailureQueued
    );
    assert_eq!(report.owner().released_jobs(), 1);
    assert_eq!(report.owner().released_waiting_targets(), 1);
    assert_eq!(report.source().unwrap().released_registrations(), 1);
    assert_eq!(report.source().unwrap().released_queued_failures(), 1);
    assert_eq!(closed.resources().resident_bytes(), 0);
    assert!(matches!(
        closed.run_one(&PanicOnCancellationProbe),
        StrictBaseOpenCoordinatorRun::AlreadyTerminal {
            phase: StrictBaseOpenCoordinatorPhase::Closed
        }
    ));

    let mut changed = coordinator(&fixture);
    let missing = match changed.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { missing, .. } => missing,
        other => panic!("source-change fixture must suspend: {other:?}"),
    };
    complete_missing_out_of_order(&mut changed, &fixture, missing);
    assert_eq!(
        changed.signal_source_changed(),
        StrictBaseOpenCoordinatorSourceChange::SourceChanged
    );
    assert_eq!(
        changed.phase(),
        StrictBaseOpenCoordinatorPhase::SourceChanged
    );
    assert_eq!(changed.resources().resident_bytes(), 0);
    assert!(matches!(
        changed.run_one(&PanicOnCancellationProbe),
        StrictBaseOpenCoordinatorRun::AlreadyTerminal {
            phase: StrictBaseOpenCoordinatorPhase::SourceChanged
        }
    ));
    let changed_report = changed.close();
    assert_eq!(changed_report.source().unwrap().released_ready_resumes(), 1);

    let mut failure_cancelled = coordinator(&fixture);
    let ticket = match failure_cancelled.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { ticket, .. } => ticket,
        other => panic!("failure-cancel fixture must suspend: {other:?}"),
    };
    failure_cancelled.fail_data(ticket);
    assert_eq!(
        failure_cancelled.cancel(),
        StrictBaseOpenCoordinatorCancel::Cancelled
    );
    assert_eq!(failure_cancelled.resources().resident_bytes(), 0);

    let mut failure_changed = coordinator(&fixture);
    let ticket = match failure_changed.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { ticket, .. } => ticket,
        other => panic!("failure-source-change fixture must suspend: {other:?}"),
    };
    failure_changed.fail_data(ticket);
    assert_eq!(
        failure_changed.signal_source_changed(),
        StrictBaseOpenCoordinatorSourceChange::SourceChanged
    );
    assert_eq!(failure_changed.resources().resident_bytes(), 0);

    let mut resume_closed = coordinator(&fixture);
    let missing = match resume_closed.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { missing, .. } => missing,
        other => panic!("resume-close fixture must suspend: {other:?}"),
    };
    complete_missing_out_of_order(&mut resume_closed, &fixture, missing);
    let report = resume_closed.close();
    assert_eq!(
        report.previous_phase(),
        StrictBaseOpenCoordinatorPhase::ResumeQueued
    );
    assert_eq!(report.source().unwrap().released_ready_resumes(), 1);
    assert_eq!(resume_closed.resources().resident_bytes(), 0);
}

#[test]
fn same_number_foreign_ticket_is_a_normal_rejection() {
    let fixture = fixture(0x97);
    let mut local = coordinator(&fixture);
    let mut foreign = coordinator(&fixture);
    let local_ticket = match local.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { ticket, .. } => ticket,
        other => panic!("local coordinator must suspend: {other:?}"),
    };
    let foreign_ticket = match foreign.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { ticket, .. } => ticket,
        other => panic!("foreign coordinator must suspend: {other:?}"),
    };
    assert_eq!(local_ticket.value(), foreign_ticket.value());
    assert_ne!(local_ticket, foreign_ticket);

    assert!(matches!(
        local.fail_data(foreign_ticket),
        StrictBaseOpenIngress::Rejected {
            phase: StrictBaseOpenCoordinatorPhase::WaitingForData,
            reason: StrictBaseOpenIngressRejectReason::Range(_),
        }
    ));
    assert_eq!(
        local.phase(),
        StrictBaseOpenCoordinatorPhase::WaitingForData
    );
    assert_eq!(local.resources().registrations(), 1);
    assert_eq!(local.resources().pending_tickets(), 1);
    assert_eq!(local.resources().queued_failures(), 0);
    local.close();
    foreign.close();
}

#[test]
fn mismatched_snapshot_ingress_commits_source_change_without_parser_progress() {
    let local = fixture(0x95);
    let foreign = fixture(0x96);
    assert_eq!(local.bytes().len(), foreign.bytes().len());
    let mut coordinator = coordinator(&local);
    let missing = match coordinator.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { missing, .. } => missing,
        other => panic!("source-change fixture must suspend: {other:?}"),
    };
    let stats = coordinator.stats();
    let job_phase = coordinator.job_phase();
    let range = missing.as_slice()[0];

    assert!(matches!(
        coordinator.supply(response_for_snapshot(&local, foreign.snapshot(), range)),
        StrictBaseOpenIngress::SourceChanged { error: Some(_) }
    ));
    assert_eq!(
        coordinator.phase(),
        StrictBaseOpenCoordinatorPhase::SourceChanged
    );
    assert_eq!(coordinator.stats(), stats);
    assert_eq!(coordinator.job_phase(), job_phase);
    assert_eq!(coordinator.resources().jobs(), 0);
    assert_eq!(coordinator.resources().resident_bytes(), 0);
    assert!(coordinator.source_change_error().is_some());
    assert!(matches!(
        coordinator.supply(response(&local, range)),
        StrictBaseOpenIngress::Rejected {
            phase: StrictBaseOpenCoordinatorPhase::SourceChanged,
            reason: StrictBaseOpenIngressRejectReason::TerminalPhase,
        }
    ));
}
