use std::fs;
use std::path::{Path, PathBuf};

use pdf_rs_digest::{hex_digest, sha256};

#[path = "support/evidence.rs"]
mod evidence;

use evidence::{RootToml, array_table_records, verify_reviewed_subjects};

const TRACE_VERSION: &str = "0.77.0";
const COMPLETED_AT: &str = "2026-07-16";
const IMPLEMENTATION_COMMIT: &str = "fe379fe1eb2ab5398f627a2db2835bcf41dc3bb0";
const IMPLEMENTATION_TREE: &str = "8a314214f5abe7c0eca0354ef7c616356966ac77";
const ISO_SNAPSHOT: &str =
    "sha256:9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff";
const ARCH_SNAPSHOT: &str =
    "sha256:53d46023770b4558705cc00f779fb3031245d473378d82869875283913157541";
const M1_PAGE_TREE_HASH: &str = "e680abd131a3a4da61262eb152820c3e4f6252c6396a15447039713da3a0f5e1";
const M1_PAGE_TREE_TEST_HASH: &str =
    "aa8f4bbb5c4475d62a29a0cce3e8f798b17ea606e185b8b97017c2bc25e14374";

const REVIEWED_SUBJECTS: [(&str, &str); 38] = [
    (
        "core/document/src/page_resources.rs",
        "827092830eb3ab2afb552a71a26f901bb2739561f8283f5bf4027bb5c233e8e1",
    ),
    (
        "core/document/src/page_xobject_lookup_limits.rs",
        "ccdb56714e3a72fc751e8e52338eef5611d258e83bf1cd172a0cd3a22e8a6bcb",
    ),
    (
        "core/document/src/image_xobject.rs",
        "1fb42079194eb9e94fbfdbb4e27746ee211e17b92a0fd624b130f3bedb249d78",
    ),
    (
        "core/document/src/image_xobject_limits.rs",
        "aa1ff2a5903e8cad44dbf36b7d07206618e246108c45a6546466b50ba71e9eca",
    ),
    (
        "core/document/src/error.rs",
        "dfda17003acc14f39dd2b256521c0ef30f38416bd84a1d4b0eac87e9f8bbde26",
    ),
    (
        "core/document/src/lib.rs",
        "dc076e36c5d3e251947cb4faf571c4f117f296afc1e65e271b67c751f95021e1",
    ),
    (
        "core/document/tests/image_xobject.rs",
        "5095c471a3e98349da5415e0b2997c135b17dfc931b9c1a000c3c10c824042cc",
    ),
    (
        "core/content/PROVENANCE.md",
        "1950aa0a84b38bdc232ea96742fed2b2e2d7580398d36b0fa093cd3b91fffc5b",
    ),
    (
        "core/content/src/image_limits.rs",
        "81e7d8b4dc5249b0fc5d8610eb856d3bd358d062d5544b5732aaaadd29fbc9ed",
    ),
    (
        "core/content/src/lib.rs",
        "dc5ada5c9d7642cfc68ab8fd54661a0e1a77e0a23e0d053ebdbc5a6ef28c6cfc",
    ),
    (
        "core/content/src/model.rs",
        "9e7812c947180621051e699be1a7e41bf701fc5846d56ba5ac7cfc02640a08cb",
    ),
    (
        "core/content/src/vm.rs",
        "214855e2e75839dfeca6c406a0915d97ae3508d8f81660b60769d6f6c2de7119",
    ),
    (
        "core/content/src/vm/image.rs",
        "651f54120f20e9e6c8fa84382dd5d57d7a59a7e7ae0a8a61314f1181f2eb722f",
    ),
    (
        "core/content/src/vm/graphics.rs",
        "3adbd580dfe3d071eff2f6aa6f604dd022186ee793bdce5128a21262c376460b",
    ),
    (
        "core/content/src/vm_error.rs",
        "c59a8e80b0d0434df001ee2b2047f96fc9feb5225dd0f66c4081b73fe3f6aa7b",
    ),
    (
        "core/content/src/vm_model.rs",
        "6f47763d1836627b258b5d584c259a59e2adf945e2d7cf050f93f497d1ca0b44",
    ),
    (
        "core/content/tests/scanner.rs",
        "8a8d293d7ad493d25ef75060bccef64fc9ff3f69addc73fe1e96b6f3d015db67",
    ),
    (
        "core/content/tests/vm.rs",
        "da036e73a3f54518f2d859ac303590ad7ee0e9650ca4db3ac7bdd19fe76d1b92",
    ),
    (
        "core/content/tests/vm_foundation.rs",
        "8ade6155db6995dbb8dff74ba120a5198cb3c052584b672e66a50dc41b7f959d",
    ),
    (
        "core/content/tests/vm_graphics.rs",
        "42b18fa15fecfa37f18ae77c8ef4bc7ee4c64f6183bffbe6e05180110cfd9edf",
    ),
    (
        "core/scene/src/graphics.rs",
        "75355cc04e50ec9547cc834c80d2137dcc4de1e3b01ca5562db65888ada6602c",
    ),
    (
        "core/scene/src/graphics_builder.rs",
        "4f93cbc3cc71b4c3178575092d9b9be1af1425ddb8d5c894de65b3b09237f246",
    ),
    (
        "core/scene/src/canonical.rs",
        "6afc284c454f99ebec04f69b32e05e463d1103fd682d99f78605a4bf5eb52179",
    ),
    (
        "core/scene/tests/scene_v2.rs",
        "a65ae893520b047cc89b578c3ea2f9c7fd848022680da13b22386ce2433650da",
    ),
    (
        "core/raster/src/reference/image.rs",
        "781afc40d9aa20f15a026af1b3613a2ba116248f6448a64893188e69417a2dfd",
    ),
    (
        "core/raster/src/reference/geometry.rs",
        "edf9562b1965de73835283ea8eb0181515bf716484cc0fb08c07fae5755c5ad3",
    ),
    (
        "core/raster/src/reference/coverage.rs",
        "a43e2ad1be3d27789a17ce2ec3a7671938cd6340438c8b1525701f9968d35139",
    ),
    (
        "core/raster/src/reference/color.rs",
        "66c13e663d41e3b12dfa81f7e5d57feb383da17ec12a804a0961578edf1e10b8",
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
        "core/raster/tests/reference_image.rs",
        "c9b024e831f6fa7aa9aedbd753337155e6ef919ad54616e423fe3cf00bb285b4",
    ),
    (
        "core/raster/tests/reference_image_support/mod.rs",
        "00521467802cbe6e339de22af09c4702e2046e3dfeaf3acc01f0d29aac539dae",
    ),
    (
        "core/raster/PROVENANCE.md",
        "1d0697c25e05f360d94cf41247b7cb82e41200bb5b2a18514873ef41bfe01475",
    ),
    (
        "plan/m3.toml",
        "611bc0c9dcb9c138d0264472c2650fe4209864d99bd990097f23421e24fc5f41",
    ),
    (
        "core/document/tests/repository_policy.rs",
        "f8df5bd6461411dcd48f2d9a6265509d77ad1a3c763f4855d965e494e1b887ab",
    ),
    (
        "core/content/tests/repository_policy.rs",
        "bbf0d8b633e89ebe368650632fb512463b928e41c076688136b8cdb06c474c41",
    ),
    (
        "core/scene/tests/repository_policy.rs",
        "2213609601e141de87940a8814ac210e731a8133e8e64ca8cc4aef1d9e4c5b46",
    ),
    (
        "core/raster/tests/repository_policy.rs",
        "a84392326a1eb72ea5134869cb443d43e98c76ebca6c3ec3f8326b8800dc394f",
    ),
];

