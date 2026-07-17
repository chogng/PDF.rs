use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_quality::case_contract::validate_case_file;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let ordinal = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "pdf-rs-m3-oracle-{label}-{}-{ordinal}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("expected")).expect("fixture expected directory is writable");
        fs::create_dir_all(root.join("evidence")).expect("fixture evidence directory is writable");
        let root = root
            .canonicalize()
            .expect("fixture root has a symlink-free canonical path");
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn write(&self, relative: &str, bytes: impl AsRef<[u8]>) {
        fs::write(self.path(relative), bytes).expect("fixture file is writable");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone, Copy)]
enum PixelOracle {
    O1,
    O3,
}

#[test]
fn o1_pixel_authority_is_content_addressed_and_read_only() {
    let fixture = Fixture::new("o1");
    let manifest = write_case(&fixture, PixelOracle::O1, 1, 1, 1, false);
    let before_manifest = digest_file(&manifest);
    let before_pixel = digest_file(&fixture.path("expected/pixel.json"));
    let before_derivation = digest_file(&fixture.path("expected/oracle.md"));

    let validated = validate_case_file(&manifest).expect("independent O1 fixture is valid");
    assert_eq!(validated.case_id(), "raster/m3-oracle/analytic-1x1");
    assert_eq!(digest_file(&manifest), before_manifest);
    assert_eq!(
        digest_file(&fixture.path("expected/pixel.json")),
        before_pixel
    );
    assert_eq!(
        digest_file(&fixture.path("expected/oracle.md")),
        before_derivation
    );
}

#[test]
fn o0_and_o1_cannot_delegate_expected_pixels_to_reference() {
    let fixture = Fixture::new("o1-reference");
    let manifest = write_case(&fixture, PixelOracle::O1, 1, 1, 1, true);
    let diagnostics =
        validate_case_file(&manifest).expect_err("O1 Reference generation must be rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "RPE-MANIFEST-0026")
    );

    let text = fs::read_to_string(&manifest).unwrap();
    fixture.write(
        "case.toml",
        text.replace("level = \"O1\"", "level = \"O0\""),
    );
    let diagnostics =
        validate_case_file(&manifest).expect_err("O0 Reference generation must be rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "RPE-MANIFEST-0026")
    );
}

#[test]
fn o3_requires_frozen_identity_independent_roles_and_hash_bound_review() {
    let fixture = Fixture::new("o3");
    let manifest = write_case(&fixture, PixelOracle::O3, 1, 1, 1, true);
    validate_case_file(&manifest).expect("fully bound O3 evidence is valid");

    let original = fs::read_to_string(&manifest).unwrap();
    for (needle, replacement, code) in [
        (
            "reviewers = [\"spec-conformance\", \"parser-security\"]",
            "reviewers = [\"spec-conformance\"]",
            "RPE-MANIFEST-0028",
        ),
        (
            "reviewers = [\"spec-conformance\", \"parser-security\"]",
            "reviewers = [\"quality-corpus\", \"parser-security\"]",
            "RPE-MANIFEST-0028",
        ),
        (
            "reviewers = [\"spec-conformance\", \"parser-security\"]",
            "reviewers = [\"pending-review\", \"parser-security\"]",
            "RPE-MANIFEST-0022",
        ),
        (
            "reviewers = [\"spec-conformance\", \"parser-security\"]",
            "reviewers = [\"review-required\", \"parser-security\"]",
            "RPE-MANIFEST-0022",
        ),
        (
            "reference_identity = \"evidence/reference-identity.json#sha256:",
            "reference_identity = \"reference-raster-v2@sha256:",
            "RPE-MANIFEST-0031",
        ),
        (
            "reference_may_generate = true",
            "reference_may_generate = false",
            "RPE-MANIFEST-0027",
        ),
    ] {
        fixture.write("case.toml", original.replace(needle, replacement));
        let diagnostics = validate_case_file(&manifest).expect_err("mutated O3 contract fails");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code() == code),
            "missing {code}: {diagnostics:?}"
        );
    }

    fixture.write("case.toml", &original);
    fixture.write(
        "evidence/review.json",
        b"{\"case_id\":\"wrong\",\"schema\":1,\"verdict\":\"pass\"}",
    );
    let diagnostics =
        validate_case_file(&manifest).expect_err("review evidence hash mismatch fails");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "RPE-CASE-0010")
    );
    let changed_hash = digest_file(&fixture.path("evidence/review.json"));
    fixture.write(
        "case.toml",
        original.replace(
            manifest_value(&original, "review_evidence_sha256"),
            &format!("sha256:{changed_hash}"),
        ),
    );
    let diagnostics =
        validate_case_file(&manifest).expect_err("hash-bound but semantically wrong review fails");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "RPE-CASE-0011")
    );
}

