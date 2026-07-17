use crate::reference::{ReferenceRenderError, ReferenceRenderErrorCode};

const MIB: u64 = 1024 * 1024;
const HARD_MAX_WIDTH: u32 = 65_536;
const HARD_MAX_HEIGHT: u32 = 65_536;
const HARD_MAX_PIXELS: u64 = 268_435_456;
const HARD_MAX_STRIDE_BYTES: u64 = 256 * MIB;
const HARD_MAX_OUTPUT_BYTES: u64 = 1024 * MIB;
const HARD_MAX_COUNT: u64 = 64_000_000;
const HARD_MAX_SAMPLES: u64 = 4_000_000_000;
const HARD_MAX_FUEL: u64 = 4_000_000_000;
const HARD_MAX_COMPONENT_BYTES: u64 = 2 * 1024 * MIB;
const HARD_MAX_WORKING_BYTES: u64 = 8 * 1024 * MIB;
const HARD_MAX_RETAINED_BYTES: u64 = 1024 * MIB;
const HARD_MAX_CLIP_DEPTH: u32 = 4_096;
const HARD_MAX_CURVE_RECURSION: u8 = 32;

/// Unvalidated aggregate Reference pixel-production limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceRasterLimitConfig {
    /// Maximum output width in device pixels.
    pub max_width: u32,
    /// Maximum output height in device pixels.
    pub max_height: u32,
    /// Maximum complete output pixel count.
    pub max_pixels: u64,
    /// Maximum bytes in one top-down RGBA row.
    pub max_stride_bytes: u64,
    /// Maximum semantic RGBA bytes in one complete output.
    pub max_output_bytes: u64,
    /// Maximum Scene commands traversed and dispatched.
    pub max_commands: u64,
    /// Maximum Scene graphics resources admitted.
    pub max_resources: u64,
    /// Maximum Scene capability requirements traversed before dispatch.
    pub max_requirements: u64,
    /// Maximum capability dependency edges traversed before dispatch.
    pub max_dependencies: u64,
    /// Maximum flattened path and glyph segments across the page.
    pub max_geometry_segments: u64,
    /// Maximum fill and glyph edges across the page.
    pub max_geometry_edges: u64,
    /// Maximum scalar geometry coverage samples across the page.
    pub max_geometry_samples: u64,
    /// Maximum live bytes retained by one coverage mask.
    pub max_coverage_bytes: u64,
    /// Maximum dash chunks generated across the page.
    pub max_dash_chunks: u64,
    /// Maximum stroke runs generated across the page.
    pub max_stroke_runs: u64,
    /// Maximum stroke primitives generated across the page.
    pub max_stroke_primitives: u64,
    /// Maximum live transient flattened/stroke geometry bytes.
    pub max_geometry_bytes: u64,
    /// Maximum nested saved clip depth.
    pub max_clip_depth: u32,
    /// Maximum live current and saved clip-mask bytes.
    pub max_clip_bytes: u64,
    /// Maximum decoded source image pixels across the page.
    pub max_image_source_pixels: u64,
    /// Maximum decoded image row stride.
    pub max_image_stride_bytes: u64,
    /// Maximum decoded image bytes across the page.
    pub max_image_decoded_bytes: u64,
    /// Maximum image sample positions across the page.
    pub max_image_samples: u64,
    /// Maximum sampled image color conversions across the page.
    pub max_image_conversions: u64,
    /// Maximum positioned glyphs across the page.
    pub max_glyphs: u64,
    /// Maximum glyph resource lookups across the page.
    pub max_glyph_resource_lookups: u64,
    /// Maximum source outline segments across the page.
    pub max_glyph_outline_segments: u64,
    /// Maximum glyph coverage samples across the page.
    pub max_glyph_samples: u64,
    /// Maximum covered glyph samples composited across the page.
    pub max_glyph_composites: u64,
    /// Maximum adaptive curve recursion admitted by the profile.
    pub max_curve_recursion: u8,
    /// Maximum deterministic traversal, raster, compositing, and conversion work.
    pub max_fuel: u64,
    /// Maximum allocator-reported bytes in the private Q16 surface.
    pub max_surface_bytes: u64,
    /// Maximum simultaneous private surface, masks, geometry, and output bytes.
    pub max_peak_working_bytes: u64,
    /// Maximum allocator-reported bytes retained by the published RGBA buffer.
    pub max_retained_bytes: u64,
}

