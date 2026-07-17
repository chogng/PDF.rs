use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use pdf_rs_protocol_codegen::{check_repository, generate_repository};

const SCHEMA: &str = include_str!("../../../protocol/engine.protocol");
const MANIFEST: &str = include_str!("../Cargo.toml");
const PROVENANCE: &str = include_str!("../PROVENANCE.md");
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempRepository {
    root: PathBuf,
}

impl TempRepository {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "pdf-rs-protocol-codegen-{}-{nonce}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("protocol")).unwrap();
        std::fs::write(root.join("protocol/engine.protocol"), SCHEMA).unwrap();
        Self { root }
    }
}

impl Drop for TempRepository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn package_has_zero_dependencies_and_no_build_script() {
    assert!(
        MANIFEST.contains("[dependencies]\n\n[lib]"),
        "the protocol generator dependency table must remain empty"
    );
    assert!(!MANIFEST.contains("build ="));
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("build.rs")
            .exists()
    );
    for required in [
        "zero dependencies",
        "no `build.rs`",
        "sha256-first-16-bytes",
        "`--check <repository-root>`",
        "rejects absolute, parent-relative,\nor symlinked",
        "not a product runtime dependency",
    ] {
        assert!(
            PROVENANCE.contains(required),
            "missing provenance: {required}"
        );
    }
}

#[test]
fn generated_paths_are_a_fixed_parent_free_allowlist() {
    let repository = TempRepository::new();
    generate_repository(&repository.root).unwrap();
    let mut actual = repository_files(&repository.root);
    actual.remove("protocol/engine.protocol");
    let expected = [
        "platform/browser/generated/engine-protocol.ts",
        "platform/desktop/generated/engine-protocol.registry",
        "protocol/generated/compatibility-vectors.json",
        "protocol/generated/invalid-vectors.json",
        "protocol/generated/schema-hash.txt",
        "runtime/protocol/src/generated.rs",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(actual, expected);
    for file in actual {
        assert!(
            Path::new(&file)
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        );
    }
}

#[test]
fn check_mode_preserves_generated_bytes_and_mtimes() {
    let repository = TempRepository::new();
    generate_repository(&repository.root).unwrap();
    let files = repository_files(&repository.root)
        .into_iter()
        .filter(|path| path != "protocol/engine.protocol")
        .collect::<Vec<_>>();
    let before = files
        .iter()
        .map(|relative_path| {
            let path = repository.root.join(relative_path);
            (
                relative_path.clone(),
                std::fs::read(&path).unwrap(),
                std::fs::metadata(&path).unwrap().modified().unwrap(),
            )
        })
        .collect::<Vec<_>>();

    check_repository(&repository.root).unwrap();

    for (relative, bytes, modified) in before {
        let path = repository.root.join(relative);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
    }
}

#[test]
fn repository_loader_rejects_schema_over_hard_limit() {
    let repository = TempRepository::new();
    let oversized = vec![b'x'; 1024 * 1024 + 1];
    std::fs::write(repository.root.join("protocol/engine.protocol"), oversized).unwrap();
    let error = generate_repository(&repository.root).unwrap_err();
    assert!(error.contains("exceeds 1048576 byte limit"), "{error}");
}

fn repository_files(root: &Path) -> BTreeSet<String> {
    fn visit(root: &Path, directory: &Path, output: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, output);
            } else {
                output.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }

    let mut output = BTreeSet::new();
    visit(root, root, &mut output);
    output
}

#[cfg(unix)]
#[test]
fn generate_rejects_symlinked_output_components() {
    use std::os::unix::fs::symlink;

    let repository = TempRepository::new();
    let redirected = repository.root.join("redirected");
    std::fs::create_dir(&redirected).unwrap();
    symlink(&redirected, repository.root.join("runtime")).unwrap();
    let error = generate_repository(&repository.root).unwrap_err();
    assert!(error.contains("symlink"), "{error}");
}
