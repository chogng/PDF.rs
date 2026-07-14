use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn product_object_core_only_depends_on_bytes_and_syntax_and_has_no_platform_io() {
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
        "core/object may depend only on the lower-level core/bytes and core/syntax crates"
    );
    assert!(
        !manifest.contains("pdf-rs-xref"),
        "core/object must remain a sibling of core/xref rather than depending on it"
    );
    assert!(
        !manifest.contains("[dev-dependencies]"),
        "core/object must not introduce development dependencies"
    );

    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources);
    sources.sort();
    assert!(
        !sources.is_empty(),
        "core/object source selection is non-empty"
    );

    let forbidden = [
        "std::fs",
        "std::net",
        "async fn",
        "tokio",
        "async_std",
        "reqwest",
        "hyper",
        "ureq",
        "pdfium",
        "mupdf",
        "poppler",
        "ghostscript",
        "pdf.js",
        "pdf_rs_xref",
        "unsafe fn",
        "unsafe impl",
        "unsafe {",
        "#[allow(unsafe_code)]",
        "#![allow(unsafe_code)]",
        "extern \"c\"",
    ];
    let mut forbids_unsafe = false;
    for path in sources {
        let source = fs::read_to_string(&path).expect("selected Rust source must be readable");
        let lowercase = source.to_ascii_lowercase();
        forbids_unsafe |= lowercase.contains("#![forbid(unsafe_code)]");
        for token in forbidden {
            assert!(
                !lowercase.contains(token),
                "forbidden product object-core token {token:?} in {}",
                path.display()
            );
        }
    }
    assert!(
        forbids_unsafe,
        "core/object must forbid unsafe code at its crate boundary"
    );
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
