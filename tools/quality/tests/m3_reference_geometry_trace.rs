use std::fs;
use std::path::{Path, PathBuf};

use pdf_rs_digest::{hex_digest, sha256};

#[path = "support/evidence.rs"]
mod evidence;

use evidence::{RootToml, array_table_records, verify_reviewed_subjects};

const TRACE_VERSION: &str = "0.78.0";
const COMPLETED_AT: &str = "2026-07-16";
const IMPLEMENTATION_COMMIT: &str = "213652f80d9d8c6749102f0aa7d6d53163e5ac3c";
const IMPLEMENTATION_TREE: &str = "de487ccfee70952024ea15b16e5c497794ec3269";
const M1_PAGE_TREE_HASH: &str = "e680abd131a3a4da61262eb152820c3e4f6252c6396a15447039713da3a0f5e1";
const M1_PAGE_TREE_TEST_HASH: &str =
    "aa8f4bbb5c4475d62a29a0cce3e8f798b17ea606e185b8b97017c2bc25e14374";

const GEOMETRY_SUBJECTS: [(&str, &str); 5] = [
    (
        "core/raster/PROVENANCE.md",
        "b7693bc88d8229fabf28f92b002112cb93656f3ef69b731ded978872d3bce926",
    ),
    (
        "core/raster/src/reference/coverage.rs",
        "a43e2ad1be3d27789a17ce2ec3a7671938cd6340438c8b1525701f9968d35139",
    ),
    (
        "core/raster/src/reference/geometry.rs",
        "edf9562b1965de73835283ea8eb0181515bf716484cc0fb08c07fae5755c5ad3",
    ),
    (
        "core/raster/tests/reference_geometry_kernel.rs",
        "c7196c5a772d9322533a0100bb9e7c3ecc361784ea6b21091e8bd6ec6f0984f5",
    ),
    (
        "core/raster/tests/reference_geometry_kernel_support/mod.rs",
        "01687caa3b76f860f11f8e421bdd0646ca9c5eafabcbc837db94d5e8c3f245f5",
    ),
];

const STROKE_CLIP_SUBJECTS: [(&str, &str); 6] = [
    GEOMETRY_SUBJECTS[0],
    GEOMETRY_SUBJECTS[1],
    GEOMETRY_SUBJECTS[2],
    (
        "core/raster/src/reference/stroke.rs",
        "011d3a508684d713228d5968c67d07bd436933e5711b50ae6f9d2218f62985dd",
    ),
    GEOMETRY_SUBJECTS[3],
    GEOMETRY_SUBJECTS[4],
];

#[test]
fn m3_geometry_and_stroke_reviews_are_exact_commit_tree_and_blob_bound() {
    let root = repository_root();
    verify_review(
        &root,
        ReviewExpectation {
            path: "docs/traceability/evidence/m3/reference-geometry-coverage/independent-review.toml",
            id: "evidence.m3.reference-geometry-coverage.independent-review",
            work_item: "M3-05",
            profile: "m3.reference-geometry-coverage.v1",
            feature: "core.reference-geometry-coverage",
            subjects: &GEOMETRY_SUBJECTS,
            scope_markers: &["M3-05 only", "color/compositing", "O0/O1/O3", "M3 exit"],
        },
    );
    verify_review(
        &root,
        ReviewExpectation {
            path: "docs/traceability/evidence/m3/reference-stroke-clip/independent-review.toml",
            id: "evidence.m3.reference-stroke-clip.independent-review",
            work_item: "M3-06",
            profile: "m3.reference-stroke-clip.v1",
            feature: "core.reference-stroke-clip",
            subjects: &STROKE_CLIP_SUBJECTS,
            scope_markers: &["M3-06 only", "color/compositing", "O0/O1/O3", "M3 exit"],
        },
    );
}

