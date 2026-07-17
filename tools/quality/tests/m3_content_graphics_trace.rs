use std::fs;
use std::path::{Path, PathBuf};

#[path = "support/evidence.rs"]
mod evidence;

use evidence::{RootToml, array_table_records, verify_reviewed_subjects};

const TRACE_VERSION: &str = "0.77.0";
const COMPLETED_AT: &str = "2026-07-16";
const IMPLEMENTATION_COMMIT: &str = "b2a0b88ce3c0f4d186f450a793909d1f72a75230";
const IMPLEMENTATION_TREE: &str = "b22d0ae88ced2194d884ec781b30f0c5ff367747";
const ISO_SNAPSHOT: &str =
    "sha256:9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff";
const ARCH_SNAPSHOT: &str =
    "sha256:53d46023770b4558705cc00f779fb3031245d473378d82869875283913157541";

const REVIEWED_SUBJECTS: [(&str, &str); 12] = [
    (
        "core/content/src/graphics_limits.rs",
        "a04af90e907643594fe7142ccdd19b6c72e7e5b238ed237709584a56cfd93d42",
    ),
    (
        "core/content/src/lib.rs",
        "3edd246755ea0305fe38f8e228fcc0fafe1105ce25354836628a8dff5dc40021",
    ),
    (
        "core/content/src/model.rs",
        "e44d7416a860a763fac8c6fabc9c7883f0af09d75795d50b06849381ea468611",
    ),
    (
        "core/content/src/vm.rs",
        "4abe742364c3a4f566ef092a923693004672bced49c68ef59df3e4a0e2525d25",
    ),
    (
        "core/content/src/vm/graphics.rs",
        "8d3418bb9e8ed51f76f7fd6a0d5ba36ff60d34fbc160eca8b7a4fa99e76a2df6",
    ),
    (
        "core/content/src/vm_error.rs",
        "b665c263308d326098237e8583828376e0686907549ef3368362f2883a3b3a5f",
    ),
    (
        "core/content/src/vm_model.rs",
        "ba754f408a98cbdbdd5a8ef587cb8ef1603bcdb533ce18008487c8f6852b0222",
    ),
    (
        "core/content/tests/scanner.rs",
        "e23d70503a50ea5c77ab72ff4fbdeb8a1efa55a639a5d3e12b5d504cfb0e22a0",
    ),
    (
        "core/content/tests/vm_graphics.rs",
        "2d5a763768320153546010fbbc53f71595f823feafa7ecd27e9b629d9a55389b",
    ),
    (
        "core/scene/src/graphics.rs",
        "85b8613c15404494dda1b13b3f2f334d8aede0206aa9b1b292ca837398e39242",
    ),
    (
        "core/scene/src/lib.rs",
        "2f343e0e8d1c649dd6b153e4f9c077fa14b57044578b8637a9586c6e89272f0a",
    ),
    (
        "docs/traceability/spec-map.toml",
        "6b1279c8efaf02ea892b5ae20559bb323da2446eeb50332c6e08cbd46b2ed972",
    ),
];

