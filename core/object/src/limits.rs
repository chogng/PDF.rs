use crate::{ObjectError, ObjectErrorCode};

const HARD_MAX_SOURCE_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_ENVELOPE_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_BOUNDARY_BYTES: u64 = 4 * 1024 * 1024;
const HARD_MAX_STREAM_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Unvalidated deterministic indirect-object framing limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectLimitConfig {
    /// Maximum immutable source length accepted by this object profile.
    pub max_source_bytes: u64,
    /// First range requested at the target indirect-object offset.
    pub initial_envelope_bytes: u64,
    /// Maximum contiguous indirect-object envelope window.
    pub max_envelope_bytes: u64,
    /// First range requested at a declared stream payload end.
    pub initial_boundary_bytes: u64,
    /// Maximum contiguous stream-boundary window.
    pub max_boundary_bytes: u64,
    /// Maximum declared stream payload length.
    pub max_stream_bytes: u64,
    /// Maximum cumulative exact requested bytes across window growth.
    pub max_total_read_bytes: u64,
    /// Maximum cumulative complete windows parsed across retries.
    pub max_total_parse_bytes: u64,
}

impl Default for ObjectLimitConfig {
    fn default() -> Self {
        Self {
            max_source_bytes: 256 * 1024 * 1024,
            initial_envelope_bytes: 4 * 1024,
            max_envelope_bytes: 1024 * 1024,
            initial_boundary_bytes: 256,
            max_boundary_bytes: 16 * 1024,
            max_stream_bytes: 128 * 1024 * 1024,
            max_total_read_bytes: 4 * 1024 * 1024,
            max_total_parse_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Validated indirect-object limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectLimits {
    pub(crate) max_source_bytes: u64,
    pub(crate) initial_envelope_bytes: u64,
    pub(crate) max_envelope_bytes: u64,
    pub(crate) initial_boundary_bytes: u64,
    pub(crate) max_boundary_bytes: u64,
    pub(crate) max_stream_bytes: u64,
    pub(crate) max_total_read_bytes: u64,
    pub(crate) max_total_parse_bytes: u64,
}

impl ObjectLimits {
    /// Validates a complete indirect-object framing budget profile.
    pub fn validate(config: ObjectLimitConfig) -> Result<Self, ObjectError> {
        let minimum_total = config
            .max_envelope_bytes
            .checked_add(config.max_boundary_bytes);
        if config.max_source_bytes == 0
            || config.max_source_bytes > HARD_MAX_SOURCE_BYTES
            || config.initial_envelope_bytes == 0
            || config.initial_envelope_bytes > config.max_envelope_bytes
            || config.max_envelope_bytes > HARD_MAX_ENVELOPE_BYTES
            || config.max_envelope_bytes > config.max_source_bytes
            || config.initial_boundary_bytes == 0
            || config.initial_boundary_bytes > config.max_boundary_bytes
            || config.max_boundary_bytes > HARD_MAX_BOUNDARY_BYTES
            || config.max_boundary_bytes > config.max_source_bytes
            || config.max_stream_bytes == 0
            || config.max_stream_bytes > HARD_MAX_STREAM_BYTES
            || config.max_stream_bytes > config.max_source_bytes
            || config.max_total_read_bytes == 0
            || config.max_total_read_bytes > HARD_MAX_TOTAL_BYTES
            || minimum_total.is_none_or(|minimum| config.max_total_read_bytes < minimum)
            || config.max_total_parse_bytes == 0
            || config.max_total_parse_bytes > HARD_MAX_TOTAL_BYTES
            || minimum_total.is_none_or(|minimum| config.max_total_parse_bytes < minimum)
        {
            return Err(ObjectError::for_code(
                ObjectErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_source_bytes: config.max_source_bytes,
            initial_envelope_bytes: config.initial_envelope_bytes,
            max_envelope_bytes: config.max_envelope_bytes,
            initial_boundary_bytes: config.initial_boundary_bytes,
            max_boundary_bytes: config.max_boundary_bytes,
            max_stream_bytes: config.max_stream_bytes,
            max_total_read_bytes: config.max_total_read_bytes,
            max_total_parse_bytes: config.max_total_parse_bytes,
        })
    }

    /// Returns the maximum accepted immutable source length.
    pub const fn max_source_bytes(self) -> u64 {
        self.max_source_bytes
    }

    /// Returns the first object-envelope request size.
    pub const fn initial_envelope_bytes(self) -> u64 {
        self.initial_envelope_bytes
    }

    /// Returns the maximum object-envelope window size.
    pub const fn max_envelope_bytes(self) -> u64 {
        self.max_envelope_bytes
    }

    /// Returns the first stream-boundary request size.
    pub const fn initial_boundary_bytes(self) -> u64 {
        self.initial_boundary_bytes
    }

    /// Returns the maximum stream-boundary window size.
    pub const fn max_boundary_bytes(self) -> u64 {
        self.max_boundary_bytes
    }

    /// Returns the maximum accepted declared stream payload length.
    pub const fn max_stream_bytes(self) -> u64 {
        self.max_stream_bytes
    }

    /// Returns the cumulative exact-read ceiling.
    pub const fn max_total_read_bytes(self) -> u64 {
        self.max_total_read_bytes
    }

    /// Returns the cumulative parse-window ceiling.
    pub const fn max_total_parse_bytes(self) -> u64 {
        self.max_total_parse_bytes
    }
}

impl Default for ObjectLimits {
    fn default() -> Self {
        Self::validate(ObjectLimitConfig::default())
            .expect("built-in object limits satisfy hard ceilings")
    }
}
