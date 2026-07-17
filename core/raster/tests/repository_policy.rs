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
        "admit_nested_cardinalities(graphics, &mut work)?",
        "preflight_graphics(graphics, &mut work)?",
        "requirement.id().value() != index",
        "!valid_requirement_context(graphics, requirement)",
        "dependency.value() >= requirement.id().value()",
        "entry.id().value() != u32::try_from(index)",
        "let mut surface = ReferenceSurface::reserve",
        "work.charge_pixel_fuel",
        "dispatch_graphics(scene, graphics, &mut surface, &mut clips, &mut work)?",
        "paint_image(",
        "paint_glyph_run(",
        "work.absorb_geometry",
        "work.absorb_image",
        "work.absorb_glyph",
        "self.stats.coverage_bytes = 0",
        "postflight_surface_capacity",
        "postflight_output_capacity",
        "rgba.try_reserve_exact(required_capacity)",
        "if rgba.len() != required_capacity",
        "work.check_cancellation()?;\n    work.stats.retained_bytes = retained_bytes;",
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
        "pure Native integrated Reference renderer",
        "integrated `reference-raster-v1` renderer",
        "not the worker/session-owned transferable `Surface` lifecycle",
        "job-private premultiplied Q16 surface",
        "M3-11 still owns final milestone trace closure",
        "no O0/O1 pixel authority",
    ] {
        assert!(
            provenance.contains(required),
            "provenance must retain bounded-scope marker {required:?}"
        );
    }
}

