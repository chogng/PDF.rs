use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn product_filters_only_depend_on_bytes_and_syntax_and_have_no_platform_io() {
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
        ],
        "core/filters may depend only on lower-level bytes and syntax primitives"
    );
    assert!(
        !manifest.contains("[dev-dependencies]"),
        "core/filters must not introduce development dependencies"
    );

    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources);
    sources.sort();
    assert!(!sources.is_empty(), "filter source selection is non-empty");
    let forbidden = [
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
        "flate2",
        "weezl",
        "openssl",
    ];
    for path in sources {
        let source = fs::read_to_string(&path).expect("selected Rust source must be readable");
        let lowercase = source.to_ascii_lowercase();
        for token in forbidden {
            assert!(
                !lowercase.contains(token),
                "forbidden product filter token {token:?} in {}",
                path.display()
            );
        }
    }
}

#[test]
fn decoded_products_are_sealed_non_clone_and_identity_is_not_a_public_filter() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let model = fs::read_to_string(crate_root.join("src/model.rs")).unwrap();
    assert!(!model.contains("impl Clone for DecodeAttestation"));
    assert!(!model.contains("impl Clone for DecodedStream"));
    assert!(!model.contains("#[derive(Clone)]\npub struct DecodeAttestation"));
    assert!(!model.contains("#[derive(Clone)]\npub struct DecodedStream"));

    let public_filter = model
        .split_once("pub enum StreamFilter")
        .unwrap()
        .1
        .split_once("\n}\n\nimpl StreamFilter")
        .unwrap()
        .0;
    assert!(
        !public_filter.contains("Identity"),
        "identity must remain an internal empty-plan path"
    );
    for declaration in [
        "pub struct DecodeAttestation {",
        "pub struct DecodedStream {",
    ] {
        let body = model
            .split_once(declaration)
            .unwrap()
            .1
            .split_once("\n}")
            .unwrap()
            .0;
        assert!(
            !body
                .lines()
                .any(|line| line.trim_start().starts_with("pub ")),
            "sealed decoded-product fields must not be public"
        );
    }
}

#[test]
fn traceability_registers_basic_filters_without_claiming_stream_integration() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/filters has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml")).unwrap();
    let spec_map =
        fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml")).unwrap();
    assert_eq!(top_level_version(&feature_map), Some("0.51.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.51.0"));

    let feature = record_with_id(&feature_map, "feature", "core.stream-filter-decode")
        .expect("basic stream-filter feature must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.basic-stream-filters.v1\"",
        "RPE-ARCH-001/5.6",
        "modules = [\"core/filters\"]",
        "core/filters::decode_behavior",
        "core/filters::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "feature must contain {required:?}"
        );
    }

    let requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.6")
        .expect("stream-filter requirement must exist");
    for required in [
        "sealed evidence",
        "ASCIIHexDecode",
        "ASCII85Decode",
        "RunLengthDecode",
        "The object and xref layers do not yet",
        "Flate, predictors, LZW",
        "feature is PLANNED",
        "does not claim general stream support or M1 exit",
        "status = \"partial\"",
    ] {
        assert!(
            requirement.contains(required),
            "requirement must contain {required:?}"
        );
    }
}

fn collect_rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory).expect("source directory must be readable");
    for entry in entries {
        let entry = entry.expect("source directory entry must be readable");
        let path = entry.path();
        let file_type = entry
            .file_type()
            .expect("source file type must be readable");
        if file_type.is_dir() {
            collect_rust_sources(&path, output);
        } else if file_type.is_file() && path.extension().is_some_and(|value| value == "rs") {
            output.push(path);
        }
    }
}

fn top_level_version(input: &str) -> Option<&str> {
    input.lines().find_map(|line| {
        line.trim()
            .strip_prefix("version = \"")
            .and_then(|value| value.strip_suffix('"'))
    })
}

fn record_with_id<'a>(input: &'a str, table: &str, identity: &str) -> Option<&'a str> {
    let marker = format!("[[{table}]]");
    input.split(&marker).skip(1).find(|record| {
        record
            .lines()
            .any(|line| line.trim() == format!("id = \"{identity}\""))
    })
}
