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
fn content_crate_has_only_the_approved_pure_dependency() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read manifest");
    let dependency_body = manifest
        .split_once("[dependencies]")
        .expect("content manifest declares an explicit dependency table")
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
        [r#"pdf-rs-syntax = { path = "../syntax" }"#]
    );
    for forbidden_table in ["[dev-dependencies]", "[build-dependencies]", "[target."] {
        assert!(
            !manifest.contains(forbidden_table),
            "core/content must not declare {forbidden_table} dependencies"
        );
    }
    for forbidden in [
        "pdf-rs-document",
        "pdf-rs-scene",
        "pdfium",
        "mupdf",
        "poppler",
        "reqwest",
        "tokio",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "forbidden dependency marker: {forbidden}"
        );
    }
}

#[test]
fn product_sources_exclude_unsafe_and_platform_io() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut joined = String::new();
    for entry in fs::read_dir(root).expect("read src") {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read source");
        joined.push_str(&source);
        joined.push('\n');
        for forbidden in [
            "unsafe {",
            "std::fs",
            "std::net",
            "std::process",
            "File::open",
            "TcpStream",
            "Command::new",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} contains forbidden marker {forbidden}",
                path.display()
            );
        }
    }
    assert!(joined.contains("#![forbid(unsafe_code)]"));
    assert!(joined.contains("#![deny(missing_docs)]"));
}

#[test]
fn m2_operator_scanner_is_pure_traceable_and_not_a_content_vm() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/content has a repository root two levels above it");
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("content library source is readable");
    let model =
        fs::read_to_string(crate_root.join("src/model.rs")).expect("content model is readable");
    let scanner =
        fs::read_to_string(crate_root.join("src/scanner.rs")).expect("scanner is readable");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance is readable");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature map is readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec map is readable");
    let plan =
        fs::read_to_string(repository_root.join("plan/m2.toml")).expect("M2 plan is readable");

    assert_eq!(top_level_version(&feature_map), Some("0.67.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.67.0"));
    for required in [
        "Pure, bounded scanning of already-decoded PDF page content streams",
        "pub struct DecodedContentStream",
        "pub struct ContentProgram",
        "pub struct ContentScanStats",
        "pub enum OperatorKind",
        "pub struct OperatorSpec",
        "pub enum ContentOperator",
        "Unknown(Vec<u8>)",
        "pub struct ContentOperatorSource",
    ] {
        assert!(
            library.contains(required) || model.contains(required),
            "content scanner boundary must contain {required:?}"
        );
    }
    for required in [
        "pub struct ContentScanJob",
        "pub enum ContentScanPoll",
        "pub fn scan_content_streams(",
        "ContentErrorCode::InvalidStreamOrder",
        "ContentErrorCode::DanglingOperands",
        "ContentLimitKind::TotalDecodedBytes",
        "ContentLimitKind::Tokens",
        "ContentLimitKind::TokenBytes",
        "ContentLimitKind::OperandsPerOperator",
        "ContentLimitKind::NestingDepth",
        "ContentLimitKind::Operators",
        "ContentLimitKind::Fuel",
        "ContentLimitKind::RetainedBytes",
    ] {
        assert!(
            scanner.contains(required),
            "content scanner implementation must contain {required:?}"
        );
    }
    for required in [
        "performs no content-stream acquisition",
        "filter decoding",
        "resource lookup",
        "graphics/text interpretation",
        "Scene construction",
        "Known operator arity and context are declarative only",
        "belong to M2-06",
    ] {
        assert!(
            provenance.contains(required),
            "content provenance must preserve boundary {required:?}"
        );
    }

    let feature = record_with_id(&feature_map, "feature", "core.content-operator-scanner")
        .expect("operator scanner feature is registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.content-operator-scanner.v1\"",
        "ISO-32000-1:2008/7.8.2",
        "RPE-ARCH-001/4.3-4.5",
        "RPE-ARCH-001/6.1-6.2",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"core/content\"]",
        "core/content::scanner",
        "core/content::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "operator-scanner feature must contain {required:?}"
        );
    }

    let content_stream = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/7.8.2")
        .expect("content-stream requirement is registered");
    for required in [
        "core.page-content-acquisition",
        "core.content-operator-scanner",
        "core/document::page_content",
        "core/content::scanner",
        "strict attested proof",
        "does not support locally repaired or acquired revision-chain authorities",
        "stream boundaries as semantic whitespace",
        "unknown operators distinct from malformed content",
        "does not frame inline images",
        "M2-06 owns those VM semantics",
    ] {
        assert!(
            content_stream.contains(required),
            "content-stream mapping must contain {required:?}"
        );
    }

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M2")
        .expect("M2 requirement is registered");
    for required in [
        "core.content-operator-scanner",
        "core/content::scanner",
        "M2-05 is complete as two bounded PLANNED profiles",
        "unknown legal operators distinct from malformed syntax",
        "does not enforce known-operator arity or structural context",
        "M2-06 Content VM execution",
        "M2-07 registered normative Scene exit evidence remain open",
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
    assert!(m2_06.contains("status = \"planned\""));
    let m2_07 = record_with_id(&plan, "work_item", "M2-07").expect("M2-07 work item exists");
    assert!(m2_07.contains("status = \"planned\""));
}