#[test]
fn m3_content_graphics_review_is_exact_commit_tree_and_blob_bound() {
    let root = repository_root();
    let review = RootToml::parse(&read_text(
        &root,
        "docs/traceability/evidence/m3/content-graphics-v2/independent-review.toml",
    ))
    .expect("M3-04 independent review has strict root TOML");

    review
        .expect_unsigned("schema", 1)
        .expect("review schema is exact");
    review
        .expect_string("type", "work-item-evidence")
        .expect("review type is exact");
    review
        .expect_string("id", "evidence.m3.content-graphics-v2.independent-review")
        .expect("review id is exact");
    review
        .expect_string("milestone", "M3")
        .expect("review milestone is exact");
    review
        .expect_string("work_item", "M3-04")
        .expect("review work item is exact");
    review
        .expect_string("profile", "m3.content-graphics-v2.v1")
        .expect("review profile is exact");
    review
        .expect_string("feature", "core.content-graphics-v2")
        .expect("review feature is exact");
    review
        .expect_string("role", "independent-review")
        .expect("review role is exact");
    review
        .expect_bool("registered", true)
        .expect("registered flag is exact");
    review
        .expect_bool("gating", true)
        .expect("gating flag is exact");
    review
        .expect_bool("external_observation", false)
        .expect("external-observation flag is exact");
    review
        .expect_bool("maturity_promotion", false)
        .expect("maturity-promotion flag is exact");
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
                "cargo test --locked --package pdf-rs-content --package pdf-rs-scene --all-targets",
                "cargo clippy --locked --package pdf-rs-content --package pdf-rs-scene --all-targets -- -D warnings",
                "cargo fmt --all -- --check",
                "git show --check b2a0b88",
                "git diff-tree --check b2a0b88^ b2a0b88",
            ],
        )
        .expect("review commands are exact");
    review
        .expect_unsigned("open_p0_p2", 0)
        .expect("open P0-P2 count is exact");
    review
        .expect_string("verdict", "SHIP")
        .expect("review verdict is exact");

    let expected_map = REVIEWED_SUBJECTS
        .iter()
        .map(|(path, _)| format!("{path}@{IMPLEMENTATION_COMMIT}"))
        .collect::<Vec<_>>();
    assert_eq!(
        review
            .array("reviewed_subject_commit_map")
            .expect("review commit map is a typed root array"),
        expected_map
    );
    let expected_subjects = REVIEWED_SUBJECTS
        .iter()
        .map(|(path, hash)| format!("{path}@{IMPLEMENTATION_COMMIT}#sha256:{hash}"))
        .collect::<Vec<_>>();
    assert_eq!(
        review
            .array("reviewed_subjects")
            .expect("reviewed subjects are a typed root array"),
        expected_subjects
    );
    assert_eq!(
        verify_reviewed_subjects(
            &root,
            &review,
            IMPLEMENTATION_COMMIT,
            Some(IMPLEMENTATION_TREE),
        )
        .expect("all M3-04 reviewed subjects are bound to the reviewed commit tree"),
        REVIEWED_SUBJECTS.len()
    );

    let scope = review
        .string("review_scope")
        .expect("review scope is a typed narrative string");
    for exclusion in ["M3-04 only", "M3-05", "O0/O1/O3", "maturity promotion"] {
        assert!(
            scope.contains(exclusion),
            "review scope is missing bounded exclusion {exclusion}"
        );
    }
}

#[test]
fn m3_content_graphics_review_rejects_decoy_verdicts_and_commit_rebinding() {
    let root = repository_root();
    let review = read_text(
        &root,
        "docs/traceability/evidence/m3/content-graphics-v2/independent-review.toml",
    );

    let decoy = review.replacen(
        "verdict = \"SHIP\"",
        "verdict = \"HOLD\"\n\n[decoy]\nverdict = \"SHIP\"",
        1,
    );
    let decoy = RootToml::parse(&decoy).expect("mutated decoy review remains parseable");
    decoy
        .expect_string("verdict", "HOLD")
        .expect("the root verdict remains authoritative");
    assert!(
        decoy.expect_string("verdict", "SHIP").is_err(),
        "a decoy table must not impersonate the root verdict"
    );

    let first_subject = format!(
        "{}@{IMPLEMENTATION_COMMIT}#sha256:{}",
        REVIEWED_SUBJECTS[0].0, REVIEWED_SUBJECTS[0].1
    );
    let rebound_subject = format!(
        "{}@ee6720f615cc3042638a957b85b377b3e0ab054c#sha256:{}",
        REVIEWED_SUBJECTS[0].0, REVIEWED_SUBJECTS[0].1
    );
    let rebound = review.replacen(&first_subject, &rebound_subject, 1);
    let rebound = RootToml::parse(&rebound).expect("mutated commit review remains parseable");
    assert!(
        verify_reviewed_subjects(
            &root,
            &rebound,
            IMPLEMENTATION_COMMIT,
            Some(IMPLEMENTATION_TREE),
        )
        .expect_err("reviewed subjects cannot be rebound away from the commit map")
        .contains("disagrees")
    );
}

