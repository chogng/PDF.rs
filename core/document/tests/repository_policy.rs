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
            r#"pdf-rs-filters = { path = "../filters" }"#,
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
fn m2_page_index_build_and_lazy_lookup_are_traceable_without_overclaim() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let page_index =
        fs::read_to_string(crate_root.join("src/page_index.rs")).expect("page index is readable");
    let page_index_job = fs::read_to_string(crate_root.join("src/page_index_job.rs"))
        .expect("page index jobs are readable");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance is readable");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature map is readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec map is readable");
    let plan =
        fs::read_to_string(repository_root.join("plan/m2.toml")).expect("M2 plan is readable");

    for required in [
        "pub struct PageIndex",
        "pub struct PageIndexLimits",
        "pub struct PageHandle",
        "pub struct PageSegmentSummary",
        "pub enum PageIndexSegmentKind",
        "pub enum PageSegmentEvidence",
        "pub(crate) struct PageIndexNodeEvidence",
        "pub(crate) fn from_lazy_root(",
        "pub(crate) fn validate_new_node_shape(",
        "segment_order: Arc<Vec<usize>>",
        "fn merge_sorted_nodes(",
        "pub(crate) struct ValidatedPageOrder",
        "pub(crate) fn admit(",
        "DocumentLimitKind::PageIndexBytes",
        "DocumentErrorCode::StalePageHandle",
    ] {
        assert!(
            page_index.contains(required),
            "page-index foundation must contain {required:?}"
        );
    }
    for required in [
        "pub struct BuildPageIndexJob",
        "pub struct LookupPageJob",
        "pub enum PageIndexBuildPoll",
        "pub enum PageLookupPoll",
        "pub fn build_page_index(",
        "pub fn build_page_index_owned(",
        "pub fn lookup_page(",
        "pub fn lookup_page_owned(",
        "DocumentErrorCode::PageIndexOutOfBounds",
        "struct PendingNodeIndex",
        "fn insert_pending_index(",
        "fn reserve_pending_storage(",
    ] {
        assert!(
            page_index_job.contains(required),
            "page-index integration must contain {required:?}"
        );
    }
    assert!(provenance.contains("opens only the strict"));
    assert!(provenance.contains("Catalog and root Pages dictionary"));
    assert!(provenance.contains("Unopened subtree Counts remain"));
    assert!(provenance.contains("unchanged M1 service"));

    let feature = record_with_id(&feature_map, "feature", "core.ordered-page-index")
        .expect("ordered page-index feature must be registered");
    assert!(feature.contains("profile = \"m2.ordered-page-index.v1\""));
    assert!(feature.contains("state = \"PLANNED\""));
    assert!(feature.contains("RPE-ARCH-001/15.3/M2"));

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M2")
        .expect("M2 requirement must be registered");
    assert!(milestone.contains("status = \"partial\""));
    assert!(milestone.contains("M2-02 is complete"));
    assert!(milestone.contains("DeclaredCount"));
    assert!(milestone.contains("ValidatedPartition"));
    assert!(milestone.contains("CompleteSubtree"));

    let m2_01 =
        record_with_id(&plan, "work_item", "M2-01").expect("M2-01 work item must be planned");
    assert!(m2_01.contains("status = \"complete\""));
    assert!(m2_01.contains("completed_at = 2026-07-16"));
    let m2_02 =
        record_with_id(&plan, "work_item", "M2-02").expect("M2-02 work item must be planned");
    assert!(m2_02.contains("status = \"complete\""));
    assert!(m2_02.contains("completed_at = 2026-07-16"));
    let m2_03 =
        record_with_id(&plan, "work_item", "M2-03").expect("M2-03 work item must be planned");
    assert!(m2_03.contains("status = \"complete\""));
    assert!(m2_03.contains("completed_at = 2026-07-16"));
}

