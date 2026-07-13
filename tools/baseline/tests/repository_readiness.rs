use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use pdf_rs_digest::sha256;

const REPORT_ID: &str = "pdfium-c040cf96-macos-arm64-build-readiness-v1";
const REPORT_HASH: &str = "b61d01628d96908a38edb94b4d08e810dfbd83c27dec2e5629e8449e941942a7";
const REPORT: &[u8] =
    include_bytes!("../pdfium/evidence/pdfium-c040cf96-macos-arm64-build-readiness-v1.toml");

type Record = BTreeMap<String, String>;

#[test]
fn repository_readiness_report_is_canonical_redacted_and_ineligible() {
    assert_eq!(hex_digest(&sha256(REPORT).unwrap()), REPORT_HASH);

    let report = std::str::from_utf8(REPORT).unwrap();
    assert!(report.ends_with('\n'));
    assert!(!report.contains('\r'));
    for forbidden in ["/Users/", "/private/tmp/", "HOME=", "lance"] {
        assert!(!report.contains(forbidden), "unredacted value: {forbidden}");
    }

    let (top, executions) = parse_report(report);
    assert_keys(
        &top,
        &[
            "schema",
            "id",
            "evidence_class",
            "recorded_at",
            "engine",
            "upstream_repository",
            "github_mirror",
            "github_mirror_status",
            "upstream_revision",
            "source_checkout_mode",
            "checkout_configuration",
            "depot_tools_revision",
            "readme_sha256",
            "deps_sha256",
            "root_license_sha256",
            "platform",
            "os_version",
            "kernel_release",
            "xcode_version",
            "target_triple",
            "gn_version",
            "ninja_version",
            "clang_version",
            "clang_sha256",
            "build_args",
            "build_args_sha256",
            "gn_targets_generated",
            "initial_build_exit_code",
            "initial_build_completed_steps",
            "initial_build_log_status",
            "pdfium_unittests_sha256",
            "pdfium_unittests_bytes",
            "pdfium_test_sha256",
            "pdfium_test_bytes",
            "pdfium_diff_sha256",
            "pdfium_diff_bytes",
            "build_readiness_outcome",
            "pixel_runner_pretest_environment_failures",
            "pixel_runner_pretest_note",
            "runtime_closure_status",
            "license_manifest_status",
            "font_fingerprint_status",
            "color_fingerprint_status",
            "sandbox_status",
            "raw_logs_committed",
            "binaries_committed",
            "upstream_data_committed",
            "adapter_exercised",
            "pdf_rs_fixture_exercised",
            "pdf_rs_corpus_manifest_exercised",
            "product_correctness_eligible",
            "performance_eligible",
            "differential_eligible",
            "baseline_registration_eligible",
            "release_gate_eligible",
            "oracle_authority",
        ],
    );

    assert_eq!(raw(&top, "schema"), "1");
    assert_eq!(quoted(&top, "id"), REPORT_ID);
    assert_eq!(quoted(&top, "evidence_class"), "upstream-build-readiness");
    assert_eq!(quoted(&top, "engine"), "pdfium");
    assert_eq!(
        quoted(&top, "upstream_repository"),
        "https://pdfium.googlesource.com/pdfium/"
    );
    assert_eq!(
        quoted(&top, "upstream_revision"),
        "c040cf96106a87220b814a1a892649cf2d7f1934"
    );
    assert_eq!(quoted(&top, "build_readiness_outcome"), "pass");
    assert_eq!(quoted(&top, "runtime_closure_status"), "incomplete");
    assert_eq!(quoted(&top, "license_manifest_status"), "incomplete");
    assert_eq!(quoted(&top, "font_fingerprint_status"), "incomplete");
    assert_eq!(quoted(&top, "color_fingerprint_status"), "incomplete");
    assert_eq!(quoted(&top, "sandbox_status"), "not-evaluated");
    assert_eq!(quoted(&top, "oracle_authority"), "not-applicable");
    assert_eq!(raw(&top, "pdf_rs_fixture_exercised"), "true");

    for key in [
        "raw_logs_committed",
        "binaries_committed",
        "upstream_data_committed",
        "adapter_exercised",
        "pdf_rs_corpus_manifest_exercised",
        "product_correctness_eligible",
        "performance_eligible",
        "differential_eligible",
        "baseline_registration_eligible",
        "release_gate_eligible",
    ] {
        assert_eq!(raw(&top, key), "false", "{key} must remain false");
    }
    for forbidden_key in [
        "build_hash",
        "environment_hash",
        "invocation_hash",
        "license_manifest_hash",
        "fonts_hash",
        "color_hash",
        "isolation_profile",
    ] {
        assert!(!top.contains_key(forbidden_key));
    }

    let expected_ids = [
        "gn-gen",
        "ninja-artifact-verification",
        "pdfium-unittests",
        "pixel-rectangles-clipped",
        "pdf-rs-generated-fixture-pageinfo",
    ];
    assert_eq!(executions.len(), expected_ids.len());
    for (execution, expected_id) in executions.iter().zip(expected_ids) {
        assert_eq!(quoted(execution, "id"), expected_id);
        assert_eq!(integer(execution, "exit_code"), 0);
    }

    assert_keys(
        &executions[0],
        &[
            "id",
            "kind",
            "program",
            "argv",
            "cwd",
            "exit_code",
            "targets_generated",
            "stdout_sha256",
            "stdout_bytes",
            "stderr_sha256",
            "stderr_bytes",
        ],
    );
    assert_keys(
        &executions[1],
        &[
            "id",
            "kind",
            "program",
            "argv",
            "cwd",
            "exit_code",
            "artifact_state",
            "stdout_sha256",
            "stdout_bytes",
            "stderr_sha256",
            "stderr_bytes",
        ],
    );
    assert_keys(
        &executions[2],
        &[
            "id",
            "kind",
            "program",
            "argv",
            "cwd",
            "exit_code",
            "tests_run",
            "tests_passed",
            "tests_skipped",
            "tests_failed",
            "stderr_warning_count",
            "stderr_classification",
            "stdout_sha256",
            "stdout_bytes",
            "stderr_sha256",
            "stderr_bytes",
        ],
    );
    assert_keys(
        &executions[3],
        &[
            "id",
            "kind",
            "program",
            "argv",
            "cwd",
            "fixture",
            "fixture_sha256",
            "fixture_bytes",
            "exit_code",
            "tests_run",
            "tests_passed",
            "tests_skipped",
            "tests_failed",
            "stdout_sha256",
            "stdout_bytes",
            "stderr_sha256",
            "stderr_bytes",
        ],
    );
    assert_keys(
        &executions[4],
        &[
            "id",
            "kind",
            "program",
            "argv",
            "cwd",
            "fixture",
            "fixture_sha256",
            "fixture_bytes",
            "exit_code",
            "pages_processed",
            "observed_page_info",
            "stdout_sha256",
            "stdout_bytes",
            "stderr_sha256",
            "stderr_bytes",
        ],
    );

    for execution in [&executions[2], &executions[3]] {
        assert_eq!(
            integer(execution, "tests_run"),
            integer(execution, "tests_passed")
                + integer(execution, "tests_skipped")
                + integer(execution, "tests_failed")
        );
        assert_eq!(integer(execution, "tests_failed"), 0);
    }
    assert_eq!(integer(&executions[2], "tests_run"), 1034);
    assert_eq!(integer(&executions[3], "tests_run"), 1);
    assert_eq!(integer(&executions[4], "pages_processed"), 1);
    assert_eq!(quoted(&executions[3], "program"), "$DEPOT_TOOLS/vpython3");
    assert_eq!(quoted(&executions[4], "cwd"), "$TMPDIR");

    for record in std::iter::once(&top).chain(executions.iter()) {
        for (key, value) in record {
            if key.ends_with("_sha256") {
                assert_sha256(quoted_value(value));
            }
        }
    }
}

