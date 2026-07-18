use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use pdf_rs_digest::{hex_digest, sha256};

#[path = "support/evidence.rs"]
mod evidence;

use evidence::{
    RootToml, TestDirectory, array_table_records, git_revision, read_repository_file,
    validate_commit_id, verify_subject_entries,
};

const EXIT_CANDIDATE: &str =
    include_str!("../../../docs/traceability/evidence/m4/fast-cpu-canary/exit-candidate.toml");
const PROMOTION: &str =
    include_str!("../../../docs/traceability/evidence/m4/fast-cpu-canary/promotion.toml");
const REVIEW_REQUEST: &str =
    include_str!("../../../docs/traceability/evidence/m4/fast-cpu-canary/review-request.toml");
const REGISTRY: &str = include_str!("../../../docs/traceability/canary-profiles.toml");
const M4_PLAN: &str = include_str!("../../../plan/m4.toml");
const R0_PLAN: &str = include_str!("../../../plan/r0.toml");
const M3_PLAN: &str = include_str!("../../../plan/m3.toml");
const M2_PLAN: &str = include_str!("../../../plan/m2.toml");
const ELECTRON_TARGET: &str = include_str!("../../../platform/electron/electron-target.toml");
const ELECTRON_PACKAGE: &str = include_str!("../../../platform/electron/package.json");

const EXIT_CANDIDATE_SHA256: &str =
    "006a3211ee6fff7fffff56ab8ee556f9885650bd7d765e5b3cf3994f587f9378";
const CANDIDATE_BASE_COMMIT: &str = "b41e7890a9906a4bd7644eba7a4016d3f4bfc76d";
const CANDIDATE_BASE_TREE: &str = "c88c82fdf66a35ed8a023a64d40381b160ed1be2";
const DECISION_RECORD: &str =
    "docs/traceability/evidence/m4/fast-cpu-canary/independent-review.toml";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndependentReviewState {
    Missing,
    Invalid,
    Valid,
}

#[test]
fn automated_m4_exit_candidate_has_only_registered_review_and_completion_gaps() {
    let root = repository_root();
    verify_automated_closure(&root);

    let candidate = RootToml::parse(EXIT_CANDIDATE).expect("M4 exit candidate");
    let expected = candidate
        .array("expected_pending_gaps")
        .expect("registered pending gaps")
        .to_vec();
    assert_eq!(
        final_exit_gaps(&root),
        expected,
        "M4 candidate gained an unregistered gap or silently satisfied a required approval"
    );
}

#[test]
#[ignore = "requires a real independent SHIP record and the reviewed M4 completion transition"]
fn final_m4_exit_requires_zero_remaining_gaps() {
    let root = repository_root();
    verify_automated_closure(&root);
    assert!(
        final_exit_gaps(&root).is_empty(),
        "M4 final exit still has gaps: {:?}",
        final_exit_gaps(&root)
    );
}

#[test]
fn independent_review_contract_distinguishes_missing_invalid_and_valid_records() {
    let repository = TestDirectory::new("pdf-rs-m4-review-contract");
    assert_eq!(
        independent_review_state(repository.path()),
        IndependentReviewState::Missing
    );

    let record = repository.path().join(DECISION_RECORD);
    fs::create_dir_all(record.parent().expect("review record parent")).expect("review directory");
    fs::write(&record, "schema = 1\nverdict = \"SHIP\"\n").expect("invalid review record");
    assert_eq!(
        independent_review_state(repository.path()),
        IndependentReviewState::Invalid
    );

    fs::write(
        &record,
        format!(
            "schema = 1\n\
             type = \"independent-review\"\n\
             milestone = \"M4\"\n\
             candidate_commit = \"5967a4a28e55ee20ebbf8ae8119658b077b9940f\"\n\
             exit_candidate = \"docs/traceability/evidence/m4/fast-cpu-canary/exit-candidate.toml#sha256:{EXIT_CANDIDATE_SHA256}\"\n\
             reviewer_roles = [\"runtime-platform\", \"graphics-color\", \"quality-corpus\"]\n\
             reviewer_identities = [\"reviewer-runtime\", \"reviewer-graphics\", \"reviewer-quality\"]\n\
             independent_review_complete = true\n\
             open_p0 = 0\n\
             open_p1 = 0\n\
             open_p2 = 0\n\
             verdict = \"SHIP\"\n"
        ),
    )
    .expect("valid review record");
    assert_eq!(
        independent_review_state(repository.path()),
        IndependentReviewState::Valid
    );
}

