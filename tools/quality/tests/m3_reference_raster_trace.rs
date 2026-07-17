use std::fs;
use std::path::{Path, PathBuf};

use pdf_rs_digest::{hex_digest, sha256};

#[path = "support/evidence.rs"]
mod evidence;

use evidence::{RootToml, array_table_records, verify_reviewed_subjects};

const TRACE_VERSION: &str = "0.78.0";
const COMPLETED_AT: &str = "2026-07-16";
const IMPLEMENTATION_COMMIT: &str = "a917aee672ce6be294e479956d5be87c835d0507";
const IMPLEMENTATION_TREE: &str = "e3f9a6f98e61afc9034df02b24fd2f3c293a97a8";
const ISO_SNAPSHOT: &str =
    "sha256:9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff";
const ARCH_SNAPSHOT: &str =
    "sha256:53d46023770b4558705cc00f779fb3031245d473378d82869875283913157541";
const M1_PAGE_TREE_HASH: &str = "e680abd131a3a4da61262eb152820c3e4f6252c6396a15447039713da3a0f5e1";
const M1_PAGE_TREE_TEST_HASH: &str =
    "aa8f4bbb5c4475d62a29a0cce3e8f798b17ea606e185b8b97017c2bc25e14374";

const REVIEWED_SUBJECTS: [(&str, &str); 9] = [
    (
        "Cargo.lock",
        "997a0ae69da189fb7475724ff2368606cc16eb419f36898b846aac1de3e0830d",
    ),
    (
        "tools/quality/Cargo.toml",
        "65390a314dbf781257ba92bfbc35e43085e0e0c8ea09b91b8394b6032cbbec88",
    ),
    (
        "tools/quality/tests/m3_reference_gate.rs",
        "fc91da374f2f61057c24324bf5526239e54d828d95b83ff4c33f37a66d1448b2",
    ),
    (
        "tools/quality/tests/m3_reference_gate_support/artifact.rs",
        "800df8ab5035c6f03b7ce7c2af2a243c50ac36427f9e1fd2c4bd40f807d90e6f",
    ),
    (
        "tools/quality/tests/m3_reference_gate_support/fixture.rs",
        "ffa3e76b1624566dd2572161f0df32d61897d8078ae4ee8b03561e3daf355b78",
    ),
    (
        "tools/quality/tests/m3_reference_gate_support/mod.rs",
        "d7202c0f220b5251918feeb8391308ba5b6a11275d689e73d38aeb4dfc3336cf",
    ),
    (
        "tools/quality/tests/m3_reference_gate_support/pending.rs",
        "6ce2caaf2672db9e8fb9088d45c32204c51cc43369ef170f1454c22ee18df8bb",
    ),
    (
        "scripts/ci.sh",
        "222adac1c5040c28b5daa521facd70de5e66dba3849c5f94ca36c3af86794c77",
    ),
    (
        "tools/quality/tests/m2_exit.rs",
        "a66079ec16fff80888a61c1db14000060dd183b1b5f9d7839747b250b680b5cd",
    ),
];

const GRAPHICS_CLAUSES: [&str; 14] = [
    "ISO-32000-1:2008/8.4.2",
    "ISO-32000-1:2008/8.4.3",
    "ISO-32000-1:2008/8.5",
    "ISO-32000-1:2008/8.6",
    "ISO-32000-1:2008/8.9",
    "ISO-32000-1:2008/9.3",
    "ISO-32000-1:2008/9.4",
    "ISO-32000-1:2008/9.6.4",
    "ISO-32000-1:2008/11.3.2-11.3.4",
    "RPE-ARCH-001/5.8-5.9",
    "RPE-ARCH-001/6.1-6.2",
    "RPE-ARCH-001/6.4-6.7",
    "RPE-ARCH-001/8.1-8.3",
    "RPE-ARCH-001/15.3/M3",
];

