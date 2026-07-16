#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_rs_bytes::{
    JobId, RangeResponse, RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision,
    SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_cache::{ReadyStoreEpoch, ReadyStoreSessionId};
use pdf_rs_document::{
    DocumentLimits, NeverCancelled, OpenStrictBaseRevisionJob, OutlineJobContext, OutlineLimits,
    PageTreeJobContext, PageTreeLimits, RevisionAttestationJobContext, RevisionAttestationLimits,
    RevisionId, StrictBaseOpenContext, StrictBaseOpenLimits,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_session::{
    M1RequestId, M1RequestIdentity, M1SessionRun, M1StrictDocumentSession, RangeResumeGeneration,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{XrefJobContext, XrefLimits};

const GENERATION: RangeResumeGeneration = RangeResumeGeneration::new(0xf1);
const OPEN_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(0xf1_01), JobId::new(0xf1_02), GENERATION);
const PAGE_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(0xf1_11), JobId::new(0xf1_12), GENERATION);
const OUTLINE_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(0xf1_21), JobId::new(0xf1_22), GENERATION);
const MAX_FUZZ_INPUT: usize = 1 << 20;
const MAX_ACTOR_TURNS: usize = 512;

fuzz_target!(|data: &[u8]| {
    if data.len() <= MAX_FUZZ_INPUT {
        exercise_native_session(data);
    }
});

fn exercise_native_session(data: &[u8]) {
    let Ok(source_len) = u64::try_from(data.len()) else {
        return;
    };
    let seed = data.first().copied().unwrap_or(0);
    let snapshot = SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([seed; 32]),
            SourceRevision::new(source_len),
        ),
        Some(source_len),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [seed.wrapping_add(1); 32],
        ),
    );
    let Ok(open_job) = OpenStrictBaseRevisionJob::new(
        snapshot,
        RevisionId::new(0xf1),
        StrictBaseOpenContext::new(
            XrefJobContext::new(
                OPEN_REQUEST.job(),
                ResumeCheckpoint::new(0xf1_03),
                ResumeCheckpoint::new(0xf1_04),
            ),
            RevisionAttestationJobContext::new(
                OPEN_REQUEST.job(),
                ResumeCheckpoint::new(0xf1_05),
                ResumeCheckpoint::new(0xf1_06),
                ResumeCheckpoint::new(0xf1_07),
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
    ) else {
        return;
    };
    let Ok(mut session) = M1StrictDocumentSession::new(
        ReadyStoreSessionId::new(0xf1_00),
        OPEN_REQUEST,
        open_job,
        Default::default(),
        ReadyStoreEpoch::new(1),
        Default::default(),
    ) else {
        return;
    };

    let mut services_requested = false;
    for _ in 0..MAX_ACTOR_TURNS {
        match session.run_one(&NeverCancelled) {
            M1SessionRun::WaitingForData { missing, .. } => {
                for range in missing.as_slice().iter().copied() {
                    let Ok(start) = usize::try_from(range.start()) else {
                        return;
                    };
                    let Ok(end) = usize::try_from(range.end_exclusive()) else {
                        return;
                    };
                    let Some(bytes) = data.get(start..end) else {
                        return;
                    };
                    let Ok(response) = RangeResponse::new(snapshot, range, bytes.to_vec()) else {
                        return;
                    };
                    let _ = session.supply(response);
                }
            }
            M1SessionRun::Ready if !services_requested => {
                services_requested = true;
                let _ = session.request_page_count(
                    PAGE_REQUEST,
                    PageTreeJobContext::new(
                        PAGE_REQUEST.job(),
                        ResumeCheckpoint::new(0xf1_13),
                        ResumeCheckpoint::new(0xf1_14),
                        RequestPriority::Metadata,
                    ),
                    PageTreeLimits::default(),
                );
                let _ = session.request_outline(
                    OUTLINE_REQUEST,
                    OutlineJobContext::new(
                        OUTLINE_REQUEST.job(),
                        ResumeCheckpoint::new(0xf1_23),
                        ResumeCheckpoint::new(0xf1_24),
                        RequestPriority::Metadata,
                    ),
                    OutlineLimits::default(),
                );
            }
            M1SessionRun::PageCountReady { .. }
            | M1SessionRun::OutlineReady { .. }
            | M1SessionRun::RequestFailed { .. }
            | M1SessionRun::Ready => {}
            M1SessionRun::NoWork
            | M1SessionRun::Failed(_)
            | M1SessionRun::Closed(_)
            | M1SessionRun::AlreadyTerminal { .. } => break,
        }
    }
}
