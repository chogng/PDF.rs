use std::fs;
use std::path::{Path, PathBuf};

fn top_level_version(document: &str) -> Option<&str> {
    document
        .lines()
        .take_while(|line| !line.starts_with("[["))
        .find_map(|line| line.strip_prefix("version = \"")?.strip_suffix('"'))
}

fn record_with_id<'a>(document: &'a str, kind: &str, id: &str) -> Option<&'a str> {
    let header = format!("{kind}]]");
    let id_line = format!("id = \"{id}\"");
    document
        .split("\n[[")
        .find(|record| record.starts_with(&header) && record.lines().any(|line| line == id_line))
}

#[test]
fn product_scene_has_only_lower_identity_dependencies_and_no_platform_io() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_root.join("Cargo.toml"))
        .expect("crate manifest must be readable during repository tests");
    let dependency_body = manifest
        .split_once("[dependencies]")
        .expect("crate manifest declares an explicit dependency table")
        .1
        .split("\n[")
        .next()
        .expect("dependency table body is present")
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        dependency_body,
        [
            r#"pdf-rs-bytes = { path = "../bytes" }"#,
            r#"pdf-rs-syntax = { path = "../syntax" }"#,
        ]
    );
    for forbidden_table in ["[dev-dependencies]", "[build-dependencies]", "[target."] {
        assert!(
            !manifest.contains(forbidden_table),
            "core/scene must not declare {forbidden_table} dependencies"
        );
    }

    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources);
    let joined = sources
        .iter()
        .map(|path| fs::read_to_string(path).expect("source must be readable"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    for forbidden in [
        "std::fs",
        "std::net",
        "async fn",
        "tokio",
        "async_std",
        "reqwest",
        "hyper",
        "pdfium",
        "mupdf",
        "pdf.js",
        "serde",
    ] {
        assert!(
            !joined.contains(forbidden),
            "forbidden product Scene token {forbidden:?}"
        );
    }
    assert!(joined.contains("#![forbid(unsafe_code)]"));
    assert!(joined.contains("#![deny(missing_docs)]"));
}

#[test]
fn canonical_scene_omits_runtime_source_identity_and_float_formatting() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let canonical = fs::read_to_string(crate_root.join("src/canonical.rs"))
        .expect("canonical Scene source is readable");
    let builder = fs::read_to_string(crate_root.join("src/builder.rs"))
        .expect("bounded Scene builder source is readable");
    let scalar =
        fs::read_to_string(crate_root.join("src/scalar.rs")).expect("scalar source is readable");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance is readable");
    let diff = fs::read_to_string(crate_root.join("src/diff.rs"))
        .expect("semantic Scene diff source is readable");

    assert!(!canonical.contains("binding().source()"));
    assert!(!canonical.contains("stable_id"));
    assert!(!canonical.contains("validator"));
    assert!(!canonical.contains("f32"));
    assert!(!canonical.contains("f64"));
    assert!(canonical.contains("push_i64(value.scaled())"));
    assert!(canonical.contains("push_hex(tag.bytes())"));
    assert!(canonical.contains("reserve_output(encoded_len)?"));
    assert!(!canonical.contains("self.push(&encoded)?"));
    assert!(builder.contains("preflight_append("));
    assert!(builder.contains("capacity_after_one("));
    assert!(!builder.contains("try_reserve_exact(1)"));
    assert!(scalar.contains("const SCALE: i128 = 1_000_000_000"));
    assert!(provenance.contains("runtime `SourceIdentity` is"));
    assert!(provenance.contains("deliberately omitted"));
    assert!(provenance.contains("nine-decimal fixed-point"));
    assert!(diff.contains("pub fn validate(config: SceneDiffLimitConfig)"));
    assert!(diff.contains("SceneLimitKind::Differences"));
    assert!(diff.contains("SceneLimitKind::DiffRetainedBytes"));
    assert!(diff.contains("SceneLimitKind::DiffCompareWork"));
    assert!(diff.contains("SceneLimitKind::DiffCanonicalBytes"));
    assert!(!diff.contains("expected_binding.source()"));
    assert!(!diff.contains("actual_binding.source()"));
}