#[test]
fn m3_geometry_plan_features_and_spec_links_are_exact() {
    let root = repository_root();
    let plan_text = read_text(&root, "plan/m3.toml");
    let feature_text = read_text(&root, "docs/traceability/feature-map.toml");
    let spec_text = read_text(&root, "docs/traceability/spec-map.toml");

    let plan_root = RootToml::parse(&plan_text).expect("M3 plan root is strict TOML");
    assert_m3_plan_phase(&root, &plan_root);
    for (id, title, dependency) in [
        (
            "M3-05",
            "Fixed-point geometry preparation and scalar coverage",
            ["M3-03", "M3-04"].as_slice(),
        ),
        (
            "M3-06",
            "Stroke geometry and deterministic clip masks",
            ["M3-05"].as_slice(),
        ),
    ] {
        let item = table_record(&plan_text, "work_item", id);
        item.expect_string("title", title)
            .unwrap_or_else(|error| panic!("{id} title: {error}"));
        item.expect_string("status", "complete")
            .unwrap_or_else(|error| panic!("{id} status: {error}"));
        item.expect_bare("completed_at", COMPLETED_AT)
            .unwrap_or_else(|error| panic!("{id} completion: {error}"));
        item.expect_array("depends_on", dependency)
            .unwrap_or_else(|error| panic!("{id} dependency: {error}"));
    }
    let color = table_record(&plan_text, "work_item", "M3-07");
    color
        .expect_string("status", "complete")
        .expect("M3-07 status is exact");
    color
        .expect_bare("completed_at", COMPLETED_AT)
        .expect("M3-07 completion is exact");
    let image = table_record(&plan_text, "work_item", "M3-08");
    image
        .expect_string("status", "complete")
        .expect("M3-08 status is exact");
    image
        .expect_bare("completed_at", COMPLETED_AT)
        .expect("M3-08 completion is exact");
    let integrated = table_record(&plan_text, "work_item", "M3-10");
    integrated
        .expect_string("status", "complete")
        .expect("M3-10 status is exact");
    integrated
        .expect_bare("completed_at", COMPLETED_AT)
        .expect("M3-10 completion is exact");
    let exit = table_record(&plan_text, "work_item", "M3-11");
    exit.expect_string("status", "complete")
        .expect("M3-11 is complete in Candidate H");
    exit.expect_bare("completed_at", COMPLETED_AT)
        .expect("M3-11 completion date is exact");

    let feature_root = RootToml::parse(&feature_text).expect("feature map root is strict TOML");
    feature_root
        .expect_string("version", TRACE_VERSION)
        .expect("feature-map version is exact");
    for (id, profile, clauses) in [
        (
            "core.reference-geometry-coverage",
            "m3.reference-geometry-coverage.v1",
            &[
                "ISO-32000-1:2008/8.5",
                "RPE-ARCH-001/6.4-6.7",
                "RPE-ARCH-001/8.1-8.3",
                "RPE-ARCH-001/15.3/M3",
                "RPE-STD-001/3",
                "RPE-STD-001/5-11",
                "RPE-STD-003/8-9",
                "RPE-STD-003/12",
                "RPE-STD-005/4-7",
                "RPE-STD-005/11",
            ][..],
        ),
        (
            "core.reference-stroke-clip",
            "m3.reference-stroke-clip.v1",
            &[
                "ISO-32000-1:2008/8.4.2",
                "ISO-32000-1:2008/8.5",
                "RPE-ARCH-001/6.4-6.7",
                "RPE-ARCH-001/8.1-8.3",
                "RPE-ARCH-001/15.3/M3",
                "RPE-STD-001/3",
                "RPE-STD-001/5-11",
                "RPE-STD-003/8-9",
                "RPE-STD-003/12",
                "RPE-STD-005/4-7",
                "RPE-STD-005/11",
            ][..],
        ),
    ] {
        let feature = table_record(&feature_text, "feature", id);
        feature
            .expect_string("owner", "graphics-color")
            .unwrap_or_else(|error| panic!("{id} owner: {error}"));
        feature
            .expect_string("state", "PLANNED")
            .unwrap_or_else(|error| panic!("{id} maturity: {error}"));
        feature
            .expect_string("profile", profile)
            .unwrap_or_else(|error| panic!("{id} profile: {error}"));
        feature
            .expect_array("clauses", clauses)
            .unwrap_or_else(|error| panic!("{id} clauses: {error}"));
        feature
            .expect_array("modules", &["core/raster"])
            .unwrap_or_else(|error| panic!("{id} modules: {error}"));
        feature
            .expect_array(
                "tests",
                &[
                    "core/raster::reference_geometry_kernel",
                    "core/raster::repository_policy",
                    "tools/quality::m3_reference_geometry_trace",
                ],
            )
            .unwrap_or_else(|error| panic!("{id} tests: {error}"));
        feature
            .expect_array("fuzz_targets", &[])
            .unwrap_or_else(|error| panic!("{id} fuzz targets: {error}"));
        feature
            .expect_array("benchmarks", &[])
            .unwrap_or_else(|error| panic!("{id} benchmarks: {error}"));
    }

    let spec_root = RootToml::parse(&spec_text).expect("spec map root is strict TOML");
    spec_root
        .expect_string("version", TRACE_VERSION)
        .expect("spec-map version is exact");
    for id in [
        "ISO-32000-1:2008/8.5",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/8.1-8.3",
        "RPE-ARCH-001/15.3/M3",
    ] {
        let record = table_record(&spec_text, "requirement", id);
        for linkage in [
            "core.reference-geometry-coverage",
            "core.reference-stroke-clip",
        ] {
            assert!(
                record
                    .array("features")
                    .expect("requirement features are typed")
                    .iter()
                    .any(|feature| feature == linkage),
                "{id} is missing {linkage}"
            );
        }
        assert!(
            record
                .array("implementation")
                .expect("requirement implementation is typed")
                .iter()
                .any(|module| module == "core/raster"),
            "{id} is missing core/raster"
        );
        for test in [
            "core/raster::reference_geometry_kernel",
            "core/raster::repository_policy",
            "tools/quality::m3_reference_geometry_trace",
        ] {
            assert!(
                record
                    .array("tests")
                    .expect("requirement tests are typed")
                    .iter()
                    .any(|entry| entry == test),
                "{id} is missing {test}"
            );
        }
        let expected_status = if id == "RPE-ARCH-001/15.3/M3"
            && root
                .join("docs/traceability/evidence/m3/reference-raster-gate/independent-review.toml")
                .is_file()
        {
            "covered"
        } else {
            "partial"
        };
        record
            .expect_string("status", expected_status)
            .unwrap_or_else(|error| panic!("{id} status: {error}"));
    }
    let clip_state = table_record(&spec_text, "requirement", "ISO-32000-1:2008/8.4.2");
    clip_state
        .expect_array(
            "features",
            &[
                "core.content-vm-scene-v1",
                "core.content-graphics-v2",
                "core.reference-stroke-clip",
                "core.reference-raster-v1",
            ],
        )
        .expect("clip-state features are exact");
    for test in [
        "core/raster::reference_geometry_kernel",
        "core/raster::repository_policy",
        "tools/quality::m3_reference_geometry_trace",
    ] {
        assert!(
            clip_state
                .array("tests")
                .expect("clip-state tests are typed")
                .iter()
                .any(|entry| entry == test),
            "clip-state requirement is missing {test}"
        );
    }
}

