use std::fs;
use std::path::{Path, PathBuf};

use pdf_rs_digest::{hex_digest, sha256};

const TRACE_VERSION: &str = "0.69.0";
const COMPLETED_AT: &str = "2026-07-16";
const M1_PAGE_TREE_SHA256: &str =
    "e680abd131a3a4da61262eb152820c3e4f6252c6396a15447039713da3a0f5e1";
const M1_PAGE_TREE_TEST_SHA256: &str =
    "aa8f4bbb5c4475d62a29a0cce3e8f798b17ea606e185b8b97017c2bc25e14374";

#[derive(Clone, Copy)]
struct SceneCase {
    slug: &'static str,
    id: &'static str,
    input_sha256: &'static str,
    strict_expected: &'static str,
    scene_sha256: Option<&'static str>,
    diagnostic: bool,
    capability: bool,
    error: bool,
}

const CASES: [SceneCase; 6] = [
    SceneCase {
        slug: "valid-state-and-marked-content",
        id: "content/m2-scene/valid-state-and-marked-content",
        input_sha256: "0cf75dfe46eae052c87ead1d833eb2f5f326ce33022fc7fe55aeff3360c70280",
        strict_expected: "success",
        scene_sha256: Some("7a686fe3461e12fbc3d89f51a113cd10f3bd535a27be0624da21b65c402b3661"),
        diagnostic: false,
        capability: false,
        error: false,
    },
    SceneCase {
        slug: "invalid-unbalanced-graphics-state",
        id: "content/m2-scene/invalid-unbalanced-graphics-state",
        input_sha256: "ff6f0896c84db5e92595d1ea4544953d04d9eacb5953f9cf27f70a6bdcb9c303",
        strict_expected: "RPE-CONTENT-VM-0007",
        scene_sha256: None,
        diagnostic: true,
        capability: false,
        error: true,
    },
    SceneCase {
        slug: "unsupported-marked-point",
        id: "content/m2-scene/unsupported-marked-point",
        input_sha256: "f31b1e863d90ff8a47c0a1ae195e901bac3bc305ab45f0e2e244b129c2fddac8",
        strict_expected: "RPE-CONTENT-UNSUPPORTED-0002",
        scene_sha256: None,
        diagnostic: true,
        capability: true,
        error: false,
    },
    SceneCase {
        slug: "resource-marked-content-properties",
        id: "content/m2-scene/resource-marked-content-properties",
        input_sha256: "50081ce0272a5698781ee760db6034b255742e74156b143d35141bd601ccba00",
        strict_expected: "success",
        scene_sha256: Some("b49a37d9c617a38cdfbc9ee3c3a86ef6e3a691220923f2403ac6bfa363d17b7c"),
        diagnostic: false,
        capability: false,
        error: false,
    },
    SceneCase {
        slug: "cancel-before-publication",
        id: "content/m2-scene/cancel-before-publication",
        input_sha256: "d9cdbfcc62b9addbb5c82e32dd0405232f50c4ba97d19e4644882733741f364f",
        strict_expected: "RPE-CONTENT-VM-0011",
        scene_sha256: None,
        diagnostic: true,
        capability: false,
        error: true,
    },
    SceneCase {
        slug: "source-change-before-resume",
        id: "content/m2-scene/source-change-before-resume",
        input_sha256: "d9cdbfcc62b9addbb5c82e32dd0405232f50c4ba97d19e4644882733741f364f",
        strict_expected: "RPE-CONTENT-VM-0014",
        scene_sha256: None,
        diagnostic: true,
        capability: false,
        error: true,
    },
];

