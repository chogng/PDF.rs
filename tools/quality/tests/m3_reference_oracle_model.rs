use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_quality::case_contract::validate_case_file;

#[path = "m3_reference_gate_support/fixture.rs"]
mod fixture;

use fixture::{FixtureSpec, ImageSpec, build_fixture};

const PATH_CLIP_CONTENT: &[u8] = b"q 0 0 50 100 re W n 0 g 0 0 100 100 re f Q \
                                  1 0 0 rg 50 0 50 100 re f";
const STROKE_CONTENT: &[u8] = b"2 w [4 2] 0 d 5 5 90 90 re S 0.5 g 10 10 80 80 re B*";
const IMAGE_CONTENT: &[u8] = b"q 100 0 0 100 0 0 cm /Im0 Do Q";
const FONT_CONTENT: &[u8] = b"BT /F0 1000 Tf 0 0 Td (A) Tj ET";
const MIXED_CONTENT: &[u8] = b"0.8 g 0 0 100 100 re f \
                               q 50 0 0 100 0 0 cm /Im0 Do Q \
                               0 g BT /F0 500 Tf 0 0 Td (A) Tj ET";
const INVALID_CONTENT: &[u8] = b"q";
const IMAGE_COMPONENTS: [u8; 6] = [255, 0, 0, 0, 0, 255];

const Q16_ONE: u32 = 65_536;
const Q16_GRAY_HALF: u32 = 32_769;
const SAMPLE_SIDE: u32 = 8;
const SAMPLES_PER_PIXEL: u32 = SAMPLE_SIDE * SAMPLE_SIDE;
const PAGE_DENOMINATOR: i64 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Case {
    ValidPathClip,
    ValidStroke,
    ValidImage,
    ValidFont,
    ValidMixed,
    UnsupportedInterpolatedImage,
    InvalidContentState,
    StrictInvalidXref,
    CancelFinalPublication,
    SourceChangeAfterPending,
    ImageDecodedOneLess,
    RasterOutputOneLess,
}

const CASES: [Case; 12] = [
    Case::ValidPathClip,
    Case::ValidStroke,
    Case::ValidImage,
    Case::ValidFont,
    Case::ValidMixed,
    Case::UnsupportedInterpolatedImage,
    Case::InvalidContentState,
    Case::StrictInvalidXref,
    Case::CancelFinalPublication,
    Case::SourceChangeAfterPending,
    Case::ImageDecodedOneLess,
    Case::RasterOutputOneLess,
];

const READY_CASES: [Case; 4] = [
    Case::ValidPathClip,
    Case::ValidStroke,
    Case::ValidImage,
    Case::ValidFont,
];

