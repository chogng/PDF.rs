use crate::{
    ContentGraphicsLimit, ContentGraphicsLimitKind, ContentOperatorSource, ContentVmError,
    ContentVmErrorCode,
};

const HARD_MAX_PATH_SEGMENTS: u64 = 50_000_000;
const HARD_MAX_PATH_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HARD_MAX_DASH_ENTRIES: u32 = 1_000_000;
const HARD_MAX_DASH_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Unvalidated deterministic limits for the explicit graphics-v2 Content profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentGraphicsLimitConfig {
    /// Maximum segments retained by the current path.
    pub max_path_segments: u64,
    /// Maximum allocator-reported capacity retained by the current path.
    pub max_path_retained_bytes: u64,
    /// Maximum entries accepted in one line-dash array.
    pub max_dash_entries: u32,
    /// Maximum aggregate unique dash-array capacity retained by active graphics states.
    pub max_dash_retained_bytes: u64,
}

impl Default for ContentGraphicsLimitConfig {
    fn default() -> Self {
        Self {
            max_path_segments: 4_000_000,
            max_path_retained_bytes: 256 * 1024 * 1024,
            max_dash_entries: 65_536,
            max_dash_retained_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Fully validated deterministic limits for the explicit graphics-v2 Content profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentGraphicsLimits {
    max_path_segments: u64,
    max_path_retained_bytes: u64,
    max_dash_entries: u32,
    max_dash_retained_bytes: u64,
}

impl ContentGraphicsLimits {
    /// Validates every nonzero dimension against fixed implementation hard ceilings.
    pub fn validate(config: ContentGraphicsLimitConfig) -> Result<Self, ContentVmError> {
        if config.max_path_segments == 0
            || config.max_path_segments > HARD_MAX_PATH_SEGMENTS
            || config.max_path_retained_bytes == 0
            || config.max_path_retained_bytes > HARD_MAX_PATH_RETAINED_BYTES
            || config.max_dash_entries == 0
            || config.max_dash_entries > HARD_MAX_DASH_ENTRIES
            || config.max_dash_retained_bytes == 0
            || config.max_dash_retained_bytes > HARD_MAX_DASH_RETAINED_BYTES
        {
            return Err(ContentVmError::new(ContentVmErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            max_path_segments: config.max_path_segments,
            max_path_retained_bytes: config.max_path_retained_bytes,
            max_dash_entries: config.max_dash_entries,
            max_dash_retained_bytes: config.max_dash_retained_bytes,
        })
    }

    /// Returns the maximum current-path segment count.
    pub const fn max_path_segments(self) -> u64 {
        self.max_path_segments
    }

    /// Returns the maximum allocator-reported current-path capacity.
    pub const fn max_path_retained_bytes(self) -> u64 {
        self.max_path_retained_bytes
    }

    /// Returns the maximum entries in one line-dash array.
    pub const fn max_dash_entries(self) -> u32 {
        self.max_dash_entries
    }

    /// Returns the maximum aggregate unique dash-array retained capacity.
    pub const fn max_dash_retained_bytes(self) -> u64 {
        self.max_dash_retained_bytes
    }

    pub(crate) fn preflight(
        self,
        kind: ContentGraphicsLimitKind,
        consumed: u64,
        attempted: u64,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        let limit = match kind {
            ContentGraphicsLimitKind::PathSegments => self.max_path_segments,
            ContentGraphicsLimitKind::PathRetainedBytes => self.max_path_retained_bytes,
            ContentGraphicsLimitKind::DashEntries => u64::from(self.max_dash_entries),
            ContentGraphicsLimitKind::DashRetainedBytes => self.max_dash_retained_bytes,
        };
        if consumed
            .checked_add(attempted)
            .is_none_or(|next| next > limit)
        {
            return Err(ContentVmError::graphics_resource(
                ContentGraphicsLimit::new(kind, limit, consumed, attempted),
                Some(source),
            ));
        }
        Ok(())
    }
}

impl Default for ContentGraphicsLimits {
    fn default() -> Self {
        Self::validate(ContentGraphicsLimitConfig::default())
            .expect("built-in Content graphics limits satisfy hard ceilings")
    }
}
