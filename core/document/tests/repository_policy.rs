use std::fs;
use std::path::{Path, PathBuf};

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

#[test]
fn product_document_core_has_only_approved_sibling_dependencies_and_no_platform_io() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_root.join("Cargo.toml"))
        .expect("crate manifest must be readable during repository tests");
    let dependency_body = manifest
        .split_once("[dependencies]")
        .expect("crate manifest declares an explicit dependency table")
        .1
        .split("\n[")
        .next()
        .expect("dependency table body is present")
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        dependency_body,
        [
            r#"pdf-rs-bytes = { path = "../bytes" }"#,
            r#"pdf-rs-syntax = { path = "../syntax" }"#,
            r#"pdf-rs-xref = { path = "../xref" }"#,
            r#"pdf-rs-object = { path = "../object" }"#,
        ]
    );
    for forbidden_table in ["[dev-dependencies]", "[build-dependencies]", "[target."] {
        assert!(
            !manifest.contains(forbidden_table),
            "core/document must not declare {forbidden_table} dependencies"
        );
    }

    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources);
    let joined = sources
        .iter()
        .map(|path| fs::read_to_string(path).expect("source must be readable"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    for forbidden in [
        "std::fs",
        "std::net",
        "async fn",
        "tokio",
        "async_std",
        "reqwest",
        "hyper",
        "pdfium",
        "mupdf",
        "pdf.js",
    ] {
        assert!(
            !joined.contains(forbidden),
            "forbidden product document-core token {forbidden:?}"
        );
    }
    assert!(joined.contains("#![forbid(unsafe_code)]"));
    assert!(joined.contains("#![deny(missing_docs)]"));
}

#[test]
fn shared_service_ownership_preserves_the_attestation_proof_boundary() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let model = fs::read_to_string(crate_root.join("src/model.rs"))
        .expect("document model source must be readable");
    let page_tree = fs::read_to_string(crate_root.join("src/page_tree.rs"))
        .expect("page-tree source must be readable");
    let outline = fs::read_to_string(crate_root.join("src/outline.rs"))
        .expect("outline source must be readable");
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("document library source must be readable");
    let provenance = fs::read_to_string(crate_root.join("PROVENANCE.md"))
        .expect("document provenance must be readable");
    let shared_impl = model
        .split_once("impl SharedAttestedRevisionIndex {")
        .and_then(|(_, tail)| tail.split_once("impl fmt::Debug for SharedAttestedRevisionIndex"))
        .map(|(body, _)| body)
        .expect("shared attested handle has one bounded public implementation");

    assert!(model.contains("pub struct SharedAttestedRevisionIndex(Arc<AttestedRevisionIndex>);"));
    assert!(model.contains("pub fn into_shared(self) -> SharedAttestedRevisionIndex"));
    assert!(!model.contains("impl From<CandidateRevisionIndex> for SharedAttestedRevisionIndex"));
    assert!(!shared_impl.contains("pub fn new("));
    assert!(library.contains("SharedAttestedRevisionIndex"));
    assert!(page_tree.contains("AttestedRevisionIndexOwner<'index>"));
    assert!(page_tree.contains("pub fn count_pages_owned("));
    assert!(page_tree.contains("Result<CountPagesJob<'static>, DocumentError>"));
    assert!(outline.contains("AttestedRevisionIndexOwner<'index>"));
    assert!(outline.contains("pub fn read_outline_owned("));
    assert!(outline.contains("Result<ReadOutlineJob<'static>, DocumentError>"));
    assert!(provenance.contains("There is no constructor from a candidate"));
    assert!(provenance.contains("without adding a Session, registry"));
}

