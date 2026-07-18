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
fn fast_raster_has_only_native_policy_and_scene_product_dependencies() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_root.join("Cargo.toml"))
        .expect("Fast raster manifest must be readable");
    assert_eq!(
        dependency_lines(&manifest, "[dependencies]"),
        [
            r#"pdf-rs-policy = { path = "../../runtime/policy" }"#,
            r#"pdf-rs-scene = { path = "../scene" }"#,
        ]
    );
    assert_eq!(
        dependency_lines(&manifest, "[dev-dependencies]"),
        [
            r#"pdf-rs-bytes = { path = "../bytes" }"#,
            r#"pdf-rs-digest = { path = "../../tools/digest" }"#,
            r#"pdf-rs-raster = { path = "../raster" }"#,
            r#"pdf-rs-syntax = { path = "../syntax" }"#,
        ]
    );
    for forbidden_table in ["[build-dependencies]", "[target."] {
        assert!(
            !manifest.contains(forbidden_table),
            "core/fast-raster must not declare {forbidden_table} dependencies"
        );
    }
    for dependency in ["../scene/Cargo.toml", "../../runtime/policy/Cargo.toml"] {
        let dependency_manifest = fs::read_to_string(crate_root.join(dependency))
            .expect("dependency manifest must be readable");
        assert!(
            !dependency_manifest.contains("pdf-rs-fast-raster"),
            "Fast raster dependencies must not depend upward on their consumer"
        );
    }

    let mut paths = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut paths);
    let sources = paths
        .iter()
        .map(|path| fs::read_to_string(path).expect("Fast raster source must be readable"))
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
            "forbidden product Fast raster token {forbidden:?}"
        );
    }
    assert!(sources.contains("#![forbid(unsafe_code)]"));
    assert!(sources.contains("#![deny(missing_docs)]"));
}

#[test]
fn fast_renderer_is_independent_bounded_and_atomically_published() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fast_root = crate_root.join("src/fast");
    let render = fs::read_to_string(fast_root.join("render.rs"))
        .expect("Fast render source must be readable");
    let kernels = fs::read_to_string(fast_root.join("kernels.rs"))
        .expect("Fast kernel source must be readable");
    let stroke = fs::read_to_string(fast_root.join("stroke.rs"))
        .expect("Fast stroke source must be readable");
    let limits =
        fs::read_to_string(fast_root.join("limits.rs")).expect("Fast limits must be readable");
    let tests = fs::read_to_string(crate_root.join("tests/fast_tiled.rs"))
        .expect("Fast tests must be readable");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance must be readable");

    let mut paths = Vec::new();
    collect_rust_sources(&fast_root, &mut paths);
    let fast_sources = paths
        .iter()
        .map(|path| fs::read_to_string(path).expect("Fast source must be readable"))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        "crate::reference",
        "ReferenceRenderJob",
        "CanonicalPixelBuffer",
        "std::fs",
        "std::net",
        "std::process",
        "unsafe {",
        "unsafe fn",
        "extern \"c\"",
    ] {
        assert!(
            !fast_sources.contains(forbidden),
            "Fast renderer must remain independent of {forbidden:?}"
        );
    }

    for required in [
        "validate_config(plan, limits)?",
        "validate_subject(scene, plan, cancellation)?",
        "let bins = build_bins",
        "record.command(),",
        "bins[tile_index].push",
        "validate_permutation",
        "let tile = self.render_one",
        "work.check()?;",
        "Ok(FastTileSet::new",
        "FastTile::new(",
        "GraphicsCommand::BeginIsolatedGroup",
    ] {
        assert!(
            render.contains(required),
            "Fast renderer must retain invariant marker {required:?}"
        );
    }
    assert!(
        !render.contains("_ =>"),
        "Fast graphics command dispatch must remain exhaustive"
    );
    for required in [
        "const SAMPLE_SIDE: i64 = 4;",
        "fn point_in_path",
        "fn composite_channel",
        "pub(crate) fn draw_image",
        "pub(crate) fn flatten_path",
    ] {
        assert!(
            kernels.contains(required),
            "Fast scalar kernels must retain {required:?}"
        );
    }
    for required in [
        "fn dashed_runs",
        "fn append_join",
        "LineCap::Square",
        "LineJoin::Miter",
        "stroke_to_device.inverse()?",
    ] {
        assert!(
            stroke.contains(required),
            "Fast scalar stroke kernel must retain {required:?}"
        );
    }
    for required in [
        "max_pixels",
        "max_commands",
        "max_bin_entries",
        "max_retained_bytes",
        "max_intermediate_bytes",
        "max_fuel",
        "max_cancellation_interval",
    ] {
        assert!(
            limits.contains(required),
            "Fast limits must retain independent dimension {required:?}"
        );
    }
    for required in [
        "whole_page_tiles_and_tile_order_are_metamorphic",
        "exact_rectangle_pixels_match_independently_enumerated_expectation",
        "registered_stroke_semantics_match_reviewed_reference_pixels",
        "advanced_stroke_joins_dashes_and_noncommuting_transforms_match_reference",
        "long_path_inner_loops_observe_cancellation_without_publication",
        "many_subpaths_are_fallibly_accounted_at_the_intermediate_boundary",
        "deep_clip_stack_uses_cached_payload_and_accounts_transient_growth",
        "every_fast_resource_dimension_has_exact_and_one_less_boundaries",
        "cancellation_and_malformed_permutations_never_publish_partial_sets",
        "complete_render_config_identity_is_retained",
    ] {
        assert!(
            tests.contains(required),
            "Fast harness must retain acceptance test {required:?}"
        );
    }
    for required in [
        "Fast CPU product-tile profile",
        "never reads or slices a Reference pixel buffer",
        "source order",
        "atomic publication",
        "tile-order permutation",
    ] {
        assert!(
            provenance.contains(required),
            "Fast provenance must retain {required:?}"
        );
    }
}
