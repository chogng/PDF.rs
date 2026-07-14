use std::collections::BTreeMap;
use std::fmt::Write as _;

use pdf_rs_baseline::{BaselineDescriptor, descriptor_identity};
use pdf_rs_digest::sha256;

const REPORT_ID: &str = "pdfium-c040cf96-macos-arm64-o4-page-count-boundary-performance-probe-v1";
const REPORT_HASH: &str = "bdd25b8d8843e62b500987a87080f0956ac8fe6ee7cdc351b6dc1689acd06e31";
const DATA_LEDGER_VERSION: &str = "0.10.0";
const REPORT: &[u8] = include_bytes!(
    "../pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-page-count-boundary-performance-probe-v1.toml"
);
const PREREQUISITE_EVIDENCE: &[u8] = include_bytes!(
    "../pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-page-count-differential-probe-v1.toml"
);
const ADAPTER_CONFIG: &[u8] = include_bytes!("../pdfium/page_count_adapter.toml");
const BUILD_DEFINITION: &[u8] = include_bytes!("../pdfium/helper/page_count.BUILD.gn");
const HELPER_SOURCE: &[u8] = include_bytes!("../pdfium/helper/pdf_rs_pdfium_page_count_probe.cc");
const ROOT_OVERLAY: &[u8] = include_bytes!("../pdfium/helper/pdfium-page-count-root.patch");
const HOST_ADAPTER: &[u8] = include_bytes!("../src/pdfium_page_count.rs");
const MEASUREMENT_HARNESS: &[u8] = include_bytes!("pdfium_page_count_performance.rs");

type Record = BTreeMap<String, String>;

#[derive(Debug, Eq, PartialEq)]
struct Summary {
    minimum_ns: u64,
    median_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    maximum_ns: u64,
    median_ci95_low_ns: u64,
    median_ci95_high_ns: u64,
}

#[test]
fn page_count_boundary_performance_evidence_is_raw_sample_and_identity_bound() {
    assert_eq!(hex(&sha256(REPORT).unwrap()), REPORT_HASH);
    let text = std::str::from_utf8(REPORT).unwrap();
    assert!(text.ends_with('\n'));
    assert!(!text.contains('\r'));
    for forbidden in [
        "/Users/",
        "/private/tmp/",
        "HOME=",
        "lance",
        "Serial Number",
        "Hardware UUID",
        "Provisioning UDID",
    ] {
        assert!(!text.contains(forbidden), "unredacted value: {forbidden}");
    }
    let report = parse_report(text);

    assert_eq!(integer(&report, "schema"), 1);
    assert_eq!(quoted(&report, "id"), REPORT_ID);
    assert_eq!(
        quoted(&report, "evidence_class"),
        "external-o4-page-count-boundary-performance-and-behavior-probe"
    );
    assert_eq!(quoted(&report, "engine"), "pdfium");
    assert_eq!(
        quoted(&report, "upstream_revision"),
        "c040cf96106a87220b814a1a892649cf2d7f1934"
    );
    assert_eq!(
        quoted(&report, "pdf_rs_revision"),
        "0f6cbde39e8e49dbcd3f784a07684a2ff7302c2c"
    );
    assert_eq!(quoted(&report, "platform"), "macos-arm64");
    assert_eq!(quoted(&report, "hardware_model"), "Mac Studio Mac14,13");
    assert_eq!(quoted(&report, "cpu"), "Apple M2 Max");
    assert_eq!(quoted(&report, "memory"), "32 GB");
    assert_eq!(quoted(&report, "power_source"), "AC Power");
    assert_eq!(
        quoted(&report, "pdf_rs_build_profile"),
        "cargo-release-optimized"
    );
    assert_eq!(
        quoted(&report, "adapter_profile"),
        "pdfium-public-c-api-page-count-v1"
    );
    assert_eq!(integer(&report, "runner_schema"), 2);

    for key in [
        "public_pdfium_c_api_only",
        "protocol_identity_verified",
        "executable_hash_verified_before_runner_construction",
        "fixture_hash_verified",
        "behavior_counts_exact",
        "behavior_repeat_outputs_identical",
        "native_vs_pdfium_behavior_differential",
        "performance_observation_recorded",
        "raw_timing_samples_committed",
    ] {
        assert!(boolean(&report, key), "{key} must be true");
    }
    for key in [
        "measurement_scopes_equal",
        "performance_scope_comparable",
        "performance_eligible",
        "correctness_eligible",
        "product_correctness_eligible",
        "differential_eligible",
        "baseline_registration_eligible",
        "release_gate_eligible",
        "raw_logs_committed",
        "fixture_pdf_committed",
        "helper_binary_committed",
        "pdfium_source_committed",
        "page_count_json_bytes_committed",
        "upstream_data_committed",
    ] {
        assert!(!boolean(&report, key), "{key} must be false");
    }
    assert_eq!(quoted(&report, "oracle_authority"), "O4");
    assert_eq!(
        quoted(&report, "measurement_scope_classification"),
        "different-development-boundary-latencies-not-engine-kernel-parity"
    );
    assert!(
        quoted(&report, "performance_ineligibility_reasons")
            .contains("different measurement scopes")
    );

    assert_digest(
        &report,
        "prerequisite_page_count_evidence_sha256",
        PREREQUISITE_EVIDENCE,
    );
    assert_digest(&report, "adapter_config_sha256", ADAPTER_CONFIG);
    assert_digest(&report, "helper_build_definition_sha256", BUILD_DEFINITION);
    assert_digest(&report, "helper_source_sha256", HELPER_SOURCE);
    assert_digest(&report, "helper_root_overlay_sha256", ROOT_OVERLAY);
    assert_digest(&report, "host_adapter_source_sha256", HOST_ADAPTER);
    assert_digest(&report, "measurement_harness_sha256", MEASUREMENT_HARNESS);
    for (declaration, digest) in [
        ("environment_declaration", "environment_sha256"),
        ("license_declaration", "license_manifest_sha256"),
        ("fonts_declaration", "fonts_sha256"),
        ("color_declaration", "color_sha256"),
    ] {
        assert_digest(&report, digest, quoted(&report, declaration).as_bytes());
    }
    for (key, value) in &report {
        if key.ends_with("_sha256") {
            assert_sha256(quoted_value(value));
        }
    }

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

    assert_eq!(integer(&report, "helper_executable_bytes"), 3_683_040);
    assert_eq!(integer(&report, "helper_reverification_exit_code"), 0);
    assert_eq!(
        quoted(&report, "helper_reverification_ninja_state"),
        "no-work-required"
    );
    assert_eq!(integer(&report, "max_request_bytes"), 262_144);
    assert_eq!(integer(&report, "max_stdout_bytes"), 176);
    assert_eq!(integer(&report, "max_stderr_bytes"), 4_096);
    assert_eq!(integer(&report, "watchdog_ms"), 2_000);
    assert_eq!(integer(&report, "trial_count"), 2);
    assert_eq!(integer(&report, "warmup_runs_per_engine_per_trial"), 5);
    assert_eq!(integer(&report, "sample_runs_per_engine_per_trial"), 50);
    assert_eq!(integer(&report, "timed_sample_runs_per_engine"), 100);
    assert_eq!(
        integer(&report, "validated_runs_per_engine_including_warmups"),
        110
    );
    assert_eq!(integer(&report, "helper_process_runs"), 110);
    assert_eq!(integer(&report, "fixture_bytes"), 15_137);
    assert_eq!(integer(&report, "fixture_page_count"), 128);

    for trial in [1_u64, 2] {
        validate_trial(&report, trial);
    }
    assert_eq!(integer(&report, "behavior_native_count"), 128);
    assert_eq!(integer(&report, "behavior_pdfium_count"), 128);
    assert_eq!(integer(&report, "behavior_timed_comparisons"), 100);
    assert_eq!(integer(&report, "behavior_warmup_comparisons"), 10);
    assert_eq!(integer(&report, "behavior_differences"), 0);
}

