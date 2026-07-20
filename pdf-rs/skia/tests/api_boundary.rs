#[test]
fn pdf_adapter_uses_only_the_public_skia_facade() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read PDF-to-Skia adapter manifest");

    assert!(manifest.contains("pdf-rs-skia ="));
    assert!(!manifest.contains("pdf-rs-skia-core"));
    assert!(!manifest.contains("pdf-rs-skia-cpu"));
    assert!(!manifest.contains("pdf-rs-skia-gpu"));
    assert!(!manifest.contains("pdf-rs-skia-image"));
    assert!(!manifest.contains("pdf-rs-skia-text"));

    let adapter = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("read PDF-to-Skia adapter source");
    for internal_crate in [
        "pdf_rs_skia_core::",
        "pdf_rs_skia_cpu::",
        "pdf_rs_skia_gpu::",
        "pdf_rs_skia_image::",
        "pdf_rs_skia_text::",
    ] {
        assert!(
            !adapter.contains(internal_crate),
            "PDF adapter must not import Skia internal crate {internal_crate}"
        );
    }

    for lower_execution_api in [
        "Canvas",
        "Surface",
        "ClipRect",
        "DisplayList",
        "DisplayListBuilder",
        "GpuCommandEncoder",
        "GpuBackend",
    ] {
        assert!(
            !adapter.contains(lower_execution_api),
            "PDF adapter must describe rendering intent rather than invoke lower Skia API {lower_execution_api}"
        );
    }
}
