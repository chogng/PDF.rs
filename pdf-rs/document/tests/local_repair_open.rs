use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    DocumentErrorCode, DocumentLimitKind, DocumentLimits, LocalRepairOpenContext,
    LocalRepairOpenError, LocalRepairOpenLimits, LocalRepairOpenPhase, LocalRepairOpenPoll,
    LocalRepairOpenStats, LocalRepairProbeLimitConfig, LocalRepairProbeLimits,
    LocalRevisionAttestationJobContext, OpenLocallyRepairedBaseRevisionJob, OutlineJobContext,
    OutlineLimits, OutlinePoll, PageCountPoll, PageTreeJobContext, PageTreeLimits,
    RevisionAttestationLimits, RevisionId,
};
use pdf_rs_object::{
    LocalObjectJobContext, ObjectJobContext, ObjectLimits, ObjectRepairKind, ObjectRepairLimits,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    LocalXrefJobContext, XrefErrorCode, XrefJobContext, XrefLimits, XrefRepairKind,
    XrefRepairLimits,
};

const JOB: JobId = JobId::new(1200);
type ProbeLimitMutation = fn(&mut LocalRepairProbeLimitConfig);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    actual_offsets: [u64; 4],
    declared_offsets: [u64; 4],
    startxref: u64,
}

struct ServiceFixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
    startxref: u64,
}

impl ServiceFixture {
    fn store(&self) -> RangeStore {
        let store = RangeStore::new(self.snapshot, Default::default()).unwrap();
        let range = ByteRange::new(0, u64::try_from(self.bytes.len()).unwrap()).unwrap();
        store
            .supply(RangeResponse::new(self.snapshot, range, self.bytes.clone()).unwrap())
            .unwrap();
        store
    }
}

impl Fixture {
    fn store(&self, supplied: bool) -> RangeStore {
        let store = RangeStore::new(self.snapshot, Default::default()).unwrap();
        if supplied {
            let range = ByteRange::new(0, u64::try_from(self.bytes.len()).unwrap()).unwrap();
            store
                .supply(RangeResponse::new(self.snapshot, range, self.bytes.clone()).unwrap())
                .unwrap();
        }
        store
    }
}

fn fixture(repaired: bool, tag: u8) -> Fixture {
    fixture_with_prefix(repaired, tag, 0)
}

fn fixture_with_prefix(repaired: bool, tag: u8, prefix_bytes: usize) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'\n', prefix_bytes));
    let mut actual_offsets = [0_u64; 4];
    let declared_length = if repaired { 3 } else { 4 };
    let stream =
        format!("3 0 obj\n<< /Length {declared_length} >>\nstream\nDATA\nendstream\nendobj\n");
    for (index, body) in [
        b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".as_slice(),
        b"2 0 obj\n42\nendobj\n".as_slice(),
        stream.as_bytes(),
        b"4 0 obj\ntrue\nendobj\n".as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        actual_offsets[index] = u64::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(body);
    }
    if prefix_bytes > 0 {
        bytes.extend(std::iter::repeat_n(b'\n', 4096));
    }
    let startxref = u64::try_from(bytes.len()).unwrap();
    let mut declared_offsets = actual_offsets;
    if repaired {
        declared_offsets[1] += 1;
    }
    let mut xref = format!(
        "xref\n0 5\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R >>\n",
        declared_offsets[0], declared_offsets[1], declared_offsets[2], declared_offsets[3]
    )
    .into_bytes();
    if repaired {
        let separator = xref
            .windows(b"0000000000 65535 f \n".len())
            .position(|window| window == b"0000000000 65535 f \n")
            .unwrap()
            + 10;
        xref[separator] = b'\t';
    }
    bytes.extend_from_slice(&xref);
    let declared_startxref = if repaired { startxref - 1 } else { startxref };
    bytes.extend_from_slice(format!("startxref\n{declared_startxref}\n%%EOF\n").as_bytes());
    let snapshot = SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([tag; 32]),
            SourceRevision::new(u64::from(tag)),
        ),
        Some(u64::try_from(bytes.len()).unwrap()),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [tag.wrapping_add(1); 32],
        ),
    );
    Fixture {
        bytes,
        snapshot,
        actual_offsets,
        declared_offsets,
        startxref,
    }
}

