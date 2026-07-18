use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn product_dependencies_are_only_bytes_and_policy() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest =
        fs::read_to_string(crate_root.join("Cargo.toml")).expect("manifest must be readable");
    let dependencies = dependency_names(&manifest, "[dependencies]");
    assert_eq!(
        dependencies,
        BTreeSet::from(["pdf-rs-bytes", "pdf-rs-policy"])
    );
    assert_eq!(
        dependency_names(&manifest, "[dev-dependencies]"),
        BTreeSet::from(["pdf-rs-scene", "pdf-rs-syntax"])
    );
    for forbidden_table in ["[build-dependencies]", "[target."] {
        assert!(
            !manifest.contains(forbidden_table),
            "tile cache must not declare {forbidden_table} dependencies"
        );
    }

    let mut paths = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut paths);
    let sources = paths
        .iter()
        .map(|path| fs::read_to_string(path).expect("source must be readable"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    for forbidden in [
        "unsafe {",
        "unsafe fn",
        "extern \"c\"",
        "std::fs",
        "std::net",
        "std::process",
        "pdfium",
        "mupdf",
        "poppler",
        "core_graphics",
        "wasm_bindgen",
        "web_sys",
    ] {
        assert!(
            !sources.contains(forbidden),
            "tile cache source contains forbidden token {forbidden:?}"
        );
    }
}

fn dependency_names<'a>(manifest: &'a str, table: &str) -> BTreeSet<&'a str> {
    manifest
        .split_once(table)
        .unwrap_or_else(|| panic!("manifest must declare {table}"))
        .1
        .split("\n[")
        .next()
        .expect("dependency table body exists")
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty())
        .collect()
}

fn collect_rust_sources(directory: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("source directory must be readable") {
        let path = entry.expect("source entry must be readable").path();
        if path.is_dir() {
            collect_rust_sources(&path, paths);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            paths.push(path);
        }
    }
}
