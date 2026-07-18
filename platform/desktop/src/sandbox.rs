//! Fail-closed product sandbox selection for the desktop worker.
//!
//! The authenticated process transport can be exercised without claiming an
//! operating-system sandbox. A product launch is a separate boundary: it stays
//! unavailable until a signed macOS host/helper package and real denial probes
//! can provide an attestation owned outside this crate.

use crate::{DesktopIpcError, DesktopIpcErrorCode, error::error};

/// Immutable identifier for the selected M4 desktop sandbox target.
pub const DESKTOP_PRODUCT_SANDBOX_TARGET_ID: &str = "m4.macos-app-sandbox-inherited-worker.v1";

/// Current build-time reason a product sandbox gate cannot be acquired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopProductSandboxAvailability {
    /// macOS is selected, but signed parent/helper packaging proof is absent.
    PackagingProofRequired,
    /// This operating-system target has no selected M4 product sandbox.
    UnsupportedTarget,
}

/// Reports the selected target without treating target detection as isolation proof.
pub const fn desktop_product_sandbox_availability() -> DesktopProductSandboxAvailability {
    if cfg!(target_os = "macos") {
        DesktopProductSandboxAvailability::PackagingProofRequired
    } else {
        DesktopProductSandboxAvailability::UnsupportedTarget
    }
}

/// Unforgeable crate-owned proof required by the product supervisor path.
///
/// There is intentionally no public constructor. A later packaging boundary
/// may acquire this only from verified code-signing, entitlement, parent-app,
/// and live filesystem/network denial evidence.
#[derive(Debug)]
pub(crate) struct DesktopProductSandboxGate {
    _private: (),
}

impl DesktopProductSandboxGate {
    /// Fails closed until the selected signed macOS package owns attestation.
    pub(crate) fn acquire() -> Result<Self, DesktopIpcError> {
        let _availability = desktop_product_sandbox_availability();
        Err(error(DesktopIpcErrorCode::IsolationUnavailable))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DESKTOP_PRODUCT_SANDBOX_TARGET_ID, DesktopProductSandboxAvailability,
        DesktopProductSandboxGate, desktop_product_sandbox_availability,
    };
    use crate::DesktopIpcErrorCode;

    #[test]
    fn product_gate_is_not_constructible_from_caller_evidence() {
        assert_eq!(
            DesktopProductSandboxGate::acquire()
                .expect_err("unsigned workspace must not claim isolation")
                .code(),
            DesktopIpcErrorCode::IsolationUnavailable
        );
        assert_eq!(
            DESKTOP_PRODUCT_SANDBOX_TARGET_ID,
            "m4.macos-app-sandbox-inherited-worker.v1"
        );
    }

    #[test]
    fn target_availability_is_explicit() {
        let expected = if cfg!(target_os = "macos") {
            DesktopProductSandboxAvailability::PackagingProofRequired
        } else {
            DesktopProductSandboxAvailability::UnsupportedTarget
        };
        assert_eq!(desktop_product_sandbox_availability(), expected);
    }
}