fn repaired_service_fixture(tag: u8) -> ServiceFixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = [0_u64; 3];
    for (index, body) in [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n".as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        offsets[index] = u64::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(body);
    }
    let startxref = u64::try_from(bytes.len()).unwrap();
    let declared_page_tree_offset = offsets[1] + 1;
    let mut xref = format!(
        "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{declared_page_tree_offset:010} 00000 n \n{:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\n",
        offsets[0], offsets[2]
    )
    .into_bytes();
    let free_separator = xref
        .windows(b"0000000000 65535 f \n".len())
        .position(|window| window == b"0000000000 65535 f \n")
        .unwrap()
        + 10;
    xref[free_separator] = b'\t';
    bytes.extend_from_slice(&xref);
    bytes.extend_from_slice(format!("startxref\n{}\n%%EOF\n", startxref - 1).as_bytes());
    let snapshot = SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([tag; 32]),
            SourceRevision::new(u64::from(tag)),
        ),
        Some(u64::try_from(bytes.len()).unwrap()),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [tag.wrapping_add(1); 32],
        ),
    );
    ServiceFixture {
        bytes,
        snapshot,
        startxref,
    }
}

fn sparse_fixture(tag: u8) -> Fixture {
    let mut fixture = fixture_with_prefix(true, tag, 4096);
    let marker = fixture
        .bytes
        .windows(b"startxref\n".len())
        .position(|window| window == b"startxref\n")
        .unwrap();
    fixture
        .bytes
        .splice(marker..marker, std::iter::repeat_n(b'\n', 2048));
    fixture.snapshot = SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([tag; 32]),
            SourceRevision::new(u64::from(tag)),
        ),
        Some(u64::try_from(fixture.bytes.len()).unwrap()),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [tag.wrapping_add(1); 32],
        ),
    );
    fixture
}

fn last_object_offset_repair_fixture(tag: u8) -> Fixture {
    let mut fixture = fixture(false, tag);
    let actual = fixture.actual_offsets[3];
    let declared = actual + 1;
    let original_row = format!("{actual:010} 00000 n \n").into_bytes();
    let repaired_row = format!("{declared:010} 00000 n \n").into_bytes();
    let xref_start = usize::try_from(fixture.startxref).unwrap();
    let relative = fixture.bytes[xref_start..]
        .windows(original_row.len())
        .position(|window| window == original_row)
        .expect("last object xref row must exist");
    let start = xref_start + relative;
    fixture.bytes[start..start + original_row.len()].copy_from_slice(&repaired_row);
    fixture.declared_offsets[3] = declared;
    fixture
}

fn object_context(job: JobId, first_checkpoint: u64) -> LocalObjectJobContext {
    object_context_with_priority(job, first_checkpoint, RequestPriority::Metadata)
}

fn object_context_with_priority(
    job: JobId,
    first_checkpoint: u64,
    priority: RequestPriority,
) -> LocalObjectJobContext {
    LocalObjectJobContext::new(
        ObjectJobContext::new(
            job,
            ResumeCheckpoint::new(first_checkpoint),
            ResumeCheckpoint::new(first_checkpoint + 1),
            priority,
        ),
        ResumeCheckpoint::new(first_checkpoint + 2),
        ResumeCheckpoint::new(first_checkpoint + 3),
        ResumeCheckpoint::new(first_checkpoint + 4),
        ResumeCheckpoint::new(first_checkpoint + 5),
    )
}

fn context() -> LocalRepairOpenContext {
    LocalRepairOpenContext::new(
        LocalXrefJobContext::new(
            XrefJobContext::new(
                JOB,
                ResumeCheckpoint::new(1201),
                ResumeCheckpoint::new(1202),
            ),
            ResumeCheckpoint::new(1203),
            ResumeCheckpoint::new(1204),
        ),
        object_context(JOB, 1205),
        LocalRevisionAttestationJobContext::new(
            ResumeCheckpoint::new(1211),
            object_context(JOB, 1212),
        ),
    )
}