#[test]
fn m2_inherited_page_values_are_traceable_as_one_bounded_profile() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let materialization = fs::read_to_string(crate_root.join("src/page_materialization.rs"))
        .expect("page materialization source is readable");
    let page_index =
        fs::read_to_string(crate_root.join("src/page_index.rs")).expect("page index is readable");
    let geometry = fs::read_to_string(crate_root.join("src/page_geometry.rs"))
        .expect("page geometry source is readable");
    let resources = fs::read_to_string(crate_root.join("src/page_resources.rs"))
        .expect("page resource source is readable");
    let limits = fs::read_to_string(crate_root.join("src/page_materialization_limits.rs"))
        .expect("page materialization limits are readable");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance is readable");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature map is readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec map is readable");
    let plan =
        fs::read_to_string(repository_root.join("plan/m2.toml")).expect("M2 plan is readable");

    assert_eq!(top_level_version(&feature_map), Some("0.68.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.68.0"));
    for required in [
        "pub struct MaterializedPage",
        "pub struct MaterializePageJob",
        "pub enum PageMaterializationPoll",
        "pub fn materialize_page(",
        "pub fn materialize_page_owned(",
        "DocumentLimitKind::PageMaterializationObjects",
        "DocumentLimitKind::PageMaterializationReferenceEdges",
        "DocumentLimitKind::PageMaterializationObjectReadBytes",
        "DocumentLimitKind::PageMaterializationObjectParseBytes",
        "DocumentLimitKind::PageMaterializationStateBytes",
        "DocumentErrorCode::PageValueAliasCycle",
        "DocumentErrorCode::UnsupportedPageValueRepresentation",
    ] {
        assert!(
            materialization.contains(required),
            "materialization profile must contain {required:?}"
        );
    }
    assert!(page_index.contains("DocumentLimitKind::PageMaterializationAncestors"));
    assert!(geometry.contains("pub struct PageValueProvenance"));
    assert!(geometry.contains("pub struct PageBoxes"));
    assert!(geometry.contains("pub enum PageRotation"));
    assert!(resources.contains("pub struct PageResourceScope"));
    assert!(resources.contains("ancestor_lookup_chain"));
    assert!(resources.contains("alias_chain"));
    assert!(limits.contains("pub struct PageMaterializationLimits"));
    assert!(limits.contains("max_retained_state_bytes"));

    for required in [
        "Proof-bound inherited page materialization",
        "already-discovered Page-to-root identity chain",
        "merge dictionaries from multiple ancestors",
        "whole-value top-level reference chain",
        "Source mismatch precedes cancellation",
        "does not materialize Contents",
    ] {
        assert!(
            provenance.contains(required),
            "materialization provenance must state {required:?}"
        );
    }

    let feature = record_with_id(&feature_map, "feature", "core.inherited-page-values")
        .expect("inherited page-values feature must be registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.inherited-page-values.v1\"",
        "ISO-32000-1:2008/7.7.3",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"core/document\"]",
        "core/document::page_materialization",
        "core/document::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "inherited page-values feature must contain {required:?}"
        );
    }

    let page_tree = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/7.7.3")
        .expect("page-tree requirement must exist");
    assert!(page_tree.contains("core.inherited-page-values"));
    assert!(page_tree.contains("core/document::page_materialization"));
    assert!(page_tree.contains("nearest non-null MediaBox, CropBox, Rotate, and Resources"));

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M2")
        .expect("M2 requirement must exist");
    assert!(milestone.contains("M2-03 is complete"));
    assert!(milestone.contains("Acquired-chain page indexing/materialization"));
    assert!(milestone.contains("M2 exit gate is not closed"));

    let m2_03 = record_with_id(&plan, "work_item", "M2-03").expect("M2-03 work item must exist");
    assert!(m2_03.contains("status = \"complete\""));
    assert!(m2_03.contains("completed_at = 2026-07-16"));
}