#[test]
fn m3_geometry_gate_is_selected_before_m2_replay_and_preserves_m1() {
    let root = repository_root();
    let ci = read_text(&root, "scripts/ci.sh");
    let quality_main = read_text(&root, "tools/quality/src/main.rs");
    let content = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_content_graphics_trace",
    );
    let geometry = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_reference_geometry_trace",
    );
    let color = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_reference_color_trace",
    );
    let m2_replay = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$PWD/$m2_scene_gate_root/debug-1\"",
    );
    assert!(content < geometry);
    assert!(geometry < color);
    assert!(color < m2_replay);
    assert_eq!(
        quality_main.matches("m3-reference-geometry-trace").count(),
        2,
        "local and PR selections must both name the geometry trace"
    );
    assert_eq!(
        quality_main.matches("m3-reference-color-trace").count(),
        2,
        "local and PR selections must both name the color trace"
    );

    assert_eq!(
        file_sha256(&root.join("core/document/src/page_tree.rs")),
        M1_PAGE_TREE_HASH
    );
    assert_eq!(
        file_sha256(&root.join("core/document/tests/page_tree_count.rs")),
        M1_PAGE_TREE_TEST_HASH
    );
}

struct ReviewExpectation<'a> {
    path: &'a str,
    id: &'a str,
    work_item: &'a str,
    profile: &'a str,
    feature: &'a str,
    subjects: &'a [(&'a str, &'a str)],
    scope_markers: &'a [&'a str],
}

