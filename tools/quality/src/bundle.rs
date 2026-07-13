use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use pdf_rs_compare::{
    CanonicalJson, ParseArtifact, ParseObject, PixelArtifact, SceneArtifact, SceneCommand,
    TextArtifact, TextRun, WritingMode, canonical_json_string, compare_parse, compare_scene,
    compare_text, encode_pixel_comparison_pngs,
};
use pdf_rs_digest::{HashError, Sha256, hex_digest, sha256};
use pdf_rs_generate::generate_one_page_pdf;

use crate::manifest::{ManifestDiagnostic, validate_manifest_file};

const MAX_SYNTHETIC_INPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SYNTHETIC_PIXELS: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BundleErrorCode {
    InvalidManifest,
    GenerateFailed,
    SourceHashMismatch,
    ArtifactFailed,
    HashFailed,
    IoFailed,
    ExistingArtifactMismatch,
    MissingInput,
    InputBudgetExceeded,
    GeneratedInputMismatch,
    RenderBudgetExceeded,
    CaseContractMismatch,
    SyntheticBudgetExceeded,
}

#[derive(Debug)]
pub struct BundleError {
    pub code: BundleErrorCode,
    pub diagnostic_id: &'static str,
    detail: &'static str,
    manifest_diagnostics: Vec<ManifestDiagnostic>,
}

impl BundleError {
    fn new(code: BundleErrorCode, diagnostic_id: &'static str, detail: &'static str) -> Self {
        Self {
            code,
            diagnostic_id,
            detail,
            manifest_diagnostics: Vec::new(),
        }
    }

    fn manifest(diagnostics: Vec<ManifestDiagnostic>) -> Self {
        Self {
            code: BundleErrorCode::InvalidManifest,
            diagnostic_id: "RPE-BUNDLE-0001",
            detail: "case manifest validation failed",
            manifest_diagnostics: diagnostics,
        }
    }

    pub fn manifest_diagnostics(&self) -> &[ManifestDiagnostic] {
        &self.manifest_diagnostics
    }
}

impl fmt::Display for BundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} ({:?}): {}",
            self.diagnostic_id, self.code, self.detail
        )
    }
}

impl std::error::Error for BundleError {}