impl Case {
    const fn slug(self) -> &'static str {
        match self {
            Self::ValidPathClip => "valid-path-clip",
            Self::ValidStroke => "valid-stroke",
            Self::ValidImage => "valid-image",
            Self::ValidFont => "valid-font",
            Self::ValidMixed => "valid-mixed",
            Self::UnsupportedInterpolatedImage => "producer-unsupported-interpolated-image",
            Self::InvalidContentState => "invalid-content-state",
            Self::StrictInvalidXref => "strict-invalid-xref",
            Self::CancelFinalPublication => "cancel-final-publication",
            Self::SourceChangeAfterPending => "source-change-after-pending",
            Self::ImageDecodedOneLess => "image-decoded-one-less",
            Self::RasterOutputOneLess => "raster-output-one-less",
        }
    }

    fn id(self) -> String {
        format!("raster/m3-reference/{}", self.slug())
    }

    const fn fixture_spec(self) -> FixtureSpec {
        match self {
            Self::ValidPathClip | Self::CancelFinalPublication | Self::RasterOutputOneLess => {
                path_spec(self.salt())
            }
            Self::ValidStroke => FixtureSpec {
                content: STROKE_CONTENT,
                image: None,
                font: false,
                invalid_startxref: false,
                salt: self.salt(),
            },
            Self::ValidImage | Self::SourceChangeAfterPending | Self::ImageDecodedOneLess => {
                image_spec(false, self.salt())
            }
            Self::ValidFont => FixtureSpec {
                content: FONT_CONTENT,
                image: None,
                font: true,
                invalid_startxref: false,
                salt: self.salt(),
            },
            Self::ValidMixed => FixtureSpec {
                content: MIXED_CONTENT,
                image: Some(ImageSpec { interpolate: false }),
                font: true,
                invalid_startxref: false,
                salt: self.salt(),
            },
            Self::UnsupportedInterpolatedImage => image_spec(true, self.salt()),
            Self::InvalidContentState => FixtureSpec {
                content: INVALID_CONTENT,
                image: None,
                font: false,
                invalid_startxref: false,
                salt: self.salt(),
            },
            Self::StrictInvalidXref => FixtureSpec {
                content: PATH_CLIP_CONTENT,
                image: None,
                font: false,
                invalid_startxref: true,
                salt: self.salt(),
            },
        }
    }

    const fn salt(self) -> u8 {
        match self {
            Self::ValidPathClip => 0xc1,
            Self::ValidStroke => 0xc2,
            Self::ValidImage => 0xc3,
            Self::ValidFont => 0xc4,
            Self::ValidMixed => 0xc5,
            Self::UnsupportedInterpolatedImage => 0xc6,
            Self::InvalidContentState => 0xc7,
            Self::StrictInvalidXref => 0xc8,
            Self::CancelFinalPublication => 0xc9,
            Self::SourceChangeAfterPending => 0xca,
            Self::ImageDecodedOneLess => 0xcb,
            Self::RasterOutputOneLess => 0xcc,
        }
    }

    const fn dimensions(self) -> (u32, u32) {
        match self {
            Self::ValidPathClip
            | Self::ValidImage
            | Self::CancelFinalPublication
            | Self::RasterOutputOneLess => (2, 1),
            Self::ValidStroke
            | Self::ValidFont
            | Self::ValidMixed
            | Self::UnsupportedInterpolatedImage
            | Self::InvalidContentState
            | Self::StrictInvalidXref
            | Self::SourceChangeAfterPending
            | Self::ImageDecodedOneLess => (8, 8),
        }
    }

    const fn budget(self) -> Budget {
        let (max_input_bytes, max_stream_output_bytes, max_total_decode_bytes, max_image_pixels) =
            match self {
                Self::ValidPathClip | Self::CancelFinalPublication => (622, 4096, 4096, 2),
                Self::ValidStroke => (606, 4096, 4096, 64),
                Self::ValidImage => (758, 4096, 4096, 2),
                Self::ValidFont => (1855, 4096, 4096, 64),
                Self::ValidMixed => (2085, 4096, 4096, 64),
                Self::UnsupportedInterpolatedImage => (776, 4096, 4096, 64),
                Self::InvalidContentState => (554, 4096, 4096, 64),
                Self::StrictInvalidXref => (622, 4096, 4096, 64),
                Self::SourceChangeAfterPending => (758, 4096, 4096, 64),
                Self::ImageDecodedOneLess => (758, 4096, 5, 64),
                Self::RasterOutputOneLess => (622, 4096, 4096, 2),
            };
        Budget {
            max_input_bytes,
            max_stream_output_bytes,
            max_total_decode_bytes,
            max_image_pixels,
            max_raster_output_bytes: match self {
                Self::ValidPathClip | Self::ValidImage | Self::CancelFinalPublication => 8,
                Self::RasterOutputOneLess => 7,
                Self::ValidStroke
                | Self::ValidFont
                | Self::ValidMixed
                | Self::UnsupportedInterpolatedImage
                | Self::InvalidContentState
                | Self::StrictInvalidXref
                | Self::SourceChangeAfterPending
                | Self::ImageDecodedOneLess => 256,
            },
            max_path_segments: match self {
                Self::ValidPathClip
                | Self::ValidStroke
                | Self::ValidMixed
                | Self::StrictInvalidXref
                | Self::CancelFinalPublication
                | Self::RasterOutputOneLess => 32,
                _ => 16,
            },
        }
    }
}

#[derive(Clone, Copy)]
struct Budget {
    max_input_bytes: u64,
    max_stream_output_bytes: u64,
    max_total_decode_bytes: u64,
    max_image_pixels: u64,
    max_raster_output_bytes: u64,
    max_path_segments: u64,
}

#[derive(Clone, Copy)]
struct NonReadyContract {
    case: Case,
    class: &'static str,
    diagnostic: &'static str,
    derivation: &'static str,
    flags: [bool; 7],
}