fn limits(first_pass: LocalRepairProbeLimits) -> LocalRepairOpenLimits {
    LocalRepairOpenLimits::new(
        XrefLimits::default(),
        XrefRepairLimits::default(),
        DocumentLimits::default(),
        first_pass,
        RevisionAttestationLimits::default(),
        ObjectLimits::default(),
        ObjectRepairLimits::default(),
        SyntaxLimits::default(),
    )
}

fn job(
    fixture: &Fixture,
    first_pass: LocalRepairProbeLimits,
) -> OpenLocallyRepairedBaseRevisionJob {
    OpenLocallyRepairedBaseRevisionJob::new(
        fixture.snapshot,
        RevisionId::new(1),
        context(),
        limits(first_pass),
    )
    .unwrap()
}

fn ready(
    fixture: &Fixture,
    first_pass: LocalRepairProbeLimits,
) -> (
    pdf_rs_document::LocallyRepairedRevisionIndex,
    LocalRepairOpenStats,
) {
    let store = fixture.store(true);
    let mut job = job(fixture, first_pass);
    let index = match job.poll(&store, &pdf_rs_document::NeverCancelled) {
        LocalRepairOpenPoll::Ready(index) => index,
        LocalRepairOpenPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
        LocalRepairOpenPoll::Failed(error) => panic!("local repair open failed: {error}"),
    };
    (index, job.stats())
}

fn failed(fixture: &Fixture, first_pass: LocalRepairProbeLimits) -> LocalRepairOpenError {
    let store = fixture.store(true);
    let mut job = job(fixture, first_pass);
    let error = match job.poll(&store, &pdf_rs_document::NeverCancelled) {
        LocalRepairOpenPoll::Failed(error) => error,
        LocalRepairOpenPoll::Ready(_) => panic!("expected local repair open failure"),
        LocalRepairOpenPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
    };
    assert!(matches!(
        job.poll(&store, &pdf_rs_document::NeverCancelled),
        LocalRepairOpenPoll::Failed(repeated) if repeated == error
    ));
    error
}

fn config_from(stats: LocalRepairOpenStats) -> LocalRepairProbeLimitConfig {
    let probe = stats.first_pass();
    LocalRepairProbeLimitConfig {
        max_objects: probe.objects_completed(),
        max_total_read_bytes: probe.read_bytes(),
        max_total_parse_bytes: probe.parse_bytes(),
        max_total_repair_scan_bytes: probe.repair_scan_bytes(),
        max_total_header_candidates: probe.header_candidates(),
        max_total_boundary_candidates: probe.boundary_candidates(),
        max_retained_evidence_bytes: probe.retained_evidence_bytes(),
    }
}

#[test]
fn one_job_publishes_the_complete_repaired_ledger_after_a_strict_trailing_object() {
    let fixture = fixture(true, 0xc1);
    let (index, stats) = ready(&fixture, LocalRepairProbeLimits::default());
    assert_eq!(index.snapshot(), fixture.snapshot);
    assert_eq!(index.startxref(), fixture.startxref);
    assert_eq!(index.object_attestations().len(), 4);
    assert_eq!(index.xref_diagnostics().len(), 2);
    assert_eq!(
        index.xref_diagnostics()[0].kind(),
        XrefRepairKind::StartXrefOffset
    );
    assert_eq!(
        index.xref_diagnostics()[1].kind(),
        XrefRepairKind::EntryWhitespace
    );
    assert_eq!(index.object_repair_evidence().len(), 4);
    let offset = &index.object_repair_evidence()[1];
    assert_eq!(offset.declared_offset(), fixture.declared_offsets[1]);
    assert_eq!(offset.effective_offset(), fixture.actual_offsets[1]);
    assert_eq!(
        offset.diagnostics()[0].kind(),
        ObjectRepairKind::ObjectOffset
    );
    let length = &index.object_repair_evidence()[2];
    assert_eq!(
        length.diagnostics()[0].kind(),
        ObjectRepairKind::DirectStreamLength
    );
    assert!(index.object_repair_evidence()[3].diagnostics().is_empty());

    let probe = stats.first_pass();
    assert_eq!(probe.objects_started(), 4);
    assert_eq!(probe.objects_completed(), 4);
    assert!(probe.read_bytes() > 0);
    assert!(probe.parse_bytes() > 0);
    assert!(probe.repair_scan_bytes() > 0);
    assert_eq!(probe.header_candidates(), 1);
    assert_eq!(probe.boundary_candidates(), 1);
    assert!(probe.retained_evidence_bytes() > 0);
    assert_eq!(stats.geometry().unwrap().repaired_offsets(), 1);
    assert_eq!(stats.geometry().unwrap().object_repairs(), 2);
    assert_eq!(stats.final_attestation().objects_attested(), 4);
}

