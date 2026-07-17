use crate::RetireReason;

/// Exact current live-resource evidence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SurfaceResourceReport {
    active_sessions: usize,
    private_surfaces: usize,
    published_surfaces: usize,
    imported_surfaces: usize,
    handles: usize,
    retained_bytes: u64,
}

impl SurfaceResourceReport {
    pub(crate) const fn new(
        active_sessions: usize,
        private_surfaces: usize,
        published_surfaces: usize,
        imported_surfaces: usize,
        handles: usize,
        retained_bytes: u64,
    ) -> Self {
        Self {
            active_sessions,
            private_surfaces,
            published_surfaces,
            imported_surfaces,
            handles,
            retained_bytes,
        }
    }

    /// Returns the number of active Sessions.
    pub const fn active_sessions(self) -> usize {
        self.active_sessions
    }

    /// Returns producer-private mutable Surfaces.
    pub const fn private_surfaces(self) -> usize {
        self.private_surfaces
    }

    /// Returns published but not imported Surfaces.
    pub const fn published_surfaces(self) -> usize {
        self.published_surfaces
    }

    /// Returns imported immutable Surfaces.
    pub const fn imported_surfaces(self) -> usize {
        self.imported_surfaces
    }

    /// Returns live fake handles.
    pub const fn handles(self) -> usize {
        self.handles
    }

    /// Returns retained live region bytes.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    /// Reports whether all Surface and handle resources are gone.
    pub const fn has_zero_surface_resources(self) -> bool {
        self.private_surfaces == 0
            && self.published_surfaces == 0
            && self.imported_surfaces == 0
            && self.handles == 0
            && self.retained_bytes == 0
    }
}

/// Exact resources removed by one lifecycle operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SurfaceReleaseReport {
    private_surfaces: usize,
    published_surfaces: usize,
    imported_surfaces: usize,
    handles: usize,
    released_bytes: u64,
}

impl SurfaceReleaseReport {
    pub(crate) const fn private(region_bytes: u64) -> Self {
        Self {
            private_surfaces: 1,
            published_surfaces: 0,
            imported_surfaces: 0,
            handles: 1,
            released_bytes: region_bytes,
        }
    }

    pub(crate) fn published(region_bytes: u64, imported: bool) -> Self {
        Self {
            private_surfaces: 0,
            published_surfaces: usize::from(!imported),
            imported_surfaces: usize::from(imported),
            handles: 1,
            released_bytes: region_bytes,
        }
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.private_surfaces += other.private_surfaces;
        self.published_surfaces += other.published_surfaces;
        self.imported_surfaces += other.imported_surfaces;
        self.handles += other.handles;
        self.released_bytes = self
            .released_bytes
            .checked_add(other.released_bytes)
            .expect("released bytes are bounded by validated aggregate capacity");
    }

    /// Returns removed private Surfaces.
    pub const fn private_surfaces(self) -> usize {
        self.private_surfaces
    }

    /// Returns removed published Surfaces.
    pub const fn published_surfaces(self) -> usize {
        self.published_surfaces
    }

    /// Returns removed imported Surfaces.
    pub const fn imported_surfaces(self) -> usize {
        self.imported_surfaces
    }

    /// Returns removed fake handles.
    pub const fn handles(self) -> usize {
        self.handles
    }

    /// Returns released region bytes.
    pub const fn released_bytes(self) -> u64 {
        self.released_bytes
    }

    /// Reports whether this operation removed no current resources.
    pub const fn is_zero(self) -> bool {
        self.private_surfaces == 0
            && self.published_surfaces == 0
            && self.imported_surfaces == 0
            && self.handles == 0
            && self.released_bytes == 0
    }
}

/// Idempotent explicit release outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseOutcome {
    /// This call removed the live Surface.
    Released(SurfaceReleaseReport),
    /// The exact Surface and lease were already terminal for this reason.
    AlreadyRetired(RetireReason),
}

/// Report returned by close, generation replacement, lease reclaim, or restart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleReport {
    released: SurfaceReleaseReport,
    current: SurfaceResourceReport,
}

impl LifecycleReport {
    pub(crate) const fn new(
        released: SurfaceReleaseReport,
        current: SurfaceResourceReport,
    ) -> Self {
        Self { released, current }
    }

    /// Returns resources removed by this operation.
    pub const fn released(self) -> SurfaceReleaseReport {
        self.released
    }

    /// Returns exact resources remaining afterward.
    pub const fn current(self) -> SurfaceResourceReport {
        self.current
    }
}