#[test]
fn source_xref_stream_acquisition_stays_proof_bound_and_partial() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let source = fs::read_to_string(crate_root.join("src/source_xref_stream.rs"))
        .expect("source xref-stream acquisition source must be readable");
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("document library source must be readable");
    let provenance = fs::read_to_string(crate_root.join("PROVENANCE.md"))
        .expect("document provenance must be readable");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable");

    for required in [
        "IndirectObjectTarget::at_xref_stream_anchor",
        "OpenObjectEnvelopeJob",
        "OpenStreamBoundaryJob",
        "parse_unfiltered_xref_stream",
        "PayloadState::Missing",
        "ReadRequest::new",
        "SourceAcquiredXrefStream",
        "SourceXrefStreamErrorDetail::Object",
        "SourceXrefStreamErrorDetail::XrefStream",
        "SourceXrefStreamErrorDetail::Source",
        "SourceXrefStreamErrorCode::UnsupportedIndirectLength",
        "IndirectObjectTargetKind::XrefStreamAnchor",
        "pub(crate) const fn xref_stream(&self) -> &XrefStream",
        "pub fn entries(&self) -> &[XrefStreamEntry]",
        "SourceXrefStreamLimitKind::PayloadBytes",
        "retained_proof_bytes",
    ] {
        assert!(
            source.contains(required),
            "source xref-stream acquisition must retain {required:?}"
        );
    }
    assert!(!source.contains("DocumentError"));
    assert!(!source.contains("pdf_rs_filters"));
    assert!(!source.contains("Vec<u8>"));
    assert!(!source.contains("pub const fn xref_stream(&self) -> &XrefStream"));
    assert!(!source.contains("pub fn xref_stream(&self) -> &XrefStream"));
    assert!(library.contains("OpenSourceXrefStreamJob"));
    for required in [
        "one active `Pending` ticket at a time",
        "single-waiting-target Range arbiter contract",
        "caller-provided payload bytes never become proof",
        "does not publicly lend the cloneable naked `XrefStream`",
        "combined retained-proof bytes",
        "does not decode filters",
        "discover or follow `/Prev`",
        "compose revision precedence",
        "partial M1 evidence",
    ] {
        assert!(
            provenance.contains(required),
            "source xref-stream provenance must state {required:?}"
        );
    }
    let feature = record_with_id(
        &feature_map,
        "feature",
        "core.source-xref-stream-acquisition",
    )
    .expect("source xref-stream acquisition feature record must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.source-xref-stream-acquisition.v1\"",
        "RPE-ARCH-001/15.3/M1",
        "modules = [\"core/document\", \"core/object\", \"core/xref\"]",
        "core/document::source_xref_stream",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "source xref-stream feature must contain {required:?}"
        );
    }
}

#[test]
fn source_revision_chain_acquisition_stays_proof_bound_and_partial() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let source = fs::read_to_string(crate_root.join("src/source_revision_chain.rs"))
        .expect("source revision-chain acquisition source must be readable");
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("document library source must be readable");
    let provenance = fs::read_to_string(crate_root.join("PROVENANCE.md"))
        .expect("document provenance must be readable");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("specification traceability map must be readable");

    for required in [
        "pub struct OpenSourceRevisionChainJob",
        "pub struct SourceAcquiredRevisionChain",
        "reserve_active_child",
        "complete_active_child",
        "max_admitted_read_bytes",
        "max_admitted_parse_bytes",
        "max_admitted_retained_bound_bytes",
        "pub(crate) const fn revision_chain(&self) -> &RevisionChain",
    ] {
        assert!(
            source.contains(required),
            "source revision-chain acquisition must retain {required:?}"
        );
    }
    assert!(!source.contains("pub const fn revision_chain(&self) -> &RevisionChain"));
    assert!(!source.contains("pub fn revision_chain(&self) -> &RevisionChain"));
    assert!(library.contains("OpenSourceRevisionChainJob"));
    assert!(library.contains("SourceAcquiredRevisionChain"));

    for required in [
        "one active lower `Pending` result",
        "strict backward `/Prev` links",
        "without publicly lending the cloneable naked `RevisionChain`",
        "admits the complete geometric worst-case read/parse work",
        "explicit conservative `retained_bound`",
        "supports only unfiltered direct-Length xref streams",
        "does not establish M1 exit",
    ] {
        assert!(
            provenance.contains(required),
            "source revision-chain provenance must state {required:?}"
        );
    }

    let feature = record_with_id(
        &feature_map,
        "feature",
        "core.source-revision-chain-acquisition",
    )
    .expect("source revision-chain acquisition feature record must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.source-revision-chain-acquisition.v1\"",
        "RPE-ARCH-001/5.4",
        "RPE-ARCH-001/15.3/M1",
        "modules = [\"core/document\", \"core/xref\", \"core/object\"]",
        "core/document::source_revision_chain",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "source revision-chain feature must contain {required:?}"
        );
    }

    for requirement_id in ["RPE-ARCH-001/5.4", "RPE-ARCH-001/15.3/M1"] {
        let requirement = record_with_id(&spec_map, "requirement", requirement_id)
            .expect("source revision-chain requirement record must exist");
        for required in [
            "core.source-revision-chain-acquisition",
            "core/document::source_revision_chain",
            "OpenSourceRevisionChainJob",
        ] {
            assert!(
                requirement.contains(required),
                "{requirement_id} must trace source revision-chain evidence {required:?}"
            );
        }
    }
    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M1")
        .expect("M1 requirement record must exist");
    assert!(milestone.contains("status = \"partial\""));
    assert!(milestone.contains("filtered or indirect-Length xref streams"));
    assert!(milestone.contains("complete Session ownership remain open"));
}

