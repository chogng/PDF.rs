use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_ANCESTOR_DEPTH: u64 = 1_024;
const HARD_MAX_OBJECTS: u64 = 4_096;
const HARD_MAX_REFERENCE_EDGES: u64 = 4_096;
const HARD_MAX_TOTAL_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_STATE_BYTES: u64 = 512 * 1024 * 1024;

/// Unvalidated deterministic limits for materializing inherited values of one Page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageMaterializationLimitConfig {
    /// Maximum Page/Pages dictionaries in the leaf-to-root inheritance chain.
    pub max_ancestor_depth: u64,
    /// Maximum proof-preserving object jobs started across ancestors and aliases.
    pub max_objects: u64,
    /// Maximum whole-object direct-reference alias edges followed across all inherited fields.
    pub max_reference_edges: u64,
    /// Maximum cumulative exact-read bytes across all child object jobs.
    pub max_total_object_read_bytes: u64,
    /// Maximum cumulative parser-window bytes across all child object jobs.
    pub max_total_object_parse_bytes: u64,
    /// Maximum allocator-reported capacity retained by materialization-owned state.
    pub max_retained_state_bytes: u64,
}

impl Default for PageMaterializationLimitConfig {
    fn default() -> Self {
        Self {
            max_ancestor_depth: 64,
            max_objects: 256,
            max_reference_edges: 64,
            max_total_object_read_bytes: 64 * 1024 * 1024,
            max_total_object_parse_bytes: 64 * 1024 * 1024,
            max_retained_state_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Validated deterministic limits for materializing inherited values of one Page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageMaterializationLimits {
    max_ancestor_depth: u64,
    max_objects: u64,
    max_reference_edges: u64,
    max_total_object_read_bytes: u64,
    max_total_object_parse_bytes: u64,
    max_retained_state_bytes: u64,
}

impl PageMaterializationLimits {
    /// Validates each independent nonzero budget against its fixed hard ceiling.
    pub fn validate(config: PageMaterializationLimitConfig) -> Result<Self, DocumentError> {
        if config.max_ancestor_depth == 0
            || config.max_ancestor_depth > HARD_MAX_ANCESTOR_DEPTH
            || config.max_objects == 0
            || config.max_objects > HARD_MAX_OBJECTS
            || config.max_reference_edges == 0
            || config.max_reference_edges > HARD_MAX_REFERENCE_EDGES
            || config.max_total_object_read_bytes == 0
            || config.max_total_object_read_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
            || config.max_total_object_parse_bytes == 0
            || config.max_total_object_parse_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
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
            max_ancestor_depth: config.max_ancestor_depth,
            max_objects: config.max_objects,
            max_reference_edges: config.max_reference_edges,
            max_total_object_read_bytes: config.max_total_object_read_bytes,
            max_total_object_parse_bytes: config.max_total_object_parse_bytes,
            max_retained_state_bytes: config.max_retained_state_bytes,
        })
    }

    /// Returns the maximum Page/Pages dictionaries in the inheritance chain.
    pub const fn max_ancestor_depth(self) -> u64 {
        self.max_ancestor_depth
    }

    /// Returns the maximum child object jobs started across ancestors and aliases.
    pub const fn max_objects(self) -> u64 {
        self.max_objects
    }

    /// Returns the aggregate whole-object alias-edge ceiling.
    pub const fn max_reference_edges(self) -> u64 {
        self.max_reference_edges
    }

    /// Returns the cumulative exact-read ceiling across child object jobs.
    pub const fn max_total_object_read_bytes(self) -> u64 {
        self.max_total_object_read_bytes
    }

    /// Returns the cumulative parser-window ceiling across child object jobs.
    pub const fn max_total_object_parse_bytes(self) -> u64 {
        self.max_total_object_parse_bytes
    }

    /// Returns the allocator-reported retained-state capacity ceiling.
    pub const fn max_retained_state_bytes(self) -> u64 {
        self.max_retained_state_bytes
    }
}

impl Default for PageMaterializationLimits {
    fn default() -> Self {
        Self::validate(PageMaterializationLimitConfig::default())
            .expect("built-in page materialization limits satisfy hard ceilings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentErrorCategory;

    #[test]
    fn defaults_are_valid() {
        let limits = PageMaterializationLimits::default();
        assert_eq!(limits.max_ancestor_depth(), 64);
        assert_eq!(limits.max_objects(), 256);
        assert_eq!(limits.max_reference_edges(), 64);
        assert_eq!(limits.max_retained_state_bytes(), 8 * 1024 * 1024);
    }

    #[test]
    fn zero_and_above_hard_ceiling_profiles_are_rejected() {
        for config in [
            PageMaterializationLimitConfig {
                max_ancestor_depth: 0,
                ..PageMaterializationLimitConfig::default()
            },
            PageMaterializationLimitConfig {
                max_ancestor_depth: HARD_MAX_ANCESTOR_DEPTH + 1,
                ..PageMaterializationLimitConfig::default()
            },
            PageMaterializationLimitConfig {
                max_retained_state_bytes: HARD_MAX_RETAINED_STATE_BYTES + 1,
                ..PageMaterializationLimitConfig::default()
            },
        ] {
            let error = PageMaterializationLimits::validate(config)
                .expect_err("invalid materialization limits must fail");
            assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
            assert_eq!(error.category(), DocumentErrorCategory::Configuration);
        }
    }

    #[test]
    fn small_independent_budgets_are_valid_for_runtime_exact_failures() {
        let limits = PageMaterializationLimits::validate(PageMaterializationLimitConfig {
            max_ancestor_depth: 64,
            max_objects: 1,
            max_reference_edges: 1,
            max_total_object_read_bytes: 1,
            max_total_object_parse_bytes: 1,
            max_retained_state_bytes: 1,
        })
        .expect("independent one-less budgets remain valid configuration");
        assert_eq!(limits.max_objects(), 1);
        assert_eq!(limits.max_retained_state_bytes(), 1);
    }
}
