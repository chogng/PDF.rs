use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_cache::{
    ReadyAdmission, ReadyLookup, ReadyStoreBinding, ReadyStoreEpoch, ReadyStoreKey,
    ReadyStoreLimits, ReadyStoreSessionId,
};
use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_document::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, DocumentLimits,
    NeverCancelled as NeverCancelledDocument, ObjectAttestationKind, OpenStrictBaseRevisionJob,
    PageCountPoll, PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits, PageTreePhase,
    ReferenceChainJobContext, ReferenceChainLimits, ReferenceChainPoll,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionId, StrictBaseOpenContext,
    StrictBaseOpenLimits, StrictBaseOpenPhase, StrictBaseOpenPoll,
};
use pdf_rs_generate::generate_one_page_pdf;
use pdf_rs_object::{IndirectObjectValue, ObjectLimits, ObjectWorkCaps};
use pdf_rs_session::{ReadySessionOwner, ReadySessionPhase};
use pdf_rs_syntax::{ObjectRef, PdfDictionary, SyntaxLimits, SyntaxObject};
use pdf_rs_xref::{XrefJobContext, XrefLimits, XrefPhase};

const PDF_BYTES: u64 = 612;
const PDF_SHA256: &str = "9c819e549afcc89d03b380c3c1bd47128aa2b70ae30a35245e6a0e30132875db";
const SOURCE_STABLE_ID: [u8; 32] = [0x6e; 32];
const READY_STORE_SESSION_ID: ReadyStoreSessionId = ReadyStoreSessionId::new(0x6e61_7469_7665_0001);
const READY_STORE_EPOCH: ReadyStoreEpoch = ReadyStoreEpoch::new(1);
const PDFIUM_O4_EVIDENCE: &str = include_str!(
    "../../baseline/pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1.toml"
);
const PDFIUM_BUILD_READINESS_EVIDENCE: &str = include_str!(
    "../../baseline/pdfium/evidence/pdfium-c040cf96-macos-arm64-build-readiness-v1.toml"
);
const FEATURE_MAP: &str = include_str!("../../../docs/traceability/feature-map.toml");
const SPEC_MAP: &str = include_str!("../../../docs/traceability/spec-map.toml");
const STARTXREF: u64 = 449;
const OBJECT_OFFSETS: [u64; 4] = [186, 235, 292, 396];
const OBJECT_UPPER_BOUNDS: [u64; 4] = [235, 292, 396, 449];
const OBJECT_SPANS: [(u64, u64); 4] = [(186, 234), (235, 291), (292, 395), (396, 448)];
const HEADER_SPANS: [(u64, u64); 4] = [(186, 193), (235, 242), (292, 299), (396, 403)];
const ENDOBJ_SPANS: [(u64, u64); 4] = [(228, 234), (285, 291), (389, 395), (442, 448)];

fn snapshot(output_sha256: [u8; 32]) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new(SOURCE_STABLE_ID),
            SourceRevision::new(1),
        ),
        Some(PDF_BYTES),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, output_sha256),
    )
}

fn unique_value<'a>(document: &'a str, key: &str) -> &'a str {
    let prefix = format!("{key} = ");
    let mut values = document
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix));
    let value = values
        .next()
        .unwrap_or_else(|| panic!("evidence must contain exactly one {key} field"));
    assert!(
        values.next().is_none(),
        "evidence must contain exactly one {key} field"
    );
    value
}

fn top_level_value<'a>(document: &'a str, key: &str) -> &'a str {
    let prefix = format!("{key} = ");
    let mut values = document
        .lines()
        .take_while(|line| !line.starts_with("[["))
        .filter_map(|line| line.strip_prefix(&prefix));
    let value = values
        .next()
        .unwrap_or_else(|| panic!("evidence top level must contain exactly one {key} field"));
    assert!(
        values.next().is_none(),
        "evidence top level must contain exactly one {key} field"
    );
    value
}

fn top_level_version(document: &str) -> Option<&str> {
    document
        .lines()
        .take_while(|line| !line.starts_with("[["))
        .find_map(|line| line.strip_prefix("version = \"")?.strip_suffix('"'))
}

fn record_with_id<'a>(document: &'a str, kind: &str, id: &str) -> Option<&'a str> {
    let header = format!("{kind}]]");
    let id_line = format!("id = \"{id}\"");
    document
        .split("\n[[")
        .find(|record| record.starts_with(&header) && record.lines().any(|line| line == id_line))
}

fn direct_dictionary(object: &AttestedObject, source: SourceSnapshot) -> &PdfDictionary {
    match object.value() {
        IndirectObjectValue::Direct(value) if value.source() == source.identity() => {
            match value.value() {
                SyntaxObject::Dictionary(dictionary) => dictionary,
                _ => panic!("canonical object must reopen as a direct dictionary"),
            }
        }
        _ => panic!("canonical object must reopen as a source-bound direct value"),
    }
}

fn dictionary_value<'a>(dictionary: &'a PdfDictionary, key: &[u8]) -> &'a SyntaxObject {
    dictionary
        .get(key)
        .unwrap_or_else(|| panic!("canonical dictionary must contain {key:?}"))
        .value()
}

