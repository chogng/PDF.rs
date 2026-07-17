use std::fmt;

macro_rules! digest_identity {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Wraps one complete SHA-256 digest.
            pub const fn new(digest: [u8; 32]) -> Self {
                Self(digest)
            }

            /// Borrows the complete digest.
            pub const fn digest(&self) -> &[u8; 32] {
                &self.0
            }

            /// Returns the complete digest.
            pub const fn into_digest(self) -> [u8; 32] {
                self.0
            }

            /// Reports whether every digest byte is zero.
            pub fn is_zero(self) -> bool {
                self.0.iter().all(|byte| *byte == 0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "([REDACTED])"))
            }
        }
    };
}

digest_identity!(
    SceneHash,
    "Canonical semantic identity of one immutable Scene."
);
digest_identity!(
    GeometryHash,
    "Canonical identity of one page geometry and coordinate-space contract."
);
digest_identity!(
    CapabilityDecisionHash,
    "Canonical identity of one complete bounded product capability decision."
);
digest_identity!(
    RenderConfigHash,
    "Canonical identity of one immutable Native render configuration."
);
digest_identity!(
    TileContentHash,
    "Generation-independent canonical identity of one product tile's pixel content."
);
digest_identity!(
    RenderPlanHash,
    "Canonical identity of one complete generation-bound Native render plan."
);
digest_identity!(
    PlannedTileHash,
    "Canonical identity of one generation-bound tile within a Native render plan."
);

/// Stable identity of one optional-content configuration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OptionalContentIdentity(u64);

impl OptionalContentIdentity {
    /// Creates an optional-content identity.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the stable numeric identity.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Nonzero epoch of one Native renderer instance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RendererEpoch(u32);

impl RendererEpoch {
    /// Creates an epoch, rejecting the reserved zero value.
    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the epoch value.
    pub const fn value(self) -> u32 {
        self.0
    }
}
