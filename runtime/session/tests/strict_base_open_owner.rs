#[allow(
    dead_code,
    reason = "shared integration support also serves the sibling Ready-owner test binary"
)]
mod support;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SourceError, SourceSnapshot,
};
use pdf_rs_document::{
    DocumentCancellation, DocumentErrorCode, DocumentLimits, NeverCancelled,
    OpenStrictBaseRevisionJob, RevisionAttestationJobContext, RevisionAttestationLimits,
    RevisionId, StrictBaseOpenContext, StrictBaseOpenLimits, StrictBaseOpenPhase,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_session::{
    RangeResumeArbiter, RangeResumeCompletion, RangeResumeDispatch, RangeResumeGeneration,
    RangeResumePermit, RangeResumeRegistrationOutcome, RangeResumeTarget, StrictBaseOpenJobOwner,
    StrictBaseOpenOwnerCancelOutcome, StrictBaseOpenOwnerFail, StrictBaseOpenOwnerPhase,
    StrictBaseOpenOwnerPoll, StrictBaseOpenOwnerResume, StrictBaseOpenOwnerSourceChangeOutcome,
    StrictBaseOpenOwnerStart, StrictBaseOpenResumeDiscardReason,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{XrefJobContext, XrefLimits};

use support::{Fixture, fixture, store_for};

const OPEN_JOB: JobId = JobId::new(4_001);
const TAIL_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(4_002);
const SECTION_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(4_003);
const SCAN_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(4_004);
const ENVELOPE_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(4_005);
const BOUNDARY_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(4_006);
const GENERATION: RangeResumeGeneration = RangeResumeGeneration::new(17);

fn strict_job(fixture: &Fixture) -> OpenStrictBaseRevisionJob {
    let context = StrictBaseOpenContext::new(
        XrefJobContext::new(OPEN_JOB, TAIL_CHECKPOINT, SECTION_CHECKPOINT),
        RevisionAttestationJobContext::new(
            OPEN_JOB,
            SCAN_CHECKPOINT,
            ENVELOPE_CHECKPOINT,
            BOUNDARY_CHECKPOINT,
            RequestPriority::Metadata,
        ),
    );
    OpenStrictBaseRevisionJob::new(
        fixture.snapshot(),
        RevisionId::new(51),
        context,
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

fn owner(fixture: &Fixture, arbiter: &RangeResumeArbiter) -> StrictBaseOpenJobOwner {
    StrictBaseOpenJobOwner::new(strict_job(fixture), GENERATION, arbiter.arbiter_id())
}

fn response(fixture: &Fixture, range: ByteRange) -> RangeResponse {
    let start = usize::try_from(range.start()).expect("fixture offset fits usize");
    let end = usize::try_from(range.end_exclusive()).expect("fixture offset fits usize");
    RangeResponse::new(
        fixture.snapshot(),
        range,
        fixture.bytes()[start..end].to_vec(),
    )
    .expect("fixture response exactly matches the supplied range")
}

fn permit_for_waiting(
    arbiter: &mut RangeResumeArbiter,
    fixture: &Fixture,
    ticket: pdf_rs_bytes::DataTicket,
    missing: pdf_rs_bytes::SmallRanges,
    target: RangeResumeTarget,
) -> RangeResumePermit {
    assert_eq!(
        arbiter.register_pending(ticket, target).unwrap(),
        RangeResumeRegistrationOutcome::Registered
    );
    for range in missing.as_slice().iter().copied() {
        arbiter.supply(response(fixture, range)).unwrap();
    }
    match arbiter.take_requeue().unwrap() {
        RangeResumeDispatch::Requeue(permit) => permit,
        RangeResumeDispatch::Empty => panic!("complete missing ranges must produce one permit"),
    }
}

fn permit_for_request(
    arbiter: &mut RangeResumeArbiter,
    fixture: &Fixture,
    range: ByteRange,
    job: JobId,
    checkpoint: ResumeCheckpoint,
    generation: RangeResumeGeneration,
) -> RangeResumePermit {
    let (ticket, missing) = match arbiter.byte_source().unwrap().poll(ReadRequest::new(
        range,
        RequestPriority::Metadata,
        job,
        checkpoint,
    )) {
        ReadPoll::Pending {
            ticket, missing, ..
        } => (ticket, missing),
        other => panic!("an incomplete independent arbiter must suspend: {other:?}"),
    };
    let target = RangeResumeTarget::new(job, checkpoint, generation);
    assert_eq!(
        arbiter.register_pending(ticket, target).unwrap(),
        RangeResumeRegistrationOutcome::Registered
    );
    for range in missing.as_slice().iter().copied() {
        arbiter.supply(response(fixture, range)).unwrap();
    }
    match arbiter.take_requeue().unwrap() {
        RangeResumeDispatch::Requeue(permit) => permit,
        RangeResumeDispatch::Empty => panic!("complete independent data must produce a permit"),
    }
}

fn start(owner: &mut StrictBaseOpenJobOwner, source: &dyn ByteSource) -> StrictBaseOpenOwnerPoll {
    match owner.start(source, &NeverCancelled) {
        StrictBaseOpenOwnerStart::Polled(outcome) => outcome,
        StrictBaseOpenOwnerStart::Rejected { phase } => {
            panic!("a fresh strict-open owner must start, not {phase:?}")
        }
    }
}

fn resume(
    owner: &mut StrictBaseOpenJobOwner,
    permit: RangeResumePermit,
    source: &dyn ByteSource,
) -> StrictBaseOpenOwnerPoll {
    match owner.resume(permit, source, &NeverCancelled) {
        StrictBaseOpenOwnerResume::Polled(outcome) => outcome,
        StrictBaseOpenOwnerResume::Discarded { reason, .. } => {
            panic!("an exact current permit must execute: {reason:?}")
        }
    }
}

struct PanicOnPollSource(SourceSnapshot);

impl ByteSource for PanicOnPollSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("a discarded permit must not poll the parser source")
    }
}

struct PanicOnCancellationProbe;

impl DocumentCancellation for PanicOnCancellationProbe {
    fn is_cancelled(&self) -> bool {
        panic!("a discarded permit must not enter the parser cancellation path")
    }
}

struct AlwaysCancelled;

impl DocumentCancellation for AlwaysCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[test]
fn exact_arbiter_permits_drive_the_owned_self_authored_open_to_ready() {
    let fixture = fixture(0x81);
    let mut arbiter = RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    let mut owner = owner(&fixture, &arbiter);
    let mut checkpoints = Vec::new();
    let mut outcome = start(&mut owner, arbiter.byte_source().unwrap());

    let index = loop {
        match outcome {
            StrictBaseOpenOwnerPoll::WaitingForData {
                ticket,
                missing,
                target,
            } => {
                checkpoints.push(target.checkpoint());
                let phase_before_supply = owner.phase();
                let job_phase_before_supply = owner.job_phase();
                let stats_before_supply = owner.stats();
                let permit = permit_for_waiting(&mut arbiter, &fixture, ticket, missing, target);
                assert_eq!(owner.phase(), phase_before_supply);
                assert_eq!(owner.job_phase(), job_phase_before_supply);
                assert_eq!(owner.stats(), stats_before_supply);
                outcome = resume(&mut owner, permit, arbiter.byte_source().unwrap());
            }
            StrictBaseOpenOwnerPoll::Ready(index) => break index,
            StrictBaseOpenOwnerPoll::Failed(error) => {
                panic!("self-authored strict open must become ready: {error}")
            }
            StrictBaseOpenOwnerPoll::Cancelled(error) => {
                panic!("never-cancelled strict open must not cancel: {error}")
            }
        }
    };

    assert_eq!(checkpoints.first(), Some(&TAIL_CHECKPOINT));
    assert!(!checkpoints.is_empty());
    assert_eq!(index.object_attestations().len(), 2);
    assert_eq!(owner.job_phase(), StrictBaseOpenPhase::Ready);
    assert_eq!(owner.stats().xref().entries(), 3);
    assert_eq!(owner.stats().attestation().objects_attested(), 2);
    assert_eq!(owner.resources().jobs(), 0);
    assert_eq!(owner.resources().waiting_targets(), 0);
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.resources().ready_requeues(), 0);
}

#[test]
fn invalid_permit_identities_and_repeated_start_never_poll_or_mutate_the_owner() {
    let fixture = fixture(0x82);
    let mut arbiter = RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    let one_byte = |start| ByteRange::new(start, 1).unwrap();
    let stale = permit_for_request(
        &mut arbiter,
        &fixture,
        one_byte(0),
        OPEN_JOB,
        TAIL_CHECKPOINT,
        RangeResumeGeneration::new(GENERATION.value() + 1),
    );
    let wrong_job = permit_for_request(
        &mut arbiter,
        &fixture,
        one_byte(1),
        JobId::new(OPEN_JOB.value() + 1),
        TAIL_CHECKPOINT,
        GENERATION,
    );
    let wrong_checkpoint = permit_for_request(
        &mut arbiter,
        &fixture,
        one_byte(2),
        OPEN_JOB,
        ResumeCheckpoint::new(TAIL_CHECKPOINT.value() + 1),
        GENERATION,
    );
    let wrong_ticket = permit_for_request(
        &mut arbiter,
        &fixture,
        one_byte(3),
        OPEN_JOB,
        TAIL_CHECKPOINT,
        GENERATION,
    );

    let mut foreign = RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    for offset in 0_u64..4 {
        let _discarded = permit_for_request(
            &mut foreign,
            &fixture,
            one_byte(offset),
            JobId::new(9_000 + offset),
            ResumeCheckpoint::new(9_100 + offset),
            GENERATION,
        );
    }
    let foreign_collision = permit_for_request(
        &mut foreign,
        &fixture,
        one_byte(4),
        OPEN_JOB,
        TAIL_CHECKPOINT,
        GENERATION,
    );

    let mut owner = owner(&fixture, &arbiter);
    let (ticket, target) = match start(&mut owner, arbiter.byte_source().unwrap()) {
        StrictBaseOpenOwnerPoll::WaitingForData { ticket, target, .. } => (ticket, target),
        other => panic!("an empty source must suspend strict open: {other:?}"),
    };
    assert_eq!(foreign_collision.ticket().value(), ticket.value());
    assert_ne!(foreign_collision.ticket(), ticket);
    assert_eq!(foreign_collision.target(), target);
    assert_ne!(foreign_collision.arbiter_id(), owner.arbiter_id());
    let phase = owner.phase();
    let job_phase = owner.job_phase();
    let stats = owner.stats();
    let resources = owner.resources();
    assert_eq!(resources.jobs(), 1);
    assert_eq!(resources.waiting_targets(), 1);
    let forbidden_source = PanicOnPollSource(fixture.snapshot());
    assert!(matches!(
        owner.start(&forbidden_source, &PanicOnCancellationProbe),
        StrictBaseOpenOwnerStart::Rejected {
            phase: StrictBaseOpenOwnerPhase::WaitingForData
        }
    ));

    let invalid = [
        (stale, StrictBaseOpenResumeDiscardReason::StaleGeneration),
        (wrong_job, StrictBaseOpenResumeDiscardReason::JobMismatch),
        (
            wrong_checkpoint,
            StrictBaseOpenResumeDiscardReason::CheckpointMismatch,
        ),
        (
            wrong_ticket,
            StrictBaseOpenResumeDiscardReason::TicketMismatch,
        ),
        (
            foreign_collision,
            StrictBaseOpenResumeDiscardReason::ArbiterMismatch,
        ),
    ];

    for (permit, expected_reason) in invalid {
        match owner.resume(permit, &forbidden_source, &PanicOnCancellationProbe) {
            StrictBaseOpenOwnerResume::Discarded { reason, .. } => {
                assert_eq!(reason, expected_reason)
            }
            StrictBaseOpenOwnerResume::Polled(_) => panic!("an invalid permit must not poll"),
        }
        assert_eq!(owner.phase(), phase);
        assert_eq!(owner.job_phase(), job_phase);
        assert_eq!(owner.stats(), stats);
        assert_eq!(owner.resources(), resources);
    }
}

#[test]
fn cancellation_token_maps_initial_and_resumed_polls_to_cancelled() {
    let fixture = fixture(0x85);
    let initial_arbiter = RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    let mut initial = owner(&fixture, &initial_arbiter);
    let initial_error =
        match initial.start(initial_arbiter.byte_source().unwrap(), &AlwaysCancelled) {
            StrictBaseOpenOwnerStart::Polled(StrictBaseOpenOwnerPoll::Cancelled(error)) => error,
            other => panic!("a cancelled initial poll must publish cancellation: {other:?}"),
        };
    assert!(initial_error.is_cancelled());
    assert_eq!(initial.phase(), StrictBaseOpenOwnerPhase::Cancelled);
    assert_eq!(initial.cancellation_error(), Some(initial_error));
    assert_eq!(initial.failure(), None);
    assert_eq!(initial.resources().jobs(), 0);
    assert_eq!(initial.resources().waiting_targets(), 0);

    let mut resume_arbiter =
        RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    let mut resumed = owner(&fixture, &resume_arbiter);
    let (ticket, missing, target) = match start(&mut resumed, resume_arbiter.byte_source().unwrap())
    {
        StrictBaseOpenOwnerPoll::WaitingForData {
            ticket,
            missing,
            target,
        } => (ticket, missing, target),
        other => panic!("an empty source must suspend before resumed cancellation: {other:?}"),
    };
    let permit = permit_for_waiting(&mut resume_arbiter, &fixture, ticket, missing, target);
    let resumed_error = match resumed.resume(
        permit,
        resume_arbiter.byte_source().unwrap(),
        &AlwaysCancelled,
    ) {
        StrictBaseOpenOwnerResume::Polled(StrictBaseOpenOwnerPoll::Cancelled(error)) => error,
        other => panic!("a cancelled resumed poll must publish cancellation: {other:?}"),
    };
    assert!(resumed_error.is_cancelled());
    assert_eq!(resumed.phase(), StrictBaseOpenOwnerPhase::Cancelled);
    assert_eq!(resumed.cancellation_error(), Some(resumed_error));
    assert_eq!(resumed.failure(), None);
    assert_eq!(resumed.resources().jobs(), 0);
    assert_eq!(resumed.resources().waiting_targets(), 0);
}

#[test]
fn parser_failure_is_retained_until_close_without_terminal_overwrite() {
    let fixture = fixture(0x86);
    let mut malformed = fixture.bytes().to_vec();
    malformed[7] = b'x';
    let range = ByteRange::new(
        0,
        u64::try_from(malformed.len()).expect("fixture length fits u64"),
    )
    .unwrap();
    let mut arbiter = RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    arbiter
        .supply(
            RangeResponse::new(fixture.snapshot(), range, malformed)
                .expect("malformed fixture still has valid response geometry"),
        )
        .unwrap();
    let mut failed = owner(&fixture, &arbiter);
    let error = match failed.start(arbiter.byte_source().unwrap(), &NeverCancelled) {
        StrictBaseOpenOwnerStart::Polled(StrictBaseOpenOwnerPoll::Failed(error)) => error,
        other => panic!("the malformed header must fail strict open: {other:?}"),
    };
    assert_eq!(
        error
            .document()
            .expect("header failure is a document error")
            .code(),
        DocumentErrorCode::InvalidDocumentHeader
    );
    assert!(!error.is_cancelled());
    assert_eq!(failed.phase(), StrictBaseOpenOwnerPhase::Failed);
    assert_eq!(failed.failure(), Some(error));
    assert_eq!(failed.cancellation_error(), None);
    assert_eq!(failed.resources().jobs(), 0);
    assert_eq!(
        failed.cancel(),
        StrictBaseOpenOwnerCancelOutcome::AlreadyTerminal {
            phase: StrictBaseOpenOwnerPhase::Failed
        }
    );
    assert_eq!(
        failed.signal_source_changed(),
        StrictBaseOpenOwnerSourceChangeOutcome::AlreadyTerminal {
            phase: StrictBaseOpenOwnerPhase::Failed
        }
    );
    assert_eq!(failed.failure(), Some(error));

    let report = failed.close();
    assert_eq!(report.previous_phase(), StrictBaseOpenOwnerPhase::Failed);
    assert_eq!(report.released_jobs(), 0);
    assert_eq!(report.released_waiting_targets(), 0);
    assert_eq!(failed.phase(), StrictBaseOpenOwnerPhase::Closed);
    assert_eq!(failed.close(), report);
}

#[test]
fn host_ticket_failure_terminates_the_exact_wait_without_repolling() {
    let fixture = fixture(0x87);
    let mut arbiter = RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    let mut failed = owner(&fixture, &arbiter);
    let (ticket, target) = match start(&mut failed, arbiter.byte_source().unwrap()) {
        StrictBaseOpenOwnerPoll::WaitingForData { ticket, target, .. } => (ticket, target),
        other => panic!("an empty source must suspend strict open: {other:?}"),
    };
    assert_eq!(
        arbiter.register_pending(ticket, target).unwrap(),
        RangeResumeRegistrationOutcome::Registered
    );
    let phase_before = failed.job_phase();
    let stats_before = failed.stats();
    let source_failure = SourceError::source_unavailable();

    let outcome = arbiter.fail_ticket(ticket).unwrap();
    assert_eq!(outcome.ticket(), ticket);
    assert_eq!(outcome.queued_failures(), 1);
    assert_eq!(failed.phase(), StrictBaseOpenOwnerPhase::WaitingForData);
    assert_eq!(failed.job_phase(), phase_before);
    assert_eq!(failed.stats(), stats_before);

    let permit = match arbiter.take_completion().unwrap() {
        RangeResumeCompletion::Failed(permit) => permit,
        other => panic!("host failure must produce one failure permit: {other:?}"),
    };
    assert_eq!(
        failed.fail_waiting(permit),
        StrictBaseOpenOwnerFail::Failed {
            ticket,
            target,
            error: source_failure,
        }
    );
    assert_eq!(
        arbiter.take_completion().unwrap(),
        RangeResumeCompletion::Empty
    );
    assert_eq!(failed.phase(), StrictBaseOpenOwnerPhase::Failed);
    assert_eq!(failed.job_phase(), phase_before);
    assert_eq!(failed.stats(), stats_before);
    assert_eq!(failed.failure(), None);
    assert_eq!(failed.source_failure(), Some(source_failure));
    assert_eq!(failed.cancellation_error(), None);
    assert_eq!(failed.resources().jobs(), 0);
    assert_eq!(failed.resources().waiting_targets(), 0);
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.resources().queued_failures(), 0);

    let report = failed.close();
    assert_eq!(report.previous_phase(), StrictBaseOpenOwnerPhase::Failed);
    assert_eq!(report.released_jobs(), 0);
    assert_eq!(report.released_waiting_targets(), 0);
    assert_eq!(failed.close(), report);
}