#[test]
fn m2_exit_is_repository_bound_complete_and_maturity_neutral() {
    let root = repository_root();
    let feature_map = read_text(&root, "docs/traceability/feature-map.toml");
    let spec_map = read_text(&root, "docs/traceability/spec-map.toml");
    let m2_plan = read_text(&root, "plan/m2.toml");
    let r0_plan = read_text(&root, "plan/r0.toml");

    assert_line(&feature_map, &format!("version = \"{TRACE_VERSION}\""));
    assert_line(&spec_map, &format!("version = \"{TRACE_VERSION}\""));

    let plan_header = m2_plan
        .split("[[work_item]]")
        .next()
        .expect("M2 plan has a top-level header");
    assert_line(plan_header, "milestone = \"M2\"");
    assert_line(plan_header, "status = \"complete\"");
    assert_line(plan_header, &format!("completed_at = {COMPLETED_AT}"));
    for ordinal in 1..=7 {
        let id = format!("M2-{ordinal:02}");
        let item = array_record(&m2_plan, "[[work_item]]", &id);
        assert_line(item, "status = \"complete\"");
        assert_line(item, &format!("completed_at = {COMPLETED_AT}"));
    }

    let milestone = array_record(&r0_plan, "[[milestone]]", "M2");
    for required in [
        "status = \"complete\"",
        "started_at = 2026-07-16",
        "completed_at = 2026-07-16",
        "reviewed_by_roles = [\"parser-security\", \"spec-conformance\"]",
        "execution_plan = \"plan/m2.toml\"",
        "docs/traceability/evidence/m2/scene-gate/independent-review.toml",
        "docs/traceability/evidence/m2/scene-gate/normative-replay.toml",
        "cargo test --locked --package pdf-rs-quality --test m2_exit",
        "feature maturity promotion beyond PLANNED",
    ] {
        assert!(
            milestone.contains(required),
            "M2 milestone is missing closure evidence: {required}"
        );
    }

    let gate_feature = array_record(&feature_map, "[[feature]]", "quality.m2-scene-gate");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.scene-gate.v1\"",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/15.3/M2",
        "tools/quality::m2_scene_gate",
        "tools/quality::m2_exit",
    ] {
        assert!(
            gate_feature.contains(required),
            "M2 gate feature is missing: {required}"
        );
    }

    let scene_requirement = array_record(&spec_map, "[[requirement]]", "RPE-ARCH-001/6.4-6.7");
    let milestone_requirement = array_record(&spec_map, "[[requirement]]", "RPE-ARCH-001/15.3/M2");
    for requirement in [scene_requirement, milestone_requirement] {
        for required in [
            "quality.m2-scene-gate",
            "tools/quality::m2_scene_gate",
            "tools/quality::m2_exit",
        ] {
            assert!(
                requirement.contains(required),
                "M2 trace linkage is missing: {required}"
            );
        }
    }
    assert_line(milestone_requirement, "status = \"covered\"");
    for required in [
        "six",
        "canonical Scene",
        "debug",
        "release",
        "PLANNED",
        "not REFERENCE",
    ] {
        assert!(
            milestone_requirement.contains(required),
            "M2 bounded coverage note is missing: {required}"
        );
    }
}

