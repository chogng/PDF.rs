use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_LOOKUPS: u64 = 65_536;
const HARD_MAX_ENTRY_VISITS: u64 = 1_048_576;

/// Unvalidated deterministic limits for marked-content property lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagePropertyLookupLimitConfig {
    /// Maximum `/Properties` names resolved through one borrowed resolver.
    pub max_lookups: u64,
    /// Maximum outer resource and inner property dictionary entries visited.
    pub max_entry_visits: u64,
}

impl Default for PagePropertyLookupLimitConfig {
    fn default() -> Self {
        Self {
            max_lookups: 256,
            max_entry_visits: 16_384,
        }
    }
}

/// Validated deterministic limits for marked-content property lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagePropertyLookupLimits {
    max_lookups: u64,
    max_entry_visits: u64,
}

impl PagePropertyLookupLimits {
    /// Validates each independent nonzero budget against its fixed hard ceiling.
    pub fn validate(config: PagePropertyLookupLimitConfig) -> Result<Self, DocumentError> {
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

    /// Returns the maximum property-name lookups.
    pub const fn max_lookups(self) -> u64 {
        self.max_lookups
    }

    /// Returns the cumulative outer and inner dictionary-entry visit ceiling.
    pub const fn max_entry_visits(self) -> u64 {
        self.max_entry_visits
    }
}

impl Default for PagePropertyLookupLimits {
    fn default() -> Self {
        Self::validate(PagePropertyLookupLimitConfig::default())
            .expect("built-in page property lookup limits satisfy hard ceilings")
    }
}

/// Cumulative work observed through one marked-content property resolver.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PagePropertyLookupStats {
    pub(crate) lookups: u64,
    pub(crate) entry_visits: u64,
}

impl PagePropertyLookupStats {
    /// Returns successfully admitted property-name lookup attempts.
    pub const fn lookups(self) -> u64 {
        self.lookups
    }

    /// Returns outer resource and inner property dictionary entries actually visited.
    pub const fn entry_visits(self) -> u64 {
        self.entry_visits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentErrorCategory;

    #[test]
    fn defaults_are_valid() {
        let limits = PagePropertyLookupLimits::default();
        assert_eq!(limits.max_lookups(), 256);
        assert_eq!(limits.max_entry_visits(), 16_384);
    }

    #[test]
    fn zero_and_above_hard_ceiling_profiles_are_rejected() {
        for config in [
            PagePropertyLookupLimitConfig {
                max_lookups: 0,
                ..PagePropertyLookupLimitConfig::default()
            },
            PagePropertyLookupLimitConfig {
                max_lookups: HARD_MAX_LOOKUPS + 1,
                ..PagePropertyLookupLimitConfig::default()
            },
            PagePropertyLookupLimitConfig {
                max_entry_visits: 0,
                ..PagePropertyLookupLimitConfig::default()
            },
            PagePropertyLookupLimitConfig {
                max_entry_visits: HARD_MAX_ENTRY_VISITS + 1,
                ..PagePropertyLookupLimitConfig::default()
            },
        ] {
            let error = PagePropertyLookupLimits::validate(config)
                .expect_err("invalid property lookup limits must fail");
            assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
            assert_eq!(error.category(), DocumentErrorCategory::Configuration);
        }
    }

    #[test]
    fn independent_minimum_runtime_budgets_are_valid() {
        let limits = PagePropertyLookupLimits::validate(PagePropertyLookupLimitConfig {
            max_lookups: 1,
            max_entry_visits: 1,
        })
        .expect("positive independent budgets validate");
        assert_eq!(limits.max_lookups(), 1);
        assert_eq!(limits.max_entry_visits(), 1);
    }
}