fn verify_automated_closure(root: &Path) {
    let candidate = RootToml::parse(EXIT_CANDIDATE).expect("M4 exit candidate");
    candidate.expect_unsigned("schema", 1).expect("schema");
    candidate
        .expect_string("type", "milestone-exit-candidate")
        .expect("candidate type");
    candidate
        .expect_string("milestone", "M4")
        .expect("milestone");
    candidate
        .expect_string("state", "AUTOMATED_CLOSURE")
        .expect("candidate state");
    candidate
        .expect_string("candidate_base_commit", CANDIDATE_BASE_COMMIT)
        .expect("candidate commit");
    candidate
        .expect_string("candidate_base_tree", CANDIDATE_BASE_TREE)
        .expect("candidate tree");
    candidate
        .expect_string("selected_target", "m4-electron-local-preview-v1")
        .expect("selected target");
    candidate
        .expect_string("profile_state", "CANARY_CANDIDATE")
        .expect("profile state");
    candidate
        .expect_string("default_renderer", "Reference CPU")
        .expect("default renderer");
    candidate
        .expect_string("canary_renderer", "Fast CPU")
        .expect("canary renderer");
    candidate
        .expect_bool("promotion_approved", false)
        .expect("candidate promotion");
    candidate
        .expect_bool("milestone_approved", false)
        .expect("candidate milestone");
    candidate
        .expect_bool("external_engine_fallback", false)
        .expect("no fallback");
    candidate
        .expect_string("required_decision_record", DECISION_RECORD)
        .expect("decision record");
    candidate
        .expect_array(
            "required_review_roles",
            &["runtime-platform", "graphics-color", "quality-corpus"],
        )
        .expect("review roles");

    assert_eq!(
        digest(EXIT_CANDIDATE.as_bytes()),
        EXIT_CANDIDATE_SHA256,
        "exit candidate bytes changed"
    );
    validate_commit_id(CANDIDATE_BASE_COMMIT).expect("canonical candidate commit");
    assert_eq!(
        git_revision(root, &format!("{CANDIDATE_BASE_COMMIT}^{{tree}}")),
        CANDIDATE_BASE_TREE,
        "candidate base tree changed"
    );
    assert_eq!(
        verify_subject_entries(
            root,
            candidate
                .array("content_references")
                .expect("content references"),
        )
        .expect("content-addressed M4 exit inputs"),
        24,
        "M4 exit input topology changed"
    );

    verify_prerequisite_plans();
    verify_m4_plan();
    verify_selected_electron_target();
    verify_default_off_candidate();
    verify_review_request();
}

fn verify_prerequisite_plans() {
    for (name, plan) in [("M2", M2_PLAN), ("M3", M3_PLAN)] {
        let root = RootToml::parse(plan).unwrap_or_else(|error| panic!("{name} plan: {error}"));
        root.expect_string("milestone", name)
            .unwrap_or_else(|error| panic!("{name} milestone: {error}"));
        root.expect_string("status", "complete")
            .unwrap_or_else(|error| panic!("{name} status: {error}"));
    }
}

fn verify_m4_plan() {
    let root = RootToml::parse(M4_PLAN).expect("M4 plan");
    root.expect_string("milestone", "M4").expect("M4 id");
    root.expect_string("status", "in_progress")
        .expect("M4 candidate status");
    root.expect_unsigned("max_parallel_work_items", 1)
        .expect("M4 serial execution");

    let work_items = array_table_records(M4_PLAN, "work_item").expect("M4 work items");
    for ordinal in 1..=8 {
        record(&work_items, &format!("M4-{ordinal:02}"), "work item")
            .expect_string("status", "complete")
            .unwrap_or_else(|error| panic!("M4-{ordinal:02}: {error}"));
    }
    let native_package = record(&work_items, "M4-09", "work item");
    native_package
        .expect_string("status", "in_progress")
        .expect("retained native target");
    native_package
        .expect_bool("blocking_selected_electron_target", false)
        .expect("native package is non-blocking");
    record(&work_items, "M4-10", "work item")
        .expect_string("status", "complete")
        .expect("Electron vertical slice");
    record(&work_items, "M4-11", "work item")
        .expect_string("status", "in_progress")
        .expect("CANARY candidate");
    record(&work_items, "M4-12", "work item")
        .expect_string("status", "planned")
        .expect("milestone gate");

    let milestone_start = R0_PLAN
        .find("[[milestone]]")
        .expect("R0 milestone registry");
    let milestones =
        array_table_records(&R0_PLAN[milestone_start..], "milestone").expect("R0 milestones");
    let m4 = record(&milestones, "M4", "milestone");
    m4.expect_string("status", "in_progress")
        .expect("R0 M4 status");
    m4.expect_unsigned("max_parallel_work_items", 1)
        .expect("R0 M4 serial execution");
}

