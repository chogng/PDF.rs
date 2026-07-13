use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

#[test]
fn cli_requires_validate_report_and_corpus_arguments() {
    let no_arguments = benchmark().output().unwrap();
    assert_eq!(no_arguments.status.code(), Some(2));
    assert!(stderr(&no_arguments).contains("usage: pdf-rs-benchmark validate"));

    let unknown = benchmark()
        .args(["inspect", "report.toml", "corpus.toml"])
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(2));
    assert!(stderr(&unknown).contains("usage: pdf-rs-benchmark validate"));
}

#[test]
fn cli_reports_non_verdict_evidence_and_redacts_invalid_metadata() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let report_path = repository.join("tests/performance/m0-synthetic-benchmark-replay-v1.toml");
    let corpus_path = repository.join("tests/corpus/manifests/t0-bootstrap-v1.toml");

    let success = benchmark()
        .args(["validate"])
        .arg(&report_path)
        .arg(&corpus_path)
        .output()
        .unwrap();
    assert!(success.status.success(), "{}", stderr(&success));
    let output = stdout(&success);
    assert!(output.contains("report_id=m0-synthetic-benchmark-replay-v1"));
    assert!(output.contains("evidence_class=synthetic-pipeline-smoke"));
    assert!(output.contains(
        "report_sha256=sha256:2d66bab0542d92e443922d4a2d2ee72f382558d5c35153bc598370747d621527"
    ));
    assert!(output.contains("sample_count=5"));
    assert!(output.contains("median_ns=100"));
    assert!(output.contains("performance_eligible=false"));
    assert!(output.contains("confidence_interval_status=not-implemented-m0"));
    assert!(output.contains("external_baseline_status=absent"));
    assert!(output.contains("verdict=not-evaluated"));

    let directory = TempDir::new();
    let report_source = fs::read_to_string(&report_path).unwrap();
    let secret_path = directory.path().join("secret-report.toml");
    let report = report_source.replacen(
        "profile = \"m0.synthetic-benchmark-replay.v1\"",
        "profile = \"secret-environment-value\"",
        1,
    );
    fs::write(&secret_path, report).unwrap();
    let failure = benchmark()
        .args(["validate"])
        .arg(&secret_path)
        .arg(&corpus_path)
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(stderr(&failure).contains("RPE-BENCHMARK-REPORT-0013"));
    assert!(!stderr(&failure).contains("secret-environment-value"));
    assert!(!stderr(&failure).contains("secret-report.toml"));

    let mismatched_id_path = directory.path().join("secret-corpus-id-report.toml");
    let mismatched_id = fs::read_to_string(&report_path).unwrap().replacen(
        "corpus_id = \"t0-bootstrap-v1\"",
        "corpus_id = \"secret-corpus-id\"",
        1,
    );
    fs::write(&mismatched_id_path, mismatched_id).unwrap();
    let failure = benchmark()
        .args(["validate"])
        .arg(&mismatched_id_path)
        .arg(&corpus_path)
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(stderr(&failure).contains("RPE-BENCHMARK-REPORT-0017"));
    assert!(!stderr(&failure).contains("secret-corpus-id"));
    assert!(!stderr(&failure).contains("secret-corpus-id-report.toml"));

    let mismatched_hash_path = directory.path().join("secret-corpus-hash-report.toml");
    let secret_hash = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    let mismatched_hash = fs::read_to_string(&report_path).unwrap().replacen(
        "4268cb945b6056d7732f22b0e90d9629f6d31ab2ba6f013e7011735989859d8e",
        secret_hash,
        1,
    );
    fs::write(&mismatched_hash_path, mismatched_hash).unwrap();
    let failure = benchmark()
        .args(["validate"])
        .arg(&mismatched_hash_path)
        .arg(&corpus_path)
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(stderr(&failure).contains("RPE-BENCHMARK-REPORT-0018"));
    assert!(!stderr(&failure).contains(secret_hash));
    assert!(!stderr(&failure).contains("secret-corpus-hash-report.toml"));

    let malformed_corpus_path = directory.path().join("secret-corpus.toml");
    fs::write(&malformed_corpus_path, "secret-corpus-content").unwrap();
    let failure = benchmark()
        .args(["validate"])
        .arg(&report_path)
        .arg(&malformed_corpus_path)
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(stderr(&failure).contains("RPE-CORPUS-MANIFEST-"));
    assert!(!stderr(&failure).contains("secret-corpus-content"));
    assert!(!stderr(&failure).contains("secret-corpus.toml"));
}

fn benchmark() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pdf-rs-benchmark"))
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let sequence = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pdf-rs-benchmark-cli-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