const NON_READY: [NonReadyContract; 7] = [
    NonReadyContract {
        case: Case::UnsupportedInterpolatedImage,
        class: "valid",
        diagnostic: "RPE-CONTENT-UNSUPPORTED-0009",
        derivation: "Independent terminal contract: outcome=unsupported; stage=content-image; diagnostic=RPE-CONTENT-UNSUPPORTED-0009; parse=true; scene=false; pixel=false; capability=true; error=false.",
        flags: [true, false, false, false, true, true, false],
    },
    NonReadyContract {
        case: Case::InvalidContentState,
        class: "invalid",
        diagnostic: "RPE-CONTENT-VM-0007",
        derivation: "Independent terminal contract: outcome=failed; stage=content-vm; diagnostic=RPE-CONTENT-VM-0007; parse=true; scene=false; pixel=false; capability=false; error=true.",
        flags: [true, false, false, false, true, false, true],
    },
    NonReadyContract {
        case: Case::StrictInvalidXref,
        class: "invalid",
        diagnostic: "RPE-XREF-0011",
        derivation: "Independent terminal contract: outcome=failed; stage=strict-open; diagnostic=RPE-XREF-0011; parse=false; scene=false; pixel=false; capability=false; error=true.",
        flags: [false, false, false, false, true, false, true],
    },
    NonReadyContract {
        case: Case::CancelFinalPublication,
        class: "valid",
        diagnostic: "RPE-RASTER-0004",
        derivation: "Independent terminal contract: outcome=cancelled; stage=reference-publication; diagnostic=RPE-RASTER-0004; parse=true; scene=true; pixel=false; capability=false; error=true.",
        flags: [true, true, false, false, true, false, true],
    },
    NonReadyContract {
        case: Case::SourceChangeAfterPending,
        class: "valid",
        diagnostic: "RPE-CONTENT-VM-0014",
        derivation: "Independent terminal contract: outcome=source-changed; stage=content-vm-resume; diagnostic=RPE-CONTENT-VM-0014; parse=true; scene=false; pixel=false; capability=false; error=true.",
        flags: [true, false, false, false, true, false, true],
    },
    NonReadyContract {
        case: Case::ImageDecodedOneLess,
        class: "valid",
        diagnostic: "RPE-CONTENT-VM-0012",
        derivation: "Independent terminal contract: outcome=resource-limited; stage=content-image; diagnostic=RPE-CONTENT-VM-0012; limit-kind=content-image-decoded-bytes; limit=5; consumed=0; attempted=6; parse=true; scene=false; pixel=false; capability=false; error=true.",
        flags: [true, false, false, false, true, false, true],
    },
    NonReadyContract {
        case: Case::RasterOutputOneLess,
        class: "valid",
        diagnostic: "RPE-RASTER-0005",
        derivation: "Independent terminal contract: outcome=resource-limited; stage=reference-preflight; diagnostic=RPE-RASTER-0005; limit-kind=reference-output-bytes; limit=7; consumed=0; attempted=8; parse=true; scene=true; pixel=false; capability=false; error=true.",
        flags: [true, true, false, false, true, false, true],
    },
];

#[test]
fn committed_inputs_match_the_frozen_literal_fixture_specs() {
    for case in CASES {
        let case_directory = case_directory(case);
        let input_path = case_directory.join("input.pdf");
        let committed = fs::read(&input_path)
            .unwrap_or_else(|error| panic!("{} input is readable: {error}", case.id()));
        let independently_rebuilt = build_fixture(case.fixture_spec()).bytes;
        assert_eq!(
            committed,
            independently_rebuilt,
            "{} input differs from the frozen literal fixture",
            case.id()
        );

        let manifest = validate_case_file(&case_directory.join("case.toml"))
            .unwrap_or_else(|diagnostics| panic!("{} invalid: {diagnostics:?}", case.id()));
        assert_eq!(manifest.case_id(), case.id());
        assert_eq!(
            manifest.string("provenance", "source"),
            Some(format!("tests/cases/raster/m3-reference/{}/input.pdf", case.slug()).as_str())
        );
        assert_eq!(
            manifest.source_sha256(),
            digest_reference(&committed),
            "{} source digest is not bound to input.pdf",
            case.id()
        );
        assert_eq!(
            manifest.positive_u64("budget", "max_input_bytes"),
            Some(u64::try_from(committed.len()).expect("fixture length fits u64")),
            "{} input budget must be exact",
            case.id()
        );
        assert_common_contract(case, &manifest);
    }
}

#[test]
fn independent_ready_models_match_committed_pixels_and_hashes_exactly() {
    for case in READY_CASES {
        let (width, height, rgba) = independently_model_ready_case(case);
        let modeled = encode_pixel_contract(width, height, &rgba);
        let case_directory = case_directory(case);
        let committed = fs::read(case_directory.join("expected/pixel.json"))
            .unwrap_or_else(|error| panic!("{} pixel artifact is readable: {error}", case.id()));
        assert_eq!(
            committed,
            modeled,
            "{} committed pixel bytes differ from the independent model",
            case.id()
        );

        let manifest = validate_case_file(&case_directory.join("case.toml"))
            .unwrap_or_else(|diagnostics| panic!("{} invalid: {diagnostics:?}", case.id()));
        assert_eq!(
            manifest.string("expected", "pixel_sha256"),
            Some(digest_reference(&modeled).as_str()),
            "{} committed pixel hash differs from independently modeled bytes",
            case.id()
        );
        let expected_level = if case == Case::ValidPathClip {
            "O0"
        } else {
            "O1"
        };
        assert_eq!(
            manifest.string("pixel_oracle", "level"),
            Some(expected_level)
        );
        assert_eq!(
            manifest.boolean("pixel_oracle", "reference_may_generate"),
            Some(false)
        );
    }
}

