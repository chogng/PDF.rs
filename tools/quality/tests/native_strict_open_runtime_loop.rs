use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, JobId, RangeResponse, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SmallRanges, SourceError, SourceIdentity, SourceRevision,
    SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    DocumentCancellation, DocumentLimits, NeverCancelled, OpenStrictBaseRevisionJob,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionId, StrictBaseOpenContext,
    StrictBaseOpenLimits, StrictBaseOpenPhase,
};
use pdf_rs_generate::generate_one_page_pdf;
use pdf_rs_object::{ObjectLimitConfig, ObjectLimits};
use pdf_rs_session::{
    RangeResumeArbiter, RangeResumeDispatch, RangeResumeGeneration, RangeResumePermit,
    RangeResumePhase, RangeResumeRegistrationOutcome, RangeResumeTarget, StrictBaseOpenCoordinator,
    StrictBaseOpenCoordinatorFailure, StrictBaseOpenCoordinatorPhase, StrictBaseOpenCoordinatorRun,
    StrictBaseOpenIngress, StrictBaseOpenJobOwner, StrictBaseOpenOwnerCancelOutcome,
    StrictBaseOpenOwnerPhase, StrictBaseOpenOwnerPoll, StrictBaseOpenOwnerResume,
    StrictBaseOpenOwnerSourceChangeOutcome, StrictBaseOpenOwnerStart,
    StrictBaseOpenResumeDiscardReason,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{XrefJobContext, XrefLimitConfig, XrefLimits};

const OPEN_JOB: JobId = JobId::new(801);
const TAIL_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(802);
const SECTION_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(803);
const SCAN_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(804);
const ENVELOPE_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(805);
const BOUNDARY_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(806);
const OPEN_GENERATION: RangeResumeGeneration = RangeResumeGeneration::new(31);
const STALE_GENERATION: RangeResumeGeneration = RangeResumeGeneration::new(30);

fn snapshot(source_len: u64, seed: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([seed; 32]),
            SourceRevision::new(u64::from(seed)),
        ),
        Some(source_len),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [seed.wrapping_add(1); 32],
        ),
    )
}

fn context() -> StrictBaseOpenContext {
    StrictBaseOpenContext::new(
        XrefJobContext::new(OPEN_JOB, TAIL_CHECKPOINT, SECTION_CHECKPOINT),
        RevisionAttestationJobContext::new(
            OPEN_JOB,
            SCAN_CHECKPOINT,
            ENVELOPE_CHECKPOINT,
            BOUNDARY_CHECKPOINT,
            RequestPriority::VisiblePage,
        ),
    )
}

fn compact_limits(source_len: u64) -> StrictBaseOpenLimits {
    let xref = XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: source_len,
        initial_tail_bytes: 32,
        max_tail_bytes: 64,
        initial_section_bytes: 64,
        max_section_bytes: 192,
        max_total_read_bytes: 512,
        max_total_parse_bytes: 512,
        max_subsections: 4,
        max_entries: 8,
    })
    .expect("the generated PDF fits the compact xref profile");
    let object = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: 40,
        max_envelope_bytes: 128,
        initial_boundary_bytes: 8,
        max_boundary_bytes: 64,
        max_stream_bytes: source_len,
        max_total_read_bytes: 256,
        max_total_parse_bytes: 256,
    })
    .expect("the generated PDF fits the compact object profile");
    StrictBaseOpenLimits::new(
        xref,
        DocumentLimits::default(),
        RevisionAttestationLimits::default(),
        object,
        SyntaxLimits::default(),
    )
}

fn new_job(source: SourceSnapshot) -> OpenStrictBaseRevisionJob {
    OpenStrictBaseRevisionJob::new(
        source,
        RevisionId::new(1),
        context(),
        compact_limits(source.len().expect("the generated source length is known")),
    )
    .expect("the strict base-open profile is valid")
}

fn new_owner(source: SourceSnapshot, arbiter: &RangeResumeArbiter) -> StrictBaseOpenJobOwner {
    StrictBaseOpenJobOwner::new(new_job(source), OPEN_GENERATION, arbiter.arbiter_id())
}

fn response(source: SourceSnapshot, pdf: &[u8], range: ByteRange) -> RangeResponse {
    let start = usize::try_from(range.start()).expect("generated PDF offsets fit usize");
    let end = usize::try_from(range.end_exclusive()).expect("generated PDF offsets fit usize");
    RangeResponse::new(source, range, pdf[start..end].to_vec())
        .expect("the supplied bytes exactly match the response range")
}

