use std::fs;
use std::path::PathBuf;

#[test]
fn content_crate_has_only_the_approved_pure_dependency() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read manifest");
    assert!(manifest.contains("pdf-rs-syntax"));
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
    for entry in fs::read_dir(root).expect("read src") {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read source");
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
}