#[test]
fn repaired_proof_owns_page_count_and_outline_service_jobs() {
    let fixture = repaired_service_fixture(0xca);
    let store = fixture.store();
    let mut open = OpenLocallyRepairedBaseRevisionJob::new(
        fixture.snapshot,
        RevisionId::new(1),
        context(),
        limits(LocalRepairProbeLimits::default()),
    )
    .unwrap();
    let repaired = match open.poll(&store, &pdf_rs_document::NeverCancelled) {
        LocalRepairOpenPoll::Ready(index) => index,
        LocalRepairOpenPoll::Pending { .. } => panic!("complete fixture must not remain pending"),
        LocalRepairOpenPoll::Failed(error) => panic!("local repair open failed: {error}"),
    };
    assert_eq!(repaired.startxref(), fixture.startxref);
    assert_eq!(repaired.xref_diagnostics().len(), 2);
    assert_eq!(
        repaired
            .object_repair_evidence()
            .iter()
            .filter(|evidence| !evidence.diagnostics().is_empty())
            .count(),
        1
    );

    let mut borrowed_pages = repaired
        .count_pages(
            PageTreeJobContext::new(
                JobId::new(1250),
                ResumeCheckpoint::new(1251),
                ResumeCheckpoint::new(1252),
                RequestPriority::VisiblePage,
            ),
            PageTreeLimits::default(),
        )
        .unwrap();
    match borrowed_pages.poll(&store, &pdf_rs_document::NeverCancelled) {
        PageCountPoll::Ready(result) => assert_eq!(result.page_count(), 1),
        PageCountPoll::Pending { .. } => {
            panic!("resident borrowed page service must not remain pending")
        }
        PageCountPoll::Failed(error) => panic!("borrowed repaired page service failed: {error}"),
    }

    let shared = repaired.into_shared();
    assert_eq!(shared.as_repaired().xref_diagnostics().len(), 2);
    assert_eq!(shared.as_repaired().object_repair_evidence().len(), 3);
    let mut pages = shared
        .count_pages_owned(
            PageTreeJobContext::new(
                JobId::new(1300),
                ResumeCheckpoint::new(1301),
                ResumeCheckpoint::new(1302),
                RequestPriority::VisiblePage,
            ),
            PageTreeLimits::default(),
        )
        .unwrap();
    let mut outline = shared
        .read_outline_owned(
            OutlineJobContext::new(
                JobId::new(1400),
                ResumeCheckpoint::new(1401),
                ResumeCheckpoint::new(1402),
                RequestPriority::Metadata,
            ),
            OutlineLimits::default(),
        )
        .unwrap();
    drop(shared);

    match pages.poll(&store, &pdf_rs_document::NeverCancelled) {
        PageCountPoll::Ready(result) => assert_eq!(result.page_count(), 1),
        PageCountPoll::Pending { .. } => panic!("resident page service must not remain pending"),
        PageCountPoll::Failed(error) => panic!("repaired page service failed: {error}"),
    }
    match outline.poll(&store, &pdf_rs_document::NeverCancelled) {
        OutlinePoll::Ready(result) => {
            assert!(result.root().is_none());
            assert!(result.items().is_empty());
            assert_eq!(result.visible_items(), 0);
        }
        OutlinePoll::Pending { .. } => panic!("resident outline service must not remain pending"),
        OutlinePoll::Failed(error) => panic!("repaired outline service failed: {error}"),
    }
}