#[test]
fn m3_reference_review_is_exact_commit_tree_and_blob_bound() {
    let root = repository_root();
    let review = RootToml::parse(&read_text(
        &root,
        "docs/traceability/evidence/m3/reference-raster-integration/independent-review.toml",
    ))
    .expect("M3-10 independent review is strict TOML");

    review.expect_unsigned("schema", 1).expect("schema");
    review
        .expect_string("type", "work-item-evidence")
        .expect("type");
    review
        .expect_string(
            "id",
            "evidence.m3.reference-raster-integration.independent-review",
        )
        .expect("id");
    review.expect_string("milestone", "M3").expect("milestone");
    review
        .expect_string("work_item", "M3-10")
        .expect("work item");
    review
        .expect_string("profile", "m3.reference-raster-v1.v1")
        .expect("profile");
    review
        .expect_string("feature", "core.reference-raster-v1")
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
        .expect("reviewer roles");
    for priority in ["open_p0", "open_p1", "open_p2", "open_p0_p2"] {
        review
            .expect_unsigned(priority, 0)
            .unwrap_or_else(|error| panic!("{priority}: {error}"));
    }
    review.expect_string("verdict", "SHIP").expect("verdict");

    let commit_map = REVIEWED_SUBJECTS
        .iter()
        .map(|(path, _)| format!("{path}@{IMPLEMENTATION_COMMIT}"))
        .collect::<Vec<_>>();
    assert_eq!(
        review
            .array("reviewed_subject_commit_map")
            .expect("commit map"),
        commit_map
    );
    let subjects = REVIEWED_SUBJECTS
        .iter()
        .map(|(path, hash)| format!("{path}@{IMPLEMENTATION_COMMIT}#sha256:{hash}"))
        .collect::<Vec<_>>();
    assert_eq!(
        review.array("reviewed_subjects").expect("subjects"),
        subjects
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
        "M3-10 only",
        "ReferenceRenderJob",
        "M3-11 O0/O1/O3",
        "REFERENCE maturity promotion",
        "M3 exit",
    ] {
        assert!(scope.contains(marker), "review scope is missing {marker}");
    }
}

