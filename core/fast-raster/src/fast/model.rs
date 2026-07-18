//! Public identities, values, and cancellation contracts.

use core::fmt;

use pdf_rs_policy::{PlannedTileIdentity, RenderConfigHash, RenderPlanHash};

/// Cooperative cancellation observed only at renderer-owned deterministic work intervals.
pub trait FastRasterCancellation: Send + Sync {
    /// Returns whether private work should terminate without publication.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation token that never cancels.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancelled;

impl FastRasterCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Versioned independent Fast CPU scalar algorithm.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FastRasterAlgorithm {
    /// Bounds-binned, 4×4 scalar coverage, Q16 compositing, nearest-image Fast profile.
    ScalarTiledV1,
}

impl FastRasterAlgorithm {
    /// Returns the stable algorithm label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ScalarTiledV1 => "fast-scalar-tiled-v1",
        }
    }
}

/// Complete implementation identity paired with the policy-owned configuration hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastRasterIdentity {
    algorithm: FastRasterAlgorithm,
    render_config_hash: RenderConfigHash,
    clipping_profile: &'static str,
    image_profile: &'static str,
    glyph_profile: &'static str,
    compositing_profile: &'static str,
}

impl FastRasterIdentity {
    pub(crate) const fn scalar_v1(render_config_hash: RenderConfigHash) -> Self {
        Self {
            algorithm: FastRasterAlgorithm::ScalarTiledV1,
            render_config_hash,
            clipping_profile: "fast-clip-mask-4x4-v1",
            image_profile: "fast-nearest-image-v1",
            glyph_profile: "fast-outline-glyph-v1",
            compositing_profile: "fast-premultiplied-q16-v1",
        }
    }

    /// Returns the implementation algorithm.
    pub const fn algorithm(self) -> FastRasterAlgorithm {
        self.algorithm
    }

    /// Returns the hash that binds tile halo, antialiasing, flatness, recursion, image and glyph
    /// sampling, compositing, output conversion, backend, quality, and cancellation interval.
    pub const fn render_config_hash(self) -> RenderConfigHash {
        self.render_config_hash
    }

    /// Returns the clipping algorithm label.
    pub const fn clipping_profile(self) -> &'static str {
        self.clipping_profile
    }

    /// Returns the image sampling algorithm label.
    pub const fn image_profile(self) -> &'static str {
        self.image_profile
    }

    /// Returns the glyph sampling algorithm label.
    pub const fn glyph_profile(self) -> &'static str {
        self.glyph_profile
    }

    /// Returns the compositing and output-conversion algorithm label.
    pub const fn compositing_profile(self) -> &'static str {
        self.compositing_profile
    }
}

/// Deterministic command binning retained for one immutable plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FastTileBins {
    plan_hash: RenderPlanHash,
    bins: Vec<Vec<u32>>,
    entries: u64,
    retained_bytes: u64,
}

impl FastTileBins {
    pub(crate) const fn new(
        plan_hash: RenderPlanHash,
        bins: Vec<Vec<u32>>,
        entries: u64,
        retained_bytes: u64,
    ) -> Self {
        Self {
            plan_hash,
            bins,
            entries,
            retained_bytes,
        }
    }

    /// Returns the exact plan identity used during binning.
    pub const fn plan_hash(&self) -> RenderPlanHash {
        self.plan_hash
    }

    /// Returns per-tile source command indices in canonical source order.
    pub fn bins(&self) -> &[Vec<u32>] {
        &self.bins
    }

    /// Returns the aggregate retained command references.
    pub const fn entries(&self) -> u64 {
        self.entries
    }

    /// Returns allocator-reported durable bin retention.
    pub const fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }

    pub(crate) fn into_inner_bins(self) -> Vec<Vec<u32>> {
        self.bins
    }
}

/// Deterministic accounting for one complete Fast job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FastRasterStats {
    commands_considered: u64,
    bin_entries: u64,
    tiles: u64,
    pixels: u64,
    retained_bytes: u64,
    peak_intermediate_bytes: u64,
    fuel: u64,
    cancellation_checks: u64,
}