#[test]
fn canonical_input_succeeds_with_zero_repair_only_aggregate_caps() {
    let fixture = fixture(false, 0xc2);
    let zero_repair = LocalRepairProbeLimits::validate(LocalRepairProbeLimitConfig {
        max_total_repair_scan_bytes: 0,
        max_total_header_candidates: 0,
        max_total_boundary_candidates: 0,
        ..LocalRepairProbeLimitConfig::default()
    })
    .unwrap();
    let (index, stats) = ready(&fixture, zero_repair);
    assert!(index.xref_diagnostics().is_empty());
    assert!(
        index
            .object_repair_evidence()
            .iter()
            .all(|proof| proof.diagnostics().is_empty())
    );
    assert_eq!(stats.first_pass().objects_completed(), 4);
    assert_eq!(stats.first_pass().repair_scan_bytes(), 0);
}

#[test]
fn every_first_pass_aggregate_accepts_exact_and_rejects_one_less() {
    let fixture = fixture(true, 0xc3);
    let (_, baseline) = ready(&fixture, LocalRepairProbeLimits::default());
    let exact_config = config_from(baseline);
    let exact = LocalRepairProbeLimits::validate(exact_config).unwrap();
    let (_, exact_stats) = ready(&fixture, exact);
    assert_eq!(exact_stats.first_pass(), baseline.first_pass());
    assert_eq!(exact_stats.first_pass().objects_completed(), 4);
    assert!(exact_stats.first_pass().repair_scan_bytes() > 0);

    let cases: [(ProbeLimitMutation, DocumentLimitKind); 7] = [
        (
            |config| config.max_objects -= 1,
            DocumentLimitKind::RepairProbeObjects,
        ),
        (
            |config| config.max_total_read_bytes -= 1,
            DocumentLimitKind::RepairProbeReadBytes,
        ),
        (
            |config| config.max_total_parse_bytes -= 1,
            DocumentLimitKind::RepairProbeParseBytes,
        ),
        (
            |config| config.max_total_repair_scan_bytes -= 1,
            DocumentLimitKind::RepairProbeScanBytes,
        ),
        (
            |config| config.max_total_header_candidates = 0,
            DocumentLimitKind::RepairProbeHeaderCandidates,
        ),
        (
            |config| config.max_total_boundary_candidates = 0,
            DocumentLimitKind::RepairProbeBoundaryCandidates,
        ),
        (
            |config| config.max_retained_evidence_bytes -= 1,
            DocumentLimitKind::RepairProbeEvidenceBytes,
        ),
    ];
    for (mutation, expected_kind) in cases {
        let mut config = exact_config;
        mutation(&mut config);
        let limits = LocalRepairProbeLimits::validate(config).unwrap();
        let error = failed(&fixture, limits);
        let document = error
            .document()
            .expect("aggregate failure stays document-owned");
        assert_eq!(document.code(), DocumentErrorCode::ResourceLimit);
        assert_eq!(document.limit().unwrap().kind(), expected_kind);
    }
}

