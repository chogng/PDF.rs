pub const PDFIUM_REVISION: &str = "c040cf96106a87220b814a1a892649cf2d7f1934";
pub const PDFIUM_BUILD_ARGS: &str = "use_remoteexec=false is_debug=false symbol_level=0 target_cpu=\"arm64\" pdf_is_standalone=true pdf_enable_v8=false pdf_enable_xfa=false pdf_use_skia=false pdf_enable_fontations=false is_component_build=false";
pub const PDFIUM_ARGS_GN: &str = concat!(
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
pub const ENVIRONMENT_DECLARATION: &str =
    "env-clear direct-child local probe; runtime and platform closure incomplete";
pub const LICENSE_DECLARATION: &str = "license closure incomplete; root license checked only";
pub const FONTS_DECLARATION: &str =
    "empty user font paths requested; platform font closure unproven";
pub const COLOR_DECLARATION: &str =
    "agg rgba8 straight-alpha top-down; srgb target and color closure unproven";
pub const EXPECTED_HELPER_SHA256: &str =
    "5d4f991ba39bfe635b59e869a14004a3377dbc16f414328a5d86d527079b9426";
pub const EXPECTED_HELPER_BYTES: usize = 4_251_312;
pub const FIXTURE_SOURCE_HASH: &str =
    "9c819e549afcc89d03b380c3c1bd47128aa2b70ae30a35245e6a0e30132875db";
pub const EXPECTED_WHITE_RGBA_HASH: &str =
    "8667e718294e9e0df1d30600ba3eeb201f764aad2dad72748643e4a285e1d1f7";
pub const COLOR_PROBE_SOURCE_HASH: &str =
    "e31c82086b3b2bec886e651c9cd572901c65227e6c6db116955f2a65f8b3d515";
pub const COLOR_PROBE_PDF_HASH: &str =
    "c2499fee0c6a91312114351d82958a7b77900961ae82c6e15c2bca10c8e11629";
pub const COLOR_PROBE_RGBA_HASH: &str =
    "3090d0b5c26c5cf3e83e705a0ed36dca3a65dbf0389e3554fcfa89a9f433575a";
pub const COLOR_PROBE_SOURCE_BYTES: usize = 374;
pub const COLOR_PROBE_PDF_BYTES: usize = 726;
pub const COLOR_PROBE_DSL: &str = concat!(
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
pub const WIDTH: u32 = 4;
pub const HEIGHT: u32 = 4;
pub const BLANK_PROBE_RUNS: u64 = 2;
pub const COLOR_PROBE_RUNS: u64 = 1;
pub const PAGE_OUT_OF_RANGE_RUNS: u64 = 1;
pub const MALFORMED_PDF_RUNS: u64 = 1;
pub const HELPER_PROCESS_RUNS: u64 =
    BLANK_PROBE_RUNS + COLOR_PROBE_RUNS + PAGE_OUT_OF_RANGE_RUNS + MALFORMED_PDF_RUNS;

pub fn rgba_len() -> usize {
    usize::try_from(u64::from(WIDTH) * u64::from(HEIGHT) * 4).unwrap()
}

pub fn analytic_quadrants() -> Vec<u8> {
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
