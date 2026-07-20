use pdf_rs_filters::DecodeLimits;
use pdf_rs_font::FontLimits;

use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_POLLS: u64 = 16_777_216;
const HARD_MAX_OBJECTS: u64 = 16;
const HARD_MAX_REFERENCE_EDGES: u64 = 16;
const HARD_MAX_METADATA_ENTRIES: u64 = 65_536;
const HARD_MAX_WIDTHS: u64 = 256;
const HARD_MAX_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_PROGRAM_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_DECODE_FUEL: u64 = 8 * 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Unvalidated deterministic limits for one embedded simple TrueType acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontResourceLimitConfig {
    /// Maximum polls, including calls that reach a terminal result.
    pub max_polls: u64,
    /// Maximum proof-preserving indirect objects opened.
    pub max_objects: u64,
    /// Maximum Font-to-descendant-container, descendant, encoding, descriptor, and program edges.
    pub max_reference_edges: u64,
    /// Maximum top-level Font, Encoding, descriptor, and embedded-program metadata entries visited.
    pub max_metadata_entries: u64,
    /// Maximum entries in the direct PDF `/Widths` array.
    pub max_widths: u64,
    /// Maximum cumulative exact-read bytes across proof-bound object jobs.
    pub max_object_read_bytes: u64,
    /// Maximum cumulative parser-window bytes across proof-bound object jobs.
    pub max_object_parse_bytes: u64,
    /// Maximum exact encoded FontFile2 payload bytes.
    pub max_encoded_bytes: u64,
    /// Maximum exact decoded TrueType program bytes.
    pub max_decoded_bytes: u64,
    /// Maximum deterministic foundational stream-decode fuel.
    pub max_decode_fuel: u64,
    /// Maximum conservatively accounted retained acquisition state.
    pub max_retained_bytes: u64,
    /// Canonical stream decoder limits beneath the font-resource budgets.
    pub decode_limits: DecodeLimits,
    /// Pure TrueType parser limits beneath the document budgets.
    pub font_limits: FontLimits,
}

impl Default for FontResourceLimitConfig {
    fn default() -> Self {
        Self {
            max_polls: 1_048_576,
            max_objects: 5,
            max_reference_edges: 4,
            max_metadata_entries: 2_048,
            max_widths: 256,
            max_object_read_bytes: 96 * 1024 * 1024,
            max_object_parse_bytes: 96 * 1024 * 1024,
            max_encoded_bytes: 16 * 1024 * 1024,
            max_decoded_bytes: 16 * 1024 * 1024,
            max_decode_fuel: 512 * 1024 * 1024,
            max_retained_bytes: 384 * 1024 * 1024,
            decode_limits: DecodeLimits::default(),
            font_limits: FontLimits::default(),
        }
    }
}

/// Validated deterministic limits for one embedded simple TrueType acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontResourceLimits {
    config: FontResourceLimitConfig,
}