#[test]
fn nested_candidate_failure_keeps_parent_read_and_parse_aggregate_evidence() {
    let fixture = last_object_offset_repair_fixture(0xc7);
    let (_, baseline) = ready(&fixture, LocalRepairProbeLimits::default());
    let exact = config_from(baseline);
    assert_eq!(baseline.first_pass().header_candidates(), 1);

    for (mutation, document_kind, object_kind) in [
        (
            (|config: &mut LocalRepairProbeLimitConfig| config.max_total_read_bytes -= 1)
                as ProbeLimitMutation,
            DocumentLimitKind::RepairProbeReadBytes,
            pdf_rs_object::ObjectLimitKind::TotalReadBytes,
        ),
        (
            (|config: &mut LocalRepairProbeLimitConfig| config.max_total_parse_bytes -= 1)
                as ProbeLimitMutation,
            DocumentLimitKind::RepairProbeParseBytes,
            pdf_rs_object::ObjectLimitKind::TotalParseBytes,
        ),
    ] {
        let mut config = exact;
        mutation(&mut config);
        let error = failed(&fixture, LocalRepairProbeLimits::validate(config).unwrap());
        let document = error
            .document()
            .expect("nested child exhaustion must remain parent-classified");
        assert_eq!(document.limit().unwrap().kind(), document_kind);
        assert_eq!(
            document.object_error().unwrap().limit().unwrap().kind(),
            object_kind
        );
    }
}

#[test]
fn sparse_out_of_order_supply_resumes_every_major_phase_without_double_charging() {
    let fixture = sparse_fixture(0xc4);
    let store = fixture.store(false);
    let mut job = job(&fixture, LocalRepairProbeLimits::default());
    let mut checkpoints = Vec::new();
    for _ in 0..256 {
        match job.poll(&store, &pdf_rs_document::NeverCancelled) {
            LocalRepairOpenPoll::Pending {
                missing,
                checkpoint,
                ..
            } => {
                checkpoints.push(checkpoint);
                let charged = job.stats();
                assert!(matches!(
                    job.poll(&store, &pdf_rs_document::NeverCancelled),
                    LocalRepairOpenPoll::Pending { checkpoint: repeated, .. }
                        if repeated == checkpoint
                ));
                assert_eq!(job.stats(), charged);
                for range in missing.as_slice().iter().rev() {
                    let midpoint = range.start() + range.len() / 2;
                    let pieces = if midpoint == range.start() {
                        vec![*range]
                    } else {
                        vec![
                            ByteRange::new(midpoint, range.end_exclusive() - midpoint).unwrap(),
                            ByteRange::new(range.start(), midpoint - range.start()).unwrap(),
                        ]
                    };
                    for piece in pieces {
                        let start = usize::try_from(piece.start()).unwrap();
                        let end = usize::try_from(piece.end_exclusive()).unwrap();
                        store
                            .supply(
                                RangeResponse::new(
                                    fixture.snapshot,
                                    piece,
                                    fixture.bytes[start..end].to_vec(),
                                )
                                .unwrap(),
                            )
                            .unwrap();
                    }
                }
            }
            LocalRepairOpenPoll::Ready(index) => {
                assert_eq!(index.object_attestations().len(), 4);
                break;
            }
            LocalRepairOpenPoll::Failed(error) => panic!("sparse open failed: {error}"),
        }
    }
    for required in [1201, 1202, 1205, 1211] {
        assert!(
            checkpoints.contains(&ResumeCheckpoint::new(required)),
            "checkpoint {required} was not observed: {checkpoints:?}"
        );
    }
    assert_eq!(job.phase(), LocalRepairOpenPhase::Ready);
}

