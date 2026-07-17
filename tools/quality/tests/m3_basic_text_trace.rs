use std::fs;
use std::path::{Path, PathBuf};

use pdf_rs_digest::{hex_digest, sha256};

#[path = "support/evidence.rs"]
mod evidence;

use evidence::{RootToml, array_table_records, verify_reviewed_subjects};

const TRACE_VERSION: &str = "0.78.0";
const COMPLETED_AT: &str = "2026-07-16";
const IMPLEMENTATION_COMMIT: &str = "e54b16e609e8fe3b47bc4a3617cf65da54d168dc";
const IMPLEMENTATION_TREE: &str = "d80008e164bcd2c951d2a093e8c6e098f0009f5d";
const ISO_SNAPSHOT: &str =
    "sha256:9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff";
const ARCH_SNAPSHOT: &str =
    "sha256:53d46023770b4558705cc00f779fb3031245d473378d82869875283913157541";
const M1_PAGE_TREE_HASH: &str = "e680abd131a3a4da61262eb152820c3e4f6252c6396a15447039713da3a0f5e1";
const M1_PAGE_TREE_TEST_HASH: &str =
    "aa8f4bbb5c4475d62a29a0cce3e8f798b17ea606e185b8b97017c2bc25e14374";

const REVIEWED_SUBJECTS: [(&str, &str); 41] = [
    (
        "core/font/PROVENANCE.md",
        "71759fd6ba115bd7ba50e9cb60adac83f80020e1ecc009e9a8ec0cc568361b6e",
    ),
    (
        "core/font/src/error.rs",
        "6ef52ebf91f3422c803257537e9c21499fff72570c9b8e860633720d485c7701",
    ),
    (
        "core/font/src/lib.rs",
        "193b6eddd948abc9a4cb2b3b4a12fb8750ef5c5f477f88c45a1b33660cb4ffe0",
    ),
    (
        "core/font/src/limits.rs",
        "ac9f7c2e563326baf9d7a6c81611d872a9367d0ca094f588735fd6774eb6bad0",
    ),
    (
        "core/font/src/model.rs",
        "d1e75045ab15dae0918da7b0ba68d79907240cf35b808a44fa6806388dbb1c9d",
    ),
    (
        "core/font/src/parse.rs",
        "adfc6a37445d5b23bdbd66d31aadd2c80d1d71e695834547d87c9d0539903b6d",
    ),
    (
        "core/font/tests/repository_policy.rs",
        "2e4aa7c2f7b102803a34419163805ad6e3c4b0df6b954c42df8b400a0e6d095b",
    ),
    (
        "core/font/tests/support/mod.rs",
        "28e3fa7c7546fa4bd27f1540a3a61ce98b25d6810c5b94e475f61f9c624ed2e2",
    ),
    (
        "core/font/tests/truetype.rs",
        "7bb6e9c9d95852003abbcc33c4e0adfdcdb62ecf4a62560610695daaee067491",
    ),
    (
        "core/document/PROVENANCE.md",
        "25f977acbfb0cb78b07e1734ec115b9c899588e690d64c7f0d58f1d55f37beb5",
    ),
    (
        "core/document/src/error.rs",
        "28fc9bd10fad9377babe684ed7b15a221d5451d09ac934bf7a17561d319663e8",
    ),
    (
        "core/document/src/font_resource.rs",
        "944e15954c2ba878aa2f498b65e86eb5bc2c63d440e07fa9ee18d686afc4f82d",
    ),
    (
        "core/document/src/font_resource_limits.rs",
        "6f9ff3a06d1821f40414063468866a865a2aa1d6e72889689e4898275d26a070",
    ),
    (
        "core/document/src/lib.rs",
        "620216f628a164a976d591b9f5c08e03c1c9fda6556955ba0632124756b9835c",
    ),
    (
        "core/document/src/page_font_lookup_limits.rs",
        "99e1764e99ecea9f35ca562b1d779a40d50a3d112ea81509eb6a0e5adb4955d5",
    ),
    (
        "core/document/src/page_resources.rs",
        "bddc6c0e12157c21b8d0604301557751a1d9d711003f9f67cdeb815a1b1a4eae",
    ),
    (
        "core/document/tests/font_resource.rs",
        "a556c30c79246731705c2841889b5f7df22bbc61ba900525e73d29da2058ab78",
    ),
    (
        "core/document/tests/repository_policy.rs",
        "43e2e57d2528d4d1011f72554234a47bb4e73ed4606766a2bc74c03b5778eb50",
    ),
    (
        "core/content/PROVENANCE.md",
        "a406fdee0481301d8a33be1a20384d392fe4ad4096c99decc4d3dfb37002a394",
    ),
    (
        "core/content/src/font_limits.rs",
        "0b3d81fd3ba370cf81296f1ebe3681584b403136f2e5999fa087c0946e7a6012",
    ),
    (
        "core/content/src/lib.rs",
        "70787aef5749005eeeed4b0a9daa678c906c3f8fb4e4e76bc8a800b1fdc3d60a",
    ),
    (
        "core/content/src/model.rs",
        "c428a473b2cc791aad1ffb49f7b1c8da1e048a2d0e0d9f78abf5049294f83f3e",
    ),
    (
        "core/content/src/vm.rs",
        "f8cafdb43dea9d545101ea65be89c42c6e0429b1985b6a1d2f1fceba396b85d4",
    ),
    (
        "core/content/src/vm/font.rs",
        "fbc484702ee6ef28b2ea88e18de55361460099f27cb9b24004d574fe43b6abe5",
    ),
    (
        "core/content/src/vm/graphics.rs",
        "b32417ffaf6640da133ac9d2fffbffb8f63123f6178360e4d01cc89fe55d0f12",
    ),
    (
        "core/content/src/vm_error.rs",
        "b6e6210bc29d4f90526ddca7a2e49e9f72213256e28a6d39aa821eef9efb1398",
    ),
    (
        "core/content/src/vm_model.rs",
        "5a20f9632688cd2b70d5788fda6ab80ae54eadc615637b253fdee9d598be1482",
    ),
    (
        "core/content/tests/repository_policy.rs",
        "01a91aeacbf369955ae70db44e8d9f186102b8516e6debd1ec15cdc48d9993d6",
    ),
    (
        "core/content/tests/vm_graphics.rs",
        "de40e5c019ba5f66a0ad5e536b5db57f30335f95b0f7253bbbdeea2accb68df3",
    ),
    (
        "core/scene/PROVENANCE.md",
        "fdf5e5eaec948e82f7fec138efafc99d7a070efbeca2d3c97a7196980338b402",
    ),
    (
        "core/scene/src/graphics.rs",
        "e8dfaed2f3fdb11ad25a7ce966f34b5d4dda86a248dbb560fa91722260709aff",
    ),
    (
        "core/scene/src/graphics_builder.rs",
        "5bf83dace4df3418a704819478f7f05d7d89b9f3c4ea43ecceee46155e818af6",
    ),
    (
        "core/scene/tests/repository_policy.rs",
        "778732c0fd559599f76b531713df21dd004c4a3b69407acd537444e9329dc772",
    ),
    (
        "core/scene/tests/scene_v2.rs",
        "1ebb95c5732e0c65ffa540f1f9b5ec685899bd24848cc6f013ea56ba29e36c42",
    ),
    (
        "core/raster/PROVENANCE.md",
        "dac38ef9d983ba489f38edae1164f33128dc6281d17b21a5419a8544c789a2d6",
    ),
    (
        "core/raster/src/reference/coverage.rs",
        "c08cc86289a39ac08ab3de867ce1d9b9c1baa4a49ebecdec09beea170dc3898b",
    ),
    (
        "core/raster/src/reference/geometry.rs",
        "6789b47aa3bdc21a98a366325865f400eee1b2b592e732a806fc25e8352d900e",
    ),
    (
        "core/raster/src/reference/glyph.rs",
        "76a4826268e8663616ac01f1182b0fbb4cbe7e4688bb3c98ddb2218890c1d8bf",
    ),
    (
        "core/raster/tests/reference_glyph.rs",
        "0cd00f13222b4663b0915678661762233e5e99fcb1cc94ac735be0903cbaa026",
    ),
    (
        "core/raster/tests/reference_glyph_support/mod.rs",
        "58a0638de12a5dda589c7ba89fd2f2b75fe2abba25a9147913a23878c0ffab84",
    ),
    (
        "core/raster/tests/repository_policy.rs",
        "1c7a33103880daf9ce86b8fb969aa6c2aed808b9dec4f84732ba66b0b6852aea",
    ),
];