#[test]
fn mounted_glyph_kernel_is_project_owned_bounded_and_renderer_integrated() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let glyph = fs::read_to_string(crate_root.join("src/reference/glyph.rs"))
        .expect("glyph source must be readable");
    let coverage = fs::read_to_string(crate_root.join("src/reference/coverage.rs"))
        .expect("coverage source must be readable");
    let module = fs::read_to_string(crate_root.join("src/reference/mod.rs"))
        .expect("Reference module source must be readable");
    let render = fs::read_to_string(crate_root.join("src/reference/render.rs"))
        .expect("Reference renderer source must be readable");
    let tests = fs::read_to_string(crate_root.join("tests/reference_glyph.rs"))
        .expect("glyph tests must be readable");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance must be readable");

    for required in [
        "pub(crate) const PROFILE: &'static str = \"reference-glyph-v1\"",
        "resources: &[GraphicsResourceEntry]",
        "resolve_outline(resources, glyph.outline())?",
        "page_map\n        .combined(glyph_to_page)?\n        .checked_concat(font_units_to_em)?",
        "rasterize_fill_union(",
        "FillRule::Nonzero",
        "ReferenceColorProfile::ReferenceColorV1.prepare_paint(run.paint())",
        "max_curve_recursion",
        "max_retained_bytes",
        "max_geometry_fuel",
        "tighten_geometry_bytes_limit(effective_geometry_bytes)",
        "coverage_geometry_peak",
        "peak_geometry_bytes",
        "GlyphLimitKind::RetainedBytes",
        "CANCELLATION_FUEL_INTERVAL: u64 = 256",
    ] {
        assert!(
            glyph.contains(required),
            "glyph kernel must retain invariant marker {required:?}"
        );
    }
    for required in [
        "pub(crate) fn rasterize_fill_union(",
        "mask.samples[index] | pixel_mask",
    ] {
        assert!(
            coverage.contains(required),
            "coverage union must retain invariant marker {required:?}"
        );
    }
    for forbidden in [
        "std::fs",
        "std::process",
        "fontconfig",
        "font-kit",
        "freetype",
        "harfbuzz",
        "core_text",
        "directwrite",
    ] {
        assert!(
            !glyph.to_ascii_lowercase().contains(forbidden),
            "glyph kernel must not use external font/runtime token {forbidden:?}"
        );
    }
    assert!(
        module.contains("mod glyph;"),
        "glyph kernel must be mounted in the product Reference module"
    );
    assert!(
        render.contains("paint_glyph_run("),
        "glyph kernel must paint in place through ReferenceRenderJob"
    );
    for required in [
        "one_em_square_uses_font_units_then_glyph_and_page_transforms",
        "page_rotations_apply_after_font_unit_and_glyph_transforms",
        "run_outlines_union_before_one_alpha_and_blend_application",
        "nonzero_glyph_resource_identifier_resolves_in_mixed_first_use_order",
        "every_glyph_budget_has_an_exact_and_one_less_boundary",
        "aggregate_retention_counts_coverage_plus_transient_geometry",
        "glyph_curve_recursion_is_bounded_and_singular_transforms_are_no_ops",
        "cancellation_is_observed_before_allocations_during_geometry_and_before_return",
    ] {
        assert!(
            tests.contains(required),
            "glyph harness must retain analytic/boundary test {required:?}"
        );
    }
    for required in [
        "`reference-glyph-v1`",
        "page-to-device, glyph-to-page, then design units divided",
        "unioned at the 64-bit sample-mask level",
        "coverage plus peak geometry",
        "The mounted glyph form likewise writes into the private Q16 surface",
        "remaining after the live surface and clip",
        "without system fonts",
    ] {
        assert!(
            provenance.contains(required),
            "glyph provenance must state {required:?}"
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
    let oracle_review = fs::read_to_string(
        repository_root
            .join("docs/traceability/evidence/m3/raster-oracle-contract/independent-review.toml"),
    )
    .expect("M3-02 review evidence must be readable");
    let text_review = fs::read_to_string(
        repository_root
            .join("docs/traceability/evidence/m3/basic-embedded-text/independent-review.toml"),
    )
    .expect("M3-09 review evidence must be readable");
    let plan =
        fs::read_to_string(repository_root.join("plan/m3.toml")).expect("M3 plan must be readable");
    let provenance =
        fs::read_to_string(crate_root.join("PROVENANCE.md")).expect("provenance must be readable");
    let ci =
        fs::read_to_string(repository_root.join("scripts/ci.sh")).expect("CI must be readable");

    assert_eq!(top_level_version(&feature_map), Some("0.76.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.76.0"));
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

    let text_feature = record_with_id(&feature_map, "feature", "core.basic-embedded-text")
        .expect("Basic embedded-text feature must be registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m3.basic-embedded-text.v1\"",
        "modules = [\"core/font\", \"core/document\", \"core/content\", \"core/scene\", \"core/raster\"]",
        "core/raster::reference_glyph",
        "core/raster::repository_policy",
        "tools/quality::m3_basic_text_trace",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            text_feature.contains(required),
            "Basic embedded-text feature must contain {required:?}"
        );
    }

    let oracle_feature =
        record_with_id(&feature_map, "feature", "quality.m3-raster-oracle-contract")
            .expect("M3 raster oracle feature must be registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m3.raster-oracle-contract.v1\"",
        "modules = [\"tools/compare\", \"tools/quality\", \"docs/traceability\"]",
        "tools/quality::m3_raster_oracle_contract",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            oracle_feature.contains(required),
            "M3 raster oracle feature must contain {required:?}"
        );
    }

    let color_feature = record_with_id(&feature_map, "feature", "core.reference-color-compositing")
        .expect("Reference color-compositing feature must be registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m3.reference-color-compositing.v1\"",
        "ISO-32000-1:2008/8.6",
        "ISO-32000-1:2008/11.3.2-11.3.4",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/8.1-8.3",
        "RPE-ARCH-001/15.3/M3",
        "modules = [\"core/raster\"]",
        "core/raster::reference_color",
        "core/raster::reference_scene_v2_boundary",
        "core/raster::repository_policy",
        "tools/quality::m3_reference_color_trace",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            color_feature.contains(required),
            "Reference color feature must contain {required:?}"
        );
    }

    let scene_requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/6.4-6.7")
        .expect("Scene architecture requirement must be registered");
    for required in [
        "core.reference-pixel-foundation",
        "core.content-graphics-v2",
        "core.scene-graphics-v2",
        "core.reference-color-compositing",
        "core.basic-image-xobjects",
        "core.basic-embedded-text",
        "\"core/raster\"",
        "core/raster::reference_foundation",
        "core/raster::reference_color",
        "core/raster::reference_image",
        "core/raster::reference_glyph",
        "tools/quality::m3_reference_color_trace",
        "tools/quality::m3_basic_image_trace",
        "tools/quality::m3_basic_text_trace",
        "M3-03 adds the incompatible m3.scene-graphics-v2.v1 schema",
        "M3-04 adds the first bounded producer",
        "M3-05 and M3-06 add independently bounded pure raster kernels",
        "M3-07 adds the allocation-free `reference-color-v1`",
        "named `soft-mask` capability",
        "M3-08 and M3-09 add proof-bound basic image and glyph Scene resources",
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
        "features = [\"core.reference-pixel-foundation\", \"core.reference-geometry-coverage\", \"core.reference-stroke-clip\", \"core.reference-color-compositing\", \"core.basic-image-xobjects\", \"core.basic-embedded-text\"]",
        "implementation = [\"core/raster\"]",
        "sRGB-reference-v1",
        "reference-color-v1",
        "structured unsupported color, blend, soft-mask, and group requirements",
        "M3-10 owns integrated ReferenceRenderJob acceptance and final dispatch",
        "register no final O0/O1 case authority",
        "All six linked feature records remain PLANNED",
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
        "core.reference-color-compositing",
        "core.basic-image-xobjects",
        "core.basic-embedded-text",
        "M3-01 through M3-04 close the bounded pixel foundation",
        "M3-05 and M3-06 close the commit-pinned geometry/coverage and stroke/clip stages",
        "M3-07 closes project-owned DeviceGray/RGB/CMYK conversion",
        "M3-08 now closes the commit-pinned basic unmasked Image XObject slice",
        "M3-09 now closes the commit-pinned bounded embedded simple TrueType slice",
        "Independent review evidence for all nine completed work items reports SHIP",
        "M3-10 and M3-11 still own integrated reference-raster-v1",
        "tools/quality::m3_raster_oracle_contract",
        "tools/quality::m3_content_graphics_trace",
        "tools/quality::m3_reference_color_trace",
        "tools/quality::m3_basic_image_trace",
        "tools/quality::m3_basic_text_trace",
        "tools/quality::purity",
        "does not claim integrated visible PDF rendering",
        "status = \"partial\"",
    ] {
        assert!(
            milestone.contains(required),
            "M3 requirement must contain {required:?}"
        );
    }

    let color_requirement = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/8.6")
        .expect("Device-color requirement must be registered");
    for required in [
        "core.reference-color-compositing",
        "implementation = [\"core/content\", \"core/scene\", \"core/raster\"]",
        "core/raster::reference_color",
        "core/raster::reference_scene_v2_boundary",
        "tools/quality::m3_reference_color_trace",
        "reference-color-v1",
        "RGB = 1 - min(1, CMY + K)",
        "unsupported color requirements fail structurally",
        "status = \"partial\"",
    ] {
        assert!(
            color_requirement.contains(required),
            "Device-color requirement must contain {required:?}"
        );
    }

    let transparency_requirement =
        record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/11.3.2-11.3.4")
            .expect("Transparency requirement must be registered");
    for required in [
        "features = [\"core.reference-color-compositing\"]",
        "implementation = [\"core/raster\"]",
        "core/raster::reference_color",
        "tools/quality::m3_reference_color_trace",
        "Normal, Multiply, and Screen source-over",
        "canonicalizes alpha zero to transparent black",
        "Soft masks and groups have named structured capability requirements",
        "status = \"partial\"",
    ] {
        assert!(
            transparency_requirement.contains(required),
            "Transparency requirement must contain {required:?}"
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
    let m3_02 = record_with_id(&plan, "work_item", "M3-02").expect("M3-02 must exist");
    assert!(m3_02.contains("status = \"complete\""));
    assert!(m3_02.contains("completed_at = 2026-07-16"));
    for index in 3..=9 {
        let id = format!("M3-{index:02}");
        let item = record_with_id(&plan, "work_item", &id)
            .unwrap_or_else(|| panic!("{id} work item must exist"));
        assert!(
            item.contains("status = \"complete\""),
            "{id} must be complete"
        );
        assert!(
            item.contains("completed_at = 2026-07-16"),
            "{id} must retain its completion date"
        );
    }
    for index in 10..=11 {
        let id = format!("M3-{index:02}");
        let item = record_with_id(&plan, "work_item", &id)
            .unwrap_or_else(|| panic!("{id} work item must exist"));
        assert!(
            item.contains("status = \"planned\""),
            "{id} must remain planned"
        );
    }
    assert!(
        !capability_profiles.contains("m3.content-graphics-v2.v1"),
        "M3-04 must not create a maturity profile"
    );
    assert!(
        !capability_profiles.contains("m3.reference-pixel-foundation.v1"),
        "M3-01 must not create a maturity profile"
    );
    assert!(
        !capability_profiles.contains("m3.raster-oracle-contract.v1"),
        "M3-02 must not create a maturity profile"
    );
    assert!(
        !capability_profiles.contains("m3.reference-color-compositing.v1"),
        "M3-07 must not create a maturity profile"
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
        "work_item = \"M3-09\"",
        "profile = \"m3.basic-embedded-text.v1\"",
        "feature = \"core.basic-embedded-text\"",
        "reviewer_roles = [\"spec-conformance\", \"parser-security\"]",
        "maturity_promotion = false",
        "open_p0_p2 = 0",
        "verdict = \"SHIP\"",
    ] {
        assert!(
            text_review.contains(required),
            "M3-09 review evidence must contain {required:?}"
        );
    }

    for required in [
        "work_item = \"M3-02\"",
        "profile = \"m3.raster-oracle-contract.v1\"",
        "reviewer_roles = [\"spec-conformance\", \"parser-security\"]",
        "maturity_promotion = false",
        "open_p0_p2 = 0",
        "verdict = \"SHIP\"",
    ] {
        assert!(
            oracle_review.contains(required),
            "M3-02 review evidence must contain {required:?}"
        );
    }

    assert!(
        ci.contains(
            "cargo test --locked --package pdf-rs-quality --test m3_content_graphics_trace"
        ),
        "M3-04 commit-bound evidence must have an explicit CI gate"
    );
    assert!(
        ci.contains(
            "cargo test --locked --package pdf-rs-quality --test m3_reference_geometry_trace"
        ),
        "M3-05/M3-06 commit-bound evidence must have an explicit CI gate"
    );
    assert!(
        ci.contains("cargo test --locked --package pdf-rs-quality --test m3_reference_color_trace"),
        "M3-07 commit-bound evidence must have an explicit CI gate"
    );
    assert!(
        ci.contains("cargo test --locked --package pdf-rs-quality --test m3_basic_text_trace"),
        "M3-09 commit-bound evidence must have an explicit CI gate"
    );

    for required in [
        "# Traceability profile boundaries",
        "`m3.reference-pixel-foundation.v1`",
        "`m3.reference-color-compositing.v1`",
        "registered as `PLANNED`",
        "not a `REFERENCE` maturity promotion",
        "not an O0/O1 pixel authority",
        "integrated `reference-raster-v1` renderer",
    ] {
        assert!(
            provenance.contains(required),
            "provenance must state {required:?}"
        );
    }
}
