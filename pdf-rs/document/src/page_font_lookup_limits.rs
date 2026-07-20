use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_LOOKUPS: u64 = 65_536;
const HARD_MAX_ENTRY_VISITS: u64 = 1_048_576;

/// Unvalidated deterministic limits for Page `/Font` lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageFontLookupLimitConfig {
    /// Maximum resource names resolved through one borrowed resolver.
    pub max_lookups: u64,
    /// Maximum outer resource and inner Font dictionary entries visited.
    pub max_entry_visits: u64,
}

impl Default for PageFontLookupLimitConfig {
    fn default() -> Self {
        Self {
            max_lookups: 256,
            max_entry_visits: 16_384,
        }
    }
}

/// Validated deterministic limits for Page `/Font` lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageFontLookupLimits {
    max_lookups: u64,
    max_entry_visits: u64,
}

impl PageFontLookupLimits {
    /// Validates each independent nonzero budget against its fixed hard ceiling.
    pub fn validate(config: PageFontLookupLimitConfig) -> Result<Self, DocumentError> {
        if config.max_lookups == 0
            || config.max_lookups > HARD_MAX_LOOKUPS
            || config.max_entry_visits == 0
            || config.max_entry_visits > HARD_MAX_ENTRY_VISITS
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_lookups: config.max_lookups,
            max_entry_visits: config.max_entry_visits,
        })
    }

    /// Returns the maximum admitted resource-name lookups.
    pub const fn max_lookups(self) -> u64 {
        self.max_lookups
    }

    /// Returns the cumulative outer and inner dictionary-entry visit ceiling.
    pub const fn max_entry_visits(self) -> u64 {
        self.max_entry_visits
    }
}

impl Default for PageFontLookupLimits {
    fn default() -> Self {
        Self::validate(PageFontLookupLimitConfig::default())
            .expect("built-in Page Font lookup limits satisfy hard ceilings")
    }
}

/// Cumulative work observed through one Page Font resolver.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageFontLookupStats {
    pub(crate) lookups: u64,
    pub(crate) entry_visits: u64,
}

impl PageFontLookupStats {
    /// Returns successfully admitted resource-name lookup attempts.
    pub const fn lookups(self) -> u64 {
        self.lookups
    }

    /// Returns outer resource and inner Font dictionary entries actually visited.
    pub const fn entry_visits(self) -> u64 {
        self.entry_visits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentErrorCategory;

    #[test]
    fn limits_validate_independently() {
        assert!(PageFontLookupLimits::default().max_lookups() > 0);
        assert!(
            PageFontLookupLimits::validate(PageFontLookupLimitConfig {
                max_lookups: 0,
                ..PageFontLookupLimitConfig::default()
            })
            .is_err()
        );
        assert!(
            PageFontLookupLimits::validate(PageFontLookupLimitConfig {
                max_entry_visits: HARD_MAX_ENTRY_VISITS + 1,
                ..PageFontLookupLimitConfig::default()
            })
            .is_err()
        );
    }

    #[test]
    fn exact_hard_ceilings_validate_and_one_more_is_rejected() {
        let hard = PageFontLookupLimits::validate(PageFontLookupLimitConfig {
            max_lookups: HARD_MAX_LOOKUPS,
            max_entry_visits: HARD_MAX_ENTRY_VISITS,
        })
        .expect("inclusive hard ceilings validate");
        assert_eq!(hard.max_lookups(), HARD_MAX_LOOKUPS);
        assert_eq!(hard.max_entry_visits(), HARD_MAX_ENTRY_VISITS);

        for config in [
            PageFontLookupLimitConfig {
                max_lookups: HARD_MAX_LOOKUPS + 1,
                ..PageFontLookupLimitConfig::default()
            },
            PageFontLookupLimitConfig {
                max_entry_visits: 0,
                ..PageFontLookupLimitConfig::default()
            },
        ] {
            let error = PageFontLookupLimits::validate(config)
                .expect_err("invalid Page Font lookup limits fail");
            assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
            assert_eq!(error.category(), DocumentErrorCategory::Configuration);
        }
    }
}