#[test]
fn m2_scene_profiles_remain_planned_after_the_bounded_normative_gate_closes() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/scene has a repository root two levels above it");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance is readable");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature map is readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec map is readable");
    let plan =
        fs::read_to_string(repository_root.join("plan/m2.toml")).expect("M2 plan is readable");

    assert_eq!(top_level_version(&feature_map), Some("0.75.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.75.0"));
    assert_eq!(
        top_level_version(&feature_map),
        top_level_version(&spec_map),
        "feature and specification maps advance as one traceability version"
    );

    for required in [
        "Traceability profile boundaries",
        "`m2.scene-v1.v1`",
        "`m2.scene-semantic-diff.v1`",
        "Both profiles remain `PLANNED`",
        "M2 exit gate by themselves",
        "M2-06 Content VM producer",
        "M2-07 normative Scene gate",
        "milestone completion is not a maturity promotion",
    ] {
        assert!(
            provenance.contains(required),
            "Scene provenance must state {required:?}"
        );
    }

    let scene = record_with_id(&feature_map, "feature", "core.scene-v1")
        .expect("Scene v1 feature must be registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.scene-v1.v1\"",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"core/scene\"]",
        "core/scene::scene_v1",
        "core/scene::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            scene.contains(required),
            "Scene feature must contain {required:?}"
        );
    }

    let diff = record_with_id(&feature_map, "feature", "core.scene-semantic-diff")
        .expect("Scene semantic-diff feature must be registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.scene-semantic-diff.v1\"",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"core/scene\"]",
        "core/scene::scene_diff",
        "core/scene::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            diff.contains(required),
            "Scene diff feature must contain {required:?}"
        );
    }

    let producer = record_with_id(&feature_map, "feature", "core.content-vm-scene-v1")
        .expect("Content VM Scene producer must be registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.content-vm-scene-v1\"",
        "ISO-32000-1:2008/14.6",
        "ISO-32000-1:2008/14.6.1",
        "ISO-32000-1:2008/14.6.2",
        "RPE-ARCH-001/6.1-6.2",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"core/content\"]",
        "core/content::vm",
        "core/content::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            producer.contains(required),
            "Content VM producer feature must contain {required:?}"
        );
    }

    let gate = record_with_id(&feature_map, "feature", "quality.m2-scene-gate")
        .expect("M2 Scene gate feature must be registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.scene-gate.v1\"",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"tools/quality\", \"docs/traceability\"]",
        "tools/quality::m2_scene_gate",
        "tools/quality::m2_exit",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            gate.contains(required),
            "M2 Scene gate feature must contain {required:?}"
        );
    }

    let scene_requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/6.4-6.7")
        .expect("Scene architecture requirement must be registered");
    assert!(scene_requirement.contains("core.content-vm-scene-v1"));
    assert!(scene_requirement.contains("core.scene-v1"));
    assert!(scene_requirement.contains("core.scene-semantic-diff"));
    assert!(scene_requirement.contains("fixed-size content-redacted"));
    assert!(scene_requirement.contains("M2-06 supplies one bounded producer"));
    assert!(scene_requirement.contains("strict-attested acquired Page content"));
    assert!(
        scene_requirement
            .contains("Unsupported and failed interpretations never own a partial Scene")
    );
    assert!(scene_requirement.contains("M2-07 now closes the bounded M2 exit gate"));
    assert!(scene_requirement.contains("All component and quality feature records remain PLANNED"));

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M2")
        .expect("M2 requirement must be registered");
    assert!(milestone.contains("M2-04 is complete as a foundation"));
    assert!(milestone.contains("M2-05 is complete as two bounded PLANNED profiles"));
    assert!(milestone.contains("M2-06 is complete as two additional bounded PLANNED profiles"));
    assert!(milestone.contains("core.content-vm-scene-v1"));
    assert!(milestone.contains("quality.m2-scene-gate"));
    assert!(milestone.contains("M2-07 completes the bounded exit"));
    assert!(milestone.contains("All nine M2 feature records remain PLANNED"));
    assert!(milestone.contains("status = \"covered\""));

    let m2_04 = record_with_id(&plan, "work_item", "M2-04").expect("M2-04 work item must exist");
    assert!(m2_04.contains("status = \"complete\""));
    assert!(m2_04.contains("completed_at = 2026-07-16"));
    let m2_06 = record_with_id(&plan, "work_item", "M2-06").expect("M2-06 work item must exist");
    assert!(m2_06.contains("status = \"complete\""));
    assert!(m2_06.contains("completed_at = 2026-07-16"));
    let m2_07 = record_with_id(&plan, "work_item", "M2-07").expect("M2-07 work item must exist");
    assert!(m2_07.contains("status = \"complete\""));
    assert!(m2_07.contains("completed_at = 2026-07-16"));
    let milestone_header = plan
        .split("[[work_item]]")
        .next()
        .expect("M2 plan has a top-level milestone header");
    assert!(milestone_header.contains("status = \"complete\""));
    assert!(milestone_header.contains("completed_at = 2026-07-16"));
}

fn collect_rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory).expect("source directory must be readable");
    for entry in entries {
        let path = entry.expect("source entry must be readable").path();
        if path.is_dir() {
            collect_rust_sources(&path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            output.push(path);
        }
    }
}
