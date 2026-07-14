use std::mem;
use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceError, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex, DocumentError,
    DocumentErrorCategory, DocumentErrorCode, DocumentLimitKind, DocumentLimits,
    DocumentRecoverability, NeverCancelled as DocumentNeverCancelled, ReferenceChainError,
    ReferenceChainJobContext, ReferenceChainLimitConfig, ReferenceChainLimits, ReferenceChainPhase,
    ReferenceChainPoll, ResolveReferenceChainJob, ResolvedReference, RevisionAttestationJobContext,
    RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::{ObjectErrorCode, ObjectLimitKind, ObjectLimits};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits, SyntaxObject};
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const REVISION_ID: RevisionId = RevisionId::new(19);
const ATTEST_JOB: JobId = JobId::new(701);
const ATTEST_SCAN: ResumeCheckpoint = ResumeCheckpoint::new(702);
const ATTEST_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(703);
const ATTEST_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(704);
const RESOLVE_JOB: JobId = JobId::new(801);
const RESOLVE_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(802);
const RESOLVE_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(803);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

fn snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0x91; 32]), SourceRevision::new(31)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xc7; 32]),
    )
}

fn other_snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0x92; 32]), SourceRevision::new(32)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xc8; 32]),
    )
}

fn fixture(bodies: &[(u32, &[u8])], size: u32) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut in_use = Vec::new();
    for &(number, body) in bodies {
        let offset = u64::try_from(bytes.len()).expect("fixture offset fits u64");
        in_use.push((number, offset));
        bytes.extend_from_slice(body);
    }
    let startxref = u64::try_from(bytes.len()).expect("fixture length fits u64");
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for number in 0..size {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else if let Some((_, offset)) = in_use.iter().find(|&&(entry, _)| entry == number) {
            format!("{offset:010} 00000 n \n")
        } else {
            "0000000000 00000 f \n".to_owned()
        };
        assert_eq!(row.len(), 20);
        bytes.extend_from_slice(row.as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
            .as_bytes(),
    );
    let source = snapshot(u64::try_from(bytes.len()).expect("fixture length fits u64"));
    Fixture {
        bytes,
        snapshot: source,
    }
}

fn terminal_fixture() -> Fixture {
    fixture(&[(1, b"1 0 obj\n42\nendobj\n")], 2)
}

fn chain_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n2 0 R\nendobj\n"),
            (2, b"2 0 obj\n3 0 R\nendobj\n"),
            (3, b"3 0 obj\n42\nendobj\n"),
        ],
        5,
    )
}

fn heap_terminal_chain_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n2 0 R\nendobj\n"),
            (2, b"2 0 obj\n<< /Meta [(terminal)] >>\nendobj\n"),
        ],
        3,
    )
}

fn nested_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n[2 0 R]\nendobj\n"),
            (2, b"2 0 obj\n7\nendobj\n"),
            (3, b"3 0 obj\n<< /Target 2 0 R >>\nendobj\n"),
        ],
        4,
    )
}

fn self_cycle_fixture() -> Fixture {
    fixture(&[(1, b"1 0 obj\n1 0 R\nendobj\n")], 2)
}

fn long_cycle_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n2 0 R\nendobj\n"),
            (2, b"2 0 obj\n3 0 R\nendobj\n"),
            (3, b"3 0 obj\n1 0 R\nendobj\n"),
        ],
        4,
    )
}

fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("test object reference is nonzero")
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    supply_range(
        &store,
        fixture,
        ByteRange::new(
            0,
            u64::try_from(fixture.bytes.len()).expect("fixture length fits u64"),
        )
        .unwrap(),
    );
    store
}

fn supply_range(store: &RangeStore, fixture: &Fixture, range: ByteRange) {
    let start = usize::try_from(range.start()).expect("fixture offset fits usize");
    let end = usize::try_from(range.end_exclusive()).expect("fixture offset fits usize");
    store
        .supply(
            RangeResponse::new(fixture.snapshot, range, fixture.bytes[start..end].to_vec())
                .expect("fixture response matches its exact range"),
        )
        .expect("fixture range fits store limits");
}

