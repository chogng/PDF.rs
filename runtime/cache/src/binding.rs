use pdf_rs_bytes::SourceSnapshot;
use pdf_rs_document::{AttestedRevisionIndex, ReferenceChainLimits, RevisionId};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};

/// Opaque runtime-issued identity for one document session.
///
/// The runtime owner must not reuse an identity within its worker epoch. This
/// primitive retains the identity in every store binding but does not allocate
/// or recycle session handles.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReadyStoreSessionId(u64);

impl ReadyStoreSessionId {
    /// Wraps one runtime-issued opaque session identity.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric identity for protocol and trace adaptation.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Runtime-selected namespace for the session cache policy and key schema.
///
/// Changing the meaning of a complete cache key or admission policy requires a
/// new epoch. The epoch names the store namespace; it is not proof that a
/// [`pdf_rs_document::ResolvedReference`] was produced by a particular resolver
/// implementation. Producing parser and resolution profiles are checked
/// separately.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReadyStoreEpoch(u32);

impl ReadyStoreEpoch {
    /// Creates an opaque cache-policy/schema epoch.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric epoch for protocol or trace adaptation.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Complete session-level binding shared by every key in one Ready store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyStoreBinding {
    session_id: ReadyStoreSessionId,
    snapshot: SourceSnapshot,
    revision_id: RevisionId,
    revision_startxref: u64,
    object_limits: ObjectLimits,
    syntax_limits: SyntaxLimits,
    epoch: ReadyStoreEpoch,
}

impl ReadyStoreBinding {
    /// Captures the immutable attested revision and parser profiles for one session store.
    pub const fn for_index(
        index: &AttestedRevisionIndex,
        session_id: ReadyStoreSessionId,
        epoch: ReadyStoreEpoch,
    ) -> Self {
        Self {
            session_id,
            snapshot: index.snapshot(),
            revision_id: index.revision_id(),
            revision_startxref: index.startxref(),
            object_limits: index.object_limits(),
            syntax_limits: index.syntax_limits(),
            epoch,
        }
    }

    /// Returns the opaque runtime-issued document-session identity.
    pub const fn session_id(self) -> ReadyStoreSessionId {
        self.session_id
    }

    /// Returns the complete immutable source snapshot.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the caller-assigned attested revision identity.
    pub const fn revision_id(self) -> RevisionId {
        self.revision_id
    }

    /// Returns the traditional xref offset anchoring the attested revision.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns the validated indirect-object framing profile.
    pub const fn object_limits(self) -> ObjectLimits {
        self.object_limits
    }

    /// Returns the validated direct-syntax profile.
    pub const fn syntax_limits(self) -> SyntaxLimits {
        self.syntax_limits
    }

    /// Returns the runtime cache-policy/schema epoch.
    pub const fn epoch(self) -> ReadyStoreEpoch {
        self.epoch
    }
}

/// Complete lookup and admission key for one resolved Ready value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyStoreKey {
    binding: ReadyStoreBinding,
    root: ObjectRef,
    resolution_limits: ReferenceChainLimits,
}

impl ReadyStoreKey {
    /// Creates a key from its complete session binding, exact root, and cold-path limits.
    pub const fn new(
        binding: ReadyStoreBinding,
        root: ObjectRef,
        resolution_limits: ReferenceChainLimits,
    ) -> Self {
        Self {
            binding,
            root,
            resolution_limits,
        }
    }

    /// Returns the complete session-level binding.
    pub const fn binding(self) -> ReadyStoreBinding {
        self.binding
    }

    /// Returns the exact requested root identity.
    pub const fn root(self) -> ObjectRef {
        self.root
    }

    /// Returns the complete validated cold-path resolution profile.
    pub const fn resolution_limits(self) -> ReferenceChainLimits {
        self.resolution_limits
    }
}