#[test]
fn m3_basic_image_review_is_exact_commit_tree_and_blob_bound() {
    let root = repository_root();
    let review_path = "docs/traceability/evidence/m3/basic-image-xobjects/independent-review.toml";
    let review = RootToml::parse(&read_text(&root, review_path))
        .expect("M3-08 independent review is strict TOML");

    review
        .expect_unsigned("schema", 1)
        .expect("schema is exact");
    review
        .expect_string("type", "work-item-evidence")
        .expect("type is exact");
    review
        .expect_string("id", "evidence.m3.basic-image-xobjects.independent-review")
        .expect("id is exact");
    review
        .expect_string("milestone", "M3")
        .expect("milestone is exact");
    review
        .expect_string("work_item", "M3-08")
        .expect("work item is exact");
    review
        .expect_string("profile", "m3.basic-image-xobjects.v1")
        .expect("profile is exact");
    review
        .expect_string("feature", "core.basic-image-xobjects")
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
        .expect("subject commit is exact");
    review
        .expect_string("reviewed_subject_tree", IMPLEMENTATION_TREE)
        .expect("subject tree is exact");
    review
        .expect_string(
            "reviewed_subject_resolution",
            "git-tree-at-reviewed-subject-commit-not-working-tree",
        )
        .expect("subject resolution is exact");
    review
        .expect_array("reviewer_roles", &["spec-conformance", "parser-security"])
        .expect("review roles are exact");
    for priority in ["open_p0", "open_p1", "open_p2", "open_p0_p2"] {
        review
            .expect_unsigned(priority, 0)
            .unwrap_or_else(|error| panic!("{priority}: {error}"));
    }
    review
        .expect_string("verdict", "SHIP")
        .expect("verdict is exact");

    let expected_map = REVIEWED_SUBJECTS
        .iter()
        .map(|(path, _)| format!("{path}@{IMPLEMENTATION_COMMIT}"))
        .collect::<Vec<_>>();
    assert_eq!(
        review
            .array("reviewed_subject_commit_map")
            .expect("subject map is typed"),
        expected_map
    );
    let expected_subjects = REVIEWED_SUBJECTS
        .iter()
        .map(|(path, hash)| format!("{path}@{IMPLEMENTATION_COMMIT}#sha256:{hash}"))
        .collect::<Vec<_>>();
    assert_eq!(
        review
            .array("reviewed_subjects")
            .expect("subjects are typed"),
        expected_subjects
    );
    assert_eq!(
        verify_reviewed_subjects(
            &root,
            &review,
            IMPLEMENTATION_COMMIT,
            Some(IMPLEMENTATION_TREE),
        )
        .expect("subjects are commit/tree/blob bound"),
        REVIEWED_SUBJECTS.len()
    );

    let scope = review
        .string("review_scope")
        .expect("review scope is typed");
    for marker in [
        "M3-08 only",
        "basic unmasked Image XObjects",
        "reference-image-v1",
        "glyph text",
        "integrated ReferenceRenderJob",
        "O0/O1/O3",
        "M3 exit",
    ] {
        assert!(scope.contains(marker), "review scope is missing {marker}");
    }
}

