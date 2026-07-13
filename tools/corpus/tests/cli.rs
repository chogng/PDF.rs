use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_rs_digest::{hex_digest, sha256};

static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);
const ABC_HASH: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const OTHER_HASH: &str = "9c819e549afcc89d03b380c3c1bd47128aa2b70ae30a35245e6a0e30132875db";

#[test]
fn cli_requires_the_validate_manifest_and_root_arguments() {
    let no_arguments = corpus().output().unwrap();
    assert_eq!(no_arguments.status.code(), Some(2));
    assert!(stderr(&no_arguments).contains("usage: pdf-rs-corpus validate"));

    let unknown = corpus()
        .args(["inspect", "manifest.toml", "."])
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(2));
    assert!(stderr(&unknown).contains("usage: pdf-rs-corpus validate"));
}

#[test]
fn cli_verifies_objects_and_redacts_paths_and_hashes() {
    let directory = TempDir::new();
    let object_path = directory.path().join("secret-object.pdf");
    let manifest_path = directory.path().join("manifest.toml");
    fs::write(&object_path, b"abc").unwrap();
    let manifest_bytes = manifest("secret-object.pdf", ABC_HASH, 3);
    let manifest_hash = hex_digest(&sha256(manifest_bytes.as_bytes()).unwrap());
    fs::write(&manifest_path, manifest_bytes).unwrap();

    let success = corpus()
        .args(["validate"])
        .arg(&manifest_path)
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(success.status.success(), "{}", stderr(&success));
    let output = stdout(&success);
    assert!(output.contains("manifest_id=cli-t0-v1"));
    assert!(output.contains(&format!("manifest_sha256=sha256:{manifest_hash}")));
    assert!(output.contains("verified_objects=1"));
    assert!(output.contains("verified_bytes=3"));

    fs::write(&manifest_path, manifest("secret-object.pdf", OTHER_HASH, 3)).unwrap();
    let failure = corpus()
        .args(["validate"])
        .arg(&manifest_path)
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(stderr(&failure).contains("RPE-CORPUS-MANIFEST-0023"));
    assert!(!stderr(&failure).contains("secret-object.pdf"));
    assert!(!stderr(&failure).contains(OTHER_HASH));
}

fn manifest(path: &str, hash: &str, max_bytes: u64) -> String {
    format!(
        "schema = 1\nid = \"cli-t0-v1\"\nversion = \"1\"\n\n[[entry]]\nsha256 = \"sha256:{hash}\"\npath = \"{path}\"\ntier = \"T0\"\npage_count = 1\nlicense_expression = \"LicenseRef-PDF.rs-SelfAuthored-Test\"\nsource = \"cli-self-authored\"\naccess = \"repository\"\nredistribution = \"prohibited\"\nfeatures = []\nmax_bytes = {max_bytes}\n"
    )
}

fn corpus() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pdf-rs-corpus"))
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
            "pdf-rs-corpus-cli-{}-{sequence}",
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
