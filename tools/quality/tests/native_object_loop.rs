use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_document::{
    AttestRevisionJob, AttestedObject, AttestedObjectJobContext, AttestedObjectPoll,
    CandidateRevisionIndex, DocumentLimits, NeverCancelled as NeverCancelledDocument,
    ObjectAttestationKind, ReferenceChainJobContext, ReferenceChainLimits, ReferenceChainPoll,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_generate::generate_one_page_pdf;
use pdf_rs_object::{IndirectObjectValue, ObjectLimits, ObjectWorkCaps};
use pdf_rs_syntax::{ObjectRef, PdfDictionary, SyntaxLimits, SyntaxObject};
use pdf_rs_xref::{OpenXrefJob, XrefEntryKind, XrefJobContext, XrefLimits, XrefPoll, XrefSection};

const PDF_BYTES: u64 = 612;
const PDF_SHA256: &str = "9c819e549afcc89d03b380c3c1bd47128aa2b70ae30a35245e6a0e30132875db";
const SOURCE_STABLE_ID: [u8; 32] = [0x6e; 32];
const PDFIUM_O4_EVIDENCE: &str = include_str!(
    "../../baseline/pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1.toml"
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

fn parse_xref(store: &RangeStore) -> XrefSection {
    let mut job = OpenXrefJob::new(
        store.snapshot(),
        XrefJobContext::new(
            JobId::new(1),
            ResumeCheckpoint::new(10),
            ResumeCheckpoint::new(11),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .expect("canonical xref job configuration is valid");

    match job.poll(store, &pdf_rs_xref::NeverCancelled) {
        XrefPoll::Ready(section) => section,
        XrefPoll::Pending { .. } => panic!("a fully supplied canonical PDF must not suspend xref"),
        XrefPoll::Failed(error) => panic!("canonical xref must parse: {error}"),
    }
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

    let source = snapshot(output_sha256);
    let store = RangeStore::new(source, Default::default()).expect("range store is valid");
    let complete_range = ByteRange::new(0, PDF_BYTES).expect("canonical range is valid");
    store
        .supply(
            RangeResponse::new(source, complete_range, pdf.clone())
                .expect("canonical response is snapshot-bound"),
        )
        .expect("canonical bytes fit the range store limits");

    let section = parse_xref(&store);
    assert_eq!(section.snapshot(), source);
    assert_eq!(section.startxref(), STARTXREF);
    assert_eq!(section.span().start(), STARTXREF);
    assert_eq!(section.span().end_exclusive(), 591);
    assert_eq!(section.declared_size(), 5);
    assert_eq!(
        (section.root().number(), section.root().generation()),
        (1, 0)
    );
    assert_eq!(
        (
            section.trailer().span().start(),
            section.trailer().span().end_exclusive()
        ),
        (566, 591)
    );

    let in_use_offsets = section
        .entries()
        .iter()
        .filter_map(|entry| match entry.kind() {
            XrefEntryKind::Free { .. } => None,
            XrefEntryKind::InUse { offset } => Some(offset),
        })
        .collect::<Vec<_>>();
    assert_eq!(in_use_offsets, OBJECT_OFFSETS);

    let candidate_index = CandidateRevisionIndex::from_xref(
        &section,
        RevisionId::new(1),
        DocumentLimits::default(),
        &NeverCancelledDocument,
    )
    .expect("canonical xref metadata yields a bounded candidate revision index");
    assert_eq!(candidate_index.snapshot(), source);
    assert_eq!(candidate_index.revision_id(), RevisionId::new(1));
    assert_eq!(candidate_index.startxref(), STARTXREF);
    assert_eq!(candidate_index.root(), ObjectRef::new(1, 0).unwrap());
    assert_eq!(candidate_index.stats().total_entries(), 5);
    assert_eq!(candidate_index.stats().in_use_entries(), 4);
    assert_eq!(candidate_index.physical_intervals().len(), 4);
    for (index, interval) in candidate_index.physical_intervals().iter().enumerate() {
        let number = u32::try_from(index + 1).unwrap();
        assert_eq!(interval.revision_id(), RevisionId::new(1));
        assert_eq!(interval.reference(), ObjectRef::new(number, 0).unwrap());
        assert_eq!(interval.xref_offset(), OBJECT_OFFSETS[index]);
        assert_eq!(interval.object_upper_bound(), OBJECT_UPPER_BOUNDS[index]);
        assert_eq!(
            interval.len(),
            OBJECT_UPPER_BOUNDS[index] - OBJECT_OFFSETS[index]
        );
    }

    let mut attestation_job = AttestRevisionJob::new(
        candidate_index,
        RevisionAttestationJobContext::new(
            JobId::new(90),
            ResumeCheckpoint::new(91),
            ResumeCheckpoint::new(92),
            ResumeCheckpoint::new(93),
            RequestPriority::VisiblePage,
        ),
        RevisionAttestationLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .expect("canonical revision-attestation job configuration is valid");
    let attested = match attestation_job.poll(&store, &NeverCancelledDocument) {
        RevisionAttestationPoll::Ready(index) => index,
        RevisionAttestationPoll::Pending { .. } => {
            panic!("a fully supplied canonical PDF must not suspend revision attestation")
        }
        RevisionAttestationPoll::Failed(error) => {
            panic!("canonical strict base revision must attest: {error}")
        }
    };

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
    assert_eq!(attested.index_stats().total_entries(), 5);
    assert_eq!(attested.object_limits(), ObjectLimits::default());
    assert_eq!(attested.syntax_limits(), SyntaxLimits::default());
    let stats = attested.attestation_stats();
    assert_eq!(stats.objects_attested(), 4);
    assert!(stats.trivia_read_bytes() > 0);
    assert!(stats.trivia_scan_bytes() > 0);
    assert!(stats.object_read_bytes() > 0);
    assert!(stats.object_parse_bytes() > 0);
    assert!(stats.retained_evidence_bytes() > 0);

    let objects = attested.object_attestations();
    assert_eq!(objects.len(), 4);
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
    assert!(objects[3].object_span().end_exclusive() <= section.startxref());

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
    let resolved_catalog = direct_dictionary(resolved_root.object(), source);
    assert!(matches!(
        dictionary_value(resolved_catalog, b"Type"),
        SyntaxObject::Name(name) if name.bytes() == b"Catalog"
    ));
    assert_eq!(
        dictionary_value(resolved_catalog, b"Pages").as_reference(),
        Some(ObjectRef::new(2, 0).unwrap())
    );

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
        "native_object_loop_result sha256={PDF_SHA256} bytes={PDF_BYTES} startxref={STARTXREF} trailer=566..591 offsets=186,235,292,396 upper_bounds=235,292,396,449 objects=dictionary,dictionary,dictionary,stream payload4=427..431:710a510a strict_base_revision_attested=true attested_object_access=true reference_chain_resolved=true resident_footprint_accounted=true pdfium_o4_same_input=true pdfium_o4_vs_analytic_different_pixels=0 native_pdfium_differential=false"
    );
}

#[test]
fn native_object_loop_traceability_is_explicit_and_non_differential() {
    assert_eq!(top_level_version(FEATURE_MAP), Some("0.19.0"));
    assert_eq!(top_level_version(SPEC_MAP), Some("0.19.0"));

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

    for requirement_id in [
        "RPE-ARCH-001/5.3",
        "RPE-ARCH-001/5.4",
        "RPE-ARCH-001/12.6",
        "RPE-ARCH-001/15.3/M0",
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
    assert!(m0_requirement.contains("\"quality.native-object-loop\""));
    assert!(m0_requirement.contains("Native/PDFium differential evidence"));

    let xref_requirement = record_with_id(SPEC_MAP, "requirement", "RPE-ARCH-001/5.4")
        .expect("the xref architecture requirement must exist");
    assert!(xref_requirement.contains("\"core.base-revision-candidate-index\""));
    assert!(xref_requirement.contains("\"core.strict-base-revision-attestation\""));
    assert!(xref_requirement.contains("\"core.attested-object-access\""));
    assert!(xref_requirement.contains("\"core.attested-reference-chain-resolution\""));
    assert!(xref_requirement.contains("\"core.attested-resident-footprint\""));
    assert!(xref_requirement.contains("\"core/document\""));
    assert!(xref_requirement.contains("header-to-startxref"));
    assert!(xref_requirement.contains("line-terminated comments"));
    assert!(xref_requirement.contains("explicit caller-lent work cap"));
    assert!(xref_requirement.contains("never a raw target"));
    assert!(xref_requirement.contains("top-level direct indirect-reference value"));
    assert!(xref_requirement.contains("full closing chain"));
    assert!(
        xref_requirement
            .contains("job-wide object, edge, depth, path-capacity, read, and parse limits")
    );
    assert!(xref_requirement.contains("not a complete object-graph resolver"));
    assert!(xref_requirement.contains("runtime inline Rust representation"));
    assert!(xref_requirement.contains("cache-admission evidence only"));
    assert!(xref_requirement.contains("stream payloads"));
    assert!(xref_requirement.contains("nested semantic graph traversal"));
    assert!(xref_requirement.contains("persistent Ready caching"));
    assert!(xref_requirement.contains("cross-job/session aggregate work"));
    assert!(xref_requirement.contains("Native/PDFium semantic or pixel differential"));
    assert!(xref_requirement.contains("does not claim M1 exit"));
}