#[test]
fn m3_basic_image_review_rejects_subject_hash_and_commit_rebinding() {
    let root = repository_root();
    let review = read_text(
        &root,
        "docs/traceability/evidence/m3/basic-image-xobjects/independent-review.toml",
    );
    let wrong_hash = review.replacen(
        REVIEWED_SUBJECTS[0].1,
        "0000000000000000000000000000000000000000000000000000000000000000",
        1,
    );
    let wrong_hash = RootToml::parse(&wrong_hash).expect("mutated review remains parseable");
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

    let first = format!(
        "{}@{IMPLEMENTATION_COMMIT}#sha256:{}",
        REVIEWED_SUBJECTS[0].0, REVIEWED_SUBJECTS[0].1
    );
    let rebound = format!(
        "{}@127ee3cac4ab5595abbecd6438e6e309a58b23cb#sha256:{}",
        REVIEWED_SUBJECTS[0].0, REVIEWED_SUBJECTS[0].1
    );
    let rebound = RootToml::parse(&review.replacen(&first, &rebound, 1))
        .expect("rebound review remains parseable");
    assert!(
        verify_reviewed_subjects(
            &root,
            &rebound,
            IMPLEMENTATION_COMMIT,
            Some(IMPLEMENTATION_TREE),
        )
        .is_err(),
        "review subjects cannot be rebound to another commit"
    );
}

