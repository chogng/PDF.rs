use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    ResumeCheckpoint, SmallRanges, SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId,
    SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    NeverCancelSourceRevisionChain, OpenSourceRevisionChainJob, SourceRevisionChainErrorCode,
    SourceRevisionChainJobContext, SourceRevisionChainLimitConfig, SourceRevisionChainLimitKind,
    SourceRevisionChainLimits, SourceRevisionChainPhase, SourceRevisionChainPoll,
    SourceRevisionPrimaryProof, SourceXrefStreamErrorCode,
};
use pdf_rs_object::{ObjectLimitConfig, ObjectLimits};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    RevisionEntryKind, RevisionEntryOrigin, RevisionLimitConfig, RevisionLimits,
    XrefAnchorLimitConfig, XrefAnchorLimits, XrefErrorCode, XrefLimitConfig, XrefLimits,
    XrefStreamErrorCode, XrefStreamLimits,
};

const JOB: JobId = JobId::new(801);
const TAIL: ResumeCheckpoint = ResumeCheckpoint::new(802);
const ANCHOR: ResumeCheckpoint = ResumeCheckpoint::new(803);
const TRADITIONAL: ResumeCheckpoint = ResumeCheckpoint::new(804);
const ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(805);
const BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(806);
const PAYLOAD: ResumeCheckpoint = ResumeCheckpoint::new(807);

fn context() -> SourceRevisionChainJobContext {
    SourceRevisionChainJobContext::new(JOB, TAIL, ANCHOR, TRADITIONAL, ENVELOPE, BOUNDARY, PAYLOAD)
}

fn snapshot(len: u64, tag: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([tag; 32]),
            SourceRevision::new(u64::from(tag)),
        ),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [tag ^ 0xa5; 32]),
    )
}

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    final_startxref: u64,
}

fn fixture(bytes: Vec<u8>, final_startxref: u64, tag: u8) -> Fixture {
    let len = u64::try_from(bytes.len()).unwrap();
    Fixture {
        bytes,
        snapshot: snapshot(len, tag),
        final_startxref,
    }
}

fn push_object(bytes: &mut Vec<u8>, number: u32, body: &[u8]) -> u64 {
    let offset = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(b"\nendobj\n");
    offset
}

fn append_final_marker(bytes: &mut Vec<u8>, startxref: u64) {
    bytes.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
}

fn traditional_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let root = push_object(&mut bytes, 1, b"<< /Type /Catalog >>");
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{root:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\n"
        )
        .as_bytes(),
    );
    append_final_marker(&mut bytes, startxref);
    fixture(bytes, startxref, tag)
}

fn append_stream_entry(payload: &mut Vec<u8>, kind: u8, field_two: u32, field_three: u16) {
    payload.push(kind);
    payload.extend_from_slice(&field_two.to_be_bytes());
    payload.extend_from_slice(&field_three.to_be_bytes());
}

fn primary_stream_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let root = push_object(&mut bytes, 1, b"<< /Type /Catalog >>");
    let startxref = u64::try_from(bytes.len()).unwrap();
    let mut payload = Vec::new();
    append_stream_entry(&mut payload, 0, 0, u16::MAX);
    append_stream_entry(&mut payload, 1, u32::try_from(root).unwrap(), 0);
    append_stream_entry(&mut payload, 1, u32::try_from(startxref).unwrap(), 0);
    bytes.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /XRef /Size 3 /Root 1 0 R /W [1 4 2] /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    append_final_marker(&mut bytes, startxref);
    fixture(bytes, startxref, tag)
}

