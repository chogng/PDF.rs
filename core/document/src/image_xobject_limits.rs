use pdf_rs_filters::DecodeLimits;

use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_WIDTH: u32 = 65_536;
const HARD_MAX_HEIGHT: u32 = 65_536;
const HARD_MAX_PIXELS: u64 = 268_435_456;
const HARD_MAX_STRIDE_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_METADATA_ENTRIES: u64 = 65_536;
const HARD_MAX_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_ENCODED_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_DECODED_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_DECODE_FUEL: u64 = 8 * 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_BYTES: u64 = 1024 * 1024 * 1024;

/// Unvalidated deterministic limits for one basic Image XObject acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageXObjectLimitConfig {
    /// Maximum positive image width.
    pub max_width: u32,
    /// Maximum positive image height.
    pub max_height: u32,
    /// Maximum checked source-pixel count.
    pub max_pixels: u64,
    /// Maximum tightly packed decoded row bytes.
    pub max_stride_bytes: u64,
    /// Maximum top-level and nested image metadata entries visited.
    pub max_metadata_entries: u64,
    /// Maximum exact-read bytes consumed by the proof-bound object job.
    pub max_object_read_bytes: u64,
    /// Maximum parser-window bytes consumed by the proof-bound object job.
    pub max_object_parse_bytes: u64,
    /// Maximum exact encoded stream payload bytes.
    pub max_encoded_bytes: u64,
    /// Maximum exact final decoded component bytes.
    pub max_decoded_bytes: u64,
    /// Maximum deterministic foundational decode fuel.
    pub max_decode_fuel: u64,
    /// Maximum conservatively accounted retained object, plan, and decoded capacity.
    pub max_retained_bytes: u64,
    /// Canonical filter and decoder limits beneath the image-specific budgets.
    pub decode_limits: DecodeLimits,
}

impl Default for ImageXObjectLimitConfig {
    fn default() -> Self {
        Self {
            max_width: 16_384,
            max_height: 16_384,
            max_pixels: 16_777_216,
            max_stride_bytes: 64 * 1024 * 1024,
            max_metadata_entries: 1_024,
            max_object_read_bytes: 64 * 1024 * 1024,
            max_object_parse_bytes: 64 * 1024 * 1024,
            max_encoded_bytes: 16 * 1024 * 1024,
            max_decoded_bytes: 64 * 1024 * 1024,
            max_decode_fuel: 512 * 1024 * 1024,
            max_retained_bytes: 160 * 1024 * 1024,
            decode_limits: DecodeLimits::default(),
        }
    }
}

/// Validated deterministic limits for one basic Image XObject acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageXObjectLimits {
    config: ImageXObjectLimitConfig,
}

