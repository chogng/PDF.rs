use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn product_xref_core_only_depends_on_bytes_and_syntax_and_has_no_platform_io() {
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
        "core/xref may depend only on the lower-level core/bytes and core/syntax crates"
    );
    assert!(
        !manifest.contains("[dev-dependencies]"),
        "core/xref must not introduce development dependencies"
    );

    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources);
    sources.sort();
    assert!(
        !sources.is_empty(),
        "core/xref source selection is non-empty"
    );

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
    ];
    for path in sources {
        let source = fs::read_to_string(&path).expect("selected Rust source must be readable");
        let lowercase = source.to_ascii_lowercase();
        for token in forbidden {
            assert!(
                !lowercase.contains(token),
                "forbidden product xref-core token {token:?} in {}",
                path.display()
            );
        }
    }
}

#[test]
fn traceability_maps_are_versioned_together_and_register_xref() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/xref has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable during repository tests");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec traceability map must be readable during repository tests");

    assert_eq!(top_level_version(&feature_map), Some("0.14.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.14.0"));
    assert_eq!(
        top_level_version(&feature_map),
        top_level_version(&spec_map),
        "feature and specification maps must advance as one semantic traceability version"
    );

    let feature = record_with_id(&feature_map, "feature", "core.traditional-xref")
        .expect("the traditional-xref feature record must exist");
    assert!(feature.contains("profile = \"m1.traditional-xref.v1\""));
    assert!(feature.contains("modules = [\"core/xref\"]"));
    assert!(feature.contains("core/xref::traditional_xref"));
    assert!(feature.contains("core/xref::limit_config"));
    assert!(feature.contains("core/xref::source_error_policy"));
    assert!(feature.contains("core/xref::repository_policy"));

    let requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.4")
        .expect("the traditional-xref architecture requirement record must exist");
    assert!(requirement.contains("\"core.traditional-xref\""));
    assert!(requirement.contains("\"core/xref\""));
    assert!(requirement.contains("core/xref::traditional_xref"));
    assert!(requirement.contains("core/xref::limit_config"));
    assert!(requirement.contains("core/xref::source_error_policy"));
    assert!(requirement.contains("core/xref::repository_policy"));
    assert!(requirement.contains("tools/quality::native_object_loop"));
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