#[test]
fn m2_normative_cases_and_evidence_are_exact_and_content_addressed() {
    let root = repository_root();
    let case_root = root.join("tests/cases/content/m2-scene");

    let mut case_directories = fs::read_dir(&case_root)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", case_root.display()))
        .map(|entry| entry.expect("M2 case directory entry is readable"))
        .map(|entry| {
            let file_type = entry
                .file_type()
                .expect("M2 case directory entry type is readable");
            assert!(file_type.is_dir(), "unexpected M2 case-root non-directory");
            assert!(
                !file_type.is_symlink(),
                "M2 case directory cannot be a symlink"
            );
            entry
                .file_name()
                .into_string()
                .expect("M2 case directory name is UTF-8")
        })
        .collect::<Vec<_>>();
    case_directories.sort();
    let mut expected_directories = CASES
        .iter()
        .map(|case| case.slug.to_owned())
        .collect::<Vec<_>>();
    expected_directories.sort();
    assert_eq!(case_directories, expected_directories);

    let mut registered_files = Vec::new();
    collect_regular_files(&case_root, &case_root, &mut registered_files);
    registered_files.sort();
    let mut expected_files = Vec::new();
    for case in CASES {
        expected_files.push(format!("{}/case.toml", case.slug));
        expected_files.push(format!("{}/input.pdf", case.slug));
        if case.scene_sha256.is_some() {
            expected_files.push(format!("{}/expected/scene.json", case.slug));
        }
    }
    expected_files.sort();
    assert_eq!(
        registered_files, expected_files,
        "the M2 registry must contain exactly six manifests, six inputs, and two Scene goldens"
    );

    for case in CASES {
        let case_directory = format!("tests/cases/content/m2-scene/{}", case.slug);
        let manifest_path = format!("{case_directory}/case.toml");
        let input_path = format!("{case_directory}/input.pdf");
        let manifest = read_text(&root, &manifest_path);

        assert_line(&manifest, &format!("id = \"{}\"", case.id));
        assert_line(&manifest, "status = \"active\"");
        assert_line(&manifest, &format!("source = \"{input_path}\""));
        assert_line(
            &manifest,
            &format!("sha256 = \"sha256:{}\"", case.input_sha256),
        );
        assert_line(&manifest, "parse = true");
        assert_line(
            &manifest,
            &format!("strict_expected = \"{}\"", case.strict_expected),
        );
        assert_line(&manifest, &format!("diagnostic = {}", case.diagnostic));
        assert_line(&manifest, &format!("capability = {}", case.capability));
        assert_line(&manifest, &format!("error = {}", case.error));
        assert_line(
            &manifest,
            &format!("scene = {}", case.scene_sha256.is_some()),
        );
        assert_line(&manifest, "native = [\"tools/quality::m2_scene_gate\"]");
        assert_eq!(
            digest_file(&root.join(&input_path)),
            case.input_sha256,
            "input hash mismatch for {}",
            case.id
        );

        match case.scene_sha256 {
            Some(expected_hash) => {
                let scene_path = format!("{case_directory}/expected/scene.json");
                assert_line(&manifest, "scene_artifact = \"expected/scene.json\"");
                assert_line(
                    &manifest,
                    &format!("scene_sha256 = \"sha256:{expected_hash}\""),
                );
                assert_eq!(
                    digest_file(&root.join(scene_path)),
                    expected_hash,
                    "Scene hash mismatch for {}",
                    case.id
                );
            }
            None => {
                assert!(!manifest.contains("scene_artifact"));
                assert!(!manifest.contains("scene_sha256"));
            }
        }
    }

    let independent_path = "docs/traceability/evidence/m2/scene-gate/independent-review.toml";
    let replay_path = "docs/traceability/evidence/m2/scene-gate/normative-replay.toml";
    let independent = read_text(&root, independent_path);
    let replay = read_text(&root, replay_path);

    for (evidence, role, verdict) in [
        (&independent, "independent-review", "verdict = \"SHIP\""),
        (&replay, "normative-replay", "verdict = \"pass\""),
    ] {
        for required in [
            "milestone = \"M2\"",
            "work_item = \"M2-07\"",
            "profile = \"m2.scene-gate.v1\"",
            "feature = \"quality.m2-scene-gate\"",
            "registered = true",
            "gating = true",
            "external_observation = false",
            "maturity_promotion = false",
            "reviewer_roles = [\"parser-security\", \"spec-conformance\"]",
            "commands = [",
            "sha256:",
            verdict,
        ] {
            assert!(
                evidence.contains(required),
                "{role} evidence is missing: {required}"
            );
        }
        assert!(
            verify_content_references(&root, evidence) > 0,
            "{role} evidence must bind at least one repository file by SHA-256"
        );
    }
    assert_line(&independent, "open_p0_p2 = 0");
    for required in [
        "case_count = 6",
        "ready_case_count = 2",
        "non_ready_case_count = 4",
        "fresh_replays = [\"debug-1\", \"debug-2\", \"release-1\", \"release-2\"]",
        "comparisons = [\"debug-1=debug-2\", \"release-1=release-2\", \"debug-1=release-1\"]",
        "canonical_result_bytes = 1150",
        "canonical_result_sha256 = \"sha256:3692ef689f479ef6bb94ecd9976f78e46d5a5ecd50992cc53a2b08014a8eabe5\"",
    ] {
        assert_line(&replay, required);
    }
    for case in CASES {
        assert!(replay.contains(case.id));
        assert!(replay.contains(case.input_sha256));
        if let Some(scene_sha256) = case.scene_sha256 {
            assert!(replay.contains(scene_sha256));
        }
    }
}

