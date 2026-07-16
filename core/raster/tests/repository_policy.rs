use std::fs;
use std::path::{Path, PathBuf};

fn dependency_lines<'a>(manifest: &'a str, table: &str) -> Vec<&'a str> {
    manifest
        .split_once(table)
        .unwrap_or_else(|| panic!("manifest must declare {table}"))
        .1
        .split("\n[")
        .next()
        .expect("dependency table body is present")
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
        .collect()
}

fn collect_rust_sources(root: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("source directory must be readable") {
        let path = entry.expect("source entry must be readable").path();
        if path.is_dir() {
            collect_rust_sources(&path, paths);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            paths.push(path);
        }
    }
}

#[test]
fn product_raster_has_one_native_scene_dependency_and_no_platform_runtime() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_root.join("Cargo.toml"))
        .expect("raster manifest must be readable");
    assert_eq!(
        dependency_lines(&manifest, "[dependencies]"),
        [r#"pdf-rs-scene = { path = "../scene" }"#]
    );
    assert_eq!(
        dependency_lines(&manifest, "[dev-dependencies]"),
        [
            r#"pdf-rs-bytes = { path = "../bytes" }"#,
            r#"pdf-rs-syntax = { path = "../syntax" }"#,
        ]
    );
    for forbidden_table in ["[build-dependencies]", "[target."] {
        assert!(
            !manifest.contains(forbidden_table),
            "core/raster must not declare {forbidden_table} dependencies"
        );
    }
    let scene_manifest = fs::read_to_string(crate_root.join("../scene/Cargo.toml"))
        .expect("Scene manifest must be readable");
    assert!(
        !scene_manifest.contains("pdf-rs-raster"),
        "Scene must not depend upward on raster"
    );

    let mut paths = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut paths);
    let sources = paths
        .iter()
        .map(|path| fs::read_to_string(path).expect("raster source must be readable"))
        .collect::<Vec<_>>()
        .join("\n");
    let lowercase = sources.to_ascii_lowercase();
    for forbidden in [
        "unsafe {",
        "unsafe fn",
        "unsafe impl",
        "unsafe extern",
        "extern \"c\"",
        "std::fs",
        "std::net",
        "std::process",
        "tokio",
        "async_std",
        "async-std",
        "reqwest",
        "hyper",
        "wgpu",
        "metal",
        "vulkan",
        "opengl",
        "core_graphics",
        "skia",
        "cairo",
        "pdfium",
        "mupdf",
        "poppler",
        "wasm_bindgen",
        "web_sys",
    ] {
        assert!(
            !lowercase.contains(forbidden),
            "forbidden product raster token {forbidden:?}"
        );
    }
    assert!(sources.contains("#![forbid(unsafe_code)]"));
    assert!(sources.contains("#![deny(missing_docs)]"));
}

#[test]
fn reference_foundation_keeps_atomic_bounded_scene_consumption_explicit() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let render = fs::read_to_string(crate_root.join("src/reference/render.rs"))
        .expect("Reference render source must be readable");
    let model = fs::read_to_string(crate_root.join("src/reference/model.rs"))
        .expect("Reference model source must be readable");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance must be readable");

    for required in [
        "scene: Option<Arc<Scene>>",
        "self.scene.take()",
        "drop(scene);",
        "SceneCommandKind::BeginMarkedContent | SceneCommandKind::EndMarkedContent => {}",
        "check_cancellation(cancellation, &mut cancellation_checks)?;\n    let mut rgba = Vec::new();",
        "rgba.try_reserve_exact(required_capacity)",
        "if rgba.len() != required_capacity",
        "ReferenceRenderStats::new(commands, pixels, fuel, retained_bytes, cancellation_checks)",
    ] {
        assert!(
            render.contains(required),
            "Reference renderer must retain invariant marker {required:?}"
        );
    }
    assert!(
        !render.contains("_ =>"),
        "Scene commands must remain exhaustively matched"
    );
    for required in [
        "pub struct CanonicalPixelBuffer",
        "This type is not the worker/session-owned transferable `Surface` lifecycle",
        ".field(\"pixels\", &\"[REDACTED]\")",
        "OpaqueSrgbStraightRgba8V1",
        "\"sRGB-reference-v1\"",
    ] {
        assert!(
            model.contains(required),
            "Reference pixel value must retain contract marker {required:?}"
        );
    }
    for required in [
        "It is not the final",
        "`reference-raster-v1` algorithm",
        "not the worker/session-owned transferable `Surface` lifecycle",
        "No visible Scene command is supported yet",
        "no O0/O1 pixel authority",
    ] {
        assert!(
            provenance.contains(required),
            "provenance must retain bounded-scope marker {required:?}"
        );
    }
}
