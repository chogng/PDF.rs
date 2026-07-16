use crate::reference::{ReferenceRenderError, ReferenceRenderErrorCode};

const HARD_MAX_WIDTH: u32 = 65_536;
const HARD_MAX_HEIGHT: u32 = 65_536;
const HARD_MAX_PIXELS: u64 = 268_435_456;
const HARD_MAX_STRIDE_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_OUTPUT_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_COMMANDS: u64 = 4_000_000;
const HARD_MAX_REQUIREMENTS: u64 = 4_000_000;
const HARD_MAX_FUEL: u64 = 1_000_000_000;
const HARD_MAX_RETAINED_BYTES: u64 = 1024 * 1024 * 1024;

/// Unvalidated Reference pixel-production limits.
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
    /// Maximum Scene commands traversed by the foundation.
    pub max_commands: u64,
    /// Maximum Scene capability requirements traversed before dispatch.
    pub max_requirements: u64,
    /// Maximum deterministic requirement-plus-command-plus-pixel work units.
    pub max_fuel: u64,
    /// Maximum allocator-reported pixel-vector capacity.
    pub max_retained_bytes: u64,
}

impl Default for ReferenceRasterLimitConfig {
    fn default() -> Self {
        Self {
            max_width: 16_384,
            max_height: 16_384,
            max_pixels: 67_108_864,
            max_stride_bytes: 64 * 1024 * 1024,
            max_output_bytes: 256 * 1024 * 1024,
            max_commands: 1_000_000,
            max_requirements: 1_000_000,
            max_fuel: 128_000_000,
            max_retained_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Validated Reference pixel-production limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceRasterLimits {
    max_width: u32,
    max_height: u32,
    max_pixels: u64,
    max_stride_bytes: u64,
    max_output_bytes: u64,
    max_commands: u64,
    max_requirements: u64,
    max_fuel: u64,
    max_retained_bytes: u64,
}

impl ReferenceRasterLimits {
    /// Validates every nonzero limit against fixed implementation hard ceilings.
    pub fn validate(config: ReferenceRasterLimitConfig) -> Result<Self, ReferenceRenderError> {
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
            || config.max_commands == 0
            || config.max_commands > HARD_MAX_COMMANDS
            || config.max_requirements == 0
            || config.max_requirements > HARD_MAX_REQUIREMENTS
            || config.max_fuel == 0
            || config.max_fuel > HARD_MAX_FUEL
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_RETAINED_BYTES
        {
            return Err(ReferenceRenderError::for_code(
                ReferenceRenderErrorCode::InvalidLimits,
            ));
        }
        Ok(Self {
            max_width: config.max_width,
            max_height: config.max_height,
            max_pixels: config.max_pixels,
            max_stride_bytes: config.max_stride_bytes,
            max_output_bytes: config.max_output_bytes,
            max_commands: config.max_commands,
            max_requirements: config.max_requirements,
            max_fuel: config.max_fuel,
            max_retained_bytes: config.max_retained_bytes,
        })
    }

    /// Returns the maximum output width.
    pub const fn max_width(self) -> u32 {
        self.max_width
    }

    /// Returns the maximum output height.
    pub const fn max_height(self) -> u32 {
        self.max_height
    }

    /// Returns the maximum complete output pixel count.
    pub const fn max_pixels(self) -> u64 {
        self.max_pixels
    }

    /// Returns the maximum bytes in one row.
    pub const fn max_stride_bytes(self) -> u64 {
        self.max_stride_bytes
    }

    /// Returns the maximum semantic output byte count.
    pub const fn max_output_bytes(self) -> u64 {
        self.max_output_bytes
    }

    /// Returns the maximum traversed Scene command count.
    pub const fn max_commands(self) -> u64 {
        self.max_commands
    }

    /// Returns the maximum traversed capability requirement count.
    pub const fn max_requirements(self) -> u64 {
        self.max_requirements
    }

    /// Returns the maximum deterministic work units.
    pub const fn max_fuel(self) -> u64 {
        self.max_fuel
    }

    /// Returns the maximum allocator-reported retained pixel capacity.
    pub const fn max_retained_bytes(self) -> u64 {
        self.max_retained_bytes
    }
}

impl Default for ReferenceRasterLimits {
    fn default() -> Self {
        Self::validate(ReferenceRasterLimitConfig::default())
            .expect("built-in Reference raster limits satisfy hard ceilings")
    }
}
