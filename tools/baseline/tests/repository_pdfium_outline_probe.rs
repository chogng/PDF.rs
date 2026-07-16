use std::collections::BTreeMap;
use std::fmt::Write as _;

use pdf_rs_baseline::{BaselineDescriptor, descriptor_identity};
use pdf_rs_digest::sha256;

const REPORT_ID: &str = "pdfium-c040cf96-macos-arm64-o4-outline-differential-probe-v1";
const REPORT_HASH: &str = "fb7ca7057d4a522f1e1d36418a237d0268c25933989db1fc34a618bd4d827b71";
const REPORT: &[u8] = include_bytes!(
    "../pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-outline-differential-probe-v1.toml"
);
const PREREQUISITE_PIXEL_EVIDENCE: &[u8] =
    include_bytes!("../pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1.toml");
const PREREQUISITE_PIXEL_BUILD_DEFINITION: &[u8] = include_bytes!("../pdfium/helper/BUILD.gn");
const PREREQUISITE_PIXEL_HELPER_SOURCE: &[u8] =
    include_bytes!("../pdfium/helper/pdf_rs_pdfium_adapter.cc");
const PREREQUISITE_PIXEL_ROOT_OVERLAY: &[u8] = include_bytes!("../pdfium/helper/pdfium-root.patch");
const BUILD_DEFINITION: &[u8] = include_bytes!("../pdfium/helper/outline.BUILD.gn");
const HELPER_SOURCE: &[u8] = include_bytes!("../pdfium/helper/pdf_rs_pdfium_outline_probe.cc");
const ROOT_OVERLAY: &[u8] = include_bytes!("../pdfium/helper/pdfium-outline-root.patch");
const HOST_ADAPTER: &[u8] = include_bytes!("../src/pdfium.rs");
const COMPARISON_TEST: &[u8] = include_bytes!("pdfium_outline_real_adapter.rs");
const PDFIUM_ARGS_GN: &str = concat!(
    "use_remoteexec = false\n",
    "is_debug = false\n",
    "symbol_level = 0\n",
    "target_cpu = \"arm64\"\n",
    "pdf_is_standalone = true\n",
    "pdf_enable_v8 = false\n",
    "pdf_enable_xfa = false\n",
    "pdf_use_skia = false\n",
    "pdf_enable_fontations = false\n",
    "is_component_build = false\n",
);

type Record = BTreeMap<String, String>;