/// Generates an intentional synthetic disagreement and writes a deterministic,
/// content-addressed failure bundle. It never invokes an external baseline.
pub fn build_synthetic_failure_bundle(
    case_manifest: &Path,
    output_root: &Path,
) -> Result<PathBuf, BundleError> {
    let manifest = validate_manifest_file(case_manifest).map_err(BundleError::manifest)?;
    validate_synthetic_case_contract(&manifest)?;
    let manifest_bytes = fs::read(case_manifest).map_err(|_| io_error())?;
    let case_directory = case_manifest.parent().ok_or_else(missing_input)?;
    let input_path = case_directory.join("input.pdf");
    let input_metadata = fs::symlink_metadata(&input_path).map_err(|_| missing_input())?;
    if !input_metadata.file_type().is_file() {
        return Err(missing_input());
    }
    let max_input_bytes = manifest
        .positive_u64("budget", "max_input_bytes")
        .expect("validated positive input budget is available");
    if max_input_bytes > MAX_SYNTHETIC_INPUT_BYTES || input_metadata.len() > max_input_bytes {
        return Err(input_budget_error());
    }
    let read_limit = max_input_bytes
        .checked_add(1)
        .ok_or_else(input_budget_error)?;
    let capacity = usize::try_from(read_limit).map_err(|_| input_budget_error())?;
    let mut pdf = Vec::new();
    pdf.try_reserve_exact(capacity)
        .map_err(|_| input_budget_error())?;
    File::open(&input_path)
        .map_err(|_| missing_input())?
        .take(read_limit)
        .read_to_end(&mut pdf)
        .map_err(|_| io_error())?;
    if u64::try_from(pdf.len()).map_err(|_| input_budget_error())? > max_input_bytes {
        return Err(input_budget_error());
    }
    let generated_pdf = generate_one_page_pdf().map_err(|_| {
        BundleError::new(
            BundleErrorCode::GenerateFailed,
            "RPE-BUNDLE-0002",
            "minimal PDF generation failed",
        )
    })?;
    if pdf != generated_pdf {
        return Err(BundleError::new(
            BundleErrorCode::GeneratedInputMismatch,
            "RPE-BUNDLE-0010",
            "adjacent input does not match the declared generator output",
        ));
    }
    let pdf_hash = sha256(&pdf).map_err(hash_error)?;
    let pdf_hash = hex_digest(&pdf_hash);
    if manifest.source_sha256() != format!("sha256:{pdf_hash}") {
        return Err(BundleError::new(
            BundleErrorCode::SourceHashMismatch,
            "RPE-BUNDLE-0003",
            "manifest source hash does not match generated input",
        ));
    }

    validate_synthetic_work_budget(&manifest)?;

    let width = u32::try_from(
        manifest
            .positive_u64("render", "width")
            .expect("validated render width is available"),
    )
    .map_err(|_| render_budget_error())?;
    let height = u32::try_from(
        manifest
            .positive_u64("render", "height")
            .expect("validated render height is available"),
    )
    .map_err(|_| render_budget_error())?;
    let pixel_count = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(render_budget_error)?;
    let max_pixels = manifest
        .positive_u64("budget", "max_image_pixels")
        .expect("validated image budget is available");
    if pixel_count > max_pixels || pixel_count > MAX_SYNTHETIC_PIXELS {
        return Err(render_budget_error());
    }

    let mut features = manifest
        .string_array("features", "ids")
        .expect("validated feature identifiers are available");
    features.sort_unstable();
    features.dedup();
    let mut artifacts = synthetic_artifacts(manifest.case_id(), &features, pdf, width, height)?;
    artifacts.insert("case-manifest.toml".into(), manifest_bytes);
    let content_address = hash_artifacts(&artifacts)?;
    let bundle_manifest = bundle_manifest(manifest.case_id(), &content_address, &artifacts);
    artifacts.insert("manifest.toml".into(), bundle_manifest.into_bytes());

    let bundle_path = output_root.join(&content_address);
    fs::create_dir_all(&bundle_path).map_err(|_| io_error())?;
    for (name, bytes) in &artifacts {
        write_new_or_verify(&bundle_path.join(name), bytes)?;
    }
    Ok(bundle_path)
}

