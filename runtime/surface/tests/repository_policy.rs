use std::fs;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn product_sources() -> Vec<PathBuf> {
    let source = crate_root().join("src");
    let mut paths = fs::read_dir(source)
        .expect("product source directory exists")
        .map(|entry| entry.expect("source entry is readable").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

#[test]
fn product_sources_are_safe_pure_rust_without_os_or_wall_clock_io() {
    let forbidden = [
        "unsafe {",
        "unsafe fn",
        "std::fs",
        "std::net",
        "std::process",
        "std::thread",
        "std::time",
        "std::io",
        "extern \"",
        "libc::",
        "tokio::",
    ];
    for path in product_sources() {
        let source = fs::read_to_string(&path).expect("product source is UTF-8");
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "{} contains forbidden product capability {needle:?}",
                path.display()
            );
        }
    }
}

#[test]
fn manifest_has_only_the_canonical_product_dependencies() {
    let manifest =
        fs::read_to_string(crate_root().join("Cargo.toml")).expect("manifest is readable");
    assert!(manifest.contains("pdf-rs-policy = { path = \"../policy\" }"));
    assert!(manifest.contains("pdf-rs-protocol = { path = \"../protocol\" }"));
    assert!(!manifest.contains("[dev-dependencies]"));
    for forbidden in ["pdfium", "mupdf", "poppler", "serde", "tokio", "libc"] {
        assert!(
            !manifest.to_ascii_lowercase().contains(forbidden),
            "manifest unexpectedly depends on {forbidden}"
        );
    }
}

#[test]
fn provenance_records_boundary_invariants_bounds_and_semantic_owners() {
    let provenance =
        fs::read_to_string(crate_root().join("PROVENANCE.md")).expect("provenance is readable");
    for required in [
        "# Scope",
        "# Trust boundary",
        "# Ownership and state invariants",
        "# Bounds and clocks",
        "# Failure and diagnostics",
        "# Semantic owners",
        "pdf-rs-protocol",
        "pdf-rs-policy",
        "only safe Rust",
        "no operating-system I/O",
        "one-shot",
        "idempotent",
        "virtual clock",
        "Worker epoch",
    ] {
        assert!(
            provenance.contains(required),
            "PROVENANCE.md must document {required:?}"
        );
    }
}

#[test]
fn crate_is_registered_as_the_runtime_surface_workspace_member() {
    let workspace_manifest = fs::read_to_string(
        crate_root()
            .parent()
            .and_then(Path::parent)
            .expect("crate is runtime/surface")
            .join("Cargo.toml"),
    )
    .expect("workspace manifest is readable");
    assert!(
        workspace_manifest
            .lines()
            .any(|line| line.trim().trim_matches(',').trim_matches('"') == "runtime/surface"),
        "root workspace must register runtime/surface"
    );
}