#[test]
fn m2_page_property_lookup_is_no_io_bounded_and_traceable() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let resources = fs::read_to_string(crate_root.join("src/page_resources.rs"))
        .expect("page resource source is readable");
    let limits = fs::read_to_string(crate_root.join("src/page_property_lookup_limits.rs"))
        .expect("page-property limits are readable");
    let error =
        fs::read_to_string(crate_root.join("src/error.rs")).expect("document errors are readable");
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("document library source is readable");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature map is readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec map is readable");
    let plan =
        fs::read_to_string(repository_root.join("plan/m2.toml")).expect("M2 plan is readable");

    assert_eq!(top_level_version(&feature_map), Some("0.68.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.68.0"));
    for required in [
        "pub struct PagePropertyReference",
        "pub struct PagePropertyResolver",
        "pub const fn property_resolver(",
        "pub fn lookup_marked_content_property(",
        "Borrowed no-I/O resolver",
        "without polling or opening the target object",
        "DocumentErrorCode::DuplicateStructuralKey",
        "DocumentErrorCode::InvalidPagePropertyResource",
        "DocumentErrorCode::UnsupportedIndirectPageProperties",
        "DocumentErrorCode::UnsupportedDirectPagePropertyDictionary",
        "property_name",
        "\"[NOT RETAINED]\"",
    ] {
        assert!(
            resources.contains(required),
            "page-property lookup must contain {required:?}"
        );
    }
    for required in [
        "pub struct PagePropertyLookupLimitConfig",
        "pub struct PagePropertyLookupLimits",
        "pub struct PagePropertyLookupStats",
        "max_lookups",
        "max_entry_visits",
        "HARD_MAX_LOOKUPS",
        "HARD_MAX_ENTRY_VISITS",
    ] {
        assert!(
            limits.contains(required),
            "page-property limits must contain {required:?}"
        );
    }
    for required in [
        "PagePropertyLookups,",
        "PagePropertyEntryVisits,",
        "InvalidPagePropertyResource,",
        "UnsupportedIndirectPageProperties,",
        "UnsupportedDirectPagePropertyDictionary,",
    ] {
        assert!(
            error.contains(required),
            "document error policy must contain {required:?}"
        );
    }
    for required in [
        "PagePropertyLookupLimitConfig",
        "PagePropertyLookupLimits",
        "PagePropertyLookupStats",
        "PagePropertyReference",
        "PagePropertyResolver",
    ] {
        assert!(
            library.contains(required),
            "document public boundary must export {required:?}"
        );
    }

    let feature = record_with_id(&feature_map, "feature", "core.page-property-lookup")
        .expect("page-property lookup feature is registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.page-property-lookup.v1\"",
        "ISO-32000-1:2008/7.8.3",
        "ISO-32000-1:2008/14.6.2",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/6.1-6.2",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"core/document\"]",
        "core/document::page_properties",
        "core/document::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "page-property feature must contain {required:?}"
        );
    }

    let page_resources = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/7.8.3")
        .expect("page-resource requirement is registered");
    for required in [
        "core.page-property-lookup",
        "core.content-vm-scene-v1",
        "core/document::page_properties",
        "no-I/O",
        "without polling for bytes",
        "fixed-size PagePropertyReference evidence",
        "never opens or attests the selected target object",
        "retain the original lower DocumentError",
        "status = \"partial\"",
    ] {
        assert!(
            page_resources.contains(required),
            "page-resource mapping must contain {required:?}"
        );
    }

    let marked_properties = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/14.6.2")
        .expect("marked-content property requirement is registered");
    for required in [
        "core.page-property-lookup",
        "core.content-vm-scene-v1",
        "direct /Properties dictionary",
        "uniquely named indirect-reference entry",
        "performs no source read",
        "independent lookup and entry-visit budgets",
        "preserves ordinary lower document and Scene errors without remapping",
        "status = \"partial\"",
    ] {
        assert!(
            marked_properties.contains(required),
            "marked-property mapping must contain {required:?}"
        );
    }

    let document_model = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.8-5.9")
        .expect("document-model requirement is registered");
    assert!(document_model.contains("core.page-property-lookup"));
    assert!(document_model.contains("core/document::page_properties"));
    assert!(document_model.contains("M2 adds four separate PLANNED document profiles"));
    assert!(document_model.contains("m2.page-property-lookup.v1"));
    assert!(document_model.contains("without polling for bytes"));

    let interpreter = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/6.1-6.2")
        .expect("content-interpreter requirement is registered");
    assert!(interpreter.contains("core.page-property-lookup"));
    assert!(interpreter.contains("original lower DocumentError"));
    assert!(interpreter.contains("M2-06 is complete"));

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M2")
        .expect("M2 requirement is registered");
    assert!(milestone.contains("core.page-property-lookup"));
    assert!(milestone.contains("core/document::page_properties"));
    assert!(milestone.contains("M2-06 is complete as two additional bounded PLANNED profiles"));
    assert!(milestone.contains("M2-07 registered normative Scene cases"));
    assert!(milestone.contains("M2 exit gate is not closed"));

    let m2_06 = record_with_id(&plan, "work_item", "M2-06").expect("M2-06 work item exists");
    assert!(m2_06.contains("status = \"complete\""));
    assert!(m2_06.contains("completed_at = 2026-07-16"));
    let m2_07 = record_with_id(&plan, "work_item", "M2-07").expect("M2-07 work item exists");
    assert!(m2_07.contains("status = \"planned\""));
}