#[test]
fn m3_basic_image_plan_feature_and_spec_links_are_exact() {
    let root = repository_root();
    let plan_text = read_text(&root, "plan/m3.toml");
    let feature_text = read_text(&root, "docs/traceability/feature-map.toml");
    let spec_text = read_text(&root, "docs/traceability/spec-map.toml");
    let capability_profiles = read_text(&root, "docs/traceability/capability-profiles.toml");

    RootToml::parse(&plan_text)
        .expect("M3 plan is strict TOML")
        .expect_string("status", "in_progress")
        .expect("M3 remains in progress");
    let item = table_record(&plan_text, "work_item", "M3-08");
    item.expect_string("title", "Basic Image XObjects")
        .expect("title is exact");
    item.expect_string("status", "complete")
        .expect("M3-08 is complete");
    item.expect_bare("completed_at", COMPLETED_AT)
        .expect("completion date is exact");
    item.expect_array("depends_on", &["M3-03", "M3-05", "M3-07"])
        .expect("dependencies are exact");
    item.expect_string(
        "outcome",
        "The document, Scene, and Reference layers support the explicitly registered basic unmasked Image XObject subset.",
    )
    .expect("outcome is exact");
    item.expect_array(
        "acceptance",
        &[
            "Width, height, components, bits, decoded bytes, stride, sampling work, and retained pixels are validated before allocation.",
            "Only registered filter, color-space, bit-depth, decode-array, and interpolation combinations are accepted.",
            "Image transforms and sampling have exact small-image O0/O1 expectations.",
            "Masks, soft masks, unsupported codecs, and oversized images remain structured non-publication outcomes.",
        ],
    )
    .expect("acceptance is exact");
    let integrated = table_record(&plan_text, "work_item", "M3-10");
    integrated
        .expect_string("status", "complete")
        .expect("M3-10 status is exact");
    integrated
        .expect_bare("completed_at", COMPLETED_AT)
        .expect("M3-10 completion is exact");
    table_record(&plan_text, "work_item", "M3-11")
        .expect_string("status", "planned")
        .expect("M3-11 remains planned");

    let feature_root = RootToml::parse(&feature_text).expect("feature map is strict TOML");
    feature_root
        .expect_string("version", TRACE_VERSION)
        .expect("feature-map version is exact");
    let feature = table_record(&feature_text, "feature", "core.basic-image-xobjects");
    feature
        .expect_string("owner", "graphics-color")
        .expect("owner is exact");
    feature
        .expect_string("state", "PLANNED")
        .expect("state remains PLANNED");
    feature
        .expect_string("profile", "m3.basic-image-xobjects.v1")
        .expect("profile is exact");
    feature
        .expect_array(
            "modules",
            &["core/document", "core/content", "core/scene", "core/raster"],
        )
        .expect("modules are exact");
    for marker in [
        "ISO-32000-1:2008/7.8.3",
        "ISO-32000-1:2008/8.9",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/6.1-6.2",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/8.1-8.3",
        "RPE-ARCH-001/15.3/M3",
    ] {
        assert!(
            feature
                .array("clauses")
                .expect("clauses are typed")
                .iter()
                .any(|clause| clause == marker),
            "feature clauses are missing {marker}"
        );
    }
    assert!(
        !capability_profiles.contains("m3.basic-image-xobjects.v1"),
        "M3-08 must not create a maturity profile"
    );

    RootToml::parse(&spec_text)
        .expect("spec map is strict TOML")
        .expect_string("version", TRACE_VERSION)
        .expect("spec-map version is exact");
    for (id, snapshot, note_markers) in [
        (
            "ISO-32000-1:2008/7.8.3",
            ISO_SNAPSHOT,
            &[
                "M3-08 XObject resolver",
                "one immutable Content execution plan",
            ][..],
        ),
        (
            "ISO-32000-1:2008/8.9",
            ISO_SNAPSHOT,
            &[
                "reference-image-v1",
                "nearest-neighbor sampling",
                "Interpolate-true rejection",
                "Masks, soft masks",
            ][..],
        ),
        (
            "RPE-ARCH-001/5.8-5.9",
            ARCH_SNAPSHOT,
            &["Image XObject", "proof"][..],
        ),
        (
            "RPE-ARCH-001/6.1-6.2",
            ARCH_SNAPSHOT,
            &["immutable", "Pending"][..],
        ),
        (
            "RPE-ARCH-001/6.4-6.7",
            ARCH_SNAPSHOT,
            &["ImageResource", "M3-10 separately accepts"][..],
        ),
        (
            "RPE-ARCH-001/8.1-8.3",
            ARCH_SNAPSHOT,
            &[
                "reference-image-v1",
                "nearest-neighbor sampling",
                "Interpolate true remains a structured unsupported outcome",
                "M3-10 now accepts one bounded strict-to-ReferenceRenderJob path",
            ][..],
        ),
        (
            "RPE-ARCH-001/15.3/M3",
            ARCH_SNAPSHOT,
            &["M3-08 closes", "M3-10 now closes", "M3-11 still owns"][..],
        ),
    ] {
        assert_requirement(
            &spec_text,
            id,
            snapshot,
            &[
                "core.basic-image-xobjects",
                "tools/quality::m3_basic_image_trace",
            ],
            note_markers,
        );
    }
}