fn filtered_primary_stream_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let root = push_object(&mut bytes, 1, b"<< /Type /Catalog >>");
    let startxref = u64::try_from(bytes.len()).unwrap();
    let mut payload = Vec::new();
    append_stream_entry(&mut payload, 0, 0, u16::MAX);
    append_stream_entry(&mut payload, 1, u32::try_from(root).unwrap(), 0);
    append_stream_entry(&mut payload, 1, u32::try_from(startxref).unwrap(), 0);
    bytes.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /XRef /Size 3 /Root 1 0 R /W [1 4 2] /Filter /ASCIIHexDecode /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    append_final_marker(&mut bytes, startxref);
    fixture(bytes, startxref, tag)
}

fn hybrid_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let root = push_object(&mut bytes, 1, b"<< /Type /Catalog >>");
    let hybrid = u64::try_from(bytes.len()).unwrap();
    let mut payload = Vec::new();
    append_stream_entry(&mut payload, 0, 0, u16::MAX);
    bytes.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /XRef /Size 3 /W [1 4 2] /Index [0 1] /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(
        format!(
            "xref\n0 3\n0000000000 65535 f \n{root:010} 00000 n \n{hybrid:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R /XRefStm {hybrid} >>\n"
        )
        .as_bytes(),
    );
    append_final_marker(&mut bytes, startxref);
    fixture(bytes, startxref, tag)
}

fn large_hybrid_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let root = push_object(&mut bytes, 1, b"<< /Type /Catalog >>");
    let hybrid = u64::try_from(bytes.len()).unwrap();
    let mut payload = Vec::new();
    for object_number in 0_u32..100 {
        match object_number {
            0 => append_stream_entry(&mut payload, 0, 0, u16::MAX),
            1 => append_stream_entry(&mut payload, 1, u32::try_from(root).unwrap(), 0),
            2 => append_stream_entry(&mut payload, 1, u32::try_from(hybrid).unwrap(), 0),
            _ => append_stream_entry(&mut payload, 0, 0, 0),
        }
    }
    bytes.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /XRef /Size 100 /W [1 4 2] /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(b"xref\n0 100\n");
    for object_number in 0_u32..100 {
        match object_number {
            0 => bytes.extend_from_slice(b"0000000000 65535 f \n"),
            1 => bytes.extend_from_slice(format!("{root:010} 00000 n \n").as_bytes()),
            2 => bytes.extend_from_slice(format!("{hybrid:010} 00000 n \n").as_bytes()),
            _ => bytes.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 100 /Root 1 0 R /XRefStm {hybrid} >>\n").as_bytes(),
    );
    append_final_marker(&mut bytes, startxref);
    fixture(bytes, startxref, tag)
}

fn incremental_fixture(tag: u8, invalid_previous: bool) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let root = push_object(&mut bytes, 1, b"<< /Type /Catalog >>");
    let base = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{root:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\n"
        )
        .as_bytes(),
    );
    append_final_marker(&mut bytes, base);
    let second = push_object(&mut bytes, 2, b"42");
    let newest = u64::try_from(bytes.len()).unwrap();
    let previous = if invalid_previous { newest } else { base };
    bytes.extend_from_slice(
        format!(
            "xref\n2 1\n{second:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R /Prev {previous} >>\n"
        )
        .as_bytes(),
    );
    append_final_marker(&mut bytes, newest);
    fixture(bytes, newest, tag)
}

fn mixed_incremental_fixture(tag: u8) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let root = push_object(&mut bytes, 1, b"<< /Type /Catalog >>");
    let base = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{root:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\n"
        )
        .as_bytes(),
    );
    append_final_marker(&mut bytes, base);

    let newest = u64::try_from(bytes.len()).unwrap();
    let mut payload = Vec::new();
    append_stream_entry(&mut payload, 1, u32::try_from(newest).unwrap(), 0);
    bytes.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /XRef /Size 3 /Prev {base} /W [1 4 2] /Index [2 1] /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    append_final_marker(&mut bytes, newest);
    fixture(bytes, newest, tag)
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let range = ByteRange::new(0, u64::try_from(fixture.bytes.len()).unwrap()).unwrap();
    store
        .supply(RangeResponse::new(fixture.snapshot, range, fixture.bytes.clone()).unwrap())
        .unwrap();
    store
}

