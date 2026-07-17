use crate::PolicyError;

const HARD_MAX_REQUIREMENTS: u32 = 4_000_000;
const HARD_MAX_DEPENDENCIES: u32 = 16_000_000;
const HARD_MAX_PARAMETERS: u32 = 4_000_000;
const WIRE_MAX_MISSING: u32 = pdf_rs_protocol::CAPABILITY_DECISION_MISSING_MAX_COUNT as u32;
const WIRE_MAX_CONTRIBUTORS: u32 =
    pdf_rs_protocol::CAPABILITY_DECISION_CONTRIBUTORS_MAX_COUNT as u32;
const WIRE_MAX_DEPENDENCIES_PER_REQUIREMENT: u32 =
    pdf_rs_protocol::CAPABILITY_REQUIREMENT_DEPENDENCIES_MAX_COUNT as u32;
const HARD_MAX_LOCATIONS: u32 = 32;
const WIRE_MAX_PLAN_REGIONS: u32 = pdf_rs_protocol::RENDER_PLAN_MANIFEST_REGIONS_MAX_COUNT as u32;
const HARD_MAX_OUTPUT_DIMENSION: u32 = i32::MAX as u32;
const HARD_MAX_OUTPUT_PIXELS: u64 = 4_000_000_000;
const HARD_MAX_CANCELLATION_INTERVAL: u32 = 1_000_000;

/// Unvalidated product capability and RenderPlan limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyLimitConfig {
    /// Maximum capability requirement nodes evaluated.
    pub max_requirements: u32,
    /// Maximum aggregate dependency edges evaluated.
    pub max_dependencies: u32,
    /// Maximum capability parameters evaluated.
    pub max_parameters: u32,
    /// Maximum dependencies accepted on one wire-projectable requirement.
    pub max_dependencies_per_requirement: u32,
    /// Maximum canonical missing requirements retained.
    pub max_missing_retained: u32,
    /// Maximum canonical decision contributors retained.
    pub max_contributors_retained: u32,
    /// Maximum canonical structured locations retained.
    pub max_locations_retained: u32,
    /// Maximum product tiles in one plan.
    pub max_tiles: u32,
    /// Maximum width or height of the requested output region.
    pub max_output_dimension: u32,
    /// Maximum output pixels represented by one plan.
    pub max_output_pixels: u64,
    /// Deterministic evaluator/planner work interval between cancellation checks.
    pub cancellation_interval: u32,
}

impl Default for PolicyLimitConfig {
    fn default() -> Self {
        Self {
            max_requirements: 250_000,
            max_dependencies: 1_000_000,
            max_parameters: 250_000,
            max_dependencies_per_requirement: WIRE_MAX_DEPENDENCIES_PER_REQUIREMENT,
            max_missing_retained: WIRE_MAX_MISSING,
            max_contributors_retained: WIRE_MAX_CONTRIBUTORS,
            max_locations_retained: HARD_MAX_LOCATIONS,
            max_tiles: WIRE_MAX_PLAN_REGIONS,
            max_output_dimension: 1_000_000,
            max_output_pixels: 1_000_000_000,
            cancellation_interval: 256,
        }
    }
}

/// Fully validated product capability and RenderPlan limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyLimits {
    config: PolicyLimitConfig,
}

impl PolicyLimits {
    /// Validates nonzero work bounds and generated-wire retention ceilings.
    pub fn validate(config: PolicyLimitConfig) -> Result<Self, PolicyError> {
        if config.max_requirements == 0
            || config.max_requirements > HARD_MAX_REQUIREMENTS
            || config.max_dependencies == 0
            || config.max_dependencies > HARD_MAX_DEPENDENCIES
            || config.max_parameters == 0
            || config.max_parameters > HARD_MAX_PARAMETERS
            || config.max_dependencies_per_requirement == 0
            || config.max_dependencies_per_requirement > WIRE_MAX_DEPENDENCIES_PER_REQUIREMENT
            || config.max_missing_retained > WIRE_MAX_MISSING
            || config.max_contributors_retained > WIRE_MAX_CONTRIBUTORS
            || config.max_locations_retained > HARD_MAX_LOCATIONS
            || config.max_tiles == 0
            || config.max_tiles > WIRE_MAX_PLAN_REGIONS
            || config.max_output_dimension == 0
            || config.max_output_dimension > HARD_MAX_OUTPUT_DIMENSION
            || config.max_output_pixels == 0
            || config.max_output_pixels > HARD_MAX_OUTPUT_PIXELS
            || config.cancellation_interval == 0
            || config.cancellation_interval > HARD_MAX_CANCELLATION_INTERVAL
        {
            return Err(PolicyError::invalid_limits());
        }
        Ok(Self { config })
    }

    /// Returns the maximum requirement count.
    pub const fn max_requirements(self) -> u32 {
        self.config.max_requirements
    }

    /// Returns the maximum aggregate dependency count.
    pub const fn max_dependencies(self) -> u32 {
        self.config.max_dependencies
    }

    /// Returns the maximum evaluated parameter count.
    pub const fn max_parameters(self) -> u32 {
        self.config.max_parameters
    }

    /// Returns the accepted dependency fanout per requirement.
    pub const fn max_dependencies_per_requirement(self) -> u32 {
        self.config.max_dependencies_per_requirement
    }

    /// Returns the retained missing-requirement prefix bound.
    pub const fn max_missing_retained(self) -> u32 {
        self.config.max_missing_retained
    }

    /// Returns the retained contributor prefix bound.
    pub const fn max_contributors_retained(self) -> u32 {
        self.config.max_contributors_retained
    }

    /// Returns the retained location prefix bound.
    pub const fn max_locations_retained(self) -> u32 {
        self.config.max_locations_retained
    }

    /// Returns the tile-count bound.
    pub const fn max_tiles(self) -> u32 {
        self.config.max_tiles
    }

    /// Returns the output-dimension bound.
    pub const fn max_output_dimension(self) -> u32 {
        self.config.max_output_dimension
    }

    /// Returns the output-pixel bound.
    pub const fn max_output_pixels(self) -> u64 {
        self.config.max_output_pixels
    }

    /// Returns the deterministic cancellation-check interval.
    pub const fn cancellation_interval(self) -> u32 {
        self.config.cancellation_interval
    }
}

impl Default for PolicyLimits {
    fn default() -> Self {
        Self::validate(PolicyLimitConfig::default())
            .expect("built-in product policy limits satisfy fixed hard ceilings")
    }
}
