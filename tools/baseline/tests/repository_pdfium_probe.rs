use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use pdf_rs_baseline::{BaselineDescriptor, descriptor_identity};
use pdf_rs_digest::sha256;
use pdf_rs_generate::{GenerateLimits, compile_dsl};

#[path = "support/pdfium_probe.rs"]
mod probe;

use probe::*;

const REPORT_ID: &str = "pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1";
const REPORT_HASH: &str = "0f098a7202958f8beb75f28c238afa530cf3aba244d07cb6f75fa2701a77ff7c";
const REPORT: &[u8] =
    include_bytes!("../pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1.toml");
const BUILD_DEFINITION: &[u8] = include_bytes!("../pdfium/helper/BUILD.gn");
const HELPER_SOURCE: &[u8] = include_bytes!("../pdfium/helper/pdf_rs_pdfium_adapter.cc");
const ROOT_OVERLAY: &[u8] = include_bytes!("../pdfium/helper/pdfium-root.patch");
const FIXTURE_SOURCE: &[u8] =
    include_bytes!("../../../tests/cases/infrastructure/synthetic-failure-bundle-001/source.dsl");

type Record = BTreeMap<String, String>;

#[test]
fn o4_pixel_probe_is_canonical_hash_bound_and_ineligible() {
    assert_eq!(hex_digest(&sha256(REPORT).unwrap()), REPORT_HASH);
    let report_text = std::str::from_utf8(REPORT).unwrap();
    assert!(report_text.ends_with('\n'));
    assert!(!report_text.contains('\r'));
    for forbidden in ["/Users/", "/private/tmp/", "HOME=", "lance"] {
        assert!(
            !report_text.contains(forbidden),
            "unredacted value: {forbidden}"
        );
    }

    let report = parse_report(report_text);
    assert_keys(
        &report,
        &[
            "schema",
            "id",
            "evidence_class",
            "recorded_at",
            "engine",
            "upstream_repository",
            "upstream_revision",
            "upstream_build_readiness_evidence",
            "source_checkout_mode",
            "source_checkout_dirty_scope",
            "platform",
            "os_version",
            "kernel_release",
            "xcode_version",
            "gn_version",
            "ninja_version",
            "adapter_profile",
            "runner_schema",
            "adapter_exercised",
            "public_pdfium_c_api_only",
            "protocol_identity_verified",
            "executable_hash_verified_before_runner_construction",
            "widget_overlay",
            "helper_build_definition",
            "helper_build_definition_sha256",
            "helper_source",
            "helper_source_sha256",
            "helper_root_overlay",
            "helper_root_overlay_sha256",
            "helper_executable_sha256",
            "helper_executable_bytes",
            "descriptor_build_hash_scope",
            "build_args",
            "build_flags_sha256",
            "build_args_command_sha256",
            "gn_targets_generated",
            "helper_build_steps",
            "helper_build_exit_code",
            "helper_build_outcome",
            "helper_build_reverification",
            "helper_build_reverification_exit_code",
            "helper_build_reverification_ninja_state",
            "strict_warning_policy",
            "environment_policy",
            "argv",
            "working_directory",
            "isolation_profile",
            "max_request_bytes",
            "max_stdout_bytes",
            "max_stderr_bytes",
            "watchdog_ms",
            "stderr_policy",
            "environment_declaration",
            "environment_sha256",
            "invocation_sha256",
            "invocation_reproducibility",
            "license_declaration",
            "license_manifest_sha256",
            "fonts_declaration",
            "fonts_sha256",
            "color_declaration",
            "color_sha256",
            "descriptor_identity_sha256",
            "runtime_closure_status",
            "license_manifest_status",
            "font_fingerprint_status",
            "color_fingerprint_status",
            "sandbox_status",
            "executable_replacement_protection_status",
            "manual_test",
            "manual_test_count",
            "manual_test_passed",
            "helper_process_runs",
            "fixture_id",
            "fixture",
            "fixture_sha256",
            "fixture_bytes",
            "fixture_hash_verified",
            "corpus_manifest_id",
            "corpus_manifest_sha256",
            "corpus_manifest_exercised",
            "fixture_page",
            "fixture_width",
            "fixture_height",
            "fixture_rgba_format",
            "fixture_rgba_bytes",
            "fixture_case_render_geometry_exercised",
            "fixture_case_render_profile_exercised",
            "fixture_case_oracle_contract_exercised",
            "blank_probe_runs",
            "blank_repeat_outputs_identical",
            "blank_observation_outcome",
            "blank_parse_channel",
            "blank_scene_channel",
            "blank_text_channel",
            "blank_pixel_channel",
            "blank_observed_rgba_sha256",
            "blank_analytic_check_kind",
            "blank_analytic_check_derivation",
            "blank_analytic_check_review_status",
            "blank_analytic_expected_rgba_sha256",
            "blank_comparison_exact",
            "blank_different_pixels",
            "blank_different_channels",
            "blank_max_channel_delta",
            "blank_total_absolute_delta",
            "color_probe_id",
            "color_probe_runs",
            "color_probe_generator",
            "color_probe_source_sha256",
            "color_probe_source_bytes",
            "color_probe_pdf_sha256",
            "color_probe_pdf_bytes",
            "color_probe_width",
            "color_probe_height",
            "color_probe_rgba_bytes",
            "color_probe_observed_rgba_sha256",
            "color_probe_analytic_check_kind",
            "color_probe_analytic_check_derivation",
            "color_probe_analytic_check_review_status",
            "color_probe_analytic_expected_rgba_sha256",
            "color_probe_comparison_exact",
            "color_probe_different_pixels",
            "color_probe_different_channels",
            "color_probe_max_channel_delta",
            "color_probe_total_absolute_delta",
            "page_out_of_range_runs",
            "page_out_of_range_outcome",
            "malformed_pdf_runs",
            "malformed_pdf_outcome",
            "oracle_authority",
            "native_engine_exercised",
            "native_vs_pdfium_differential",
            "product_correctness_eligible",
            "performance_eligible",
            "differential_eligible",
            "baseline_registration_eligible",
            "release_gate_eligible",
            "verdict",
            "raw_logs_committed",
            "helper_binary_committed",
            "pdfium_source_committed",
            "fixture_pdf_committed",
            "rgba_bytes_committed",
            "upstream_data_committed",
        ],
    );

    assert_eq!(integer(&report, "schema"), 1);
    assert_eq!(quoted(&report, "id"), REPORT_ID);
    assert_eq!(
        quoted(&report, "evidence_class"),
        "external-o4-pixel-adapter-probe"
    );
    assert_eq!(quoted(&report, "engine"), "pdfium");
    assert_eq!(quoted(&report, "upstream_revision"), PDFIUM_REVISION);
    assert_eq!(quoted(&report, "platform"), "macos-arm64");
    assert_eq!(
        quoted(&report, "adapter_profile"),
        "pdfium-public-c-api-pixel-only-v1"
    );
    assert_eq!(integer(&report, "runner_schema"), 2);
    for key in [
        "adapter_exercised",
        "public_pdfium_c_api_only",
        "protocol_identity_verified",
        "executable_hash_verified_before_runner_construction",
        "fixture_hash_verified",
        "fixture_case_render_geometry_exercised",
        "blank_repeat_outputs_identical",
        "blank_comparison_exact",
        "color_probe_comparison_exact",
    ] {
        assert!(boolean(&report, key), "{key} must be true");
    }
    for key in [
        "corpus_manifest_exercised",
        "fixture_case_render_profile_exercised",
        "fixture_case_oracle_contract_exercised",
        "native_engine_exercised",
        "native_vs_pdfium_differential",
        "product_correctness_eligible",
        "performance_eligible",
        "differential_eligible",
        "baseline_registration_eligible",
        "release_gate_eligible",
        "raw_logs_committed",
        "helper_binary_committed",
        "pdfium_source_committed",
        "fixture_pdf_committed",
        "rgba_bytes_committed",
        "upstream_data_committed",
    ] {
        assert!(!boolean(&report, key), "{key} must be false");
    }
    assert_eq!(quoted(&report, "oracle_authority"), "O4");
    assert_eq!(quoted(&report, "verdict"), "not-evaluated");
    for key in [
        "runtime_closure_status",
        "license_manifest_status",
        "font_fingerprint_status",
        "color_fingerprint_status",
    ] {
        assert_eq!(quoted(&report, key), "incomplete");
    }
    assert_eq!(
        quoted(&report, "sandbox_status"),
        "direct-child-supervision-only"
    );
    assert_eq!(
        quoted(&report, "executable_replacement_protection_status"),
        "not-established"
    );

    for (key, value) in &report {
        if key.ends_with("_sha256") {
            assert_sha256(quoted_value(value));
        }
    }
    assert_report_digest(&report, "helper_build_definition_sha256", BUILD_DEFINITION);
    assert_report_digest(&report, "helper_source_sha256", HELPER_SOURCE);
    assert_report_digest(&report, "helper_root_overlay_sha256", ROOT_OVERLAY);
    assert_eq!(
        quoted(&report, "build_args").replace("\\\"", "\""),
        PDFIUM_BUILD_ARGS
    );
    assert_report_digest(&report, "build_flags_sha256", PDFIUM_ARGS_GN.as_bytes());
    assert_report_digest(
        &report,
        "build_args_command_sha256",
        PDFIUM_BUILD_ARGS.as_bytes(),
    );
    assert_eq!(
        quoted(&report, "helper_executable_sha256"),
        format!("sha256:{EXPECTED_HELPER_SHA256}")
    );
    assert_eq!(
        integer(&report, "helper_executable_bytes"),
        u64::try_from(EXPECTED_HELPER_BYTES).unwrap()
    );
    for (declaration, digest) in [
        ("environment_declaration", "environment_sha256"),
        ("license_declaration", "license_manifest_sha256"),
        ("fonts_declaration", "fonts_sha256"),
        ("color_declaration", "color_sha256"),
    ] {
        assert_report_digest(&report, digest, quoted(&report, declaration).as_bytes());
    }
    assert_eq!(
        quoted(&report, "environment_declaration"),
        ENVIRONMENT_DECLARATION
    );
    assert_eq!(quoted(&report, "license_declaration"), LICENSE_DECLARATION);
    assert_eq!(quoted(&report, "fonts_declaration"), FONTS_DECLARATION);
    assert_eq!(quoted(&report, "color_declaration"), COLOR_DECLARATION);

    let descriptor = BaselineDescriptor {
        id: quoted(&report, "adapter_profile").into(),
        engine: quoted(&report, "engine").into(),
        upstream_revision: quoted(&report, "upstream_revision").into(),
        build_hash: decoded_digest(&report, "helper_executable_sha256"),
        build_flags_hash: decoded_digest(&report, "build_flags_sha256"),
        environment_hash: decoded_digest(&report, "environment_sha256"),
        invocation_hash: decoded_digest(&report, "invocation_sha256"),
        license_manifest_hash: decoded_digest(&report, "license_manifest_sha256"),
        fonts_hash: decoded_digest(&report, "fonts_sha256"),
        color_hash: decoded_digest(&report, "color_sha256"),
        platform: quoted(&report, "platform").into(),
    };
    assert_eq!(
        descriptor_identity(&descriptor).unwrap(),
        decoded_digest(&report, "descriptor_identity_sha256")
    );

    assert_eq!(integer(&report, "helper_build_exit_code"), 0);
    assert_eq!(quoted(&report, "helper_build_outcome"), "pass");
    assert_eq!(integer(&report, "helper_build_reverification_exit_code"), 0);
    assert_eq!(
        quoted(&report, "helper_build_reverification_ninja_state"),
        "no-work-required"
    );
    assert_eq!(integer(&report, "manual_test_count"), 1);
    assert_eq!(integer(&report, "manual_test_passed"), 1);
    assert_eq!(integer(&report, "max_stdout_bytes"), 176);
    assert_eq!(integer(&report, "watchdog_ms"), 2000);
    assert_eq!(raw(&report, "argv"), "[]");

    let fixture = compile_dsl(FIXTURE_SOURCE, GenerateLimits::default()).unwrap();
    assert_eq!(
        quoted(&report, "fixture_sha256"),
        format!("sha256:{}", hex_digest(&fixture.output_sha256()))
    );
    assert_eq!(
        quoted(&report, "fixture_sha256"),
        format!("sha256:{FIXTURE_SOURCE_HASH}")
    );
    assert_eq!(
        integer(&report, "fixture_bytes"),
        u64::try_from(fixture.bytes().len()).unwrap()
    );
    assert_eq!(integer(&report, "fixture_width"), u64::from(WIDTH));
    assert_eq!(integer(&report, "fixture_height"), u64::from(HEIGHT));
    assert_eq!(
        integer(&report, "fixture_rgba_bytes"),
        u64::try_from(rgba_len()).unwrap()
    );
    assert_eq!(integer(&report, "blank_probe_runs"), BLANK_PROBE_RUNS);
    assert_eq!(quoted(&report, "blank_parse_channel"), "unsupported");
    assert_eq!(quoted(&report, "blank_scene_channel"), "unsupported");
    assert_eq!(quoted(&report, "blank_text_channel"), "unsupported");
    assert_eq!(quoted(&report, "blank_pixel_channel"), "produced");
    assert_eq!(
        quoted(&report, "blank_observed_rgba_sha256"),
        quoted(&report, "blank_analytic_expected_rgba_sha256")
    );
    assert_eq!(
        quoted(&report, "blank_observed_rgba_sha256"),
        format!("sha256:{EXPECTED_WHITE_RGBA_HASH}")
    );
    assert_report_digest(
        &report,
        "blank_observed_rgba_sha256",
        &vec![255; rgba_len()],
    );
    assert_zero_diff(&report, "blank");

    assert_eq!(
        integer(&report, "color_probe_source_bytes"),
        u64::try_from(COLOR_PROBE_DSL.len()).unwrap()
    );
    assert_eq!(
        integer(&report, "color_probe_source_bytes"),
        u64::try_from(COLOR_PROBE_SOURCE_BYTES).unwrap()
    );
    assert_eq!(
        quoted(&report, "color_probe_source_sha256"),
        format!("sha256:{COLOR_PROBE_SOURCE_HASH}")
    );
    assert_report_digest(
        &report,
        "color_probe_source_sha256",
        COLOR_PROBE_DSL.as_bytes(),
    );
    let color_probe = compile_dsl(COLOR_PROBE_DSL.as_bytes(), GenerateLimits::default()).unwrap();
    assert_eq!(
        quoted(&report, "color_probe_pdf_sha256"),
        format!("sha256:{}", hex_digest(&color_probe.output_sha256()))
    );
    assert_eq!(
        quoted(&report, "color_probe_pdf_sha256"),
        format!("sha256:{COLOR_PROBE_PDF_HASH}")
    );
    assert_eq!(
        integer(&report, "color_probe_pdf_bytes"),
        u64::try_from(color_probe.bytes().len()).unwrap()
    );
    assert_eq!(
        integer(&report, "color_probe_pdf_bytes"),
        u64::try_from(COLOR_PROBE_PDF_BYTES).unwrap()
    );
    assert_eq!(integer(&report, "color_probe_width"), u64::from(WIDTH));
    assert_eq!(integer(&report, "color_probe_height"), u64::from(HEIGHT));
    assert_eq!(
        integer(&report, "color_probe_rgba_bytes"),
        u64::try_from(rgba_len()).unwrap()
    );
    assert_eq!(
        quoted(&report, "color_probe_observed_rgba_sha256"),
        quoted(&report, "color_probe_analytic_expected_rgba_sha256")
    );
    assert_eq!(
        quoted(&report, "color_probe_observed_rgba_sha256"),
        format!("sha256:{COLOR_PROBE_RGBA_HASH}")
    );
    assert_report_digest(
        &report,
        "color_probe_observed_rgba_sha256",
        &analytic_quadrants(),
    );
    assert_zero_diff(&report, "color_probe");
    let counted_runs = integer(&report, "blank_probe_runs")
        + integer(&report, "color_probe_runs")
        + integer(&report, "page_out_of_range_runs")
        + integer(&report, "malformed_pdf_runs");
    assert_eq!(counted_runs, HELPER_PROCESS_RUNS);
    assert_eq!(integer(&report, "helper_process_runs"), counted_runs);
    assert_eq!(
        quoted(&report, "page_out_of_range_outcome"),
        "RPE-BASELINE-0006"
    );
    assert_eq!(
        quoted(&report, "malformed_pdf_outcome"),
        "RPE-BASELINE-0006"
    );
}

