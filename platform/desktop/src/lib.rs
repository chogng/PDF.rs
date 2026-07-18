//! Authenticated, bounded desktop host-to-worker transport.
//!
//! The host owns source bytes and capability backing. The child process receives
//! only authenticated, length-bounded canonical frames and opaque capability
//! descriptors. No module here parses PDF data or performs rendering.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod auth;
mod capability;
mod error;
mod limits;
mod process;
mod source;
#[cfg(unix)]
mod unix;
mod wire;

pub use auth::{DesktopLaunchAuth, DesktopLaunchId};
pub use capability::{
    CapabilityClass, CapabilityRights, DesktopCapability, DesktopCapabilityTable,
};
pub use error::{DesktopIpcError, DesktopIpcErrorCode};
pub use limits::{DesktopIpcLimitConfig, DesktopIpcLimits};
pub use process::{DesktopEpochManager, DesktopHostProcess, PendingDesktopRecord, run_child_stdio};
pub use source::{HostRangeBridge, HostSourceSnapshot, SourceSegment};
#[cfg(unix)]
pub use unix::{
    ReadOnlySharedRegion, receive_capability_fds, send_capability_fds, validate_read_only_fd,
};
pub use wire::{
    DesktopDirection, DesktopRecordBinding, DesktopWireRecord, validate_engine_hello_event,
    validate_host_hello_command,
};
