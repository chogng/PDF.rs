use pdf_rs_filters::DecodeLimits;

use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_STREAMS: u64 = 65_536;
const HARD_MAX_ARRAY_ENTRIES: u64 = 65_536;
const HARD_MAX_OBJECTS: u64 = 131_072;
const HARD_MAX_REFERENCE_EDGES: u64 = 131_072;
const HARD_MAX_ALIAS_DEPTH: u64 = 256;
const HARD_MAX_TOTAL_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_TOTAL_ENCODED_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_TOTAL_DECODED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const HARD_MAX_TOTAL_DECODE_FUEL: u64 = 32 * 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_STATE_BYTES: u64 = 1024 * 1024 * 1024;

/// Unvalidated deterministic limits for acquiring and decoding one Page's content streams.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageContentLimitConfig {
    /// Maximum content streams published for one Page.
    pub max_streams: u64,
    /// Maximum entries in one direct or whole-object-aliased Contents array.
    pub max_array_entries: u64,
    /// Maximum proof-preserving object jobs across the Page, aliases, and streams.
    pub max_objects: u64,
    /// Maximum Contents, alias, and array-entry reference edges followed.
    pub max_reference_edges: u64,
    /// Maximum exact object identities retained in the whole-object Contents alias chain.
    pub max_alias_depth: u64,
    /// Maximum cumulative exact-read bytes across child object jobs.
    pub max_total_object_read_bytes: u64,
    /// Maximum cumulative parser-window bytes across child object jobs.
    pub max_total_object_parse_bytes: u64,
    /// Maximum cumulative exact encoded payload bytes.
    pub max_total_encoded_bytes: u64,
    /// Maximum cumulative final decoded bytes.
    pub max_total_decoded_bytes: u64,
    /// Maximum cumulative deterministic decode fuel.
    pub max_total_decode_fuel: u64,
    /// Maximum allocator-reported acquisition-owned state and published proof capacity.
    pub max_retained_state_bytes: u64,
    /// Per-stream canonical filter and decoder limits.
    pub decode_limits: DecodeLimits,
}

impl Default for PageContentLimitConfig {
    fn default() -> Self {
        Self {
            max_streams: 256,
            max_array_entries: 256,
            max_objects: 512,
            max_reference_edges: 512,
            max_alias_depth: 64,
            max_total_object_read_bytes: 128 * 1024 * 1024,
            max_total_object_parse_bytes: 128 * 1024 * 1024,
            max_total_encoded_bytes: 128 * 1024 * 1024,
            max_total_decoded_bytes: 256 * 1024 * 1024,
            max_total_decode_fuel: 2 * 1024 * 1024 * 1024,
            max_retained_state_bytes: 384 * 1024 * 1024,
            decode_limits: DecodeLimits::default(),
        }
    }
}

/// Validated deterministic limits for acquiring and decoding one Page's content streams.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageContentLimits {
    max_streams: u64,
    max_array_entries: u64,
    max_objects: u64,
    max_reference_edges: u64,
    max_alias_depth: u64,
    max_total_object_read_bytes: u64,
    max_total_object_parse_bytes: u64,
    max_total_encoded_bytes: u64,
    max_total_decoded_bytes: u64,
    max_total_decode_fuel: u64,
    max_retained_state_bytes: u64,
    decode_limits: DecodeLimits,
}

impl PageContentLimits {
    /// Validates every independent nonzero budget against fixed implementation ceilings.
    pub fn validate(config: PageContentLimitConfig) -> Result<Self, DocumentError> {
        if config.max_streams == 0
            || config.max_streams > HARD_MAX_STREAMS
            || config.max_array_entries == 0
            || config.max_array_entries > HARD_MAX_ARRAY_ENTRIES
            || config.max_objects == 0
            || config.max_objects > HARD_MAX_OBJECTS
            || config.max_reference_edges == 0
            || config.max_reference_edges > HARD_MAX_REFERENCE_EDGES
            || config.max_alias_depth == 0
            || config.max_alias_depth > HARD_MAX_ALIAS_DEPTH
            || config.max_total_object_read_bytes == 0
            || config.max_total_object_read_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
            || config.max_total_object_parse_bytes == 0
            || config.max_total_object_parse_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
            || config.max_total_encoded_bytes == 0
            || config.max_total_encoded_bytes > HARD_MAX_TOTAL_ENCODED_BYTES
            || config.max_total_decoded_bytes == 0
            || config.max_total_decoded_bytes > HARD_MAX_TOTAL_DECODED_BYTES
            || config.max_total_decode_fuel == 0
            || config.max_total_decode_fuel > HARD_MAX_TOTAL_DECODE_FUEL
            || config.max_retained_state_bytes == 0
            || config.max_retained_state_bytes > HARD_MAX_RETAINED_STATE_BYTES
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }

