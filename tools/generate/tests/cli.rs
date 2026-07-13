use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_rs_generate::{GenerateLimits, ONE_PAGE_DSL, generate_one_page_pdf};

static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

#[test]
fn cli_requires_exactly_two_arguments() {
    let no_arguments = generator().output().unwrap();
    assert_eq!(no_arguments.status.code(), Some(2));
    assert!(stderr(&no_arguments).contains("usage: pdf-rs-generate"));

    let too_many = generator()
        .args(["source.dsl", "output.pdf", "extra"])
        .output()
        .unwrap();
    assert_eq!(too_many.status.code(), Some(2));
    assert!(stderr(&too_many).contains("usage: pdf-rs-generate"));
}

#[test]
fn cli_replays_source_and_redacts_invalid_content() {
    let directory = TempDir::new();
    let source = directory.path().join("source.dsl");
    let output = directory.path().join("output.pdf");
    fs::write(&source, ONE_PAGE_DSL).unwrap();

    let success = generator().args([&source, &output]).output().unwrap();
    assert!(success.status.success(), "{}", stderr(&success));
    assert_eq!(fs::read(&output).unwrap(), generate_one_page_pdf().unwrap());

    let secret = "fixture-secret-that-must-not-leak";
    fs::write(&source, format!("document! {secret}")).unwrap();
    let failure = generator().args([&source, &output]).output().unwrap();
    assert!(!failure.status.success());
    assert!(stderr(&failure).contains("RPE-GENERATE-0007"));
    assert!(!stderr(&failure).contains(secret));
}

#[test]
fn cli_rejects_non_regular_and_oversized_sources() {
    let directory = TempDir::new();
    let output = directory.path().join("output.pdf");
    let directory_failure = generator()
        .args([directory.path(), &output])
        .output()
        .unwrap();
    assert!(!directory_failure.status.success());
    assert_eq!(
        stderr(&directory_failure).trim(),
        "failed to read bounded DSL source"
    );

    let oversized = directory.path().join("oversized.dsl");
    fs::write(
        &oversized,
        vec![b' '; GenerateLimits::default().max_source_bytes() + 1],
    )
    .unwrap();
    let oversized_failure = generator().args([&oversized, &output]).output().unwrap();
    assert!(!oversized_failure.status.success());
    assert_eq!(
        stderr(&oversized_failure).trim(),
        "failed to read bounded DSL source"
    );

    #[cfg(unix)]
    {
        let source = directory.path().join("source.dsl");
        let link = directory.path().join("source-link.dsl");
        fs::write(&source, ONE_PAGE_DSL).unwrap();
        std::os::unix::fs::symlink(source, &link).unwrap();
        let symlink_failure = generator().args([&link, &output]).output().unwrap();
        assert!(!symlink_failure.status.success());
        assert_eq!(
            stderr(&symlink_failure).trim(),
            "failed to read bounded DSL source"
        );
    }
}

fn generator() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pdf-rs-generate"))
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let sequence = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pdf-rs-generate-cli-{}-{sequence}",
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
