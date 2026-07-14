use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_document::{
    AttestRevisionJob, CandidateRevisionIndex, DocumentLimits,
    NeverCancelled as NeverCancelledDocument, ObjectAttestationKind, RevisionAttestationJobContext,
    RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_generate::generate_one_page_pdf;
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
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

    println!(
        "native_object_loop_result sha256={PDF_SHA256} bytes={PDF_BYTES} startxref={STARTXREF} trailer=566..591 offsets=186,235,292,396 upper_bounds=235,292,396,449 objects=dictionary,dictionary,dictionary,stream payload4=427..431:710a510a strict_base_revision_attested=true pdfium_o4_same_input=true pdfium_o4_vs_analytic_different_pixels=0 native_pdfium_differential=false"
    );
}

#[test]
fn native_object_loop_traceability_is_explicit_and_non_differential() {
    assert_eq!(top_level_version(FEATURE_MAP), Some("0.15.0"));
    assert_eq!(top_level_version(SPEC_MAP), Some("0.15.0"));

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
    assert!(xref_requirement.contains("\"core/document\""));
    assert!(xref_requirement.contains("header-to-startxref"));
    assert!(xref_requirement.contains("line-terminated comments"));
    assert!(xref_requirement.contains("not an object/reference resolver"));
    assert!(xref_requirement.contains("Native/PDFium semantic or pixel differential"));
    assert!(xref_requirement.contains("does not claim M1 exit"));
}