fn verify_review(root: &Path, expected: ReviewExpectation<'_>) {
    let review =
        RootToml::parse(&read_text(root, expected.path)).expect("review evidence is strict TOML");
    review
        .expect_unsigned("schema", 1)
        .expect("schema is exact");
    review
        .expect_string("type", "work-item-evidence")
        .expect("type is exact");
    review
        .expect_string("id", expected.id)
        .expect("id is exact");
    review
        .expect_string("milestone", "M3")
        .expect("milestone is exact");
    review
        .expect_string("work_item", expected.work_item)
        .expect("work item is exact");
    review
        .expect_string("profile", expected.profile)
        .expect("profile is exact");
    review
        .expect_string("feature", expected.feature)
        .expect("feature is exact");
    review
        .expect_string("role", "independent-review")
        .expect("role is exact");
    review
        .expect_bool("registered", true)
        .expect("registered is exact");
    review.expect_bool("gating", true).expect("gating is exact");
    review
        .expect_bool("external_observation", false)
        .expect("external observation is exact");
    review
        .expect_bool("maturity_promotion", false)
        .expect("maturity promotion is exact");
    review
        .expect_bare("reviewed_at", COMPLETED_AT)
        .expect("review date is exact");
    review
        .expect_string("implementation_commit", IMPLEMENTATION_COMMIT)
        .expect("implementation commit is exact");
    review
        .expect_string("reviewed_subject_commit", IMPLEMENTATION_COMMIT)
        .expect("reviewed subject commit is exact");
    review
        .expect_string("reviewed_subject_tree", IMPLEMENTATION_TREE)
        .expect("reviewed subject tree is exact");
    review
        .expect_string(
            "reviewed_subject_resolution",
            "git-tree-at-reviewed-subject-commit-not-working-tree",
        )
        .expect("review resolution is exact");
    review
        .expect_array("reviewer_roles", &["spec-conformance", "parser-security"])
        .expect("reviewer roles are exact");
    review
        .expect_array(
            "commands",
            &[
                "cargo test --locked --package pdf-rs-raster --test reference_geometry_kernel",
                "cargo test --locked --release --package pdf-rs-raster --test reference_geometry_kernel",
                "cargo test --locked --package pdf-rs-raster --all-targets",
                "cargo clippy --locked --package pdf-rs-raster --all-targets -- -D warnings",
                "cargo fmt --all -- --check",
                "git show --check 213652f",
                "git diff-tree --check 213652f^ 213652f",
            ],
        )
        .expect("review commands are exact");
    review
        .expect_unsigned("open_p0_p2", 0)
        .expect("open findings are exact");
    review
        .expect_string("verdict", "SHIP")
        .expect("review verdict is exact");

    let expected_map = expected
        .subjects
        .iter()
        .map(|(path, _)| format!("{path}@{IMPLEMENTATION_COMMIT}"))
        .collect::<Vec<_>>();
    assert_eq!(
        review
            .array("reviewed_subject_commit_map")
            .expect("review commit map is typed"),
        expected_map
    );
    let expected_subjects = expected
        .subjects
        .iter()
        .map(|(path, hash)| format!("{path}@{IMPLEMENTATION_COMMIT}#sha256:{hash}"))
        .collect::<Vec<_>>();
    assert_eq!(
        review
            .array("reviewed_subjects")
            .expect("reviewed subjects are typed"),
        expected_subjects
    );
    assert_eq!(
        verify_reviewed_subjects(
            root,
            &review,
            IMPLEMENTATION_COMMIT,
            Some(IMPLEMENTATION_TREE),
        )
        .expect("reviewed subjects are commit/tree/blob bound"),
        expected.subjects.len()
    );
    let scope = review
        .string("review_scope")
        .expect("review scope is typed");
    for marker in expected.scope_markers {
        assert!(scope.contains(marker), "review scope is missing {marker}");
    }
}

fn table_record(document: &str, table: &str, id: &str) -> RootToml {
    let records = array_table_records(document, table)
        .unwrap_or_else(|error| panic!("cannot parse [[{table}]] records: {error}"));
    let mut matches = records
        .into_iter()
        .filter(|record| record.string("id").is_ok_and(|candidate| candidate == id));
    let record = matches
        .next()
        .unwrap_or_else(|| panic!("missing [[{table}]] record {id}"));
    assert!(
        matches.next().is_none(),
        "duplicate [[{table}]] record {id}"
    );
    record
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("quality crate has a repository root")
        .to_path_buf()
}

fn assert_m3_plan_phase(root: &Path, plan: &RootToml) {
    if root
        .join("docs/traceability/evidence/m3/reference-raster-gate/independent-review.toml")
        .is_file()
    {
        plan.expect_string("status", "complete")
            .expect("M3 is complete after final independent review");
        plan.expect_bare("completed_at", COMPLETED_AT)
            .expect("completed M3 has the exact completion date");
    } else {
        plan.expect_string("status", "in_progress")
            .expect("Candidate H keeps M3 in progress before final independent review");
        assert!(
            plan.bare("completed_at").is_err(),
            "Candidate H must not predeclare milestone completion"
        );
    }
}

fn read_text(root: &Path, relative: &str) -> String {
    fs::read_to_string(root.join(relative))
        .unwrap_or_else(|error| panic!("cannot read {relative}: {error}"))
}

fn position(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("missing required marker {needle:?}"))
}

fn file_sha256(path: &Path) -> String {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let digest =
        sha256(&bytes).unwrap_or_else(|error| panic!("cannot hash {}: {error}", path.display()));
    hex_digest(&digest)
}
