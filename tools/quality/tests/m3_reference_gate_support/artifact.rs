use std::fs;
use std::path::Path;

use pdf_rs_compare::{CanonicalJson, PixelArtifact};
use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_raster::reference::{
    CanonicalPixelBuffer, ReferenceRasterLimits, ReferenceRenderConfig, ReferenceRenderIdentity,
    ReferenceRenderStats,
};
use pdf_rs_scene::SceneBinding;

use super::pending::PendingEvent;
use super::registry::{CaseContract, REFERENCE_IMPLEMENTATION_SHA256};

const REFERENCE_IMPLEMENTATION_COMMIT: &str = "8c3e28c8ce4cbe5113cc565a36744158e283a7fb";
const REFERENCE_IMPLEMENTATION_TREE: &str = "724c2a646114a8aff0fabe29f6008a8b73802783";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NormalizedOutcome {
    pub(super) case_id: String,
    pub(super) outcome: &'static str,
    pub(super) stage: &'static str,
    pub(super) diagnostic_id: Option<&'static str>,
    pub(super) exact_verdict: &'static str,
    pub(super) expected_pixel_sha256: Option<String>,
    pub(super) input_sha256: String,
    pub(super) manifest_sha256: String,
    pub(super) oracle_level: String,
    pub(super) pixel_oracle_level: Option<String>,
    pub(super) scene_sha256: Option<String>,
    pub(super) pixel_sha256: Option<String>,
    pub(super) scene: Option<Vec<u8>>,
    pub(super) pixel: Option<Vec<u8>>,
    pub(super) audit: Vec<u8>,
}

#[derive(Clone, Copy)]
pub(super) struct RenderEvidence {
    pub(super) binding: SceneBinding,
    pub(super) config: ReferenceRenderConfig,
    pub(super) identity: ReferenceRenderIdentity,
    pub(super) limits: ReferenceRasterLimits,
    pub(super) stats: ReferenceRenderStats,
    pub(super) phase: &'static str,
}

#[derive(Clone, Copy)]
pub(super) struct LimitEvidence {
    pub(super) kind: &'static str,
    pub(super) limit: u64,
    pub(super) consumed: u64,
    pub(super) attempted: u64,
}

pub(super) struct OutcomeInput<'a> {
    pub(super) contract: &'a CaseContract,
    pub(super) outcome: &'static str,
    pub(super) stage: &'static str,
    pub(super) diagnostic_id: Option<&'static str>,
    pub(super) pending: &'a [PendingEvent],
    pub(super) scene: Option<Vec<u8>>,
    pub(super) pixel: Option<Vec<u8>>,
    pub(super) render: Option<RenderEvidence>,
    pub(super) limit: Option<LimitEvidence>,
}

pub(super) fn normalize(input: OutcomeInput<'_>) -> NormalizedOutcome {
    input.contract.assert_observed(
        input.outcome,
        input.stage,
        input.diagnostic_id,
        input.scene.as_deref(),
        input.pixel.as_deref(),
    );
    let input_sha256 = digest(&input.contract.input);
    assert_eq!(
        input_sha256, input.contract.input_sha256,
        "formal input hash changed after registry validation"
    );
    let scene_sha256 = input.scene.as_deref().map(digest);
    let pixel_sha256 = input.pixel.as_deref().map(digest);
    let audit = audit_json(
        &input,
        &input_sha256,
        scene_sha256.as_deref(),
        pixel_sha256.as_deref(),
    );
    NormalizedOutcome {
        case_id: input.contract.id().to_owned(),
        outcome: input.outcome,
        stage: input.stage,
        diagnostic_id: input.diagnostic_id,
        exact_verdict: "pass",
        expected_pixel_sha256: input.contract.expected_pixel_sha256.clone(),
        input_sha256,
        manifest_sha256: input.contract.manifest_sha256.clone(),
        oracle_level: input.contract.oracle_level.clone(),
        pixel_oracle_level: input.contract.pixel_oracle_level.clone(),
        scene_sha256,
        pixel_sha256,
        scene: input.scene,
        pixel: input.pixel,
        audit,
    }
}

pub(super) fn canonical_pixel(buffer: &CanonicalPixelBuffer) -> Vec<u8> {
    PixelArtifact::new(buffer.width(), buffer.height(), buffer.rgba().to_vec())
        .expect("Reference output is a complete RGBA8 pixel artifact")
        .to_canonical_json()
        .into_bytes()
}

