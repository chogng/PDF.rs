use pdf_rs_protocol::ProtocolLimits;

use crate::error::error;
use crate::{SurfaceError, SurfaceErrorCode};

const HARD_MAX_SESSIONS_PER_EPOCH: usize = 65_536;
const HARD_MAX_LIVE_SURFACES: usize = 65_536;
const HARD_MAX_HANDLES: usize = 65_536;
const HARD_MAX_SURFACE_IDS_PER_EPOCH: u64 = 16 * 1024 * 1024;
const HARD_MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const HARD_MAX_LEASE_TICKS: u64 = 1_000_000_000_000;

/// Caller-selected bounded Surface-owner capacities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceLimitConfig {
    /// Canonical protocol ceilings used on both producer and consumer validation.
    pub protocol: ProtocolLimits,
    /// Maximum distinct Session IDs admitted during one Worker epoch.
    pub max_sessions_per_epoch: usize,
    /// Maximum concurrently private or published Surfaces.
    pub max_live_surfaces: usize,
    /// Maximum concurrently allocated fake platform handles.
    pub max_handles: usize,
    /// Maximum Surface IDs issued during one Worker epoch.
    pub max_surface_ids_per_epoch: u64,
    /// Maximum total bytes retained by all live Surface regions.
    pub max_total_bytes: u64,
    /// Exact deterministic lease duration in virtual ticks.
    pub lease_ticks: u64,
}

impl Default for SurfaceLimitConfig {
    fn default() -> Self {
        Self {
            protocol: ProtocolLimits::default(),
            max_sessions_per_epoch: 1_024,
            max_live_surfaces: 4_096,
            max_handles: 4_096,
            max_surface_ids_per_epoch: 1_048_576,
            max_total_bytes: 1024 * 1024 * 1024,
            lease_ticks: 30_000,
        }
    }
}

/// Fully validated Surface-owner limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceLimits {
    protocol: ProtocolLimits,
    max_sessions_per_epoch: usize,
    max_live_surfaces: usize,
    max_handles: usize,
    max_surface_ids_per_epoch: u64,
    max_total_bytes: u64,
    lease_ticks: u64,
}

impl SurfaceLimits {
    /// Validates every independent capacity without allocating.
    pub fn new(config: SurfaceLimitConfig) -> Result<Self, SurfaceError> {
        if config.max_sessions_per_epoch == 0
            || config.max_sessions_per_epoch > HARD_MAX_SESSIONS_PER_EPOCH
            || config.max_live_surfaces == 0
            || config.max_live_surfaces > HARD_MAX_LIVE_SURFACES
            || config.max_handles == 0
            || config.max_handles > HARD_MAX_HANDLES
            || config.max_surface_ids_per_epoch == 0
            || config.max_surface_ids_per_epoch > HARD_MAX_SURFACE_IDS_PER_EPOCH
            || config.max_total_bytes == 0
            || config.max_total_bytes > HARD_MAX_TOTAL_BYTES
            || config.lease_ticks == 0
            || config.lease_ticks > HARD_MAX_LEASE_TICKS
        {
            return Err(error(SurfaceErrorCode::InvalidLimits));
        }
        Ok(Self {
            protocol: config.protocol,
            max_sessions_per_epoch: config.max_sessions_per_epoch,
            max_live_surfaces: config.max_live_surfaces,
            max_handles: config.max_handles,
            max_surface_ids_per_epoch: config.max_surface_ids_per_epoch,
            max_total_bytes: config.max_total_bytes,
            lease_ticks: config.lease_ticks,
        })
    }

    /// Returns canonical protocol validation limits.
    pub const fn protocol(self) -> ProtocolLimits {
        self.protocol
    }

    /// Returns the maximum distinct Session IDs per Worker epoch.
    pub const fn max_sessions_per_epoch(self) -> usize {
        self.max_sessions_per_epoch
    }

    /// Returns the maximum concurrent live Surfaces.
    pub const fn max_live_surfaces(self) -> usize {
        self.max_live_surfaces
    }

    /// Returns the maximum concurrent fake handles.
    pub const fn max_handles(self) -> usize {
        self.max_handles
    }

    /// Returns the maximum Surface IDs issued per Worker epoch.
    pub const fn max_surface_ids_per_epoch(self) -> u64 {
        self.max_surface_ids_per_epoch
    }

    /// Returns the maximum aggregate retained bytes.
    pub const fn max_total_bytes(self) -> u64 {
        self.max_total_bytes
    }

    /// Returns the exact virtual lease duration.
    pub const fn lease_ticks(self) -> u64 {
        self.lease_ticks
    }
}

impl Default for SurfaceLimits {
    fn default() -> Self {
        Self::new(SurfaceLimitConfig::default())
            .expect("crate-owned Surface defaults obey hard ceilings")
    }
}
