use crate::{
    ContentImageLimit, ContentImageLimitKind, ContentOperatorSource, ContentVmError,
    ContentVmErrorCode,
};

const HARD_MAX_IMAGE_USES: u64 = 50_000_000;
const HARD_MAX_UNIQUE_IMAGES: u64 = 10_000_000;
const HARD_MAX_DECODED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HARD_MAX_PLANNING_OPERATORS: u64 = 100_000_000;
const HARD_MAX_CACHE_PROBES: u64 = 100_000_000;
const HARD_MAX_PLAN_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HARD_MAX_CACHE_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HARD_MAX_ACQUISITION_POLLS: u64 = 50_000_000;

/// Unvalidated aggregate limits for Image XObjects used by one Content interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentImageLimitConfig {
    /// Maximum executed `Do` operators retained by the interpreted result.
    pub max_image_uses: u64,
    /// Maximum distinct proof-bound Image XObjects acquired by the exact cache.
    pub max_unique_images: u64,
    /// Maximum aggregate decoded bytes copied into distinct Scene image resources.
    pub max_decoded_bytes: u64,
    /// Maximum operators structurally inspected by the one image-planning pass.
    pub max_planning_operators: u64,
    /// Maximum exact-cache key comparisons during image planning.
    pub max_cache_probes: u64,
    /// Maximum allocator-reported capacity retained by operator/proof planning state.
    pub max_plan_retained_bytes: u64,
    /// Maximum allocator-reported metadata capacity retained by the exact image cache.
    pub max_cache_retained_bytes: u64,
    /// Maximum calls admitted into lower Image XObject acquisition jobs.
    pub max_acquisition_polls: u64,
}

impl Default for ContentImageLimitConfig {
    fn default() -> Self {
        Self {
            max_image_uses: 1_000_000,
            max_unique_images: 65_536,
            max_decoded_bytes: 512 * 1024 * 1024,
            max_planning_operators: 4_000_000,
            max_cache_probes: 4_000_000,
            max_plan_retained_bytes: 64 * 1024 * 1024,
            max_cache_retained_bytes: 64 * 1024 * 1024,
            max_acquisition_polls: 1_000_000,
        }
    }
}

/// Validated aggregate limits for Image XObjects used by one Content interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentImageLimits {
    config: ContentImageLimitConfig,
}

impl ContentImageLimits {
    /// Validates every positive aggregate budget against fixed implementation ceilings.
    pub fn validate(config: ContentImageLimitConfig) -> Result<Self, ContentVmError> {
        if config.max_image_uses == 0
            || config.max_image_uses > HARD_MAX_IMAGE_USES
            || config.max_unique_images == 0
            || config.max_unique_images > HARD_MAX_UNIQUE_IMAGES
            || config.max_decoded_bytes == 0
            || config.max_decoded_bytes > HARD_MAX_DECODED_BYTES
            || config.max_planning_operators == 0
            || config.max_planning_operators > HARD_MAX_PLANNING_OPERATORS
            || config.max_cache_probes == 0
            || config.max_cache_probes > HARD_MAX_CACHE_PROBES
            || config.max_plan_retained_bytes == 0
            || config.max_plan_retained_bytes > HARD_MAX_PLAN_RETAINED_BYTES
            || config.max_cache_retained_bytes == 0
            || config.max_cache_retained_bytes > HARD_MAX_CACHE_RETAINED_BYTES
            || config.max_acquisition_polls == 0
            || config.max_acquisition_polls > HARD_MAX_ACQUISITION_POLLS
        {
            return Err(ContentVmError::new(ContentVmErrorCode::InvalidLimits, None));
        }
        Ok(Self { config })
    }

    /// Returns the aggregate executed Image XObject-use ceiling.
    pub const fn max_image_uses(self) -> u64 {
        self.config.max_image_uses
    }

    /// Returns the distinct acquired Image XObject ceiling.
    pub const fn max_unique_images(self) -> u64 {
        self.config.max_unique_images
    }

    /// Returns the aggregate distinct decoded-byte ceiling.
    pub const fn max_decoded_bytes(self) -> u64 {
        self.config.max_decoded_bytes
    }