struct PhaseCountingSource<'a> {
    inner: &'a RangeStore,
    stream_polls: AtomicUsize,
}

impl<'a> PhaseCountingSource<'a> {
    fn new(inner: &'a RangeStore) -> Self {
        Self {
            inner,
            stream_polls: AtomicUsize::new(0),
        }
    }

    fn stream_polls(&self) -> usize {
        self.stream_polls.load(Ordering::Relaxed)
    }
}

impl ByteSource for PhaseCountingSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.inner.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        if matches!(request.checkpoint(), ENVELOPE | BOUNDARY | PAYLOAD) {
            self.stream_polls.fetch_add(1, Ordering::Relaxed);
        }
        self.inner.poll(request)
    }
}

#[allow(
    clippy::result_large_err,
    reason = "test helper preserves the complete copyable lower error contract"
)]
fn job_with(
    fixture: &Fixture,
    context: SourceRevisionChainJobContext,
    limits: SourceRevisionChainLimits,
) -> Result<OpenSourceRevisionChainJob, pdf_rs_document::SourceRevisionChainError> {
    OpenSourceRevisionChainJob::new(
        fixture.snapshot,
        context,
        limits,
        XrefLimits::default(),
        XrefAnchorLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
        XrefStreamLimits::default(),
        RevisionLimits::default(),
    )
}

fn job(fixture: &Fixture) -> OpenSourceRevisionChainJob {
    job_with(fixture, context(), SourceRevisionChainLimits::default()).unwrap()
}

fn run_ready(
    fixture: &Fixture,
) -> (
    OpenSourceRevisionChainJob,
    pdf_rs_document::SourceAcquiredRevisionChain,
) {
    let store = supplied_store(fixture);
    let mut job = job(fixture);
    let ready = match job.poll(&store, &NeverCancelSourceRevisionChain) {
        SourceRevisionChainPoll::Ready(ready) => ready,
        other => panic!("fully supplied source did not complete: {other:?}"),
    };
    (job, ready)
}

fn failed(outcome: SourceRevisionChainPoll) -> pdf_rs_document::SourceRevisionChainError {
    match outcome {
        SourceRevisionChainPoll::Failed(error) => error,
        other => panic!("expected failure, got {other:?}"),
    }
}

fn supply_missing(store: &RangeStore, fixture: &Fixture, missing: &SmallRanges) {
    for range in missing.as_slice() {
        let start = usize::try_from(range.start()).unwrap();
        let end = usize::try_from(range.end_exclusive()).unwrap();
        store
            .supply(
                RangeResponse::new(fixture.snapshot, *range, fixture.bytes[start..end].to_vec())
                    .unwrap(),
            )
            .unwrap();
    }
}

#[test]
fn traditional_primary_retains_final_anchor_section_and_chain_without_naked_chain_access() {
    let fixture = traditional_fixture(0x91);
    let (job, ready) = run_ready(&fixture);
    assert_eq!(job.phase(), SourceRevisionChainPhase::Complete);
    assert_eq!(ready.snapshot(), fixture.snapshot);
    assert_eq!(ready.final_marker().startxref(), fixture.final_startxref);
    assert_eq!(ready.root().number(), 1);
    assert_eq!(ready.proofs().len(), 1);
    let primary = ready.proofs()[0].primary();
    assert_eq!(primary.anchor().startxref(), fixture.final_startxref);
    assert!(matches!(
        primary,
        SourceRevisionPrimaryProof::Traditional { .. }
    ));
    assert!(primary.traditional().is_some());
    assert!(ready.proofs()[0].hybrid().is_none());
    assert_eq!(ready.stats().revisions(), 1);
    assert_eq!(ready.stats().sections(), 1);
    assert_eq!(ready.stats().traditional_jobs(), 1);
    assert_eq!(ready.stats().stream_jobs(), 0);
    assert!(ready.stats().retained_bound_bytes() > ready.stats().chain().unwrap().retained_bytes());
}