#[test]
fn page_count_boundary_performance_evidence_is_traced_but_not_registered() {
    let ledger = include_str!("../../../docs/traceability/data-ledger.toml");
    assert_line(ledger, &format!("version = \"{DATA_LEDGER_VERSION}\""));
    let record = array_record(
        ledger,
        "data",
        "baseline.evidence.pdfium-c040cf96-macos-arm64-o4-page-count-boundary-performance-probe-v1",
    );
    for expected in [
        "kind = \"project-authored-external-o4-page-count-boundary-performance-evidence\"".to_owned(),
        "source = \"tools/baseline/pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-page-count-boundary-performance-probe-v1.toml\"".to_owned(),
        format!("source_hash = \"sha256:{REPORT_HASH}\""),
        "contains_personal_data = false".to_owned(),
        "validated_by = \"cargo test --package pdf-rs-baseline --test repository_pdfium_page_count_performance_probe\"".to_owned(),
        "owner = \"baseline-release\"".to_owned(),
    ] {
        assert!(record.lines().any(|line| line.trim() == expected));
    }

    let baseline_ledger = include_str!("../../../docs/traceability/baseline-ledger.toml");
    assert_line(baseline_ledger, "baseline = []");

    let feature_map = include_str!("../../../docs/traceability/feature-map.toml");
    let page_count = array_record(feature_map, "feature", "core.strict-page-count");
    let boundary = array_record(feature_map, "feature", "quality.baseline-protocol-boundary");
    for record in [page_count, boundary] {
        assert!(record.contains("tools/baseline::pdfium_page_count_performance"));
        assert!(record.contains("tools/baseline::repository_pdfium_page_count_performance_probe"));
        assert!(record.contains("state = \"PLANNED\""));
    }
    let spec_map = include_str!("../../../docs/traceability/spec-map.toml");
    assert!(spec_map.contains("tools/baseline::pdfium_page_count_performance"));
    assert!(spec_map.contains("tools/baseline::repository_pdfium_page_count_performance_probe"));

    let readme = include_str!("../pdfium/README.md");
    let provenance = include_str!("../PROVENANCE.md");
    for document in [readme, provenance] {
        assert!(document.contains(REPORT_ID));
        assert!(document.contains("performance_eligible=false"));
        assert!(document.contains("128"));
    }

    let ci = include_str!("../../../scripts/ci.sh");
    assert!(!ci.contains("pdfium_page_count_performance"));
    assert!(!ci.contains("pdf_rs_pdfium_page_count_probe"));
}