#[test]
fn m3_reference_review_rejects_hash_and_commit_rebinding() {
    let root = repository_root();
    let review_text = read_text(
        &root,
        "docs/traceability/evidence/m3/reference-raster-integration/independent-review.toml",
    );
    let wrong_hash = RootToml::parse(&review_text.replacen(
        REVIEWED_SUBJECTS[0].1,
        "0000000000000000000000000000000000000000000000000000000000000000",
        1,
    ))
    .expect("parseable hash mutation");
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
        "{}@e54b16e609e8fe3b47bc4a3617cf65da54d168dc#sha256:{}",
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
fn m3_reference_plan_feature_and_spec_links_are_exact() {
    let root = repository_root();
    let plan_text = read_text(&root, "plan/m3.toml");
    let feature_text = read_text(&root, "docs/traceability/feature-map.toml");
    let spec_text = read_text(&root, "docs/traceability/spec-map.toml");
    let profiles = read_text(&root, "docs/traceability/capability-profiles.toml");
    let final_review = root
        .join("docs/traceability/evidence/m3/reference-raster-gate/independent-review.toml")
        .is_file();

    let plan_root = RootToml::parse(&plan_text).expect("M3 plan TOML");
    assert_m3_plan_phase(&root, &plan_root);
    let integrated = table_record(&plan_text, "work_item", "M3-10");
    integrated
        .expect_string("title", "Integrated reference-raster-v1 renderer")
        .expect("title");
    integrated
        .expect_string("status", "complete")
        .expect("status");
    integrated
        .expect_bare("completed_at", COMPLETED_AT)
        .expect("completion date");
    integrated
        .expect_array("depends_on", &["M3-02", "M3-06", "M3-07", "M3-08", "M3-09"])
        .expect("dependencies");
    let exit = table_record(&plan_text, "work_item", "M3-11");
    exit.expect_string("status", "complete")
        .expect("M3-11 is complete in Candidate H");
    exit.expect_bare("completed_at", COMPLETED_AT)
        .expect("M3-11 completion date is exact");

    let feature_root = RootToml::parse(&feature_text).expect("feature map TOML");
    feature_root
        .expect_string("version", TRACE_VERSION)
        .expect("feature version");
    let feature = table_record(&feature_text, "feature", "core.reference-raster-v1");
    feature
        .expect_string("owner", "graphics-color")
        .expect("owner");
    feature
        .expect_string("state", "REFERENCE")
        .expect("selected integrated feature is REFERENCE");
    feature
        .expect_string("profile", "m3.reference-raster-v1.v1")
        .expect("profile");
    feature
        .expect_array("clauses", &GRAPHICS_CLAUSES)
        .expect("graphics clauses");
    feature
        .expect_array(
            "modules",
            &[
                "core/font",
                "core/document",
                "core/content",
                "core/scene",
                "core/raster",
                "tools/quality",
            ],
        )
        .expect("modules");
    for test in [
        "core/font::truetype",
        "core/document::image_xobject",
        "core/document::font_resource",
        "core/content::vm_graphics",
        "core/scene::scene_v2",
        "core/raster::reference_geometry_kernel",
        "core/raster::reference_color",
        "core/raster::reference_image",
        "core/raster::reference_glyph",
        "core/raster::reference_integrated_renderer",
        "tools/quality::m3_reference_gate",
        "tools/quality::m3_reference_oracle_model",
        "tools/quality::m3_reference_raster_trace",
        "tools/quality::m3_exit",
    ] {
        assert!(
            feature
                .array("tests")
                .expect("feature tests")
                .iter()
                .any(|candidate| candidate == test),
            "feature is missing {test}"
        );
    }
    assert!(
        profiles.contains("m3.reference-raster-v1.v1"),
        "M3-10 did not promote the profile, but M3-11 must register the selected REFERENCE profile"
    );

    RootToml::parse(&spec_text)
        .expect("spec map TOML")
        .expect_string("version", TRACE_VERSION)
        .expect("spec version");
    if final_review {
        for stale in [
            "M3 milestone awaits the final independent SHIP review",
            "M3 milestone still awaits the final independent SHIP review",
        ] {
            assert!(
                !spec_text.contains(stale),
                "completed M3 trace notes retain stale closure text {stale:?}"
            );
        }
    }
    for id in GRAPHICS_CLAUSES {
        let requirement = table_record(&spec_text, "requirement", id);
        requirement
            .expect_string(
                "snapshot_hash",
                if id.starts_with("ISO-") {
                    ISO_SNAPSHOT
                } else {
                    ARCH_SNAPSHOT
                },
            )
            .unwrap_or_else(|error| panic!("{id} snapshot: {error}"));
        let expected_status = if id == "RPE-ARCH-001/15.3/M3" && final_review {
            "covered"
        } else {
            "partial"
        };
        requirement
            .expect_string("status", expected_status)
            .unwrap_or_else(|error| panic!("{id} status: {error}"));
        assert!(
            requirement
                .array("features")
                .expect("features")
                .iter()
                .any(|candidate| candidate == "core.reference-raster-v1"),
            "{id} is missing the integrated feature"
        );
        for test in [
            "core/raster::reference_integrated_renderer",
            "tools/quality::m3_reference_gate",
            "tools/quality::m3_reference_oracle_model",
            "tools/quality::m3_reference_raster_trace",
            "tools/quality::m3_exit",
        ] {
            assert!(
                requirement
                    .array("tests")
                    .expect("tests")
                    .iter()
                    .any(|candidate| candidate == test),
                "{id} is missing {test}"
            );
        }
    }

    for (id, markers) in [
        (
            "RPE-ARCH-001/6.4-6.7",
            &[
                "M3-10 separately accepts",
                "M3-11 later closes registered O0/O1/O3 pixel authority",
                "All other M2 and M3 component feature records remain PLANNED",
                "final independent SHIP review",
            ][..],
        ),
        (
            "RPE-ARCH-001/8.1-8.3",
            &[
                "M3-10 accepts one bounded strict-to-ReferenceRenderJob path",
                "M3-10 itself did not promote it",
                "M3-11 later closes registered O0/O1/O3 pixel authority",
                "six other linked component feature records remain PLANNED",
                "final independent SHIP review",
            ][..],
        ),
        (
            "RPE-ARCH-001/15.3/M3",
            &[
                "M3-10 closes",
                "first ten completed work items",
                "M3-10 itself did not promote the profile",
                "registered formal O0/O1/O3 pixel authority",
                "All eleven work items are complete",
                "final independent SHIP review",
            ][..],
        ),
    ] {
        let notes = table_record(&spec_text, "requirement", id)
            .string("notes")
            .expect("notes")
            .to_owned();
        for marker in markers {
            assert!(notes.contains(marker), "{id} notes are missing {marker}");
        }
    }
}

#[test]
fn m3_reference_ci_and_provenance_preserve_scope_and_m1() {
    let root = repository_root();
    let ci = read_text(&root, "scripts/ci.sh");
    let gate = position(
        &ci,
        "PDF_RS_M3_REFERENCE_GATE_OUTPUT=\"$PWD/$m3_reference_gate_root/debug-1\"",
    );
    let trace = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_reference_raster_trace",
    );
    let maturity = position(
        &ci,
        "validate-m1-maturity docs/traceability/capability-profiles.toml",
    );
    assert!(gate < trace && trace < maturity);

    let quality_main = read_text(&root, "tools/quality/src/main.rs");
    assert_eq!(quality_main.matches("m3-reference-gate").count(), 2);
    assert_eq!(quality_main.matches("m3-reference-raster-trace").count(), 2);

    let provenance = read_text(&root, "tools/quality/PROVENANCE.md");
    for marker in [
        "M3-10 integrated Reference gate",
        "`m3.reference-raster-v1.v1`",
        IMPLEMENTATION_COMMIT,
        IMPLEMENTATION_TREE,
        "M3-11",
        "not a REFERENCE maturity promotion",
    ] {
        assert!(
            provenance.contains(marker),
            "quality provenance is missing {marker}"
        );
    }

    assert_eq!(
        file_sha256(&root.join("core/document/src/page_tree.rs")),
        M1_PAGE_TREE_HASH
    );
    assert_eq!(
        file_sha256(&root.join("core/document/tests/page_tree_count.rs")),
        M1_PAGE_TREE_TEST_HASH
    );
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
        .unwrap_or_else(|| panic!("missing {needle}"))
}

fn file_sha256(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("cannot read {path:?}: {error}"));
    hex_digest(&sha256(&bytes).expect("SHA-256"))
}