#[test]
fn m2_ci_replays_fresh_profiles_before_exit_and_preserves_m1() {
    let root = repository_root();
    let ci = read_text(&root, "scripts/ci.sh");
    let quality_main = read_text(&root, "tools/quality/src/main.rs");
    let gate_source = read_text(&root, "tools/quality/tests/m2_scene_gate_support/mod.rs");

    let validate_cases = position(&ci, "validate-cases tests/cases");
    let debug_1 = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$m2_scene_gate_root/debug-1\"",
    );
    let debug_2 = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$m2_scene_gate_root/debug-2\"",
    );
    let release_1 = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$m2_scene_gate_root/release-1\"",
    );
    let release_2 = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$m2_scene_gate_root/release-2\"",
    );
    assert!(validate_cases < debug_1);
    assert!(debug_1 < debug_2);
    assert!(debug_2 < release_1);
    assert!(release_1 < release_2);

    let diff_positions = positions(&ci, "diff --recursive --brief");
    assert_eq!(diff_positions.len(), 3);
    assert!(release_2 < diff_positions[0]);
    let first_diff = &ci[diff_positions[0]..diff_positions[1]];
    let second_diff = &ci[diff_positions[1]..diff_positions[2]];
    let third_diff = &ci[diff_positions[2]..];
    assert!(first_diff.contains("\"$m2_scene_gate_root/debug-1\""));
    assert!(first_diff.contains("\"$m2_scene_gate_root/debug-2\""));
    assert!(second_diff.contains("\"$m2_scene_gate_root/release-1\""));
    assert!(second_diff.contains("\"$m2_scene_gate_root/release-2\""));
    assert!(third_diff.contains("\"$m2_scene_gate_root/debug-1\""));
    assert!(third_diff.contains("\"$m2_scene_gate_root/release-1\""));

    let m2_exit = position(&ci, "cargo test --locked -p pdf-rs-quality --test m2_exit");
    let m1_maturity = position(
        &ci,
        "validate-m1-maturity docs/traceability/capability-profiles.toml",
    );
    assert!(diff_positions[2] < m2_exit);
    assert!(m2_exit < m1_maturity);

    let fixed_root = position(
        &ci,
        "m2_scene_gate_root=\"target/ci-artifacts/m2-scene-gate\"",
    );
    let fixed_root_guard = position(
        &ci,
        "[[ \"$m2_scene_gate_root\" != \"target/ci-artifacts/m2-scene-gate\" ]]",
    );
    let symlink_guard = position(
        &ci,
        "[[ -L \"target\" || -L \"target/ci-artifacts\" || -L \"$m2_scene_gate_root\" ]]",
    );
    let destructive_clean = position(&ci, "rm -rf -- \"$m2_scene_gate_root\"");
    assert!(fixed_root < fixed_root_guard);
    assert!(fixed_root_guard < symlink_guard);
    assert!(symlink_guard < destructive_clean);
    assert!(destructive_clean < debug_1);

    let shared_checks = "fmt,clippy,test,parser-mutation-smoke,case-manifests,m2-scene-gate,m2-exit,m1-maturity,product-purity,product-release-closure,synthetic-failure-bundle";
    assert!(quality_main.contains(&format!("checks: \"{shared_checks}\"")));
    assert!(
        quality_main.contains(&format!("checks: \"{shared_checks},doc\"")),
        "PR selection must add documentation to the shared local M2 closure"
    );
    assert!(
        quality_main
            .contains("local/pr checks include m2-scene-gate profile replay and m2-exit closure")
    );
    for required in [
        "if outcome.kind != OutcomeKind::Ready",
        "assert!(outcome.scene.is_none());",
        "assert!(outcome.diff.is_none());",
    ] {
        assert!(
            gate_source.contains(required),
            "non-Ready Scene suppression is not statically pinned: {required}"
        );
    }

    assert_eq!(
        digest_file(&root.join("core/document/src/page_tree.rs")),
        M1_PAGE_TREE_SHA256
    );
    assert_eq!(
        digest_file(&root.join("core/document/tests/page_tree_count.rs")),
        M1_PAGE_TREE_TEST_SHA256
    );
}