impl FontResourceLimits {
    /// Validates every independent nonzero budget against fixed implementation ceilings.
    pub fn validate(config: FontResourceLimitConfig) -> Result<Self, DocumentError> {
        if config.max_polls == 0
            || config.max_polls > HARD_MAX_POLLS
            || config.max_objects < 2
            || config.max_objects > HARD_MAX_OBJECTS
            || config.max_reference_edges == 0
            || config.max_reference_edges > HARD_MAX_REFERENCE_EDGES
            || config.max_metadata_entries == 0
            || config.max_metadata_entries > HARD_MAX_METADATA_ENTRIES
            || config.max_widths < 95
            || config.max_widths > HARD_MAX_WIDTHS
            || config.max_object_read_bytes == 0
            || config.max_object_read_bytes > HARD_MAX_OBJECT_BYTES
            || config.max_object_parse_bytes == 0
            || config.max_object_parse_bytes > HARD_MAX_OBJECT_BYTES
            || config.max_encoded_bytes == 0
            || config.max_encoded_bytes > HARD_MAX_PROGRAM_BYTES
            || config.max_decoded_bytes == 0
            || config.max_decoded_bytes > HARD_MAX_PROGRAM_BYTES
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

    /// Returns the poll ceiling.
    pub const fn max_polls(self) -> u64 {
        self.config.max_polls
    }
    /// Returns the proof-preserving object ceiling.
    pub const fn max_objects(self) -> u64 {
        self.config.max_objects
    }
    /// Returns the indirect reference-edge ceiling.
    pub const fn max_reference_edges(self) -> u64 {
        self.config.max_reference_edges
    }
    /// Returns the metadata-entry visit ceiling.
    pub const fn max_metadata_entries(self) -> u64 {
        self.config.max_metadata_entries
    }
    /// Returns the direct Widths entry ceiling.
    pub const fn max_widths(self) -> u64 {
        self.config.max_widths
    }
    /// Returns the cumulative object exact-read ceiling.
    pub const fn max_object_read_bytes(self) -> u64 {
        self.config.max_object_read_bytes
    }
    /// Returns the cumulative object parser-window ceiling.
    pub const fn max_object_parse_bytes(self) -> u64 {
        self.config.max_object_parse_bytes
    }
    /// Returns the exact encoded program ceiling.
    pub const fn max_encoded_bytes(self) -> u64 {
        self.config.max_encoded_bytes
    }
    /// Returns the exact decoded program ceiling.
    pub const fn max_decoded_bytes(self) -> u64 {
        self.config.max_decoded_bytes
    }
    /// Returns the stream decoder fuel ceiling.
    pub const fn max_decode_fuel(self) -> u64 {
        self.config.max_decode_fuel
    }
    /// Returns the aggregate retained-state ceiling.
    pub const fn max_retained_bytes(self) -> u64 {
        self.config.max_retained_bytes
    }
    /// Returns the lower canonical stream decoder limits.
    pub const fn decode_limits(self) -> DecodeLimits {
        self.config.decode_limits
    }
    /// Returns the lower pure TrueType parser limits.
    pub const fn font_limits(self) -> FontLimits {
        self.config.font_limits
    }
}

impl Default for FontResourceLimits {
    fn default() -> Self {
        Self::validate(FontResourceLimitConfig::default())
            .expect("built-in font-resource limits satisfy hard ceilings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentErrorCategory;

    #[test]
    fn exact_registered_minimums_validate() {
        let limits = FontResourceLimits::validate(FontResourceLimitConfig {
            max_polls: 1,
            max_objects: 2,
            max_reference_edges: 1,
            max_metadata_entries: 1,
            max_widths: 95,
            max_object_read_bytes: 1,
            max_object_parse_bytes: 1,
            max_encoded_bytes: 1,
            max_decoded_bytes: 1,
            max_decode_fuel: 1,
            max_retained_bytes: 1,
            decode_limits: DecodeLimits::default(),
            font_limits: FontLimits::default(),
        })
        .expect("independent positive limits validate");
        assert_eq!(limits.max_objects(), 2);
        assert_eq!(limits.max_widths(), 95);
    }

    #[test]
    fn below_registered_object_shape_is_rejected() {
        let error = FontResourceLimits::validate(FontResourceLimitConfig {
            max_reference_edges: 0,
            ..FontResourceLimitConfig::default()
        })
        .expect_err("at least one proof edge is mandatory");
        assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
    }

    #[test]
    fn zero_and_above_hard_ceiling_profiles_are_rejected() {
        for config in [
            FontResourceLimitConfig {
                max_polls: 0,
                ..FontResourceLimitConfig::default()
            },
            FontResourceLimitConfig {
                max_objects: HARD_MAX_OBJECTS + 1,
                ..FontResourceLimitConfig::default()
            },
            FontResourceLimitConfig {
                max_widths: HARD_MAX_WIDTHS + 1,
                ..FontResourceLimitConfig::default()
            },
            FontResourceLimitConfig {
                max_retained_bytes: HARD_MAX_RETAINED_BYTES + 1,
                ..FontResourceLimitConfig::default()
            },
        ] {
            let error = FontResourceLimits::validate(config)
                .expect_err("invalid Font resource limits must fail");
            assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
            assert_eq!(error.category(), DocumentErrorCategory::Configuration);
        }
    }
}