#[test]
fn outline_differential_evidence_is_hash_bound_and_scope_limited() {
    assert_eq!(hex(&sha256(REPORT).unwrap()), REPORT_HASH);
    let text = std::str::from_utf8(REPORT).unwrap();
    assert!(text.ends_with('\n'));
    assert!(!text.contains('\r'));
    for forbidden in ["/Users/", "/private/tmp/", "HOME=", "lance"] {
        assert!(!text.contains(forbidden), "unredacted value: {forbidden}");
    }
    let report = parse_report(text);

    assert_eq!(integer(&report, "schema"), 1);
    assert_eq!(quoted(&report, "id"), REPORT_ID);
    assert_eq!(
        quoted(&report, "evidence_class"),
        "external-o4-outline-differential-probe"
    );
    assert_eq!(quoted(&report, "engine"), "pdfium");
    assert_eq!(
        quoted(&report, "upstream_revision"),
        "c040cf96106a87220b814a1a892649cf2d7f1934"
    );
    assert_eq!(quoted(&report, "platform"), "macos-arm64");
    assert_eq!(
        quoted(&report, "chromium_mac_build_instructions"),
        "https://chromium.googlesource.com/chromium/src/+/main/docs/mac_build_instructions.md"
    );
    assert_eq!(
        quoted(&report, "adapter_profile"),
        "pdfium-public-c-api-outline-v1"
    );
    assert_eq!(integer(&report, "runner_schema"), 2);

    for key in [
        "adapter_exercised",
        "public_pdfium_c_api_only",
        "protocol_identity_verified",
        "executable_hash_verified_before_runner_construction",
        "normalized_public_api_observation",
        "valid_fixture_hash_verified",
        "valid_repeat_outputs_identical",
        "valid_observable_subset_exact",
        "invalid_prev_fixture_hash_verified",
        "native_engine_exercised",
        "pdfium_engine_exercised",
        "comparison_executed",
        "native_vs_pdfium_differential",
    ] {
        assert!(boolean(&report, key), "{key} must be true");
    }
    for key in [
        "outline_root_count_compared",
        "last_parent_prev_compared",
        "raw_destination_action_shape_compared",
        "product_correctness_eligible",
        "performance_eligible",
        "differential_eligible",
        "baseline_registration_eligible",
        "release_gate_eligible",
        "raw_logs_committed",
        "helper_binary_committed",
        "pdfium_source_committed",
        "fixture_pdf_committed",
        "outline_json_bytes_committed",
        "upstream_data_committed",
    ] {
        assert!(!boolean(&report, key), "{key} must be false");
    }
    assert_eq!(quoted(&report, "oracle_authority"), "O4");
    assert_eq!(
        quoted(&report, "verdict"),
        "observable-subset-exact-with-expected-strictness-difference"
    );
    assert_eq!(
        quoted(&report, "invalid_prev_native_diagnostic"),
        "RPE-DOCUMENT-0041"
    );
    assert_eq!(
        quoted(&report, "invalid_prev_classification"),
        "expected-strictness-difference"
    );
    assert!(quoted(&report, "invalid_prev_reason").contains("do not expose or validate"));
    assert!(quoted(&report, "public_api_blind_spots").contains("Prev"));

    assert_digest(
        &report,
        "prerequisite_pixel_evidence_sha256",
        PREREQUISITE_PIXEL_EVIDENCE,
    );
    assert_digest(
        &report,
        "prerequisite_pixel_build_definition_sha256",
        PREREQUISITE_PIXEL_BUILD_DEFINITION,
    );
    assert_digest(
        &report,
        "prerequisite_pixel_helper_source_sha256",
        PREREQUISITE_PIXEL_HELPER_SOURCE,
    );
    assert_digest(
        &report,
        "prerequisite_pixel_root_overlay_sha256",
        PREREQUISITE_PIXEL_ROOT_OVERLAY,
    );
    assert_digest(&report, "helper_build_definition_sha256", BUILD_DEFINITION);
    assert_digest(&report, "helper_source_sha256", HELPER_SOURCE);
    assert_digest(&report, "helper_root_overlay_sha256", ROOT_OVERLAY);
    assert_digest(&report, "host_adapter_source_sha256", HOST_ADAPTER);
    assert_digest(&report, "comparison_test_sha256", COMPARISON_TEST);
    assert_digest(&report, "build_flags_sha256", PDFIUM_ARGS_GN.as_bytes());
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

    assert_eq!(integer(&report, "helper_executable_bytes"), 3_710_416);
    assert_eq!(integer(&report, "gn_targets_generated"), 599);
    assert_eq!(integer(&report, "helper_build_steps"), 2);
    assert_eq!(integer(&report, "helper_build_exit_code"), 0);
    assert_eq!(quoted(&report, "helper_build_outcome"), "pass");
    assert_eq!(
        quoted(&report, "helper_build_reverification_ninja_state"),
        "no-work-required"
    );
    assert_eq!(integer(&report, "manual_test_count"), 1);
    assert_eq!(integer(&report, "manual_test_passed"), 1);
    assert_eq!(integer(&report, "helper_process_runs"), 3);
    assert_eq!(integer(&report, "valid_probe_runs"), 2);
    assert_eq!(integer(&report, "invalid_prev_probe_runs"), 1);
    assert_eq!(integer(&report, "valid_fixture_bytes"), 786);
    assert_eq!(integer(&report, "invalid_prev_fixture_bytes"), 786);
    assert_eq!(integer(&report, "valid_observable_items"), 3);
    assert_eq!(integer(&report, "valid_outline_json_bytes"), 197);
    assert_eq!(integer(&report, "valid_different_observable_records"), 0);
    assert_eq!(
        quoted(&report, "valid_native_json_sha256"),
        quoted(&report, "valid_pdfium_json_sha256")
    );
    assert_eq!(
        quoted(&report, "valid_pdfium_json_sha256"),
        quoted(&report, "invalid_prev_pdfium_json_sha256")
    );
}

