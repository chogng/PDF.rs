use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const FUZZ_SEEDS: [&str; 3] = ["minimal.pdf", "truncated-header.pdf", "nested-outline.pdf"];
const DEPENDENCY_LEDGER: &str = include_str!("../../../docs/traceability/dependency-ledger.toml");
const FUZZ_MANIFEST: &str = include_str!("../fuzz/Cargo.toml");
const FUZZ_LOCK: &str = include_str!("../fuzz/Cargo.lock");
const PRODUCT_LOCK: &str = include_str!("../../../Cargo.lock");
const CI_WORKFLOW: &str = include_str!("../../../.github/workflows/ci.yml");

#[test]
fn registered_fuzz_dependencies_are_ledgered_and_product_isolated() {
    assert!(DEPENDENCY_LEDGER.contains("version = \"0.2.0\""));
    assert_eq!(
        DEPENDENCY_LEDGER
            .matches("name = \"libfuzzer-sys\"")
            .count(),
        1
    );
    assert_eq!(
        DEPENDENCY_LEDGER.matches("name = \"cargo-fuzz\"").count(),
        1
    );

    let libfuzzer = dependency_record("libfuzzer-sys");
    for required in [
        "version = \"0.4.13\"",
        "scope = \"test\"",
        "source_hash = \"sha256:a9fd2f41a1cba099f79a0b6b6c35656cf7c03351a7bae8ff0f28f25270f929d2\"",
        "license_expression = \"(MIT OR Apache-2.0) AND NCSA\"",
        "license_decision = \"conditional\"",
        "budget_hook = true",
        "cancellation_hook = false",
        "wasm = \"blocked\"",
        "native = \"supported\"",
    ] {
        assert!(
            libfuzzer.contains(required),
            "missing libfuzzer ledger field: {required}"
        );
    }

    let cargo_fuzz = dependency_record("cargo-fuzz");
    for required in [
        "version = \"0.13.2\"",
        "scope = \"toolchain\"",
        "source_hash = \"sha256:5acfd01930e49823e58c30dd8012d3338a620377d7c7d4cc140ca4b2169400e2\"",
        "license_expression = \"MIT OR Apache-2.0\"",
        "license_decision = \"conditional\"",
        "budget_hook = true",
        "cancellation_hook = false",
        "wasm = \"blocked\"",
        "native = \"supported\"",
    ] {
        assert!(
            cargo_fuzz.contains(required),
            "missing cargo-fuzz ledger field: {required}"
        );
    }

    assert!(FUZZ_MANIFEST.contains("libfuzzer-sys = \"=0.4.13\""));
    let locked = FUZZ_LOCK
        .split("[[package]]")
        .find(|record| record.contains("name = \"libfuzzer-sys\""))
        .expect("nested lockfile must pin libfuzzer-sys");
    assert!(locked.contains("version = \"0.4.13\""));
    assert!(locked.contains(
        "checksum = \"a9fd2f41a1cba099f79a0b6b6c35656cf7c03351a7bae8ff0f28f25270f929d2\""
    ));
    assert!(!PRODUCT_LOCK.contains("name = \"libfuzzer-sys\""));
    assert!(!PRODUCT_LOCK.contains("name = \"cargo-fuzz\""));
    assert!(CI_WORKFLOW.contains("cargo install --locked --version 0.13.2 cargo-fuzz"));
    assert!(CI_WORKFLOW.contains("actual=\"$(cargo fuzz --version)\""));
    assert!(CI_WORKFLOW.contains("cargo-fuzz 0.13.2"));
    let provision = CI_WORKFLOW
        .find("cargo install --locked --version 0.13.2 cargo-fuzz")
        .expect("PR CI must provision the locked fuzz toolchain");
    let pr_gate = CI_WORKFLOW
        .find("./scripts/ci.sh pr")
        .expect("PR CI must execute the repository gate");
    assert!(
        provision < pr_gate,
        "fuzz provisioning must precede the PR gate"
    );
}

#[test]
fn registered_m1_document_service_fuzz_target_builds() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fuzz/Cargo.toml");
    let status = Command::new("cargo")
        .arg("check")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(&manifest)
        .status()
        .expect("cargo must launch for the registered fuzz package");
    assert!(status.success(), "registered M1 fuzz target must compile");
}

#[test]
fn registered_m1_document_service_fuzz_toolchain_is_pinned() {
    let output = Command::new("cargo")
        .arg("fuzz")
        .arg("--version")
        .output()
        .expect("cargo-fuzz must be installed for the registered target");
    assert!(
        output.status.success(),
        "cargo-fuzz version query must pass"
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("cargo-fuzz version output must be UTF-8")
            .trim(),
        "cargo-fuzz 0.13.2",
        "the registered fuzz evidence must use its pinned cargo-fuzz release"
    );
}

