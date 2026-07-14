use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    DocumentLimits, NeverCancelled as NeverCancelledDocument, OpenStrictBaseRevisionJob,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionId, StrictBaseOpenContext,
    StrictBaseOpenLimits, StrictBaseOpenPhase, StrictBaseOpenPoll,
};
use pdf_rs_generate::generate_one_page_pdf;
use pdf_rs_object::{ObjectLimitConfig, ObjectLimits};
use pdf_rs_session::{
    RangeResumeArbiter, RangeResumeCancelOutcome, RangeResumeDispatch, RangeResumeErrorCode,
    RangeResumeGeneration, RangeResumePhase, RangeResumeRegistrationOutcome, RangeResumeTarget,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{XrefJobContext, XrefLimitConfig, XrefLimits};

const OPEN_JOB: JobId = JobId::new(701);
const TAIL_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(702);
const SECTION_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(703);
const SCAN_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(704);
const ENVELOPE_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(705);
const BOUNDARY_CHECKPOINT: ResumeCheckpoint = ResumeCheckpoint::new(706);
const OPEN_GENERATION: RangeResumeGeneration = RangeResumeGeneration::new(11);
const CANCEL_GENERATION: RangeResumeGeneration = RangeResumeGeneration::new(12);
const CHANGED_GENERATION: RangeResumeGeneration = RangeResumeGeneration::new(13);

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

fn response(source: SourceSnapshot, pdf: &[u8], range: ByteRange) -> RangeResponse {
    let start = usize::try_from(range.start()).expect("generated PDF offsets fit usize");
    let end = usize::try_from(range.end_exclusive()).expect("generated PDF offsets fit usize");
    RangeResponse::new(source, range, pdf[start..end].to_vec())
        .expect("the supplied bytes exactly match the response range")
}

fn register(
    arbiter: &mut RangeResumeArbiter,
    ticket: pdf_rs_bytes::DataTicket,
    checkpoint: ResumeCheckpoint,
    generation: RangeResumeGeneration,
) -> RangeResumeTarget {
    let target = RangeResumeTarget::new(OPEN_JOB, checkpoint, generation);
    assert_eq!(
        arbiter.register_pending(ticket, target).unwrap(),
        RangeResumeRegistrationOutcome::Registered
    );
    target
}

#[test]
fn native_range_resume_loop_is_out_of_order_one_shot_and_terminal_safe() {
    let pdf = generate_one_page_pdf().expect("canonical one-page PDF generation succeeds");
    let source_len = u64::try_from(pdf.len()).expect("generated PDF length fits u64");
    assert_eq!(source_len, 612);

    let source = snapshot(source_len, 0x71);
    let mut arbiter = RangeResumeArbiter::new(source, Default::default()).unwrap();
    let mut job = new_job(source);
    let mut observed_checkpoints = Vec::new();
    let mut pending_turns = 0_u64;

    let attested = loop {
        let outcome = job.poll(
            arbiter
                .byte_source()
                .expect("the active arbiter lends its empty Range source"),
            &NeverCancelledDocument,
        );
        match outcome {
            StrictBaseOpenPoll::Ready(index) => break index,
            StrictBaseOpenPoll::Failed(error) => {
                panic!("out-of-order Range supply must complete strict open: {error}")
            }
            StrictBaseOpenPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                pending_turns += 1;
                assert!(pending_turns < 64, "strict open must make bounded progress");
                observed_checkpoints.push(checkpoint);
                let target = register(&mut arbiter, ticket, checkpoint, OPEN_GENERATION);
                assert_eq!(arbiter.resources().registrations(), 1);
                assert_eq!(arbiter.resources().pending_tickets(), 1);

                let phase_before_supply = job.phase();
                let mut lower_halves = Vec::new();
                for range in missing.as_slice().iter().copied() {
                    assert!(
                        range.len() > 1,
                        "real strict-open requests must remain splittable in this fixture"
                    );
                    let lower_len = range.len() / 2;
                    let lower = ByteRange::new(range.start(), lower_len).unwrap();
                    let upper =
                        ByteRange::new(range.start() + lower_len, range.len() - lower_len).unwrap();
                    let supplied = arbiter.supply(response(source, &pdf, upper)).unwrap();
                    assert_eq!(supplied.ready_tickets(), 0);
                    assert_eq!(supplied.queued_requeues(), 0);
                    assert_eq!(job.phase(), phase_before_supply);
                    assert_eq!(
                        arbiter.take_requeue().unwrap(),
                        RangeResumeDispatch::Empty,
                        "an upper half must not wake a ticket missing its lower half"
                    );
                    lower_halves.push(lower);
                }

                let last = lower_halves.len() - 1;
                for (index, lower) in lower_halves.into_iter().enumerate() {
                    let supplied = arbiter.supply(response(source, &pdf, lower)).unwrap();
                    assert_eq!(job.phase(), phase_before_supply);
                    if index == last {
                        assert_eq!(supplied.ready_tickets(), 1);
                        assert_eq!(supplied.queued_requeues(), 1);
                        assert_eq!(
                            arbiter.take_requeue().unwrap(),
                            RangeResumeDispatch::Requeue(target)
                        );
                        assert_eq!(
                            arbiter.take_requeue().unwrap(),
                            RangeResumeDispatch::Empty,
                            "one completed ticket must dispatch its exact target once"
                        );
                    } else {
                        assert_eq!(supplied.ready_tickets(), 0);
                        assert_eq!(supplied.queued_requeues(), 0);
                        assert_eq!(arbiter.take_requeue().unwrap(), RangeResumeDispatch::Empty);
                    }
                }
            }
        }
    };

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
    assert_eq!(job.phase(), StrictBaseOpenPhase::Ready);
    assert_eq!(job.stats().xref().entries(), 5);
    assert_eq!(job.stats().attestation().objects_attested(), 4);
    assert_eq!(attested.object_attestations().len(), 4);
    assert_eq!(arbiter.resources().registrations(), 0);
    assert_eq!(arbiter.resources().ready_requeues(), 0);
    let open_release = arbiter.close();
    assert_eq!(open_release.released_cached_bytes(), source_len);
    assert_eq!(arbiter.resources().resident_bytes(), 0);

    let cancel_source = snapshot(source_len, 0x72);
    let mut cancel_arbiter = RangeResumeArbiter::new(cancel_source, Default::default()).unwrap();
    let mut cancel_job = new_job(cancel_source);
    let cancel_phase = cancel_job.phase();
    let (cancel_ticket, cancel_missing, cancel_checkpoint) = match cancel_job.poll(
        cancel_arbiter.byte_source().unwrap(),
        &NeverCancelledDocument,
    ) {
        StrictBaseOpenPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => (ticket, missing, checkpoint),
        other => panic!("an empty cancellation source must suspend: {other:?}"),
    };
    let cancel_target = register(
        &mut cancel_arbiter,
        cancel_ticket,
        cancel_checkpoint,
        CANCEL_GENERATION,
    );
    assert_eq!(
        cancel_arbiter.cancel(OPEN_JOB, CANCEL_GENERATION).unwrap(),
        RangeResumeCancelOutcome::Cancelled {
            target: cancel_target
        }
    );
    for range in cancel_missing.as_slice().iter().copied() {
        let late = cancel_arbiter
            .supply(response(cancel_source, &pdf, range))
            .unwrap();
        assert_eq!(late.ready_tickets(), 0);
        assert_eq!(late.queued_requeues(), 0);
    }
    assert_eq!(cancel_job.phase(), cancel_phase);
    assert_eq!(
        cancel_arbiter.take_requeue().unwrap(),
        RangeResumeDispatch::Empty
    );
    cancel_arbiter.close();
    assert_eq!(cancel_arbiter.resources().resident_bytes(), 0);

    let changed_source = snapshot(source_len, 0x73);
    let mut changed_arbiter = RangeResumeArbiter::new(changed_source, Default::default()).unwrap();
    let mut changed_job = new_job(changed_source);
    let (changed_ticket, changed_missing, changed_checkpoint) = match changed_job.poll(
        changed_arbiter.byte_source().unwrap(),
        &NeverCancelledDocument,
    ) {
        StrictBaseOpenPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => (ticket, missing, checkpoint),
        other => panic!("an empty changed source must suspend: {other:?}"),
    };
    register(
        &mut changed_arbiter,
        changed_ticket,
        changed_checkpoint,
        CHANGED_GENERATION,
    );
    let release = changed_arbiter.signal_source_changed().unwrap();
    assert_eq!(release.released_registrations(), 1);
    assert_eq!(release.released_pending_tickets(), 1);
    assert_eq!(changed_arbiter.phase(), RangeResumePhase::SourceChanged);
    assert_eq!(changed_arbiter.resources().resident_bytes(), 0);

    let late_range = changed_missing.as_slice()[0];
    let first_late = changed_arbiter
        .supply(response(changed_source, &pdf, late_range))
        .expect_err("a source-changed arbiter must reject late bytes");
    let repeated_late = changed_arbiter
        .supply(response(changed_source, &pdf, late_range))
        .expect_err("the same late response must remain rejected");
    assert_eq!(first_late, repeated_late);
    assert_eq!(first_late.code(), RangeResumeErrorCode::SourceChanged);
    assert_eq!(first_late.diagnostic_id(), "RPE-SESSION-0004");
    assert_eq!(changed_arbiter.resources().registrations(), 0);
    assert_eq!(changed_arbiter.resources().cached_bytes(), 0);
    assert_eq!(changed_arbiter.resources().resident_bytes(), 0);
    assert_eq!(changed_arbiter.close(), release);

    println!(
        "native_range_resume_loop_result bytes={source_len} pending_turns={pending_turns} checkpoints=tail,section,scan,envelope,boundary upper_before_lower=true exact_requeue_once=true cancel_late_requeue=false source_changed_late_rejected=true terminal_resources_zero=true"
    );
}
