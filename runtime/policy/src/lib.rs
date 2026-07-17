//! Bounded product capability policy and backend-neutral Native render planning.
//!
//! This crate is the product boundary between immutable [`pdf_rs_scene::Scene`] values and
//! Native raster scheduling. It evaluates the complete Scene capability graph before expensive
//! raster allocation, retains canonical bounded decisions, and creates complete render and tile
//! identities only for supported decisions. It contains no renderer implementation, platform I/O,
//! external-engine fallback, or dependency on the M3 Reference renderer.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod canonical_hash;
mod capability;
mod error;
mod identity;
mod limits;
mod protocol_projection;
mod render_config;
mod render_plan;

pub use capability::{
    CapabilityContributor, CapabilityContributorKind, CapabilityDecision, CapabilityEvaluator,
    CapabilityLocation, CapabilityProfile, CapabilityProfileId, CapabilityRejectionCode,
    CapabilityScope, CapabilityStatus, CapabilitySubject, CollectionCompleteness,
    MissingCapabilityRequirement, NeverCancelled, PolicyCancellation,
};
pub use error::{
    PolicyError, PolicyErrorCategory, PolicyErrorCode, PolicyLimit, PolicyLimitKind,
    PolicyRecoverability,
};
pub use identity::{
    CapabilityDecisionHash, GeometryHash, OptionalContentIdentity, PlannedTileHash,
    RenderConfigHash, RenderPlanHash, RendererEpoch, SceneHash, TileContentHash,
};
pub use limits::{PolicyLimitConfig, PolicyLimits};
pub use render_config::{
    AlphaMode, AntialiasMode, ColorProfile, CompositingMode, GlyphSampling, ImageSampling,
    NativeBackend, OutputProfile, PixelFormat, QualityPolicy, RenderConfig, RenderConfigInput,
};
pub use render_plan::{
    DeviceRect, PlannedTileIdentity, RenderPlan, RenderPlanId, RenderPlanOutcome,
    RenderPlanRequest, TileContentKey, ViewportIdentity, ZoomRatio, create_render_plan,
};
