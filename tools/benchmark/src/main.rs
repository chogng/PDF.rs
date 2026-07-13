#![forbid(unsafe_code)]

use std::env;
use std::path::Path;
use std::process::ExitCode;

use pdf_rs_benchmark::{BenchmarkReportLimits, load_report_file, validate_report_corpus};
use pdf_rs_corpus::{CorpusManifestLimits, load_manifest_file};
use pdf_rs_digest::hex_digest;

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        usage();
        return ExitCode::from(2);
    };
    let Some(report_path) = arguments.next() else {
        usage();
        return ExitCode::from(2);
    };
    let Some(corpus_path) = arguments.next() else {
        usage();
        return ExitCode::from(2);
    };
    if command != "validate" || arguments.next().is_some() {
        usage();
        return ExitCode::from(2);
    }

    let report = match load_report_file(Path::new(&report_path), BenchmarkReportLimits::default()) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("benchmark report validation failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let corpus = match load_manifest_file(Path::new(&corpus_path), CorpusManifestLimits::default())
    {
        Ok(corpus) => corpus,
        Err(error) => {
            eprintln!("benchmark corpus validation failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = validate_report_corpus(&report, &corpus) {
        eprintln!("benchmark report validation failed: {error}");
        return ExitCode::FAILURE;
    }

    let statistics = report.summary().statistics;
    println!("report_id={}", report.id());
    println!("evidence_class={}", report.evidence_class().as_str());
    println!(
        "report_sha256=sha256:{}",
        hex_digest(&report.source_sha256())
    );
    println!("corpus_id={}", report.metadata().corpus_id());
    println!("scenario={}", report.scenario().as_str());
    println!("timing_domain={}", report.timing_domain().as_str());
    println!("sample_count={}", statistics.sample_count);
    println!("median_ns={}", statistics.median.get());
    println!("p95_ns={}", statistics.p95.get());
    println!("p99_ns={}", statistics.p99.get());
    println!("sample_count_status={}", report.sample_count_status());
    println!("performance_eligible={}", report.performance_eligible());
    println!(
        "confidence_interval_status={}",
        report.confidence_interval_status()
    );
    println!(
        "external_baseline_status={}",
        report.external_baseline_status()
    );
    println!("verdict={}", report.verdict());
    ExitCode::SUCCESS
}

fn usage() {
    eprintln!("usage: pdf-rs-benchmark validate <benchmark-report.toml> <corpus-manifest.toml>");
}
