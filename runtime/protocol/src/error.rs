use std::error::Error;
use std::fmt;

/// Stable machine-readable protocol failure.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ProtocolErrorCode {
    /// A configured protocol limit is zero, inconsistent, or above a hard ceiling.
    InvalidLimits,
    /// A desktop frame is shorter than the fixed header.
    TruncatedHeader,
    /// A desktop frame exceeds the configured global frame ceiling.
    FrameTooLarge,
    /// The declared payload length does not equal the bytes following the header.
    FrameLengthMismatch,
    /// The peer uses an incompatible protocol major.
    UnsupportedMajor,
    /// The peer uses a protocol minor outside the accepted compatibility window.
    UnsupportedMinor,
    /// The generated registry has no descriptor for the message identifier.
    UnknownMessage,
    /// Header flags contain a bit not accepted by the generated message descriptor.
    InvalidFlags,
    /// A payload exceeds the global payload limit.
    PayloadTooLarge,
    /// A payload exceeds its generated message-specific limit.
    MessagePayloadTooLarge,
    /// The observed out-of-band transfer count violates the message descriptor.
    InvalidTransferCount,
    /// A receive-direction sequence duplicates or precedes an accepted sequence.
    NonMonotonicSequence,
    /// Correlation fields do not have the shape required by the message descriptor.
    InvalidCorrelation,
    /// Endpoint roles cannot form one Host-to-Engine connection.
    InvalidEndpointRole,
    /// A schema mismatch cannot be accepted by negotiated compatibility.
    IncompatibleSchema,
    /// A peer advertises a zero or unusable transport bound.
    InvalidEndpointLimits,
    /// Surface identity or owner does not match the receiving session and worker epoch.
    InvalidSurfaceOwner,
    /// Surface generation or renderer epoch is stale or zero where the contract requires identity.
    InvalidSurfaceEpoch,
    /// Surface dimensions, stride, or byte count are invalid.
    InvalidSurfaceLayout,
    /// Surface pixel format and alpha representation form an unsupported wire combination.
    InvalidSurfaceFormat,
    /// A Surface transport refers to an absent, duplicate, or wrong transfer slot.
    InvalidSurfaceSlot,
    /// A shared-memory offset or range overflows or exceeds the declared region.
    InvalidSurfaceRange,
    /// Checked protocol arithmetic overflowed.
    NumericOverflow,
    /// Generated schema metadata violates an internal invariant.
    InvalidGeneratedDescriptor,
    /// A mandatory endpoint-capability bit is unknown to this generated protocol.
    UnknownMandatoryCapability,
    /// The peer does not advertise a required known endpoint capability.
    MissingMandatoryCapability,
    /// An endpoint requires a capability that it does not itself advertise as supported.
    InvalidEndpointCapabilities,
    /// Viewport geometry, scale, revision, generation, or canonical ordering is invalid.
    InvalidViewport,
    /// A data range is empty or disagrees with its declared byte length.
    InvalidDataRange,
    /// A data segment refers to a missing, duplicate, or wrong-length transfer slot.
    InvalidTransferBinding,
    /// Surface render-plan, scene, decision, configuration, or backend identity is stale.
    InvalidSurfacePlan,
    /// Surface placement does not match the accepted render plan.
    InvalidSurfaceRegion,
}

impl ProtocolErrorCode {
    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        match self {
            Self::InvalidLimits => "RPE-PROTOCOL-0001",
            Self::TruncatedHeader => "RPE-PROTOCOL-0002",
            Self::FrameTooLarge => "RPE-PROTOCOL-0003",
            Self::FrameLengthMismatch => "RPE-PROTOCOL-0004",
            Self::UnsupportedMajor => "RPE-PROTOCOL-0005",
            Self::UnsupportedMinor => "RPE-PROTOCOL-0006",
            Self::UnknownMessage => "RPE-PROTOCOL-0007",
            Self::InvalidFlags => "RPE-PROTOCOL-0008",
            Self::PayloadTooLarge => "RPE-PROTOCOL-0009",
            Self::MessagePayloadTooLarge => "RPE-PROTOCOL-0010",
            Self::InvalidTransferCount => "RPE-PROTOCOL-0011",
            Self::NonMonotonicSequence => "RPE-PROTOCOL-0012",
            Self::InvalidCorrelation => "RPE-PROTOCOL-0013",
            Self::InvalidEndpointRole => "RPE-PROTOCOL-0014",
            Self::IncompatibleSchema => "RPE-PROTOCOL-0015",
            Self::InvalidEndpointLimits => "RPE-PROTOCOL-0016",
            Self::InvalidSurfaceOwner => "RPE-PROTOCOL-0017",
            Self::InvalidSurfaceEpoch => "RPE-PROTOCOL-0018",
            Self::InvalidSurfaceLayout => "RPE-PROTOCOL-0019",
            Self::InvalidSurfaceFormat => "RPE-PROTOCOL-0020",
            Self::InvalidSurfaceSlot => "RPE-PROTOCOL-0021",
            Self::InvalidSurfaceRange => "RPE-PROTOCOL-0022",
            Self::NumericOverflow => "RPE-PROTOCOL-0023",
            Self::InvalidGeneratedDescriptor => "RPE-PROTOCOL-0024",
            Self::UnknownMandatoryCapability => "RPE-PROTOCOL-0025",
            Self::MissingMandatoryCapability => "RPE-PROTOCOL-0026",
            Self::InvalidSurfacePlan => "RPE-PROTOCOL-0027",
            Self::InvalidSurfaceRegion => "RPE-PROTOCOL-0028",
            Self::InvalidEndpointCapabilities => "RPE-PROTOCOL-0029",
            Self::InvalidViewport => "RPE-PROTOCOL-0030",
            Self::InvalidDataRange => "RPE-PROTOCOL-0031",
            Self::InvalidTransferBinding => "RPE-PROTOCOL-0032",
        }
    }
}