#[test]
fn traceability_registers_strict_base_open_as_a_planned_product_composition() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("specification traceability map must be readable");

    let feature = record_with_id(&feature_map, "feature", "core.strict-base-open")
        .expect("strict base-open feature record must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.strict-base-open.v1\"",
        "RPE-ARCH-001/5.4",
        "RPE-STD-002/6-7",
        "RPE-STD-005/5-9",
        "modules = [\"core/document\"]",
        "core/document::strict_base_open",
        "core/document::repository_policy",
        "tools/quality::native_object_loop",
        "tools/quality::native_range_resume_loop",
        "tools/quality::native_strict_open_runtime_loop",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "strict base-open feature must contain {required:?}"
        );
    }

    let requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.4")
        .expect("strict base-revision architecture requirement must exist");
    for required in [
        "core.strict-base-open",
        "core/document::strict_base_open",
        "core/document::repository_policy",
        "tools/quality::native_range_resume_loop",
        "tools/quality::native_strict_open_runtime_loop",
        "runtime.strict-base-open-job-owner",
        "runtime/session::strict_base_open_owner",
        "status = \"partial\"",
        "product entry that composes xref discovery, candidate construction, and attestation under one JobId and five distinct checkpoints",
        "preserves complete lower xref or document errors and cumulative phase accounting",
        "propagates Pending without double charging",
        "publishes only the sealed `AttestedRevisionIndex`",
        "neither the xref section nor candidate index crosses the entry boundary",
        "Component and generated-PDF quality tests cover all five checkpoints",
        "reverse physical delivery",
        "Ready crosses the boundary only as an opaque move-only handoff",
        "cancellation in both child layers",
        "Resume execution and source-failure disposition require exact arbiter",
        "generic multi-job scheduler and complete Session lifecycle",
        "does not claim M1 exit",
    ] {
        assert!(
            requirement.contains(required),
            "strict base-revision mapping must contain {required:?}"
        );
    }
}

#[test]
fn traceability_registers_revision_resolution_without_claiming_complete_m1_support() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("specification traceability map must be readable");

    let feature = record_with_id(
        &feature_map,
        "feature",
        "core.revision-aware-object-resolver",
    )
    .expect("revision-aware resolver feature record must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.revision-aware-uncompressed-resolver.v1\"",
        "RPE-ARCH-001/5.3-5.4",
        "RPE-ARCH-001/15.3/M1",
        "RPE-STD-002/6-7",
        "RPE-STD-005/7-10",
        "modules = [\"core/document\"]",
        "core/document::revision_resolver",
        "core/document::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "revision resolver feature must contain {required:?}"
        );
    }

    let architecture = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.4")
        .expect("xref and object architecture mapping must exist");
    for required in [
        "core.revision-aware-object-resolver",
        "core/document::revision_resolver",
        "latest-wins",
        "resolves indirect Length",
        "unknown-type null",
        "do not linearly attest update placement",
        "core.object-stream-resolution",
        "m1.unfiltered-object-stream-resolution.v1",
        "latest effective uncompressed container",
        "object-stream scheduling and ownership",
        "does not claim M1 exit",
        "status = \"partial\"",
    ] {
        assert!(
            architecture.contains(required),
            "architecture mapping must contain {required:?}"
        );
    }
}

