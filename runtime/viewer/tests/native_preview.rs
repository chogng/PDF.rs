use pdf_rs_viewer::NativeDocument;

const MIXED_PDF: &[u8] =
    include_bytes!("../../../tests/cases/raster/m3-reference/valid-mixed/input.pdf");

#[test]
fn strict_pdf_rs_pipeline_renders_real_mixed_page_content() {
    let mut document = NativeDocument::open(MIXED_PDF.to_vec()).expect("strict Native open");
    assert_eq!(document.page_count(), 1);

    let surface = document
        .render_page(0, 128)
        .expect("registered mixed page renders");
    assert_eq!(surface.page_index(), 0);
    assert_eq!(surface.width(), 128);
    assert_eq!(surface.height(), 128);
    assert_eq!(surface.stride(), 512);
    assert_eq!(surface.pixels().len(), 128 * 128 * 4);
    assert!(
        surface
            .pixels()
            .chunks_exact(4)
            .any(|pixel| pixel != [255, 255, 255, 255]),
        "the PDF's path, image, and glyph content must produce non-white pixels"
    );
}

#[test]
fn page_and_output_bounds_fail_closed() {
    let mut document = NativeDocument::open(MIXED_PDF.to_vec()).expect("strict Native open");
    assert!(document.render_page(1, 128).is_err());
    assert!(document.render_page(0, 0).is_err());
    assert!(document.render_page(0, 4_097).is_err());
}