    /// Returns the one-pass image-planning operator ceiling.
    pub const fn max_planning_operators(self) -> u64 {
        self.config.max_planning_operators
    }

    /// Returns the aggregate exact-cache comparison ceiling.
    pub const fn max_cache_probes(self) -> u64 {
        self.config.max_cache_probes
    }

    /// Returns the operator/proof planning-capacity ceiling.
    pub const fn max_plan_retained_bytes(self) -> u64 {
        self.config.max_plan_retained_bytes
    }

    /// Returns the exact-cache metadata-capacity ceiling.
    pub const fn max_cache_retained_bytes(self) -> u64 {
        self.config.max_cache_retained_bytes
    }

    /// Returns the lower acquisition-poll ceiling.
    pub const fn max_acquisition_polls(self) -> u64 {
        self.config.max_acquisition_polls
    }

    pub(crate) fn preflight(
        self,
        kind: ContentImageLimitKind,
        consumed: u64,
        attempted: u64,
        source: Option<ContentOperatorSource>,
    ) -> Result<(), ContentVmError> {
        let limit =
            match kind {
                ContentImageLimitKind::ImageUses => self.max_image_uses(),
                ContentImageLimitKind::UniqueImages => self.max_unique_images(),
                ContentImageLimitKind::DecodedBytes => self.max_decoded_bytes(),
                ContentImageLimitKind::PlanningOperators => self.max_planning_operators(),
                ContentImageLimitKind::CacheProbes => self.max_cache_probes(),
                ContentImageLimitKind::PlanRetainedBytes
                | ContentImageLimitKind::PlanAllocation => self.max_plan_retained_bytes(),
                ContentImageLimitKind::CacheRetainedBytes
                | ContentImageLimitKind::CacheAllocation => self.max_cache_retained_bytes(),
                ContentImageLimitKind::DecodedAllocation => self.max_decoded_bytes(),
                ContentImageLimitKind::AcquisitionPolls => self.max_acquisition_polls(),
            };
        if consumed
            .checked_add(attempted)
            .is_none_or(|next| next > limit)
        {
            return Err(ContentVmError::image_resource(
                ContentImageLimit::new(kind, limit, consumed, attempted),
                source,
            ));
        }
        Ok(())
    }
}

impl Default for ContentImageLimits {
    fn default() -> Self {
        Self::validate(ContentImageLimitConfig::default())
            .expect("built-in Content image limits satisfy hard ceilings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentVmErrorCategory, ContentVmRecoverability};

    #[test]
    fn defaults_and_minimums_are_valid() {
        let defaults = ContentImageLimits::default();
        assert_eq!(defaults.max_unique_images(), 65_536);
        assert_eq!(defaults.max_decoded_bytes(), 512 * 1024 * 1024);

        let minimum = ContentImageLimits::validate(ContentImageLimitConfig {
            max_image_uses: 1,
            max_unique_images: 1,
            max_decoded_bytes: 1,
            max_planning_operators: 1,
            max_cache_probes: 1,
            max_plan_retained_bytes: 1,
            max_cache_retained_bytes: 1,
            max_acquisition_polls: 1,
        })
        .expect("positive independent image budgets validate");
        assert_eq!(minimum.max_image_uses(), 1);
        assert_eq!(minimum.max_acquisition_polls(), 1);
    }

    #[test]
    fn invalid_profiles_and_resource_context_are_structured() {
        let error = ContentImageLimits::validate(ContentImageLimitConfig {
            max_unique_images: 0,
            ..ContentImageLimitConfig::default()
        })
        .expect_err("zero image budget must fail");
        assert_eq!(error.code(), ContentVmErrorCode::InvalidLimits);

        let limits = ContentImageLimits::default();
        let error = limits
            .preflight(
                ContentImageLimitKind::ImageUses,
                limits.max_image_uses(),
                1,
                None,
            )
            .expect_err("one use above the limit must fail");
        assert_eq!(error.code(), ContentVmErrorCode::ResourceLimit);
        assert_eq!(error.category(), ContentVmErrorCategory::Resource);
        assert_eq!(
            error.recoverability(),
            ContentVmRecoverability::ReduceWorkload
        );
        assert_eq!(
            error.image_limit().expect("image limit").kind(),
            ContentImageLimitKind::ImageUses
        );
    }
}