#[test]
fn reviewed_o3_mixed_is_hash_bound_without_independent_pixel_derivation() {
    const PIXEL_SHA256: &str =
        "sha256:f5a08df588fd4fa06d55c02334c53b021ff446c2f4c01a7accc692883e0b89d5";
    const RAW_RGBA_SHA256: &str =
        "05c2256f5ef14fc8c0733f273a2827846bf0b854bbaec5027e0278ca7f864a1e";
    const IMPLEMENTATION_COMMIT: &str = "8c3e28c8ce4cbe5113cc565a36744158e283a7fb";
    const IMPLEMENTATION_TREE: &str = "724c2a646114a8aff0fabe29f6008a8b73802783";
    const IDENTITY_REFERENCE: &str = "evidence/reference-identity.json#sha256:a447941c0e9f7e06c2005bc0d3a1742c075a60c41db6b68aac88443df36feff6";
    const REVIEW_SHA256: &str =
        "sha256:78014b98b9bfb3b1187d19c5fad73bb94e3ec069e307b147a06f01aedb7d5363";
    const IDENTITY: &[u8] = b"{\"algorithm\":\"reference-raster-v1\",\"implementation_sha256\":\"sha256:0088e35c0824ab38b7e2ba41ff56c89d9bf246b611e968cee19cc36475327f5b\",\"schema\":1}";

    assert!(
        !READY_CASES.contains(&Case::ValidMixed),
        "the reviewed O3 golden must never enter the independent Ready pixel models"
    );
    let directory = case_directory(Case::ValidMixed);
    let manifest = validate_case_file(&directory.join("case.toml"))
        .unwrap_or_else(|diagnostics| panic!("valid-mixed invalid: {diagnostics:?}"));
    assert_eq!(manifest.string("oracle", "level"), Some("O1"));
    assert_eq!(
        manifest.boolean("oracle", "reference_may_generate"),
        Some(false)
    );
    assert_eq!(manifest.string("pixel_oracle", "level"), Some("O3"));
    assert_eq!(
        manifest.boolean("pixel_oracle", "reference_may_generate"),
        Some(true)
    );
    assert_eq!(
        manifest.string("expected", "pixel_sha256"),
        Some(PIXEL_SHA256)
    );
    assert_eq!(
        manifest.string("pixel_oracle", "reference_identity"),
        Some(IDENTITY_REFERENCE)
    );
    assert_eq!(
        manifest.string("pixel_oracle", "review_evidence_sha256"),
        Some(REVIEW_SHA256)
    );
    assert_eq!(
        manifest.string_array("pixel_oracle", "reviewers"),
        Some(vec!["spec-conformance", "parser-security"])
    );
    assert_eq!(
        fs::read(directory.join("evidence/reference-identity.json"))
            .expect("mixed identity is readable"),
        IDENTITY
    );

    let root = repository_root();
    let commit_object = format!("{IMPLEMENTATION_COMMIT}^{{commit}}");
    let commit_status = Command::new("git")
        .current_dir(&root)
        .args(["cat-file", "-e", commit_object.as_str()])
        .status()
        .expect("git can verify the frozen Reference commit");
    assert!(
        commit_status.success(),
        "frozen Reference commit must remain reachable"
    );
    let tree_object = format!("{IMPLEMENTATION_COMMIT}^{{tree}}");
    let tree_output = Command::new("git")
        .current_dir(&root)
        .args(["rev-parse", tree_object.as_str()])
        .output()
        .expect("git can resolve the frozen Reference tree");
    assert!(tree_output.status.success());
    assert_eq!(
        tree_output.stdout,
        format!("{IMPLEMENTATION_TREE}\n").as_bytes(),
        "frozen Reference commit resolves to a different repository tree"
    );
    let implementation_listing = Command::new("git")
        .current_dir(&root)
        .args([
            "ls-tree",
            "-r",
            "--full-tree",
            IMPLEMENTATION_COMMIT,
            "--",
            "core/raster",
        ])
        .output()
        .expect("git can enumerate the frozen core/raster implementation");
    assert!(implementation_listing.status.success());
    assert_eq!(
        hex_digest(
            &sha256(&implementation_listing.stdout)
                .expect("bounded core/raster listing fits SHA-256 framing")
        ),
        "0088e35c0824ab38b7e2ba41ff56c89d9bf246b611e968cee19cc36475327f5b",
        "Reference identity must hash the exact git ls-tree stdout bytes"
    );

    let oracle =
        fs::read_to_string(directory.join("expected/oracle.md")).expect("mixed oracle is readable");
    for marker in [
        "Fill`, `Save`, `DrawImage`, `Restore`, `DrawGlyphRun",
        RAW_RGBA_SHA256,
        PIXEL_SHA256
            .strip_prefix("sha256:")
            .expect("literal pixel digest is prefixed"),
        IMPLEMENTATION_COMMIT,
        IMPLEMENTATION_TREE,
        "git ls-tree -r --full-tree",
        "does not derive the final 8 by 8 RGBA bytes",
    ] {
        assert!(
            oracle.contains(marker),
            "mixed oracle omits required O3 marker {marker}"
        );
    }
}

