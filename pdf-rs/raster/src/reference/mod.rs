//! Bounded Reference pixel production for immutable Scene values.

mod color;
mod coverage;
mod error;
mod geometry;
mod glyph;
mod image;
mod limits;
mod model;
mod render;
mod stroke;
mod surface;

pub use color::{
    NormalizedQ16, PremultipliedRgbaQ16, ReferenceBlendMode, ReferenceColorProfile,
    ReferenceDeviceColor, ReferenceSrgbQ16,
};
pub use error::{
    ReferenceGraphicsCommandKind, ReferenceRenderError, ReferenceRenderErrorCategory,
    ReferenceRenderErrorCode, ReferenceRenderLimit, ReferenceRenderLimitKind,
    ReferenceRenderRecoverability, ReferenceRenderUnsupported, ReferenceRenderUnsupportedKind,
};
pub use limits::{ReferenceRasterLimitConfig, ReferenceRasterLimits};
pub use model::{
    AlphaMode, CanonicalPixelBuffer, DevicePixelSize, PixelFormat, PixelOrigin,
    ReferenceCapabilityDecision, ReferenceOutputProfile, ReferencePixelBufferVersion,
    ReferenceRasterAlgorithm, ReferenceRenderConfig, ReferenceRenderIdentity, ReferenceRenderStats,
};
pub use render::{
    ReferenceRasterCancellation, ReferenceRenderJob, ReferenceRenderPhase, ReferenceRenderPoll,
};
