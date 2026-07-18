use std::fmt;

/// Stable policy failure code.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PolicyErrorCode {
    /// A caller supplied an invalid limit profile.
    InvalidLimits,
    /// Checked arithmetic could not represent a required value.
    NumericOverflow,
    /// A bounded product-policy dimension was exceeded.
    ResourceLimit,
    /// Cooperative cancellation was observed.
    Cancelled,
    /// Scene canonicalization failed before policy evaluation.
    SceneCanonicalization,
    /// A fallible bounded allocation failed.
    Allocation,
    /// A RenderConfig field combination is invalid.
    InvalidRenderConfig,
    /// A render-plan request is invalid or noncanonical.
    InvalidRenderRequest,
    /// A decision or Scene identity does not match the planning subject.
    IdentityMismatch,
    /// Product document revisions are nonzero and zero was supplied.
    InvalidDocumentRevision,
}

impl PolicyErrorCode {
    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        match self {
            Self::InvalidLimits => "RPE-POLICY-0001",
            Self::NumericOverflow => "RPE-POLICY-0002",
            Self::ResourceLimit => "RPE-POLICY-0003",
            Self::Cancelled => "RPE-POLICY-0004",
            Self::SceneCanonicalization => "RPE-POLICY-0005",
            Self::Allocation => "RPE-POLICY-0006",
            Self::InvalidRenderConfig => "RPE-POLICY-0007",
            Self::InvalidRenderRequest => "RPE-POLICY-0008",
            Self::IdentityMismatch => "RPE-POLICY-0009",
            Self::InvalidDocumentRevision => "RPE-POLICY-0010",
        }
    }
}

/// Stable broad policy failure category.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PolicyErrorCategory {
    /// Invalid caller configuration or cross-value invariant.
    InvalidInput,
    /// A deterministic work or retention bound was exceeded.
    Resource,
    /// Cooperative cancellation was observed.
    Cancelled,
    /// A lower immutable Scene could not provide its canonical identity.
    Scene,
    /// An internal allocation or arithmetic operation failed.
    Internal,
}

/// Stable recovery guidance for a policy failure.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PolicyRecoverability {
    /// The same input and limits will deterministically fail again.
    Permanent,
    /// A caller may retry with an explicitly larger admitted product budget.
    RetryWithBudget,
    /// A newer viewport or request may retry after cancellation.
    RetryNewRequest,
    /// The failure indicates an internal implementation fault.
    InternalFault,
}

/// Independently bounded policy dimension.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PolicyLimitKind {
    /// Scene capability requirement nodes.
    Requirements,
    /// Aggregate dependency edges.
    Dependencies,
    /// Capability-specific parameters evaluated.
    Parameters,
    /// Missing requirements retained in a decision.
    MissingRetained,
    /// Decision contributors retained.
    ContributorsRetained,
    /// Sensitive structured locations retained.
    LocationsRetained,
    /// Tiles in one immutable RenderPlan.
    Tiles,
    /// Output dimensions in device pixels.
    OutputDimension,
    /// Output pixel count.
    OutputPixels,
    /// Resumable work units admitted by one policy-job poll.
    PollWorkUnits,
    /// Bytes retained by one owned pollable policy job.
    JobRetainedBytes,
    /// Bytes admitted to the explicitly atomic Scene canonicalization phase.
    AtomicCanonicalBytes,
}

/// Exact checked resource-limit evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyLimit {
    kind: PolicyLimitKind,
    maximum: u64,
    current: u64,
    attempted: u64,
}

impl PolicyLimit {
    pub(crate) const fn new(
        kind: PolicyLimitKind,
        maximum: u64,
        current: u64,
        attempted: u64,
    ) -> Self {
        Self {
            kind,
            maximum,
            current,
            attempted,
        }
    }

    /// Returns the bounded dimension.
    pub const fn kind(self) -> PolicyLimitKind {
        self.kind
    }