#[test]
fn m3_basic_text_review_is_exact_commit_tree_and_blob_bound() {
    let root = repository_root();
    let review = RootToml::parse(&read_text(
        &root,
        "docs/traceability/evidence/m3/basic-embedded-text/independent-review.toml",
    ))
    .expect("M3-09 independent review is strict TOML");

    review.expect_unsigned("schema", 1).expect("schema");
    review
        .expect_string("type", "work-item-evidence")
        .expect("type");
    review
        .expect_string("id", "evidence.m3.basic-embedded-text.independent-review")
        .expect("id");
    review.expect_string("milestone", "M3").expect("milestone");
    review
        .expect_string("work_item", "M3-09")
        .expect("work item");
    review
        .expect_string("profile", "m3.basic-embedded-text.v1")
        .expect("profile");
    review
        .expect_string("feature", "core.basic-embedded-text")
        .expect("feature");
    review
        .expect_string("role", "independent-review")
        .expect("role");
    review.expect_bool("registered", true).expect("registered");
    review.expect_bool("gating", true).expect("gating");
    review
        .expect_bool("external_observation", false)
        .expect("external observation");
    review
        .expect_bool("maturity_promotion", false)
        .expect("maturity promotion");
    review
        .expect_bare("reviewed_at", COMPLETED_AT)
        .expect("review date");
    review
        .expect_string("implementation_commit", IMPLEMENTATION_COMMIT)
        .expect("implementation commit");
    review
        .expect_string("reviewed_subject_commit", IMPLEMENTATION_COMMIT)
        .expect("subject commit");
    review
        .expect_string("reviewed_subject_tree", IMPLEMENTATION_TREE)
        .expect("subject tree");
    review
        .expect_string(
            "reviewed_subject_resolution",
            "git-tree-at-reviewed-subject-commit-not-working-tree",
        )
        .expect("subject resolution");
    review
        .expect_array("reviewer_roles", &["spec-conformance", "parser-security"])
        .expect("review roles");
    for priority in ["open_p0", "open_p1", "open_p2", "open_p0_p2"] {
        review
            .expect_unsigned(priority, 0)
            .unwrap_or_else(|error| panic!("{priority}: {error}"));
    }
    review.expect_string("verdict", "SHIP").expect("verdict");

    let expected_map = REVIEWED_SUBJECTS
        .iter()
        .map(|(path, _)| format!("{path}@{IMPLEMENTATION_COMMIT}"))
        .collect::<Vec<_>>();
    assert_eq!(
        review
            .array("reviewed_subject_commit_map")
            .expect("subject map"),
        expected_map
    );
    let expected_subjects = REVIEWED_SUBJECTS
        .iter()
        .map(|(path, hash)| format!("{path}@{IMPLEMENTATION_COMMIT}#sha256:{hash}"))
        .collect::<Vec<_>>();
    assert_eq!(
        review.array("reviewed_subjects").expect("subjects"),
        expected_subjects
    );
    assert_eq!(
        verify_reviewed_subjects(
            &root,
            &review,
            IMPLEMENTATION_COMMIT,
            Some(IMPLEMENTATION_TREE)
        )
        .expect("commit/tree/blob binding"),
        REVIEWED_SUBJECTS.len()
    );

    let scope = review.string("review_scope").expect("scope");
    for marker in [
        "M3-09 only",
        "simple TrueType",
        "reference-glyph-v1",
        "integrated reference-raster-v1",
        "O0/O1/O3",
        "maturity promotion",
        "M3 exit",
    ] {
        assert!(scope.contains(marker), "review scope is missing {marker}");
    }
}