#[test]
fn m2_exit_documentation_keeps_the_gate_bounded() {
    let root = repository_root();
    let provenance = read_text(&root, "tools/quality/PROVENANCE.md");
    let trace_readme = read_text(&root, "docs/traceability/README.md");

    for document in [&provenance, &trace_readme] {
        for required in [
            "milestone evidence",
            "not a maturity promotion",
            "six",
            "fresh",
            "debug",
            "release",
            "profile-stable",
            "paths",
            "painting",
            "text showing",
            "rendering",
        ] {
            assert!(
                document.contains(required),
                "M2 gate documentation is missing bounded statement: {required}"
            );
        }
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("quality crate is nested below the repository root")
        .to_path_buf()
}

fn read_text(root: &Path, relative: &str) -> String {
    let path = root.join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn digest_file(path: &Path) -> String {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let digest =
        sha256(&bytes).unwrap_or_else(|error| panic!("cannot hash {}: {error}", path.display()));
    hex_digest(&digest)
}

fn collect_regular_files(root: &Path, directory: &Path, output: &mut Vec<String>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
    {
        let entry = entry.expect("M2 registry entry is readable");
        let file_type = entry
            .file_type()
            .expect("M2 registry entry type is readable");
        assert!(
            !file_type.is_symlink(),
            "M2 registry cannot contain symbolic links"
        );
        if file_type.is_dir() {
            collect_regular_files(root, &entry.path(), output);
        } else {
            assert!(file_type.is_file(), "M2 registry entries must be regular");
            output.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .expect("M2 registry file stays below its root")
                    .to_str()
                    .expect("M2 registry path is UTF-8")
                    .replace('\\', "/"),
            );
        }
    }
}

fn verify_content_references(root: &Path, evidence: &str) -> usize {
    let mut verified = 0;
    for line in evidence.lines() {
        let value = line
            .trim()
            .trim_end_matches(',')
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'));
        let Some((path, expected_hash)) = value.and_then(|value| value.split_once("#sha256:"))
        else {
            continue;
        };
        assert!(
            !path.is_empty()
                && !path.starts_with('/')
                && !path.contains('\\')
                && !path.split('/').any(|component| component == ".."),
            "non-canonical evidence path: {path}"
        );
        assert_eq!(
            digest_file(&root.join(path)),
            expected_hash,
            "evidence hash mismatch for {path}"
        );
        verified += 1;
    }
    verified
}

fn array_record<'a>(document: &'a str, header: &str, id: &str) -> &'a str {
    let id_line = format!("id = \"{id}\"");
    document
        .split(header)
        .skip(1)
        .find(|record| record.lines().any(|line| line == id_line))
        .unwrap_or_else(|| panic!("missing {header} record {id}"))
}

fn assert_line(document: &str, expected: &str) {
    assert!(
        document.lines().any(|line| line == expected),
        "missing exact line: {expected}"
    );
}

fn position(document: &str, needle: &str) -> usize {
    document
        .find(needle)
        .unwrap_or_else(|| panic!("missing required repository text: {needle}"))
}

fn positions(document: &str, needle: &str) -> Vec<usize> {
    document
        .match_indices(needle)
        .map(|(index, _)| index)
        .collect()
}