impl FastRasterStats {
    #[allow(
        clippy::too_many_arguments,
        reason = "published statistics retain every independently bounded dimension explicitly"
    )]
    pub(crate) const fn new(
        commands_considered: u64,
        bin_entries: u64,
        tiles: u64,
        pixels: u64,
        retained_bytes: u64,
        peak_intermediate_bytes: u64,
        fuel: u64,
        cancellation_checks: u64,
    ) -> Self {
        Self {
            commands_considered,
            bin_entries,
            tiles,
            pixels,
            retained_bytes,
            peak_intermediate_bytes,
            fuel,
            cancellation_checks,
        }
    }

    /// Returns Scene commands considered by the binning pass.
    pub const fn commands_considered(self) -> u64 {
        self.commands_considered
    }

    /// Returns retained command references across all bins.
    pub const fn bin_entries(self) -> u64 {
        self.bin_entries
    }

    /// Returns atomically published tiles.
    pub const fn tiles(self) -> u64 {
        self.tiles
    }

    /// Returns product pixels published across the tile set.
    pub const fn pixels(self) -> u64 {
        self.pixels
    }

    /// Returns durable bin and pixel bytes.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    /// Returns the greatest simultaneous private working-byte estimate.
    pub const fn peak_intermediate_bytes(self) -> u64 {
        self.peak_intermediate_bytes
    }

    /// Returns deterministic scalar work.
    pub const fn fuel(self) -> u64 {
        self.fuel
    }

    /// Returns renderer-owned cancellation probes.
    pub const fn cancellation_checks(self) -> u64 {
        self.cancellation_checks
    }
}

/// One complete immutable product tile.
pub struct FastTile {
    identity: PlannedTileIdentity,
    raster_identity: FastRasterIdentity,
    stride: u32,
    pixels: Vec<u8>,
}

impl FastTile {
    pub(crate) fn new(
        identity: PlannedTileIdentity,
        raster_identity: FastRasterIdentity,
        stride: u32,
        pixels: Vec<u8>,
    ) -> Self {
        Self {
            identity,
            raster_identity,
            stride,
            pixels,
        }
    }

    /// Borrows the complete generation-bound tile identity.
    pub const fn identity(&self) -> &PlannedTileIdentity {
        &self.identity
    }

    /// Returns the complete implementation/configuration identity.
    pub const fn raster_identity(&self) -> FastRasterIdentity {
        self.raster_identity
    }

    /// Returns the exact top-down RGBA8 row stride.
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// Borrows complete straight-alpha sRGB RGBA8 bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, crate::fast::FastRasterError> {
        u64::try_from(self.pixels.capacity()).map_err(|_| {
            crate::fast::FastRasterError::for_code(
                crate::fast::FastRasterErrorCode::NumericOverflow,
            )
        })
    }
}

impl fmt::Debug for FastTile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FastTile")
            .field("identity", &self.identity)
            .field("raster_identity", &self.raster_identity)
            .field("stride", &self.stride)
            .field("byte_length", &self.pixels.len())
            .field("pixels", &"[REDACTED]")
            .finish()
    }
}

/// Complete atomically published tile set for one RenderPlan.
#[derive(Debug)]
pub struct FastTileSet {
    plan_hash: RenderPlanHash,
    tiles: Vec<FastTile>,
    stats: FastRasterStats,
}

impl FastTileSet {
    pub(crate) const fn new(
        plan_hash: RenderPlanHash,
        tiles: Vec<FastTile>,
        stats: FastRasterStats,
    ) -> Self {
        Self {
            plan_hash,
            tiles,
            stats,
        }
    }

    /// Returns the exact plan identity.
    pub const fn plan_hash(&self) -> RenderPlanHash {
        self.plan_hash
    }

    /// Borrows complete tiles in the caller-requested permutation.
    pub fn tiles(&self) -> &[FastTile] {
        &self.tiles
    }

    /// Returns complete deterministic accounting.
    pub const fn stats(&self) -> FastRasterStats {
        self.stats
    }
}