#[test]
fn m3_trace_tables_reject_multiline_string_decoys_and_duplicate_ids() {
    let root_decoy = r#"
decoy = '''
schema = 1
type = "work-item-evidence"
implementation_commit = "b2a0b88ce3c0f4d186f450a793909d1f72a75230"
verdict = "SHIP"
[stop]
'''
verdict = "HOLD"
"#;
    assert!(
        RootToml::parse(root_decoy)
            .expect_err("root multiline literals are outside the strict trace subset")
            .contains("multiline string")
    );
    assert!(
        RootToml::parse("verdict = \"SHIP\"\n[stop\nverdict = \"HOLD\"").is_err(),
        "a malformed pseudo-header cannot terminate root evidence"
    );

    let multiline_decoy = r#"
description = """
[[feature]]
id = "core.content-graphics-v2"
owner = "graphics-color"
state = "PLANNED"
"""

[[feature]]
id = "real.feature"
owner = "graphics-color"
state = "PLANNED"
"#;
    assert!(
        array_table_records(multiline_decoy, "feature")
            .expect_err("multiline strings are outside the strict trace subset")
            .contains("multiline string")
    );

    let duplicate = r#"
[[feature]]
id = "core.content-graphics-v2"
owner = "graphics-color"

[[feature]]
id = "core.content-graphics-v2"
owner = "graphics-color"
"#;
    let result =
        std::panic::catch_unwind(|| table_record(duplicate, "feature", "core.content-graphics-v2"));
    assert!(
        result.is_err(),
        "duplicate trace record ids must fail closed"
    );

    let harmless_string = r#"
description = "the bytes [[feature]] are not a table header"

[[feature]]
id = "real.feature"
owner = "graphics-color"
"#;
    let records =
        array_table_records(harmless_string, "feature").expect("single-line strings are lexical");
    assert_eq!(records.len(), 1);
    records[0]
        .expect_string("id", "real.feature")
        .expect("only the real table is parsed");

    let comment_decoy = r#"
# [[feature]]
# id = "core.content-graphics-v2"
# owner = "graphics-color"

[[feature]]
id = "real.feature"
owner = "graphics-color"
"#;
    let records =
        array_table_records(comment_decoy, "feature").expect("comments are not table records");
    assert_eq!(records.len(), 1);
    records[0]
        .expect_string("id", "real.feature")
        .expect("only the uncommented table is parsed");
}