fn parsed_xref(fixture: &Fixture) -> XrefSection {
    let store = supplied_store(fixture);
    let mut job = OpenXrefJob::new(
        fixture.snapshot,
        XrefJobContext::new(
            JobId::new(601),
            ResumeCheckpoint::new(602),
            ResumeCheckpoint::new(603),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &XrefNeverCancelled) {
        XrefPoll::Ready(section) => section,
        XrefPoll::Pending { .. } => panic!("complete xref source must not suspend"),
        XrefPoll::Failed(error) => panic!("self-authored xref must parse: {error}"),
    }
}

fn candidate(fixture: &Fixture) -> CandidateRevisionIndex {
    CandidateRevisionIndex::from_xref(
        &parsed_xref(fixture),
        REVISION_ID,
        DocumentLimits::default(),
        &DocumentNeverCancelled,
    )
    .expect("self-authored xref yields a candidate")
}

fn ready_index(fixture: &Fixture) -> AttestedRevisionIndex {
    let store = supplied_store(fixture);
    let mut job = AttestRevisionJob::new(
        candidate(fixture),
        RevisionAttestationJobContext::new(
            ATTEST_JOB,
            ATTEST_SCAN,
            ATTEST_ENVELOPE,
            ATTEST_BOUNDARY,
            RequestPriority::Metadata,
        ),
        RevisionAttestationLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Ready(index) => index,
        RevisionAttestationPoll::Pending { .. } => {
            panic!("complete attestation source must not suspend")
        }
        RevisionAttestationPoll::Failed(error) => {
            panic!("self-authored revision must attest: {error}")
        }
    }
}

fn context(priority: RequestPriority) -> ReferenceChainJobContext {
    ReferenceChainJobContext::new(RESOLVE_JOB, RESOLVE_ENVELOPE, RESOLVE_BOUNDARY, priority)
}

fn limits_with(
    max_objects: u64,
    max_reference_edges: u64,
    max_depth: u64,
    max_read: u64,
    max_parse: u64,
) -> ReferenceChainLimits {
    ReferenceChainLimits::validate(ReferenceChainLimitConfig {
        max_objects,
        max_reference_edges,
        max_depth,
        max_total_object_read_bytes: max_read,
        max_total_object_parse_bytes: max_parse,
        ..ReferenceChainLimitConfig::default()
    })
    .expect("test reference-chain limits validate")
}

fn poll_ready<'index>(
    index: &'index AttestedRevisionIndex,
    root: ObjectRef,
    source: &dyn ByteSource,
    limits: ReferenceChainLimits,
) -> (ResolvedReference, ResolveReferenceChainJob<'index>) {
    let mut job = index
        .resolve_reference_chain(root, context(RequestPriority::VisiblePage), limits)
        .expect("reference-chain job must construct");
    let resolved = match job.poll(source, &DocumentNeverCancelled) {
        ReferenceChainPoll::Ready(resolved) => resolved,
        ReferenceChainPoll::Pending { .. } => panic!("complete source must not suspend"),
        ReferenceChainPoll::Failed(error) => panic!("resolution must succeed: {error}"),
    };
    (resolved, job)
}

struct FailureSnapshot {
    cause: DocumentError,
    prefix: Vec<ObjectRef>,
    terminal: ObjectRef,
    len: usize,
    pointer: *const ReferenceChainError,
}

fn poll_failure(
    job: &mut ResolveReferenceChainJob<'_>,
    source: &dyn ByteSource,
    cancellation: &dyn pdf_rs_document::DocumentCancellation,
) -> FailureSnapshot {
    let failure = match job.poll(source, cancellation) {
        ReferenceChainPoll::Failed(error) => FailureSnapshot {
            cause: error.document_error(),
            prefix: error.chain().prefix().to_vec(),
            terminal: error.chain().terminal(),
            len: error.chain().len(),
            pointer: std::ptr::from_ref(error),
        },
        ReferenceChainPoll::Ready(_) => panic!("expected failure, got Ready"),
        ReferenceChainPoll::Pending { .. } => panic!("complete or failing source must not pend"),
    };
    assert_eq!(job.phase(), ReferenceChainPhase::Failed);
    match job.poll(source, cancellation) {
        ReferenceChainPoll::Failed(repeated) => {
            assert_eq!(std::ptr::from_ref(repeated), failure.pointer)
        }
        _ => panic!("terminal failure must replay the same borrowed error"),
    }
    failure
}

