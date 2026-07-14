use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repository_root() -> PathBuf {
    crate_root()
        .parent()
        .and_then(Path::parent)
        .expect("runtime/cache has a repository root two levels above it")
        .to_path_buf()
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

fn rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("source directory must be readable") {
        let path = entry.expect("source entry must be readable").path();
        if path.is_dir() {
            rust_sources(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

#[test]
fn product_source_remains_runtime_owner_code_without_io_async_or_external_engines() {
    let mut sources = Vec::new();
    rust_sources(&crate_root().join("src"), &mut sources);
    sources.sort();
    assert!(!sources.is_empty());

    let forbidden = [
        "std::fs", "std::net", "tokio", "async fn", "reqwest", "unsafe {", "pdfium", "PDFium",
    ];
    for source in sources {
        let text = fs::read_to_string(&source).expect("product source must be UTF-8");
        for token in forbidden {
            assert!(
                !text.contains(token),
                "{} contains forbidden product token {token:?}",
                source.display()
            );
        }
    }
}

#[test]
fn product_dependencies_are_only_the_declared_in_repository_core_layers() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml"))
        .expect("cache manifest must be readable");
    let dependency_block = manifest
        .split("[dependencies]")
        .nth(1)
        .and_then(|text| text.split("[dev-dependencies]").next())
        .expect("manifest must contain product dependencies");
    let names: BTreeSet<_> = dependency_block
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "pdf-rs-bytes",
            "pdf-rs-document",
            "pdf-rs-object",
            "pdf-rs-syntax",
        ])
    );
}

#[test]
fn provenance_declares_session_scope_and_non_persistence() {
    let provenance = fs::read_to_string(crate_root().join("PROVENANCE.md"))
        .expect("cache provenance must be readable");
    for required in [
        "session-scoped",
        "complete keys",
        "deterministic eviction",
        "ReadySessionOwner",
        "not a persistent",
        "No PDFium",
    ] {
        assert!(
            provenance.contains(required),
            "provenance must declare {required:?}"
        );
    }
}

#[test]
fn traceability_registers_cache_semantics_and_owner_integration_directly() {
    let root = repository_root();
    let feature_map = fs::read_to_string(root.join("docs/traceability/feature-map.toml"))
        .expect("feature map must be readable");
    let spec_map = fs::read_to_string(root.join("docs/traceability/spec-map.toml"))
        .expect("spec map must be readable");
    assert_eq!(top_level_version(&feature_map), Some("0.25.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.25.0"));

    let feature = record_with_id(&feature_map, "feature", "runtime.session-ready-store")
        .expect("session Ready-store feature must exist");
    for required in [
        "profile = \"m1.session-ready-store.v1\"",
        "modules = [\"runtime/cache\"]",
        "runtime/cache::ready_store",
        "runtime/cache::repository_policy",
        "tools/quality::native_object_loop",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "feature must contain {required:?}"
        );
    }

    let requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/9.1")
        .expect("Document actor requirement must exist");
    for required in [
        "runtime.session-ready-store",
        "runtime/cache::ready_store",
        "runtime/cache::repository_policy",
        "exact-key borrowed warm hit",
        "complete-key",
        "deterministic bounded LRU",
        "close report exactly matches the admitted resident total",
        "Persistent caching",
        "cross-session reuse",
        "Native/PDFium semantic or pixel differential",
    ] {
        assert!(
            requirement.contains(required),
            "requirement must contain {required:?}"
        );
    }
}