#[test]
fn primary_xref_stream_uses_exact_tail_bound_and_composes_root() {
    let fixture = primary_stream_fixture(0x92);
    let (_, ready) = run_ready(&fixture);
    let primary = ready.proofs()[0].primary();
    assert!(matches!(primary, SourceRevisionPrimaryProof::Stream { .. }));
    let stream = primary.stream().unwrap();
    assert_eq!(stream.container().number(), 2);
    assert_eq!(
        stream.framed_container().object_upper_bound(),
        ready.final_marker().tail_start()
    );
    assert_eq!(
        stream.framed_container().revision_startxref(),
        fixture.final_startxref
    );
    assert_eq!(ready.stats().stream_jobs(), 1);
    assert_eq!(
        ready.entry(1).unwrap().entry().kind(),
        RevisionEntryKind::Uncompressed {
            offset: 9,
            generation: 0,
        }
    );
}

#[test]
fn hybrid_keeps_both_anchors_and_original_sections_under_primary_precedence() {
    let fixture = hybrid_fixture(0x93);
    let (_, ready) = run_ready(&fixture);
    let proof = &ready.proofs()[0];
    let primary = proof.primary();
    let hybrid = proof.hybrid().unwrap();
    assert_eq!(
        primary.kind(),
        pdf_rs_xref::RevisionPrimaryKind::Traditional
    );
    assert!(hybrid.anchor().startxref() < primary.anchor().startxref());
    assert_eq!(
        hybrid.section().framed_container().object_upper_bound(),
        primary.anchor().startxref()
    );
    assert_eq!(
        hybrid.section().framed_container().revision_startxref(),
        primary.anchor().startxref()
    );
    assert_eq!(ready.stats().sections(), 2);
    assert_eq!(ready.stats().anchor_jobs(), 2);
    assert_eq!(ready.stats().stream_jobs(), 1);
    assert_eq!(
        ready.entry(2).unwrap().origin(),
        RevisionEntryOrigin::Primary
    );
}

#[test]
fn incremental_prev_walk_is_newest_to_oldest_and_latest_wins() {
    let fixture = incremental_fixture(0x94, false);
    let (_, ready) = run_ready(&fixture);
    assert_eq!(ready.proofs().len(), 2);
    let newest = ready.proofs()[0].primary().anchor().startxref();
    let base = ready.proofs()[1].primary().anchor().startxref();
    assert_eq!(newest, fixture.final_startxref);
    assert!(base < newest);
    assert!(
        ready.proofs()[1]
            .primary()
            .traditional()
            .unwrap()
            .span()
            .end_exclusive()
            <= newest
    );
    let root = ready.entry(1).unwrap();
    assert_eq!(root.revision().ordinal(), 0);
    let newest_entry = ready.entry(2).unwrap();
    assert_eq!(newest_entry.revision().ordinal(), 1);
    assert_eq!(ready.stats().revisions(), 2);
}

#[test]
fn mixed_stream_update_walks_to_a_traditional_base_with_exact_upper_bounds() {
    let fixture = mixed_incremental_fixture(0x9c);
    let (_, ready) = run_ready(&fixture);
    assert_eq!(ready.proofs().len(), 2);
    let newest = ready.proofs()[0].primary();
    let base = ready.proofs()[1].primary();
    assert_eq!(newest.kind(), pdf_rs_xref::RevisionPrimaryKind::Stream);
    assert_eq!(base.kind(), pdf_rs_xref::RevisionPrimaryKind::Traditional);
    assert_eq!(
        newest
            .stream()
            .unwrap()
            .framed_container()
            .object_upper_bound(),
        ready.final_marker().tail_start()
    );
    assert_eq!(
        base.anchor().startxref(),
        base.traditional().unwrap().startxref()
    );
    assert!(base.anchor().startxref() < newest.anchor().startxref());
    assert_eq!(ready.entry(2).unwrap().revision().ordinal(), 1);
    assert_eq!(ready.stats().stream_jobs(), 1);
    assert_eq!(ready.stats().traditional_jobs(), 1);
}

