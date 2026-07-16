use std::fmt;

use pdf_rs_document::{
    AcquiredPageContent, ImageXObjectStats, MaterializedPage, PagePropertyLookupStats,
    PagePropertyReference, PageXObjectLookupStats, PageXObjectReference,
};
use pdf_rs_scene::{GraphicsResourceSource, Matrix, Scene};

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

/// Exact Page-XObject proof and Scene resource identity retained for one executed `Do`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedImageUse {
    source: ContentOperatorSource,
    xobject: PageXObjectReference,
    resource_source: GraphicsResourceSource,
}

impl ResolvedImageUse {
    pub(crate) const fn new(
        source: ContentOperatorSource,
        xobject: PageXObjectReference,
        resource_source: GraphicsResourceSource,
    ) -> Self {
        Self {
            source,
            xobject,
            resource_source,
        }
    }

    /// Returns exact decoded operator-token provenance and page-global ordinal.
    pub const fn source(self) -> ContentOperatorSource {
        self.source
    }

    /// Returns the inherited Page resource-name lookup proof.
    pub const fn xobject(self) -> PageXObjectReference {
        self.xobject
    }

    /// Returns the exact Scene source/decode identity selected for the draw.
    pub const fn resource_source(self) -> GraphicsResourceSource {
        self.resource_source
    }
}

/// Aggregate Image XObject lookup, acquisition, and exact-cache accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContentImageStats {
    image_uses: u64,
    lookups: u64,
    cache_hits: u64,
    acquisitions: u64,
    unique_images: u64,
    object_read_bytes: u64,
    object_parse_bytes: u64,
    metadata_entries: u64,
    encoded_bytes: u64,
    decoded_bytes: u64,
    scan_passes: u64,
    scan_input_bytes: u64,
    planning_operators: u64,
    cache_probes: u64,
    decode_fuel: u64,
    peak_acquisition_retained_bytes: u64,
    plan_retained_bytes: u64,
    peak_plan_retained_bytes: u64,
    cache_retained_bytes: u64,
    peak_cache_retained_bytes: u64,
    acquisition_polls: u64,
    execution_passes: u64,
}

impl ContentImageStats {
    /// Returns executed `Do` uses admitted in the current complete replay.
    pub const fn image_uses(self) -> u64 {
        self.image_uses
    }

    /// Returns Page XObject name lookups across all interpretation attempts.
    pub const fn lookups(self) -> u64 {
        self.lookups
    }

    /// Returns exact-cache hits across all interpretation attempts.
    pub const fn cache_hits(self) -> u64 {
        self.cache_hits
    }

    /// Returns distinct proof-bound acquisitions that reached ready.
    pub const fn acquisitions(self) -> u64 {
        self.acquisitions
    }

    /// Returns distinct decoded resources retained by the exact cache.
    pub const fn unique_images(self) -> u64 {
        self.unique_images
    }

    /// Returns exact object-source bytes consumed by successful distinct acquisitions.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }

    /// Returns parser-window bytes consumed by successful distinct acquisitions.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }

    /// Returns image metadata entries visited by successful distinct acquisitions.
    pub const fn metadata_entries(self) -> u64 {
        self.metadata_entries
    }

    /// Returns encoded payload bytes consumed by successful distinct acquisitions.
    pub const fn encoded_bytes(self) -> u64 {
        self.encoded_bytes
    }