struct PanicSource(SourceSnapshot);

impl ByteSource for PanicSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        panic!("source poll must not run")
    }
}

struct EofSource(SourceSnapshot);

impl ByteSource for EofSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.0
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        ReadPoll::EndOfFile
    }
}

struct FailingSource {
    snapshot: SourceSnapshot,
    error: SourceError,
}

impl ByteSource for FailingSource {
    fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    fn poll(&self, _request: ReadRequest) -> ReadPoll<ByteSlice> {
        ReadPoll::Failed(self.error)
    }
}

fn assert_chain(
    chain: &pdf_rs_document::ReferenceChain,
    prefix: &[ObjectRef],
    terminal: ObjectRef,
) {
    assert_eq!(chain.prefix(), prefix);
    assert_eq!(chain.terminal(), terminal);
    assert_eq!(
        chain.get(0),
        Some(prefix.first().copied().unwrap_or(terminal))
    );
    assert_eq!(chain.len(), prefix.len() + 1);
}

#[test]
fn terminal_root_and_three_object_chain_return_exact_paths_values_and_stats() {
    let terminal = terminal_fixture();
    let terminal_index = ready_index(&terminal);
    let terminal_store = supplied_store(&terminal);
    let (resolved, mut job) = poll_ready(
        &terminal_index,
        object_ref(1),
        &terminal_store,
        ReferenceChainLimits::default(),
    );
    assert_chain(resolved.chain(), &[], object_ref(1));
    assert_eq!(resolved.root(), object_ref(1));
    assert_eq!(resolved.terminal_reference(), object_ref(1));
    assert_eq!(job.phase(), ReferenceChainPhase::Ready);
    assert_eq!(job.stats().objects_started(), 1);
    assert_eq!(job.stats().reference_edges(), 0);
    assert_eq!(job.stats().max_depth(), 1);
    assert!(job.stats().object_read_bytes() > 0);
    assert_eq!(
        job.stats().object_read_bytes(),
        job.stats().object_parse_bytes()
    );
    assert!(
        job.stats().retained_path_bytes()
            >= u64::try_from(std::mem::size_of::<ObjectRef>()).unwrap()
    );
    let footprint = resolved
        .try_resident_footprint()
        .expect("terminal-root footprint fits u64");
    assert_eq!(
        footprint.inline_bytes(),
        u64::try_from(mem::size_of::<ResolvedReference>()).unwrap()
    );
    assert_eq!(
        footprint.syntax_heap_bytes(),
        resolved.object().syntax_heap_bytes()
    );
    assert_eq!(footprint.syntax_heap_bytes(), 0);
    assert_eq!(
        footprint.chain_capacity_bytes(),
        job.stats().retained_path_bytes()
    );
    assert!(footprint.chain_capacity_bytes() > 0);
    assert_eq!(
        footprint.total_bytes(),
        footprint
            .inline_bytes()
            .checked_add(footprint.syntax_heap_bytes())
            .and_then(|value| value.checked_add(footprint.chain_capacity_bytes()))
            .unwrap()
    );
    let pdf_rs_object::IndirectObjectValue::Direct(value) = resolved.object().value() else {
        panic!("terminal object must be direct")
    };
    assert!(matches!(value.value(), SyntaxObject::Integer(42)));

    let complete_error = match job.poll(&terminal_store, &DocumentNeverCancelled) {
        ReferenceChainPoll::Failed(error) => error,
        _ => panic!("a successful chain job is one-shot"),
    };
    assert_eq!(
        complete_error.document_error().code(),
        DocumentErrorCode::JobAlreadyComplete
    );

    let chain = chain_fixture();
    let chain_index = ready_index(&chain);
    let chain_store = supplied_store(&chain);
    let (resolved, mut job) = poll_ready(
        &chain_index,
        object_ref(1),
        &chain_store,
        ReferenceChainLimits::default(),
    );
    assert_chain(
        resolved.chain(),
        &[object_ref(1), object_ref(2)],
        object_ref(3),
    );
    assert_eq!(job.stats().objects_started(), 3);
    assert_eq!(job.stats().reference_edges(), 2);
    assert_eq!(job.stats().max_depth(), 3);
    assert!(job.stats().object_read_bytes() > 0);
    assert_eq!(
        job.stats().object_read_bytes(),
        job.stats().object_parse_bytes()
    );
    let pdf_rs_object::IndirectObjectValue::Direct(value) = resolved.object().value() else {
        panic!("chain terminal must be direct")
    };
    assert!(matches!(value.value(), SyntaxObject::Integer(42)));
    let root_offset = chain_index
        .attestation(object_ref(1))
        .unwrap()
        .xref_offset();
    assert_ne!(
        root_offset,
        chain_index
            .attestation(object_ref(3))
            .unwrap()
            .xref_offset()
    );
    let complete_error = match job.poll(&chain_store, &DocumentNeverCancelled) {
        ReferenceChainPoll::Failed(error) => error,
        _ => panic!("a successful multi-hop chain job is one-shot"),
    };
    assert_eq!(complete_error.code(), DocumentErrorCode::JobAlreadyComplete);
    assert_eq!(complete_error.reference(), Some(object_ref(1)));
    assert_eq!(complete_error.offset(), Some(root_offset));
}