pub(super) fn write_outputs(root: &Path, outcomes: &[NormalizedOutcome]) {
    assert!(
        root.file_name().is_some(),
        "M3 Reference output must name a dedicated directory"
    );
    if root.exists() {
        assert!(
            root.is_dir(),
            "existing M3 Reference output root must be a directory"
        );
        assert!(
            fs::read_dir(root)
                .expect("existing M3 Reference output root is readable")
                .next()
                .is_none(),
            "existing M3 Reference output root must be empty"
        );
    }
    fs::create_dir_all(root).expect("M3 Reference gate output root is writable");
    fs::write(root.join("result.json"), result_json(outcomes))
        .expect("canonical M3 Reference result is writable");

    for outcome in outcomes {
        let directory = root.join(&outcome.case_id);
        fs::create_dir_all(&directory).expect("case artifact directory is writable");
        fs::write(directory.join("audit.json"), &outcome.audit)
            .expect("canonical case audit is writable");
        if let Some(scene) = &outcome.scene {
            fs::write(directory.join("scene.json"), scene)
                .expect("canonical Native Scene is writable");
        }
        if let Some(pixel) = &outcome.pixel {
            fs::write(directory.join("pixel.json"), pixel)
                .expect("canonical Reference pixels are writable");
        }
    }
}

fn result_json(outcomes: &[NormalizedOutcome]) -> Vec<u8> {
    let mut output = String::from("{\"cases\":[");
    for (index, outcome) in outcomes.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        output.push_str("{\"case_id\":");
        push_string(&mut output, &outcome.case_id);
        output.push_str(",\"diagnostic_id\":");
        push_optional_string(&mut output, outcome.diagnostic_id);
        output.push_str(",\"exact_verdict\":");
        push_string(&mut output, outcome.exact_verdict);
        output.push_str(",\"expected_pixel_sha256\":");
        push_optional_string(&mut output, outcome.expected_pixel_sha256.as_deref());
        output.push_str(",\"input_sha256\":");
        push_string(&mut output, &outcome.input_sha256);
        output.push_str(",\"manifest_sha256\":");
        push_string(&mut output, &outcome.manifest_sha256);
        output.push_str(",\"oracle_level\":");
        push_string(&mut output, &outcome.oracle_level);
        output.push_str(",\"outcome\":");
        push_string(&mut output, outcome.outcome);
        output.push_str(",\"pixel_oracle_level\":");
        push_optional_string(&mut output, outcome.pixel_oracle_level.as_deref());
        output.push_str(",\"pixel_sha256\":");
        push_optional_string(&mut output, outcome.pixel_sha256.as_deref());
        output.push_str(",\"scene_sha256\":");
        push_optional_string(&mut output, outcome.scene_sha256.as_deref());
        output.push_str(",\"stage\":");
        push_string(&mut output, outcome.stage);
        output.push('}');
    }
    output.push_str("],\"schema\":2}");
    output.into_bytes()
}

fn audit_json(
    input: &OutcomeInput<'_>,
    input_sha256: &str,
    scene_sha256: Option<&str>,
    pixel_sha256: Option<&str>,
) -> Vec<u8> {
    let mut output = String::from("{\"binding\":");
    if let Some(render) = input.render {
        push_binding(&mut output, render.binding);
    } else {
        output.push_str("null");
    }
    output.push_str(",\"capability_decision\":");
    push_optional_string(
        &mut output,
        match (input.outcome, input.render.is_some()) {
            ("ready", true) => Some("renderer-supported"),
            ("unsupported", true) => Some("renderer-unsupported"),
            ("unsupported", false) => Some("producer-unsupported"),
            _ => None,
        },
    );
    output.push_str(",\"case_id\":");
    push_string(&mut output, input.contract.id());
    output.push_str(",\"config\":");
    if let Some(render) = input.render {
        push_config(&mut output, render.config);
    } else {
        output.push_str("null");
    }
    output.push_str(",\"diagnostic_id\":");
    push_optional_string(&mut output, input.diagnostic_id);
    output.push_str(",\"exact_verdict\":\"pass\"");
    output.push_str(",\"expected_pixel_sha256\":");
    push_optional_string(&mut output, input.contract.expected_pixel_sha256.as_deref());
    output.push_str(",\"identity\":");
    if let Some(render) = input.render {
        push_identity(&mut output, render.identity);
    } else {
        output.push_str("null");
    }
    output.push_str(",\"input_sha256\":");
    push_string(&mut output, input_sha256);
    output.push_str(",\"manifest_sha256\":");
    push_string(&mut output, &input.contract.manifest_sha256);
    output.push_str(",\"limit\":");
    if let Some(limit) = input.limit {
        output.push_str("{\"attempted\":");
        push_u64(&mut output, limit.attempted);
        output.push_str(",\"consumed\":");
        push_u64(&mut output, limit.consumed);
        output.push_str(",\"kind\":");
        push_string(&mut output, limit.kind);
        output.push_str(",\"limit\":");
        push_u64(&mut output, limit.limit);
        output.push('}');
    } else {
        output.push_str("null");
    }
    output.push_str(",\"limits\":");
    if let Some(render) = input.render {
        push_limits(&mut output, render.limits);
    } else {
        output.push_str("null");
    }
    output.push_str(",\"outcome\":");
    push_string(&mut output, input.outcome);
    output.push_str(",\"oracle_level\":");
    push_string(&mut output, &input.contract.oracle_level);
    output.push_str(",\"pending\":[");
    for (index, event) in input.pending.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        output.push_str("{\"checkpoint_role\":");
        push_string(&mut output, event.checkpoint_role);
        output.push_str(",\"ordinal\":");
        push_u64(&mut output, event.ordinal);
        output.push_str(",\"ranges\":[");
        for (range_index, (start, len)) in event.ranges.iter().enumerate() {
            if range_index != 0 {
                output.push(',');
            }
            output.push_str("{\"len\":");
            push_u64(&mut output, *len);
            output.push_str(",\"start\":");
            push_u64(&mut output, *start);
            output.push('}');
        }
        output.push_str("],\"stage\":");
        push_string(&mut output, event.stage);
        output.push('}');
    }
    output.push_str("],\"pixel_sha256\":");
    push_optional_string(&mut output, pixel_sha256);
    output.push_str(",\"pixel_oracle_level\":");
    push_optional_string(&mut output, input.contract.pixel_oracle_level.as_deref());
    output.push_str(",\"render_phase\":");
    push_optional_string(&mut output, input.render.map(|render| render.phase));
    output.push_str(",\"scene_sha256\":");
    push_optional_string(&mut output, scene_sha256);
    output.push_str(",\"schema\":2,\"stage\":");
    push_string(&mut output, input.stage);
    output.push_str(",\"stats\":");
    if let Some(render) = input.render {
        push_stats(&mut output, render.stats);
    } else {
        output.push_str("null");
    }
    output.push('}');
    output.into_bytes()
}