#[test]
fn repository_readiness_is_data_ledger_bound_but_not_a_baseline() {
    let ledger = include_str!("../../../docs/traceability/data-ledger.toml");
    let record = array_record(
        ledger,
        "data",
        "baseline.evidence.pdfium-c040cf96-macos-arm64-build-readiness-v1",
    );
    for expected in [
        "kind = \"project-authored-upstream-build-readiness-evidence\"",
        "source = \"tools/baseline/pdfium/evidence/pdfium-c040cf96-macos-arm64-build-readiness-v1.toml\"",
        "source_hash = \"sha256:b61d01628d96908a38edb94b4d08e810dfbd83c27dec2e5629e8449e941942a7\"",
        "license_expression = \"LicenseRef-PDF.rs-SelfAuthored-Test\"",
        "contains_personal_data = false",
        "validated_by = \"cargo test --package pdf-rs-baseline --test repository_readiness\"",
        "owner = \"baseline-release\"",
    ] {
        assert_line(record, expected);
    }

    let baseline = include_str!("../../../docs/traceability/baseline-ledger.toml");
    assert_line(baseline, "status = \"initial\"");
    assert_line(baseline, "baseline = []");
    assert!(!baseline.contains("[[baseline]]"));

    let adapter = include_str!("../pdfium/adapter.toml");
    for key in [
        "runner_executable",
        "build_hash",
        "build_flags_hash",
        "environment_hash",
        "invocation_hash",
        "isolation_profile",
        "license_manifest_hash",
        "fonts_hash",
        "color_hash",
        "platform",
    ] {
        assert_line(
            adapter,
            &format!("{key} = \"REQUIRED_BEFORE_DIFFERENTIAL\""),
        );
    }
}

