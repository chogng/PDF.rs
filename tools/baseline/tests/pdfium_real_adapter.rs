use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pdf_rs_baseline::{
    BaselineChannel, BaselineDescriptor, BaselineErrorCode, BaselineRequest, BaselineRunner,
    OracleAuthority, PDFIUM_PIXEL_ADAPTER_PROFILE, PdfiumPixelAdapter, ProcessLimits, ProcessSpec,
    descriptor_identity,
};
use pdf_rs_compare::{PixelArtifact, compare_pixels};
use pdf_rs_digest::sha256;
use pdf_rs_generate::{GenerateLimits, compile_dsl};

#[path = "support/pdfium_probe.rs"]
mod probe;

use probe::*;

#[test]
fn analytic_probe_inputs_are_fixed_without_running_pdfium() {
    assert_eq!(COLOR_PROBE_DSL.len(), COLOR_PROBE_SOURCE_BYTES);
    assert_eq!(
        hex(&sha256(COLOR_PROBE_DSL.as_bytes()).unwrap()),
        COLOR_PROBE_SOURCE_HASH
    );
    let generated = compile_dsl(COLOR_PROBE_DSL.as_bytes(), GenerateLimits::default()).unwrap();
    assert_eq!(generated.bytes().len(), COLOR_PROBE_PDF_BYTES);
    assert_eq!(hex(&generated.source_sha256()), COLOR_PROBE_SOURCE_HASH);
    assert_eq!(hex(&generated.output_sha256()), COLOR_PROBE_PDF_HASH);
    assert_eq!(
        hex(&sha256(&vec![255; rgba_len()]).unwrap()),
        EXPECTED_WHITE_RGBA_HASH
    );
    assert_eq!(
        hex(&sha256(&analytic_quadrants()).unwrap()),
        COLOR_PROBE_RGBA_HASH
    );
}

