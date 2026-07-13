use std::fs;
use std::path::PathBuf;

use pdf_rs_benchmark::{
    BenchmarkEvidenceClass, BenchmarkReportLimits, SYNTHETIC_BENCHMARK_PROFILE, encode_report,
    load_report_file, validate_report_corpus,
};
use pdf_rs_corpus::{CorpusManifestLimits, load_manifest_file};
use pdf_rs_digest::hex_digest;

const REPORT_LEDGER_ID: &str = "benchmark.report.m0-synthetic-benchmark-replay-v1";
const REPORT_HASH: &str = "2d66bab0542d92e443922d4a2d2ee72f382558d5c35153bc598370747d621527";
const CORPUS_HASH: &str = "4268cb945b6056d7732f22b0e90d9629f6d31ab2ba6f013e7011735989859d8e";
const SPECIFICATION_HASH: &str = "53d46023770b4558705cc00f779fb3031245d473378d82869875283913157541";
const LICENSE: &str = "LicenseRef-PDF.rs-SelfAuthored-Test";

#[test]
fn repository_report_is_canonical_non_verdict_and_ledger_bound() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let report_path = repository.join("tests/performance/m0-synthetic-benchmark-replay-v1.toml");
    let corpus_path = repository.join("tests/corpus/manifests/t0-bootstrap-v1.toml");
    let ledger_path = repository.join("docs/traceability/data-ledger.toml");
    let feature_map_path = repository.join("docs/traceability/feature-map.toml");
    let spec_map_path = repository.join("docs/traceability/spec-map.toml");
    let ci_path = repository.join("scripts/ci.sh");

    let report = load_report_file(&report_path, BenchmarkReportLimits::default()).unwrap();
    let corpus = load_manifest_file(&corpus_path, CorpusManifestLimits::default()).unwrap();
    validate_report_corpus(&report, &corpus).unwrap();
    assert_eq!(
        encode_report(&report, BenchmarkReportLimits::default()).unwrap(),
        fs::read(&report_path).unwrap()
    );
    assert_eq!(hex_digest(&report.source_sha256()), REPORT_HASH);
    assert_eq!(hex_digest(&corpus.source_sha256()), CORPUS_HASH);
    assert_eq!(
        report.evidence_class(),
        BenchmarkEvidenceClass::SyntheticPipelineSmoke
    );
    assert_eq!(report.metadata().profile(), SYNTHETIC_BENCHMARK_PROFILE);
    assert_eq!(report.metadata().corpus_id(), corpus.manifest().id());
    assert_eq!(
        report.metadata().corpus_hash(),
        format!("sha256:{CORPUS_HASH}")
    );
    assert!(!report.performance_eligible());
    assert_eq!(report.confidence_interval_status(), "not-implemented-m0");
    assert_eq!(report.external_baseline_status(), "absent");
    assert_eq!(report.verdict(), "not-evaluated");

    let ledger = fs::read_to_string(ledger_path).unwrap();
    let record = array_record(&ledger, "[[data]]", REPORT_LEDGER_ID);
    assert_line(
        record,
        "kind = \"project-authored-synthetic-benchmark-report\"",
    );
    assert_line(
        record,
        "source = \"tests/performance/m0-synthetic-benchmark-replay-v1.toml\"",
    );
    assert_line(record, &format!("source_hash = \"sha256:{REPORT_HASH}\""));
    assert_line(record, &format!("license_expression = \"{LICENSE}\""));
    assert_line(
        record,
        "redistribution = \"disabled pending project-owner license approval; repository validation only\"",
    );
    assert_line(record, "contains_personal_data = false");
    assert_line(record, "authored_by = \"quality-corpus\"");
    assert_line(record, "format_schema = 1");
    assert_line(
        record,
        "validated_by = \"cargo run --quiet --package pdf-rs-benchmark -- validate tests/performance/m0-synthetic-benchmark-replay-v1.toml tests/corpus/manifests/t0-bootstrap-v1.toml\"",
    );
    assert!(
        !record
            .lines()
            .any(|line| line.starts_with("generated_by ="))
    );
    assert!(!record.lines().any(|line| line.starts_with("output_hash =")));

    let feature_map = fs::read_to_string(feature_map_path).unwrap();
    let feature = array_record(&feature_map, "[[feature]]", "quality.benchmark-harness");
    assert_line(feature, "profile = \"m0.synthetic-benchmark-replay.v1\"");
    assert_line(
        feature,
        "tests = [\"tools/benchmark/src/report.rs::tests\", \"tools/benchmark/tests/cli.rs\", \"tools/benchmark/tests/repository_report.rs\", \"tools/quality/tests/parser_mutation_smoke.rs\"]",
    );
    assert_line(feature, "benchmarks = []");

    let spec_map = fs::read_to_string(spec_map_path).unwrap();
    let milestone = array_record(&spec_map, "[[requirement]]", "RPE-ARCH-001/15.3/M0");
    assert!(milestone.contains("measured benchmark evidence"));
    let benchmark_requirement = array_record(&spec_map, "[[requirement]]", "RPE-ARCH-001/12.21");
    assert_line(
        benchmark_requirement,
        &format!("snapshot_hash = \"sha256:{SPECIFICATION_HASH}\""),
    );
    assert_line(
        benchmark_requirement,
        "tests = [\"tools/benchmark/src/report.rs::tests\", \"tools/benchmark/tests/cli.rs\", \"tools/benchmark/tests/repository_report.rs\", \"tools/quality/tests/parser_mutation_smoke.rs\"]",
    );
    assert!(benchmark_requirement.contains("explicitly remain performance-ineligible"));

    let ci = fs::read_to_string(ci_path).unwrap();
    let generator = ci
        .find("cargo run --quiet --package pdf-rs-generate --")
        .unwrap();
    let corpus = ci
        .find("cargo run --quiet --package pdf-rs-corpus --")
        .unwrap();
    let benchmark = ci
        .find("cargo run --quiet --package pdf-rs-benchmark --")
        .unwrap();
    assert!(generator < corpus && corpus < benchmark);
    let benchmark_command = &ci[benchmark..];
    assert!(
        benchmark_command
            .contains("validate tests/performance/m0-synthetic-benchmark-replay-v1.toml")
    );
    assert!(benchmark_command.contains("tests/corpus/manifests/t0-bootstrap-v1.toml"));
}

fn array_record<'a>(document: &'a str, header: &str, id: &str) -> &'a str {
    let expected = format!("id = \"{id}\"");
    document
        .split(header)
        .skip(1)
        .find(|record| record.lines().any(|line| line == expected))
        .unwrap_or_else(|| panic!("missing {header} record for {id}"))
}

fn assert_line(document: &str, expected: &str) {
    assert!(
        document.lines().any(|line| line == expected),
        "missing metadata line: {expected}"
    );
}
