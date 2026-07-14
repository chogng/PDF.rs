use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pdf_rs_baseline::{
    BaselineChannel, BaselineDescriptor, BaselineErrorCode, BaselineRequest, BaselineRunner,
    OracleAuthority, PDFIUM_PIXEL_ADAPTER_PROFILE, PdfiumPixelAdapter, ProcessLimits, ProcessSpec,
};
use pdf_rs_compare::{PixelArtifact, compare_pixels};
use pdf_rs_digest::sha256;
use pdf_rs_generate::{GenerateLimits, compile_dsl};

const PDFIUM_REVISION: &str = "c040cf96106a87220b814a1a892649cf2d7f1934";
const PDFIUM_BUILD_ARGS: &str = "use_remoteexec=false is_debug=false symbol_level=0 target_cpu=\"arm64\" pdf_is_standalone=true pdf_enable_v8=false pdf_enable_xfa=false pdf_use_skia=false pdf_enable_fontations=false is_component_build=false";
const PDFIUM_ARGS_GN: &str = concat!(
    "use_remoteexec = false\n",
    "is_debug = false\n",
    "symbol_level = 0\n",
    "target_cpu = \"arm64\"\n",
    "pdf_is_standalone = true\n",
    "pdf_enable_v8 = false\n",
    "pdf_enable_xfa = false\n",
    "pdf_use_skia = false\n",
    "pdf_enable_fontations = false\n",
    "is_component_build = false\n",
);
const FIXTURE_SOURCE_HASH: &str =
    "9c819e549afcc89d03b380c3c1bd47128aa2b70ae30a35245e6a0e30132875db";
const EXPECTED_WHITE_RGBA_HASH: &str =
    "8667e718294e9e0df1d30600ba3eeb201f764aad2dad72748643e4a285e1d1f7";
const COLOR_PROBE_DSL: &str = concat!(
    "document(version: \"1.7\") {\n",
    "  object(1) = catalog(pages: ref(2));\n",
    "  object(2) = pages(kids: [ref(3)], count: 1);\n",
    "  object(3) = page(\n",
    "    media_box: [0, 0, 200, 200],\n",
    "    resources: {},\n",
    "    contents: ref(4)\n",
    "  );\n",
    "  stream(4) { \"q\\n",
    "0 0 1 rg 0 100 100 100 re f\\n",
    "1 1 0 rg 100 100 100 100 re f\\n",
    "1 0 0 rg 0 0 100 100 re f\\n",
    "0 1 0 rg 100 0 100 100 re f\\n",
    "Q\\n\" }\n",
    "  xref(kind: table);\n",
    "}\n",
);
const WIDTH: u32 = 4;
const HEIGHT: u32 = 4;

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

    let limits =
        ProcessLimits::new(1024 * 1024, 1024 * 1024, 4096, Duration::from_secs(10)).unwrap();
    let process = ProcessSpec::new(
        &executable,
        Vec::new(),
        Vec::new(),
        &repository,
        "uncontained-local-pdfium-probe-v1",
    )
    .unwrap();
    let helper_hash = sha256(&fs::read(&executable).unwrap()).unwrap();
    let invocation_hash = process.identity(limits).unwrap();
    let descriptor = BaselineDescriptor {
        id: PDFIUM_PIXEL_ADAPTER_PROFILE.into(),
        engine: "pdfium".into(),
        upstream_revision: PDFIUM_REVISION.into(),
        build_hash: helper_hash,
        build_flags_hash: digest(PDFIUM_ARGS_GN),
        environment_hash: digest(
            "env-clear direct-child local probe; runtime and platform closure incomplete",
        ),
        invocation_hash,
        license_manifest_hash: digest("license closure incomplete; root license checked only"),
        fonts_hash: digest("empty user font paths requested; platform font closure unproven"),
        color_hash: digest(
            "agg rgba8 straight-alpha top-down; srgb target and color closure unproven",
        ),
        platform: format!("local-{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    };
    let adapter = PdfiumPixelAdapter::new(descriptor, process, limits).unwrap();

    let first = observe(&adapter, pdf.clone(), 0).unwrap();
    let second = observe(&adapter, pdf.clone(), 0).unwrap();
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
    let color_observation = observe(&adapter, color_probe.into_bytes(), 0).unwrap();
    let color_rgba = produced_rgba(&color_observation.rgba);
    let expected_color_rgba = analytic_quadrants();
    let expected_color_hash = sha256(&expected_color_rgba).unwrap();
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

    let page_error = observe(&adapter, pdf, 1).err().unwrap();
    assert_eq!(page_error.code, BaselineErrorCode::RunnerFailed);
    assert_eq!(page_error.diagnostic_id, "RPE-BASELINE-0006");

    let malformed = b"%PDF-1.7\nnot-a-document\n".to_vec();
    let malformed_error = observe(&adapter, malformed, 0).err().unwrap();
    assert_eq!(malformed_error.code, BaselineErrorCode::RunnerFailed);
    assert_eq!(malformed_error.diagnostic_id, "RPE-BASELINE-0006");

    println!("pdfium_revision={PDFIUM_REVISION}");
    println!("helper_sha256={}", hex(&helper_hash));
    println!("build_flags_sha256={}", hex(&digest(PDFIUM_ARGS_GN)));
    println!(
        "build_args_command_sha256={}",
        hex(&digest(PDFIUM_BUILD_ARGS))
    );
    println!("invocation_sha256={}", hex(&invocation_hash));
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
    println!("color_probe_pdf_sha256={}", hex(&color_pdf_hash));
    println!("color_probe_rgba_sha256={}", hex(&expected_color_hash));
    println!("color_probe_different_pixels=0");
    println!("color_probe_different_channels=0");
    println!("color_probe_max_channel_delta=0,0,0,0");
    println!("color_probe_total_absolute_delta=0");
    println!("page_out_of_range=RPE-BASELINE-0006");
    println!("malformed_pdf=RPE-BASELINE-0006");
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

fn rgba_len() -> usize {
    usize::try_from(u64::from(WIDTH) * u64::from(HEIGHT) * 4).unwrap()
}

fn analytic_quadrants() -> Vec<u8> {
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const YELLOW: [u8; 4] = [255, 255, 0, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    let mut rgba = Vec::with_capacity(rgba_len());
    for row in 0..HEIGHT {
        let (left, right) = if row < HEIGHT / 2 {
            (BLUE, YELLOW)
        } else {
            (RED, GREEN)
        };
        for column in 0..WIDTH {
            rgba.extend_from_slice(if column < WIDTH / 2 { &left } else { &right });
        }
    }
    rgba
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
