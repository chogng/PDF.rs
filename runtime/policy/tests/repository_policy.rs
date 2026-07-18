use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn product_policy_has_no_reference_renderer_tools_or_external_engine_dependency() {
    let root = repository_root();
    let manifest = fs::read_to_string(root.join("runtime/policy/Cargo.toml")).unwrap();
    assert!(manifest.contains("pdf-rs-bytes"));
    assert!(manifest.contains("pdf-rs-scene"));
    assert!(!manifest.contains("pdf-rs-fast-raster"));
    assert!(!manifest.contains("pdf-rs-raster"));
    assert!(!manifest.contains("../../tools"));

    let mut sources = Vec::new();
    collect_rs(&root.join("runtime/policy/src"), &mut sources);
    for source in sources {
        let text = fs::read_to_string(&source).unwrap();
        assert!(
            !text.contains("ReferenceRenderJob"),
            "{} must not invoke or slice Reference output",
            source.display()
        );
        for forbidden in ["pdfium", "pdf.js", "pdfjs", "mupdf", "poppler"] {
            assert!(
                !text.to_ascii_lowercase().contains(forbidden),
                "{} contains forbidden external engine {forbidden}",
                source.display()
            );
        }
    }
}

#[test]
fn crate_is_registered_as_a_product_workspace_member() {
    let root = repository_root();
    let workspace = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(workspace.contains("\"runtime/policy\""));
    let provenance = fs::read_to_string(root.join("runtime/policy/PROVENANCE.md")).unwrap();
    for heading in [
        "# Purpose",
        "# Dependency direction",
        "# Canonical identity",
        "# Capability policy",
        "# Known limitations",
    ] {
        assert!(provenance.contains(heading), "missing {heading}");
    }
}

fn collect_rs(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            output.push(path);
        }
    }
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}