fn push_binding(output: &mut String, binding: SceneBinding) {
    let source = binding.source();
    output.push_str("{\"page_index\":");
    push_u64(output, u64::from(binding.page_index()));
    output.push_str(",\"page_object\":{\"generation\":");
    push_u64(output, u64::from(binding.page_object().generation()));
    output.push_str(",\"number\":");
    push_u64(output, u64::from(binding.page_object().number()));
    output.push_str("},\"revision_startxref\":");
    push_u64(output, binding.revision_startxref());
    output.push_str(",\"source_revision\":");
    push_u64(output, source.revision().value());
    output.push_str(",\"source_stable_id_hex\":\"");
    output.push_str(&hex_digest(&source.stable_id().digest()));
    output.push_str("\"}");
}

fn push_config(output: &mut String, config: ReferenceRenderConfig) {
    output.push_str("{\"alpha\":\"straight\",\"height\":");
    push_u64(output, u64::from(config.size().height()));
    output.push_str(",\"origin\":\"top-left\",\"pixel_format\":\"rgba8\",\"profile\":");
    push_string(output, config.profile().label());
    output.push_str(",\"width\":");
    push_u64(output, u64::from(config.size().width()));
    output.push('}');
}

fn push_identity(output: &mut String, identity: ReferenceRenderIdentity) {
    output.push_str("{\"color\":");
    push_string(output, identity.color().label());
    output.push_str(",\"glyph\":");
    push_string(output, identity.glyph_label());
    output.push_str(",\"image\":");
    push_string(output, identity.image_label());
    output.push_str(",\"implementation_commit\":");
    push_string(output, REFERENCE_IMPLEMENTATION_COMMIT);
    output.push_str(",\"implementation_sha256\":");
    push_string(output, REFERENCE_IMPLEMENTATION_SHA256);
    output.push_str(",\"implementation_tree\":");
    push_string(output, REFERENCE_IMPLEMENTATION_TREE);
    output.push_str(",\"output\":");
    push_string(output, identity.output().label());
    output.push_str(",\"raster\":");
    push_string(output, identity.raster().label());
    output.push('}');
}

