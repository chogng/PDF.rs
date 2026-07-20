//! Immutable, bounded, backend-neutral Scene v1 foundations.
//!
//! This crate owns semantic Scene values after PDF document and content processing. It contains
//! no source acquisition, object resolution, platform I/O, renderer types, or external-engine
//! fallback. Scene construction is bounded, numeric values use checked fixed-point arithmetic,
//! and canonical JSON omits runtime source identity while preserving stable page and command
//! provenance.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod builder;
mod canonical;
mod diff;
mod error;
mod graphics;
mod graphics_builder;
mod limits;
mod model;
mod scalar;

pub use builder::SceneBuilder;
pub use diff::{
    SceneDiff, SceneDiffField, SceneDiffKind, SceneDiffLimitConfig, SceneDiffLimits,
    SceneDiffSection, SceneDiffStats, SceneDifference, compare_scenes,
};
pub use error::{
    SceneError, SceneErrorCategory, SceneErrorCode, SceneLimit, SceneLimitKind, SceneRecoverability,
};
pub use graphics::{
    BlendMode, CapabilityContext, CapabilityRequirement, CapabilityRequirementId, CapabilityStatus,
    DashPattern, DashPatternBuilder, DeviceColor, FillRule, GlyphOutline, GlyphPainting, GlyphRun,
    GlyphUse, GraphicsCapability, GraphicsCommand, GraphicsCommandRecord, GraphicsResource,
    GraphicsResourceEntry, GraphicsResourceId, GraphicsResourceSource, GraphicsScene,
    GraphicsSceneStats, ImageColorSpace, ImageResource, LineCap, LineJoin, LineStyle, Paint,
    PathResource, PathResourceBuilder, PathSegment, PositionedGlyph, SceneBounds, ScenePoint,
    SceneUnit,
};
pub use graphics_builder::{GraphicsSceneBuilder, GraphicsSceneLimitConfig, GraphicsSceneLimits};
pub use limits::{SceneLimitConfig, SceneLimits};
pub use model::{
    CapabilityDecision, CommandSource, FeatureReport, PageGeometry, PageRotation, ResourceId,
    Scene, SceneBinding, SceneCanonicalObserver, SceneCommand, SceneCommandKind, SceneFeature,
    SceneName, SceneRect, SceneResource, SceneResourceKind, SceneStats, SceneVersion,
};
pub use scalar::{Matrix, SceneScalar};