#[test]
fn one_active_pending_replays_same_ticket_then_visits_every_distinct_phase_checkpoint() {
    let fixture = large_hybrid_fixture(0x95);
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let source_len = fixture.snapshot.len().unwrap();
    let xref_limits = XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: source_len,
        initial_tail_bytes: 32,
        max_tail_bytes: source_len,
        initial_section_bytes: 64,
        max_section_bytes: source_len,
        max_total_read_bytes: source_len * 4,
        max_total_parse_bytes: source_len * 4,
        max_subsections: 1024,
        max_entries: 1024,
    })
    .unwrap();
    let anchor_limits = XrefAnchorLimits::validate(XrefAnchorLimitConfig {
        max_source_bytes: source_len,
        max_anchor_bytes: 16,
    })
    .unwrap();
    let object_limits = ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: source_len,
        initial_envelope_bytes: 64,
        max_envelope_bytes: 256,
        initial_boundary_bytes: 32,
        max_boundary_bytes: 128,
        max_stream_bytes: 2048,
        max_total_read_bytes: 384,
        max_total_parse_bytes: 384,
    })
    .unwrap();
    let mut job = OpenSourceRevisionChainJob::new(
        fixture.snapshot,
        context(),
        SourceRevisionChainLimits::default(),
        xref_limits,
        anchor_limits,
        object_limits,
        SyntaxLimits::default(),
        XrefStreamLimits::default(),
        RevisionLimits::default(),
    )
    .unwrap();
    let (first_ticket, first_missing, before) =
        match job.poll(&store, &NeverCancelSourceRevisionChain) {
            SourceRevisionChainPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                assert_eq!(checkpoint, TAIL);
                (ticket, missing, job.stats())
            }
            other => panic!("empty store must suspend at tail: {other:?}"),
        };
    match job.poll(&store, &NeverCancelSourceRevisionChain) {
        SourceRevisionChainPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => {
            assert_eq!(ticket, first_ticket);
            assert_eq!(missing, first_missing);
            assert_eq!(checkpoint, TAIL);
            assert_eq!(job.stats(), before);
        }
        other => panic!("unchanged source must replay one ticket: {other:?}"),
    }
    supply_missing(&store, &fixture, &first_missing);

    let mut checkpoints = vec![TAIL];
    let mut reached_ready = false;
    for _ in 0..64 {
        match job.poll(&store, &NeverCancelSourceRevisionChain) {
            SourceRevisionChainPoll::Pending {
                missing,
                checkpoint,
                ..
            } => {
                checkpoints.push(checkpoint);
                supply_missing(&store, &fixture, &missing);
            }
            SourceRevisionChainPoll::Ready(ready) => {
                assert_eq!(ready.proofs().len(), 1);
                reached_ready = true;
                break;
            }
            SourceRevisionChainPoll::Failed(error) => panic!("sparse replay failed: {error:?}"),
        }
    }
    for checkpoint in [TAIL, ANCHOR, TRADITIONAL, ENVELOPE, PAYLOAD, BOUNDARY] {
        assert!(
            checkpoints.contains(&checkpoint),
            "missing checkpoint {checkpoint:?}: {checkpoints:?}"
        );
    }
    assert!(
        reached_ready,
        "sparse resume did not finish within the bounded poll loop"
    );
}