    /// Returns aggregate decoded bytes across distinct cached resources.
    pub const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }

    /// Returns complete Content scan passes, which is at most one.
    pub const fn scan_passes(self) -> u64 {
        self.scan_passes
    }

    /// Returns decoded Content bytes retained by the single scan.
    pub const fn scan_input_bytes(self) -> u64 {
        self.scan_input_bytes
    }

    /// Returns operators inspected by the single structural planning pass.
    pub const fn planning_operators(self) -> u64 {
        self.planning_operators
    }

    /// Returns exact-cache key comparisons across interpretation attempts.
    pub const fn cache_probes(self) -> u64 {
        self.cache_probes
    }

    /// Returns foundational decode fuel consumed by successful distinct acquisitions.
    pub const fn decode_fuel(self) -> u64 {
        self.decode_fuel
    }

    /// Returns peak lower acquisition retention across successful distinct images.
    pub const fn peak_acquisition_retained_bytes(self) -> u64 {
        self.peak_acquisition_retained_bytes
    }

    /// Returns allocator-reported operator/proof plan capacity, including unique path and dash
    /// payloads retained by planned graphics actions.
    pub const fn plan_retained_bytes(self) -> u64 {
        self.plan_retained_bytes
    }

    /// Returns peak allocator-reported operator/proof plan capacity, including unique path and
    /// dash payloads retained by planned graphics actions.
    pub const fn peak_plan_retained_bytes(self) -> u64 {
        self.peak_plan_retained_bytes
    }

    /// Returns allocator-reported exact-cache metadata capacity.
    pub const fn cache_retained_bytes(self) -> u64 {
        self.cache_retained_bytes
    }

    /// Returns peak allocator-reported exact-cache metadata capacity.
    pub const fn peak_cache_retained_bytes(self) -> u64 {
        self.peak_cache_retained_bytes
    }

    /// Returns calls admitted into lower resumable Image XObject acquisition jobs.
    pub const fn acquisition_polls(self) -> u64 {
        self.acquisition_polls
    }

    /// Returns VM/Scene execution passes, which is at most one.
    pub const fn execution_passes(self) -> u64 {
        self.execution_passes
    }

    pub(crate) fn record_lookup(&mut self) -> Option<()> {
        self.lookups = self.lookups.checked_add(1)?;
        Some(())
    }

    pub(crate) fn record_cache_hit(&mut self) -> Option<()> {
        self.cache_hits = self.cache_hits.checked_add(1)?;
        Some(())
    }

    pub(crate) fn record_scan(&mut self, bytes: u64) -> Option<()> {
        self.scan_passes = self.scan_passes.checked_add(1)?;
        self.scan_input_bytes = self.scan_input_bytes.checked_add(bytes)?;
        Some(())
    }

    pub(crate) fn record_planning_operator(&mut self) -> Option<()> {
        self.planning_operators = self.planning_operators.checked_add(1)?;
        Some(())
    }

    pub(crate) fn record_cache_probe(&mut self) -> Option<()> {
        self.cache_probes = self.cache_probes.checked_add(1)?;
        Some(())
    }

    pub(crate) fn record_acquisition_poll(&mut self) -> Option<()> {
        self.acquisition_polls = self.acquisition_polls.checked_add(1)?;
        Some(())
    }

    pub(crate) fn record_plan_retained(&mut self, retained: u64) {
        self.plan_retained_bytes = retained;
        self.peak_plan_retained_bytes = self.peak_plan_retained_bytes.max(retained);
    }

    pub(crate) fn record_cache_retained(&mut self, retained: u64) {
        self.cache_retained_bytes = retained;
        self.peak_cache_retained_bytes = self.peak_cache_retained_bytes.max(retained);
    }

    pub(crate) fn record_execution_pass(&mut self) -> Option<()> {
        self.execution_passes = self.execution_passes.checked_add(1)?;
        Some(())
    }

    pub(crate) fn record_acquisition(
        &mut self,
        decoded_bytes: u64,
        cache_retained_bytes: u64,
        acquisition: ImageXObjectStats,
    ) -> Option<()> {
        self.acquisitions = self.acquisitions.checked_add(1)?;
        self.unique_images = self.unique_images.checked_add(1)?;
        self.object_read_bytes = self
            .object_read_bytes
            .checked_add(acquisition.object_read_bytes())?;
        self.object_parse_bytes = self
            .object_parse_bytes
            .checked_add(acquisition.object_parse_bytes())?;
        self.metadata_entries = self
            .metadata_entries
            .checked_add(acquisition.metadata_entries())?;
        self.encoded_bytes = self
            .encoded_bytes
            .checked_add(acquisition.encoded_bytes())?;
        self.decoded_bytes = self.decoded_bytes.checked_add(decoded_bytes)?;
        self.decode_fuel = self.decode_fuel.checked_add(acquisition.decode_fuel())?;
        self.peak_acquisition_retained_bytes = self
            .peak_acquisition_retained_bytes
            .max(acquisition.peak_retained_bytes());
        self.cache_retained_bytes = cache_retained_bytes;
        self.peak_cache_retained_bytes = self.peak_cache_retained_bytes.max(cache_retained_bytes);
        Some(())
    }

    pub(crate) fn set_image_uses(&mut self, image_uses: u64) {
        self.image_uses = image_uses;
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
    image_uses: u64,
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
        image_uses: u64,
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
            image_uses,
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

    /// Returns the executed Image XObject-use count.
    pub const fn image_uses(self) -> u64 {
        self.image_uses
    }

    /// Returns allocator-reported capacity retained by the published interpreted result.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    /// Returns peak VM-owned transient and final capacity.
    ///
    /// This includes decoded stream descriptors, the transient scanned program, current and
    /// planned-action path capacity, unique active or planned-action dash-array capacity,
    /// graphics-state stack capacity, and retained property and image-use proofs. Acquired Page
    /// content, the exact image cache, decoded image payloads, and Scene storage remain under their
    /// independent sealed budgets and are not included.
    pub const fn peak_retained_bytes(self) -> u64 {
        self.peak_retained_bytes
    }
}