fn validate_trial(report: &Record, trial: u64) {
    let native = series(report, &format!("trial_{trial}_native_raw_ns"));
    let pdfium = series(report, &format!("trial_{trial}_pdfium_raw_ns"));
    assert_eq!(native.len(), 50);
    assert_eq!(pdfium.len(), 50);
    assert!(native.iter().all(|&sample| sample > 0));
    assert!(pdfium.iter().all(|&sample| sample > 0));

    let native_summary = summarize(&native);
    let pdfium_summary = summarize(&pdfium);
    assert_summary(report, &format!("trial_{trial}_native"), &native_summary);
    assert_summary(report, &format!("trial_{trial}_pdfium"), &pdfium_summary);
    let ratio_milli = pdfium_summary
        .median_ns
        .checked_mul(1_000)
        .unwrap()
        .checked_div(native_summary.median_ns)
        .unwrap();
    assert_eq!(
        integer(
            report,
            &format!("trial_{trial}_pdfium_to_native_median_ratio_milli")
        ),
        ratio_milli
    );
    assert_eq!(
        integer(report, &format!("trial_{trial}_raw_log_bytes")),
        2_664
    );
}

fn summarize(samples: &[u64]) -> Summary {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    Summary {
        minimum_ns: sorted[0],
        median_ns: nearest_rank(&sorted, 50, 100),
        p95_ns: nearest_rank(&sorted, 95, 100),
        p99_ns: nearest_rank(&sorted, 99, 100),
        maximum_ns: sorted[49],
        median_ci95_low_ns: sorted[17],
        median_ci95_high_ns: sorted[32],
    }
}

fn nearest_rank(sorted: &[u64], numerator: usize, denominator: usize) -> u64 {
    let rank = (sorted.len() * numerator).div_ceil(denominator);
    sorted[rank - 1]
}

fn assert_summary(report: &Record, prefix: &str, summary: &Summary) {
    for (suffix, value) in [
        ("minimum_ns", summary.minimum_ns),
        ("median_ns", summary.median_ns),
        ("p95_ns", summary.p95_ns),
        ("p99_ns", summary.p99_ns),
        ("maximum_ns", summary.maximum_ns),
        ("median_ci95_low_ns", summary.median_ci95_low_ns),
        ("median_ci95_high_ns", summary.median_ci95_high_ns),
    ] {
        assert_eq!(integer(report, &format!("{prefix}_{suffix}")), value);
    }
}

fn parse_report(input: &str) -> Record {
    let mut record = BTreeMap::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("invalid report line: {line}"));
        assert!(
            record
                .insert(key.trim().into(), value.trim().into())
                .is_none()
        );
    }
    record
}

fn quoted<'a>(report: &'a Record, key: &str) -> &'a str {
    quoted_value(report.get(key).unwrap_or_else(|| panic!("missing {key}")))
}

fn quoted_value(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or_else(|| panic!("expected quoted value: {value}"))
}

fn integer(report: &Record, key: &str) -> u64 {
    report
        .get(key)
        .unwrap_or_else(|| panic!("missing {key}"))
        .parse()
        .unwrap_or_else(|_| panic!("invalid integer: {key}"))
}

fn boolean(report: &Record, key: &str) -> bool {
    report
        .get(key)
        .unwrap_or_else(|| panic!("missing {key}"))
        .parse()
        .unwrap_or_else(|_| panic!("invalid boolean: {key}"))
}

fn series(report: &Record, key: &str) -> Vec<u64> {
    quoted(report, key)
        .split(',')
        .map(|value| {
            value
                .parse()
                .unwrap_or_else(|_| panic!("invalid sample in {key}"))
        })
        .collect()
}

fn assert_digest(report: &Record, key: &str, bytes: &[u8]) {
    assert_eq!(
        quoted(report, key),
        format!("sha256:{}", hex(&sha256(bytes).unwrap()))
    );
}

fn assert_sha256(value: &str) {
    let value = value.strip_prefix("sha256:").unwrap();
    assert_eq!(value.len(), 64);
    assert!(value.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

fn decoded_digest(report: &Record, key: &str) -> [u8; 32] {
    let encoded = quoted(report, key).strip_prefix("sha256:").unwrap();
    let mut output = [0_u8; 32];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
    }
    output
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => panic!("invalid hex"),
    }
}

fn array_record<'a>(input: &'a str, table: &str, id: &str) -> &'a str {
    let header = format!("[[{table}]]");
    input
        .split(&header)
        .skip(1)
        .find(|record| {
            record
                .lines()
                .any(|line| line.trim() == format!("id = \"{id}\""))
        })
        .unwrap_or_else(|| panic!("missing {table} record {id}"))
}

fn assert_line(input: &str, expected: &str) {
    assert!(input.lines().any(|line| line.trim() == expected));
}

fn hex(value: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in value {
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}
