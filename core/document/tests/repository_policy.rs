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
        "upper-half-before-lower delivery",
        "cancellation in both child layers",
        "owner-mediated generation validation",
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
    assert_eq!(top_level_version(&feature_map), Some("0.36.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.36.0"));

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
