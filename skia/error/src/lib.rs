use std::fmt;

/// Stable machine-readable drawing failure code.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SkiaErrorCode {
    /// A coordinate or intermediate calculation overflowed.
    NumericOverflow,
    /// A geometry value is invalid.
    InvalidGeometry,
    /// A command refers to a missing display-list resource.
    InvalidResource,
    /// A bitmap's dimensions and pixel buffer disagree.
    InvalidImage,
    /// A path operation violates contour ordering.
    InvalidPath,
    /// A configured resource ceiling is invalid.
    InvalidLimits,
    /// A resource ceiling was reached.
    ResourceLimit,
    /// A fallible allocation failed.
    AllocationFailed,
    /// A stack restore was requested without a matching save.
    RestoreUnderflow,
    /// The requested operation needs a not-yet-implemented transform mode.
    UnsupportedTransform,
    /// A glyph outline provider could not resolve requested text data.
    TextResolverFailed,
}

/// Source-redacted graphics error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SkiaError {
    code: SkiaErrorCode,
}

impl SkiaError {
    /// Creates one stable drawing failure.
    pub const fn new(code: SkiaErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure code.
    pub const fn code(self) -> SkiaErrorCode {
        self.code
    }
}

impl fmt::Display for SkiaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self.code)
    }
}

impl std::error::Error for SkiaError {}