#[test]
fn reference_footprint_counts_terminal_heap_once_for_root_and_multi_hop_results() {
    let fixture = heap_terminal_chain_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);

    let (root_terminal, root_job) = poll_ready(
        &index,
        object_ref(2),
        &store,
        ReferenceChainLimits::default(),
    );
    let root_footprint = root_terminal
        .try_resident_footprint()
        .expect("root-terminal footprint fits u64");
    assert!(root_footprint.syntax_heap_bytes() > 0);
    assert_eq!(root_terminal.chain().prefix(), &[]);
    assert_eq!(
        root_footprint.chain_capacity_bytes(),
        root_job.stats().retained_path_bytes()
    );
    assert!(root_footprint.chain_capacity_bytes() > 0);

    let (multi_hop, multi_job) = poll_ready(
        &index,
        object_ref(1),
        &store,
        ReferenceChainLimits::default(),
    );
    let multi_footprint = multi_hop
        .try_resident_footprint()
        .expect("multi-hop footprint fits u64");
    assert_eq!(multi_hop.chain().prefix(), &[object_ref(1)]);
    assert_eq!(multi_hop.terminal_reference(), object_ref(2));
    assert_eq!(
        multi_footprint.syntax_heap_bytes(),
        root_footprint.syntax_heap_bytes()
    );
    assert_eq!(
        multi_footprint.syntax_heap_bytes(),
        multi_hop.object().syntax_heap_bytes()
    );
    assert_eq!(
        multi_footprint.chain_capacity_bytes(),
        multi_job.stats().retained_path_bytes()
    );
    assert_eq!(
        multi_footprint.inline_bytes(),
        u64::try_from(mem::size_of::<ResolvedReference>()).unwrap()
    );
    assert_eq!(multi_footprint.total_bytes(), root_footprint.total_bytes());
}

#[test]
fn references_nested_in_arrays_and_dictionaries_are_terminal_values() {
    let fixture = nested_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);

    for (root, expected) in [(object_ref(1), "array"), (object_ref(3), "dictionary")] {
        let (resolved, job) = poll_ready(&index, root, &store, ReferenceChainLimits::default());
        assert_chain(resolved.chain(), &[], root);
        assert_eq!(job.stats().objects_started(), 1);
        assert_eq!(job.stats().reference_edges(), 0);
        let pdf_rs_object::IndirectObjectValue::Direct(value) = resolved.object().value() else {
            panic!("nested-reference fixture must remain direct")
        };
        match expected {
            "array" => assert!(matches!(value.value(), SyntaxObject::Array(_))),
            "dictionary" => assert!(matches!(value.value(), SyntaxObject::Dictionary(_))),
            _ => unreachable!(),
        }
    }
}

