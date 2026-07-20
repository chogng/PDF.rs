#[test]
fn public_facade_does_not_transparently_reexport_implementation_crates() {
    let facade = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("read public facade source");

    assert!(!facade.contains("pub use pdf_rs_skia_core::*"));
    assert!(!facade.contains("pub use pdf_rs_skia_cpu::*"));
    assert!(!facade.contains("DisplayList"));
    assert!(!facade.contains("DrawCommand"));
    assert!(!facade.contains("ImageId"));
    assert!(!facade.contains("PathId"));
    assert!(!facade.contains("GlyphRunId"));
}
