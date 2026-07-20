#[test]
fn pdf_adapter_uses_only_the_public_skia_facade() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read PDF-to-Skia adapter manifest");

    assert!(manifest.contains("pdf-rs-skia ="));
    assert!(!manifest.contains("pdf-rs-skia-core"));
    assert!(!manifest.contains("pdf-rs-skia-cpu"));
    assert!(!manifest.contains("pdf-rs-skia-gpu"));
    assert!(!manifest.contains("pdf-rs-skia-text"));
}