#[test]
fn exact_object_identity_cycles_retain_the_full_prefix_and_closing_terminal() {
    for (fixture, expected_prefix, terminal) in [
        (self_cycle_fixture(), vec![object_ref(1)], object_ref(1)),
        (
            long_cycle_fixture(),
            vec![object_ref(1), object_ref(2), object_ref(3)],
            object_ref(1),
        ),
    ] {
        let index = ready_index(&fixture);
        let store = supplied_store(&fixture);
        let mut job = index
            .resolve_reference_chain(
                object_ref(1),
                context(RequestPriority::Metadata),
                ReferenceChainLimits::default(),
            )
            .unwrap();
        let failure = poll_failure(&mut job, &store, &DocumentNeverCancelled);
        assert_eq!(failure.cause.code(), DocumentErrorCode::ReferenceCycle);
        assert_eq!(failure.cause.category(), DocumentErrorCategory::Syntax);
        assert_eq!(
            failure.cause.recoverability(),
            DocumentRecoverability::CorrectInput
        );
        assert_eq!(failure.prefix, expected_prefix);
        assert_eq!(failure.terminal, terminal);
        assert_eq!(failure.len, failure.prefix.len() + 1);
    }
}

#[test]
fn missing_free_and_generation_mismatch_targets_retain_the_attempted_chain() {
    let cases = [
        (
            fixture(&[(1, b"1 0 obj\n4 0 R\nendobj\n")], 4),
            object_ref(4),
            DocumentErrorCode::MissingObject,
        ),
        (
            fixture(&[(1, b"1 0 obj\n2 0 R\nendobj\n")], 3),
            object_ref(2),
            DocumentErrorCode::FreeObject,
        ),
        (
            fixture(
                &[
                    (1, b"1 0 obj\n2 1 R\nendobj\n"),
                    (2, b"2 0 obj\n9\nendobj\n"),
                ],
                3,
            ),
            ObjectRef::new(2, 1).unwrap(),
            DocumentErrorCode::GenerationMismatch,
        ),
    ];

    for (fixture, terminal, expected) in cases {
        let index = ready_index(&fixture);
        let store = supplied_store(&fixture);
        let mut job = index
            .resolve_reference_chain(
                object_ref(1),
                context(RequestPriority::Metadata),
                ReferenceChainLimits::default(),
            )
            .unwrap();
        let failure = poll_failure(&mut job, &store, &DocumentNeverCancelled);
        assert_eq!(failure.cause.code(), expected);
        assert_eq!(failure.cause.category(), DocumentErrorCategory::Lookup);
        assert_eq!(failure.prefix, [object_ref(1)]);
        assert_eq!(failure.terminal, terminal);
        assert_eq!(failure.len, 2);
    }
}

#[test]
fn exact_root_lookup_precedes_invalid_context_and_tight_work_profiles() {
    let fixture = chain_fixture();
    let index = ready_index(&fixture);
    let invalid_context = ReferenceChainJobContext::new(
        RESOLVE_JOB,
        RESOLVE_ENVELOPE,
        RESOLVE_ENVELOPE,
        RequestPriority::Metadata,
    );
    let tight = limits_with(1, 1, 1, 1, 1);
    let cases = [
        (object_ref(5), DocumentErrorCode::MissingObject),
        (object_ref(4), DocumentErrorCode::FreeObject),
        (
            ObjectRef::new(1, 1).unwrap(),
            DocumentErrorCode::GenerationMismatch,
        ),
    ];
    for (root, expected) in cases {
        let error = index
            .resolve_reference_chain(root, invalid_context, tight)
            .expect_err("lookup must fail before context validation");
        assert_eq!(error.document_error().code(), expected);
        assert_chain(error.chain(), &[], root);
    }

    let store = supplied_store(&fixture);
    let (resolved, _) = poll_ready(
        &index,
        object_ref(1),
        &store,
        ReferenceChainLimits::default(),
    );
    assert_eq!(resolved.terminal_reference(), object_ref(3));
}

fn assert_chain_limit(
    failure: &FailureSnapshot,
    kind: DocumentLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
) {
    assert_eq!(failure.cause.code(), DocumentErrorCode::ResourceLimit);
    assert_eq!(failure.cause.category(), DocumentErrorCategory::Resource);
    assert_eq!(
        failure.cause.recoverability(),
        DocumentRecoverability::ReduceWorkload
    );
    let detail = failure.cause.limit().expect("aggregate limit is retained");
    assert_eq!(detail.kind(), kind);
    assert_eq!(detail.limit(), limit);
    assert_eq!(detail.consumed(), consumed);
    assert_eq!(detail.attempted(), attempted);
}

