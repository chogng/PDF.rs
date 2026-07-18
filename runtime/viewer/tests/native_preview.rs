use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

use pdf_rs_viewer::{NativeDocument, NativeRendererKind, NativeViewerErrorCode};

const MIXED_PDF: &[u8] =
    include_bytes!("../../../tests/cases/raster/m3-reference/valid-mixed/input.pdf");
const READABLE_PDF: &[u8] = include_bytes!("../../../tests/desktop/readable-preview.pdf");
// These are coarse regression bounds for the actual Electron canvas sizes, not
// a claim that the two independently sampled rasterizers are byte-identical.
const ELECTRON_RENDER_WIDTHS: [u32; 2] = [384, 480];
const ELECTRON_MAXIMUM_CHANNEL_DELTA: u8 = 96;
const ELECTRON_CHANGED_CHANNEL_DIVISOR: usize = 16;

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
fn cancelled_render_never_publishes_a_surface() {
    let mut document = NativeDocument::open(MIXED_PDF.to_vec()).expect("strict Native open");
    let cancellation = AtomicBool::new(true);
    let error = document
        .render_page_with_renderer_and_cancellation(
            0,
            128,
            NativeRendererKind::ReferenceCpu,
            &cancellation,
        )
        .expect_err("pre-cancelled render cannot publish");
    assert_eq!(error.code(), NativeViewerErrorCode::Cancelled);
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

#[test]
fn readable_page_renders_through_explicit_fast_cpu_qualification_path() {
    let mut document = NativeDocument::open(READABLE_PDF.to_vec()).expect("strict Native open");
    let surface = document
        .render_page_with_renderer(0, 306, NativeRendererKind::FastCpu)
        .expect("readable page renders through Fast CPU");
    assert_eq!(surface.page_index(), 0);
    assert_eq!(surface.renderer(), NativeRendererKind::FastCpu);
    assert_eq!(surface.width(), 306);
    assert_eq!(surface.height(), 396);
    assert_eq!(surface.stride(), 1_224);
    assert!(
        surface
            .pixels()
            .chunks_exact(4)
            .any(|pixel| pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 100),
        "Fast CPU page must retain dark text pixels"
    );
    let reference = document
        .render_page_with_renderer(0, 306, NativeRendererKind::ReferenceCpu)
        .expect("readable page renders through Reference CPU");
    let maximum_delta = surface
        .pixels()
        .iter()
        .zip(reference.pixels())
        .map(|(fast, reference)| fast.abs_diff(*reference))
        .max()
        .expect("surface is nonempty");
    let changed_channels = surface
        .pixels()
        .iter()
        .zip(reference.pixels())
        .filter(|(fast, reference)| fast != reference)
        .count();
    assert!(
        maximum_delta <= 64,
        "Fast coverage must stay within the reviewed readable-page channel bound: {maximum_delta}"
    );
    assert!(
        changed_channels <= 12_500,
        "Fast coverage changed too much of the readable page: {changed_channels} channels"
    );
}

#[test]
fn electron_readable_pages_stay_within_bounded_fast_reference_difference() {
    let mut document = NativeDocument::open(READABLE_PDF.to_vec()).expect("strict Native open");

    for page in 0..document.page_count() {
        for width in ELECTRON_RENDER_WIDTHS {
            let fast = document
                .render_page_with_renderer(page, width, NativeRendererKind::FastCpu)
                .expect("Electron-sized page renders through Fast CPU");
            let reference = document
                .render_page_with_renderer(page, width, NativeRendererKind::ReferenceCpu)
                .expect("Electron-sized page renders through Reference CPU");

            assert_eq!(fast.width(), reference.width());
            assert_eq!(fast.height(), reference.height());
            assert_eq!(fast.stride(), reference.stride());
            assert_eq!(fast.pixels().len(), reference.pixels().len());

            let mut maximum_delta = 0_u8;
            let mut changed_channels = 0_usize;
            let mut total_delta = 0_u64;
            for (index, (fast, reference)) in
                fast.pixels().iter().zip(reference.pixels()).enumerate()
            {
                let delta = fast.abs_diff(*reference);
                maximum_delta = maximum_delta.max(delta);
                changed_channels += usize::from(delta != 0);
                total_delta += u64::from(delta);
                if index % 4 == 3 {
                    assert_eq!(
                        delta, 0,
                        "page {page} width {width} changed an alpha channel"
                    );
                }
            }

            assert!(
                maximum_delta <= ELECTRON_MAXIMUM_CHANNEL_DELTA,
                "page {page} width {width} channel delta exceeded the bounded Electron regression envelope: {maximum_delta}"
            );
            assert!(
                changed_channels <= fast.pixels().len() / ELECTRON_CHANGED_CHANNEL_DIVISOR,
                "page {page} width {width} changed too many channels: {changed_channels}"
            );
            assert!(
                total_delta <= u64::try_from(fast.pixels().len()).expect("surface length fits u64"),
                "page {page} width {width} exceeded mean absolute channel delta 1: {total_delta}"
            );
        }
    }
}