#[test]
fn cancellation_source_change_and_terminal_replay_are_stable() {
    let fixture = traditional_fixture(0x96);
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let cancellation = AtomicBool::new(false);
    let mut cancelled = job(&fixture);
    assert!(matches!(
        cancelled.poll(&store, &cancellation),
        SourceRevisionChainPoll::Pending { .. }
    ));
    cancellation.store(true, std::sync::atomic::Ordering::Release);
    let first = failed(cancelled.poll(&store, &cancellation));
    assert_eq!(first.code(), SourceRevisionChainErrorCode::Cancelled);

    struct SnapshotOnly(SourceSnapshot);
    impl ByteSource for SnapshotOnly {
        fn snapshot(&self) -> SourceSnapshot {
            self.0
        }

        fn poll(&self, _: ReadRequest) -> ReadPoll<ByteSlice> {
            panic!("snapshot mismatch must precede lower polling")
        }
    }
    let changed = SnapshotOnly(snapshot(fixture.snapshot.len().unwrap(), 0xe6));
    let replay = failed(cancelled.poll(&changed, &NeverCancelSourceRevisionChain));
    assert_eq!(replay, first);

    let full = supplied_store(&fixture);
    let mut complete = job(&fixture);
    assert!(matches!(
        complete.poll(&full, &NeverCancelSourceRevisionChain),
        SourceRevisionChainPoll::Ready(_)
    ));
    let replay = failed(complete.poll(&changed, &cancellation));
    assert_eq!(
        replay.code(),
        SourceRevisionChainErrorCode::JobAlreadyComplete
    );

    let mut mismatch = job(&fixture);
    let error = failed(mismatch.poll(&changed, &cancellation));
    assert_eq!(error.code(), SourceRevisionChainErrorCode::SnapshotMismatch);
}

#[test]
fn lower_backward_geometry_is_preserved_before_another_anchor_read() {
    let fixture = incremental_fixture(0x97, true);
    let store = supplied_store(&fixture);
    let mut job = job(&fixture);
    let error = failed(job.poll(&store, &NeverCancelSourceRevisionChain));
    assert_eq!(error.code(), SourceRevisionChainErrorCode::XrefFailure);
    assert_eq!(
        error.xref_error().unwrap().code(),
        XrefErrorCode::InvalidTrailer
    );
    assert_eq!(job.stats().anchor_jobs(), 1);
}