#[test]
fn outline_evidence_is_data_bound_but_not_a_registered_baseline() {
    let ledger = include_str!("../../../docs/traceability/data-ledger.toml");
    assert_line(ledger, "version = \"0.11.0\"");
    let record = array_record(
        ledger,
        "data",
        "baseline.evidence.pdfium-c040cf96-macos-arm64-o4-outline-differential-probe-v1",
    );
    for expected in [
        "kind = \"project-authored-external-o4-outline-differential-evidence\"",
        "source = \"tools/baseline/pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-outline-differential-probe-v1.toml\"",
        "source_hash = \"sha256:fb7ca7057d4a522f1e1d36418a237d0268c25933989db1fc34a618bd4d827b71\"",
        "contains_personal_data = false",
        "validated_by = \"cargo test --package pdf-rs-baseline --test repository_pdfium_outline_probe\"",
        "owner = \"baseline-release\"",
    ] {
        assert_line(record, expected);
    }

    let baseline = include_str!("../../../docs/traceability/baseline-ledger.toml");
    assert_line(baseline, "status = \"initial\"");
    assert_line(baseline, "baseline = []");
    assert!(!baseline.contains("[[baseline]]"));

    let adapter = include_str!("../pdfium/outline_adapter.toml");
    assert_line(
        adapter,
        "adapter_profile = \"pdfium-public-c-api-outline-v1\"",
    );
    assert_line(
        adapter,
        "probe_evidence = \"evidence/pdfium-c040cf96-macos-arm64-o4-outline-differential-probe-v1.toml\"",
    );
    assert_line(
        adapter,
        "registration_status = \"blocked-incomplete-closure-and-sandbox\"",
    );
    assert!(adapter.contains("REQUIRED_BEFORE_REGISTERED_BASELINE"));
    assert!(adapter.contains("strict_topology_blind_spots"));

    let readme = include_str!("../pdfium/README.md");
    let provenance = include_str!("../PROVENANCE.md");
    for document in [readme, provenance] {
        assert!(document.contains(REPORT_ID));
        assert!(document.contains("non-gating"));
        assert!(document.contains("not a registered baseline"));
        assert!(document.contains("/Prev"));
    }
    assert!(
        readme.contains(
            "chromium.googlesource.com/chromium/src/+/main/docs/mac_build_instructions.md"
        )
    );

    let feature_map = include_str!("../../../docs/traceability/feature-map.toml");
    let spec_map = include_str!("../../../docs/traceability/spec-map.toml");
    assert_line(feature_map, "version = \"0.66.0\"");
    assert_line(spec_map, "version = \"0.66.0\"");
    let outline = array_record(feature_map, "feature", "core.strict-outline");
    assert_line(outline, "state = \"DIFFERENTIAL\"");
    assert!(!outline.contains("tools/baseline::pdfium_outline_real_adapter"));
    assert!(!outline.contains("tools/baseline::repository_pdfium_outline_probe"));
    assert!(outline.contains("tools/quality::m1_document_service_differential"));
    assert!(outline.contains("fuzz.m1documentservices"));
    assert!(spec_map.contains("tools/baseline::pdfium_outline_real_adapter"));
    assert!(spec_map.contains("tools/baseline::repository_pdfium_outline_probe"));
    assert!(spec_map.contains("expected strictness difference"));
    assert!(spec_map.contains("not a registered baseline"));

    let ci = include_str!("../../../scripts/ci.sh");
    assert!(!ci.contains("PDF_RS_PDFIUM_OUTLINE_ADAPTER"));
    assert!(!ci.contains("pdf_rs_pdfium_outline_probe"));
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

fn assert_digest(record: &Record, key: &str, bytes: &[u8]) {
    assert_eq!(
        quoted(record, key),
        format!("sha256:{}", hex(&sha256(bytes).unwrap()))
    );
}

fn assert_sha256(value: &str) {
    let digest = value
        .strip_prefix("sha256:")
        .unwrap_or_else(|| panic!("missing sha256 prefix: {value}"));
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

fn decoded_digest(record: &Record, key: &str) -> [u8; 32] {
    let value = quoted(record, key).strip_prefix("sha256:").unwrap();
    let mut output = [0_u8; 32];
    for (index, slot) in output.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap();
    }
    output
}

fn array_record<'a>(input: &'a str, table: &str, id: &str) -> &'a str {
    let marker = format!("[[{table}]]");
    input
        .split(&marker)
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
