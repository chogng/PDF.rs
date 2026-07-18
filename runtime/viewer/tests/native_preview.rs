use std::collections::BTreeSet;

use pdf_rs_viewer::{NativeDocument, NativeRendererKind};

const MIXED_PDF: &[u8] =
    include_bytes!("../../../tests/cases/raster/m3-reference/valid-mixed/input.pdf");
const READABLE_PDF: &[u8] = include_bytes!("../../../tests/desktop/readable-preview.pdf");

#[test]
fn strict_pdf_rs_pipeline_renders_real_mixed_page_content() {
    let mut document = NativeDocument::open(MIXED_PDF.to_vec()).expect("strict Native open");
    assert_eq!(document.page_count(), 1);

    let surface = document
        .render_page(0, 128)
        .expect("registered mixed page renders");
    assert_eq!(surface.page_index(), 0);
    assert_eq!(surface.renderer(), NativeRendererKind::ReferenceCpu);
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

#[test]
fn readable_two_page_document_renders_text_and_layout() {
    let mut document = NativeDocument::open(READABLE_PDF.to_vec()).expect("strict Native open");
    assert_eq!(document.page_count(), 2);

    for page in 0..2 {
        let surface = document
            .render_page(page, 306)
            .expect("readable page renders");
        assert_eq!(surface.page_index(), page);
        assert_eq!(surface.renderer(), NativeRendererKind::ReferenceCpu);
        assert_eq!(surface.width(), 306);
        assert_eq!(surface.height(), 396);
        assert_eq!(surface.stride(), 1_224);
        assert_eq!(surface.pixels().len(), 306 * 396 * 4);

        let colors = surface
            .pixels()
            .chunks_exact(4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            .collect::<BTreeSet<_>>();
        assert!(
            colors.len() >= 8,
            "page {page} must retain its text, cards, rules, and background colors"
        );
        assert!(
            colors.contains(&[255, 255, 255, 255]),
            "page {page} must contain white text or paper pixels"
        );
        assert!(
            colors
                .iter()
                .any(|color| color[0] < 80 && color[1] < 80 && color[2] < 100),
            "page {page} must contain dark text pixels"
        );
    }
}