impl Default for ReferenceRasterLimitConfig {
    fn default() -> Self {
        Self {
            max_width: 16_384,
            max_height: 16_384,
            max_pixels: 67_108_864,
            max_stride_bytes: 64 * MIB,
            max_output_bytes: 256 * MIB,
            max_commands: 1_000_000,
            max_resources: 1_000_000,
            max_requirements: 1_000_000,
            max_dependencies: 4_000_000,
            max_geometry_segments: 16_000_000,
            max_geometry_edges: 16_000_000,
            max_geometry_samples: 1_000_000_000,
            max_coverage_bytes: 256 * MIB,
            max_dash_chunks: 4_000_000,
            max_stroke_runs: 1_000_000,
            max_stroke_primitives: 8_000_000,
            max_geometry_bytes: 256 * MIB,
            max_clip_depth: 256,
            max_clip_bytes: 256 * MIB,
            max_image_source_pixels: 67_108_864,
            max_image_stride_bytes: 64 * MIB,
            max_image_decoded_bytes: 256 * MIB,
            max_image_samples: 1_000_000_000,
            max_image_conversions: 1_000_000_000,
            max_glyphs: 4_000_000,
            max_glyph_resource_lookups: 4_000_000,
            max_glyph_outline_segments: 16_000_000,
            max_glyph_samples: 1_000_000_000,
            max_glyph_composites: 1_000_000_000,
            max_curve_recursion: 16,
            max_fuel: 1_000_000_000,
            max_surface_bytes: 1024 * MIB,
            max_peak_working_bytes: 2 * 1024 * MIB,
            max_retained_bytes: 256 * MIB,
        }
    }
}

/// Validated aggregate Reference pixel-production limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceRasterLimits {
    config: ReferenceRasterLimitConfig,
}

impl ReferenceRasterLimits {
    /// Validates every nonzero limit against fixed implementation hard ceilings.
    pub fn validate(config: ReferenceRasterLimitConfig) -> Result<Self, ReferenceRenderError> {
        let counts = [
            config.max_commands,
            config.max_resources,
            config.max_requirements,
            config.max_dependencies,
            config.max_geometry_segments,
            config.max_geometry_edges,
            config.max_dash_chunks,
            config.max_stroke_runs,
            config.max_stroke_primitives,
            config.max_glyphs,
            config.max_glyph_resource_lookups,
            config.max_glyph_outline_segments,
        ];
        let samples = [
            config.max_geometry_samples,
            config.max_image_samples,
            config.max_image_conversions,
            config.max_glyph_samples,
            config.max_glyph_composites,
        ];
        let component_bytes = [
            config.max_coverage_bytes,
            config.max_geometry_bytes,
            config.max_clip_bytes,
            config.max_image_stride_bytes,
            config.max_image_decoded_bytes,
            config.max_surface_bytes,
        ];
        if config.max_width == 0
            || config.max_width > HARD_MAX_WIDTH
            || config.max_height == 0
            || config.max_height > HARD_MAX_HEIGHT
            || config.max_pixels == 0
            || config.max_pixels > HARD_MAX_PIXELS
            || config.max_stride_bytes == 0
            || config.max_stride_bytes > HARD_MAX_STRIDE_BYTES
            || config.max_output_bytes == 0
            || config.max_output_bytes > HARD_MAX_OUTPUT_BYTES
            || counts
                .iter()
                .any(|value| *value == 0 || *value > HARD_MAX_COUNT)
            || samples
                .iter()
                .any(|value| *value == 0 || *value > HARD_MAX_SAMPLES)
            || component_bytes
                .iter()
                .any(|value| *value == 0 || *value > HARD_MAX_COMPONENT_BYTES)
            || config.max_image_source_pixels == 0
            || config.max_image_source_pixels > HARD_MAX_PIXELS
            || config.max_clip_depth == 0
            || config.max_clip_depth > HARD_MAX_CLIP_DEPTH
            || config.max_curve_recursion == 0
            || config.max_curve_recursion > HARD_MAX_CURVE_RECURSION
            || config.max_fuel == 0
            || config.max_fuel > HARD_MAX_FUEL
            || config.max_peak_working_bytes == 0
            || config.max_peak_working_bytes > HARD_MAX_WORKING_BYTES
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_RETAINED_BYTES
        {
            return Err(ReferenceRenderError::for_code(
                ReferenceRenderErrorCode::InvalidLimits,
            ));
        }
        Ok(Self { config })
    }

    /// Returns the complete validated configuration as a value.
    pub const fn config(self) -> ReferenceRasterLimitConfig {
        self.config
    }
}

macro_rules! limit_getters {
    ($(($name:ident, $field:ident, $ty:ty, $doc:literal)),* $(,)?) => {
        impl ReferenceRasterLimits {
            $(
                #[doc = $doc]
                pub const fn $name(self) -> $ty {
                    self.config.$field
                }
            )*
        }
    };
}

