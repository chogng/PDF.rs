use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
        "not a persistent",
        "No PDFium",
    ] {
        assert!(
            provenance.contains(required),
            "provenance must declare {required:?}"
        );
    }
}