#[test]
fn m3_basic_image_provenance_and_ci_gate_preserve_scope_and_m1() {
    let root = repository_root();
    let content = read_text(&root, "core/content/PROVENANCE.md");
    for marker in [
        "proof-bound Image XObject acquisition",
        "immutable semantic execution",
        "exact-key resumable acquisition",
        "single Scene materialization",
        "semantic-failure-before-resource ordering",
    ] {
        assert!(
            content.contains(marker),
            "Content provenance is missing {marker:?}"
        );
    }
    let raster = read_text(&root, "core/raster/PROVENANCE.md");
    for marker in [
        "`reference-image-v1`",
        "basic unmasked image",
        "samples nearest-neighbor texels",
        "M3-08 and M3-09 likewise do not independently promote the pixel profile",
        "not a `REFERENCE` maturity promotion",
        "not an O0/O1 pixel authority",
        "M3 exit decision",
    ] {
        assert!(
            raster.contains(marker),
            "Raster provenance is missing {marker:?}"
        );
    }
    let review = read_text(
        &root,
        "docs/traceability/evidence/m3/basic-image-xobjects/independent-review.toml",
    );
    assert!(
        review.contains("nearest-neighbor sampling, structured interpolation rejection"),
        "review scope must match the exact staged sampling policy"
    );
    assert!(
        !review.contains("bilinear"),
        "M3-08 review evidence must not claim unimplemented bilinear sampling"
    );
    let document_image = read_text(&root, "core/document/src/image_xobject.rs");
    assert!(document_image.contains("SyntaxObject::Boolean(true) =>"));
    assert!(document_image.contains("ImageXObjectUnsupportedKind::Interpolation"));
    let raster_image = read_text(&root, "core/raster/src/reference/image.rs");
    assert!(raster_image.contains("if image.interpolate()"));
    assert!(raster_image.contains("ImageFailure::UnsupportedInterpolation"));
    let raster_image_tests = read_text(&root, "core/raster/tests/reference_image.rs");
    assert!(raster_image_tests.contains("interpolated_and_mismatched_inputs_fail_structurally"));

    let ci = read_text(&root, "scripts/ci.sh");
    let color = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_reference_color_trace",
    );
    let image = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_basic_image_trace",
    );
    let m2_replay = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$m2_scene_gate_root/debug-1\"",
    );
    assert!(color < image);
    assert!(image < m2_replay);

    let quality_main = read_text(&root, "tools/quality/src/main.rs");
    assert_eq!(
        quality_main.matches("m3-basic-image-trace").count(),
        2,
        "local and PR selections must both name the image trace"
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
            .expect("features are typed")
            .join("\n"),
        requirement
            .array("implementation")
            .expect("implementation is typed")
            .join("\n"),
        requirement
            .array("tests")
            .expect("tests are typed")
            .join("\n"),
    ]
    .join("\n");
    for required in required_links {
        assert!(joined.contains(required), "{id} is missing {required}");
    }
    let notes = [
        requirement.string("notes").unwrap_or_default(),
        requirement.string("m3_image_notes").unwrap_or_default(),
    ]
    .join("\n");
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