fn verify_selected_electron_target() {
    let target = root(ELECTRON_TARGET);
    target
        .expect_string("target_id", "m4-electron-local-preview-v1")
        .expect("target id");
    target
        .expect_bool("selected", true)
        .expect("selected target");
    target
        .expect_string("status", "complete")
        .expect("target status");
    let platform = table(ELECTRON_TARGET, "platform");
    platform
        .expect_string("runtime", "electron")
        .expect("runtime");
    platform
        .expect_string("distribution", "local-development-only")
        .expect("distribution");
    for field in [
        "package_required",
        "signing_required",
        "notarization_required",
    ] {
        platform
            .expect_bool(field, false)
            .unwrap_or_else(|error| panic!("{field}: {error}"));
    }
    let security = table(ELECTRON_TARGET, "security");
    security
        .expect_bool("context_isolation", true)
        .expect("context isolation");
    security
        .expect_bool("renderer_sandbox", true)
        .expect("renderer sandbox");
    for field in [
        "node_integration",
        "remote_module",
        "renderer_filesystem_access",
        "renderer_process_access",
    ] {
        security
            .expect_bool(field, false)
            .unwrap_or_else(|error| panic!("{field}: {error}"));
    }
    for forbidden in [
        "\"build\":",
        "\"package\":",
        "electron-builder",
        "electron-forge",
    ] {
        assert!(
            !ELECTRON_PACKAGE.contains(forbidden),
            "local Electron target gained packaging surface {forbidden}"
        );
    }
}

fn verify_default_off_candidate() {
    let promotion = root(PROMOTION);
    promotion
        .expect_string("state", "CANARY_CANDIDATE")
        .expect("promotion state");
    promotion
        .expect_bool("promotion_approved", false)
        .expect("promotion pending");
    promotion
        .expect_bool("enabled_by_default", false)
        .expect("default off");
    let exposure = table(PROMOTION, "exposure");
    exposure
        .expect_string("default_path", "Reference CPU")
        .expect("Reference default");
    exposure
        .expect_string("canary_path", "Fast CPU")
        .expect("Fast canary");
    exposure
        .expect_bool("rollback_rehearsed", true)
        .expect("rollback");
    exposure
        .expect_bool("unsupported_semantics_unchanged", true)
        .expect("Unsupported unchanged");

    let profiles = array_table_records(REGISTRY, "profile").expect("CANARY registry");
    let profile = record(&profiles, "m4.fast-cpu-r0-basic-page.v1", "CANARY profile");
    profile
        .expect_string("state", "CANARY_CANDIDATE")
        .expect("registry candidate");
    profile
        .expect_bool("enabled_by_default", false)
        .expect("registry default off");
    profile
        .expect_bool("promotion_approved", false)
        .expect("registry pending");
}

fn verify_review_request() {
    let request = RootToml::parse(REVIEW_REQUEST).expect("review request");
    request
        .expect_string("state", "PENDING")
        .expect("review request state");
    request
        .expect_array("completed_reviewers", &[])
        .expect("no fabricated reviewers");
    request
        .expect_bool("promotion_approved", false)
        .expect("promotion pending");
    request
        .expect_bool("milestone_approved", false)
        .expect("milestone pending");
    request
        .expect_string("decision", "PENDING")
        .expect("review decision");
}