#[test]
fn context_limits_cancellation_and_source_change_are_stable_and_classified() {
    let defaults = LocalRepairProbeLimitConfig::default();
    let zero_repair = LocalRepairProbeLimits::validate(LocalRepairProbeLimitConfig {
        max_total_repair_scan_bytes: 0,
        max_total_header_candidates: 0,
        max_total_boundary_candidates: 0,
        ..defaults
    })
    .unwrap();
    assert_eq!(zero_repair.max_total_repair_scan_bytes(), 0);
    assert_eq!(zero_repair.max_total_header_candidates(), 0);
    assert_eq!(zero_repair.max_total_boundary_candidates(), 0);
    for invalid in [
        LocalRepairProbeLimitConfig {
            max_objects: 0,
            ..defaults
        },
        LocalRepairProbeLimitConfig {
            max_total_read_bytes: 0,
            ..defaults
        },
        LocalRepairProbeLimitConfig {
            max_total_parse_bytes: 0,
            ..defaults
        },
        LocalRepairProbeLimitConfig {
            max_retained_evidence_bytes: 0,
            ..defaults
        },
        LocalRepairProbeLimitConfig {
            max_objects: 4_000_001,
            ..defaults
        },
        LocalRepairProbeLimitConfig {
            max_total_read_bytes: 1024 * 1024 * 1024 + 1,
            ..defaults
        },
        LocalRepairProbeLimitConfig {
            max_total_parse_bytes: 1024 * 1024 * 1024 + 1,
            ..defaults
        },
        LocalRepairProbeLimitConfig {
            max_total_repair_scan_bytes: 1024 * 1024 * 1024 + 1,
            ..defaults
        },
        LocalRepairProbeLimitConfig {
            max_total_header_candidates: 8_000_001,
            ..defaults
        },
        LocalRepairProbeLimitConfig {
            max_total_boundary_candidates: 8_000_001,
            ..defaults
        },
        LocalRepairProbeLimitConfig {
            max_retained_evidence_bytes: 512 * 1024 * 1024 + 1,
            ..defaults
        },
    ] {
        assert_eq!(
            LocalRepairProbeLimits::validate(invalid)
                .unwrap_err()
                .code(),
            DocumentErrorCode::InvalidLimits
        );
    }

    let source_fixture = fixture(true, 0xc5);
    let duplicate = LocalRepairOpenContext::new(
        context().xref(),
        context().first_pass_object(),
        LocalRevisionAttestationJobContext::new(
            context().first_pass_object().header_scan_checkpoint(),
            context().final_attestation().object_context(),
        ),
    );
    let error = OpenLocallyRepairedBaseRevisionJob::new(
        source_fixture.snapshot,
        RevisionId::new(1),
        duplicate,
        LocalRepairOpenLimits::default(),
    )
    .unwrap_err();
    let document = error.document().unwrap();
    assert_eq!(
        document.code(),
        DocumentErrorCode::InvalidLocalRepairOpenContext
    );
    assert_eq!(document.diagnostic_id(), "RPE-DOCUMENT-0057");

    for invalid in [
        LocalRepairOpenContext::new(
            context().xref(),
            object_context(JobId::new(1201), 1205),
            context().final_attestation(),
        ),
        LocalRepairOpenContext::new(
            context().xref(),
            object_context_with_priority(JOB, 1205, RequestPriority::VisiblePage),
            context().final_attestation(),
        ),
    ] {
        let error = OpenLocallyRepairedBaseRevisionJob::new(
            source_fixture.snapshot,
            RevisionId::new(1),
            invalid,
            LocalRepairOpenLimits::default(),
        )
        .unwrap_err();
        assert_eq!(
            error.document().unwrap().code(),
            DocumentErrorCode::InvalidLocalRepairOpenContext
        );
    }

    let store = source_fixture.store(true);
    let cancelled = AtomicBool::new(true);
    let mut cancelled_job = job(&source_fixture, LocalRepairProbeLimits::default());
    let cancellation_error = match cancelled_job.poll(&store, &cancelled) {
        LocalRepairOpenPoll::Failed(error) => error,
        outcome => panic!("cancelled open must fail: {outcome:?}"),
    };
    assert!(cancellation_error.is_cancelled());
    assert!(matches!(
        cancelled_job.poll(&store, &cancelled),
        LocalRepairOpenPoll::Failed(repeated) if repeated == cancellation_error
    ));

    let foreign = fixture(true, 0xc6);
    let foreign_store = foreign.store(true);
    let mut changed = job(&source_fixture, LocalRepairProbeLimits::default());
    let error = match changed.poll(&foreign_store, &pdf_rs_document::NeverCancelled) {
        LocalRepairOpenPoll::Failed(error) => error,
        outcome => panic!("source mismatch must fail: {outcome:?}"),
    };
    assert_eq!(
        error.xref().unwrap().code(),
        XrefErrorCode::SnapshotMismatch
    );
    assert!(matches!(
        changed.poll(&foreign_store, &pdf_rs_document::NeverCancelled),
        LocalRepairOpenPoll::Failed(repeated) if repeated == error
    ));
}