#[test]
fn non_ready_contracts_pin_terminal_semantics_flags_and_budgets() {
    const FLAG_KEYS: [&str; 7] = [
        "parse",
        "scene",
        "text",
        "pixel",
        "diagnostic",
        "capability",
        "error",
    ];
    for expected in NON_READY {
        let manifest = validate_case_file(&case_directory(expected.case).join("case.toml"))
            .unwrap_or_else(|diagnostics| {
                panic!("{} invalid: {diagnostics:?}", expected.case.id())
            });
        assert_eq!(
            manifest.string("validity", "class"),
            Some(expected.class),
            "{} validity class changed",
            expected.case.id()
        );
        assert_eq!(
            manifest.string("validity", "strict_expected"),
            Some(expected.diagnostic),
            "{} terminal diagnostic changed",
            expected.case.id()
        );
        assert_eq!(
            manifest.string("oracle", "derivation"),
            Some(expected.derivation),
            "{} outcome/stage/limit contract changed",
            expected.case.id()
        );
        for (key, value) in FLAG_KEYS.into_iter().zip(expected.flags) {
            assert_eq!(
                manifest.boolean("expected", key),
                Some(value),
                "{} expected.{key} changed",
                expected.case.id()
            );
        }
        assert_eq!(manifest.raw("expected", "pixel_artifact"), None);
        assert_eq!(manifest.raw("expected", "pixel_sha256"), None);
        assert_eq!(manifest.raw("pixel_oracle", "level"), None);
        assert_exact_budget(expected.case, &manifest);
    }
}

#[test]
fn formal_case_tree_and_model_are_closed_regular_files() {
    let root = case_root();
    let mut actual_slugs = Vec::new();
    for entry in fs::read_dir(&root).expect("M3 case root is readable") {
        let entry = entry.expect("M3 case entry is readable");
        let file_type = entry.file_type().expect("M3 case entry type is readable");
        assert!(
            !file_type.is_symlink(),
            "{} must not be a symbolic link",
            entry.path().display()
        );
        assert!(
            file_type.is_dir(),
            "{} must be a case directory",
            entry.path().display()
        );
        actual_slugs.push(
            entry
                .file_name()
                .into_string()
                .expect("M3 case slug is UTF-8"),
        );
    }
    actual_slugs.sort();
    let mut expected_slugs = CASES
        .into_iter()
        .map(|case| case.slug().to_owned())
        .collect::<Vec<_>>();
    expected_slugs.sort();
    assert_eq!(actual_slugs, expected_slugs);

    for case in CASES {
        let mut actual = Vec::new();
        collect_relative_files(&case_directory(case), &case_directory(case), &mut actual);
        actual.sort();
        let mut expected = if READY_CASES.contains(&case) {
            vec![
                "case.toml".to_owned(),
                "expected/oracle.md".to_owned(),
                "expected/pixel.json".to_owned(),
                "input.pdf".to_owned(),
            ]
        } else if case == Case::ValidMixed {
            vec![
                "case.toml".to_owned(),
                "evidence/reference-identity.json".to_owned(),
                "evidence/review.json".to_owned(),
                "expected/oracle.md".to_owned(),
                "expected/pixel.json".to_owned(),
                "input.pdf".to_owned(),
            ]
        } else {
            vec!["case.toml".to_owned(), "input.pdf".to_owned()]
        };
        expected.sort();
        assert_eq!(
            actual,
            expected,
            "{} has an extra or missing file",
            case.id()
        );
    }

    let model = repository_root().join("tools/quality/tests/m3_reference_oracle_model.rs");
    let metadata = fs::symlink_metadata(&model).expect("oracle model metadata is readable");
    assert!(!metadata.file_type().is_symlink());
    assert!(metadata.file_type().is_file());
}

#[test]
fn oracle_model_source_is_read_only_and_has_no_integrated_render_dependency() {
    let source = include_str!("m3_reference_oracle_model.rs");
    let forbidden = [
        ["pdf_rs_", "raster"].concat(),
        ["Reference", "RenderJob"].concat(),
        ["artifact", ".rs"].concat(),
        ["fs::", "write"].concat(),
        ["File::", "create"].concat(),
        ["Open", "Options"].concat(),
        ["update", "-golden"].concat(),
        ["update", "_golden"].concat(),
    ];
    for token in forbidden {
        assert!(
            !source.contains(&token),
            "independent oracle source contains forbidden token {token}"
        );
    }
}

