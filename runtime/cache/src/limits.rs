use crate::{ReadyStoreError, ReadyStoreErrorCode};

const HARD_MAX_ENTRIES: u64 = 65_536;
const HARD_MAX_VALUE_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_RESIDENT_BYTES: u64 = 1024 * 1024 * 1024;

/// Unvalidated deterministic limits for one session Ready store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyStoreLimitConfig {
    /// Maximum logical Ready entries retained at once.
    pub max_entries: u64,
    /// Maximum value-owned footprint accepted for one Ready result.
    pub max_value_bytes: u64,
    /// Maximum metadata backing plus retained value heap owned by the store.
    pub max_resident_bytes: u64,
}

impl Default for ReadyStoreLimitConfig {
    fn default() -> Self {
        Self {
            max_entries: 128,
            max_value_bytes: 8 * 1024 * 1024,
            max_resident_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Validated Ready-store limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyStoreLimits {
    pub(crate) max_entries: usize,
    pub(crate) max_value_bytes: u64,
    pub(crate) max_resident_bytes: u64,
}

impl ReadyStoreLimits {
    /// Validates a complete session Ready-store budget profile.
    pub fn validate(config: ReadyStoreLimitConfig) -> Result<Self, ReadyStoreError> {
        let max_entries = usize::try_from(config.max_entries).ok();
        if config.max_entries == 0
            || config.max_entries > HARD_MAX_ENTRIES
            || max_entries.is_none()
            || config.max_value_bytes == 0
            || config.max_value_bytes > HARD_MAX_VALUE_BYTES
            || config.max_resident_bytes == 0
            || config.max_resident_bytes > HARD_MAX_RESIDENT_BYTES
            || config.max_value_bytes > config.max_resident_bytes
        {
            return Err(ReadyStoreError::for_code(
                ReadyStoreErrorCode::InvalidLimits,
            ));
        }
        let Some(max_entries) = max_entries else {
            return Err(ReadyStoreError::for_code(
                ReadyStoreErrorCode::InvalidLimits,
            ));
        };
        Ok(Self {
            max_entries,
            max_value_bytes: config.max_value_bytes,
            max_resident_bytes: config.max_resident_bytes,
        })
    }

    /// Returns the maximum logical entry count.
    pub fn max_entries(self) -> u64 {
        u64::try_from(self.max_entries)
            .expect("validated Ready-store entry ceilings always fit in u64")
    }

    /// Returns the maximum value-owned footprint accepted for one result.
    pub const fn max_value_bytes(self) -> u64 {
        self.max_value_bytes
    }

    /// Returns the metadata-plus-value-heap resident ceiling.
    pub const fn max_resident_bytes(self) -> u64 {
        self.max_resident_bytes
    }
}

impl Default for ReadyStoreLimits {
    fn default() -> Self {
        Self::validate(ReadyStoreLimitConfig::default())
            .expect("built-in Ready-store limits satisfy fixed ceilings")
    }
}
