use std::num::NonZeroU32;

use crate::{PolicyError, PolicyLimitKind};

const HARD_MAX_POLL_WORK_UNITS: u32 = 4_096;
const HARD_MAX_ATOMIC_CANONICAL_BYTES: u64 = 8 * 1024 * 1024;
const HARD_MAX_JOB_RETAINED_BYTES: u64 = 512 * 1024 * 1024;
const CANONICAL_WRITER_MIN_CAPACITY: u64 = 64;

/// Validated nonzero amount of resumable policy work admitted by one poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyPollBudget(NonZeroU32);

impl PolicyPollBudget {
    /// Validates a nonzero poll budget against the fixed actor-turn ceiling.
    pub fn new(work_units: NonZeroU32) -> Result<Self, PolicyError> {
        if work_units.get() > HARD_MAX_POLL_WORK_UNITS {
            return Err(PolicyError::resource(
                PolicyLimitKind::PollWorkUnits,
                u64::from(HARD_MAX_POLL_WORK_UNITS),
                0,
                u64::from(work_units.get()),
            ));
        }
        Ok(Self(work_units))
    }

    /// Returns the maximum work units this poll may consume.
    pub const fn work_units(self) -> NonZeroU32 {
        self.0
    }
}

/// Unvalidated allocation limits for one owned pollable policy job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyJobLimitConfig {
    /// Maximum bytes one non-resumable Scene canonicalization phase may publish.
    pub max_atomic_canonical_bytes: u64,
    /// Maximum aggregate bytes retained by job-owned working vectors.
    pub max_retained_bytes: u64,
}

impl Default for PolicyJobLimitConfig {
    fn default() -> Self {
        Self {
            max_atomic_canonical_bytes: 1024 * 1024,
            max_retained_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Validated allocation limits for one owned pollable policy job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyJobLimits {
    config: PolicyJobLimitConfig,
}

impl PolicyJobLimits {
    /// Validates nonzero limits against fixed process ceilings.
    ///
    /// Scene canonical writers start with 64 bytes and geometrically double their capacity. The
    /// retained-byte ceiling therefore covers twice the permitted logical canonical output (or
    /// the initial 64-byte allocation), so the atomic serializer cannot exceed the job budget
    /// before its allocator-reported capacity is charged.
    pub fn validate(config: PolicyJobLimitConfig) -> Result<Self, PolicyError> {
        let minimum_retained_bytes = config
            .max_atomic_canonical_bytes
            .checked_mul(2)
            .map(|bytes| bytes.max(CANONICAL_WRITER_MIN_CAPACITY));
        if config.max_atomic_canonical_bytes == 0
            || config.max_atomic_canonical_bytes > HARD_MAX_ATOMIC_CANONICAL_BYTES
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_JOB_RETAINED_BYTES
            || minimum_retained_bytes.is_none_or(|minimum| config.max_retained_bytes < minimum)
        {
            return Err(PolicyError::invalid_limits());
        }
        Ok(Self { config })
    }

    /// Returns the byte ceiling for the one explicitly atomic canonicalization phase.
    pub const fn max_atomic_canonical_bytes(self) -> u64 {
        self.config.max_atomic_canonical_bytes
    }

    /// Returns the aggregate job-owned retained byte ceiling.
    pub const fn max_retained_bytes(self) -> u64 {
        self.config.max_retained_bytes
    }

    pub(crate) const fn synchronous_compatibility(
        max_atomic_canonical_bytes: u64,
        max_retained_bytes: u64,
    ) -> Self {
        Self {
            config: PolicyJobLimitConfig {
                max_atomic_canonical_bytes,
                max_retained_bytes,
            },
        }
    }
}

impl Default for PolicyJobLimits {
    fn default() -> Self {
        Self::validate(PolicyJobLimitConfig::default())
            .expect("built-in pollable policy limits satisfy fixed hard ceilings")
    }
}

/// Observable state returned after one bounded policy-job poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyJobPoll {
    /// More bounded work remains.
    Pending,
    /// The job reached one replayable terminal result.
    Ready,
}

/// Deterministic allocation and work accounting for one owned policy job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PolicyJobStats {
    work_units: u64,
    allocations: u64,
    retained_bytes: u64,
    peak_retained_bytes: u64,
    atomic_canonical_bytes: u64,
}

impl PolicyJobStats {
    /// Returns cumulative resumable work units consumed.
    pub const fn work_units(self) -> u64 {
        self.work_units
    }

    /// Returns successful job-owned allocation operations.
    pub const fn allocations(self) -> u64 {
        self.allocations
    }

    /// Returns current job-owned retained vector capacity in bytes.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    /// Returns peak job-owned retained vector capacity in bytes.
    pub const fn peak_retained_bytes(self) -> u64 {
        self.peak_retained_bytes
    }

    /// Returns bytes published by the explicitly atomic canonicalization phase.
    pub const fn atomic_canonical_bytes(self) -> u64 {
        self.atomic_canonical_bytes
    }

    pub(crate) fn charge_work(&mut self) -> Result<(), PolicyError> {
        self.work_units = self
            .work_units
            .checked_add(1)
            .ok_or_else(PolicyError::numeric_overflow)?;
        Ok(())
    }

    pub(crate) fn charge_allocation(
        &mut self,
        bytes: u64,
        limits: PolicyJobLimits,
    ) -> Result<(), PolicyError> {
        let attempted = self
            .retained_bytes
            .checked_add(bytes)
            .ok_or_else(PolicyError::numeric_overflow)?;
        if attempted > limits.max_retained_bytes() {
            return Err(PolicyError::resource(
                PolicyLimitKind::JobRetainedBytes,
                limits.max_retained_bytes(),
                self.retained_bytes,
                bytes,
            ));
        }
        self.allocations = self
            .allocations
            .checked_add(1)
            .ok_or_else(PolicyError::numeric_overflow)?;
        self.retained_bytes = attempted;
        self.peak_retained_bytes = self.peak_retained_bytes.max(attempted);
        Ok(())
    }

    pub(crate) fn release(&mut self, bytes: u64) -> Result<(), PolicyError> {
        self.retained_bytes = self
            .retained_bytes
            .checked_sub(bytes)
            .ok_or_else(PolicyError::identity_mismatch)?;
        Ok(())
    }

    pub(crate) fn set_atomic_canonical_bytes(&mut self, bytes: u64) {
        self.atomic_canonical_bytes = bytes;
    }

    pub(crate) const fn clear_retained(&mut self) {
        self.retained_bytes = 0;
    }
}