impl ImageXObjectLimits {
    /// Validates every independent nonzero budget against fixed implementation ceilings.
    pub fn validate(config: ImageXObjectLimitConfig) -> Result<Self, DocumentError> {
        if config.max_width == 0
            || config.max_width > HARD_MAX_WIDTH
            || config.max_height == 0
            || config.max_height > HARD_MAX_HEIGHT
            || config.max_pixels == 0
            || config.max_pixels > HARD_MAX_PIXELS
            || config.max_stride_bytes == 0
            || config.max_stride_bytes > HARD_MAX_STRIDE_BYTES
            || config.max_metadata_entries == 0
            || config.max_metadata_entries > HARD_MAX_METADATA_ENTRIES
            || config.max_object_read_bytes == 0
            || config.max_object_read_bytes > HARD_MAX_OBJECT_BYTES
            || config.max_object_parse_bytes == 0
            || config.max_object_parse_bytes > HARD_MAX_OBJECT_BYTES
            || config.max_encoded_bytes == 0
            || config.max_encoded_bytes > HARD_MAX_ENCODED_BYTES
            || config.max_decoded_bytes == 0
            || config.max_decoded_bytes > HARD_MAX_DECODED_BYTES
            || config.max_decode_fuel == 0
            || config.max_decode_fuel > HARD_MAX_DECODE_FUEL
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_RETAINED_BYTES
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self { config })
    }

    /// Returns the positive width ceiling.
    pub const fn max_width(self) -> u32 {
        self.config.max_width
    }

    /// Returns the positive height ceiling.
    pub const fn max_height(self) -> u32 {
        self.config.max_height
    }

    /// Returns the source-pixel ceiling.
    pub const fn max_pixels(self) -> u64 {
        self.config.max_pixels
    }

    /// Returns the tightly packed decoded-row ceiling.
    pub const fn max_stride_bytes(self) -> u64 {
        self.config.max_stride_bytes
    }

    /// Returns the aggregate metadata-entry visit ceiling.
    pub const fn max_metadata_entries(self) -> u64 {
        self.config.max_metadata_entries
    }

    /// Returns the object exact-read ceiling.
    pub const fn max_object_read_bytes(self) -> u64 {
        self.config.max_object_read_bytes
    }

    /// Returns the object parser-window ceiling.
    pub const fn max_object_parse_bytes(self) -> u64 {
        self.config.max_object_parse_bytes
    }

    /// Returns the exact encoded-payload ceiling.
    pub const fn max_encoded_bytes(self) -> u64 {
        self.config.max_encoded_bytes
    }

    /// Returns the exact final decoded-byte ceiling.
    pub const fn max_decoded_bytes(self) -> u64 {
        self.config.max_decoded_bytes
    }

    /// Returns the deterministic decoder-fuel ceiling.
    pub const fn max_decode_fuel(self) -> u64 {
        self.config.max_decode_fuel
    }

    /// Returns the conservatively accounted retained-capacity ceiling.
    pub const fn max_retained_bytes(self) -> u64 {
        self.config.max_retained_bytes
    }

    /// Returns the underlying canonical filter and decoder profile.
    pub const fn decode_limits(self) -> DecodeLimits {
        self.config.decode_limits
    }
}

impl Default for ImageXObjectLimits {
    fn default() -> Self {
        Self::validate(ImageXObjectLimitConfig::default())
            .expect("built-in Image XObject limits satisfy hard ceilings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentErrorCategory;

    #[test]
    fn defaults_and_independent_minimums_are_valid() {
        let defaults = ImageXObjectLimits::default();
        assert_eq!(defaults.max_width(), 16_384);
        assert_eq!(defaults.max_decoded_bytes(), 64 * 1024 * 1024);

        let minimum = ImageXObjectLimits::validate(ImageXObjectLimitConfig {
            max_width: 1,
            max_height: 1,
            max_pixels: 1,
            max_stride_bytes: 1,
            max_metadata_entries: 1,
            max_object_read_bytes: 1,
            max_object_parse_bytes: 1,
            max_encoded_bytes: 1,
            max_decoded_bytes: 1,
            max_decode_fuel: 1,
            max_retained_bytes: 1,
            decode_limits: DecodeLimits::default(),
        })
        .expect("positive independent runtime budgets validate");
        assert_eq!(minimum.max_pixels(), 1);
        assert_eq!(minimum.max_decode_fuel(), 1);
    }

    #[test]
    fn zero_and_above_hard_ceiling_profiles_are_rejected() {
        for config in [
            ImageXObjectLimitConfig {
                max_width: 0,
                ..ImageXObjectLimitConfig::default()
            },
            ImageXObjectLimitConfig {
                max_width: HARD_MAX_WIDTH + 1,
                ..ImageXObjectLimitConfig::default()
            },
            ImageXObjectLimitConfig {
                max_metadata_entries: HARD_MAX_METADATA_ENTRIES + 1,
                ..ImageXObjectLimitConfig::default()
            },
            ImageXObjectLimitConfig {
                max_retained_bytes: HARD_MAX_RETAINED_BYTES + 1,
                ..ImageXObjectLimitConfig::default()
            },
        ] {
            let error = ImageXObjectLimits::validate(config)
                .expect_err("invalid Image XObject limits must fail");
            assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
            assert_eq!(error.category(), DocumentErrorCategory::Configuration);
        }
    }
}
