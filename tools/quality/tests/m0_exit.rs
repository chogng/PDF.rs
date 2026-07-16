const PLAN: &str = include_str!("../../../plan/r0.toml");
const SPEC_MAP: &str = include_str!("../../../docs/traceability/spec-map.toml");
const FEATURE_MAP: &str = include_str!("../../../docs/traceability/feature-map.toml");
const BASELINE_LEDGER: &str = include_str!("../../../docs/traceability/baseline-ledger.toml");
const CASE: &str =
    include_str!("../../../tests/cases/infrastructure/synthetic-failure-bundle-001/case.toml");
const ORACLE: &str = include_str!(
    "../../../tests/cases/infrastructure/synthetic-failure-bundle-001/expected/oracle.md"
);
const CI: &str = include_str!("../../../scripts/ci.sh");
const BASELINE_PROVENANCE: &str = include_str!("../../baseline/PROVENANCE.md");
const PDFIUM_README: &str = include_str!("../../baseline/pdfium/README.md");

#[test]
fn m0_exit_is_recorded_with_reproducible_evidence_and_bounded_claims() {
    let plan_header = PLAN
        .split("[[milestone]]")
        .next()
        .expect("the plan has a top-level header");
    assert_line(plan_header, "version = \"0.3.0\"");
    assert_line(plan_header, "status = \"active\"");
    assert_line(plan_header, "last_updated = 2026-07-16");

    let m0 = array_record(PLAN, "[[milestone]]", "M0");
    for required in [
        "case manifest and generators",
        "runner schema and parse/scene/text/pixel diff skeletons",
        "benchmark harness and corpus manager",
        "process-isolated external baseline runner",
        "status = \"complete\"",
        "completed_at = 2026-07-15",
        "reviewed_by_roles = [\"spec-conformance\", \"baseline-release\"]",
        "infrastructure/synthetic-failure-bundle-001/case.toml",
        "--test process_harness --test pdfium_adapter_contract",
        "prepare-product-build-proof",
        "check-product-build-closure",
        "platform containment and complete runtime, license, font, and color closure before registered DIFFERENTIAL CI or untrusted inputs",
    ] {
        assert!(
            m0.contains(required),
            "missing M0 plan evidence: {required}"
        );
    }
    let m1 = array_record(PLAN, "[[milestone]]", "M1");
    for required in [
        "start_date = 2026-07-15",
        "status = \"complete\"",
        "completed_at = 2026-07-15",
        "reviewed_by_roles = [\"quality-corpus\", \"spec-conformance\"]",
        "validate-m1-maturity docs/traceability/capability-profiles.toml",
        "registered three-seed libFuzzer replay and real cmin",
        "generic multi-job scheduler with priority, fairness, backpressure",
        "platform-enforced isolation and baseline-ledger registration",
    ] {
        assert!(
            m1.contains(required),
            "missing M1 closure evidence: {required}"
        );
    }
    assert!(date_value(m1, "start_date") <= date_value(m1, "completed_at"));

    assert_line(SPEC_MAP, "version = \"0.72.0\"");
    assert_line(FEATURE_MAP, "version = \"0.72.0\"");
    let requirement = array_record(SPEC_MAP, "[[requirement]]", "RPE-ARCH-001/15.3/M0");
    for required in [
        "status = \"covered\"",
        "tools/quality::m0_exit",
        "active, independently reviewed O1 synthetic case",
        "complete Parse, Scene, Text, and Pixel comparison artifacts",
        "fresh out-of-repository release target",
        "zero build-script, native, external-engine, unknown, symlink, stale, or incomplete artifacts",
        "process-level black box used only with fixed, self-authored, hash-bound inputs",
        "not a registered baseline",
        "expected strictness difference",
        "external probes remain unregistered and non-gating",
        "remain open as later maturity or release work; none is part of the accepted M0 exit gate",
    ] {
        assert!(
            requirement.contains(required),
            "missing bounded M0 trace statement: {required}"
        );
    }

    for feature_id in [
        "quality.minimal-pdf-generator",
        "quality.native-object-loop",
        "quality.canonical-diff",
        "quality.failure-bundle",
        "quality.baseline-protocol-boundary",
        "quality.benchmark-harness",
        "quality.corpus-manager",
    ] {
        let feature = array_record(FEATURE_MAP, "[[feature]]", feature_id);
        assert!(feature.contains("RPE-ARCH-001/15.3/M0"));
        assert_line(feature, "state = \"PLANNED\"");
    }

    for required in [
        "status = \"active\"",
        "level = \"O1\"",
        "reviewers = [\"spec-conformance\"]",
        "last_reviewed = \"2026-07-15\"",
        "external_observation = []",
    ] {
        assert_line(CASE, required);
    }
    for required in [
        "Parse, Scene, Text, and Pixel artifacts",
        "synthetic artifact constructors",
        "not an external-engine observation",
        "reviewed this derivation",
    ] {
        assert!(ORACLE.contains(required));
    }

    let manifest_scan = position(CI, "check-product-purity .");
    let proof_prepare = position(CI, "prepare-product-build-proof");
    let product_build = position(CI, "CARGO_TARGET_DIR=\"$product_target\" cargo build");
    let proof_check = position(CI, "check-product-build-closure");
    let bundle = position(CI, "synthetic-bundle");
    assert!(manifest_scan < proof_prepare);
    assert!(proof_prepare < product_build);
    assert!(product_build < proof_check);
    assert!(proof_check < bundle);
    for required in [
        "mktemp -d",
        "--locked",
        "--release",
        "--lib",
        "--package pdf-rs-bytes",
        "--package pdf-rs-content",
        "--package pdf-rs-session",
        "target/ci-artifacts/m0-failure-bundles",
    ] {
        assert!(CI.contains(required));
    }

    assert_line(BASELINE_LEDGER, "status = \"initial\"");
    assert_line(BASELINE_LEDGER, "baseline = []");
    assert!(
        BASELINE_PROVENANCE.contains("satisfies the M0 external-runner and build-isolation scope")
    );
    assert!(BASELINE_PROVENANCE.contains("before registered DIFFERENTIAL CI"));
    assert!(PDFIUM_README.contains("closes the external-runner infrastructure boundary"));
    assert!(PDFIUM_README.contains("remain blocking for registered"));
    assert!(PDFIUM_README.contains("DIFFERENTIAL CI or untrusted inputs"));
    for document in [BASELINE_PROVENANCE, PDFIUM_README] {
        assert!(document.contains("not a registered baseline"));
        assert!(!document.contains("remain M0-blocking"));
        assert!(!document.contains("not the M0 external baseline runner exit condition"));
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
        .unwrap_or_else(|| panic!("missing CI step: {needle}"))
}

fn date_value<'a>(record: &'a str, key: &str) -> &'a str {
    let prefix = format!("{key} = ");
    record
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("missing date field: {key}"))
}
