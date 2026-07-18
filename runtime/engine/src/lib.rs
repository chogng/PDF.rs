//! Bounded Native Worker and Session integration.
//!
//! [`NativeWorkerRegistry`] is a synchronous actor owner. Command ingress only
//! validates typed protocol values and admits bounded work; parser, range,
//! policy, raster, release, close, shutdown, and restart results cross explicit
//! bounded reentry queues. Capability policy is reported before Fast CPU raster
//! work, and only the scheduler's exact terminal decision may publish a Surface.
//!
//! This crate performs no filesystem, network, platform transport, or external
//! engine work.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::large_enum_variant,
    clippy::result_large_err,
    reason = "proof-bearing actor messages retain ownership on bounded admission failure"
)]

mod error;
mod limits;
mod model;
mod registry;

pub use error::{EngineIntegrationError, EngineIntegrationErrorCode};
pub use limits::{NativeWorkerConfig, NativeWorkerLimitConfig};
pub use model::{
    ActorProgress, ImportedSurfaceBytes, NativeCapabilityCompletion, NativePlanCompletion,
    NativePolicyFailure, NativePolicyTask, NativeRasterCompletion, NativeRasterFailure,
    NativeRasterTask, NativeWorkerEvent, NativeWorkerPhase, NativeWorkerResources, OpenCompletion,
    Reentry, ReentryAdmissionError, SessionPhase, SurfacePublication,
};
pub use registry::NativeWorkerRegistry;
