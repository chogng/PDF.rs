use std::fmt;
use std::sync::Arc;

use pdf_rs_scene::SceneBinding;

use crate::reference::{
    ReferenceColorProfile, ReferenceRasterLimits, ReferenceRenderError, ReferenceRenderErrorCode,
};

/// Version of the immutable Reference pixel-buffer value schema.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReferencePixelBufferVersion {
    /// Initial value-only canonical pixel-buffer schema.
    V1,
}

/// Row origin and traversal direction for canonical pixels.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PixelOrigin {
    /// The first four bytes are the top-left pixel and rows proceed downward.
    TopLeft,
}

/// Canonical published pixel encoding.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PixelFormat {
    /// Eight-bit red, green, blue, and alpha channels in that byte order.
    Rgba8,
}

/// Alpha representation of published pixels.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlphaMode {
    /// Color channels are published without alpha premultiplication.
    Straight,
}

/// Fixed output encoding of the M3 Reference pixel foundation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReferenceOutputProfile {
    /// Top-down opaque `sRGB-reference-v1` straight-alpha RGBA8 output.
    OpaqueSrgbStraightRgba8V1,
}

/// Versioned deterministic Scene-to-pixel algorithm.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReferenceRasterAlgorithm {
    /// M3 fixed-point mapping, geometry, 8x8 coverage, resource sampling, and compositing rules.
    ReferenceRasterV1,
}

impl ReferenceRasterAlgorithm {
    /// Returns the stable renderer algorithm label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReferenceRasterV1 => "reference-raster-v1",
        }
    }
}

/// Exact versioned algorithms used to produce one canonical buffer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReferenceRenderIdentity {
    raster: ReferenceRasterAlgorithm,
    color: ReferenceColorProfile,
    output: ReferenceOutputProfile,
}

impl ReferenceRenderIdentity {
    pub(crate) const fn reference_v1(output: ReferenceOutputProfile) -> Self {
        Self {
            raster: ReferenceRasterAlgorithm::ReferenceRasterV1,
            color: ReferenceColorProfile::ReferenceColorV1,
            output,
        }
    }

    /// Returns the complete Scene-to-pixel algorithm identity.
    pub const fn raster(self) -> ReferenceRasterAlgorithm {
        self.raster
    }

    /// Returns the deterministic color and compositing profile.
    pub const fn color(self) -> ReferenceColorProfile {
        self.color
    }

    /// Returns the canonical byte-output profile.
    pub const fn output(self) -> ReferenceOutputProfile {
        self.output
    }

    /// Returns the mounted basic-image algorithm label.
    pub const fn image_label(self) -> &'static str {
        "reference-image-v1"
    }

    /// Returns the mounted embedded-glyph algorithm label.
    pub const fn glyph_label(self) -> &'static str {
        "reference-glyph-v1"
    }
}

/// Renderer-owned capability decision retained with a successful output.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReferenceCapabilityDecision {
    /// Every Scene requirement and command belongs to the exact mounted Reference profile.
    Supported,
}

impl ReferenceOutputProfile {
    /// Returns the canonical row origin.
    pub const fn origin(self) -> PixelOrigin {
        match self {
            Self::OpaqueSrgbStraightRgba8V1 => PixelOrigin::TopLeft,
        }
    }

    /// Returns the canonical pixel format.
    pub const fn pixel_format(self) -> PixelFormat {
        match self {
            Self::OpaqueSrgbStraightRgba8V1 => PixelFormat::Rgba8,
        }
    }

    /// Returns the published alpha representation.
    pub const fn alpha_mode(self) -> AlphaMode {
        match self {
            Self::OpaqueSrgbStraightRgba8V1 => AlphaMode::Straight,
        }
    }

    /// Returns the stable output-profile label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::OpaqueSrgbStraightRgba8V1 => "sRGB-reference-v1",
        }
    }
}

/// Positive device-pixel dimensions for one complete output.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DevicePixelSize {
    width: u32,
    height: u32,
}

impl DevicePixelSize {
    /// Creates positive dimensions without performing raster-budget admission.
    pub fn new(width: u32, height: u32) -> Result<Self, ReferenceRenderError> {
        if width == 0 || height == 0 {
            return Err(ReferenceRenderError::for_code(
                ReferenceRenderErrorCode::InvalidConfig,
            ));
        }
        Ok(Self { width, height })
    }

