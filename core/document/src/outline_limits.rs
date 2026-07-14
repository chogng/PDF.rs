use crate::{DocumentError, DocumentErrorCode, TextStringLimitConfig, TextStringLimits};

const HARD_MAX_ITEMS: u64 = 65_536;
const HARD_MAX_DEPTH: u64 = 4_096;
const HARD_MAX_SIBLINGS_PER_LEVEL: u64 = 65_536;
const HARD_MAX_TITLE_INPUT_BYTES: u64 = 1024 * 1024;
const HARD_MAX_TITLE_UTF8_BYTES: u64 = 4 * 1024 * 1024;
const HARD_MAX_TOTAL_TITLE_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_TOTAL_TITLE_UTF8_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_TOTAL_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_BYTES: u64 = 512 * 1024 * 1024;

/// Unvalidated deterministic limits for one strict document-outline traversal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutlineLimitConfig {
    /// Maximum outline item dictionaries accepted by one traversal.
    pub max_items: u64,
    /// Maximum root-relative outline-item depth accepted by one traversal.
    pub max_depth: u64,
    /// Maximum sibling outline items accepted at any one level.
    pub max_siblings_per_level: u64,
    /// Maximum decoded PDF string bytes accepted for one outline title.
    pub max_title_input_bytes: u64,
    /// Maximum UTF-8 bytes retained for one decoded outline title.
    pub max_title_utf8_bytes: u64,
    /// Maximum cumulative decoded PDF string bytes across all outline titles.
    pub max_total_title_input_bytes: u64,
    /// Maximum cumulative logical or allocator-retained UTF-8 title bytes.
    pub max_total_title_utf8_bytes: u64,
    /// Maximum cumulative exact-read bytes across all child object jobs.
    pub max_total_object_read_bytes: u64,
    /// Maximum cumulative parser-window bytes across all child object jobs.
    pub max_total_object_parse_bytes: u64,
    /// Maximum allocator-reported capacity retained by outline-owned values.
    pub max_retained_bytes: u64,
}

impl Default for OutlineLimitConfig {
    fn default() -> Self {
        Self {
            max_items: 4_096,
            max_depth: 64,
            max_siblings_per_level: 1_024,
            max_title_input_bytes: 64 * 1024,
            max_title_utf8_bytes: 256 * 1024,
            max_total_title_input_bytes: 8 * 1024 * 1024,
            max_total_title_utf8_bytes: 32 * 1024 * 1024,
            max_total_object_read_bytes: 64 * 1024 * 1024,
            max_total_object_parse_bytes: 64 * 1024 * 1024,
            max_retained_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Validated deterministic limits for one strict document-outline traversal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutlineLimits {
    max_items: u64,
    max_depth: u64,
    max_siblings_per_level: u64,
    max_title_input_bytes: u64,
    max_title_utf8_bytes: u64,
    max_total_title_input_bytes: u64,
    max_total_title_utf8_bytes: u64,
    max_total_object_read_bytes: u64,
    max_total_object_parse_bytes: u64,
    max_retained_bytes: u64,
    title_limits: TextStringLimits,
}

impl OutlineLimits {
    /// Validates nonzero limits, cross-field relationships, and fixed hard ceilings.
    pub fn validate(config: OutlineLimitConfig) -> Result<Self, DocumentError> {
        if config.max_items == 0
            || config.max_items > HARD_MAX_ITEMS
            || config.max_depth == 0
            || config.max_depth > HARD_MAX_DEPTH
            || config.max_depth > config.max_items
            || config.max_siblings_per_level == 0
            || config.max_siblings_per_level > HARD_MAX_SIBLINGS_PER_LEVEL
            || config.max_siblings_per_level > config.max_items
            || config.max_title_input_bytes == 0
            || config.max_title_input_bytes > HARD_MAX_TITLE_INPUT_BYTES
            || config.max_title_input_bytes > config.max_total_title_input_bytes
            || config.max_title_utf8_bytes == 0
            || config.max_title_utf8_bytes > HARD_MAX_TITLE_UTF8_BYTES
            || config.max_title_utf8_bytes > config.max_total_title_utf8_bytes
            || config.max_total_title_input_bytes == 0
            || config.max_total_title_input_bytes > HARD_MAX_TOTAL_TITLE_INPUT_BYTES
            || config.max_total_title_utf8_bytes == 0
            || config.max_total_title_utf8_bytes > HARD_MAX_TOTAL_TITLE_UTF8_BYTES
            || config.max_total_object_read_bytes == 0
            || config.max_total_object_read_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
            || config.max_total_object_parse_bytes == 0
            || config.max_total_object_parse_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_RETAINED_BYTES
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }

        let title_limits = TextStringLimits::validate(TextStringLimitConfig {
            max_input_bytes: config.max_title_input_bytes,
            max_utf8_bytes: config.max_title_utf8_bytes,
        })
        .expect("validated outline title limits satisfy text-string hard ceilings");

        Ok(Self {
            max_items: config.max_items,
            max_depth: config.max_depth,
            max_siblings_per_level: config.max_siblings_per_level,
            max_title_input_bytes: config.max_title_input_bytes,
            max_title_utf8_bytes: config.max_title_utf8_bytes,
            max_total_title_input_bytes: config.max_total_title_input_bytes,
            max_total_title_utf8_bytes: config.max_total_title_utf8_bytes,
            max_total_object_read_bytes: config.max_total_object_read_bytes,
            max_total_object_parse_bytes: config.max_total_object_parse_bytes,
            max_retained_bytes: config.max_retained_bytes,
            title_limits,
        })
    }

    /// Returns the maximum outline item dictionaries accepted.
    pub const fn max_items(self) -> u64 {
        self.max_items
    }

    /// Returns the maximum root-relative outline-item depth accepted.
    pub const fn max_depth(self) -> u64 {
        self.max_depth
    }

    /// Returns the maximum sibling outline items accepted at one level.
    pub const fn max_siblings_per_level(self) -> u64 {
        self.max_siblings_per_level
    }

    /// Returns the decoded PDF string byte ceiling for one outline title.
    pub const fn max_title_input_bytes(self) -> u64 {
        self.max_title_input_bytes
    }

    /// Returns the retained UTF-8 byte ceiling for one outline title.
    pub const fn max_title_utf8_bytes(self) -> u64 {
        self.max_title_utf8_bytes
    }

    /// Returns the cumulative decoded PDF string byte ceiling for all titles.
    pub const fn max_total_title_input_bytes(self) -> u64 {
        self.max_total_title_input_bytes
    }

    /// Returns the cumulative logical or allocator-retained UTF-8 title-byte ceiling.
    pub const fn max_total_title_utf8_bytes(self) -> u64 {
        self.max_total_title_utf8_bytes
    }

    /// Returns the cumulative exact-read ceiling across child object jobs.
    pub const fn max_total_object_read_bytes(self) -> u64 {
        self.max_total_object_read_bytes
    }

    /// Returns the cumulative parser-window ceiling across child object jobs.
    pub const fn max_total_object_parse_bytes(self) -> u64 {
        self.max_total_object_parse_bytes
    }

    /// Returns the allocator-reported retained-capacity ceiling.
    pub const fn max_retained_bytes(self) -> u64 {
        self.max_retained_bytes
    }

    /// Returns validated per-title PDF text-string limits.
    pub const fn title_limits(self) -> TextStringLimits {
        self.title_limits
    }
}

impl Default for OutlineLimits {
    fn default() -> Self {
        Self::validate(OutlineLimitConfig::default())
            .expect("built-in outline limits satisfy hard ceilings and relationships")
    }
}
