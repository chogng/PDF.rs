use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_LOOKUPS: u64 = 65_536;
const HARD_MAX_ENTRY_VISITS: u64 = 1_048_576;

/// Unvalidated deterministic limits for Page `/XObject` lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageXObjectLookupLimitConfig {
    /// Maximum resource names resolved through one borrowed resolver.
    pub max_lookups: u64,
    /// Maximum outer resource and inner XObject dictionary entries visited.
    pub max_entry_visits: u64,
}

impl Default for PageXObjectLookupLimitConfig {
    fn default() -> Self {
        Self {
            max_lookups: 256,
            max_entry_visits: 16_384,
        }
    }
}

/// Validated deterministic limits for Page `/XObject` lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageXObjectLookupLimits {
    max_lookups: u64,
    max_entry_visits: u64,
}

impl PageXObjectLookupLimits {
    /// Validates each independent nonzero budget against its fixed hard ceiling.
    pub fn validate(config: PageXObjectLookupLimitConfig) -> Result<Self, DocumentError> {
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

impl Default for PageXObjectLookupLimits {
    fn default() -> Self {
        Self::validate(PageXObjectLookupLimitConfig::default())
            .expect("built-in Page XObject lookup limits satisfy hard ceilings")
    }
}

/// Cumulative work observed through one Page XObject resolver.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageXObjectLookupStats {
    pub(crate) lookups: u64,
    pub(crate) entry_visits: u64,
}

impl PageXObjectLookupStats {
    /// Returns successfully admitted resource-name lookup attempts.
    pub const fn lookups(self) -> u64 {
        self.lookups
    }

    /// Returns outer resource and inner XObject dictionary entries actually visited.
    pub const fn entry_visits(self) -> u64 {
        self.entry_visits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentErrorCategory;

    #[test]
    fn defaults_and_independent_minimums_are_valid() {
        let defaults = PageXObjectLookupLimits::default();
        assert_eq!(defaults.max_lookups(), 256);
        assert_eq!(defaults.max_entry_visits(), 16_384);

        let minimum = PageXObjectLookupLimits::validate(PageXObjectLookupLimitConfig {
            max_lookups: 1,
            max_entry_visits: 1,
        })
        .expect("positive independent lookup budgets validate");
        assert_eq!(minimum.max_lookups(), 1);
        assert_eq!(minimum.max_entry_visits(), 1);
    }

    #[test]
    fn zero_and_above_hard_ceiling_profiles_are_rejected() {
        for config in [
            PageXObjectLookupLimitConfig {
                max_lookups: 0,
                ..PageXObjectLookupLimitConfig::default()
            },
            PageXObjectLookupLimitConfig {
                max_lookups: HARD_MAX_LOOKUPS + 1,
                ..PageXObjectLookupLimitConfig::default()
            },
            PageXObjectLookupLimitConfig {
                max_entry_visits: 0,
                ..PageXObjectLookupLimitConfig::default()
            },
            PageXObjectLookupLimitConfig {
                max_entry_visits: HARD_MAX_ENTRY_VISITS + 1,
                ..PageXObjectLookupLimitConfig::default()
            },
        ] {
            let error = PageXObjectLookupLimits::validate(config)
                .expect_err("invalid XObject lookup limits must fail");
            assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
            assert_eq!(error.category(), DocumentErrorCategory::Configuration);
        }
    }
}