    /// Returns the output width in pixels.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Returns the output height in pixels.
    pub const fn height(self) -> u32 {
        self.height
    }
}

/// Immutable output configuration for the Reference pixel foundation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReferenceRenderConfig {
    size: DevicePixelSize,
    profile: ReferenceOutputProfile,
}

impl ReferenceRenderConfig {
    /// Creates opaque `sRGB-reference-v1` straight-alpha RGBA8 output.
    pub fn opaque_srgb(width: u32, height: u32) -> Result<Self, ReferenceRenderError> {
        Ok(Self {
            size: DevicePixelSize::new(width, height)?,
            profile: ReferenceOutputProfile::OpaqueSrgbStraightRgba8V1,
        })
    }

    /// Returns the exact device-pixel dimensions.
    pub const fn size(self) -> DevicePixelSize {
        self.size
    }

    /// Returns the fixed output encoding.
    pub const fn profile(self) -> ReferenceOutputProfile {
        self.profile
    }
}

/// Deterministic work and allocator-capacity accounting for one render attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReferenceRenderStats {
    pub(crate) commands: u64,
    pub(crate) resources: u64,
    pub(crate) requirements: u64,
    pub(crate) dependencies: u64,
    pub(crate) pixels: u64,
    pub(crate) geometry_segments: u64,
    pub(crate) geometry_edges: u64,
    pub(crate) geometry_samples: u64,
    pub(crate) coverage_bytes: u64,
    pub(crate) peak_coverage_bytes: u64,
    pub(crate) dash_chunks: u64,
    pub(crate) stroke_runs: u64,
    pub(crate) stroke_primitives: u64,
    pub(crate) geometry_bytes: u64,
    pub(crate) peak_geometry_bytes: u64,
    pub(crate) clip_depth: u64,
    pub(crate) clip_bytes: u64,
    pub(crate) peak_clip_bytes: u64,
    pub(crate) image_commands: u64,
    pub(crate) image_source_pixels: u64,
    pub(crate) image_stride_bytes: u64,
    pub(crate) image_decoded_bytes: u64,
    pub(crate) image_samples: u64,
    pub(crate) image_conversions: u64,
    pub(crate) glyph_runs: u64,
    pub(crate) glyphs: u64,
    pub(crate) glyph_resource_lookups: u64,
    pub(crate) glyph_outline_segments: u64,
    pub(crate) glyph_samples: u64,
    pub(crate) glyph_composites: u64,
    pub(crate) final_conversion_pixels: u64,
    pub(crate) fuel: u64,
    pub(crate) surface_bytes: u64,
    pub(crate) peak_working_bytes: u64,
    pub(crate) retained_bytes: u64,
    pub(crate) cancellation_checks: u64,
}

impl ReferenceRenderStats {
    pub(crate) const fn new(
        commands: u64,
        requirements: u64,
        pixels: u64,
        fuel: u64,
        retained_bytes: u64,
        cancellation_checks: u64,
    ) -> Self {
        Self {
            commands,
            resources: 0,
            requirements,
            dependencies: 0,
            pixels,
            geometry_segments: 0,
            geometry_edges: 0,
            geometry_samples: 0,
            coverage_bytes: 0,
            peak_coverage_bytes: 0,
            dash_chunks: 0,
            stroke_runs: 0,
            stroke_primitives: 0,
            geometry_bytes: 0,
            peak_geometry_bytes: 0,
            clip_depth: 0,
            clip_bytes: 0,
            peak_clip_bytes: 0,
            image_commands: 0,
            image_source_pixels: 0,
            image_stride_bytes: 0,
            image_decoded_bytes: 0,
            image_samples: 0,
            image_conversions: 0,
            glyph_runs: 0,
            glyphs: 0,
            glyph_resource_lookups: 0,
            glyph_outline_segments: 0,
            glyph_samples: 0,
            glyph_composites: 0,
            final_conversion_pixels: 0,
            fuel,
            surface_bytes: 0,
            peak_working_bytes: retained_bytes,
            retained_bytes,
            cancellation_checks,
        }
    }

    /// Returns the aggregate Scene command count admitted before command traversal.
    pub const fn commands(self) -> u64 {
        self.commands
    }

    /// Returns the aggregate capability requirement count admitted before nested traversal.
    pub const fn requirements(self) -> u64 {
        self.requirements
    }