#[test]
fn traceability_registers_core_repaired_open_without_claiming_a_session() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("specification traceability map must be readable");
    let index_source =
        fs::read_to_string(crate_root.join("src/index.rs")).expect("index source must be readable");
    let repair_source = fs::read_to_string(crate_root.join("src/repair.rs"))
        .expect("repair source must be readable");
    let open_source = fs::read_to_string(crate_root.join("src/local_repair_open.rs"))
        .expect("local repaired-open source must be readable");
    assert!(index_source.contains("pub(crate) fn from_locally_parsed_xref"));
    assert!(!index_source.contains("pub fn from_locally_parsed_xref"));
    assert!(!repair_source.contains("pub fn into_candidate"));
    assert!(!repair_source.contains("pub fn candidate("));
    assert!(!repair_source.contains("pub fn into_attested"));
    assert!(!repair_source.contains("pub fn attested("));
    assert!(!repair_source.contains("Deref for LocallyRepairedRevisionIndex"));
    assert!(!repair_source.contains("AsRef<AttestedRevisionIndex>"));
    assert!(open_source.contains("OpenLocallyRepairedBaseRevisionJob"));
    assert!(open_source.contains("new_with_parent_caps"));
    assert!(!open_source.contains("pub fn plan("));

    let feature = record_with_id(&feature_map, "feature", "core.local-repair")
        .expect("local-repair feature record must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.r1-local-repair.v1\"",
        "core/document::local_repair_geometry",
        "core/document::local_repair_open",
        "tools/quality::maturity",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "local-repair feature must contain {required:?}"
        );
    }

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M1")
        .expect("M1 requirement record must exist");
    for required in [
        "core/document::local_repair_geometry",
        "core/document::local_repair_open",
        "document repair pipeline retains the local-xref proof",
        "pairs every proof with its interval",
        "linearly attests the header, every effective object, and all top-level trivia",
        "aggregate child-work caps",
        "LocallyRepairedRevisionIndex",
        "complete xref/object repair ledger",
        "single core repaired-open coordinator",
        "seventeen globally distinct checkpoints",
        "first-pass aggregate",
        "complete Session ownership remain open",
        "does not claim M1 exit",
        "status = \"partial\"",
    ] {
        assert!(
            milestone.contains(required),
            "M1 repair geometry mapping must contain {required:?}"
        );
    }
}

