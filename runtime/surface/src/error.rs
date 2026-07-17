use core::fmt;

/// Stable content-free failure categories for the Surface lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceErrorCode {
    /// Caller-selected limits are zero, inconsistent, or above hard ceilings.
    InvalidLimits,
    /// Worker identity or Worker epoch is zero, foreign, or stale.
    InvalidWorker,
    /// Session identity is zero, unknown, foreign, or already closed.
    InvalidSession,
    /// Viewport generation is zero, stale, or non-monotonic.
    InvalidGeneration,
    /// RenderPlan, renderer epoch, region, configuration, or digest identity is invalid.
    InvalidPlan,
    /// Dimensions, stride, format, alpha, byte range, or storage extent is invalid.
    InvalidLayout,
    /// Checked arithmetic or virtual-clock arithmetic overflowed.
    NumericOverflow,
    /// A precharged session, Surface, handle, byte, or per-epoch identity limit is exhausted.
    CapacityExceeded,
    /// The requested Surface identity was never issued in the current Worker epoch.
    UnknownSurface,
    /// The requested lifecycle operation is invalid for the current Surface state.
    InvalidState,
    /// Publication was attempted before the complete pixel range was initialized.
    IncompleteSurface,
    /// Surface worker/session ownership does not match the exact operation context.
    InvalidOwner,
    /// The sensitive lease token does not match the issued Surface.
    InvalidLease,
    /// The fake handle identity, transfer token, extent, or bound Surface is invalid.
    InvalidHandle,
    /// The imported handle has the wrong platform-handle class.
    InvalidHandleClass,
    /// The imported handle does not grant exactly immutable read-only access.
    InvalidHandleAccess,
    /// The one-shot handle transfer or import has already been consumed.
    TransferConsumed,
}

impl SurfaceErrorCode {
    /// Returns a stable diagnostic identifier with no payload or handle data.
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::InvalidLimits => "RPE-SURFACE-0001",
            Self::InvalidWorker => "RPE-SURFACE-0002",
            Self::InvalidSession => "RPE-SURFACE-0003",
            Self::InvalidGeneration => "RPE-SURFACE-0004",
            Self::InvalidPlan => "RPE-SURFACE-0005",
            Self::InvalidLayout => "RPE-SURFACE-0006",
            Self::NumericOverflow => "RPE-SURFACE-0007",
            Self::CapacityExceeded => "RPE-SURFACE-0008",
            Self::UnknownSurface => "RPE-SURFACE-0009",
            Self::InvalidState => "RPE-SURFACE-0010",
            Self::IncompleteSurface => "RPE-SURFACE-0011",
            Self::InvalidOwner => "RPE-SURFACE-0012",
            Self::InvalidLease => "RPE-SURFACE-0013",
            Self::InvalidHandle => "RPE-SURFACE-0014",
            Self::InvalidHandleClass => "RPE-SURFACE-0015",
            Self::InvalidHandleAccess => "RPE-SURFACE-0016",
            Self::TransferConsumed => "RPE-SURFACE-0017",
        }
    }
}

/// Stable redacted Surface lifecycle error.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct SurfaceError {
    code: SurfaceErrorCode,
}

impl SurfaceError {
    pub(crate) const fn new(code: SurfaceErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure category.
    pub const fn code(self) -> SurfaceErrorCode {
        self.code
    }

    /// Returns the stable diagnostic identifier.
    pub const fn stable_id(self) -> &'static str {
        self.code.stable_id()
    }
}

impl fmt::Debug for SurfaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceError")
            .field("code", &self.code)
            .field("stable_id", &self.stable_id())
            .finish()
    }
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.stable_id())
    }
}

impl std::error::Error for SurfaceError {}

pub(crate) const fn error(code: SurfaceErrorCode) -> SurfaceError {
    SurfaceError::new(code)
}
