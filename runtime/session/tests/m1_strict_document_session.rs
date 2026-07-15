use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RequestPriority, ResumeCheckpoint, SmallRanges,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_cache::{ReadyStoreEpoch, ReadyStoreSessionId};
use pdf_rs_document::{
    DocumentLimits, NeverCancelled, OpenStrictBaseRevisionJob, OutlineJobContext, OutlineLimits,
    PageTreeJobContext, PageTreeLimits, RevisionAttestationJobContext, RevisionAttestationLimits,
    RevisionId, StrictBaseOpenContext, StrictBaseOpenLimits,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_session::{
    M1RequestId, M1RequestIdentity, M1Service, M1SessionCancel, M1SessionCancelRejectReason,
    M1SessionClose, M1SessionFailure, M1SessionIngress, M1SessionIngressRejectReason,
    M1SessionPhase, M1SessionRequestError, M1SessionRun, M1SessionWait, M1StrictDocumentSession,
    RangeResumeGeneration,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{XrefJobContext, XrefLimits};

const GENERATION: RangeResumeGeneration = RangeResumeGeneration::new(73);
const OPEN_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(9_001), JobId::new(9_002), GENERATION);
const PAGE_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(9_101), JobId::new(9_102), GENERATION);
const OUTLINE_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(9_201), JobId::new(9_202), GENERATION);
const PAGE_CLOSE_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(9_301), JobId::new(9_302), GENERATION);
const OUTLINE_CLOSE_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(9_401), JobId::new(9_402), GENERATION);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

fn snapshot(seed: u8, len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([seed; 32]),
            SourceRevision::new(u64::from(seed)),
        ),
        Some(len),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [seed.wrapping_add(1); 32],
        ),
    )
}