fn assert_common_contract(case: Case, manifest: &pdf_rs_quality::manifest::CaseManifest) {
    assert_eq!(
        manifest.string_array("runners", "native"),
        Some(vec![
            "tools/quality::m3_reference_gate",
            "tools/quality::m3_reference_oracle_model",
        ])
    );
    assert_eq!(
        manifest.string_array("runners", "external_observation"),
        Some(Vec::new())
    );
    assert_eq!(manifest.string("tolerance", "mode"), Some("exact"));
    assert_eq!(
        manifest.string("render", "color_profile"),
        Some("srgb-reference-v1")
    );
    assert_eq!(manifest.string("render", "alpha"), Some("straight"));
    assert_eq!(manifest.string("render", "antialias"), Some("reference-v1"));
    assert_eq!(
        manifest.string("render", "renderer_epoch"),
        Some("reference-raster-v1")
    );
    let (width, height) = case.dimensions();
    assert_eq!(
        manifest.positive_u64("render", "width"),
        Some(u64::from(width))
    );
    assert_eq!(
        manifest.positive_u64("render", "height"),
        Some(u64::from(height))
    );
    assert_eq!(manifest.positive_u64("render", "dpr_milli"), Some(1000));
    assert_eq!(
        manifest.boolean("oracle", "reference_may_generate"),
        Some(false)
    );
    assert_exact_budget(case, manifest);
}

fn assert_exact_budget(case: Case, manifest: &pdf_rs_quality::manifest::CaseManifest) {
    let budget = case.budget();
    for (key, expected) in [
        ("max_input_bytes", budget.max_input_bytes),
        ("max_objects", 11),
        ("max_resolve_depth", 16),
        ("max_stream_output_bytes", budget.max_stream_output_bytes),
        ("max_total_decode_bytes", budget.max_total_decode_bytes),
        ("max_image_pixels", budget.max_image_pixels),
        ("max_raster_output_bytes", budget.max_raster_output_bytes),
        ("max_path_segments", budget.max_path_segments),
        ("max_scene_commands", 16),
        ("max_group_depth", 8),
        ("operator_fuel", 4096),
        ("decode_fuel", 4096),
        ("watchdog_ms", 500),
    ] {
        assert_eq!(
            manifest.positive_u64("budget", key),
            Some(expected),
            "{} budget.{key} changed",
            case.id()
        );
    }
}

fn independently_model_ready_case(case: Case) -> (u32, u32, Vec<u8>) {
    match case {
        Case::ValidPathClip => (2, 1, path_clip_rgba()),
        Case::ValidStroke => (8, 8, stroke_rgba()),
        Case::ValidImage => (2, 1, image_rgba()),
        Case::ValidFont => (8, 8, font_rgba()),
        _ => panic!("{} is not a Ready oracle case", case.id()),
    }
}

fn path_clip_rgba() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(2 * 4);
    for pixel_x in 0..2 {
        let black = sample_count(2, 1, pixel_x, 0, |x, _| x <= 50 * PAGE_DENOMINATOR);
        let red = sample_count(2, 1, pixel_x, 0, |x, _| x >= 50 * PAGE_DENOMINATOR);
        let mut channels = [Q16_ONE; 3];
        for channel in &mut channels {
            *channel = coverage_average(*channel, 0, black);
        }
        channels[0] = coverage_average(channels[0], Q16_ONE, red);
        channels[1] = coverage_average(channels[1], 0, red);
        channels[2] = coverage_average(channels[2], 0, red);
        rgba.extend(channels.map(q16_to_u8));
        rgba.push(255);
    }
    rgba
}

fn stroke_rgba() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(8 * 8 * 4);
    for pixel_y in 0..8 {
        for pixel_x in 0..8 {
            let outer = sample_count(8, 8, pixel_x, pixel_y, |x, y| {
                dashed_rectangle_contains(Rectangle::new(5, 5, 95, 95), x, y)
            });
            let fill = sample_count(8, 8, pixel_x, pixel_y, |x, y| {
                within(x, 10 * PAGE_DENOMINATOR, 90 * PAGE_DENOMINATOR)
                    && within(y, 10 * PAGE_DENOMINATOR, 90 * PAGE_DENOMINATOR)
            });
            let inner = sample_count(8, 8, pixel_x, pixel_y, |x, y| {
                dashed_rectangle_contains(Rectangle::new(10, 10, 90, 90), x, y)
            });
            let after_outer = coverage_average(Q16_ONE, 0, outer);
            let after_fill = coverage_average(after_outer, Q16_GRAY_HALF, fill);
            let channel = coverage_average(after_fill, 0, inner);
            let gray = q16_to_u8(channel);
            rgba.extend([gray, gray, gray, 255]);
        }
    }
    rgba
}