#[test]
fn m3_basic_text_review_rejects_subject_hash_and_commit_rebinding() {
    let root = repository_root();
    let review_text = read_text(
        &root,
        "docs/traceability/evidence/m3/basic-embedded-text/independent-review.toml",
    );
    let wrong_hash = RootToml::parse(&review_text.replacen(
        REVIEWED_SUBJECTS[0].1,
        "0000000000000000000000000000000000000000000000000000000000000000",
        1,
    ))
    .expect("parseable mutation");
    assert!(
        verify_reviewed_subjects(
            &root,
            &wrong_hash,
            IMPLEMENTATION_COMMIT,
            Some(IMPLEMENTATION_TREE)
        )
        .is_err()
    );
    let first = format!(
        "{}@{IMPLEMENTATION_COMMIT}#sha256:{}",
        REVIEWED_SUBJECTS[0].0, REVIEWED_SUBJECTS[0].1
    );
    let rebound = format!(
        "{}@fe379fe1eb2ab5398f627a2db2835bcf41dc3bb0#sha256:{}",
        REVIEWED_SUBJECTS[0].0, REVIEWED_SUBJECTS[0].1
    );
    let rebound =
        RootToml::parse(&review_text.replacen(&first, &rebound, 1)).expect("parseable rebind");
    assert!(
        verify_reviewed_subjects(
            &root,
            &rebound,
            IMPLEMENTATION_COMMIT,
            Some(IMPLEMENTATION_TREE)
        )
        .is_err()
    );
}