#[test]
fn m3_content_graphics_plan_and_trace_registration_are_exact() {
    let root = repository_root();
    let plan_text = read_text(&root, "plan/m3.toml");
    let feature_text = read_text(&root, "docs/traceability/feature-map.toml");
    let spec_text = read_text(&root, "docs/traceability/spec-map.toml");

    let plan_root = RootToml::parse(&plan_text).expect("M3 plan root is strict TOML");
    plan_root
        .expect_unsigned("schema", 1)
        .expect("M3 plan schema is exact");
    plan_root
        .expect_string("milestone", "M3")
        .expect("M3 plan milestone is exact");
    plan_root
        .expect_string("status", "in_progress")
        .expect("M3 remains in progress after M3-04");
    plan_root
        .expect_bare("started_at", COMPLETED_AT)
        .expect("M3 start date is exact");

    let work_item = table_record(&plan_text, "work_item", "M3-04");
    work_item
        .expect_string("title", "Content VM path, paint, line, and clip semantics")
        .expect("M3-04 title is exact");
    work_item
        .expect_string("lane", "content-vm")
        .expect("M3-04 lane is exact");
    work_item
        .expect_string("status", "complete")
        .expect("M3-04 status is exact");
    work_item
        .expect_bare("completed_at", COMPLETED_AT)
        .expect("M3-04 completion date is exact");
    work_item
        .expect_array("depends_on", &["M3-03"])
        .expect("M3-04 dependency is exact");
    work_item
        .expect_string(
            "outcome",
            "The bounded Content VM produces exact Scene path, fill, stroke, line-state, and clipping semantics.",
        )
        .expect("M3-04 outcome is exact");
    work_item
        .expect_array(
            "acceptance",
            &[
                "Path construction and painting operators validate operand count, type, context, and deterministic cost before mutation.",
                "Graphics-state save and restore retain every registered line, color, alpha, blend, and clip field.",
                "Current-path and clipping semantics are independently bounded and never publish malformed partial Scene state.",
                "Equivalent path and matrix formulations have model and metamorphic tests.",
            ],
        )
        .expect("M3-04 acceptance criteria are exact");

    let feature_root = RootToml::parse(&feature_text).expect("feature map root is strict TOML");
    feature_root
        .expect_unsigned("schema", 1)
        .expect("feature map schema is exact");
    feature_root
        .expect_string("version", TRACE_VERSION)
        .expect("feature map version is exact");
    feature_root
        .expect_string("status", "active")
        .expect("feature map status is exact");
    let feature = table_record(&feature_text, "feature", "core.content-graphics-v2");
    feature
        .expect_string("owner", "graphics-color")
        .expect("feature owner is exact");
    feature
        .expect_string("state", "PLANNED")
        .expect("feature maturity remains PLANNED");
    feature
        .expect_string("profile", "m3.content-graphics-v2.v1")
        .expect("feature profile is exact");
    feature
        .expect_array(
            "clauses",
            &[
                "ISO-32000-1:2008/7.8.2",
                "ISO-32000-1:2008/8.4.2",
                "ISO-32000-1:2008/8.4.3",
                "ISO-32000-1:2008/8.5",
                "ISO-32000-1:2008/8.6",
                "RPE-ARCH-001/6.1-6.2",
                "RPE-ARCH-001/6.4-6.7",
                "RPE-ARCH-001/15.3/M3",
                "RPE-STD-001/5-11",
                "RPE-STD-003/8-9",
                "RPE-STD-005/4-7",
            ],
        )
        .expect("feature clauses are exact");
    feature
        .expect_array("modules", &["core/content", "core/scene"])
        .expect("feature modules are exact");
    feature
        .expect_array(
            "tests",
            &[
                "core/content::scanner",
                "core/content::vm_graphics",
                "core/content::repository_policy",
                "core/scene::scene_v2",
                "tools/quality::m3_content_graphics_trace",
            ],
        )
        .expect("feature tests include the independent trace gate");
    feature
        .expect_array("fuzz_targets", &[])
        .expect("feature fuzz targets remain empty");
    feature
        .expect_array("benchmarks", &[])
        .expect("feature benchmarks remain empty");
    feature
        .expect_string("introduced_in", "0.1.0")
        .expect("feature introduction version is exact");

    let spec_root = RootToml::parse(&spec_text).expect("spec map root is strict TOML");
    spec_root
        .expect_unsigned("schema", 1)
        .expect("spec map schema is exact");
    spec_root
        .expect_string("version", TRACE_VERSION)
        .expect("spec map version is exact");
    spec_root
        .expect_string("status", "active")
        .expect("spec map status is exact");

    assert_requirement(
        &spec_text,
        RequirementExpectation {
            id: "ISO-32000-1:2008/7.8.2",
            snapshot: ISO_SNAPSHOT,
            features: &[
                "core.page-content-acquisition",
                "core.content-operator-scanner",
                "core.content-vm-scene-v1",
                "core.content-graphics-v2",
                "core.basic-embedded-text",
            ],
            implementation: &["core/document", "core/content", "core/scene"],
            tests: &[
                "core/document::page_content",
                "core/document::font_resource",
                "core/document::repository_policy",
                "core/content::scanner",
                "core/content::vm",
                "core/content::vm_graphics",
                "core/content::repository_policy",
                "core/scene::scene_v2",
                "tools/quality::m3_content_graphics_trace",
                "tools/quality::m3_basic_text_trace",
            ],
            note: "M3-04 extends",
        },
    );
    assert_requirement(
        &spec_text,
        RequirementExpectation {
            id: "ISO-32000-1:2008/8.4.2",
            snapshot: ISO_SNAPSHOT,
            features: &[
                "core.content-vm-scene-v1",
                "core.content-graphics-v2",
                "core.reference-stroke-clip",
                "core.reference-raster-v1",
            ],
            implementation: &["core/content", "core/scene", "core/raster"],
            tests: &[
                "core/content::vm",
                "core/content::vm_graphics",
                "core/content::repository_policy",
                "core/scene::scene_v2",
                "core/raster::reference_geometry_kernel",
                "core/raster::repository_policy",
                "tools/quality::m3_reference_geometry_trace",
                "tools/quality::m3_reference_gate",
                "tools/quality::m3_reference_raster_trace",
            ],
            note: "M3-06 adds a staged deterministic clip stack",
        },
    );
    assert_requirement(
        &spec_text,
        RequirementExpectation {
            id: "ISO-32000-1:2008/8.4.3",
            snapshot: ISO_SNAPSHOT,
            features: &[
                "core.content-vm-scene-v1",
                "core.content-graphics-v2",
                "core.reference-raster-v1",
            ],
            implementation: &["core/content", "core/scene"],
            tests: &[
                "core/content::vm",
                "core/content::vm_graphics",
                "core/content::repository_policy",
                "core/scene::scene_v1",
                "core/scene::scene_v2",
                "tools/quality::m3_reference_gate",
                "tools/quality::m3_reference_raster_trace",
            ],
            note: "M3-04 applies",
        },
    );
    assert_requirement(
        &spec_text,
        RequirementExpectation {
            id: "ISO-32000-1:2008/8.5",
            snapshot: ISO_SNAPSHOT,
            features: &[
                "core.content-graphics-v2",
                "core.scene-graphics-v2",
                "core.reference-geometry-coverage",
                "core.reference-stroke-clip",
                "core.reference-raster-v1",
            ],
            implementation: &["core/content", "core/scene", "core/raster"],
            tests: &[
                "core/content::scanner",
                "core/content::vm_graphics",
                "core/content::repository_policy",
                "core/scene::scene_v2",
                "core/raster::reference_geometry_kernel",
                "core/raster::repository_policy",
                "tools/quality::m3_content_graphics_trace",
                "tools/quality::m3_reference_geometry_trace",
                "tools/quality::m3_reference_gate",
                "tools/quality::m3_reference_raster_trace",
            ],
            note: "M3-05 and M3-06 add",
        },
    );
    assert_requirement(
        &spec_text,
        RequirementExpectation {
            id: "ISO-32000-1:2008/8.6",
            snapshot: ISO_SNAPSHOT,
            features: &[
                "core.content-graphics-v2",
                "core.scene-graphics-v2",
                "core.reference-color-compositing",
                "core.reference-raster-v1",
            ],
            implementation: &["core/content", "core/scene", "core/raster"],
            tests: &[
                "core/content::scanner",
                "core/content::vm_graphics",
                "core/content::repository_policy",
                "core/scene::scene_v2",
                "core/raster::reference_color",
                "core/raster::reference_scene_v2_boundary",
                "core/raster::repository_policy",
                "tools/quality::m3_content_graphics_trace",
                "tools/quality::m3_reference_color_trace",
                "tools/quality::m3_reference_gate",
                "tools/quality::m3_reference_raster_trace",
            ],
            note: "M3-07 freezes project-owned",
        },
    );
    assert_requirement(
        &spec_text,
        RequirementExpectation {
            id: "ISO-32000-1:2008/11.3.2-11.3.4",
            snapshot: ISO_SNAPSHOT,
            features: &[
                "core.reference-color-compositing",
                "core.reference-raster-v1",
            ],
            implementation: &["core/raster"],
            tests: &[
                "core/raster::reference_color",
                "core/raster::reference_scene_v2_boundary",
                "core/raster::repository_policy",
                "tools/quality::m3_reference_color_trace",
                "tools/quality::m3_reference_gate",
                "tools/quality::m3_reference_raster_trace",
            ],
            note: "M3-07 implements",
        },
    );
    assert_requirement(
        &spec_text,
        RequirementExpectation {
            id: "RPE-ARCH-001/6.1-6.2",
            snapshot: ARCH_SNAPSHOT,
            features: &[
                "core.content-operator-scanner",
                "core.page-property-lookup",
                "core.content-vm-scene-v1",
                "core.content-graphics-v2",
                "core.basic-image-xobjects",
                "core.basic-embedded-text",
                "core.reference-raster-v1",
            ],
            implementation: &["core/font", "core/document", "core/content", "core/scene"],
            tests: &[
                "core/font::truetype",
                "core/document::page_properties",
                "core/document::image_xobject",
                "core/document::font_resource",
                "core/document::repository_policy",
                "core/content::scanner",
                "core/content::vm",
                "core/content::vm_graphics",
                "core/content::repository_policy",
                "core/scene::scene_v1",
                "core/scene::scene_v2",
                "tools/quality::m3_content_graphics_trace",
                "tools/quality::m3_basic_image_trace",
                "tools/quality::m3_basic_text_trace",
                "tools/quality::m3_reference_gate",
                "tools/quality::m3_reference_raster_trace",
            ],
            note: "M3-04 adds",
        },
    );
    assert_requirement(
        &spec_text,
        RequirementExpectation {
            id: "RPE-ARCH-001/6.4-6.7",
            snapshot: ARCH_SNAPSHOT,
            features: &[
                "core.content-vm-scene-v1",
                "core.content-graphics-v2",
                "core.scene-v1",
                "core.scene-semantic-diff",
                "core.scene-graphics-v2",
                "quality.m2-scene-gate",
                "core.reference-pixel-foundation",
                "core.reference-geometry-coverage",
                "core.reference-stroke-clip",
                "core.reference-color-compositing",
                "core.basic-image-xobjects",
                "core.basic-embedded-text",
                "core.reference-raster-v1",
            ],
            implementation: &[
                "core/content",
                "core/scene",
                "core/raster",
                "tools/quality",
                "docs/traceability",
            ],
            tests: &[
                "core/content::vm",
                "core/content::vm_graphics",
                "core/content::repository_policy",
                "core/scene::scene_v1",
                "core/scene::scene_v2",
                "core/scene::scene_diff",
                "core/scene::scene_diff_v2_budget",
                "core/scene::repository_policy",
                "core/raster::reference_foundation",
                "core/raster::reference_scene_v2_boundary",
                "core/raster::reference_geometry_kernel",
                "core/raster::reference_color",
                "core/raster::reference_image",
                "core/raster::reference_glyph",
                "core/raster::repository_policy",
                "tools/quality::m2_scene_gate",
                "tools/quality::m2_exit",
                "tools/quality::m3_content_graphics_trace",
                "tools/quality::m3_reference_geometry_trace",
                "tools/quality::m3_reference_color_trace",
                "tools/quality::m3_basic_image_trace",
                "tools/quality::m3_basic_text_trace",
                "tools/quality::m3_reference_gate",
                "tools/quality::m3_reference_raster_trace",
            ],
            note: "M3-07 adds",
        },
    );
    assert_requirement(
        &spec_text,
        RequirementExpectation {
            id: "RPE-ARCH-001/15.3/M3",
            snapshot: ARCH_SNAPSHOT,
            features: &[
                "core.reference-pixel-foundation",
                "core.scene-graphics-v2",
                "core.content-graphics-v2",
                "core.reference-geometry-coverage",
                "core.reference-stroke-clip",
                "core.reference-color-compositing",
                "core.basic-image-xobjects",
                "core.basic-embedded-text",
                "quality.m3-raster-oracle-contract",
                "core.reference-raster-v1",
            ],
            implementation: &[
                "core/font",
                "core/document",
                "core/content",
                "core/scene",
                "core/raster",
                "tools/compare",
                "tools/quality",
                "docs/traceability",
            ],
            tests: &[
                "core/font::truetype",
                "core/font::repository_policy",
                "core/document::image_xobject",
                "core/document::font_resource",
                "core/document::repository_policy",
                "core/content::scanner",
                "core/content::vm_graphics",
                "core/content::repository_policy",
                "core/scene::scene_v2",
                "core/scene::scene_diff_v2_budget",
                "core/scene::repository_policy",
                "core/raster::reference_foundation",
                "core/raster::reference_scene_v2_boundary",
                "core/raster::reference_geometry_kernel",
                "core/raster::reference_color",
                "core/raster::reference_image",
                "core/raster::reference_glyph",
                "core/raster::repository_policy",
                "tools/compare::pixel",
                "tools/quality::m3_raster_oracle_contract",
                "tools/quality::m3_content_graphics_trace",
                "tools/quality::m3_reference_geometry_trace",
                "tools/quality::m3_reference_color_trace",
                "tools/quality::m3_basic_image_trace",
                "tools/quality::m3_basic_text_trace",
                "tools/quality::m2_exit",
                "tools/quality::purity",
                "tools/quality::m3_reference_gate",
                "tools/quality::m3_reference_raster_trace",
            ],
            note: "M3-08 closes",
        },
    );
}

