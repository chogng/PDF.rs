//! Nonzero opaque identities used by the scheduler.

use core::fmt;
use core::num::NonZeroU64;

macro_rules! nonzero_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// Creates an identity, returning `None` for the reserved zero value.
            #[must_use]
            pub const fn new(value: u64) -> Option<Self> {
                match NonZeroU64::new(value) {
                    Some(value) => Some(Self(value)),
                    None => None,
                }
            }

            /// Returns the nonzero integer representation.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(formatter)
            }
        }
    };
}

nonzero_id!(
    SessionId,
    "A session identity which is never reused within one scheduler instance."
);
nonzero_id!(
    WorkId,
    "A work identity which is never reused within one scheduler instance."
);
nonzero_id!(
    ResourceId,
    "An opaque completed-resource identity transferred to terminal arbitration."
);
nonzero_id!(
    Generation,
    "A monotonically increasing nonzero viewport generation."
);
