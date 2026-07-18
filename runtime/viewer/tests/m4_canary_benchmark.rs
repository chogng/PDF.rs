use std::hint::black_box;
use std::time::Instant;

use pdf_rs_viewer::{NativeDocument, NativePageSurface, NativeRendererKind};

const READABLE_PDF: &[u8] = include_bytes!("../../../tests/desktop/readable-preview.pdf");
const SAMPLE_COUNT: usize = 21;
const PREVIEW_WIDTH: u32 = 153;
const VIEWPORT_WIDTH: u32 = 306;

#[test]
#[ignore = "captures local release-profile M4 canary measurements"]
fn captures_fixed_scope_native_viewer_samples() {
    let fast_preview = samples(|| first_preview(NativeRendererKind::FastCpu));
    let reference_preview = samples(|| first_preview(NativeRendererKind::ReferenceCpu));
    let fast_viewport = samples(|| first_full_viewport(NativeRendererKind::FastCpu));
    let reference_viewport = samples(|| first_full_viewport(NativeRendererKind::ReferenceCpu));

    compare_readable_pixels(PREVIEW_WIDTH);
    compare_readable_pixels(VIEWPORT_WIDTH);

    print_samples("fast-cpu-v1", "first_preview_ns", &fast_preview);
    print_samples("reference-cpu-v1", "first_preview_ns", &reference_preview);
    print_samples("fast-cpu-v1", "first_full_viewport_ns", &fast_viewport);
    print_samples(
        "reference-cpu-v1",
        "first_full_viewport_ns",
        &reference_viewport,
    );
    println!(
        "m4-benchmark output_memory_bytes preview={} full_viewport={} sample_count={SAMPLE_COUNT}",
        surface_bytes(PREVIEW_WIDTH, 198),
        surface_bytes(VIEWPORT_WIDTH, 396),
    );
}

fn samples(mut render: impl FnMut() -> NativePageSurface) -> Vec<u64> {
    let warmup = render();
    assert_surface(&warmup);
    black_box(pixel_fingerprint(warmup.pixels()));

    let mut samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut fingerprint = None;
    for _ in 0..SAMPLE_COUNT {
        let start = Instant::now();
        let surface = render();
        let elapsed = u64::try_from(start.elapsed().as_nanos()).expect("sample fits u64");
        assert_surface(&surface);
        let observed = pixel_fingerprint(surface.pixels());
        assert_eq!(
            *fingerprint.get_or_insert(observed),
            observed,
            "every timed render must publish identical pixels"
        );
        black_box(observed);
        assert!(elapsed > 0);
        samples.push(elapsed);
    }
    samples
}

fn first_preview(renderer: NativeRendererKind) -> NativePageSurface {
    let mut document =
        NativeDocument::open(READABLE_PDF.to_vec()).expect("benchmark strict Native open");
    document
        .render_page_with_renderer(0, PREVIEW_WIDTH, renderer)
        .expect("benchmark first preview")
}

fn first_full_viewport(renderer: NativeRendererKind) -> NativePageSurface {
    let mut document =
        NativeDocument::open(READABLE_PDF.to_vec()).expect("benchmark strict Native open");
    let preview = document
        .render_page_with_renderer(0, PREVIEW_WIDTH, renderer)
        .expect("benchmark untimed preview warmup");
    assert_surface(&preview);
    black_box(pixel_fingerprint(preview.pixels()));
    document
        .render_page_with_renderer(0, VIEWPORT_WIDTH, renderer)
        .expect("benchmark first full viewport")
}

fn compare_readable_pixels(width: u32) {
    let mut document =
        NativeDocument::open(READABLE_PDF.to_vec()).expect("comparison strict Native open");
    let fast = document
        .render_page_with_renderer(0, width, NativeRendererKind::FastCpu)
        .expect("comparison Fast CPU");
    let reference = document
        .render_page_with_renderer(0, width, NativeRendererKind::ReferenceCpu)
        .expect("comparison Reference CPU");
    assert_eq!(fast.width(), reference.width());
    assert_eq!(fast.height(), reference.height());
    assert_eq!(fast.stride(), reference.stride());
    let maximum_delta = fast
        .pixels()
        .iter()
        .zip(reference.pixels())
        .map(|(fast, reference)| fast.abs_diff(*reference))
        .max()
        .expect("nonempty comparison");
    let changed_channels = fast
        .pixels()
        .iter()
        .zip(reference.pixels())
        .filter(|(fast, reference)| fast != reference)
        .count();
    let maximum_delta_bound = match width {
        PREVIEW_WIDTH => 72,
        VIEWPORT_WIDTH => 64,
        _ => panic!("benchmark width must be registered"),
    };
    assert!(
        maximum_delta <= maximum_delta_bound,
        "readable-page channel delta exceeded: {maximum_delta} > {maximum_delta_bound}"
    );
    let maximum_changed_channels =
        usize::try_from(u64::from(width) * u64::from(fast.height()) / 9).expect("bound fits usize");
    assert!(
        changed_channels <= maximum_changed_channels,
        "readable-page changed-channel bound exceeded: {changed_channels}"
    );
}

fn assert_surface(surface: &NativePageSurface) {
    assert_eq!(surface.page_index(), 0);
    assert_eq!(surface.stride(), surface.width() * 4);
    assert_eq!(
        surface.pixels().len(),
        surface_bytes(surface.width(), surface.height())
    );
    assert!(
        surface
            .pixels()
            .chunks_exact(4)
            .any(|pixel| pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 100),
        "benchmark surface retains readable dark content"
    );
}

fn surface_bytes(width: u32, height: u32) -> usize {
    usize::try_from(u64::from(width) * u64::from(height) * 4).expect("surface bytes fit usize")
}

fn pixel_fingerprint(pixels: &[u8]) -> u64 {
    pixels.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        hash.wrapping_mul(0x0000_0100_0000_01b3) ^ u64::from(*byte)
    })
}

fn print_samples(renderer: &str, metric: &str, samples: &[u64]) {
    assert_eq!(samples.len(), SAMPLE_COUNT);
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];
    let p95 = sorted[(sorted.len() * 95).div_ceil(100) - 1];
    let p99 = sorted[(sorted.len() * 99).div_ceil(100) - 1];
    println!(
        "m4-benchmark renderer={renderer} metric={metric} median_ns={median} p95_ns={p95} p99_ns={p99} raw_samples_ns={samples:?}"
    );
}
