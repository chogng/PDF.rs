use crate::{SceneError, SceneErrorCode};

const HARD_MAX_COMMANDS: u32 = 4_000_000;
const HARD_MAX_RESOURCES: u32 = 1_000_000;
const HARD_MAX_MARKED_CONTENT_DEPTH: u32 = 65_536;
const HARD_MAX_NAME_BYTES: u32 = 16 * 1024 * 1024;
const HARD_MAX_RETAINED_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_RESOURCE_INDEX_WORK: u64 = 1_000_000_000_000;
const HARD_MAX_CANONICAL_BYTES: u64 = 1024 * 1024 * 1024;

/// Unvalidated Scene construction, ownership, and serialization limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneLimitConfig {
    /// Maximum semantic commands retained by one Scene.
    pub max_commands: u32,
    /// Maximum stable resources retained by one Scene.
    pub max_resources: u32,
    /// Maximum active marked-content nesting depth.
    pub max_marked_content_depth: u32,
    /// Maximum decoded bytes retained by one marked-content tag.
    pub max_name_bytes: u32,
    /// Maximum allocator-reported retained element and scalar-buffer capacity.
    pub max_retained_bytes: u64,
    /// Maximum resource-index comparison bounds and insertion shifts.
    pub max_resource_index_work: u64,
    /// Maximum canonical JSON bytes emitted for one Scene.
    pub max_canonical_bytes: u64,
}

impl Default for SceneLimitConfig {
    fn default() -> Self {
        Self {
            max_commands: 250_000,
            max_resources: 65_536,
            max_marked_content_depth: 1_024,
            max_name_bytes: 64 * 1024,
            max_retained_bytes: 128 * 1024 * 1024,
            max_resource_index_work: 4_000_000_000,
            max_canonical_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Validated Scene construction, ownership, and canonical-output limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneLimits {
    max_commands: u32,
    max_resources: u32,
    max_marked_content_depth: u32,
    max_name_bytes: u32,
    max_retained_bytes: u64,
    max_resource_index_work: u64,
    max_canonical_bytes: u64,
}

impl SceneLimits {
    /// Validates every nonzero limit against fixed implementation hard ceilings.
    pub fn validate(config: SceneLimitConfig) -> Result<Self, SceneError> {
        if config.max_commands == 0
            || config.max_commands > HARD_MAX_COMMANDS
            || config.max_resources == 0
            || config.max_resources > HARD_MAX_RESOURCES
            || config.max_marked_content_depth == 0
            || config.max_marked_content_depth > HARD_MAX_MARKED_CONTENT_DEPTH
            || config.max_name_bytes == 0
            || config.max_name_bytes > HARD_MAX_NAME_BYTES
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_RETAINED_BYTES
            || config.max_resource_index_work == 0
            || config.max_resource_index_work > HARD_MAX_RESOURCE_INDEX_WORK
            || config.max_canonical_bytes == 0
            || config.max_canonical_bytes > HARD_MAX_CANONICAL_BYTES
        {
            return Err(SceneError::for_code(SceneErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            max_commands: config.max_commands,
            max_resources: config.max_resources,
            max_marked_content_depth: config.max_marked_content_depth,
            max_name_bytes: config.max_name_bytes,
            max_retained_bytes: config.max_retained_bytes,
            max_resource_index_work: config.max_resource_index_work,
            max_canonical_bytes: config.max_canonical_bytes,
        })
    }

    /// Returns the maximum retained command count.
    pub const fn max_commands(self) -> u32 {
        self.max_commands
    }

    /// Returns the maximum retained resource count.
    pub const fn max_resources(self) -> u32 {
        self.max_resources
    }

    /// Returns the maximum active marked-content depth.
    pub const fn max_marked_content_depth(self) -> u32 {
        self.max_marked_content_depth
    }

    /// Returns the maximum decoded bytes retained by one marked-content tag.
    pub const fn max_name_bytes(self) -> u32 {
        self.max_name_bytes
    }

    /// Returns the maximum allocator-reported Scene retention.
    pub const fn max_retained_bytes(self) -> u64 {
        self.max_retained_bytes
    }

    /// Returns the maximum resource-index comparison-bound and insertion-shift work.
    pub const fn max_resource_index_work(self) -> u64 {
        self.max_resource_index_work
    }

    /// Returns the maximum canonical JSON output size.
    pub const fn max_canonical_bytes(self) -> u64 {
        self.max_canonical_bytes
    }
}

impl Default for SceneLimits {
    fn default() -> Self {
        Self::validate(SceneLimitConfig::default())
            .expect("built-in Scene limits satisfy hard ceilings")
    }
}