    /// Returns graphics resources admitted by renderer-owned preflight.
    pub const fn resources(self) -> u64 {
        self.resources
    }

    /// Returns aggregate capability dependency edges admitted before nested preflight traversal.
    pub const fn dependencies(self) -> u64 {
        self.dependencies
    }

    /// Returns the complete output pixel count.
    pub const fn pixels(self) -> u64 {
        self.pixels
    }

    /// Returns flattened path and glyph segments charged across the page.
    pub const fn geometry_segments(self) -> u64 {
        self.geometry_segments
    }

    /// Returns fill and glyph edges charged across the page.
    pub const fn geometry_edges(self) -> u64 {
        self.geometry_edges
    }

    /// Returns scalar 8x8 geometry samples charged across the page.
    pub const fn geometry_samples(self) -> u64 {
        self.geometry_samples
    }

    /// Returns transient coverage bytes still live at the observable boundary.
    ///
    /// Completed commands and terminal unwinds report zero; retained clip masks are accounted by
    /// `clip_bytes`, while historical transient coverage remains in `peak_coverage_bytes`.
    pub const fn coverage_bytes(self) -> u64 {
        self.coverage_bytes
    }

    /// Returns the greatest allocator-reported live coverage-mask capacity, including failed work.
    pub const fn peak_coverage_bytes(self) -> u64 {
        self.peak_coverage_bytes
    }

    /// Returns generated dash chunks.
    pub const fn dash_chunks(self) -> u64 {
        self.dash_chunks
    }

    /// Returns generated stroke runs.
    pub const fn stroke_runs(self) -> u64 {
        self.stroke_runs
    }

    /// Returns generated stroke primitives.
    pub const fn stroke_primitives(self) -> u64 {
        self.stroke_primitives
    }

    /// Returns the greatest observed current transient geometry capacity in one child operation.
    pub const fn geometry_bytes(self) -> u64 {
        self.geometry_bytes
    }

    /// Returns the greatest simultaneous geometry capacity, including rejected actual replacement.
    pub const fn peak_geometry_bytes(self) -> u64 {
        self.peak_geometry_bytes
    }

    /// Returns saved clip depth at the latest completed observable command boundary.
    pub const fn clip_depth(self) -> u64 {
        self.clip_depth
    }

    /// Returns live current and saved clip bytes at the latest completed observable boundary.
    pub const fn clip_bytes(self) -> u64 {
        self.clip_bytes
    }

    /// Returns the greatest simultaneous clip-mask capacity, including failed temporary work.
    pub const fn peak_clip_bytes(self) -> u64 {
        self.peak_clip_bytes
    }

    /// Returns executed basic-image commands.
    pub const fn image_commands(self) -> u64 {
        self.image_commands
    }

    /// Returns aggregate decoded image source pixels admitted.
    pub const fn image_source_pixels(self) -> u64 {
        self.image_source_pixels
    }

    /// Returns the greatest decoded row stride admitted for an image command.
    pub const fn image_stride_bytes(self) -> u64 {
        self.image_stride_bytes
    }

    /// Returns aggregate decoded image bytes admitted.
    pub const fn image_decoded_bytes(self) -> u64 {
        self.image_decoded_bytes
    }

    /// Returns image sample positions inspected.
    pub const fn image_samples(self) -> u64 {
        self.image_samples
    }

    /// Returns image samples converted from a registered Device color.
    pub const fn image_conversions(self) -> u64 {
        self.image_conversions
    }

    /// Returns executed glyph-run commands.
    pub const fn glyph_runs(self) -> u64 {
        self.glyph_runs
    }

    /// Returns positioned glyph lookups executed across mounted glyph runs.
    pub const fn glyphs(self) -> u64 {
        self.glyphs
    }

    /// Returns glyph-outline resource lookups.
    pub const fn glyph_resource_lookups(self) -> u64 {
        self.glyph_resource_lookups
    }

    /// Returns project-owned outline segments inspected across glyph runs.
    pub const fn glyph_outline_segments(self) -> u64 {
        self.glyph_outline_segments
    }

    /// Returns 8x8 glyph coverage samples inspected.
    pub const fn glyph_samples(self) -> u64 {
        self.glyph_samples
    }

    /// Returns covered glyph samples composited.
    pub const fn glyph_composites(self) -> u64 {
        self.glyph_composites
    }