fn final_exit_gaps(repository: &Path) -> Vec<String> {
    let mut gaps = Vec::new();
    match independent_review_state(repository) {
        IndependentReviewState::Missing => {
            gaps.push("independent-review-record-missing".to_owned());
        }
        IndependentReviewState::Invalid => {
            gaps.push("independent-review-record-invalid".to_owned());
        }
        IndependentReviewState::Valid => {}
    }

    let promotion = root(PROMOTION);
    let review = table(PROMOTION, "review");
    if review.boolean("independent_review_complete").ok() != Some(true) {
        gaps.push("promotion-review-incomplete".to_owned());
    }
    if promotion.boolean("promotion_approved").ok() != Some(true) {
        gaps.push("promotion-not-approved".to_owned());
    }

    let profiles = array_table_records(REGISTRY, "profile").expect("CANARY registry");
    let profile = record(&profiles, "m4.fast-cpu-r0-basic-page.v1", "CANARY profile");
    if profile.string("state").ok() != Some("CANARY")
        || profile.boolean("promotion_approved").ok() != Some(true)
    {
        gaps.push("registry-not-promoted-to-canary".to_owned());
    }

    let work_items = array_table_records(M4_PLAN, "work_item").expect("M4 work items");
    for (id, gap) in [
        ("M4-11", "m4-11-not-complete"),
        ("M4-12", "m4-12-not-complete"),
    ] {
        if record(&work_items, id, "work item").string("status").ok() != Some("complete") {
            gaps.push(gap.to_owned());
        }
    }
    if root(M4_PLAN).string("status").ok() != Some("complete") {
        gaps.push("m4-plan-not-complete".to_owned());
    }

    let milestone_start = R0_PLAN
        .find("[[milestone]]")
        .expect("R0 milestone registry");
    let milestones =
        array_table_records(&R0_PLAN[milestone_start..], "milestone").expect("R0 milestones");
    if record(&milestones, "M4", "milestone").string("status").ok() != Some("complete") {
        gaps.push("r0-m4-milestone-not-complete".to_owned());
    }
    gaps
}

fn independent_review_state(repository: &Path) -> IndependentReviewState {
    if !repository.join(DECISION_RECORD).exists() {
        return IndependentReviewState::Missing;
    }
    let Ok(bytes) = read_repository_file(repository, DECISION_RECORD) else {
        return IndependentReviewState::Invalid;
    };
    let Ok(document) = std::str::from_utf8(&bytes) else {
        return IndependentReviewState::Invalid;
    };
    let Ok(review) = RootToml::parse(document) else {
        return IndependentReviewState::Invalid;
    };
    let Ok(roles) = review.array("reviewer_roles") else {
        return IndependentReviewState::Invalid;
    };
    let Ok(identities) = review.array("reviewer_identities") else {
        return IndependentReviewState::Invalid;
    };
    let unique_identities = identities.iter().collect::<BTreeSet<_>>();
    let valid = review.string("type").ok() == Some("independent-review")
        && review.string("milestone").ok() == Some("M4")
        && review.string("candidate_commit").ok()
            == Some("5967a4a28e55ee20ebbf8ae8119658b077b9940f")
        && review.string("exit_candidate").ok()
            == Some(&format!(
                "docs/traceability/evidence/m4/fast-cpu-canary/exit-candidate.toml#sha256:{EXIT_CANDIDATE_SHA256}"
            ))
        && roles
            == [
                "runtime-platform".to_owned(),
                "graphics-color".to_owned(),
                "quality-corpus".to_owned(),
            ]
        && identities.len() == 3
        && unique_identities.len() == 3
        && identities
            .iter()
            .all(|identity| !identity.trim().is_empty())
        && review.boolean("independent_review_complete").ok() == Some(true)
        && review.unsigned("open_p0").ok() == Some(0)
        && review.unsigned("open_p1").ok() == Some(0)
        && review.unsigned("open_p2").ok() == Some(0)
        && review.string("verdict").ok() == Some("SHIP");
    if valid {
        IndependentReviewState::Valid
    } else {
        IndependentReviewState::Invalid
    }
}

fn record<'a>(records: &'a [RootToml], id: &str, kind: &str) -> &'a RootToml {
    records
        .iter()
        .find(|record| record.string("id").ok() == Some(id))
        .unwrap_or_else(|| panic!("missing {kind} {id}"))
}

fn root(document: &str) -> RootToml {
    let end = document.find("\n[").unwrap_or(document.len());
    RootToml::parse(&document[..end]).expect("document root cannot be parsed")
}

fn table(document: &str, name: &str) -> RootToml {
    let header = format!("[{name}]\n");
    let start = document
        .find(&header)
        .unwrap_or_else(|| panic!("missing table [{name}]"))
        + header.len();
    let rest = &document[start..];
    let end = rest.find("\n[").map_or(rest.len(), |offset| offset + 1);
    RootToml::parse(&rest[..end])
        .unwrap_or_else(|error| panic!("[{name}] cannot be parsed: {error}"))
}

fn digest(bytes: &[u8]) -> String {
    hex_digest(&sha256(bytes).expect("bounded SHA-256"))
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("quality crate is under tools/quality")
        .to_path_buf()
}
