use std::fs;
use std::path::{Path, PathBuf};

fn source_files(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).expect("read scheduler source directory") {
            let entry = entry.expect("read scheduler source entry");
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn product_source_is_safe_and_contains_no_runtime_or_io_primitives() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let forbidden = [
        "unsafe {",
        "unsafe fn",
        "std::thread",
        "thread::",
        "std::time",
        "Instant",
        "SystemTime",
        "sleep(",
        "std::fs",
        "std::net",
        "tokio",
        "async_std",
        "extern crate",
    ];
    for path in source_files(&root) {
        let source = fs::read_to_string(&path).expect("read scheduler source");
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "{} contains forbidden primitive {needle:?}",
                path.display()
            );
        }
    }
}

#[test]
fn crate_declares_no_dependencies() {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("read manifest");
    let dependencies = manifest
        .split_once("[dependencies]")
        .expect("dependencies section")
        .1
        .trim();
    assert!(
        dependencies.is_empty(),
        "unexpected dependencies: {dependencies}"
    );
}

#[test]
fn provenance_records_virtual_time_capacity_and_terminal_ownership() {
    let provenance =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("PROVENANCE.md"))
            .expect("read provenance");
    for required in [
        "virtual tick",
        "precharges",
        "critical queue",
        "TerminalArbiter",
        "DiscardAndRelease",
        "does not read a wall clock",
    ] {
        assert!(
            provenance.contains(required),
            "missing provenance statement {required:?}"
        );
    }
}