#[test]
fn object_edge_and_depth_limits_accept_exact_boundaries_and_reject_one_less() {
    let fixture = chain_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let defaults = ReferenceChainLimits::default();
    let read = defaults.max_total_object_read_bytes();
    let parse = defaults.max_total_object_parse_bytes();

    for limits in [
        limits_with(3, 2, 3, read, parse),
        limits_with(3, 2, 256, read, parse),
        limits_with(256, 2, 3, read, parse),
    ] {
        let (resolved, job) = poll_ready(&index, object_ref(1), &store, limits);
        assert_eq!(resolved.terminal_reference(), object_ref(3));
        assert_eq!(job.stats().objects_started(), 3);
        assert_eq!(job.stats().reference_edges(), 2);
        assert_eq!(job.stats().max_depth(), 3);
    }

    let cases = [
        (
            limits_with(2, 2, 3, read, parse),
            DocumentLimitKind::ReferenceChainObjects,
            2,
            3,
        ),
        (
            limits_with(3, 1, 3, read, parse),
            DocumentLimitKind::ReferenceChainEdges,
            1,
            2,
        ),
        (
            limits_with(3, 2, 2, read, parse),
            DocumentLimitKind::ReferenceChainDepth,
            2,
            2,
        ),
    ];
    for (limits, kind, ceiling, reached_depth) in cases {
        let mut job = index
            .resolve_reference_chain(object_ref(1), context(RequestPriority::Metadata), limits)
            .unwrap();
        let failure = poll_failure(&mut job, &store, &DocumentNeverCancelled);
        assert_chain_limit(&failure, kind, ceiling, ceiling, 1);
        assert_eq!(failure.prefix, [object_ref(1), object_ref(2)]);
        assert_eq!(failure.terminal, object_ref(3));
        assert_eq!(job.stats().objects_started(), 2);
        assert_eq!(job.stats().max_depth(), reached_depth);
    }
}

#[test]
fn aggregate_read_and_parse_caps_accept_exact_work_and_retain_lower_one_less_errors() {
    let fixture = chain_fixture();
    let index = ready_index(&fixture);
    let store = supplied_store(&fixture);
    let (baseline, baseline_job) = poll_ready(
        &index,
        object_ref(1),
        &store,
        ReferenceChainLimits::default(),
    );
    assert_eq!(baseline.terminal_reference(), object_ref(3));
    let read = baseline_job.stats().object_read_bytes();
    let parse = baseline_job.stats().object_parse_bytes();
    assert!(read > 1);
    assert!(parse > 1);

    let (resolved, exact) = poll_ready(
        &index,
        object_ref(1),
        &store,
        limits_with(3, 2, 3, read, parse),
    );
    assert_eq!(resolved.terminal_reference(), object_ref(3));
    assert_eq!(exact.stats().object_read_bytes(), read);
    assert_eq!(exact.stats().object_parse_bytes(), parse);

    for (limits, kind, lower_kind, ceiling) in [
        (
            limits_with(3, 2, 3, read - 1, parse),
            DocumentLimitKind::ReferenceChainObjectReadBytes,
            ObjectLimitKind::TotalReadBytes,
            read - 1,
        ),
        (
            limits_with(3, 2, 3, read, parse - 1),
            DocumentLimitKind::ReferenceChainObjectParseBytes,
            ObjectLimitKind::TotalParseBytes,
            parse - 1,
        ),
    ] {
        let mut job = index
            .resolve_reference_chain(object_ref(1), context(RequestPriority::Metadata), limits)
            .unwrap();
        let failure = poll_failure(&mut job, &store, &DocumentNeverCancelled);
        assert_eq!(failure.cause.code(), DocumentErrorCode::ResourceLimit);
        let aggregate = failure.cause.limit().expect("aggregate detail is retained");
        assert_eq!(aggregate.kind(), kind);
        assert_eq!(aggregate.limit(), ceiling);
        assert!(aggregate.consumed() <= ceiling);
        assert!(aggregate.attempted() > 0);
        let lower = failure
            .cause
            .object_error()
            .expect("complete lower object error is retained");
        assert_eq!(lower.code(), ObjectErrorCode::ResourceLimit);
        assert_eq!(
            lower.limit().expect("lower limit detail").kind(),
            lower_kind
        );
        assert!(job.stats().object_read_bytes() <= read);
        assert!(job.stats().object_parse_bytes() <= parse);
    }
}

