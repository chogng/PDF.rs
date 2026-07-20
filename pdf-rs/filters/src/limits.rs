use crate::{DecodeError, DecodeErrorCode};

const HARD_MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_LAYER_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_TOTAL_OUTPUT_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_FINAL_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_RETAINED_CAPACITY_BYTES: u64 = 512 * 1024 * 1024;
const HARD_MAX_FUEL: u64 = 8 * 1024 * 1024 * 1024;
const HARD_MAX_CANCELLATION_INTERVAL_FUEL: u64 = 1024 * 1024;
pub(crate) const HARD_MAX_FILTERS: u16 = 32;

/// Unvalidated deterministic stream-decoding limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeLimitConfig {
    /// Maximum bytes in the exact physical encoded input slice.
    pub max_input_bytes: u64,
    /// Maximum filters in one canonical plan.
    pub max_filters: u16,
    /// Maximum bytes emitted by any single filter layer.
    pub max_layer_output_bytes: u64,
    /// Maximum cumulative bytes emitted by all filter layers.
    pub max_total_output_bytes: u64,
    /// Maximum bytes in the final decoded result.
    pub max_final_output_bytes: u64,
    /// Maximum allocator-reported capacity simultaneously retained by outputs.
    pub max_retained_capacity_bytes: u64,
    /// Maximum deterministic work units.
    pub max_fuel: u64,
    /// Most fuel units allowed between cooperative cancellation probes.
    pub cancellation_check_interval_fuel: u64,
}

impl Default for DecodeLimitConfig {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_filters: 8,
            max_layer_output_bytes: 64 * 1024 * 1024,
            max_total_output_bytes: 128 * 1024 * 1024,
            max_final_output_bytes: 64 * 1024 * 1024,
            max_retained_capacity_bytes: 96 * 1024 * 1024,
            max_fuel: 512 * 1024 * 1024,
            cancellation_check_interval_fuel: 256,
        }
    }
}

/// Validated stream-decoding limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeLimits {
    pub(crate) max_input_bytes: u64,
    pub(crate) max_filters: u16,
    pub(crate) max_layer_output_bytes: u64,
    pub(crate) max_total_output_bytes: u64,
    pub(crate) max_final_output_bytes: u64,
    pub(crate) max_retained_capacity_bytes: u64,
    pub(crate) max_fuel: u64,
    pub(crate) cancellation_check_interval_fuel: u64,
}

impl DecodeLimits {
    /// Validates a complete stream-decoding budget profile.
    pub fn validate(config: DecodeLimitConfig) -> Result<Self, DecodeError> {
        if config.max_input_bytes == 0
            || config.max_input_bytes > HARD_MAX_INPUT_BYTES
            || config.max_filters == 0
            || config.max_filters > HARD_MAX_FILTERS
            || config.max_layer_output_bytes == 0
            || config.max_layer_output_bytes > HARD_MAX_LAYER_OUTPUT_BYTES
            || config.max_total_output_bytes == 0
            || config.max_total_output_bytes > HARD_MAX_TOTAL_OUTPUT_BYTES
            || config.max_final_output_bytes == 0
            || config.max_final_output_bytes > HARD_MAX_FINAL_OUTPUT_BYTES
            || config.max_final_output_bytes > config.max_layer_output_bytes
            || config.max_final_output_bytes > config.max_total_output_bytes
            || config.max_retained_capacity_bytes == 0
            || config.max_retained_capacity_bytes > HARD_MAX_RETAINED_CAPACITY_BYTES
            || config.max_retained_capacity_bytes < config.max_final_output_bytes
            || config.max_fuel == 0
            || config.max_fuel > HARD_MAX_FUEL
            || config.cancellation_check_interval_fuel == 0
            || config.cancellation_check_interval_fuel > HARD_MAX_CANCELLATION_INTERVAL_FUEL
            || config.cancellation_check_interval_fuel > config.max_fuel
        {
            return Err(DecodeError::for_code(DecodeErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            max_input_bytes: config.max_input_bytes,
            max_filters: config.max_filters,
            max_layer_output_bytes: config.max_layer_output_bytes,
            max_total_output_bytes: config.max_total_output_bytes,
            max_final_output_bytes: config.max_final_output_bytes,
            max_retained_capacity_bytes: config.max_retained_capacity_bytes,
            max_fuel: config.max_fuel,
            cancellation_check_interval_fuel: config.cancellation_check_interval_fuel,
        })
    }

    /// Returns the maximum exact physical input bytes.
    pub const fn max_input_bytes(self) -> u64 {
        self.max_input_bytes
    }

    /// Returns the maximum canonical filter count.
    pub const fn max_filters(self) -> u16 {
        self.max_filters
    }

    /// Returns the maximum output bytes for one layer.
    pub const fn max_layer_output_bytes(self) -> u64 {
        self.max_layer_output_bytes
    }

    /// Returns the maximum cumulative output bytes for all layers.
    pub const fn max_total_output_bytes(self) -> u64 {
        self.max_total_output_bytes
    }

    /// Returns the maximum final decoded bytes.
    pub const fn max_final_output_bytes(self) -> u64 {
        self.max_final_output_bytes
    }

    /// Returns the maximum simultaneously retained output capacity.
    pub const fn max_retained_capacity_bytes(self) -> u64 {
        self.max_retained_capacity_bytes
    }

    /// Returns the deterministic fuel ceiling.
    pub const fn max_fuel(self) -> u64 {
        self.max_fuel
    }

    /// Returns the maximum fuel units between cancellation probes.
    pub const fn cancellation_check_interval_fuel(self) -> u64 {
        self.cancellation_check_interval_fuel
    }
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self::validate(DecodeLimitConfig::default())
            .expect("built-in stream-decoding limits satisfy hard ceilings")
    }
}
