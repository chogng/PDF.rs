use std::fs;
use std::path::{Path, PathBuf};

use pdf_rs_digest::{hex_digest, sha256};

#[path = "support/evidence.rs"]
mod evidence;

use evidence::{RootToml, array_table_records, verify_reviewed_subjects};

const TRACE_VERSION: &str = "0.78.0";
const COMPLETED_AT: &str = "2026-07-16";
const IMPLEMENTATION_COMMIT: &str = "127ee3cac4ab5595abbecd6438e6e309a58b23cb";
const IMPLEMENTATION_TREE: &str = "c5e3ba0d5cd5e5de7b30845bb177e196ec8655b7";
const ISO_SNAPSHOT: &str =
    "sha256:9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff";
const ARCH_SNAPSHOT: &str =
    "sha256:53d46023770b4558705cc00f779fb3031245d473378d82869875283913157541";
const M1_PAGE_TREE_HASH: &str = "e680abd131a3a4da61262eb152820c3e4f6252c6396a15447039713da3a0f5e1";
const M1_PAGE_TREE_TEST_HASH: &str =
    "aa8f4bbb5c4475d62a29a0cce3e8f798b17ea606e185b8b97017c2bc25e14374";

const REVIEWED_SUBJECTS: [(&str, &str); 10] = [
    (
        "core/raster/PROVENANCE.md",
        "1d0697c25e05f360d94cf41247b7cb82e41200bb5b2a18514873ef41bfe01475",
    ),
    (
        "core/raster/src/reference/color.rs",
        "66c13e663d41e3b12dfa81f7e5d57feb383da17ec12a804a0961578edf1e10b8",
    ),
    (
        "core/raster/src/reference/error.rs",
        "790c88e9e44473d5fae003e7abf249cd84efda1b113fa09d26cd5cbfc0daa6cc",
    ),
    (
        "core/raster/src/reference/mod.rs",
        "1a2e44993f84d9cd4c795c76b5cda543217ac1b7efdc6a69112b753fbe4326d4",
    ),
    (
        "core/raster/src/reference/render.rs",
        "ee6a03c1b84d057d30f4a1d3e4170a11fe8fff050c64d93b6d0b48c3886ad3b3",
    ),
    (
        "core/raster/tests/reference_color.rs",
        "55923ce8b597113aa7392b6535764bdef2179c73a18db55aed6492ae9df8073d",
    ),
    (
        "core/raster/tests/reference_scene_v2_boundary.rs",
        "7c54a456bf8d9ec5cce7c7cfee97a491bbd029ac1eec849c3853bca4478dd24b",
    ),
    (
        "core/scene/src/canonical.rs",
        "6afc284c454f99ebec04f69b32e05e463d1103fd682d99f78605a4bf5eb52179",
    ),
    (
        "core/scene/src/graphics.rs",
        "75355cc04e50ec9547cc834c80d2137dcc4de1e3b01ca5562db65888ada6602c",
    ),
    (
        "core/scene/tests/scene_v2.rs",
        "a65ae893520b047cc89b578c3ea2f9c7fd848022680da13b22386ce2433650da",
    ),
];

const REVIEW_COMMANDS: [&str; 12] = [
    "cargo test --locked --package pdf-rs-raster --test reference_color",
    "cargo test --locked --release --package pdf-rs-raster --test reference_color",
    "cargo test --locked --package pdf-rs-raster --test reference_scene_v2_boundary",
    "cargo test --locked --release --package pdf-rs-raster --test reference_scene_v2_boundary",
    "cargo test --locked --package pdf-rs-scene --all-targets",
    "cargo test --locked --release --package pdf-rs-scene --all-targets",
    "cargo test --locked --package pdf-rs-raster --all-targets",
    "cargo test --locked --release --package pdf-rs-raster --all-targets",
    "cargo clippy --locked --package pdf-rs-raster --package pdf-rs-scene --all-targets -- -D warnings",
    "cargo fmt --all -- --check",
    "git show --check 127ee3c",
    "git diff-tree --check 127ee3c^ 127ee3c",
];