#[test]
fn traceability_registers_strict_page_count_without_claiming_a_page_index() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("specification traceability map must be readable");
    assert_eq!(top_level_version(&feature_map), Some("0.56.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.56.0"));

    let feature = record_with_id(&feature_map, "feature", "core.strict-page-count")
        .expect("strict page-count feature record must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.strict-page-count.v1\"",
        "RPE-ARCH-001/5.8-5.9",
        "modules = [\"core/document\"]",
        "core/document::page_tree_count",
        "core/document::page_tree_limit_config",
        "core/document::repository_policy",
        "tools/baseline::pdfium_page_count_real_adapter",
        "tools/baseline::repository_pdfium_page_count_probe",
        "tools/quality::native_object_loop",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "feature must contain {required:?}"
        );
    }

    let ownership = record_with_id(&feature_map, "feature", "core.shared-attested-service-jobs")
        .expect("shared attested service-job feature must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.shared-attested-service-jobs.v1\"",
        "RPE-ARCH-001/9.1",
        "core/document::page_tree_count",
        "core/document::outline",
        "core/document::repository_policy",
    ] {
        assert!(
            ownership.contains(required),
            "owned service-job feature must contain {required:?}"
        );
    }

    let requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.8-5.9")
        .expect("document-model and page-tree requirement must exist");
    for required in [
        "core.strict-page-count",
        "core/document",
        "core/document::page_tree_count",
        "tools/baseline::pdfium_page_count_real_adapter",
        "tools/baseline::repository_pdfium_page_count_probe",
        "tools/quality::native_object_loop",
        "open-addressing table",
        "exact Parent back-links",
        "never uses untrusted Count or Kids data for allocation",
        "valid one-page and nested three-page fixtures match exactly",
        "RPE-DOCUMENT-0033",
        "PDFium page_count=4",
        "expected strictness difference",
        "revision-chain",
        "lazy PageIndex",
        "page_count=1",
        "pages_processed=1",
        "not a registered page-count differential",
        "not a registered baseline or correctness oracle",
        "feature state remains PLANNED",
        "sealed cloneable handle",
        "not a Session, scheduler, cache, request lifecycle, or alternate proof source",
        "do not claim M1 or M2 exit",
        "status = \"partial\"",
    ] {
        assert!(
            requirement.contains(required),
            "requirement must contain {required:?}"
        );
    }
}

#[test]
fn traceability_registers_bounded_pdf_text_strings() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("specification traceability map must be readable");

    let feature = record_with_id(&feature_map, "feature", "core.pdf-text-string")
        .expect("PDF text-string feature record must exist");
    for required in [
        "profile = \"m1.pdf-text-string.v1\"",
        "ISO-32000-1:2008/7.9.2.2",
        "ISO-32000-1:2008/D.3",
        "RPE-ARCH-001/5.8-5.9",
        "modules = [\"core/document\"]",
        "core/document::text_string",
        "core/document::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "feature must contain {required:?}"
        );
    }

    for (requirement_id, required) in [
        (
            "ISO-32000-1:2008/7.9.2.2",
            [
                "core.pdf-text-string",
                "core.strict-outline",
                "core/document::text_string",
                "core/document::outline",
                "FE FF selects UTF-16BE",
                "supplementary scalars",
                "does not implement the PDF 2.0 UTF-8 extension",
                "status = \"partial\"",
            ],
        ),
        (
            "ISO-32000-1:2008/D.3",
            [
                "core.pdf-text-string",
                "core.strict-outline",
                "core/document::text_string",
                "core/document::outline",
                "manually transcribed",
                "Undefined codes are rejected",
                "not font encoding",
                "status = \"partial\"",
            ],
        ),
    ] {
        let requirement = record_with_id(&spec_map, "requirement", requirement_id)
            .unwrap_or_else(|| panic!("requirement {requirement_id} must exist"));
        for fragment in required {
            assert!(
                requirement.contains(fragment),
                "requirement {requirement_id} must contain {fragment:?}"
            );
        }
    }
}