#[test]
fn pending_replays_the_same_checkpoint_ticket_and_work_without_double_charging() {
    let fixture = chain_fixture();
    let index = ready_index(&fixture);
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut job = index
        .resolve_reference_chain(
            object_ref(1),
            context(RequestPriority::VisiblePage),
            ReferenceChainLimits::default(),
        )
        .unwrap();
    assert_eq!(job.phase(), ReferenceChainPhase::Unresolved);

    let (ticket, missing, checkpoint) = match job.poll(&store, &DocumentNeverCancelled) {
        ReferenceChainPoll::Pending {
            ticket,
            missing,
            checkpoint,
        } => (ticket, missing, checkpoint),
        _ => panic!("empty source must suspend on the root envelope"),
    };
    assert_eq!(checkpoint, RESOLVE_ENVELOPE);
    assert_eq!(job.phase(), ReferenceChainPhase::Resolving);
    let charged = job.stats();
    assert!(charged.object_read_bytes() > 0);
    assert_eq!(charged.object_parse_bytes(), 0);

    match job.poll(&store, &DocumentNeverCancelled) {
        ReferenceChainPoll::Pending {
            ticket: repeated_ticket,
            missing: repeated_missing,
            checkpoint: repeated_checkpoint,
        } => {
            assert_eq!(repeated_ticket, ticket);
            assert_eq!(repeated_missing, missing);
            assert_eq!(repeated_checkpoint, checkpoint);
        }
        _ => panic!("unchanged source must replay Pending"),
    }
    assert_eq!(job.stats(), charged);

    for range in missing.as_slice() {
        supply_range(&store, &fixture, *range);
    }
    let resolved = loop {
        match job.poll(&store, &DocumentNeverCancelled) {
            ReferenceChainPoll::Ready(resolved) => break resolved,
            ReferenceChainPoll::Pending { missing, .. } => {
                for range in missing.as_slice() {
                    supply_range(&store, &fixture, *range);
                }
            }
            ReferenceChainPoll::Failed(error) => {
                panic!("supplying every requested range must resolve: {error}")
            }
        }
    };
    assert_eq!(resolved.terminal_reference(), object_ref(3));
    assert_eq!(job.stats().objects_started(), 3);
    assert_eq!(job.stats().reference_edges(), 2);
}

#[test]
fn cancellation_before_first_read_and_between_hops_is_terminal_without_an_extra_poll() {
    let fixture = chain_fixture();
    let index = ready_index(&fixture);
    let cancelled = AtomicBool::new(true);
    let mut first = index
        .resolve_reference_chain(
            object_ref(1),
            context(RequestPriority::Metadata),
            ReferenceChainLimits::default(),
        )
        .unwrap();
    let failure = poll_failure(&mut first, &PanicSource(fixture.snapshot), &cancelled);
    assert_eq!(failure.cause.code(), DocumentErrorCode::Cancelled);
    assert_eq!(first.stats().object_read_bytes(), 0);

    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    let mut between = index
        .resolve_reference_chain(
            object_ref(1),
            context(RequestPriority::Metadata),
            ReferenceChainLimits::default(),
        )
        .unwrap();
    let missing = match between.poll(&store, &DocumentNeverCancelled) {
        ReferenceChainPoll::Pending { missing, .. } => missing,
        _ => panic!("empty root range must pend"),
    };
    for range in missing.as_slice() {
        supply_range(&store, &fixture, *range);
    }
    assert!(matches!(
        between.poll(&store, &DocumentNeverCancelled),
        ReferenceChainPoll::Pending { .. }
    ));
    assert!(between.stats().objects_started() >= 2);
    assert!(between.stats().reference_edges() >= 1);

    let cancelled = AtomicBool::new(true);
    let failure = poll_failure(&mut between, &PanicSource(fixture.snapshot), &cancelled);
    assert_eq!(failure.cause.code(), DocumentErrorCode::Cancelled);
    assert_eq!(
        failure.cause.category(),
        DocumentErrorCategory::Cancellation
    );
}