#[test]
fn generated_pdf_completes_strict_base_revision_attestation_loop() {
    let pdf = generate_one_page_pdf().expect("canonical PDF generation succeeds");
    let output_sha256 = sha256(&pdf).expect("canonical PDF fits the SHA-256 framing limit");
    let output_sha256_hex = hex_digest(&output_sha256);
    assert_eq!(u64::try_from(pdf.len()).unwrap(), PDF_BYTES);
    assert_eq!(output_sha256_hex, PDF_SHA256);
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "id"),
        "\"pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1\""
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "upstream_revision"),
        "\"c040cf96106a87220b814a1a892649cf2d7f1934\""
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "upstream_build_readiness_evidence"),
        "\"pdfium-c040cf96-macos-arm64-build-readiness-v1\""
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "platform"),
        "\"macos-arm64\""
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "adapter_profile"),
        "\"pdfium-public-c-api-pixel-only-v1\""
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "fixture_sha256"),
        format!("\"sha256:{PDF_SHA256}\"")
    );
    assert_eq!(unique_value(PDFIUM_O4_EVIDENCE, "fixture_bytes"), "612");
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "fixture_hash_verified"),
        "true"
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "adapter_exercised"),
        "true"
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "blank_comparison_exact"),
        "true"
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "blank_different_pixels"),
        "0"
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "blank_analytic_check_kind"),
        "\"O1-analytic-expectation-vs-O4-observation\""
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "native_engine_exercised"),
        "false"
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "native_vs_pdfium_differential"),
        "false"
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "differential_eligible"),
        "false"
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "product_correctness_eligible"),
        "false"
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "performance_eligible"),
        "false"
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "baseline_registration_eligible"),
        "false"
    );
    assert_eq!(
        unique_value(PDFIUM_O4_EVIDENCE, "release_gate_eligible"),
        "false"
    );
    assert_eq!(
        top_level_value(PDFIUM_BUILD_READINESS_EVIDENCE, "id"),
        "\"pdfium-c040cf96-macos-arm64-build-readiness-v1\""
    );
    assert_eq!(
        top_level_value(PDFIUM_BUILD_READINESS_EVIDENCE, "engine"),
        "\"pdfium\""
    );
    assert_eq!(
        top_level_value(PDFIUM_BUILD_READINESS_EVIDENCE, "platform"),
        "\"macos-arm64\""
    );
    assert_eq!(
        top_level_value(PDFIUM_BUILD_READINESS_EVIDENCE, "upstream_revision"),
        "\"c040cf96106a87220b814a1a892649cf2d7f1934\""
    );
    assert_eq!(
        top_level_value(PDFIUM_BUILD_READINESS_EVIDENCE, "build_readiness_outcome"),
        "\"pass\""
    );
    assert_eq!(
        top_level_value(PDFIUM_BUILD_READINESS_EVIDENCE, "pdf_rs_fixture_exercised"),
        "true"
    );
    assert_eq!(
        top_level_value(PDFIUM_BUILD_READINESS_EVIDENCE, "differential_eligible"),
        "false"
    );
    assert_eq!(
        top_level_value(PDFIUM_BUILD_READINESS_EVIDENCE, "oracle_authority"),
        "\"not-applicable\""
    );
    let pdfium_pageinfo = record_with_id(
        PDFIUM_BUILD_READINESS_EVIDENCE,
        "execution",
        "pdf-rs-generated-fixture-pageinfo",
    )
    .expect("PDFium build-readiness evidence must retain the same-input pageinfo execution");
    assert_eq!(
        unique_value(pdfium_pageinfo, "kind"),
        "\"project-fixture-parse-smoke\""
    );
    assert_eq!(
        unique_value(pdfium_pageinfo, "program"),
        "\"out/Testing/pdfium_test\""
    );
    assert_eq!(
        unique_value(pdfium_pageinfo, "argv"),
        "[\"--show-pageinfo\", \"$PDF_RS_ROOT/tests/cases/infrastructure/synthetic-failure-bundle-001/input.pdf\"]"
    );
    assert_eq!(
        unique_value(pdfium_pageinfo, "fixture_sha256"),
        format!("\"sha256:{PDF_SHA256}\"")
    );
    assert_eq!(unique_value(pdfium_pageinfo, "fixture_bytes"), "612");
    assert_eq!(unique_value(pdfium_pageinfo, "exit_code"), "0");
    let pdfium_pages_processed = unique_value(pdfium_pageinfo, "pages_processed")
        .parse::<u64>()
        .expect("recorded PDFium pages_processed is a u64");
    assert_eq!(pdfium_pages_processed, 1);
    assert_eq!(
        unique_value(pdfium_pageinfo, "observed_page_info"),
        "\"page 0 MediaBox 0,0,200,200; no CropBox, BleedBox, TrimBox, or ArtBox\""
    );

    let source = snapshot(output_sha256);
    let store = RangeStore::new(source, Default::default()).expect("range store is valid");
    let complete_range = ByteRange::new(0, PDF_BYTES).expect("canonical range is valid");
    store
        .supply(
            RangeResponse::new(source, complete_range, pdf.clone())
                .expect("canonical response is snapshot-bound"),
        )
        .expect("canonical bytes fit the range store limits");

    let open_context = StrictBaseOpenContext::new(
        XrefJobContext::new(
            JobId::new(90),
            ResumeCheckpoint::new(91),
            ResumeCheckpoint::new(92),
        ),
        RevisionAttestationJobContext::new(
            JobId::new(90),
            ResumeCheckpoint::new(93),
            ResumeCheckpoint::new(94),
            ResumeCheckpoint::new(95),
            RequestPriority::VisiblePage,
        ),
    );
    let mut open_job = OpenStrictBaseRevisionJob::new(
        source,
        RevisionId::new(1),
        open_context,
        StrictBaseOpenLimits::new(
            XrefLimits::default(),
            DocumentLimits::default(),
            RevisionAttestationLimits::default(),
            ObjectLimits::default(),
            SyntaxLimits::default(),
        ),
    )
    .expect("canonical strict base-open configuration is valid");
    assert_eq!(open_job.phase(), StrictBaseOpenPhase::Xref(XrefPhase::Tail));
    let attested = match open_job.poll(&store, &NeverCancelledDocument) {
        StrictBaseOpenPoll::Ready(index) => index,
        StrictBaseOpenPoll::Pending { .. } => {
            panic!("a fully supplied canonical PDF must not suspend strict base opening")
        }
        StrictBaseOpenPoll::Failed(error) => {
            panic!("canonical strict base revision must open and attest: {error}")
        }
    };
    assert_eq!(open_job.phase(), StrictBaseOpenPhase::Ready);
    let open_stats = open_job.stats();
    assert_eq!(open_stats.xref().entries(), 5);
    let index_stats = open_stats
        .index()
        .expect("successful strict base opening retains candidate-index accounting");
    assert_eq!(index_stats.total_entries(), 5);
    assert_eq!(index_stats.in_use_entries(), 4);
    assert_eq!(open_stats.attestation().objects_attested(), 4);

    assert_eq!(attested.snapshot(), source);
    assert_eq!(attested.revision_id(), RevisionId::new(1));
    assert_eq!(attested.startxref(), STARTXREF);
    assert_eq!(attested.root(), ObjectRef::new(1, 0).unwrap());
    assert_eq!(attested.header().source(), source.identity());
    assert_eq!(
        (
            attested.header().span().start(),
            attested.header().span().end_exclusive(),
            attested.header().value().major(),
            attested.header().value().minor(),
        ),
        (0, 8, 1, 7)
    );
    assert_eq!(attested.index_stats(), index_stats);
    assert_eq!(attested.object_limits(), ObjectLimits::default());
    assert_eq!(attested.syntax_limits(), SyntaxLimits::default());
    let stats = attested.attestation_stats();
    assert_eq!(stats, open_stats.attestation());
    assert_eq!(stats.objects_attested(), 4);
    assert!(stats.trivia_read_bytes() > 0);
    assert!(stats.trivia_scan_bytes() > 0);
    assert!(stats.object_read_bytes() > 0);
    assert!(stats.object_parse_bytes() > 0);
    assert!(stats.retained_evidence_bytes() > 0);

    let page_tree_limits = PageTreeLimits::validate(PageTreeLimitConfig {
        max_nodes: 8,
        max_depth: 4,
        max_pages: 4,
        max_kids_per_node: 4,
        max_total_object_read_bytes: 1 << 20,
        max_total_object_parse_bytes: 1 << 20,
        max_retained_traversal_bytes: 4 << 10,
    })
    .expect("the canonical one-page loop uses a valid compact page-tree profile");
    let mut page_count_job = attested
        .count_pages(
            PageTreeJobContext::new(
                JobId::new(250),
                ResumeCheckpoint::new(251),
                ResumeCheckpoint::new(252),
                RequestPriority::Metadata,
            ),
            page_tree_limits,
        )
        .expect("the attested index mints a bounded strict page-count job");
    assert_eq!(page_count_job.phase(), PageTreePhase::Catalog);
    assert_eq!(page_count_job.stats().objects_started(), 0);
    assert!(page_count_job.stats().reserved_traversal_bytes() > 0);
    let page_count = match page_count_job.poll(&store, &NeverCancelledDocument) {
        PageCountPoll::Ready(count) => count,
        PageCountPoll::Pending { .. } => {
            panic!("a fully supplied canonical PDF must not suspend page counting")
        }
        PageCountPoll::Failed(error) => {
            panic!("canonical strict page tree must count: {error}")
        }
    };
    assert_eq!(page_count_job.phase(), PageTreePhase::Ready);
    assert_eq!(page_count.catalog().snapshot(), source);
    assert_eq!(page_count.catalog().revision_id(), RevisionId::new(1));
    assert_eq!(page_count.catalog().revision_startxref(), STARTXREF);
    assert_eq!(page_count.catalog().root(), ObjectRef::new(1, 0).unwrap());
    assert_eq!(page_count.catalog().pages(), ObjectRef::new(2, 0).unwrap());
    let native_page_count = page_count.page_count();
    assert_eq!(native_page_count, 1);
    let page_tree_stats = page_count.stats();
    assert_eq!(page_tree_stats, page_count_job.stats());
    assert_eq!(page_tree_stats.objects_started(), 3);
    assert_eq!(page_tree_stats.nodes_started(), 2);
    assert_eq!(page_tree_stats.pages(), 1);
    assert_eq!(page_tree_stats.max_depth(), 2);
    assert_eq!(page_tree_stats.max_kids_per_node(), 1);
    assert!(page_tree_stats.object_read_bytes() > 0);
    assert!(page_tree_stats.object_parse_bytes() > 0);
    assert!(page_tree_stats.reserved_traversal_bytes() > 0);
    assert!(
        page_tree_stats.reserved_traversal_bytes()
            <= page_tree_limits.max_retained_traversal_bytes()
    );
    let native_pdfium_page_count_smoke_equal = native_page_count == pdfium_pages_processed;
    assert!(native_pdfium_page_count_smoke_equal);

    let objects = attested.object_attestations();
    for (index, object) in objects.iter().enumerate() {
        let number = u32::try_from(index + 1).unwrap();
        assert_eq!(object.revision_id(), RevisionId::new(1));
        assert_eq!(object.reference(), ObjectRef::new(number, 0).unwrap());
        assert_eq!(object.xref_offset(), OBJECT_OFFSETS[index]);
        assert_eq!(object.object_upper_bound(), OBJECT_UPPER_BOUNDS[index]);
        assert_eq!(
            (
                object.header_span().start(),
                object.header_span().end_exclusive()
            ),
            HEADER_SPANS[index]
        );
        assert_eq!(
            (
                object.object_span().start(),
                object.object_span().end_exclusive()
            ),
            OBJECT_SPANS[index]
        );
        assert_eq!(
            (
                object.endobj_span().start(),
                object.endobj_span().end_exclusive()
            ),
            ENDOBJ_SPANS[index]
        );
    }
    for pair in objects.windows(2) {
        assert!(pair[0].object_span().end_exclusive() <= pair[1].object_span().start());
    }
    assert!(objects[3].object_span().end_exclusive() <= STARTXREF);

    for (index, object) in objects[..3].iter().enumerate() {
        assert_eq!(object.kind(), ObjectAttestationKind::Dictionary);
        assert_eq!(
            attested
                .attestation(ObjectRef::new(u32::try_from(index + 1).unwrap(), 0).unwrap())
                .expect("attested exact reference remains lookup-addressable"),
            object
        );
    }
    let ObjectAttestationKind::Stream { data_span, .. } = objects[3].kind() else {
        panic!("canonical object four must be a stream");
    };
    assert_eq!((data_span.start(), data_span.end_exclusive()), (427, 431));
    assert_eq!(
        attested
            .attestation(ObjectRef::new(4, 0).unwrap())
            .expect("stream evidence remains lookup-addressable"),
        &objects[3]
    );
    let payload_start = usize::try_from(data_span.start()).unwrap();
    let payload_end = usize::try_from(data_span.end_exclusive()).unwrap();
    assert_eq!(&pdf[payload_start..payload_end], b"q\nQ\n");

    let root_reference = ObjectRef::new(1, 0).unwrap();
    let chain_limits = ReferenceChainLimits::default();
    let mut chain_job = attested
        .resolve_reference_chain(
            root_reference,
            ReferenceChainJobContext::new(
                JobId::new(290),
                ResumeCheckpoint::new(291),
                ResumeCheckpoint::new(292),
                RequestPriority::VisiblePage,
            ),
            chain_limits,
        )
        .expect("the attested index mints the bounded canonical root-chain job");
    let resolved_root = match chain_job.poll(&store, &NeverCancelledDocument) {
        ReferenceChainPoll::Ready(resolved) => resolved,
        ReferenceChainPoll::Pending { .. } => {
            panic!("a fully supplied canonical PDF must not suspend root-chain resolution")
        }
        ReferenceChainPoll::Failed(error) => {
            panic!("canonical attested root chain must resolve: {error}")
        }
    };
    assert_eq!(resolved_root.root(), root_reference);
    assert_eq!(resolved_root.terminal(), root_reference);
    assert_eq!(resolved_root.chain().root(), root_reference);
    assert_eq!(resolved_root.chain().terminal(), root_reference);
    assert_eq!(resolved_root.chain().len(), 1);
    assert!(resolved_root.chain().prefix().is_empty());
    assert_eq!(resolved_root.object().attestation(), &objects[0]);
    assert_eq!(resolved_root.object().snapshot(), source);
    let chain_stats = chain_job.stats();
    assert_eq!(resolved_root.stats(), chain_stats);
    assert_eq!(chain_stats.objects_started(), 1);
    assert_eq!(chain_stats.reference_edges(), 0);
    assert_eq!(chain_stats.max_depth(), 1);
    assert!(chain_stats.object_read_bytes() > 0);
    assert!(chain_stats.object_parse_bytes() > 0);
    assert!(chain_stats.retained_path_bytes() > 0);
    assert!(chain_stats.retained_path_bytes() <= chain_limits.max_retained_path_bytes());
    let resolved_footprint = resolved_root
        .try_resident_footprint()
        .expect("canonical resolved-root footprint fits u64");
    assert_eq!(
        resolved_footprint.inline_bytes(),
        u64::try_from(std::mem::size_of_val(&resolved_root)).unwrap()
    );
    assert_eq!(
        resolved_footprint.syntax_heap_bytes(),
        resolved_root.object().syntax_heap_bytes()
    );
    assert!(resolved_footprint.syntax_heap_bytes() > 0);
    assert_eq!(
        resolved_footprint.chain_capacity_bytes(),
        chain_stats.retained_path_bytes()
    );
    assert!(resolved_footprint.chain_capacity_bytes() > 0);
    assert_eq!(
        resolved_footprint.total_bytes(),
        resolved_footprint
            .inline_bytes()
            .checked_add(resolved_footprint.syntax_heap_bytes())
            .and_then(|value| value.checked_add(resolved_footprint.chain_capacity_bytes()))
            .unwrap()
    );
    let ready_binding =
        ReadyStoreBinding::for_index(&attested, READY_STORE_SESSION_ID, READY_STORE_EPOCH);
    let ready_key = ReadyStoreKey::new(ready_binding, root_reference, chain_limits);
    let mut ready_owner = ReadySessionOwner::new(ready_binding, ReadyStoreLimits::default())
        .expect("canonical session Ready-store metadata fits its owner budget");
    assert_eq!(ready_owner.session_id(), READY_STORE_SESSION_ID);
    assert_eq!(ready_owner.binding().unwrap(), ready_binding);
    assert_eq!(ready_owner.phase(), ReadySessionPhase::Ready);
    let initial_stats = ready_owner.stats().unwrap();
    let metadata_baseline = initial_stats.metadata_bytes();
    assert!(metadata_baseline > 0);
    assert_eq!(initial_stats.entries(), 0);
    assert_eq!(initial_stats.value_heap_bytes(), 0);
    assert_eq!(initial_stats.resident_bytes(), metadata_baseline);

    let expected_ready_heap = resolved_footprint
        .syntax_heap_bytes()
        .checked_add(resolved_footprint.chain_capacity_bytes())
        .unwrap();
    let admitted = match ready_owner
        .try_admit(ready_key, resolved_root, &NeverCancelledDocument)
        .expect("canonical Ready admission must not fail")
    {
        ReadyAdmission::Admitted(admitted) => admitted,
        ReadyAdmission::Rejected(rejected) => {
            panic!(
                "canonical resolved root must be retained: {:?}",
                rejected.reason()
            )
        }
    };
    assert!(!admitted.replaced());
    assert_eq!(admitted.evicted(), 0);
    let admitted_stats = ready_owner.stats().unwrap();
    assert_eq!(admitted_stats.entries(), 1);
    assert_eq!(admitted_stats.admissions(), 1);
    assert_eq!(admitted_stats.replacements(), 0);
    assert_eq!(admitted_stats.evictions(), 0);
    assert_eq!(admitted_stats.value_heap_bytes(), expected_ready_heap);
    assert_eq!(
        admitted_stats.resident_bytes(),
        metadata_baseline.checked_add(expected_ready_heap).unwrap()
    );
    assert_eq!(
        admitted_stats.peak_resident_bytes(),
        admitted_stats.resident_bytes()
    );
    let ready_store_admitted_resident_bytes = admitted_stats.resident_bytes();

    {
        let cached_root = match ready_owner
            .lookup(ready_key, &NeverCancelledDocument)
            .expect("canonical exact Ready lookup must not fail")
        {
            ReadyLookup::Hit(value) => value,
            ReadyLookup::Miss(reason) => {
                panic!("canonical exact Ready key must hit: {reason:?}")
            }
        };
        assert_eq!(cached_root.root(), root_reference);
        assert_eq!(cached_root.terminal(), root_reference);
        assert_eq!(cached_root.limits(), chain_limits);
        assert_eq!(cached_root.object().attestation(), &objects[0]);
        let resolved_catalog = direct_dictionary(cached_root.object(), source);
        assert!(matches!(
            dictionary_value(resolved_catalog, b"Type"),
            SyntaxObject::Name(name) if name.bytes() == b"Catalog"
        ));
        assert_eq!(
            dictionary_value(resolved_catalog, b"Pages").as_reference(),
            Some(ObjectRef::new(2, 0).unwrap())
        );
    }
    let hit_stats = ready_owner.stats().unwrap();
    assert_eq!(hit_stats.hits(), 1);
    assert_eq!(hit_stats.misses(), 0);
    assert_eq!(
        hit_stats.resident_bytes(),
        ready_store_admitted_resident_bytes
    );
    let close_report = ready_owner.close();
    assert_eq!(close_report.session_id(), READY_STORE_SESSION_ID);
    assert_eq!(close_report.released_entries(), 1);
    assert_eq!(close_report.released_metadata_bytes(), metadata_baseline);
    assert_eq!(
        close_report.released_value_heap_bytes(),
        expected_ready_heap
    );
    assert_eq!(
        close_report.released_resident_bytes(),
        ready_store_admitted_resident_bytes
    );
    assert_eq!(
        close_report.peak_resident_bytes(),
        ready_store_admitted_resident_bytes
    );
    assert_eq!(ready_owner.phase(), ReadySessionPhase::Closed);
    assert_eq!(ready_owner.close_report(), Some(close_report));
    assert_eq!(ready_owner.resources().entries(), 0);
    assert_eq!(ready_owner.resources().metadata_bytes(), 0);
    assert_eq!(ready_owner.resources().value_heap_bytes(), 0);
    assert_eq!(ready_owner.resources().resident_bytes(), 0);
    assert_eq!(ready_owner.close(), close_report);
    let ready_store_released_resident_bytes = close_report.released_resident_bytes();

    let access_caps = ObjectWorkCaps::new(
        attested.object_limits().max_total_read_bytes(),
        attested.object_limits().max_total_parse_bytes(),
    )
    .expect("the retained object profile yields valid full per-access caps");
    let mut reopened = Vec::with_capacity(OBJECT_OFFSETS.len());
    for number in 1_u32..=4 {
        let numeric = u64::from(number);
        let reference = ObjectRef::new(number, 0).unwrap();
        let mut access = attested
            .open_object(
                reference,
                AttestedObjectJobContext::new(
                    JobId::new(300 + numeric),
                    ResumeCheckpoint::new(400 + numeric * 2),
                    ResumeCheckpoint::new(401 + numeric * 2),
                    RequestPriority::VisiblePage,
                ),
                access_caps,
            )
            .expect("only the attested index mints a proof-preserving object access job");
        let object = match access.poll(&store, &NeverCancelledDocument) {
            AttestedObjectPoll::Ready(object) => object,
            AttestedObjectPoll::Pending { .. } => {
                panic!("a fully supplied canonical PDF must not suspend attested object access")
            }
            AttestedObjectPoll::Failed(error) => {
                panic!("canonical attested object must reopen: {error}")
            }
        };
        assert_eq!(
            object.attestation(),
            &objects[usize::try_from(number - 1).unwrap()]
        );
        assert_eq!(object.snapshot(), source);
        assert_eq!(object.revision_id(), RevisionId::new(1));
        assert_eq!(object.revision_startxref(), STARTXREF);
        assert_eq!(object.reference(), reference);
        let footprint = object
            .try_resident_footprint()
            .expect("canonical reopened-object footprint fits u64");
        assert_eq!(
            footprint.inline_bytes(),
            u64::try_from(std::mem::size_of_val(&object)).unwrap()
        );
        assert_eq!(footprint.syntax_heap_bytes(), object.syntax_heap_bytes());
        assert!(footprint.syntax_heap_bytes() > 0);
        assert_eq!(footprint.chain_capacity_bytes(), 0);
        assert_eq!(
            footprint.total_bytes(),
            footprint
                .inline_bytes()
                .checked_add(footprint.syntax_heap_bytes())
                .unwrap()
        );
        reopened.push(object);
    }
    let catalog = direct_dictionary(&reopened[0], source);
    assert_eq!(catalog.entries().len(), 2);
    assert!(matches!(
        dictionary_value(catalog, b"Type"),
        SyntaxObject::Name(name) if name.bytes() == b"Catalog"
    ));
    assert_eq!(
        dictionary_value(catalog, b"Pages").as_reference(),
        Some(ObjectRef::new(2, 0).unwrap())
    );

    let pages = direct_dictionary(&reopened[1], source);
    assert_eq!(pages.entries().len(), 3);
    assert!(matches!(
        dictionary_value(pages, b"Type"),
        SyntaxObject::Name(name) if name.bytes() == b"Pages"
    ));
    let SyntaxObject::Array(kids) = dictionary_value(pages, b"Kids") else {
        panic!("canonical Pages /Kids must be an array")
    };
    assert_eq!(kids.values().len(), 1);
    assert_eq!(
        kids.values()[0].value().as_reference(),
        Some(ObjectRef::new(3, 0).unwrap())
    );
    assert_eq!(dictionary_value(pages, b"Count").as_integer(), Some(1));

    let page = direct_dictionary(&reopened[2], source);
    assert_eq!(page.entries().len(), 5);
    assert!(matches!(
        dictionary_value(page, b"Type"),
        SyntaxObject::Name(name) if name.bytes() == b"Page"
    ));
    assert_eq!(
        dictionary_value(page, b"Parent").as_reference(),
        Some(ObjectRef::new(2, 0).unwrap())
    );
    let SyntaxObject::Array(media_box) = dictionary_value(page, b"MediaBox") else {
        panic!("canonical Page /MediaBox must be an array")
    };
    assert_eq!(media_box.values().len(), 4);
    for (value, expected) in media_box.values().iter().zip([0, 0, 200, 200]) {
        assert_eq!(value.value().as_integer(), Some(expected));
    }
    assert!(matches!(
        dictionary_value(page, b"Resources"),
        SyntaxObject::Dictionary(resources) if resources.entries().is_empty()
    ));
    assert_eq!(
        dictionary_value(page, b"Contents").as_reference(),
        Some(ObjectRef::new(4, 0).unwrap())
    );

    let IndirectObjectValue::Stream(reopened_stream) = reopened[3].value() else {
        panic!("reopened canonical object four must remain a stream")
    };
    assert_eq!(reopened_stream.dictionary().source(), source.identity());
    assert_eq!(reopened_stream.dictionary().value().entries().len(), 1);
    assert_eq!(
        dictionary_value(reopened_stream.dictionary().value(), b"Length").as_integer(),
        Some(4)
    );
    assert_eq!(reopened_stream.data_span(), data_span);
    let reopened_payload_start = usize::try_from(reopened_stream.data_span().start()).unwrap();
    let reopened_payload_end =
        usize::try_from(reopened_stream.data_span().end_exclusive()).unwrap();
    assert_eq!(
        &pdf[reopened_payload_start..reopened_payload_end],
        b"q\nQ\n"
    );

    println!(
        "native_object_loop_result sha256={PDF_SHA256} bytes={PDF_BYTES} startxref={STARTXREF} trailer=566..591 offsets=186,235,292,396 upper_bounds=235,292,396,449 objects=dictionary,dictionary,dictionary,stream payload4=427..431:710a510a strict_base_open_composed=true strict_base_revision_attested=true attested_object_access=true strict_catalog_validated=true strict_page_tree_counted=true native_page_count={native_page_count} page_tree_objects_started={} page_tree_nodes_started={} page_tree_max_depth={} page_tree_reserved_traversal_bytes={} reference_chain_resolved=true resident_footprint_accounted=true session_ready_store_admitted=true session_ready_store_borrowed_hit=true session_ready_owner_closed=true session_ready_owner_resources_zero=true session_ready_owner_close_idempotent=true ready_store_metadata_bytes={metadata_baseline} ready_store_admitted_resident_bytes={ready_store_admitted_resident_bytes} ready_store_released_resident_bytes={ready_store_released_resident_bytes} pdfium_build_readiness_pageinfo_pages_processed={pdfium_pages_processed} native_pdfium_page_count_smoke_equal={native_pdfium_page_count_smoke_equal} pdfium_o4_same_input=true pdfium_o4_vs_analytic_different_pixels=0 native_pdfium_differential=false",
        page_tree_stats.objects_started(),
        page_tree_stats.nodes_started(),
        page_tree_stats.max_depth(),
        page_tree_stats.reserved_traversal_bytes(),
    );
}