limit_getters!(
    (
        max_width,
        max_width,
        u32,
        "Returns the maximum output width."
    ),
    (
        max_height,
        max_height,
        u32,
        "Returns the maximum output height."
    ),
    (
        max_pixels,
        max_pixels,
        u64,
        "Returns the maximum output pixel count."
    ),
    (
        max_stride_bytes,
        max_stride_bytes,
        u64,
        "Returns the maximum RGBA row bytes."
    ),
    (
        max_output_bytes,
        max_output_bytes,
        u64,
        "Returns the maximum semantic output bytes."
    ),
    (
        max_commands,
        max_commands,
        u64,
        "Returns the maximum command count."
    ),
    (
        max_resources,
        max_resources,
        u64,
        "Returns the maximum resource count."
    ),
    (
        max_requirements,
        max_requirements,
        u64,
        "Returns the maximum requirement count."
    ),
    (
        max_dependencies,
        max_dependencies,
        u64,
        "Returns the maximum dependency-edge count."
    ),
    (
        max_geometry_segments,
        max_geometry_segments,
        u64,
        "Returns the aggregate segment limit."
    ),
    (
        max_geometry_edges,
        max_geometry_edges,
        u64,
        "Returns the aggregate edge limit."
    ),
    (
        max_geometry_samples,
        max_geometry_samples,
        u64,
        "Returns the aggregate geometry sample limit."
    ),
    (
        max_coverage_bytes,
        max_coverage_bytes,
        u64,
        "Returns the live coverage-mask byte limit."
    ),
    (
        max_dash_chunks,
        max_dash_chunks,
        u64,
        "Returns the aggregate dash-chunk limit."
    ),
    (
        max_stroke_runs,
        max_stroke_runs,
        u64,
        "Returns the aggregate stroke-run limit."
    ),
    (
        max_stroke_primitives,
        max_stroke_primitives,
        u64,
        "Returns the aggregate stroke-primitive limit."
    ),
    (
        max_geometry_bytes,
        max_geometry_bytes,
        u64,
        "Returns the transient geometry byte limit."
    ),
    (
        max_clip_depth,
        max_clip_depth,
        u32,
        "Returns the saved clip-depth limit."
    ),
    (
        max_clip_bytes,
        max_clip_bytes,
        u64,
        "Returns the live clip-mask byte limit."
    ),
    (
        max_image_source_pixels,
        max_image_source_pixels,
        u64,
        "Returns the aggregate source-image pixel limit."
    ),
    (
        max_image_stride_bytes,
        max_image_stride_bytes,
        u64,
        "Returns the decoded image stride limit."
    ),
    (
        max_image_decoded_bytes,
        max_image_decoded_bytes,
        u64,
        "Returns the aggregate decoded-image byte limit."
    ),
    (
        max_image_samples,
        max_image_samples,
        u64,
        "Returns the aggregate image sample limit."
    ),
    (
        max_image_conversions,
        max_image_conversions,
        u64,
        "Returns the aggregate image conversion limit."
    ),
    (
        max_glyphs,
        max_glyphs,
        u64,
        "Returns the aggregate glyph limit."
    ),
    (
        max_glyph_resource_lookups,
        max_glyph_resource_lookups,
        u64,
        "Returns the aggregate glyph lookup limit."
    ),
    (
        max_glyph_outline_segments,
        max_glyph_outline_segments,
        u64,
        "Returns the aggregate outline-segment limit."
    ),
    (
        max_glyph_samples,
        max_glyph_samples,
        u64,
        "Returns the aggregate glyph sample limit."
    ),
    (
        max_glyph_composites,
        max_glyph_composites,
        u64,
        "Returns the aggregate glyph composite limit."
    ),
    (
        max_curve_recursion,
        max_curve_recursion,
        u8,
        "Returns the adaptive curve-recursion limit."
    ),
    (
        max_fuel,
        max_fuel,
        u64,
        "Returns the aggregate deterministic fuel limit."
    ),
    (
        max_surface_bytes,
        max_surface_bytes,
        u64,
        "Returns the private Q16 surface byte limit."
    ),
    (
        max_peak_working_bytes,
        max_peak_working_bytes,
        u64,
        "Returns the simultaneous private working-byte limit."
    ),
    (
        max_retained_bytes,
        max_retained_bytes,
        u64,
        "Returns the published RGBA retained-byte limit."
    ),
);

impl Default for ReferenceRasterLimits {
    fn default() -> Self {
        Self::validate(ReferenceRasterLimitConfig::default())
            .expect("built-in Reference raster limits satisfy hard ceilings")
    }
}