#[test]
#[ignore = "requires PDF_RS_PDFIUM_ADAPTER pointing to the separately built PDFium helper"]
fn real_pdfium_adapter_matches_analytic_pixel_probes() {
    let executable = required_adapter_path();
    let repository = repository_root();
    let fixture =
        repository.join("tests/cases/infrastructure/synthetic-failure-bundle-001/input.pdf");
    let pdf = fs::read(&fixture).unwrap_or_else(|error| {
        panic!(
            "read {} ({error}); replay the canonical generator first",
            fixture.display()
        )
    });
    assert_eq!(hex(&sha256(&pdf).unwrap()), FIXTURE_SOURCE_HASH);

    assert_eq!(std::env::consts::OS, "macos");
    assert_eq!(std::env::consts::ARCH, "aarch64");
    let limits = ProcessLimits::new(4096, 176, 4096, Duration::from_secs(2)).unwrap();
    let process = ProcessSpec::new(
        &executable,
        Vec::new(),
        Vec::new(),
        &repository,
        "uncontained-local-pdfium-probe-v1",
    )
    .unwrap();
    let helper_bytes = fs::read(&executable).unwrap();
    assert_eq!(helper_bytes.len(), EXPECTED_HELPER_BYTES);
    let helper_hash = sha256(&helper_bytes).unwrap();
    assert_eq!(hex(&helper_hash), EXPECTED_HELPER_SHA256);
    let invocation_hash = process.identity(limits).unwrap();
    let build_flags_hash = digest(PDFIUM_ARGS_GN);
    let environment_hash = digest(ENVIRONMENT_DECLARATION);
    let license_manifest_hash = digest(LICENSE_DECLARATION);
    let fonts_hash = digest(FONTS_DECLARATION);
    let color_hash = digest(COLOR_DECLARATION);
    let descriptor = BaselineDescriptor {
        id: PDFIUM_PIXEL_ADAPTER_PROFILE.into(),
        engine: "pdfium".into(),
        upstream_revision: PDFIUM_REVISION.into(),
        build_hash: helper_hash,
        build_flags_hash,
        environment_hash,
        invocation_hash,
        license_manifest_hash,
        fonts_hash,
        color_hash,
        platform: "macos-arm64".into(),
    };
    let descriptor_hash = descriptor_identity(&descriptor).unwrap();
    let adapter = PdfiumPixelAdapter::new(descriptor, process, limits).unwrap();

    let mut helper_process_runs = 0_u64;
    let first = observe_counted(&adapter, pdf.clone(), 0, &mut helper_process_runs).unwrap();
    let second = observe_counted(&adapter, pdf.clone(), 0, &mut helper_process_runs).unwrap();
    assert_eq!(first.authority(), OracleAuthority::O4Observation);
    assert_eq!(first.parse_json, BaselineChannel::Unsupported);
    assert_eq!(first.scene_json, BaselineChannel::Unsupported);
    assert_eq!(first.text_json, BaselineChannel::Unsupported);
    let first_rgba = produced_rgba(&first.rgba);
    let second_rgba = produced_rgba(&second.rgba);
    assert_eq!(first_rgba, second_rgba);
    assert_eq!(hex(&sha256(first_rgba).unwrap()), EXPECTED_WHITE_RGBA_HASH);

    let analytic = PixelArtifact::new(WIDTH, HEIGHT, vec![255; rgba_len()]).unwrap();
    let observed = PixelArtifact::new(WIDTH, HEIGHT, first_rgba.to_vec()).unwrap();
    let comparison = compare_pixels(&analytic, &observed).unwrap();
    assert!(comparison.summary().is_exact());
    assert_eq!(comparison.summary().different_pixels(), 0);
    assert_eq!(comparison.summary().different_channels(), 0);
    assert_eq!(comparison.summary().max_channel_delta(), [0; 4]);
    assert_eq!(comparison.summary().total_absolute_delta(), 0);

    let color_probe = compile_dsl(COLOR_PROBE_DSL.as_bytes(), GenerateLimits::default()).unwrap();
    let color_source_hash = color_probe.source_sha256();
    let color_pdf_hash = color_probe.output_sha256();
    let color_pdf_bytes = color_probe.bytes().len();
    assert_eq!(hex(&color_source_hash), COLOR_PROBE_SOURCE_HASH);
    assert_eq!(hex(&color_pdf_hash), COLOR_PROBE_PDF_HASH);
    assert_eq!(color_pdf_bytes, COLOR_PROBE_PDF_BYTES);
    let color_observation = observe_counted(
        &adapter,
        color_probe.into_bytes(),
        0,
        &mut helper_process_runs,
    )
    .unwrap();
    let color_rgba = produced_rgba(&color_observation.rgba);
    let expected_color_rgba = analytic_quadrants();
    let expected_color_hash = sha256(&expected_color_rgba).unwrap();
    assert_eq!(hex(&expected_color_hash), COLOR_PROBE_RGBA_HASH);
    let color_comparison = compare_pixels(
        &PixelArtifact::new(WIDTH, HEIGHT, expected_color_rgba).unwrap(),
        &PixelArtifact::new(WIDTH, HEIGHT, color_rgba.to_vec()).unwrap(),
    )
    .unwrap();
    assert!(color_comparison.summary().is_exact());
    assert_eq!(color_comparison.summary().different_pixels(), 0);
    assert_eq!(color_comparison.summary().different_channels(), 0);
    assert_eq!(color_comparison.summary().max_channel_delta(), [0; 4]);
    assert_eq!(color_comparison.summary().total_absolute_delta(), 0);

    let page_error = observe_counted(&adapter, pdf, 1, &mut helper_process_runs)
        .err()
        .unwrap();
    assert_eq!(page_error.code, BaselineErrorCode::RunnerFailed);
    assert_eq!(page_error.diagnostic_id, "RPE-BASELINE-0006");

    let malformed = b"%PDF-1.7\nnot-a-document\n".to_vec();
    let malformed_error = observe_counted(&adapter, malformed, 0, &mut helper_process_runs)
        .err()
        .unwrap();
    assert_eq!(malformed_error.code, BaselineErrorCode::RunnerFailed);
    assert_eq!(malformed_error.diagnostic_id, "RPE-BASELINE-0006");
    assert_eq!(helper_process_runs, HELPER_PROCESS_RUNS);

    println!("pdfium_revision={PDFIUM_REVISION}");
    println!("helper_sha256={}", hex(&helper_hash));
    println!("build_flags_sha256={}", hex(&build_flags_hash));
    println!(
        "build_args_command_sha256={}",
        hex(&digest(PDFIUM_BUILD_ARGS))
    );
    println!("environment_sha256={}", hex(&environment_hash));
    println!("invocation_sha256={}", hex(&invocation_hash));
    println!("license_manifest_sha256={}", hex(&license_manifest_hash));
    println!("fonts_sha256={}", hex(&fonts_hash));
    println!("color_sha256={}", hex(&color_hash));
    println!("descriptor_identity_sha256={}", hex(&descriptor_hash));
    println!("fixture_sha256={FIXTURE_SOURCE_HASH}");
    println!("rgba_sha256={EXPECTED_WHITE_RGBA_HASH}");
    println!("repeat_rgba_sha256={EXPECTED_WHITE_RGBA_HASH}");
    println!("geometry={WIDTH}x{HEIGHT}");
    println!("rgba_bytes={}", first_rgba.len());
    println!("different_pixels=0");
    println!("different_channels=0");
    println!("max_channel_delta=0,0,0,0");
    println!("total_absolute_delta=0");
    println!("color_probe_source_sha256={}", hex(&color_source_hash));
    println!("color_probe_source_bytes={}", COLOR_PROBE_DSL.len());
    println!("color_probe_pdf_sha256={}", hex(&color_pdf_hash));
    println!("color_probe_pdf_bytes={color_pdf_bytes}");
    println!("color_probe_rgba_sha256={}", hex(&expected_color_hash));
    println!("color_probe_different_pixels=0");
    println!("color_probe_different_channels=0");
    println!("color_probe_max_channel_delta=0,0,0,0");
    println!("color_probe_total_absolute_delta=0");
    println!("page_out_of_range=RPE-BASELINE-0006");
    println!("malformed_pdf=RPE-BASELINE-0006");
    println!("helper_process_runs={helper_process_runs}");
    println!("native_engine_exercised=false");
    println!("differential_eligible=false");
}