#[test]
fn snapshot_mismatch_precedes_cancellation_and_any_source_read() {
    let fixture = terminal_fixture();
    let index = ready_index(&fixture);
    let mut job = index
        .resolve_reference_chain(
            object_ref(1),
            context(RequestPriority::Metadata),
            ReferenceChainLimits::default(),
        )
        .unwrap();
    let cancelled = AtomicBool::new(true);
    let failure = poll_failure(
        &mut job,
        &PanicSource(other_snapshot(fixture.snapshot.len().unwrap())),
        &cancelled,
    );
    assert_eq!(
        failure.cause.code(),
        DocumentErrorCode::SourceSnapshotMismatch
    );
    assert_eq!(failure.cause.category(), DocumentErrorCategory::Source);
    assert_eq!(
        failure.cause.recoverability(),
        DocumentRecoverability::ReopenSource
    );
    assert_eq!(job.stats().object_read_bytes(), 0);
}

#[test]
fn lower_source_failure_and_in_range_eof_propagate_with_the_chain() {
    let fixture = terminal_fixture();
    let index = ready_index(&fixture);
    let source_error = SourceError::source_unavailable();
    let failing = FailingSource {
        snapshot: fixture.snapshot,
        error: source_error,
    };
    let mut job = index
        .resolve_reference_chain(
            object_ref(1),
            context(RequestPriority::Metadata),
            ReferenceChainLimits::default(),
        )
        .unwrap();
    let failure = poll_failure(&mut job, &failing, &DocumentNeverCancelled);
    assert_eq!(failure.cause.code(), DocumentErrorCode::SourceFailure);
    assert_eq!(failure.cause.source_error(), Some(source_error));
    assert_eq!(
        failure.cause.object_error_code(),
        Some(ObjectErrorCode::SourceFailure)
    );
    assert_eq!(failure.prefix, []);
    assert_eq!(failure.terminal, object_ref(1));

    let mut job = index
        .resolve_reference_chain(
            object_ref(1),
            context(RequestPriority::Metadata),
            ReferenceChainLimits::default(),
        )
        .unwrap();
    let failure = poll_failure(
        &mut job,
        &EofSource(fixture.snapshot),
        &DocumentNeverCancelled,
    );
    assert_eq!(
        failure.cause.code(),
        DocumentErrorCode::UnexpectedEndOfSource
    );
    assert_eq!(
        failure.cause.object_error_code(),
        Some(ObjectErrorCode::UnexpectedEndOfSource)
    );
    assert_eq!(failure.prefix, []);
    assert_eq!(failure.terminal, object_ref(1));
}

#[test]
fn debug_output_redacts_semantic_values_private_child_state_and_path_entries() {
    let secret = fixture(&[(1, b"1 0 obj\n(CHAIN_SECRET_VALUE)\nendobj\n")], 2);
    let secret_index = ready_index(&secret);
    let secret_store = supplied_store(&secret);
    let (resolved, job) = poll_ready(
        &secret_index,
        object_ref(1),
        &secret_store,
        ReferenceChainLimits::default(),
    );
    let resolved_debug = format!("{resolved:?}");
    let job_debug = format!("{job:?}");
    for debug in [&resolved_debug, &job_debug] {
        assert!(!debug.contains("CHAIN_SECRET_VALUE"));
        assert!(!debug.contains("OpenAttestedObjectJob"));
        assert!(!debug.contains("IndirectObject"));
    }

    let cycle = long_cycle_fixture();
    let cycle_index = ready_index(&cycle);
    let cycle_store = supplied_store(&cycle);
    let mut cycle_job = cycle_index
        .resolve_reference_chain(
            object_ref(1),
            context(RequestPriority::Metadata),
            ReferenceChainLimits::default(),
        )
        .unwrap();
    let error_debug = match cycle_job.poll(&cycle_store, &DocumentNeverCancelled) {
        ReferenceChainPoll::Failed(error) => format!("{error:?}"),
        _ => panic!("cycle fixture must fail"),
    };
    let cycle_job_debug = format!("{cycle_job:?}");
    for debug in [&error_debug, &cycle_job_debug] {
        assert!(!debug.contains("number: 2"));
        assert!(!debug.contains("OpenAttestedObjectJob"));
        assert!(!debug.contains("IndirectObject"));
    }
    assert!(!error_debug.contains("number: 3"));
}