#[test]
fn aggregate_work_and_retained_bound_accept_exact_and_reject_one_less() {
    let fixture = traditional_fixture(0x98);
    let (_, ready) = run_ready(&fixture);
    let measured = ready.stats();
    assert_eq!(measured.max_admitted_read_bytes(), measured.read_bytes());
    assert_eq!(measured.max_admitted_parse_bytes(), measured.parse_bytes());
    let exact = SourceRevisionChainLimits::validate(SourceRevisionChainLimitConfig {
        max_total_read_bytes: measured.read_bytes(),
        max_total_parse_bytes: measured.parse_bytes(),
        max_retained_bound_bytes: measured.retained_bound_bytes(),
    })
    .unwrap();
    let store = supplied_store(&fixture);
    let mut exact_job = job_with(&fixture, context(), exact).unwrap();
    assert!(matches!(
        exact_job.poll(&store, &NeverCancelSourceRevisionChain),
        SourceRevisionChainPoll::Ready(_)
    ));

    for (limits, expected) in [
        (
            SourceRevisionChainLimitConfig {
                max_total_read_bytes: measured.read_bytes() - 1,
                max_total_parse_bytes: measured.parse_bytes(),
                max_retained_bound_bytes: measured.retained_bound_bytes(),
            },
            SourceRevisionChainLimitKind::ReadBytes,
        ),
        (
            SourceRevisionChainLimitConfig {
                max_total_read_bytes: measured.read_bytes(),
                max_total_parse_bytes: measured.parse_bytes() - 1,
                max_retained_bound_bytes: measured.retained_bound_bytes(),
            },
            SourceRevisionChainLimitKind::ParseBytes,
        ),
        (
            SourceRevisionChainLimitConfig {
                max_total_read_bytes: measured.read_bytes(),
                max_total_parse_bytes: measured.parse_bytes(),
                max_retained_bound_bytes: measured.retained_bound_bytes() - 1,
            },
            SourceRevisionChainLimitKind::RetainedBoundBytes,
        ),
    ] {
        let configured_limit = match expected {
            SourceRevisionChainLimitKind::ReadBytes => limits.max_total_read_bytes,
            SourceRevisionChainLimitKind::ParseBytes => limits.max_total_parse_bytes,
            SourceRevisionChainLimitKind::RetainedBoundBytes => limits.max_retained_bound_bytes,
            _ => unreachable!("this table covers only byte dimensions"),
        };
        let limits = SourceRevisionChainLimits::validate(limits).unwrap();
        let mut constrained = job_with(&fixture, context(), limits).unwrap();
        let error = failed(constrained.poll(&store, &NeverCancelSourceRevisionChain));
        assert_eq!(error.code(), SourceRevisionChainErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), expected);
        assert!(error.limit().unwrap().attempted() > error.limit().unwrap().limit());
        match expected {
            SourceRevisionChainLimitKind::ReadBytes => {
                assert!(constrained.stats().read_bytes() <= configured_limit);
                assert!(constrained.stats().max_admitted_read_bytes() <= configured_limit);
            }
            SourceRevisionChainLimitKind::ParseBytes => {
                assert!(constrained.stats().parse_bytes() <= configured_limit);
                assert!(constrained.stats().max_admitted_parse_bytes() <= configured_limit);
            }
            SourceRevisionChainLimitKind::RetainedBoundBytes => {
                assert!(constrained.stats().retained_bound_bytes() <= configured_limit);
            }
            _ => unreachable!("this table covers only byte dimensions"),
        }
    }

    let child_retained = measured.max_admitted_retained_bound_bytes();
    let limits = SourceRevisionChainLimits::validate(SourceRevisionChainLimitConfig {
        max_total_read_bytes: SourceRevisionChainLimits::default().max_total_read_bytes(),
        max_total_parse_bytes: SourceRevisionChainLimits::default().max_total_parse_bytes(),
        max_retained_bound_bytes: child_retained - 1,
    })
    .unwrap();
    let mut preadmission = job_with(&fixture, context(), limits).unwrap();
    let error = failed(preadmission.poll(&store, &NeverCancelSourceRevisionChain));
    assert_eq!(
        error.limit().unwrap().kind(),
        SourceRevisionChainLimitKind::RetainedBoundBytes
    );
    assert_eq!(preadmission.stats().sections(), 0);
}

#[test]
fn stream_child_admission_accepts_exact_and_rejects_one_less_before_stream_polling() {
    let fixture = primary_stream_fixture(0x9d);
    let (_, ready) = run_ready(&fixture);
    let measured = ready.stats();
    let admitted = SourceRevisionChainLimitConfig {
        max_total_read_bytes: measured.max_admitted_read_bytes(),
        max_total_parse_bytes: measured.max_admitted_parse_bytes(),
        max_retained_bound_bytes: measured.max_admitted_retained_bound_bytes(),
    };
    assert!(admitted.max_total_read_bytes > 0);
    assert!(admitted.max_total_parse_bytes > 0);
    assert!(admitted.max_retained_bound_bytes > 0);

    let store = supplied_store(&fixture);
    let exact_source = PhaseCountingSource::new(&store);
    let exact_limits = SourceRevisionChainLimits::validate(admitted).unwrap();
    let mut exact_job = job_with(&fixture, context(), exact_limits).unwrap();
    assert!(matches!(
        exact_job.poll(&exact_source, &NeverCancelSourceRevisionChain),
        SourceRevisionChainPoll::Ready(_)
    ));
    assert!(exact_source.stream_polls() > 0);

    for (limits, expected) in [
        (
            SourceRevisionChainLimitConfig {
                max_total_read_bytes: admitted.max_total_read_bytes - 1,
                ..admitted
            },
            SourceRevisionChainLimitKind::ReadBytes,
        ),
        (
            SourceRevisionChainLimitConfig {
                max_total_parse_bytes: admitted.max_total_parse_bytes - 1,
                ..admitted
            },
            SourceRevisionChainLimitKind::ParseBytes,
        ),
        (
            SourceRevisionChainLimitConfig {
                max_retained_bound_bytes: admitted.max_retained_bound_bytes - 1,
                ..admitted
            },
            SourceRevisionChainLimitKind::RetainedBoundBytes,
        ),
    ] {
        let source = PhaseCountingSource::new(&store);
        let limits = SourceRevisionChainLimits::validate(limits).unwrap();
        let mut constrained = job_with(&fixture, context(), limits).unwrap();
        let error = failed(constrained.poll(&source, &NeverCancelSourceRevisionChain));
        assert_eq!(error.code(), SourceRevisionChainErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), expected);
        assert_eq!(constrained.stats().stream_jobs(), 0);
        assert_eq!(source.stream_polls(), 0);
    }
}