#[test]
fn m3_basic_text_plan_feature_and_spec_links_are_exact() {
    let root = repository_root();
    let plan_text = read_text(&root, "plan/m3.toml");
    let feature_text = read_text(&root, "docs/traceability/feature-map.toml");
    let spec_text = read_text(&root, "docs/traceability/spec-map.toml");
    let capability_profiles = read_text(&root, "docs/traceability/capability-profiles.toml");

    let plan_root = RootToml::parse(&plan_text).expect("M3 plan TOML");
    assert_m3_plan_phase(&root, &plan_root);
    let item = table_record(&plan_text, "work_item", "M3-09");
    item.expect_string("title", "Basic embedded text and glyph coverage")
        .expect("title");
    item.expect_string("status", "complete").expect("status");
    item.expect_bare("completed_at", COMPLETED_AT)
        .expect("date");
    item.expect_array("depends_on", &["M3-03", "M3-05", "M3-07"])
        .expect("dependencies");
    item.expect_string("outcome", "The registered embedded simple-font subset produces deterministic positioned glyph outlines and Reference coverage.").expect("outcome");
    item.expect_array("acceptance", &[
        "PDF character mapping, text state, text matrices, showing operators, advances, and glyph provenance remain project-owned semantics.",
        "Tests use embedded project-owned fonts or analytic glyph fixtures and never depend on system fonts, shaping, hinting, or platform antialiasing.",
        "Font tables, glyph segments, recursion, retained bytes, and raster work are independently bounded.",
        "Missing or unsupported font features produce structured capability outcomes before pixel publication.",
    ]).expect("acceptance");
    let integrated = table_record(&plan_text, "work_item", "M3-10");
    integrated
        .expect_string("status", "complete")
        .expect("M3-10 status");
    integrated
        .expect_bare("completed_at", COMPLETED_AT)
        .expect("M3-10 completion");
    let exit = table_record(&plan_text, "work_item", "M3-11");
    exit.expect_string("status", "complete")
        .expect("M3-11 is complete in Candidate H");
    exit.expect_bare("completed_at", COMPLETED_AT)
        .expect("M3-11 completion date is exact");

    let feature_root = RootToml::parse(&feature_text).expect("feature TOML");
    feature_root
        .expect_string("version", TRACE_VERSION)
        .expect("feature version");
    let feature = table_record(&feature_text, "feature", "core.basic-embedded-text");
    feature
        .expect_string("owner", "graphics-color")
        .expect("owner");
    feature.expect_string("state", "PLANNED").expect("state");
    feature
        .expect_string("profile", "m3.basic-embedded-text.v1")
        .expect("profile");
    feature
        .expect_array(
            "modules",
            &[
                "core/font",
                "core/document",
                "core/content",
                "core/scene",
                "core/raster",
            ],
        )
        .expect("modules");
    for marker in [
        "ISO-32000-1:2008/7.8.3",
        "ISO-32000-1:2008/9.3",
        "ISO-32000-1:2008/9.4",
        "ISO-32000-1:2008/9.6.4",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/6.1-6.2",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/8.1-8.3",
        "RPE-ARCH-001/15.3/M3",
    ] {
        assert!(
            feature
                .array("clauses")
                .expect("clauses")
                .iter()
                .any(|clause| clause == marker),
            "missing feature clause {marker}"
        );
    }
    for marker in [
        "core/font::truetype",
        "core/document::font_resource",
        "core/content::vm_graphics",
        "core/scene::scene_v2",
        "core/raster::reference_glyph",
        "tools/quality::m3_basic_text_trace",
    ] {
        assert!(
            feature
                .array("tests")
                .expect("tests")
                .iter()
                .any(|test| test == marker),
            "missing feature test {marker}"
        );
    }
    assert!(
        !capability_profiles.contains("m3.basic-embedded-text.v1"),
        "M3-09 must not create a maturity profile"
    );

    RootToml::parse(&spec_text)
        .expect("spec TOML")
        .expect_string("version", TRACE_VERSION)
        .expect("spec version");
    for (id, snapshot, markers) in [
        (
            "ISO-32000-1:2008/7.8.2",
            ISO_SNAPSHOT,
            &[
                "M3-08 and M3-09 subsequently add",
                "composite or external fonts",
            ][..],
        ),
        (
            "ISO-32000-1:2008/7.8.3",
            ISO_SNAPSHOT,
            &["M3-09 Font resolver", "FontFile2"][..],
        ),
        (
            "ISO-32000-1:2008/9.3",
            ISO_SNAPSHOT,
            &[
                "q and Q operators save and restore",
                "BT resets only those two matrices",
            ][..],
        ),
        (
            "ISO-32000-1:2008/9.4",
            ISO_SNAPSHOT,
            &["text and line matrices", "TJ"][..],
        ),
        (
            "ISO-32000-1:2008/9.6.4",
            ISO_SNAPSHOT,
            &["simple TrueType", "hinting"][..],
        ),
        (
            "RPE-ARCH-001/5.8-5.9",
            ARCH_SNAPSHOT,
            &["Page Font lookup", "FontFile2"][..],
        ),
        (
            "RPE-ARCH-001/6.1-6.2",
            ARCH_SNAPSHOT,
            &["historical M2-06 mapping", "M3-08 and M3-09 then add"][..],
        ),
        (
            "RPE-ARCH-001/6.4-6.7",
            ARCH_SNAPSHOT,
            &["GlyphOutline", "not retroactively expanded"][..],
        ),
        (
            "RPE-ARCH-001/8.1-8.3",
            ARCH_SNAPSHOT,
            &[
                "reference-image-v1",
                "reference-glyph-v1",
                "six other linked component feature records remain PLANNED",
            ][..],
        ),
        (
            "RPE-ARCH-001/15.3/M3",
            ARCH_SNAPSHOT,
            &[
                "M3-09 closes",
                "first ten completed work items",
                "M3-10 closes",
                "M3-11 later closes",
                "All eleven work items are complete",
            ][..],
        ),
    ] {
        assert_requirement(&spec_text, id, snapshot, markers);
    }

    for (id, markers) in [
        (
            "ISO-32000-1:2008/8.4.2",
            &[
                "registered text-state parameters",
                "never saving the current path, text matrix, or text line matrix",
                "later integration work exists",
            ][..],
        ),
        (
            "ISO-32000-1:2008/8.4.3",
            &["M3-08 and M3-09 carry", "M3-10 accepts those transforms"][..],
        ),
        (
            "RPE-ARCH-001/6.4-6.7",
            &["M3-10 separately accepts", "M3-03 through M3-10"][..],
        ),
        (
            "RPE-ARCH-001/8.1-8.3",
            &[
                "M3-10 accepts one bounded strict-to-ReferenceRenderJob path",
                "Advanced fonts and text",
            ][..],
        ),
    ] {
        let requirement = table_record(&spec_text, "requirement", id);
        let notes = [
            requirement.string("notes").unwrap_or_default(),
            requirement.string("m3_image_notes").unwrap_or_default(),
            requirement.string("m3_text_notes").unwrap_or_default(),
        ]
        .join("\n");
        for marker in markers {
            assert!(notes.contains(marker), "{id} notes are missing {marker}");
        }
    }
    for contradiction in [
        "after M3-04, inline images, Forms, text showing, fonts, Image XObjects",
        "saved independently from graphics-state q/Q semantics",
        "so text state, ExtGState resources",
        "Inline images, Forms, text showing, font and image resources",
        "All three staged raster families remain outside ReferenceRenderJob until M3-10",
        "These staged kernels are not mounted into ReferenceRenderJob",
        "do not add glyphs, images",
        "All four features remain PLANNED",
        "Glyph, image, Form, and final raster mapping remain outside",
    ] {
        assert!(
            !spec_text.contains(contradiction),
            "spec-map retains stale contradiction {contradiction:?}"
        );
    }
}