fn fixture(seed: u8) -> Fixture {
    let bodies: &[(u32, &[u8])] = &[
        (
            1,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>\nendobj\n",
        ),
        (
            2,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        ),
        (3, b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n"),
        (
            4,
            b"4 0 obj\n<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>\nendobj\n",
        ),
        (5, b"5 0 obj\n<< /Title (Only) /Parent 4 0 R >>\nendobj\n"),
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for &(number, body) in bodies {
        offsets.push((
            number,
            u64::try_from(bytes.len()).expect("fixture offset fits u64"),
        ));
        bytes.extend_from_slice(body);
    }
    let startxref = u64::try_from(bytes.len()).expect("fixture length fits u64");
    let size = 6_u32;
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for number in 0..size {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else {
            let offset = offsets
                .iter()
                .find(|(candidate, _)| *candidate == number)
                .map(|(_, offset)| *offset)
                .expect("every nonzero object is in use");
            format!("{offset:010} 00000 n \n")
        };
        assert_eq!(row.len(), 20);
        bytes.extend_from_slice(row.as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
            .as_bytes(),
    );
    let source = snapshot(
        seed,
        u64::try_from(bytes.len()).expect("fixture length fits u64"),
    );
    Fixture {
        bytes,
        snapshot: source,
    }
}

fn strict_job(fixture: &Fixture) -> OpenStrictBaseRevisionJob {
    OpenStrictBaseRevisionJob::new(
        fixture.snapshot,
        RevisionId::new(91),
        StrictBaseOpenContext::new(
            XrefJobContext::new(
                OPEN_REQUEST.job(),
                ResumeCheckpoint::new(9_003),
                ResumeCheckpoint::new(9_004),
            ),
            RevisionAttestationJobContext::new(
                OPEN_REQUEST.job(),
                ResumeCheckpoint::new(9_005),
                ResumeCheckpoint::new(9_006),
                ResumeCheckpoint::new(9_007),
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
    .expect("the generated strict-open job validates")
}

fn session(fixture: &Fixture) -> M1StrictDocumentSession {
    M1StrictDocumentSession::new(
        ReadyStoreSessionId::new(0x51_0001),
        OPEN_REQUEST,
        strict_job(fixture),
        Default::default(),
        ReadyStoreEpoch::new(11),
        Default::default(),
    )
    .expect("built-in owner limits validate")
}

fn response(fixture: &Fixture, range: ByteRange) -> RangeResponse {
    response_with_snapshot(fixture, fixture.snapshot, range)
}

fn response_with_snapshot(
    fixture: &Fixture,
    observed: SourceSnapshot,
    range: ByteRange,
) -> RangeResponse {
    let start = usize::try_from(range.start()).expect("fixture offset fits usize");
    let end = usize::try_from(range.end_exclusive()).expect("fixture offset fits usize");
    RangeResponse::new(observed, range, fixture.bytes[start..end].to_vec())
        .expect("fixture response geometry validates")
}

fn supply_reverse(session: &mut M1StrictDocumentSession, fixture: &Fixture, missing: SmallRanges) {
    let mut pieces = Vec::new();
    for range in missing.as_slice().iter().copied() {
        if range.len() == 1 {
            pieces.push(range);
        } else {
            let lower_len = range.len() / 2;
            pieces.push(ByteRange::new(range.start(), lower_len).unwrap());
            pieces
                .push(ByteRange::new(range.start() + lower_len, range.len() - lower_len).unwrap());
        }
    }
    pieces.sort_by_key(|range| std::cmp::Reverse(range.start()));
    let last = pieces.len() - 1;
    for (index, range) in pieces.into_iter().enumerate() {
        match session.supply(response(fixture, range)) {
            M1SessionIngress::Accepted { wake_scheduler, .. } => {
                assert_eq!(wake_scheduler, index == last);
            }
            other => panic!("valid reverse-order source data must be accepted: {other:?}"),
        }
    }
}

fn drive_ready(session: &mut M1StrictDocumentSession, fixture: &Fixture) {
    loop {
        match session.run_one(&NeverCancelled) {
            M1SessionRun::WaitingForData {
                owner: M1SessionWait::Opening(request),
                missing,
                ..
            } => {
                assert_eq!(request, OPEN_REQUEST);
                supply_reverse(session, fixture, missing);
            }
            M1SessionRun::Ready => break,
            other => panic!("generated strict document must reach Ready: {other:?}"),
        }
    }
    assert_eq!(session.phase(), M1SessionPhase::Ready);
}

fn page_context(request: M1RequestIdentity) -> PageTreeJobContext {
    PageTreeJobContext::new(
        request.job(),
        ResumeCheckpoint::new(9_103),
        ResumeCheckpoint::new(9_104),
        RequestPriority::Metadata,
    )
}

fn outline_context(request: M1RequestIdentity) -> OutlineJobContext {
    OutlineJobContext::new(
        request.job(),
        ResumeCheckpoint::new(9_203),
        ResumeCheckpoint::new(9_204),
        RequestPriority::Metadata,
    )
}

#[test]
fn reverse_range_open_two_service_round_robin_and_ordered_close_are_one_actor() {
    let fixture = fixture(0xc1);
    let mut session = session(&fixture);
    assert_eq!(session.phase(), M1SessionPhase::Created);
    assert_eq!(session.resources().opening_jobs(), 1);
    assert!(matches!(
        session.supply(response(&fixture, ByteRange::new(0, 1).unwrap())),
        M1SessionIngress::Rejected {
            phase: M1SessionPhase::Created,
            reason: M1SessionIngressRejectReason::NotWaiting,
        }
    ));

    drive_ready(&mut session, &fixture);
    assert_eq!(session.resources().index_handles(), 1);
    assert_eq!(session.resources().cache_entries(), 0);
    assert_eq!(session.resources().service_jobs(), 0);

    session
        .request_page_count(
            PAGE_REQUEST,
            page_context(PAGE_REQUEST),
            PageTreeLimits::default(),
        )
        .unwrap();
    session
        .request_outline(
            OUTLINE_REQUEST,
            outline_context(OUTLINE_REQUEST),
            OutlineLimits::default(),
        )
        .unwrap();
    assert_eq!(session.resources().service_jobs(), 2);

    match session.run_one(&NeverCancelled) {
        M1SessionRun::PageCountReady { request, result } => {
            assert_eq!(request, PAGE_REQUEST);
            assert_eq!(result.page_count(), 1);
        }
        other => panic!("page count owns the first fair turn: {other:?}"),
    }
    assert_eq!(session.resources().service_jobs(), 1);
    match session.run_one(&NeverCancelled) {
        M1SessionRun::OutlineReady { request, result } => {
            assert_eq!(request, OUTLINE_REQUEST);
            assert_eq!(result.items().len(), 1);
            assert_eq!(result.items()[0].title(), "Only");
        }
        other => panic!("outline owns the second fair turn: {other:?}"),
    }
    assert_eq!(session.resources().service_jobs(), 0);

    session
        .request_page_count(
            PAGE_CLOSE_REQUEST,
            page_context(PAGE_CLOSE_REQUEST),
            PageTreeLimits::default(),
        )
        .unwrap();
    session
        .request_outline(
            OUTLINE_CLOSE_REQUEST,
            outline_context(OUTLINE_CLOSE_REQUEST),
            OutlineLimits::default(),
        )
        .unwrap();
    assert_eq!(session.resources().service_jobs(), 2);

    assert_eq!(session.close(), M1SessionClose::Queued);
    assert_eq!(session.phase(), M1SessionPhase::Closing);
    assert!(session.resources().resident_bytes() > 0);
    assert!(matches!(
        session.observe_snapshot(fixture.snapshot),
        M1SessionIngress::Rejected {
            phase: M1SessionPhase::Closing,
            reason: M1SessionIngressRejectReason::TerminalPhase,
        }
    ));
    let report = match session.run_one(&NeverCancelled) {
        M1SessionRun::Closed(report) => report,
        other => panic!("close must finish on its own non-parser turn: {other:?}"),
    };
    assert_eq!(report.previous_phase(), M1SessionPhase::Ready);
    assert_eq!(report.released_service_jobs(), 2);
    assert!(report.cache().is_some());
    assert_eq!(report.released_index_handles(), 3);
    assert!(report.source().is_some());
    assert_eq!(session.phase(), M1SessionPhase::Closed);
    assert_eq!(session.resources().resident_bytes(), 0);
    assert_eq!(session.resources().service_jobs(), 0);
    assert_eq!(session.close(), M1SessionClose::AlreadyClosed(report));
    assert!(matches!(
        session.run_one(&NeverCancelled),
        M1SessionRun::AlreadyTerminal {
            phase: M1SessionPhase::Closed
        }
    ));
}

#[test]
fn request_generation_and_exact_cancellation_reject_stale_or_mismatched_handles() {
    let fixture = fixture(0xc2);
    let mut session = session(&fixture);
    drive_ready(&mut session, &fixture);

    let stale = M1RequestIdentity::new(
        PAGE_REQUEST.request_id(),
        PAGE_REQUEST.job(),
        RangeResumeGeneration::new(GENERATION.value() + 1),
    );
    assert_eq!(
        session.request_page_count(stale, page_context(stale), PageTreeLimits::default()),
        Err(M1SessionRequestError::StaleGeneration {
            expected: GENERATION,
            actual: stale.generation(),
        })
    );
    session
        .request_page_count(
            PAGE_REQUEST,
            page_context(PAGE_REQUEST),
            PageTreeLimits::default(),
        )
        .unwrap();

    let unknown = M1RequestIdentity::new(M1RequestId::new(99), PAGE_REQUEST.job(), GENERATION);
    assert_eq!(
        session.cancel_request(unknown),
        M1SessionCancel::Rejected {
            phase: M1SessionPhase::Ready,
            reason: M1SessionCancelRejectReason::NotActive,
        }
    );
    assert_eq!(
        session.cancel_request(stale),
        M1SessionCancel::Rejected {
            phase: M1SessionPhase::Ready,
            reason: M1SessionCancelRejectReason::IdentityMismatch,
        }
    );
    assert_eq!(session.resources().service_jobs(), 1);
    assert_eq!(
        session.cancel_request(PAGE_REQUEST),
        M1SessionCancel::Cancelled {
            request: PAGE_REQUEST,
            service: Some(M1Service::PageCount),
        }
    );
    assert_eq!(session.resources().service_jobs(), 0);
    assert!(matches!(
        session.cancel_request(PAGE_REQUEST),
        M1SessionCancel::Rejected {
            reason: M1SessionCancelRejectReason::NotActive,
            ..
        }
    ));
}

#[test]
fn opening_cancel_source_change_and_pending_close_have_zero_terminal_resources() {
    let local = fixture(0xc3);

    let mut cancelled = session(&local);
    assert!(matches!(
        cancelled.run_one(&NeverCancelled),
        M1SessionRun::WaitingForData { .. }
    ));
    let stale = M1RequestIdentity::new(
        OPEN_REQUEST.request_id(),
        OPEN_REQUEST.job(),
        RangeResumeGeneration::new(GENERATION.value() + 1),
    );
    assert_eq!(
        cancelled.cancel_request(stale),
        M1SessionCancel::Rejected {
            phase: M1SessionPhase::WaitingForData,
            reason: M1SessionCancelRejectReason::IdentityMismatch,
        }
    );
    assert_eq!(
        cancelled.cancel_request(OPEN_REQUEST),
        M1SessionCancel::Cancelled {
            request: OPEN_REQUEST,
            service: None,
        }
    );
    assert_eq!(cancelled.phase(), M1SessionPhase::Failed);
    assert_eq!(cancelled.resources().resident_bytes(), 0);
    assert!(matches!(
        cancelled.run_one(&NeverCancelled),
        M1SessionRun::AlreadyTerminal {
            phase: M1SessionPhase::Failed
        }
    ));
    assert_eq!(cancelled.close(), M1SessionClose::Queued);
    let cancelled_report = match cancelled.run_one(&NeverCancelled) {
        M1SessionRun::Closed(report) => report,
        other => panic!("failed session must close on one non-parser turn: {other:?}"),
    };
    assert_eq!(
        cancelled_report.failure(),
        Some(M1SessionFailure::OpeningCancelled)
    );
    assert_eq!(
        cancelled.close(),
        M1SessionClose::AlreadyClosed(cancelled_report)
    );

    let mut changed = session(&local);
    let missing = match changed.run_one(&NeverCancelled) {
        M1SessionRun::WaitingForData { missing, .. } => missing,
        other => panic!("empty source must suspend: {other:?}"),
    };
    let foreign = fixture(0xc4);
    assert_eq!(local.bytes.len(), foreign.bytes.len());
    let range = missing.as_slice()[0];
    assert_eq!(
        changed.supply(response_with_snapshot(&local, foreign.snapshot, range)),
        M1SessionIngress::SourceChanged
    );
    assert_eq!(changed.phase(), M1SessionPhase::Failed);
    assert_eq!(changed.resources().resident_bytes(), 0);

    let mut closing = session(&local);
    assert!(matches!(
        closing.run_one(&NeverCancelled),
        M1SessionRun::WaitingForData { .. }
    ));
    assert_eq!(closing.close(), M1SessionClose::Queued);
    let report = match closing.run_one(&NeverCancelled) {
        M1SessionRun::Closed(report) => report,
        other => panic!("pending opening must close without a parser poll: {other:?}"),
    };
    assert_eq!(report.previous_phase(), M1SessionPhase::WaitingForData);
    assert!(report.opening().is_some());
    assert_eq!(closing.resources().resident_bytes(), 0);

    let mut ready_changed = session(&local);
    drive_ready(&mut ready_changed, &local);
    ready_changed
        .request_page_count(
            PAGE_REQUEST,
            page_context(PAGE_REQUEST),
            PageTreeLimits::default(),
        )
        .unwrap();
    ready_changed
        .request_outline(
            OUTLINE_REQUEST,
            outline_context(OUTLINE_REQUEST),
            OutlineLimits::default(),
        )
        .unwrap();
    assert_eq!(
        ready_changed.signal_source_changed(),
        M1SessionIngress::SourceChanged
    );
    assert_eq!(ready_changed.phase(), M1SessionPhase::Failed);
    assert_eq!(ready_changed.resources().resident_bytes(), 0);
    assert_eq!(ready_changed.resources().service_jobs(), 0);

    let mut detected_change = session(&local);
    drive_ready(&mut detected_change, &local);
    detected_change
        .request_page_count(
            PAGE_REQUEST,
            page_context(PAGE_REQUEST),
            PageTreeLimits::default(),
        )
        .unwrap();
    detected_change
        .request_outline(
            OUTLINE_REQUEST,
            outline_context(OUTLINE_REQUEST),
            OutlineLimits::default(),
        )
        .unwrap();
    assert_eq!(
        detected_change.observe_snapshot(foreign.snapshot),
        M1SessionIngress::SourceChanged
    );
    assert_eq!(detected_change.phase(), M1SessionPhase::Failed);
    assert_eq!(detected_change.resources().resident_bytes(), 0);
    assert_eq!(detected_change.resources().service_jobs(), 0);
}

#[test]
fn host_ticket_failure_is_terminal_without_inline_parser_execution() {
    let fixture = fixture(0xc5);
    let mut session = session(&fixture);
    let ticket = match session.run_one(&NeverCancelled) {
        M1SessionRun::WaitingForData { ticket, .. } => ticket,
        other => panic!("empty source must suspend: {other:?}"),
    };
    assert!(matches!(
        session.fail_data(ticket),
        M1SessionIngress::Accepted {
            wake_scheduler: true,
            ..
        }
    ));
    assert_ne!(session.phase(), M1SessionPhase::Failed);
    match session.run_one(&NeverCancelled) {
        M1SessionRun::Failed(M1SessionFailure::Opening(_)) => {}
        other => panic!("failure is consumed only by the later actor turn: {other:?}"),
    }
    assert_eq!(session.phase(), M1SessionPhase::Failed);
    assert_eq!(session.resources().resident_bytes(), 0);
}