#[test]
fn reference_identity_is_content_addressed_and_canonical() {
    let fixture = Fixture::new("identity");
    let manifest = write_case(&fixture, PixelOracle::O3, 1, 1, 1, true);
    let original_manifest = fs::read_to_string(&manifest).unwrap();
    let original_review = fs::read_to_string(fixture.path("evidence/review.json")).unwrap();
    let original_identity = manifest_value(&original_manifest, "reference_identity");

    fixture.write(
        "evidence/reference-identity.json",
        b"{\"algorithm\":\"reference-raster-v2\",\"implementation_sha256\":\"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"schema\":1}",
    );
    let diagnostics =
        validate_case_file(&manifest).expect_err("changed identity bytes fail their content hash");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "RPE-CASE-0012")
    );

    let changed_identity_hash = digest_file(&fixture.path("evidence/reference-identity.json"));
    let changed_identity =
        format!("evidence/reference-identity.json#sha256:{changed_identity_hash}");
    let changed_review = original_review.replace(original_identity, &changed_identity);
    fixture.write("evidence/review.json", changed_review);
    let changed_review_hash = digest_file(&fixture.path("evidence/review.json"));
    fixture.write(
        "case.toml",
        original_manifest
            .replace(original_identity, &changed_identity)
            .replace(
                manifest_value(&original_manifest, "review_evidence_sha256"),
                &format!("sha256:{changed_review_hash}"),
            ),
    );
    let diagnostics = validate_case_file(&manifest)
        .expect_err("hash-bound alternate identity encoding remains invalid");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "RPE-CASE-0013")
    );
}

#[test]
fn derivation_hash_and_linked_file_size_are_enforced() {
    let fixture = Fixture::new("derivation");
    let manifest = write_case(&fixture, PixelOracle::O1, 1, 1, 1, false);
    fixture.write("expected/oracle.md", b"changed independent derivation\n");
    assert_code(&manifest, "RPE-CASE-0009");

    let original_manifest = fs::read_to_string(&manifest).unwrap();
    fixture.write("expected/pixel.json", vec![b'x'; 257]);
    let oversized_hash = digest_file(&fixture.path("expected/pixel.json"));
    fixture.write(
        "case.toml",
        original_manifest.replace(
            manifest_value(&original_manifest, "pixel_sha256"),
            &format!("sha256:{oversized_hash}"),
        ),
    );
    assert_code(&manifest, "RPE-CASE-0004");
}