fn push_limits(output: &mut String, limits: ReferenceRasterLimits) {
    let config = limits.config();
    let fields = [
        ("max_clip_bytes", config.max_clip_bytes),
        ("max_clip_depth", u64::from(config.max_clip_depth)),
        ("max_commands", config.max_commands),
        ("max_coverage_bytes", config.max_coverage_bytes),
        ("max_curve_recursion", u64::from(config.max_curve_recursion)),
        ("max_dash_chunks", config.max_dash_chunks),
        ("max_dependencies", config.max_dependencies),
        ("max_fuel", config.max_fuel),
        ("max_geometry_bytes", config.max_geometry_bytes),
        ("max_geometry_edges", config.max_geometry_edges),
        ("max_geometry_samples", config.max_geometry_samples),
        ("max_geometry_segments", config.max_geometry_segments),
        ("max_glyph_composites", config.max_glyph_composites),
        (
            "max_glyph_outline_segments",
            config.max_glyph_outline_segments,
        ),
        (
            "max_glyph_resource_lookups",
            config.max_glyph_resource_lookups,
        ),
        ("max_glyph_samples", config.max_glyph_samples),
        ("max_glyphs", config.max_glyphs),
        ("max_height", u64::from(config.max_height)),
        ("max_image_conversions", config.max_image_conversions),
        ("max_image_decoded_bytes", config.max_image_decoded_bytes),
        ("max_image_samples", config.max_image_samples),
        ("max_image_source_pixels", config.max_image_source_pixels),
        ("max_image_stride_bytes", config.max_image_stride_bytes),
        ("max_output_bytes", config.max_output_bytes),
        ("max_peak_working_bytes", config.max_peak_working_bytes),
        ("max_pixels", config.max_pixels),
        ("max_requirements", config.max_requirements),
        ("max_resources", config.max_resources),
        ("max_retained_bytes", config.max_retained_bytes),
        ("max_stride_bytes", config.max_stride_bytes),
        ("max_stroke_primitives", config.max_stroke_primitives),
        ("max_stroke_runs", config.max_stroke_runs),
        ("max_surface_bytes", config.max_surface_bytes),
        ("max_width", u64::from(config.max_width)),
    ];
    push_u64_object(output, &fields);
}

fn push_stats(output: &mut String, stats: ReferenceRenderStats) {
    let fields = [
        ("cancellation_checks", stats.cancellation_checks()),
        ("clip_bytes", stats.clip_bytes()),
        ("clip_depth", stats.clip_depth()),
        ("commands", stats.commands()),
        ("coverage_bytes", stats.coverage_bytes()),
        ("dash_chunks", stats.dash_chunks()),
        ("dependencies", stats.dependencies()),
        ("final_conversion_pixels", stats.final_conversion_pixels()),
        ("fuel", stats.fuel()),
        ("geometry_bytes", stats.geometry_bytes()),
        ("geometry_edges", stats.geometry_edges()),
        ("geometry_samples", stats.geometry_samples()),
        ("geometry_segments", stats.geometry_segments()),
        ("glyph_composites", stats.glyph_composites()),
        ("glyph_outline_segments", stats.glyph_outline_segments()),
        ("glyph_resource_lookups", stats.glyph_resource_lookups()),
        ("glyph_runs", stats.glyph_runs()),
        ("glyph_samples", stats.glyph_samples()),
        ("glyphs", stats.glyphs()),
        ("image_commands", stats.image_commands()),
        ("image_conversions", stats.image_conversions()),
        ("image_decoded_bytes", stats.image_decoded_bytes()),
        ("image_samples", stats.image_samples()),
        ("image_source_pixels", stats.image_source_pixels()),
        ("image_stride_bytes", stats.image_stride_bytes()),
        ("peak_clip_bytes", stats.peak_clip_bytes()),
        ("peak_coverage_bytes", stats.peak_coverage_bytes()),
        ("peak_geometry_bytes", stats.peak_geometry_bytes()),
        ("peak_working_bytes", stats.peak_working_bytes()),
        ("pixels", stats.pixels()),
        ("requirements", stats.requirements()),
        ("resources", stats.resources()),
        ("retained_bytes", stats.retained_bytes()),
        ("stroke_primitives", stats.stroke_primitives()),
        ("stroke_runs", stats.stroke_runs()),
        ("surface_bytes", stats.surface_bytes()),
    ];
    push_u64_object(output, &fields);
}

fn push_u64_object(output: &mut String, fields: &[(&str, u64)]) {
    output.push('{');
    for (index, (key, value)) in fields.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        push_string(output, key);
        output.push(':');
        push_u64(output, *value);
    }
    output.push('}');
}

fn digest(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex_digest(&sha256(bytes).expect("M3 gate artifact fits SHA-256 framing"))
    )
}

fn push_optional_string(output: &mut String, value: Option<&str>) {
    if let Some(value) = value {
        push_string(output, value);
    } else {
        output.push_str("null");
    }
}

fn push_string(output: &mut String, value: &str) {
    assert!(
        value
            .bytes()
            .all(|byte| !byte.is_ascii_control() && !b"\\\"".contains(&byte)),
        "M3 gate strings are stable identifiers requiring no JSON escaping"
    );
    output.push('"');
    output.push_str(value);
    output.push('"');
}

fn push_u64(output: &mut String, value: u64) {
    output.push_str(&value.to_string());
}
