#[cfg(unix)]
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{DesktopIpcError, DesktopIpcErrorCode, error::error};

static NEXT_LAUNCH_ID: AtomicU64 = AtomicU64::new(1);

/// Nonzero identity for one Host-created child-process launch.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DesktopLaunchId(u64);

impl DesktopLaunchId {
    /// Returns the never-reused-in-process launch number.
    pub const fn value(self) -> u64 {
        self.0
    }
    pub(crate) const fn from_bootstrap(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }
}

/// Per-launch authentication material retained only by Host and its spawned child.
#[derive(Clone, Eq, PartialEq)]
pub struct DesktopLaunchAuth {
    launch: DesktopLaunchId,
    token: [u8; 32],
}

impl DesktopLaunchAuth {
    /// Creates a fresh launch identity and reads an unpredictable Unix token.
    pub fn new() -> Result<Self, DesktopIpcError> {
        let launch = next_launch_id()?;
        let mut token = [0_u8; 32];
        #[cfg(unix)]
        {
            std::fs::File::open("/dev/urandom")
                .and_then(|mut random| random.read_exact(&mut token))
                .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        }
        #[cfg(not(unix))]
        {
            return Err(error(DesktopIpcErrorCode::InvalidConfiguration));
        }
        if token.iter().all(|byte| *byte == 0) {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        Ok(Self {
            launch: DesktopLaunchId(launch),
            token,
        })
    }

    /// Returns this launch identity.
    pub const fn launch(&self) -> DesktopLaunchId {
        self.launch
    }

    /// Compares a received token without exposing it through Debug or Display.
    pub(crate) fn matches(&self, candidate: &[u8; 32]) -> bool {
        let mut difference = 0_u8;
        for (left, right) in self.token.iter().zip(candidate) {
            difference |= left ^ right;
        }
        difference == 0
    }

    /// Encodes the opaque token only for inherited child launch configuration.
    pub(crate) const fn token(&self) -> &[u8; 32] {
        &self.token
    }

    pub(crate) fn from_bootstrap(
        launch: DesktopLaunchId,
        token: [u8; 32],
    ) -> Result<Self, DesktopIpcError> {
        if launch.value() == 0 || token.iter().all(|byte| *byte == 0) {
            return Err(error(DesktopIpcErrorCode::Authentication));
        }
        Ok(Self { launch, token })
    }
}

impl core::fmt::Debug for DesktopLaunchAuth {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("DesktopLaunchAuth")
            .field("launch", &self.launch)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

fn next_launch_id() -> Result<u64, DesktopIpcError> {
    let mut current = NEXT_LAUNCH_ID.load(Ordering::Acquire);
    loop {
        if current == 0 || current == u64::MAX {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        let next = current
            .checked_add(1)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        match NEXT_LAUNCH_ID.compare_exchange_weak(
            current,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Ok(current),
            Err(observed) => current = observed,
        }
    }
}
