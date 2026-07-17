#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Pure bounded ownership and publication lifecycle for immutable Native surfaces.

mod error;
mod limits;
mod model;
mod owner;
mod report;

pub use error::{SurfaceError, SurfaceErrorCode};
pub use limits::{SurfaceLimitConfig, SurfaceLimits};
pub use model::{
    AcquiredSurface, AllocatedSurface, FakeHandleDescriptor, FakeHandleId, FakeHandleParts,
    HandleAccess, HandleClass, ImportedSurface, PublishedSurface, RetireReason, SurfaceAccess,
    SurfaceAllocation, SurfaceConsumerContext, SurfacePlanIdentity, SurfaceTransfer, WorkerEpoch,
};
pub use owner::SurfaceOwner;
pub use report::{LifecycleReport, ReleaseOutcome, SurfaceReleaseReport, SurfaceResourceReport};