#[test]
fn linked_pixel_contract_rejects_hash_encoding_dimensions_budget_and_symlinks() {
    let fixture = Fixture::new("negative");
    let manifest = write_case(&fixture, PixelOracle::O1, 1, 1, 1, false);
    let original_manifest = fs::read_to_string(&manifest).unwrap();

    fixture.write(
        "expected/pixel.json",
        b"{\"height\":1,\"rgba_hex\":\"ffffffff\",\"schema\":1,\"width\":1}\n",
    );
    replace_pixel_hash(&fixture, &original_manifest);
    assert_code(&manifest, "RPE-CASE-0006");

    fixture.write(
        "expected/pixel.json",
        b"{\"height\":1,\"rgba_hex\":\"ffffffffffffffff\",\"schema\":1,\"width\":2}",
    );
    let dimension_hash = digest_file(&fixture.path("expected/pixel.json"));
    fixture.write(
        "case.toml",
        original_manifest
            .replace("max_image_pixels = 1", "max_image_pixels = 2")
            .replace(
                manifest_value(&original_manifest, "pixel_sha256"),
                &format!("sha256:{dimension_hash}"),
            ),
    );
    assert_code(&manifest, "RPE-CASE-0007");

    fixture.write(
        "expected/pixel.json",
        b"{\"height\":1,\"rgba_hex\":\"ffffffffffffffff\",\"schema\":1,\"width\":2}",
    );
    let two_pixel_hash = digest_file(&fixture.path("expected/pixel.json"));
    fixture.write(
        "case.toml",
        original_manifest.replace("width = 1", "width = 2").replace(
            manifest_value(&original_manifest, "pixel_sha256"),
            &format!("sha256:{two_pixel_hash}"),
        ),
    );
    assert_code(&manifest, "RPE-CASE-0008");

    fixture.write("case.toml", &original_manifest);
    fixture.write(
        "expected/pixel.json",
        b"{\"height\":1,\"rgba_hex\":\"000000ff\",\"schema\":1,\"width\":1}",
    );
    assert_code(&manifest, "RPE-CASE-0005");
    fixture.write(
        "expected/pixel.json",
        b"{\"height\":1,\"rgba_hex\":\"ffffffff\",\"schema\":1,\"width\":1}",
    );

    fixture.write(
        "case.toml",
        original_manifest.replace(
            "max_stream_output_bytes = 4096",
            "max_stream_output_bytes = 4096\nmax_raster_output_bytes = 3",
        ),
    );
    assert_code(&manifest, "RPE-CASE-0008");

    fixture.write(
        "case.toml",
        original_manifest.replace(
            "max_stream_output_bytes = 4096",
            "max_stream_output_bytes = 4096\nmax_raster_output_bytes = 4",
        ),
    );
    validate_case_file(&manifest).expect("exact Raster output-byte budget is valid");
    fixture.write("case.toml", &original_manifest);

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let target = fixture.path("outside-pixel.json");
        fs::write(
            &target,
            b"{\"height\":1,\"rgba_hex\":\"ffffffff\",\"schema\":1,\"width\":1}",
        )
        .unwrap();
        fs::remove_file(fixture.path("expected/pixel.json")).unwrap();
        symlink(&target, fixture.path("expected/pixel.json")).unwrap();
        assert_code(&manifest, "RPE-CASE-0003");

        let linked_case = fixture.root.with_extension("linked-case");
        symlink(&fixture.root, &linked_case).unwrap();
        assert_code(&linked_case.join("case.toml"), "RPE-CASE-0001");
        fs::remove_file(linked_case).unwrap();
    }
}

#[test]
fn pixel_artifact_and_pixel_oracle_are_one_optional_extension() {
    let fixture = Fixture::new("paired-extension");
    let manifest = write_case(&fixture, PixelOracle::O1, 1, 1, 1, false);
    let original = fs::read_to_string(&manifest).unwrap();
    let oracle_start = original.find("\n[pixel_oracle]\n").unwrap();
    let budget_start = original.find("\n[budget]\n").unwrap();
    let without_oracle = format!("{}{}", &original[..oracle_start], &original[budget_start..]);
    fixture.write("case.toml", without_oracle);
    let diagnostics =
        validate_case_file(&manifest).expect_err("pixel artifact requires its pixel oracle");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "RPE-MANIFEST-0010")
    );

    let expected_start = original.find("pixel_artifact = ").unwrap();
    let expected_end = original[expected_start..]
        .find("\n\n[oracle]")
        .map(|offset| expected_start + offset)
        .unwrap();
    let without_artifact = format!(
        "{}{}",
        &original[..expected_start],
        &original[expected_end..]
    );
    fixture.write("case.toml", without_artifact);
    let diagnostics =
        validate_case_file(&manifest).expect_err("pixel oracle requires its pixel artifact");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "RPE-MANIFEST-0010")
    );
}