#[test]
fn o4_probe_is_data_bound_but_not_registered_or_required_by_ci() {
    let report = parse_report(std::str::from_utf8(REPORT).unwrap());
    let ledger = include_str!("../../../docs/traceability/data-ledger.toml");
    let record = array_record(
        ledger,
        "data",
        "baseline.evidence.pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1",
    );
    for expected in [
        "kind = \"project-authored-external-o4-adapter-probe-evidence\"",
        "source = \"tools/baseline/pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1.toml\"",
        "source_hash = \"sha256:0f098a7202958f8beb75f28c238afa530cf3aba244d07cb6f75fa2701a77ff7c\"",
        "contains_personal_data = false",
        "validated_by = \"cargo test --package pdf-rs-baseline --test repository_pdfium_probe\"",
        "owner = \"baseline-release\"",
    ] {
        assert_line(record, expected);
    }

    let manifest = include_bytes!("../../../tests/corpus/manifests/t0-bootstrap-v1.toml");
    assert_eq!(
        quoted(&report, "corpus_manifest_sha256"),
        format!("sha256:{}", hex_digest(&sha256(manifest).unwrap()))
    );
    let manifest_text = std::str::from_utf8(manifest).unwrap();
    assert_line(
        manifest_text,
        "path = \"tests/cases/infrastructure/synthetic-failure-bundle-001/input.pdf\"",
    );
    assert_line(
        manifest_text,
        "source = \"fixture.infrastructure.synthetic-failure-bundle-001\"",
    );
    assert_line(
        manifest_text,
        "sha256 = \"sha256:9c819e549afcc89d03b380c3c1bd47128aa2b70ae30a35245e6a0e30132875db\"",
    );
    let case =
        include_str!("../../../tests/cases/infrastructure/synthetic-failure-bundle-001/case.toml");
    assert_line(case, "width = 4");
    assert_line(case, "height = 4");
    assert_line(case, "color_profile = \"srgb-reference-v1\"");
    assert!(!case.contains("one RGBA pixel with two changed channels"));
    let oracle = include_str!(
        "../../../tests/cases/infrastructure/synthetic-failure-bundle-001/expected/oracle.md"
    );
    assert!(oracle.contains("synthetic artifact constructors"));
    assert!(!oracle.contains("white RGBA"));

    let baseline = include_str!("../../../docs/traceability/baseline-ledger.toml");
    assert_line(baseline, "status = \"initial\"");
    assert_line(baseline, "baseline = []");
    assert!(!baseline.contains("[[baseline]]"));

    let adapter = include_str!("../pdfium/adapter.toml");
    assert_line(
        adapter,
        "probe_evidence = \"evidence/pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1.toml\"",
    );
    assert_line(
        adapter,
        "registration_status = \"blocked-incomplete-closure-and-sandbox\"",
    );
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

    let readme = normalized(include_str!("../pdfium/README.md"));
    let provenance = normalized(include_str!("../PROVENANCE.md"));
    for document in [&readme, &provenance] {
        assert!(document.contains(REPORT_ID));
        assert!(
            document.contains("not a Native/PDFium differential")
                || document.contains("not a Native/PDFium comparison")
        );
    }
    assert!(readme.contains("eligibility fields remain false"));
    assert!(provenance.contains("No Native engine was run"));

    let feature_map = include_str!("../../../docs/traceability/feature-map.toml");
    let spec_map = include_str!("../../../docs/traceability/spec-map.toml");
    for test in [
        "tools/baseline::pdfium_adapter_contract",
        "tools/baseline::pdfium_real_adapter",
        "tools/baseline::repository_pdfium_probe",
    ] {
        assert!(feature_map.contains(test));
        assert!(spec_map.contains(test));
    }
    assert!(spec_map.contains("without Native pixels"));
    assert!(spec_map.contains("one real, non-gating Native/PDFium O4 comparison"));

    let ci = include_str!("../../../scripts/ci.sh");
    assert!(!ci.contains("PDF_RS_PDFIUM_ADAPTER"));
    assert!(!ci.contains("pdf_rs_pdfium_adapter"));
}

