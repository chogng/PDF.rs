//! Authenticated, bounded desktop host-to-worker transport.
//!
//! The host owns source bytes and grants only exact immutable range segments.
//! The child process receives authenticated, length-bounded canonical frames,
//! drives the Native engine through bounded turns, and publishes immutable
//! Surfaces through epoch-bound read-only shared-memory capabilities.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod auth;
mod capability;
mod error;
mod limits;
mod native_adapter;
mod process;
mod sandbox;
mod source;
mod supervisor;
#[cfg(unix)]
mod unix;
mod wire;

pub use auth::{DesktopLaunchAuth, DesktopLaunchId};
pub use capability::{
    CapabilityClass, CapabilityRights, DesktopCapability, DesktopCapabilityTable,
};
pub use error::{DesktopIpcError, DesktopIpcErrorCode};
pub use limits::{DesktopIpcLimitConfig, DesktopIpcLimits};
pub use process::{
    DESKTOP_CHILD_PANIC_EXIT_CODE, DesktopEpochManager, DesktopHostProcess, PendingDesktopRecord,
    run_child_stdio,
};
pub use sandbox::{
    DESKTOP_PRODUCT_SANDBOX_TARGET_ID, DesktopProductSandboxAvailability,
    desktop_product_sandbox_availability,
};
pub use source::{HostRangeBridge, HostSourceSnapshot, SourceSegment};
pub use supervisor::{
    DesktopChildSupervisor, DesktopEpochCleanup, DesktopSupervisionError, DesktopSupervisorConfig,
    DesktopSupervisorState, DesktopWorkerFault, DesktopWorkerFaultKind,
};
#[cfg(unix)]
pub use unix::{
    ReadOnlySharedRegion, receive_capability_fds, send_capability_fds, validate_read_only_fd,
};
pub use wire::{
    DesktopDirection, DesktopRecordBinding, DesktopWireRecord, validate_engine_hello_event,
    validate_host_hello_command,
};