fn image_rgba() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(2 * 4);
    for pixel_x in 0..2_usize {
        let image_x_numerator = 2 * pixel_x + 1;
        let texel = (image_x_numerator * 2) / 4;
        let offset = texel * 3;
        rgba.extend_from_slice(&IMAGE_COMPONENTS[offset..offset + 3]);
        rgba.push(255);
    }
    rgba
}

fn font_rgba() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(8 * 8 * 4);
    for pixel_y in 0..8 {
        for pixel_x in 0..8 {
            let coverage = sample_count(8, 8, pixel_x, pixel_y, |x, y| {
                let device_x = x;
                let device_y = 100 * PAGE_DENOMINATOR - y;
                device_x < device_y
            });
            let gray = q16_to_u8(coverage_average(Q16_ONE, 0, coverage));
            rgba.extend([gray, gray, gray, 255]);
        }
    }
    rgba
}

#[derive(Clone, Copy)]
struct Rectangle {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl Rectangle {
    const fn new(x0: i64, y0: i64, x1: i64, y1: i64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    const fn width(self) -> i64 {
        self.x1 - self.x0
    }

    const fn height(self) -> i64 {
        self.y1 - self.y0
    }

    const fn perimeter(self) -> i64 {
        2 * (self.width() + self.height())
    }

    const fn sides(self) -> [Side; 4] {
        let width = self.width();
        let height = self.height();
        [
            Side::new(0, width, self.x0, self.y0, self.x1, self.y0),
            Side::new(width, width + height, self.x1, self.y0, self.x1, self.y1),
            Side::new(
                width + height,
                2 * width + height,
                self.x1,
                self.y1,
                self.x0,
                self.y1,
            ),
            Side::new(
                2 * width + height,
                2 * (width + height),
                self.x0,
                self.y1,
                self.x0,
                self.y0,
            ),
        ]
    }

    const fn vertices(self) -> [(i64, PagePoint); 4] {
        let width = self.width();
        let height = self.height();
        [
            (0, PagePoint::new(self.x0, self.y0)),
            (width, PagePoint::new(self.x1, self.y0)),
            (width + height, PagePoint::new(self.x1, self.y1)),
            (2 * width + height, PagePoint::new(self.x0, self.y1)),
        ]
    }
}

#[derive(Clone, Copy)]
struct Side {
    t0: i64,
    t1: i64,
    start: PagePoint,
    end: PagePoint,
}

impl Side {
    const fn new(t0: i64, t1: i64, x0: i64, y0: i64, x1: i64, y1: i64) -> Self {
        Self {
            t0,
            t1,
            start: PagePoint::new(x0, y0),
            end: PagePoint::new(x1, y1),
        }
    }