    /// Returns Q16 pixels converted to final straight RGBA8.
    pub const fn final_conversion_pixels(self) -> u64 {
        self.final_conversion_pixels
    }

    /// Returns deterministic preflight, initialization, raster, compositing, and conversion work.
    pub const fn fuel(self) -> u64 {
        self.fuel
    }

    /// Returns allocator-reported capacity retained by a successfully published RGBA buffer.
    ///
    /// Failed and unsupported jobs report zero even if private output allocation was attempted.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    /// Returns allocator-reported bytes allocated by the private Q16 working surface.
    ///
    /// This remains observable in failed-job statistics after the private surface is dropped.
    pub const fn surface_bytes(self) -> u64 {
        self.surface_bytes
    }

    /// Returns the greatest simultaneous allocator-reported private working allocation.
    ///
    /// An actual allocator overcapacity value is recorded before a component or aggregate
    /// postflight limit rejects that allocation.
    pub const fn peak_working_bytes(self) -> u64 {
        self.peak_working_bytes
    }

    /// Returns cooperative cancellation probes performed before publication.
    pub const fn cancellation_checks(self) -> u64 {
        self.cancellation_checks
    }
}

/// Immutable value-only canonical Reference pixel output.
///
/// This type is not the worker/session-owned transferable `Surface` lifecycle described for M4.
/// It owns only one complete pixel value and its exact Native Scene binding.
#[derive(Clone, Eq, PartialEq)]
pub struct CanonicalPixelBuffer {
    version: ReferencePixelBufferVersion,
    identity: ReferenceRenderIdentity,
    capability: ReferenceCapabilityDecision,
    binding: SceneBinding,
    config: ReferenceRenderConfig,
    limits: ReferenceRasterLimits,
    stride_bytes: u64,
    rgba: Arc<Vec<u8>>,
    stats: ReferenceRenderStats,
}

impl CanonicalPixelBuffer {
    pub(crate) fn new(
        identity: ReferenceRenderIdentity,
        binding: SceneBinding,
        config: ReferenceRenderConfig,
        limits: ReferenceRasterLimits,
        stride_bytes: u64,
        rgba: Vec<u8>,
        stats: ReferenceRenderStats,
    ) -> Self {
        Self {
            version: ReferencePixelBufferVersion::V1,
            identity,
            capability: ReferenceCapabilityDecision::Supported,
            binding,
            config,
            limits,
            stride_bytes,
            rgba: Arc::new(rgba),
            stats,
        }
    }

    /// Returns the value-schema version.
    pub const fn version(&self) -> ReferencePixelBufferVersion {
        self.version
    }

    /// Returns the exact renderer, color, resource, and output identity.
    pub const fn identity(&self) -> ReferenceRenderIdentity {
        self.identity
    }

    /// Returns the renderer-owned successful capability decision.
    pub const fn capability_decision(&self) -> ReferenceCapabilityDecision {
        self.capability
    }

    /// Returns the exact Native Scene source/page binding.
    pub const fn binding(&self) -> SceneBinding {
        self.binding
    }

    /// Returns the complete output configuration.
    pub const fn config(&self) -> ReferenceRenderConfig {
        self.config
    }

    /// Returns the complete validated raster profile used by this render.
    pub const fn limits(&self) -> ReferenceRasterLimits {
        self.limits
    }

    /// Returns the output width.
    pub const fn width(&self) -> u32 {
        self.config.size.width
    }

    /// Returns the output height.
    pub const fn height(&self) -> u32 {
        self.config.size.height
    }

    /// Returns the exact number of bytes in one top-down row.
    pub const fn stride_bytes(&self) -> u64 {
        self.stride_bytes
    }

    /// Borrows complete row-major RGBA8 bytes.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Returns deterministic work and retained-capacity accounting.
    pub const fn stats(&self) -> ReferenceRenderStats {
        self.stats
    }
}

impl fmt::Debug for CanonicalPixelBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalPixelBuffer")
            .field("version", &self.version)
            .field("identity", &self.identity)
            .field("capability", &self.capability)
            .field("page_index", &self.binding.page_index())
            .field("page_object", &self.binding.page_object())
            .field("config", &self.config)
            .field("stride_bytes", &self.stride_bytes)
            .field("rgba_bytes", &self.rgba.len())
            .field("stats", &self.stats)
            .field("pixels", &"[REDACTED]")
            .finish()
    }
}