#[test]
fn m3_color_review_is_exact_commit_tree_and_blob_bound() {
    let root = repository_root();
    let review = RootToml::parse(&read_text(
        &root,
        "docs/traceability/evidence/m3/reference-color-compositing/independent-review.toml",
    ))
    .expect("M3-07 independent review is strict TOML");

    review
        .expect_unsigned("schema", 1)
        .expect("schema is exact");
    review
        .expect_string("type", "work-item-evidence")
        .expect("type is exact");
    review
        .expect_string(
            "id",
            "evidence.m3.reference-color-compositing.independent-review",
        )
        .expect("id is exact");
    review
        .expect_string("milestone", "M3")
        .expect("milestone is exact");
    review
        .expect_string("work_item", "M3-07")
        .expect("work item is exact");
    review
        .expect_string("profile", "m3.reference-color-compositing.v1")
        .expect("profile is exact");
    review
        .expect_string("feature", "core.reference-color-compositing")
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
        .expect_array("commands", &REVIEW_COMMANDS)
        .expect("review commands are exact");
    for priority in ["open_p0", "open_p1", "open_p2", "open_p0_p2"] {
        review
            .expect_unsigned(priority, 0)
            .unwrap_or_else(|error| panic!("{priority}: {error}"));
    }
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
            .expect("review commit map is typed"),
        expected_map
    );
    let expected_subjects = REVIEWED_SUBJECTS
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
            &root,
            &review,
            IMPLEMENTATION_COMMIT,
            Some(IMPLEMENTATION_TREE),
        )
        .expect("reviewed subjects are commit/tree/blob bound"),
        REVIEWED_SUBJECTS.len()
    );

    let scope = review
        .string("review_scope")
        .expect("review scope is typed");
    for marker in [
        "M3-07 only",
        "Scene soft-mask capability",
        "images",
        "integrated ReferenceRenderJob",
        "O0/O1/O3",
        "M3 exit",
    ] {
        assert!(scope.contains(marker), "review scope is missing {marker}");
    }
}

#[test]
fn m3_color_review_rejects_subject_hash_and_commit_rebinding() {
    let root = repository_root();
    let review = read_text(
        &root,
        "docs/traceability/evidence/m3/reference-color-compositing/independent-review.toml",
    );

    let wrong_hash = review.replacen(
        REVIEWED_SUBJECTS[0].1,
        "0000000000000000000000000000000000000000000000000000000000000000",
        1,
    );
    let wrong_hash = RootToml::parse(&wrong_hash).expect("mutated hash review remains parseable");
    assert!(
        verify_reviewed_subjects(
            &root,
            &wrong_hash,
            IMPLEMENTATION_COMMIT,
            Some(IMPLEMENTATION_TREE),
        )
        .is_err(),
        "a substituted subject hash must fail closed"
    );

    let first_subject = format!(
        "{}@{IMPLEMENTATION_COMMIT}#sha256:{}",
        REVIEWED_SUBJECTS[0].0, REVIEWED_SUBJECTS[0].1
    );
    let rebound_subject = format!(
        "{}@213652f80d9d8c6749102f0aa7d6d53163e5ac3c#sha256:{}",
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
        .is_err(),
        "review subjects cannot be rebound away from the reviewed commit"
    );
}

