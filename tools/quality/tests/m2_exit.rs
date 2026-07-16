use std::fs;
use std::path::{Path, PathBuf};

use pdf_rs_digest::{hex_digest, sha256};

#[path = "support/evidence.rs"]
mod evidence;

use evidence::{
    RootToml, TestDirectory, git_output, git_revision, read_commit_blob, read_repository_file,
    split_subject_entry, validate_commit_id, validate_relative_path, validate_sha256,
    verify_reviewed_subjects, verify_subject_entries,
};

const TRACE_VERSION: &str = "0.73.0";
const COMPLETED_AT: &str = "2026-07-16";
const M1_PAGE_TREE_SHA256: &str =
    "e680abd131a3a4da61262eb152820c3e4f6252c6396a15447039713da3a0f5e1";
const M1_PAGE_TREE_TEST_SHA256: &str =
    "aa8f4bbb5c4475d62a29a0cce3e8f798b17ea606e185b8b97017c2bc25e14374";
const M2_CONTENT_VM_COMMIT: &str = "ee6720f615cc3042638a957b85b377b3e0ab054c";

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
    let independent_root =
        RootToml::parse(&independent).expect("independent evidence has strict root TOML");
    let replay_root = RootToml::parse(&replay).expect("replay evidence has strict root TOML");

    assert_m2_evidence_header(
        &independent_root,
        "milestone-evidence",
        "evidence.m2.scene-gate.independent-review",
        "independent-review",
    );
    independent_root
        .expect_bare("reviewed_at", COMPLETED_AT)
        .expect("independent review date is exact");
    independent_root
        .expect_array("reviewer_roles", &["parser-security", "spec-conformance"])
        .expect("independent reviewer roles are exact");
    independent_root
        .expect_string(
            "reviewed_subject_resolution",
            "git-tree-at-reviewed-subject-commit-for-versioned-product-subjects",
        )
        .expect("reviewed subject resolution is exact");
    independent_root
        .expect_array(
            "reviewed_subject_commit_map",
            &[concat!(
                "core/content/src/vm.rs@",
                "ee6720f615cc3042638a957b85b377b3e0ab054c"
            )],
        )
        .expect("independent commit map is exact");
    independent_root
        .expect_array(
            "reviewed_subjects",
            &[
                concat!(
                    "core/content/src/vm.rs@",
                    "ee6720f615cc3042638a957b85b377b3e0ab054c",
                    "#sha256:2b866dca0b778c579a23db58f16b627fb338dcf83c4a34880c063c04c43c9eef"
                ),
                "tools/quality/tests/m2_scene_gate_support/mod.rs#sha256:af3807fbbc93fa09ca135ca27e4eeafafaaefb595e8e0bef75cde600ffb5caec",
                "docs/traceability/evidence/m2/scene-gate/subjects/tools-quality-m2-exit.rs#sha256:2cdcb920fe52065d2f385f2b734ff874e91d1c1dff3eff8586b73a8d2960c4da",
                "docs/traceability/evidence/m2/scene-gate/subjects/scripts-ci-m2.sh#sha256:7dc3aecf35f0dd0800a3ee053e92f3601cd5a45fe7c53ff317133d81806e10d2",
                "plan/m2.toml#sha256:b0378ae2c3388626871863462362649ee291ee11142bee7686ec909f4cced55a",
                "docs/traceability/evidence/m2/scene-gate/subjects/feature-map-0.69.0.toml#sha256:5fab896e6efc372fef9910a831fe32f455b7c61539f249530821080dc0cd9168",
                "docs/traceability/evidence/m2/scene-gate/subjects/spec-map-0.69.0.toml#sha256:9c7fb744920b771242d61f56fbd2cf55dea311150e88880cf6e714be1444861c",
                "tests/cases/content/m2-scene/valid-state-and-marked-content/case.toml#sha256:113da47429ce11b8f0bc0b6234630b7852f6a35c3be2acc9c57f4e6b02375fb4",
                "tests/cases/content/m2-scene/valid-state-and-marked-content/expected/scene.json#sha256:7a686fe3461e12fbc3d89f51a113cd10f3bd535a27be0624da21b65c402b3661",
                "tests/cases/content/m2-scene/resource-marked-content-properties/case.toml#sha256:c9499da843fcbcab57ca671d203789fecdfd0ade036b7548c6c90738667c6c69",
                "tests/cases/content/m2-scene/resource-marked-content-properties/expected/scene.json#sha256:b49a37d9c617a38cdfbc9ee3c3a86ef6e3a691220923f2403ac6bfa363d17b7c",
            ],
        )
        .expect("independent reviewed subjects are exact");
    independent_root
        .expect_array(
            "commands",
            &[
                "cargo test --locked --package pdf-rs-content --test vm",
                "cargo test --locked --release --package pdf-rs-content --test vm",
                "cargo test --locked --package pdf-rs-quality --test m2_scene_gate",
                "cargo test --locked --release --package pdf-rs-quality --test m2_scene_gate",
                "cargo test --locked --package pdf-rs-quality --test m2_exit",
                "cargo test --locked --package pdf-rs-quality --test m0_exit",
                "bash -n scripts/ci.sh",
                "cargo fmt --all --check",
                "cargo clippy --workspace --all-targets --all-features -- -D warnings",
            ],
        )
        .expect("independent commands are exact");
    independent_root
        .expect_unsigned("open_p0_p2", 0)
        .expect("independent open finding count is exact");
    independent_root
        .expect_string("verdict", "SHIP")
        .expect("independent verdict is exact");
    assert!(
        independent_root
            .optional_string("reviewed_subject_tree")
            .expect("optional M2 reviewed tree is typed")
            .is_none(),
        "M2 evidence must not claim an unregistered reviewed tree"
    );
    assert_eq!(
        verify_reviewed_subjects(&root, &independent_root, M2_CONTENT_VM_COMMIT, None)
            .expect("independent reviewed subjects are bound"),
        11
    );

    assert_m2_evidence_header(
        &replay_root,
        "milestone-evidence",
        "evidence.m2.scene-gate.normative-replay",
        "normative-replay",
    );
    replay_root
        .expect_bare("reviewed_at", COMPLETED_AT)
        .expect("replay date is exact");
    replay_root
        .expect_array("reviewer_roles", &["parser-security", "spec-conformance"])
        .expect("replay reviewer roles are exact");
    replay_root
        .expect_string("runner", "tools/quality::m2_scene_gate")
        .expect("replay runner is exact");
    replay_root
        .expect_string("exit_test", "tools/quality::m2_exit")
        .expect("replay exit test is exact");
    replay_root
        .expect_array(
            "commands",
            &[
                "PDF_RS_M2_SCENE_GATE_OUTPUT=target/ci-artifacts/m2-scene-gate/debug-1 cargo test --locked --package pdf-rs-quality --test m2_scene_gate",
                "PDF_RS_M2_SCENE_GATE_OUTPUT=target/ci-artifacts/m2-scene-gate/debug-2 cargo test --locked --package pdf-rs-quality --test m2_scene_gate",
                "PDF_RS_M2_SCENE_GATE_OUTPUT=target/ci-artifacts/m2-scene-gate/release-1 cargo test --locked --release --package pdf-rs-quality --test m2_scene_gate",
                "PDF_RS_M2_SCENE_GATE_OUTPUT=target/ci-artifacts/m2-scene-gate/release-2 cargo test --locked --release --package pdf-rs-quality --test m2_scene_gate",
                "diff --recursive --brief target/ci-artifacts/m2-scene-gate/debug-1 target/ci-artifacts/m2-scene-gate/debug-2",
                "diff --recursive --brief target/ci-artifacts/m2-scene-gate/release-1 target/ci-artifacts/m2-scene-gate/release-2",
                "diff --recursive --brief target/ci-artifacts/m2-scene-gate/debug-1 target/ci-artifacts/m2-scene-gate/release-1",
                "cargo test --locked --package pdf-rs-quality --test m2_exit",
            ],
        )
        .expect("replay commands are exact");
    replay_root
        .expect_unsigned("case_count", 6)
        .expect("replay case count is exact");
    replay_root
        .expect_unsigned("ready_case_count", 2)
        .expect("replay ready count is exact");
    replay_root
        .expect_unsigned("non_ready_case_count", 4)
        .expect("replay non-ready count is exact");

    let expected_manifests = CASES
        .iter()
        .map(|case| format!("tests/cases/content/m2-scene/{}/case.toml", case.slug))
        .collect::<Vec<_>>();
    assert_eq!(
        replay_root
            .array("case_manifests")
            .expect("replay case manifests are a typed root array"),
        expected_manifests
    );
    let expected_input_hashes = CASES
        .iter()
        .map(|case| {
            format!(
                "tests/cases/content/m2-scene/{}/input.pdf#sha256:{}",
                case.slug, case.input_sha256
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        replay_root
            .array("input_hashes")
            .expect("replay input hashes are a typed root array"),
        expected_input_hashes
    );
    let expected_scene_goldens = CASES
        .iter()
        .filter_map(|case| {
            case.scene_sha256.map(|hash| {
                format!(
                    "tests/cases/content/m2-scene/{}/expected/scene.json#sha256:{hash}",
                    case.slug
                )
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        replay_root
            .array("scene_goldens")
            .expect("replay Scene goldens are a typed root array"),
        expected_scene_goldens
    );
    replay_root
        .expect_array(
            "non_ready_diagnostics",
            &[
                "RPE-CONTENT-VM-0007",
                "RPE-CONTENT-UNSUPPORTED-0002",
                "RPE-CONTENT-VM-0011",
                "RPE-CONTENT-VM-0014",
            ],
        )
        .expect("non-ready diagnostics are exact");
    replay_root
        .expect_array(
            "fresh_replays",
            &["debug-1", "debug-2", "release-1", "release-2"],
        )
        .expect("fresh replay names are exact");
    replay_root
        .expect_array(
            "comparisons",
            &[
                "debug-1=debug-2",
                "release-1=release-2",
                "debug-1=release-1",
            ],
        )
        .expect("replay comparisons are exact");
    replay_root
        .expect_unsigned("canonical_result_bytes", 1150)
        .expect("canonical result byte count is exact");
    replay_root
        .expect_string(
            "canonical_result_sha256",
            "sha256:3692ef689f479ef6bb94ecd9976f78e46d5a5ecd50992cc53a2b08014a8eabe5",
        )
        .expect("canonical result hash is exact");
    replay_root
        .expect_string(
            "output_root_policy",
            "absent-or-empty dedicated directory; never recursively delete caller-selected content",
        )
        .expect("output root policy is exact");
    replay_root
        .expect_string("verdict", "pass")
        .expect("replay verdict is exact");

    let mut replay_subjects = replay_root
        .array("input_hashes")
        .expect("replay input hashes are a root array")
        .to_vec();
    replay_subjects.extend_from_slice(
        replay_root
            .array("scene_goldens")
            .expect("replay Scene goldens are a root array"),
    );
    assert!(
        verify_subject_entries(&root, &replay_subjects)
            .expect("replay subjects are repository-bound")
            > 0
    );
    assert_eq!(replay_subjects.len(), 8);
}

#[test]
fn m2_ci_replays_fresh_profiles_before_exit_and_preserves_m1() {
    let root = repository_root();
    let workflow = read_text(&root, ".github/workflows/ci.yml");
    let ci = read_text(&root, "scripts/ci.sh");
    let quality_main = read_text(&root, "tools/quality/src/main.rs");
    let gate_source = read_text(&root, "tools/quality/tests/m2_scene_gate_support/mod.rs");

    const FULL_HISTORY_CHECKOUT: &str = "\
      - name: Check out repository
        uses: actions/checkout@v4
        with:
          fetch-depth: 0";
    assert_eq!(
        workflow.matches("uses: actions/checkout@v4").count(),
        1,
        "CI must have exactly one pinned checkout step"
    );
    assert_eq!(
        workflow.matches("fetch-depth:").count(),
        1,
        "CI must declare exactly one checkout history policy"
    );
    assert!(
        workflow.contains(FULL_HISTORY_CHECKOUT),
        "history-bound M2 evidence verification requires a full Git checkout"
    );

    let validate_cases = position(&ci, "validate-cases tests/cases");
    let raster_oracle = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_raster_oracle_contract",
    );
    let content_graphics_trace = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_content_graphics_trace",
    );
    let reference_geometry_trace = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_reference_geometry_trace",
    );
    let reference_color_trace = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_reference_color_trace",
    );
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
    assert!(validate_cases < raster_oracle);
    assert!(raster_oracle < content_graphics_trace);
    assert!(content_graphics_trace < reference_geometry_trace);
    assert!(reference_geometry_trace < reference_color_trace);
    assert!(reference_color_trace < debug_1);
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

    let shared_checks = "fmt,clippy,test,parser-mutation-smoke,case-manifests,m3-raster-oracle-contract,m3-content-graphics-trace,m3-reference-geometry-trace,m3-reference-color-trace,m2-scene-gate,m2-exit,m1-maturity,product-purity,product-release-closure,synthetic-failure-bundle";
    assert!(quality_main.contains(&format!("checks: \"{shared_checks}\"")));
    assert!(
        quality_main.contains(&format!("checks: \"{shared_checks},doc\"")),
        "PR selection must add documentation to the shared local M2 closure"
    );
    assert!(
        quality_main.contains(
            "local/pr checks include the M3 raster-oracle, Content graphics, Reference geometry, and Reference color contracts, M2 Scene profile replay, and M2 exit closure"
        )
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

#[test]
fn m2_evidence_binding_rejects_decoy_tables_and_map_confusion() {
    let root = repository_root();
    let zero_hash = "0".repeat(64);
    let decoy = format!(
        r#"verdict = "HOLD"
[decoy]
verdict = "SHIP"
implementation_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit_map = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}",
]
reviewed_subjects = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}#sha256:{zero_hash}",
]
"#
    );
    let decoy = RootToml::parse(&decoy).expect("the decoy document is structurally readable");
    decoy
        .expect_string("verdict", "HOLD")
        .expect("root HOLD cannot be replaced by a decoy SHIP");
    assert!(decoy.string("implementation_commit").is_err());
    assert!(verify_reviewed_subjects(&root, &decoy, M2_CONTENT_VM_COMMIT, None).is_err());

    let missing_map = format!(
        r#"implementation_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subjects = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}#sha256:{zero_hash}",
]
"#
    );
    let missing_map = RootToml::parse(&missing_map).expect("missing-map evidence parses");
    assert!(verify_reviewed_subjects(&root, &missing_map, M2_CONTENT_VM_COMMIT, None).is_err());

    let scalar_map = format!(
        r#"implementation_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit_map = "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}"
reviewed_subjects = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}#sha256:{zero_hash}",
]
"#
    );
    let scalar_map = RootToml::parse(&scalar_map).expect("scalar-map evidence parses");
    assert!(verify_reviewed_subjects(&root, &scalar_map, M2_CONTENT_VM_COMMIT, None).is_err());

    let other_commit = "0000000000000000000000000000000000000000";
    let wrong_map_commit = format!(
        r#"implementation_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit_map = [
  "core/content/src/vm.rs@{other_commit}",
]
reviewed_subjects = [
  "core/content/src/vm.rs@{other_commit}#sha256:{zero_hash}",
]
"#
    );
    let wrong_map_commit = RootToml::parse(&wrong_map_commit).expect("wrong-map evidence parses");
    assert!(
        verify_reviewed_subjects(&root, &wrong_map_commit, M2_CONTENT_VM_COMMIT, None)
            .expect_err("map commit must equal the frozen review commit")
            .contains("not")
    );

    let subject_map_mismatch = format!(
        r#"implementation_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit_map = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}",
]
reviewed_subjects = [
  "core/content/src/vm.rs@{other_commit}#sha256:{zero_hash}",
]
"#
    );
    let subject_map_mismatch =
        RootToml::parse(&subject_map_mismatch).expect("subject-map mismatch parses");
    assert!(
        verify_reviewed_subjects(&root, &subject_map_mismatch, M2_CONTENT_VM_COMMIT, None)
            .expect_err("same-path subject and map commits must agree")
            .contains("disagrees")
    );

    let duplicate_map = format!(
        r#"implementation_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit_map = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}",
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}",
]
reviewed_subjects = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}#sha256:{zero_hash}",
]
"#
    );
    let duplicate_map = RootToml::parse(&duplicate_map).expect("duplicate map evidence parses");
    assert!(
        verify_reviewed_subjects(&root, &duplicate_map, M2_CONTENT_VM_COMMIT, None)
            .expect_err("duplicate map paths must fail")
            .contains("duplicate commit-map")
    );

    let duplicate_subject = format!(
        r#"implementation_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit_map = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}",
]
reviewed_subjects = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}#sha256:{zero_hash}",
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}#sha256:{zero_hash}",
]
"#
    );
    let duplicate_subject =
        RootToml::parse(&duplicate_subject).expect("duplicate subject evidence parses");
    assert!(
        verify_reviewed_subjects(&root, &duplicate_subject, M2_CONTENT_VM_COMMIT, None)
            .expect_err("duplicate subject paths must fail")
            .contains("duplicate reviewed-subject")
    );

    let unbound_pin = format!(
        r#"implementation_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit_map = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}",
]
reviewed_subjects = [
  "plan/m2.toml@{M2_CONTENT_VM_COMMIT}#sha256:{zero_hash}",
]
"#
    );
    let unbound_pin = RootToml::parse(&unbound_pin).expect("unbound-pin evidence parses");
    assert!(verify_reviewed_subjects(&root, &unbound_pin, M2_CONTENT_VM_COMMIT, None).is_err());

    let unpinned_subject = format!(
        r#"implementation_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit = "{M2_CONTENT_VM_COMMIT}"
reviewed_subject_commit_map = [
  "core/content/src/vm.rs@{M2_CONTENT_VM_COMMIT}",
]
reviewed_subjects = [
  "core/content/src/vm.rs#sha256:{zero_hash}",
]
"#
    );
    let unpinned_subject =
        RootToml::parse(&unpinned_subject).expect("unpinned-subject evidence parses");
    assert!(
        verify_reviewed_subjects(&root, &unpinned_subject, M2_CONTENT_VM_COMMIT, None)
            .expect_err("mapped subjects must carry their commit in the locator")
            .contains("explicit @commit")
    );
}

