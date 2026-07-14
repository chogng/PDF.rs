use crate::{XrefError, XrefErrorCode};

const HARD_MAX_SOURCE_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_TAIL_BYTES: u64 = 4 * 1024 * 1024;
const HARD_MAX_SECTION_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_SUBSECTIONS: u64 = 65_536;
const HARD_MAX_ENTRIES: u64 = 4_000_000;

/// Unvalidated deterministic traditional-xref limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefLimitConfig {
    /// Maximum immutable source length accepted by this bootstrap.
    pub max_source_bytes: u64,
    /// First tail range requested from the source end.
    pub initial_tail_bytes: u64,
    /// Maximum tail range searched for the final `startxref`.
    pub max_tail_bytes: u64,
    /// First range requested at the declared xref offset.
    pub initial_section_bytes: u64,
    /// Maximum contiguous traditional xref section window.
    pub max_section_bytes: u64,
    /// Maximum cumulative exact requested bytes across window growth.
    pub max_total_read_bytes: u64,
    /// Maximum cumulative complete windows scanned across retries.
    pub max_total_parse_bytes: u64,
    /// Maximum traditional xref subsections.
    pub max_subsections: u64,
    /// Maximum parsed entries and trailer `/Size`.
    pub max_entries: u64,
}

impl Default for XrefLimitConfig {
    fn default() -> Self {
        Self {
            max_source_bytes: 256 * 1024 * 1024,
            initial_tail_bytes: 1024,
            max_tail_bytes: 64 * 1024,
            initial_section_bytes: 4 * 1024,
            max_section_bytes: 1024 * 1024,
            max_total_read_bytes: 4 * 1024 * 1024,
            max_total_parse_bytes: 4 * 1024 * 1024,
            max_subsections: 4096,
            max_entries: 50_000,
        }
    }
}

/// Validated xref limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefLimits {
    pub(crate) max_source_bytes: u64,
    pub(crate) initial_tail_bytes: u64,
    pub(crate) max_tail_bytes: u64,
    pub(crate) initial_section_bytes: u64,
    pub(crate) max_section_bytes: u64,
    pub(crate) max_total_read_bytes: u64,
    pub(crate) max_total_parse_bytes: u64,
    pub(crate) max_subsections: u64,
    pub(crate) max_entries: u64,
}

impl XrefLimits {
    /// Validates a complete traditional-xref budget profile.
    pub fn validate(config: XrefLimitConfig) -> Result<Self, XrefError> {
        let minimum_total = config.max_tail_bytes.checked_add(config.max_section_bytes);
        if config.max_source_bytes == 0
            || config.max_source_bytes > HARD_MAX_SOURCE_BYTES
            || config.initial_tail_bytes == 0
            || config.initial_tail_bytes > config.max_tail_bytes
            || config.max_tail_bytes > HARD_MAX_TAIL_BYTES
            || config.max_tail_bytes > config.max_source_bytes
            || config.initial_section_bytes == 0
            || config.initial_section_bytes > config.max_section_bytes
            || config.max_section_bytes > HARD_MAX_SECTION_BYTES
            || config.max_section_bytes > config.max_source_bytes
            || config.max_total_read_bytes == 0
            || config.max_total_read_bytes > HARD_MAX_TOTAL_BYTES
            || minimum_total.is_none_or(|minimum| config.max_total_read_bytes < minimum)
            || config.max_total_parse_bytes == 0
            || config.max_total_parse_bytes > HARD_MAX_TOTAL_BYTES
            || minimum_total.is_none_or(|minimum| config.max_total_parse_bytes < minimum)
            || config.max_subsections == 0
            || config.max_subsections > HARD_MAX_SUBSECTIONS
            || config.max_entries == 0
            || config.max_entries > HARD_MAX_ENTRIES
        {
            return Err(XrefError::for_code(XrefErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            max_source_bytes: config.max_source_bytes,
            initial_tail_bytes: config.initial_tail_bytes,
            max_tail_bytes: config.max_tail_bytes,
            initial_section_bytes: config.initial_section_bytes,
            max_section_bytes: config.max_section_bytes,
            max_total_read_bytes: config.max_total_read_bytes,
            max_total_parse_bytes: config.max_total_parse_bytes,
            max_subsections: config.max_subsections,
            max_entries: config.max_entries,
        })
    }

    /// Returns the maximum accepted source length.
    pub const fn max_source_bytes(self) -> u64 {
        self.max_source_bytes
    }

    /// Returns the first tail request size.
    pub const fn initial_tail_bytes(self) -> u64 {
        self.initial_tail_bytes
    }

    /// Returns the maximum tail search size.
    pub const fn max_tail_bytes(self) -> u64 {
        self.max_tail_bytes
    }

    /// Returns the first xref-section request size.
    pub const fn initial_section_bytes(self) -> u64 {
        self.initial_section_bytes
    }

    /// Returns the maximum xref-section window size.
    pub const fn max_section_bytes(self) -> u64 {
        self.max_section_bytes
    }

    /// Returns the cumulative exact-read ceiling.
    pub const fn max_total_read_bytes(self) -> u64 {
        self.max_total_read_bytes
    }

    /// Returns the cumulative parse-window ceiling.
    pub const fn max_total_parse_bytes(self) -> u64 {
        self.max_total_parse_bytes
    }

    /// Returns the subsection ceiling.
    pub const fn max_subsections(self) -> u64 {
        self.max_subsections
    }

    /// Returns the entry and trailer-size ceiling.
    pub const fn max_entries(self) -> u64 {
        self.max_entries
    }
}

impl Default for XrefLimits {
    fn default() -> Self {
        Self::validate(XrefLimitConfig::default())
            .expect("built-in xref limits satisfy hard ceilings")
    }
}
