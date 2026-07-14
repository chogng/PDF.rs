use std::fmt;

/// Stable, content-independent host identity for a source.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceStableId([u8; 32]);

impl SourceStableId {
    /// Wraps a host-provided stable source identity digest.
    pub const fn new(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// Returns the stable identity digest.
    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Debug for SourceStableId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceStableId([REDACTED])")
    }
}

/// Monotonic host revision of one stable source identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceRevision(u64);

impl SourceRevision {
    /// Creates a source revision value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric revision value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Immutable source identity bound to one revision.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceIdentity {
    stable_id: SourceStableId,
    revision: SourceRevision,
}

impl SourceIdentity {
    /// Creates an immutable source identity.
    pub const fn new(stable_id: SourceStableId, revision: SourceRevision) -> Self {
        Self {
            stable_id,
            revision,
        }
    }

    /// Returns the host-stable identity.
    pub const fn stable_id(self) -> SourceStableId {
        self.stable_id
    }

    /// Returns the bound source revision.
    pub const fn revision(self) -> SourceRevision {
        self.revision
    }
}

impl fmt::Debug for SourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceIdentity")
            .field("stable_id", &self.stable_id)
            .field("revision", &self.revision)
            .finish()
    }
}

/// Validation mechanism used to freeze one immutable source snapshot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SourceValidatorKind {
    /// Digest of a strong HTTP entity tag or equivalent validator.
    StrongEntityTag,
    /// Digest of a complete response frozen by the host as immutable bytes.
    FrozenResponse,
    /// Digest of a stable local-file or host object version descriptor.
    HostVersion,
}

/// Source validator represented only by kind and a non-sensitive digest.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceValidator {
    kind: SourceValidatorKind,
    digest: [u8; 32],
}

impl SourceValidator {
    /// Creates a validator from its mechanism and host-computed digest.
    pub const fn new(kind: SourceValidatorKind, digest: [u8; 32]) -> Self {
        Self { kind, digest }
    }

    /// Returns the validator mechanism.
    pub const fn kind(self) -> SourceValidatorKind {
        self.kind
    }

    /// Returns the host-computed validator digest.
    pub const fn digest(self) -> [u8; 32] {
        self.digest
    }
}

impl fmt::Debug for SourceValidator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceValidator")
            .field("kind", &self.kind)
            .field("digest", &"[REDACTED]")
            .finish()
    }
}

/// Immutable source metadata bound for one document session.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct SourceSnapshot {
    identity: SourceIdentity,
    len: Option<u64>,
    validator: SourceValidator,
}

impl SourceSnapshot {
    /// Creates a source snapshot with an optional known total length.
    pub const fn new(
        identity: SourceIdentity,
        len: Option<u64>,
        validator: SourceValidator,
    ) -> Self {
        Self {
            identity,
            len,
            validator,
        }
    }

    /// Returns the immutable source identity.
    pub const fn identity(self) -> SourceIdentity {
        self.identity
    }

    /// Returns the known source length, when the host supplied one.
    pub const fn len(self) -> Option<u64> {
        self.len
    }

    /// Reports whether the snapshot is known to contain zero bytes.
    pub const fn is_empty(self) -> bool {
        matches!(self.len, Some(0))
    }

    /// Returns the validator bound to this snapshot.
    pub const fn validator(self) -> SourceValidator {
        self.validator
    }
}

impl fmt::Debug for SourceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceSnapshot")
            .field("identity", &self.identity)
            .field("len", &self.len)
            .field("validator", &self.validator)
            .finish()
    }
}
