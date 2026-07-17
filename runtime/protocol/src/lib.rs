//! Validated, dependency-free Native Engine protocol foundations.
//!
//! Wire types and message descriptors are generated from the canonical Engine schema. Handwritten
//! modules validate untrusted desktop frames, compatibility, correlation, transfer ownership, and
//! Surface bounds before runtime code can observe a payload or platform resource.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod frame;
#[rustfmt::skip]
mod generated;
mod limits;
mod validation;

pub use error::{ProtocolError, ProtocolErrorCategory, ProtocolErrorCode, ProtocolRecoverability};
pub use frame::{
    DESKTOP_FRAME_HEADER_BYTES, DesktopFrameDecoder, FrameMessagePolicy, SequenceTracker,
    ValidatedDesktopFrame,
};
pub use generated::*;
pub use limits::{ProtocolLimitConfig, ProtocolLimits};
pub use validation::{
    CompatibleHandshake, HandshakeCompatibility, ProtocolValidator, SurfacePlanBinding,
    SurfaceRenderIdentity, SurfaceValidationContext, ValidatedSurface,
};