fn synthetic_artifacts(
    case_id: &str,
    features: &[&str],
    pdf: Vec<u8>,
    width: u32,
    height: u32,
) -> Result<BTreeMap<String, Vec<u8>>, BundleError> {
    let reference_parse = ParseArtifact::new(
        1,
        vec![
            ParseObject::new(1, 0, "catalog", "synthetic-catalog-v1"),
            ParseObject::new(2, 0, "pages", "synthetic-pages-v1"),
            ParseObject::new(3, 0, "page", "synthetic-page-v1"),
            ParseObject::new(4, 0, "stream", "synthetic-stream-v1"),
        ],
        vec![],
    );
    let native_parse = reference_parse.clone();

    let identity = [1_000_000, 0, 0, 1_000_000, 0, 0];
    let reference_scene = SceneArtifact::new(
        1,
        vec![
            SceneCommand::new("save", "q", Some(4), identity),
            SceneCommand::new("restore", "Q", Some(4), identity),
        ],
    );
    let native_scene = SceneArtifact::new(
        1,
        vec![SceneCommand::new(
            "save",
            "synthetic-intentional-mismatch",
            Some(4),
            identity,
        )],
    );

    let reference_text = TextArtifact::new(
        1,
        vec![TextRun::new(
            vec![80, 68, 70],
            [0, 0, 3_000_000, 0, 3_000_000, 1_000_000, 0, 1_000_000],
            "PDF",
            WritingMode::HorizontalLtr,
        )],
    );
    let native_text = TextArtifact::new(
        1,
        vec![TextRun::new(
            vec![80, 66, 70],
            [0, 0, 3_000_000, 0, 3_000_000, 1_000_000, 0, 1_000_000],
            "PBF",
            WritingMode::HorizontalLtr,
        )],
    );

    let rgba_len = pixel_count_to_rgba_len(width, height)?;
    let mut baseline_rgba = Vec::new();
    baseline_rgba
        .try_reserve_exact(rgba_len)
        .map_err(|_| artifact_error())?;
    baseline_rgba.resize(rgba_len, 255);
    let mut native_rgba = Vec::new();
    native_rgba
        .try_reserve_exact(rgba_len)
        .map_err(|_| artifact_error())?;
    native_rgba.extend_from_slice(&baseline_rgba);
    native_rgba[0] = 0;
    native_rgba[1] = 64;
    let native_pixels =
        PixelArtifact::new(width, height, native_rgba).map_err(|_| artifact_error())?;
    let baseline_pixels =
        PixelArtifact::new(width, height, baseline_rgba).map_err(|_| artifact_error())?;
    let pngs = encode_pixel_comparison_pngs(&native_pixels, &baseline_pixels)
        .map_err(|_| artifact_error())?;

    let parse_diff = compare_parse(&reference_parse, &native_parse);
    let scene_diff = compare_scene(&reference_scene, &native_scene);
    let text_diff = compare_text(&reference_text, &native_text);
    let diagnostics = format!(
        "{{\"parse\":{},\"pixel\":{},\"scene\":{},\"schema\":1,\"text\":{}}}",
        parse_diff.to_canonical_json(),
        pngs.summary().to_canonical_json(),
        scene_diff.to_canonical_json(),
        text_diff.to_canonical_json(),
    );

    let quoted_case = canonical_json_string(case_id);
    let encoded_features = features
        .iter()
        .map(|feature| canonical_json_string(feature))
        .collect::<Vec<_>>()
        .join(",");
    let mut artifacts = BTreeMap::new();
    artifacts.insert("minimized.pdf".into(), pdf);
    artifacts.insert(
        "feature-report.json".into(),
        format!("{{\"case_id\":{quoted_case},\"features\":[{encoded_features}],\"schema\":1}}")
            .into_bytes(),
    );
    artifacts.insert("diagnostics.json".into(), diagnostics.into_bytes());
    artifacts.insert(
        "parse-native.json".into(),
        native_parse.to_canonical_json().into_bytes(),
    );
    artifacts.insert(
        "parse-reference.json".into(),
        reference_parse.to_canonical_json().into_bytes(),
    );
    artifacts.insert(
        "parse-diff.json".into(),
        parse_diff.to_canonical_json().into_bytes(),
    );
    artifacts.insert(
        "scene-native.json".into(),
        native_scene.to_canonical_json().into_bytes(),
    );
    artifacts.insert(
        "scene-reference.json".into(),
        reference_scene.to_canonical_json().into_bytes(),
    );
    artifacts.insert(
        "text-native.json".into(),
        native_text.to_canonical_json().into_bytes(),
    );
    artifacts.insert(
        "text-baseline.json".into(),
        reference_text.to_canonical_json().into_bytes(),
    );
    artifacts.insert("native.png".into(), pngs.native_png().to_vec());
    artifacts.insert("baseline.png".into(), pngs.baseline_png().to_vec());
    artifacts.insert("diff.png".into(), pngs.difference_png().to_vec());
    artifacts.insert(
        "capability-decision.json".into(),
        b"{\"policy_version\":1,\"profile\":\"m0.synthetic-artifacts.v1\",\"status\":\"supported\"}"
            .to_vec(),
    );
    artifacts.insert(
        "protocol-trace.json".into(),
        b"{\"events\":[],\"privacy\":\"no-document-content\",\"schema\":1}".to_vec(),
    );
    artifacts.insert(
        "environment.json".into(),
        b"{\"environment\":\"deterministic-synthetic\",\"schema\":1}".to_vec(),
    );
    Ok(artifacts)
}

