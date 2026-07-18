use std::path::{Path, PathBuf};

#[path = "support/evidence.rs"]
mod evidence;

use evidence::{
    RootToml, array_table_records, git_revision, validate_commit_id, verify_subject_entries,
};

const REQUEST: &str =
    include_str!("../../../docs/traceability/evidence/m4/fast-cpu-canary/review-request.toml");
const PLAN: &str = include_str!("../../../plan/m4.toml");
const R0_PLAN: &str = include_str!("../../../plan/r0.toml");
const CANDIDATE_COMMIT: &str = "72bbd3b9383147c97f50060347a47aca2bde105c";
const CANDIDATE_TREE: &str = "4ca19d3cc92670482e4a8f51617e61eb5d728cc4";

#[test]
fn m4_review_request_is_hash_bound_reproducible_and_still_pending() {
    let request = RootToml::parse(REQUEST).expect("M4 review request");
    request.expect_unsigned("schema", 1).expect("schema");
    request
        .expect_string("type", "independent-review-request")
        .expect("request type");
    request.expect_string("milestone", "M4").expect("milestone");
    request
        .expect_string("promotion_work_item", "M4-11")
        .expect("promotion work item");
    request
        .expect_string("milestone_gate_work_item", "M4-12")
        .expect("milestone gate work item");
    request
        .expect_string("state", "PENDING")
        .expect("pending state");
    request
        .expect_string("candidate_commit", CANDIDATE_COMMIT)
        .expect("candidate commit");
    request
        .expect_string("candidate_tree", CANDIDATE_TREE)
        .expect("candidate tree");
    request
        .expect_array(
            "promotion_required_roles",
            &["graphics-color", "quality-corpus"],
        )
        .expect("promotion roles");
    request
        .expect_array(
            "milestone_exit_required_roles",
            &["runtime-platform", "graphics-color", "quality-corpus"],
        )
        .expect("milestone roles");
    request
        .expect_array("completed_reviewers", &[])
        .expect("no fabricated reviewers");
    request
        .expect_bool("promotion_approved", false)
        .expect("promotion unapproved");
    request
        .expect_bool("milestone_approved", false)
        .expect("milestone unapproved");
    request
        .expect_string("decision", "PENDING")
        .expect("pending decision");

    validate_commit_id(CANDIDATE_COMMIT).expect("canonical candidate commit");
    let root = repository_root();
    assert_eq!(
        git_revision(&root, &format!("{CANDIDATE_COMMIT}^{{tree}}")),
        CANDIDATE_TREE,
        "candidate tree changed"
    );

    let subjects = [
        request.string("promotion").expect("promotion reference"),
        request
            .string("canary_registry")
            .expect("registry reference"),
        request.string("candidate_gate").expect("gate reference"),
        request
            .string("electron_viewer_regression")
            .expect("Electron viewer regression reference"),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert_eq!(
        verify_subject_entries(&root, &subjects).expect("review-request subjects"),
        4
    );

    request
        .expect_array(
            "commands",
            &[
                "cargo test --locked --package pdf-rs-quality --test m4_fast_canary_gate",
                "cargo test --locked --package pdf-rs-quality --test m4_fast_canary_holdout",
                "cargo test --locked --package pdf-rs-quality --test m4_fast_raster_fuzz -- --nocapture",
                "cargo test --locked --package pdf-rs-fast-raster",
                "cargo test --locked --package pdf-rs-viewer --all-targets",
                "cargo test --locked --package pdf-rs-electron-bridge --all-targets",
                "npm --prefix platform/electron test",
                "cargo run --quiet --locked --package pdf-rs-quality -- check-product-purity .",
                "cargo fmt --all -- --check",
                "git diff --check",
            ],
        )
        .expect("review commands");

    let plan = RootToml::parse(PLAN).expect("M4 plan");
    plan.expect_string("status", "in_progress")
        .expect("M4 remains in progress");
    plan.expect_unsigned("max_parallel_work_items", 1)
        .expect("M4 remains serial");
    let work_items = array_table_records(PLAN, "work_item").expect("M4 work items");
    let m4_11 = work_item(&work_items, "M4-11");
    m4_11
        .expect_string("status", "in_progress")
        .expect("M4-11 remains in progress");
    let m4_12 = work_item(&work_items, "M4-12");
    m4_12
        .expect_string("status", "planned")
        .expect("M4-12 remains planned");

    let milestone_start = R0_PLAN
        .find("[[milestone]]")
        .expect("R0 milestone registry");
    let milestones =
        array_table_records(&R0_PLAN[milestone_start..], "milestone").expect("R0 milestones");
    let m4 = record(&milestones, "M4", "milestone");
    m4.expect_unsigned("max_parallel_work_items", 1)
        .expect("R0 M4 remains serial");
}

fn work_item<'a>(records: &'a [RootToml], id: &str) -> &'a RootToml {
    record(records, id, "work item")
}

fn record<'a>(records: &'a [RootToml], id: &str, kind: &str) -> &'a RootToml {
    records
        .iter()
        .find(|record| record.string("id").ok() == Some(id))
        .unwrap_or_else(|| panic!("missing {kind} {id}"))
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("quality crate is under tools/quality")
        .to_path_buf()
}
