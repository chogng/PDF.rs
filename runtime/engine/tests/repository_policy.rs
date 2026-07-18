use std::fs;
use std::path::Path;

#[test]
fn native_worker_has_no_external_engine_or_platform_io_dependency() {
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    for forbidden in [
        "pdfium",
        "mupdf",
        "poppler",
        "lopdf",
        "reqwest",
        "tokio",
        "libloading",
    ] {
        assert!(
            !manifest.to_ascii_lowercase().contains(forbidden),
            "forbidden dependency marker: {forbidden}"
        );
    }

    let registry =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/registry.rs")).unwrap();
    for forbidden in ["std::fs", "std::net", "Command::new", "unsafe {"] {
        assert!(
            !registry.contains(forbidden),
            "forbidden runtime boundary: {forbidden}"
        );
    }
}
