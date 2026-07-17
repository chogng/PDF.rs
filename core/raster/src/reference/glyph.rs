//! Deterministic staged glyph-outline coverage and paint production.
//!
//! `reference-glyph-v1` consumes only project-owned Scene glyph outlines and positioned glyph
//! uses. Font mapping, shaping, hinting, and operating-system font fallback are outside this
//! kernel. The module remains staged outside `ReferenceRenderJob` until the M3 integrated
//! renderer connects all visible command kinds in source order.

use std::mem::size_of;

use pdf_rs_scene::{
    FillRule, GlyphOutline, GlyphRun, GraphicsResource, GraphicsResourceEntry, Matrix, PageGeometry,
};

use super::coverage::{CoverageMask, SAMPLES_PER_PIXEL, rasterize_fill_union};
use super::geometry::{
    Affine, Fixed, GeometryCancellation, GeometryFailure, GeometryLimitKind, GeometryLimits,
    GeometryWork, PageDeviceMap, flatten_path,
};
use super::{NormalizedQ16, PremultipliedRgbaQ16, ReferenceColorProfile};

const FLATNESS_TOLERANCE_DENOMINATOR: i64 = 256;
const CANCELLATION_FUEL_INTERVAL: u64 = 256;
const HARD_MAX_CURVE_RECURSION: u8 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GlyphLimitKind {
    Glyphs,
    ResourceLookups,
    OutlineSegments,
    FlattenedSegments,
    Edges,
    Samples,
    CoverageBytes,
    OutputPixels,
    Composites,
    GeometryBytes,
    RetainedBytes,
    GeometryFuel,
    Fuel,
    CurveRecursion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GlyphFailure {
    NumericOverflow,
    InvalidGlyph,
    InvalidResource {
        resource: u32,
    },
    Cancelled,
    Allocation {
        attempted_bytes: u64,
    },
    Limit {
        kind: GlyphLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    },
}

#[derive(Clone, Copy)]
struct RetainedGeometryContext {
    retained_limit: u64,
    coverage_bytes: u64,
}

impl From<GeometryFailure> for GlyphFailure {
    fn from(value: GeometryFailure) -> Self {
        Self::from_geometry(value, None)
    }
}

impl GlyphFailure {
    fn from_geometry(value: GeometryFailure, retained: Option<RetainedGeometryContext>) -> Self {
        match value {
            GeometryFailure::NumericOverflow => Self::NumericOverflow,
            GeometryFailure::InvalidGeometry => Self::InvalidGlyph,
            GeometryFailure::Cancelled => Self::Cancelled,
            GeometryFailure::Allocation { attempted_bytes } => Self::Allocation { attempted_bytes },
            GeometryFailure::Limit {
                kind,
                limit,
                consumed,
                attempted,
            } => {
                if kind == GeometryLimitKind::GeometryBytes
                    && let Some(retained) = retained
                {
                    let Some(consumed) = retained.coverage_bytes.checked_add(consumed) else {
                        return Self::NumericOverflow;
                    };
                    return Self::Limit {
                        kind: GlyphLimitKind::RetainedBytes,
                        limit: retained.retained_limit,
                        consumed,
                        attempted,
                    };
                }
                let kind = match kind {
                    GeometryLimitKind::CurveRecursion => GlyphLimitKind::CurveRecursion,
                    GeometryLimitKind::Segments => GlyphLimitKind::FlattenedSegments,
                    GeometryLimitKind::Edges => GlyphLimitKind::Edges,
                    GeometryLimitKind::Samples => GlyphLimitKind::Samples,
                    GeometryLimitKind::CoverageBytes => GlyphLimitKind::CoverageBytes,
                    GeometryLimitKind::GeometryBytes => GlyphLimitKind::GeometryBytes,
                    GeometryLimitKind::Fuel => GlyphLimitKind::GeometryFuel,
                    GeometryLimitKind::DashChunks
                    | GeometryLimitKind::StrokeRuns
                    | GeometryLimitKind::StrokePrimitives
                    | GeometryLimitKind::ClipDepth
                    | GeometryLimitKind::ClipBytes => return Self::InvalidGlyph,
                };
                Self::Limit {
                    kind,
                    limit,
                    consumed,
                    attempted,
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GlyphLimits {
    pub(crate) max_glyphs: u64,
    pub(crate) max_resource_lookups: u64,
    pub(crate) max_outline_segments: u64,
    pub(crate) max_flattened_segments: u64,
    pub(crate) max_edges: u64,
    pub(crate) max_samples: u64,
    pub(crate) max_coverage_bytes: u64,
    pub(crate) max_output_pixels: u64,
    pub(crate) max_composites: u64,
    pub(crate) max_geometry_bytes: u64,
    pub(crate) max_retained_bytes: u64,
    pub(crate) max_geometry_fuel: u64,
    pub(crate) max_fuel: u64,
    pub(crate) max_curve_recursion: u8,
}

impl Default for GlyphLimits {
    fn default() -> Self {
        Self {
            max_glyphs: 4_000_000,
            max_resource_lookups: 4_000_000,
            max_outline_segments: 16_000_000,
            max_flattened_segments: 16_000_000,
            max_edges: 16_000_000,
            max_samples: 1_000_000_000,
            max_coverage_bytes: 256 * 1024 * 1024,
            max_output_pixels: 67_108_864,
            max_composites: 1_000_000_000,
            max_geometry_bytes: 256 * 1024 * 1024,
            max_retained_bytes: 1024 * 1024 * 1024,
            max_geometry_fuel: 1_000_000_000,
            max_fuel: 1_000_000_000,
            max_curve_recursion: 16,
        }
    }
}

pub(crate) trait GlyphCancellation {
    fn is_cancelled(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct GlyphStats {
    glyphs: u64,
    resource_lookups: u64,
    outline_segments: u64,
    flattened_segments: u64,
    edges: u64,
    samples: u64,
    coverage_bytes: u64,
    output_pixels: u64,
    composites: u64,
    geometry_bytes: u64,
    peak_geometry_bytes: u64,
    retained_bytes: u64,
    geometry_fuel: u64,
    fuel: u64,
    cancellation_checks: u64,
}

impl GlyphStats {
    pub(crate) const fn glyphs(self) -> u64 {
        self.glyphs
    }

    pub(crate) const fn resource_lookups(self) -> u64 {
        self.resource_lookups
    }

    pub(crate) const fn outline_segments(self) -> u64 {
        self.outline_segments
    }

    pub(crate) const fn flattened_segments(self) -> u64 {
        self.flattened_segments
    }

    pub(crate) const fn edges(self) -> u64 {
        self.edges
    }

    pub(crate) const fn samples(self) -> u64 {
        self.samples
    }

    pub(crate) const fn coverage_bytes(self) -> u64 {
        self.coverage_bytes
    }

    pub(crate) const fn output_pixels(self) -> u64 {
        self.output_pixels
    }

    pub(crate) const fn composites(self) -> u64 {
        self.composites
    }

    pub(crate) const fn geometry_bytes(self) -> u64 {
        self.geometry_bytes
    }

    pub(crate) const fn peak_geometry_bytes(self) -> u64 {
        self.peak_geometry_bytes
    }

    pub(crate) const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    pub(crate) const fn geometry_fuel(self) -> u64 {
        self.geometry_fuel
    }

    pub(crate) const fn fuel(self) -> u64 {
        self.fuel
    }

    pub(crate) const fn cancellation_checks(self) -> u64 {
        self.cancellation_checks
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GlyphRaster {
    width: u32,
    height: u32,
    pixels: Vec<PremultipliedRgbaQ16>,
    stats: GlyphStats,
}

impl GlyphRaster {
    pub(crate) const PROFILE: &'static str = "reference-glyph-v1";

    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    pub(crate) fn pixels(&self) -> &[PremultipliedRgbaQ16] {
        &self.pixels
    }

    pub(crate) fn pixel(&self, x: u32, y: u32) -> Option<PremultipliedRgbaQ16> {
        pixel_index(self.width, self.height, x, y).map(|index| self.pixels[index])
    }

    pub(crate) const fn stats(&self) -> GlyphStats {
        self.stats
    }
}

struct GlyphWork<'a> {
    limits: GlyphLimits,
    cancellation: &'a dyn GlyphCancellation,
    stats: GlyphStats,
    fuel_since_cancellation: u64,
}

impl<'a> GlyphWork<'a> {
    fn new(
        limits: GlyphLimits,
        cancellation: &'a dyn GlyphCancellation,
    ) -> Result<Self, GlyphFailure> {
        if limits.max_glyphs == 0
            || limits.max_resource_lookups == 0
            || limits.max_outline_segments == 0
            || limits.max_flattened_segments == 0
            || limits.max_edges == 0
            || limits.max_samples == 0
            || limits.max_coverage_bytes == 0
            || limits.max_output_pixels == 0
            || limits.max_composites == 0
            || limits.max_geometry_bytes == 0
            || limits.max_retained_bytes == 0
            || limits.max_geometry_fuel == 0
            || limits.max_fuel == 0
            || limits.max_curve_recursion == 0
            || limits.max_curve_recursion > HARD_MAX_CURVE_RECURSION
        {
            return Err(GlyphFailure::InvalidGlyph);
        }
        let mut work = Self {
            limits,
            cancellation,
            stats: GlyphStats::default(),
            fuel_since_cancellation: 0,
        };
        work.check_cancellation()?;
        Ok(work)
    }

    fn ensure(
        &self,
        kind: GlyphLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    ) -> Result<u64, GlyphFailure> {
        let next = consumed
            .checked_add(attempted)
            .ok_or(GlyphFailure::NumericOverflow)?;
        if next > limit {
            return Err(GlyphFailure::Limit {
                kind,
                limit,
                consumed,
                attempted,
            });
        }
        Ok(next)
    }

    fn admit_glyphs(&mut self, glyphs: u64) -> Result<(), GlyphFailure> {
        self.stats.glyphs =
            self.ensure(GlyphLimitKind::Glyphs, self.limits.max_glyphs, 0, glyphs)?;
        self.stats.resource_lookups = self.ensure(
            GlyphLimitKind::ResourceLookups,
            self.limits.max_resource_lookups,
            0,
            glyphs,
        )?;
        Ok(())
    }

    fn charge_outline_segments(&mut self, amount: u64) -> Result<(), GlyphFailure> {
        self.stats.outline_segments = self.ensure(
            GlyphLimitKind::OutlineSegments,
            self.limits.max_outline_segments,
            self.stats.outline_segments,
            amount,
        )?;
        Ok(())
    }

    fn charge_composites(&mut self, amount: u64) -> Result<(), GlyphFailure> {
        self.stats.composites = self.ensure(
            GlyphLimitKind::Composites,
            self.limits.max_composites,
            self.stats.composites,
            amount,
        )?;
        self.charge_fuel(amount)
    }

    fn charge_fuel(&mut self, amount: u64) -> Result<(), GlyphFailure> {
        self.stats.fuel = self.ensure(
            GlyphLimitKind::Fuel,
            self.limits.max_fuel,
            self.stats.fuel,
            amount,
        )?;
        self.fuel_since_cancellation = self
            .fuel_since_cancellation
            .checked_add(amount)
            .ok_or(GlyphFailure::NumericOverflow)?;
        while self.fuel_since_cancellation >= CANCELLATION_FUEL_INTERVAL {
            self.check_cancellation()?;
            self.fuel_since_cancellation -= CANCELLATION_FUEL_INTERVAL;
        }
        Ok(())
    }

    fn check_cancellation(&mut self) -> Result<(), GlyphFailure> {
        self.stats.cancellation_checks = self
            .stats
            .cancellation_checks
            .checked_add(1)
            .ok_or(GlyphFailure::NumericOverflow)?;
        if self.cancellation.is_cancelled() {
            return Err(GlyphFailure::Cancelled);
        }
        Ok(())
    }
}

struct GlyphGeometryCancellation<'a>(&'a dyn GlyphCancellation);

impl GeometryCancellation for GlyphGeometryCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the staged glyph kernel keeps resources, geometry, backdrop, clip, limits, and cancellation explicit"
)]
pub(crate) fn rasterize_glyph_run(
    run: &GlyphRun,
    resources: &[GraphicsResourceEntry],
    geometry: PageGeometry,
    output_width: u32,
    output_height: u32,
    backdrop: &[PremultipliedRgbaQ16],
    clip: Option<&CoverageMask>,
    limits: GlyphLimits,
    cancellation: &dyn GlyphCancellation,
) -> Result<GlyphRaster, GlyphFailure> {
    let mut work = GlyphWork::new(limits, cancellation)?;
    let glyph_count =
        u64::try_from(run.glyphs().len()).map_err(|_| GlyphFailure::NumericOverflow)?;
    work.admit_glyphs(glyph_count)?;

    let output_pixels = u64::from(output_width)
        .checked_mul(u64::from(output_height))
        .ok_or(GlyphFailure::NumericOverflow)?;
    work.stats.output_pixels = work.ensure(
        GlyphLimitKind::OutputPixels,
        limits.max_output_pixels,
        0,
        output_pixels,
    )?;
    if output_pixels == 0 || u64::try_from(backdrop.len()).unwrap_or(u64::MAX) != output_pixels {
        return Err(GlyphFailure::InvalidGlyph);
    }
    if let Some(clip) = clip
        && (clip.width() != output_width || clip.height() != output_height)
    {
        return Err(GlyphFailure::InvalidGlyph);
    }
    let pixel_item_bytes = u64::try_from(size_of::<PremultipliedRgbaQ16>())
        .map_err(|_| GlyphFailure::NumericOverflow)?;
    let semantic_pixel_bytes = output_pixels
        .checked_mul(pixel_item_bytes)
        .ok_or(GlyphFailure::NumericOverflow)?;
    let semantic_coverage_bytes = output_pixels
        .checked_mul(u64::try_from(size_of::<u64>()).unwrap_or(u64::MAX))
        .ok_or(GlyphFailure::NumericOverflow)?;
    let semantic_retained_bytes = semantic_coverage_bytes
        .checked_add(semantic_pixel_bytes)
        .ok_or(GlyphFailure::NumericOverflow)?;
    work.ensure(
        GlyphLimitKind::RetainedBytes,
        limits.max_retained_bytes,
        0,
        semantic_retained_bytes,
    )?;

    let geometry_limits = GeometryLimits {
        max_segments: limits.max_flattened_segments,
        max_edges: limits.max_edges,
        max_samples: limits.max_samples,
        max_coverage_bytes: limits.max_coverage_bytes,
        max_geometry_bytes: limits.max_geometry_bytes,
        max_fuel: limits.max_geometry_fuel,
        ..GeometryLimits::default()
    };
    let geometry_cancellation = GlyphGeometryCancellation(cancellation);
    let mut geometry_work = GeometryWork::new(geometry_limits, &geometry_cancellation)?;
    let page_map = PageDeviceMap::new(geometry, output_width, output_height)?;
    let flatness = Fixed::from_raw(Fixed::ONE.raw() / FLATNESS_TOLERANCE_DENOMINATOR);
    let mut coverage = CoverageMask::empty(output_width, output_height, &mut geometry_work)?;
    work.stats.coverage_bytes = coverage.retained_bytes()?;
    work.ensure(
        GlyphLimitKind::RetainedBytes,
        limits.max_retained_bytes,
        work.stats.coverage_bytes,
        semantic_pixel_bytes,
    )?;
    let retained_geometry_bytes = limits
        .max_retained_bytes
        .checked_sub(work.stats.coverage_bytes)
        .ok_or(GlyphFailure::NumericOverflow)?;
    let retained_geometry_binds = retained_geometry_bytes <= limits.max_geometry_bytes;
    let effective_geometry_bytes = retained_geometry_bytes.min(limits.max_geometry_bytes);
    geometry_work.tighten_geometry_bytes_limit(effective_geometry_bytes)?;
    let retained_geometry = retained_geometry_binds.then_some(RetainedGeometryContext {
        retained_limit: limits.max_retained_bytes,
        coverage_bytes: work.stats.coverage_bytes,
    });

    for glyph in run.glyphs() {
        let outline = resolve_outline(resources, glyph.outline())?;
        let outline_segments = u64::try_from(outline.outline().segments().len())
            .map_err(|_| GlyphFailure::NumericOverflow)?;
        work.charge_outline_segments(outline_segments)?;
        work.charge_fuel(1)?;

        let transform =
            glyph_device_transform(page_map, glyph.transform(), outline.units_per_em())?;
        let path = flatten_path(
            outline.outline(),
            transform,
            transform,
            flatness,
            limits.max_curve_recursion,
            &mut geometry_work,
        )
        .map_err(|failure| GlyphFailure::from_geometry(failure, retained_geometry))?;
        let edges = super::coverage::FillEdges::from_path(&path, &mut geometry_work)
            .map_err(|failure| GlyphFailure::from_geometry(failure, retained_geometry))?;
        rasterize_fill_union(&edges, FillRule::Nonzero, &mut coverage, &mut geometry_work)
            .map_err(|failure| GlyphFailure::from_geometry(failure, retained_geometry))?;
    }

    work.stats.flattened_segments = geometry_work.segments();
    work.stats.edges = geometry_work.edges();
    work.stats.samples = geometry_work.samples();
    work.stats.geometry_bytes = geometry_work.geometry_bytes();
    work.stats.peak_geometry_bytes = geometry_work.peak_geometry_bytes();
    work.stats.geometry_fuel = geometry_work.fuel();
    work.stats.cancellation_checks = work
        .stats
        .cancellation_checks
        .checked_add(geometry_work.cancellation_checks())
        .ok_or(GlyphFailure::NumericOverflow)?;
    let coverage_geometry_peak = work
        .stats
        .coverage_bytes
        .checked_add(work.stats.peak_geometry_bytes)
        .ok_or(GlyphFailure::NumericOverflow)?;
    work.stats.retained_bytes = work.ensure(
        GlyphLimitKind::RetainedBytes,
        limits.max_retained_bytes,
        0,
        coverage_geometry_peak,
    )?;
    let capacity = usize::try_from(output_pixels).map_err(|_| GlyphFailure::NumericOverflow)?;
    work.check_cancellation()?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(capacity)
        .map_err(|_| GlyphFailure::Allocation {
            attempted_bytes: semantic_pixel_bytes,
        })?;
    let pixel_retained_bytes = u64::try_from(pixels.capacity())
        .map_err(|_| GlyphFailure::NumericOverflow)?
        .checked_mul(pixel_item_bytes)
        .ok_or(GlyphFailure::NumericOverflow)?;
    let coverage_pixel_peak = work.ensure(
        GlyphLimitKind::RetainedBytes,
        limits.max_retained_bytes,
        work.stats.coverage_bytes,
        pixel_retained_bytes,
    )?;
    work.stats.retained_bytes = work.stats.retained_bytes.max(coverage_pixel_peak);
    for pixel in backdrop {
        pixels.push(*pixel);
        work.charge_fuel(1)?;
    }

    let (source, blend_mode) = ReferenceColorProfile::ReferenceColorV1.prepare_paint(run.paint());
    work.charge_fuel(1)?;
    for y in 0..output_height {
        for x in 0..output_width {
            let index =
                pixel_index(output_width, output_height, x, y).ok_or(GlyphFailure::InvalidGlyph)?;
            let glyph_mask = coverage
                .sample_mask(x, y)
                .ok_or(GlyphFailure::InvalidGlyph)?;
            let clip_mask = clip
                .and_then(|mask| mask.sample_mask(x, y))
                .unwrap_or(u64::MAX);
            let covered = u64::from((glyph_mask & clip_mask).count_ones());
            if covered != 0 {
                let backdrop_pixel = pixels[index];
                let painted = blend_mode.source_over(source, backdrop_pixel);
                pixels[index] = coverage_average(backdrop_pixel, painted, covered)?;
            }
            work.charge_composites(covered)?;
            work.charge_fuel(1)?;
        }
    }
    work.check_cancellation()?;

    Ok(GlyphRaster {
        width: output_width,
        height: output_height,
        pixels,
        stats: work.stats,
    })
}

fn resolve_outline(
    resources: &[GraphicsResourceEntry],
    resource: pdf_rs_scene::GraphicsResourceId,
) -> Result<&GlyphOutline, GlyphFailure> {
    let index = usize::try_from(resource.value()).map_err(|_| GlyphFailure::NumericOverflow)?;
    let entry = resources
        .get(index)
        .filter(|entry| entry.id() == resource)
        .ok_or(GlyphFailure::InvalidResource {
            resource: resource.value(),
        })?;
    match entry.resource() {
        GraphicsResource::GlyphOutline(outline) => Ok(outline),
        GraphicsResource::Path(_) | GraphicsResource::Image(_) => {
            Err(GlyphFailure::InvalidResource {
                resource: resource.value(),
            })
        }
    }
}

fn glyph_device_transform(
    page_map: PageDeviceMap,
    glyph_to_page: Matrix,
    units_per_em: u16,
) -> Result<Affine, GlyphFailure> {
    let units_per_em = Fixed::from_i64(i64::from(units_per_em))?;
    let scale = Fixed::ONE.checked_div(units_per_em)?;
    let font_units_to_em = Affine::new(
        scale,
        Fixed::ZERO,
        Fixed::ZERO,
        scale,
        Fixed::ZERO,
        Fixed::ZERO,
    );
    Ok(page_map
        .combined(glyph_to_page)?
        .checked_concat(font_units_to_em)?)
}

fn coverage_average(
    backdrop: PremultipliedRgbaQ16,
    painted: PremultipliedRgbaQ16,
    covered: u64,
) -> Result<PremultipliedRgbaQ16, GlyphFailure> {
    if covered > u64::from(SAMPLES_PER_PIXEL) {
        return Err(GlyphFailure::InvalidGlyph);
    }
    let uncovered = u64::from(SAMPLES_PER_PIXEL)
        .checked_sub(covered)
        .ok_or(GlyphFailure::NumericOverflow)?;
    let average = |background: NormalizedQ16,
                   foreground: NormalizedQ16|
     -> Result<NormalizedQ16, GlyphFailure> {
        let sum = u64::from(background.bits())
            .checked_mul(uncovered)
            .and_then(|value| {
                u64::from(foreground.bits())
                    .checked_mul(covered)
                    .and_then(|foreground| value.checked_add(foreground))
            })
            .ok_or(GlyphFailure::NumericOverflow)?;
        let rounded = sum
            .checked_add(u64::from(SAMPLES_PER_PIXEL / 2))
            .ok_or(GlyphFailure::NumericOverflow)?
            / u64::from(SAMPLES_PER_PIXEL);
        let bits = u32::try_from(rounded).map_err(|_| GlyphFailure::NumericOverflow)?;
        NormalizedQ16::from_bits(bits).ok_or(GlyphFailure::NumericOverflow)
    };
    PremultipliedRgbaQ16::new(
        average(backdrop.red(), painted.red())?,
        average(backdrop.green(), painted.green())?,
        average(backdrop.blue(), painted.blue())?,
        average(backdrop.alpha(), painted.alpha())?,
    )
    .ok_or(GlyphFailure::NumericOverflow)
}

fn pixel_index(width: u32, height: u32, x: u32, y: u32) -> Option<usize> {
    if x >= width || y >= height {
        return None;
    }
    let index = u64::from(y)
        .checked_mul(u64::from(width))?
        .checked_add(u64::from(x))?;
    usize::try_from(index).ok()
}
