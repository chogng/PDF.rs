use core::fmt;
use std::error::Error;

/// Stable integration-layer failure codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineIntegrationErrorCode {
    /// A configured capacity or identity is invalid.
    InvalidConfig,
    /// A protocol value or correlation failed canonical validation.
    Protocol,
    /// The command is not valid in the current Worker or Session phase.
    InvalidState,
    /// A never-reused identity was repeated or exhausted.
    InvalidIdentity,
    /// A bounded queue or registry cannot accept more work.
    Backpressure,
    /// Immutable source, document, Scene, plan, or completion identity drifted.
    IdentityMismatch,
    /// Capability policy or RenderPlan construction failed closed.
    Policy,
    /// Fast Native rasterization failed.
    Raster,
    /// Tile-cache ownership or admission failed.
    Cache,
    /// Scheduler admission, dispatch, or terminal arbitration failed.
    Scheduler,
    /// Surface allocation, publication, release, or invalidation failed.
    Surface,
    /// Checked arithmetic or an internal invariant failed closed.
    Internal,
}

/// Redacted deterministic integration failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineIntegrationError {
    code: EngineIntegrationErrorCode,
}

impl EngineIntegrationError {
    pub(crate) const fn new(code: EngineIntegrationErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable error code.
    pub const fn code(self) -> EngineIntegrationErrorCode {
        self.code
    }
}

impl fmt::Display for EngineIntegrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "native engine integration failure ({:?})",
            self.code
        )
    }
}

impl Error for EngineIntegrationError {}

pub(crate) const fn invalid_config() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::InvalidConfig)
}

pub(crate) const fn protocol() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::Protocol)
}

pub(crate) const fn invalid_state() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::InvalidState)
}

pub(crate) const fn invalid_identity() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::InvalidIdentity)
}

pub(crate) const fn backpressure() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::Backpressure)
}

pub(crate) const fn identity_mismatch() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::IdentityMismatch)
}

pub(crate) const fn policy() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::Policy)
}

pub(crate) const fn cache() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::Cache)
}

pub(crate) const fn scheduler() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::Scheduler)
}

pub(crate) const fn surface() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::Surface)
}

pub(crate) const fn internal() -> EngineIntegrationError {
    EngineIntegrationError::new(EngineIntegrationErrorCode::Internal)
}