#[test]
fn stale_failure_permit_is_consumed_without_mutating_the_waiting_job() {
    let fixture = fixture(0x88);
    let mut arbiter = RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    let mut waiting = owner(&fixture, &arbiter);
    let (ticket, target) = match start(&mut waiting, arbiter.byte_source().unwrap()) {
        StrictBaseOpenOwnerPoll::WaitingForData { ticket, target, .. } => (ticket, target),
        other => panic!("an empty source must suspend strict open: {other:?}"),
    };
    let stale_target = RangeResumeTarget::new(
        target.job(),
        target.checkpoint(),
        RangeResumeGeneration::new(GENERATION.value() + 1),
    );
    assert_eq!(
        arbiter.register_pending(ticket, stale_target).unwrap(),
        RangeResumeRegistrationOutcome::Registered
    );
    let owner_phase = waiting.phase();
    let job_phase = waiting.job_phase();
    let stats = waiting.stats();
    arbiter.fail_ticket(ticket).unwrap();
    let permit = match arbiter.take_completion().unwrap() {
        RangeResumeCompletion::Failed(permit) => permit,
        other => panic!("host failure must produce one failure permit: {other:?}"),
    };
    match waiting.fail_waiting(permit) {
        StrictBaseOpenOwnerFail::Discarded { reason, .. } => {
            assert_eq!(reason, StrictBaseOpenResumeDiscardReason::StaleGeneration)
        }
        StrictBaseOpenOwnerFail::Failed { .. } => {
            panic!("a stale failure permit must not terminate current work")
        }
    }
    assert_eq!(waiting.phase(), owner_phase);
    assert_eq!(waiting.job_phase(), job_phase);
    assert_eq!(waiting.stats(), stats);
    assert_eq!(waiting.failure(), None);
    assert_eq!(waiting.source_failure(), None);
    assert_eq!(waiting.resources().jobs(), 1);
    assert_eq!(waiting.resources().waiting_targets(), 1);

    let report = waiting.close();
    assert_eq!(
        report.previous_phase(),
        StrictBaseOpenOwnerPhase::WaitingForData
    );
    assert_eq!(report.released_jobs(), 1);
    assert_eq!(report.released_waiting_targets(), 1);
}

