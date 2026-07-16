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

fn top_level_version(document: &str) -> Option<&str> {
    document
        .lines()
        .take_while(|line| !line.starts_with("[["))
        .find_map(|line| line.strip_prefix("version = \"")?.strip_suffix('"'))
}

fn record_with_id<'a>(document: &'a str, kind: &str, id: &str) -> Option<&'a str> {
    let header = format!("{kind}]]");
    let id_line = format!("id = \"{id}\"");
    document
        .split("\n[[")
        .find(|record| record.starts_with(&header) && record.lines().any(|line| line == id_line))
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

#[test]
fn m3_reference_pixel_foundation_is_traceable_without_maturity_overclaim() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/raster has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature map must be readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec map must be readable");
    let capability_profiles =
        fs::read_to_string(repository_root.join("docs/traceability/capability-profiles.toml"))
            .expect("capability profiles must be readable");
    let review =
        fs::read_to_string(repository_root.join(
            "docs/traceability/evidence/m3/reference-pixel-foundation/independent-review.toml",
        ))
        .expect("M3-01 review evidence must be readable");
    let plan =
        fs::read_to_string(repository_root.join("plan/m3.toml")).expect("M3 plan must be readable");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance must be readable");

    assert_eq!(top_level_version(&feature_map), Some("0.70.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.70.0"));
    assert_eq!(
        top_level_version(&feature_map),
        top_level_version(&spec_map),
        "feature and specification maps advance together"
    );

    let feature = record_with_id(&feature_map, "feature", "core.reference-pixel-foundation")
        .expect("Reference pixel foundation feature must be registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m3.reference-pixel-foundation.v1\"",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/8.1-8.3",
        "RPE-ARCH-001/15.3/M3",
        "modules = [\"core/raster\"]",
        "core/raster::reference_foundation",
        "core/raster::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "Reference pixel feature must contain {required:?}"
        );
    }

    let scene_requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/6.4-6.7")
        .expect("Scene architecture requirement must be registered");
    for required in [
        "core.reference-pixel-foundation",
        "\"core/raster\"",
        "core/raster::reference_foundation",
        "one pure exhaustive Scene consumer",
        "does not yet add paths",
        "status = \"partial\"",
    ] {
        assert!(
            scene_requirement.contains(required),
            "Scene requirement must contain {required:?}"
        );
    }

    let reference_requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/8.1-8.3")
        .expect("Reference architecture requirement must be registered");
    for required in [
        "features = [\"core.reference-pixel-foundation\"]",
        "implementation = [\"core/raster\"]",
        "sRGB-reference-v1",
        "not the final reference-raster-v1",
        "no O0/O1 oracle registration",
        "status = \"partial\"",
    ] {
        assert!(
            reference_requirement.contains(required),
            "Reference requirement must contain {required:?}"
        );
    }

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M3")
        .expect("M3 milestone requirement must be registered");
    for required in [
        "M3-01 is complete",
        "M3-02 through M3-11 remain planned",
        "tools/quality::purity",
        "does not claim visible PDF rendering",
        "status = \"partial\"",
    ] {
        assert!(
            milestone.contains(required),
            "M3 requirement must contain {required:?}"
        );
    }

    let m3_header = plan
        .split("[[work_item]]")
        .next()
        .expect("M3 plan has a milestone header");
    assert!(m3_header.contains("status = \"in_progress\""));
    assert!(m3_header.contains("started_at = 2026-07-16"));
    let m3_01 = record_with_id(&plan, "work_item", "M3-01").expect("M3-01 must exist");
    assert!(m3_01.contains("status = \"complete\""));
    assert!(m3_01.contains("completed_at = 2026-07-16"));
    for index in 2..=11 {
        let id = format!("M3-{index:02}");
        let item = record_with_id(&plan, "work_item", &id)
            .unwrap_or_else(|| panic!("{id} work item must exist"));
        assert!(
            item.contains("status = \"planned\""),
            "{id} must remain planned"
        );
    }
    assert!(
        !capability_profiles.contains("m3.reference-pixel-foundation.v1"),
        "M3-01 must not create a maturity profile"
    );

    for required in [
        "work_item = \"M3-01\"",
        "profile = \"m3.reference-pixel-foundation.v1\"",
        "implementation_commit = \"3faef55df731fda03090238d1d83acc4bbfa675d\"",
        "reviewer_roles = [\"spec-conformance\", \"parser-security\"]",
        "maturity_promotion = false",
        "open_p0_p2 = 0",
        "verdict = \"SHIP\"",
    ] {
        assert!(
            review.contains(required),
            "review evidence must contain {required:?}"
        );
    }

    for required in [
        "# Traceability profile boundaries",
        "`m3.reference-pixel-foundation.v1`",
        "registered as `PLANNED`",
        "not a `REFERENCE` maturity promotion",
        "not an O0/O1 pixel authority",
        "not the final `reference-raster-v1` algorithm",
    ] {
        assert!(
            provenance.contains(required),
            "provenance must state {required:?}"
        );
    }
}