        Ok(Self {
            max_streams: config.max_streams,
            max_array_entries: config.max_array_entries,
            max_objects: config.max_objects,
            max_reference_edges: config.max_reference_edges,
            max_alias_depth: config.max_alias_depth,
            max_total_object_read_bytes: config.max_total_object_read_bytes,
            max_total_object_parse_bytes: config.max_total_object_parse_bytes,
            max_total_encoded_bytes: config.max_total_encoded_bytes,
            max_total_decoded_bytes: config.max_total_decoded_bytes,
            max_total_decode_fuel: config.max_total_decode_fuel,
            max_retained_state_bytes: config.max_retained_state_bytes,
            decode_limits: config.decode_limits,
        })
    }

    /// Returns the maximum number of published content streams.
    pub const fn max_streams(self) -> u64 {
        self.max_streams
    }

    /// Returns the maximum number of entries in a Contents array.
    pub const fn max_array_entries(self) -> u64 {
        self.max_array_entries
    }

    /// Returns the maximum number of child object jobs.
    pub const fn max_objects(self) -> u64 {
        self.max_objects
    }

    /// Returns the maximum number of followed reference edges.
    pub const fn max_reference_edges(self) -> u64 {
        self.max_reference_edges
    }

    /// Returns the maximum retained whole-object Contents alias depth.
    pub const fn max_alias_depth(self) -> u64 {
        self.max_alias_depth
    }

    /// Returns the cumulative exact-read ceiling across child object jobs.
    pub const fn max_total_object_read_bytes(self) -> u64 {
        self.max_total_object_read_bytes
    }

    /// Returns the cumulative parser-window ceiling across child object jobs.
    pub const fn max_total_object_parse_bytes(self) -> u64 {
        self.max_total_object_parse_bytes
    }

    /// Returns the cumulative exact encoded-payload ceiling.
    pub const fn max_total_encoded_bytes(self) -> u64 {
        self.max_total_encoded_bytes
    }

    /// Returns the cumulative final decoded-byte ceiling.
    pub const fn max_total_decoded_bytes(self) -> u64 {
        self.max_total_decoded_bytes
    }

    /// Returns the cumulative deterministic decode-fuel ceiling.
    pub const fn max_total_decode_fuel(self) -> u64 {
        self.max_total_decode_fuel
    }

    /// Returns the acquisition-owned retained-state ceiling.
    pub const fn max_retained_state_bytes(self) -> u64 {
        self.max_retained_state_bytes
    }

    /// Returns the per-stream canonical filter and decoder limits.
    pub const fn decode_limits(self) -> DecodeLimits {
        self.decode_limits
    }
}

impl Default for PageContentLimits {
    fn default() -> Self {
        Self::validate(PageContentLimitConfig::default())
            .expect("built-in page-content limits satisfy hard ceilings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentErrorCategory;

    #[test]
    fn defaults_are_valid_and_independent_small_runtime_budgets_are_allowed() {
        let defaults = PageContentLimits::default();
        assert_eq!(defaults.max_streams(), 256);
        assert_eq!(defaults.max_array_entries(), 256);
        assert_eq!(defaults.max_alias_depth(), 64);

        let limits = PageContentLimits::validate(PageContentLimitConfig {
            max_streams: 1,
            max_array_entries: 1,
            max_objects: 1,
            max_reference_edges: 1,
            max_alias_depth: 1,
            max_total_object_read_bytes: 1,
            max_total_object_parse_bytes: 1,
            max_total_encoded_bytes: 1,
            max_total_decoded_bytes: 1,
            max_total_decode_fuel: 1,
            max_retained_state_bytes: 1,
            decode_limits: DecodeLimits::default(),
        })
        .expect("independent one-less profiles remain valid runtime configurations");
        assert_eq!(limits.max_total_encoded_bytes(), 1);
        assert_eq!(limits.max_total_decode_fuel(), 1);
    }

    #[test]
    fn zero_and_above_hard_ceiling_profiles_are_rejected() {
        for config in [
            PageContentLimitConfig {
                max_streams: 0,
                ..PageContentLimitConfig::default()
            },
            PageContentLimitConfig {
                max_streams: HARD_MAX_STREAMS + 1,
                ..PageContentLimitConfig::default()
            },
            PageContentLimitConfig {
                max_alias_depth: HARD_MAX_ALIAS_DEPTH + 1,
                ..PageContentLimitConfig::default()
            },
            PageContentLimitConfig {
                max_retained_state_bytes: HARD_MAX_RETAINED_STATE_BYTES + 1,
                ..PageContentLimitConfig::default()
            },
        ] {
            let error =
                PageContentLimits::validate(config).expect_err("invalid page-content limits fail");
            assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
            assert_eq!(error.category(), DocumentErrorCategory::Configuration);
        }
    }
}