fn hash_artifacts(artifacts: &BTreeMap<String, Vec<u8>>) -> Result<String, BundleError> {
    let mut hasher = Sha256::new();
    hasher
        .update(b"PDFRS-FAILURE-BUNDLE-1")
        .map_err(hash_error)?;
    for (name, bytes) in artifacts {
        let name_len =
            u64::try_from(name.len()).map_err(|_| hash_error(HashError::InputTooLong))?;
        let bytes_len =
            u64::try_from(bytes.len()).map_err(|_| hash_error(HashError::InputTooLong))?;
        hasher.update(&name_len.to_be_bytes()).map_err(hash_error)?;
        hasher.update(name.as_bytes()).map_err(hash_error)?;
        hasher
            .update(&bytes_len.to_be_bytes())
            .map_err(hash_error)?;
        hasher.update(bytes).map_err(hash_error)?;
    }
    Ok(format!(
        "sha256-{}",
        hex_digest(&hasher.finalize().map_err(hash_error)?)
    ))
}

fn bundle_manifest(
    case_id: &str,
    content_address: &str,
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> String {
    let mut output = format!(
        "schema = 1\ncase_id = \"{case_id}\"\ncontent_address = \"{content_address}\"\nprivacy = \"self-authored-synthetic\"\n"
    );
    output.push_str("artifacts = [\n");
    for name in artifacts.keys() {
        output.push_str(&format!("  \"{name}\",\n"));
    }
    output.push_str("]\n");
    output
}

fn write_new_or_verify(path: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(bytes).map_err(|_| io_error()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = fs::read(path).map_err(|_| io_error())?;
            if existing == bytes {
                Ok(())
            } else {
                Err(BundleError::new(
                    BundleErrorCode::ExistingArtifactMismatch,
                    "RPE-BUNDLE-0007",
                    "content-addressed artifact already exists with different bytes",
                ))
            }
        }
        Err(_) => Err(io_error()),
    }
}

fn artifact_error() -> BundleError {
    BundleError::new(
        BundleErrorCode::ArtifactFailed,
        "RPE-BUNDLE-0004",
        "synthetic artifact construction failed",
    )
}

fn hash_error(_error: HashError) -> BundleError {
    BundleError::new(
        BundleErrorCode::HashFailed,
        "RPE-BUNDLE-0005",
        "content-address calculation failed",
    )
}

fn io_error() -> BundleError {
    BundleError::new(
        BundleErrorCode::IoFailed,
        "RPE-BUNDLE-0006",
        "failure bundle filesystem operation failed",
    )
}

fn missing_input() -> BundleError {
    BundleError::new(
        BundleErrorCode::MissingInput,
        "RPE-BUNDLE-0008",
        "case manifest must have an adjacent input.pdf",
    )
}

fn input_budget_error() -> BundleError {
    BundleError::new(
        BundleErrorCode::InputBudgetExceeded,
        "RPE-BUNDLE-0009",
        "fixture input exceeds its manifest byte budget",
    )
}

fn render_budget_error() -> BundleError {
    BundleError::new(
        BundleErrorCode::RenderBudgetExceeded,
        "RPE-BUNDLE-0011",
        "render geometry exceeds a representable or declared pixel budget",
    )
}

fn validate_synthetic_case_contract(
    manifest: &crate::manifest::CaseManifest,
) -> Result<(), BundleError> {
    let all_artifacts_expected = [
        "parse",
        "scene",
        "text",
        "pixel",
        "diagnostic",
        "capability",
    ]
    .into_iter()
    .all(|key| manifest.boolean("expected", key) == Some(true));
    let contract_matches = all_artifacts_expected
        && manifest.boolean("expected", "error") == Some(false)
        && manifest.string("oracle", "level") == Some("O1")
        && manifest.boolean("oracle", "reference_may_generate") == Some(false)
        && manifest.string("tolerance", "mode") == Some("exact");
    if contract_matches {
        Ok(())
    } else {
        Err(BundleError::new(
            BundleErrorCode::CaseContractMismatch,
            "RPE-BUNDLE-0012",
            "case contract does not authorize the complete exact O1 synthetic bundle",
        ))
    }
}

fn validate_synthetic_work_budget(
    manifest: &crate::manifest::CaseManifest,
) -> Result<(), BundleError> {
    let within_budget = [
        ("max_objects", 4_u64),
        ("max_resolve_depth", 3),
        ("max_scene_commands", 2),
        ("operator_fuel", 2),
    ]
    .into_iter()
    .all(|(key, required)| {
        manifest
            .positive_u64("budget", key)
            .is_some_and(|available| available >= required)
    });
    if within_budget {
        Ok(())
    } else {
        Err(BundleError::new(
            BundleErrorCode::SyntheticBudgetExceeded,
            "RPE-BUNDLE-0013",
            "synthetic parse or scene work exceeds the declared case budget",
        ))
    }
}