#[test]
fn native_object_loop_traceability_is_explicit_and_non_differential() {
    assert_eq!(top_level_version(FEATURE_MAP), Some("0.53.0"));
    assert_eq!(top_level_version(SPEC_MAP), Some("0.53.0"));

    let feature = record_with_id(FEATURE_MAP, "feature", "quality.native-object-loop")
        .expect("the Native object-loop feature record must exist");
    assert!(feature.contains("owner = \"quality-corpus\""));
    assert!(feature.contains("state = \"PLANNED\""));
    assert!(feature.contains("profile = \"m1.native-object-loop.v1\""));
    assert!(feature.contains("clauses = [\"RPE-ARCH-001/12.6\", \"RPE-ARCH-001/15.3/M0\"]"));
    assert!(feature.contains("modules = [\"tools/quality\"]"));
    assert!(feature.contains("tests = [\"tools/quality::native_object_loop\"]"));
    assert!(feature.contains("fuzz_targets = []"));
    assert!(feature.contains("benchmarks = []"));

    let range_feature = record_with_id(FEATURE_MAP, "feature", "quality.native-range-resume-loop")
        .expect("the Native Range-resume-loop feature record must exist");
    assert!(range_feature.contains("owner = \"quality-corpus\""));
    assert!(range_feature.contains("state = \"PLANNED\""));
    assert!(range_feature.contains("profile = \"m1.native-range-resume-loop.v1\""));
    assert!(range_feature.contains("RPE-ARCH-001/15.3/M1"));
    assert!(range_feature.contains("modules = [\"tools/quality\"]"));
    assert!(range_feature.contains("tools/quality::native_range_resume_loop"));
    assert!(range_feature.contains("fuzz_targets = []"));
    assert!(range_feature.contains("benchmarks = []"));

    let strict_runtime_feature = record_with_id(
        FEATURE_MAP,
        "feature",
        "quality.native-strict-open-runtime-loop",
    )
    .expect("the Native strict-open runtime-loop feature record must exist");
    assert!(strict_runtime_feature.contains("owner = \"quality-corpus\""));
    assert!(strict_runtime_feature.contains("state = \"PLANNED\""));
    assert!(strict_runtime_feature.contains("profile = \"m1.native-strict-open-runtime-loop.v1\""));
    for clause in [
        "RPE-ARCH-001/5.1-5.2",
        "RPE-ARCH-001/5.4",
        "RPE-ARCH-001/12.6",
        "RPE-ARCH-001/14.2",
        "RPE-ARCH-001/15.3/M1",
    ] {
        assert!(strict_runtime_feature.contains(clause));
    }
    assert!(strict_runtime_feature.contains("modules = [\"tools/quality\"]"));
    assert!(
        strict_runtime_feature
            .contains("tests = [\"tools/quality::native_strict_open_runtime_loop\"]")
    );
    assert!(strict_runtime_feature.contains("fuzz_targets = []"));
    assert!(strict_runtime_feature.contains("benchmarks = []"));

    let candidate_feature =
        record_with_id(FEATURE_MAP, "feature", "core.base-revision-candidate-index")
            .expect("the candidate revision-index feature record must exist");
    assert!(candidate_feature.contains("owner = \"parser-security\""));
    assert!(candidate_feature.contains("state = \"PLANNED\""));
    assert!(candidate_feature.contains("profile = \"m1.strict-base-revision-index.v1\""));
    assert!(candidate_feature.contains("modules = [\"core/document\"]"));
    assert!(candidate_feature.contains("core/document::revision_index"));
    assert!(candidate_feature.contains("tools/quality::native_object_loop"));
    assert!(candidate_feature.contains("fuzz_targets = []"));
    assert!(candidate_feature.contains("benchmarks = []"));

    let attestation_feature = record_with_id(
        FEATURE_MAP,
        "feature",
        "core.strict-base-revision-attestation",
    )
    .expect("the strict base-revision attestation feature record must exist");
    assert!(attestation_feature.contains("owner = \"parser-security\""));
    assert!(attestation_feature.contains("state = \"PLANNED\""));
    assert!(attestation_feature.contains("profile = \"m1.strict-base-revision-attestation.v1\""));
    assert!(attestation_feature.contains("modules = [\"core/document\"]"));
    assert!(attestation_feature.contains("core/document::revision_attestation"));
    assert!(attestation_feature.contains("core/document::revision_attestation_limit_config"));
    assert!(attestation_feature.contains("tools/quality::native_object_loop"));
    assert!(attestation_feature.contains("fuzz_targets = []"));
    assert!(attestation_feature.contains("benchmarks = []"));

    let strict_open_feature = record_with_id(FEATURE_MAP, "feature", "core.strict-base-open")
        .expect("the strict base-open feature record must exist");
    assert!(strict_open_feature.contains("owner = \"parser-security\""));
    assert!(strict_open_feature.contains("state = \"PLANNED\""));
    assert!(strict_open_feature.contains("profile = \"m1.strict-base-open.v1\""));
    assert!(strict_open_feature.contains("modules = [\"core/document\"]"));
    assert!(strict_open_feature.contains("core/document::strict_base_open"));
    assert!(strict_open_feature.contains("core/document::repository_policy"));
    assert!(strict_open_feature.contains("tools/quality::native_object_loop"));
    assert!(strict_open_feature.contains("tools/quality::native_range_resume_loop"));
    assert!(strict_open_feature.contains("tools/quality::native_strict_open_runtime_loop"));
    assert!(strict_open_feature.contains("fuzz_targets = []"));
    assert!(strict_open_feature.contains("benchmarks = []"));

    let access_feature = record_with_id(FEATURE_MAP, "feature", "core.attested-object-access")
        .expect("the proof-preserving object-access feature record must exist");
    assert!(access_feature.contains("owner = \"parser-security\""));
    assert!(access_feature.contains("state = \"PLANNED\""));
    assert!(access_feature.contains("profile = \"m1.attested-object-access.v1\""));
    assert!(access_feature.contains("modules = [\"core/document\"]"));
    assert!(access_feature.contains("core/document::attested_object_access"));
    assert!(access_feature.contains("tools/quality::native_object_loop"));
    assert!(access_feature.contains("fuzz_targets = []"));
    assert!(access_feature.contains("benchmarks = []"));

    let chain_feature = record_with_id(
        FEATURE_MAP,
        "feature",
        "core.attested-reference-chain-resolution",
    )
    .expect("the bounded attested reference-chain feature record must exist");
    assert!(chain_feature.contains("owner = \"parser-security\""));
    assert!(chain_feature.contains("state = \"PLANNED\""));
    assert!(chain_feature.contains("profile = \"m1.attested-reference-chain.v1\""));
    assert!(chain_feature.contains("modules = [\"core/document\"]"));
    assert!(chain_feature.contains("core/document::reference_chain_resolution"));
    assert!(chain_feature.contains("core/document::reference_chain_limit_config"));
    assert!(chain_feature.contains("tools/quality::native_object_loop"));
    assert!(chain_feature.contains("fuzz_targets = []"));
    assert!(chain_feature.contains("benchmarks = []"));

    let resident_feature =
        record_with_id(FEATURE_MAP, "feature", "core.attested-resident-footprint")
            .expect("the attested resident-footprint feature record must exist");
    assert!(resident_feature.contains("owner = \"parser-security\""));
    assert!(resident_feature.contains("state = \"PLANNED\""));
    assert!(resident_feature.contains("profile = \"m1.attested-resident-footprint.v1\""));
    assert!(
        resident_feature
            .contains("modules = [\"core/syntax\", \"core/object\", \"core/document\"]")
    );
    assert!(resident_feature.contains("core/syntax::parser_behavior"));
    assert!(resident_feature.contains("core/object::object_behavior"));
    assert!(resident_feature.contains("core/document::attested_object_access"));
    assert!(resident_feature.contains("core/document::reference_chain_resolution"));
    assert!(resident_feature.contains("tools/quality::native_object_loop"));
    assert!(resident_feature.contains("fuzz_targets = []"));
    assert!(resident_feature.contains("benchmarks = []"));

    let page_count_feature = record_with_id(FEATURE_MAP, "feature", "core.strict-page-count")
        .expect("the strict page-count feature record must exist");
    assert!(page_count_feature.contains("owner = \"parser-security\""));
    assert!(page_count_feature.contains("state = \"PLANNED\""));
    assert!(page_count_feature.contains("profile = \"m1.strict-page-count.v1\""));
    assert!(page_count_feature.contains("RPE-ARCH-001/5.8-5.9"));
    assert!(page_count_feature.contains("modules = [\"core/document\"]"));
    assert!(page_count_feature.contains("core/document::page_tree_count"));
    assert!(page_count_feature.contains("core/document::page_tree_limit_config"));
    assert!(page_count_feature.contains("core/document::repository_policy"));
    assert!(page_count_feature.contains("tools/baseline::pdfium_page_count_real_adapter"));
    assert!(page_count_feature.contains("tools/baseline::repository_pdfium_page_count_probe"));
    assert!(page_count_feature.contains("tools/quality::native_object_loop"));
    assert!(page_count_feature.contains("fuzz_targets = []"));
    assert!(page_count_feature.contains("benchmarks = []"));

    let ready_store_feature = record_with_id(FEATURE_MAP, "feature", "runtime.session-ready-store")
        .expect("the session Ready-store feature record must exist");
    assert!(ready_store_feature.contains("owner = \"runtime-platform\""));
    assert!(ready_store_feature.contains("state = \"PLANNED\""));
    assert!(ready_store_feature.contains("profile = \"m1.session-ready-store.v1\""));
    assert!(ready_store_feature.contains("modules = [\"runtime/cache\"]"));
    assert!(ready_store_feature.contains("runtime/cache::ready_store"));
    assert!(ready_store_feature.contains("runtime/cache::repository_policy"));
    assert!(ready_store_feature.contains("tools/quality::native_object_loop"));
    assert!(ready_store_feature.contains("fuzz_targets = []"));
    assert!(ready_store_feature.contains("benchmarks = []"));

    let ready_owner_feature = record_with_id(FEATURE_MAP, "feature", "runtime.ready-session-owner")
        .expect("the Ready-session owner feature record must exist");
    assert!(ready_owner_feature.contains("owner = \"runtime-platform\""));
    assert!(ready_owner_feature.contains("state = \"PLANNED\""));
    assert!(ready_owner_feature.contains("profile = \"m1.ready-session-owner.v1\""));
    assert!(ready_owner_feature.contains("modules = [\"runtime/session\"]"));
    assert!(ready_owner_feature.contains("runtime/session::ready_owner"));
    assert!(ready_owner_feature.contains("runtime/session::repository_policy"));
    assert!(ready_owner_feature.contains("tools/quality::native_object_loop"));
    assert!(ready_owner_feature.contains("fuzz_targets = []"));
    assert!(ready_owner_feature.contains("benchmarks = []"));

    let coordinator_feature = record_with_id(
        FEATURE_MAP,
        "feature",
        "runtime.strict-base-open-coordinator",
    )
    .expect("the strict-base open coordinator feature record must exist");
    assert!(coordinator_feature.contains("owner = \"runtime-platform\""));
    assert!(coordinator_feature.contains("state = \"PLANNED\""));
    assert!(coordinator_feature.contains("profile = \"m1.strict-base-open-coordinator.v1\""));
    for clause in [
        "RPE-ARCH-001/5.1-5.2",
        "RPE-ARCH-001/5.4",
        "RPE-ARCH-001/14.2",
        "RPE-ARCH-001/15.3/M1",
        "RPE-STD-002/5-7",
        "RPE-STD-005/5",
        "RPE-STD-005/8",
    ] {
        assert!(coordinator_feature.contains(clause));
    }
    assert!(coordinator_feature.contains("modules = [\"runtime/session\"]"));
    assert!(coordinator_feature.contains("runtime/session::strict_base_open_coordinator"));
    assert!(coordinator_feature.contains("runtime/session::repository_policy"));
    assert!(coordinator_feature.contains("tools/quality::native_strict_open_runtime_loop"));
    assert!(coordinator_feature.contains("fuzz_targets = []"));
    assert!(coordinator_feature.contains("benchmarks = []"));

    for requirement_id in [
        "RPE-ARCH-001/5.3",
        "RPE-ARCH-001/5.4",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/9.1",
        "RPE-ARCH-001/14.2",
        "RPE-ARCH-001/12.6",
        "RPE-ARCH-001/15.3/M1",
    ] {
        let requirement = record_with_id(SPEC_MAP, "requirement", requirement_id)
            .unwrap_or_else(|| panic!("requirement {requirement_id} must exist"));
        assert!(requirement.contains("tools/quality::native_object_loop"));
        assert!(requirement.contains("status = \"partial\""));
    }

    let generator_requirement = record_with_id(SPEC_MAP, "requirement", "RPE-ARCH-001/12.6")
        .expect("the fixture-generator requirement must exist");
    assert!(generator_requirement.contains("\"quality.native-object-loop\""));
    assert!(generator_requirement.contains("\"tools/quality\""));

    let m0_requirement = record_with_id(SPEC_MAP, "requirement", "RPE-ARCH-001/15.3/M0")
        .expect("the M0 quality-infrastructure requirement must exist");
    assert!(m0_requirement.contains("status = \"covered\""));
    assert!(m0_requirement.contains("\"quality.native-object-loop\""));
    assert!(m0_requirement.contains("formal-strict-base-open"));
    assert!(m0_requirement.contains("ReadySessionOwner"));
    assert!(m0_requirement.contains("page_count=1"));
    assert!(m0_requirement.contains("pages_processed=1"));
    assert!(m0_requirement.contains("rather than a registered page-count differential"));
    assert!(m0_requirement.contains("pdfium_page_count_real_adapter"));
    assert!(m0_requirement.contains("repository_pdfium_page_count_probe"));
    assert!(
        m0_requirement
            .contains("valid one-page and nested three-page fixtures are exact and repeatable")
    );
    assert!(m0_requirement.contains("RPE-DOCUMENT-0033"));
    assert!(m0_requirement.contains("PDFium page_count=4"));
    assert!(m0_requirement.contains("expected strictness difference"));
    assert!(m0_requirement.contains("feature states remain PLANNED"));
    assert!(m0_requirement.contains("M1 exit is not claimed complete"));
    assert!(m0_requirement.contains("synchronously close the store"));
    assert!(m0_requirement.contains("zero post-close resources"));
    assert!(m0_requirement.contains("broad corpus and pixel differential evidence"));

    let page_tree_requirement = record_with_id(SPEC_MAP, "requirement", "RPE-ARCH-001/5.8-5.9")
        .expect("the document-model and page-tree requirement must exist");
    assert!(page_tree_requirement.contains("\"core.strict-page-count\""));
    assert!(page_tree_requirement.contains("core/document::page_tree_count"));
    assert!(page_tree_requirement.contains("tools/baseline::pdfium_page_count_real_adapter"));
    assert!(page_tree_requirement.contains("tools/baseline::repository_pdfium_page_count_probe"));
    assert!(page_tree_requirement.contains("tools/quality::native_object_loop"));
    assert!(page_tree_requirement.contains("open-addressing table"));
    assert!(
        page_tree_requirement
            .contains("valid one-page and nested three-page fixtures match exactly")
    );
    assert!(page_tree_requirement.contains("RPE-DOCUMENT-0033"));
    assert!(page_tree_requirement.contains("PDFium page_count=4"));
    assert!(page_tree_requirement.contains("expected strictness difference"));
    assert!(page_tree_requirement.contains("page_count=1"));
    assert!(page_tree_requirement.contains("pages_processed=1"));
    assert!(page_tree_requirement.contains("non-gating smoke observation"));
    assert!(page_tree_requirement.contains("not a registered page-count differential"));
    assert!(page_tree_requirement.contains("feature state remains PLANNED"));
    assert!(page_tree_requirement.contains("do not claim M1 or M2 exit"));

    let xref_requirement = record_with_id(SPEC_MAP, "requirement", "RPE-ARCH-001/5.4")
        .expect("the xref architecture requirement must exist");
    assert!(xref_requirement.contains("\"core.base-revision-candidate-index\""));
    assert!(xref_requirement.contains("\"core.strict-base-revision-attestation\""));
    assert!(xref_requirement.contains("\"core.attested-object-access\""));
    assert!(xref_requirement.contains("\"core.attested-reference-chain-resolution\""));
    assert!(xref_requirement.contains("\"core.attested-resident-footprint\""));
    assert!(xref_requirement.contains("\"runtime.strict-base-open-coordinator\""));
    assert!(xref_requirement.contains("\"core/document\""));
    assert!(xref_requirement.contains("runtime/session::strict_base_open_coordinator"));
    assert!(xref_requirement.contains("tools/quality::native_strict_open_runtime_loop"));
    assert!(xref_requirement.contains("header-to-startxref"));
    assert!(xref_requirement.contains("line-terminated comments"));
    assert!(xref_requirement.contains("caller-lent work cap"));
    assert!(xref_requirement.contains("one-shot reopen jobs under retained profiles"));
    assert!(xref_requirement.contains("follows only top-level whole-object aliases"));
    assert!(xref_requirement.contains("exact cycle chains and aggregate limits"));
    assert!(xref_requirement.contains("general graph traversal"));
    assert!(xref_requirement.contains("value-owned footprint evidence"));
    assert!(xref_requirement.contains("for later cache admission"));
    assert!(xref_requirement.contains("payload containment"));
    assert!(xref_requirement.contains("persistent reuse and coalescing"));
    assert!(xref_requirement.contains("parent budget hierarchy"));
    assert!(xref_requirement.contains("makes public run_one the only parser entry"));
    assert!(xref_requirement.contains("queued resume or failure completion"));
    assert!(xref_requirement.contains("Host ingress never polls"));
    assert!(xref_requirement.contains("failure completion without parser or cancellation polling"));
    assert!(xref_requirement.contains("opaque move-only handoff"));
    assert!(xref_requirement.contains("same private source owner"));
    assert!(
        xref_requirement.contains("generic multi-job scheduler and complete Session lifecycle")
    );
    assert!(xref_requirement.contains("Native/PDFium semantic or pixel differential"));
    assert!(xref_requirement.contains("does not claim M1 exit"));

    let byte_access_requirement = record_with_id(SPEC_MAP, "requirement", "RPE-ARCH-001/5.1-5.2")
        .expect("the byte-access architecture requirement must exist");
    for required in [
        "runtime.strict-base-open-coordinator",
        "runtime/session::strict_base_open_coordinator",
        "runtime/session::repository_policy",
        "tools/quality::native_strict_open_runtime_loop",
        "Host supply",
        "snapshot observation",
        "ticket failure",
        "never invoke parser code inline",
        "public run_one method is the only parser entry",
        "every host ingress as queue-only work",
        "without polling the parser or probing cancellation",
        "opaque move-only handoff",
        "same private source owner",
        "generic multi-job scheduler",
        "complete Session/request/Worker ownership",
        "does not claim M1 exit",
    ] {
        assert!(
            byte_access_requirement.contains(required),
            "byte-access mapping must contain {required:?}"
        );
    }

    let lifecycle_requirement = record_with_id(SPEC_MAP, "requirement", "RPE-ARCH-001/14.2")
        .expect("the lifecycle architecture requirement must exist");
    for required in [
        "runtime.strict-base-open-coordinator",
        "runtime/session::strict_base_open_coordinator",
        "runtime/session::repository_policy",
        "tools/quality::native_strict_open_runtime_loop",
        "Public run_one is its only parser entry",
        "Host supply, snapshot observation, and failure ingress only queue work",
        "a failure turn does not poll the parser or probe cancellation",
        "opaque move-only handoff",
        "same private source owner",
        "not one complete Session",
        "generic job queue and scheduler",
    ] {
        assert!(
            lifecycle_requirement.contains(required),
            "lifecycle mapping must contain {required:?}"
        );
    }

    let m1_requirement = record_with_id(SPEC_MAP, "requirement", "RPE-ARCH-001/15.3/M1")
        .expect("the M1 architecture requirement must exist");
    for required in [
        "runtime.strict-base-open-coordinator",
        "runtime/session::strict_base_open_coordinator",
        "runtime/session::repository_policy",
        "tools/quality::native_strict_open_runtime_loop",
        "one-job strict-open coordinator",
        "Coordinator public run_one is the only parser entry",
        "host ingress only mutates Range state and may queue completion",
        "never polls parser code",
        "later exclusive actor turn",
        "consumes one exact failure completion",
        "without a parser poll or cancellation probe",
        "opaque move-only handoff",
        "same private source owner",
        "coordinator then reports zero resources",
        "consuming close returns exact owner-release evidence",
        "not a complete Session",
        "generic multi-job scheduler",
        "does not claim M1 exit",
    ] {
        assert!(
            m1_requirement.contains(required),
            "M1 mapping must contain {required:?}"
        );
    }
    for required in [
        "The sibling direct lower-owner path",
        "arbiter-bound move-only dispatch",
        "exact issuer/ticket/job/checkpoint/generation validation",
        "stale-generation rejection without parser work",
    ] {
        assert!(
            m1_requirement.contains(required),
            "M1 direct-owner evidence must contain {required:?}"
        );
    }
}
