use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn product_scene_has_only_lower_identity_dependencies_and_no_platform_io() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_root.join("Cargo.toml"))
        .expect("crate manifest must be readable during repository tests");
    let dependency_body = manifest
        .split_once("[dependencies]")
        .expect("crate manifest declares an explicit dependency table")
        .1
        .split("\n[")
        .next()
        .expect("dependency table body is present")
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        dependency_body,
        [
            r#"pdf-rs-bytes = { path = "../bytes" }"#,
            r#"pdf-rs-syntax = { path = "../syntax" }"#,
        ]
    );
    for forbidden_table in ["[dev-dependencies]", "[build-dependencies]", "[target."] {
        assert!(
            !manifest.contains(forbidden_table),
            "core/scene must not declare {forbidden_table} dependencies"
        );
    }

    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources);
    let joined = sources
        .iter()
        .map(|path| fs::read_to_string(path).expect("source must be readable"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    for forbidden in [
        "std::fs",
        "std::net",
        "async fn",
        "tokio",
        "async_std",
        "reqwest",
        "hyper",
        "pdfium",
        "mupdf",
        "pdf.js",
        "serde",
    ] {
        assert!(
            !joined.contains(forbidden),
            "forbidden product Scene token {forbidden:?}"
        );
    }
    assert!(joined.contains("#![forbid(unsafe_code)]"));
    assert!(joined.contains("#![deny(missing_docs)]"));
}

#[test]
fn canonical_scene_omits_runtime_source_identity_and_float_formatting() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let canonical = fs::read_to_string(crate_root.join("src/canonical.rs"))
        .expect("canonical Scene source is readable");
    let builder = fs::read_to_string(crate_root.join("src/builder.rs"))
        .expect("bounded Scene builder source is readable");
    let scalar =
        fs::read_to_string(crate_root.join("src/scalar.rs")).expect("scalar source is readable");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance is readable");
    let diff = fs::read_to_string(crate_root.join("src/diff.rs"))
        .expect("semantic Scene diff source is readable");

    assert!(!canonical.contains("binding().source()"));
    assert!(!canonical.contains("stable_id"));
    assert!(!canonical.contains("validator"));
    assert!(!canonical.contains("f32"));
    assert!(!canonical.contains("f64"));
    assert!(canonical.contains("push_i64(value.scaled())"));
    assert!(canonical.contains("push_hex(tag.bytes())"));
    assert!(canonical.contains("reserve_output(encoded_len)?"));
    assert!(!canonical.contains("self.push(&encoded)?"));
    assert!(builder.contains("preflight_append("));
    assert!(builder.contains("capacity_after_one("));
    assert!(!builder.contains("try_reserve_exact(1)"));
    assert!(scalar.contains("const SCALE: i128 = 1_000_000_000"));
    assert!(provenance.contains("runtime `SourceIdentity` is"));
    assert!(provenance.contains("deliberately omitted"));
    assert!(provenance.contains("nine-decimal fixed-point"));
    assert!(diff.contains("pub fn validate(config: SceneDiffLimitConfig)"));
    assert!(diff.contains("SceneLimitKind::Differences"));
    assert!(diff.contains("SceneLimitKind::DiffRetainedBytes"));
    assert!(diff.contains("SceneLimitKind::DiffCanonicalBytes"));
    assert!(!diff.contains("expected_binding.source()"));
    assert!(!diff.contains("actual_binding.source()"));
}

fn collect_rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory).expect("source directory must be readable");
    for entry in entries {
        let path = entry.expect("source entry must be readable").path();
        if path.is_dir() {
            collect_rust_sources(&path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            output.push(path);
        }
    }
}