/// Stable protocol failure category.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ProtocolErrorCategory {
    /// Invalid local configuration or generated metadata.
    Configuration,
    /// Invalid fixed frame structure or length.
    Framing,
    /// Protocol-version or schema incompatibility.
    Compatibility,
    /// Unknown message or invalid message-specific header policy.
    Message,
    /// Receive-direction replay or ordering failure.
    Sequence,
    /// Invalid worker, session, request, or generation correlation.
    Correlation,
    /// Invalid transfer count or slot.
    Transfer,
    /// Invalid Surface identity, format, layout, range, or epoch.
    Surface,
    /// Checked numeric overflow.
    Numeric,
}

/// Stable response policy for a protocol failure.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ProtocolRecoverability {
    /// Correct trusted local configuration before starting the endpoint.
    CorrectConfiguration,
    /// Reject the current untrusted frame without exposing its payload.
    RejectFrame,
    /// Reject or terminate the current protocol connection.
    RejectConnection,
    /// Reject the Surface while allowing policy to decide whether the session can continue.
    RejectSurface,
}

/// Content- and handle-redacted structured protocol error.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProtocolError {
    code: ProtocolErrorCode,
    category: ProtocolErrorCategory,
    recoverability: ProtocolRecoverability,
}

impl ProtocolError {
    pub(crate) const fn for_code(code: ProtocolErrorCode) -> Self {
        let (category, recoverability) = match code {
            ProtocolErrorCode::InvalidLimits | ProtocolErrorCode::InvalidGeneratedDescriptor => (
                ProtocolErrorCategory::Configuration,
                ProtocolRecoverability::CorrectConfiguration,
            ),
            ProtocolErrorCode::TruncatedHeader
            | ProtocolErrorCode::FrameTooLarge
            | ProtocolErrorCode::FrameLengthMismatch
            | ProtocolErrorCode::PayloadTooLarge
            | ProtocolErrorCode::MessagePayloadTooLarge => (
                ProtocolErrorCategory::Framing,
                ProtocolRecoverability::RejectFrame,
            ),
            ProtocolErrorCode::UnsupportedMajor
            | ProtocolErrorCode::UnsupportedMinor
            | ProtocolErrorCode::InvalidEndpointRole
            | ProtocolErrorCode::IncompatibleSchema
            | ProtocolErrorCode::InvalidEndpointLimits
            | ProtocolErrorCode::UnknownMandatoryCapability
            | ProtocolErrorCode::MissingMandatoryCapability
            | ProtocolErrorCode::InvalidEndpointCapabilities => (
                ProtocolErrorCategory::Compatibility,
                ProtocolRecoverability::RejectConnection,
            ),
            ProtocolErrorCode::UnknownMessage
            | ProtocolErrorCode::InvalidFlags
            | ProtocolErrorCode::InvalidViewport
            | ProtocolErrorCode::InvalidDataRange => (
                ProtocolErrorCategory::Message,
                ProtocolRecoverability::RejectFrame,
            ),
            ProtocolErrorCode::NonMonotonicSequence => (
                ProtocolErrorCategory::Sequence,
                ProtocolRecoverability::RejectConnection,
            ),
            ProtocolErrorCode::InvalidCorrelation => (
                ProtocolErrorCategory::Correlation,
                ProtocolRecoverability::RejectFrame,
            ),
            ProtocolErrorCode::InvalidTransferCount
            | ProtocolErrorCode::InvalidTransferBinding
            | ProtocolErrorCode::InvalidSurfaceSlot => (
                ProtocolErrorCategory::Transfer,
                ProtocolRecoverability::RejectFrame,
            ),
            ProtocolErrorCode::InvalidSurfaceOwner
            | ProtocolErrorCode::InvalidSurfaceEpoch
            | ProtocolErrorCode::InvalidSurfaceLayout
            | ProtocolErrorCode::InvalidSurfaceFormat
            | ProtocolErrorCode::InvalidSurfaceRange
            | ProtocolErrorCode::InvalidSurfacePlan
            | ProtocolErrorCode::InvalidSurfaceRegion => (
                ProtocolErrorCategory::Surface,
                ProtocolRecoverability::RejectSurface,
            ),
            ProtocolErrorCode::NumericOverflow => (
                ProtocolErrorCategory::Numeric,
                ProtocolRecoverability::RejectFrame,
            ),
        };
        Self {
            code,
            category,
            recoverability,
        }
    }

    /// Returns the stable machine-readable failure code.
    pub const fn code(self) -> ProtocolErrorCode {
        self.code
    }

    /// Returns the stable failure category.
    pub const fn category(self) -> ProtocolErrorCategory {
        self.category
    }

    /// Returns the stable response policy.
    pub const fn recoverability(self) -> ProtocolRecoverability {
        self.recoverability
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.code.diagnostic_id()
    }
}

impl fmt::Debug for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProtocolError")
            .field("code", &self.code)
            .field("category", &self.category)
            .field("recoverability", &self.recoverability)
            .field("diagnostic_id", &self.diagnostic_id())
            .field("payload", &"[REDACTED]")
            .field("platform_handle", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: rejected Native Engine protocol input",
            self.diagnostic_id()
        )
    }
}

impl Error for ProtocolError {}
