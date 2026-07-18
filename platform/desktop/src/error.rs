use core::fmt;
use std::error::Error;

/// Stable, payload- and handle-redacted desktop IPC failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopIpcErrorCode {
    /// Configuration or a fixed capacity is invalid.
    InvalidConfiguration,
    /// A framed record is truncated, oversized, or has an invalid layout.
    InvalidFrame,
    /// The launch token, peer process identity, direction, or epoch is invalid.
    Authentication,
    /// A direction-local sequence is duplicate, zero, or regressing.
    Sequence,
    /// A capability descriptor is foreign, stale, wrongly owned, or lacks rights.
    Capability,
    /// A source ticket/range is not an exact immutable host snapshot request.
    Source,
    /// Pipe I/O disconnected or failed.
    Disconnected,
    /// Child lifecycle, shutdown, or restart state is invalid.
    Lifecycle,
    /// A child panic was contained at the process boundary.
    ChildPanic,
    /// Checked arithmetic or a bounded allocation failed.
    ResourceLimit,
}

/// Deterministic redacted desktop IPC error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopIpcError {
    code: DesktopIpcErrorCode,
}

impl DesktopIpcError {
    pub(crate) const fn new(code: DesktopIpcErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable category without exposing payloads, tokens, or handles.
    pub const fn code(self) -> DesktopIpcErrorCode {
        self.code
    }
}

impl fmt::Display for DesktopIpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "desktop IPC failure ({:?})", self.code)
    }
}

impl Error for DesktopIpcError {}

pub(crate) const fn error(code: DesktopIpcErrorCode) -> DesktopIpcError {
    DesktopIpcError::new(code)
}
