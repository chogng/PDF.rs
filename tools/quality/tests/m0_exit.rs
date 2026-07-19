const PLAN: &str = include_str!("../../../plan/r0.toml");
const M4_PLAN: &str = include_str!("../../../plan/m4.toml");
const M5_PLAN: &str = include_str!("../../../plan/m5.toml");
const M6_PLAN: &str = include_str!("../../../plan/m6.toml");
const POST_R0_FONT_TEXT_PLAN: &str = include_str!("../../../plan/post-r0-font-text.toml");
const R0_RELEASE_PROFILE: &str = include_str!("../../../release/profiles/r0.toml");
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
    assert_line(plan_header, "version = \"0.5.0\"");
    assert_line(plan_header, "status = \"active\"");
    assert_line(plan_header, "last_updated = 2026-07-18");

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

    assert_line(SPEC_MAP, "version = \"0.78.0\"");
    assert_line(FEATURE_MAP, "version = \"0.78.0\"");
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
    let desktop_worker_build = position(CI, "--bin pdf-rs-desktop-worker");
    let proof_check = position(CI, "check-product-build-closure");
    let bundle = position(CI, "synthetic-bundle");
    assert!(manifest_scan < proof_prepare);
    assert!(proof_prepare < product_build);
    assert!(product_build < desktop_worker_build);
    assert!(desktop_worker_build < proof_check);
    assert!(proof_check < bundle);
    for required in [
        "mktemp -d",
        "--locked",
        "--release",
        "--lib",
        "--bin pdf-rs-desktop-worker",
        "--package pdf-rs-bytes",
        "--package pdf-rs-browser-worker",
        "--package pdf-rs-content",
        "--package pdf-rs-desktop",
        "--package pdf-rs-engine",
        "--package pdf-rs-policy",
        "--package pdf-rs-protocol",
        "--package pdf-rs-scheduler",
        "--package pdf-rs-session",
        "--package pdf-rs-surface",
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

#[test]
fn font_text_roadmap_closes_post_r0_delivery_gaps_without_widening_r0() {
    let root = PLAN
        .split("[post_r0_font_text]")
        .nth(1)
        .expect("root roadmap must register the Post-R0 Font/Text milestone");
    for required in [
        "title = \"Post-R0 advanced font, text, shaping, and structure\"",
        "advanced simple-font encodings, CMaps, CID collections, and CID-to-glyph mappings beyond the R0 matrix",
        "authoring-only shaping, font subsetting/embedding, encoding, ToUnicode generation, and writer round trip",
        "separate controlled-font-fallback decision plus conditional bundled-font/system-font implementation gate",
        "execution_plan = \"plan/post-r0-font-text.toml\"",
    ] {
        assert!(
            root.contains(required),
            "missing root Post-R0 delivery contract: {required}"
        );
    }

    let work_items = POST_R0_FONT_TEXT_PLAN
        .split("[[work_item]]")
        .skip(1)
        .collect::<Vec<_>>();
    assert_eq!(work_items.len(), 12);
    for (index, expected) in (1..=12)
        .map(|value| format!("id = \"FT1-{value:02}\""))
        .enumerate()
    {
        assert_line(work_items[index], &expected);
    }

    let advanced_encoding = array_record(POST_R0_FONT_TEXT_PLAN, "[[work_item]]", "FT1-02");
    for required in [
        "Advanced font encoding and CID mapping closure",
        "BaseEncoding/Differences and symbolic-font cases",
        "CIDFontType0/CFF and CIDFontType2 combinations",
        "Registry/Ordering/Supplement",
        "usecmap depth/cycles",
        "aggregate CJK coverage cannot hide a failed family",
    ] {
        assert!(advanced_encoding.contains(required));
    }

    let vertical = array_record(POST_R0_FONT_TEXT_PLAN, "[[work_item]]", "FT1-04");
    for required in [
        "depends_on = [\"FT1-01\", \"FT1-02\"]",
        "Identity-V",
        "W2/DW2",
        "vertical origins",
        "glyph orientation",
    ] {
        assert!(vertical.contains(required));
    }

    let shaping = array_record(POST_R0_FONT_TEXT_PLAN, "[[work_item]]", "FT1-08");
    for required in [
        "Authoring-only shaping contract and dependency gate",
        "script, language, direction, OpenType features, variation coordinates",
        "dependency-ledger review",
        "parser, document reader, Content VM, Scene replay, text extraction, selection, copy, and search cannot call the shaper",
        "Arabic joining",
        "Indic reordering",
    ] {
        assert!(shaping.contains(required));
    }
    let authored_writer = array_record(POST_R0_FONT_TEXT_PLAN, "[[work_item]]", "FT1-09");
    for required in [
        "depends_on = [\"FT1-08\"]",
        "start_gates = [\"M8 ChangeSet/incremental-writer and reopen-validation contracts frozen\"]",
        "subset only used glyph closure",
        "CMap/encoding, ToUnicode",
        "reopen generated documents through the normal reader",
    ] {
        assert!(authored_writer.contains(required));
    }

    let fallback_decision = array_record(POST_R0_FONT_TEXT_PLAN, "[[work_item]]", "FT1-10");
    for required in [
        "fail-closed, bundled fixed font pack, and system-font adapter",
        "complete platform-font fingerprints",
        "System font enumeration alone",
        "Record one terminal decision",
        "activates FT1-11 but does not enable fallback by default",
    ] {
        assert!(fallback_decision.contains(required));
    }
    let fallback_implementation = array_record(POST_R0_FONT_TEXT_PLAN, "[[work_item]]", "FT1-11");
    for required in [
        "condition = \"Required only when FT1-10 approves",
        "Keep fallback disabled by default",
        "complete font-environment fingerprint",
        "core owns deterministic candidate scoring and rejection",
        "must not call authoring shaping for already positioned PDF text",
        "kill switch, and rollback drill",
    ] {
        assert!(fallback_implementation.contains(required));
    }

    for required in [
        "beyond the frozen R0 matrix",
        "authoring-only Unicode shaping",
        "System-font fallback",
    ] {
        assert!(
            M6_PLAN.contains(required),
            "M6 must explicitly exclude Post-R0 scope: {required}"
        );
    }
    assert!(M4_PLAN.contains("system-font fallback"));
    assert!(M4_PLAN.contains("advanced text"));
    assert!(M5_PLAN.contains("M5 adds no CMap, ToUnicode, Unicode semantic model"));
    assert!(M5_PLAN.contains("Any Native font/text capability expansion"));
    assert!(!R0_RELEASE_PROFILE.contains("\"ft1."));
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