#[test]
fn repository_docs_do_not_overstate_pdfium_readiness() {
    let readme = normalized(include_str!("../pdfium/README.md"));
    assert!(readme.contains(REPORT_ID));
    assert!(readme.contains("It is only upstream build-readiness evidence."));
    assert!(readme.contains("The baseline ledger therefore remains empty"));

    let provenance = normalized(include_str!("../PROVENANCE.md"));
    assert!(provenance.contains("did not run the baseline protocol or adapter"));
    assert!(provenance.contains("produce an O4 comparison"));
    assert!(provenance.contains("measure performance"));
    assert!(provenance.contains("register a baseline"));
    assert!(!provenance.contains("No external executable was run and"));

    let ci = include_str!("../../../scripts/ci.sh");
    assert!(!ci.contains("pdfium"));
    assert!(!include_str!("../../../docs/traceability/feature-map.toml").contains(REPORT_ID));
    assert!(!include_str!("../../../docs/traceability/spec-map.toml").contains(REPORT_ID));
}

fn parse_report(input: &str) -> (Record, Vec<Record>) {
    let mut top = Record::new();
    let mut executions = Vec::new();
    let mut current: Option<Record> = None;

    for (index, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[execution]]" {
            if let Some(record) = current.take() {
                executions.push(record);
            }
            current = Some(Record::new());
            continue;
        }
        assert!(
            !line.starts_with('['),
            "unexpected table at line {}",
            index + 1
        );
        let (key, value) = line
            .split_once(" = ")
            .unwrap_or_else(|| panic!("non-canonical assignment at line {}", index + 1));
        let target = current.as_mut().unwrap_or(&mut top);
        assert!(
            target.insert(key.to_owned(), value.to_owned()).is_none(),
            "duplicate key {key} at line {}",
            index + 1
        );
    }
    if let Some(record) = current {
        executions.push(record);
    }
    (top, executions)
}

fn assert_keys(record: &Record, expected: &[&str]) {
    let actual: BTreeSet<&str> = record.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = expected.iter().copied().collect();
    assert_eq!(actual, expected);
}

fn raw<'a>(record: &'a Record, key: &str) -> &'a str {
    record
        .get(key)
        .unwrap_or_else(|| panic!("missing key {key}"))
}

fn quoted<'a>(record: &'a Record, key: &str) -> &'a str {
    quoted_value(raw(record, key))
}

fn quoted_value(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or_else(|| panic!("expected quoted value: {value}"))
}

fn integer(record: &Record, key: &str) -> u64 {
    raw(record, key)
        .parse()
        .unwrap_or_else(|_| panic!("expected integer for {key}"))
}

fn assert_sha256(value: &str) {
    let digest = value
        .strip_prefix("sha256:")
        .unwrap_or_else(|| panic!("missing sha256 prefix: {value}"));
    assert_eq!(digest.len(), 64);
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "non-canonical SHA-256: {value}"
    );
}

fn array_record<'a>(document: &'a str, table: &str, id: &str) -> &'a str {
    let marker = format!("[[{table}]]");
    let id_line = format!("id = \"{id}\"");
    document
        .split(&marker)
        .skip(1)
        .find(|record| record.lines().any(|line| line.trim() == id_line))
        .unwrap_or_else(|| panic!("missing {table} record for {id}"))
}

fn assert_line(document: &str, expected: &str) {
    assert!(
        document.lines().any(|line| line.trim() == expected),
        "missing line: {expected}"
    );
}

fn normalized(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn hex_digest(digest: &[u8; 32]) -> String {
    let mut value = String::with_capacity(64);
    for byte in digest {
        write!(&mut value, "{byte:02x}").unwrap();
    }
    value
}