fn parse_report(input: &str) -> Record {
    let mut report = Record::new();
    for (index, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
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
        assert!(
            report.insert(key.into(), value.into()).is_none(),
            "duplicate key {key} at line {}",
            index + 1
        );
    }
    report
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

fn boolean(record: &Record, key: &str) -> bool {
    match raw(record, key) {
        "true" => true,
        "false" => false,
        value => panic!("expected boolean for {key}, got {value}"),
    }
}

fn assert_report_digest(record: &Record, key: &str, bytes: &[u8]) {
    assert_eq!(
        quoted(record, key),
        format!("sha256:{}", hex_digest(&sha256(bytes).unwrap()))
    );
}

fn decoded_digest(record: &Record, key: &str) -> [u8; 32] {
    let value = quoted(record, key)
        .strip_prefix("sha256:")
        .unwrap_or_else(|| panic!("missing sha256 prefix for {key}"));
    let mut output = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
    }
    output
}

fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => panic!("invalid lowercase hex"),
    }
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

fn assert_zero_diff(record: &Record, prefix: &str) {
    assert!(boolean(record, &format!("{prefix}_comparison_exact")));
    assert_eq!(integer(record, &format!("{prefix}_different_pixels")), 0);
    assert_eq!(integer(record, &format!("{prefix}_different_channels")), 0);
    assert_eq!(
        raw(record, &format!("{prefix}_max_channel_delta")),
        "[0, 0, 0, 0]"
    );
    assert_eq!(
        integer(record, &format!("{prefix}_total_absolute_delta")),
        0
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