    fn point_at(self, t: i64) -> PagePoint {
        let offset = t - self.t0;
        PagePoint::new(
            self.start.x + (self.end.x - self.start.x).signum() * offset,
            self.start.y + (self.end.y - self.start.y).signum() * offset,
        )
    }
}

#[derive(Clone, Copy)]
struct PagePoint {
    x: i64,
    y: i64,
}

impl PagePoint {
    const fn new(x: i64, y: i64) -> Self {
        Self { x, y }
    }
}

fn dashed_rectangle_contains(rectangle: Rectangle, sample_x: i64, sample_y: i64) -> bool {
    let perimeter = rectangle.perimeter();
    for side in rectangle.sides() {
        let mut dash_start = 0;
        while dash_start < perimeter {
            let dash_end = (dash_start + 4).min(perimeter);
            let overlap_start = dash_start.max(side.t0);
            let overlap_end = dash_end.min(side.t1);
            if overlap_start < overlap_end {
                let start = side.point_at(overlap_start);
                let end = side.point_at(overlap_end);
                if segment_stroke_contains(start, end, sample_x, sample_y) {
                    return true;
                }
            }
            dash_start += 6;
        }
    }

    for (t, vertex) in rectangle.vertices() {
        if dash_is_on_before(t, perimeter)
            && dash_is_on_after(t)
            && within(
                sample_x,
                (vertex.x - 1) * PAGE_DENOMINATOR,
                (vertex.x + 1) * PAGE_DENOMINATOR,
            )
            && within(
                sample_y,
                (vertex.y - 1) * PAGE_DENOMINATOR,
                (vertex.y + 1) * PAGE_DENOMINATOR,
            )
        {
            return true;
        }
    }
    false
}

fn segment_stroke_contains(start: PagePoint, end: PagePoint, sample_x: i64, sample_y: i64) -> bool {
    if start.y == end.y {
        within(
            sample_x,
            start.x.min(end.x) * PAGE_DENOMINATOR,
            start.x.max(end.x) * PAGE_DENOMINATOR,
        ) && within(
            sample_y,
            (start.y - 1) * PAGE_DENOMINATOR,
            (start.y + 1) * PAGE_DENOMINATOR,
        )
    } else {
        debug_assert_eq!(start.x, end.x);
        within(
            sample_x,
            (start.x - 1) * PAGE_DENOMINATOR,
            (start.x + 1) * PAGE_DENOMINATOR,
        ) && within(
            sample_y,
            start.y.min(end.y) * PAGE_DENOMINATOR,
            start.y.max(end.y) * PAGE_DENOMINATOR,
        )
    }
}

const fn dash_is_on_after(t: i64) -> bool {
    t % 6 < 4
}

const fn dash_is_on_before(t: i64, perimeter: i64) -> bool {
    let remainder = if t == 0 { perimeter % 6 } else { t % 6 };
    remainder > 0 && remainder <= 4
}

fn sample_count(
    width: u32,
    height: u32,
    pixel_x: u32,
    pixel_y: u32,
    mut predicate: impl FnMut(i64, i64) -> bool,
) -> u32 {
    let mut covered = 0;
    for sample_y in 0..SAMPLE_SIDE {
        for sample_x in 0..SAMPLE_SIDE {
            let page_x =
                200 * (16 * i64::from(pixel_x) + 2 * i64::from(sample_x) + 1) / i64::from(width);
            let page_y = 100 * PAGE_DENOMINATOR
                - 200 * (16 * i64::from(pixel_y) + 2 * i64::from(sample_y) + 1) / i64::from(height);
            if predicate(page_x, page_y) {
                covered += 1;
            }
        }
    }
    covered
}

fn coverage_average(backdrop: u32, painted: u32, covered: u32) -> u32 {
    assert!(covered <= SAMPLES_PER_PIXEL);
    let uncovered = SAMPLES_PER_PIXEL - covered;
    let numerator = u64::from(backdrop) * u64::from(uncovered)
        + u64::from(painted) * u64::from(covered)
        + u64::from(SAMPLES_PER_PIXEL / 2);
    u32::try_from(numerator / u64::from(SAMPLES_PER_PIXEL))
        .expect("bounded Q16 coverage average fits u32")
}

fn q16_to_u8(value: u32) -> u8 {
    assert!(value <= Q16_ONE);
    u8::try_from((u64::from(value) * 255 + u64::from(Q16_ONE / 2)) / u64::from(Q16_ONE))
        .expect("bounded Q16 channel fits u8")
}

const fn within(value: i64, minimum: i64, maximum: i64) -> bool {
    value >= minimum && value <= maximum
}

fn encode_pixel_contract(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    assert_eq!(
        rgba.len(),
        usize::try_from(u64::from(width) * u64::from(height) * 4)
            .expect("bounded output length fits usize")
    );
    let mut hex = String::with_capacity(rgba.len() * 2);
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for byte in rgba {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    format!("{{\"height\":{height},\"rgba_hex\":\"{hex}\",\"schema\":1,\"width\":{width}}}")
        .into_bytes()
}

const fn path_spec(salt: u8) -> FixtureSpec {
    FixtureSpec {
        content: PATH_CLIP_CONTENT,
        image: None,
        font: false,
        invalid_startxref: false,
        salt,
    }
}

const fn image_spec(interpolate: bool, salt: u8) -> FixtureSpec {
    FixtureSpec {
        content: IMAGE_CONTENT,
        image: Some(ImageSpec { interpolate }),
        font: false,
        invalid_startxref: false,
        salt,
    }
}

fn digest_reference(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex_digest(&sha256(bytes).expect("bounded case artifact fits SHA-256 framing"))
    )
}

fn collect_relative_files(root: &Path, current: &Path, output: &mut Vec<String>) {
    for entry in fs::read_dir(current)
        .unwrap_or_else(|error| panic!("{} is readable: {error}", current.display()))
    {
        let entry = entry.expect("case entry is readable");
        let metadata = fs::symlink_metadata(entry.path()).expect("case entry metadata is readable");
        assert!(
            !metadata.file_type().is_symlink(),
            "{} must not be a symbolic link",
            entry.path().display()
        );
        if metadata.is_dir() {
            collect_relative_files(root, &entry.path(), output);
        } else {
            assert!(
                metadata.is_file(),
                "{} must be a regular file",
                entry.path().display()
            );
            output.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .expect("case file remains below case root")
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
}

fn case_directory(case: Case) -> PathBuf {
    case_root().join(case.slug())
}

fn case_root() -> PathBuf {
    repository_root().join("tests/cases/raster/m3-reference")
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root is canonical")
}