fn pixel_count_to_rgba_len(width: u32, height: u32) -> Result<usize, BundleError> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(render_budget_error)?;
    usize::try_from(bytes).map_err(|_| render_budget_error())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use pdf_rs_generate::generate_one_page_pdf;

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn emits_a_complete_deterministic_bundle() {
        let root = temp_dir("complete");
        let manifest_path = root.join("case.toml");
        write_case(&manifest_path, &manifest_for_generated_pdf());
        let output = root.join("failure");

        let first = build_synthetic_failure_bundle(&manifest_path, &output).unwrap();
        let second = build_synthetic_failure_bundle(&manifest_path, &output).unwrap();
        assert_eq!(first, second);
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("sha256-")
        );

        for name in [
            "manifest.toml",
            "case-manifest.toml",
            "minimized.pdf",
            "feature-report.json",
            "diagnostics.json",
            "parse-native.json",
            "parse-reference.json",
            "parse-diff.json",
            "scene-native.json",
            "scene-reference.json",
            "text-native.json",
            "text-baseline.json",
            "native.png",
            "baseline.png",
            "diff.png",
            "capability-decision.json",
            "protocol-trace.json",
            "environment.json",
        ] {
            assert!(first.join(name).is_file(), "missing {name}");
        }
        let diagnostics = fs::read_to_string(first.join("diagnostics.json")).unwrap();
        assert!(diagnostics.contains("\"parse\":{\"artifact\":\"parse\",\"exact\":true"));
        assert!(diagnostics.contains("\"scene\":{\"artifact\":\"scene\",\"exact\":false"));
        assert!(diagnostics.contains("\"text\":{\"artifact\":\"text\",\"exact\":false"));
        assert!(diagnostics.contains("\"different_pixels\":1"));
        assert_eq!(
            fs::read_to_string(first.join("feature-report.json")).unwrap(),
            "{\"case_id\":\"infrastructure/synthetic-failure-bundle-001\",\"features\":[\"quality.canonical-diff\",\"quality.failure-bundle\",\"quality.minimal-pdf-generator\"],\"schema\":1}"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_a_manifest_for_different_source_bytes() {
        let root = temp_dir("hash-mismatch");
        let manifest_path = root.join("case.toml");
        let manifest = manifest_for_generated_pdf().replace(
            "sha256:",
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff#",
        );
        write_case(&manifest_path, &manifest);
        let error = build_synthetic_failure_bundle(&manifest_path, &root.join("failure"))
            .err()
            .unwrap();
        assert!(matches!(
            error.code,
            BundleErrorCode::InvalidManifest | BundleErrorCode::SourceHashMismatch
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn binds_adjacent_input_budget_and_render_configuration() {
        let root = temp_dir("bindings");
        let manifest_path = root.join("case.toml");
        let canonical = manifest_for_generated_pdf();

        fs::write(&manifest_path, &canonical).unwrap();
        let missing = build_synthetic_failure_bundle(&manifest_path, &root.join("missing"))
            .err()
            .unwrap();
        assert_eq!(missing.code, BundleErrorCode::MissingInput);

        write_case(
            &manifest_path,
            &canonical.replace("max_input_bytes = 65536", "max_input_bytes = 1"),
        );
        let budget = build_synthetic_failure_bundle(&manifest_path, &root.join("budget"))
            .err()
            .unwrap();
        assert_eq!(budget.code, BundleErrorCode::InputBudgetExceeded);

        write_case(
            &manifest_path,
            &canonical.replace("max_input_bytes = 65536", "max_input_bytes = 16777217"),
        );
        let tool_ceiling =
            build_synthetic_failure_bundle(&manifest_path, &root.join("tool-ceiling"))
                .err()
                .unwrap();
        assert_eq!(tool_ceiling.code, BundleErrorCode::InputBudgetExceeded);

        write_case(&manifest_path, &canonical);
        let first = build_synthetic_failure_bundle(&manifest_path, &root.join("render")).unwrap();
        write_case(&manifest_path, &canonical.replace("width = 4", "width = 5"));
        let second = build_synthetic_failure_bundle(&manifest_path, &root.join("render")).unwrap();
        assert_ne!(first, second);
        assert!(second.join("case-manifest.toml").is_file());

        write_case(
            &manifest_path,
            &canonical
                .replace("width = 4", "width = 1048577")
                .replace("max_image_pixels = 4096", "max_image_pixels = 1048577"),
        );
        let pixel_ceiling =
            build_synthetic_failure_bundle(&manifest_path, &root.join("pixel-ceiling"))
                .err()
                .unwrap();
        assert_eq!(pixel_ceiling.code, BundleErrorCode::RenderBudgetExceeded);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_contract_and_work_budget_mismatches() {
        let root = temp_dir("contract");
        let manifest_path = root.join("case.toml");
        let canonical = manifest_for_generated_pdf();

        write_case(
            &manifest_path,
            &canonical.replace("scene = true", "scene = false"),
        );
        let contract = build_synthetic_failure_bundle(&manifest_path, &root.join("contract"))
            .err()
            .unwrap();
        assert_eq!(contract.code, BundleErrorCode::CaseContractMismatch);

        write_case(
            &manifest_path,
            &canonical.replace("max_objects = 64", "max_objects = 1"),
        );
        let budget = build_synthetic_failure_bundle(&manifest_path, &root.join("budget"))
            .err()
            .unwrap();
        assert_eq!(budget.code, BundleErrorCode::SyntheticBudgetExceeded);

        fs::remove_dir_all(root).unwrap();
    }

    fn manifest_for_generated_pdf() -> String {
        let pdf = generate_one_page_pdf().unwrap();
        let hash = hex_digest(&sha256(&pdf).unwrap());
        format!(
            r#"schema = 1
[identity]
id = "infrastructure/synthetic-failure-bundle-001"
title = "Synthetic failure bundle"
owner = "quality-corpus"
status = "active"
introduced_in = "0.1.0"
[specification]
document = "RPE-ARCH-001"
version = "0.3"
clauses = ["15.3/M0"]
interpretation = "Exercise every synthetic artifact channel."
[provenance]
kind = "self-authored-generated"
source = "tools/generate"
sha256 = "sha256:{hash}"
license = "LicenseRef-PDF.rs-SelfAuthored-Test"
redistributable = false
access = "repository"
[features]
ids = ["quality.failure-bundle", "quality.minimal-pdf-generator", "quality.canonical-diff"]
requirements = ["m0.synthetic-artifacts.v1"]
[validity]
class = "valid"
strict_expected = "success"
recovery_expected = "not-applicable"
[expected]
parse = true
scene = true
text = true
pixel = true
diagnostic = true
capability = true
error = false
[oracle]
level = "O1"
derivation = "expected/oracle.md"
reviewers = ["spec-conformance"]
reference_may_generate = false
last_reviewed = "2026-07-13"
[budget]
max_input_bytes = 65536
max_objects = 64
max_resolve_depth = 16
max_stream_output_bytes = 1048576
max_total_decode_bytes = 1048576
max_image_pixels = 4096
max_path_segments = 4096
max_scene_commands = 4096
max_group_depth = 8
operator_fuel = 20000
decode_fuel = 1048576
watchdog_ms = 500
[render]
width = 4
height = 4
dpr_milli = 1000
color_profile = "srgb-reference-v1"
alpha = "straight"
antialias = "reference-v1"
renderer_epoch = "synthetic-v1"
[tolerance]
mode = "exact"
[runners]
native = ["synthetic-m0"]
external_observation = []
[history]
entries = ["2026-07-13: introduced"]
"#
        )
    }

    fn temp_dir(label: &str) -> PathBuf {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pdf-rs-bundle-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_case(manifest_path: &Path, manifest: &str) {
        fs::write(manifest_path, manifest).unwrap();
        fs::write(
            manifest_path.parent().unwrap().join("input.pdf"),
            generate_one_page_pdf().unwrap(),
        )
        .unwrap();
    }
}