#[test]
fn traceability_registers_strict_outline_as_a_partial_bootstrap() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("specification traceability map must be readable");

    let feature = record_with_id(&feature_map, "feature", "core.strict-outline")
        .expect("strict outline feature record must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.strict-outline.v1\"",
        "ISO-32000-1:2008/7.3.9",
        "ISO-32000-1:2008/7.3.10",
        "ISO-32000-1:2008/7.7.2",
        "ISO-32000-1:2008/12.3.3",
        "ISO-32000-1:2008/7.9.2.2",
        "ISO-32000-1:2008/D.3",
        "RPE-ARCH-001/5.8-5.9",
        "modules = [\"core/document\"]",
        "core/document::outline",
        "core/document::outline_limit_config",
        "core/document::repository_policy",
        "tools/baseline::pdfium_outline_real_adapter",
        "tools/baseline::repository_pdfium_outline_probe",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "strict outline feature must contain {required:?}"
        );
    }

    let null_requirement = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/7.3.9")
        .expect("null-object requirement must exist");
    for required in [
        "sha256:9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff",
        "features = [\"core.strict-outline\"]",
        "implementation = [\"core/document\"]",
        "core/document::outline",
        "core/document::repository_policy",
        "status = \"partial\"",
        "direct null values as omitted",
        "undefined indirect reference",
        "reference resolving to null",
        "strict root/item dictionary shape failures",
        "does not claim general dictionary",
    ] {
        assert!(
            null_requirement.contains(required),
            "null-object requirement must contain {required:?}"
        );
    }

    let indirect_requirement = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/7.3.10")
        .expect("indirect-object equivalence requirement must exist");
    for required in [
        "sha256:9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff",
        "features = [\"core.strict-outline\"]",
        "implementation = [\"core/document\"]",
        "core/document::outline",
        "core/document::repository_policy",
        "status = \"partial\"",
        "structural First, Last, Parent, Prev, and Next references",
        "indirect form of any of those semantic fields",
        "before dereferencing",
        "makes no claim that the referenced target",
    ] {
        assert!(
            indirect_requirement.contains(required),
            "indirect-object equivalence requirement must contain {required:?}"
        );
    }

    let catalog_requirement = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/7.7.2")
        .expect("Catalog Outlines requirement must exist");
    for required in [
        "sha256:9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff",
        "features = [\"core.strict-outline\"]",
        "implementation = [\"core/document\"]",
        "core/document::outline",
        "core/document::repository_policy",
        "status = \"partial\"",
        "missing or null field returns an empty outline",
        "exact indirect reference",
        "Page counting deliberately does not inspect",
        "Catalog aliases",
        "persistent Catalog cache remain unsupported",
    ] {
        assert!(
            catalog_requirement.contains(required),
            "Catalog Outlines requirement must contain {required:?}"
        );
    }

    let outline_requirement = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/12.3.3")
        .expect("outline dictionary requirement must exist");
    for required in [
        "sha256:9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff",
        "features = [\"core.strict-outline\"]",
        "implementation = [\"core/document\"]",
        "core/document::outline",
        "core/document::outline_limit_config",
        "core/document::repository_policy",
        "status = \"partial\"",
        "paired First and Last boundaries",
        "exact Parent and Prev links",
        "initial Prev and terminal Next shape",
        "nonempty root requires Count",
        "empty root requires Count omission",
        "decoded direct text-string titles",
        "never executes an action",
        "permits indirect outline-root Type and indirect Title, Count, Dest, and A forms",
        "does not judge the referenced target valid",
        "destination resolution",
        "non-gating O4 PDFium public-bookmark comparison matches Native exactly",
        "wrong-Prev fixture yields Native RPE-DOCUMENT-0041",
        "public API does not expose Prev",
        "contained baseline registration",
        "broad corpus differential evidence remain open",
        "does not claim ISO conformance or M1 exit",
    ] {
        assert!(
            outline_requirement.contains(required),
            "outline dictionary requirement must contain {required:?}"
        );
    }

    let architecture_requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.8-5.9")
        .expect("document-model architecture requirement must exist");
    for required in [
        "core.strict-outline",
        "core/document::outline",
        "core/document::outline_limit_config",
        "tools/baseline::pdfium_outline_real_adapter",
        "tools/baseline::repository_pdfium_outline_probe",
        "m1.strict-outline.v1",
        "recursively recomputed item/root Count semantics",
        "indirect-null equivalence",
        "pinned, non-gating O4 PDFium public-bookmark comparison matches Native exactly",
        "expected wrong-Prev strictness difference",
        "not a registered baseline or correctness oracle",
        "do not claim M1 or M2 exit",
        "status = \"partial\"",
    ] {
        assert!(
            architecture_requirement.contains(required),
            "document-model architecture requirement must contain {required:?}"
        );
    }
}

fn collect_rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("source directory must be readable") {
        let entry = entry.expect("source entry must be readable");
        let path = entry.path();
        if entry
            .file_type()
            .expect("file type must be readable")
            .is_dir()
        {
            collect_rust_sources(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}
