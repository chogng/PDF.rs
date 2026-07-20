use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_TOTAL_ENTRIES: u64 = 4_000_000;
const HARD_MAX_IN_USE_ENTRIES: u64 = 4_000_000;
const HARD_MAX_LOGICAL_INDEX_BYTES: u64 = 512 * 1024 * 1024;
const HARD_MAX_SORT_STEPS: u64 = 1_000_000_000;

/// Unvalidated deterministic candidate-revision index limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentLimitConfig {
    /// Maximum xref rows accepted from one candidate revision.
    pub max_total_entries: u64,
    /// Maximum in-use xref rows accepted from one candidate revision.
    pub max_in_use_entries: u64,
    /// Maximum conservatively accounted allocator capacity for logical and physical entries.
    pub max_logical_index_bytes: u64,
    /// Maximum comparisons and swaps performed by the cancellable physical-offset sort.
    pub max_sort_steps: u64,
}

impl Default for DocumentLimitConfig {
    fn default() -> Self {
        Self {
            max_total_entries: 50_000,
            max_in_use_entries: 50_000,
            max_logical_index_bytes: 8 * 1024 * 1024,
            max_sort_steps: 8_000_000,
        }
    }
}

/// Validated candidate-revision index limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentLimits {
    pub(crate) max_total_entries: u64,
    pub(crate) max_in_use_entries: u64,
    pub(crate) max_logical_index_bytes: u64,
    pub(crate) max_sort_steps: u64,
}

impl DocumentLimits {
    /// Validates a complete candidate-revision index budget profile.
    pub fn validate(config: DocumentLimitConfig) -> Result<Self, DocumentError> {
        if config.max_total_entries == 0
            || config.max_total_entries > HARD_MAX_TOTAL_ENTRIES
            || config.max_in_use_entries == 0
            || config.max_in_use_entries > HARD_MAX_IN_USE_ENTRIES
            || config.max_in_use_entries > config.max_total_entries
            || config.max_logical_index_bytes == 0
            || config.max_logical_index_bytes > HARD_MAX_LOGICAL_INDEX_BYTES
            || config.max_sort_steps == 0
            || config.max_sort_steps > HARD_MAX_SORT_STEPS
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_total_entries: config.max_total_entries,
            max_in_use_entries: config.max_in_use_entries,
            max_logical_index_bytes: config.max_logical_index_bytes,
            max_sort_steps: config.max_sort_steps,
        })
    }

    /// Returns the maximum accepted xref-row count.
    pub const fn max_total_entries(self) -> u64 {
        self.max_total_entries
    }

    /// Returns the maximum accepted in-use xref-row count.
    pub const fn max_in_use_entries(self) -> u64 {
        self.max_in_use_entries
    }

    /// Returns the maximum conservatively accounted entry-capacity bytes.
    pub const fn max_logical_index_bytes(self) -> u64 {
        self.max_logical_index_bytes
    }

    /// Returns the maximum physical-offset sorting work.
    pub const fn max_sort_steps(self) -> u64 {
        self.max_sort_steps
    }
}

impl Default for DocumentLimits {
    fn default() -> Self {
        Self::validate(DocumentLimitConfig::default())
            .expect("built-in document limits satisfy hard ceilings")
    }
}