impl Default for ContentVmStats {
    fn default() -> Self {
        Self::new(0, 0, 0, 0, 0, 0, 0, 0, 0)
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
    image_uses: Vec<ResolvedImageUse>,
    final_ctm: Matrix,
    scan_stats: ContentScanStats,
    vm_stats: ContentVmStats,
    property_stats: PagePropertyLookupStats,
    xobject_stats: PageXObjectLookupStats,
    image_stats: ContentImageStats,
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
        image_uses: Vec<ResolvedImageUse>,
        final_ctm: Matrix,
        scan_stats: ContentScanStats,
        vm_stats: ContentVmStats,
        property_stats: PagePropertyLookupStats,
        xobject_stats: PageXObjectLookupStats,
        image_stats: ContentImageStats,
    ) -> Self {
        Self {
            acquired,
            scene,
            property_uses,
            image_uses,
            final_ctm,
            scan_stats,
            vm_stats,
            property_stats,
            xobject_stats,
            image_stats,
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

    /// Returns executed Image XObject proofs in exact execution order.
    pub fn image_uses(&self) -> &[ResolvedImageUse] {
        &self.image_uses
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

    /// Returns Page XObject lookup work from the final complete replay.
    pub const fn xobject_stats(&self) -> PageXObjectLookupStats {
        self.xobject_stats
    }

    /// Returns aggregate Image XObject acquisition and exact-cache work.
    pub const fn image_stats(&self) -> ContentImageStats {
        self.image_stats
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
            .field("image_use_count", &self.image_uses.len())
            .field("scan_stats", &self.scan_stats)
            .field("vm_stats", &self.vm_stats)
            .field("property_stats", &self.property_stats)
            .field("xobject_stats", &self.xobject_stats)
            .field("image_stats", &self.image_stats)
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
        let stats = ContentVmStats::new(11, 29, 3, 5, 7, 13, 17, 1_024, 4_096);
        assert_eq!(stats.operators(), 11);
        assert_eq!(stats.fuel(), 29);
        assert_eq!(stats.max_graphics_state_depth(), 3);
        assert_eq!(stats.max_compatibility_depth(), 5);
        assert_eq!(stats.max_marked_content_depth(), 7);
        assert_eq!(stats.property_uses(), 13);
        assert_eq!(stats.image_uses(), 17);
        assert_eq!(stats.retained_bytes(), 1_024);
        assert_eq!(stats.peak_retained_bytes(), 4_096);
    }
}