#[test]
fn cancel_before_resume_wins_but_committed_ready_survives_late_cancel() {
    let fixture = fixture(0x83);
    let mut arbiter = RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    let mut cancelled = owner(&fixture, &arbiter);
    let (ticket, missing, target) = match start(&mut cancelled, arbiter.byte_source().unwrap()) {
        StrictBaseOpenOwnerPoll::WaitingForData {
            ticket,
            missing,
            target,
        } => (ticket, missing, target),
        other => panic!("an empty source must suspend strict open: {other:?}"),
    };
    let late = permit_for_waiting(&mut arbiter, &fixture, ticket, missing, target);
    assert_eq!(
        cancelled.cancel(),
        StrictBaseOpenOwnerCancelOutcome::Cancelled {
            target: Some(target)
        }
    );
    match cancelled.resume(
        late,
        &PanicOnPollSource(fixture.snapshot()),
        &PanicOnCancellationProbe,
    ) {
        StrictBaseOpenOwnerResume::Discarded { reason, .. } => assert_eq!(
            reason,
            StrictBaseOpenResumeDiscardReason::NotWaiting(StrictBaseOpenOwnerPhase::Cancelled)
        ),
        StrictBaseOpenOwnerResume::Polled(_) => panic!("cancelled work must not poll"),
    }
    assert_eq!(cancelled.phase(), StrictBaseOpenOwnerPhase::Cancelled);
    assert_eq!(cancelled.cancellation_error(), None);
    assert_eq!(cancelled.resources().jobs(), 0);
    assert_eq!(cancelled.resources().waiting_targets(), 0);

    let complete_source = store_for(&fixture);
    let mut completed = owner(&fixture, &arbiter);
    assert!(matches!(
        start(&mut completed, &complete_source),
        StrictBaseOpenOwnerPoll::Ready(_)
    ));
    let committed_phase = completed.phase();
    let committed_stats = completed.stats();
    assert_eq!(
        completed.cancel(),
        StrictBaseOpenOwnerCancelOutcome::AlreadyTerminal {
            phase: StrictBaseOpenOwnerPhase::Ready
        }
    );
    assert_eq!(completed.phase(), committed_phase);
    assert_eq!(completed.stats(), committed_stats);
    assert_eq!(completed.resources().jobs(), 0);
    assert_eq!(completed.resources().waiting_targets(), 0);
}