#[test]
fn m3_basic_text_ci_and_provenance_preserve_scope_and_m1() {
    let root = repository_root();
    for (path, markers) in [
        (
            "core/font/PROVENANCE.md",
            &["TrueType", "project-owned"][..],
        ),
        ("core/document/PROVENANCE.md", &["Font", "FontFile2"][..]),
        ("core/content/PROVENANCE.md", &["text", "glyph"][..]),
        ("core/scene/PROVENANCE.md", &["glyph", "immutable"][..]),
        (
            "core/raster/PROVENANCE.md",
            &["reference-glyph-v1", "not an O0/O1 pixel authority"][..],
        ),
    ] {
        let text = read_text(&root, path);
        for marker in markers {
            assert!(text.contains(marker), "{path} is missing {marker}");
        }
    }
    let ci = read_text(&root, "scripts/ci.sh");
    let image = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_basic_image_trace",
    );
    let text = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_basic_text_trace",
    );
    let m2_replay = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$m2_scene_gate_root/debug-1\"",
    );
    assert!(image < text && text < m2_replay);
    let quality_main = read_text(&root, "tools/quality/src/main.rs");
    assert_eq!(quality_main.matches("m3-basic-text-trace").count(), 2);
    assert_eq!(
        file_sha256(&root.join("core/document/src/page_tree.rs")),
        M1_PAGE_TREE_HASH
    );
    assert_eq!(
        file_sha256(&root.join("core/document/tests/page_tree_count.rs")),
        M1_PAGE_TREE_TEST_HASH
    );
}