#[test]
fn m2_root_toml_parser_preserves_types_arrays_and_root_scope() {
    let parsed = RootToml::parse(
        r#"schema = 1
type = "milestone-evidence"
registered = true
gating = false
reviewed_at = 2026-07-16
reviewer_roles = ["parser-security", "spec, conformance"]
commands = [
  "first",
  "second, with comma",
]
verdict = "HOLD"

[decoy]
schema = 2
verdict = "SHIP"
"#,
    )
    .expect("typed root evidence parses");
    parsed
        .expect_unsigned("schema", 1)
        .expect("schema is a canonical u64");
    parsed
        .expect_string("type", "milestone-evidence")
        .expect("type is a basic string");
    parsed
        .expect_bool("registered", true)
        .expect("registered is a boolean");
    parsed
        .expect_bool("gating", false)
        .expect("gating is a boolean");
    parsed
        .expect_bare("reviewed_at", "2026-07-16")
        .expect("reviewed_at is a bare date");
    parsed
        .expect_array("reviewer_roles", &["parser-security", "spec, conformance"])
        .expect("inline arrays retain commas inside strings");
    parsed
        .expect_array("commands", &["first", "second, with comma"])
        .expect("multiline arrays retain commas inside strings");
    parsed
        .expect_string("verdict", "HOLD")
        .expect("table values cannot impersonate root values");

    assert!(RootToml::parse("schema = 01").is_err());
    assert!(RootToml::parse("roles = [\"parser\\\\-security\"]").is_err());
    assert!(RootToml::parse("verdict = \"SHIP\"\nverdict = \"HOLD\"").is_err());
    for malformed_header in [
        "verdict = \"SHIP\"\n[decoy\nverdict = \"HOLD\"",
        "verdict = \"SHIP\"\n[]\nverdict = \"HOLD\"",
        "verdict = \"SHIP\"\n[[decoy]]junk\nverdict = \"HOLD\"",
    ] {
        assert!(
            RootToml::parse(malformed_header).is_err(),
            "malformed pseudo-headers must fail closed"
        );
    }
    assert!(
        RootToml::parse(
            r#"decoy = '''
schema = 1
verdict = "SHIP"
[stop]
'''
verdict = "HOLD"
"#
        )
        .is_err(),
        "multiline literal strings cannot impersonate root evidence"
    );
    assert!(
        RootToml::parse(
            r#"decoy = """
schema = 1
verdict = "SHIP"
[stop]
"""
verdict = "HOLD"
"#
        )
        .is_err(),
        "multiline basic strings cannot impersonate root evidence"
    );
    let wrong_type = RootToml::parse("schema = \"1\"").expect("quoted schema parses as a string");
    assert!(wrong_type.unsigned("schema").is_err());
}

#[test]
fn m2_evidence_locators_require_canonical_paths_and_digests() {
    for path in [
        "",
        "/absolute",
        "C:/windows",
        r"C:\windows",
        ".",
        "./file",
        "directory/.",
        "..",
        "../file",
        "directory/../file",
        "directory//file",
        "directory/",
        r"directory\file",
        "directory:stream",
        "file@commit",
    ] {
        assert!(
            validate_relative_path(path).is_err(),
            "accepted non-canonical evidence path {path:?}"
        );
    }
    assert_eq!(
        validate_relative_path("directory/file").expect("normal relative path is accepted"),
        PathBuf::from("directory/file")
    );

    let lowercase_hash = "a".repeat(64);
    assert!(validate_sha256(&lowercase_hash).is_ok());
    assert!(validate_sha256(&lowercase_hash.to_uppercase()).is_err());
    assert!(validate_sha256(&"0".repeat(63)).is_err());

    assert!(validate_commit_id(M2_CONTENT_VM_COMMIT).is_ok());
    assert!(validate_commit_id(&M2_CONTENT_VM_COMMIT.to_uppercase()).is_err());
    assert!(validate_commit_id(&"0".repeat(39)).is_err());
    assert!(
        split_subject_entry("path#sha256:00#sha256:11").is_err(),
        "multiple digest separators must be rejected"
    );
}

#[test]
fn m2_unpinned_subjects_reject_non_regular_files_and_symlinks() {
    let directory = TestDirectory::new("pdf-rs-m2-evidence-files");
    let root = directory.path().join("repository");
    fs::create_dir_all(root.join("nested")).expect("create synthetic repository");
    fs::write(root.join("nested/subject"), b"bound").expect("write regular subject");

    assert_eq!(
        read_repository_file(&root, "nested/subject").expect("regular file is readable"),
        b"bound"
    );
    assert!(read_repository_file(&root, "nested").is_err());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = directory.path().join("outside");
        fs::create_dir_all(&outside).expect("create outside directory");
        fs::write(outside.join("subject"), b"outside").expect("write outside subject");
        symlink(outside.join("subject"), root.join("subject-link")).expect("create file symlink");
        symlink(&outside, root.join("directory-link")).expect("create directory symlink");

        assert!(read_repository_file(&root, "subject-link").is_err());
        assert!(read_repository_file(&root, "directory-link/subject").is_err());
    }
}

#[test]
fn m2_pinned_subjects_require_reachable_commits_and_regular_blobs() {
    let directory = TestDirectory::new("pdf-rs-m2-evidence-git");
    let root = directory.path();
    git_output(root, &["init", "--quiet"]).expect("initialize synthetic repository");
    fs::create_dir_all(root.join("nested")).expect("create nested subject directory");
    fs::write(root.join("bound.txt"), b"base").expect("write base subject");
    fs::write(root.join("nested/subject.txt"), b"nested").expect("write nested subject");
    git_output(root, &["add", "--", "bound.txt", "nested/subject.txt"])
        .expect("stage base subjects");
    git_output(
        root,
        &[
            "-c",
            "user.name=PDF.rs",
            "-c",
            "user.email=pdf-rs@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
    )
    .expect("commit base subjects");
    let base = git_revision(root, "HEAD");

    git_output(root, &["checkout", "--quiet", "--orphan", "current"])
        .expect("create unrelated current history");
    fs::write(root.join("bound.txt"), b"current").expect("write current subject");
    git_output(root, &["add", "--", "bound.txt", "nested/subject.txt"])
        .expect("stage current subjects");
    git_output(
        root,
        &[
            "-c",
            "user.name=PDF.rs",
            "-c",
            "user.email=pdf-rs@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "current",
        ],
    )
    .expect("commit current subjects");
    let current = git_revision(root, "HEAD");

    assert_eq!(
        read_commit_blob(root, "bound.txt", &current).expect("reachable regular blob is readable"),
        b"current"
    );
    assert!(
        read_commit_blob(root, "bound.txt", &base)
            .expect_err("unrelated commit must fail reachability")
            .contains("not an ancestor")
    );
    assert!(
        read_commit_blob(root, "nested", &current)
            .expect_err("tree object must not be accepted as a subject")
            .contains("not one exact regular blob")
    );

    let object = format!("{current}:bound.txt");
    let blob = git_revision(root, &object);
    assert!(
        read_commit_blob(root, "bound.txt", &blob).is_err(),
        "a blob object id must not be accepted as a commit"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        symlink("bound.txt", root.join("subject-link")).expect("create Git symlink subject");
        git_output(root, &["add", "--", "subject-link"]).expect("stage Git symlink subject");
        git_output(
            root,
            &[
                "-c",
                "user.name=PDF.rs",
                "-c",
                "user.email=pdf-rs@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "symlink",
            ],
        )
        .expect("commit Git symlink subject");
        let symlink_commit = git_revision(root, "HEAD");
        assert!(
            read_commit_blob(root, "subject-link", &symlink_commit)
                .expect_err("Git mode 120000 must not be accepted as a regular blob")
                .contains("not one exact regular blob")
        );
    }
}

fn assert_m2_evidence_header(evidence: &RootToml, evidence_type: &str, id: &str, role: &str) {
    evidence
        .expect_unsigned("schema", 1)
        .expect("M2 evidence schema is exact");
    evidence
        .expect_string("type", evidence_type)
        .expect("M2 evidence type is exact");
    evidence
        .expect_string("id", id)
        .expect("M2 evidence id is exact");
    evidence
        .expect_string("milestone", "M2")
        .expect("M2 evidence milestone is exact");
    evidence
        .expect_string("work_item", "M2-07")
        .expect("M2 evidence work item is exact");
    evidence
        .expect_string("profile", "m2.scene-gate.v1")
        .expect("M2 evidence profile is exact");
    evidence
        .expect_string("feature", "quality.m2-scene-gate")
        .expect("M2 evidence feature is exact");
    evidence
        .expect_string("role", role)
        .expect("M2 evidence role is exact");
    evidence
        .expect_bool("registered", true)
        .expect("M2 evidence registered flag is exact");
    evidence
        .expect_bool("gating", true)
        .expect("M2 evidence gating flag is exact");
    evidence
        .expect_bool("external_observation", false)
        .expect("M2 evidence external-observation flag is exact");
    evidence
        .expect_bool("maturity_promotion", false)
        .expect("M2 evidence maturity-promotion flag is exact");
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