#[test]
fn m3_color_plan_feature_and_spec_links_are_exact() {
    let root = repository_root();
    let plan_text = read_text(&root, "plan/m3.toml");
    let feature_text = read_text(&root, "docs/traceability/feature-map.toml");
    let spec_text = read_text(&root, "docs/traceability/spec-map.toml");
    let capability_profiles = read_text(&root, "docs/traceability/capability-profiles.toml");

    let plan_root = RootToml::parse(&plan_text).expect("M3 plan root is strict TOML");
    assert_m3_plan_phase(&root, &plan_root);
    let item = table_record(&plan_text, "work_item", "M3-07");
    item.expect_string("title", "Base color and alpha compositing")
        .expect("M3-07 title is exact");
    item.expect_string("status", "complete")
        .expect("M3-07 is complete");
    item.expect_bare("completed_at", COMPLETED_AT)
        .expect("M3-07 completion date is exact");
    item.expect_array("depends_on", &["M3-06"])
        .expect("M3-07 dependency is exact");
    item.expect_string(
        "outcome",
        "Reference pixels implement the registered device colors, constant alpha, and basic blend modes with deterministic conversion.",
    )
    .expect("M3-07 outcome is exact");
    item.expect_array(
        "acceptance",
        &[
            "DeviceGray, DeviceRGB, and DeviceCMYK conversion is versioned and checked without platform color management.",
            "Internal premultiplied-alpha arithmetic and straight-alpha RGBA8 publication use fixed rounding rules.",
            "Normal, Multiply, and Screen have independently derived channel and layered-shape tests.",
            "Unsupported color spaces, masks, groups, and blend modes produce structured capability outcomes rather than missing pixels.",
        ],
    )
    .expect("M3-07 acceptance is exact");
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
    let feature = table_record(&feature_text, "feature", "core.reference-color-compositing");
    feature
        .expect_string("owner", "graphics-color")
        .expect("feature owner is exact");
    feature
        .expect_string("state", "PLANNED")
        .expect("feature remains PLANNED");
    feature
        .expect_string("profile", "m3.reference-color-compositing.v1")
        .expect("feature profile is exact");
    feature
        .expect_array(
            "clauses",
            &[
                "ISO-32000-1:2008/8.6",
                "ISO-32000-1:2008/11.3.2-11.3.4",
                "RPE-ARCH-001/6.4-6.7",
                "RPE-ARCH-001/8.1-8.3",
                "RPE-ARCH-001/15.3/M3",
                "RPE-STD-001/3",
                "RPE-STD-001/5-11",
                "RPE-STD-003/8-9",
                "RPE-STD-003/12",
                "RPE-STD-005/4-7",
                "RPE-STD-005/11",
            ],
        )
        .expect("feature clauses are exact");
    feature
        .expect_array("modules", &["core/raster"])
        .expect("feature modules are exact");
    feature
        .expect_array(
            "tests",
            &[
                "core/raster::reference_color",
                "core/raster::reference_scene_v2_boundary",
                "core/raster::repository_policy",
                "tools/quality::m3_reference_color_trace",
            ],
        )
        .expect("feature tests are exact");
    feature
        .expect_array("fuzz_targets", &[])
        .expect("feature fuzz targets remain empty");
    feature
        .expect_array("benchmarks", &[])
        .expect("feature benchmarks remain empty");
    assert!(
        !capability_profiles.contains("m3.reference-color-compositing.v1"),
        "M3-07 must not create a maturity profile"
    );

    let spec_root = RootToml::parse(&spec_text).expect("spec map root is strict TOML");
    spec_root
        .expect_string("version", TRACE_VERSION)
        .expect("spec-map version is exact");
    assert_requirement(
        &spec_text,
        "ISO-32000-1:2008/8.6",
        ISO_SNAPSHOT,
        &[
            "core.reference-color-compositing",
            "core/raster",
            "core/raster::reference_color",
            "core/raster::reference_scene_v2_boundary",
            "tools/quality::m3_reference_color_trace",
        ],
        &[
            "reference-color-v1",
            "RGB = 1 - min(1, CMY + K)",
            "unsupported color requirements fail structurally",
        ],
    );
    assert_requirement(
        &spec_text,
        "ISO-32000-1:2008/11.3.2-11.3.4",
        ISO_SNAPSHOT,
        &[
            "core.reference-color-compositing",
            "core/raster",
            "core/raster::reference_color",
            "core/raster::reference_scene_v2_boundary",
            "tools/quality::m3_reference_color_trace",
        ],
        &[
            "premultiplied project-sRGB Q16",
            "Normal, Multiply, and Screen",
            "3x3 layered-shape",
            "Soft masks and groups",
        ],
    );
    for (id, note_markers) in [
        (
            "RPE-ARCH-001/6.4-6.7",
            &[
                "reference-color-v1",
                "soft-mask",
                "M3-10 separately accepts",
            ][..],
        ),
        (
            "RPE-ARCH-001/8.1-8.3",
            &[
                "m3.reference-color-compositing.v1",
                "transparent-black canonicalization",
                "M3-10 accepts one bounded strict-to-ReferenceRenderJob path",
            ][..],
        ),
        (
            "RPE-ARCH-001/15.3/M3",
            &[
                "M3-07 closes project-owned",
                "M3-08 closes",
                "M3-10 closes",
                "M3-11 later closes",
                "final independent SHIP review",
            ][..],
        ),
    ] {
        assert_requirement(
            &spec_text,
            id,
            ARCH_SNAPSHOT,
            &[
                "core.reference-color-compositing",
                "core/raster",
                "core/raster::reference_color",
                "core/raster::reference_scene_v2_boundary",
                "tools/quality::m3_reference_color_trace",
            ],
            note_markers,
        );
    }
}