#[test]
fn source_change_and_close_drop_the_job_before_discarding_late_permits() {
    let fixture = fixture(0x84);
    let forbidden_source = PanicOnPollSource(fixture.snapshot());

    let mut changed_arbiter =
        RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    let mut changed = owner(&fixture, &changed_arbiter);
    let (ticket, missing, target) =
        match start(&mut changed, changed_arbiter.byte_source().unwrap()) {
            StrictBaseOpenOwnerPoll::WaitingForData {
                ticket,
                missing,
                target,
            } => (ticket, missing, target),
            other => panic!("an empty source must suspend strict open: {other:?}"),
        };
    let late_changed = permit_for_waiting(&mut changed_arbiter, &fixture, ticket, missing, target);
    assert_eq!(
        changed.signal_source_changed(),
        StrictBaseOpenOwnerSourceChangeOutcome::SourceChanged {
            target: Some(target)
        }
    );
    match changed.resume(late_changed, &forbidden_source, &PanicOnCancellationProbe) {
        StrictBaseOpenOwnerResume::Discarded { reason, .. } => assert_eq!(
            reason,
            StrictBaseOpenResumeDiscardReason::NotWaiting(StrictBaseOpenOwnerPhase::SourceChanged)
        ),
        StrictBaseOpenOwnerResume::Polled(_) => panic!("source-changed work must not poll"),
    }
    assert_eq!(changed.phase(), StrictBaseOpenOwnerPhase::SourceChanged);
    assert_eq!(changed.resources().jobs(), 0);
    assert_eq!(changed.resources().waiting_targets(), 0);

    let mut closed_arbiter =
        RangeResumeArbiter::new(fixture.snapshot(), Default::default()).unwrap();
    let mut closed = owner(&fixture, &closed_arbiter);
    let (ticket, missing, target) = match start(&mut closed, closed_arbiter.byte_source().unwrap())
    {
        StrictBaseOpenOwnerPoll::WaitingForData {
            ticket,
            missing,
            target,
        } => (ticket, missing, target),
        other => panic!("an empty source must suspend strict open: {other:?}"),
    };
    let late_closed = permit_for_waiting(&mut closed_arbiter, &fixture, ticket, missing, target);
    let report = closed.close();
    assert_eq!(
        report.previous_phase(),
        StrictBaseOpenOwnerPhase::WaitingForData
    );
    assert_eq!(report.released_jobs(), 1);
    assert_eq!(report.released_waiting_targets(), 1);
    assert_eq!(closed.close(), report);
    match closed.resume(late_closed, &forbidden_source, &PanicOnCancellationProbe) {
        StrictBaseOpenOwnerResume::Discarded { reason, .. } => assert_eq!(
            reason,
            StrictBaseOpenResumeDiscardReason::NotWaiting(StrictBaseOpenOwnerPhase::Closed)
        ),
        StrictBaseOpenOwnerResume::Polled(_) => panic!("closed work must not poll"),
    }
    assert_eq!(closed.phase(), StrictBaseOpenOwnerPhase::Closed);
    assert_eq!(closed.resources().jobs(), 0);
    assert_eq!(closed.resources().waiting_targets(), 0);
}