fn observe(
    adapter: &PdfiumPixelAdapter,
    pdf: Vec<u8>,
    page: u32,
) -> Result<pdf_rs_baseline::BaselineObservation, pdf_rs_baseline::BaselineError> {
    let source_hash = sha256(&pdf).unwrap();
    let request = BaselineRequest::new(source_hash, pdf, page, WIDTH, HEIGHT).unwrap();
    adapter.observe(&request)
}

fn observe_counted(
    adapter: &PdfiumPixelAdapter,
    pdf: Vec<u8>,
    page: u32,
    runs: &mut u64,
) -> Result<pdf_rs_baseline::BaselineObservation, pdf_rs_baseline::BaselineError> {
    *runs = runs.checked_add(1).unwrap();
    observe(adapter, pdf, page)
}

fn produced_rgba(channel: &BaselineChannel<Vec<u8>>) -> &[u8] {
    match channel {
        BaselineChannel::Produced(rgba) => rgba,
        BaselineChannel::Unsupported => panic!("PDFium pixel profile returned unsupported"),
        BaselineChannel::Failed => panic!("PDFium pixel profile failed the canonical blank probe"),
    }
}

fn required_adapter_path() -> PathBuf {
    let value = std::env::var_os("PDF_RS_PDFIUM_ADAPTER")
        .unwrap_or_else(|| panic!("PDF_RS_PDFIUM_ADAPTER is required for this ignored test"));
    PathBuf::from(value)
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn digest(value: &str) -> [u8; 32] {
    sha256(value.as_bytes()).unwrap()
}

fn hex(value: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in value {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}