#[test]
fn m3_color_provenance_and_ci_gate_preserve_scope_and_m1() {
    let root = repository_root();
    let provenance = read_text(&root, "core/raster/PROVENANCE.md");
    for marker in [
        "`m3.reference-color-compositing.v1` remains `PLANNED`",
        "`reference-color-v1`",
        "`sRGB-reference-v1`",
        "`reference-raster-v1`",
        "RGB = 1 - min(1, CMY + K)",
        "premultiplied project-sRGB Q16",
        "Normal, Multiply, and Screen",
        "allocation-free",
        "transparent black",
        "capability outcomes before the mounted color kernel",
        "M3-10 uses that same profile",
        "not a `REFERENCE` maturity promotion",
        "not an O0/O1 pixel authority",
        "M3 exit decision",
    ] {
        assert!(
            provenance.contains(marker),
            "Reference provenance is missing {marker:?}"
        );
    }

    let workflow = read_text(&root, ".github/workflows/ci.yml");
    assert!(workflow.contains("uses: actions/checkout@v4"));
    assert!(workflow.contains("fetch-depth: 0"));

    let ci = read_text(&root, "scripts/ci.sh");
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
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$m2_scene_gate_root/debug-1\"",
    );
    assert!(content < geometry);
    assert!(geometry < color);
    assert!(color < m2_replay);

    let quality_main = read_text(&root, "tools/quality/src/main.rs");
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

fn assert_requirement(
    document: &str,
    id: &str,
    snapshot: &str,
    required_links: &[&str],
    note_markers: &[&str],
) {
    let requirement = table_record(document, "requirement", id);
    requirement
        .expect_string("snapshot_hash", snapshot)
        .unwrap_or_else(|error| panic!("{id} snapshot: {error}"));
    requirement
        .expect_string("status", "partial")
        .unwrap_or_else(|error| panic!("{id} status: {error}"));
    let joined = [
        requirement
            .array("features")
            .expect("requirement features are typed")
            .join("\n"),
        requirement
            .array("implementation")
            .expect("requirement implementation is typed")
            .join("\n"),
        requirement
            .array("tests")
            .expect("requirement tests are typed")
            .join("\n"),
    ]
    .join("\n");
    for required in required_links {
        assert!(joined.contains(required), "{id} is missing {required}");
    }
    let notes = requirement
        .string("notes")
        .expect("requirement notes are typed");
    for marker in note_markers {
        assert!(notes.contains(marker), "{id} notes are missing {marker}");
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