fn start_owner(
    owner: &mut StrictBaseOpenJobOwner,
    source: &dyn ByteSource,
) -> StrictBaseOpenOwnerPoll {
    match owner.start(source, &NeverCancelled) {
        StrictBaseOpenOwnerStart::Polled(outcome) => outcome,
        StrictBaseOpenOwnerStart::Rejected { phase } => {
            panic!("a fresh strict-open owner must start, not {phase:?}")
        }
    }
}

fn resume_owner(
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

fn register(arbiter: &mut RangeResumeArbiter, ticket: DataTicket, target: RangeResumeTarget) {
    assert_eq!(
        arbiter.register_pending(ticket, target).unwrap(),
        RangeResumeRegistrationOutcome::Registered
    );
}

fn take_exact_permit(
    arbiter: &mut RangeResumeArbiter,
    ticket: DataTicket,
    target: RangeResumeTarget,
) -> RangeResumePermit {
    let permit = match arbiter.take_requeue().unwrap() {
        RangeResumeDispatch::Requeue(permit) => permit,
        RangeResumeDispatch::Empty => panic!("a completed ticket must yield one permit"),
    };
    assert_eq!(permit.arbiter_id(), arbiter.arbiter_id());
    assert_eq!(permit.ticket(), ticket);
    assert_eq!(permit.target(), target);
    assert_eq!(
        arbiter.take_requeue().unwrap(),
        RangeResumeDispatch::Empty,
        "one completed ticket must dispatch its exact permit once"
    );
    permit
}

fn complete_out_of_order(
    arbiter: &mut RangeResumeArbiter,
    source: SourceSnapshot,
    pdf: &[u8],
    ticket: DataTicket,
    missing: SmallRanges,
    target: RangeResumeTarget,
) -> RangeResumePermit {
    register(arbiter, ticket, target);
    assert_eq!(arbiter.resources().registrations(), 1);
    assert_eq!(arbiter.resources().pending_tickets(), 1);

    let mut lower_halves = Vec::with_capacity(missing.len());
    for range in missing.as_slice().iter().copied() {
        assert!(
            range.len() > 1,
            "real strict-open requests must remain splittable in this fixture"
        );
        let lower_len = range.len() / 2;
        let lower = ByteRange::new(range.start(), lower_len).unwrap();
        let upper = ByteRange::new(range.start() + lower_len, range.len() - lower_len).unwrap();
        let supplied = arbiter.supply(response(source, pdf, upper)).unwrap();
        assert_eq!(supplied.ready_tickets(), 0);
        assert_eq!(supplied.queued_requeues(), 0);
        assert_eq!(arbiter.take_requeue().unwrap(), RangeResumeDispatch::Empty);
        lower_halves.push(lower);
    }

    let last = lower_halves.len() - 1;
    for (index, lower) in lower_halves.into_iter().enumerate() {
        let supplied = arbiter.supply(response(source, pdf, lower)).unwrap();
        if index == last {
            assert_eq!(supplied.ready_tickets(), 1);
            assert_eq!(supplied.queued_requeues(), 1);
        } else {
            assert_eq!(supplied.ready_tickets(), 0);
            assert_eq!(supplied.queued_requeues(), 0);
            assert_eq!(arbiter.take_requeue().unwrap(), RangeResumeDispatch::Empty);
        }
    }
    take_exact_permit(arbiter, ticket, target)
}

fn complete_in_order(
    arbiter: &mut RangeResumeArbiter,
    source: SourceSnapshot,
    pdf: &[u8],
    ticket: DataTicket,
    missing: SmallRanges,
    target: RangeResumeTarget,
) -> RangeResumePermit {
    register(arbiter, ticket, target);
    let last = missing.len() - 1;
    for (index, range) in missing.as_slice().iter().copied().enumerate() {
        let supplied = arbiter.supply(response(source, pdf, range)).unwrap();
        if index == last {
            assert_eq!(supplied.ready_tickets(), 1);
            assert_eq!(supplied.queued_requeues(), 1);
        } else {
            assert_eq!(supplied.ready_tickets(), 0);
            assert_eq!(supplied.queued_requeues(), 0);
        }
    }
    take_exact_permit(arbiter, ticket, target)
}

fn early_stale_permit(
    arbiter: &mut RangeResumeArbiter,
    source: SourceSnapshot,
    pdf: &[u8],
) -> RangeResumePermit {
    let early_range = ByteRange::new(0, 1).unwrap();
    let (ticket, missing) = match arbiter.byte_source().unwrap().poll(ReadRequest::new(
        early_range,
        RequestPriority::Metadata,
        OPEN_JOB,
        TAIL_CHECKPOINT,
    )) {
        ReadPoll::Pending {
            ticket, missing, ..
        } => (ticket, missing),
        other => panic!("an empty arbiter must suspend the early stale request: {other:?}"),
    };
    let target = RangeResumeTarget::new(OPEN_JOB, TAIL_CHECKPOINT, STALE_GENERATION);
    register(arbiter, ticket, target);
    for range in missing.as_slice().iter().copied() {
        let supplied = arbiter.supply(response(source, pdf, range)).unwrap();
        assert_eq!(supplied.ready_tickets(), 1);
        assert_eq!(supplied.queued_requeues(), 1);
    }
    take_exact_permit(arbiter, ticket, target)
}

fn waiting_with_late_permit(
    owner: &mut StrictBaseOpenJobOwner,
    arbiter: &mut RangeResumeArbiter,
    source: SourceSnapshot,
    pdf: &[u8],
) -> (RangeResumeTarget, RangeResumePermit) {
    let (ticket, missing, target) = match start_owner(owner, arbiter.byte_source().unwrap()) {
        StrictBaseOpenOwnerPoll::WaitingForData {
            ticket,
            missing,
            target,
        } => (ticket, missing, target),
        other => panic!("an empty runtime source must suspend strict open: {other:?}"),
    };
    let permit = complete_in_order(arbiter, source, pdf, ticket, missing, target);
    assert_eq!(owner.phase(), StrictBaseOpenOwnerPhase::WaitingForData);
    assert_eq!(owner.waiting_target(), Some(target));
    (target, permit)
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

fn assert_late_permit_discarded(
    owner: &mut StrictBaseOpenJobOwner,
    permit: RangeResumePermit,
    source: SourceSnapshot,
    phase: StrictBaseOpenOwnerPhase,
) {
    let job_phase = owner.job_phase();
    let stats = owner.stats();
    let resources = owner.resources();
    match owner.resume(
        permit,
        &PanicOnPollSource(source),
        &PanicOnCancellationProbe,
    ) {
        StrictBaseOpenOwnerResume::Discarded { reason, .. } => {
            assert_eq!(reason, StrictBaseOpenResumeDiscardReason::NotWaiting(phase))
        }
        StrictBaseOpenOwnerResume::Polled(_) => panic!("terminal work must not poll"),
    }
    assert_eq!(owner.phase(), phase);
    assert_eq!(owner.job_phase(), job_phase);
    assert_eq!(owner.stats(), stats);
    assert_eq!(owner.resources(), resources);
    assert_eq!(resources.jobs(), 0);
    assert_eq!(resources.waiting_targets(), 0);
}

#[test]
fn native_strict_open_runtime_loop_is_generation_gated_and_one_shot() {
    let pdf = generate_one_page_pdf().expect("canonical one-page PDF generation succeeds");
    let source_len = u64::try_from(pdf.len()).expect("generated PDF length fits u64");
    assert_eq!(source_len, 612);

    let source = snapshot(source_len, 0x91);
    let mut arbiter = RangeResumeArbiter::new(source, Default::default()).unwrap();
    let mut stale = Some(early_stale_permit(&mut arbiter, source, &pdf));
    let mut owner = new_owner(source, &arbiter);
    let mut observed_checkpoints = Vec::new();
    let mut pending_turns = 0_u64;
    let mut stale_injected = false;
    let mut outcome = start_owner(&mut owner, arbiter.byte_source().unwrap());

    let attested = loop {
        match outcome {
            StrictBaseOpenOwnerPoll::WaitingForData {
                ticket,
                missing,
                target,
            } => {
                pending_turns += 1;
                assert!(pending_turns < 64, "strict open must make bounded progress");
                observed_checkpoints.push(target.checkpoint());
                assert_eq!(target.job(), OPEN_JOB);
                assert_eq!(target.generation(), OPEN_GENERATION);

                if let Some(stale) = stale.take() {
                    let phase = owner.phase();
                    let job_phase = owner.job_phase();
                    let stats = owner.stats();
                    let resources = owner.resources();
                    let waiting_target = owner.waiting_target();
                    match owner.resume(stale, &PanicOnPollSource(source), &PanicOnCancellationProbe)
                    {
                        StrictBaseOpenOwnerResume::Discarded {
                            target: stale_target,
                            reason,
                            ..
                        } => {
                            assert_eq!(stale_target.generation(), STALE_GENERATION);
                            assert_eq!(reason, StrictBaseOpenResumeDiscardReason::StaleGeneration);
                        }
                        StrictBaseOpenOwnerResume::Polled(_) => {
                            panic!("a stale generation must not poll")
                        }
                    }
                    assert_eq!(owner.phase(), phase);
                    assert_eq!(owner.job_phase(), job_phase);
                    assert_eq!(owner.stats(), stats);
                    assert_eq!(owner.resources(), resources);
                    assert_eq!(owner.waiting_target(), waiting_target);
                    stale_injected = true;
                }

                let phase = owner.phase();
                let job_phase = owner.job_phase();
                let stats = owner.stats();
                let resources = owner.resources();
                let waiting_target = owner.waiting_target();
                let permit =
                    complete_out_of_order(&mut arbiter, source, &pdf, ticket, missing, target);
                assert_eq!(owner.phase(), phase);
                assert_eq!(owner.job_phase(), job_phase);
                assert_eq!(owner.stats(), stats);
                assert_eq!(owner.resources(), resources);
                assert_eq!(owner.waiting_target(), waiting_target);
                outcome = resume_owner(&mut owner, permit, arbiter.byte_source().unwrap());
            }
            StrictBaseOpenOwnerPoll::Ready(index) => break index,
            StrictBaseOpenOwnerPoll::Failed(error) => {
                panic!("generation-gated Range supply must complete strict open: {error}")
            }
            StrictBaseOpenOwnerPoll::Cancelled(error) => {
                panic!("never-cancelled strict open must not cancel: {error}")
            }
        }
    };

    assert!(stale_injected);
    for checkpoint in [
        TAIL_CHECKPOINT,
        SECTION_CHECKPOINT,
        SCAN_CHECKPOINT,
        ENVELOPE_CHECKPOINT,
        BOUNDARY_CHECKPOINT,
    ] {
        assert!(
            observed_checkpoints.contains(&checkpoint),
            "all five real child checkpoints must suspend at least once: {observed_checkpoints:?}"
        );
    }
    assert_eq!(owner.phase(), StrictBaseOpenOwnerPhase::Ready);
    assert_eq!(owner.job_phase(), StrictBaseOpenPhase::Ready);
    assert_eq!(owner.stats().xref().entries(), 5);
    assert_eq!(owner.stats().attestation().objects_attested(), 4);
    assert_eq!(attested.object_attestations().len(), 4);
    assert_eq!(owner.resources().jobs(), 0);
    assert_eq!(owner.resources().waiting_targets(), 0);
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.resources().ready_requeues(), 0);
    let release = arbiter.close();
    assert_eq!(release.released_cached_bytes(), source_len);
    assert_eq!(arbiter.resources().resident_bytes(), 0);

    println!(
        "native_strict_open_runtime_loop_result bytes={source_len} pending_turns={pending_turns} checkpoints=tail,section,scan,envelope,boundary upper_before_lower=true exact_permit_once=true move_only_permit=true generation_validated=true stale_dispatch_dropped=true stale_dispatch_no_poll=true ready_resources_zero=true scheduler_scope=single_strict_open_job"
    );
}

#[test]
fn native_strict_open_runtime_terminals_drop_late_permits_and_resources() {
    let pdf = generate_one_page_pdf().expect("canonical one-page PDF generation succeeds");
    let source_len = u64::try_from(pdf.len()).expect("generated PDF length fits u64");
    assert_eq!(source_len, 612);

    let cancel_source = snapshot(source_len, 0x92);
    let mut cancel_arbiter = RangeResumeArbiter::new(cancel_source, Default::default()).unwrap();
    let mut cancelled = new_owner(cancel_source, &cancel_arbiter);
    let (cancel_target, late_cancel) =
        waiting_with_late_permit(&mut cancelled, &mut cancel_arbiter, cancel_source, &pdf);
    assert_eq!(
        cancelled.cancel(),
        StrictBaseOpenOwnerCancelOutcome::Cancelled {
            target: Some(cancel_target)
        }
    );
    cancel_arbiter.close();
    assert_eq!(cancel_arbiter.resources().resident_bytes(), 0);
    assert_late_permit_discarded(
        &mut cancelled,
        late_cancel,
        cancel_source,
        StrictBaseOpenOwnerPhase::Cancelled,
    );

    let changed_source = snapshot(source_len, 0x93);
    let mut changed_arbiter = RangeResumeArbiter::new(changed_source, Default::default()).unwrap();
    let mut changed = new_owner(changed_source, &changed_arbiter);
    let (changed_target, late_changed) =
        waiting_with_late_permit(&mut changed, &mut changed_arbiter, changed_source, &pdf);
    assert_eq!(
        changed.signal_source_changed(),
        StrictBaseOpenOwnerSourceChangeOutcome::SourceChanged {
            target: Some(changed_target)
        }
    );
    let changed_release = changed_arbiter.signal_source_changed().unwrap();
    assert_eq!(changed_arbiter.phase(), RangeResumePhase::SourceChanged);
    assert_eq!(changed_arbiter.resources().resident_bytes(), 0);
    assert_late_permit_discarded(
        &mut changed,
        late_changed,
        changed_source,
        StrictBaseOpenOwnerPhase::SourceChanged,
    );
    assert_eq!(changed_arbiter.close(), changed_release);

    let close_source = snapshot(source_len, 0x94);
    let mut close_arbiter = RangeResumeArbiter::new(close_source, Default::default()).unwrap();
    let mut closed = new_owner(close_source, &close_arbiter);
    let (_close_target, late_closed) =
        waiting_with_late_permit(&mut closed, &mut close_arbiter, close_source, &pdf);
    let owner_close = closed.close();
    assert_eq!(
        owner_close.previous_phase(),
        StrictBaseOpenOwnerPhase::WaitingForData
    );
    assert_eq!(owner_close.released_jobs(), 1);
    assert_eq!(owner_close.released_waiting_targets(), 1);
    assert_eq!(closed.close(), owner_close);
    let arbiter_close = close_arbiter.close();
    assert_eq!(close_arbiter.phase(), RangeResumePhase::Closed);
    assert_eq!(close_arbiter.resources().resident_bytes(), 0);
    assert_eq!(close_arbiter.close(), arbiter_close);
    assert_late_permit_discarded(
        &mut closed,
        late_closed,
        close_source,
        StrictBaseOpenOwnerPhase::Closed,
    );
    assert_eq!(closed.close(), owner_close);

    println!(
        "native_strict_open_runtime_terminal_result cancel_late_permit_dropped=true source_change_late_permit_dropped=true close_late_permit_dropped=true terminal_resources_zero=true scheduler_scope=single_strict_open_job"
    );
}

#[test]
fn native_strict_open_coordinator_closes_range_turns_and_failure_without_poll() {
    let pdf = generate_one_page_pdf().expect("canonical one-page PDF generation succeeds");
    let source_len = u64::try_from(pdf.len()).expect("generated PDF length fits u64");
    assert_eq!(source_len, 612);

    let source = snapshot(source_len, 0x95);
    let mut coordinator =
        StrictBaseOpenCoordinator::new(new_job(source), OPEN_GENERATION, Default::default())
            .expect("default Range limits validate for the coordinator");
    let mut observed_checkpoints = Vec::new();
    let mut pending_turns = 0_u64;
    let mut no_work_without_poll = false;
    let mut outcome = coordinator.run_one(&NeverCancelled);

    let ready = loop {
        match outcome {
            StrictBaseOpenCoordinatorRun::WaitingForData { missing, .. } => {
                pending_turns += 1;
                assert!(pending_turns < 64, "strict open must make bounded progress");
                let checkpoint = coordinator
                    .waiting_checkpoint()
                    .expect("published Waiting retains its registered checkpoint");
                if !observed_checkpoints.contains(&checkpoint) {
                    observed_checkpoints.push(checkpoint);
                }
                assert_eq!(
                    coordinator.phase(),
                    StrictBaseOpenCoordinatorPhase::WaitingForData
                );
                assert_eq!(coordinator.resources().jobs(), 1);
                assert_eq!(coordinator.resources().waiting_targets(), 1);
                assert_eq!(coordinator.resources().registrations(), 1);
                assert_eq!(coordinator.resources().pending_tickets(), 1);
                assert_eq!(coordinator.resources().ready_resumes(), 0);
                assert_eq!(coordinator.resources().queued_failures(), 0);

                let stats = coordinator.stats();
                let job_phase = coordinator.job_phase();
                let mut lower_halves = Vec::with_capacity(missing.len());
                for range in missing.as_slice().iter().copied() {
                    assert!(
                        range.len() > 1,
                        "real strict-open requests must remain splittable"
                    );
                    let lower_len = range.len() / 2;
                    let lower = ByteRange::new(range.start(), lower_len).unwrap();
                    let upper =
                        ByteRange::new(range.start() + lower_len, range.len() - lower_len).unwrap();
                    assert!(matches!(
                        coordinator.supply(response(source, &pdf, upper)),
                        StrictBaseOpenIngress::Accepted {
                            wake_scheduler: false,
                            ..
                        }
                    ));
                    assert_eq!(
                        coordinator.phase(),
                        StrictBaseOpenCoordinatorPhase::WaitingForData
                    );
                    assert_eq!(coordinator.stats(), stats);
                    assert_eq!(coordinator.job_phase(), job_phase);
                    lower_halves.push(lower);
                }

                if !no_work_without_poll {
                    assert!(matches!(
                        coordinator.run_one(&PanicOnCancellationProbe),
                        StrictBaseOpenCoordinatorRun::NoWork
                    ));
                    assert_eq!(coordinator.stats(), stats);
                    assert_eq!(coordinator.job_phase(), job_phase);
                    assert_eq!(
                        coordinator.phase(),
                        StrictBaseOpenCoordinatorPhase::WaitingForData
                    );
                    no_work_without_poll = true;
                }

                let last = lower_halves.len() - 1;
                for (index, lower) in lower_halves.into_iter().enumerate() {
                    match coordinator.supply(response(source, &pdf, lower)) {
                        StrictBaseOpenIngress::Accepted { wake_scheduler, .. } => {
                            assert_eq!(wake_scheduler, index == last)
                        }
                        other => panic!("valid generated bytes must be accepted: {other:?}"),
                    }
                    let expected_phase = if index == last {
                        StrictBaseOpenCoordinatorPhase::ResumeQueued
                    } else {
                        StrictBaseOpenCoordinatorPhase::WaitingForData
                    };
                    assert_eq!(coordinator.phase(), expected_phase);
                    assert_eq!(coordinator.stats(), stats);
                    assert_eq!(coordinator.job_phase(), job_phase);
                }
                assert_eq!(coordinator.resources().pending_tickets(), 0);
                assert_eq!(coordinator.resources().ready_resumes(), 1);
                assert_eq!(coordinator.resources().queued_failures(), 0);
                outcome = coordinator.run_one(&NeverCancelled);
            }
            StrictBaseOpenCoordinatorRun::Ready(ready) => break ready,
            other => panic!("coordinated generated strict open must reach Ready: {other:?}"),
        }
    };

    assert!(no_work_without_poll);
    assert_eq!(
        observed_checkpoints,
        vec![
            TAIL_CHECKPOINT,
            SECTION_CHECKPOINT,
            SCAN_CHECKPOINT,
            ENVELOPE_CHECKPOINT,
            BOUNDARY_CHECKPOINT,
        ]
    );
    assert_eq!(ready.index().snapshot(), source);
    assert_eq!(ready.index().object_attestations().len(), 4);
    assert_eq!(
        coordinator.phase(),
        StrictBaseOpenCoordinatorPhase::ReadyHandedOff
    );
    assert_eq!(coordinator.resources().jobs(), 0);
    assert_eq!(coordinator.resources().waiting_targets(), 0);
    assert_eq!(coordinator.resources().registrations(), 0);
    assert_eq!(coordinator.resources().pending_tickets(), 0);
    assert_eq!(coordinator.resources().ready_resumes(), 0);
    assert_eq!(coordinator.resources().queued_failures(), 0);
    assert_eq!(coordinator.resources().cached_bytes(), 0);
    assert_eq!(coordinator.resources().resident_bytes(), 0);
    assert!(matches!(
        coordinator.run_one(&PanicOnCancellationProbe),
        StrictBaseOpenCoordinatorRun::AlreadyTerminal {
            phase: StrictBaseOpenCoordinatorPhase::ReadyHandedOff
        }
    ));

    let handoff_resources = ready.source_resources();
    assert_eq!(handoff_resources.registrations(), 0);
    assert_eq!(handoff_resources.pending_tickets(), 0);
    assert_eq!(handoff_resources.ready_resumes(), 0);
    assert_eq!(handoff_resources.queued_failures(), 0);
    assert_eq!(handoff_resources.cached_bytes(), source_len);
    let source_release = ready.close();
    assert_eq!(source_release.released_registrations(), 0);
    assert_eq!(source_release.released_pending_tickets(), 0);
    assert_eq!(source_release.released_ready_resumes(), 0);
    assert_eq!(source_release.released_queued_failures(), 0);
    assert_eq!(source_release.released_cached_bytes(), source_len);

    let failure_source = snapshot(source_len, 0x96);
    let mut failed = StrictBaseOpenCoordinator::new(
        new_job(failure_source),
        OPEN_GENERATION,
        Default::default(),
    )
    .expect("default Range limits validate for the failure coordinator");
    let ticket = match failed.run_one(&NeverCancelled) {
        StrictBaseOpenCoordinatorRun::WaitingForData { ticket, .. } => ticket,
        other => panic!("empty failure source must suspend first: {other:?}"),
    };
    assert_eq!(
        failed.phase(),
        StrictBaseOpenCoordinatorPhase::WaitingForData
    );
    assert_eq!(failed.resources().registrations(), 1);
    assert_eq!(failed.resources().pending_tickets(), 1);
    let stats = failed.stats();
    let job_phase = failed.job_phase();
    assert!(matches!(
        failed.fail_data(ticket),
        StrictBaseOpenIngress::Accepted {
            wake_scheduler: true,
            cached_bytes: 0,
        }
    ));
    assert_eq!(
        failed.phase(),
        StrictBaseOpenCoordinatorPhase::FailureQueued
    );
    assert_eq!(failed.stats(), stats);
    assert_eq!(failed.job_phase(), job_phase);
    assert_eq!(failed.resources().jobs(), 1);
    assert_eq!(failed.resources().waiting_targets(), 1);
    assert_eq!(failed.resources().registrations(), 1);
    assert_eq!(failed.resources().pending_tickets(), 0);
    assert_eq!(failed.resources().ready_resumes(), 0);
    assert_eq!(failed.resources().queued_failures(), 1);

    let source_failure =
        StrictBaseOpenCoordinatorFailure::Source(SourceError::source_unavailable());
    assert!(matches!(
        failed.run_one(&PanicOnCancellationProbe),
        StrictBaseOpenCoordinatorRun::Failed(error) if error == source_failure
    ));
    assert_eq!(failed.failure(), Some(source_failure));
    assert_eq!(failed.phase(), StrictBaseOpenCoordinatorPhase::Failed);
    assert_eq!(failed.stats(), stats);
    assert_eq!(failed.job_phase(), job_phase);
    assert_eq!(failed.resources().jobs(), 0);
    assert_eq!(failed.resources().waiting_targets(), 0);
    assert_eq!(failed.resources().registrations(), 0);
    assert_eq!(failed.resources().pending_tickets(), 0);
    assert_eq!(failed.resources().ready_resumes(), 0);
    assert_eq!(failed.resources().queued_failures(), 0);
    assert_eq!(failed.resources().cached_bytes(), 0);
    assert_eq!(failed.resources().resident_bytes(), 0);
    assert!(matches!(
        failed.run_one(&PanicOnCancellationProbe),
        StrictBaseOpenCoordinatorRun::AlreadyTerminal {
            phase: StrictBaseOpenCoordinatorPhase::Failed
        }
    ));

    println!(
        "native_strict_open_coordinator_result bytes={source_len} pending_turns={pending_turns} checkpoints=tail,section,scan,envelope,boundary upper_before_lower=true incomplete_no_work_no_poll=true ingress_non_inline=true ready_handoff=true handoff_cached_bytes={source_len} failure_queued=true failure_no_poll=true terminal_resources_zero=true scheduler_scope=single_strict_open_coordinator"
    );
}
