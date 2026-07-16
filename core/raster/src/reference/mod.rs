//! Bounded Reference pixel production for immutable Scene values.

mod error;
mod limits;
mod model;
mod render;

pub use error::{
    ReferenceRenderError, ReferenceRenderErrorCategory, ReferenceRenderErrorCode,
    ReferenceRenderLimit, ReferenceRenderLimitKind, ReferenceRenderRecoverability,
    ReferenceRenderUnsupported, ReferenceRenderUnsupportedKind,
};
pub use limits::{ReferenceRasterLimitConfig, ReferenceRasterLimits};
pub use model::{
    AlphaMode, CanonicalPixelBuffer, DevicePixelSize, PixelFormat, PixelOrigin,
    ReferenceOutputProfile, ReferencePixelBufferVersion, ReferenceRenderConfig,
    ReferenceRenderStats,
};
pub use render::{
    ReferenceRasterCancellation, ReferenceRenderJob, ReferenceRenderPhase, ReferenceRenderPoll,
};
