use std::fmt;

use pdf_rs_document::{
    AcquiredPageContent, MaterializedPage, PagePropertyLookupStats, PagePropertyReference,
};
use pdf_rs_scene::{Matrix, Scene};

use crate::{ContentOperatorSource, ContentScanStats};

/// Observable phase of one sealed Page interpretation job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentVmPhase {
    /// No interpretation attempt has run.
    Pending,
    /// One immutable interpreted Page was published.
    Ready,
    /// A validated feature was classified as unsupported.
    Unsupported,
    /// Scanning, document lookup, Scene construction, or VM execution failed.
    Failed,
}

/// Exact Page-property proof retained for one `BDC` operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedPropertyUse {
    source: ContentOperatorSource,
    property: PagePropertyReference,
}

impl ResolvedPropertyUse {
    pub(crate) const fn new(
        source: ContentOperatorSource,
        property: PagePropertyReference,
    ) -> Self {
        Self { source, property }
    }

    /// Returns exact decoded operator-token provenance and page-global ordinal.
    pub const fn source(self) -> ContentOperatorSource {
        self.source
    }

    /// Returns the fixed-size inherited resource lookup proof.
    pub const fn property(self) -> PagePropertyReference {
        self.property
    }
}

/// Deterministic Content VM work, nesting, and ownership accounting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentVmStats {
    operators: u64,
    fuel: u64,
    max_graphics_state_depth: u32,
    max_compatibility_depth: u32,
    max_marked_content_depth: u32,
    property_uses: u64,
    retained_bytes: u64,
    peak_retained_bytes: u64,
}

impl ContentVmStats {
    #[allow(
        clippy::too_many_arguments,
        reason = "the public accounting snapshot retains each independent VM dimension"
    )]
    pub(crate) const fn new(
        operators: u64,
        fuel: u64,
        max_graphics_state_depth: u32,
        max_compatibility_depth: u32,
        max_marked_content_depth: u32,
        property_uses: u64,
        retained_bytes: u64,
        peak_retained_bytes: u64,
    ) -> Self {
        Self {
            operators,
            fuel,
            max_graphics_state_depth,
            max_compatibility_depth,
            max_marked_content_depth,
            property_uses,
            retained_bytes,
            peak_retained_bytes,
        }
    }

    /// Returns the admitted page-global operator count.
    pub const fn operators(self) -> u64 {
        self.operators
    }

    /// Returns deterministic VM work units.
    pub const fn fuel(self) -> u64 {
        self.fuel
    }

    /// Returns the deepest saved graphics-state nesting reached.
    pub const fn max_graphics_state_depth(self) -> u32 {
        self.max_graphics_state_depth
    }

    /// Returns the deepest compatibility-section nesting reached.
    pub const fn max_compatibility_depth(self) -> u32 {
        self.max_compatibility_depth
    }

    /// Returns the deepest marked-content nesting reached.
    pub const fn max_marked_content_depth(self) -> u32 {
        self.max_marked_content_depth
    }

    /// Returns the marked-content property-reference count.
    pub const fn property_uses(self) -> u64 {
        self.property_uses
    }

    /// Returns allocator-reported capacity retained by the published interpreted result.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    /// Returns peak VM-owned transient and final capacity.
    ///
    /// This includes decoded stream descriptors, the transient scanned program, current-path
    /// capacity, unique active dash-array capacity, graphics-state stack capacity, and retained
    /// property proofs. Acquired Page content and Scene storage remain under their independent
    /// sealed budgets and are not included.
    pub const fn peak_retained_bytes(self) -> u64 {
        self.peak_retained_bytes
    }
}

impl Default for ContentVmStats {
    fn default() -> Self {
        Self::new(0, 0, 0, 0, 0, 0, 0, 0)
    }
}

/// Immutable successful result of one sealed acquired-Page interpretation.
///
/// The value owns the exact proof-bearing acquisition, semantic Scene, and every property lookup
/// proof. It therefore remains valid after the source and cancellation inputs used by `poll`
/// have been dropped.
pub struct InterpretedPage {
    acquired: AcquiredPageContent,
    scene: Scene,
    property_uses: Vec<ResolvedPropertyUse>,
    final_ctm: Matrix,
    scan_stats: ContentScanStats,
    vm_stats: ContentVmStats,
    property_stats: PagePropertyLookupStats,
}

impl InterpretedPage {
    #[allow(
        clippy::too_many_arguments,
        reason = "atomic publication retains each sealed lower result and accounting snapshot"
    )]
    pub(crate) const fn new(
        acquired: AcquiredPageContent,
        scene: Scene,
        property_uses: Vec<ResolvedPropertyUse>,
        final_ctm: Matrix,
        scan_stats: ContentScanStats,
        vm_stats: ContentVmStats,
        property_stats: PagePropertyLookupStats,
    ) -> Self {
        Self {
            acquired,
            scene,
            property_uses,
            final_ctm,
            scan_stats,
            vm_stats,
            property_stats,
        }
    }

    /// Borrows the exact proof-bearing acquired Page content.
    pub const fn acquired_content(&self) -> &AcquiredPageContent {
        &self.acquired
    }

    /// Borrows the materialized Page and inherited resource proof.
    pub const fn page(&self) -> &MaterializedPage {
        self.acquired.page()
    }

    /// Borrows the immutable semantic Scene.
    pub const fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Returns `BDC` property proofs in exact execution order.
    pub fn property_uses(&self) -> &[ResolvedPropertyUse] {
        &self.property_uses
    }

    /// Returns the current transformation matrix after the final operator.
    pub const fn final_ctm(&self) -> Matrix {
        self.final_ctm
    }

    /// Returns the complete lower scanner accounting snapshot.
    pub const fn scan_stats(&self) -> ContentScanStats {
        self.scan_stats
    }

    /// Returns the complete VM accounting snapshot.
    pub const fn vm_stats(&self) -> ContentVmStats {
        self.vm_stats
    }

    /// Returns cumulative inherited property lookup work.
    pub const fn property_stats(&self) -> PagePropertyLookupStats {
        self.property_stats
    }
}

impl fmt::Debug for InterpretedPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InterpretedPage")
            .field("handle", &self.acquired.handle())
            .field("scene_command_count", &self.scene.commands().len())
            .field("scene_resource_count", &self.scene.resources().len())
            .field("property_use_count", &self.property_uses.len())
            .field("scan_stats", &self.scan_stats)
            .field("vm_stats", &self.vm_stats)
            .field("property_stats", &self.property_stats)
            .field("scene", &"[REDACTED]")
            .field("final_ctm", &"[REDACTED]")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::ContentVmStats;

    #[test]
    fn nonzero_stats_report_each_independent_dimension() {
        let stats = ContentVmStats::new(11, 29, 3, 5, 7, 13, 1_024, 4_096);
        assert_eq!(stats.operators(), 11);
        assert_eq!(stats.fuel(), 29);
        assert_eq!(stats.max_graphics_state_depth(), 3);
        assert_eq!(stats.max_compatibility_depth(), 5);
        assert_eq!(stats.max_marked_content_depth(), 7);
        assert_eq!(stats.property_uses(), 13);
        assert_eq!(stats.retained_bytes(), 1_024);
        assert_eq!(stats.peak_retained_bytes(), 4_096);
    }
}