#[test]
fn registered_fuzz_seed_enters_nonempty_outline_traversal() {
    let quality = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fuzz_seed = fs::read(quality.join("fuzz/corpus/m1_document_services/nested-outline.pdf"))
        .expect("registered nonempty-outline fuzz seed must remain readable");
    let registered_case =
        fs::read(quality.join("../../tests/cases/document/m1-services/nested-valid/input.pdf"))
            .expect("registered nested-valid service fixture must remain readable");
    assert_eq!(
        fuzz_seed, registered_case,
        "the fuzz seed must stay content-identical to the registered nonempty-outline fixture"
    );
    assert!(
        fuzz_seed
            .windows(b"/Outlines".len())
            .any(|window| window == b"/Outlines")
            && fuzz_seed
                .windows(b"/First".len())
                .any(|window| window == b"/First"),
        "the bound seed must reach a present, nonempty outline root"
    );
}

#[test]
fn registered_m1_document_service_fuzz_target_replays_seeded_runs() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fuzz");
    let corpus = TempDirectory::new("pdf-rs-m1-fuzz-corpus");
    let artifacts = corpus.path().join("artifacts");
    let dictionary = package.join("dictionaries/pdf.dict");
    fs::create_dir(&artifacts).expect("temporary artifact directory must be created");
    for seed in FUZZ_SEEDS {
        fs::copy(
            package.join("corpus/m1_document_services").join(seed),
            corpus.path().join(seed),
        )
        .expect("registered seed must copy into the temporary replay corpus");
    }

    let artifact_prefix = format!("-artifact_prefix={}/", artifacts.display());
    let dictionary_arg = format!("-dict={}", dictionary.display());
    let mut command = Command::new("cargo");
    command
        .arg("fuzz")
        .arg("run")
        .arg("--fuzz-dir")
        .arg(&package)
        .arg("--sanitizer")
        .arg("none")
        .arg("m1documentservices")
        .arg(corpus.path())
        .arg("--")
        .arg("-seed=424242")
        .arg("-runs=64")
        .arg("-max_len=1048576")
        .arg("-timeout=1")
        .arg("-rss_limit_mb=512")
        .arg(&dictionary_arg)
        .arg(artifact_prefix);
    assert_eq!(
        command
            .get_args()
            .filter(|argument| *argument == corpus.path().as_os_str())
            .count(),
        1,
        "fuzz run must receive exactly one corpus positional argument"
    );
    let status = command
        .status()
        .expect("cargo-fuzz must launch the registered coverage-guided target");
    assert!(
        status.success(),
        "bounded seeded libFuzzer replay must pass"
    );
}

#[test]
fn registered_m1_document_service_fuzz_corpus_is_coverage_minimized() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fuzz");
    let corpus = TempDirectory::new("pdf-rs-m1-fuzz-cmin");
    let artifacts = TempDirectory::new("pdf-rs-m1-fuzz-cmin-artifacts");
    assert_ne!(
        artifacts.path(),
        corpus.path(),
        "minimizer crash artifacts must not share the corpus directory"
    );
    for seed in FUZZ_SEEDS {
        fs::copy(
            package.join("corpus/m1_document_services").join(seed),
            corpus.path().join(seed),
        )
        .expect("registered seed must copy into the temporary minimizer corpus");
    }
    let input_count = corpus.file_count();
    let dictionary_arg = format!("-dict={}", package.join("dictionaries/pdf.dict").display());
    let artifact_prefix = format!("-artifact_prefix={}/", artifacts.path().display());
    let mut command = Command::new("cargo");
    command
        .arg("fuzz")
        .arg("cmin")
        .arg("--fuzz-dir")
        .arg(&package)
        .arg("--sanitizer")
        .arg("none")
        .arg("m1documentservices")
        .arg(corpus.path())
        .arg("--")
        .arg("-seed=424242")
        .arg("-max_len=1048576")
        .arg("-timeout=1")
        .arg("-rss_limit_mb=512")
        .arg(dictionary_arg)
        .arg(artifact_prefix);
    assert_eq!(
        command
            .get_args()
            .filter(|argument| *argument == corpus.path().as_os_str())
            .count(),
        1,
        "fuzz cmin must receive exactly one corpus positional argument"
    );
    let status = command
        .status()
        .expect("cargo-fuzz must launch the registered coverage minimizer");
    assert!(
        status.success(),
        "coverage-guided corpus minimization must pass"
    );
    let minimized_count = corpus.file_count();
    assert!(
        minimized_count > 0,
        "minimization must retain coverage inputs"
    );
    assert!(
        minimized_count <= input_count,
        "minimization cannot expand the registered seed corpus"
    );
    assert!(
        FUZZ_SEEDS
            .iter()
            .all(|seed| !corpus.path().join(seed).exists()),
        "cargo-fuzz cmin must replace the source corpus with coverage-selected outputs"
    );
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(prefix: &str) -> Self {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{epoch}", std::process::id()));
        fs::create_dir(&path).expect("temporary fuzz directory must be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file_count(&self) -> usize {
        fs::read_dir(&self.0)
            .expect("temporary corpus must remain readable")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .count()
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn dependency_record(name: &str) -> &'static str {
    DEPENDENCY_LEDGER
        .split("[[dependency]]")
        .find(|record| record.contains(&format!("name = \"{name}\"")))
        .expect("registered fuzz dependency must have one ledger record")
}