#[test]
fn m3_history_bound_trace_gate_requires_full_git_checkout() {
    let root = repository_root();
    let workflow = read_text(&root, ".github/workflows/ci.yml");
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
        "commit/tree/blob-bound M3 evidence requires the full Git object graph"
    );
}

struct RequirementExpectation<'a> {
    id: &'a str,
    snapshot: &'a str,
    features: &'a [&'a str],
    implementation: &'a [&'a str],
    tests: &'a [&'a str],
    note: &'a str,
}

fn assert_requirement(document: &str, expected: RequirementExpectation<'_>) {
    let requirement = table_record(document, "requirement", expected.id);
    requirement
        .expect_string("snapshot_hash", expected.snapshot)
        .unwrap_or_else(|error| panic!("{} snapshot: {error}", expected.id));
    requirement
        .expect_array("features", expected.features)
        .unwrap_or_else(|error| panic!("{} features: {error}", expected.id));
    requirement
        .expect_array("implementation", expected.implementation)
        .unwrap_or_else(|error| panic!("{} implementation: {error}", expected.id));
    requirement
        .expect_array("tests", expected.tests)
        .unwrap_or_else(|error| panic!("{} tests: {error}", expected.id));
    requirement
        .expect_string("status", "partial")
        .unwrap_or_else(|error| panic!("{} status: {error}", expected.id));
    let notes = requirement
        .string("notes")
        .unwrap_or_else(|error| panic!("{} notes: {error}", expected.id));
    assert!(
        notes.contains(expected.note),
        "{} notes are missing {:?}",
        expected.id,
        expected.note
    );
}

fn table_record(document: &str, table: &str, id: &str) -> RootToml {
    let records = array_table_records(document, table)
        .unwrap_or_else(|error| panic!("cannot parse [[{table}]] records: {error}"));
    let mut matching = Vec::new();
    for record in records {
        let record_id = record
            .string("id")
            .unwrap_or_else(|error| panic!("[[{table}]] record id: {error}"));
        if record_id == id {
            matching.push(record);
        }
    }
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one [[{table}]] record {id}"
    );
    matching.pop().expect("one matching record exists")
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
