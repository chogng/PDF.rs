use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_quality::case_contract::validate_case_file;
use pdf_rs_quality::manifest::CaseManifest;

use super::Case;

const CASE_PREFIX: &str = "raster/m3-reference/";
const REQUIRED_RUNNER: &str = "tools/quality::m3_reference_gate";
const EXPECTED_CASES: usize = 12;

pub(super) const REFERENCE_IMPLEMENTATION_SHA256: &str =
    "sha256:0088e35c0824ab38b7e2ba41ff56c89d9bf246b611e968cee19cc36475327f5b";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExpectedFlags {
    pub(super) parse: bool,
    pub(super) scene: bool,
    pub(super) text: bool,
    pub(super) pixel: bool,
    pub(super) diagnostic: bool,
    pub(super) capability: bool,
    pub(super) error: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExpectedTerminal {
    pub(super) outcome: &'static str,
    pub(super) stage: &'static str,
    pub(super) diagnostic_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CaseContract {
    pub(super) case: Case,
    pub(super) manifest_sha256: String,
    pub(super) input: Vec<u8>,
    pub(super) input_sha256: String,
    pub(super) oracle_level: String,
    pub(super) pixel_oracle_level: Option<String>,
    pub(super) expected_pixel_sha256: Option<String>,
    pub(super) expected_pixel: Option<Vec<u8>>,
    pub(super) expected: ExpectedFlags,
    pub(super) terminal: ExpectedTerminal,
    pub(super) max_input_bytes: u64,
    pub(super) max_objects: u64,
    pub(super) max_resolve_depth: u64,
    pub(super) max_stream_output_bytes: u64,
    pub(super) max_total_decode_bytes: u64,
    pub(super) max_image_pixels: u64,
    pub(super) max_raster_output_bytes: u64,
    pub(super) max_path_segments: u64,
    pub(super) max_scene_commands: u64,
    pub(super) max_group_depth: u64,
    pub(super) operator_fuel: u64,
    pub(super) decode_fuel: u64,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl CaseContract {
    pub(super) fn id(&self) -> &'static str {
        self.case.id()
    }

    pub(super) fn assert_observed(
        &self,
        outcome: &str,
        stage: &str,
        diagnostic_id: Option<&str>,
        scene: Option<&[u8]>,
        pixel: Option<&[u8]>,
    ) {
        assert_eq!(
            outcome,
            self.terminal.outcome,
            "case={} outcome differs from its formal manifest contract",
            self.id()
        );
        assert_eq!(
            stage,
            self.terminal.stage,
            "case={} stage differs from its formal manifest contract",
            self.id()
        );
        assert_eq!(
            diagnostic_id,
            self.terminal.diagnostic_id.as_deref(),
            "case={} diagnostic differs from validity.strict_expected",
            self.id()
        );

        let observed = ExpectedFlags {
            parse: stage != "strict-open",
            scene: scene.is_some(),
            text: false,
            pixel: pixel.is_some(),
            diagnostic: diagnostic_id.is_some(),
            capability: outcome == "unsupported",
            error: !matches!(outcome, "ready" | "unsupported"),
        };
        assert_eq!(
            observed,
            self.expected,
            "case={} observed publication flags differ from [expected]",
            self.id()
        );
        match (&self.expected_pixel, pixel) {
            (Some(expected), Some(actual)) => assert_eq!(
                actual,
                expected,
                "case={} Reference output differs byte-for-byte from the committed pixel oracle",
                self.id()
            ),
            (None, None) => {}
            _ => panic!(
                "case={} committed and observed pixel publication disagree",
                self.id()
            ),
        }
    }
}

pub(super) fn load_registry() -> Vec<CaseContract> {
    let root = case_root();
    assert_regular_directory(&root, "M3 Reference case root");
    let mut directories = fs::read_dir(&root)
        .expect("M3 Reference case root is readable")
        .map(|entry| entry.expect("M3 Reference case entry is readable"))
        .collect::<Vec<_>>();
    directories.sort_by_key(|entry| entry.file_name());

    let mut contracts = Vec::with_capacity(directories.len());
    let mut ids = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for entry in directories {
        let file_type = entry
            .file_type()
            .expect("M3 Reference case entry type is readable");
        assert!(
            !file_type.is_symlink() && file_type.is_dir(),
            "{} must be a regular case directory",
            entry.path().display()
        );
        let slug = entry
            .file_name()
            .into_string()
            .expect("M3 Reference case slug is UTF-8");
        let contract = load_case(&entry.path(), &slug);
        assert!(
            ids.insert(contract.id()),
            "duplicate M3 Reference case id {}",
            contract.id()
        );
        assert!(
            kinds.insert(contract.case),
            "duplicate M3 Reference behavior selector {:?}",
            contract.case
        );
        contracts.push(contract);
    }

    assert_eq!(
        contracts.len(),
        EXPECTED_CASES,
        "formal M3 Reference registry must contain exactly twelve cases"
    );
    assert_eq!(
        kinds.len(),
        EXPECTED_CASES,
        "formal M3 Reference registry must cover every behavior selector exactly once"
    );
    contracts
}

fn load_case(directory: &Path, slug: &str) -> CaseContract {
    let manifest_path = directory.join("case.toml");
    let manifest_bytes = read_regular_file(&manifest_path, "case manifest");
    let manifest = validate_case_file(&manifest_path).unwrap_or_else(|diagnostics| {
        panic!("{} invalid: {diagnostics:?}", manifest_path.display())
    });
    let expected_id = format!("{CASE_PREFIX}{slug}");
    assert_eq!(
        manifest.case_id(),
        expected_id,
        "{} identity.id must match its registry directory",
        manifest_path.display()
    );
    let case = Case::from_id(manifest.case_id())
        .unwrap_or_else(|| panic!("unregistered M3 Reference behavior {}", manifest.case_id()));
    assert_eq!(case.id(), manifest.case_id());

    let expected_source = format!("tests/cases/raster/m3-reference/{slug}/input.pdf");
    assert_eq!(
        manifest.string("provenance", "source"),
        Some(expected_source.as_str()),
        "case={} provenance.source must name its committed input",
        case.id()
    );
    assert!(
        manifest
            .string_array("runners", "native")
            .expect("validated case has native runners")
            .contains(&REQUIRED_RUNNER),
        "case={} must register the integrated Reference gate",
        case.id()
    );
    assert_eq!(manifest.string("tolerance", "mode"), Some("exact"));
    assert_eq!(
        manifest.boolean("oracle", "reference_may_generate"),
        Some(false),
        "case={} case-wide authority may not be Reference-generated",
        case.id()
    );

    let input_path = directory.join("input.pdf");
    let input = read_regular_file(&input_path, "case input");
    let max_input_bytes = budget(&manifest, "max_input_bytes");
    assert_eq!(
        u64::try_from(input.len()).expect("case input length fits u64"),
        max_input_bytes,
        "case={} input budget must be exact",
        case.id()
    );
    let input_sha256 = digest(&input);
    assert_eq!(
        input_sha256,
        manifest.source_sha256(),
        "case={} committed input differs from provenance.sha256",
        case.id()
    );

    let expected = ExpectedFlags {
        parse: flag(&manifest, "parse"),
        scene: flag(&manifest, "scene"),
        text: flag(&manifest, "text"),
        pixel: flag(&manifest, "pixel"),
        diagnostic: flag(&manifest, "diagnostic"),
        capability: flag(&manifest, "capability"),
        error: flag(&manifest, "error"),
    };
    let terminal = expected_terminal(&manifest);
    assert_eq!(expected.diagnostic, terminal.diagnostic_id.is_some());
    assert_eq!(expected.pixel, terminal.outcome == "ready");
    assert_eq!(
        expected.scene,
        matches!(terminal.outcome, "ready" | "cancelled")
            || terminal.stage == "reference-preflight"
    );
    assert_eq!(expected.capability, terminal.outcome == "unsupported");
    assert_eq!(
        expected.error,
        !matches!(terminal.outcome, "ready" | "unsupported")
    );

    let expected_pixel_sha256 = manifest
        .string("expected", "pixel_sha256")
        .map(str::to_owned);
    let expected_pixel = manifest
        .string("expected", "pixel_artifact")
        .map(|relative| read_regular_file(&directory.join(relative), "expected pixel artifact"));
    assert_eq!(expected_pixel.is_some(), expected.pixel);
    assert_eq!(expected_pixel_sha256.is_some(), expected.pixel);
    if let (Some(bytes), Some(expected_hash)) = (&expected_pixel, &expected_pixel_sha256) {
        assert_eq!(
            digest(bytes),
            *expected_hash,
            "case={} committed pixel differs from expected.pixel_sha256",
            case.id()
        );
    }

    let oracle_level = manifest
        .string("oracle", "level")
        .expect("validated case has oracle.level")
        .to_owned();
    assert!(
        matches!(oracle_level.as_str(), "O0" | "O1"),
        "case={} formal semantic authority must be O0 or O1",
        case.id()
    );
    let pixel_oracle_level = manifest.string("pixel_oracle", "level").map(str::to_owned);
    validate_pixel_oracle(
        &manifest,
        directory,
        case,
        pixel_oracle_level.as_deref(),
        expected.pixel,
    );

    let width = render_dimension(&manifest, "width");
    let height = render_dimension(&manifest, "height");
    let max_image_pixels = budget(&manifest, "max_image_pixels");
    let output_pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .expect("M3 render pixel count fits u64");
    assert!(
        output_pixels <= max_image_pixels,
        "case={} render dimensions exceed budget.max_image_pixels",
        case.id()
    );

    CaseContract {
        case,
        manifest_sha256: digest(&manifest_bytes),
        input,
        input_sha256,
        oracle_level,
        pixel_oracle_level,
        expected_pixel_sha256,
        expected_pixel,
        expected,
        terminal,
        max_input_bytes,
        max_objects: budget(&manifest, "max_objects"),
        max_resolve_depth: budget(&manifest, "max_resolve_depth"),
        max_stream_output_bytes: budget(&manifest, "max_stream_output_bytes"),
        max_total_decode_bytes: budget(&manifest, "max_total_decode_bytes"),
        max_image_pixels,
        max_raster_output_bytes: budget(&manifest, "max_raster_output_bytes"),
        max_path_segments: budget(&manifest, "max_path_segments"),
        max_scene_commands: budget(&manifest, "max_scene_commands"),
        max_group_depth: budget(&manifest, "max_group_depth"),
        operator_fuel: budget(&manifest, "operator_fuel"),
        decode_fuel: budget(&manifest, "decode_fuel"),
        width,
        height,
    }
}

fn validate_pixel_oracle(
    manifest: &CaseManifest,
    directory: &Path,
    case: Case,
    level: Option<&str>,
    publishes_pixel: bool,
) {
    assert_eq!(
        level.is_some(),
        publishes_pixel,
        "case={} pixel publication and pixel_oracle must agree",
        case.id()
    );
    match level {
        None => {
            assert!(manifest.raw("pixel_oracle", "reference_identity").is_none());
            assert!(manifest.raw("pixel_oracle", "review_evidence").is_none());
            assert!(
                manifest
                    .raw("pixel_oracle", "review_evidence_sha256")
                    .is_none()
            );
        }
        Some("O0" | "O1") => {
            assert_eq!(
                manifest.boolean("pixel_oracle", "reference_may_generate"),
                Some(false)
            );
            assert!(manifest.raw("pixel_oracle", "reference_identity").is_none());
            assert!(manifest.raw("pixel_oracle", "review_evidence").is_none());
            assert!(
                manifest
                    .raw("pixel_oracle", "review_evidence_sha256")
                    .is_none()
            );
        }
        Some("O3") => {
            assert_eq!(
                case,
                Case::ValidMixed,
                "only the reviewed mixed case may use O3 pixels"
            );
            assert_eq!(
                manifest.boolean("pixel_oracle", "reference_may_generate"),
                Some(true)
            );
            assert_eq!(
                manifest.string_array("pixel_oracle", "reviewers"),
                Some(vec!["spec-conformance", "parser-security"])
            );
            let identity_reference = manifest
                .string("pixel_oracle", "reference_identity")
                .expect("O3 identity is required");
            let (identity_path, _) = identity_reference
                .rsplit_once("#sha256:")
                .expect("validated O3 identity is content-addressed");
            let identity = read_regular_file(
                &directory.join(identity_path),
                "O3 Reference implementation identity",
            );
            let expected_identity = format!(
                "{{\"algorithm\":\"reference-raster-v1\",\"implementation_sha256\":\"{REFERENCE_IMPLEMENTATION_SHA256}\",\"schema\":1}}"
            );
            assert_eq!(
                identity,
                expected_identity.as_bytes(),
                "mixed O3 identity must bind the reviewed pdf-rs/raster implementation"
            );
            assert!(manifest.string("pixel_oracle", "review_evidence").is_some());
            assert!(
                manifest
                    .string("pixel_oracle", "review_evidence_sha256")
                    .is_some()
            );
        }
        Some(other) => panic!(
            "case={} unsupported M3 Reference pixel oracle {other}",
            case.id()
        ),
    }
}

fn expected_terminal(manifest: &CaseManifest) -> ExpectedTerminal {
    let strict = manifest
        .string("validity", "strict_expected")
        .expect("validated case has validity.strict_expected");
    let (outcome, stage, diagnostic_id) = match strict {
        "success" => ("ready", "reference-render", None),
        "RPE-CONTENT-UNSUPPORTED-0009" => ("unsupported", "content-image", Some(strict.to_owned())),
        "RPE-CONTENT-VM-0007" => ("failed", "content-vm", Some(strict.to_owned())),
        "RPE-XREF-0011" => ("failed", "strict-open", Some(strict.to_owned())),
        "RPE-RASTER-0004" => (
            "cancelled",
            "reference-publication",
            Some(strict.to_owned()),
        ),
        "RPE-CONTENT-VM-0014" => (
            "source-changed",
            "content-vm-resume",
            Some(strict.to_owned()),
        ),
        "RPE-CONTENT-VM-0012" => ("resource-limited", "content-image", Some(strict.to_owned())),
        "RPE-RASTER-0005" => (
            "resource-limited",
            "reference-preflight",
            Some(strict.to_owned()),
        ),
        other => panic!("unregistered M3 Reference terminal diagnostic {other}"),
    };
    ExpectedTerminal {
        outcome,
        stage,
        diagnostic_id,
    }
}

fn budget(manifest: &CaseManifest, key: &str) -> u64 {
    manifest
        .positive_u64("budget", key)
        .unwrap_or_else(|| panic!("validated M3 Reference manifest has budget.{key}"))
}

fn render_dimension(manifest: &CaseManifest, key: &str) -> u32 {
    u32::try_from(
        manifest
            .positive_u64("render", key)
            .unwrap_or_else(|| panic!("validated M3 Reference manifest has render.{key}")),
    )
    .unwrap_or_else(|_| panic!("render.{key} fits u32"))
}

fn flag(manifest: &CaseManifest, key: &str) -> bool {
    manifest
        .boolean("expected", key)
        .unwrap_or_else(|| panic!("validated M3 Reference manifest has expected.{key}"))
}

fn case_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/cases/raster/m3-reference")
}

fn assert_regular_directory(path: &Path, label: &str) {
    let metadata =
        fs::symlink_metadata(path).unwrap_or_else(|error| panic!("{label} metadata: {error}"));
    assert!(!metadata.file_type().is_symlink(), "{label} is a symlink");
    assert!(metadata.file_type().is_dir(), "{label} is not a directory");
}

fn read_regular_file(path: &Path, label: &str) -> Vec<u8> {
    let metadata = fs::symlink_metadata(path)
        .unwrap_or_else(|error| panic!("{label} {} metadata: {error}", path.display()));
    assert!(
        !metadata.file_type().is_symlink() && metadata.file_type().is_file(),
        "{label} {} must be a regular file",
        path.display()
    );
    fs::read(path).unwrap_or_else(|error| panic!("{label} {} is readable: {error}", path.display()))
}

fn digest(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex_digest(&sha256(bytes).expect("formal M3 case artifact fits SHA-256 framing"))
    )
}