#[test]
fn m2_page_content_acquisition_is_proof_bound_and_traceable() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let page_content = fs::read_to_string(crate_root.join("src/page_content.rs"))
        .expect("page-content acquisition source is readable");
    let limits = fs::read_to_string(crate_root.join("src/page_content_limits.rs"))
        .expect("page-content limits are readable");
    let error =
        fs::read_to_string(crate_root.join("src/error.rs")).expect("document errors are readable");
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("document library source is readable");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature map is readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec map is readable");
    let plan =
        fs::read_to_string(repository_root.join("plan/m2.toml")).expect("M2 plan is readable");

    assert_eq!(top_level_version(&feature_map), Some("0.68.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.68.0"));
    for required in [
        "pub struct PageContentJobContext",
        "pub enum PageContentPhase",
        "pub struct PageContentStats",
        "pub struct AcquiredPageContentStream",
        "pub enum PageContentDecode",
        "pub struct EmptyIdentityContent",
        "pub struct AcquiredPageContent",
        "pub enum PageContentPoll",
        "pub struct AcquirePageContentJob",
        "AttestedRevisionIndexOwner<'index>",
        "pub fn acquire_page_content(",
        "pub fn acquire_page_content_owned(",
        "FilterPlan::preflight_pdf_dictionary",
        "FilterPlan::retained_heap_upper_bound",
        "FilterPlan::from_pdf_dictionary",
        "DecodeRequest::new",
        "PageContentDecode::EmptyIdentity",
        "enum ParentDecodeBudget",
        "ObjectWorkCaps::new_with_retained_bytes",
        "SyntaxLimitKind::RetainedBytes",
        "fn prioritize_runtime_error(",
        "DecodeErrorCategory::Unsupported => DocumentErrorCode::UnsupportedPageContentFilter",
        "DecodeErrorCategory::Syntax => DocumentErrorCode::PageContentDecodeFailure",
    ] {
        assert!(
            page_content.contains(required),
            "page-content acquisition must contain {required:?}"
        );
    }
    for required in [
        "pub struct PageContentLimitConfig",
        "pub struct PageContentLimits",
        "max_alias_depth",
        "max_streams",
        "max_array_entries",
        "max_objects",
        "max_reference_edges",
        "max_total_object_read_bytes",
        "max_total_object_parse_bytes",
        "max_total_encoded_bytes",
        "max_total_decoded_bytes",
        "max_total_decode_fuel",
        "max_retained_state_bytes",
    ] {
        assert!(
            limits.contains(required),
            "page-content limits must contain {required:?}"
        );
    }
    for required in [
        "PageContentAliasDepth,",
        "PageContentStreamInputBytes,",
        "PageContentStreamFilters,",
        "PageContentStreamFilterPlanBytes,",
        "PageContentStreamLayerOutputBytes,",
        "PageContentStreamTotalOutputBytes,",
        "PageContentStreamFinalOutputBytes,",
        "PageContentStreamDecodeFuel,",
        "PageContentStreamRetainedBytes,",
        "DocumentErrorCode::DuplicatePageContents",
        "DocumentErrorCode::InvalidPageContents",
        "DocumentErrorCode::UnsupportedPageContentsRepresentation",
        "DocumentErrorCode::PageContentAliasCycle",
        "DocumentErrorCode::PageContentDecodeFailure",
        "DocumentErrorCode::UnsupportedPageContentFilter",
    ] {
        assert!(
            error.contains(required),
            "document error policy must contain {required:?}"
        );
    }
    for required in [
        "AcquirePageContentJob",
        "AcquiredPageContent",
        "AcquiredPageContentStream",
        "EmptyIdentityContent",
        "PageContentDecode",
        "PageContentJobContext",
        "PageContentPoll",
        "PageContentLimitConfig",
        "PageContentLimits",
    ] {
        assert!(
            library.contains(required),
            "document public boundary must export {required:?}"
        );
    }

    let feature = record_with_id(&feature_map, "feature", "core.page-content-acquisition")
        .expect("page-content acquisition feature is registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.page-content-acquisition.v1\"",
        "ISO-32000-1:2008/7.7.3",
        "ISO-32000-1:2008/7.8.2",
        "RPE-ARCH-001/5.6",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"core/document\"]",
        "core/document::page_content",
        "core/document::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "page-content feature must contain {required:?}"
        );
    }

    let page_tree = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/7.7.3")
        .expect("page-tree requirement exists");
    for required in [
        "core.page-content-acquisition",
        "core/document::page_content",
        "M2-05 consumes that exact materialized Page",
        "strict attested authority",
        "supported whole-object aliases",
        "Locally repaired and acquired-chain page indexing/materialization/content acquisition",
    ] {
        assert!(
            page_tree.contains(required),
            "page-tree mapping must contain {required:?}"
        );
    }

    let content_stream = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/7.8.2")
        .expect("content-stream requirement exists");
    for required in [
        "core.page-content-acquisition",
        "core.content-operator-scanner",
        "core.content-vm-scene-v1",
        "core/document::page_content",
        "core/content::vm",
        "execution order",
        "sealed DecodedStream",
        "zero-length unfiltered identity proof",
        "strict attested proof",
        "proof-bearing AcquiredPageContent",
        "validates known operand shapes before state or unsupported policy",
        "scanner, document, and Scene failures retain their original structured diagnostic types",
        "Inline images, Forms, paths, painting, text showing",
    ] {
        assert!(
            content_stream.contains(required),
            "content-stream mapping must contain {required:?}"
        );
    }

    let stream_decode = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.6")
        .expect("stream-decoding requirement exists");
    assert!(stream_decode.contains("core.page-content-acquisition"));
    assert!(stream_decode.contains("core/document::page_content"));
    assert!(stream_decode.contains("m2.page-content-acquisition.v1"));
    assert!(stream_decode.contains("explicit identity proof"));

    let document_model = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.8-5.9")
        .expect("document-model requirement exists");
    assert!(document_model.contains("core.page-content-acquisition"));
    assert!(document_model.contains("core/document::page_content"));
    assert!(document_model.contains("M2 adds four separate PLANNED document profiles"));
    assert!(document_model.contains("m2.page-property-lookup.v1"));
    assert!(
        document_model.contains("Acquired-chain page indexing/materialization/content acquisition")
    );

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M2")
        .expect("M2 requirement exists");
    for required in [
        "core.page-content-acquisition",
        "core/document::page_content",
        "M2-05 is complete as two bounded PLANNED profiles",
        "strict-attested Page content acquisition",
        "ordered exact physical, filter, and decoded proof",
        "Acquired-chain page indexing/materialization/content acquisition",
        "M2-06 is complete as two additional bounded PLANNED profiles",
        "sealed Content VM consumes only strict-attested AcquiredPageContent",
        "M2-07 registered normative Scene cases",
        "M2 exit gate is not closed",
    ] {
        assert!(
            milestone.contains(required),
            "M2 mapping must contain {required:?}"
        );
    }

    let m2_05 = record_with_id(&plan, "work_item", "M2-05").expect("M2-05 work item exists");
    assert!(m2_05.contains("status = \"complete\""));
    assert!(m2_05.contains("completed_at = 2026-07-16"));
    let m2_06 = record_with_id(&plan, "work_item", "M2-06").expect("M2-06 work item exists");
    assert!(m2_06.contains("status = \"complete\""));
    assert!(m2_06.contains("completed_at = 2026-07-16"));
    let m2_07 = record_with_id(&plan, "work_item", "M2-07").expect("M2-07 work item exists");
    assert!(m2_07.contains("status = \"planned\""));
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
        "parse_decoded_xref_stream",
        "DecodeRequest::new",
        "decoded_proof: Option<DecodedStream>",
        "PayloadState::Missing",
        "ReadRequest::new",
        "SourceAcquiredXrefStream",
        "SourceXrefStreamErrorDetail::Object",
        "SourceXrefStreamErrorDetail::XrefStream",
        "SourceXrefStreamErrorDetail::Source",
        "SourceXrefStreamErrorCode::UnsupportedIndirectLength",
        "SourceXrefStreamErrorCode::UnsupportedEmptyFilteredPayload",
        "IndirectObjectTargetKind::XrefStreamAnchor",
        "pub(crate) const fn xref_stream(&self) -> &XrefStream",
        "pub fn entries(&self) -> &[XrefStreamEntry]",
        "SourceXrefStreamLimitKind::PayloadBytes",
        "plan_retained_heap_bytes",
        "FilterPlan::retained_heap_upper_bound",
        "retained_proof_bytes",
    ] {
        assert!(
            source.contains(required),
            "source xref-stream acquisition must retain {required:?}"
        );
    }
    assert!(!source.contains("DocumentError"));
    assert!(!source.contains("mem::size_of::<StreamFilter>()"));
    assert!(!source.contains("mem::size_of::<FilterStage>()"));
    assert!(source.contains("pdf_rs_filters"));
    assert!(!source.contains("Vec<u8>"));
    assert!(!source.contains("pub const fn xref_stream(&self) -> &XrefStream"));
    assert!(!source.contains("pub fn xref_stream(&self) -> &XrefStream"));
    assert!(library.contains("OpenSourceXrefStreamJob"));
    for required in [
        "one active `Pending` ticket at a time",
        "single-waiting-target Range arbiter contract",
        "caller-provided payload bytes never become proof",
        "does not publicly lend the cloneable naked `XrefStream`",
        "`DecodedStream` proof beside the semantic table",
        "strict direct `/Filter` and `/DecodeParms`",
        "temporary name, parameter, and stage vectors are hard-bounded",
        "UnsupportedEmptyFilteredPayload",
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
        "RPE-ARCH-001/5.6",
        "RPE-ARCH-001/15.3/M1",
        "modules = [\"core/document\", \"core/filters\", \"core/object\", \"core/xref\"]",
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
        "compatibility constructor remains unfiltered",
        "opt-in decode constructor pre-admits",
        "composes filtered stream sections",
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
        "RPE-ARCH-001/5.6",
        "RPE-ARCH-001/15.3/M1",
        "modules = [\"core/document\", \"core/filters\", \"core/xref\", \"core/object\"]",
        "core/document::source_revision_chain",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "source revision-chain feature must contain {required:?}"
        );
    }

    for requirement_id in [
        "RPE-ARCH-001/5.4",
        "RPE-ARCH-001/5.6",
        "RPE-ARCH-001/15.3/M1",
    ] {
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
    assert!(milestone.contains("status = \"covered\""));
    assert!(milestone.contains("indirect-Length xref streams"));
    assert!(milestone.contains("source-acquired document owner now closes"));
    assert!(milestone.contains("acquired-chain page-count/outline integration"));
    assert!(milestone.contains("strict page-count and outline at DIFFERENTIAL"));
}

