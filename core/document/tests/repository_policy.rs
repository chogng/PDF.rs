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
    assert_eq!(top_level_version(&feature_map), Some("0.24.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.24.0"));

    let feature = record_with_id(&feature_map, "feature", "core.strict-page-count")
        .expect("strict page-count feature record must exist");
    for required in [
        "profile = \"m1.strict-page-count.v1\"",
        "RPE-ARCH-001/5.8-5.9",
        "modules = [\"core/document\"]",
        "core/document::page_tree_count",
        "core/document::page_tree_limit_config",
        "core/document::repository_policy",
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
        "tools/quality::native_object_loop",
        "open-addressing table",
        "exact Parent back-links",
        "never uses untrusted Count or Kids data for allocation",
        "does not implement revision chains",
        "reusable lazy PageIndex",
        "page_count=1",
        "pages_processed=1",
        "not a registered semantic differential",
        "Native/PDFium differential evidence",
        "does not claim M1 or M2 exit",
        "status = \"partial\"",
    ] {
        assert!(
            requirement.contains(required),
            "requirement must contain {required:?}"
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