    /// Returns the admitted maximum.
    pub const fn maximum(self) -> u64 {
        self.maximum
    }

    /// Returns the value admitted before this operation.
    pub const fn current(self) -> u64 {
        self.current
    }

    /// Returns the additional attempted amount.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Structured product-policy failure, with content and source values redacted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyError {
    code: PolicyErrorCode,
    category: PolicyErrorCategory,
    recoverability: PolicyRecoverability,
    limit: Option<PolicyLimit>,
}

impl PolicyError {
    pub(crate) const fn invalid_limits() -> Self {
        Self::new(
            PolicyErrorCode::InvalidLimits,
            PolicyErrorCategory::InvalidInput,
            PolicyRecoverability::Permanent,
        )
    }

    pub(crate) const fn numeric_overflow() -> Self {
        Self::new(
            PolicyErrorCode::NumericOverflow,
            PolicyErrorCategory::Internal,
            PolicyRecoverability::InternalFault,
        )
    }

    pub(crate) const fn cancelled() -> Self {
        Self::new(
            PolicyErrorCode::Cancelled,
            PolicyErrorCategory::Cancelled,
            PolicyRecoverability::RetryNewRequest,
        )
    }

    pub(crate) const fn scene_canonicalization() -> Self {
        Self::new(
            PolicyErrorCode::SceneCanonicalization,
            PolicyErrorCategory::Scene,
            PolicyRecoverability::Permanent,
        )
    }

    pub(crate) const fn allocation() -> Self {
        Self::new(
            PolicyErrorCode::Allocation,
            PolicyErrorCategory::Internal,
            PolicyRecoverability::InternalFault,
        )
    }

    pub(crate) const fn invalid_render_config() -> Self {
        Self::new(
            PolicyErrorCode::InvalidRenderConfig,
            PolicyErrorCategory::InvalidInput,
            PolicyRecoverability::Permanent,
        )
    }

    pub(crate) const fn invalid_render_request() -> Self {
        Self::new(
            PolicyErrorCode::InvalidRenderRequest,
            PolicyErrorCategory::InvalidInput,
            PolicyRecoverability::Permanent,
        )
    }

    pub(crate) const fn identity_mismatch() -> Self {
        Self::new(
            PolicyErrorCode::IdentityMismatch,
            PolicyErrorCategory::InvalidInput,
            PolicyRecoverability::Permanent,
        )
    }

    pub(crate) const fn invalid_document_revision() -> Self {
        Self::new(
            PolicyErrorCode::InvalidDocumentRevision,
            PolicyErrorCategory::InvalidInput,
            PolicyRecoverability::Permanent,
        )
    }

    pub(crate) const fn resource(
        kind: PolicyLimitKind,
        maximum: u64,
        current: u64,
        attempted: u64,
    ) -> Self {
        Self {
            code: PolicyErrorCode::ResourceLimit,
            category: PolicyErrorCategory::Resource,
            recoverability: PolicyRecoverability::RetryWithBudget,
            limit: Some(PolicyLimit::new(kind, maximum, current, attempted)),
        }
    }

    const fn new(
        code: PolicyErrorCode,
        category: PolicyErrorCategory,
        recoverability: PolicyRecoverability,
    ) -> Self {
        Self {
            code,
            category,
            recoverability,
            limit: None,
        }
    }

    /// Returns the stable error code.
    pub const fn code(self) -> PolicyErrorCode {
        self.code
    }

    /// Returns the broad category.
    pub const fn category(self) -> PolicyErrorCategory {
        self.category
    }

    /// Returns stable recovery guidance.
    pub const fn recoverability(self) -> PolicyRecoverability {
        self.recoverability
    }

    /// Returns exact limit evidence when this is a resource failure.
    pub const fn limit(self) -> Option<PolicyLimit> {
        self.limit
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.code.diagnostic_id()
    }
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "product policy failure {}", self.diagnostic_id())
    }
}

impl std::error::Error for PolicyError {}