#[cfg(unix)]
#[test]
fn case_tree_validation_rejects_symlink_entries_instead_of_skipping_them() {
    use std::os::unix::fs::symlink;
    use std::process::Command;

    let fixture = Fixture::new("tree-symlink");
    let linked = fixture.path("linked-case");
    symlink("expected", &linked).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pdf-rs-quality"))
        .arg("validate-cases")
        .arg(&fixture.root)
        .output()
        .expect("quality CLI executes");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("RPE-CASE-0014"),
        "{output:?}"
    );
}

#[test]
fn existing_m0_and_m2_manifests_remain_backward_compatible() {
    let root = repository_root();
    for path in [
        "tests/cases/infrastructure/synthetic-failure-bundle-001/case.toml",
        "tests/cases/content/m2-scene/valid-state-and-marked-content/case.toml",
    ] {
        validate_case_file(&root.join(path))
            .unwrap_or_else(|diagnostics| panic!("{path} regressed: {diagnostics:?}"));
    }
}

#[test]
fn validator_source_has_no_expected_artifact_write_path() {
    let source = include_str!("../src/case_contract.rs");
    for forbidden in [
        "fs::write",
        "OpenOptions",
        "File::create",
        "rename(",
        "remove_file",
    ] {
        assert!(
            !source.contains(forbidden),
            "case validator must remain read-only: {forbidden}"
        );
    }
}

#[test]
fn ci_runs_the_read_only_contract_without_a_golden_update_mode() {
    let ci = include_str!("../../../scripts/ci.sh");
    let validate_cases = ci
        .find("validate-cases tests/cases")
        .expect("case tree validation remains in CI");
    let oracle_contract = ci
        .find("cargo test --locked --package pdf-rs-quality --test m3_raster_oracle_contract")
        .expect("M3 raster oracle contract is explicit in CI");
    let m2_gate = ci
        .find("m2_scene_gate_root=")
        .expect("frozen M2 gate remains in CI");
    assert!(validate_cases < oracle_contract);
    assert!(oracle_contract < m2_gate);
    for forbidden in [
        "update-golden",
        "update_golden",
        "overwrite-expected",
        "accept-pixels",
    ] {
        assert!(
            !ci.contains(forbidden),
            "CI exposes forbidden mode: {forbidden}"
        );
    }
}