#[test]
fn source_acquired_document_services_stay_proof_bound_and_planned() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/document has a repository root two levels above it");
    let object_source = fs::read_to_string(crate_root.join("src/acquired_object.rs"))
        .expect("source-acquired object source must be readable");
    let service_source = fs::read_to_string(crate_root.join("src/acquired_services.rs"))
        .expect("source-acquired service source must be readable");
    let provenance = fs::read_to_string(crate_root.join("PROVENANCE.md"))
        .expect("document provenance must be readable");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("specification traceability map must be readable");

    for required in [
        "pub struct SourceAcquiredDocument",
        "pub struct OpenAcquiredObjectJob",
        "pub enum AcquiredObjectCoordinate",
        "pub enum AcquiredObjectPoll",
    ] {
        assert!(
            object_source.contains(required),
            "source-acquired object boundary must retain {required:?}"
        );
    }
    for required in [
        "pub struct CountAcquiredPagesJob",
        "pub struct ReadAcquiredOutlineJob",
        "pub struct AcquiredPageCount",
        "pub struct AcquiredOutline",
    ] {
        assert!(
            service_source.contains(required),
            "source-acquired service boundary must retain {required:?}"
        );
    }
    for required in [
        "retains that original move-only proof",
        "input-derived `entries + sections` physical-anchor upper bound",
        "effective-uncompressed indirect `/Length`",
        "Before any source poll",
        "are conservative bounds, not allocations",
        "latest-wins behavior across traditional, primary xref-stream",
        "do not claim top-level attestation",
    ] {
        assert!(
            provenance.contains(required),
            "source-acquired provenance must state {required:?}"
        );
    }

    let feature = record_with_id(
        &feature_map,
        "feature",
        "core.source-acquired-document-services",
    )
    .expect("source-acquired document-services feature must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.source-acquired-document-services.v1\"",
        "RPE-ARCH-001/5.3-5.4",
        "RPE-ARCH-001/5.6",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/15.3/M1",
        "modules = [\"core/document\"]",
        "core/document::acquired_object",
        "core/document::acquired_services",
        "core/document::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "source-acquired feature must contain {required:?}"
        );
    }

    for requirement_id in [
        "RPE-ARCH-001/5.3",
        "RPE-ARCH-001/5.4",
        "RPE-ARCH-001/5.6",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/15.3/M1",
    ] {
        let requirement = record_with_id(&spec_map, "requirement", requirement_id)
            .expect("source-acquired requirement record must exist");
        for required in [
            "core.source-acquired-document-services",
            "core/document::acquired_services",
        ] {
            assert!(
                requirement.contains(required),
                "{requirement_id} must trace source-acquired evidence {required:?}"
            );
        }
    }
    for requirement_id in [
        "RPE-ARCH-001/5.3",
        "RPE-ARCH-001/5.4",
        "RPE-ARCH-001/5.6",
        "RPE-ARCH-001/15.3/M1",
    ] {
        let requirement = record_with_id(&spec_map, "requirement", requirement_id)
            .expect("source-acquired object requirement record must exist");
        assert!(
            requirement.contains("core/document::acquired_object"),
            "{requirement_id} must trace the acquired-object implementation"
        );
    }

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M1")
        .expect("M1 requirement record must exist");
    assert!(milestone.contains("status = \"covered\""));
    assert!(milestone.contains("source-acquired document owner now closes"));
    assert!(milestone.contains("strict page-count and outline at DIFFERENTIAL"));
    assert!(milestone.contains("these bounded gates cover M1"));
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
        "generic scheduler and complete Session",
        "contribute to the covered M1 byte-and-object gate",
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
        "schedules exact filtered or unfiltered object-stream payload reads",
        "contribute to the covered M1 byte-and-object gate",
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
    let model_source =
        fs::read_to_string(crate_root.join("src/model.rs")).expect("model source must be readable");
    let page_tree_source = fs::read_to_string(crate_root.join("src/page_tree.rs"))
        .expect("page-tree source must be readable");
    let outline_source = fs::read_to_string(crate_root.join("src/outline.rs"))
        .expect("outline source must be readable");
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
    assert!(!repair_source.contains("pub fn as_attested"));
    assert!(repair_source.contains("pub struct SharedLocallyRepairedRevisionIndex"));
    assert!(repair_source.contains("pub fn as_repaired"));
    assert!(repair_source.contains("pub fn into_shared"));
    assert!(model_source.contains("RepairedBorrowed"));
    assert!(model_source.contains("RepairedShared"));
    assert!(page_tree_source.contains("AttestedRevisionIndexOwner::RepairedBorrowed"));
    assert!(page_tree_source.contains("AttestedRevisionIndexOwner::RepairedShared"));
    assert!(outline_source.contains("AttestedRevisionIndexOwner::RepairedBorrowed"));
    assert!(outline_source.contains("AttestedRevisionIndexOwner::RepairedShared"));
    assert!(open_source.contains("OpenLocallyRepairedBaseRevisionJob"));
    assert!(open_source.contains("new_with_parent_caps"));
    assert!(!open_source.contains("pub fn plan("));

    let feature = record_with_id(&feature_map, "feature", "core.local-repair")
        .expect("local-repair feature record must exist");
    for required in [
        "state = \"REFERENCE\"",
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
        "repaired proof separately owns core page-count/outline jobs without losing R1 provenance",
        "source-acquired document owner now closes",
        "strict R0 and bounded local R1 at REFERENCE",
        "status = \"covered\"",
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
    assert_eq!(top_level_version(&feature_map), Some("0.68.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.68.0"));

    let feature = record_with_id(&feature_map, "feature", "core.strict-page-count")
        .expect("strict page-count feature record must exist");
    for required in [
        "state = \"DIFFERENTIAL\"",
        "profile = \"m1.strict-page-count.v1\"",
        "ISO-32000-1:2008/7.7.3",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/11.5-11.7",
        "RPE-ARCH-001/15.3/M1",
        "modules = [\"core/document\"]",
        "core/document::page_tree_count",
        "core/document::page_tree_limit_config",
        "core/document::repository_policy",
        "core/document::local_repair_open",
        "core/document::acquired_services",
        "tools/quality::native_object_loop",
        "tools/quality::m1_document_service_differential",
        "tools/quality::m1_document_service_maturity",
        "tools/quality::m1_document_service_fuzz",
        "fuzz_targets = [\"fuzz.m1documentservices\"]",
        "benchmarks = [\"benchmark.m1-native-document-services\"]",
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
        "source-acquired document owner now lends",
        "traditional, primary xref-stream",
        "lazy PageIndex",
        "page_count=1",
        "pages_processed=1",
        "rather than a registered page-count differential",
        "not a registered baseline or correctness oracle",
        "Registered project-owned DIFFERENTIAL promotion is complete at bounded M1 scale",
        "sealed cloneable strict or locally repaired handles",
        "repaired ownership retains the complete xref/object diagnostic ledger",
        "bounded M1 Session continues to select strict base opening",
        "contributes to covered M1 exit but does not claim M2",
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
fn traceability_registers_strict_outline_as_a_bounded_differential_profile() {
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
        "state = \"DIFFERENTIAL\"",
        "profile = \"m1.strict-outline.v1\"",
        "ISO-32000-1:2008/7.3.9",
        "ISO-32000-1:2008/7.3.10",
        "ISO-32000-1:2008/7.7.2",
        "ISO-32000-1:2008/12.3.3",
        "ISO-32000-1:2008/7.9.2.2",
        "ISO-32000-1:2008/D.3",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/11.5-11.7",
        "RPE-ARCH-001/15.3/M1",
        "modules = [\"core/document\"]",
        "core/document::outline",
        "core/document::outline_limit_config",
        "core/document::repository_policy",
        "core/document::local_repair_open",
        "core/document::acquired_services",
        "tools/quality::m1_document_service_differential",
        "tools/quality::m1_document_service_maturity",
        "tools/quality::m1_document_service_fuzz",
        "fuzz_targets = [\"fuzz.m1documentservices\"]",
        "benchmarks = [\"benchmark.m1-native-document-services\"]",
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
        "baseline-ledger registration and platform-enforced isolation remain open",
        "project-owned O0/O1/O2, disjoint holdout, fuzz/minimizer, benchmark, fingerprint, and full-session graph",
        "contributes to bounded M1 exit without claiming ISO conformance or M2",
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
        "Registered project-owned DIFFERENTIAL promotion is complete at bounded M1 scale",
        "contributes to covered M1 exit but does not claim M2",
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
