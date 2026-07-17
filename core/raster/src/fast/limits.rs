use crate::fast::{FastRasterError, FastRasterErrorCode, FastRasterLimitKind};

const HARD_MAX_PIXELS: u64 = 1 << 32;
const HARD_MAX_COMMANDS: u64 = 1 << 24;
const HARD_MAX_BIN_ENTRIES: u64 = 1 << 30;
const HARD_MAX_BYTES: u64 = 1 << 40;
const HARD_MAX_FUEL: u64 = 1 << 48;
const HARD_MAX_CANCELLATION_INTERVAL: u64 = 1_000_000;

/// Unvalidated independent Fast CPU resource limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastRasterLimitConfig {
    /// Maximum product pixels across one requested tile set.
    pub max_pixels: u64,
    /// Maximum Scene commands considered by one binning pass.
    pub max_commands: u64,
    /// Maximum retained command references across all bins.
    pub max_bin_entries: u64,
    /// Maximum durable bin metadata plus published pixel bytes.
    pub max_retained_bytes: u64,
    /// Maximum simultaneous private surfaces, masks, stacks, and geometry.
    pub max_intermediate_bytes: u64,
    /// Maximum deterministic work units for one job.
    pub max_fuel: u64,
    /// Maximum accepted RenderConfig cancellation interval.
    pub max_cancellation_interval: u64,
}

impl Default for FastRasterLimitConfig {
    fn default() -> Self {
        Self {
            max_pixels: 67_108_864,
            max_commands: 1_048_576,
            max_bin_entries: 16_777_216,
            max_retained_bytes: 536_870_912,
            max_intermediate_bytes: 268_435_456,
            max_fuel: 4_294_967_296,
            max_cancellation_interval: 4_096,
        }
    }
}

/// Validated immutable Fast CPU resource limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastRasterLimits {
    config: FastRasterLimitConfig,
}

impl FastRasterLimits {
    /// Validates nonzero independently hard-capped resource dimensions.
    pub fn validate(config: FastRasterLimitConfig) -> Result<Self, FastRasterError> {
        let fields = [
            (
                FastRasterLimitKind::Pixels,
                config.max_pixels,
                HARD_MAX_PIXELS,
            ),
            (
                FastRasterLimitKind::Commands,
                config.max_commands,
                HARD_MAX_COMMANDS,
            ),
            (
                FastRasterLimitKind::BinEntries,
                config.max_bin_entries,
                HARD_MAX_BIN_ENTRIES,
            ),
            (
                FastRasterLimitKind::RetainedBytes,
                config.max_retained_bytes,
                HARD_MAX_BYTES,
            ),
            (
                FastRasterLimitKind::IntermediateBytes,
                config.max_intermediate_bytes,
                HARD_MAX_BYTES,
            ),
            (FastRasterLimitKind::Fuel, config.max_fuel, HARD_MAX_FUEL),
            (
                FastRasterLimitKind::CancellationInterval,
                config.max_cancellation_interval,
                HARD_MAX_CANCELLATION_INTERVAL,
            ),
        ];
        for (kind, value, hard) in fields {
            if value == 0 || value > hard {
                return Err(FastRasterError::resource(kind, hard, value));
            }
        }
        Ok(Self { config })
    }

    /// Returns the complete validated configuration.
    pub const fn config(self) -> FastRasterLimitConfig {
        self.config
    }

    /// Returns the product-pixel limit.
    pub const fn max_pixels(self) -> u64 {
        self.config.max_pixels
    }

    /// Returns the command limit.
    pub const fn max_commands(self) -> u64 {
        self.config.max_commands
    }

    /// Returns the aggregate bin-entry limit.
    pub const fn max_bin_entries(self) -> u64 {
        self.config.max_bin_entries
    }

    /// Returns the durable retention limit.
    pub const fn max_retained_bytes(self) -> u64 {
        self.config.max_retained_bytes
    }

    /// Returns the private simultaneous-working limit.
    pub const fn max_intermediate_bytes(self) -> u64 {
        self.config.max_intermediate_bytes
    }

    /// Returns the deterministic fuel limit.
    pub const fn max_fuel(self) -> u64 {
        self.config.max_fuel
    }

    /// Returns the maximum accepted cancellation interval.
    pub const fn max_cancellation_interval(self) -> u64 {
        self.config.max_cancellation_interval
    }
}

impl Default for FastRasterLimits {
    fn default() -> Self {
        Self::validate(FastRasterLimitConfig::default())
            .expect("built-in Fast CPU limits satisfy hard ceilings")
    }
}

pub(crate) fn checked_total(
    kind: FastRasterLimitKind,
    current: u64,
    additional: u64,
    limit: u64,
) -> Result<u64, FastRasterError> {
    let total = current
        .checked_add(additional)
        .ok_or_else(|| FastRasterError::for_code(FastRasterErrorCode::NumericOverflow))?;
    if total > limit {
        return Err(FastRasterError::resource(kind, limit, total));
    }
    Ok(total)
}
