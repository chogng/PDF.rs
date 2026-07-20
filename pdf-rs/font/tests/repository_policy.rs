use std::fs;
use std::path::{Path, PathBuf};

fn collect_rust_sources(root: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("font source directory is readable") {
        let path = entry.expect("font source entry is readable").path();
        if path.is_dir() {
            collect_rust_sources(&path, paths);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            paths.push(path);
        }
    }
}

#[test]
fn font_core_is_pure_rust_and_has_no_platform_or_external_engine_dependency() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("font manifest is readable");
    let dependencies = manifest
        .split_once("[dependencies]")
        .expect("font manifest has an explicit dependency table")
        .1
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert!(
        dependencies.is_empty(),
        "font core has no product dependencies"
    );
    assert!(!manifest.contains("[build-dependencies]"));
    assert!(!manifest.contains("[target."));

    let mut paths = Vec::new();
    collect_rust_sources(&root.join("src"), &mut paths);
    paths.sort();
    let sources = paths
        .iter()
        .map(|path| fs::read_to_string(path).expect("font source is readable"))
        .collect::<Vec<_>>()
        .join("\n");
    let lowercase = sources.to_ascii_lowercase();
    for forbidden in [
        "unsafe {",
        "unsafe fn",
        "unsafe impl",
        "extern \"c\"",
        "std::fs",
        "std::net",
        "std::process",
        "fontconfig",
        "freetype",
        "coretext",
        "directwrite",
        "harfbuzz",
        "pdfium",
        "mupdf",
        "poppler",
    ] {
        assert!(
            !lowercase.contains(forbidden),
            "font product source contains forbidden token {forbidden:?}"
        );
    }
    assert!(sources.contains("#![forbid(unsafe_code)]"));
    assert!(sources.contains("#![deny(missing_docs)]"));
}