fn write_case(
    fixture: &Fixture,
    oracle: PixelOracle,
    width: u64,
    height: u64,
    max_pixels: u64,
    reference_may_generate: bool,
) -> PathBuf {
    let pixel = format!(
        "{{\"height\":{height},\"rgba_hex\":\"{}\",\"schema\":1,\"width\":{width}}}",
        "ff".repeat(usize::try_from(width * height * 4).unwrap())
    );
    fixture.write("expected/pixel.json", pixel.as_bytes());
    fixture.write(
        "expected/oracle.md",
        b"Every sample is analytically opaque white; no renderer output was used.\n",
    );
    let pixel_hash = digest_file(&fixture.path("expected/pixel.json"));
    let derivation_hash = digest_file(&fixture.path("expected/oracle.md"));
    let level = match oracle {
        PixelOracle::O1 => "O1",
        PixelOracle::O3 => "O3",
    };
    fixture.write(
        "evidence/reference-identity.json",
        format!(
            "{{\"algorithm\":\"reference-raster-v1\",\"implementation_sha256\":\"sha256:{}\",\"schema\":1}}",
            "a".repeat(64)
        ),
    );
    let identity_hash = digest_file(&fixture.path("evidence/reference-identity.json"));
    let reference_identity = format!("evidence/reference-identity.json#sha256:{identity_hash}");
    let derivation = format!("expected/oracle.md#sha256:{derivation_hash}");
    let pixel_reference = format!("expected/pixel.json#sha256:{pixel_hash}");
    let review = format!(
        "{{\"case_id\":\"raster/m3-oracle/analytic-1x1\",\"derivation\":\"{derivation}\",\"independent\":true,\"pixel_reference\":\"{pixel_reference}\",\"reference_identity\":\"{reference_identity}\",\"reviewers\":[\"spec-conformance\",\"parser-security\"],\"schema\":1,\"verdict\":\"pass\"}}"
    );
    if matches!(oracle, PixelOracle::O3) {
        fixture.write("evidence/review.json", review.as_bytes());
    }
    let review_hash = if matches!(oracle, PixelOracle::O3) {
        Some(digest_file(&fixture.path("evidence/review.json")))
    } else {
        None
    };
    let o3_fields = review_hash.map_or_else(String::new, |review_hash| {
        format!(
            "reference_identity = \"{reference_identity}\"\nreview_evidence = \"evidence/review.json\"\nreview_evidence_sha256 = \"sha256:{review_hash}\"\n"
        )
    });
    let reviewers = if matches!(oracle, PixelOracle::O3) {
        "[\"spec-conformance\", \"parser-security\"]"
    } else {
        "[\"spec-conformance\"]"
    };
    let manifest = format!(
        r#"schema = 1

[identity]
id = "raster/m3-oracle/analytic-1x1"
title = "Analytic opaque white pixel"
owner = "quality-corpus"
status = "active"
introduced_in = "0.1.0"

[specification]
document = "RPE-ARCH-001"
version = "1"
clauses = ["RPE-ARCH-001/15.3/M3"]
interpretation = "Analytic pixel authority contract."

[provenance]
kind = "self-authored-generated"
source = "tools/quality/tests/m3_raster_oracle_contract.rs"
sha256 = "sha256:{source_hash}"
license = "LicenseRef-PDF.rs-SelfAuthored-Test"
redistributable = false
access = "repository"

[features]
ids = ["quality.m3-raster-oracle-contract"]
requirements = ["RPE-ARCH-001/15.3/M3"]

[validity]
class = "valid"
strict_expected = "success"
recovery_expected = "not-applicable"

[expected]
parse = true
scene = true
text = false
pixel = true
diagnostic = false
capability = false
error = false
pixel_artifact = "expected/pixel.json"
pixel_sha256 = "sha256:{pixel_hash}"

[oracle]
level = "O1"
derivation = "Finite self-authored manifest fixture."
reviewers = ["quality-corpus"]
reference_may_generate = false
last_reviewed = "2026-07-16"

[pixel_oracle]
level = "{level}"
derivation = "{derivation}"
reviewers = {reviewers}
reference_may_generate = {reference_may_generate}
last_reviewed = "2026-07-16"
{o3_fields}
[budget]
max_input_bytes = 4096
max_objects = 16
max_resolve_depth = 8
max_stream_output_bytes = 4096
max_total_decode_bytes = 4096
max_image_pixels = {max_pixels}
max_path_segments = 16
max_scene_commands = 16
max_group_depth = 4
operator_fuel = 100
decode_fuel = 100
watchdog_ms = 500

[render]
width = {width}
height = {height}
dpr_milli = 1000
color_profile = "srgb-reference-v1"
alpha = "straight"
antialias = "reference-v1"
renderer_epoch = "reference-raster-v1"

[tolerance]
mode = "exact"

[runners]
native = ["tools/quality::m3_reference_gate"]
external_observation = []

[history]
entries = ["2026-07-16: activated for contract validation"]
"#,
        source_hash = "1".repeat(64),
    );
    fixture.write("case.toml", manifest);
    fixture.path("case.toml")
}

fn replace_pixel_hash(fixture: &Fixture, original_manifest: &str) {
    let hash = digest_file(&fixture.path("expected/pixel.json"));
    fixture.write(
        "case.toml",
        original_manifest.replace(
            manifest_value(original_manifest, "pixel_sha256"),
            &format!("sha256:{hash}"),
        ),
    );
}

fn manifest_value<'a>(manifest: &'a str, key: &str) -> &'a str {
    manifest
        .lines()
        .find_map(|line| {
            line.strip_prefix(&format!("{key} = \""))
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or_else(|| panic!("missing {key}"))
}

fn assert_code(manifest: &Path, expected: &str) {
    let diagnostics = validate_case_file(manifest).expect_err("fixture must be rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == expected),
        "missing {expected}: {diagnostics:?}"
    );
}

fn digest_file(path: &Path) -> String {
    hex_digest(&sha256(&fs::read(path).expect("fixture file is readable")).unwrap())
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root is canonical")
}