fn assert_requirement(document: &str, id: &str, snapshot: &str, note_markers: &[&str]) {
    let requirement = table_record(document, "requirement", id);
    requirement
        .expect_string("snapshot_hash", snapshot)
        .unwrap_or_else(|error| panic!("{id} snapshot: {error}"));
    requirement
        .expect_string("status", "partial")
        .unwrap_or_else(|error| panic!("{id} status: {error}"));
    let links = [
        requirement.array("features").expect("features").join("\n"),
        requirement
            .array("implementation")
            .expect("implementation")
            .join("\n"),
        requirement.array("tests").expect("tests").join("\n"),
    ]
    .join("\n");
    for required in [
        "core.basic-embedded-text",
        "tools/quality::m3_basic_text_trace",
    ] {
        assert!(links.contains(required), "{id} is missing {required}");
    }
    let notes = [
        requirement.string("notes").unwrap_or_default(),
        requirement.string("m3_text_notes").unwrap_or_default(),
    ]
    .join("\n");
    for marker in note_markers {
        assert!(notes.contains(marker), "{id} notes are missing {marker}");
    }
}

fn table_record(document: &str, table: &str, id: &str) -> RootToml {
    let records = array_table_records(document, table)
        .unwrap_or_else(|error| panic!("cannot parse [[{table}]]: {error}"));
    let mut matches = records
        .into_iter()
        .filter(|record| record.string("id").is_ok_and(|candidate| candidate == id));
    let record = matches
        .next()
        .unwrap_or_else(|| panic!("missing [[{table}]] {id}"));
    assert!(matches.next().is_none(), "duplicate [[{table}]] {id}");
    record
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
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
        .unwrap_or_else(|| panic!("missing marker {needle:?}"))
}

fn file_sha256(path: &Path) -> String {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let digest =
        sha256(&bytes).unwrap_or_else(|error| panic!("cannot hash {}: {error}", path.display()));
    hex_digest(&digest)
}