#[test]
fn context_count_and_lower_xref_failures_preserve_machine_evidence() {
    let fixture = traditional_fixture(0x99);
    let invalid = SourceRevisionChainJobContext::new(
        JOB,
        TAIL,
        ANCHOR,
        TRADITIONAL,
        ENVELOPE,
        BOUNDARY,
        BOUNDARY,
    );
    let error = match job_with(&fixture, invalid, SourceRevisionChainLimits::default()) {
        Err(error) => error,
        Ok(_) => panic!("duplicate checkpoints must be rejected"),
    };
    assert_eq!(
        error.code(),
        SourceRevisionChainErrorCode::InvalidJobContext
    );

    let one_revision = RevisionLimits::validate(RevisionLimitConfig {
        max_revisions: 1,
        max_sections: 2,
        max_entries: 100,
        max_retained_bytes: 1024 * 1024,
    })
    .unwrap();
    let incremental = incremental_fixture(0x9a, false);
    let store = supplied_store(&incremental);
    let mut limited = OpenSourceRevisionChainJob::new(
        incremental.snapshot,
        context(),
        SourceRevisionChainLimits::default(),
        XrefLimits::default(),
        XrefAnchorLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
        XrefStreamLimits::default(),
        one_revision,
    )
    .unwrap();
    let error = failed(limited.poll(&store, &NeverCancelSourceRevisionChain));
    assert_eq!(error.code(), SourceRevisionChainErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        SourceRevisionChainLimitKind::Revisions
    );

    let mut malformed = traditional_fixture(0x9b);
    let anchor = usize::try_from(malformed.final_startxref).unwrap();
    malformed.bytes[anchor..anchor + 4].copy_from_slice(b"nope");
    let store = supplied_store(&malformed);
    let mut malformed_job = job(&malformed);
    let error = failed(malformed_job.poll(&store, &NeverCancelSourceRevisionChain));
    assert_eq!(error.code(), SourceRevisionChainErrorCode::XrefFailure);
    assert_eq!(
        error.xref_error().unwrap().code(),
        XrefErrorCode::InvalidXrefAnchor
    );

    let filtered = filtered_primary_stream_fixture(0x9d);
    let store = supplied_store(&filtered);
    let mut filtered_job = job(&filtered);
    let error = failed(filtered_job.poll(&store, &NeverCancelSourceRevisionChain));
    assert_eq!(
        error.code(),
        SourceRevisionChainErrorCode::SourceXrefStreamFailure
    );
    let lower = error.source_xref_stream_error().unwrap();
    assert_eq!(lower.code(), SourceXrefStreamErrorCode::XrefStreamFailure);
    assert_eq!(
        lower.xref_stream_error().unwrap().code(),
        XrefStreamErrorCode::UnsupportedFilter
    );
}
