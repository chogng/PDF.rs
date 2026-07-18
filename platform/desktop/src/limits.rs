use crate::{DesktopIpcError, DesktopIpcErrorCode, error::error};

const HARD_MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
/// SCM_RIGHTS is intentionally capped well below kernel ancillary-message limits.
const HARD_MAX_CAPABILITIES: usize = 64;
const HARD_MAX_SOURCE_BYTES: usize = 512 * 1024 * 1024;

/// Unvalidated host-to-child desktop transport limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopIpcLimitConfig {
    /// Maximum complete authenticated IPC record bytes, excluding its u32 prefix.
    pub max_record_bytes: usize,
    /// Maximum OOB capability descriptors in one record and one host table.
    pub max_capabilities: usize,
    /// Maximum immutable Host-owned source snapshot bytes.
    pub max_source_bytes: usize,
    /// Maximum aggregate OOB shared-memory capability bytes across source and Surface regions.
    pub max_capability_bytes: usize,
}

impl Default for DesktopIpcLimitConfig {
    fn default() -> Self {
        Self {
            max_record_bytes: 4 * 1024 * 1024,
            max_capabilities: 64,
            max_source_bytes: 128 * 1024 * 1024,
            max_capability_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Validated, allocation-safe desktop transport limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopIpcLimits {
    max_record_bytes: usize,
    max_capabilities: usize,
    max_source_bytes: usize,
    max_capability_bytes: usize,
}

impl DesktopIpcLimits {
    /// Validates fixed capacity bounds before any transport allocation.
    pub fn new(config: DesktopIpcLimitConfig) -> Result<Self, DesktopIpcError> {
        if config.max_record_bytes == 0
            || config.max_record_bytes > HARD_MAX_RECORD_BYTES
            || config.max_capabilities == 0
            || config.max_capabilities > HARD_MAX_CAPABILITIES
            || config.max_source_bytes == 0
            || config.max_source_bytes > HARD_MAX_SOURCE_BYTES
            || config.max_capability_bytes == 0
            || config.max_capability_bytes > HARD_MAX_SOURCE_BYTES
        {
            return Err(error(DesktopIpcErrorCode::InvalidConfiguration));
        }
        Ok(Self {
            max_record_bytes: config.max_record_bytes,
            max_capabilities: config.max_capabilities,
            max_source_bytes: config.max_source_bytes,
            max_capability_bytes: config.max_capability_bytes,
        })
    }

    /// Returns the outer framed-record limit.
    pub const fn max_record_bytes(self) -> usize {
        self.max_record_bytes
    }
    /// Returns the exact capability-table capacity.
    pub const fn max_capabilities(self) -> usize {
        self.max_capabilities
    }
    /// Returns the immutable source snapshot capacity.
    pub const fn max_source_bytes(self) -> usize {
        self.max_source_bytes
    }
    /// Returns the aggregate capability backing limit.
    pub const fn max_capability_bytes(self) -> usize {
        self.max_capability_bytes
    }
}
