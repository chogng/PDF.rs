use std::fmt;
use std::sync::Arc;

use pdf_rs_scene::SceneBinding;

use crate::reference::{ReferenceRenderError, ReferenceRenderErrorCode};

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

/// Deterministic work and retained-capacity accounting for one published pixel buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceRenderStats {
    commands: u64,
    pixels: u64,
    fuel: u64,
    retained_bytes: u64,
    cancellation_checks: u64,
}

impl ReferenceRenderStats {
    pub(crate) const fn new(
        commands: u64,
        pixels: u64,
        fuel: u64,
        retained_bytes: u64,
        cancellation_checks: u64,
    ) -> Self {
        Self {
            commands,
            pixels,
            fuel,
            retained_bytes,
            cancellation_checks,
        }
    }

    /// Returns the traversed Scene command count.
    pub const fn commands(self) -> u64 {
        self.commands
    }

    /// Returns the complete output pixel count.
    pub const fn pixels(self) -> u64 {
        self.pixels
    }

    /// Returns deterministic command-plus-pixel work units.
    pub const fn fuel(self) -> u64 {
        self.fuel
    }

    /// Returns allocator-reported retained pixel-vector capacity.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
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
    binding: SceneBinding,
    config: ReferenceRenderConfig,
    stride_bytes: u64,
    rgba: Arc<Vec<u8>>,
    stats: ReferenceRenderStats,
}

impl CanonicalPixelBuffer {
    pub(crate) fn new(
        binding: SceneBinding,
        config: ReferenceRenderConfig,
        stride_bytes: u64,
        rgba: Vec<u8>,
        stats: ReferenceRenderStats,
    ) -> Self {
        Self {
            version: ReferencePixelBufferVersion::V1,
            binding,
            config,
            stride_bytes,
            rgba: Arc::new(rgba),
            stats,
        }
    }

    /// Returns the value-schema version.
    pub const fn version(&self) -> ReferencePixelBufferVersion {
        self.version
    }

    /// Returns the exact Native Scene source/page binding.
    pub const fn binding(&self) -> SceneBinding {
        self.binding
    }

    /// Returns the complete output configuration.
    pub const fn config(&self) -> ReferenceRenderConfig {
        self.config
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
